//! Extended JSON (canonical or relaxed) → BSON with the key order of the text. serde_json's `Map` sorts keys, which would
//! move a command's name away from the first key (`{"find": …}`) and reorder documents, so objects are read as pair lists
//! and only the type wrappers (`{"$oid": …}`, `{"$date": …}`) go through bson's own Extended JSON parser.
use crate::error::{DbError, Result};
use bson::{Bson, Document};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Json;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> std::result::Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E>(self, v: bool) -> std::result::Result<Json, E> {
                Ok(Json::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> std::result::Result<Json, E> {
                Ok(Json::Number(v.into()))
            }
            fn visit_u64<E>(self, v: u64) -> std::result::Result<Json, E> {
                Ok(Json::Number(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Json, E> {
                serde_json::Number::from_f64(v).map(Json::Number).ok_or_else(|| E::custom("not a finite number"))
            }
            fn visit_str<E>(self, v: &str) -> std::result::Result<Json, E> {
                Ok(Json::String(v.to_owned()))
            }
            fn visit_string<E>(self, v: String) -> std::result::Result<Json, E> {
                Ok(Json::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Json, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_element()? {
                    out.push(v);
                }
                Ok(Json::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Json, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = a.next_entry()? {
                    out.push((k, v));
                }
                Ok(Json::Object(out))
            }
        }
        d.deserialize_any(V)
    }
}

/// Keys that make an object an Extended JSON type wrapper rather than a document.
const WRAPPERS: &[&str] = &[
    "$oid",
    "$symbol",
    "$numberInt",
    "$numberLong",
    "$numberDouble",
    "$numberDecimal",
    "$binary",
    "$code",
    "$timestamp",
    "$regularExpression",
    "$dbPointer",
    "$date",
    "$minKey",
    "$maxKey",
    "$undefined",
    "$uuid",
];

impl Json {
    /// Plain serde_json for a wrapper's inside (its key order doesn't matter).
    fn plain(self) -> serde_json::Value {
        match self {
            Json::Null => serde_json::Value::Null,
            Json::Bool(b) => b.into(),
            Json::Number(n) => serde_json::Value::Number(n),
            Json::String(s) => s.into(),
            Json::Array(a) => a.into_iter().map(Json::plain).collect(),
            Json::Object(o) => serde_json::Value::Object(o.into_iter().map(|(k, v)| (k, v.plain())).collect()),
        }
    }

    fn bson(self) -> Result<Bson> {
        Ok(match self {
            Json::Null => Bson::Null,
            Json::Bool(b) => Bson::Boolean(b),
            // Relaxed Extended JSON: integers that fit are int32, then int64, else double (as bson's own parser does).
            Json::Number(n) => match n.as_i64() {
                Some(i) => i32::try_from(i).map(Bson::Int32).unwrap_or(Bson::Int64(i)),
                None => Bson::Double(n.as_f64().unwrap_or(f64::NAN)),
            },
            Json::String(s) => Bson::String(s),
            Json::Array(a) => Bson::Array(a.into_iter().map(Json::bson).collect::<Result<_>>()?),
            Json::Object(o) if o.first().is_some_and(|(k, _)| WRAPPERS.contains(&k.as_str())) => {
                let v = Json::Object(o).plain();
                Bson::try_from(v).map_err(|e| DbError::Parse(e.to_string()))?
            }
            Json::Object(o) => Bson::Document(document(o)?),
        })
    }
}

fn document(pairs: Vec<(String, Json)>) -> Result<Document> {
    let mut d = Document::new();
    for (k, v) in pairs {
        d.insert(k, v.bson()?);
    }
    Ok(d)
}

/// One Extended JSON object (e.g. a `db.runCommand` document or a fixture document) as a BSON document, key order kept.
pub fn parse_document(text: &str) -> Result<Document> {
    match serde_json::from_str::<Json>(text).map_err(|e| DbError::Parse(e.to_string()))? {
        Json::Object(o) => document(o),
        _ => Err(DbError::Parse("Expected a JSON object, e.g. {\"ping\": 1}.".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;

    #[test]
    fn keeps_key_order_and_types() {
        let d = parse_document(
            r#"{"find":"c","filter":{"_id":{"$oid":"65a000000000000000000001"}},"limit":{"$numberLong":"5"},"z":1,"a":2.5,
                "big":3000000000,"ref":{"$ref":"users","$id":{"$numberInt":"1"}},"when":{"$date":{"$numberLong":"0"}}}"#,
        )
        .unwrap();
        assert_eq!(
            d.keys().map(String::as_str).collect::<Vec<_>>(),
            ["find", "filter", "limit", "z", "a", "big", "ref", "when"]
        );
        assert_eq!(
            d.get_document("filter").unwrap().get("_id"),
            Some(&Bson::ObjectId(ObjectId::parse_str("65a000000000000000000001").unwrap()))
        );
        assert_eq!(
            (d.get("limit"), d.get("z"), d.get("a")),
            (Some(&Bson::Int64(5)), Some(&Bson::Int32(1)), Some(&Bson::Double(2.5)))
        );
        assert_eq!(d.get("big"), Some(&Bson::Int64(3_000_000_000)));
        let r = d.get_document("ref").unwrap();
        assert_eq!(r.keys().map(String::as_str).collect::<Vec<_>>(), ["$ref", "$id"], "a DBRef stays a document");
        assert!(matches!(d.get("when"), Some(Bson::DateTime(_))));
        assert!(parse_document("[1]").is_err());
        assert!(parse_document("{").is_err());
        assert!(parse_document(r#"{"x":{"$oid":"nope"}}"#).is_err());
    }
}
