//! MySQL driver against `mysql-primary` (docker-compose.yml). Scratch tables are `rs_my_*` in `shop`, one per test so the
//! tests can run in parallel; each test drops its table before and after.
mod support;

use goku_engine::DbError;
use goku_engine::limits::Paging;
use goku_engine::model::{ObjectKind, ObjectRef, QueryResult, Sort};
use goku_engine::profile::{
    ConnectionProfile, Credentials, DatabaseKind, HostPort, SslMode, TargetSession, TopologyMode,
};
use goku_engine::session::{DatabaseSession, connect};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn mysql() -> (ConnectionProfile, Credentials) {
    support::profile(DatabaseKind::Mysql)
}

async fn open(p: ConnectionProfile, c: Credentials) -> Box<dyn DatabaseSession> {
    connect(p, c, &CancellationToken::new()).await.expect("connect")
}

async fn exec(s: &dyn DatabaseSession, sql: &str) -> QueryResult {
    s.execute("shop", sql, &CancellationToken::new()).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn cells(r: &QueryResult) -> Vec<Vec<Option<&str>>> {
    r.rows.iter().map(|row| row.iter().map(|c| c.as_deref()).collect()).collect()
}

fn table(name: &str) -> ObjectRef {
    ObjectRef { database: "shop".into(), schema: String::new(), name: name.into(), kind: ObjectKind::Table }
}

/// Number of server threads still running a statement that contains `marker`.
async fn running(s: &dyn DatabaseSession, marker: &str) -> usize {
    let sql = format!(
        "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE '%{marker}%' AND ID <> CONNECTION_ID()"
    );
    exec(s, &sql).await.rows[0][0].as_deref().unwrap().parse().unwrap()
}

async fn wait_until_gone(s: &dyn DatabaseSession, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while running(s, marker).await > 0 {
        assert!(Instant::now() < deadline, "{marker} still runs on the server");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn connects_and_lists_the_catalog() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        let (p, c) = mysql();
        let s = open(p, c).await;
        let cancel = CancellationToken::new();
        let info = s.server_info(&cancel).await.unwrap();
        assert_eq!(info.product, "MySQL");
        assert!(info.version.starts_with("8."), "{}", info.version);
        assert_eq!(info.current_user, "dbc@%");
        assert!(info.connected_host.ends_with(":3306") && !info.is_read_only_node);
        let dbs = s.databases(&cancel).await.unwrap();
        assert!(dbs.contains(&"shop".into()) && dbs.contains(&"analytics".into()));
        let at = |n: &str| dbs.iter().position(|d| d == n).unwrap();
        assert!(at("shop") < at("information_schema"), "system schemas last: {dbs:?}");
        assert!(s.schemas("shop", &cancel).await.unwrap().is_empty());
        let objects = s.objects("shop", "", &cancel).await.unwrap();
        let names: Vec<_> = objects.iter().map(|o| o.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        let totals = objects.iter().find(|o| o.name == "order_totals").unwrap();
        assert_eq!(totals.kind, ObjectKind::View);
        assert!(objects.iter().any(|o| o.name == "customers" && o.kind == ObjectKind::Table));
        let cols = s.columns(&table("customers"), &cancel).await.unwrap();
        let summary: Vec<_> = cols
            .iter()
            .map(|a| (a.name.as_str(), a.type_name.as_str(), a.nullable, a.is_primary_key, a.is_unique))
            .collect();
        assert_eq!(
            summary,
            [
                ("id", "bigint auto_increment", false, true, true),
                ("email", "varchar(255)", false, false, true),
                ("full_name", "varchar(200)", false, false, false),
                ("created_at", "datetime DEFAULT_GENERATED", false, false, false),
                ("profile", "json", true, false, false),
            ]
        );
        assert_eq!(cols[3].default_value.as_deref(), Some("CURRENT_TIMESTAMP"));
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn wrong_password_and_unreachable_hosts_fail_fast() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        let (p, mut c) = mysql();
        c.password = Some("not-the-password".into());
        match connect(p.clone(), c, &CancellationToken::new()).await {
            Err(DbError::ConnectionFailed(m)) => {
                assert!(m.contains("[1045/28000]") && m.starts_with("mysql-primary:3306: "), "{m}");
                assert!(!m.contains("not-the-password"));
            }
            other => panic!("expected an auth failure, got {:?}", other.map(|_| ())),
        }
        let (mut p, c) = mysql();
        p.hosts = vec![HostPort::new("10.255.255.1", 3306)];
        p.connect_timeout_seconds = 1;
        let started = Instant::now();
        let r = connect(p, c.clone(), &CancellationToken::new()).await;
        assert!(started.elapsed() < Duration::from_millis(2500), "took {:?}", started.elapsed());
        assert!(matches!(r, Err(DbError::ConnectionFailed(_))), "{:?}", r.map(|_| ()));

        // multiHost: the dead host is skipped; a target nothing matches lists every host.
        let (mut p, c) = mysql();
        p.mode = TopologyMode::MultiHost;
        p.connect_timeout_seconds = 1;
        p.hosts.insert(0, HostPort::new("10.255.255.1", 3306));
        let s = open(p.clone(), c.clone()).await;
        assert_eq!(s.server_info(&CancellationToken::new()).await.unwrap().connected_host, "mysql-primary:3306");
        s.close().await;
        p.target_session = TargetSession::Standby;
        match connect(p, c, &CancellationToken::new()).await {
            Err(DbError::NoSuitableHost(m)) => {
                assert!(
                    m.contains("10.255.255.1:3306: ") && m.contains("writable primary does not match Standby"),
                    "{m}"
                )
            }
            other => panic!("expected no suitable host, got {:?}", other.map(|_| ())),
        }
    })
    .await;
}

#[tokio::test]
async fn tls_modes() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        let cipher = "SHOW SESSION STATUS LIKE 'Ssl_cipher'";
        let (p, c) = mysql();
        assert_eq!(p.ssl.mode, SslMode::Require);
        let s = open(p.clone(), c.clone()).await;
        let r = exec(&*s, cipher).await;
        assert!(r.rows[0][1].as_deref().is_some_and(|v| !v.is_empty()), "{:?}", r.rows);
        s.close().await;

        let mut prefer = p.clone();
        prefer.ssl.mode = SslMode::Prefer;
        let s = open(prefer, c.clone()).await;
        assert!(exec(&*s, cipher).await.rows[0][1].as_deref().is_some_and(|v| !v.is_empty()));
        s.close().await;

        // The server's certificate is self-signed: full verification against the built-in roots must refuse it.
        let mut full = p.clone();
        full.ssl.mode = SslMode::VerifyFull;
        assert!(matches!(connect(full, c.clone(), &CancellationToken::new()).await, Err(DbError::ConnectionFailed(_))));

        let mut missing = p;
        missing.ssl.mode = SslMode::VerifyCa;
        missing.ssl.ca_certificate_path = "/nonexistent/ca.pem".into();
        assert!(matches!(
            connect(missing, c, &CancellationToken::new()).await,
            Err(DbError::InvalidConfiguration(m)) if m.contains("not readable")
        ));
    })
    .await;
}

