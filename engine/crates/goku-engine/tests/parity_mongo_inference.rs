//! Mongo inference must match GokuCore: every parity/mongo-inference/*.json (written by ParityFixtureTests.swift).
//! Each fixture's database is dropped, filled with its documents (in order) and indexes, introspected (three times, to
//! catch order-dependent results), normalized like parity_schema.rs and dropped again.
#[path = "support/mongo.rs"]
mod mongo_support;
#[path = "support/parity.rs"]
mod parity;
mod support;

use bson::{Document, doc};
use goku_engine::drivers::mongo::parse_document;
use goku_engine::profile::DatabaseKind;
use goku_engine::session::connect;
use mongodb::IndexModel;
use mongodb::options::IndexOptions;
use parity::{normalize, numbers_as_f64};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct Index {
    name: String,
    fields: Vec<String>,
    unique: bool,
}

#[derive(Deserialize)]
struct Collection {
    name: String,
    documents: Vec<String>,
    indexes: Vec<Index>,
}

#[derive(Deserialize)]
struct Input {
    database: String,
    collections: Vec<Collection>,
}

#[derive(Deserialize)]
struct Fixture {
    input: Input,
    output: Value,
}

async fn fill(client: &mongodb::Client, input: &Input) {
    let db = client.database(&input.database);
    for c in &input.collections {
        if c.documents.is_empty() {
            db.create_collection(&c.name).await.unwrap();
        } else {
            let docs: Vec<Document> = c.documents.iter().map(|d| parse_document(d).unwrap()).collect();
            db.collection::<Document>(&c.name).insert_many(docs).ordered(true).await.unwrap();
        }
        for i in &c.indexes {
            let mut keys = doc! {};
            for f in &i.fields {
                keys.insert(f.clone(), 1);
            }
            let options = IndexOptions::builder().name(i.name.clone()).unique(i.unique).build();
            let model = IndexModel::builder().keys(keys).options(options).build();
            db.collection::<Document>(&c.name).create_index(model).await.unwrap();
        }
    }
}

#[tokio::test]
async fn inference_matches_gokucore() {
    if !support::enabled() {
        eprintln!("skipped: GOKU_IT not set");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../parity/mongo-inference");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let client = mongo_support::raw().await;
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let f: Fixture = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let db = f.input.database.clone();
        let keep: Vec<String> =
            f.output["entities"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_owned()).collect();
        let want = numbers_as_f64(f.output.clone());
        mongo_support::drop_db(&client, &db).await;
        fill(&client, &f.input).await;
        let (mut profile, credentials) = support::profile(DatabaseKind::Mongodb);
        profile.database = db.clone();
        let cancel = CancellationToken::new();
        let session = connect(profile, credentials, &cancel).await.unwrap_or_else(|e| panic!("{name}: connect: {e}"));
        let mut results = Vec::new();
        support::run(120, async {
            for _ in 0..3 {
                let model = session.introspect(&db, &[], &cancel).await.unwrap_or_else(|e| panic!("{name}: {e}"));
                results.push(serde_json::to_value(model).unwrap());
            }
        })
        .await;
        session.close().await;
        mongo_support::drop_db(&client, &db).await;
        for raw in &results[1..] {
            assert_eq!(raw, &results[0], "{name}: introspection isn't stable (attribute order included)");
        }
        let got = normalize(results.remove(0), &keep);
        if got != want {
            panic!(
                "{name} differs from GokuCore.\n--- want\n{}\n--- got\n{}",
                serde_json::to_string_pretty(&want).unwrap(),
                serde_json::to_string_pretty(&got).unwrap()
            );
        }
    }
    eprintln!("compared {} mongo-inference fixtures", files.len());
}
