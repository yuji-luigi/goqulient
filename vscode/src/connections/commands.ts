// Connection commands: add (to user settings, password into SecretStorage), forget a saved password, open settings.
import { randomUUID } from 'node:crypto';
import * as vscode from 'vscode';
import { UrlError, profileFromUrl } from './envProfiles.ts';
import type { Connection } from './model.ts';
import { DEFAULT_PORT, DEFAULT_USER, KINDS, KIND_LABEL, endpointSummary, needsPassword, type Kind, type ProfileJson } from './profile.ts';
import type { ConnectionStore } from './store.ts';

const TITLE = 'Add Goku Connection';

export function registerConnectionCommands(context: vscode.ExtensionContext, store: ConnectionStore): vscode.Disposable[] {
  return [
    vscode.commands.registerCommand('goku.addConnection', () => addConnection(store)),
    vscode.commands.registerCommand('goku.forgetPassword', (arg?: Connection | string) => forgetPassword(store, arg)),
    vscode.commands.registerCommand('goku.openSettings', () =>
      vscode.commands.executeCommand('workbench.action.openSettings', `@ext:${context.extension.id}`),
    ),
  ];
}

function input(step: string, options: vscode.InputBoxOptions): Thenable<string | undefined> {
  return vscode.window.showInputBox({ title: `${TITLE} (${step})`, ignoreFocusOut: true, ...options });
}

async function addConnection(store: ConnectionStore): Promise<void> {
  type KindItem = vscode.QuickPickItem & { db?: Kind };
  const choice = await vscode.window.showQuickPick<KindItem>(
    [...KINDS.map((k) => ({ label: KIND_LABEL[k], db: k })), { label: 'From a connection URL…', detail: 'postgres://, mysql://, mongodb://, mongodb+srv://' }],
    { title: TITLE, placeHolder: 'Database', ignoreFocusOut: true },
  );
  if (!choice) return;
  let profile: ProfileJson;
  let password: string | undefined;
  if (choice.db) {
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
  if (action) await vscode.commands.executeCommand('goku.openSettings');
}

async function forgetPassword(store: ConnectionStore, arg?: Connection | string): Promise<void> {
  let conn = typeof arg === 'string' ? store.get(arg) : arg;
  if (!conn) {
    const candidates = store.list().filter((c) => c.source !== 'env' && c.profile && needsPassword(c.profile));
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
  void vscode.window.showInformationMessage(`Forgot the saved password for “${conn.name}”.`);
}
