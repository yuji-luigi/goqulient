//! JSON-RPC 2.0 dispatch. Every request runs in its own task with a cancellation token (`$/cancelRequest`), replies
//! go through one writer task, and the end of stdin (the extension went away) cancels everything and ends the process.
use crate::framing::{read_message, write_message};
use goku_engine::DbError;
use goku_engine::profile::ConnectionProfile;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const PROTOCOL_VERSION: u32 = 1;

/// JSON-RPC / LSP error codes, plus the engine's own (documented in docs/vscode-extension.md).
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    pub const REQUEST_CANCELLED: i64 = -32800;
    /// A `DbError`: `message` is its display message, `data` is `{kind, detail}`.
    pub const ENGINE_ERROR: i64 = -32001;
    pub const PROTOCOL_MISMATCH: i64 = -32003;
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), data: None }
    }
}

impl From<DbError> for RpcError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::Cancelled => Self::new(code::REQUEST_CANCELLED, e.display_message()),
            _ => Self {
                code: code::ENGINE_ERROR,
                message: e.display_message(),
                data: serde_json::to_value(&e).ok().map(redact_value),
            },
        }
    }
}

fn redact_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(goku_engine::redact::secrets(&s)),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, redact_value(v))).collect()),
        other => other,
    }
}

fn params<T: for<'de> Deserialize<'de>>(p: Value) -> Result<T, RpcError> {
    serde_json::from_value(if p.is_null() { json!({}) } else { p })
        .map_err(|e| RpcError::new(code::INVALID_PARAMS, format!("Invalid parameters: {e}")))
}

#[derive(Default)]
struct State {
    initialized: bool,
    shutdown: bool,
    in_flight: HashMap<String, CancellationToken>,
}

/// What the main loop should do after `exit` or the end of input.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// `exit` after `shutdown`.
    Clean,
    /// `exit` without `shutdown`, or stdin closed: the client went away.
    Abrupt,
}

pub async fn run<R, W>(mut reader: R, writer: W) -> Exit
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(message) = rx.recv().await {
            if let Err(e) = write_message(&mut writer, &message).await {
                eprintln!("goku-engine: can't write to the client: {e}");
                break;
            }
        }
    });
    let state = Arc::new(Mutex::new(State::default()));
    let exit = loop {
        let message = match read_message(&mut reader).await {
            Ok(Some(m)) => m,
            Ok(None) => break Exit::Abrupt,
            Err(e) => {
                // A broken frame leaves the stream unreadable: report once, then stop like a closed stdin.
                eprintln!("goku-engine: {e}");
                let _ = tx.send(error_response(Value::Null, RpcError::new(code::PARSE_ERROR, e.to_string())));
                break Exit::Abrupt;
            }
        };
        if let Some(exit) = handle(message, &state, &tx) {
            break exit;
        }
    };
    for token in state.lock().unwrap().in_flight.values() {
        token.cancel();
    }
    drop(tx);
    // Cancelled requests answer and drop their senders; one stuck in a driver must not keep the process alive.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer_task).await;
    exit
}

fn error_response(id: Value, e: RpcError) -> Value {
    let mut error = json!({"code": e.code, "message": e.message});
    if let Some(data) = e.data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

fn response(id: &Value, result: Result<Value, RpcError>) -> Value {
    match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => error_response(id.clone(), e),
    }
}

fn id_key(id: &Value) -> String {
    id.to_string()
}

