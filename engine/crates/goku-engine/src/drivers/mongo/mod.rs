//! MongoDB driver (port of GokuCore's `MongoSession`) on the official `mongodb` crate with rustls.
//! - Timeouts: connect (DNS for SRV, server selection, handshake, auth, `ping`) runs under one `connect_timeout_seconds`
//!   deadline, and server selection gives up a little earlier so its error (which says why) wins. Every later await runs
//!   under `query_timeout_seconds + 5` with `maxTimeMS = query_timeout_seconds` on reads. A timed-out or cancelled
//!   operation is dropped: the pool closes a connection checked in mid-command, and `maxTimeMS` stops the server side.
//! - Read-only is a client check only (as in the app): `execute` refuses commands outside the read allowlist; nothing
//!   else in this session writes.
mod extjson;
pub mod format;
pub mod infer;
pub mod shell;

pub use extjson::parse_document;
use shell::{MongoCall, command_writes};

use crate::deadline::with_deadline;
use crate::drivers::lock;
use crate::error::{DbError, Result};
use crate::limits::{HARD_MAX_ROWS, Paging, RowCollector, clip};
use crate::model::{Attribute, ObjectKind, ObjectRef, QueryResult, ResultColumn, SchemaModel, ServerInfo, Sort};
use crate::profile::{ConnectionProfile, Credentials, MongoReadPreference, SslMode, TopologyMode};
use crate::query::Statement;
use crate::redact;
use crate::session::DatabaseSession;
use async_trait::async_trait;
use bson::{Bson, Document, doc};
use mongodb::error::ErrorKind;
use mongodb::options::{ClientOptions, Credential, ReadPreference, SelectionCriteria, ServerAddress, Tls, TlsOptions};
use mongodb::{Client, Cursor};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub async fn connect(
    profile: ConnectionProfile,
    credentials: Credentials,
    cancel: &CancellationToken,
) -> Result<Box<dyn DatabaseSession>> {
    if profile.ssh.enabled {
        return Err(DbError::Unsupported("SSH tunnels aren't supported by the engine yet.".into()));
    }
    // ring is the engine's only rustls provider; installing it as the process default keeps rustls from guessing.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let connected_to = match profile.mode {
        TopologyMode::Single => profile.hosts.first().map(ToString::to_string).unwrap_or_default(),
        TopologyMode::Srv => profile.srv_host.clone(),
        _ => profile.endpoint_summary(),
    };
    // The client is created inside the deadline; a late or failed one is shut down so its monitors stop.
    let slot: Arc<Mutex<Option<Client>>> = Arc::default();
    let late = slot.clone();
    let criteria = SelectionCriteria::ReadPreference(read_preference(profile.read_preference));
    let result = with_deadline(
        profile.connect_timeout_seconds as f64,
        &format!("Connecting to {connected_to}"),
        cancel,
        move || {
            if let Some(c) = lock(&late).take() {
                tokio::spawn(shutdown(c));
            }
        },
        async {
            let options = client_options(&profile, &credentials).await?;
            let client = Client::with_options(options).map_err(|e| DbError::ConnectionFailed(message(&e)))?;
            *lock(&slot) = Some(client.clone());
            client
                .database("admin")
                .run_command(doc! {"ping": 1})
                .selection_criteria(criteria)
                .await
                .map_err(|e| DbError::ConnectionFailed(message(&e)))?;
            Ok(client)
        },
    )
    .await;
    match result {
        Ok(client) => Ok(Box::new(MongoSession { profile, client, connected_to, closed: AtomicBool::new(false) })),
        Err(e) => {
            let client = lock(&slot).take();
            if let Some(c) = client {
                shutdown(c).await;
            }
            Err(e)
        }
    }
}

async fn shutdown(client: Client) {
    let _ = tokio::time::timeout(Duration::from_secs(2), client.shutdown().immediate(true)).await;
}

fn read_preference(p: MongoReadPreference) -> ReadPreference {
    match p {
        MongoReadPreference::Primary => ReadPreference::Primary,
        MongoReadPreference::PrimaryPreferred => ReadPreference::PrimaryPreferred { options: None },
        MongoReadPreference::Secondary => ReadPreference::Secondary { options: None },
        MongoReadPreference::SecondaryPreferred => ReadPreference::SecondaryPreferred { options: None },
        MongoReadPreference::Nearest => ReadPreference::Nearest { options: None },
    }
}

