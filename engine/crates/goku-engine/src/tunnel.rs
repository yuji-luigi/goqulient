//! SSH local port forwarding (port of GokuCore's `SSHTunnel.swift`, like `ssh -L`) on russh with the ring backend.
//! Each database host gets its own listener on 127.0.0.1 (never another interface); every socket accepted there is
//! piped through a `direct-tcpip` channel to the host as the SSH server resolves it.
//! - Host keys are pinned (`ssh.trustedHostKey`, OpenSSH format). An empty pin fails with `DbError::UnknownHostKey`
//!   (key + SHA-256 fingerprint) before authenticating, so a client can ask the user and retry with the key; GokuCore
//!   trusts the first key silently instead. A different key fails hard.
//! - Connect, key exchange and authentication run under one `connect_timeout_seconds` deadline, each channel open under
//!   another. russh runs the connection in a task of its own, so the socket is wrapped (`Killable`): a timeout, a
//!   cancel, `close` or dropping the `Tunnel` cuts it, which ends that task, the listeners and the pipes.
//! - Secrets (`sshPassword`, `sshKeyPassphrase`, the key itself) only reach russh; errors name the user, host and key
//!   path, never a secret.
#![warn(clippy::indexing_slicing)]
use crate::deadline::with_deadline;
use crate::drivers::lock;
use crate::error::{DbError, Result};
use crate::profile::{Credentials, HostPort, SshAuthKind, SshSettings};
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// GokuCore's default when `privateKeyPath` is empty.
const DEFAULT_KEY: &str = "~/.ssh/id_ed25519";
/// A keepalive every 15 s, and the connection ends after 3 unanswered ones: a bastion that vanished is noticed in
/// under a minute instead of when the next query times out.
const KEEPALIVE: Duration = Duration::from_secs(15);
/// How long `close` waits to hand the polite SSH disconnect to the connection task before cutting the socket.
const DISCONNECT_WAIT: Duration = Duration::from_secs(1);

const RSA_SHA1_HINT: &str = " Note: RSA keys are signed with ssh-rsa (SHA-1), which OpenSSH 8.8+ servers reject by \
                             default. Use an ed25519 key instead.";

/// An open SSH connection that forwards local ports. Closing (or dropping) it closes its listeners and connections.
pub struct Tunnel {
    handle: Arc<Handle<HostKeyCheck>>,
    /// Cuts the socket, stops the listeners and the pipes.
    kill: CancellationToken,
    host_key: String,
    timeout_seconds: f64,
    /// Why the last channel open failed (the database driver only sees a closed socket).
    forward_error: Arc<Mutex<Option<String>>>,
}

impl Tunnel {
    /// Connects to the SSH server, checks its host key against `settings.trusted_host_key` and authenticates, all within
    /// `timeout_seconds`. Errors: `UnknownHostKey` (nothing pinned), `ConnectionFailed` (unreachable, key mismatch),
    /// `SshAuthentication` (rejected), `SshKeyLocked` (encrypted key without the right passphrase),
    /// `InvalidConfiguration` (unreadable key), `Timeout`, `Cancelled`.
    pub async fn open(
        settings: &SshSettings,
        credentials: &Credentials,
        timeout_seconds: i64,
        cancel: &CancellationToken,
    ) -> Result<Tunnel> {
        let port = u16::try_from(settings.port)
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| DbError::InvalidConfiguration("The SSH port must be between 1 and 65535.".into()))?;
        let key = match settings.auth {
            SshAuthKind::Password => None,
            SshAuthKind::PrivateKey => Some(load_key(settings, credentials).await?),
        };
        let kill = CancellationToken::new();
        let armed = kill.clone().drop_guard(); // any early return below cuts the socket
        let seconds = timeout_seconds.max(1) as f64;
        let cut = kill.clone();
        let (handle, host_key) = with_deadline(
            seconds,
            &format!("SSH connect to {}", settings.host),
            cancel,
            move || cut.cancel(),
            connect(settings, port, credentials, key, &kill),
        )
        .await?;
        Ok(Tunnel {
            handle: Arc::new(handle),
            kill: armed.disarm(),
            host_key,
            timeout_seconds: seconds,
            forward_error: Arc::default(),
        })
    }

    /// The server's host key in OpenSSH format (the one `trustedHostKey` pinned).
    pub fn host_key(&self) -> &str {
        &self.host_key
    }

    /// Listens on 127.0.0.1 (a free port) and forwards every connection to `target`, as the SSH server resolves it.
    /// Returns the local address to dial. Each channel open is bounded by the connect timeout; a failed one closes the
    /// local socket and is kept for `forward_error`.
    pub async fn forward(&self, target: &HostPort) -> Result<HostPort> {
        let local = |e: io::Error| DbError::ConnectionFailed(format!("Could not open local tunnel port: {e}"));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(local)?;
        let port = listener.local_addr().map_err(local)?.port();
        let to = Target {
            handle: self.handle.clone(),
            kill: self.kill.clone(),
            target: target.clone(),
            seconds: self.timeout_seconds,
            error: self.forward_error.clone(),
        };
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    biased;
                    _ = to.kill.cancelled() => break,
                    a = listener.accept() => a,
                };
                match accepted {
                    Ok((socket, peer)) => {
                        tokio::spawn(to.clone().pipe(socket, peer));
                    }
                    // Out of file descriptors and the like: wait a little rather than spin.
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        });
        Ok(HostPort::new(Ipv4Addr::LOCALHOST.to_string(), port.into()))
    }

    /// Why the last forwarded connection couldn't reach its target, if one couldn't.
    pub fn forward_error(&self) -> Option<String> {
        lock(&self.forward_error).clone()
    }

    /// Says goodbye to the SSH server (bounded) and cuts the connection; listeners and pipes stop. Idempotent.
    pub async fn close(&self) {
        if self.kill.is_cancelled() {
            return;
        }
        // Best effort: the socket is cut right after either way.
        let _ =
            tokio::time::timeout(DISCONNECT_WAIT, self.handle.disconnect(russh::Disconnect::ByApplication, "", "en"))
                .await;
        self.kill.cancel();
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.kill.cancel();
    }
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Tunnel").field("host_key", &self.host_key).field("closed", &self.kill.is_cancelled()).finish()
    }
}

