//! PostgreSQL (port of GokuCore's `PostgresSession`): tokio-postgres over rustls, host selection by `targetSession`, one
//! pooled connection per (purpose, database) with at most `MAX_POOLED` open, `statement_timeout` and
//! `default_transaction_read_only` set per connection. A request that times out or is cancelled sends a server-side
//! cancel and drops its connection, so a late cancel can never hit the next statement on a reused connection.
mod format;
mod introspect;
mod tls;

use crate::deadline::with_deadline;
use crate::drivers::lock;
use crate::error::{DbError, Result};
use crate::limits::{Paging, RowCollector};
use crate::model::{Attribute, ObjectKind, ObjectRef, QueryResult, ResultColumn, SchemaModel, ServerInfo, Sort};
use crate::profile::{ConnectionProfile, Credentials, SslMode, TargetSession};
use crate::query::Statement;
use crate::redact;
use crate::session::{DatabaseSession, Endpoint};
use async_trait::async_trait;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::AbortHandle;
use tokio_postgres::types::{FromSql, ToSql, Type};
use tokio_postgres::{CancelToken, Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;
use tokio_util::sync::CancellationToken;

/// GokuCore's `maxPooledConnections`.
const MAX_POOLED: usize = 8;
/// Hard guard against runaway catalogs in metadata queries (Swift's `rows`).
const META_ROW_GUARD: usize = 200_000;
const APPLICATION_NAME: &str = "Goku";

type Params<'a> = &'a [&'a (dyn ToSql + Sync)];
const NO_PARAMS: [&(dyn ToSql + Sync); 0] = [];
/// The connection a request holds, for its `on_timeout`.
type Slot = Arc<Mutex<Option<Arc<Conn>>>>;

/// Connects to the first of `endpoints` (one per profile host, in order) whose role matches `targetSession`.
pub async fn connect(
    profile: ConnectionProfile,
    credentials: Credentials,
    endpoints: Vec<Endpoint>,
    cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    let tls = tls::connector(&profile.ssl)?;
    let first = endpoints.first().cloned().ok_or_else(|| DbError::InvalidConfiguration("No host.".into()))?;
    let shared =
        Shared { password: credentials.password, endpoint: first, tls, state: Mutex::new(State::default()), profile };
    let database = shared.database("").to_owned();
    let passes: &[TargetSession] = match shared.profile.target_session {
        TargetSession::PreferStandby => &[TargetSession::Standby, TargetSession::Any],
        t => &[t],
    };
    let mut failures = Vec::new();
    let mut chosen = None;
    'passes: for &pass in passes {
        for endpoint in &endpoints {
            let host = &endpoint.display;
            match shared.probe(endpoint, &database, pass, cancel).await {
                Ok(Ok(conn)) => {
                    chosen = Some((endpoint.clone(), conn));
                    break 'passes;
                }
                Ok(Err(recovery)) => failures.push(format!(
                    "{host}: {} does not match {}",
                    if recovery { "standby" } else { "primary" },
                    target_name(pass)
                )),
                Err(DbError::Cancelled) => return Err(DbError::Cancelled),
                Err(e) => failures.push(redact::secrets(&format!("{host}: {e}"))),
            }
        }
    }
    let Some((endpoint, conn)) = chosen else {
        if endpoints.len() == 1 && failures.len() == 1 {
            return Err(DbError::ConnectionFailed(failures.remove(0)));
        }
        return Err(DbError::NoSuitableHost(failures.join("\n")));
    };
    // The probe connection is already prepared: keep it as the catalog connection of the default database.
    let shared = Shared { endpoint, ..shared };
    lock(&shared.state).insert(pool_key(Purpose::Meta, &database), Arc::new(conn));
    Ok(Box::new(PostgresSession(Arc::new(shared))))
}

/// Swift's `TargetSession.displayName`.
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

/// `PostgresSession.accepts`.
fn accepts(target: TargetSession, recovery: bool, read_only: bool) -> bool {
    match target {
        TargetSession::Any | TargetSession::PreferStandby => true,
        TargetSession::ReadWrite => !recovery && !read_only,
        TargetSession::ReadOnly => recovery || read_only,
        TargetSession::Primary => !recovery,
        TargetSession::Standby => recovery,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Catalog and introspection queries.
    Meta,
    /// Grid pages and statements, so a slow page doesn't block the tree.
    Data,
}

fn pool_key(purpose: Purpose, database: &str) -> String {
    format!("{}\u{1f}{database}", if purpose == Purpose::Meta { "meta" } else { "data" })
}

/// One physical connection. Requests take `lock` for their whole run: a cancel targets the backend, not a statement,
/// so two requests must never share a connection at the same time.
struct Conn {
    client: Client,
    cancel: CancelToken,
    task: AbortHandle,
    lock: Arc<tokio::sync::Mutex<()>>,
    /// Set by `discard`: a request that was waiting for `lock` must not run here.
    discarded: AtomicBool,
    /// Notices the server sent, for the request that holds `lock`.
    notices: Notices,
}

/// At most this many notices per statement (a `RAISE NOTICE` in a loop must not grow memory).
const MAX_NOTICES: usize = 100;

type Notices = Arc<Mutex<Vec<String>>>;

/// Drives the connection and keeps its notices (`NOTICE`, `WARNING`, …) for `query/run`; aborting the task closes the
/// socket.
fn spawn_connection<S, T>(connection: tokio_postgres::Connection<S, T>, notices: Notices) -> AbortHandle
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut connection = connection;
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(tokio_postgres::AsyncMessage::Notice(n)) => {
                    let mut kept = lock(&notices);
                    if kept.len() < MAX_NOTICES {
                        kept.push(crate::limits::clip(&redact::secrets(&format!("{}: {}", n.severity(), n.message()))));
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
    .abort_handle()
}

#[derive(Default)]
struct State {
    /// key → (connection, last used), LRU-evicted beyond `MAX_POOLED`. An evicted connection that is still running a
    /// request closes when that request drops it.
    pool: HashMap<String, (Arc<Conn>, Instant)>,
    closed: bool,
    type_names: HashMap<u32, String>,
}

impl State {
    fn insert(&mut self, key: String, conn: Arc<Conn>) -> Vec<Arc<Conn>> {
        self.pool.insert(key.clone(), (conn, Instant::now()));
        let mut evicted = Vec::new();
        while self.pool.len() > MAX_POOLED {
            let Some(oldest) =
                self.pool.iter().filter(|(k, _)| **k != key).min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone())
            else {
                break;
            };
            evicted.extend(self.pool.remove(&oldest).map(|(c, _)| c));
        }
        evicted
    }
}

struct Shared {
    profile: ConnectionProfile,
    password: Option<String>,
    /// The host chosen by `connect`; every later connection goes there (no failover, as in GokuCore).
    endpoint: Endpoint,
    tls: MakeRustlsConnect,
    state: Mutex<State>,
}

pub struct PostgresSession(Arc<Shared>);

/// One request's hold on a connection.
struct Lease {
    conn: Arc<Conn>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// A text cell, formatted from the binary value like `PGValueFormatter`.
struct Cell(Option<String>);

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Cell(Some(format::format(raw, ty.oid()))))
    }
    fn from_sql_null(_: &Type) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Cell(None))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

