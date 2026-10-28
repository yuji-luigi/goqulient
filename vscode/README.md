# Goku for VS Code (preview)

Browse PostgreSQL, MySQL/MariaDB and MongoDB databases from VS Code, with the same engine rules as the Goku app:
every network wait has a deadline, results are capped, read-only connections are enforced by the server too, and
passwords stay in VS Code's secret storage.

**Status: early preview.** The engine and its protocol are in place; the database browser, table viewer and ER diagrams
are being built (see `docs/vscode-extension.md` in the repository).

## Settings
- `goku.connections`: connections in the Goku app's profile format (no passwords; Goku asks for them).
- `goku.envFiles`: workspace files searched for `DATABASE_URL`, `MONGODB_URI`, `PG*`, `MYSQL_*` (read-only by default,
  trusted workspaces only).
- `goku.enginePath`: use a locally built engine (machine setting, for development).

## Platforms
Windows, macOS and Linux (x64 and arm64), including Remote-SSH, WSL and dev containers.
