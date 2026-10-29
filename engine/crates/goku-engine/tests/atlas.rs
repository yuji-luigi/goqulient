//! Optional cloud test: SRV + TLS against a MongoDB Atlas cluster. Runs only when GOKU_ATLAS_USER, GOKU_ATLAS_PASSWORD
//! and GOKU_ATLAS_HOST are set (`docker run --env-file .env.atlas`); never prints them. It creates and drops a scratch
//! database `rs_atlas_<random>`. Commands the free tier (M0) refuses are skipped with a note.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "a failing test panics")]
use goku_engine::DbError;
use goku_engine::limits::Paging;
use goku_engine::model::{ObjectKind, ObjectRef};
use goku_engine::profile::{ConnectionProfile, Credentials, DatabaseKind, SslMode, TopologyMode};
use goku_engine::session::{DatabaseSession, connect};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn atlas() -> Option<(ConnectionProfile, Credentials)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let (user, password, host) = (var("GOKU_ATLAS_USER")?, var("GOKU_ATLAS_PASSWORD")?, var("GOKU_ATLAS_HOST")?);
    let mut p = ConnectionProfile::new("atlas", DatabaseKind::Mongodb);
    p.mode = TopologyMode::Srv;
    p.srv_host = host.trim().to_owned();
    p.user = user.trim().to_owned();
    p.ssl.mode = SslMode::VerifyFull;
    p.connect_timeout_seconds = 20;
    p.query_timeout_seconds = 30;
    Some((p, Credentials { password: Some(password), ..Credentials::default() }))
}

/// The steps, as a `Result` so the scratch database is dropped even when one fails.
async fn steps(s: &dyn DatabaseSession, db: &str, cancel: &CancellationToken) -> Result<(), String> {
    let e = |what: &'static str| move |err: DbError| format!("{what}: {}", err.display_message());
    let info = s.server_info(cancel).await.map_err(e("server_info"))?;
    if !info.product.starts_with("MongoDB") || info.version.is_empty() {
        return Err(format!("server_info: unexpected {} {}", info.product, info.version));
    }
    eprintln!("atlas: {} {}", info.product, info.version);
    s.databases(cancel).await.map_err(e("databases"))?;
    let insert_users = r#"{"insert": "users", "documents": [
        {"_id": {"$oid": "65a000000000000000000001"}, "name": "Ann"},
        {"_id": {"$oid": "65a000000000000000000002"}, "name": "Bob", "managerId": {"$oid": "65a000000000000000000001"}}]}"#;
    let insert_posts = r#"{"insert": "posts", "documents": [
        {"_id": 1, "title": "Hello", "authorId": {"$oid": "65a000000000000000000001"}},
        {"_id": 2, "title": "Again", "authorId": {"$oid": "65a000000000000000000002"}}]}"#;
    s.execute(db, insert_users, cancel).await.map_err(e("insert users"))?;
    s.execute(db, insert_posts, cancel).await.map_err(e("insert posts"))?;
    let objects: Vec<String> =
        s.objects(db, "", cancel).await.map_err(e("objects"))?.into_iter().map(|o| o.name).collect();
    if objects != ["posts", "users"] {
        return Err(format!("objects: {objects:?}"));
    }
    let posts =
        ObjectRef { database: db.into(), schema: String::new(), name: "posts".into(), kind: ObjectKind::Collection };
    let page = s.browse(&posts, Paging::new(0, 1), None, cancel).await.map_err(e("browse"))?;
    if page.rows.len() != 1 || !page.truncated || page.rows[0][0].as_deref() != Some("1") {
        return Err(format!("browse: {} rows, truncated {}", page.rows.len(), page.truncated));
    }
    let model = s.introspect(db, &[], cancel).await.map_err(e("introspect"))?;
    let rels: Vec<&str> = model.relationships.iter().map(|r| r.id.as_str()).collect();
    if rels != ["posts(authorId)->users(_id)#posts.authorId"] {
        return Err(format!("introspect relationships: {rels:?}"));
    }
    Ok(())
}

#[tokio::test]
async fn atlas_srv_tls_round_trip() {
    let Some((mut profile, credentials)) = atlas() else {
        eprintln!("skipped: GOKU_ATLAS_* not set");
        return;
    };
    let db = format!("rs_atlas_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    profile.database = db.clone();
    let cancel = CancellationToken::new();
    let session = match connect(profile, credentials, &cancel).await {
        Ok(s) => s,
        Err(e) => panic!("connect to Atlas failed (IP access list?): {}", e.display_message()),
    };
    let result = tokio::time::timeout(Duration::from_secs(120), steps(session.as_ref(), &db, &cancel))
        .await
        .unwrap_or_else(|_| Err("timed out".into()));
    match session.execute(&db, r#"{"dropDatabase": 1}"#, &cancel).await {
        Ok(_) => {}
        Err(e) => eprintln!("atlas: dropDatabase refused, drop {db} by hand: {}", e.display_message()),
    }
    session.close().await;
    result.unwrap();
}