fn cells(row: &tokio_postgres::Row) -> Result<Vec<Option<String>>> {
    (0..row.len()).map(|i| row.try_get::<_, Cell>(i).map(|c| c.0).map_err(clean)).collect()
}

/// GokuCore's `clean`: the server message with detail, hint and SQLSTATE; 25006 (read-only transaction) is
/// `ServerReadOnly`. Other errors keep their cause chain ("error connecting to server: Connection refused").
fn clean(e: tokio_postgres::Error) -> DbError {
    if let Some(db) = e.as_db_error() {
        let mut text = format!("[{}] {}", db.code().code(), db.message());
        if let Some(d) = db.detail() {
            text += &format!("\nDetail: {d}");
        }
        if let Some(h) = db.hint() {
            text += &format!("\nHint: {h}");
        }
        return if db.code().code() == "25006" { DbError::ServerReadOnly(text) } else { DbError::Query(text) };
    }
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        let part = s.to_string();
        if !text.contains(&part) {
            text += &format!(": {part}");
        }
        source = s.source();
    }
    DbError::Query(text)
}

/// `COPY … FROM STDIN` / `TO STDOUT`: the server switches to the copy sub-protocol, which tokio-postgres' query API
/// can't serve (it drops the connection).
fn copies_through_the_client(sql: &str) -> bool {
    let n = crate::query::sql::Normalized::new(sql, crate::query::sql::Dialect::Postgres);
    n.leading_keyword() == "COPY"
        && n.text
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|w| w.eq_ignore_ascii_case("STDIN") || w.eq_ignore_ascii_case("STDOUT"))
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn qualified(o: &ObjectRef) -> String {
    if o.schema.is_empty() {
        quote_ident(&o.name)
    } else {
        format!("{}.{}", quote_ident(&o.schema), quote_ident(&o.name))
    }
}

