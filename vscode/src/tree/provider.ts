// The "GoQulient" sidebar (VSX-11): source → connection → database → schema (PostgreSQL) → kind group → object → columns.
// Connecting is lazy (first expand), catalog lists are cached per session until Refresh, and node objects are kept
// stable so `onDidChangeTreeData` can refresh one node (VS Code matches elements by identity).
import * as vscode from 'vscode';
import type { Connection } from '../connections/model.ts';
import type { ConnectionStore } from '../connections/store.ts';
import { EngineError } from '../engine/client.ts';
import type { Attribute, ObjectRef } from '../views/protocol.ts';
import {
  KIND_SINGULAR,
  columnDescription,
  connectionDescription,
  databaseFilter,
  connectionTooltip,
  groupObjects,
  isBrowsable,
  sourceGroups,
  type KindGroup,
} from './model.ts';
import type { Sessions, Target } from './sessions.ts';

export const VIEW_ID = 'goqulient.connections';

export type Node =
  | { type: 'source'; key: string; label: string; connections: ConnectionNode[] }
  | ConnectionNode
  | { type: 'database'; conn: ConnectionNode; parent: Node; database: string }
  | { type: 'schema'; conn: ConnectionNode; parent: Node; database: string; schema: string }
  | { type: 'group'; conn: ConnectionNode; parent: Node; database: string; schema: string; group: KindGroup }
  | ObjectNode
  | { type: 'column'; conn: ConnectionNode; parent: Node; ref: ObjectRef; column: Attribute }
  | MessageNode;

export interface ConnectionNode {
  type: 'connection';
  conn: Connection;
  source?: Node;
}

export interface ObjectNode {
  type: 'object';
  conn: ConnectionNode;
  parent: Node;
  ref: ObjectRef;
}

export interface MessageNode {
  type: 'message';
  parent: Node;
  tone: 'loading' | 'error' | 'empty';
  text: string;
}

type CatalogNode = Exclude<Node, { type: 'source' | 'message' | 'column' }>;

export function target(c: Connection): Target {
  return { id: c.id, name: c.name, key: JSON.stringify(c.profile ?? null) };
}

export class ConnectionTree implements vscode.TreeDataProvider<Node>, vscode.Disposable {
  private readonly store: ConnectionStore;
  private readonly sessions: Sessions;
  private readonly changed = new vscode.EventEmitter<Node | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private roots: Node[] = [];
  private nodes = new Map<string, ConnectionNode>();
  /** Catalog answers by node path (connection id + epoch first), so Refresh can drop a node and everything below. */
  private readonly cache = new Map<string, Promise<unknown>>();
  private readonly disposables: vscode.Disposable[] = [];
  private readonly onFirstShow: () => void;
  /** Per-connection "Show All Databases" choices (`goqulient.showAllDatabases.<id>`). */
  private readonly state: vscode.Memento;
  private shown = false;

