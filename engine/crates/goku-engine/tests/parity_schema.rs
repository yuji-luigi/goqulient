//! Introspection must match GokuCore: every parity/schema/*.json (written by ParityFixtureTests.swift), after the
//! normalization in parity/README.md. An engine whose driver isn't implemented yet (`Unsupported`) is skipped.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "a failing test panics")]
#[path = "support/parity.rs"]
mod parity;
mod support;

use goku_engine::DbError;
use goku_engine::profile::DatabaseKind;
use goku_engine::session::connect;
use parity::{normalize, numbers_as_f64};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct Input {
    database: String,
    schemas: Vec<String>,
    #[serde(default)]
    setup: Vec<String>,
    #[serde(default)]
    teardown: Vec<String>,
    entities: Vec<String>,
}

#[derive(Deserialize)]
struct Fixture {
    input: Input,
    output: Value,
}

#[tokio::test]
async fn introspection_matches_gokucore() {
    if !support::enabled() {
        eprintln!("skipped: GOKU_IT not set");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../parity/schema");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut compared = 0;
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let f: Fixture = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let kind = match f.output["engine"].as_str().unwrap() {
            "postgres" => DatabaseKind::Postgres,
            "mysql" => DatabaseKind::Mysql,
            _ => DatabaseKind::Mongodb,
        };
        let (mut profile, credentials) = support::profile(kind);
        profile.database = f.input.database.clone();
        let cancel = CancellationToken::new();
        let session = match connect(profile, credentials, &cancel).await {
            Err(DbError::Unsupported(m)) => {
                eprintln!("{name}: skipped ({m})");
                continue;
            }
            other => other.unwrap_or_else(|e| panic!("{name}: connect: {e}")),
        };
        support::run(120, async {
            for s in &f.input.teardown {
                let _ = session.execute(&f.input.database, s, &cancel).await;
            }
            for s in &f.input.setup {
                session
                    .execute(&f.input.database, s, &cancel)
                    .await
                    .unwrap_or_else(|e| panic!("{name}: setup {s}: {e}"));
            }
            let got = session.introspect(&f.input.database, &f.input.schemas, &cancel).await;
            for s in &f.input.teardown {
                let _ = session.execute(&f.input.database, s, &cancel).await;
            }
            let got = normalize(
                serde_json::to_value(got.unwrap_or_else(|e| panic!("{name}: introspect: {e}"))).unwrap(),
                &f.input.entities,
            );
            let want = numbers_as_f64(f.output.clone());
            if got != want {
                panic!(
                    "{name} differs from GokuCore.\n--- want\n{}\n--- got\n{}",
                    serde_json::to_string_pretty(&want).unwrap(),
                    serde_json::to_string_pretty(&got).unwrap()
                );
            }
        })
        .await;
        session.close().await;
        compared += 1;
    }
    eprintln!("compared {compared} schema fixtures");
}
