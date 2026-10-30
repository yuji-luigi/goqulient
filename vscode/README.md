# GoQulient for VS Code (preview)

Browse PostgreSQL, MySQL/MariaDB and MongoDB databases from the VS Code sidebar: tables, views and collections with
their columns, a fast read-only data grid, and ER diagrams (including relationships inferred from MongoDB documents).
It runs on the same engine rules as the Goku app: every network wait has a deadline, results are capped, read-only
connections are enforced by the server too, and passwords stay in VS Code's secret storage.

![The GoQulient sidebar and an ER diagram](media/screenshot-er.png)

## Features
- **Connections from where they already are**: `DATABASE_URL`, `POSTGRES_URL`, `MYSQL_URL`, `MONGODB_URI`, `PG*`,
  `MYSQL_*` and the Docker images' `POSTGRES_*` / `MYSQL_ROOT_PASSWORD` variables in your workspace `.env`, or
  connections you add to your settings (**GoQulient: Add Connection**, by fields or from a URL).
  Any other variable becomes a connection when a marker comment comes before it:
  ```sh
  # DATABASE_URL: 'Interaction test'
  DB_URI="mongodb://user:password@db.example.com:27017/interaction"
  ```
- **Sidebar tree**: connection → database → schema (PostgreSQL) → tables / views / materialized views / collections →
  columns (type, primary/foreign key, NOT NULL; field presence for MongoDB). Connections open when you expand them;
  a slow one can be cancelled (**Cancel Connecting**), a failure shows the server's message with Retry.
- **Table viewer**: double-click a table or collection (or press Enter). Virtualised grid for wide and long tables,
  paging, sorting by column, copying cells and rows as TSV; documents view for MongoDB.
- **SSH tunnels**: reach a database behind a bastion with an `ssh` object in the connection's settings (below).
- **ER diagrams**: on a database or schema, **Open ER Diagram**. Foreign keys for SQL databases, inferred references for
  MongoDB; drag tables (positions are saved per workspace), auto layout, search, an inspector, SVG/PNG export.

![Table viewer](media/screenshot-table.png)

## Settings
| Setting | |
|---|---|
| `goqulient.connections` | Connections in the Goku app's profile format (`name`, `kind`, `hosts`, `user`, `database`, `ssl`, `ssh`, `readOnly`, …). No passwords here: GoQulient asks and keeps them in secret storage. |
| `goqulient.envFiles` | Files searched for connection variables, relative to each workspace folder (default `.env`). |
| `goqulient.envConnections` | More variables holding connection URLs, e.g. `{"url": "ANALYTICS_DB_URL", "name": "Analytics", "readOnly": false}`. |
| `goqulient.showAllDatabases` | List every database on the server for connections that name one (default: only that database). **Show All Databases** in a connection's context menu changes it for that connection. |
| `goqulient.enginePath` | Use a locally built engine (machine setting, for development). |

### SSH tunnels
Add an `ssh` object to a connection in `goqulient.connections`:
```json
{
  "name": "Orders (prod)",
  "kind": "postgres",
  "hosts": [{ "host": "orders-db.internal", "port": 5432 }],
  "user": "reporting",
  "database": "orders",
  "ssh": { "enabled": true, "host": "bastion.example.com", "port": 22, "user": "me", "auth": "privateKey", "privateKeyPath": "~/.ssh/id_ed25519" }
}
```
- `auth`: `password` (the default; GoQulient asks for the SSH password) or `privateKey` (`privateKeyPath`, default
  `~/.ssh/id_ed25519`; OpenSSH, PEM and PuTTY keys). An encrypted key's passphrase is asked for when the key needs one.
  The SSH password and passphrase are kept like the database password (secret storage or until VS Code closes);
  **Forget Saved Password** forgets them too.
- The first time, GoQulient shows the SSH server's key fingerprint and asks whether to trust it; yes saves the key as
  `trustedHostKey` in that connection's settings. If the server later presents a different key, the connection stops
  with a warning (a reinstalled server, or someone in between): check with your administrator before you clear it.
- Host names in `hosts` are resolved by the SSH server, so internal names work. The tunnel listens on 127.0.0.1 only and
  closes with the connection. Every host of a multi-host profile goes through the same tunnel; MongoDB connects directly
  to the first host that answers (no replica-set discovery through a tunnel, as in the Goku app), and its TLS
  verification modes don't work through a tunnel (use `"ssl": {"mode": "require"}` there).
- Not supported: `mongodb+srv://` and URI-mode MongoDB connections, SSH agents, jump hosts and `~/.ssh/config`.
  Connections from `.env` files never use a tunnel.

## Security
- **Read-only by default** for connections found in `.env` files (a `DATABASE_URL` is often production). Read-only is
  enforced by the client and by the database session (PostgreSQL and MySQL); `goqulient.envConnections[].readOnly: false`
  turns it off for one variable.
- **Secrets**: passwords (database and SSH) are never written to settings or logs. Typed passwords go to VS Code's secret storage (or are
  kept until VS Code closes, your choice) and are only handed out for the server they were typed for. Passwords in
  `.env` are read when connecting and never stored. The engine receives them over its private stdin pipe, never on the
  command line or in the environment; shown endpoints and error messages are redacted.
- **Workspace Trust**: in an untrusted workspace GoQulient ignores its `.env` files and workspace settings; only your own
  user settings apply.
- The engine is a separate process per window with a crash budget; it never takes VS Code down.

## Platforms
Windows, macOS and Linux, x64 and arm64 (one package per platform with its engine), including Remote-SSH, WSL and dev
containers, where the engine runs on the remote side.

## Not yet
A query editor, editing rows and exporting data are planned. Feedback and bug reports are welcome:
[github.com/yuji-luigi/goqulient/issues](https://github.com/yuji-luigi/goqulient/issues). GoQulient is open source
under the MIT license.
