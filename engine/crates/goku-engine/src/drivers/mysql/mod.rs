//! MySQL / MariaDB (port of `MySQLSession.swift`). Two lazily opened connections, `editor` (browse, execute) and `meta`
//! (catalog, introspection), each serialised by its mutex like Swift's `AsyncLock`. A query that times out, is cancelled
//! or is abandoned at the row cap gets `KILL QUERY <id>` from a short-lived second connection, and its connection is
//! discarded (never dropped implicitly: mysql_async's `Drop` would drain the rest of the result in the background).
mod catalog;
mod format;

use crate::deadline::with_deadline;
use crate::error::{DbError, Result};
use crate::limits::{Paging, RowCollector};
use crate::model::{Attribute, ObjectRef, QueryResult, ResultColumn, SchemaModel, ServerInfo, Sort};
use crate::profile::{ConnectionProfile, Credentials, HostPort, SslMode, SslSettings, TargetSession};
use crate::session::DatabaseSession;
use async_trait::async_trait;
use mysql_async::prelude::{Protocol, Queryable};
use mysql_async::{ClientIdentity, Conn, DriverError, Opts, OptsBuilder, Params, SslOpts};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Rows read and thrown away after the cap before the query is killed instead (browse asks for one extra row).
const DRAIN_ROWS: usize = 1000;

pub async fn connect(
    profile: ConnectionProfile,
    credentials: Credentials,
    cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    if profile.ssh.enabled {
        return Err(DbError::Unsupported("SSH tunnels aren't available in the engine yet".into()));
    }
    // mysql_async builds its rustls config with the process-wide provider; pick one in case several are compiled in.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = ssl_opts(&profile.ssl)?;
    let passes = match profile.target_session {
        TargetSession::PreferStandby => vec![TargetSession::Standby, TargetSession::Any],
        t => vec![t],
    };
    let mut failures = Vec::new();
    for pass in passes {
        for i in 0..profile.hosts.len() {
            let host = profile.hosts[i].clone();
            let (mut live, opts) = match open(&profile, &credentials, &host, tls.clone(), cancel).await {
                Ok(v) => v,
                Err(DbError::Cancelled) => return Err(DbError::Cancelled),
                Err(e) => {
                    failures.push(format!("{host}: {e}"));
                    continue;
                }
            };
            let probe = "SELECT @@global.read_only OR @@global.super_read_only";
            let read_only = match on_conn(&mut live.conn, probe, &profile, cancel).await {
                Ok(r) => r.rows.first().and_then(|r| r[0].as_deref()) == Some("1"),
                Err(DbError::Query(_)) => false, // MariaDB has no super_read_only
                Err(e) => {
                    discard(live.conn);
                    if e == DbError::Cancelled {
                        return Err(e);
                    }
                    failures.push(format!("{host}: {e}"));
                    continue;
                }
            };
            if accepts(pass, read_only) {
                return Ok(Box::new(MySqlSession {
                    profile,
                    host,
                    opts,
                    editor: Mutex::new(None),
                    meta: Mutex::new(Some(live)),
                    shutdown: CancellationToken::new(),
                }));
            }
            discard(live.conn);
            let role = if read_only { "read-only replica" } else { "writable primary" };
            failures.push(format!("{host}: {role} does not match {}", target_name(pass)));
        }
    }
    if profile.hosts.len() == 1 && failures.len() == 1 {
        return Err(DbError::ConnectionFailed(failures.remove(0)));
    }
    Err(DbError::NoSuitableHost(failures.join("\n")))
}

/// `PostgresSession.accepts`, with MySQL's `read_only` standing in for recovery.
fn accepts(target: TargetSession, read_only: bool) -> bool {
    match target {
        TargetSession::Any | TargetSession::PreferStandby => true,
        TargetSession::ReadWrite | TargetSession::Primary => !read_only,
        TargetSession::ReadOnly | TargetSession::Standby => read_only,
    }
}

