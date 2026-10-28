//! MongoDB driver against the Docker `mongo` service (GOKU_IT=1): connect, catalog, browse (paging, sort, caps, cell and
//! document text), `maxTimeMS`, the client deadline, cancel, read-only and `close`. Scratch databases are `rs_mg_*`.
#[path = "support/mongo.rs"]
mod mongo_support;
mod support;

use bson::{Binary, DateTime, Decimal128, Document, doc, oid::ObjectId, spec::BinarySubtype};
use goku_engine::DbError;
use goku_engine::limits::{CLIP_MARKER, Paging};
use goku_engine::model::{ObjectKind, ObjectRef, Sort};
use goku_engine::profile::{DatabaseKind, HostPort};
use goku_engine::session::{DatabaseSession, connect};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

async fn session() -> Box<dyn DatabaseSession> {
    let (p, c) = support::profile(DatabaseKind::Mongodb);
    connect(p, c, &CancellationToken::new()).await.unwrap()
}

fn object(db: &str, name: &str) -> ObjectRef {
    ObjectRef { database: db.into(), schema: String::new(), name: name.into(), kind: ObjectKind::Collection }
}

#[tokio::test]
async fn connect_catalog_and_close() {
    if !support::enabled() {
        return;
    }
    support::run(60, async {
        let s = session().await;
        let cancel = CancellationToken::new();
        let info = s.server_info(&cancel).await.unwrap();
        assert_eq!(info.product, "MongoDB");
        assert!(info.version.starts_with("7."), "{}", info.version);
        assert!(!info.is_read_only_node && !info.connected_host.is_empty());
        let dbs = s.databases(&cancel).await.unwrap();
        assert!(dbs.contains(&"shop".to_owned()));
        let first_system = dbs.iter().position(|d| ["admin", "config", "local"].contains(&d.as_str())).unwrap();
        assert!(dbs[..first_system].contains(&"shop".to_owned()), "system databases come last: {dbs:?}");
        assert!(s.schemas("shop", &cancel).await.unwrap().is_empty());
        let names: Vec<String> = s.objects("shop", "", &cancel).await.unwrap().into_iter().map(|o| o.name).collect();
        for n in ["customers", "events", "orders", "products", "reviews"] {
            assert!(names.contains(&n.to_owned()), "{names:?}");
        }
        let mut sorted = names.clone();
        sorted.sort_by(|a, b| (a.starts_with("system."), a).cmp(&(b.starts_with("system."), b)));
        assert_eq!(names, sorted);
        let cols = s.columns(&object("shop", "customers"), &cancel).await.unwrap();
        assert_eq!(cols[0].name, "_id");
        assert!(cols[0].is_primary_key);
        let address = cols.iter().find(|c| c.name == "address").unwrap();
        assert_eq!((address.type_name.as_str(), address.nullable, address.presence), ("object", true, Some(0.5)));
        assert!(cols.iter().all(|c| !c.name.contains('.')), "top-level fields only");
        s.close().await;
        s.close().await;
        assert_eq!(s.server_info(&cancel).await, Err(DbError::NotConnected));
    })
    .await;
}

#[tokio::test]
async fn unreachable_hosts_fail_within_the_connect_timeout() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        // A non-routable address (packets dropped) and a closed port on a live host.
        for host in [HostPort::new("10.255.255.1", 27017), HostPort::new("mongo", 1)] {
            let (mut p, c) = support::profile(DatabaseKind::Mongodb);
            p.hosts = vec![host.clone()];
            p.connect_timeout_seconds = 2;
            let started = Instant::now();
            let e = match connect(p, c, &CancellationToken::new()).await {
                Ok(_) => panic!("{host} connected"),
                Err(e) => e,
            };
            let took = started.elapsed();
            assert!(took < Duration::from_millis(2600), "{host}: {took:?}");
            assert!(matches!(e, DbError::ConnectionFailed(_) | DbError::Timeout(_)), "{host}: {e:?}");
        }
        // Cancelling a connect stops it at once.
        let (mut p, c) = support::profile(DatabaseKind::Mongodb);
        p.hosts = vec![HostPort::new("10.255.255.1", 27017)];
        p.connect_timeout_seconds = 30;
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            token.cancel();
        });
        let started = Instant::now();
        assert!(matches!(connect(p, c, &cancel).await, Err(DbError::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(2));
    })
    .await;
}

