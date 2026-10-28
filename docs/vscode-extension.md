# VS Code extension + Rust engine

Status (2026-10-28): **M0 done** (skeleton: engine library + stdio server, extension that runs it, parity fixtures, CI).
No database access yet. This page is both the PM plan (decisions, milestones, tickets) and the agent doc for what exists
(file map, protocol, invariants, commands), like the pages under `docs/features/`.

## Files
| Concern | File |
|---|---|
| Cargo workspace, pinned toolchain (`rust-toolchain.toml`), `rustfmt.toml` | `engine/` |
| `DbError` (same wording as GokuCore's `DBError`), `display_message()` | `engine/crates/goku-engine/src/error.rs` |
| `with_deadline` (timeout or cancel → `on_timeout` once) | `engine/crates/goku-engine/src/deadline.rs` |
| `RowCollector`, `clip`, `Paging`, limits | `engine/crates/goku-engine/src/limits.rs` |
| ER layout (port of `ERLayout.swift`, checked against `Tests/Parity/er-layout`) | `engine/crates/goku-engine/src/er.rs`, `tests/parity_er_layout.rs` |
| `secrets` / `uri` redaction (same patterns as `Redaction.swift`) | `engine/crates/goku-engine/src/redact.rs` |
| `ConnectionProfile` + tolerant decoding, `validate`, `decode_profiles`, `Credentials` (masked `Debug`) | `engine/crates/goku-engine/src/profile.rs` |
| Framing (LSP `Content-Length`, 16 MiB cap) | `engine/crates/goku-engine-server/src/framing.rs` |
| Dispatch, error codes, methods | `engine/crates/goku-engine-server/src/server.rs` |
| Protocol tests over an in-memory pipe | `engine/crates/goku-engine-server/src/tests.rs` |
| Extension entry, commands | `vscode/src/extension.ts` |
| `EngineClient` (spawn, initialize, requests, cancel, stop, crash budget), `EngineError` | `vscode/src/engine/client.ts` |
| Engine binary lookup (`bin/<platform>-<arch>/`, `goku.enginePath`) | `vscode/src/engine/binary.ts` |
| Webview entry points `openTable` / `openErDiagram` (for the sidebar) | `vscode/src/views/index.ts` |
| Table panel (paging, cancel, copy to clipboard), ER panel (introspect, `er/layout`, saved positions, export) | `vscode/src/views/{tablePanel,erPanel}.ts` |
| Panel setup (CSP, `localResourceRoots`), HTML shell + nonce, saved-position merge | `vscode/src/views/{panel,html,layout}.ts` |
| Message types (both directions) + engine shapes (`QueryResult`, `SchemaModel`) | `vscode/src/views/protocol.ts` |
| Webview code: grid, ER diagram, DOM helpers, styles (`tsc -p webview`, bundled to `dist/webview/`) | `vscode/webview/{table,er,host}.ts`, `*.css` |
| Pure webview helpers (grid virtualisation, TSV; ER geometry, crow's foot, pan/zoom) | `vscode/webview/lib/{grid,er}.ts` |
| Dev harness (fake `acquireVsCodeApi`, fixtures, fake themes) + Playwright screenshots | `vscode/webview-dev/` |
| Parity fixtures (written by `Tests/GokuCoreTests/ParityFixtureTests.swift`) | `Tests/Parity/` + its `README.md` |
| CI (Linux/macOS/Windows: fmt, clippy, cargo test, typecheck, build, Node tests against the engine) | `.github/workflows/extension.yml` |
| Release: engine per target + one VSIX per target, publish | `.github/workflows/extension-release.yml`, `release-please-config.json` |

## Commands
```bash
scripts/dev.sh engine test [filter]   # cargo test in the `engine` container (rust:1.99.1, target dir in a volume)
scripts/dev.sh engine check           # rustfmt --check + clippy -D warnings
scripts/dev.sh engine build           # release build (Linux)
scripts/dev.sh vscode test            # builds the engine (debug), then typecheck + Node tests against it in `vscode`
scripts/dev.sh vscode build           # dist/extension.cjs
GOKU_WRITE_PARITY=1 scripts/dev.sh test ParityFixture   # regenerate Tests/Parity (Swift side)
```
Nothing Rust or Node is installed on the Mac; native macOS and Windows builds happen in CI.

## Goal
A VS Code extension that shows databases the way Goku does, on **Windows, macOS and Linux** (including Remote-SSH, WSL
and dev containers):
- connections from VS Code settings or the workspace `.env`;
- sidebar tree: connection → database → schema → tables/views/collections → columns;
- read-only table/collection viewer;
- ER diagrams for PostgreSQL, MySQL/MariaDB and MongoDB (inferred relationships), like the app.

Long term: the Mac app moves onto the same Rust engine so both products behave the same (milestone M4, an experiment, not
a commitment).

## Decisions (PM; the user may override)
| # | Decision | Why |
|---|---|---|
| D1 | Engine in **Rust**, UI in TypeScript | Windows is required. Swift's server stack (SwiftNIO, postgres-nio, MongoKitten, Citadel) isn't dependable there; Rust's drivers are. A Rust engine can later be shared with the Mac app. |
| D2 | Engine = **library crate** + thin **stdio server binary** | The extension spawns the server; the Mac app can later link the library through uniffi. Nothing VS Code-specific goes into the library. |
| D3 | Same repo: `engine/` (Cargo workspace) and `vscode/` (extension) | Shares the Docker databases, the connection format and the parity fixtures; M4 happens here too. |
| D4 | The safety features in AGENTS.md rule 4 are **requirements for the engine**, ported as behaviour, not as code | Same product promise: "don't crash the computer", don't hurt the database. |
| D5 | Behaviour parity with GokuCore is checked by **shared JSON fixtures** | Two engines drift unless a test fails when they do. |
| D6 | Protocol: **JSON-RPC 2.0, LSP framing** (`Content-Length`) over stdio, `vscode-jsonrpc` on the TS side | Standard, supports cancellation (`$/cancelRequest`) and progress, and debugging tools exist. |
| D7 | Secrets: VS Code **SecretStorage**; sent to the engine only over stdin, never argv, env, settings or logs | AGENTS.md rule 5. |
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
                                                       │   tunnel: SSH (russh)                         │
                                                       └──────────────────────────────────────────────┘
```
- One tokio runtime per process (the counterpart of the single `MultiThreadedEventLoopGroup`).
- If the server crashes the extension restarts it (at most 3 times in 3 minutes), reopens the tree and shows the error. A
  crash never takes VS Code down.
- ER sizes: the webview measures box sizes with its own font (`measureText`) and sends them through the extension to
  `er/layout` in the engine. The engine owns the layout, so the app can use the same one later.

### Crates (M0 uses the first and last rows; the rest are planned, to confirm in the driver tickets)
| Concern | Crate |
|---|---|
| Runtime, timeouts | `tokio` |
| PostgreSQL | `tokio-postgres` (+ `tokio-postgres-rustls`); `CancelToken` for server-side cancel |
| MySQL / MariaDB | `mysql_async` (rustls feature); `KILL QUERY` on a second connection to cancel |
| MongoDB | official `mongodb` crate; `maxTimeMS` |
| TLS | `rustls` + `rustls-platform-verifier` (uses the Windows/macOS certificate store) |
| SSH tunnels | `russh` (pure Rust, works on Windows) |
| JSON, errors, misc | `serde`, `serde_json`, `thiserror`, `tokio-util` (`CancellationToken`), `fancy-regex` (redaction backreference), `unicode-segmentation` (clip by grapheme like Swift), `uuid` |
| App binding (M4) | `uniffi` |

### Protocol (`protocolVersion` 1)
Implemented: `initialize`, `shutdown`, `exit`, `$/cancelRequest`, `profile/validate`, `er/layout`, and `debug/wait` in debug builds
only (tests). The rest of the table is planned.

| Method | Result |
|---|---|
| `initialize {protocolVersion, clientName}` | `{protocolVersion, engineVersion, capabilities: {methods}}`; a different version → `-32003` |
| `profile/validate {profile}` | `{profile (normalized, defaults filled), endpoint (redacted)}`, no network |
| `connection/open {profile, secrets}` → `{sessionId, serverInfo}` | `profile` = the JSON shape of `ConnectionProfile` |
| `connection/close {sessionId}` | |
| `catalog/databases`, `catalog/schemas`, `catalog/objects`, `catalog/columns` | same lists as the app's sidebar |
| `table/page {sessionId, object, offset, limit, sort?}` | rows, capped; truncated cells flagged |
| `schema/introspect {sessionId, database, schemas?, sampleSize?}` | `SchemaModel` JSON |
| `er/layout {entities, sizes, edges?, options?}` | `{rects: {id: {origin: {x, y}, size: {width, height}}}}`; same numbers as `ERLayout.layout` |
| `query/run` (M2) | results; destructive → `needsConfirmation` |
| `$/cancelRequest` | cancels on the server (`pg_cancel_backend`, `KILL QUERY`, kill the Mongo op) |

Errors: `-32001` engine error (`message` = the `DbError` display message with secrets removed, `data` = `{kind, detail}`,
e.g. `kind: "timeout"`), `-32800` cancelled, `-32002` before `initialize`, `-32003` protocol mismatch, plus the standard
`-32700/-32600/-32601/-32602`. The extension sees them as `EngineError` (`code`, `kind`, `cancelled`).

### Invariants (don't break)
- stdout carries only protocol frames; logs go to stderr (the extension prints them in the "Goku" output channel).
- The library never creates a runtime or prints; the server builds exactly one multi-thread runtime.
- Every request runs in its own task with a `CancellationToken`; `$/cancelRequest` and the end of stdin cancel it. When
  stdin ends the server exits within ~2 s even if a request is stuck, so it never outlives the extension host.
- Requests before `initialize` and after `shutdown` are refused. A broken frame ends the server (the stream can't resync).
- `goku.enginePath` is machine scope (a workspace can't point it at another program); the engine is spawned without a shell
  and without secrets in argv or env.
- `ConnectionProfile` decoding mirrors Swift exactly (AGENTS.md rule 6); a new field needs a default in `from_fields`
  and a test, on both sides.

## Webviews (VSX-12, VSX-13)
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
  from a synthetic 2,500 × 50 table, a Mongo collection, `Tests/Parity/schema/*.json` or 300 synthetic tables, with a stub
  layout; the real layout comes from the engine) and `themes.css` (`?theme=dark|light|hc`), under the same CSP with
  `'self'` for the nonce. Screenshots (gitignored, look at them) and frame timings:
  ```bash
  docker run --rm --ipc=host -v "$PWD":/w -v goku-node-modules:/w/vscode/node_modules -w /w/vscode \
    mcr.microsoft.com/playwright:v1.55.0-noble bash -c "npm ci && npm run build && \
    npm i --no-save playwright@1.55.0 && node webview-dev/snapshot.mjs"   # → webview-dev/snapshots/*.png
  ```
  It fails on page errors and CSP violations.
- ⚠ Known gaps: copies of clipped cells copy the clipped text (no `fullRows` yet); no webview serializer, so panels don't
  survive a window reload; one editor font measurement per model (a font change needs Reload); the dev harness's layout
  is a stub, not `er/layout`; panels are not tested in a real VS Code yet (VSX-11's `@vscode/test-electron`).

## Connections in VS Code
- **Settings** `goku.connections`: array of profiles (user or workspace scope), same field names as `ConnectionProfile`
  (`kind`, `host`, `port`, `user`, `database`, `ssl`, `ssh`, `readOnly`, ...). Never contains passwords: the first connect
  asks for them and stores them in SecretStorage, keyed by connection id.
- **`.env` discovery** `goku.envFiles` (default `[".env"]`):
  - recognised URL variables: `DATABASE_URL`, `POSTGRES_URL`, `MYSQL_URL`, `MONGODB_URI`, `MONGO_URL`;
  - recognised part variables: `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE`, `MYSQL_HOST`/... ;
  - `goku.envConnections` maps custom variable names.
  The file is read when connecting, in place; nothing is copied. A changed file updates the tree.
- **Goku app connections** (macOS, later, M3): import through `goku` (`connections` plus the agent-access rules), never by
  reading the app's Keychain directly.

## Safety requirements for the engine (from AGENTS.md rule 4 and README "Safety")
1. Every network wait goes through one `with_deadline(dur, fut, on_timeout)` helper whose `on_timeout` closes the socket
   or cancels the server query. Server-side limits too: `statement_timeout`, `max_execution_time`, `maxTimeMS`.
2. Rows are streamed and capped (default 1,000, hard cap 100,000) and long cells are truncated. Nothing loads a whole
   result into memory.
3. Read-only is enforced by the client check **and** the server session (`default_transaction_read_only`,
   `SET SESSION TRANSACTION READ ONLY`); MongoDB has the client check only, as in the app.
4. Destructive statements need confirmation (the same list as `connections-and-drivers.md#read-only-and-destructive-checks`).
5. (M3) A grid edit must hit exactly one row or the whole batch rolls back.
6. At most 8 pooled connections per PostgreSQL session.
7. Secrets never reach logs; logged statements and URIs are masked.

## Testing
- Rust tests run in the **`engine` container** (same network and databases as `dev`; integration tests will be gated by
  `GOKU_IT=1`, set there). Database tests share the Docker databases with the Swift suites (AGENTS.md rule 2): don't run
  them alongside Swift tests, `--selftest` or `--uidemo`.
- Extension tests run with Node's built-in runner on the `.ts` sources (type stripping, `erasableSyntaxOnly`) in the
  `vscode` container, against the engine's Linux debug build (`GOKU_ENGINE_BIN`).
- **Parity fixtures**: a GokuCore test (`ParityFixtureTests.swift`) writes JSON snapshots to `Tests/Parity/` and
  compares against them on every Swift run (shapes and normalization: `Tests/Parity/README.md`):
  - `SchemaModel` per engine for the seeded test schemas;
  - Mongo inference on a fixed document set;
  - `ERLayout` output for fixed inputs.
  Rust tests must produce the same JSON. Changing either side means regenerating the fixtures in the same commit:
  `GOKU_WRITE_PARITY=1 scripts/dev.sh test ParityFixture`.
- CI (`extension.yml`): Linux, macOS and **Windows** each run the Rust tests and the Node tests against their own engine
  build. Database integration tests will run on Linux only (service containers); an extension-host smoke test
  (`@vscode/test-electron`) comes with VSX-11.
- Extension: `@vscode/test-electron` for the tree and commands; webviews are checked by screenshot, the way `--uidemo` is.

## Release
- release-please has a second package `vscode` (release type `node`, tag `vscode-vX.Y.Z`, own PR via
  `separate-pull-requests`); the root package has `exclude-paths: ["engine", "vscode"]` and
  `include-component-in-tag: false`, so app tags stay `vX.Y.Z` and engine/extension commits never bump the app. The
  extension release also bumps `engine/Cargo.toml`'s workspace version (the engine ships inside the VSIX).
- `release.yml` calls `extension-release.yml` (`workflow_call`) when release-please reports `vscode--release_created`; a
  tag pushed with `GITHUB_TOKEN` wouldn't start a workflow by itself. Run `extension-release.yml` by hand to get the six
  VSIXs as an artifact without publishing.
- `extension-release.yml`: engine per target (Linux: static musl via `cargo-zigbuild`, so any distro and Alpine work;
  macOS: arm64 + x64 on one runner; Windows: x64 + arm64 MSVC), the native ones run `--version`; then one
  `vsce package --target` per target with the binary in `bin/<target>/`. The extension restores a lost exec bit before
  spawning.
- Publishing needs the `VSCE_PAT` (Marketplace) and `OVSX_PAT` (Open VSX) secrets and the Marketplace publisher
  `yuji-luigi` (`vscode/package.json`; create it, or change the field, before the first publish). Without the secrets
  the job only warns.
- ⚠ Untested until the branch is pushed: the release workflows (in particular the arm64 cross builds once the drivers
  add TLS crates) and release-please's root-path outputs with two packages. Check the first release PR before merging.
- macOS binaries are not signed or notarized (VS Code extracts the VSIX itself, so no quarantine flag is set).
- Name: **Goku** (user decision 2026-10-28). Marketplace extension ids are permanent, so the extension keeps it even if
  the app is renamed later; settings use the `goku.*` prefix.

## Milestones and tickets
| Ticket | Title | Milestone | Depends on | Status |
|---|---|---|---|---|
| VSX-1 | `engine/` Cargo workspace, CI matrix (Linux/macOS/Windows build + unit tests), Rust in Docker (`engine` service) | M0 skeleton | – | done |
| VSX-2 | Protocol v1 + `goku-engine-server`: framing, dispatch, `initialize`, cancellation, structured errors | M0 | VSX-1 | done |
| VSX-3 | Safety primitives: `with_deadline`, row collector with caps, cell truncation, secret masking, the single runtime | M0 | VSX-1 | done |
| VSX-4 | `vscode/` skeleton: esbuild, `EngineClient` (spawn, restart, output channel), platform binary lookup | M0 | VSX-2 | done |
| VSX-5 | Parity fixtures exported from GokuCore (Swift test in Docker) | M0 | – | done |
| VSX-6 | PostgreSQL: connect (TLS modes), catalog, `table/page`, read-only session, introspection | M1 MVP | VSX-3 | |
| VSX-7 | MySQL/MariaDB: same scope as VSX-6 | M1 | VSX-3 | |
| VSX-8 | MongoDB: connect (single/multi-host/SRV/URI), catalog, documents page, schema inference | M1 | VSX-3 | |
| VSX-9 | `er/layout`: port `ERLayout.swift` + parity tests | M1 | VSX-5 | done |
| VSX-10 | Connections: settings, SecretStorage, `.env` discovery, Workspace Trust, read-only default for env | M1 | VSX-4 | |
| VSX-11 | Sidebar tree + commands (refresh, connect/disconnect, open table, ER diagram) | M1 | VSX-6..8, VSX-10 | |
| VSX-12 | Table/collection viewer webview (virtualised, paged, copy cells) | M1 | VSX-11 | done (webview + `openTable`; wired by VSX-11) |
| VSX-13 | ER diagram webview: SVG, pan/zoom, drag, inspector, saved positions (`workspaceState`), PNG/SVG export | M1 | VSX-9, VSX-11 | done (webview + `openErDiagram`; wired by VSX-11) |
| VSX-14 | Packaging + release: 6 platform VSIXs, Open VSX, release-please package | M1 | VSX-4 | done (untested in CI) |
| VSX-15 | Docs: this page becomes the agent doc; README for the Marketplace | M1 | all M1 | |
| – | SSH tunnels, query editor (SQL + Mongo shell, CodeLens Run, results, destructive confirm, cancel) | M2 | M1 | |
| – | Grid editing, filter builder, export CSV/JSON, cluster host selection, Goku app import | M3 | M2 | |
| – | Mac app on the Rust engine through uniffi, one engine behind `DatabaseSession` at a time (experiment branch) | M4 | M3 | |

Ticket files are written as `docs/tickets/VSX-<n>-<slug>.md` when a workstream starts.

## Risks / open questions
- MySQL cancel and MariaDB differences in `mysql_async`, and the Mongo shell syntax in M2 (port `MongoShellParser`),
  are the largest porting risks.
- Windows CI can't run Linux service containers: database integration tests run on Linux only. Whether Windows also gets
  a smoke test against a database the job installs is decided in VSX-6.
- Native macOS/Windows engine builds are only exercised in CI (no Rust on the Mac); cross builds for the arm64 targets
  come with VSX-14.
- Inherited from GokuCore: Mongo inference isn't deterministic (`$sample` order), see `er-diagrams.md`. The Rust port
  should not copy that blindly; decide in VSX-8 whether both sides sort attributes.
- Fixture parity covers outputs, not timing or cancel behaviour. Those need their own Rust tests, modelled on the Swift
  deadline tests in `UnitTests.swift`/`IntegrationSupport.swift` and "Diagnosing hangs" in `docs/TESTING.md`.
