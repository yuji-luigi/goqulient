// The sidebar's pure parts (VSX-11): kind groups in the app's order, labels, ER layout keys, source groups and the
// wrong-password check. No `vscode` import, so the Node tests cover them.
import type { Connection, Source } from '../connections/model.ts';
import type { Attribute, ObjectKind, ObjectRef } from '../views/protocol.ts';

/** `DBObjectKind.allCases` order, as the app's sidebar groups objects. */
export const KIND_ORDER: readonly ObjectKind[] = ['table', 'view', 'materializedView', 'collection', 'function', 'sequence'];

export const KIND_GROUP: Record<ObjectKind, string> = {
  table: 'Tables',
  view: 'Views',
  materializedView: 'Materialized Views',
  collection: 'Collections',
  function: 'Functions',
  sequence: 'Sequences',
};

export const KIND_SINGULAR: Record<ObjectKind, string> = {
  table: 'Table',
  view: 'View',
  materializedView: 'Materialized View',
  collection: 'Collection',
  function: 'Function',
  sequence: 'Sequence',
};

/** Kinds the table viewer can open (functions and sequences have no rows to page through). */
export function isBrowsable(kind: ObjectKind): boolean {
  return kind === 'table' || kind === 'view' || kind === 'materializedView' || kind === 'collection';
}

export interface KindGroup {
  kind: ObjectKind;
  label: string;
  objects: ObjectRef[];
}

/** Non-empty groups in the app's order; objects keep the engine's order. Unknown kinds are left out. */
export function groupObjects(objects: readonly ObjectRef[]): KindGroup[] {
  return KIND_ORDER.flatMap((kind) => {
    const items = objects.filter((o) => o.kind === kind);
    return items.length ? [{ kind, label: KIND_GROUP[kind], objects: items }] : [];
  });
}

/** The Memento key the ER panel saves positions under: one per connection, database and schema selection. */
export function erLayoutKey(connectionId: string, database: string, schemas: readonly string[]): string {
  return `goqulient.er:${connectionId}:${database}:${[...schemas].join(',')}`;
}

/**
 * The server rejected the user name or password (so asking again can help). Matches the drivers' wording: PostgreSQL
 * 28P01, MySQL 1045 (only "using password", not 1044 "to database"), MongoDB's SCRAM/auth failures.
 */
export function isAuthFailure(message: string): boolean {
  return /password authentication failed|access denied for user[^\n]*\(using password|authentication failed|SCRAM failure/i.test(message);
}

export interface SourceGroup {
  key: string;
  label: string;
  connections: Connection[];
}

const SOURCE_LABEL: Record<Exclude<Source, 'env'>, string> = { user: 'User Settings', workspace: 'Workspace Settings' };

/** User settings, workspace settings, then one group per env file, in list order. Empty groups are left out. */
export function sourceGroups(connections: readonly Connection[]): SourceGroup[] {
  const groups = new Map<string, SourceGroup>();
  for (const c of connections) {
    const key = c.source === 'env' ? `env:${c.file ?? ''}` : c.source;
    const label = c.source === 'env' ? (c.file ?? '.env') : SOURCE_LABEL[c.source];
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { key, label, connections: [] }));
    g.connections.push(c);
  }
  return [...groups.values()];
}

/** The connection row's description: endpoint (already redacted), read-only badge, or why it can't be used. */
export function connectionDescription(c: Connection, status?: string): string {
  if (c.invalid) return c.invalid;
  return [status, c.readOnly ? 'read-only' : undefined, c.endpoint || undefined].filter(Boolean).join(' · ');
}

/** Plain-text tooltip lines for a connection; no secrets (the endpoint is redacted, profiles hold no password). */
export function connectionTooltip(c: Connection, server?: { product: string; version: string; currentUser: string }): string {
  const where = c.source === 'env' ? `${c.variable ?? ''} in ${c.file ?? '.env'}` : SOURCE_LABEL[c.source];
  return [
    c.name,
    c.endpoint && `Endpoint: ${c.endpoint}`,
    c.profile?.database && `Database: ${c.profile.database}`,
    `From: ${where}`,
    c.readOnly ? 'Read-only' : 'Writable',
    server && `Server: ${server.product} ${server.version} as ${server.currentUser}`,
    c.invalid && `Can't be used: ${c.invalid}`,
  ]
    .filter(Boolean)
    .join('\n');
}

/** "integer · PK · NOT NULL", the way the app's column rows read. */
export function columnDescription(c: Attribute): string {
  return [
    c.type,
    c.isPrimaryKey ? 'PK' : undefined,
    c.isForeignKey ? 'FK' : undefined,
    c.isUnique && !c.isPrimaryKey ? 'unique' : undefined,
    c.nullable ? undefined : 'NOT NULL',
    c.presence !== undefined && c.presence < 1 ? `${Math.round(c.presence * 100)}%` : undefined,
  ]
    .filter(Boolean)
    .join(' · ');
}
