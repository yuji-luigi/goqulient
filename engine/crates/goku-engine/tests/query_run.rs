//! `query::run` (the `query/run` method) on every engine against the Docker databases (GOKU_IT=1): multi-statement
//! scripts and their spans, stopping at the first error, read-only refusal before anything is sent (and the server's
//! own refusal), confirmation of destructive statements, the row cap, cancel and timeouts. Scratch objects are
//! `rs_pg_q_*`, `rs_my_q_*` (tables in `shop`) and `rs_mg_q_*` (databases), with random suffixes, dropped at the end.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "a failing test panics")]
#[path = "support/mongo.rs"]
mod mongo_support;
mod support;

use goku_engine::DbError;
use goku_engine::profile::{ConnectionProfile, Credentials, DatabaseKind};
use goku_engine::query::{self, ResultKind, RunOptions, RunOutcome, Span, StatementResult};
use goku_engine::session::{DatabaseSession, connect};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

macro_rules! require_it {
    () => {
        if !support::enabled() {
            eprintln!("skipped: GOKU_IT not set");
            return;
        }
    };
}

fn none() -> CancellationToken {
    CancellationToken::new()
}

fn suffix() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}

async fn open(p: ConnectionProfile, c: Credentials) -> Box<dyn DatabaseSession> {
    connect(p, c, &none()).await.unwrap_or_else(|e| panic!("connect: {e}"))
}

async fn run_with(s: &dyn DatabaseSession, text: &str, options: RunOptions, cancel: &CancellationToken) -> RunOutcome {
    query::run(s, text, &options, cancel).await.unwrap_or_else(|e| panic!("{text}: {e}"))
}

async fn results(s: &dyn DatabaseSession, text: &str) -> Vec<StatementResult> {
    match run_with(s, text, RunOptions::default(), &none()).await {
        RunOutcome::Results { results } => results,
        other => panic!("{text}: {other:?}"),
    }
}

async fn confirmed(s: &dyn DatabaseSession, text: &str) -> Vec<StatementResult> {
    match run_with(s, text, RunOptions { confirmed: true, ..RunOptions::default() }, &none()).await {
        RunOutcome::Results { results } => results,
        other => panic!("{text}: {other:?}"),
    }
}

fn kinds(r: &[StatementResult]) -> Vec<ResultKind> {
    r.iter().map(|r| r.kind).collect()
}

/// The text a span points at (UTF-16 offsets, as an editor counts them).
fn at(text: &str, span: Span) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16(&units[span.start..span.end]).unwrap()
}

fn cell(r: &StatementResult) -> Option<&str> {
    r.rows.first().and_then(|row| row.first()).and_then(|c| c.as_deref())
}

fn error_kind(r: &StatementResult) -> &'static str {
    r.error.as_ref().map_or("", |e| e.kind)
}

/// Cancels `cancel` after `ms`.
fn cancel_after(ms: u64) -> CancellationToken {
    let cancel = CancellationToken::new();
    let k = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        k.cancel();
    });
    cancel
}