impl Shared {
    fn database<'a>(&'a self, database: &'a str) -> &'a str {
        if !database.is_empty() {
            database
        } else if !self.profile.database.is_empty() {
            &self.profile.database
        } else {
            "postgres"
        }
    }

    fn query_seconds(&self) -> f64 {
        self.profile.query_timeout_seconds as f64
    }

    /// Opens a connection (no session settings yet) within `connect_timeout_seconds`. A connect that finishes after the
    /// deadline has its connection task aborted, which closes the socket. Behind a tunnel it dials the local end
    /// (`hostaddr`) while TLS still checks the certificate against the profile's host name (`host`).
    async fn open(&self, endpoint: &Endpoint, database: &str, cancel: &CancellationToken) -> Result<Conn> {
        let host = &endpoint.display;
        let seconds = self.profile.connect_timeout_seconds as f64;
        let slot: Arc<Mutex<Option<AbortHandle>>> = Arc::default();
        let late = slot.clone();
        let abort = move || {
            if let Some(h) = lock(&late).take() {
                h.abort();
            }
        };
        with_deadline(seconds, &format!("Connecting to {host}"), cancel, abort, async {
            let mut cfg = Config::new();
            cfg.host(&host.host)
                .port(u16::try_from(endpoint.dial.port).map_err(|_| DbError::InvalidConfiguration("port".into()))?)
                .user(&self.profile.user)
                .dbname(database)
                .application_name(APPLICATION_NAME)
                .connect_timeout(Duration::from_secs_f64(seconds))
                .ssl_mode(match self.profile.ssl.mode {
                    SslMode::Disable => tokio_postgres::config::SslMode::Disable,
                    SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
                    _ => tokio_postgres::config::SslMode::Require,
                });
            if let Some(p) = &self.password {
                cfg.password(p);
            }
            if endpoint.tunnelled {
                let local = endpoint.dial.host.parse().map_err(|_| DbError::InvalidConfiguration("tunnel".into()))?;
                cfg.hostaddr(local);
            }
            let (client, connection) = cfg.connect(self.tls.clone()).await.map_err(clean)?;
            let notices = Notices::default();
            let task = spawn_connection(connection, notices.clone());
            *lock(&slot) = Some(task.clone());
            Ok(Conn {
                cancel: client.cancel_token(),
                client,
                task,
                lock: Default::default(),
                discarded: Default::default(),
                notices,
            })
        })
        .await
    }

