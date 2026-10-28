//! End-to-end tests of the protocol over an in-memory pipe (no database).
use crate::framing::{read_message, write_message};
use crate::server::{Exit, PROTOCOL_VERSION, code, run};
use serde_json::{Value, json};
use tokio::io::{BufReader, DuplexStream, ReadHalf, WriteHalf, duplex, split};
use tokio::task::JoinHandle;

struct Client {
    to_server: WriteHalf<DuplexStream>,
    from_server: BufReader<ReadHalf<DuplexStream>>,
    server: JoinHandle<Exit>,
}

impl Client {
    fn start() -> Self {
        let (client, server) = duplex(1 << 20);
        let (server_read, server_write) = split(server);
        let server = tokio::spawn(run(BufReader::new(server_read), server_write));
        let (from_server, to_server) = split(client);
        Self { to_server, from_server: BufReader::new(from_server), server }
    }

    async fn send(&mut self, message: Value) {
        write_message(&mut self.to_server, &message).await.unwrap();
    }

    async fn request(&mut self, id: i64, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await;
    }

    async fn next(&mut self) -> Value {
        read_message(&mut self.from_server).await.unwrap().expect("server closed the stream")
    }

    async fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.request(id, method, params).await;
        let r = self.next().await;
        assert_eq!(r["id"], id);
        r
    }

    async fn initialized() -> Self {
        let mut c = Self::start();
        let r = c.call(1, "initialize", json!({"protocolVersion": PROTOCOL_VERSION, "clientName": "test"})).await;
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        c
    }
}

#[tokio::test]
async fn requests_before_initialize_are_refused() {
    let mut c = Client::start();
    let r = c.call(1, "profile/validate", json!({})).await;
    assert_eq!(r["error"]["code"], code::SERVER_NOT_INITIALIZED);
}

#[tokio::test]
async fn protocol_mismatch_is_reported_and_leaves_it_uninitialized() {
    let mut c = Client::start();
    let r = c.call(1, "initialize", json!({"protocolVersion": 99})).await;
    assert_eq!(r["error"]["code"], code::PROTOCOL_MISMATCH);
    assert_eq!(r["error"]["data"]["engineProtocolVersion"], PROTOCOL_VERSION);
    let r = c.call(2, "shutdown", Value::Null).await;
    assert_eq!(r["error"]["code"], code::SERVER_NOT_INITIALIZED);
}

#[tokio::test]
async fn initialize_reports_versions_and_methods() {
    let mut c = Client::start();
    let r = c.call(7, "initialize", json!({"protocolVersion": 1})).await;
    assert_eq!(r["result"]["engineVersion"], goku_engine::VERSION);
    assert!(r["result"]["capabilities"]["methods"].as_array().unwrap().contains(&json!("profile/validate")));
}

#[tokio::test]
async fn unknown_method_and_bad_params() {
    let mut c = Client::initialized().await;
    assert_eq!(c.call(2, "table/drop", json!({})).await["error"]["code"], code::METHOD_NOT_FOUND);
    assert_eq!(c.call(3, "profile/validate", json!({"nope": 1})).await["error"]["code"], code::INVALID_PARAMS);
}

#[tokio::test]
async fn profile_validate_normalizes_and_redacts() {
    let mut c = Client::initialized().await;
    let r = c
        .call(
            2,
            "profile/validate",
            json!({"profile": {"name": "pg", "kind": "postgres", "hosts": [{"host": "db", "port": 5433}]}}),
        )
        .await;
    assert_eq!(r["result"]["endpoint"], "db:5433");
    assert_eq!(r["result"]["profile"]["user"], "postgres");
    assert_eq!(r["result"]["profile"]["ssl"]["mode"], "prefer");

    let r = c
        .call(
            3,
            "profile/validate",
            json!({"profile": {"name": "m", "kind": "mongodb", "mode": "uri", "uri": "mongodb://u:hunter2@h/db"}}),
        )
        .await;
    assert_eq!(r["result"]["endpoint"], "mongodb://u:****@h/db");

    let r = c.call(4, "profile/validate", json!({"profile": {"name": "m", "kind": "mysql", "hosts": []}})).await;
    assert_eq!(r["error"]["code"], code::ENGINE_ERROR);
    assert_eq!(r["error"]["message"], "Invalid configuration: Every host needs a name and a port between 1 and 65535.");
    assert_eq!(r["error"]["data"]["kind"], "invalidConfiguration");

    let r = c.call(5, "profile/validate", json!({"profile": {"name": "x", "kind": "oracle"}})).await;
    assert_eq!(r["error"]["data"]["kind"], "invalidConfiguration");
}