/// Polls a one-cell count on `s` until it is 0: the server stopped the statement.
async fn until_zero(s: &dyn DatabaseSession, sql: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let r = s.execute("shop", sql, &none()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        if r.rows[0][0].as_deref() == Some("0") {
            return;
        }
        assert!(Instant::now() < deadline, "still running on the server: {sql}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// MARK: - PostgreSQL

#[tokio::test]
async fn postgres_script_errors_and_spans() {
    require_it!();
    support::run(60, async {
        let (p, c) = support::profile(DatabaseKind::Postgres);
        let s = open(p, c).await;
        let t = format!("rs_pg_q_{}", suffix());
        let script = format!(
            "CREATE TABLE {t} (id int PRIMARY KEY, note text);\n-- é 🇮🇹\nINSERT INTO {t} VALUES (1, 'a;b'), (2, $$c;d$$);\n\
             SELECT id, note FROM {t} ORDER BY id;\nDO $$ BEGIN RAISE NOTICE 'hello %', 42; END $$"
        );
        let r = confirmed(&*s, &script).await;
        assert_eq!(kinds(&r), [ResultKind::Affected, ResultKind::Affected, ResultKind::Rows, ResultKind::Affected]);
        assert_eq!(r[1].affected_rows, Some(2));
        assert_eq!(r[2].rows, [[Some("1".into()), Some("a;b".into())], [Some("2".into()), Some("c;d".into())]]);
        assert_eq!(r[3].notices, ["NOTICE: hello 42"]);
        for result in &r {
            assert_eq!(at(&script, result.span), result.statement, "the span points at the statement");
            assert_eq!(result.database, "shop");
        }
        assert!(at(&script, r[1].span).starts_with("-- é 🇮🇹\nINSERT"), "comments before a statement belong to it");

        // The script stops at the first error, which says which statement failed.
        let script = format!("SELECT 1; SELECT * FROM {t}_missing; INSERT INTO {t} VALUES (3, 'never')");
        let r = results(&*s, &script).await;
        assert_eq!(kinds(&r), [ResultKind::Rows, ResultKind::Error]);
        assert_eq!(at(&script, r[1].span), format!("SELECT * FROM {t}_missing"));
        let e = r[1].error.as_ref().unwrap();
        assert!(e.kind == "query" && e.message.starts_with("[42P01]"), "{e:?}");
        assert_eq!(cell(&results(&*s, &format!("SELECT count(*) FROM {t}")).await[0]), Some("2"), "nothing after it ran");
        s.execute("shop", &format!("DROP TABLE {t}"), &none()).await.unwrap();
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn postgres_read_only_is_checked_before_sending_and_by_the_server() {
    require_it!();
    support::run(60, async {
        let (p, c) = support::profile(DatabaseKind::Postgres);
        let rw = open(p.clone(), c.clone()).await;
        let t = format!("rs_pg_q_{}", suffix());
        for sql in [format!("CREATE TABLE {t} (id int)"), format!("CREATE SEQUENCE {t}_seq")] {
            rw.execute("shop", &sql, &none()).await.unwrap();
        }
        let mut ro = p.clone();
        ro.read_only = true;
        let s = open(ro, c).await;
        // Refused by the client: nothing is sent, so the DROP never reaches the server (and needs no confirmation).
        let r = results(&*s, &format!("SELECT 1; DROP TABLE {t}; SELECT 2")).await;
        assert_eq!(kinds(&r), [ResultKind::Rows, ResultKind::Error]);
        assert_eq!(error_kind(&r[1]), "readOnly");
        assert!(rw.execute("shop", &format!("SELECT * FROM {t}"), &none()).await.is_ok(), "the table is still there");
        // A write the client check can't see (`SELECT nextval(…)`) is refused by the read-only session.
        let r = results(&*s, &format!("SELECT nextval('{t}_seq')")).await;
        assert_eq!(error_kind(&r[0]), "serverReadOnly", "{:?}", r[0].error);
        s.close().await;
        for sql in [format!("DROP TABLE {t}"), format!("DROP SEQUENCE {t}_seq")] {
            rw.execute("shop", &sql, &none()).await.unwrap();
        }
        rw.close().await;
    })
    .await;
}

#[tokio::test]
async fn postgres_destructive_statements_wait_for_confirmation() {
    require_it!();
    support::run(60, async {
        let (p, c) = support::profile(DatabaseKind::Postgres);
        let s = open(p, c).await;
        let t = format!("rs_pg_q_{}", suffix());
        s.execute("shop", &format!("CREATE TABLE {t} (id int)"), &none()).await.unwrap();
        let script = format!("INSERT INTO {t} VALUES (1), (2);\nDELETE FROM {t}");
        let RunOutcome::NeedsConfirmation { findings } = run_with(&*s, &script, RunOptions::default(), &none()).await
        else {
            panic!("a DELETE without WHERE needs confirmation")
        };
        assert_eq!(findings.len(), 1);
        assert_eq!(
            (findings[0].finding.action.as_str(), findings[0].finding.object.as_deref()),
            ("deletes every row in", Some(t.as_str()))
        );
        assert_eq!(at(&script, findings[0].span), format!("DELETE FROM {t}"));
        let count = format!("SELECT count(*) FROM {t}");
        assert_eq!(cell(&results(&*s, &count).await[0]), Some("0"), "nothing ran, not even the INSERT before it");
        let r = confirmed(&*s, &script).await;
        assert_eq!((r[0].affected_rows, r[1].affected_rows), (Some(2), Some(2)));
        s.execute("shop", &format!("DROP TABLE {t}"), &none()).await.unwrap();
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn postgres_row_cap_cancel_and_timeout() {
    require_it!();
    support::run(60, async {
        let (mut p, c) = support::profile(DatabaseKind::Postgres);
        p.query_timeout_seconds = 1;
        let s = open(p.clone(), c.clone()).await;
        // The cap: 5 rows, truncated, and the session keeps working (the rest was cancelled on the server).
        let options = RunOptions { max_rows: Some(5), ..RunOptions::default() };
        let RunOutcome::Results { results: r } =
            run_with(&*s, "SELECT generate_series(1, 100000) AS n; SELECT 'after'", options, &none()).await
        else {
            panic!()
        };
        assert_eq!((r[0].rows.len(), r[0].truncated), (5, true));
        assert_eq!(cell(&r[1]), Some("after"));
        // The server's statement_timeout ends the script with that statement's error.
        let r = results(&*s, "SELECT 1; SELECT pg_sleep(5); SELECT 3").await;
        assert_eq!(kinds(&r), [ResultKind::Rows, ResultKind::Error]);
        assert!(r[1].error.as_ref().unwrap().message.starts_with("[57014]"), "{:?}", r[1].error);
        s.close().await;

        // Cancel: the request fails as cancelled and the server stops the query.
        p.query_timeout_seconds = 30;
        let s = open(p, c).await;
        let marker = format!("rs_pg_q_cancel_{}", suffix());
        let started = Instant::now();
        let r =
            query::run(&*s, &format!("SELECT pg_sleep(20) AS {marker}"), &RunOptions::default(), &cancel_after(300))
                .await;
        assert_eq!(r.err(), Some(DbError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        until_zero(
            &*s,
            &format!(
                "SELECT count(*)::text FROM pg_stat_activity WHERE query LIKE '%{marker}%' AND state = 'active' \
                 AND pid <> pg_backend_pid()"
            ),
        )
        .await;
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn postgres_copy_through_the_client_is_refused() {
    require_it!();
    support::run(30, async {
        let (p, c) = support::profile(DatabaseKind::Postgres);
        let s = open(p, c).await;
        let t = format!("rs_pg_q_{}", suffix());
        s.execute("shop", &format!("CREATE TABLE {t} (id int)"), &none()).await.unwrap();
        // The editor has no data to stream: refused before anything is sent, and the session keeps working.
        let r = results(&*s, &format!("COPY {t} FROM STDIN; SELECT 1")).await;
        assert_eq!((kinds(&r), error_kind(&r[0])), (vec![ResultKind::Error], "unsupported"));
        assert_eq!(cell(&results(&*s, "SELECT 2").await[0]), Some("2"));
        s.execute("shop", &format!("DROP TABLE {t}"), &none()).await.unwrap();
        s.close().await;
    })
    .await;
}

// MARK: - MySQL

#[tokio::test]
async fn mysql_script_use_errors_and_confirmation() {
    require_it!();
    support::run(60, async {
        let (p, c) = support::profile(DatabaseKind::Mysql);
        let s = open(p, c).await;
        let t = format!("rs_my_q_{}", suffix());
        let script = format!(
            "CREATE TABLE {t} (id INT PRIMARY KEY, note TEXT);\nINSERT INTO {t} VALUES (1, 'a;b'), (2, \"c\\\";d\");\n\
             DELIMITER //\nSELECT id, note FROM {t} ORDER BY id //\nDELIMITER ;\nUSE analytics; SELECT DATABASE()"
        );
        let r = results(&*s, &script).await;
        assert_eq!(
            kinds(&r),
            [ResultKind::Affected, ResultKind::Affected, ResultKind::Rows, ResultKind::Affected, ResultKind::Rows]
        );
        assert_eq!(r[2].rows, [[Some("1".into()), Some("a;b".into())], [Some("2".into()), Some("c\";d".into())]]);
        assert_eq!((r[4].database.as_str(), cell(&r[4])), ("analytics", Some("analytics")), "USE moves the rest");
        for result in &r {
            assert_eq!(at(&script, result.span), result.statement);
        }

        let r =
            results(&*s, &format!("SELECT 1; SELECT * FROM {t}_missing; INSERT INTO {t} VALUES (3, 'never')")).await;
        assert_eq!(kinds(&r), [ResultKind::Rows, ResultKind::Error]);
        assert!(r[1].error.as_ref().unwrap().message.contains("1146"), "{:?}", r[1].error);

        let script = format!("INSERT INTO {t} VALUES (3, 'x'); UPDATE {t} SET note = 'all'");
        let RunOutcome::NeedsConfirmation { findings } = run_with(&*s, &script, RunOptions::default(), &none()).await
        else {
            panic!("an UPDATE without WHERE needs confirmation")
        };
        assert_eq!(findings[0].finding.action, "updates every row in");
        assert_eq!(cell(&results(&*s, &format!("SELECT COUNT(*) FROM {t}")).await[0]), Some("2"), "nothing ran");
        let r = confirmed(&*s, &script).await;
        assert_eq!((r[0].affected_rows, r[1].affected_rows), (Some(1), Some(3)));
        s.execute("shop", &format!("DROP TABLE {t}"), &none()).await.unwrap();
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn mysql_read_only_row_cap_cancel_and_timeout() {
    require_it!();
    support::run(90, async {
        let (mut p, c) = support::profile(DatabaseKind::Mysql);
        let rw = open(p.clone(), c.clone()).await;
        let t = format!("rs_my_q_{}", suffix());
        rw.execute("shop", &format!("CREATE TABLE {t} (id INT)"), &none()).await.unwrap();
        let mut ro = p.clone();
        ro.read_only = true;
        let s = open(ro, c.clone()).await;
        let r = results(&*s, &format!("SELECT 1; INSERT INTO {t} VALUES (1)")).await;
        assert_eq!((kinds(&r), error_kind(&r[1])), (vec![ResultKind::Rows, ResultKind::Error], "readOnly"));
        assert_eq!(
            rw.execute("shop", &format!("SELECT COUNT(*) FROM {t}"), &none()).await.unwrap().rows[0][0].as_deref(),
            Some("0")
        );
        s.close().await;
        rw.execute("shop", &format!("DROP TABLE {t}"), &none()).await.unwrap();

        // Cap: 5 rows of 810,000, truncated; the query is killed and the next statement runs on a fresh connection.
        p.query_timeout_seconds = 1;
        let s = open(p.clone(), c.clone()).await;
        let many = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 900) SELECT a.n FROM s a, s b";
        let options = RunOptions { max_rows: Some(5), ..RunOptions::default() };
        let RunOutcome::Results { results: r } =
            run_with(&*s, &format!("{many}; SELECT 'after'"), options, &none()).await
        else {
            panic!()
        };
        assert_eq!((r[0].rows.len(), r[0].truncated, cell(&r[1])), (5, true, Some("after")));
        // The client deadline (query + 2 s) for a statement max_execution_time doesn't cover (DO, confirmed: it runs
        // code the danger check can't see).
        let started = Instant::now();
        let r = confirmed(&*s, "SELECT 1; DO SLEEP(30); SELECT 3").await;
        assert_eq!((kinds(&r), error_kind(&r[1])), (vec![ResultKind::Rows, ResultKind::Error], "timeout"));
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        s.close().await;

        // Cancel kills the query on the server.
        p.query_timeout_seconds = 60;
        let s = open(p, c.clone()).await;
        let marker = format!("rs_my_q_cancel_{}", suffix());
        let started = Instant::now();
        let r = query::run(&*s, &format!("SELECT SLEEP(30) AS {marker}"), &RunOptions::default(), &cancel_after(500)).await;
        assert_eq!(r.err(), Some(DbError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        until_zero(
            &*rw,
            &format!(
                "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE '%{marker}%' AND ID <> CONNECTION_ID()"
            ),
        )
        .await;
        s.close().await;
        rw.close().await;
    })
    .await;
}

// MARK: - MongoDB

fn mongo_profile(db: &str) -> (ConnectionProfile, Credentials) {
    let (mut p, c) = support::profile(DatabaseKind::Mongodb);
    p.database = db.into();
    (p, c)
}

#[tokio::test]
async fn mongo_shell_scripts_writes_and_confirmation() {
    require_it!();
    let raw = mongo_support::raw().await;
    let db = mongo_support::scratch("rs_mg_q");
    support::run(60, async {
        let (p, c) = mongo_profile(&db);
        let s = open(p, c).await;
        let script = "db.items.insertOne({ _id: 1, name: 'a' })\n\
                      db.items.insertMany([{ _id: 2, name: 'b' }, { _id: 3, name: 'c' }]);\n\
                      db.items.updateOne({ _id: 2 }, { $set: { name: 'B' } })\n\
                      db.items.find({}, { name: 1 }).sort({ _id: -1 })\n\
                      db.items.countDocuments({ _id: { $gt: 1 } }); use other; db.getName()";
        let r = results(&*s, script).await;
        assert_eq!(
            kinds(&r),
            [
                ResultKind::Affected,
                ResultKind::Affected,
                ResultKind::Affected,
                ResultKind::Rows,
                ResultKind::Rows,
                ResultKind::Message,
                ResultKind::Rows
            ]
        );
        assert_eq!(r[0].message.as_deref(), Some("inserted _id 1"));
        assert_eq!(r[2].message.as_deref(), Some("matched 1, modified 1"));
        assert_eq!(r[3].documents.as_ref().unwrap().len(), 3);
        assert_eq!(cell(&r[4]), Some("2"));
        assert_eq!((r[6].database.as_str(), cell(&r[6])), ("other", Some("other")), "use moves the rest");
        for result in &r {
            assert_eq!(at(script, result.span), result.statement);
        }

        // Stops at the first error; a duplicate key is an error even though the server answers ok: 1.
        let r =
            results(&*s, "db.items.find().limit(1); db.items.insertOne({ _id: 1 }); db.items.deleteOne({ _id: 2 })")
                .await;
        assert_eq!(kinds(&r), [ResultKind::Rows, ResultKind::Error]);
        assert!(r[1].error.as_ref().unwrap().message.contains("E11000"), "{:?}", r[1].error);
        assert_eq!(cell(&results(&*s, "db.items.countDocuments({})").await[0]), Some("3"), "nothing after it ran");
        // A script that doesn't parse runs nothing.
        let r = results(&*s, "db.items.deleteMany({}); db.items.find(").await;
        assert_eq!((r.len(), error_kind(&r[0])), (1, "parse"));

        let script = "db.items.insertOne({ _id: 9 }); db.items.deleteMany({})";
        let RunOutcome::NeedsConfirmation { findings } = run_with(&*s, script, RunOptions::default(), &none()).await
        else {
            panic!("deleteMany({{}}) needs confirmation")
        };
        assert_eq!(findings[0].finding.warning, "Deletes every document in items");
        assert_eq!(cell(&results(&*s, "db.items.countDocuments({})").await[0]), Some("3"), "nothing ran");
        let r = confirmed(&*s, script).await;
        assert_eq!(r[1].affected_rows, Some(4));
        s.close().await;
    })
    .await;
    mongo_support::drop_db(&raw, &db).await;
}

#[tokio::test]
async fn mongo_read_only_row_cap_cancel_and_timeout() {
    require_it!();
    let raw = mongo_support::raw().await;
    let db = mongo_support::scratch("rs_mg_q");
    let docs: Vec<bson::Document> = (0..20).map(|i| bson::doc! {"_id": i}).collect();
    raw.database(&db).collection::<bson::Document>("c").insert_many(docs).await.unwrap();
    support::run(90, async {
        let (mut p, c) = mongo_profile(&db);
        let mut ro = p.clone();
        ro.read_only = true;
        let s = open(ro, c.clone()).await;
        // MongoDB has no server-side read-only: the client check refuses every write, including untargeted ones.
        for write in ["db.c.insertOne({ _id: 99 })", "db.c.aggregate([{ $out: 'copy' }])", "{ applyOps: [] }"] {
            let r = results(&*s, &format!("db.c.find().limit(1); {write}")).await;
            assert_eq!(
                (kinds(&r), error_kind(&r[1])),
                (vec![ResultKind::Rows, ResultKind::Error], "readOnly"),
                "{write}"
            );
        }
        s.close().await;
        let count = raw.database(&db).collection::<bson::Document>("c").count_documents(bson::doc! {}).await.unwrap();
        assert_eq!(count, 20);

        p.query_timeout_seconds = 1;
        let s = open(p.clone(), c.clone()).await;
        let options = RunOptions { max_rows: Some(5), ..RunOptions::default() };
        let RunOutcome::Results { results: r } =
            run_with(&*s, "db.c.find(); db.c.aggregate([{ $match: {} }]); db.c.find().limit(3)", options, &none())
                .await
        else {
            panic!()
        };
        assert_eq!((r[0].rows.len(), r[0].truncated), (5, true));
        assert_eq!((r[1].rows.len(), r[1].truncated), (5, true));
        assert_eq!((r[2].rows.len(), r[2].truncated), (3, false), "the user's own limit is a complete answer");
        // maxTimeMS (the query timeout) stops 20 × 1 s of server-side JavaScript.
        let started = Instant::now();
        let r = results(&*s, "db.c.find({ $where: 'sleep(1000) || true' })").await;
        assert!(r[0].error.as_ref().unwrap().message.contains("code 50"), "{:?}", r[0].error);
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
        s.close().await;

        // Cancel returns at once; maxTimeMS (5 s here) ends the dropped operation on the server, as for other requests.
        p.query_timeout_seconds = 5;
        let s = open(p, c).await;
        let started = Instant::now();
        let slow = "db.c.find({ $where: 'sleep(1000) || true' })";
        let r = query::run(&*s, slow, &RunOptions::default(), &cancel_after(300)).await;
        assert_eq!(r.err(), Some(DbError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert_eq!(cell(&results(&*s, "db.c.countDocuments({})").await[0]), Some("20"), "the session keeps working");
        s.close().await;
    })
    .await;
    mongo_support::drop_db(&raw, &db).await;
}
