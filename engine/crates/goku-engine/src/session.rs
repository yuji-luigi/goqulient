//! The driver contract (the Rust counterpart of GokuCore's `DatabaseSession`).
//! Every method that waits on the network bounds itself with `with_deadline` (connect: `connect_timeout_seconds`,
//! everything else: `query_timeout_seconds`) and stops early when `cancel` fires, cancelling the server-side work too.
use crate::drivers;
use crate::error::{DbError, Result};
use crate::limits::Paging;
use crate::model::{Attribute, ObjectRef, QueryResult, SchemaModel, ServerInfo, Sort};
use crate::profile::{ConnectionProfile, Credentials, DatabaseKind, HostPort};
use crate::tunnel::Tunnel;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait DatabaseSession: Send + Sync {
    fn profile(&self) -> &ConnectionProfile;

    async fn server_info(&self, cancel: &CancellationToken) -> Result<ServerInfo>;
    async fn databases(&self, cancel: &CancellationToken) -> Result<Vec<String>>;
    /// PostgreSQL schemas; empty for MySQL and MongoDB.
    async fn schemas(&self, database: &str, cancel: &CancellationToken) -> Result<Vec<String>>;
    /// Tables, views, materialized views, collections, in the order the sidebar shows them.
    async fn objects(&self, database: &str, schema: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>>;
    /// The columns (MongoDB: inferred fields) of one object, for the tree. Cheap: no relationship work.
    async fn columns(&self, object: &ObjectRef, cancel: &CancellationToken) -> Result<Vec<Attribute>>;
    /// One page of grid data, capped by the profile's `max_rows` and `HARD_MAX_ROWS`. Read-only by construction.
    async fn browse(
        &self,
        object: &ObjectRef,
        paging: Paging,
        sort: Option<&Sort>,
        cancel: &CancellationToken,
    ) -> Result<QueryResult>;
    /// The ER model; ends with `SchemaModel::finalize_relationships`.
    async fn introspect(&self, database: &str, schemas: &[String], cancel: &CancellationToken) -> Result<SchemaModel>;
    /// Runs one statement as is (tests and, from M2, the query editor behind the read-only/destructive checks).
    /// The server session still refuses writes on a read-only profile.
    async fn execute(&self, database: &str, statement: &str, cancel: &CancellationToken) -> Result<QueryResult>;
    /// Closes the connection(s) and any SSH tunnel. Never fails; idempotent.
    async fn close(&self);
}

/// Where a driver dials one of the profile's hosts, and how it names that host in messages and TLS checks. They differ
/// behind an SSH tunnel (GokuCore's `Endpoint`): `dial` is the tunnel's local end, `display` the host in the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub dial: HostPort,
    pub display: HostPort,
    pub tunnelled: bool,
}

impl Endpoint {
    pub fn direct(host: &HostPort) -> Self {
        Self { dial: host.clone(), display: host.clone(), tunnelled: false }
    }
}

/// Opens a session: validates the profile, opens the SSH tunnel when `ssh.enabled` (one forwarded local port per host,
/// port of `DatabaseConnector.openWithDetails`), connects within `connect_timeout_seconds`, and for a read-only profile
/// makes the server session read-only too (PostgreSQL `default_transaction_read_only`, MySQL
/// `SET SESSION TRANSACTION READ ONLY`). Closing the session closes the tunnel.
pub async fn connect(
    profile: ConnectionProfile,
    credentials: Credentials,
    cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    profile.validate()?;
    if !profile.ssh.enabled {
        let endpoints = profile.hosts.iter().map(Endpoint::direct).collect();
        return open(profile, credentials, endpoints, cancel).await;
    }
    let tunnel = Tunnel::open(&profile.ssh, &credentials, profile.connect_timeout_seconds, cancel).await?;
    let mut endpoints = Vec::with_capacity(profile.hosts.len());
    for host in &profile.hosts {
        endpoints.push(Endpoint { dial: tunnel.forward(host).await?, display: host.clone(), tunnelled: true });
    }
    match open(profile, credentials, endpoints, cancel).await {
        Ok(inner) => Ok(Box::new(Tunnelled { inner, tunnel })),
        Err(e) => {
            let e = match tunnel.forward_error() {
                Some(why) => with_note(e, &why),
                None => e,
            };
            tunnel.close().await;
            Err(e)
        }
    }
}

async fn open(
    profile: ConnectionProfile,
    credentials: Credentials,
    endpoints: Vec<Endpoint>,
    cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    match profile.kind {
        DatabaseKind::Postgres => drivers::postgres::connect(profile, credentials, endpoints, cancel).await,
        DatabaseKind::Mysql => drivers::mysql::connect(profile, credentials, endpoints, cancel).await,
        DatabaseKind::Mongodb => drivers::mongo::connect(profile, credentials, endpoints, cancel).await,
    }
}

/// The driver only saw a closed socket; the tunnel knows why (e.g. the SSH server couldn't reach the host).
fn with_note(e: DbError, note: &str) -> DbError {
    match e {
        DbError::ConnectionFailed(m) => DbError::ConnectionFailed(format!("{m}\n{note}")),
        DbError::NoSuitableHost(m) => DbError::NoSuitableHost(format!("{m}\n{note}")),
        DbError::Timeout(m) => DbError::Timeout(format!("{m}\n{note}")),
        DbError::Query(m) => DbError::Query(format!("{m}\n{note}")),
        other => other,
    }
}

/// A driver session behind an SSH tunnel: closing it closes the driver's connections first (their goodbyes still go
/// through the tunnel), then the tunnel.
struct Tunnelled {
    inner: Box<dyn DatabaseSession>,
    tunnel: Tunnel,
}

#[async_trait]
impl DatabaseSession for Tunnelled {
    fn profile(&self) -> &ConnectionProfile {
        self.inner.profile()
    }
    async fn server_info(&self, cancel: &CancellationToken) -> Result<ServerInfo> {
        self.inner.server_info(cancel).await
    }
    async fn databases(&self, cancel: &CancellationToken) -> Result<Vec<String>> {
        self.inner.databases(cancel).await
    }
    async fn schemas(&self, database: &str, cancel: &CancellationToken) -> Result<Vec<String>> {
        self.inner.schemas(database, cancel).await
    }
    async fn objects(&self, database: &str, schema: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>> {
        self.inner.objects(database, schema, cancel).await
    }
    async fn columns(&self, object: &ObjectRef, cancel: &CancellationToken) -> Result<Vec<Attribute>> {
        self.inner.columns(object, cancel).await
    }
    async fn browse(
        &self,
        object: &ObjectRef,
        paging: Paging,
        sort: Option<&Sort>,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        self.inner.browse(object, paging, sort, cancel).await
    }
    async fn introspect(&self, database: &str, schemas: &[String], cancel: &CancellationToken) -> Result<SchemaModel> {
        self.inner.introspect(database, schemas, cancel).await
    }
    async fn execute(&self, database: &str, statement: &str, cancel: &CancellationToken) -> Result<QueryResult> {
        self.inner.execute(database, statement, cancel).await
    }
    async fn close(&self) {
        self.inner.close().await;
        self.tunnel.close().await;
    }
}