  constructor(store: ConnectionStore, sessions: Sessions, state: vscode.Memento, onFirstShow: () => void) {
    this.store = store;
    this.sessions = sessions;
    this.state = state;
    this.onFirstShow = onFirstShow;
    this.rebuild();
    const off = sessions.onDidChange((id, collapsed) => this.sessionChanged(id, collapsed));
    this.disposables.push(
      this.changed,
      store.onDidChange(() => this.reload()),
      { dispose: off },
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('goqulient.showAllDatabases')) this.changed.fire(undefined);
      }),
    );
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }

  connection(id: string): ConnectionNode | undefined {
    return this.nodes.get(id);
  }

  /** Re-reads settings and env files, drops every cached list and redraws. */
  async refreshAll(): Promise<void> {
    this.cache.clear();
    await this.store.refresh();
    this.changed.fire(undefined);
  }

  /** The only database `conn` shows, or undefined when it lists every database on the server. */
  databaseFilter(conn: Connection): string | undefined {
    const setting = vscode.workspace.getConfiguration('goqulient').get<boolean>('showAllDatabases') === true;
    return databaseFilter(conn.profile?.database, this.state.get<boolean>(showAllKey(conn.id)), setting);
  }

  /** Remembers "Show All Databases" for this connection (in the workspace) and redraws it. */
  async setShowAllDatabases(node: ConnectionNode, showAll: boolean): Promise<void> {
    await this.state.update(showAllKey(node.conn.id), showAll);
    this.changed.fire(node);
  }

  /** Drops the cached lists of `node` and everything below it, then redraws it (a failed connection tries again). */
  refresh(node: Node): void {
    if (node.type === 'source') return this.changed.fire(undefined);
    if (node.type === 'message') return this.refresh(node.parent);
    if (node.type === 'connection' && this.sessions.state(node.conn.id).status === 'failed') {
      this.sessions.retry(node.conn.id);
      return;
    }
    const prefix = this.path(node);
    for (const k of this.cache.keys()) if (k === prefix || k.startsWith(`${prefix}\u0000`)) this.cache.delete(k);
    if (node.type === 'group') this.cache.delete(this.listKey(node));
    this.changed.fire(node);
  }

  getTreeItem(node: Node): vscode.TreeItem {
    const C = vscode.TreeItemCollapsibleState;
    switch (node.type) {
      case 'source': {
        const item = new vscode.TreeItem(node.label, C.Expanded);
        item.id = `source:${node.key}`;
        item.iconPath = new vscode.ThemeIcon(node.key.startsWith('env:') ? 'file' : 'settings-gear');
        item.contextValue = 'goqulient.source';
        return item;
      }
      case 'connection':
        return this.connectionItem(node);
      case 'database': {
        const item = new vscode.TreeItem(node.database, C.Collapsed);
        item.id = this.path(node);
        item.iconPath = new vscode.ThemeIcon('database');
        item.contextValue = 'goqulient.database';
        item.tooltip = `Database ${node.database}`;
        return item;
      }
      case 'schema': {
        const item = new vscode.TreeItem(node.schema, C.Collapsed);
        item.id = this.path(node);
        item.iconPath = new vscode.ThemeIcon('symbol-namespace');
        item.contextValue = 'goqulient.schema';
        item.tooltip = `Schema ${node.schema} in ${node.database}`;
        return item;
      }
      case 'group': {
        const item = new vscode.TreeItem(node.group.label, C.Collapsed);
        item.id = this.path(node);
        item.description = String(node.group.objects.length);
        item.iconPath = new vscode.ThemeIcon('folder');
        item.contextValue = 'goqulient.group';
        return item;
      }
      case 'object': {
        const { ref } = node;
        const browsable = isBrowsable(ref.kind);
        const item = new vscode.TreeItem(ref.name, browsable ? C.Collapsed : C.None);
        item.id = this.path(node);
        item.iconPath = new vscode.ThemeIcon(OBJECT_ICON[ref.kind] ?? 'symbol-misc');
        item.contextValue = browsable ? 'goqulient.object' : 'goqulient.routine';
        item.tooltip = `${KIND_SINGULAR[ref.kind]} ${ref.schema ? `${ref.schema}.` : ''}${ref.name}`;
        if (browsable) item.command = { command: 'goqulient.tree.activate', title: 'Open', arguments: [node] };
        return item;
      }
      case 'column': {
        const { column } = node;
        const item = new vscode.TreeItem(column.name, C.None);
        item.id = this.path(node);
        item.description = columnDescription(column);
        item.iconPath = new vscode.ThemeIcon(column.isPrimaryKey ? 'key' : column.isForeignKey ? 'references' : 'symbol-field');
        item.contextValue = 'goqulient.column';
        item.tooltip = [column.name, column.type, column.defaultValue && `Default: ${column.defaultValue}`, column.comment]
          .filter(Boolean)
          .join('\n');
        return item;
      }
      case 'message': {
        const item = new vscode.TreeItem(node.text, C.None);
        item.id = `${node.parent.type === 'source' ? node.parent.key : this.path(node.parent as CatalogNode)}\u0000msg`;
        item.tooltip = node.text;
        if (node.tone === 'loading') item.iconPath = new vscode.ThemeIcon('loading~spin');
        if (node.tone === 'error') {
          item.iconPath = new vscode.ThemeIcon('error', new vscode.ThemeColor('errorForeground'));
          item.contextValue = 'goqulient.error';
          item.command = { command: 'goqulient.retry', title: 'Retry', arguments: [node] };
          item.tooltip = `${node.text}\n\nClick to retry.`;
        }
        return item;
      }
    }
  }

  getParent(node: Node): Node | undefined {
    if (node.type === 'source') return undefined;
    if (node.type === 'connection') return node.source;
    return node.parent;
  }

  async getChildren(node?: Node): Promise<Node[]> {
    if (!node) {
      if (!this.shown) {
        this.shown = true;
        this.onFirstShow();
      }
      return this.roots;
    }
    switch (node.type) {
      case 'source':
        return node.connections;
      case 'connection':
        return this.connectionChildren(node);
      case 'message':
      case 'column':
        return [];
      default:
        return this.catalog(node);
    }
  }

  // MARK: - Connections

  private connectionItem(node: ConnectionNode): vscode.TreeItem {
    const c = node.conn;
    const state = this.sessions.state(c.id);
    const C = vscode.TreeItemCollapsibleState;
    const item = new vscode.TreeItem(c.name, c.invalid ? C.None : C.Collapsed);
    item.id = this.path(node);
    const status = state.status === 'connecting' ? 'connecting…' : undefined;
    item.description = connectionDescription(c, status);
    item.tooltip = connectionTooltip(c, state.status === 'connected' ? state.serverInfo : undefined);
    item.iconPath = c.invalid
      ? new vscode.ThemeIcon('warning', new vscode.ThemeColor('problemsWarningIcon.foreground'))
      : state.status === 'connecting'
        ? new vscode.ThemeIcon('loading~spin')
        : state.status === 'connected'
          ? new vscode.ThemeIcon('database', new vscode.ThemeColor('testing.iconPassed'))
          : state.status === 'failed'
            ? new vscode.ThemeIcon('database', new vscode.ThemeColor('errorForeground'))
            : new vscode.ThemeIcon('database');
    // Space-separated tokens, matched with `viewItem =~ /\bconnected\b/` in package.json menus.
    const scope = !c.profile?.database?.trim() ? undefined : this.databaseFilter(c) === undefined ? 'dball' : 'dbfilter';
    item.contextValue = ['goqulient.connection', c.invalid ? 'invalid' : state.status, c.source === 'env' ? 'env' : 'settings', scope]
      .filter(Boolean)
      .join(' ');
    return item;
  }

  private async connectionChildren(node: ConnectionNode): Promise<Node[]> {
    const c = node.conn;
    if (c.invalid) return [];
    const state = this.sessions.state(c.id);
    if (state.status === 'failed') return [{ type: 'message', parent: node, tone: 'error', text: state.message }];
    if (state.status !== 'connected') {
      if (state.status === 'idle') this.startConnect(node);
      return [{ type: 'message', parent: node, tone: 'loading', text: 'Connecting…' }];
    }
    return this.catalog(node);
  }

  private startConnect(node: ConnectionNode): void {
    const done = this.sessions.connect(target(node.conn));
    void vscode.window.withProgress({ location: { viewId: VIEW_ID } }, () => done);
  }

  private sessionChanged(id: string, collapsed: boolean): void {
    const node = this.nodes.get(id);
    if (!node) return;
    for (const k of this.cache.keys()) if (k.startsWith(`${id}\u0000`)) this.cache.delete(k);
    // A new epoch means a new node id; VS Code wants the parent refreshed for that (and shows it collapsed).
    this.changed.fire(collapsed ? node.source : node);
  }

  private reload(): void {
    this.rebuild();
    void this.sessions.sync(this.store.list().map(target));
    this.changed.fire(undefined);
  }

  private rebuild(): void {
    const list = this.store.list();
    const nodes = new Map<string, ConnectionNode>();
    const node = (c: Connection): ConnectionNode => {
      const n = this.nodes.get(c.id) ?? { type: 'connection', conn: c };
      n.conn = c;
      nodes.set(c.id, n);
      return n;
    };
    const groups = sourceGroups(list);
    if (groups.length > 1) {
      this.roots = groups.map((g) => {
        const source: Node = { type: 'source', key: g.key, label: g.label, connections: [] };
        source.connections = g.connections.map((c) => Object.assign(node(c), { source }));
        return source;
      });
    } else {
      this.roots = list.map((c) => Object.assign(node(c), { source: undefined }));
    }
    this.nodes = nodes;
  }

  // MARK: - Catalog

  /**
   * Node path: connection id + epoch, then the catalog path, each level a prefix of the next (Refresh drops a prefix).
   * The epoch makes a reconnected node a fresh one. Objects hang off the database (or schema), not their kind group.
   */
  path(node: CatalogNode | Exclude<Node, { type: 'source' | 'message' }>): string {
    const conn = node.type === 'connection' ? node : node.conn;
    const base = `${conn.conn.id}\u0000${this.sessions.epoch(conn.conn.id)}`;
    const list = (database: string, schema: string) => `${base}\u0000d:${database}${schema ? `\u0000s:${schema}` : ''}`;
    switch (node.type) {
      case 'connection':
        return base;
      case 'database':
        return list(node.database, '');
      case 'schema':
        return list(node.database, node.schema);
      case 'group':
        return `${list(node.database, node.schema)}\u0000g:${node.group.kind}`;
      case 'object':
        return `${list(node.ref.database, node.ref.schema)}\u0000o:${node.ref.kind}:${node.ref.name}`;
      case 'column':
        return `${list(node.ref.database, node.ref.schema)}\u0000o:${node.ref.kind}:${node.ref.name}\u0000c:${node.column.name}`;
    }
  }

  private async catalog(node: CatalogNode): Promise<Node[]> {
    const conn = node.type === 'connection' ? node : node.conn;
    const kind = conn.conn.kind;
    try {
      switch (node.type) {
        case 'connection': {
          const only = this.databaseFilter(conn.conn);
          if (only !== undefined) return [{ type: 'database', conn, parent: node, database: only }];
          const { databases } = await this.cached<{ databases: string[] }>(node, 'catalog/databases', {});
          if (!databases.length) return [{ type: 'message', parent: node, tone: 'empty', text: 'No databases' }];
          return databases.map((database) => ({ type: 'database', conn, parent: node, database }));
        }
        case 'database': {
          if (kind === 'postgres') {
            const { schemas } = await this.cached<{ schemas: string[] }>(node, 'catalog/schemas', { database: node.database });
            if (!schemas.length) return [{ type: 'message', parent: node, tone: 'empty', text: 'No schemas' }];
            return schemas.map((schema) => ({ type: 'schema', conn, parent: node, database: node.database, schema }));
          }
          return this.groups(node, conn, node.database, '');
        }
        case 'schema':
          return this.groups(node, conn, node.database, node.schema);
        case 'group': {
          const { objects } = await this.cached<{ objects: ObjectRef[] }>(this.listKey(node), 'catalog/objects', {
            database: node.database,
            schema: node.schema,
          });
          return objects.filter((o) => o.kind === node.group.kind).map((ref) => ({ type: 'object', conn, parent: node, ref }));
        }
        case 'object': {
          const { columns } = await this.cached<{ columns: Attribute[] }>(node, 'catalog/columns', { object: node.ref });
          if (!columns.length) return [{ type: 'message', parent: node, tone: 'empty', text: 'No columns' }];
          return columns.map((column) => ({ type: 'column', conn, parent: node, ref: node.ref, column }));
        }
      }
    } catch (e) {
      // The engine restarted: the connection went idle (and collapsed) already; nothing to show here.
      if (e instanceof EngineError && e.kind === 'notConnected') return [];
      const message = e instanceof Error ? e.message : String(e);
      this.cache.delete(this.path(node));
      return [{ type: 'message', parent: node, tone: 'error', text: message }];
    }
  }

  private async groups(node: CatalogNode, conn: ConnectionNode, database: string, schema: string): Promise<Node[]> {
    const { objects } = await this.cached<{ objects: ObjectRef[] }>(this.path(node), 'catalog/objects', { database, schema });
    const groups = groupObjects(objects);
    if (!groups.length) return [{ type: 'message', parent: node, tone: 'empty', text: 'Empty' }];
    return groups.map((group) => ({ type: 'group', conn, parent: node, database, schema, group }));
  }

  /** The database's (or schema's) object list, which a kind group shows a slice of. */
  private listKey(node: Extract<Node, { type: 'group' }>): string {
    return this.path(node).replace(/\u0000g:[^\u0000]*$/, '');
  }

  private cached<R>(node: CatalogNode | string, method: string, params: Record<string, unknown>): Promise<R> {
    const key = typeof node === 'string' ? node : this.path(node);
    const id = key.slice(0, key.indexOf('\u0000'));
    let p = this.cache.get(key) as Promise<R> | undefined;
    if (!p) {
      p = this.sessions.request<R>(id, method, params);
      this.cache.set(key, p);
      p.catch(() => this.cache.get(key) === p && this.cache.delete(key));
    }
    return p;
  }
}

function showAllKey(id: string): string {
  return `goqulient.showAllDatabases.${id}`;
}

const OBJECT_ICON: Record<string, string> = {
  table: 'table',
  view: 'eye',
  materializedView: 'layers',
  collection: 'json',
  function: 'symbol-method',
  sequence: 'symbol-number',
};
