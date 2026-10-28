//! Port of GokuCore's `ConnectionProfile` (`ConnectionProfile.swift` in the Goku app). Same JSON field names,
//! and the same tolerant decoding (AGENTS.md, "Profiles"): only `name` and `kind` are required, missing or null
//! fields get their defaults, unknown enum cases a safe fallback (an unknown SSL mode becomes the strictest), malformed values fail.
use crate::error::DbError;
use crate::limits::{DEFAULT_MAX_ROWS, HARD_MAX_ROWS};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// String enums encoded by their Swift raw values.
macro_rules! raw_enum {
    ($(#[$m:meta])* $name:ident { $($case:ident = $raw:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($case),+ }
        impl $name {
            pub const ALL: &[$name] = &[$($name::$case),+];
            pub fn raw(self) -> &'static str { match self { $($name::$case => $raw),+ } }
            pub fn from_raw(s: &str) -> Option<Self> { match s { $($raw => Some($name::$case),)+ _ => None } }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { s.serialize_str(self.raw()) }
        }
    };
}

raw_enum!(DatabaseKind { Postgres = "postgres", Mysql = "mysql", Mongodb = "mongodb" });
raw_enum!(TopologyMode { Single = "single", MultiHost = "multiHost", Srv = "srv", Uri = "uri" });
raw_enum!(TargetSession {
    Any = "any", ReadWrite = "readWrite", ReadOnly = "readOnly", Primary = "primary", Standby = "standby",
    PreferStandby = "preferStandby",
});
raw_enum!(MongoReadPreference {
    Primary = "primary", PrimaryPreferred = "primaryPreferred", Secondary = "secondary",
    SecondaryPreferred = "secondaryPreferred", Nearest = "nearest",
});
raw_enum!(SslMode { Disable = "disable", Prefer = "prefer", Require = "require", VerifyCa = "verifyCA", VerifyFull = "verifyFull" });
raw_enum!(SshAuthKind { Password = "password", PrivateKey = "privateKey" });
raw_enum!(AgentAccess { Off = "off", Read = "read", ReadWrite = "readWrite" });

impl DatabaseKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Mysql => "MySQL / MariaDB",
            Self::Mongodb => "MongoDB",
        }
    }
    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::Mysql => 3306,
            Self::Mongodb => 27017,
        }
    }
    pub fn default_user(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "root",
            Self::Mongodb => "",
        }
    }
}

impl<'de> Deserialize<'de> for DatabaseKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_raw(&s).ok_or_else(|| D::Error::custom(format!("unknown database kind “{s}”")))
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string().to_uppercase()
}

/// The keyed container of one JSON object, with Swift's `value` / `oneOf` helpers.
struct Fields<'a>(&'a Map<String, Value>);

impl Fields<'_> {
    fn present(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    /// Missing or null → `fallback`. A value that is present but malformed fails.
    fn value<T: DeserializeOwned>(&self, key: &str, fallback: impl FnOnce() -> T) -> Result<T, String> {
        match self.present(key) {
            None => Ok(fallback()),
            Some(v) => T::deserialize(v).map_err(|e| format!("“{key}”: {e}")),
        }
    }

    fn required<T: DeserializeOwned>(&self, key: &str) -> Result<T, String> {
        let v = self.present(key).ok_or_else(|| format!("“{key}” is missing"))?;
        T::deserialize(v).map_err(|e| format!("“{key}”: {e}"))
    }

    /// Like `value`, but a case this version doesn't know (e.g. added by a newer one) becomes `unknown`.
    fn one_of<T>(&self, key: &str, from_raw: fn(&str) -> Option<T>, missing: T, unknown: T) -> Result<T, String> {
        match self.present(key) {
            None => Ok(missing),
            Some(Value::String(s)) => Ok(from_raw(s).unwrap_or(unknown)),
            Some(other) => Err(format!("“{key}”: expected a string, found {other}")),
        }
    }
}