#[tokio::test]
async fn read_only_profile_is_refused_by_the_server() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        let (p, c) = mysql();
        let rw = open(p.clone(), c.clone()).await;
        exec(&*rw, "DROP TABLE IF EXISTS rs_my_ro").await;
        exec(&*rw, "CREATE TABLE rs_my_ro (id INT PRIMARY KEY)").await;
        let mut ro = p;
        ro.read_only = true;
        let s = open(ro, c).await;
        let r = s.execute("shop", "INSERT INTO rs_my_ro VALUES (1)", &CancellationToken::new()).await;
        match r {
            Err(DbError::ServerReadOnly(m)) => assert!(m.starts_with("[1792/25006]"), "{m}"),
            other => panic!("expected a server read-only error, got {other:?}"),
        }
        assert_eq!(cells(&exec(&*s, "SELECT COUNT(*) FROM rs_my_ro").await), [[Some("0")]]);
        s.close().await;
        exec(&*rw, "DROP TABLE rs_my_ro").await;
        rw.close().await;
    })
    .await;
}

#[tokio::test]
async fn server_limit_client_deadline_and_cancel_kill_the_query() {
    if !support::enabled() {
        return;
    }
    support::run(60, async {
        let (mut p, c) = mysql();
        p.query_timeout_seconds = 1;
        let s = open(p.clone(), c.clone()).await;
        let watcher = open(mysql().0, c.clone()).await;
        assert_eq!(cells(&exec(&*s, "SELECT @@SESSION.max_execution_time").await), [[Some("1000")]]);

        // max_execution_time interrupts a SELECT on the server (SLEEP returns 1 when interrupted).
        let started = Instant::now();
        let r = s.execute("shop", "SELECT SLEEP(5) AS rs_my_limit", &CancellationToken::new()).await;
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
        match r {
            Ok(r) => assert_eq!(cells(&r), [[Some("1")]]),
            Err(DbError::Query(m)) => assert!(m.contains("3024"), "{m}"),
            Err(e) => panic!("{e}"),
        }

        // DO isn't a SELECT, so only the client deadline (query + 2 s) stops it, and KILL QUERY ends it on the server.
        let started = Instant::now();
        let r = s.execute("shop", "DO SLEEP(30) + 0 * LENGTH('rs_my_deadline')", &CancellationToken::new()).await;
        assert!(matches!(r, Err(DbError::Timeout(_))), "{r:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
        wait_until_gone(&*watcher, "rs_my_deadline").await;
        assert_eq!(cells(&exec(&*s, "SELECT 1").await), [[Some("1")]], "the session reconnects");
        s.close().await;

        // Cancel from the caller.
        p.query_timeout_seconds = 60;
        let s = open(p, c).await;
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            trigger.cancel();
        });
        let started = Instant::now();
        let r = s.execute("shop", "SELECT SLEEP(30) AS rs_my_cancel", &cancel).await;
        assert_eq!(r, Err(DbError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2), "took {:?}", started.elapsed());
        wait_until_gone(&*watcher, "rs_my_cancel").await;
        assert_eq!(cells(&exec(&*s, "SELECT 2").await), [[Some("2")]]);
        s.close().await;
        watcher.close().await;
    })
    .await;
}