fn target_name(t: TargetSession) -> &'static str {
    match t {
        TargetSession::Any => "Any node",
        TargetSession::ReadWrite => "Read-write",
        TargetSession::ReadOnly => "Read-only",
        TargetSession::Primary => "Primary",
        TargetSession::Standby => "Standby / replica",
        TargetSession::PreferStandby => "Prefer standby",
    }
}

/// `TLSFactory.configuration`: prefer/require encrypt without verifying, verifyCA skips the host name, verifyFull checks
/// both. A CA file replaces the built-in roots.
fn ssl_opts(s: &SslSettings) -> Result<Option<SslOpts>> {
    let mut o = match s.mode {
        SslMode::Disable => return Ok(None),
        SslMode::Prefer | SslMode::Require => SslOpts::default().with_danger_accept_invalid_certs(true),
        SslMode::VerifyCa => SslOpts::default().with_danger_skip_domain_validation(true),
        SslMode::VerifyFull => SslOpts::default(),
    };
    let ca = expand(&s.ca_certificate_path);
    if !ca.is_empty() {
        if std::fs::File::open(&ca).is_err() {
            return Err(DbError::InvalidConfiguration(format!("CA certificate not readable: {ca}")));
        }
        o = o.with_root_certs(vec![PathBuf::from(ca).into()]).with_disable_built_in_roots(true);
    }
    let (cert, key) = (expand(&s.client_certificate_path), expand(&s.client_key_path));
    if !cert.is_empty() || !key.is_empty() {
        if cert.is_empty() || key.is_empty() {
            return Err(DbError::InvalidConfiguration("Client certificate and key must both be set.".into()));
        }
        o = o.with_client_identity(Some(ClientIdentity::new(PathBuf::from(cert).into(), PathBuf::from(key).into())));
    }
    Ok(Some(o))
}

fn expand(path: &str) -> String {
    let p = path.trim();
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
            format!("{home}{rest}")
        }
        _ => p.to_owned(),
    }
}

fn build_opts(profile: &ConnectionProfile, credentials: &Credentials, host: &HostPort, tls: Option<SslOpts>) -> Opts {
    OptsBuilder::default()
        .ip_or_hostname(host.host.clone())
        .tcp_port(host.port as u16)
        .user(Some(profile.user.clone()))
        .pass(credentials.password.clone())
        .db_name((!profile.database.is_empty()).then(|| profile.database.clone()))
        .ssl_opts(tls)
        .prefer_socket(false)
        .tcp_nodelay(true)
        .into()
}

/// One connection to `host` with the session settings, and the options that worked (`prefer` may drop TLS).
async fn open(
    profile: &ConnectionProfile,
    credentials: &Credentials,
    host: &HostPort,
    tls: Option<SslOpts>,
    cancel: &CancellationToken,
) -> Result<(Live, Opts)> {
    let opts = build_opts(profile, credentials, host, tls);
    let conn = match dial(&opts, host, profile, cancel).await? {
        Err(mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer))
            if profile.ssl.mode == SslMode::Prefer =>
        {
            let plain = build_opts(profile, credentials, host, None);
            let conn = dial(&plain, host, profile, cancel).await?.map_err(|e| clean(&e))?;
            return Ok((setup(conn, profile, cancel).await?, plain));
        }
        other => other.map_err(|e| clean(&e))?,
    };
    Ok((setup(conn, profile, cancel).await?, opts))
}

/// Dropping `Conn::new` mid-way closes its socket, so the deadline needs no hook.
async fn dial(
    opts: &Opts,
    host: &HostPort,
    profile: &ConnectionProfile,
    cancel: &CancellationToken,
) -> Result<std::result::Result<Conn, mysql_async::Error>> {
    let what = format!("Connecting to {host}");
    with_deadline(profile.connect_timeout_seconds as f64, &what, cancel, || {}, async {
        Ok(Conn::new(opts.clone()).await)
    })
    .await
}