    /// Per-connection safety settings, enforced by the server (`PostgresSession.prepare`).
    async fn prepare(&self, conn: &Conn, cancel: &CancellationToken) -> Result<()> {
        let mut sql = format!("SET statement_timeout = {}", self.profile.query_timeout_seconds * 1000);
        if self.profile.read_only {
            sql += "; SET default_transaction_read_only = on";
        }
        let abort = conn.task.clone();
        with_deadline(self.query_seconds(), "Preparing the connection", cancel, move || abort.abort(), async {
            conn.client.batch_execute(&sql).await.map_err(clean)
        })
        .await
    }

    /// Connects to `host` and checks its role against `target`: the prepared connection when it matches, otherwise
    /// whether the server is in recovery (the probe connection is dropped).
    async fn probe(
        &self,
        host: &Endpoint,
        database: &str,
        target: TargetSession,
        cancel: &CancellationToken,
    ) -> Result<Result<Conn, bool>> {
        let conn = self.open(host, database, cancel).await?;
        let abort = conn.task.clone();
        let role =
            with_deadline(self.query_seconds(), "Checking the server role", cancel, move || abort.abort(), async {
                let rows = conn
                    .client
                    .query("SELECT pg_is_in_recovery(), current_setting('transaction_read_only')", &[])
                    .await
                    .map_err(clean)?;
                let row = rows.first().ok_or_else(|| DbError::Query("Could not determine node role".into()))?;
                Ok((row.try_get::<_, bool>(0).map_err(clean)?, row.try_get::<_, String>(1).map_err(clean)? == "on"))
            })
            .await?;
        if !accepts(target, role.0, role.1) {
            return Ok(Err(role.0));
        }
        self.prepare(&conn, cancel).await?;
        Ok(Ok(conn))
    }

    /// The pooled connection for (purpose, database), opened and prepared on first use.
    async fn connection(&self, database: &str, purpose: Purpose, cancel: &CancellationToken) -> Result<Arc<Conn>> {
        let key = pool_key(purpose, database);
        {
            let mut s = lock(&self.state);
            if s.closed {
                return Err(DbError::NotConnected);
            }
            if let Some((conn, used)) = s.pool.get_mut(&key) {
                if !conn.client.is_closed() {
                    *used = Instant::now();
                    return Ok(conn.clone());
                }
                s.pool.remove(&key);
            }
        }
        let conn = self.open(&self.endpoint, database, cancel).await?;
        self.prepare(&conn, cancel).await?;
        let conn = Arc::new(conn);
        let evicted = {
            let mut s = lock(&self.state);
            if s.closed {
                conn.task.abort();
                return Err(DbError::NotConnected);
            }
            if let Some((other, _)) = s.pool.get(&key).filter(|(c, _)| !c.client.is_closed()) {
                return Ok(other.clone()); // a concurrent request opened one first; ours closes when dropped
            }
            s.insert(key, conn.clone())
        };
        drop(evicted);
        Ok(conn)
    }

    /// Takes `conn` out of the pool and stops whatever it runs: server-side cancel first (bounded by the connect
    /// timeout), then the socket is closed.
    fn discard(&self, conn: &Arc<Conn>) {
        conn.discarded.store(true, Ordering::SeqCst);
        lock(&self.state).pool.retain(|_, (c, _)| !Arc::ptr_eq(c, conn));
        let (token, task, tls) = (conn.cancel.clone(), conn.task.clone(), self.tls.clone());
        let seconds = self.profile.connect_timeout_seconds as f64;
        tokio::spawn(async move {
            let never = CancellationToken::new();
            let _ = with_deadline(seconds, "Cancelling the query", &never, || {}, async {
                token.cancel_query(tls).await.map_err(clean)
            })
            .await;
            task.abort();
        });
    }