/// Implements `Deserialize` through `from_fields`, so nested types decode as tolerantly as the profile.
macro_rules! tolerant {
    ($name:ident) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let map = Map::deserialize(d)?;
                Self::from_fields(&Fields(&map)).map_err(D::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct HostPort {
    pub id: String,
    pub host: String,
    pub port: i64,
}

impl HostPort {
    pub fn new(host: impl Into<String>, port: i64) -> Self {
        Self { id: new_id(), host: host.into(), port }
    }
    fn from_fields(f: &Fields) -> Result<Self, String> {
        Ok(Self { id: f.value("id", new_id)?, host: f.required("host")?, port: f.required("port")? })
    }
}
tolerant!(HostPort);

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SslSettings {
    pub mode: SslMode,
    pub ca_certificate_path: String,
    pub client_certificate_path: String,
    pub client_key_path: String,
}

impl Default for SslSettings {
    fn default() -> Self {
        Self {
            mode: SslMode::Prefer,
            ca_certificate_path: String::new(),
            client_certificate_path: String::new(),
            client_key_path: String::new(),
        }
    }
}

impl SslSettings {
    fn from_fields(f: &Fields) -> Result<Self, String> {
        Ok(Self {
            mode: f.one_of("mode", SslMode::from_raw, SslMode::Prefer, SslMode::VerifyFull)?,
            ca_certificate_path: f.value("caCertificatePath", String::new)?,
            client_certificate_path: f.value("clientCertificatePath", String::new)?,
            client_key_path: f.value("clientKeyPath", String::new)?,
        })
    }
}
tolerant!(SslSettings);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSettings {
    pub enabled: bool,
    pub host: String,
    pub port: i64,
    pub user: String,
    pub auth: SshAuthKind,
    pub private_key_path: String,
    /// Expected host key in OpenSSH format ("ssh-ed25519 AAAA..."). Empty = trust on first use.
    pub trusted_host_key: String,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: SshAuthKind::Password,
            private_key_path: String::new(),
            trusted_host_key: String::new(),
        }
    }
}

impl SshSettings {
    fn from_fields(f: &Fields) -> Result<Self, String> {
        Ok(Self {
            enabled: f.value("enabled", || false)?,
            host: f.value("host", String::new)?,
            port: f.value("port", || 22)?,
            user: f.value("user", String::new)?,
            auth: f.one_of("auth", SshAuthKind::from_raw, SshAuthKind::Password, SshAuthKind::Password)?,
            private_key_path: f.value("privateKeyPath", String::new)?,
            trusted_host_key: f.value("trustedHostKey", String::new)?,
        })
    }
}
tolerant!(SshSettings);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ReadOnlyRule {
    pub id: String,
    pub database: String,
    /// Empty = the whole database. `schema.table` is accepted; only the last part is compared.
    pub table: String,
}

impl ReadOnlyRule {
    fn from_fields(f: &Fields) -> Result<Self, String> {
        Ok(Self {
            id: f.value("id", new_id)?,
            database: f.value("database", String::new)?,
            table: f.value("table", String::new)?,
        })
    }
}
tolerant!(ReadOnlyRule);

/// Secrets are kept apart from the profile: the extension keeps them in SecretStorage and sends them only over stdin.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Credentials {
    pub password: Option<String>,
    pub ssh_password: Option<String>,
    pub ssh_key_passphrase: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mask = |s: &Option<String>| s.as_ref().map(|_| "****");
        f.debug_struct("Credentials")
            .field("password", &mask(&self.password))
            .field("ssh_password", &mask(&self.ssh_password))
            .field("ssh_key_passphrase", &mask(&self.ssh_key_passphrase))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub kind: DatabaseKind,
    pub mode: TopologyMode,
    pub hosts: Vec<HostPort>,
    pub user: String,
    pub database: String,
    pub ssl: SslSettings,
    pub ssh: SshSettings,
    pub target_session: TargetSession,
    // MongoDB specific
    pub srv_host: String,
    pub uri: String,
    pub replica_set: String,
    pub auth_source: String,
    pub read_preference: MongoReadPreference,
    // Safety limits
    pub connect_timeout_seconds: i64,
    pub query_timeout_seconds: i64,
    pub max_rows: i64,
    pub read_only: bool,
    pub read_only_rules: Vec<ReadOnlyRule>,
    pub agent_access: AgentAccess,
    /// Not used by the engine; kept as JSON so a round trip doesn't lose the app's per-object agent levels.
    pub agent_rules: Vec<Value>,
    pub color_tag: String,
    pub group: String,
}