/// Builds the driver options. Single and multi-host profiles are built field by field (nothing to escape); SRV and URI
/// go through the driver's parser, which resolves the SRV/TXT records.
async fn client_options(p: &ConnectionProfile, c: &Credentials) -> Result<ClientOptions> {
    let connect = Duration::from_secs(p.connect_timeout_seconds.max(1) as u64);
    let invalid = |e: mongodb::error::Error| DbError::InvalidConfiguration(message(&e));
    let mut o = match p.mode {
        TopologyMode::Uri => {
            ClientOptions::parse(inject_password(p.uri.trim(), c.password.as_deref())).await.map_err(invalid)?
        }
        TopologyMode::Srv => {
            if p.srv_host.contains(['/', '?', '@', ',', ':', '#']) || p.srv_host.trim().is_empty() {
                return Err(DbError::InvalidConfiguration("SRV mode needs a bare DNS host name.".into()));
            }
            // The user (never the password) goes into the URI: the driver applies the TXT record's `authSource`
            // (Atlas: admin) only to a credential that the URI carries.
            let user = if p.user.is_empty() { String::new() } else { format!("{}@", percent_encode(&p.user)) };
            let uri = format!("mongodb+srv://{user}{}/{}", p.srv_host.trim(), percent_encode(&p.database));
            ClientOptions::parse(uri).await.map_err(invalid)?
        }
        TopologyMode::Single | TopologyMode::MultiHost => {
            let mut o = ClientOptions::default();
            o.hosts = p
                .hosts
                .iter()
                .map(|h| ServerAddress::Tcp { host: h.host.clone(), port: Some(h.port as u16) })
                .collect();
            o.direct_connection = Some(p.mode == TopologyMode::Single);
            if p.mode == TopologyMode::MultiHost && !p.replica_set.is_empty() {
                o.repl_set_name = Some(p.replica_set.clone());
            }
            o
        }
    };
    let uri = p.mode == TopologyMode::Uri;
    // A URI keeps its own TLS settings (like the app); the profile's apply to the other modes.
    if !uri && let Some(tls) = tls(p)? {
        o.tls = Some(tls);
    }
    apply_credential(p, c, &mut o);
    if !uri || o.connect_timeout.is_none() {
        o.connect_timeout = Some(connect);
    }
    let selection = connect.mul_f64(0.9).max(Duration::from_millis(500));
    o.server_selection_timeout = Some(o.server_selection_timeout.map_or(selection, |t| t.min(selection)));
    if !uri || o.selection_criteria.is_none() {
        o.selection_criteria = Some(SelectionCriteria::ReadPreference(read_preference(p.read_preference)));
    }
    if o.default_database.is_none() && !p.database.is_empty() {
        o.default_database = Some(p.database.clone());
    }
    o.app_name.get_or_insert_with(|| "GokuEngine".into());
    o.max_pool_size = Some(o.max_pool_size.unwrap_or(8).min(8));
    Ok(o)
}

/// Same mapping as the app: `prefer` = no TLS (except SRV, where the driver's default, TLS on, stays), `require` = TLS
/// without verification, `verifyCA`/`verifyFull` = full verification. rustls can't skip only the host name check, so
/// `verifyCA` is verified like `verifyFull` (stricter than the app). The driver takes the client certificate and key as
/// one PEM file.
fn tls(p: &ConnectionProfile) -> Result<Option<Tls>> {
    let bad = |m: String| Err(DbError::InvalidConfiguration(m));
    let mut t = TlsOptions::default();
    match p.ssl.mode {
        SslMode::Disable | SslMode::Prefer => return Ok((p.mode != TopologyMode::Srv).then_some(Tls::Disabled)),
        SslMode::Require => t.allow_invalid_certificates = Some(true),
        SslMode::VerifyCa | SslMode::VerifyFull => {
            let ca = expand(&p.ssl.ca_certificate_path);
            if !ca.as_os_str().is_empty() {
                if std::fs::metadata(&ca).is_err() {
                    return bad(format!("CA certificate not readable: {}", ca.display()));
                }
                t.ca_file_path = Some(ca);
            }
        }
    }
    let (cert, key) = (expand(&p.ssl.client_certificate_path), expand(&p.ssl.client_key_path));
    match (cert.as_os_str().is_empty(), key.as_os_str().is_empty()) {
        (true, true) => {}
        (false, false) if cert == key => t.cert_key_file_path = Some(cert),
        (false, false) => {
            return bad(
                "MongoDB needs the client certificate and its key in one PEM file: set both paths to that file.".into(),
            );
        }
        _ => return bad("Client certificate and key must both be set.".into()),
    }
    Ok(Some(Tls::Enabled(t)))
}

fn expand(path: &str) -> PathBuf {
    let p = path.trim();
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default();
            PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(p),
    }
}

/// URI mode: the URI decides user and password (the stored one was injected by `inject_password`); only a missing
/// auth source comes from the profile. Other modes: the profile's user, auth source = `authSource`, else (unless the SRV
/// TXT record set one) the database, else admin.
fn apply_credential(p: &ConnectionProfile, c: &Credentials, o: &mut ClientOptions) {
    if p.mode == TopologyMode::Uri {
        if let Some(cred) = o.credential.as_mut()
            && cred.username.is_some()
            && cred.source.is_none()
            && !p.auth_source.is_empty()
        {
            cred.source = Some(p.auth_source.clone());
        }
        return;
    }
    if p.user.is_empty() {
        return;
    }
    let cred = o.credential.get_or_insert_with(Credential::default);
    cred.username = Some(p.user.clone());
    cred.password = Some(c.password.clone().unwrap_or_default());
    if !p.auth_source.is_empty() {
        cred.source = Some(p.auth_source.clone());
    } else if cred.source.is_none() {
        cred.source = Some(if p.database.is_empty() { "admin".into() } else { p.database.clone() });
    }
}