/// Everything one forwarded connection needs.
#[derive(Clone)]
struct Target {
    handle: Arc<Handle<HostKeyCheck>>,
    kill: CancellationToken,
    target: HostPort,
    seconds: f64,
    error: Arc<Mutex<Option<String>>>,
}

impl Target {
    /// Opens the `direct-tcpip` channel and copies both ways until either side closes or the tunnel does. A channel open
    /// that times out only drops the local socket: a late confirmation leaves a channel that lives until the tunnel
    /// closes.
    async fn pipe(self, mut socket: TcpStream, peer: SocketAddr) {
        let what = format!("SSH tunnel to {}", self.target);
        let opened = with_deadline(self.seconds, &what, &self.kill, || {}, async {
            self.handle
                .channel_open_direct_tcpip(
                    self.target.host.clone(),
                    u32::try_from(self.target.port).unwrap_or(0),
                    peer.ip().to_string(),
                    u32::from(peer.port()),
                )
                .await
                .map_err(|e| DbError::ConnectionFailed(format!("{what}: {e}")))
        })
        .await;
        let channel = match opened {
            Ok(c) => c,
            Err(DbError::Cancelled) => return,
            Err(e) => {
                *lock(&self.error) = Some(e.to_string());
                return;
            }
        };
        let mut remote = channel.into_stream();
        tokio::select! {
            _ = self.kill.cancelled() => {}
            _ = tokio::io::copy_bidirectional(&mut socket, &mut remote) => {}
        }
    }
}

/// TCP connect, SSH handshake with the host key check, then authentication.
async fn connect(
    settings: &SshSettings,
    port: u16,
    credentials: &Credentials,
    key: Option<PrivateKey>,
    kill: &CancellationToken,
) -> Result<(Handle<HostKeyCheck>, String)> {
    let at = format!("{}:{port}", settings.host);
    let tcp = TcpStream::connect((settings.host.as_str(), port))
        .await
        .map_err(|e| DbError::ConnectionFailed(format!("SSH connection to {at}: {e}")))?;
    let _ = tcp.set_nodelay(true); // latency only
    let seen: Arc<Mutex<Option<PublicKey>>> = Arc::default();
    let check = HostKeyCheck { expected: settings.trusted_host_key.trim().to_owned(), seen: seen.clone() };
    let config = Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE),
        keepalive_max: 3,
        nodelay: true,
        ..client::Config::default()
    });
    let mut handle = match client::connect_stream(config, Killable::new(tcp, kill), check).await {
        Ok(h) => h,
        Err(russh::Error::UnknownKey) => return Err(host_key_error(settings, lock(&seen).take())),
        Err(e) => return Err(DbError::ConnectionFailed(format!("SSH connection to {at}: {e}"))),
    };
    let host_key = lock(&seen).as_ref().map(openssh).unwrap_or_default();
    let rejected = |legacy_rsa: bool| {
        let mut m = format!(
            "SSH authentication failed for {}@{}: wrong password, or the key is not in authorized_keys.",
            settings.user, settings.host
        );
        if legacy_rsa {
            m += RSA_SHA1_HINT;
        }
        DbError::SshAuthentication(m)
    };
    let failed = |e: russh::Error| DbError::ConnectionFailed(format!("SSH authentication with {at}: {e}"));
    let (result, legacy_rsa) = match key {
        None => {
            let password = credentials.ssh_password.clone().unwrap_or_default();
            (handle.authenticate_password(settings.user.clone(), password).await.map_err(failed)?, false)
        }
        Some(key) => {
            // RSA: the best signature the server offers (rsa-sha2-512/256); without its list, legacy ssh-rsa (SHA-1).
            let rsa = key.algorithm().is_rsa();
            let hash = if rsa { handle.best_supported_rsa_hash().await.map_err(failed)?.flatten() } else { None };
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            (handle.authenticate_publickey(settings.user.clone(), key).await.map_err(failed)?, rsa && hash.is_none())
        }
    };
    if !result.success() {
        return Err(rejected(legacy_rsa));
    }
    Ok((handle, host_key))
}

