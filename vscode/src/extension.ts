import * as vscode from 'vscode';
import { registerConnectionCommands } from './connections/commands.ts';
import { ConnectionStore } from './connections/store.ts';
import { resolveBinary } from './engine/binary.ts';
import { EngineClient, EngineError } from './engine/client.ts';

let engine: EngineClient | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel('Goku', { log: true });
  engine = new EngineClient({
    binary: () =>
      resolveBinary({
        extensionPath: context.extensionPath,
        override: vscode.workspace.getConfiguration('goku').get<string>('enginePath'),
      }),
    clientName: `${vscode.env.appName} ${vscode.version}`,
    log,
    onGaveUp: (reason) => void vscode.window.showErrorMessage(reason, 'Show Log').then((a) => a && log.show()),
  });
  const client = engine;
  const store = new ConnectionStore({
    secrets: context.secrets,
    log,
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
  context.subscriptions.push(
    log,
    store,
    ...registerConnectionCommands(context, store),
    vscode.commands.registerCommand('goku.showLog', () => log.show()),
    vscode.commands.registerCommand('goku.engineInfo', () =>
      report(log, async () => {
        const info = await client.info();
        void vscode.window.showInformationMessage(`Goku engine ${info.engineVersion} (protocol ${info.protocolVersion})`);
      }),
    ),
    vscode.commands.registerCommand('goku.restartEngine', () =>
      report(log, async () => {
        const info = await client.restart();
        void vscode.window.showInformationMessage(`Goku engine ${info.engineVersion} restarted.`);
      }),
    ),
  );
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