/// Handles one incoming message; `Some` ends the main loop.
fn handle(message: Value, state: &Arc<Mutex<State>>, tx: &mpsc::UnboundedSender<Value>) -> Option<Exit> {
    let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_owned) else {
        // Responses from the client: the server sends no requests yet, so there's nothing to match them to.
        if message.get("id").is_none() || message.get("result").is_none() && message.get("error").is_none() {
            let _ =
                tx.send(error_response(Value::Null, RpcError::new(code::INVALID_REQUEST, "Not a JSON-RPC message")));
        }
        return None;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = message.get("id").cloned() else {
        return notification(&method, params, state);
    };
    let reply = |result: Result<Value, RpcError>| response(&id, result);
    {
        let mut s = state.lock().unwrap();
        match method.as_str() {
            "initialize" => {
                let result = initialize(params);
                s.initialized |= result.is_ok();
                let _ = tx.send(reply(result));
                return None;
            }
            _ if !s.initialized => {
                let _ = tx
                    .send(reply(Err(RpcError::new(code::SERVER_NOT_INITIALIZED, "The engine isn't initialized yet."))));
                return None;
            }
            "shutdown" => {
                s.shutdown = true;
                let _ = tx.send(reply(Ok(Value::Null)));
                return None;
            }
            _ if s.shutdown => {
                let _ = tx.send(reply(Err(RpcError::new(code::INVALID_REQUEST, "The engine is shutting down."))));
                return None;
            }
            _ => {}
        }
    }
    let token = CancellationToken::new();
    state.lock().unwrap().in_flight.insert(id_key(&id), token.clone());
    let (state, tx) = (state.clone(), tx.clone());
    tokio::spawn(async move {
        let result = tokio::select! {
            r = request(&method, params, &token) => r,
            _ = token.cancelled() => Err(DbError::Cancelled.into()),
        };
        state.lock().unwrap().in_flight.remove(&id_key(&id));
        let _ = tx.send(response(&id, result));
    });
    None
}

fn notification(method: &str, params: Value, state: &Arc<Mutex<State>>) -> Option<Exit> {
    match method {
        "exit" => Some(if state.lock().unwrap().shutdown { Exit::Clean } else { Exit::Abrupt }),
        "$/cancelRequest" => {
            if let Some(id) = params.get("id")
                && let Some(token) = state.lock().unwrap().in_flight.get(&id_key(id))
            {
                token.cancel();
            }
            None
        }
        _ => None, // unknown notifications are ignored, as in LSP
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    protocol_version: Option<u32>,
    client_name: Option<String>,
}

fn initialize(p: Value) -> Result<Value, RpcError> {
    let p: InitializeParams = params(p)?;
    let version = p.protocol_version.unwrap_or(0);
    if version != PROTOCOL_VERSION {
        return Err(RpcError {
            code: code::PROTOCOL_MISMATCH,
            message: format!(
                "The extension speaks protocol {version}, this engine speaks {PROTOCOL_VERSION}. Reinstall the extension so both match."
            ),
            data: Some(json!({"engineProtocolVersion": PROTOCOL_VERSION})),
        });
    }
    if let Some(client) = p.client_name {
        eprintln!("goku-engine {}: initialized by {client}", goku_engine::VERSION);
    }
    Ok(json!({
        "protocolVersion": PROTOCOL_VERSION,
        "engineVersion": goku_engine::VERSION,
        "capabilities": {"methods": METHODS},
    }))
}

const METHODS: &[&str] = &["initialize", "shutdown", "profile/validate"];

async fn request(method: &str, p: Value, cancel: &CancellationToken) -> Result<Value, RpcError> {
    match method {
        "profile/validate" => profile_validate(p),
        #[cfg(debug_assertions)]
        "debug/wait" => debug_wait(p, cancel).await,
        _ => {
            let _ = cancel;
            Err(RpcError::new(code::METHOD_NOT_FOUND, format!("Unknown method “{method}”")))
        }
    }
}

#[derive(Deserialize)]
struct ProfileParams {
    profile: Value,
}

/// Decodes a profile tolerantly and validates it, without touching the network: the extension checks settings with it.
fn profile_validate(p: Value) -> Result<Value, RpcError> {
    let p: ProfileParams = params(p)?;
    let profile =
        ConnectionProfile::deserialize(&p.profile).map_err(|e| DbError::InvalidConfiguration(e.to_string()))?;
    profile.validate()?;
    Ok(json!({"profile": profile, "endpoint": profile.endpoint_summary()}))
}

/// Debug builds only: waits under a deadline, so tests can exercise cancellation and timeouts end to end.
#[cfg(debug_assertions)]
async fn debug_wait(p: Value, cancel: &CancellationToken) -> Result<Value, RpcError> {
    #[derive(Deserialize)]
    struct Wait {
        seconds: f64,
        deadline: f64,
    }
    let w: Wait = params(p)?;
    goku_engine::deadline::with_deadline(w.deadline, "debug/wait", cancel, || {}, async {
        tokio::time::sleep(std::time::Duration::from_secs_f64(w.seconds)).await;
        Ok(json!({"waited": w.seconds}))
    })
    .await
    .map_err(Into::into)
}