/// Session settings (Swift's `openConnection`): utf8mb4, read-only transactions for a read-only profile, and the
/// server-side statement limit (MySQL `max_execution_time` in ms, MariaDB `max_statement_time` in s; each fails on the other).
async fn setup(mut conn: Conn, profile: &ConnectionProfile, cancel: &CancellationToken) -> Result<Live> {
    let mut required = vec!["SET NAMES utf8mb4".to_owned()];
    if profile.read_only {
        required.push("SET SESSION TRANSACTION READ ONLY".into());
    }
    let q = profile.query_timeout_seconds;
    let optional =
        [format!("SET SESSION max_execution_time = {}", q * 1000), format!("SET SESSION max_statement_time = {q}")];
    let r = with_deadline(q as f64, "Session setup", cancel, || {}, async {
        for sql in &required {
            if let Err(e) = conn.query_drop(sql.as_str()).await {
                return Ok(Err(e));
            }
        }
        for sql in &optional {
            if let Err(e) = conn.query_drop(sql.as_str()).await
                && !matches!(e, mysql_async::Error::Server(_))
            {
                return Ok(Err(e));
            }
        }
        Ok(Ok(()))
    })
    .await;
    match r {
        Ok(Ok(())) => Ok(Live { id: conn.id(), database: profile.database.clone(), conn }),
        Ok(Err(e)) => {
            discard(conn);
            Err(clean(&e))
        }
        Err(e) => {
            discard(conn);
            Err(e)
        }
    }
}

/// One statement on a connection that isn't in a slot yet (the role probe). The caller discards `conn` on an error
/// other than `Query`.
async fn on_conn(
    conn: &mut Conn,
    sql: &str,
    profile: &ConnectionProfile,
    cancel: &CancellationToken,
) -> Result<QueryResult> {
    let r = with_deadline(profile.query_timeout_seconds as f64, "Query", cancel, || {}, async {
        Ok(collect(conn, sql, Params::Empty, 1).await)
    })
    .await?;
    match r {
        Ok(c) if !c.abandoned => Ok(c.result),
        Ok(_) => Err(DbError::ConnectionFailed("probe returned too many rows".into())),
        Err(e @ mysql_async::Error::Server(_)) => Err(clean(&e)),
        Err(e) => Err(DbError::ConnectionFailed(clean(&e).to_string())),
    }
}

/// Closes a connection whose state is unknown: COM_QUIT without draining, bounded; the socket closes either way.
fn discard(conn: Conn) {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => {
            h.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(2), conn.disconnect()).await;
            });
        }
        Err(_) => drop(conn),
    }
}

/// `MySQLSession.clean`: server errors read `[code/state] message`; 1792 (read-only transaction) is `ServerReadOnly`.
fn clean(e: &mysql_async::Error) -> DbError {
    match e {
        mysql_async::Error::Server(s) => {
            let text = format!("[{}/{}] {}", s.code, s.state, s.message);
            if s.code == 1792 { DbError::ServerReadOnly(text) } else { DbError::Query(text) }
        }
        other => DbError::Query(crate::redact::secrets(&other.to_string())),
    }
}

struct Live {
    conn: Conn,
    /// Thread id for `KILL QUERY`.
    id: u32,
    /// Default database, tracked to issue `USE` only when it changes.
    database: String,
}

struct Collected {
    result: QueryResult,
    /// Stopped reading before the end of the result: the connection still has rows pending.
    abandoned: bool,
}

/// Prepared (binary) protocol, falling back to the text protocol for statements that can't be prepared (1295), like
/// Swift's `collect`. The first result set is read into a `RowCollector`; later ones (CALL) are dropped.
async fn collect(
    conn: &mut Conn,
    sql: &str,
    params: Params,
    max_rows: usize,
) -> std::result::Result<Collected, mysql_async::Error> {
    let fallback = matches!(params, Params::Empty);
    match conn.exec_iter(sql, params).await {
        Ok(r) => read(r, max_rows).await,
        Err(mysql_async::Error::Server(e)) if e.code == 1295 && fallback => {
            let r = conn.query_iter(sql).await?;
            read(r, max_rows).await
        }
        Err(e) => Err(e),
    }
}