#[tokio::test]
async fn row_cap_paging_and_sort() {
    if !support::enabled() {
        return;
    }
    support::run(60, async {
        let (mut p, c) = mysql();
        p.max_rows = 10;
        let s = open(p, c).await;
        let cancel = CancellationToken::new();
        exec(&*s, "DROP TABLE IF EXISTS rs_my_rows").await;
        exec(&*s, "CREATE TABLE rs_my_rows (id INT PRIMARY KEY, label VARCHAR(10))").await;
        let values: Vec<String> = (1..=25).map(|i| format!("({i}, 'r{i}')")).collect();
        let inserted = exec(&*s, &format!("INSERT INTO rs_my_rows VALUES {}", values.join(","))).await;
        assert_eq!((inserted.affected_rows, inserted.columns.len()), (Some(25), 0));

        let r = exec(&*s, "SELECT * FROM rs_my_rows ORDER BY id").await;
        assert_eq!((r.rows.len(), r.truncated, r.affected_rows), (10, true, None));
        assert_eq!(r.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "label"]);

        let t = table("rs_my_rows");
        let ids = |r: &QueryResult| r.rows.iter().map(|row| row[0].clone().unwrap()).collect::<Vec<_>>();
        let by_id = Sort { column: "id".into(), descending: false };
        let page = s.browse(&t, Paging::new(5, 5), Some(&by_id), &cancel).await.unwrap();
        assert_eq!((ids(&page), page.truncated), (["6", "7", "8", "9", "10"].map(String::from).to_vec(), true));
        let last = s.browse(&t, Paging::new(20, 10), Some(&by_id), &cancel).await.unwrap();
        assert_eq!((ids(&last), last.truncated), (["21", "22", "23", "24", "25"].map(String::from).to_vec(), false));
        let capped = s.browse(&t, Paging::new(0, 500), Some(&by_id), &cancel).await.unwrap();
        assert_eq!((capped.rows.len(), capped.truncated), (10, true), "max_rows caps a page");
        let desc = Sort { column: "id".into(), descending: true };
        assert_eq!(ids(&s.browse(&t, Paging::new(0, 2), Some(&desc), &cancel).await.unwrap()), ["25", "24"]);
        let evil = Sort { column: "id` DESC; DROP TABLE rs_my_rows; --".into(), descending: false };
        assert!(matches!(s.browse(&t, Paging::new(0, 2), Some(&evil), &cancel).await, Err(DbError::Query(m)) if m.contains("1054")));
        let empty = s.browse(&table("rs_my_rows"), Paging::new(100, 5), None, &cancel).await.unwrap();
        assert_eq!((empty.rows.len(), empty.columns.len(), empty.truncated), (0, 2, false));

        // Far past the cap: reading stops, the query is killed and the next statement gets a fresh connection.
        let r = exec(&*s, "SELECT a.id AS rs_my_cross FROM rs_my_rows a, rs_my_rows b, rs_my_rows c, rs_my_rows d").await;
        assert_eq!((r.rows.len(), r.truncated), (10, true));
        assert_eq!(cells(&exec(&*s, "SELECT COUNT(*) FROM rs_my_rows").await), [[Some("25")]]);

        exec(&*s, "DROP TABLE rs_my_rows").await;
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn cells_are_formatted_like_the_app() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        let (p, c) = mysql();
        let s = open(p, c).await;
        exec(&*s, "DROP TABLE IF EXISTS rs_my_types").await;
        exec(
            &*s,
            "CREATE TABLE rs_my_types (id INT PRIMARY KEY, tiny_u TINYINT UNSIGNED, big_u BIGINT UNSIGNED, bits BIT(10), \
             j JSON, t TIME(3), neg_t TIME, d DATE, dt DATETIME(6), dec_v DECIMAL(10,3), f FLOAT, dbl DOUBLE, \
             bin VARBINARY(4), txt VARCHAR(10), yr YEAR, en ENUM('a','b'), n INT NULL)",
        )
        .await;
        exec(
            &*s,
            "INSERT INTO rs_my_types VALUES (1, 255, 18446744073709551615, b'1111111111', '{\"a\":1,\"b\":[true,null]}', \
             '12:34:56.500', '-838:59:59', '2026-01-02', '2026-01-02 03:04:05.120000', 1.5, 3, 2.25, x'DEADBEEF', \
             'héllo', 2026, 'b', NULL)",
        )
        .await;
        let want = [
            ("id", "int", Some("1")),
            ("tiny_u", "tinyint unsigned", Some("255")),
            ("big_u", "bigint unsigned", Some("18446744073709551615")),
            ("bits", "bit", Some("1023")),
            ("j", "json", Some(r#"{"a": 1, "b": [true, null]}"#)),
            ("t", "time", Some("12:34:56.5")),
            ("neg_t", "time", Some("-838:59:59")),
            ("d", "date", Some("2026-01-02")),
            ("dt", "datetime", Some("2026-01-02 03:04:05.12")),
            ("dec_v", "decimal", Some("1.500")),
            ("f", "float", Some("3.0")),
            ("dbl", "double", Some("2.25")),
            ("bin", "varbinary", Some("0xdeadbeef")),
            ("txt", "varchar", Some("héllo")),
            ("yr", "year", Some("2026")),
            ("en", "char", Some("b")),
            ("n", "int", None),
        ];
        let page = s.browse(&table("rs_my_types"), Paging::default(), None, &CancellationToken::new()).await.unwrap();
        let got: Vec<_> = page
            .columns
            .iter()
            .zip(&page.rows[0])
            .map(|(c, v)| (c.name.as_str(), c.type_name.as_str(), v.as_deref()))
            .collect();
        assert_eq!(got, want);
        exec(&*s, "DROP TABLE rs_my_types").await;
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn close_ends_every_connection() {
    if !support::enabled() {
        return;
    }
    support::run(30, async {
        let (p, c) = mysql();
        let watcher = open(p.clone(), c.clone()).await;
        let s = open(p, c).await;
        let cancel = CancellationToken::new();
        s.server_info(&cancel).await.unwrap(); // meta
        let editor = exec(&*s, "SELECT CONNECTION_ID()").await.rows[0][0].clone().unwrap();
        let count = |id: String| {
            let w = &watcher;
            async move {
                let sql = format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id}");
                exec(&**w, &sql).await.rows[0][0].clone().unwrap()
            }
        };
        assert_eq!(count(editor.clone()).await, "1");
        s.close().await;
        s.close().await;
        let deadline = Instant::now() + Duration::from_secs(3);
        while count(editor.clone()).await != "0" {
            assert!(Instant::now() < deadline, "editor connection still open");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(matches!(s.server_info(&cancel).await, Err(DbError::NotConnected | DbError::Cancelled)));
        watcher.close().await;
    })
    .await;
}