/// Port of the app's URI password injection: the extension stores a URI's password apart (SecretStorage) and sends the
/// URI as `scheme://user@host…`; the password goes back, percent-encoded, only when the URI has a user and no password.
fn inject_password(uri: &str, password: Option<&str>) -> String {
    let Some(pw) = password.filter(|p| !p.is_empty()) else { return uri.to_owned() };
    let Some(start) = uri.find("://").map(|i| i + 3) else { return uri.to_owned() };
    let end = uri[start..].find(['/', '?']).map_or(uri.len(), |i| start + i);
    match uri[start..end].rfind('@').map(|i| start + i) {
        Some(at) if !uri[start..at].contains(':') => format!("{}:{}{}", &uri[..at], percent_encode(pw), &uri[at..]),
        _ => uri.to_owned(),
    }
}

/// RFC 3986 unreserved characters stay, every other byte becomes `%XX`.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Driver error → text without secrets, server errors as `message (code N)` like the app.
fn message(e: &mongodb::error::Error) -> String {
    let m = match e.kind.as_ref() {
        ErrorKind::Command(c) => format!("{} (code {})", c.message, c.code),
        _ => e.to_string(),
    };
    redact::secrets(&m)
}

fn clean(e: mongodb::error::Error) -> DbError {
    DbError::Query(message(&e))
}

/// Reads at most `max` documents; dropping the cursor afterwards kills the server cursor.
async fn collect(mut cursor: Cursor<Document>, max: usize) -> mongodb::error::Result<Vec<Document>> {
    let mut out = Vec::new();
    while out.len() < max && cursor.advance().await? {
        out.push(cursor.deserialize_current()?);
    }
    Ok(out)
}

fn int(v: Option<&Bson>) -> Option<i64> {
    match v? {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.is_finite() => Some(*d as i64),
        _ => None,
    }
}

pub struct MongoSession {
    profile: ConnectionProfile,
    client: Client,
    connected_to: String,
    closed: AtomicBool,
}

impl MongoSession {
    fn max_time(&self) -> Duration {
        Duration::from_secs(self.profile.query_timeout_seconds.max(1) as u64)
    }

    fn read_criteria(&self) -> SelectionCriteria {
        SelectionCriteria::ReadPreference(read_preference(self.profile.read_preference))
    }

