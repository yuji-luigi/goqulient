# VS Code extension + Rust engine (plan, 2026-10-28)

Status: **planned, nothing built yet.** This page is the PM plan: decisions, architecture, milestones and tickets. Once
code lands it becomes the agent doc for the extension (file map, invariants, checklists), like the pages under
`docs/features/`.

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

### Planned crates (to confirm in VSX-1)
| Concern | Crate |
|---|---|
| Runtime, timeouts | `tokio` |
| PostgreSQL | `tokio-postgres` (+ `tokio-postgres-rustls`); `CancelToken` for server-side cancel |
| MySQL / MariaDB | `mysql_async` (rustls feature); `KILL QUERY` on a second connection to cancel |
| MongoDB | official `mongodb` crate; `maxTimeMS` |
| TLS | `rustls` + `rustls-platform-verifier` (uses the Windows/macOS certificate store) |
| SSH tunnels | `russh` (pure Rust, works on Windows) |
| JSON | `serde`, `serde_json` |
| App binding (M4) | `uniffi` |

### Protocol sketch (`protocolVersion` 1, finalized in VSX-2)
| Method | Result |
|---|---|
| `initialize {protocolVersion, clientName}` | engine version, capabilities |
| `connection/open {profile, secrets}` → `{sessionId, serverInfo}` | `profile` = the JSON shape of `ConnectionProfile` |
| `connection/close {sessionId}` | |
| `catalog/databases`, `catalog/schemas`, `catalog/objects`, `catalog/columns` | same lists as the app's sidebar |
| `table/page {sessionId, object, offset, limit, sort?}` | rows, capped; truncated cells flagged |
| `schema/introspect {sessionId, database, schemas?, sampleSize?}` | `SchemaModel` JSON |
| `er/layout {entities, sizes, edges, options?}` | rects |
| `query/run` (M2) | results; destructive → `needsConfirmation` |
| `$/cancelRequest` | cancels on the server (`pg_cancel_backend`, `KILL QUERY`, kill the Mongo op) |

Errors carry the same `displayMessage` wording as `DBError`.

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
- Rust integration tests run in the **dev container** against the same Docker databases (AGENTS.md rules 1 and 2): add a
  Rust toolchain to `docker/` and an `engine` subcommand to `scripts/dev.sh test`. Don't run them alongside Swift tests,
  `--selftest` or `--uidemo`.
- **Parity fixtures**: a GokuCore test writes JSON snapshots to `Tests/Fixtures/parity/`:
  - `SchemaModel` per engine for the seeded test schemas;
  - Mongo inference on a fixed document set;
  - `ERLayout` output for fixed inputs.
  Rust tests must produce the same JSON. Changing either side means regenerating the fixtures in the same commit.
- CI matrix (GitHub Actions): Linux runs build, unit and integration tests (service containers). macOS and **Windows** run
  build, unit tests and an extension-host smoke test (`@vscode/test-electron`) that spawns the real server.
- Extension: `@vscode/test-electron` for the tree and commands; webviews are checked by screenshot, the way `--uidemo` is.

## Release
- release-please gets a second package `vscode` (release type `node`, tag `vscode-vX.Y.Z`); the root package adds
  `exclude-paths: ["engine", "vscode"]` so engine work doesn't bump the app. The engine's version is the extension's (it
  ships inside the VSIX).
- Publishing needs the `VSCE_PAT` (Marketplace) and `OVSX_PAT` (Open VSX) secrets and a Marketplace publisher id.
- Name: **Goku** (user decision 2026-10-28). Marketplace extension ids are permanent, so the extension keeps it even if
  the app is renamed later; settings use the `goku.*` prefix.

## Milestones and tickets
| Ticket | Title | Milestone | Depends on |
|---|---|---|---|
| VSX-1 | `engine/` Cargo workspace, crate choices confirmed, CI matrix (Linux/macOS/Windows build + unit tests), Rust in the dev container | M0 skeleton | – |
| VSX-2 | Protocol v1 + `goku-engine-server`: framing, dispatch, `initialize`, cancellation, structured errors | M0 | VSX-1 |
| VSX-3 | Safety primitives: `with_deadline`, row collector with caps, cell truncation, secret masking, the single runtime | M0 | VSX-1 |
| VSX-4 | `vscode/` skeleton: esbuild, `EngineClient` (spawn, restart, output channel), platform binary lookup | M0 | VSX-2 |
| VSX-5 | Parity fixtures exported from GokuCore (Swift test in Docker) | M0 | – |
| VSX-6 | PostgreSQL: connect (TLS modes), catalog, `table/page`, read-only session, introspection | M1 MVP | VSX-3 |
| VSX-7 | MySQL/MariaDB: same scope as VSX-6 | M1 | VSX-3 |
| VSX-8 | MongoDB: connect (single/multi-host/SRV/URI), catalog, documents page, schema inference | M1 | VSX-3 |
| VSX-9 | `er/layout`: port `ERLayout.swift` + parity tests | M1 | VSX-5 |
| VSX-10 | Connections: settings, SecretStorage, `.env` discovery, Workspace Trust, read-only default for env | M1 | VSX-4 |
| VSX-11 | Sidebar tree + commands (refresh, connect/disconnect, open table, ER diagram) | M1 | VSX-6..8, VSX-10 |
| VSX-12 | Table/collection viewer webview (virtualised, paged, copy cells) | M1 | VSX-11 |
| VSX-13 | ER diagram webview: SVG, pan/zoom, drag, inspector, saved positions (`workspaceState`), PNG/SVG export | M1 | VSX-9, VSX-11 |
| VSX-14 | Packaging + release: 6 platform VSIXs, Open VSX, release-please package | M1 | VSX-4 |
| VSX-15 | Docs: this page becomes the agent doc; README for the Marketplace | M1 | all M1 |
| – | SSH tunnels, query editor (SQL + Mongo shell, CodeLens Run, results, destructive confirm, cancel) | M2 | M1 |
| – | Grid editing, filter builder, export CSV/JSON, cluster host selection, Goku app import | M3 | M2 |
| – | Mac app on the Rust engine through uniffi, one engine behind `DatabaseSession` at a time (experiment branch) | M4 | M3 |

Ticket files are written as `docs/tickets/VSX-<n>-<slug>.md` when a workstream starts.

## Risks / open questions
- MySQL cancel and MariaDB differences in `mysql_async`, and the Mongo shell syntax in M2 (port `MongoShellParser`),
  are the largest porting risks.
- Windows CI can't run Linux service containers: integration tests run on Linux only. Windows gets a smoke test
  against a database started by the job (to decide in VSX-1).
- Fixture parity covers outputs, not timing or cancel behaviour. Those need their own Rust tests, modelled on the Swift
  deadline tests in `UnitTests.swift`/`IntegrationSupport.swift` and "Diagnosing hangs" in `docs/TESTING.md`.
