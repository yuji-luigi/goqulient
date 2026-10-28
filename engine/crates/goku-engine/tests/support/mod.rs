//! Integration test helpers: the Docker test databases (docker-compose.yml), like `IT` in IntegrationSupport.swift.
//! Gated by GOKU_IT=1 (set in the `engine` container), so `cargo test` elsewhere (CI on macOS/Windows) skips them.
#![allow(dead_code)]
use goku_engine::profile::{ConnectionProfile, Credentials, DatabaseKind, HostPort, SslMode};
use std::time::Duration;

pub fn enabled() -> bool {
    std::env::var("GOKU_IT").as_deref() == Ok("1")
}

fn host(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_owned())
}

pub fn profile(kind: DatabaseKind) -> (ConnectionProfile, Credentials) {
    let mut p = ConnectionProfile::new("it", kind);
    p.database = "shop".into();
    p.query_timeout_seconds = 5;
    p.connect_timeout_seconds = 3;
    let password = match kind {
        DatabaseKind::Postgres => {
            p.hosts = vec![HostPort::new(host("PG_HOST", "pg-primary"), 5432)];
            p.user = "dbc".into();
            p.ssl.mode = SslMode::Disable;
            Some("dbcpass".to_owned())
        }
        DatabaseKind::Mysql => {
            p.hosts = vec![HostPort::new(host("MYSQL_HOST", "mysql-primary"), 3306)];
            p.user = "dbc".into();
            p.ssl.mode = SslMode::Require;
            Some("dbcpass".to_owned())
        }
        DatabaseKind::Mongodb => {
            p.hosts = vec![HostPort::new(host("MONGO_HOST", "mongo"), 27017)];
            p.user = String::new();
            p.ssl.mode = SslMode::Disable;
            None
        }
    };
    (p, Credentials { password, ..Credentials::default() })
}

/// Every integration test body runs under a hard deadline so a hung socket fails fast.
pub async fn run<F: std::future::Future<Output = ()>>(seconds: u64, body: F) {
    tokio::time::timeout(Duration::from_secs(seconds), body).await.expect("integration test hung");
}
