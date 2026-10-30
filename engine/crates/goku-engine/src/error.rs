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
}

impl DbError {
    /// The `kind` tag of the JSON shape (`{kind, detail}`), for results that carry an error without its detail.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidConfiguration(_) => "invalidConfiguration",
            Self::ConnectionFailed(_) => "connectionFailed",
            Self::NoSuitableHost(_) => "noSuitableHost",
            Self::Timeout(_) => "timeout",
            Self::Cancelled => "cancelled",
            Self::ReadOnly => "readOnly",
            Self::ReadOnlyObject(_) => "readOnlyObject",
            Self::ServerReadOnly(_) => "serverReadOnly",
            Self::NotConnected => "notConnected",
            Self::Unsupported(_) => "unsupported",
            Self::Query(_) => "query",
            Self::Parse(_) => "parse",
        }
    }

    /// The text shown to the user, secrets removed and capped like GokuCore's `displayMessage`.
    pub fn display_message(&self) -> String {
        let s = crate::redact::secrets(&self.to_string());
        if s.chars().count() > 2000 { s.chars().take(2000).collect::<String>() + "…" } else { s }
    }
}

pub type Result<T, E = DbError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_matches_the_json_tag() {
        for e in [
            DbError::InvalidConfiguration(String::new()),
            DbError::ConnectionFailed(String::new()),
            DbError::NoSuitableHost(String::new()),
            DbError::Timeout(String::new()),
            DbError::Cancelled,
            DbError::ReadOnly,
            DbError::ReadOnlyObject(String::new()),
            DbError::ServerReadOnly(String::new()),
            DbError::NotConnected,
            DbError::Unsupported(String::new()),
            DbError::Query(String::new()),
            DbError::Parse(String::new()),
        ] {
            assert_eq!(serde_json::to_value(&e).unwrap()["kind"], e.kind());
        }
    }
}
