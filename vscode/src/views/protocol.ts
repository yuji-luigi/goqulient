// Messages between the extension and its webviews, plus the engine shapes they carry (engine/crates/goku-engine/src/
// model.rs). Types only: imported by both sides (the webview with `import type`), so no `vscode` and no DOM here.

export type ObjectKind = 'table' | 'view' | 'materializedView' | 'collection' | 'function' | 'sequence';

export interface ObjectRef {
  database: string;
  /** PostgreSQL schema; empty for MySQL/MongoDB. */
  schema: string;
  name: string;
  kind: ObjectKind;
}

export interface Sort {
  column: string;
  descending: boolean;
}

export interface ResultColumn {
  name: string;
  typeName: string;
}

export interface QueryResult {
  columns: ResultColumn[];
  rows: (string | null)[][];
  /** More rows existed than were returned. */
  truncated: boolean;
  affectedRows?: number;
  /** MongoDB: one shell-style document per row. */
  documents?: string[];
  elapsedMs: number;
}

// `query/run` (engine/crates/goku-engine/src/query/mod.rs). Not used by a webview yet: the query editor builds on it.

export interface QueryRunParams {
  sessionId: string;
  /** One or more statements (SQL, or Mongo shell syntax). */
  text: string;
  /** The database to start in; the connection's when absent. */
  database?: string;
  /** The user agreed to the statements of a previous `needsConfirmation` answer. */
  confirmed?: boolean;
  /** Row cap per statement; the profile's `maxRows` when absent, at most 100,000. */
  maxRows?: number;
}

/** UTF-16 offsets into `QueryRunParams.text`, `end` exclusive (`TextDocument.positionAt` takes them as they are). */
export interface TextSpan {
  start: number;
  end: number;
}

export type StatementKind = 'rows' | 'affected' | 'message' | 'error';

export interface StatementResult {
  kind: StatementKind;
  /** The statement as written (trimmed; SQL keeps its leading comments). */
  statement: string;
  span: TextSpan;
  /** The database it ran in (after `USE` / `use`). */
  database: string;
  columns: ResultColumn[];
  rows: (string | null)[][];
  /** More rows existed than `maxRows`; the server was stopped. */
  truncated: boolean;
  affectedRows?: number;
  /** MongoDB: one shell-style document per row. */
  documents?: string[];
  /** "inserted _id 1", "switched to db x", … */
  message?: string;
  /** Server notices (PostgreSQL `NOTICE` / `WARNING`). */
  notices: string[];
  elapsedMs: number;
  /** Set when `kind` is `error`; nothing after this statement ran. */
  error?: { message: string; kind: string };
}

/** A destructive statement (the Goku app's danger check), shown before anything runs. */
export interface DangerFinding {
  warning: string;
  /** What it does, in words: "drops table", "deletes every row in". */
  action: string;
  /** The table or collection, when it can be told. */
  object?: string;
  database: string;
  statement: string;
  span: TextSpan;
}

export type QueryRunResult =
  | { outcome: 'results'; results: StatementResult[] }
  | { outcome: 'needsConfirmation'; findings: DangerFinding[] };

export interface Attribute {
  name: string;
  type: string;
  nullable: boolean;
  isPrimaryKey: boolean;
  isForeignKey: boolean;
  isUnique: boolean;
  defaultValue?: string;
  comment?: string;
  presence?: number;
}

export interface IndexInfo {
  name: string;
  columns: string[];
  unique: boolean;
  primary: boolean;
}

export interface Entity {
  id: string;
  schema: string;
  name: string;
  kind: ObjectKind;
  attributes: Attribute[];
  indexes: IndexInfo[];
  comment?: string;
  rowEstimate?: number;
}

export interface Relationship {
  id: string;
  name: string;
  fromEntity: string;
  fromColumns: string[];
  toEntity: string;
  toColumns: string[];
  cardinality: 'manyToOne' | 'oneToOne';
  optional: boolean;
  onDelete?: string;
  onUpdate?: string;
  inferred: boolean;
}

export interface SchemaModel {
  database: string;
  engine: string;
  entities: Entity[];
  relationships: Relationship[];
  warnings: string[];
}

export interface Point {
  x: number;
  y: number;
}

export interface Size {
  width: number;
  height: number;
}

/** `er/layout` result entry. */
export interface LayoutRect {
  origin: Point;
  size: Size;
}

// MARK: - Table viewer

export type TableToWebview =
  | { type: 'init'; title: string; object: ObjectRef; readOnly: boolean; pageSize: number }
  | { type: 'loading'; requestId: number; offset: number; sort?: Sort }
  | { type: 'page'; requestId: number; offset: number; sort?: Sort; result: QueryResult }
  /** `message` is the engine's (already redacted) display message. */
  | { type: 'error'; requestId: number; message: string }
  | { type: 'cancelled'; requestId: number };

export type TableFromWebview =
  | { type: 'ready' }
  /** Loads the page at `offset`; cancels whatever is in flight. */
  | { type: 'load'; offset: number; sort?: Sort }
  | { type: 'cancel' }
  | { type: 'copy'; text: string };

// MARK: - ER diagram

export type ErToWebview =
  | { type: 'init'; title: string; database: string; schemas: string[] }
  | { type: 'loading'; message: string }
  | { type: 'error'; message: string }
  /** With `positions` (and `sizes`) the webview renders at once; without, it measures and answers `sizes`. */
  | { type: 'model'; model: SchemaModel; positions?: Record<string, Point>; sizes?: Record<string, Size> }
  | { type: 'layout'; positions: Record<string, Point> }
  | { type: 'exported'; path: string };

export type ErFromWebview =
  | { type: 'ready' }
  | { type: 'sizes'; sizes: Record<string, Size> }
  /** Positions after a drag (top-left, world units); the extension saves them in `workspaceState`. */
  | { type: 'moved'; positions: Record<string, Point> }
  /** Discards saved positions and lays everything out again. */
  | { type: 'autoLayout' }
  | { type: 'reload' }
  | { type: 'cancel' }
  /** `data`: SVG text, or base64 PNG bytes. */
  | { type: 'export'; format: 'svg' | 'png'; data: string };