#[tokio::test]
async fn cancel_request_answers_with_request_cancelled() {
    let mut c = Client::initialized().await;
    c.request(2, "debug/wait", json!({"seconds": 30, "deadline": 60})).await;
    c.request(3, "profile/validate", json!({"profile": {"name": "a", "kind": "postgres"}})).await;
    // Other requests keep being answered while one is waiting.
    assert_eq!(c.next().await["id"], 3);
    c.send(json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 2}})).await;
    let r = c.next().await;
    assert_eq!((r["id"].clone(), r["error"]["code"].clone()), (json!(2), json!(code::REQUEST_CANCELLED)));
}

#[tokio::test]
async fn deadline_answers_with_a_timeout() {
    let mut c = Client::initialized().await;
    let r = c.call(2, "debug/wait", json!({"seconds": 30, "deadline": 0.05})).await;
    assert_eq!(r["error"]["code"], code::ENGINE_ERROR);
    assert_eq!(r["error"]["data"]["kind"], "timeout");
    assert_eq!(r["error"]["message"], "Timed out: debug/wait exceeded 0s");
}

#[tokio::test]
async fn shutdown_then_exit_is_clean() {
    let mut c = Client::initialized().await;
    assert_eq!(c.call(2, "shutdown", Value::Null).await["result"], Value::Null);
    assert_eq!(c.call(3, "profile/validate", json!({})).await["error"]["code"], code::INVALID_REQUEST);
    c.send(json!({"jsonrpc": "2.0", "method": "exit"})).await;
    assert_eq!(c.server.await.unwrap(), Exit::Clean);
}

#[tokio::test]
async fn closing_stdin_cancels_work_and_ends_the_server() {
    let mut c = Client::initialized().await;
    c.request(2, "debug/wait", json!({"seconds": 3600, "deadline": 7200})).await;
    let Client { mut to_server, from_server, server } = c;
    tokio::io::AsyncWriteExt::shutdown(&mut to_server).await.unwrap();
    let exit =
        tokio::time::timeout(std::time::Duration::from_secs(5), server).await.expect("server kept running").unwrap();
    assert_eq!(exit, Exit::Abrupt);
    drop(from_server);
}

#[tokio::test]
async fn broken_frame_reports_a_parse_error_and_stops() {
    let mut c = Client::start();
    tokio::io::AsyncWriteExt::write_all(&mut c.to_server, b"Content-Length: 4\r\n\r\n{]}x").await.unwrap();
    assert_eq!(c.next().await["error"]["code"], code::PARSE_ERROR);
    assert_eq!(c.server.await.unwrap(), Exit::Abrupt);
}

#[tokio::test]
async fn er_layout_places_parents_left() {
    let mut c = Client::initialized().await;
    let size = json!({"width": 100, "height": 40});
    let r = c
        .call(
            2,
            "er/layout",
            json!({"entities": ["orders", "customers"], "sizes": {"orders": size, "customers": size},
                   "edges": [{"child": "orders", "parent": "customers"}]}),
        )
        .await;
    let rects = &r["result"]["rects"];
    assert_eq!(rects["customers"]["origin"]["x"], 0.0);
    assert_eq!(rects["orders"]["origin"]["x"], 190.0);
    assert_eq!(c.call(3, "er/layout", json!({"entities": ["a"]})).await["error"]["code"], code::INVALID_PARAMS);
}

#[tokio::test]
async fn session_methods_need_an_open_session() {
    let mut c = Client::initialized().await;
    let r = c.call(2, "catalog/databases", json!({"sessionId": "nope"})).await;
    assert_eq!(
        (r["error"]["code"].clone(), r["error"]["data"]["kind"].clone()),
        (json!(code::ENGINE_ERROR), json!("notConnected"))
    );
    // Opening validates first, so a bad profile never reaches a driver.
    let r = c.call(3, "connection/open", json!({"profile": {"name": "x", "kind": "postgres", "maxRows": 0}})).await;
    assert_eq!(r["error"]["data"]["kind"], "invalidConfiguration");
    assert_eq!(c.call(4, "connection/close", json!({"sessionId": "nope"})).await["result"], Value::Null);
}
