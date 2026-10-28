//! Parity helpers shared by the parity tests (included with `#[path]`): the normalization in Tests/Parity/README.md.
#![allow(dead_code)]
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Numbers as f64 (Swift writes `90`, serde `90.0`), recursively.
pub fn numbers_as_f64(v: Value) -> Value {
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
