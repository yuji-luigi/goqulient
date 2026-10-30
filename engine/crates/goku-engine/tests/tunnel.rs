//! SSH tunnels through the Docker `ssh-bastion` (docker/ssh, GOKU_IT=1): password and key auth, host-key pinning, the
//! deadline, and each driver behind the tunnel. The bastion's test keys and host keys are in `SSH_KEYS` (the `ssh-keys`
//! volume). "Through the tunnel" is checked on the server: the client address it sees is the bastion's.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "a failing test panics")]
mod support;

use goku_engine::DbError;
use goku_engine::profile::{ConnectionProfile, Credentials, DatabaseKind, HostPort, SshAuthKind, TopologyMode};
use goku_engine::session::{DatabaseSession, connect};
use goku_engine::tunnel::Tunnel;
use std::net::IpAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::OnceCell;
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

fn bastion() -> String {
    std::env::var("SSH_HOST").unwrap_or_else(|_| "ssh-bastion".into())
}

fn keys() -> String {
    std::env::var("SSH_KEYS").unwrap_or_else(|_| "/ssh-keys".into())
}

/// "type base64" of each host key the bastion has.
fn host_keys() -> Vec<String> {
    ["ed25519", "ecdsa", "rsa"]
        .iter()
        .map(|t| std::fs::read_to_string(format!("{}/host_{t}.pub", keys())).unwrap())
        .map(|k| k.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .collect()
}

async fn bastion_ip() -> IpAddr {
    tokio::net::lookup_host((bastion(), 22)).await.unwrap().next().unwrap().ip()
}

/// The host key the bastion presents to russh, learnt once through `UnknownHostKey` (what a client does).
async fn trusted() -> &'static str {
    static KEY: OnceCell<String> = OnceCell::const_new();
    KEY.get_or_init(|| async {
        let (p, c) = ssh(DatabaseKind::Postgres, SshAuthKind::Password);
        let mut p = p;
        p.ssh.trusted_host_key.clear();
        match connect(p, c, &none()).await.err() {
            Some(DbError::UnknownHostKey { host_key, .. }) => host_key,
            other => panic!("expected UnknownHostKey, got {other:?}"),
        }
    })
    .await
}

/// The integration profile for `kind`, through the bastion with `auth` (password `sshpass` or the plain ed25519 key),
/// the host key not pinned yet.
fn ssh(kind: DatabaseKind, auth: SshAuthKind) -> (ConnectionProfile, Credentials) {
    let (mut p, mut c) = support::profile(kind);
    p.ssh.enabled = true;
    p.ssh.host = bastion();
    p.ssh.port = 22;
    p.ssh.user = "dbc".into();
    p.ssh.auth = auth;
    match auth {
        SshAuthKind::Password => c.ssh_password = Some("sshpass".into()),
        SshAuthKind::PrivateKey => p.ssh.private_key_path = format!("{}/id_ed25519", keys()),
    }
    (p, c)
}

async fn pinned(kind: DatabaseKind, auth: SshAuthKind) -> (ConnectionProfile, Credentials) {
    let (mut p, c) = ssh(kind, auth);
    p.ssh.trusted_host_key = trusted().await.to_owned();
    (p, c)
}

async fn open(p: ConnectionProfile, c: Credentials) -> Box<dyn DatabaseSession> {
    connect(p, c, &none()).await.unwrap_or_else(|e| panic!("connect: {e}"))
}

async fn cell(s: &dyn DatabaseSession, database: &str, statement: &str) -> String {
    let r = s.execute(database, statement, &none()).await.unwrap_or_else(|e| panic!("{statement}: {e}"));
    r.rows[0][0].clone().unwrap_or_default()
}

#[tokio::test]
async fn an_unpinned_host_key_is_reported_with_its_fingerprint() {
    require_it!();
    support::run(30, async {
        let (p, c) = ssh(DatabaseKind::Postgres, SshAuthKind::Password);
        let Some(DbError::UnknownHostKey { host, port, key_type, host_key, fingerprint }) =
            connect(p, c, &none()).await.err()
        else {
            panic!("expected UnknownHostKey");
        };
        assert_eq!((host.as_str(), port), (bastion().as_str(), 22));
        assert!(host_keys().contains(&host_key), "{host_key} is not one of the bastion's host keys");
        assert!(host_key.starts_with(&key_type));
        assert!(fingerprint.starts_with("SHA256:") && fingerprint.len() > 40, "{fingerprint}");
    })
    .await;
}