/// Reads and decrypts the private key off the runtime (bcrypt-pbkdf takes a while on purpose).
async fn load_key(settings: &SshSettings, credentials: &Credentials) -> Result<PrivateKey> {
    let path =
        expand(if settings.private_key_path.trim().is_empty() { DEFAULT_KEY } else { &settings.private_key_path });
    let passphrase = credentials.ssh_key_passphrase.clone().filter(|p| !p.is_empty());
    tokio::task::spawn_blocking(move || {
        let text = std::fs::read_to_string(&path)
            .map_err(|_| DbError::InvalidConfiguration(format!("Cannot read SSH private key at {path}")))?;
        russh::keys::decode_secret_key(&text, passphrase.as_deref()).map_err(|e| match (e, &passphrase) {
            (russh::keys::Error::KeyIsEncrypted, None) => {
                DbError::SshKeyLocked(format!("The SSH private key at {path} is encrypted: enter its passphrase."))
            }
            (e, Some(_)) => DbError::SshKeyLocked(format!("Could not load SSH key (wrong passphrase?): {e}")),
            (e, None) => DbError::InvalidConfiguration(format!("Could not load SSH key at {path}: {e}")),
        })
    })
    .await
    .map_err(|e| DbError::InvalidConfiguration(format!("Could not load SSH key: {e}")))?
}

fn host_key_error(settings: &SshSettings, seen: Option<PublicKey>) -> DbError {
    let Some(key) = seen else {
        return DbError::ConnectionFailed("The SSH server presented no usable host key.".into());
    };
    if settings.trusted_host_key.trim().is_empty() {
        DbError::UnknownHostKey {
            host: settings.host.clone(),
            port: settings.port,
            key_type: key.algorithm().as_str().to_owned(),
            host_key: openssh(&key),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        }
    } else {
        // GokuCore's wording.
        DbError::ConnectionFailed(
            "SSH host key does not match the trusted key. Possible man-in-the-middle; update the trusted key only if \
             the server was reinstalled."
                .into(),
        )
    }
}

/// "type base64" without a comment.
fn openssh(key: &PublicKey) -> String {
    normalise(&key.to_openssh().unwrap_or_default())
}

/// Compares "type base64" and ignores trailing comments (`HostKeyRecorder.normalise`).
fn normalise(key: &str) -> String {
    key.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

/// Accepts the server's key only when it equals the pinned one, and remembers it for the error.
struct HostKeyCheck {
    expected: String,
    seen: Arc<Mutex<Option<PublicKey>>>,
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, presented: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        // A host certificate is pinned by its key, like a plain host key.
        let key = match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(c) => PublicKey::new(c.public_key().clone(), ""),
        };
        let trusted = !self.expected.is_empty() && normalise(&self.expected) == openssh(&key);
        *lock(&self.seen) = Some(key);
        Ok(trusted)
    }
}

/// The bastion socket, cut once `kill` fires (every read or write fails from then on, which ends russh's task).
struct Killable {
    inner: TcpStream,
    kill: Pin<Box<WaitForCancellationFutureOwned>>,
    killed: bool,
}

impl Killable {
    fn new(inner: TcpStream, kill: &CancellationToken) -> Self {
        Self { inner, kill: Box::pin(kill.clone().cancelled_owned()), killed: false }
    }

    /// Registers the waker on the kill switch, so a read that waits forever still wakes up when it fires.
    fn cut(&mut self, cx: &mut Context<'_>) -> bool {
        if !self.killed && self.kill.as_mut().poll(cx).is_ready() {
            self.killed = true;
        }
        self.killed
    }
}

fn aborted<T>() -> Poll<io::Result<T>> {
    Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionAborted, "SSH tunnel closed")))
}

