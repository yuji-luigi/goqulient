# Testing

Everything runs in Docker: you need Docker (with Compose v2) and nothing else. No Rust or Node on your machine.

## Commands
```bash
scripts/dev.sh up                     # test databases + the `engine` and `vscode` tool containers (waits until healthy)
scripts/dev.sh engine test [filter]   # cargo test: unit, protocol, database integration and parity tests
scripts/dev.sh engine check           # rustfmt --check + clippy -D warnings
scripts/dev.sh engine build           # release build (Linux)
scripts/dev.sh vscode test            # builds the engine (debug), then typecheck + Node tests against it
scripts/dev.sh vscode build           # dist/extension.cjs + dist/webview/
scripts/dev.sh vscode e2e             # the extension in a real VS Code (xvfb) against the test databases (read-only)
GOKU_E2E_SCREENSHOTS=vscode/webview-dev/snapshots scripts/dev.sh vscode e2e   # … and keep window screenshots
scripts/dev.sh vscode run [folder]    # native engine (needs cargo) + this extension in a new VS Code window
scripts/dev.sh vscode package <dir>   # one VSIX per engine binary under <dir> (downloaded from a release run) -> dist/
scripts/dev.sh down                   # remove containers and volumes
scripts/dev.sh env                    # this checkout's compose project, ports and limits (Parallel worktrees)
```
`engine shell` / `vscode shell` open a shell in the tool containers. A pull request needs `engine test`, `engine check`
and `vscode test` to pass; run `vscode e2e` too when you touch the tree, the views or the connection store.

