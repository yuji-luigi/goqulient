use serde::Serialize;

/// Mirrors GokuCore's `DBError`; `Display` is its `errorDescription`, so both products word errors the same way.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "camelCase")]
pub enum DbError {
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("No suitable host: {0}")]
    NoSuitableHost(String),
    #[error("Timed out: {0}")]
    Timeout(String),
    #[error("Cancelled")]
    Cancelled,
    #[error("This connection is read-only. Disable read-only mode in the connection settings to modify data.")]
    ReadOnly,
    #[error("{0} is read-only in this connection's settings (Advanced → Read-only databases and tables).")]
    ReadOnlyObject(String),
    /// The server refused a write because the transaction, session or node is read-only (PostgreSQL 25006, MySQL 1792).
    #[error("{0}")]
    ServerReadOnly(String),
    #[error("Not connected")]
    NotConnected,
    #[error("Not supported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Query(String),
    #[error("Parse error: {0}")]
    Parse(String),
    /// The SSH server rejected the password or the key (worded as GokuCore's `connectionFailed`, so the text is the
    /// app's). Its own kind so a client can ask for the SSH password again rather than the database's.
    #[error("Connection failed: {0}")]
    SshAuthentication(String),
    /// The SSH private key is encrypted and no passphrase was given, or the passphrase is wrong.
    #[error("Invalid configuration: {0}")]
    SshKeyLocked(String),
    /// The profile pins no host key (`ssh.trustedHostKey` is empty): nothing was sent to the server. A client shows the
    /// fingerprint, and on the user's yes saves `host_key` as `trustedHostKey` and connects again.
    #[error("The SSH host {host}:{port} is not trusted yet. Its {key_type} key fingerprint is {fingerprint}.")]
    #[serde(rename_all = "camelCase")]
    UnknownHostKey {
        host: String,
        port: i64,
        key_type: String,
        /// OpenSSH format ("ssh-ed25519 AAAA…"), ready for `trustedHostKey`.
        host_key: String,
        /// `SHA256:…`, as `ssh-keygen -lf` prints it.
        fingerprint: String,
    },
}

impl DbError {
    /// The text shown to the user, secrets removed and capped like GokuCore's `displayMessage`.
    pub fn display_message(&self) -> String {
        let s = crate::redact::secrets(&self.to_string());
        if s.chars().count() > 2000 { s.chars().take(2000).collect::<String>() + "…" } else { s }
    }
}

pub type Result<T, E = DbError> = std::result::Result<T, E>;