#[tokio::test]
async fn max_time_deadline_and_cancel() {
    if !support::enabled() {
        return;
    }
    let raw = mongo_support::raw().await;
    let db = mongo_support::scratch("rs_mg_slow");
    let docs: Vec<Document> = (0..8).map(|i| doc! {"_id": i}).collect();
    raw.database(&db).collection::<Document>("c").insert_many(docs).await.unwrap();
    support::run(90, async {
        let (mut p, c) = support::profile(DatabaseKind::Mongodb);
        p.query_timeout_seconds = 1;
        let s = connect(p, c, &CancellationToken::new()).await.unwrap();
        let cancel = CancellationToken::new();
        // 8 × 1 s of server-side JavaScript: maxTimeMS (1 s) stops it on the server.
        let slow = r#"{"find": "c", "filter": {"$where": "sleep(1000) || true"}}"#;
        let started = Instant::now();
        let e = s.execute(&db, slow, &cancel).await.unwrap_err();
        assert!(e.to_string().contains("code 50"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
        // An explicit huge maxTimeMS: the client deadline (query + 5 s) gives up and drops the operation.
        let unbounded = r#"{"find": "c", "filter": {"$where": "sleep(1000) || true"}, "maxTimeMS": 600000}"#;
        let started = Instant::now();
        let e = s.execute(&db, unbounded, &cancel).await.unwrap_err();
        assert!(matches!(e, DbError::Timeout(_)), "{e:?}");
        assert!(started.elapsed() < Duration::from_secs(7), "{:?}", started.elapsed());
        // Cancel returns at once; the session keeps working.
        let token = CancellationToken::new();
        let t = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            t.cancel();
        });
        let started = Instant::now();
        assert_eq!(s.execute(&db, slow, &token).await, Err(DbError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(s.server_info(&cancel).await.is_ok());
        s.close().await;
    })
    .await;
    mongo_support::drop_db(&raw, &db).await;
}

#[tokio::test]
async fn browse_pages_sorts_caps_and_formats() {
    if !support::enabled() {
        return;
    }
    let raw = mongo_support::raw().await;
    let db = mongo_support::scratch("rs_mg_browse");
    let coll = raw.database(&db).collection::<Document>("items");
    let docs: Vec<Document> = (0..25).map(|i| doc! {"_id": i, "n": 100 - i, "s": format!("x{i}")}).collect();
    coll.insert_many(docs).await.unwrap();
    let oid = ObjectId::parse_str("65a000000000000000000001").unwrap();
    let typed = doc! {
        "_id": oid, "name": "typed", "long": 3_000_000_000i64, "double": 2.0, "ratio": 0.25,
        "price": "9.99".parse::<Decimal128>().unwrap(), "ok": true, "nothing": null,
        "when": DateTime::from_millis(1_709_212_455_123), "tags": ["a", 1], "nested": {"k": "v", "i": 1},
        "uid": Binary { subtype: BinarySubtype::Uuid, bytes: (0u8..16).collect() },
        "big": "y".repeat(40_000),
    };
    raw.database(&db).collection::<Document>("typed").insert_one(typed).await.unwrap();
    support::run(60, async {
        let s = session().await;
        let cancel = CancellationToken::new();
        let items = object(&db, "items");
        let r = s.browse(&items, Paging::new(5, 10), None, &cancel).await.unwrap();
        assert_eq!(r.rows.len(), 10);
        assert!(r.truncated, "more documents exist");
        assert_eq!(r.rows[0][0].as_deref(), Some("5"));
        let cols: Vec<_> = r.columns.iter().map(|c| (c.name.as_str(), c.type_name.as_str())).collect();
        assert_eq!(cols, [("_id", "int"), ("n", "int"), ("s", "string")]);
        assert_eq!(r.documents.as_ref().unwrap()[0], "{\n  _id: 5,\n  n: 95,\n  s: \"x5\"\n}");
        let r = s.browse(&items, Paging::new(20, 10), None, &cancel).await.unwrap();
        assert_eq!((r.rows.len(), r.truncated), (5, false));
        let by_n = Sort { column: "n".into(), descending: false };
        let r = s.browse(&items, Paging::new(0, 3), Some(&by_n), &cancel).await.unwrap();
        assert_eq!(r.rows.iter().map(|r| r[1].clone().unwrap()).collect::<Vec<_>>(), ["76", "77", "78"]);
        let desc = Sort { column: "_id".into(), descending: true };
        let r = s.browse(&items, Paging::new(0, 1), Some(&desc), &cancel).await.unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("24"));

        let r = s.browse(&object(&db, "typed"), Paging::default(), None, &cancel).await.unwrap();
        let cell = |name: &str| {
            let i = r.columns.iter().position(|c| c.name == name).unwrap();
            r.rows[0][i].clone()
        };
        assert_eq!(cell("_id").as_deref(), Some("ObjectId('65a000000000000000000001')"));
        assert_eq!(cell("name").as_deref(), Some("typed"));
        assert_eq!(cell("long").as_deref(), Some("NumberLong(3000000000)"));
        assert_eq!(cell("double").as_deref(), Some("2.0"));
        assert_eq!(cell("ratio").as_deref(), Some("0.25"));
        assert_eq!(cell("price").as_deref(), Some("NumberDecimal('9.99')"));
        assert_eq!(cell("ok").as_deref(), Some("true"));
        assert_eq!(cell("nothing"), None);
        assert_eq!(cell("when").as_deref(), Some("ISODate('2024-02-29T13:14:15.123Z')"));
        assert_eq!(cell("tags").as_deref(), Some("[\"a\", 1]"));
        assert_eq!(cell("nested").as_deref(), Some("{ k: \"v\", i: 1 }"));
        assert_eq!(cell("uid").as_deref(), Some("UUID('00010203-0405-0607-0809-0a0b0c0d0e0f')"));
        assert!(cell("big").unwrap().ends_with(CLIP_MARKER));
        assert!(r.documents.as_ref().unwrap()[0].ends_with(CLIP_MARKER), "documents are clipped too");
        let types: Vec<_> = r.columns.iter().map(|c| c.type_name.as_str()).collect();
        assert_eq!(types[..4], ["objectId", "string", "long", "double"]);
        s.close().await;

        // The profile's row cap wins over a larger page.
        let (mut p, c) = support::profile(DatabaseKind::Mongodb);
        p.max_rows = 3;
        let capped = connect(p, c, &cancel).await.unwrap();
        let r = capped.browse(&items, Paging::new(0, 100), None, &cancel).await.unwrap();
        assert_eq!((r.rows.len(), r.truncated, r.documents.unwrap().len()), (3, true, 3));
        // A cursor command: the first batch, capped, the leftover server cursor killed.
        let r = capped.execute(&db, r#"{"find": "items", "batchSize": 10}"#, &cancel).await.unwrap();
        assert_eq!((r.rows.len(), r.truncated), (3, true));
        let r = capped.execute(&db, r#"{"count": "items"}"#, &cancel).await.unwrap();
        let n = r.columns.iter().position(|c| c.name == "n").unwrap();
        assert_eq!(r.rows[0][n].as_deref(), Some("25"));
        capped.close().await;
    })
    .await;
    mongo_support::drop_db(&raw, &db).await;
}

#[tokio::test]
async fn read_only_profile_refuses_write_commands() {
    if !support::enabled() {
        return;
    }
    let raw = mongo_support::raw().await;
    let db = mongo_support::scratch("rs_mg_ro");
    raw.database(&db).collection::<Document>("c").insert_one(doc! {"_id": 1}).await.unwrap();
    support::run(30, async {
        let (mut p, c) = support::profile(DatabaseKind::Mongodb);
        p.read_only = true;
        let s = connect(p, c, &CancellationToken::new()).await.unwrap();
        let cancel = CancellationToken::new();
        for write in [
            r#"{"insert": "c", "documents": [{"_id": 2}]}"#,
            r#"{"drop": "c"}"#,
            r#"{"aggregate": "c", "pipeline": [{"$out": "d"}], "cursor": {}}"#,
        ] {
            assert_eq!(s.execute(&db, write, &cancel).await, Err(DbError::ReadOnly), "{write}");
        }
        let r = s.execute(&db, r#"{"find": "c"}"#, &cancel).await.unwrap();
        assert_eq!(r.rows.len(), 1);
        assert!(matches!(s.execute(&db, "not json", &cancel).await, Err(DbError::Parse(_))));
        s.close().await;
    })
    .await;
    let n = raw.database(&db).collection::<Document>("c").count_documents(doc! {}).await.unwrap();
    assert_eq!(n, 1, "nothing was written");
    let names = raw.database(&db).list_collection_names().await.unwrap();
    assert_eq!(names, ["c"]);
    mongo_support::drop_db(&raw, &db).await;
}
