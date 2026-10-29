//! MongoDB test helpers (included with `#[path]`): a plain driver client for setup/teardown of `rs_mg_*` scratch
//! databases, so the session under test only reads.
#![allow(dead_code, reason = "each test crate uses only some of the helpers")]
use mongodb::Client;
use mongodb::options::ClientOptions;
use std::time::Duration;

pub async fn raw() -> Client {
    let host = std::env::var("MONGO_HOST").unwrap_or_else(|_| "mongo".into());
    let mut o = ClientOptions::parse(format!("mongodb://{host}:27017/?directConnection=true")).await.unwrap();
    o.server_selection_timeout = Some(Duration::from_secs(5));
    Client::with_options(o).unwrap()
}

/// `<prefix>_<8 hex>`, so parallel tests and other sessions never share a database.
pub fn scratch(prefix: &str) -> String {
    format!("{prefix}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

pub async fn drop_db(client: &Client, name: &str) {
    let _ = tokio::time::timeout(Duration::from_secs(30), client.database(name).drop()).await;
}