impl ConnectionProfile {
    pub fn new(name: impl Into<String>, kind: DatabaseKind) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            kind,
            mode: TopologyMode::Single,
            hosts: vec![HostPort::new("localhost", kind.default_port().into())],
            user: kind.default_user().into(),
            database: String::new(),
            ssl: SslSettings::default(),
            ssh: SshSettings::default(),
            target_session: TargetSession::Any,
            srv_host: String::new(),
            uri: String::new(),
            replica_set: String::new(),
            auth_source: String::new(),
            read_preference: MongoReadPreference::Primary,
            connect_timeout_seconds: 10,
            query_timeout_seconds: 60,
            max_rows: DEFAULT_MAX_ROWS as i64,
            read_only: false,
            read_only_rules: Vec::new(),
            agent_access: AgentAccess::Off,
            agent_rules: Vec::new(),
            color_tag: String::new(),
            group: String::new(),
        }
    }

    fn from_fields(f: &Fields) -> Result<Self, String> {
        let kind: DatabaseKind = f.required("kind")?;
        Ok(Self {
            name: f.required("name")?,
            id: f.value("id", new_id)?,
            mode: f.one_of("mode", TopologyMode::from_raw, TopologyMode::Single, TopologyMode::Single)?,
            hosts: f.value("hosts", || vec![HostPort::new("localhost", kind.default_port().into())])?,
            user: f.value("user", || kind.default_user().to_owned())?,
            database: f.value("database", String::new)?,
            ssl: f.value("ssl", SslSettings::default)?,
            ssh: f.value("ssh", SshSettings::default)?,
            target_session: f.one_of(
                "targetSession",
                TargetSession::from_raw,
                TargetSession::Any,
                TargetSession::Any,
            )?,
            srv_host: f.value("srvHost", String::new)?,
            uri: f.value("uri", String::new)?,
            replica_set: f.value("replicaSet", String::new)?,
            auth_source: f.value("authSource", String::new)?,
            read_preference: f.one_of(
                "readPreference",
                MongoReadPreference::from_raw,
                MongoReadPreference::Primary,
                MongoReadPreference::Primary,
            )?,
            connect_timeout_seconds: f.value("connectTimeoutSeconds", || 10)?,
            query_timeout_seconds: f.value("queryTimeoutSeconds", || 60)?,
            max_rows: f.value("maxRows", || DEFAULT_MAX_ROWS as i64)?,
            read_only: f.value("readOnly", || false)?,
            read_only_rules: f.value("readOnlyRules", Vec::new)?,
            agent_access: f.one_of("agentAccess", AgentAccess::from_raw, AgentAccess::Off, AgentAccess::Off)?,
            agent_rules: f.value("agentRules", Vec::new)?,
            color_tag: f.value("colorTag", String::new)?,
            group: f.value("group", String::new)?,
            kind,
        })
    }

    /// Short human description of the endpoint, never includes secrets.
    pub fn endpoint_summary(&self) -> String {
        match self.mode {
            TopologyMode::Srv => format!("srv://{}", self.srv_host),
            TopologyMode::Uri => crate::redact::uri(&self.uri),
            _ => self.hosts.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
        }
    }

    pub fn validate(&self) -> Result<(), DbError> {
        let bad = |m: &str| Err(DbError::InvalidConfiguration(m.into()));
        match self.mode {
            TopologyMode::Single | TopologyMode::MultiHost => {
                if self.hosts.is_empty()
                    || !self.hosts.iter().all(|h| !h.host.trim().is_empty() && (1..=65535).contains(&h.port))
                {
                    return bad("Every host needs a name and a port between 1 and 65535.");
                }
                if self.mode == TopologyMode::Single && self.hosts.len() != 1 {
                    return bad("Single-server mode needs exactly one host.");
                }
            }
            TopologyMode::Srv => {
                if self.kind != DatabaseKind::Mongodb || self.srv_host.is_empty() {
                    return bad("SRV mode needs a DNS host name (MongoDB only).");
                }
            }
            TopologyMode::Uri => {
                if self.kind != DatabaseKind::Mongodb
                    || !(self.uri.starts_with("mongodb://") || self.uri.starts_with("mongodb+srv://"))
                {
                    return bad("Connection string must start with mongodb:// or mongodb+srv://");
                }
            }
        }
        if self.kind != DatabaseKind::Mongodb && self.user.is_empty() {
            return bad("User name is required.");
        }
        if self.ssh.enabled {
            if self.ssh.host.is_empty() || self.ssh.user.is_empty() {
                return bad("SSH tunnel needs a host and user.");
            }
            if matches!(self.mode, TopologyMode::Srv | TopologyMode::Uri) {
                return bad("SSH tunnel requires explicit hosts (not SRV / URI).");
            }
        }
        if !(1..=300).contains(&self.connect_timeout_seconds)
            || !(1..=86_400).contains(&self.query_timeout_seconds)
            || !(1..=HARD_MAX_ROWS as i64).contains(&self.max_rows)
        {
            return bad("Timeouts or row limit out of range.");
        }
        if self.read_only_rules.iter().any(|r| r.database.trim().is_empty()) {
            return bad("Every read-only rule needs a database name.");
        }
        Ok(())
    }
}
tolerant!(ConnectionProfile);

