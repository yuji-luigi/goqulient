//! Query execution must match GokuCore: statement splitting, the SQL read-only / rules checks, the danger check and
//! the Mongo shell parser, over every case in parity/{sql-split,sql-safety,danger,mongo-shell} (written by
//! ParityQueryFixtureTests.swift). No database.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "a failing test panics")]
use bson::{Bson, Document};
use goku_engine::drivers::mongo::shell::{self, MongoCall};
use goku_engine::profile::{ConnectionProfile, DatabaseKind, ReadOnlyRule};
use goku_engine::query::{danger, sql};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::PathBuf;

fn fixture(path: &str) -> Value {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../parity").join(path);
    serde_json::from_str(&std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{path}: {e}"))).unwrap()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect()
}

fn dialect(name: &str) -> sql::Dialect {
    if name == "postgres" { sql::Dialect::Postgres } else { sql::Dialect::Mysql }
}

/// Every case is checked; the failures are listed together, so one run shows all the differences.
fn report(file: &str, failures: Vec<String>) {
    assert!(failures.is_empty(), "{file}: {} case(s) differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn sql_split_matches_swift() {
    for file in ["sql-split/postgres.json", "sql-split/mysql.json"] {
        let f = fixture(file);
        let d = dialect(f["input"]["dialect"].as_str().unwrap());
        let database = f["input"]["database"].as_str().unwrap();
        let mut failures = Vec::new();
        for (script, want) in strings(&f["input"]["scripts"]).iter().zip(f["output"].as_array().unwrap()) {
            let got = sql::split_tracking_database(script, d, database);
            let got = json!({
                "statements": got.iter().map(|s| s.text.clone()).collect::<Vec<_>>(),
                "databases": got.iter().map(|s| s.database.clone()).collect::<Vec<_>>(),
            });
            if &got != want {
                failures.push(format!("{script:?}\n  want {want}\n  got  {got}"));
            }
        }
        report(file, failures);
    }
}

#[test]
fn sql_safety_matches_swift() {
    for file in ["sql-safety/postgres.json", "sql-safety/mysql.json"] {
        let f = fixture(file);
        let name = f["input"]["dialect"].as_str().unwrap();
        let d = dialect(name);
        let kind = if name == "postgres" { DatabaseKind::Postgres } else { DatabaseKind::Mysql };
        let database = f["input"]["database"].as_str().unwrap();
        let mut read_only = ConnectionProfile::new("p", kind);
        read_only.read_only = true;
        let mut ruled = ConnectionProfile::new("p", kind);
        ruled.read_only_rules = f["input"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| ReadOnlyRule {
                id: "r".into(),
                database: r["database"].as_str().unwrap().into(),
                table: r["table"].as_str().unwrap().into(),
            })
            .collect();
        let refusal =
            |p: &ConnectionProfile, st: &str| sql::check_sql(p, st, database, d).err().map(|e| e.display_message());
        let mut failures = Vec::new();
        for (st, want) in strings(&f["input"]["statements"]).iter().zip(f["output"].as_array().unwrap()) {
            let n = sql::Normalized::new(st, d);
            let mut got = json!({
                "normalized": n.text, "executableComment": n.has_executable_comment, "keyword": n.leading_keyword(),
                "writes": sql::is_potentially_writing(st, d), "returnsRows": sql::returns_rows(st),
            });
            for (key, value) in [
                ("useTarget", sql::use_target(st, d)),
                ("readOnlyRefusal", refusal(&read_only, st)),
                ("rulesRefusal", refusal(&ruled, st)),
            ] {
                if let Some(v) = value {
                    got[key] = json!(v);
                }
            }
            if &got != want {
                failures.push(format!("{st:?}\n  want {want}\n  got  {got}"));
            }
        }
        report(file, failures);
    }
}

#[test]
fn danger_check_matches_swift() {
    for (file, kind) in [
        ("danger/postgres.json", DatabaseKind::Postgres),
        ("danger/mysql.json", DatabaseKind::Mysql),
        ("danger/mongodb.json", DatabaseKind::Mongodb),
    ] {
        let f = fixture(file);
        let database = f["input"]["database"].as_str().unwrap();
        let mut failures = Vec::new();
        for (script, want) in strings(&f["input"]["scripts"]).iter().zip(f["output"].as_array().unwrap()) {
            let got = serde_json::to_value(danger::findings(script, kind, database)).unwrap();
            if &got != want {
                failures.push(format!("{script:?}\n  want {want}\n  got  {got}"));
            }
        }
        report(file, failures);
    }
}

// MARK: - Mongo shell

/// Swift's `ExtendedJSON.string` (canonical), an independent writer, so the text compares exactly: key order, BSON
/// types and number spellings.
fn canonical(v: &Bson) -> String {
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn double(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let mut s = goku_engine::drivers::mongo::format::double(d);
    if !s.contains(['.', 'e', 'E']) {
        s += ".0";
    }
    s
}

fn write(v: &Bson, out: &mut String) {
    use base64::Engine as _;
    match v {
        Bson::String(s) => quote(s, out),
        Bson::Int32(i) => _ = write!(out, r#"{{"$numberInt":"{i}"}}"#),
        Bson::Int64(i) => _ = write!(out, r#"{{"$numberLong":"{i}"}}"#),
        Bson::Double(d) => _ = write!(out, r#"{{"$numberDouble":"{}"}}"#, double(*d)),
        Bson::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Bson::Null => out.push_str("null"),
        Bson::ObjectId(o) => _ = write!(out, r#"{{"$oid":"{}"}}"#, o.to_hex()),
        Bson::DateTime(d) => _ = write!(out, r#"{{"$date":{{"$numberLong":"{}"}}}}"#, d.timestamp_millis()),
        Bson::Decimal128(d) => _ = write!(out, r#"{{"$numberDecimal":"{d}"}}"#),
        Bson::Binary(b) => {
            let data = base64::engine::general_purpose::STANDARD.encode(&b.bytes);
            _ = write!(out, r#"{{"$binary":{{"base64":"{data}","subType":"{:02x}"}}}}"#, u8::from(b.subtype));
        }
        Bson::RegularExpression(r) => {
            out.push_str(r#"{"$regularExpression":{"pattern":"#);
            quote(r.pattern.as_str(), out);
            out.push_str(r#","options":"#);
            quote(r.options.as_str(), out);
            out.push_str("}}");
        }
        Bson::Timestamp(t) => _ = write!(out, r#"{{"$timestamp":{{"t":{},"i":{}}}}}"#, t.time, t.increment),
        Bson::MinKey => out.push_str(r#"{"$minKey":1}"#),
        Bson::MaxKey => out.push_str(r#"{"$maxKey":1}"#),
        Bson::JavaScriptCode(c) => {
            out.push_str(r#"{"$code":"#);
            quote(c, out);
            out.push('}');
        }
        Bson::JavaScriptCodeWithScope(c) => {
            out.push_str(r#"{"$code":"#);
            quote(&c.code, out);
            out.push_str(r#","$scope":"#);
            write(&Bson::Document(c.scope.clone()), out);
            out.push('}');
        }
        Bson::Document(d) => {
            out.push('{');
            for (i, (k, v)) in d.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                quote(k, out);
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
        Bson::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(v, out);
            }
            out.push(']');
        }
        other => panic!("no canonical form for {other:?}"),
    }
}

fn doc(d: &Document) -> String {
    canonical(&Bson::Document(d.clone()))
}

/// `CallFixture` in ParityQueryFixtureTests.swift.
fn call_json(call: &MongoCall) -> Value {
    match call {
        MongoCall::ShowDatabases => json!({"kind": "showDatabases"}),
        MongoCall::ShowCollections => json!({"kind": "showCollections"}),
        MongoCall::GetName => json!({"kind": "getName"}),
        MongoCall::Use(n) => json!({"kind": "use", "name": n}),
        MongoCall::Command(d) => json!({"kind": "command", "document": doc(d)}),
        MongoCall::AdminCommand(d) => json!({"kind": "adminCommand", "document": doc(d)}),
        MongoCall::Find(f) => {
            let mut v = json!({
                "kind": "find", "collection": f.collection, "filter": doc(&f.filter), "skip": f.skip, "explain": f.explain,
            });
            if let Some(p) = &f.projection {
                v["projection"] = json!(doc(p));
            }
            if let Some(s) = &f.sort {
                v["sort"] = json!(doc(s));
            }
            if let Some(l) = f.limit {
                v["limit"] = json!(l);
            }
            v
        }
        MongoCall::Aggregate { collection, pipeline } => {
            json!({"kind": "aggregate", "collection": collection, "pipeline": canonical(&Bson::Array(pipeline.clone()))})
        }
        MongoCall::Count { collection, filter } => {
            json!({"kind": "count", "collection": collection, "filter": doc(filter)})
        }
        MongoCall::Insert { collection, documents } => {
            json!({"kind": "insert", "collection": collection, "documents": documents.iter().map(doc).collect::<Vec<_>>()})
        }
        MongoCall::Update { collection, filter, update, multi, upsert } => json!({
            "kind": "update", "collection": collection, "filter": doc(filter), "update": canonical(update),
            "multi": multi, "upsert": upsert,
        }),
        MongoCall::Delete { collection, filter, multi } => {
            json!({"kind": "delete", "collection": collection, "filter": doc(filter), "multi": multi})
        }
        MongoCall::RenameCollection { from, to, drop_target } => {
            json!({"kind": "renameCollection", "from": from, "to": to, "dropTarget": drop_target})
        }
    }
}

#[test]
fn mongo_shell_values_match_swift() {
    let f = fixture("mongo-shell/values.json");
    let mut failures = Vec::new();
    for (text, want) in strings(&f["input"]["values"]).iter().zip(f["output"].as_array().unwrap()) {
        let got = match shell::parse_value(text) {
            Ok(v) => json!({"value": canonical(&v)}),
            Err(e) => json!({"error": e.display_message()}),
        };
        if &got != want {
            failures.push(format!("{text:?}\n  want {want}\n  got  {got}"));
        }
    }
    report("mongo-shell/values.json", failures);
}

#[test]
fn mongo_shell_statements_match_swift() {
    let f = fixture("mongo-shell/statements.json");
    let database = f["input"]["database"].as_str().unwrap();
    let mut failures = Vec::new();
    for (script, want) in strings(&f["input"]["scripts"]).iter().zip(f["output"].as_array().unwrap()) {
        let got = match shell::parse_script(script, database) {
            Ok(statements) => {
                let statements: Vec<Value> = statements
                    .iter()
                    .map(|st| {
                        let mut call = call_json(&st.call);
                        // `db.runCommand([1, 2])`: Swift keeps the array, the engine its document form ("0", "1", …);
                        // the server refuses both, so only the spelling differs.
                        if let (MongoCall::Command(d), Some(Value::String(w))) =
                            (&st.call, want.pointer("/statements/0/call/document"))
                            && w.starts_with('[')
                        {
                            call["document"] = json!(canonical(&Bson::Array(d.values().cloned().collect())));
                        }
                        let mut v = json!({
                            "text": st.text, "database": st.database, "call": call, "isWrite": st.call.is_write(),
                        });
                        if let Some(targets) = st.call.write_targets(&st.database) {
                            v["writeTargets"] = targets
                                .into_iter()
                                .map(|(database, collection)| match collection {
                                    Some(c) => json!({"database": database, "collection": c}),
                                    None => json!({"database": database}),
                                })
                                .collect();
                        }
                        v
                    })
                    .collect();
                json!({"statements": statements})
            }
            Err(e) => json!({"error": e.display_message()}),
        };
        if &got != want {
            failures.push(format!("{script:?}\n  want {want}\n  got  {got}"));
        }
    }
    report("mongo-shell/statements.json", failures);
}

#[test]
fn every_query_fixture_directory_is_checked() {
    for (dir, files) in [
        ("sql-split", vec!["mysql.json", "postgres.json"]),
        ("sql-safety", vec!["mysql.json", "postgres.json"]),
        ("danger", vec!["mongodb.json", "mysql.json", "postgres.json"]),
        ("mongo-shell", vec!["statements.json", "values.json"]),
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../parity").join(dir);
        let mut present: Vec<String> = std::fs::read_dir(&path)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        present.sort();
        assert_eq!(present, files, "{dir}: a sync added or removed a fixture; check it here too");
    }
}
