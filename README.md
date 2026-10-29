# GoQulient

Browse PostgreSQL, MySQL/MariaDB and MongoDB databases from the VS Code sidebar: tables, views and collections with
their columns, a fast read-only data grid, and ER diagrams (including relationships inferred from MongoDB documents).
The database work happens in a small Rust engine that the extension runs next to VS Code, with hard limits: every
network wait has a deadline, results are capped, read-only connections are enforced by the database session too, and
passwords stay in VS Code's secret storage.

> Preview: published as a pre-release until 1.0. No SSH tunnels and no query editor yet.

![The GoQulient sidebar and an ER diagram](vscode/media/screenshot-er.png)

## Features
- **Connections from where they already are**: `DATABASE_URL`, `POSTGRES_URL`, `MYSQL_URL`, `MONGODB_URI`, `PG*`,
  `MYSQL_*` and the Docker images' `POSTGRES_*` / `MYSQL_ROOT_PASSWORD` variables in your workspace `.env` (read-only by
  default, and only in trusted workspaces), any variable after a `# DATABASE_URL: 'Name'` comment, or connections in
  your settings (**GoQulient: Add Connection**).
- **Sidebar tree**: connection → database → schema → tables / views / materialized views / collections → columns.
  Connections open when you expand them and can be cancelled.
- **Table viewer**: virtualised grid for wide and long tables, paging, sorting, copy as TSV; a documents view for
  MongoDB.
- **ER diagrams**: foreign keys for SQL databases, inferred references for MongoDB; drag tables (positions are saved per
  workspace), auto layout, search, an inspector, SVG/PNG export.

![Table viewer](vscode/media/screenshot-table.png)

The user guide (settings, security model) is the extension's own [README](vscode/README.md), which is also its
Marketplace page.

## Platforms
Windows, macOS and Linux, x64 and arm64: one VSIX per platform, each with its engine binary (Linux builds are static,
so any distribution works). Remote-SSH, WSL and dev containers work too; the engine runs on the remote side.

## Quick start
Install **GoQulient** from the VS Code Marketplace or Open VSX, or a VSIX from a release:
```bash
code --install-extension goqulient-<platform>.vsix   # e.g. goqulient-darwin-arm64.vsix
```
Open a folder whose `.env` has a `DATABASE_URL`, or run **GoQulient: Add Connection**, then expand the connection in
the GoQulient view.

To build and test from source you only need Docker:
```bash
git clone https://github.com/yuji-luigi/goqulient && cd goqulient
scripts/dev.sh up            # test databases + build containers (compose project "goqulient")
scripts/dev.sh engine test   # Rust engine: unit, integration and parity tests
scripts/dev.sh vscode test   # extension: typecheck + tests against the engine
scripts/dev.sh vscode e2e    # the extension inside a real VS Code (headless)
scripts/dev.sh down
```
See [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/TESTING.md](docs/TESTING.md). Releasing and publishing to the
Marketplace and Open VSX: [docs/RELEASING.md](docs/RELEASING.md) and [docs/PUBLISHING.md](docs/PUBLISHING.md).

## How it works
```
VS Code extension (TypeScript, vscode/)  ── JSON-RPC over stdio ──  goku-engine-server (Rust, engine/)
  connections, sidebar tree, webviews                                  PostgreSQL · MySQL/MariaDB · MongoDB drivers,
  secrets in SecretStorage                                             deadlines, row caps, read-only, ER layout
```
One engine process per VS Code window; if it crashes, the extension restarts it. Details:
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Relation to the Goku app
GoQulient comes from **Goku**, a native macOS database client. Its engine is a Rust port
of the app's Swift engine (GokuCore), with the same connection format, the same error wording, the same introspection
and the same ER layout. The two are kept in parity by JSON fixtures that the app's test suite generates and this
repository's tests must reproduce ([`parity/`](parity/README.md)). You don't need the app to use, build or change
GoQulient.

## Repository layout
| Path | |
|---|---|
| `vscode/` | the extension (TypeScript) |
| `engine/` | the Rust engine: `goku-engine` (library) + `goku-engine-server` (stdio JSON-RPC server) |
| `parity/` | fixtures shared with the Goku app |
| `docker/`, `docker-compose.yml`, `scripts/dev.sh` | test databases and the Docker-only dev workflow |
| `docs/` | [architecture](docs/ARCHITECTURE.md), [testing](docs/TESTING.md), [releasing](docs/RELEASING.md), [publishing](docs/PUBLISHING.md) |

## Contributing, security, license
- Contributions are welcome: [CONTRIBUTING.md](CONTRIBUTING.md). Coding agents start at [AGENTS.md](AGENTS.md).
- Found a vulnerability? Please report it privately: [SECURITY.md](SECURITY.md).
- [MIT](LICENSE) © 2026 yuji-luigi
