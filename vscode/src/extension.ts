import * as vscode from 'vscode';
import { registerConnectionCommands } from './connections/commands.ts';
import { SSH_SECRET_LABEL, sshEndpoint, trustPrompt, withHostKey } from './connections/ssh.ts';
import { ConnectionStore } from './connections/store.ts';
import { resolveBinary } from './engine/binary.ts';
import { EngineClient, EngineError } from './engine/client.ts';
import { registerTreeCommands } from './tree/commands.ts';
import { ConnectionTree, VIEW_ID, type Node } from './tree/provider.ts';
import { Sessions, type ConnectHooks } from './tree/sessions.ts';
import type { Engine } from './views/index.ts';

let engine: EngineClient | undefined;

/** What the end-to-end test (`test-e2e/`) reaches into; only returned when VS Code runs the extension's tests. */
export interface TestApi {
  tree: ConnectionTree;
  view: vscode.TreeView<Node>;
  sessions: Sessions;
  store: ConnectionStore;
  engine: EngineClient;
  /** Methods the webviews' engine requests answered, in order (`table/page`, `schema/introspect`, `er/layout`). */
  answered: string[];
}

export function activate(context: vscode.ExtensionContext): TestApi | undefined {
  const log = vscode.window.createOutputChannel('GoQulient', { log: true });
  // Development and test hosts may point at an engine build through the environment (`scripts/dev.sh vscode run`, the
  // e2e test); an installed extension only takes the machine-scope setting.
  const devEngine = context.extensionMode !== vscode.ExtensionMode.Production ? process.env.GOKU_ENGINE_PATH : undefined;
  engine = new EngineClient({
    binary: () =>
      resolveBinary({
        extensionPath: context.extensionPath,
        override: vscode.workspace.getConfiguration('goqulient').get<string>('enginePath') || devEngine,
      }),
    clientName: `${vscode.env.appName} ${vscode.version}`,
    log,
    onGaveUp: (reason) => void vscode.window.showErrorMessage(reason, 'Show Log').then((a) => a && log.show()),
  });
  const client = engine;
  const store = new ConnectionStore({
    secrets: context.secrets,
    log,
    // Listing connections must not start the engine; validation runs once something else started it.
    canValidate: () => client.isRunning,
    validate: async (profile) => {
      try {
        await client.request('profile/validate', { profile });
        return undefined;
      } catch (e) {
        if (e instanceof EngineError && e.kind === 'invalidConfiguration') return e.message;
        throw e;
      }
    },
  });
  const sessions = new Sessions(client, connectHooks(store));
  const tree = new ConnectionTree(store, sessions, context.workspaceState, () =>
    // The tree was shown: start the engine now, so expanding a connection doesn't wait for it.
    void client.info().catch((e) => log.warn(`Couldn't start the engine: ${e instanceof Error ? e.message : String(e)}`)),
  );
  const view = vscode.window.createTreeView<Node>(VIEW_ID, { treeDataProvider: tree, showCollapseAll: true });
  const answered: string[] = [];
  const testing = context.extensionMode === vscode.ExtensionMode.Test;
  const views: Engine = {
    request: async (method, params, token) => {
      const r = await client.request(method, params, token);
      if (testing) answered.push(method);
      return r as never;
    },
  };
  const offStart = client.onDidStart(() => void store.revalidate());
  context.subscriptions.push(
    log,
    store,
    tree,
    view,
    { dispose: offStart },
    { dispose: () => sessions.dispose() },
    ...registerConnectionCommands(context, store),
    ...registerTreeCommands({ context, tree, view, sessions, engine: views }),
    vscode.commands.registerCommand('goqulient.showLog', () => log.show()),
    vscode.commands.registerCommand('goqulient.engineInfo', () =>
      report(log, async () => {
        const info = await client.info();
        void vscode.window.showInformationMessage(`GoQulient engine ${info.engineVersion} (protocol ${info.protocolVersion})`);
      }),
    ),
    vscode.commands.registerCommand('goqulient.restartEngine', () =>
      report(log, async () => {
        const info = await client.restart();
        void vscode.window.showInformationMessage(`GoQulient engine ${info.engineVersion} restarted.`);
      }),
    ),
  );
  return testing ? { tree, view, sessions, store, engine: client, answered } : undefined;
}

