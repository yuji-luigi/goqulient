// Shared webview panel setup: scripts on, local resources limited to dist/webview, nonce'd CSP, typed messages.
import * as vscode from 'vscode';
import type { EngineClient } from '../engine/client.ts';
import { nonce, shell } from './html.ts';

/** What the views need from the engine (the sidebar passes the `EngineClient`). */
export type Engine = Pick<EngineClient, 'request'>;

/**
 * No `retainContextWhenHidden`: a hidden grid or diagram would keep its whole DOM alive. The extension keeps the last
 * page / model / positions instead and sends them again when the webview comes back (`ready`).
 */
export function createPanel(
  viewType: string,
  title: string,
  extensionUri: vscode.Uri,
  entry: 'table' | 'er',
): vscode.WebviewPanel {
  const root = vscode.Uri.joinPath(extensionUri, 'dist', 'webview');
  const panel = vscode.window.createWebviewPanel(viewType, title, vscode.ViewColumn.Active, {
    enableScripts: true,
    localResourceRoots: [root],
    enableFindWidget: false,
  });
  const w = panel.webview;
  panel.webview.html = shell({
    cspSource: w.cspSource,
    nonce: nonce(),
    scriptUri: w.asWebviewUri(vscode.Uri.joinPath(root, `${entry}.js`)).toString(),
    styleUri: w.asWebviewUri(vscode.Uri.joinPath(root, `${entry}.css`)).toString(),
    title,
  });
  return panel;
}

/** `postMessage` typed to one view's protocol; a disposed panel drops the message instead of throwing. */
export function poster<T>(panel: vscode.WebviewPanel): (message: T) => void {
  let disposed = false;
  panel.onDidDispose(() => (disposed = true));
  return (message) => {
    if (!disposed) void panel.webview.postMessage(message);
  };
}

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
