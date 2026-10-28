// The connections the tree shows: `goku.connections` (user + workspace scope) and `.env` discovery, filtered by Workspace
// Trust, kept current by a file watcher and a configuration listener. Passwords: settings connections use SecretStorage
// (`goku.password.<id>`), env connections re-read their file at connect time and store nothing (D7, D8).
import { posix } from 'node:path';
import * as vscode from 'vscode';
import { parseEnv } from './envParse.ts';
import { envConnections, sanitizeMappings, type EnvConnection, type EnvFile, type EnvMapping } from './envProfiles.ts';
import { mergeConnections, type Connection } from './model.ts';
import { needsPassword, secretFingerprint, type Credentials, type ProfileJson } from './profile.ts';

/** Resolves to why `profile` is invalid, or undefined when it's fine; throws when the engine can't be asked. */
export type Validator = (profile: ProfileJson) => Promise<string | undefined>;

export interface ConnectionStoreOptions {
  secrets: vscode.SecretStorage;
  log: vscode.LogOutputChannel;
  validate?: Validator;
  /** Validation only runs once this is true, so listing connections never starts the engine (call `revalidate` later). */
  canValidate?: () => boolean;
}

/** Where `getCredentials` found a password, so a rejected one can be handled (see `promptPassword`). */
export type PasswordSource = 'saved' | 'session' | 'typed' | 'none';

interface EnvSource {
  uri: vscode.Uri;
  file: EnvFile;
  mappings: EnvMapping[];
}

/** What a saved password is stored as: tied to the server it was typed for (see `secretFingerprint`). */
interface SavedSecret {
  fingerprint: string;
  password: string;
}

const MAX_ENV_BYTES = 1 << 20;
const SETTINGS = ['goku.connections', 'goku.envFiles', 'goku.envConnections'];

export function passwordKey(id: string): string {
  return `goku.password.${id}`;
}

export class ConnectionStore implements vscode.Disposable {
  private readonly options: ConnectionStoreOptions;
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChange = this.changed.event;
  private connections: Connection[] = [];
  private envById = new Map<string, EnvSource>();
  private watchers: vscode.Disposable[] = [];
  private readonly disposables: vscode.Disposable[] = [];
  /** Passwords the user chose not to save, until VS Code closes. */
  private readonly sessionSecrets = new Map<string, SavedSecret>();
  /** Engine verdicts by profile JSON: undefined = fine, string = why not. */
  private readonly verdicts = new Map<string, string | undefined>();
  private generation = 0;
  private debounce: ReturnType<typeof setTimeout> | undefined;