/// Profiles that decoded, and entries that didn't (raw JSON + why), kept so a writer can put them back unchanged.
pub type DecodedProfiles = (Vec<ConnectionProfile>, Vec<(Value, String)>);

/// A list of profiles (`connections.json`, a workspace setting): each entry decodes on its own, so one unreadable entry
/// (e.g. an engine added by a newer version) doesn't hide the others. Unreadable entries come back as raw JSON with why.
pub fn decode_profiles(data: &str) -> Result<DecodedProfiles, DbError> {
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(data) else {
        return Err(DbError::Parse("The connections file is not a JSON array.".into()));
    };
    let mut profiles = Vec::new();
    let mut unreadable = Vec::new();
    for entry in entries {
        match ConnectionProfile::deserialize(&entry) {
            Ok(p) => profiles.push(p),
            Err(e) => unreadable.push((entry, e.to_string())),
        }
    }
    Ok((profiles, unreadable))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decode(v: Value) -> Result<ConnectionProfile, String> {
        ConnectionProfile::deserialize(&v).map_err(|e| e.to_string())
    }

    #[test]
    fn minimal_profile_gets_defaults() {
        let p = decode(json!({"name": "pg", "kind": "postgres"})).unwrap();
        assert_eq!(
            (p.mode, p.user.as_str(), p.hosts.len(), p.hosts[0].port),
            (TopologyMode::Single, "postgres", 1, 5432)
        );
        assert_eq!((p.connect_timeout_seconds, p.query_timeout_seconds, p.max_rows), (10, 60, 1000));
        assert_eq!(
            (p.ssl.mode, p.ssh.port, p.read_only, p.agent_access),
            (SslMode::Prefer, 22, false, AgentAccess::Off)
        );
        assert_eq!(p.id.len(), 36);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn nulls_count_as_missing() {
        let p = decode(json!({"name": "m", "kind": "mysql", "user": null, "ssl": null, "maxRows": null})).unwrap();
        assert_eq!((p.user.as_str(), p.ssl.mode, p.max_rows), ("root", SslMode::Prefer, 1000));
    }

    #[test]
    fn unknown_enum_cases_fall_back_safely() {
        let p = decode(json!({
            "name": "x", "kind": "mongodb", "mode": "quantum", "targetSession": "nope",
            "readPreference": "closest", "agentAccess": "root", "ssl": {"mode": "maybe"}, "ssh": {"auth": "kerberos"}
        }))
        .unwrap();
        assert_eq!(p.mode, TopologyMode::Single);
        assert_eq!(p.target_session, TargetSession::Any);
        assert_eq!(p.read_preference, MongoReadPreference::Primary);
        assert_eq!(p.agent_access, AgentAccess::Off);
        assert_eq!(p.ssl.mode, SslMode::VerifyFull, "an unknown SSL mode must become the strictest");
        assert_eq!(p.ssh.auth, SshAuthKind::Password);
    }

    #[test]
    fn required_and_malformed_fields_fail() {
        assert!(decode(json!({"kind": "postgres"})).is_err());
        assert!(decode(json!({"name": "x"})).is_err());
        assert!(decode(json!({"name": "x", "kind": "oracle"})).is_err());
        assert!(decode(json!({"name": "x", "kind": "postgres", "maxRows": "many"})).is_err());
        assert!(decode(json!({"name": "x", "kind": "postgres", "mode": 3})).is_err());
        assert!(decode(json!({"name": "x", "kind": "postgres", "hosts": [{"host": "h"}]})).is_err());
    }

    #[test]
    fn unknown_fields_are_ignored_and_round_trip_keeps_names() {
        let p = decode(json!({
            "name": "x", "kind": "postgres", "futureField": 1, "agentRules": [{"database": "d", "level": "read"}],
            "hosts": [{"host": "db", "port": 6543}], "ssl": {"mode": "verifyCA", "caCertificatePath": "/ca.pem"}
        }))
        .unwrap();
        let back = serde_json::to_value(&p).unwrap();
        assert_eq!(back["ssl"]["mode"], "verifyCA");
        assert_eq!(back["ssl"]["caCertificatePath"], "/ca.pem");
        assert_eq!(back["hosts"][0]["port"], 6543);
        assert_eq!(back["agentRules"][0]["level"], "read");
        assert!(back.get("futureField").is_none());
        assert_eq!(decode(back).unwrap(), p);
    }

    #[test]
    fn list_keeps_unreadable_entries() {
        let (ok, bad) = decode_profiles(
            r#"[{"name":"a","kind":"postgres"},{"name":"b","kind":"cassandra"},{"name":"c","kind":"mongodb"}]"#,
        )
        .unwrap();
        assert_eq!(ok.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].0["kind"], "cassandra");
        assert!(decode_profiles("{}").is_err());
    }

    #[test]
    fn validation_matches_gokucore() {
        let mut p = ConnectionProfile::new("x", DatabaseKind::Postgres);
        p.hosts[0].port = 0;
        assert!(p.validate().is_err());
        p.hosts[0].port = 5432;
        p.max_rows = HARD_MAX_ROWS as i64 + 1;
        assert!(p.validate().is_err());
        p.max_rows = 10;
        p.mode = TopologyMode::Uri;
        assert!(p.validate().is_err(), "URI mode is MongoDB only");
        let mut m = ConnectionProfile::new("m", DatabaseKind::Mongodb);
        m.mode = TopologyMode::Uri;
        m.uri = "mongodb://u:secret@h/db".into();
        assert!(m.validate().is_ok());
        assert_eq!(m.endpoint_summary(), "mongodb://u:****@h/db");
        m.ssh.enabled = true;
        m.ssh.host = "bastion".into();
        m.ssh.user = "me".into();
        assert!(m.validate().is_err(), "SSH needs explicit hosts");
        m.mode = TopologyMode::MultiHost;
        m.hosts = vec![HostPort::new("a", 27017), HostPort::new("::1", 27018)];
        assert!(m.validate().is_ok());
        assert_eq!(m.endpoint_summary(), "a:27017, [::1]:27018");
    }

    #[test]
    fn credentials_never_print() {
        let c: Credentials = serde_json::from_value(json!({"password": "hunter2", "sshPassword": null})).unwrap();
        let s = format!("{c:?}");
        assert!(!s.contains("hunter2") && s.contains("****"));
    }
}