async fn read<P: Protocol>(
    mut r: mysql_async::QueryResult<'_, 'static, P>,
    max_rows: usize,
) -> std::result::Result<Collected, mysql_async::Error> {
    let columns = r.columns().map(|c| c.to_vec()).unwrap_or_default();
    let mut result = QueryResult {
        columns: columns
            .iter()
            .map(|c| ResultColumn { name: c.name_str().into_owned(), type_name: format::type_name(c) })
            .collect(),
        ..QueryResult::default()
    };
    if columns.is_empty() {
        result.affected_rows = Some(r.affected_rows());
        r.drop_result().await?;
        return Ok(Collected { result, abandoned: false });
    }
    let mut collector = RowCollector::new(max_rows);
    let mut drained = 0;
    while let Some(row) = r.next().await? {
        if collector.is_full() {
            collector.mark_truncated();
            drained += 1;
            if drained >= DRAIN_ROWS {
                (result.rows, result.truncated) = collector.into_rows();
                return Ok(Collected { result, abandoned: true });
            }
            continue;
        }
        let values = row.unwrap();
        collector.append(values.iter().zip(&columns).map(|(v, c)| format::format(v, c)).collect());
    }
    r.drop_result().await?;
    (result.rows, result.truncated) = collector.into_rows();
    Ok(Collected { result, abandoned: false })
}

#[derive(Clone, Copy)]
enum Purpose {
    Editor,
    Meta,
}

pub struct MySqlSession {
    profile: ConnectionProfile,
    host: HostPort,
    /// Options of the chosen host (they hold the password: reconnects and `KILL QUERY` need it, as in Swift).
    opts: Opts,
    editor: Mutex<Option<Live>>,
    meta: Mutex<Option<Live>>,
    /// Fired by `close()`: stops running work, which kills its query.
    shutdown: CancellationToken,
}

impl MySqlSession {
    fn query_seconds(&self) -> f64 {
        self.profile.query_timeout_seconds as f64 + 2.0
    }

    /// One statement on `purpose`'s connection, (re)opened when needed, under `query timeout + 2` like Swift.
    async fn run(
        &self,
        purpose: Purpose,
        database: Option<&str>,
        sql: &str,
        params: Params,
        max_rows: usize,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let token = cancel.child_token();
        let link = async {
            self.shutdown.cancelled().await;
            token.cancel();
            std::future::pending::<()>().await
        };
        tokio::select! {
            biased;
            r = self.run_linked(purpose, database, sql, params, max_rows, &token) => r,
            _ = link => unreachable!("pending"),
        }
    }

    async fn run_linked(
        &self,
        purpose: Purpose,
        database: Option<&str>,
        sql: &str,
        params: Params,
        max_rows: usize,
        token: &CancellationToken,
    ) -> Result<QueryResult> {
        let slot = match purpose {
            Purpose::Editor => &self.editor,
            Purpose::Meta => &self.meta,
        };
        let seconds = self.query_seconds();
        let mut guard =
            with_deadline(seconds, "Waiting for the connection", token, || {}, async { Ok(slot.lock().await) }).await?;
        if self.shutdown.is_cancelled() {
            return Err(DbError::NotConnected);
        }
        if guard.is_none() {
            let conn = dial(&self.opts, &self.host, &self.profile, token).await?.map_err(|e| clean(&e))?;
            *guard = Some(setup(conn, &self.profile, token).await?);
        }
        let Some(live) = guard.as_mut() else { return Err(DbError::NotConnected) }; // opened above
        let id = live.id;
        let started = Instant::now();
        let r = with_deadline(seconds, "Query", token, || self.kill(id), async {
            if let Some(db) = database.filter(|d| !d.is_empty() && *d != live.database) {
                if let Err(e) = live.conn.query_drop(format!("USE {}", format::ident(db))).await {
                    return Ok(Err(e));
                }
                live.database = db.to_owned();
            }
            let c = collect(&mut live.conn, sql, params, max_rows).await;
            if c.is_ok() && is_use(sql) {
                // A USE in the statement moves the default database; ask the server where it landed.
                if let Ok(Some(db)) = live.conn.query_first::<Option<String>, _>("SELECT DATABASE()").await {
                    live.database = db.unwrap_or_default();
                }
            }
            Ok(c)
        })
        .await;
        let drop_conn = |slot: &mut Option<Live>| {
            if let Some(l) = slot.take() {
                discard(l.conn);
            }
        };
        let slot = &mut *guard;
        match r {
            Err(e) => {
                drop_conn(slot); // the query was killed by `on_timeout`; its connection is mid-result
                Err(e)
            }
            Ok(Err(e)) => {
                if !matches!(e, mysql_async::Error::Server(_)) {
                    drop_conn(slot);
                }
                Err(clean(&e))
            }
            Ok(Ok(mut c)) => {
                if c.abandoned {
                    self.kill(id);
                    drop_conn(slot);
                }
                c.result.elapsed_ms = started.elapsed().as_millis() as u64;
                Ok(c.result)
            }
        }
    }

