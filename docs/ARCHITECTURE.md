# Architecture

GoQulient is a VS Code extension (TypeScript, `vscode/`) that spawns a Rust database engine (`engine/`, a library crate
plus a stdio JSON-RPC server). This page is the map for contributors and coding agents: decisions, the process
architecture, the protocol, invariants, the drivers, the webviews, the sidebar tree and how connections are found.
Testing is in [TESTING.md](TESTING.md), releases in [RELEASING.md](RELEASING.md), the rules every change follows in
[AGENTS.md](../AGENTS.md).

**Where it comes from.** The engine is a Rust port of *GokuCore*, the engine of the Goku macOS app (Swift). Many
comments say "port of GokuCore's `X`" and name the Swift file (`PostgresSession.swift`, `ERLayout.swift`, …): that file
lives in the app's repository, not here. The two engines are kept in step by shared JSON fixtures (`parity/`, see
[TESTING.md](TESTING.md#parity-fixtures)); this repo can be built, tested and changed without the app.

Status: PostgreSQL, MySQL/MariaDB and MongoDB drivers, the sidebar tree, the table viewer, ER diagrams, connections from
settings and `.env`, and an end-to-end test in a real VS Code. No SSH tunnels and no query editor yet (see
[Roadmap](#roadmap)).

## Files
| Concern | File |
|---|---|
| Cargo workspace, pinned toolchain (`rust-toolchain.toml`), `rustfmt.toml` | `engine/` |
| `DbError` (same wording as GokuCore's `DBError`), `display_message()` | `engine/crates/goku-engine/src/error.rs` |
| `with_deadline` (timeout or cancel → `on_timeout` once) | `engine/crates/goku-engine/src/deadline.rs` |
| `RowCollector`, `clip`, `Paging`, limits | `engine/crates/goku-engine/src/limits.rs` |
| ER layout (port of `ERLayout.swift`, checked against `parity/er-layout`) | `engine/crates/goku-engine/src/er.rs`, `tests/parity_er_layout.rs` |
| PostgreSQL driver: connect + host selection + pool + cancel (`mod.rs`), rustls modes (`tls.rs`), `PGValueFormatter` port (`format.rs`), catalog + introspection (`introspect.rs`); integration tests | `engine/crates/goku-engine/src/drivers/postgres/`, `tests/postgres.rs` |
| `secrets` / `uri` redaction (same patterns as `Redaction.swift`) | `engine/crates/goku-engine/src/redact.rs` |
| `ConnectionProfile` + tolerant decoding, `validate`, `decode_profiles`, `Credentials` (masked `Debug`) | `engine/crates/goku-engine/src/profile.rs` |
| MySQL/MariaDB driver (`mysql_async` + rustls/ring): connect, host selection, TLS modes, `editor`/`meta` connections, `KILL QUERY`; catalog + introspection; cell formatting (`MySQLValueFormatter` port) | `engine/crates/goku-engine/src/drivers/mysql/{mod,catalog,format}.rs`, `tests/mysql.rs` |
| MongoDB session: connect (single/multi-host/SRV/URI, TLS, URI password injection), catalog, browse, `execute` (one JSON command), read-only check | `engine/crates/goku-engine/src/drivers/mongo/mod.rs` |
| BSON → shell text and the Mongo grid (port of `BSONFormat.swift`) | `engine/crates/goku-engine/src/drivers/mongo/format.rs` |
| Mongo inference (port of `MongoSchemaInference.swift`) | `engine/crates/goku-engine/src/drivers/mongo/infer.rs` |
| Extended JSON → BSON keeping key order (`parse_document`) | `engine/crates/goku-engine/src/drivers/mongo/extjson.rs` |
| Mongo tests: integration, inference parity, optional Atlas; helpers `support/mongo.rs`, `support/parity.rs` (shared normalization) | `engine/crates/goku-engine/tests/{mongo,parity_mongo_inference,atlas}.rs` |
| Framing (LSP `Content-Length`, 16 MiB cap) | `engine/crates/goku-engine-server/src/framing.rs` |
| Dispatch, error codes, methods | `engine/crates/goku-engine-server/src/server.rs` |
| Protocol tests over an in-memory pipe | `engine/crates/goku-engine-server/src/tests.rs` |
| Extension entry: engine, store, tree, commands; the test API returned in test mode | `vscode/src/extension.ts` |
| `EngineClient` (spawn, initialize, requests, cancel, stop, crash budget, `onDidStart`/`onDidStop`, `isRunning`), `EngineError` | `vscode/src/engine/client.ts` |
| Sidebar `TreeDataProvider`: node types, catalog cache by node path, connection items | `vscode/src/tree/provider.ts` |
| Sessions by connection id: lazy connect, cancel, password retry, engine stop, `sync` | `vscode/src/tree/sessions.ts` |
| Tree commands: open table (double-click/Enter), ER, refresh, disconnect, cancel, retry, copy name | `vscode/src/tree/commands.ts` |
| Pure tree helpers: kind groups in app order, source groups, descriptions, `erLayoutKey`, `isAuthFailure` | `vscode/src/tree/model.ts` |
| Activity bar icon | `vscode/media/activitybar.svg` |
| End-to-end test in a real VS Code (`@vscode/test-electron`): launcher + suite, Docker image (xvfb) | `vscode/test-e2e/{run.mjs,suite.cjs}`, `docker/vscode-e2e/Dockerfile` |
| Engine binary lookup (`bin/<platform>-<arch>/`, `goqulient.enginePath`) | `vscode/src/engine/binary.ts` |
| Profile JSON types, `endpointSummary`, `splitUrl`, `needsPassword`, `secretFingerprint` | `vscode/src/connections/profile.ts` |
| dotenv parser (no expansion) | `vscode/src/connections/envParse.ts` |
| URL → profile + credentials, `.env` discovery, `goqulient.envConnections` | `vscode/src/connections/envProfiles.ts` |
| Settings decoding (invalid entries kept), merge, trust filter, duplicate ids | `vscode/src/connections/model.ts` |
| `ConnectionStore`: settings + `.env`, watcher, trust, SecretStorage, password prompt, engine validation | `vscode/src/connections/store.ts` |
| `goqulient.addConnection`, `goqulient.forgetPassword` (also from a tree node), `goqulient.openSettings` | `vscode/src/connections/commands.ts` |
| URI redaction (same rules as `redact::uri`) | `vscode/src/connections/redact.ts` |
| Webview entry points `openTable` / `openErDiagram` (for the sidebar) | `vscode/src/views/index.ts` |
| Table panel (paging, cancel, copy to clipboard), ER panel (introspect, `er/layout`, saved positions, export) | `vscode/src/views/{tablePanel,erPanel}.ts` |
| Panel setup (CSP, `localResourceRoots`), HTML shell + nonce, saved-position merge | `vscode/src/views/{panel,html,layout}.ts` |
| Message types (both directions) + engine shapes (`QueryResult`, `SchemaModel`) | `vscode/src/views/protocol.ts` |
| Webview code: grid, ER diagram, DOM helpers, styles (`tsc -p webview`, bundled to `dist/webview/`) | `vscode/webview/{table,er,host}.ts`, `*.css` |
| Pure webview helpers (grid virtualisation, TSV; ER geometry, crow's foot, pan/zoom) | `vscode/webview/lib/{grid,er}.ts` |
| Dev harness (fake `acquireVsCodeApi`, fixtures, fake themes) + Playwright screenshots | `vscode/webview-dev/` |
| Parity fixtures (generated by the Goku app's test suite; see [TESTING.md](TESTING.md#parity-fixtures)) | `parity/` + its `README.md` |
| Test databases and tool containers (compose project `goqulient`), seeds | `docker-compose.yml`, `docker/{postgres,mysql,mongo}/init/` |
| Dev commands (everything in Docker) | `scripts/dev.sh` |
| CI (Linux/macOS/Windows build + unit tests; Linux: database integration tests and the e2e) | `.github/workflows/ci.yml` |
| Release: release-please, then the engine per target + one VSIX per target, publish | `.github/workflows/{release,extension-release}.yml`, `release-please-config.json` |

## Goal
A VS Code extension that shows databases the way the Goku app does, on **Windows, macOS and Linux** (including
Remote-SSH, WSL and dev containers):
- connections from VS Code settings or the workspace `.env`;
- sidebar tree: connection → database → schema → tables/views/collections → columns;
- read-only table/collection viewer;
- ER diagrams for PostgreSQL, MySQL/MariaDB and MongoDB (inferred relationships).

Long term the Mac app may move onto the same Rust engine, so both products behave the same (an experiment, not a
commitment).

## Decisions
| # | Decision | Why |
|---|---|---|
| D1 | Engine in **Rust**, UI in TypeScript | Windows is required. Swift's server stack (SwiftNIO, postgres-nio, MongoKitten, Citadel) isn't dependable there; Rust's drivers are. A Rust engine can later be shared with the Mac app. |
| D2 | Engine = **library crate** + thin **stdio server binary** | The extension spawns the server; the Mac app could later link the library through uniffi. Nothing VS Code-specific goes into the library. |
| D3 | One repo: `engine/` (Cargo workspace) and `vscode/` (extension) | The extension ships the engine inside its VSIX, so they are versioned, tested and released together, against the same Docker databases. |
| D4 | The Goku app's safety features are **requirements for the engine**, ported as behaviour, not as code ([AGENTS.md](../AGENTS.md#safety)) | Same product promise: "don't crash the computer", don't hurt the database. |
| D5 | Behaviour parity with GokuCore is checked by **shared JSON fixtures** | Two engines drift unless a test fails when they do. |
| D6 | Protocol: **JSON-RPC 2.0, LSP framing** (`Content-Length`) over stdio, `vscode-jsonrpc` on the TS side | Standard, supports cancellation (`$/cancelRequest`) and progress, and debugging tools exist. |
| D7 | Secrets: VS Code **SecretStorage**; sent to the engine only over stdin, never argv, env, settings or logs | A password that reaches a log or a process list has leaked. |
| D8 | Connections found in `.env` are **read-only by default**, and `.env` is read only in trusted workspaces | A `DATABASE_URL` is often production; Workspace Trust stops an untrusted repo from steering the extension. |
| D9 | Ship **platform-specific VSIXs** (`vsce package --target`): win32-x64, win32-arm64, darwin-x64, darwin-arm64, linux-x64, linux-arm64, plus Open VSX | One binary per target, as rust-analyzer does. |

## Architecture
```
VS Code (extension host, Node)                         goku-engine-server (Rust, one process per window)
┌────────────────────────────────────────┐  stdio      ┌──────────────────────────────────────────────┐
│ ConnectionProvider (settings, .env,    │  JSON-RPC   │ rpc: framing, dispatch, $/cancelRequest      │
│   SecretStorage, Workspace Trust)      │◄───────────►│ goku-engine (library)                        │
│ TreeDataProvider (sidebar)             │             │   session: Postgres / MySQL / Mongo          │
│ EngineClient (spawn, restart, log)     │             │   safety: deadline, RowCollector, read-only, │
│ Webviews: table grid, ER diagram (SVG) │             │           destructive check                  │
└────────────────────────────────────────┘             │   introspect → SchemaModel; Mongo inference  │
                                                       │   er_layout (port of ERLayout.swift)          │
                                                       │   tunnel: SSH (russh, planned)                │
                                                       └──────────────────────────────────────────────┘
```
- One tokio runtime per process (the counterpart of the single `MultiThreadedEventLoopGroup`).
- If the server crashes the extension restarts it (at most 3 times in 3 minutes), reopens the tree and shows the error. A
  crash never takes VS Code down.
- ER sizes: the webview measures box sizes with its own font (`measureText`) and sends them through the extension to
  `er/layout` in the engine. The engine owns the layout, so the app can use the same one later.

### Crates
| Concern | Crate |
|---|---|
| Runtime, timeouts | `tokio` |
| PostgreSQL | `tokio-postgres` (+ `tokio-postgres-rustls`); `CancelToken` for server-side cancel |
| MySQL / MariaDB | `mysql_async` (rustls feature); `KILL QUERY` on a second connection to cancel |
| MongoDB | official `mongodb` crate; `maxTimeMS` |
| TLS | `rustls` + `rustls-platform-verifier` (uses the Windows/macOS certificate store) |
| SSH tunnels (planned) | `russh` (pure Rust, works on Windows) |
| JSON, errors, misc | `serde`, `serde_json`, `thiserror`, `tokio-util` (`CancellationToken`), `fancy-regex` (redaction backreference), `unicode-segmentation` (clip by grapheme like Swift), `uuid` |
| App binding (planned) | `uniffi` |

### Protocol (`protocolVersion` 1)
Implemented: everything in the table except `query/run` (planned), plus `debug/wait` in debug builds only (tests). Session
methods live in `goku-engine-server/src/sessions.rs`; an unknown `sessionId` is `NotConnected` (`kind: "notConnected"`),
which the tree treats as "the engine restarted" and reconnects.

| Method | Result |
|---|---|
| `initialize {protocolVersion, clientName}` | `{protocolVersion, engineVersion, capabilities: {methods}}`; a different version → `-32003` |
| `profile/validate {profile}` | `{profile (normalized, defaults filled), endpoint (redacted)}`, no network |
| `connection/open {profile, credentials}` → `{sessionId, serverInfo, endpoint}` | `profile` = the JSON shape of `ConnectionProfile`; a failing first query closes the session |
| `connection/close {sessionId}` | |
| `catalog/databases`, `catalog/schemas`, `catalog/objects`, `catalog/columns` | same lists as the app's sidebar |
| `table/page {sessionId, object, offset, limit, sort?}` | rows, capped; truncated cells flagged |
| `schema/introspect {sessionId, database, schemas?, sampleSize?}` | `SchemaModel` JSON |
| `er/layout {entities, sizes, edges?, options?}` | `{rects: {id: {origin: {x, y}, size: {width, height}}}}`; same numbers as `ERLayout.layout` |
| `query/run` (planned) | results; destructive → `needsConfirmation` |
| `$/cancelRequest` | cancels on the server (`pg_cancel_backend`, `KILL QUERY`, kill the Mongo op) |

Errors: `-32001` engine error (`message` = the `DbError` display message with secrets removed, `data` = `{kind, detail}`,
e.g. `kind: "timeout"`), `-32800` cancelled, `-32002` before `initialize`, `-32003` protocol mismatch, plus the standard
`-32700/-32600/-32601/-32602`. The extension sees them as `EngineError` (`code`, `kind`, `cancelled`).

### Invariants (don't break)
- stdout carries only protocol frames; logs go to stderr (the extension prints them in the "GoQulient" output channel).
- The library never creates a runtime or prints; the server builds exactly one multi-thread runtime.
- Every request runs in its own task with a `CancellationToken`; `$/cancelRequest` and the end of stdin cancel it. When
  stdin ends the server exits within ~2 s even if a request is stuck, so it never outlives the extension host.
- Requests before `initialize` and after `shutdown` are refused. A broken frame ends the server (the stream can't resync).
- `goqulient.enginePath` is machine scope (a workspace can't point it at another program); the engine is spawned without a shell
  and without secrets in argv or env.
- `ConnectionProfile` decoding mirrors the Goku app exactly (AGENTS.md, "Profiles"); a new field needs a default in
  `from_fields` and a test, here and in the app.

## PostgreSQL driver (gotchas)
- A request that times out or is cancelled sends `CancelToken::cancel_query` and then **drops its connection** (the
  next request opens a fresh one). A cancel targets the backend, not the statement, so reusing the connection could
  let a late cancel kill the next statement. Each connection serves one request at a time (`Conn::lock`).
- `execute` cut at the row cap also cancels and drops the connection, so the server stops streaming the rest.
  `browse` asks for `LIMIT n+1` instead: the extra row only sets `truncated` (GokuCore's browse never sets it).
- Cells are decoded from the binary protocol by the `PGValueFormatter` port, so timestamps are shown in UTC like the app
  (not in the session time zone). Floats follow Swift's `description` (`1e+20`, `1.0` for float4, `1` for float8).
- TLS: prefer/require don't verify; verifyCA/verifyFull use `rustls-platform-verifier` (the OS store), or only the
  `caCertificatePath` file when set. Unlike GokuCore, verifyFull against an IP address checks the IP SAN.
- The Docker `pg-primary` has no TLS; `tests/postgres.rs` checks the verifying modes only when `PG_TLS_HOST` and
  `PG_TLS_CA` point at a TLS server whose certificate names that host.
- SSH tunnels aren't implemented: a profile with `ssh.enabled` fails with `Unsupported`.

## MySQL driver (gotchas)
- Never let a `mysql_async::Conn` drop with a result pending: its `Drop` drains the rest of the result in a background
  task with no deadline. A connection that timed out, was cancelled or stopped at the row cap goes through `discard`
  (bounded `COM_QUIT`) after `KILL QUERY` from a second connection.
- Keep the statement cache (default size): with `stmt_cache_size(0)` mysql_async never closes the statements that
  `exec_iter` prepares, and they pile up on the server until `max_prepared_stmt_count`.
- `connect` installs rustls' ring provider as the process default (mysql_async uses `ClientConfig::builder()`, which
  panics when several providers are compiled in and none is installed).
- Without a CA file the verify modes trust `webpki-roots` (Mozilla's list), not the OS store as the app and the
  PostgreSQL driver (`rustls-platform-verifier`) do. ⚠ Unify: a corporate CA in the OS store works for PG but not MySQL.

## MongoDB driver (gotchas)
Official `mongodb` crate 3.9 with `bson-3`, rustls on **ring** (no aws-lc-rs, no OpenSSL), the hickory resolver for SRV and
`cert-key-password` (so `tlsCertificateKeyFilePassword` works in URIs). Differences from GokuCore and gotchas:
- **Timeouts**: the whole connect (SRV/TXT lookup, server selection, TLS, auth, `ping`) runs under one
  `connect_timeout_seconds` deadline; server selection gives up at 90 % of it so its error (which says why) usually wins.
  Operations: `query + 5` s, `maxTimeMS = query` on find/aggregate/count/distinct. A timed-out or cancelled operation is
  dropped: the driver's pool closes a connection checked in mid-command, and `maxTimeMS` stops the server (no `killOp`).
- **TLS**: `prefer` = no TLS (SRV: TLS stays on), `require` = no verification, `verifyCA` = **full verification** (rustls
  can't skip only the host name check), client certificate and key must be **one PEM file** (the driver takes one path).
  A URI keeps its own TLS parameters.
- **URI mode**: the URI decides user and password. The stored password is injected percent-encoded only when the URI has
  a user and no password (`inject_password`, like `MongoSession.connectionString`); no user in the URI = no auth (the
  profile's `user` is ignored, as in the app). **SRV mode** puts the user (never the password) into the SRV URI, because
  the driver applies the TXT record's `authSource` (Atlas: `admin`) only to a credential carried by the URI.
- **Read-only**: client check only, in `execute` (allowlist = `MongoCall.readCommands`; `$out`/`$merge` are writes). Any
  read-only rule for the database blocks writes there (per-collection targets come with the shell parser).
- `execute` takes one Extended JSON command document (`{"find": "c", "filter": {}}`); a cursor reply shows its first
  batch (capped) and kills the server cursor. `parse_document` keeps key order; serde_json's `Map` would sort the keys.
- `browse`: page size capped by `maxRows`; `truncated` is set when more documents exist (GokuCore's `browse` never sets it).
- **Inference determinism**: a collection whose `count` is ≤ 100 is read whole with `find` in natural order (the same
  documents `$sample` would return, in a stable order), so attribute order and value-probe samples don't change between
  runs. Larger collections still use `$sample` (random, as in the app). Both parity fixtures pass, three runs each.
- SSH tunnels: `Unsupported` for now.

## Webviews
Two `WebviewPanel`s, opened by the sidebar through `vscode/src/views/index.ts`. Plain TypeScript + DOM, no framework.
```
extension host (src/views/*Panel.ts)            webview (webview/*.ts → dist/webview/*.js)
  engine.request(…, CancellationToken)  ◄──────  {type:'ready'|'load'|'sizes'|'moved'|…}
  keeps last page / model / positions   ──────►  {type:'init'|'loading'|'page'|'model'|'layout'|'error'|…}
```
- **Table** (`openTable(engine, sessionId, object, readOnly, {extensionUri, pageSize})`, title `name — database`, one
  panel per session + object): `ready` → `table/page {offset 0, limit pageSize}`. The webview asks for pages with
  `load {offset, sort?}` (header click cycles asc → desc → none and restarts at offset 0; Prev/Next by `pageSize`, Next
  only when `truncated`). Every request has its own `CancellationTokenSource`: a newer `load`, the Cancel button
  (`cancel`) or closing the panel cancels it, and answers carry `requestId` so a stale one is dropped. Grid: rows and
  columns virtualised (fixed 22 px rows, cells absolutely placed inside the scrolling body; ~370 cells in the DOM for
  1,000 × 50), header and row numbers follow the scroll by transform, column widths measured on a 200-row sample, drag to
  resize. NULL is a dim italic `NULL`; a value ending in the engine's clip marker gets a marker badge and a tooltip.
  Selection (click, shift, drag, row numbers, corner, arrows/Page/Home/End, ⌘/Ctrl+A); ⌘/Ctrl+C and a context menu copy
  as TSV (`copy {text}` → `vscode.env.clipboard`; one cell = raw value, NULL = empty, RFC 4180 quoting as the app's
  `RowCopy`). MongoDB pages with `documents` get a Grid / Documents toggle.
- **ER** (`openErDiagram(engine, sessionId, database, schemas, layoutKey, workspaceState, {extensionUri, title?})`):
  `schema/introspect` under a cancellable progress notification (+ Cancel in the webview) → `model` (capped at 1,500
  entities with a warning, like the app) → the webview measures box sizes with the editor font (`entitySize`, the app's
  `recomputeSizes` formula) → `sizes` → `er/layout` **only for entities without a saved position** → positions. Saved
  positions (`{version: 1, positions: {id: {x, y}}}` in the Memento under `layoutKey`, decoded tolerantly) win; new
  entities keep their fresh relative layout and move as one block below the saved ones (`mergeLayout`), so they never
  overlap a box placed by hand (the app's ⚠). Drag end sends `moved`; Auto Layout re-lays everything out and saves.
  Rendering mirrors `ERPainter`: header colour from the schema hash, PK/FK/UQ badges, bold = NOT NULL, italic =
  presence < 1, Béziers anchored at the first FK column's row, crow's foot at the child, `||`/`o|` at the parent,
  inferred dashed. Only boxes and curves intersecting the viewport (+120 px) are in the DOM; below 40 % rows become bars
  and below 18 % only the box remains, so 300 tables stay at one frame per vsync in the harness. Hover or selection
  dims everything but the entity and its neighbours; search (⌘/Ctrl+F, Enter = next) dims non-matches. The inspector
  shows columns, indexes, relationships (click to jump), or a relationship's FK details; warnings and a legend when
  nothing is selected. Export SVG/PNG renders the whole diagram in light colours (PNG ≤ 2×, ≤ 16,000 px) and sends it
  as `export {format, data}`; the extension asks `showSaveDialog` and writes the file.
- Invariants:
  - CSP `default-src 'none'`, scripts only with the per-load nonce, styles from `cspSource`, images also `blob:`/`data:`
    (PNG export); no network. Code never sets `style` attributes or `innerHTML` (CSSOM and `textContent` only).
  - `localResourceRoots` = `dist/webview`. The HTML shell carries no data (no secrets, no rows): everything arrives by
    `postMessage` and only engine output is shown; errors are the engine's redacted `message`.
  - No `retainContextWhenHidden`: the panel keeps the last page / model / sizes / positions and re-sends them on
    `ready`; the webview keeps view mode, scroll, pan/zoom and selection in `setState`.
  - Colours only from `--vscode-*` variables (light, dark, high contrast); export uses a fixed light palette.
  - `webview/lib/*` and `src/views/{protocol,html,layout}.ts` stay free of `vscode` and DOM (Node tests import them).
- Dev harness: `webview-dev/{table,er}.html` load the bundles with `fake-host.js` (a fake `acquireVsCodeApi` answering
  from a synthetic 2,500 × 50 table, a Mongo collection, `parity/schema/*.json` or 300 synthetic tables, with a stub
  layout; the real layout comes from the engine) and `themes.css` (`?theme=dark|light|hc`), under the same CSP with
  `'self'` for the nonce. Screenshots (gitignored, look at them) and frame timings:
  ```bash
  docker run --rm --ipc=host -v "$PWD":/w -v goqulient-webview-node-modules:/w/vscode/node_modules -w /w/vscode \
    mcr.microsoft.com/playwright:v1.55.0-noble bash -c "npm ci && npm run build && \
    npm i --no-save playwright@1.55.0 && node webview-dev/snapshot.mjs"   # → webview-dev/snapshots/*.png
  ```
  It fails on page errors and CSP violations.
- ⚠ Known gaps: copies of clipped cells copy the clipped text (no `fullRows` yet); no webview serializer, so panels don't
  survive a window reload; one editor font measurement per model (a font change needs Reload); the dev harness's layout
  is a stub, not `er/layout`. In a real VS Code the e2e test checks that each engine's first page and the ER model +
  `er/layout` arrive (not what is drawn: look at its screenshots). The sidebar passes titles that name the connection
  (see the tree section).

## Sidebar tree
View `goqulient.connections` in the "GoQulient" activity bar container. `ConnectionTree` (`tree/provider.ts`) shows the store's list;
`Sessions` (`tree/sessions.ts`) owns the engine sessions; `tree/commands.ts` registers the node commands.
```
[User Settings | Workspace Settings | <env file>]   only when connections come from more than one source
  connection            database icon; description "connecting… · read-only · <redacted endpoint>" or why it's invalid
    database            the profile's database only, or catalog/databases with "Show All Databases" (see below)
      schema            PostgreSQL only: catalog/schemas
        Tables (n) …    catalog/objects grouped in DBObjectKind.allCases order (app's ObjectGroups), empty kinds hidden
          object        table/view/materialized view/collection: double-click or Enter opens it; functions/sequences don't
            column      catalog/columns: "type · PK · FK · unique · NOT NULL · 60%" (presence for Mongo)
```
- **Connect on first expand**: `getChildren(connection)` starts `Sessions.connect` and returns a "Connecting…" child;
  the node shows `loading~spin`, the view a progress bar, and the context menu (and an inline ✕) **Cancel Connecting**,
  which cancels the `connection/open` request (`$/cancelRequest`, the engine cancels the connect). A failure is a child
  item with the engine's redacted message; clicking it (or Retry/Refresh) tries again.
- **Wrong password** (`isAuthFailure`: PostgreSQL 28P01, MySQL 1045 "using password", Mongo auth/SCRAM): asked once
  more. A saved password first gets a warning with "Enter Password" / "Forget Saved Password"; a session or just-typed
  password is dropped and prompted again. Env connections aren't re-prompted (the file is the source).
- **Node ids** are paths: `<connId>\0<epoch>\0d:<db>\0s:<schema>\0g:<kind>` / `\0o:<kind>:<name>\0c:<column>`, each
  a prefix of its children's. Catalog answers are cached by the same path; Refresh on a node drops its prefix (a kind
  group also drops its database/schema object list, which it re-reads). The **epoch** grows whenever a connection goes
  back to idle (Disconnect, Cancel, engine stop, `NotConnected`): its id changes, so VS Code shows it collapsed and the
  next expand reconnects, without an error.
- **Engine start** is lazy: the root `getChildren` (the view was shown) starts it; listing connections never does
  (`ConnectionStore` validates only while `engine.isRunning`, then again on `onDidStart`). `onDidStop` (crash, Restart
  Engine) makes every connection idle.
- **Store changes** (settings, `.env`, trust) rebuild the roots, keep node objects per id (VS Code matches elements by
  identity for `onDidChangeTreeData(node)`), and `Sessions.sync` closes sessions whose connection is gone or whose
  profile JSON changed.
- **Actions**: Open Table (inline, context menu, double-click = two activations of the item command within 500 ms,
  Enter = keybinding `goqulient.tree.enter` when the view has focus; other nodes keep the list's Enter) → `openTable` with
  `maxRows` (default 1,000) as page size and the connection's read-only flag. ER Diagram (database = all schemas,
  schema = that one) → `openErDiagram` with `erLayoutKey` = `goqulient.er:<connId>:<db>:<schemas,>`. Refresh, Disconnect, Copy
  Name, Forget Saved Password (settings connections). View title: Add Connection, Refresh Connections, Open Settings.
  Welcome content when the list is empty (trusted and untrusted variants).
- `contextValue`: `goqulient.connection <idle|connecting|connected|failed|invalid> <settings|env> [dbfilter|dball]`, `goqulient.database`,
  `goqulient.schema`, `goqulient.group`, `goqulient.object`, `goqulient.routine`, `goqulient.column`, `goqulient.error`; menus match them with
  `viewItem =~ /…/`.
- Secrets: labels, descriptions and tooltips only use names, the redacted endpoint and engine messages (the e2e checks
  every item it sees for the test password).
- **Databases shown**: a profile that names a database shows only that one (no `catalog/databases` call, so it also works
  without the right to list databases); a profile without one lists every database. "Show All Databases" / "Show Only the
  Connection's Database" in the connection's context menu (`contextValue` token `dbfilter` / `dball`) is remembered per
  connection in `workspaceState` (`goqulient.showAllDatabases.<id>`) and wins over the `goqulient.showAllDatabases`
  setting (default false). `databaseFilter` in `tree/model.ts`.
- **Panel titles** name the connection: `categories — shop (DATABASE_URL)`, `shop — ER diagram (DATABASE_URL)`.
  `shortConnectionName` uses the variable for an env connection with its default name, else the connection name;
  `withConnection` cuts the name with `…` to keep the title within 60 characters (never below 12). The panels take it as
  `TableOptions.title` / `ErOptions.title`.
- ⚠ Known gaps: no filter box; no "connected" indicator per database for PostgreSQL's per-database pools; double-click relies on timing.

### Checklists
**Add a tree node type**
1. Add it to `Node` in `tree/provider.ts` with `conn` and `parent` (for `getParent`/`reveal`).
2. Give it a path in `path()` that extends its parent's path (Refresh and the cache depend on prefixes).
3. `getTreeItem`: id = path, codicon, `contextValue` `goqulient.<type>`, tooltip without secrets.
4. Produce it in `catalog()` (cached through `cached(node, method, params)`), with a message node for empty lists.
5. Menus in `package.json` (`view/item/context`, `viewItem` regex) and, if needed, `nameOf`/`connectionOf` in
   `tree/commands.ts`.
6. Unit-test pure parts in `test/tree.test.ts`; extend `test-e2e/suite.cjs` if it needs a database.

**Add a protocol method**
1. Engine: implement it in the library, register it (`Sessions::METHODS` + `dispatch` for session methods, else
   `server.rs` `METHODS`), test it in `goku-engine-server/src/tests.rs` (and the driver tests).
2. Errors: return `DbError` (the message is redacted by `display_message`); honour the `CancellationToken`.
3. Extension: call `engine.request` (views) or `sessions.request(connId, …)` (tree, adds `sessionId` and handles
   `NotConnected`); add the result type to `views/protocol.ts` if a webview uses it.
4. Document it in the Protocol table above.

## Connections in VS Code
`ConnectionStore` (`store.ts`) builds the list the tree shows; the merge itself is the pure `mergeConnections` in
`model.ts` (unit-tested), fed from `inspect('goqulient.connections')`, the env files and `workspace.isTrusted`.
- **Settings** `goqulient.connections`: array of profiles, same JSON as `ConnectionProfile` (`name`, `kind`, `hosts`, `user`,
  `database`, `ssl`, `ssh`, `readOnly`, ...). User **and** workspace entries are both listed (user first), not one
  overriding the other.
  - Decoded tolerantly without the engine: only `name` and `kind` are checked locally. An entry that fails (not an object,
    no name, unknown kind, or `profile/validate` says no) stays in the list with `invalid` = the reason; nothing is
    dropped, and `goqulient.addConnection` appends to the user list without touching the existing entries.
  - `id`: the profile's `id`, else a hash of scope + name + kind. A repeated id keeps the first entry and marks the others
    invalid, so a workspace entry can't borrow a user entry's saved password.
  - Validation through the engine runs in the background after each refresh (cached by profile JSON), but only while the
    engine runs (`canValidate`): listing connections never starts it. When the engine starts, `revalidate()` runs.
- **`.env` discovery** `goqulient.envFiles` (resource scope, default `[".env"]`, relative to each workspace folder; multi-root
  labels are `<folder>/<path>`):
  - URL variables: `DATABASE_URL`, `POSTGRES_URL`, `POSTGRESQL_URL`, `MYSQL_URL`, `MONGODB_URI`, `MONGO_URL`,
    `MONGODB_URL`. A `DATABASE_URL` with another scheme (sqlite, redis, ...) is skipped silently; the others are listed
    invalid with the reason.
  - Part variables: `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE`/`PGSSLMODE` (host and port lists like libpq) and
    `MYSQL_HOST`/`MYSQL_PORT`/`MYSQL_USER`/`MYSQL_PASSWORD`/`MYSQL_DATABASE`; needs a host or a database.
  - Docker image variables: `POSTGRES_USER`/`POSTGRES_PASSWORD`/`POSTGRES_DB` (+ `POSTGRES_HOST`/`POSTGRES_PORT`, else
    localhost:5432) are a separate `POSTGRES_* (<file>)` connection with the image's defaults (user `postgres`, database
    = user); `MYSQL_ROOT_PASSWORD` without `MYSQL_USER` makes the `MYSQL_*` connection connect as root (and is enough on
    its own).
  - `goqulient.envConnections` `[{url: "VAR", name?, file?, kind?, readOnly?}]` adds variables; an entry for a well-known
    variable replaces its default name/read-only. `file` limits it to one env file; `kind` is for schemes that don't say.
  - Schemes: `postgres(ql)[+driver]://` (`sslmode`, `sslrootcert`/`sslcert`/`sslkey`, `target_session_attrs`,
    `connect_timeout`, multi-host, `host`/`user`/`password`/`dbname` query params), `mysql`/`mariadb`/`mysql2[+driver]://`
    (`ssl-mode`, `ssl`), `mongodb://` and `mongodb+srv://` (both **URI mode**, options kept as written). sslmode mapping:
    disable → `disable`, allow/prefer → `prefer`, require → `require`, verify-ca → `verifyCA`, verify-full/verify-identity
    → `verifyFull`, unknown → `verifyFull` (same fallback as the engine).
  - Name `<VAR> (<file>)` (`PG* (<file>)`, `MYSQL_* (<file>)` for parts); id `env-` + sha256(folder URI | path, variable),
    so it survives restarts and doesn't change with the value.
  - Read-only by default (D8), `goqulient.envConnections[].readOnly: false` overrides it.
  - A watcher on each env file and a configuration listener refresh the list (`onDidChange`). The file is parsed again at
    connect time (`openParams`), so the password is never kept or stored.
  - Parser (`envParse.ts`): `export`, `#` lines, ` #` comments after unquoted values (a `#` without whitespace before it is
    part of the value), single quotes and backticks literal, double quotes with `\n \r \t \" \\`, quoted values may span
    lines, **no** `$VAR` expansion. Errors carry line numbers only. Files over 1 MiB aren't read.
- **Passwords** (D7, AGENTS.md "Safety"): profiles never hold one. A URL's password moves to `credentials`, which only
  `connection/open` receives. For Mongo URI mode the password is cut out of the URI (`mongodb://user@host/...`) and sent as
  `credentials.password`, percent-decoded; the engine puts it back into `scheme://user@host`, percent-encoded, like
  `MongoSession.connectionString` (`inject_password` in the Mongo driver). Settings
  connections keep it in SecretStorage under `goqulient.password.<id>` as `{fingerprint, password}`; the fingerprint
  (`secretFingerprint`: kind, servers, user) must still match, otherwise the user is asked again, so a changed host or a
  copied id never receives a saved password. The prompt (`showInputBox({password: true})`) offers "save" or "until VS Code
  closes" (kept in memory only). `goqulient.forgetPassword` removes both.
- **Workspace Trust**: untrusted → workspace `goqulient.connections` and every env file are ignored (user settings still
  apply); `goqulient.connections`, `goqulient.envFiles`, `goqulient.envConnections` are `restrictedConfigurations`.
  `onDidGrantWorkspaceTrust` refreshes.
- **Logs** never contain a password or a full URI: endpoints go through `redactUri` (`redact.ts`, the engine's URI rules,
  tests mirror `redact.rs`), errors name the variable and file, never the value.
- **Goku app connections** (macOS, planned): import through the app's `goku` CLI (`connections` plus its agent-access
  rules), never by reading the app's Keychain directly.
- ⚠ Not handled yet: SSH passwords/passphrases are never prompted (SSH tunnels are planned); a `tlsCertificateKeyFilePassword` in a Mongo
  URI stays in the URI (redacted when shown); `PG*`/`MYSQL_*` part connections can't be renamed or made writable through
  `goqulient.envConnections`; Unix-socket hosts are rejected.

## Safety requirements for the engine
These are the rules in [AGENTS.md](../AGENTS.md#safety), as the engine implements them:
1. Every network wait goes through one `with_deadline(dur, fut, on_timeout)` helper whose `on_timeout` closes the socket
   or cancels the server query. Server-side limits too: `statement_timeout`, `max_execution_time`, `maxTimeMS`.
2. Rows are streamed and capped (default 1,000, hard cap 100,000) and long cells are truncated. Nothing loads a whole
   result into memory.
3. Read-only is enforced by the client check **and** the server session (`default_transaction_read_only`,
   `SET SESSION TRANSACTION READ ONLY`); MongoDB has the client check only, as in the app.
4. Destructive statements need confirmation (the same statement list as the Goku app), once the query editor exists.
5. A grid edit must hit exactly one row or the whole batch rolls back, once grid editing exists.
6. At most 8 pooled connections per PostgreSQL session.
7. Secrets never reach logs; logged statements and URIs are masked.

## Roadmap
| Milestone | Scope |
|---|---|
| Done | Engine + protocol, PostgreSQL, MySQL/MariaDB and MongoDB drivers (no SSH), `er/layout` with parity tests, connections (settings, SecretStorage, `.env`, Workspace Trust), sidebar tree, table viewer, ER diagram, packaging for six targets, end-to-end test |
| Next | SSH tunnels; query editor (SQL + Mongo shell, CodeLens Run, results, destructive confirmation, cancel) |
| Later | Grid editing, filter builder, export CSV/JSON, cluster host selection, import of Goku app connections |
| Experiment | The Mac app on this Rust engine through uniffi |

## Risks / open questions
- MySQL cancel and MariaDB differences in `mysql_async`, and the Mongo shell syntax of the query editor (a port of the
  app's `MongoShellParser`), are the largest porting risks.
- Windows CI can't run Linux service containers: database integration tests run on Linux only.
- Native macOS/Windows engine builds and the arm64 cross builds are only exercised in CI.
- Inherited from GokuCore: Mongo inference on collections over 100 documents uses `$sample`, so it isn't deterministic
  there (see the MongoDB driver above).
- Fixture parity covers outputs, not timing or cancel behaviour. Those need their own Rust tests (`deadline.rs`, the
  driver integration tests).
