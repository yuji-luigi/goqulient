// Sidebar commands (VSX-11): open a table (button, double-click, Enter), ER diagram, refresh, disconnect, cancel
// connecting, retry, copy name. Commands that take a node also work from the palette on the tree's selection.
import * as vscode from 'vscode';
import type { Engine } from '../views/index.ts';
import { openErDiagram, openTable } from '../views/index.ts';
import { erLayoutKey } from './model.ts';
import type { ConnectionNode, ConnectionTree, Node, ObjectNode } from './provider.ts';
import type { Sessions } from './sessions.ts';

const DOUBLE_CLICK_MS = 500;
const DEFAULT_PAGE_SIZE = 1000;

export interface TreeCommandContext {
  context: vscode.ExtensionContext;
  tree: ConnectionTree;
  view: vscode.TreeView<Node>;
  sessions: Sessions;
  engine: Engine;
}

export function registerTreeCommands(c: TreeCommandContext): vscode.Disposable[] {
  const { tree, view, sessions } = c;
  const selected = (node?: Node) => node ?? view.selection[0];
  let lastClick: { path: string; at: number } | undefined;
  return [
    vscode.commands.registerCommand('goqulient.refresh', () => tree.refreshAll()),
    vscode.commands.registerCommand('goqulient.refreshNode', (node?: Node) => {
      const n = selected(node);
      if (n) tree.refresh(n);
      else void tree.refreshAll();
    }),
    vscode.commands.registerCommand('goqulient.retry', (node?: Node) => {
      const n = selected(node);
      if (n) tree.refresh(n);
    }),
    vscode.commands.registerCommand('goqulient.disconnect', async (node?: Node) => {
      const conn = connectionOf(selected(node));
      if (conn) await sessions.disconnect(conn.conn.id);
    }),
    vscode.commands.registerCommand('goqulient.cancelConnecting', (node?: Node) => {
      const conn = connectionOf(selected(node));
      if (conn) sessions.cancel(conn.conn.id);
    }),
    vscode.commands.registerCommand('goqulient.openTable', (node?: Node) => {
      const n = selected(node);
      if (n?.type === 'object') return openObject(c, n);
    }),
    // A tree item's command runs on every click: open on the second click of a double-click, or at once for Enter.
    vscode.commands.registerCommand('goqulient.tree.activate', (node: ObjectNode) => {
      const now = Date.now();
      const path = tree.path(node);
      const double = lastClick?.path === path && now - lastClick.at < DOUBLE_CLICK_MS;
      lastClick = double ? undefined : { path, at: now };
      if (double) return openObject(c, node);
    }),
    vscode.commands.registerCommand('goqulient.tree.enter', () => {
      const n = view.selection[0];
      if (n?.type === 'object') return openObject(c, n);
      return vscode.commands.executeCommand('list.select'); // everything else keeps the list's own Enter (expand)
    }),
    vscode.commands.registerCommand('goqulient.openErDiagram', (node?: Node) => {
      const n = selected(node);
      if (n?.type === 'database') return openEr(c, n.conn, n.database, []);
      if (n?.type === 'schema') return openEr(c, n.conn, n.database, [n.schema]);
    }),
    vscode.commands.registerCommand('goqulient.copyName', async (node?: Node) => {
      const name = nameOf(selected(node));
      if (name !== undefined) await vscode.env.clipboard.writeText(name);
    }),
  ];
}

function connectionOf(node?: Node): ConnectionNode | undefined {
  if (!node || node.type === 'source') return undefined;
  if (node.type === 'message') return connectionOf(node.parent);
  return node.type === 'connection' ? node : node.conn;
}

function nameOf(node?: Node): string | undefined {
  switch (node?.type) {
    case 'connection':
      return node.conn.name;
    case 'database':
      return node.database;
    case 'schema':
      return node.schema;
    case 'object':
      return node.ref.name;
    case 'column':
      return node.column.name;
    case 'message':
      return node.text;
    default:
      return undefined;
  }
}

function openObject(c: TreeCommandContext, node: ObjectNode): vscode.WebviewPanel | undefined {
  const sessionId = c.sessions.sessionId(node.conn.conn.id);
  if (!sessionId) return undefined; // disconnected meanwhile: the tree already shows it collapsed
  const conn = node.conn.conn;
  const maxRows = conn.profile?.maxRows;
  const pageSize = typeof maxRows === 'number' && maxRows >= 1 ? maxRows : DEFAULT_PAGE_SIZE;
  return openTable(c.engine, sessionId, node.ref, conn.readOnly, { extensionUri: c.context.extensionUri, pageSize });
}

function openEr(c: TreeCommandContext, conn: ConnectionNode, database: string, schemas: string[]): vscode.WebviewPanel | undefined {
  const sessionId = c.sessions.sessionId(conn.conn.id);
  if (!sessionId) return undefined;
  const key = erLayoutKey(conn.conn.id, database, schemas);
  return openErDiagram(c.engine, sessionId, database, schemas, key, c.context.workspaceState, {
    extensionUri: c.context.extensionUri,
  });
}