impl AsyncRead for Killable {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.cut(cx) {
            return aborted();
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Killable {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.cut(cx) {
            return aborted();
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.cut(cx) {
            return aborted();
        }
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

fn expand(path: &str) -> String {
    let p = path.trim();
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
                Ok(home) => format!("{home}{rest}"),
                Err(_) => p.to_owned(),
            }
        }
        _ => p.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(port: u16) -> SshSettings {
        SshSettings {
            enabled: true,
            host: "127.0.0.1".into(),
            port: port.into(),
            user: "u".into(),
            ..SshSettings::default()
        }
    }

    #[test]
    fn host_keys_compare_without_comments() {
        assert_eq!(normalise("ssh-ed25519  AAAAC3Nz me@host\n"), "ssh-ed25519 AAAAC3Nz");
        assert_eq!(normalise(" ssh-rsa AAAA "), "ssh-rsa AAAA");
        assert_eq!(normalise(""), "");
    }

    /// A server that accepts the TCP connection and then says nothing (no SSH banner): the deadline must fire and
    /// the socket must be closed on our side.
    #[tokio::test]
    async fn a_silent_server_times_out_and_the_socket_closes() {
        use tokio::io::AsyncReadExt;
        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = server.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move { server.accept().await.unwrap().0 });
        let r = Tunnel::open(&settings(port), &Credentials::default(), 1, &CancellationToken::new()).await;
        assert!(matches!(r, Err(DbError::Timeout(ref m)) if m.contains("SSH connect to 127.0.0.1")), "{r:?}");
        let mut socket = accepted.await.unwrap();
        // The client wrote its banner; after the cut the server reads EOF instead of waiting forever.
        let mut rest = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut rest)).await;
        assert!(read.is_ok(), "the client socket stayed open after the timeout");
    }

    #[tokio::test]
    async fn cancel_stops_the_handshake() {
        let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = server.local_addr().unwrap().port();
        let _accepted = tokio::spawn(async move { server.accept().await.unwrap().0 });
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let r = Tunnel::open(&settings(port), &Credentials::default(), 30, &cancel).await;
        assert_eq!(r.err(), Some(DbError::Cancelled));
    }

    #[tokio::test]
    async fn unreadable_and_locked_keys() {
        let mut s = settings(22);
        s.auth = SshAuthKind::PrivateKey;
        s.private_key_path = "/nonexistent/id_ed25519".into();
        let r = Tunnel::open(&s, &Credentials::default(), 1, &CancellationToken::new()).await;
        assert_eq!(
            r.err(),
            Some(DbError::InvalidConfiguration("Cannot read SSH private key at /nonexistent/id_ed25519".into()))
        );
        // An encrypted key (passphrase "keypass"), made here so no private key is committed.
        let dir = std::env::temp_dir().join(format!("goku-tunnel-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id_ed25519_enc");
        std::fs::write(&path, encrypted_key("keypass")).unwrap();
        s.private_key_path = path.display().to_string();
        let locked = Tunnel::open(&s, &Credentials::default(), 1, &CancellationToken::new()).await;
        assert!(matches!(locked, Err(DbError::SshKeyLocked(ref m)) if m.contains("is encrypted")), "{locked:?}");
        let right = Credentials { ssh_key_passphrase: Some("keypass".into()), ..Credentials::default() };
        assert!(load_key(&s, &right).await.is_ok(), "the right passphrase opens it");
        let wrong = Credentials { ssh_key_passphrase: Some("nope".into()), ..Credentials::default() };
        let r = Tunnel::open(&s, &wrong, 1, &CancellationToken::new()).await;
        assert!(matches!(r, Err(DbError::SshKeyLocked(ref m)) if m.contains("wrong passphrase")), "{r:?}");
        assert!(!format!("{r:?}").contains("nope"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_host_key_error_shape() {
        let key = random_key().public_key().clone();
        let e = host_key_error(&settings(2222), Some(key.clone()));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "unknownHostKey");
        assert_eq!(v["detail"]["host"], "127.0.0.1");
        assert_eq!(v["detail"]["port"], 2222);
        assert_eq!(v["detail"]["keyType"], "ssh-ed25519");
        assert_eq!(v["detail"]["hostKey"], openssh(&key));
        assert!(v["detail"]["fingerprint"].as_str().unwrap().starts_with("SHA256:"));
        assert!(e.to_string().contains("SHA256:"));
        let mut pinned = settings(2222);
        pinned.trusted_host_key = "ssh-ed25519 AAAAother".into();
        assert!(
            matches!(host_key_error(&pinned, Some(key)), DbError::ConnectionFailed(m) if m.contains("man-in-the-middle"))
        );
    }

    /// A fresh ed25519 key: test keys are generated, never committed.
    fn random_key() -> PrivateKey {
        PrivateKey::random(&mut russh::keys::key::safe_rng(), russh::keys::Algorithm::Ed25519).unwrap()
    }

    /// A fresh ed25519 key encrypted with `passphrase`, in OpenSSH format (as `ssh-keygen -N` writes it).
    fn encrypted_key(passphrase: &str) -> String {
        let key = random_key().encrypt(&mut russh::keys::key::safe_rng(), passphrase).unwrap();
        key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().to_string()
    }
}