    /// Catalog query on `meta`: every row (up to the hard cap), cells as text.
    async fn rows(&self, sql: &str, params: Vec<&str>, cancel: &CancellationToken) -> Result<Vec<Vec<Option<String>>>> {
        let params = if params.is_empty() {
            Params::Empty
        } else {
            Params::Positional(params.into_iter().map(mysql_async::Value::from).collect())
        };
        Ok(self.run(Purpose::Meta, None, sql, params, crate::limits::HARD_MAX_ROWS, cancel).await?.rows)
    }

    /// `KILL QUERY` from a short-lived connection, in the background and bounded (Swift's `kill`).
    fn kill(&self, id: u32) {
        let Ok(h) = tokio::runtime::Handle::try_current() else { return };
        let opts = self.opts.clone();
        let seconds = (self.profile.connect_timeout_seconds + self.profile.query_timeout_seconds) as u64;
        h.spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(seconds), async {
                let mut c = Conn::new(opts).await?;
                c.query_drop(format!("KILL QUERY {id}")).await?;
                c.disconnect().await
            })
            .await;
        });
    }
}

fn is_use(sql: &str) -> bool {
    let s = sql.trim_start();
    s.len() > 3 && s[..3].eq_ignore_ascii_case("use") && s[3..].starts_with(|c: char| c.is_whitespace() || c == '`')
}

impl Drop for MySqlSession {
    fn drop(&mut self) {
        for slot in [&mut self.editor, &mut self.meta] {
            if let Some(l) = slot.get_mut().take() {
                discard(l.conn);
            }
        }
    }
}

#[async_trait]
impl DatabaseSession for MySqlSession {
    fn profile(&self) -> &ConnectionProfile {
        &self.profile
    }

    async fn server_info(&self, cancel: &CancellationToken) -> Result<ServerInfo> {
        let sql = "SELECT VERSION(), @@version_comment, CURRENT_USER(), @@global.read_only OR @@global.super_read_only";
        let rows = match self.rows(sql, vec![], cancel).await {
            Err(DbError::Query(_)) => {
                self.rows("SELECT VERSION(), @@version_comment, CURRENT_USER(), @@global.read_only", vec![], cancel)
                    .await?
            }
            other => other?,
        };
        let row = rows.into_iter().next().ok_or_else(|| DbError::Query("empty VERSION()".into()))?;
        let cell = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
        let version = cell(0);
        let product = if version.to_lowercase().contains("mariadb") {
            "MariaDB"
        } else if cell(1).contains("Percona") {
            "Percona Server"
        } else {
            "MySQL"
        };
        Ok(ServerInfo {
            product: product.into(),
            version,
            connected_host: self.host.to_string(),
            current_user: cell(2),
            is_read_only_node: cell(3) == "1",
        })
    }

    async fn databases(&self, cancel: &CancellationToken) -> Result<Vec<String>> {
        let sql = "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA \
                   ORDER BY SCHEMA_NAME IN ('mysql','information_schema','performance_schema','sys'), SCHEMA_NAME";
        Ok(self.rows(sql, vec![], cancel).await?.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect())
    }

