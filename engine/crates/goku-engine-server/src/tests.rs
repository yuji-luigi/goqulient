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

#[tokio::test]
async fn query_run_is_listed_and_checks_its_params() {
    let mut c = Client::initialized().await;
    let r = c.call(2, "initialize", json!({"protocolVersion": PROTOCOL_VERSION})).await;
    assert!(r["result"]["capabilities"]["methods"].as_array().unwrap().contains(&json!("query/run")));
    assert_eq!(c.call(3, "query/run", json!({"sessionId": "s1"})).await["error"]["code"], code::INVALID_PARAMS);
    let r = c.call(4, "query/run", json!({"sessionId": "nope", "text": "SELECT 1"})).await;
    assert_eq!(r["error"]["data"]["kind"], "notConnected");
}

/// `query/run` end to end against the Docker PostgreSQL (GOKU_IT=1, set in the `engine` container): result shapes,
/// confirmation, and `$/cancelRequest` stopping a running statement.
#[tokio::test]
async fn query_run_results_confirmation_and_cancel() {
    if std::env::var("GOKU_IT").as_deref() != Ok("1") {
        return;
    }
    let host = std::env::var("PG_HOST").unwrap_or_else(|_| "pg-primary".into());
    let mut c = Client::initialized().await;
    let profile = json!({"name": "it", "kind": "postgres", "hosts": [{"host": host, "port": 5432}], "user": "dbc",
                         "database": "shop", "ssl": {"mode": "disable"}, "queryTimeoutSeconds": 30});
    let r = c.call(2, "connection/open", json!({"profile": profile, "credentials": {"password": "dbcpass"}})).await;
    let session = r["result"]["sessionId"].as_str().expect("connection/open").to_owned();

    let text = "SELECT 1 AS a;\n-- é\nSELECT 'x' AS b, NULL AS c";
    let r = c.call(3, "query/run", json!({"sessionId": session, "text": text, "maxRows": 10})).await;
    let results = &r["result"]["results"];
    assert_eq!(r["result"]["outcome"], "results");
    assert_eq!(results.as_array().unwrap().len(), 2);
    assert_eq!(results[0]["kind"], "rows");
    assert_eq!(results[0]["columns"], json!([{"name": "a", "typeName": "int4"}]));
    assert_eq!(results[1]["rows"], json!([["x", null]]));
    assert_eq!(results[1]["statement"], "-- é\nSELECT 'x' AS b, NULL AS c");
    assert_eq!(results[1]["span"], json!({"start": 15, "end": 46}));
    assert_eq!((results[1]["database"].clone(), results[1]["truncated"].clone()), (json!("shop"), json!(false)));
    assert!(results[1]["elapsedMs"].is_u64() && results[1]["notices"] == json!([]));

    let r = c.call(4, "query/run", json!({"sessionId": session, "text": "SELECT 1; TRUNCATE rs_pg_proto_none"})).await;
    let found = &r["result"];
    assert_eq!(found["outcome"], "needsConfirmation");
    assert_eq!(found["findings"][0]["action"], "truncates table");
    assert_eq!(found["findings"][0]["object"], "rs_pg_proto_none");
    assert_eq!(found["findings"][0]["span"], json!({"start": 10, "end": 35}));
    let r = c
        .call(5, "query/run", json!({"sessionId": session, "text": "TRUNCATE rs_pg_proto_none", "confirmed": true}))
        .await;
    assert_eq!(r["result"]["results"][0]["kind"], "error", "confirmed: it runs (and fails: no such table)");
    assert_eq!(r["result"]["results"][0]["error"]["kind"], "query");

    c.request(6, "query/run", json!({"sessionId": session, "text": "SELECT pg_sleep(20)"})).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    c.send(json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 6}})).await;
    let r = c.next().await;
    assert_eq!((r["id"].clone(), r["error"]["code"].clone()), (json!(6), json!(code::REQUEST_CANCELLED)));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    let r = c.call(7, "query/run", json!({"sessionId": session, "text": "SELECT 2"})).await;
    assert_eq!(r["result"]["results"][0]["rows"], json!([["2"]]), "the session keeps working");
    c.call(8, "connection/close", json!({"sessionId": session})).await;
}