    /// Takes the (purpose, database) connection for one request. The lease is recorded in `slot`, so the request's
    /// `on_timeout` (`discard_on_timeout`) only stops a server query once there is one.
    async fn lease(
        self: &Arc<Self>,
        database: &str,
        purpose: Purpose,
        cancel: &CancellationToken,
        slot: &Slot,
    ) -> Result<Lease> {
        loop {
            let conn = self.connection(database, purpose, cancel).await?;
            let guard = conn.lock.clone().lock_owned().await;
            if conn.discarded.load(Ordering::SeqCst) {
                continue; // discarded while we waited: take a fresh connection
            }
            *lock(slot) = Some(conn.clone());
            return Ok(Lease { conn, _guard: guard });
        }
    }

    fn discard_on_timeout(self: &Arc<Self>, slot: &Slot) -> impl FnOnce() + use<> {
        let (me, slot) = (self.clone(), slot.clone());
        move || {
            if let Some(conn) = lock(&slot).take() {
                me.discard(&conn);
            }
        }
    }

    /// A metadata query, rows as display strings (Swift's `rows`).
    async fn rows(
        self: &Arc<Self>,
        database: &str,
        sql: &str,
        params: Params<'_>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Vec<Option<String>>>> {
        let db = self.database(database);
        let slot = Slot::default();
        with_deadline(self.query_seconds(), "Metadata query", cancel, self.discard_on_timeout(&slot), async {
            let lease = self.lease(db, Purpose::Meta, cancel, &slot).await?;
            let stream = lease.conn.client.query_raw(sql, params.iter().copied()).await.map_err(clean)?;
            let mut stream = std::pin::pin!(stream);
            let mut out = Vec::new();
            while let Some(row) = stream.next().await {
                out.push(cells(&row.map_err(clean)?)?);
                if out.len() >= META_ROW_GUARD {
                    break;
                }
            }
            Ok(out)
        })
        .await
    }

    /// Runs one statement: rows (capped at `max_rows`) when it describes a result, the affected-row count otherwise.
    /// With `stop_server_on_cap`, a result cut at the cap cancels the rest on the server instead of draining it.
    async fn run(
        self: &Arc<Self>,
        database: &str,
        sql: &str,
        max_rows: usize,
        stop_server_on_cap: bool,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let started = Instant::now();
        let db = self.database(database);
        let slot = Slot::default();
        let seconds = self.query_seconds() + 2.0;
        let mut result = with_deadline(seconds, "Query", cancel, self.discard_on_timeout(&slot), async {
            let lease = self.lease(db, Purpose::Data, cancel, &slot).await?;
            lock(&lease.conn.notices).clear(); // late ones from the previous request
            let client = &lease.conn.client;
            let fail = |e| self.failed(&lease.conn, e);
            let stmt = client.prepare(sql).await.map_err(fail)?;
            if stmt.columns().is_empty() {
                let n = client.execute_raw(&stmt, NO_PARAMS).await.map_err(fail)?;
                let notices = std::mem::take(&mut *lock(&lease.conn.notices));
                return Ok(QueryResult { affected_rows: Some(n), notices, ..QueryResult::default() });
            }
            let oids: Vec<u32> = stmt.columns().iter().map(|c| c.type_().oid()).collect();
            let names = self.type_names(client, &oids).await;
            let columns = stmt
                .columns()
                .iter()
                .zip(names)
                .map(|(c, type_name)| ResultColumn { name: c.name().to_owned(), type_name })
                .collect();
            let mut stream = std::pin::pin!(client.query_raw(&stmt, NO_PARAMS).await.map_err(fail)?);
            let mut collector = RowCollector::new(max_rows);
            while let Some(row) = stream.next().await {
                if collector.is_full() {
                    collector.mark_truncated();
                    if stop_server_on_cap {
                        // The rest is still streaming: stop it on the server instead of reading it all.
                        self.discard(&lease.conn);
                    }
                    break;
                }
                collector.append(cells(&row.map_err(fail)?)?);
            }
            let (rows, truncated) = collector.into_rows();
            let notices = std::mem::take(&mut *lock(&lease.conn.notices));
            Ok(QueryResult { columns, rows, truncated, notices, ..QueryResult::default() })
        })
        .await?;
        result.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }

    /// A statement error. One the server didn't send (an unexpected message, a lost socket) leaves the connection
    /// unusable or out of step, so it is discarded and the next request opens a fresh one.
    fn failed(&self, conn: &Arc<Conn>, e: tokio_postgres::Error) -> DbError {
        if e.as_db_error().is_none() {
            self.discard(conn);
        }
        clean(e)
    }

    /// Display names for result column types: Swift's table, then `format_type` (cached per session; unknown → "oid N").
    async fn type_names(&self, client: &Client, oids: &[u32]) -> Vec<String> {
        let missing: Vec<u32> = {
            let s = lock(&self.state);
            let mut m: Vec<u32> = oids
                .iter()
                .copied()
                .filter(|o| !format::TYPE_NAMES.contains_key(o) && !s.type_names.contains_key(o))
                .collect();
            m.sort_unstable();
            m.dedup();
            m
        };
        if !missing.is_empty() {
            let list = missing.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            let sql = format!("SELECT oid::text, format_type(oid, NULL) FROM pg_type WHERE oid IN ({list})");
            if let Ok(rows) = client.query(&sql, &[]).await {
                let mut s = lock(&self.state);
                for r in rows {
                    if let (Ok(oid), Ok(name)) = (r.try_get::<_, String>(0), r.try_get::<_, String>(1))
                        && let Ok(oid) = oid.parse()
                    {
                        s.type_names.insert(oid, name);
                    }
                }
            }
        }
        let s = lock(&self.state);
        oids.iter()
            .map(|o| {
                format::TYPE_NAMES
                    .get(o)
                    .map(|n| (*n).to_owned())
                    .or_else(|| s.type_names.get(o).cloned())
                    .unwrap_or_else(|| format!("oid {o}"))
            })
            .collect()
    }
}

#[async_trait]
impl DatabaseSession for PostgresSession {
    fn profile(&self) -> &ConnectionProfile {
        &self.0.profile
    }

    async fn server_info(&self, cancel: &CancellationToken) -> Result<ServerInfo> {
        let sql =
            "SELECT version(), current_user, coalesce(host(inet_server_addr()), 'local socket'), pg_is_in_recovery()";
        let rows = self.0.rows("", sql, &[], cancel).await?;
        let row = rows.first().ok_or_else(|| DbError::Query("empty version()".into()))?;
        let full = row[0].clone().unwrap_or_default();
        let product = if full.contains("CockroachDB") {
            "CockroachDB"
        } else if full.contains("Redshift") {
            "Amazon Redshift"
        } else {
            "PostgreSQL"
        };
        Ok(ServerInfo {
            product: product.into(),
            version: full.split(' ').filter(|s| !s.is_empty()).nth(1).map(str::to_owned).unwrap_or(full.clone()),
            connected_host: self.0.endpoint.display.to_string(),
            current_user: row[1].clone().unwrap_or_default(),
            is_read_only_node: row[3].as_deref() == Some("true"),
        })
    }

    async fn databases(&self, cancel: &CancellationToken) -> Result<Vec<String>> {
        let sql = "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY 1";
        Ok(self.0.rows("", sql, &[], cancel).await?.into_iter().filter_map(|mut r| r.remove(0)).collect())
    }

    async fn schemas(&self, database: &str, cancel: &CancellationToken) -> Result<Vec<String>> {
        introspect::schemas(&self.0, database, cancel).await
    }

