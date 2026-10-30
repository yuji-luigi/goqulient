// Connection commands: add (to user settings, password into SecretStorage), forget saved passwords (the database's and
// an SSH tunnel's), open settings.
import { randomUUID } from 'node:crypto';
import * as vscode from 'vscode';
import { UrlError, pastedUrl, profileFromUrl } from './envProfiles.ts';
import type { Connection } from './model.ts';
import { DEFAULT_PORT, DEFAULT_USER, KINDS, KIND_LABEL, endpointSummary, needsPassword, type Kind, type ProfileJson } from './profile.ts';
import { usesSsh } from './ssh.ts';
import type { ConnectionStore } from './store.ts';

const TITLE = 'Add GoQulient Connection';

export function registerConnectionCommands(context: vscode.ExtensionContext, store: ConnectionStore): vscode.Disposable[] {
  return [
    vscode.commands.registerCommand('goqulient.addConnection', () => addConnection(store)),
    vscode.commands.registerCommand('goqulient.forgetPassword', (arg?: Connection | string | { conn: Connection }) =>
      forgetPassword(store, arg && typeof arg === 'object' && 'conn' in arg ? arg.conn : arg),
    ),
    vscode.commands.registerCommand('goqulient.openSettings', () =>
      vscode.commands.executeCommand('workbench.action.openSettings', `@ext:${context.extension.id}`),
    ),
  ];
}

function input(step: string, options: vscode.InputBoxOptions): Thenable<string | undefined> {
  return vscode.window.showInputBox({ title: `${TITLE} (${step})`, ignoreFocusOut: true, ...options });
}

type KindItem = vscode.QuickPickItem & { db?: Kind; url?: string };

/** The first step: a database kind, "From a connection URL…", or a URL pasted straight into the filter box. */
function pickKind(): Promise<KindItem | undefined> {
  const items: KindItem[] = [
    ...KINDS.map((k) => ({ label: KIND_LABEL[k], db: k })),
    { label: 'From a connection URL…', detail: 'postgres://, mysql://, mongodb://, mongodb+srv://' },
  ];
  const qp = vscode.window.createQuickPick<KindItem>();
  qp.title = TITLE;
  qp.placeholder = 'Pick a database, or paste a connection URL';
  qp.ignoreFocusOut = true;
  qp.items = items;
  return new Promise((resolve) => {
    let picked: KindItem | undefined;
    qp.onDidChangeValue((v) => {
      const p = pastedUrl(v);
      // alwaysShow: a URL never matches the filter. The item never repeats the URL, so the password isn't shown twice.
      qp.items = p
        ? [{ label: p.ok ? '$(plug) Use this connection URL' : '$(warning) Not a usable connection URL', detail: p.detail, url: p.url, alwaysShow: true }]
        : items;
    });
    qp.onDidAccept(() => {
      picked = qp.activeItems[0];
      if (picked) qp.hide();
    });
    qp.onDidHide(() => {
      qp.dispose();
      resolve(picked);
    });
    qp.show();
  });
}

async function addConnection(store: ConnectionStore): Promise<void> {
  const choice = await pickKind();
  if (!choice) return;
  let profile: ProfileJson;
  let password: string | undefined;
  if (choice.url !== undefined && pastedUrl(choice.url)?.ok) {
    const parsed = profileFromUrl(choice.url);
    profile = parsed.profile;
    password = parsed.credentials.password;
  } else if (choice.db) {
    const kind = choice.db;
    const host = await input('host', { prompt: 'Host', value: 'localhost', validateInput: (v) => (v.trim() ? undefined : 'Required.') });
    if (host === undefined) return;
    const port = await input('port', {
      prompt: 'Port',
      value: String(DEFAULT_PORT[kind]),
      validateInput: (v) => (/^\d+$/.test(v.trim()) && +v >= 1 && +v <= 65535 ? undefined : 'A number between 1 and 65535.'),
    });
    if (port === undefined) return;
    const user = await input('user', {
      prompt: kind === 'mongodb' ? 'User (leave empty without authentication)' : 'User',
      value: DEFAULT_USER[kind],
      validateInput: (v) => (kind === 'mongodb' || v.trim() ? undefined : 'Required.'),
    });
    if (user === undefined) return;
    const database = await input('database', { prompt: 'Database (optional)' });
    if (database === undefined) return;
    profile = { name: '', kind, hosts: [{ host: host.trim(), port: Number(port) }], user: user.trim(), database: database.trim() };
  } else {
    const url = await input('URL', {
      prompt: 'Connection URL. A password in it goes to VS Code’s secret storage, not to settings.',
      password: true,
      value: choice.url, // a pasted URL that didn't parse: show why, and let it be fixed
      validateInput: (v) => {
        try {
          profileFromUrl(v);
          return undefined;
        } catch (e) {
          return e instanceof UrlError ? e.message : 'Not a connection URL.';
        }
      },
    });
    if (url === undefined) return;
    const parsed = profileFromUrl(url);
    profile = parsed.profile;
    password = parsed.credentials.password;
  }
  const where = profile.mode === 'uri' ? 'MongoDB' : endpointSummary(profile);
  const name = await input('name', {
    prompt: 'Name',
    value: profile.database ? `${profile.database} on ${where}` : where,
    validateInput: (v) => (v.trim() ? undefined : 'Required.'),
  });
  if (name === undefined) return;
  profile = { ...profile, id: randomUUID().toUpperCase(), name: name.trim() };
  try {
    await store.addToUserSettings(profile);
  } catch (e) {
    void vscode.window.showErrorMessage(e instanceof Error ? e.message : String(e));
    return;
  }
  if (password === undefined && needsPassword(profile)) {
    password = await input('password', { prompt: 'Password (Esc to be asked when connecting)', password: true });
  }
  if (password !== undefined) await store.savePassword(profile.id!, profile, password);
  const action = await vscode.window.showInformationMessage(`Added “${profile.name}” to your user settings.`, 'Open Settings');
  if (action) await vscode.commands.executeCommand('goqulient.openSettings');
}

async function forgetPassword(store: ConnectionStore, arg?: Connection | string): Promise<void> {
  let conn = typeof arg === 'string' ? store.get(arg) : arg;
  if (!conn) {
    const candidates = store.list().filter((c) => c.source !== 'env' && c.profile && (needsPassword(c.profile) || usesSsh(c.profile)));
    if (!candidates.length) {
      void vscode.window.showInformationMessage('No connection from settings uses a password.');
      return;
    }
    const pick = await vscode.window.showQuickPick(
      candidates.map((c) => ({ label: c.name, description: c.endpoint, conn: c })),
      { title: 'Forget Saved Password', placeHolder: 'Connection' },
    );
    if (!pick) return;
    conn = pick.conn;
  }
  if (conn.source === 'env') {
    void vscode.window.showInformationMessage(`“${conn.name}” reads its password from ${conn.file}; nothing is saved.`);
    return;
  }
  await store.forgetPassword(conn.id);
  const what = conn.profile && usesSsh(conn.profile) ? 'passwords and SSH secrets' : 'password';
  void vscode.window.showInformationMessage(`Forgot the saved ${what} for “${conn.name}”.`);
}