## Docker topology
`docker-compose.yml`, compose project **`goqulient`** (so it never collides with another project's containers or
volumes; a linked git worktree gets a project of its own, see [Parallel worktrees](#parallel-worktrees)). The
databases are throwaway: seeded from `docker/*/init/` on first start, passwords are test-only, and `scripts/dev.sh down`
deletes them.

| Service | Image | Host port (127.0.0.1) | In the network | Credentials |
|---|---|---|---|---|
| `pg-primary` | `postgres:17` | 25432 | `pg-primary:5432` | `dbc` / `dbcpass`, database `shop` (schemas `public`, `inventory`) |
| `mysql-primary` | `mysql:8.4` (self-signed TLS) | 23306 | `mysql-primary:3306` | `dbc` / `dbcpass` (root: `rootpass`), databases `shop`, `analytics` |
| `mongo` | `mongo:7` | 27217 | `mongo:27017` | no auth, database `shop` |
| `ssh-bastion` | `docker/ssh` (Alpine + OpenSSH) | 22222 | `ssh-bastion:22` | `dbc` / `sshpass`; keys in the `ssh-keys` volume |
| `engine` | `rust:1.99.1-bookworm` | – | – | `GOKU_IT=1`, `PG_HOST`, `MYSQL_HOST`, `MONGO_HOST`, `SSH_HOST`, `SSH_KEYS` (`ssh-keys` mounted read-only at `/ssh-keys`) |
| `vscode` | `node:24-bookworm` | – | – | `GOKU_ENGINE_BIN` = the engine's Linux debug build |
| `vscode-e2e` (profile `e2e`) | `docker/vscode-e2e` (Node + xvfb + VS Code's libraries) | – | – | started by `vscode e2e` only |

The bastion runs by default (`up` starts it with the databases; 64 MB, a quarter CPU). At first start it writes test
key pairs into the `ssh-keys` volume (`id_ed25519` without a passphrase, `id_ed25519_enc` and the 3072-bit `id_rsa`
with the passphrase `keypass`, all in `authorized_keys`) and copies its host keys there (`host_{ed25519,ecdsa,rsa}.pub`).
It forwards to the databases by service name, so a native VS Code can try a tunnel too: SSH host `127.0.0.1`, port
22222, user `dbc`, password `sshpass`, database host `pg-primary:5432`.

Build output stays in volumes (`engine-target`, `engine-cargo`, `vscode-node-modules`, `vscode-test`), so a Linux build
never lands in your checkout. The host ports are for trying the extension in your own VS Code against the test
databases, e.g. `DATABASE_URL=postgres://dbc:dbcpass@127.0.0.1:25432/shop` in a scratch folder's `.env`.

## Parallel worktrees
Two git worktrees (say, two coding agents) can run `scripts/dev.sh` at the same time without sharing containers, mounts,
databases or host ports:
- `dev.sh` exports `COMPOSE_PROJECT_NAME`: `goqulient` in the main checkout (and in CI, or a copy without `.git`), the
  worktree's folder name in a linked worktree (`git worktree add ../goqulient-tunnels` → project `goqulient-tunnels`).
  Each project has its own containers, volumes (so the first build in a new worktree is a cold one) and `./` mount.
- A linked worktree publishes the databases and the bastion on ports derived from its name (25432/23306/27217/22222
  plus the same offset, 1–999); `GOKU_PG_PORT`, `GOKU_MYSQL_PORT`, `GOKU_MONGO_PORT` and `GOKU_SSH_PORT` override them. `scripts/dev.sh env` prints the name,
  ports and limits as `export` lines: `eval "$(scripts/dev.sh env)"` before a bare `docker compose` command (such as
  the Atlas one above) so it reaches the same project.
- A linked worktree's `engine` container defaults to 3 GB and 2 CPUs (`GOKU_ENGINE_MEM`, `GOKU_ENGINE_CPUS`; the main
  checkout keeps 5 GB and 4). These are limits, not reservations: a project really uses the engine's build (up to its
  limit) plus about 1.5 GB for the databases and the `vscode` container. With Docker's ~11.7 GB and 6 CPUs on a laptop,
  give **every** project that runs at the same time 3 GB / 2 CPUs, the main checkout included
  (`GOKU_ENGINE_MEM=3g GOKU_ENGINE_CPUS=2 scripts/dev.sh …`), and run at most two at once. Changing a limit recreates
  that container (the build cache is in a volume, so nothing is lost).
- `scripts/dev.sh down` in a worktree removes only that worktree's project. Run it before `git worktree remove`.

## Rust tests (`engine/`)
- Unit tests live next to the code; protocol tests (framing, dispatch, cancellation, error codes) run the server over an
  in-memory pipe (`goku-engine-server/src/tests.rs`).
- **Database integration tests** (`tests/{postgres,mysql,mongo}.rs`, and `tests/query_run.rs` for `query/run` on all
  three) run only with `GOKU_IT=1` (set in the `engine`
  container), so a plain `cargo test` on macOS or Windows skips them. Helpers: `tests/support/`. Scratch objects are named
  `rs_pg_*`, `rs_my_*`, `rs_mg_<random>` and dropped. Every test body runs under a hard deadline (`support::run`).
- The test databases are shared by everything that uses the compose project: don't run two test runs at once in one
  checkout (each worktree has its own project).
- **SSH tunnels** (`tests/tunnel.rs`) go through `ssh-bastion`: unknown and mismatched host keys (the key a client
  would pin is learnt from the `unknownHostKey` error and checked against the bastion's host key files), password and
  key auth (plain and encrypted ed25519, encrypted RSA), wrong password and passphrase, an unreachable target, a
  "bastion" that never sends its banner (a local listener: the connect deadline fires), closing the tunnel, and each
  driver behind it. Each driver test checks on the server that the client address is the bastion's. Unit tests in
  `tunnel.rs` and the protocol tests (an in-process russh server) cover the deadline, cancel, key files and the error
  shape without Docker.
- The PostgreSQL server has no TLS, so `tests/postgres.rs` checks the verifying TLS modes only when `PG_TLS_HOST` and
  `PG_TLS_CA` point at a TLS server whose certificate names that host.
- **Atlas** (optional, `tests/atlas.rs`): SRV + TLS against a MongoDB Atlas cluster. Put `GOKU_ATLAS_USER`,
  `GOKU_ATLAS_PASSWORD` and `GOKU_ATLAS_HOST` in a file of your own, e.g. `.env.atlas` at the repo root (gitignored), and
  pass the file to Docker, never the values on the command line:
  ```bash
  docker compose run --rm --env-from-file .env.atlas engine cargo test -p goku-engine --test atlas -- --nocapture
  ```
  It prints only the server version and creates/drops `rs_atlas_<random>`. Atlas'
  IP access list must allow your address. Without the variables the test is skipped.

## Parity fixtures
`parity/` holds JSON snapshots of GokuCore behaviour (the Goku app's engine) that the Rust engine must reproduce:
`SchemaModel` per engine for the seeded test schemas, Mongo inference on fixed document sets, and `ERLayout` output for
fixed inputs. Shapes and normalization: [`parity/README.md`](../parity/README.md).

- `tests/parity_er_layout.rs` and `tests/parity_query.rs` (splitting, SQL safety, danger check, Mongo shell; no
  database), `tests/parity_schema.rs` and `tests/parity_mongo_inference.rs` (need the databases, `GOKU_IT=1`) compare
  parsed values, not text (`parity_query.rs` writes Mongo values as canonical Extended JSON the way the app does and
  compares that text, so key order and BSON types count).
- The fixtures are **generated by the Goku app's test suite** against the same seeds. They are never edited by hand
  here: a change arrives as a sync pull request from the app's repository that updates `parity/` (and, when the seeds
  changed, `docker/*/init/`).
- If a Rust change makes a parity test fail, the Rust side is wrong, or the app has to change first. Don't "fix" the
  JSON. Open an issue if you think a fixture is wrong.
- The database fixtures depend on the image versions (PostgreSQL 17, MySQL 8.4, MongoDB 7) and on the seeds; keep both
  in step with the app.

## Extension tests (`vscode/`)
- Node's built-in runner on the `.ts` sources (type stripping, `erasableSyntaxOnly`): `npm test` runs
  `test/**/*.test.ts`. `npm run check` = typecheck + tests.
- `test/client.test.ts` runs the real engine (`GOKU_ENGINE_BIN`, a debug build for `debug/wait`), which `vscode test`
  builds first in the `engine` container. Without the variable those tests are skipped.
- Pure modules (`webview/lib/*`, `src/views/{protocol,html,layout}.ts`, `src/tree/model.ts`, `src/connections/*` except
  `store.ts`/`commands.ts`) stay free of `vscode` and the DOM, so these tests can import them.

## End-to-end test
`scripts/dev.sh vscode e2e` (`vscode/test-e2e/`): `@vscode/test-electron` downloads VS Code (stable, `GOKU_E2E_VSCODE`
to pin; cached in the `vscode-test` volume) and runs it under xvfb in the `vscode-e2e` image on the compose network,
with a scratch workspace whose `.env` has `DATABASE_URL` (pg-primary), `MYSQL_URL` (`ssl-mode=REQUIRED`) and
`MONGODB_URI`, and `GOKU_ENGINE_PATH` = the Linux debug engine. `test-e2e/suite.cjs` uses the test API that `activate`
returns in `ExtensionMode.Test` (`tree`, `view`, `sessions`, `store`, `engine`, `answered`) and checks:
- the engine isn't started by activation; three read-only connections;
- each shows only `shop` until Show All Databases (then `postgres` too, and back);
- each expands to its tables/collections and columns;
- `goqulient.openTable` gets a `table/page` answer for each engine and its title names the connection;
- the ER diagram gets `schema/introspect` and `er/layout` (so the webview measured the boxes);
- Disconnect and Restart Engine leave connections idle and the next expand reconnects;
- a user-settings connection through `ssh-bastion`: the engine answers `unknownHostKey`, the store pins that key into the
  user settings (what "Trust and Connect" does; the modal itself can't be clicked there), and expanding connects through
  the tunnel;
- no item shows the test password or the SSH password.

It only reads. Set `GOKU_E2E_SCREENSHOTS` to keep `table.png`/`er.png` of the window and look at them: the test checks
that the data arrives, not what is drawn.

## Webview dev harness
`vscode/webview-dev/{table,er}.html` load the webview bundles with `fake-host.js` (a fake `acquireVsCodeApi` with
synthetic data and `parity/schema/*.json`) and `themes.css` (`?theme=dark|light|hc`). Screenshots (gitignored) and frame
timings, in the Playwright image:
```bash
docker run --rm --ipc=host -v "$PWD":/w -v goqulient-webview-node-modules:/w/vscode/node_modules -w /w/vscode \
  mcr.microsoft.com/playwright:v1.55.0-noble bash -c "npm ci && npm run build && \
  npm i --no-save playwright@1.55.0 && node webview-dev/snapshot.mjs"   # → webview-dev/snapshots/*.png
docker volume rm goqulient-webview-node-modules                         # when you're done
```
It fails on page errors and CSP violations.

## Seeing the extension in your VS Code
`scripts/dev.sh vscode run [folder]` needs `cargo` on your machine (the engine must be built for your OS): it builds
`engine/target/debug/goku-engine-server`, builds `vscode/dist` (with npm if present, else in the `vscode` container: the
output is plain JavaScript) and runs `code --new-window --extensionDevelopmentPath=vscode <folder>` with
`GOKU_ENGINE_PATH` set. That variable is only read in development and test hosts (`ExtensionMode`), never by an
installed extension. If your `code` hands the window to an already running VS Code that doesn't pass the variable on,
set `goqulient.enginePath` in your user settings instead. Without cargo, install a VSIX from the "Extension release"
workflow's artifacts (`code --install-extension goqulient-<target>.vsix`); [PUBLISHING.md](PUBLISHING.md#1-try-a-build-before-publishing)
covers the other editors and which file to pick.

## CI
`.github/workflows/ci.yml`:
- **build** (Linux, macOS, Windows): `cargo fmt --check` + clippy (Linux), `cargo test` (integration tests skip
  themselves), the engine debug build, then `npm ci`, typecheck, build and the Node tests against that engine.
- **integration** (Linux): `scripts/dev.sh up`, `engine test` (all integration and parity tests, `GOKU_IT=1`) and
  `vscode e2e`, i.e. the same Docker setup as on your machine. The databases come from compose rather than GitHub
  service containers because service containers start before the checkout, so they can't mount the seed scripts.
