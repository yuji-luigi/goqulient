//! Introspection must match GokuCore: every Tests/Parity/schema/*.json (written by ParityFixtureTests.swift), after the
//! normalization in Tests/Parity/README.md. An engine whose driver isn't implemented yet (`Unsupported`) is skipped.
mod support;

use goku_engine::DbError;
use goku_engine::profile::DatabaseKind;
use goku_engine::session::connect;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashSet;
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

/// Numbers as f64 (Swift writes `90`, serde `90.0`), recursively.
fn numbers_as_f64(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null),
        Value::Array(a) => Value::Array(a.into_iter().map(numbers_as_f64).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, numbers_as_f64(v))).collect()),
        other => other,
    }
}

/// README "Normalization": drop rowEstimate, keep the listed entities (and the relationships/warnings between them),
/// sort MongoDB attributes by name.
pub fn normalize(mut model: Value, keep: &[String]) -> Value {
    let keep: HashSet<&str> = keep.iter().map(String::as_str).collect();
    let mongo = model["engine"] == "mongodb";
    let o = model.as_object_mut().unwrap();
    let entities: Vec<Value> = o["entities"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| keep.contains(e["id"].as_str().unwrap()))
        .cloned()
        .map(|mut e| {
            let eo: &mut Map<String, Value> = e.as_object_mut().unwrap();
            eo.remove("rowEstimate");
            if mongo {
                eo["attributes"].as_array_mut().unwrap().sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            }
            e
        })
        .collect();
    o["entities"] = Value::Array(entities);
    let rels: Vec<Value> = o["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| keep.contains(r["fromEntity"].as_str().unwrap()) && keep.contains(r["toEntity"].as_str().unwrap()))
        .cloned()
        .collect();
    o["relationships"] = Value::Array(rels);
    let warnings: Vec<Value> = o
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| {
            let w = w.as_str().unwrap();
            !w.contains(" references ") || keep.iter().any(|id| w.starts_with(&format!("{id}.")))
        })
        .collect();
    o.insert("warnings".into(), Value::Array(warnings));
    numbers_as_f64(model)
}

#[tokio::test]
async fn introspection_matches_gokucore() {
    if !support::enabled() {
        eprintln!("skipped: GOKU_IT not set");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../Tests/Parity/schema");
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