    /// Every operation: refused after `close`, bounded by the query deadline, dropped on timeout or cancel.
    async fn run<T>(
        &self,
        what: &str,
        cancel: &CancellationToken,
        op: impl Future<Output = mongodb::error::Result<T>>,
    ) -> Result<T> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(DbError::NotConnected);
        }
        let seconds = self.profile.query_timeout_seconds as f64 + 5.0;
        with_deadline(seconds, what, cancel, || {}, async { op.await.map_err(clean) }).await
    }

    async fn command(&self, database: &str, cmd: Document, cancel: &CancellationToken) -> Result<Document> {
        let db = self.client.database(database);
        self.run("MongoDB command", cancel, async {
            db.run_command(cmd).selection_criteria(self.read_criteria()).await
        })
        .await
    }

    /// A command for `query/run`: writes go to the primary, reads follow the profile's read preference. Fails on
    /// `ok: 0` and, like the app, on the first `writeErrors` entry (the server answers those with `ok: 1`).
    async fn run_command(
        &self,
        database: &str,
        cmd: Document,
        writable: bool,
        cancel: &CancellationToken,
    ) -> Result<Document> {
        let db = self.client.database(database);
        let criteria =
            if writable { SelectionCriteria::ReadPreference(ReadPreference::Primary) } else { self.read_criteria() };
        let reply = self
            .run("MongoDB command", cancel, async { db.run_command(cmd).selection_criteria(criteria).await })
            .await?;
        if let Ok(errors) = reply.get_array("writeErrors")
            && let Some(first) = errors.first().and_then(Bson::as_document)
        {
            let code = int(first.get("code")).map(|c| format!(" (code {c})")).unwrap_or_default();
            return Err(DbError::Query(redact::secrets(
                &(first.get_str("errmsg").unwrap_or("Write error").to_owned() + &code),
            )));
        }
        Ok(reply)
    }

    /// The first batch of a cursor reply, and whether the server had more; a cursor left open is killed.
    async fn first_batch(&self, reply: &Document, database: &str, cancel: &CancellationToken) -> (Vec<Document>, bool) {
        let Ok(cursor) = reply.get_document("cursor") else { return (Vec::new(), false) };
        let docs: Vec<Document> = cursor
            .get_array("firstBatch")
            .map(|b| b.iter().filter_map(|d| d.as_document().cloned()).collect())
            .unwrap_or_default();
        let id = int(cursor.get("id")).unwrap_or(0);
        if id != 0
            && let Some((_, coll)) = cursor.get_str("ns").ok().and_then(|ns| ns.split_once('.'))
        {
            let kill = doc! {"killCursors": coll, "cursors": [id]};
            let _ = self.command(database, kill, cancel).await; // best effort, like the app's `try?`
        }
        (docs, id != 0)
    }

    /// Port of `MongoSession.run`: one parsed shell call, capped at `cap` documents (reads ask for `cap + 1` to tell
    /// truncation and carry `maxTimeMS`).
    async fn run_call(
        &self,
        call: &MongoCall,
        db: &str,
        cap: usize,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let started = Instant::now();
        let cap = cap.clamp(1, HARD_MAX_ROWS);
        let max_time_ms = self.max_time().as_millis() as i64;
        match call {
            MongoCall::Use(name) => Ok(message_result(None, format!("switched to db {name}"), started)),
            MongoCall::GetName => Ok(column_result("db", "string", vec![db.to_owned()], started)),
            MongoCall::ShowDatabases => {
                let cmd = doc! {"listDatabases": 1, "authorizedDatabases": true};
                let reply = self.run_command("admin", cmd, false, cancel).await?;
                let docs: Vec<Document> = reply
                    .get_array("databases")
                    .map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect())
                    .unwrap_or_default();
                Ok(documents_result(&docs, cap, false, started))
            }
            MongoCall::ShowCollections => {
                let names = self.list_objects(db, cancel).await?.into_iter().map(|o| o.name).collect();
                Ok(column_result("collection", "string", names, started))
            }
            MongoCall::Command(d) | MongoCall::AdminCommand(d) => {
                let target = call.database(db);
                let mut cmd = d.clone();
                if let Some(name) = cmd.keys().next()
                    && ["find", "aggregate", "count", "distinct"].contains(&name.as_str())
                    && !cmd.contains_key("maxTimeMS")
                {
                    cmd.insert("maxTimeMS", max_time_ms);
                }
                let reply = self.run_command(target, cmd, call.is_write(), cancel).await?;
                if reply.get_document("cursor").is_ok() {
                    let (docs, more) = self.first_batch(&reply, target, cancel).await;
                    return Ok(documents_result(&docs, cap, more, started));
                }
                Ok(documents_result(std::slice::from_ref(&reply), cap, false, started))
            }
            MongoCall::Find(f) => {
                let limit = f.limit.map_or(cap as i64, |l| if l == 0 { cap as i64 } else { l }).min(cap as i64);
                let mut cmd = doc! {"find": f.collection.clone(), "filter": f.filter.clone()};
                if let Some(p) = f.projection.as_ref().filter(|p| !p.is_empty()) {
                    cmd.insert("projection", p.clone());
                }
                if let Some(s) = f.sort.as_ref().filter(|s| !s.is_empty()) {
                    cmd.insert("sort", s.clone());
                }
                if f.skip > 0 {
                    cmd.insert("skip", f.skip);
                }
                cmd.insert("limit", limit + 1);
                cmd.insert("batchSize", limit + 1);
                cmd.insert("singleBatch", true);
                cmd.insert("maxTimeMS", max_time_ms);
                if f.explain {
                    let explain = doc! {"explain": cmd, "verbosity": "queryPlanner"};
                    let reply = self.run_command(db, explain, false, cancel).await?;
                    return Ok(documents_result(std::slice::from_ref(&reply), cap, false, started));
                }
                let reply = self.run_command(db, cmd, false, cancel).await?;
                let (docs, _) = self.first_batch(&reply, db, cancel).await;
                let shown = usize::try_from(limit).unwrap_or(0).min(docs.len());
                let mut r = documents_result(docs.get(..shown).unwrap_or_default(), cap, false, started);
                // A `.limit(n)` of the user's own is a complete answer.
                r.truncated = docs.len() > shown && f.limit.is_none();
                Ok(r)
            }
            MongoCall::Aggregate { collection, pipeline } => {
                let cmd = doc! {
                    "aggregate": collection.clone(), "pipeline": pipeline.clone(),
                    "cursor": {"batchSize": cap as i64 + 1}, "maxTimeMS": max_time_ms,
                };
                let reply = self.run_command(db, cmd, call.is_write(), cancel).await?;
                let (docs, more) = self.first_batch(&reply, db, cancel).await;
                Ok(documents_result(&docs, cap, more, started))
            }
            MongoCall::Count { collection, filter } => {
                let pipeline = [doc! {"$match": filter.clone()}, doc! {"$count": "n"}];
                let cmd = doc! {"aggregate": collection.clone(), "pipeline": pipeline, "cursor": {}, "maxTimeMS": max_time_ms};
                let reply = self.run_command(db, cmd, false, cancel).await?;
                let (docs, _) = self.first_batch(&reply, db, cancel).await;
                let n = int(docs.first().and_then(|d| d.get("n"))).unwrap_or(0);
                Ok(column_result("count", "int", vec![n.to_string()], started))
            }
            MongoCall::Insert { collection, documents } => {
                if documents.is_empty() {
                    return Err(DbError::InvalidConfiguration("Nothing to insert.".into()));
                }
                let with_ids: Vec<Document> = documents
                    .iter()
                    .map(|d| {
                        if d.contains_key("_id") {
                            return d.clone();
                        }
                        let mut out = doc! {"_id": bson::oid::ObjectId::new()};
                        out.extend(d.clone());
                        out
                    })
                    .collect();
                let one_id = match with_ids.as_slice() {
                    [only] => only.get("_id").map(|id| format::literal(id, None)),
                    _ => None,
                };
                let cmd = doc! {"insert": collection.clone(), "documents": with_ids, "ordered": true};
                let reply = self.run_command(db, cmd, true, cancel).await?;
                let message = one_id.map_or_else(|| "inserted".to_owned(), |id| format!("inserted _id {id}"));
                Ok(message_result(int(reply.get("n")), message, started))
            }
            MongoCall::Update { collection, filter, update, multi, upsert } => {
                let u = doc! {"q": filter.clone(), "u": update.clone(), "multi": *multi, "upsert": *upsert};
                let reply =
                    self.run_command(db, doc! {"update": collection.clone(), "updates": [u]}, true, cancel).await?;
                let (matched, modified) = (int(reply.get("n")).unwrap_or(0), int(reply.get("nModified")).unwrap_or(0));
                Ok(message_result(Some(modified), format!("matched {matched}, modified {modified}"), started))
            }
            MongoCall::Delete { collection, filter, multi } => {
                let d = doc! {"q": filter.clone(), "limit": if *multi { 0 } else { 1 }};
                let reply =
                    self.run_command(db, doc! {"delete": collection.clone(), "deletes": [d]}, true, cancel).await?;
                Ok(message_result(int(reply.get("n")), "deleted".into(), started))
            }
            MongoCall::RenameCollection { from, to, drop_target } => {
                let cmd = doc! {"renameCollection": format!("{db}.{from}"), "to": format!("{db}.{to}"), "dropTarget": *drop_target};
                self.run_command("admin", cmd, true, cancel).await?;
                Ok(message_result(None, "renamed".into(), started))
            }
        }
    }

    async fn list_objects(&self, database: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>> {
        let db = self.client.database(database);
        let cmd = doc! {"listCollections": 1, "nameOnly": false, "authorizedCollections": true};
        let docs = self
            .run("Listing collections", cancel, async {
                collect(db.run_cursor_command(cmd).selection_criteria(self.read_criteria()).await?, HARD_MAX_ROWS).await
            })
            .await?;
        let mut objects: Vec<ObjectRef> = docs
            .iter()
            .filter_map(|d| {
                let name = d.get_str("name").ok()?;
                let kind =
                    if d.get_str("type").ok() == Some("view") { ObjectKind::View } else { ObjectKind::Collection };
                Some(ObjectRef { database: database.to_owned(), schema: String::new(), name: name.to_owned(), kind })
            })
            .collect();
        objects.sort_by(|a, b| (a.name.starts_with("system."), &a.name).cmp(&(b.name.starts_with("system."), &b.name)));
        Ok(objects)
    }

    async fn count(&self, database: &str, collection: &str, cancel: &CancellationToken) -> Result<Option<i64>> {
        let max_time_ms = self.max_time().as_millis() as i64;
        let cmd = doc! {"count": collection, "maxTimeMS": max_time_ms};
        Ok(int(self.command(database, cmd, cancel).await?.get("n")))
    }

    async fn list_indexes(
        &self,
        database: &str,
        collection: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Document>> {
        let db = self.client.database(database);
        self.run("Listing indexes", cancel, async {
            let cmd = doc! {"listIndexes": collection};
            collect(db.run_cursor_command(cmd).selection_criteria(self.read_criteria()).await?, 1000).await
        })
        .await
    }

    async fn first_documents(
        &self,
        database: &str,
        collection: &str,
        n: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<Document>> {
        let coll = self.client.database(database).collection::<Document>(collection);
        self.run("Reading documents", cancel, async {
            collect(coll.find(doc! {}).limit(n as i64).batch_size(n as u32).max_time(self.max_time()).await?, n).await
        })
        .await
    }

    /// Up to `n` documents for inference. A collection that has no more than `n` (by its `count`) is read whole in
    /// natural order, which gives the same document set as `$sample` but in a stable order, so field order and value
    /// probes don't change between runs. Larger ones use `$sample` (random, as in the app), falling back to `find`.
    async fn sample(
        &self,
        database: &str,
        collection: &str,
        n: usize,
        count: Option<i64>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Document>> {
        if count.is_some_and(|c| c <= n as i64)
            && let Some(docs) = swallow(self.first_documents(database, collection, n, cancel).await)?
        {
            return Ok(docs);
        }
        let coll = self.client.database(database).collection::<Document>(collection);
        let sampled = self
            .run("Sampling documents", cancel, async {
                let size = n as i64;
                let pipeline = [doc! {"$sample": {"size": size}}];
                collect(coll.aggregate(pipeline).batch_size(n as u32).max_time(self.max_time()).await?, n).await
            })
            .await;
        match swallow(sampled)? {
            Some(docs) => Ok(docs),
            None => Ok(swallow(self.first_documents(database, collection, n, cancel).await)?.unwrap_or_default()),
        }
    }
}

/// Errors that the app ignores (sampling, indexes, probes, `connectionStatus`) become `None`; cancelling still stops.
fn swallow<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(DbError::Cancelled) => Err(DbError::Cancelled),
        Err(_) => Ok(None),
    }
}

/// Grid result in the app's shape: columns and cells from `format::grid`, `documents` as indented shell text, both clipped.
fn documents_result(docs: &[Document], cap: usize, more: bool, started: Instant) -> QueryResult {
    let shown = &docs[..docs.len().min(cap)];
    let (columns, rows) = format::grid(shown);
    let mut collector = RowCollector::new(cap);
    for row in rows {
        collector.append(row);
    }
    if more || docs.len() > cap {
        collector.mark_truncated();
    }
    let (rows, truncated) = collector.into_rows();
    QueryResult {
        columns,
        rows,
        truncated,
        documents: Some(shown.iter().map(|d| clip(&format::document_literal(d, Some(2)))).collect()),
        elapsed_ms: started.elapsed().as_millis() as u64,
        ..QueryResult::default()
    }
}

/// A one-column result (`show collections`, `db.getName()`, a count).
fn column_result(name: &str, type_name: &str, values: Vec<String>, started: Instant) -> QueryResult {
    QueryResult {
        columns: vec![ResultColumn { name: name.into(), type_name: type_name.into() }],
        rows: values.into_iter().map(|v| vec![Some(clip(&v))]).collect(),
        elapsed_ms: started.elapsed().as_millis() as u64,
        ..QueryResult::default()
    }
}

fn message_result(affected: Option<i64>, message: String, started: Instant) -> QueryResult {
    QueryResult {
        affected_rows: affected.map(|n| n.max(0) as u64),
        message: Some(message),
        elapsed_ms: started.elapsed().as_millis() as u64,
        ..QueryResult::default()
    }
}

#[async_trait]
impl DatabaseSession for MongoSession {
    fn profile(&self) -> &ConnectionProfile {
        &self.profile
    }

    async fn server_info(&self, cancel: &CancellationToken) -> Result<ServerInfo> {
        let build = self.command("admin", doc! {"buildInfo": 1}, cancel).await?;
        let hello = self.command("admin", doc! {"hello": 1}, cancel).await?;
        let status = swallow(self.command("admin", doc! {"connectionStatus": 1}, cancel).await)?;
        let users: Vec<&str> = status
            .as_ref()
            .and_then(|s| s.get_document("authInfo").ok())
            .and_then(|a| a.get_array("authenticatedUsers").ok())
            .map(|u| u.iter().filter_map(|u| u.as_document()?.get_str("user").ok()).collect())
            .unwrap_or_default();
        let writable = hello.get_bool("isWritablePrimary").or_else(|_| hello.get_bool("ismaster")).unwrap_or(false);
        Ok(ServerInfo {
            product: if hello.get_str("msg").ok() == Some("isdbgrid") { "MongoDB (mongos)" } else { "MongoDB" }.into(),
            version: build.get_str("version").unwrap_or("?").into(),
            connected_host: hello.get_str("me").map(str::to_owned).unwrap_or_else(|_| self.connected_to.clone()),
            current_user: users.join(", "),
            is_read_only_node: !writable,
        })
    }

    async fn databases(&self, cancel: &CancellationToken) -> Result<Vec<String>> {
        let cmd = doc! {"listDatabases": 1, "nameOnly": true, "authorizedDatabases": true};
        match self.command("admin", cmd, cancel).await {
            Ok(r) => {
                let mut names: Vec<String> = r
                    .get_array("databases")
                    .map(|a| a.iter().filter_map(|d| Some(d.as_document()?.get_str("name").ok()?.to_owned())).collect())
                    .unwrap_or_default();
                let system = |n: &str| ["admin", "config", "local"].contains(&n);
                names.sort_by(|a, b| (system(a), a).cmp(&(system(b), b)));
                Ok(names)
            }
            // A user limited to one database may not list them: show the profile's database, like the app.
            Err(e) if !matches!(e, DbError::Cancelled | DbError::NotConnected) && !self.profile.database.is_empty() => {
                Ok(vec![self.profile.database.clone()])
            }
            Err(e) => Err(e),
        }
    }

    async fn schemas(&self, _database: &str, _cancel: &CancellationToken) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn objects(&self, database: &str, _schema: &str, cancel: &CancellationToken) -> Result<Vec<ObjectRef>> {
        self.list_objects(database, cancel).await
    }

    /// Top-level fields of a sample (no relationships, no indexes).
    async fn columns(&self, object: &ObjectRef, cancel: &CancellationToken) -> Result<Vec<Attribute>> {
        let count = swallow(self.count(&object.database, &object.name, cancel).await)?.flatten();
        let docs = self.sample(&object.database, &object.name, infer::SAMPLE_SIZE, count, cancel).await?;
        let fields = infer::Fields::from_documents(&docs);
        Ok(fields.attributes().into_iter().filter(|a| !a.name.contains('.') && !a.name.contains("[]")).collect())
    }

    async fn browse(
        &self,
        object: &ObjectRef,
        paging: Paging,
        sort: Option<&Sort>,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let started = Instant::now();
        let cap = paging.limit.min(self.profile.max_rows.max(1) as usize).clamp(1, HARD_MAX_ROWS);
        let coll = self.client.database(&object.database).collection::<Document>(&object.name);
        let docs = self
            .run("Reading documents", cancel, async {
                let mut find = coll
                    .find(doc! {})
                    .skip(paging.offset as u64)
                    .limit(cap as i64 + 1)
                    .batch_size(cap as u32 + 1)
                    .max_time(self.max_time());
                if let Some(s) = sort {
                    let mut by = Document::new();
                    by.insert(s.column.clone(), if s.descending { -1 } else { 1 });
                    find = find.sort(by);
                }
                collect(find.await?, cap + 1).await
            })
            .await?;
        Ok(documents_result(&docs, cap, false, started))
    }

    async fn introspect(&self, database: &str, _schemas: &[String], cancel: &CancellationToken) -> Result<SchemaModel> {
        self.introspect_database(database, cancel).await
    }

    /// One command document in Extended JSON (`db.runCommand` style), e.g. `{"find": "users", "filter": {}}`. Writes are
    /// refused on a read-only profile or a database with read-only rules (the shell parser and its per-collection
    /// targets come with the query editor).
    async fn execute(&self, database: &str, statement: &str, cancel: &CancellationToken) -> Result<QueryResult> {
        let started = Instant::now();
        let mut cmd = parse_document(statement)?;
        let Some(name) = cmd.keys().next().cloned() else {
            return Err(DbError::Parse("Empty command document.".into()));
        };
        let database = if database.is_empty() {
            if self.profile.database.is_empty() { "test" } else { &self.profile.database }
        } else {
            database
        };
        let writes = command_writes(&cmd);
        if writes {
            if self.profile.read_only {
                return Err(DbError::ReadOnly);
            }
            if let Some(rule) = self.profile.read_only_rules.iter().find(|r| r.database == database) {
                let what =
                    if rule.table.is_empty() { database.to_owned() } else { format!("{database}.{}", rule.table) };
                return Err(DbError::ReadOnlyObject(what));
            }
        }
        if ["find", "aggregate", "count", "distinct"].contains(&name.as_str()) && !cmd.contains_key("maxTimeMS") {
            cmd.insert("maxTimeMS", self.max_time().as_millis() as i64);
        }
        let criteria =
            if writes { SelectionCriteria::ReadPreference(ReadPreference::Primary) } else { self.read_criteria() };
        let db = self.client.database(database);
        let reply = self
            .run("MongoDB command", cancel, async { db.run_command(cmd).selection_criteria(criteria).await })
            .await?;
        let cap = self.profile.max_rows.clamp(1, HARD_MAX_ROWS as i64) as usize;
        let Ok(cursor) = reply.get_document("cursor") else {
            return Ok(documents_result(std::slice::from_ref(&reply), cap, false, started));
        };
        let docs: Vec<Document> = cursor
            .get_array("firstBatch")
            .map(|b| b.iter().filter_map(|d| d.as_document().cloned()).collect())
            .unwrap_or_default();
        let id = int(cursor.get("id")).unwrap_or(0);
        if id != 0
            && let Some((db_name, coll)) = cursor.get_str("ns").ok().and_then(|ns| ns.split_once('.'))
        {
            let kill = doc! {"killCursors": coll, "cursors": [id]};
            let _ = self.command(db_name, kill, cancel).await;
        }
        Ok(documents_result(&docs, cap, id != 0, started))
    }

    async fn run_statement(
        &self,
        database: &str,
        statement: &Statement,
        max_rows: usize,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let Statement::Mongo(call) = statement else {
            return Err(DbError::Unsupported("SQL on a MongoDB connection".into()));
        };
        self.run_call(call, database, max_rows, cancel).await
    }

    async fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            shutdown(self.client.clone()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{DatabaseKind, HostPort};

    fn profile(mode: TopologyMode) -> ConnectionProfile {
        let mut p = ConnectionProfile::new("m", DatabaseKind::Mongodb);
        p.mode = mode;
        p
    }

    #[tokio::test]
    async fn options_for_hosts() {
        let mut p = profile(TopologyMode::MultiHost);
        p.hosts = vec![HostPort::new("a", 27017), HostPort::new("b", 27018)];
        p.replica_set = "rs0".into();
        p.user = "app".into();
        p.database = "shop".into();
        p.read_preference = MongoReadPreference::SecondaryPreferred;
        p.connect_timeout_seconds = 4;
        let c = Credentials { password: Some("pw".into()), ..Credentials::default() };
        let o = client_options(&p, &c).await.unwrap();
        assert_eq!(o.hosts.len(), 2);
        assert_eq!((o.direct_connection, o.repl_set_name.as_deref()), (Some(false), Some("rs0")));
        assert_eq!(o.tls, Some(Tls::Disabled), "prefer means no TLS, like the app");
        let cred = o.credential.unwrap();
        assert_eq!(
            (cred.username.as_deref(), cred.password.as_deref(), cred.source.as_deref()),
            (Some("app"), Some("pw"), Some("shop"))
        );
        assert_eq!(o.connect_timeout, Some(Duration::from_secs(4)));
        assert!(o.server_selection_timeout.unwrap() < Duration::from_secs(4));
        assert!(matches!(
            o.selection_criteria,
            Some(SelectionCriteria::ReadPreference(ReadPreference::SecondaryPreferred { .. }))
        ));
        p.mode = TopologyMode::Single;
        p.hosts.truncate(1);
        p.auth_source = "admin".into();
        p.ssl.mode = SslMode::Require;
        let o = client_options(&p, &c).await.unwrap();
        assert_eq!((o.direct_connection, o.repl_set_name), (Some(true), None));
        assert_eq!(o.credential.unwrap().source.as_deref(), Some("admin"));
        let Some(Tls::Enabled(t)) = o.tls else { panic!("TLS expected") };
        assert_eq!(t.allow_invalid_certificates, Some(true));
    }

    #[tokio::test]
    async fn options_for_uri() {
        let mut p = profile(TopologyMode::Uri);
        p.uri = "mongodb://app@h1,h2/db?replicaSet=rs&connectTimeoutMS=2000&readPreference=secondary".into();
        p.user = "ignored".into();
        p.ssl.mode = SslMode::VerifyFull;
        let pw = "p@ss:w/rd%41";
        let c = Credentials { password: Some(pw.into()), ..Credentials::default() };
        let o = client_options(&p, &c).await.unwrap();
        let cred = o.credential.unwrap();
        assert_eq!((cred.username.as_deref(), cred.password.as_deref()), (Some("app"), Some(pw)));
        assert_eq!(o.connect_timeout, Some(Duration::from_secs(2)), "the URI's own timeout is kept");
        assert!(matches!(
            o.selection_criteria,
            Some(SelectionCriteria::ReadPreference(ReadPreference::Secondary { .. }))
        ));
        assert_eq!(o.tls, None, "a URI keeps its own TLS settings");
        p.uri = "mongodb://app:own@h/".into();
        let cred = client_options(&p, &c).await.unwrap().credential.unwrap();
        assert_eq!(cred.password.as_deref(), Some("own"), "a URI's own password is left alone");
        p.uri = "mongodb://h/".into();
        p.auth_source = "admin".into();
        assert!(client_options(&p, &c).await.unwrap().credential.is_none(), "no user in the URI = no auth");
        p.uri = "mongodb://u@h:99999/?tlsCertificateKeyFilePassword=sekrit".into();
        let e = client_options(&p, &c).await.unwrap_err().to_string();
        assert!(!e.contains("sekrit") && !e.contains("p%40ss") && !e.contains(pw), "{e}");
    }

    #[test]
    fn password_injection() {
        let pw = Some("p@ss:w/rd%");
        assert_eq!(inject_password("mongodb://u@h:1/db?x=1", pw), "mongodb://u:p%40ss%3Aw%2Frd%25@h:1/db?x=1");
        assert_eq!(
            inject_password("mongodb+srv://u@c.example.net/", pw),
            "mongodb+srv://u:p%40ss%3Aw%2Frd%25@c.example.net/"
        );
        assert_eq!(inject_password("mongodb://u:own@h/", pw), "mongodb://u:own@h/", "own password kept");
        assert_eq!(inject_password("mongodb://h/?authMechanism=X@Y", pw), "mongodb://h/?authMechanism=X@Y", "no user");
        assert_eq!(inject_password("mongodb://u@h/", None), "mongodb://u@h/");
        assert_eq!(inject_password("mongodb://u@h/", Some("")), "mongodb://u@h/");
    }

    #[test]
    fn tls_modes() {
        let mut p = profile(TopologyMode::Srv);
        p.ssl.mode = SslMode::Disable;
        assert_eq!(tls(&p).unwrap(), None, "SRV keeps TLS on");
        p.ssl.mode = SslMode::VerifyCa;
        p.ssl.ca_certificate_path = "/nonexistent/ca.pem".into();
        assert!(matches!(tls(&p), Err(DbError::InvalidConfiguration(_))));
        p.ssl.ca_certificate_path.clear();
        p.ssl.client_certificate_path = "/c.pem".into();
        assert!(tls(&p).is_err(), "both or neither");
        p.ssl.client_key_path = "/k.pem".into();
        assert!(tls(&p).is_err(), "one PEM file");
        p.ssl.client_key_path = "/c.pem".into();
        let Some(Tls::Enabled(t)) = tls(&p).unwrap() else { panic!() };
        assert_eq!((t.cert_key_file_path, t.allow_invalid_certificates), (Some("/c.pem".into()), None));
    }

    #[test]
    fn write_classification() {
        let w = |s: &str| command_writes(&parse_document(s).unwrap());
        assert!(!w(r#"{"find": "x", "filter": {}}"#));
        assert!(!w(r#"{"aggregate": "x", "pipeline": [{"$match": {}}], "cursor": {}}"#));
        assert!(w(r#"{"aggregate": "x", "pipeline": [{"$out": "y"}], "cursor": {}}"#));
        assert!(w(r#"{"insert": "x", "documents": [{}]}"#));
        assert!(w(r#"{"dropDatabase": 1}"#));
        assert!(!w(r#"{"explain": {"find": "x"}}"#));
        assert!(w(r#"{"explain": {"delete": "x", "deletes": []}}"#));
        assert!(w(r#"{"explain": 1}"#));
    }
}