    async fn objects(&self, database: &str, schema: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>> {
        let sql = "SELECT c.relname, c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                   WHERE n.nspname = $1 AND c.relkind IN ('r','p','v','m','f') ORDER BY 1";
        let rows = self.0.rows(database, sql, &[&schema], cancel).await?;
        let database = self.0.database(database);
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let kind = match r[1].as_deref() {
                    Some("v") => ObjectKind::View,
                    Some("m") => ObjectKind::MaterializedView,
                    _ => ObjectKind::Table,
                };
                Some(ObjectRef { database: database.to_owned(), schema: schema.to_owned(), name: r[0].clone()?, kind })
            })
            .collect())
    }

    async fn columns(&self, object: &ObjectRef, cancel: &CancellationToken) -> Result<Vec<Attribute>> {
        introspect::columns(&self.0, object, cancel).await
    }

    async fn browse(
        &self,
        object: &ObjectRef,
        paging: Paging,
        sort: Option<&Sort>,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let limit = paging.limit.min(self.0.profile.max_rows.max(1) as usize);
        let mut sql = format!("SELECT * FROM {}", qualified(object));
        if let Some(s) = sort {
            sql += &format!(" ORDER BY {}{}", quote_ident(&s.column), if s.descending { " DESC" } else { "" });
        }
        // One row past the page tells the grid there is more.
        sql += &format!(" LIMIT {} OFFSET {}", limit + 1, paging.offset);
        self.0.run(&object.database, &sql, limit, false, cancel).await
    }

    async fn introspect(&self, database: &str, schemas: &[String], cancel: &CancellationToken) -> Result<SchemaModel> {
        introspect::introspect(&self.0, database, schemas, cancel).await
    }

    async fn execute(&self, database: &str, statement: &str, cancel: &CancellationToken) -> Result<QueryResult> {
        self.0.run(database, statement, self.0.profile.max_rows.max(1) as usize, true, cancel).await
    }

    async fn run_statement(
        &self,
        database: &str,
        statement: &Statement,
        max_rows: usize,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let Statement::Sql(sql) = statement else {
            return Err(DbError::Unsupported("Mongo shell statements on a PostgreSQL connection".into()));
        };
        if copies_through_the_client(sql) {
            return Err(DbError::Unsupported(
                "COPY … FROM STDIN / TO STDOUT streams data through the client; use a server file or psql".into(),
            ));
        }
        self.0.run(database, sql, max_rows, true, cancel).await
    }

    async fn close(&self) {
        let conns: Vec<Arc<Conn>> = {
            let mut s = lock(&self.0.state);
            s.closed = true;
            s.pool.drain().map(|(_, (c, _))| c).collect()
        };
        for conn in conns {
            if conn.lock.try_lock().is_err() {
                self.0.discard(&conn); // still running a request: cancel it on the server, then close
            } else if Arc::strong_count(&conn) > 1 {
                conn.task.abort(); // a request is about to use it
            } // otherwise dropping the last handle ends the connection with a Terminate message
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_selection_matches_gokucore() {
        use TargetSession::*;
        // (target, recovery, read_only) → accepted
        let cases = [
            (Any, true, true, true),
            (ReadWrite, false, false, true),
            (ReadWrite, false, true, false),
            (ReadOnly, false, true, true),
            (ReadOnly, false, false, false),
            (Primary, false, true, true),
            (Primary, true, true, false),
            (Standby, true, true, true),
            (Standby, false, false, false),
        ];
        for (t, rec, ro, want) in cases {
            assert_eq!(accepts(t, rec, ro), want, "{t:?} recovery={rec} read_only={ro}");
        }
    }

    #[test]
    fn copy_through_the_client_is_recognised() {
        assert!(
            copies_through_the_client("COPY t FROM STDIN") && copies_through_the_client("copy (select 1) to stdout")
        );
        assert!(
            !copies_through_the_client("COPY t FROM '/tmp/stdin.csv'") && !copies_through_the_client("SELECT 'stdin'")
        );
    }

    #[test]
    fn identifiers_are_quoted() {
        let o = ObjectRef { database: "d".into(), schema: "my\"s".into(), name: "t".into(), kind: ObjectKind::Table };
        assert_eq!(qualified(&o), r#""my""s"."t""#);
        assert_eq!(quote_ident(r#"x"; DROP TABLE y; --"#), r#""x""; DROP TABLE y; --""#);
    }
}
