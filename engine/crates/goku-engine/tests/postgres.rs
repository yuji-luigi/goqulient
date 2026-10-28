//! PostgreSQL driver against the Docker `pg-primary` (GOKU_IT=1). Scratch objects are named `rs_pg_*` and dropped.
//! TLS: the Docker server has no TLS, so only prefer/require/bad files are checked there. With `PG_TLS_HOST` (a server
//! whose certificate names that host) and `PG_TLS_CA` (its CA, PEM) set, the verifying modes are checked too.
mod support;

use goku_engine::DbError;
use goku_engine::limits::{Paging, is_clipped};
use goku_engine::model::{ObjectKind, ObjectRef, Sort};
use goku_engine::profile::{
    ConnectionProfile, Credentials, DatabaseKind, HostPort, SslMode, TargetSession, TopologyMode,
};
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

fn pg() -> (ConnectionProfile, Credentials) {
    support::profile(DatabaseKind::Postgres)
}

fn none() -> CancellationToken {
    CancellationToken::new()
}

async fn open(p: ConnectionProfile, c: Credentials) -> Box<dyn DatabaseSession> {
    connect(p, c, &none()).await.unwrap_or_else(|e| panic!("connect: {e}"))
}

async fn exec(s: &dyn DatabaseSession, sql: &str) {
    s.execute("shop", sql, &none()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn scalar(s: &dyn DatabaseSession, sql: &str) -> Option<String> {
    let r = s.execute("shop", sql, &none()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    r.rows[0][0].clone()
}

/// Polls `sql` (one text cell) on a fresh session until it returns `want`, for server-side effects that lag a little.
async fn eventually(sql: &str, want: &str) {
    let (p, c) = pg();
    let monitor = open(p, c).await;
    let mut last = None;
    for _ in 0..40 {
        last = scalar(&*monitor, sql).await;
        if last.as_deref() == Some(want) {
            monitor.close().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{sql}: wanted {want}, last {last:?}");
}

fn table(name: &str) -> ObjectRef {
    ObjectRef { database: "shop".into(), schema: "public".into(), name: name.into(), kind: ObjectKind::Table }
}

#[tokio::test]
async fn connect_and_catalog() {
    require_it!();
    support::run(60, async {
        let (p, c) = pg();
        let s = open(p, c).await;
        let info = s.server_info(&none()).await.unwrap();
        assert_eq!((info.product.as_str(), info.current_user.as_str()), ("PostgreSQL", "dbc"));
        assert!(info.version.starts_with("17"), "{}", info.version);
        assert!(info.connected_host.ends_with(":5432") && !info.is_read_only_node);
        let dbs = s.databases(&none()).await.unwrap();
        assert!(dbs.contains(&"shop".into()) && dbs.contains(&"postgres".into()), "{dbs:?}");
        let schemas = s.schemas("shop", &none()).await.unwrap();
        assert_eq!(schemas[0], "public");
        assert!(schemas.contains(&"inventory".into()) && !schemas.iter().any(|n| n.starts_with("pg_")), "{schemas:?}");

        let objects = s.objects("shop", "public", &none()).await.unwrap();
        let names: Vec<&str> = objects.iter().map(|o| o.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "ordered by name");
        let kind = |n: &str| objects.iter().find(|o| o.name == n).map(|o| o.kind);
        assert_eq!(kind("customers"), Some(ObjectKind::Table));
        assert_eq!(kind("order_totals"), Some(ObjectKind::View));
        assert!(kind("customers_id_seq").is_none(), "sequences aren't listed");
        assert!(objects.iter().all(|o| o.database == "shop" && o.schema == "public"));
        // Another database goes through its own connection.
        assert!(s.schemas("postgres", &none()).await.unwrap().contains(&"public".into()));

        let cols = s.columns(&table("customers"), &none()).await.unwrap();
        let names: Vec<&str> = cols.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["id", "email", "full_name", "created_at", "profile"]);
        assert!(cols[0].is_primary_key && !cols[0].nullable && cols[0].type_name == "bigint");
        assert!(cols[1].is_unique && cols[1].type_name == "character varying(255)");
        assert_eq!(cols[3].default_value.as_deref(), Some("now()"));
        assert!(cols[4].nullable);
        let orders = s.columns(&table("orders"), &none()).await.unwrap();
        assert!(orders.iter().find(|a| a.name == "customer_id").unwrap().is_foreign_key);
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn wrong_password_fails_without_echoing_it() {
    require_it!();
    support::run(30, async {
        let (p, _) = pg();
        let c = Credentials { password: Some("not-the-password".into()), ..Credentials::default() };
        let e = connect(p, c, &none()).await.err().expect("must fail");
        let m = e.display_message();
        assert!(matches!(e, DbError::ConnectionFailed(_)), "{m}");
        assert!(m.contains("28P01") && !m.contains("not-the-password"), "{m}");
    })
    .await;
}

#[tokio::test]
async fn unreachable_hosts_fail_fast() {
    require_it!();
    support::run(30, async {
        let (mut p, c) = pg();
        p.connect_timeout_seconds = 2;
        p.hosts = vec![HostPort::new("10.255.255.1", 5432)];
        let t = Instant::now();
        let e = connect(p.clone(), c.clone(), &none()).await.err().expect("must fail");
        assert!(t.elapsed() < Duration::from_millis(3500), "{:?}", t.elapsed());
        assert!(matches!(&e, DbError::ConnectionFailed(m) if m.starts_with("10.255.255.1:5432: ")), "{e}");

        p.hosts = vec![HostPort::new(support::profile(DatabaseKind::Postgres).0.hosts[0].host.clone(), 1)];
        let e = connect(p, c, &none()).await.err().expect("closed port");
        assert!(matches!(&e, DbError::ConnectionFailed(m) if m.contains("error connecting to server")), "{e}");

        // A cancelled connect stops at once.
        let (mut p, c) = pg();
        p.hosts = vec![HostPort::new("10.255.255.1", 5432)];
        let cancel = CancellationToken::new();
        let k = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            k.cancel();
        });
        let t = Instant::now();
        assert_eq!(connect(p, c, &cancel).await.err(), Some(DbError::Cancelled));
        assert!(t.elapsed() < Duration::from_secs(2));
    })
    .await;
}

#[tokio::test]
async fn multi_host_picks_by_target_session() {
    require_it!();
    support::run(60, async {
        let (mut p, c) = pg();
        let good = p.hosts[0].clone();
        p.mode = TopologyMode::MultiHost;
        p.hosts = vec![HostPort::new(good.host.clone(), 1), good.clone()];
        for target in
            [TargetSession::Any, TargetSession::Primary, TargetSession::ReadWrite, TargetSession::PreferStandby]
        {
            p.target_session = target;
            let s = open(p.clone(), c.clone()).await;
            assert_eq!(s.server_info(&none()).await.unwrap().connected_host, good.to_string(), "{target:?}");
            s.close().await;
        }
        p.target_session = TargetSession::Standby;
        let e = connect(p.clone(), c.clone(), &none()).await.err().expect("no standby");
        let DbError::NoSuitableHost(m) = &e else { panic!("{e}") };
        assert!(m.contains(&format!("{}:1: ", good.host)), "{m}");
        assert!(m.contains(&format!("{good}: primary does not match Standby / replica")), "{m}");
        p.target_session = TargetSession::ReadOnly;
        assert!(matches!(connect(p, c, &none()).await, Err(DbError::NoSuitableHost(_))));
    })
    .await;
}

#[tokio::test]
async fn tls_modes() {
    require_it!();
    support::run(60, async {
        let (mut p, c) = pg();
        p.ssl.mode = SslMode::Prefer; // the Docker server has ssl = off: prefer falls back to plaintext
        open(p.clone(), c.clone()).await.close().await;
        for mode in [SslMode::Require, SslMode::VerifyCa, SslMode::VerifyFull] {
            p.ssl.mode = mode;
            let e = connect(p.clone(), c.clone(), &none()).await.err().expect("no TLS on the server");
            assert!(matches!(&e, DbError::ConnectionFailed(m) if m.contains("TLS")), "{mode:?}: {e}");
        }
        p.ssl.ca_certificate_path = "/nonexistent/ca.pem".into();
        assert!(matches!(connect(p.clone(), c.clone(), &none()).await, Err(DbError::InvalidConfiguration(_))));

        let (Ok(host), Ok(ca)) = (std::env::var("PG_TLS_HOST"), std::env::var("PG_TLS_CA")) else {
            eprintln!("PG_TLS_HOST / PG_TLS_CA not set: verifying modes not checked against a TLS server");
            return;
        };
        p.hosts = vec![HostPort::new(host.clone(), 5432)];
        let ssl_in_use = "SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()";
        for (mode, ca_path) in [
            (SslMode::Prefer, ""),
            (SslMode::Require, ""),
            (SslMode::VerifyCa, ca.as_str()),
            (SslMode::VerifyFull, ca.as_str()),
        ] {
            p.ssl.mode = mode;
            p.ssl.ca_certificate_path = ca_path.into();
            let s = open(p.clone(), c.clone()).await;
            assert_eq!(scalar(&*s, ssl_in_use).await.as_deref(), Some("true"), "{mode:?}");
            s.close().await;
        }
        p.ssl.ca_certificate_path = String::new();
        let e = connect(p.clone(), c.clone(), &none()).await.err().expect("self-made CA isn't a platform root");
        assert!(e.to_string().contains("certificate"), "{e}");
        // By address, the name check fails for verifyFull but not for verifyCA.
        let ip = tokio::net::lookup_host((host.as_str(), 5432)).await.unwrap().next().unwrap().ip().to_string();
        p.hosts = vec![HostPort::new(ip, 5432)];
        p.ssl.ca_certificate_path = ca.clone();
        let e = connect(p.clone(), c.clone(), &none()).await.err().expect("IP isn't in the certificate");
        assert!(e.to_string().contains("certificate"), "{e}");
        p.ssl.mode = SslMode::VerifyCa;
        open(p, c).await.close().await;
    })
    .await;
}

#[tokio::test]
async fn read_only_profile_is_enforced_by_the_server() {
    require_it!();
    support::run(30, async {
        let (mut p, c) = pg();
        p.read_only = true;
        let s = open(p, c).await;
        let e = s.execute("shop", "CREATE TABLE rs_pg_ro (id int)", &none()).await.expect_err("refused");
        assert!(matches!(&e, DbError::ServerReadOnly(m) if m.starts_with("[25006]")), "{e}");
        let e = s.execute("shop", "UPDATE customers SET full_name = full_name", &none()).await.err().unwrap();
        assert!(matches!(e, DbError::ServerReadOnly(_)), "{e}");
        assert_eq!(scalar(&*s, "SELECT count(*) FROM customers").await.as_deref(), Some("2"));
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn statement_timeout_and_cancellation_stop_the_server_query() {
    require_it!();
    support::run(60, async {
        let (mut p, c) = pg();
        p.query_timeout_seconds = 1;
        let s = open(p, c.clone()).await;
        // Server-side statement_timeout.
        let t = Instant::now();
        let e = s.execute("shop", "SELECT pg_sleep(5)", &none()).await.expect_err("statement_timeout");
        assert!(matches!(&e, DbError::Query(m) if m.starts_with("[57014]")), "{e}");
        assert!(t.elapsed() < Duration::from_millis(2500), "{:?}", t.elapsed());
        assert_eq!(scalar(&*s, "SELECT 1").await.as_deref(), Some("1"), "the session recovers");

        // Client deadline (query + 2 s) with the server limit switched off: the query is cancelled on the server.
        exec(&*s, "SET statement_timeout = 0").await;
        let t = Instant::now();
        let e = s.execute("shop", "SELECT pg_sleep(30) /* rs_pg_deadline_probe */", &none()).await.err().unwrap();
        assert!(matches!(e, DbError::Timeout(_)), "{e}");
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        let gone = |marker: &str| {
            format!(
                "SELECT count(*)::text FROM pg_stat_activity WHERE query LIKE '%{marker}%' AND state = 'active' \
                 AND pid <> pg_backend_pid()"
            )
        };
        eventually(&gone("rs_pg_deadline_probe"), "0").await;
        assert_eq!(scalar(&*s, "SHOW statement_timeout").await.as_deref(), Some("1s"), "fresh, prepared connection");
        s.close().await;

        // The user's cancel.
        let (mut p, c) = pg();
        p.query_timeout_seconds = 30;
        let s = open(p, c).await;
        let cancel = CancellationToken::new();
        let k = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            k.cancel();
        });
        let t = Instant::now();
        let r = s.execute("shop", "SELECT pg_sleep(20) /* rs_pg_cancel_probe */", &cancel).await;
        assert_eq!(r.err(), Some(DbError::Cancelled));
        assert!(t.elapsed() < Duration::from_secs(2));
        eventually(&gone("rs_pg_cancel_probe"), "0").await;
        assert_eq!(scalar(&*s, "SELECT 2").await.as_deref(), Some("2"));
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn browse_pages_sorts_and_caps() {
    require_it!();
    support::run(60, async {
        let (p, c) = pg();
        let s = open(p.clone(), c.clone()).await;
        exec(&*s, "DROP TABLE IF EXISTS rs_pg_rows").await;
        exec(&*s, r#"CREATE TABLE rs_pg_rows (id int PRIMARY KEY, "we""ird" text)"#).await;
        let insert = "INSERT INTO rs_pg_rows SELECT g, 'r' || g FROM generate_series(1, 25) g";
        assert_eq!(s.execute("shop", insert, &none()).await.unwrap().affected_rows, Some(25));
        let t = table("rs_pg_rows");
        let ids = |r: &goku_engine::model::QueryResult| -> Vec<String> {
            r.rows.iter().map(|row| row[0].clone().unwrap()).collect()
        };

        let page = s.browse(&t, Paging::new(0, 10), None, &none()).await.unwrap();
        assert_eq!((page.rows.len(), page.truncated), (10, true));
        let cols: Vec<(&str, &str)> = page.columns.iter().map(|c| (c.name.as_str(), c.type_name.as_str())).collect();
        assert_eq!(cols, [("id", "int4"), ("we\"ird", "text")]);
        let last = s.browse(&t, Paging::new(20, 10), None, &none()).await.unwrap();
        assert_eq!((last.rows.len(), last.truncated), (5, false));

        let desc = Sort { column: "id".into(), descending: true };
        assert_eq!(ids(&s.browse(&t, Paging::new(0, 3), Some(&desc), &none()).await.unwrap()), ["25", "24", "23"]);
        let weird = Sort { column: "we\"ird".into(), descending: false };
        let r = s.browse(&t, Paging::new(0, 2), Some(&weird), &none()).await.unwrap();
        assert_eq!(r.rows.iter().map(|r| r[1].clone().unwrap()).collect::<Vec<_>>(), ["r1", "r10"]);
        let hostile = Sort { column: "id; DROP TABLE rs_pg_rows; --".into(), descending: false };
        let e = s.browse(&t, Paging::new(0, 2), Some(&hostile), &none()).await.err().unwrap();
        assert!(e.to_string().contains("42703"), "an unknown column, not SQL: {e}");

        // The profile's maxRows caps pages and statements; a capped statement doesn't break the session.
        let (mut small, c2) = pg();
        small.max_rows = 7;
        let s2 = open(small, c2).await;
        let page = s2.browse(&t, Paging::new(0, 10), None, &none()).await.unwrap();
        assert_eq!((page.rows.len(), page.truncated), (7, true));
        let r = s2.execute("shop", "SELECT g FROM generate_series(1, 1000000) g", &none()).await.unwrap();
        assert_eq!((r.rows.len(), r.truncated, r.rows[6][0].as_deref()), (7, true, Some("7")));
        assert_eq!(scalar(&*s2, "SELECT 'still fine'").await.as_deref(), Some("still fine"));
        s2.close().await;

        exec(&*s, "DROP TABLE rs_pg_rows").await;
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn cells_are_formatted_like_the_app() {
    require_it!();
    support::run(30, async {
        let (p, c) = pg();
        let s = open(p, c).await;
        let cases: &[(&str, Option<&str>)] = &[
            ("true", Some("true")),
            ("-5::int2", Some("-5")),
            ("9007199254740993::int8", Some("9007199254740993")),
            ("1.5::float4", Some("1.5")),
            ("1::float4", Some("1.0")),
            ("1::float8", Some("1")),
            ("0.1::float8", Some("0.1")),
            ("1e20::float8", Some("1e+20")),
            ("'-Infinity'::float8", Some("-Infinity")),
            ("12.340::numeric", Some("12.340")),
            ("-0.001::numeric", Some("-0.001")),
            ("100000::numeric", Some("100000")),
            ("123456789.000001::numeric", Some("123456789.000001")),
            ("'NaN'::numeric", Some("NaN")),
            ("'12.5'::money", Some("$12.50")),
            ("'ab'::char(3)", Some("ab ")),
            ("'x'::\"char\"", Some("x")),
            (r#"'{"a":1}'::jsonb"#, Some(r#"{"a": 1}"#)),
            (r#"'{"a":1}'::json"#, Some(r#"{"a":1}"#)),
            ("'00000000-0000-0000-0000-00000000000a'::uuid", Some("00000000-0000-0000-0000-00000000000a")),
            ("'2024-02-29'::date", Some("2024-02-29")),
            ("'0001-12-31 BC'::date", Some("0001-12-31 BC")),
            ("'infinity'::date", Some("infinity")),
            ("'13:45:01.5'::time", Some("13:45:01.5")),
            ("'12:00:00+05:30'::timetz", Some("12:00:00+05:30")),
            ("'2024-01-02 03:04:05.123456'::timestamp", Some("2024-01-02 03:04:05.123456")),
            ("'2024-01-02 03:04:05+02'::timestamptz", Some("2024-01-02 01:04:05+00")),
            ("'1 year 2 mons 3 days 04:05:06'::interval", Some("1 year 2 mons 3 days 04:05:06")),
            ("'-1 day'::interval", Some("-1 day")),
            ("'192.168.0.1'::inet", Some("192.168.0.1")),
            ("'192.168.0.1/24'::inet", Some("192.168.0.1/24")),
            ("'10.0.0.0/8'::cidr", Some("10.0.0.0/8")),
            ("'2001:db8::1'::inet", Some("2001:db8::1")),
            ("'08:00:2b:01:02:03'::macaddr", Some("08:00:2b:01:02:03")),
            ("B'101'::bit(3)", Some("101")),
            ("point(1.5, 2)", Some("(1.5,2)")),
            ("ARRAY[1,NULL,3]", Some("{1,NULL,3}")),
            (r#"ARRAY['a b', '"', '']"#, Some(r#"{"a b","\"",""}"#)),
            ("ARRAY[[1,2],[3,4]]", Some("{{1,2},{3,4}}")),
            ("'{}'::int[]", Some("{}")),
            ("'[1,5)'::int4range", Some("[1,5)")),
            ("'(,10]'::int8range", Some("(,11)")),
            ("'empty'::int4range", Some("empty")),
            (r"'\x0102ff'::bytea", Some(r"\x0102ff")),
            ("ROW(1, 'a b', NULL)", Some(r#"(1,"a b",)"#)),
            ("42::oid", Some("42")),
            ("NULL::text", None),
        ];
        let select = cases.iter().map(|(e, _)| *e).collect::<Vec<_>>().join(", ");
        let r = s.execute("shop", &format!("SELECT {select}"), &none()).await.unwrap();
        for (i, (expr, want)) in cases.iter().enumerate() {
            assert_eq!(r.rows[0][i].as_deref(), *want, "{expr}");
        }
        let r = s
            .execute("shop", "SELECT 1, ARRAY[1], ARRAY[now()], 'pg_class'::regclass, repeat('x', 40000)", &none())
            .await
            .unwrap();
        let types: Vec<&str> = r.columns.iter().map(|c| c.type_name.as_str()).collect();
        assert_eq!(types, ["int4", "int4[]", "timestamp with time zone[]", "regclass", "text"]);
        assert_eq!(r.rows[0][3].as_deref(), Some("1259"));
        assert!(is_clipped(r.rows[0][4].as_deref()), "long cells are clipped");
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn close_closes_every_connection() {
    require_it!();
    support::run(30, async {
        let (p, c) = pg();
        let s = open(p, c).await;
        s.databases(&none()).await.unwrap();
        let data_pid = scalar(&*s, "SELECT pg_backend_pid()::text").await.unwrap();
        s.introspect("shop", &["inventory".into()], &none()).await.unwrap();
        s.close().await;
        s.close().await; // idempotent
        assert_eq!(s.databases(&none()).await.err(), Some(DbError::NotConnected));
        assert_eq!(s.execute("shop", "SELECT 1", &none()).await.err(), Some(DbError::NotConnected));
        eventually(&format!("SELECT count(*)::text FROM pg_stat_activity WHERE pid = {data_pid}"), "0").await;
    })
    .await;
}

#[tokio::test]
async fn pool_keeps_at_most_eight_connections() {
    require_it!();
    support::run(120, async {
        let (p, c) = pg();
        let admin = open(p.clone(), c.clone()).await;
        let dbs: Vec<String> = (0..10).map(|i| format!("rs_pg_db_{i}")).collect();
        for db in &dbs {
            exec(&*admin, &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await;
            exec(&*admin, &format!("CREATE DATABASE {db}")).await;
        }
        let s = open(p, c).await;
        for db in &dbs {
            assert!(s.schemas(db, &none()).await.unwrap().contains(&"public".into()));
        }
        let count = "SELECT count(*)::text FROM pg_stat_activity WHERE datname LIKE 'rs_pg_db_%'";
        // shop's catalog connection + 10 databases: the 3 least recently used (shop, db_0, db_1) are closed.
        eventually(count, "8").await;
        s.close().await;
        eventually(count, "0").await;
        for db in &dbs {
            exec(&*admin, &format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)")).await;
        }
        admin.close().await;
    })
    .await;
}
