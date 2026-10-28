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
| MySQL/MariaDB driver (`mysql_async` + rustls/ring): connect, host selection, TLS modes, `editor`/`meta` connections, `KILL QUERY`; catalog + introspection; cell formatting (`MySQLValueFormatter` port) | `engine/crates/goku-engine/src/drivers/mysql/{mod,catalog,format}.rs`, `tests/mysql.rs` |
| Framing (LSP `Content-Length`, 16 MiB cap) | `engine/crates/goku-engine-server/src/framing.rs` |
| Dispatch, error codes, methods | `engine/crates/goku-engine-server/src/server.rs` |
| Protocol tests over an in-memory pipe | `engine/crates/goku-engine-server/src/tests.rs` |
| Extension entry, commands | `vscode/src/extension.ts` |
| `EngineClient` (spawn, initialize, requests, cancel, stop, crash budget), `EngineError` | `vscode/src/engine/client.ts` |
| Engine binary lookup (`bin/<platform>-<arch>/`, `goku.enginePath`) | `vscode/src/engine/binary.ts` |
| Profile JSON types, `endpointSummary`, `splitUrl`, `needsPassword`, `secretFingerprint` | `vscode/src/connections/profile.ts` |
| dotenv parser (no expansion) | `vscode/src/connections/envParse.ts` |
| URL → profile + credentials, `.env` discovery, `goku.envConnections` | `vscode/src/connections/envProfiles.ts` |
| Settings decoding (invalid entries kept), merge, trust filter, duplicate ids | `vscode/src/connections/model.ts` |
| `ConnectionStore`: settings + `.env`, watcher, trust, SecretStorage, password prompt, engine validation | `vscode/src/connections/store.ts` |
| `goku.addConnection`, `goku.forgetPassword`, `goku.openSettings` | `vscode/src/connections/commands.ts` |
| URI redaction (same rules as `redact::uri`) | `vscode/src/connections/redact.ts` |
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

### MySQL driver
- Never let a `mysql_async::Conn` drop with a result pending: its `Drop` drains the rest of the result in a background
  task with no deadline. A connection that timed out, was cancelled or stopped at the row cap goes through `discard`
  (bounded `COM_QUIT`) after `KILL QUERY` from a second connection.
- Keep the statement cache (default size): with `stmt_cache_size(0)` mysql_async never closes the statements that
  `exec_iter` prepares, and they pile up on the server until `max_prepared_stmt_count`.
- `connect` installs rustls' ring provider as the process default (mysql_async uses `ClientConfig::builder()`, which
  panics when several providers are compiled in and none is installed).
