//! The driver contract (the Rust counterpart of GokuCore's `DatabaseSession`).
//! Every method that waits on the network bounds itself with `with_deadline` (connect: `connect_timeout_seconds`,
//! everything else: `query_timeout_seconds`) and stops early when `cancel` fires, cancelling the server-side work too.
use crate::drivers;
use crate::error::Result;
use crate::limits::Paging;
use crate::model::{Attribute, ObjectRef, QueryResult, SchemaModel, ServerInfo, Sort};
use crate::profile::{ConnectionProfile, Credentials, DatabaseKind};
use crate::query::Statement;
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
    /// One statement of a `query/run` script, after `query::run` checked it (read-only, rules, confirmation): rows
    /// capped at `max_rows` (the server stops streaming the rest), the affected count, or a message; notices included.
    /// A statement of the other language is `Unsupported`.
    async fn run_statement(
        &self,
        database: &str,
        statement: &Statement,
        max_rows: usize,
        cancel: &CancellationToken,
    ) -> Result<QueryResult>;
    /// Closes the connection(s) and any SSH tunnel. Never fails; idempotent.
    async fn close(&self);
}

/// Opens a session: validates the profile, connects within `connect_timeout_seconds`, and for a read-only profile makes
/// the server session read-only too (PostgreSQL `default_transaction_read_only`, MySQL `SET SESSION TRANSACTION READ ONLY`).
pub async fn connect(
    profile: ConnectionProfile,
    credentials: Credentials,
    cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    profile.validate()?;
    match profile.kind {
        DatabaseKind::Postgres => drivers::postgres::connect(profile, credentials, cancel).await,
        DatabaseKind::Mysql => drivers::mysql::connect(profile, credentials, cancel).await,
        DatabaseKind::Mongodb => drivers::mongo::connect(profile, credentials, cancel).await,
    }
}