#[tokio::test]
async fn a_different_pinned_key_fails_hard() {
    require_it!();
    support::run(30, async {
        let (mut p, c) = ssh(DatabaseKind::Postgres, SshAuthKind::Password);
        let presented = trusted().await;
        p.ssh.trusted_host_key = host_keys().into_iter().find(|k| k != presented).unwrap();
        let e = connect(p, c, &none()).await.err().unwrap();
        assert!(matches!(&e, DbError::ConnectionFailed(m) if m.contains("does not match the trusted key")), "{e:?}");
    })
    .await;
}

#[tokio::test]
async fn postgres_through_the_tunnel_with_a_password() {
    require_it!();
    support::run(30, async {
        let (p, c) = pinned(DatabaseKind::Postgres, SshAuthKind::Password).await;
        let s = open(p, c).await;
        let info = s.server_info(&none()).await.unwrap();
        assert_eq!(info.connected_host, "pg-primary:5432", "the profile's host, not the tunnel's local end");
        let client: IpAddr = cell(&*s, "shop", "SELECT host(inet_client_addr())").await.parse().unwrap();
        assert_eq!(client, bastion_ip().await, "PostgreSQL saw a client other than the bastion");
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn postgres_with_an_encrypted_ed25519_key_and_multiple_hosts() {
    require_it!();
    support::run(30, async {
        let (mut p, mut c) = pinned(DatabaseKind::Postgres, SshAuthKind::PrivateKey).await;
        p.ssh.private_key_path = format!("{}/id_ed25519_enc", keys());
        c.ssh_key_passphrase = Some("keypass".into());
        // The first host doesn't exist behind the bastion: its channel fails and the next host is tried.
        p.mode = TopologyMode::MultiHost;
        p.hosts.insert(0, HostPort::new("nowhere.invalid", 5432));
        let s = open(p, c).await;
        assert_eq!(s.server_info(&none()).await.unwrap().connected_host, "pg-primary:5432");
        assert_eq!(cell(&*s, "shop", "SELECT 1::text").await, "1");
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn an_unreachable_target_says_why() {
    require_it!();
    support::run(30, async {
        let (mut p, c) = pinned(DatabaseKind::Postgres, SshAuthKind::Password).await;
        p.hosts = vec![HostPort::new("nowhere.invalid", 5432)];
        let e = connect(p, c, &none()).await.err().unwrap();
        assert!(e.to_string().contains("SSH tunnel to nowhere.invalid:5432"), "{e}");
    })
    .await;
}

#[tokio::test]
async fn mysql_through_the_tunnel_with_a_key() {
    require_it!();
    support::run(30, async {
        let (p, c) = pinned(DatabaseKind::Mysql, SshAuthKind::PrivateKey).await;
        let s = open(p, c).await;
        assert_eq!(s.server_info(&none()).await.unwrap().connected_host, "mysql-primary:3306");
        // TLS (ssl-mode require) runs inside the tunnel.
        let cipher = cell(
            &*s,
            "shop",
            "SELECT VARIABLE_VALUE FROM performance_schema.session_status WHERE VARIABLE_NAME = 'Ssl_cipher'",
        )
        .await;
        assert!(!cipher.is_empty(), "no TLS through the tunnel");
        let client: IpAddr = cell(&*s, "shop", "SELECT SUBSTRING_INDEX(USER(), '@', -1)").await.parse().unwrap();
        assert_eq!(client, bastion_ip().await, "MySQL saw a client other than the bastion");
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn mongo_through_the_tunnel_with_an_encrypted_rsa_key() {
    require_it!();
    support::run(40, async {
        let (mut p, mut c) = pinned(DatabaseKind::Mongodb, SshAuthKind::PrivateKey).await;
        p.ssh.private_key_path = format!("{}/id_rsa", keys());
        c.ssh_key_passphrase = Some("keypass".into());
        // A multi-host profile behind a tunnel is a direct connection to the first host that answers.
        p.mode = TopologyMode::MultiHost;
        p.hosts.insert(0, HostPort::new("nowhere.invalid", 27017));
        let s = open(p, c).await;
        assert_eq!(s.server_info(&none()).await.unwrap().connected_host, "mongo:27017");
        let r = s.execute("admin", r#"{"whatsmyuri": 1}"#, &none()).await.unwrap();
        let reply = r.documents.unwrap().remove(0); // shell text: { you: "<ip>:<port>", ok: 1.0 }
        assert!(reply.contains(&format!("\"{}:", bastion_ip().await)), "MongoDB saw another client: {reply}");
        s.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_wrong_ssh_password_is_an_ssh_authentication_error() {
    require_it!();
    support::run(30, async {
        let (p, mut c) = pinned(DatabaseKind::Postgres, SshAuthKind::Password).await;
        c.ssh_password = Some("not-the-password".into());
        let e = connect(p, c, &none()).await.err().unwrap();
        assert!(matches!(&e, DbError::SshAuthentication(m) if m.contains("dbc@")), "{e:?}");
        assert!(!e.display_message().contains("not-the-password"));
    })
    .await;
}

#[tokio::test]
async fn an_encrypted_key_needs_the_right_passphrase() {
    require_it!();
    support::run(30, async {
        let (mut p, mut c) = pinned(DatabaseKind::Postgres, SshAuthKind::PrivateKey).await;
        p.ssh.private_key_path = format!("{}/id_rsa", keys());
        let e = connect(p.clone(), c.clone(), &none()).await.err().unwrap();
        assert!(matches!(&e, DbError::SshKeyLocked(m) if m.contains("encrypted")), "{e:?}");
        c.ssh_key_passphrase = Some("wrong".into());
        let e = connect(p, c, &none()).await.err().unwrap();
        assert!(matches!(&e, DbError::SshKeyLocked(m) if m.contains("wrong passphrase")), "{e:?}");
    })
    .await;
}

/// A "bastion" that takes the TCP connection and never sends its SSH banner: the whole connect gives up at the connect
/// timeout (unit tests in tunnel.rs check that the socket is closed too).
#[tokio::test]
async fn a_bastion_that_does_not_answer_times_out() {
    require_it!();
    support::run(30, async {
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = silent.local_addr().unwrap().port();
        let _held = tokio::spawn(async move { silent.accept().await.map(|(socket, _)| socket) });
        let (mut p, c) = pinned(DatabaseKind::Postgres, SshAuthKind::Password).await;
        p.ssh.host = "127.0.0.1".into();
        p.ssh.port = port.into();
        p.connect_timeout_seconds = 2;
        let started = std::time::Instant::now();
        let e = connect(p, c, &none()).await.err().unwrap();
        assert!(matches!(&e, DbError::Timeout(m) if m.contains("SSH connect")), "{e:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    })
    .await;
}

#[tokio::test]
async fn closing_the_tunnel_closes_its_listener_and_connections() {
    require_it!();
    support::run(30, async {
        let (p, c) = pinned(DatabaseKind::Postgres, SshAuthKind::Password).await;
        let tunnel = Tunnel::open(&p.ssh, &c, 5, &none()).await.unwrap();
        assert_eq!(tunnel.host_key(), trusted().await);
        let local = tunnel.forward(&p.hosts[0]).await.unwrap();
        assert_eq!(local.host, "127.0.0.1");
        let addr = (local.host.as_str(), local.port as u16);
        // A PostgreSQL SSLRequest gets its one-byte answer through the tunnel.
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(&[0, 0, 0, 8, 4, 210, 22, 47]).await.unwrap();
        let mut answer = [0u8; 1];
        stream.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[0], b'N', "the Docker server has no TLS");
        tunnel.close().await;
        let mut rest = Vec::new();
        let eof = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
        assert!(eof.is_ok(), "the piped connection stayed open");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(tokio::net::TcpStream::connect(addr).await.is_err(), "the listener still accepts");
    })
    .await;
}