- Without a CA file the verify modes trust `webpki-roots` (Mozilla's list), not the OS store as the app does.

## Connections in VS Code
`ConnectionStore` (`store.ts`) builds the list the tree shows; the merge itself is the pure `mergeConnections` in
`model.ts` (unit-tested), fed from `inspect('goku.connections')`, the env files and `workspace.isTrusted`.
- **Settings** `goku.connections`: array of profiles, same JSON as `ConnectionProfile` (`name`, `kind`, `hosts`, `user`,
  `database`, `ssl`, `ssh`, `readOnly`, ...). User **and** workspace entries are both listed (user first), not one
  overriding the other.
  - Decoded tolerantly without the engine: only `name` and `kind` are checked locally. An entry that fails (not an object,
    no name, unknown kind, or `profile/validate` says no) stays in the list with `invalid` = the reason; nothing is
    dropped, and `goku.addConnection` appends to the user list without touching the existing entries.
  - `id`: the profile's `id`, else a hash of scope + name + kind. A repeated id keeps the first entry and marks the others
    invalid, so a workspace entry can't borrow a user entry's saved password.
  - Validation through the engine runs in the background after each refresh (cached by profile JSON). If the engine can't
    start, the list stays unvalidated and a warning goes to the log.
- **`.env` discovery** `goku.envFiles` (resource scope, default `[".env"]`, relative to each workspace folder; multi-root
  labels are `<folder>/<path>`):
  - URL variables: `DATABASE_URL`, `POSTGRES_URL`, `POSTGRESQL_URL`, `MYSQL_URL`, `MONGODB_URI`, `MONGO_URL`,
    `MONGODB_URL`. A `DATABASE_URL` with another scheme (sqlite, redis, ...) is skipped silently; the others are listed
    invalid with the reason.
  - Part variables: `PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD`/`PGDATABASE`/`PGSSLMODE` (host and port lists like libpq) and
    `MYSQL_HOST`/`MYSQL_PORT`/`MYSQL_USER`/`MYSQL_PASSWORD`/`MYSQL_DATABASE`; needs a host or a database.
  - `goku.envConnections` `[{url: "VAR", name?, file?, kind?, readOnly?}]` adds variables; an entry for a well-known
    variable replaces its default name/read-only. `file` limits it to one env file; `kind` is for schemes that don't say.
  - Schemes: `postgres(ql)[+driver]://` (`sslmode`, `sslrootcert`/`sslcert`/`sslkey`, `target_session_attrs`,
    `connect_timeout`, multi-host, `host`/`user`/`password`/`dbname` query params), `mysql`/`mariadb`/`mysql2[+driver]://`
    (`ssl-mode`, `ssl`), `mongodb://` and `mongodb+srv://` (both **URI mode**, options kept as written). sslmode mapping:
    disable → `disable`, allow/prefer → `prefer`, require → `require`, verify-ca → `verifyCA`, verify-full/verify-identity
    → `verifyFull`, unknown → `verifyFull` (same fallback as the engine).
  - Name `<VAR> (<file>)` (`PG* (<file>)`, `MYSQL_* (<file>)` for parts); id `env-` + sha256(folder URI | path, variable),
    so it survives restarts and doesn't change with the value.
  - Read-only by default (D8), `goku.envConnections[].readOnly: false` overrides it.
  - A watcher on each env file and a configuration listener refresh the list (`onDidChange`). The file is parsed again at
    connect time (`openParams`), so the password is never kept or stored.
  - Parser (`envParse.ts`): `export`, `#` lines, ` #` comments after unquoted values (a `#` without whitespace before it is
    part of the value), single quotes and backticks literal, double quotes with `\n \r \t \" \\`, quoted values may span
    lines, **no** `$VAR` expansion. Errors carry line numbers only. Files over 1 MiB aren't read.
- **Passwords** (D7, AGENTS.md rule 5): profiles never hold one. A URL's password moves to `credentials`, which only
  `connection/open` receives. For Mongo URI mode the password is cut out of the URI (`mongodb://user@host/...`) and sent as
  `credentials.password`, percent-decoded; the engine must put it back into `scheme://user@host`, percent-encoded, like
  `MongoSession.connectionString` (⚠ the Rust Mongo driver, VSX-8, doesn't exist yet: it must do this). Settings
  connections keep it in SecretStorage under `goku.password.<id>` as `{fingerprint, password}`; the fingerprint
  (`secretFingerprint`: kind, servers, user) must still match, otherwise the user is asked again, so a changed host or a
  copied id never receives a saved password. The prompt (`showInputBox({password: true})`) offers "save" or "until VS Code
  closes" (kept in memory only). `goku.forgetPassword` removes both.
- **Workspace Trust**: untrusted → workspace `goku.connections` and every env file are ignored (user settings still
  apply); `goku.connections`, `goku.envFiles`, `goku.envConnections` are `restrictedConfigurations`.
  `onDidGrantWorkspaceTrust` refreshes.
- **Logs** never contain a password or a full URI: endpoints go through `redactUri` (`redact.ts`, the engine's URI rules,
  tests mirror `redact.rs`), errors name the variable and file, never the value.
- **Goku app connections** (macOS, later, M3): import through `goku` (`connections` plus the agent-access rules), never by
  reading the app's Keychain directly.
- ⚠ Not handled yet: SSH passwords/passphrases are never prompted (SSH is M2); a `tlsCertificateKeyFilePassword` in a Mongo
  URI stays in the URI (redacted when shown); `PG*`/`MYSQL_*` part connections can't be renamed or made writable through
  `goku.envConnections`; Unix-socket hosts are rejected.

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
| VSX-7 | MySQL/MariaDB: same scope as VSX-6 | M1 | VSX-3 | done (no SSH) |
| VSX-8 | MongoDB: connect (single/multi-host/SRV/URI), catalog, documents page, schema inference | M1 | VSX-3 | |
| VSX-9 | `er/layout`: port `ERLayout.swift` + parity tests | M1 | VSX-5 | done |
| VSX-10 | Connections: settings, SecretStorage, `.env` discovery, Workspace Trust, read-only default for env | M1 | VSX-4 | done (no tree yet; Mongo URI password injection waits for VSX-8) |
| VSX-11 | Sidebar tree + commands (refresh, connect/disconnect, open table, ER diagram) | M1 | VSX-6..8, VSX-10 | |
| VSX-12 | Table/collection viewer webview (virtualised, paged, copy cells) | M1 | VSX-11 | |
| VSX-13 | ER diagram webview: SVG, pan/zoom, drag, inspector, saved positions (`workspaceState`), PNG/SVG export | M1 | VSX-9, VSX-11 | |
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