  constructor(options: ConnectionStoreOptions) {
    this.options = options;
    this.disposables.push(
      this.changed,
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (SETTINGS.some((s) => e.affectsConfiguration(s))) void this.refresh();
      }),
      vscode.workspace.onDidChangeWorkspaceFolders(() => void this.refresh()),
      vscode.workspace.onDidGrantWorkspaceTrust(() => void this.refresh()),
    );
    void this.refresh();
  }

  list(): readonly Connection[] {
    return this.connections;
  }

  get(id: string): Connection | undefined {
    return this.connections.find((c) => c.id === id);
  }

  /** Re-reads settings and env files, then validates through the engine when it's available. */
  async refresh(): Promise<void> {
    const generation = ++this.generation;
    const trusted = vscode.workspace.isTrusted;
    const sources = trusted ? this.envSources() : [];
    this.watch(sources);
    const env: EnvConnection[] = [];
    const envById = new Map<string, EnvSource>();
    for (const s of sources) {
      for (const c of await this.readEnv(s)) {
        env.push({ ...c, credentials: undefined }); // the password is read again at connect time, not kept
        envById.set(c.id, s);
      }
    }
    const inspected = vscode.workspace.getConfiguration('goku').inspect<unknown>('connections');
    const list = mergeConnections({ user: inspected?.globalValue, workspace: inspected?.workspaceValue, env, trusted });
    if (generation !== this.generation) return;
    this.envById = envById;
    this.connections = this.applyVerdicts(list);
    for (const c of this.connections) if (c.invalid) this.options.log.warn(`Connection “${c.name}”: ${c.invalid}`);
    this.changed.fire();
    await this.validateAll(generation);
  }

  /** Profile + credentials for `connection/open`, read fresh (an env file may have changed). Undefined = cancelled. */
  async openParams(conn: Connection): Promise<{ profile: ProfileJson; credentials: Credentials } | undefined> {
    if (conn.source === 'env') {
      const fresh = await this.envConnection(conn);
      return { profile: fresh.profile!, credentials: fresh.credentials ?? {} };
    }
    if (!conn.profile || conn.invalid) throw new Error(conn.invalid ?? `“${conn.name}” can't be used.`);
    const credentials = await this.getCredentials(conn);
    return credentials && { profile: conn.profile, credentials };
  }

  /** Settings connections: SecretStorage, else a password prompt (with an option to save). Undefined = cancelled. */
  async getCredentials(conn: Connection): Promise<Credentials | undefined> {
    return (await this.credentialsWithSource(conn))?.credentials;
  }

  /** `getCredentials`, plus where the password came from. */
  async credentialsWithSource(conn: Connection): Promise<{ credentials: Credentials; source: PasswordSource } | undefined> {
    if (conn.source === 'env') return { credentials: (await this.envConnection(conn)).credentials ?? {}, source: 'none' };
    if (!conn.profile) throw new Error(conn.invalid ?? `“${conn.name}” can't be used.`);
    if (!needsPassword(conn.profile)) return { credentials: {}, source: 'none' };
    const fingerprint = secretFingerprint(conn.profile);
    const session = this.sessionSecrets.get(conn.id);
    if (session?.fingerprint === fingerprint) return { credentials: { password: session.password }, source: 'session' };
    const saved = parseSecret(await this.options.secrets.get(passwordKey(conn.id)));
    if (saved?.fingerprint === fingerprint) return { credentials: { password: saved.password }, source: 'saved' };
    const prompt = saved ? `The saved password was for a different server (${conn.endpoint} now).` : conn.endpoint;
    const credentials = await this.promptPassword(conn, prompt);
    return credentials && { credentials, source: 'typed' };
  }

  /** Asks for the password of a settings connection (e.g. after the server rejected one). Undefined = cancelled. */
  async promptPassword(conn: Connection, prompt: string): Promise<Credentials | undefined> {
    if (!conn.profile) throw new Error(conn.invalid ?? `“${conn.name}” can't be used.`);
    const fingerprint = secretFingerprint(conn.profile);
    const password = await vscode.window.showInputBox({
      title: `Password for ${conn.name}`,
      prompt,
      password: true,
      ignoreFocusOut: true,
    });
    if (password === undefined) return undefined;
    const save = await vscode.window.showQuickPick(
      [
        { label: 'Save in VS Code secret storage', save: true },
        { label: 'Use until VS Code closes', save: false },
      ],
      { title: `Password for ${conn.name}`, ignoreFocusOut: true },
    );
    if (save?.save) await this.savePassword(conn.id, conn.profile, password);
    else this.sessionSecrets.set(conn.id, { fingerprint, password });
    return { password };
  }

  /** Forgets a password kept until VS Code closes (not the saved one). */
  forgetSessionPassword(id: string): void {
    this.sessionSecrets.delete(id);
  }

  /** Validates through the engine now (e.g. once it has started); a no-op while `canValidate` says no. */
  revalidate(): Promise<void> {
    return this.validateAll(this.generation);
  }

  async savePassword(id: string, profile: ProfileJson, password: string): Promise<void> {
    const secret: SavedSecret = { fingerprint: secretFingerprint(profile), password };
    await this.options.secrets.store(passwordKey(id), JSON.stringify(secret));
    this.sessionSecrets.delete(id);
  }

  async forgetPassword(id: string): Promise<void> {
    this.sessionSecrets.delete(id);
    await this.options.secrets.delete(passwordKey(id));
  }

  /** Appends to user-scope `goku.connections`, keeping every existing entry exactly as it is. */
  async addToUserSettings(profile: ProfileJson): Promise<void> {
    const config = vscode.workspace.getConfiguration('goku');
    const current = config.inspect<unknown>('connections')?.globalValue;
    if (current !== undefined && !Array.isArray(current)) {
      throw new Error('goku.connections in your user settings isn’t a list. Fix it first (Goku: Open Settings).');
    }
    await config.update('connections', [...(current ?? []), profile], vscode.ConfigurationTarget.Global);
  }

  dispose(): void {
    clearTimeout(this.debounce);
    for (const d of [...this.watchers, ...this.disposables]) d.dispose();
    this.watchers = [];
  }

  private envSources(): EnvSource[] {
    const folders = vscode.workspace.workspaceFolders ?? [];
    return folders.flatMap((folder) => {
      const config = vscode.workspace.getConfiguration('goku', folder.uri);
      const paths = config.get<unknown>('envFiles');
      const mappings = sanitizeMappings(config.get<unknown>('envConnections'));
      if (!Array.isArray(paths)) return [];
      return paths
        .filter((p): p is string => typeof p === 'string' && !!p.trim())
        .map((path) => {
          const p = path.trim();
          const absolute = posix.isAbsolute(p) || /^[A-Za-z]:[\\/]/.test(p);
          const uri = absolute
            ? vscode.Uri.file(p).with({ scheme: folder.uri.scheme, authority: folder.uri.authority })
            : vscode.Uri.joinPath(folder.uri, p);
          const label = folders.length > 1 && !absolute ? `${folder.name}/${p}` : p;
          return { uri, mappings, file: { key: `${folder.uri.toString()}|${p}`, label, path: p } };
        });
    });
  }

  private async readEnv(s: EnvSource): Promise<EnvConnection[]> {
    let bytes: Uint8Array;
    try {
      if ((await vscode.workspace.fs.stat(s.uri)).size > MAX_ENV_BYTES) {
        this.options.log.warn(`${s.file.label} is larger than 1 MiB; not reading it.`);
        return [];
      }
      bytes = await vscode.workspace.fs.readFile(s.uri);
    } catch {
      return []; // a listed env file that doesn't exist is normal
    }
    const { vars, errors } = parseEnv(new TextDecoder().decode(bytes));
    for (const e of errors) this.options.log.warn(`${s.file.label}:${e.line}: ${e.message}`);
    return envConnections(vars, s.file, s.mappings);
  }

  private async envConnection(conn: Connection): Promise<EnvConnection & { profile: ProfileJson }> {
    const source = this.envById.get(conn.id);
    if (!vscode.workspace.isTrusted || !source) throw new Error(`“${conn.name}” is no longer available.`);
    const fresh = (await this.readEnv(source)).find((c) => c.id === conn.id);
    if (!fresh) throw new Error(`${conn.variable ?? conn.name} is no longer set in ${source.file.label}.`);
    if (!fresh.profile) throw new Error(fresh.error ?? `“${conn.name}” can't be used.`);
    return fresh as EnvConnection & { profile: ProfileJson };
  }

  private watch(sources: EnvSource[]): void {
    for (const w of this.watchers) w.dispose();
    this.watchers = sources.map((s) => {
      const pattern = new vscode.RelativePattern(vscode.Uri.joinPath(s.uri, '..'), posix.basename(s.uri.path));
      const watcher = vscode.workspace.createFileSystemWatcher(pattern);
      const later = () => {
        clearTimeout(this.debounce);
        this.debounce = setTimeout(() => void this.refresh(), 200);
      };
      watcher.onDidCreate(later);
      watcher.onDidChange(later);
      watcher.onDidDelete(later);
      return watcher;
    });
  }

  private applyVerdicts(list: Connection[]): Connection[] {
    return list.map((c) => {
      if (!c.profile || c.invalid) return c;
      const verdict = this.verdicts.get(JSON.stringify(c.profile));
      return verdict ? { ...c, invalid: verdict } : c;
    });
  }

  private async validateAll(generation: number): Promise<void> {
    const validate = this.options.validate;
    if (!validate || this.options.canValidate?.() === false) return;
    let changed = false;
    for (const c of this.connections) {
      if (!c.profile || c.invalid) continue;
      const key = JSON.stringify(c.profile);
      if (this.verdicts.has(key)) continue;
      try {
        const verdict = await validate(c.profile);
        this.verdicts.set(key, verdict);
        if (verdict !== undefined) {
          this.options.log.warn(`Connection “${c.name}”: ${verdict}`);
          changed = true;
        }
      } catch (e) {
        // No engine (yet): list without the engine's verdicts and try again on the next refresh.
        this.options.log.warn(`Couldn't validate connections: ${e instanceof Error ? e.message : String(e)}`);
        break;
      }
      if (generation !== this.generation) return;
    }
    if (!changed || generation !== this.generation) return;
    this.connections = this.applyVerdicts(this.connections);
    this.changed.fire();
  }
}

function parseSecret(raw: string | undefined): SavedSecret | undefined {
  if (!raw) return undefined;
  try {
    const s = JSON.parse(raw) as Partial<SavedSecret>;
    return typeof s.fingerprint === 'string' && typeof s.password === 'string' ? (s as SavedSecret) : undefined;
  } catch {
    return undefined;
  }
}