    async fn schemas(&self, _database: &str, _cancel: &CancellationToken) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn objects(&self, database: &str, _schema: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>> {
        catalog::objects(self, database, cancel).await
    }

    async fn columns(&self, object: &ObjectRef, cancel: &CancellationToken) -> Result<Vec<Attribute>> {
        catalog::columns(self, object, cancel).await
    }

    async fn browse(
        &self,
        object: &ObjectRef,
        paging: Paging,
        sort: Option<&Sort>,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let limit = paging.limit.min(self.profile.max_rows.max(1) as usize);
        let table = if object.database.is_empty() {
            format::ident(&object.name)
        } else {
            format!("{}.{}", format::ident(&object.database), format::ident(&object.name))
        };
        let mut sql = format!("SELECT * FROM {table}");
        if let Some(s) = sort {
            sql += &format!(" ORDER BY {}{}", format::ident(&s.column), if s.descending { " DESC" } else { "" });
        }
        // One extra row tells whether another page exists.
        sql += &format!(" LIMIT {} OFFSET {}", limit + 1, paging.offset);
        self.run(Purpose::Editor, None, &sql, Params::Empty, limit, cancel).await
    }

    async fn introspect(&self, database: &str, _schemas: &[String], cancel: &CancellationToken) -> Result<SchemaModel> {
        catalog::introspect(self, database, cancel).await
    }

    async fn execute(&self, database: &str, statement: &str, cancel: &CancellationToken) -> Result<QueryResult> {
        let max_rows = self.profile.max_rows.max(1) as usize;
        self.run(Purpose::Editor, Some(database), statement, Params::Empty, max_rows, cancel).await
    }

    async fn close(&self) {
        self.shutdown.cancel();
        for slot in [&self.editor, &self.meta] {
            // Running work stops on `shutdown` and releases the lock; bounded anyway.
            if let Ok(mut guard) = tokio::time::timeout(Duration::from_secs(5), slot.lock()).await
                && let Some(l) = guard.take()
            {
                let _ = tokio::time::timeout(Duration::from_secs(2), l.conn.disconnect()).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_selection_rules() {
        assert!(accepts(TargetSession::Any, true) && accepts(TargetSession::PreferStandby, false));
        assert!(accepts(TargetSession::Primary, false) && !accepts(TargetSession::Primary, true));
        assert!(accepts(TargetSession::Standby, true) && !accepts(TargetSession::ReadOnly, false));
    }

    #[test]
    fn tls_settings() {
        let mut s = SslSettings { mode: SslMode::Disable, ..SslSettings::default() };
        assert!(ssl_opts(&s).unwrap().is_none());
        s.mode = SslMode::Require;
        assert!(ssl_opts(&s).unwrap().unwrap().accept_invalid_certs());
        s.mode = SslMode::VerifyCa;
        let o = ssl_opts(&s).unwrap().unwrap();
        assert!(o.skip_domain_validation() && !o.accept_invalid_certs());
        s.ca_certificate_path = "/nonexistent/ca.pem".into();
        assert!(matches!(ssl_opts(&s), Err(DbError::InvalidConfiguration(m)) if m.contains("not readable")));
        s.ca_certificate_path.clear();
        s.client_key_path = "/k.pem".into();
        assert!(matches!(ssl_opts(&s), Err(DbError::InvalidConfiguration(_))));
    }

    #[test]
    fn use_detection() {
        assert!(is_use("  use shop") && is_use("USE `x`") && !is_use("user") && !is_use("SELECT 1"));
    }

    #[test]
    fn server_errors_keep_code_and_state() {
        let e = mysql_async::Error::Server(mysql_async::ServerError {
            code: 1792,
            message: "Cannot execute statement in a READ ONLY transaction.".into(),
            state: "25006".into(),
        });
        assert_eq!(
            clean(&e),
            DbError::ServerReadOnly("[1792/25006] Cannot execute statement in a READ ONLY transaction.".into())
        );
    }
}