/**
 * Credentials for `connection/open`, one more prompt when the server rejects a password from settings, and for SSH
 * tunnels the host-key trust prompt and the SSH password / key passphrase prompts.
 */
function connectHooks(store: ConnectionStore): ConnectHooks {
  const connection = (id: string) => {
    const conn = store.get(id);
    if (!conn) throw new Error('This connection no longer exists.');
    return conn;
  };
  return {
    async params(target) {
      const conn = connection(target.id);
      if (conn.source === 'env') {
        const p = await store.openParams(conn);
        return p && { ...p, source: 'none' };
      }
      if (!conn.profile || conn.invalid) throw new Error(conn.invalid ?? `“${conn.name}” can't be used.`);
      const c = await store.credentialsWithSource(conn);
      return c && { profile: conn.profile, credentials: c.credentials, source: c.source, sources: c.sources };
    },
    async rejected(target, _message, source) {
      const conn = connection(target.id);
      if (source === 'saved') {
        const action = await vscode.window.showWarningMessage(
          `The server rejected the saved password for “${conn.name}”.`,
          'Enter Password',
          'Forget Saved Password',
        );
        if (!action) return undefined;
        if (action === 'Forget Saved Password') await store.forgetPassword(conn.id);
      } else {
        store.forgetSessionPassword(conn.id);
      }
      return store.promptPassword(conn, `The server rejected the password for ${conn.endpoint}. Enter it again.`);
    },
    async sshSecret(target, secret, _message, source) {
      const conn = connection(target.id);
      if (conn.source === 'env' || !conn.profile) return undefined;
      const label = SSH_SECRET_LABEL[secret];
      if (source === 'saved') {
        const action = await vscode.window.showWarningMessage(
          `The saved ${label.toLowerCase()} for “${conn.name}” didn’t work.`,
          `Enter ${label}`,
          `Forget Saved ${label}`,
        );
        if (!action) return undefined;
        if (action.startsWith('Forget')) await store.forgetPassword(conn.id, secret);
      } else if (source) {
        store.forgetSessionSecret(conn.id, secret);
      }
      const prompt =
        secret === 'sshKeyPassphrase'
          ? source
            ? 'Wrong passphrase. Enter the passphrase of the SSH private key again.'
            : 'The SSH private key is encrypted. Enter its passphrase.'
          : `The SSH server rejected the password for ${sshEndpoint(conn.profile)}. Enter it again.`;
      return store.promptSecret(conn, secret, prompt);
    },
    async trustHostKey(target, info) {
      const conn = connection(target.id);
      if (!conn.profile || (conn.source !== 'user' && conn.source !== 'workspace')) return undefined;
      const { message, detail } = trustPrompt(conn.name, info);
      const yes = 'Trust and Connect';
      if ((await vscode.window.showWarningMessage(message, { modal: true, detail }, yes)) !== yes) return undefined;
      return { profile: withHostKey(conn.profile, info.hostKey), save: () => store.trustHostKey(conn, info.hostKey) };
    },
  };
}

export async function deactivate(): Promise<void> {
  await engine?.stop();
  engine = undefined;
}

/** Runs a command body; errors go to the log and a notification (engine messages are already redacted). */
async function report(log: vscode.LogOutputChannel, body: () => Promise<void>): Promise<void> {
  try {
    await body();
  } catch (e) {
    if (e instanceof EngineError && e.cancelled) return;
    const message = e instanceof Error ? e.message : String(e);
    log.error(message);
    void vscode.window.showErrorMessage(message);
  }
}
