//! Open database sessions, by id. Methods take `sessionId`; an unknown id is `NotConnected` (the extension reconnects).
use crate::server::lock;
use crate::server::{RpcError, params};
use goku_engine::DbError;
use goku_engine::limits::{HARD_MAX_ROWS, Paging};
use goku_engine::model::{ObjectRef, Sort};
use goku_engine::profile::{ConnectionProfile, Credentials};
use goku_engine::query::{self, RunOptions};
use goku_engine::session::{self, DatabaseSession};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<HashMap<String, Arc<dyn DatabaseSession>>>>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Open {
    profile: Value,
    #[serde(default)]
    credentials: Credentials,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    session_id: String,
    text: String,
    database: Option<String>,
    #[serde(default)]
    confirmed: bool,
    max_rows: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    session_id: String,
    #[serde(default)]
    database: String,
    #[serde(default)]
    schema: String,
    #[serde(default)]
    schemas: Vec<String>,
    object: Option<ObjectRef>,
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
    sort: Option<Sort>,
}

impl Sessions {
    pub const METHODS: &[&str] = &[
        "connection/open",
        "connection/close",
        "catalog/databases",
        "catalog/schemas",
        "catalog/objects",
        "catalog/columns",
        "table/page",
        "schema/introspect",
        "query/run",
    ];

    fn get(&self, id: &str) -> Result<Arc<dyn DatabaseSession>, RpcError> {
        lock(&self.0).get(id).cloned().ok_or_else(|| DbError::NotConnected.into())
    }

    /// `None` when `method` isn't a session method.
    pub async fn request(&self, method: &str, p: Value, cancel: &CancellationToken) -> Option<Result<Value, RpcError>> {
        if !Self::METHODS.contains(&method) {
            return None;
        }
        Some(self.dispatch(method, p, cancel).await)
    }

    async fn dispatch(&self, method: &str, p: Value, cancel: &CancellationToken) -> Result<Value, RpcError> {
        if method == "connection/open" {
            let o: Open = params(p)?;
            let profile =
                ConnectionProfile::deserialize(&o.profile).map_err(|e| DbError::InvalidConfiguration(e.to_string()))?;
            let endpoint = profile.endpoint_summary();
            let s: Arc<dyn DatabaseSession> = session::connect(profile, o.credentials, cancel).await?.into();
            let info = match s.server_info(cancel).await {
                Ok(info) => info,
                Err(e) => {
                    s.close().await; // don't leak the session when the first query fails (GokuCore's open item)
                    return Err(e.into());
                }
            };
            let id = uuid_like();
            lock(&self.0).insert(id.clone(), s);
            return Ok(json!({"sessionId": id, "serverInfo": info, "endpoint": endpoint}));
        }
        if method == "query/run" {
            let r: Run = params(p)?;
            let s = self.get(&r.session_id)?;
            let options = RunOptions {
                database: r.database,
                confirmed: r.confirmed,
                max_rows: r.max_rows.map(|n| n.clamp(1, HARD_MAX_ROWS as i64) as usize),
            };
            let outcome = query::run(s.as_ref(), &r.text, &options, cancel).await?;
            return Ok(serde_json::to_value(outcome).unwrap_or(Value::Null));
        }
        let t: Target = params(p)?;
        if method == "connection/close" {
            let s = lock(&self.0).remove(&t.session_id);
            if let Some(s) = s {
                s.close().await;
            }
            return Ok(Value::Null);
        }
        let s = self.get(&t.session_id)?;
        let object = || {
            t.object.clone().ok_or_else(|| RpcError::new(crate::server::code::INVALID_PARAMS, "“object” is missing"))
        };
        Ok(match method {
            "catalog/databases" => json!({"databases": s.databases(cancel).await?}),
            "catalog/schemas" => json!({"schemas": s.schemas(&t.database, cancel).await?}),
            "catalog/objects" => json!({"objects": s.objects(&t.database, &t.schema, cancel).await?}),
            "catalog/columns" => json!({"columns": s.columns(&object()?, cancel).await?}),
            "table/page" => {
                let limit = t.limit.unwrap_or(s.profile().max_rows);
                let page = s.browse(&object()?, Paging::new(t.offset, limit), t.sort.as_ref(), cancel).await?;
                serde_json::to_value(page).unwrap_or(Value::Null)
            }
            "schema/introspect" => {
                serde_json::to_value(s.introspect(&t.database, &t.schemas, cancel).await?).unwrap_or(Value::Null)
            }
            _ => unreachable!("listed in METHODS"),
        })
    }

    /// Closes every session (end of input or `exit`), each bounded by its own driver deadlines.
    pub async fn close_all(&self) {
        let all: Vec<_> = lock(&self.0).drain().map(|(_, s)| s).collect();
        for s in all {
            s.close().await;
        }
    }
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("s{}", NEXT.fetch_add(1, Ordering::Relaxed))
}
