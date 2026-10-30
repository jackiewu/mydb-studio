use serde::{Deserialize, Serialize};

/// A saved connection to a MySQL / MariaDB or Redis server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    /// What the server is. Profiles saved before Redis support have none
    /// and are MySQL.
    #[serde(default)]
    pub kind: ConnectionKind,
    pub host: String,
    pub port: u16,
    /// For Redis, the ACL user name; empty for the `default` user.
    pub user: String,
    /// Stored locally in the app database. Moving secrets to the OS keychain
    /// is tracked as a follow-up.
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub ssl: SslConfig,
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    /// Reach the server through a Google Cloud IAP tunnel to a VM. Alone,
    /// `port` is the port on the VM; with `ssh`, IAP carries the SSH
    /// connection and `host`/`port` are as seen from the VM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iap: Option<IapConfig>,
    /// Starred connections sort first on the home screen and sidebar.
    #[serde(default)]
    pub favorite: bool,
    /// When this connection was last opened (RFC 3339). Maintained by the
    /// store; ignored when saving.
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// What AI tools may do with this connection through the MCP server.
    /// Redis connections are never offered to them.
    #[serde(default)]
    pub mcp: McpAccess,
    /// Whether editor runs on this connection are saved to the records.
    #[serde(default)]
    pub record_mode: RecordMode,
    /// Redis only: the database (0 to 15) selected after connecting.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub redis_db: u8,
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    #[default]
    Mysql,
    Redis,
}

impl ConnectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionKind::Mysql => "mysql",
            ConnectionKind::Redis => "redis",
        }
    }
}

/// How far the MCP server lets AI tools use a connection.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpAccess {
    /// Not listed to AI tools at all.
    Off,
    /// Queries that only read (SELECT, SHOW, EXPLAIN …).
    #[default]
    ReadOnly,
    /// Any statement, including changes to data and schema.
    ReadWrite,
}

/// Whether runs in the editor are saved to the records for a connection.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode {
    /// New tabs follow the app setting ("record by default").
    #[default]
    Auto,
    /// New tabs record; the user can turn it off per tab.
    On,
    /// New tabs don't record; the user can turn it on per tab.
    Off,
    /// Every run is recorded and recording can't be turned off.
    Forced,
}

impl ConnectionProfile {
    /// This profile with the SSH host key seen on connecting pinned, if it
    /// had none: later connects through it (a reconnect, a cancel) then
    /// refuse a different key instead of trusting whatever answers.
    pub fn with_pinned_host_key(&self, seen: Option<&str>) -> ConnectionProfile {
        let mut p = self.clone();
        if let (Some(ssh), Some(fp)) = (p.ssh.as_mut(), seen.filter(|f| !f.is_empty())) {
            ssh.known_host_fingerprint
                .get_or_insert_with(|| fp.to_string());
        }
        p
    }

    pub fn new(name: impl Into<String>, host: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            kind: ConnectionKind::Mysql,
            host: host.into(),
            port: 3306,
            user: user.into(),
            password: None,
            database: None,
            ssl: SslConfig::default(),
            ssh: None,
            iap: None,
            favorite: false,
            last_used_at: None,
            mcp: McpAccess::default(),
            record_mode: RecordMode::default(),
            redis_db: 0,
        }
    }

    /// A Redis connection with the default port and user.
    pub fn new_redis(name: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            kind: ConnectionKind::Redis,
            port: 6379,
            ..Self::new(name, host, "")
        }
    }

    /// Whether AI tools may see and use this connection through MCP. The
    /// MCP server only speaks SQL, so Redis connections are left out.
    pub fn mcp_allowed(&self) -> bool {
        self.kind == ConnectionKind::Mysql && self.mcp != McpAccess::Off
    }

    /// `user@host:port`, as shown in records.
    pub fn server_label(&self) -> String {
        match (self.kind, self.user.is_empty()) {
            (ConnectionKind::Redis, true) => format!("{}:{}", self.host, self.port),
            _ => format!("{}@{}:{}", self.user, self.host, self.port),
        }
    }
}

/// TLS settings, mirroring the `--ssl-mode` values of the mysql CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SslConfig {
    #[serde(default)]
    pub mode: SslMode,
    /// PEM or DER file with the CA certificate(s) to trust.
    #[serde(default)]
    pub ca_path: Option<String>,
    /// Client certificate chain, for servers that require X.509 auth.
    #[serde(default)]
    pub cert_path: Option<String>,
    /// Private key matching `cert_path`.
    #[serde(default)]
    pub key_path: Option<String>,
    /// The cloud service the settings were filled in for (`aws`, `gcp`,
    /// `azure`), so the form can show its hints again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
}

/// Amazon RDS's CA bundle for every region, shipped with the app so AWS
/// connections can verify the server without a download. From
/// <https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem>.
pub const AWS_RDS_CA_BUNDLE: &str = include_str!("../certs/aws-rds-global-bundle.pem");

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SslMode {
    /// Plain TCP.
    #[default]
    Disabled,
    /// Encrypt, but do not verify the server certificate.
    Required,
    /// Encrypt and verify the certificate chain, but not the host name.
    VerifyCa,
    /// Encrypt and verify both the chain and the host name.
    VerifyIdentity,
}

/// Reach the database through an SSH jump host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// SHA-256 fingerprint of the jump host key, learned on first connect.
    /// A different key on a later connect is refused.
    #[serde(default)]
    pub known_host_fingerprint: Option<String>,
    /// A Unix socket on the jump host to connect to instead of the
    /// profile's host and port, e.g. `/run/mysqld/mysqld.sock`, for
    /// accounts that may only log in from `localhost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket_path: Option<String>,
}

/// A Compute Engine VM reached with `gcloud compute start-iap-tunnel`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IapConfig {
    /// VM instance name.
    pub instance: String,
    /// Empty: gcloud's configured default zone.
    #[serde(default)]
    pub zone: String,
    /// Empty: gcloud's configured default project.
    #[serde(default)]
    pub project: String,
    /// Full path to gcloud, when it isn't found on its own.
    #[serde(default)]
    pub gcloud_path: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum SshAuth {
    Password {
        password: String,
    },
    Key {
        key_path: String,
        #[serde(default)]
        passphrase: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_seen_host_key_gets_pinned_but_never_replaced() {
        let mut p = ConnectionProfile::new("prod", "db.internal", "app");
        assert!(p.with_pinned_host_key(Some("SHA256:a")).ssh.is_none());
        p.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "jackie".into(),
            auth: SshAuth::Password {
                password: String::new(),
            },
            known_host_fingerprint: None,
            socket_path: None,
        });
        let pinned = p.with_pinned_host_key(Some("SHA256:a"));
        let fp = |p: &ConnectionProfile| p.ssh.as_ref().unwrap().known_host_fingerprint.clone();
        assert_eq!(fp(&pinned).as_deref(), Some("SHA256:a"));
        assert_eq!(
            fp(&pinned.with_pinned_host_key(Some("SHA256:b"))).as_deref(),
            Some("SHA256:a")
        );
        assert_eq!(fp(&p.with_pinned_host_key(Some(""))), None);
    }

    #[test]
    fn profile_round_trips_through_json() {
        let mut p = ConnectionProfile::new("prod", "db.internal", "app");
        p.ssl.mode = SslMode::VerifyCa;
        p.ssl.ca_path = Some("/etc/ssl/ca.pem".into());
        p.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "jackie".into(),
            auth: SshAuth::Key {
                key_path: "~/.ssh/id_ed25519".into(),
                passphrase: None,
            },
            known_host_fingerprint: None,
            socket_path: None,
        });
        p.iap = Some(IapConfig {
            instance: "db-vm".into(),
            zone: "asia-east1-b".into(),
            project: "my-proj".into(),
            gcloud_path: None,
        });
        let json = serde_json::to_string(&p).unwrap();
        let back: ConnectionProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        // Profiles without IAP are written as before.
        assert!(
            !serde_json::to_string(&ConnectionProfile::new("x", "h", "u"))
                .unwrap()
                .contains("iap")
        );
    }

    #[test]
    fn minimal_json_uses_defaults() {
        let p: ConnectionProfile = serde_json::from_str(
            r#"{"id":"1","name":"local","host":"127.0.0.1","port":3306,"user":"root"}"#,
        )
        .unwrap();
        assert_eq!(p.ssl.mode, SslMode::Disabled);
        assert!(p.ssh.is_none());
        // Saved before Redis support: a MySQL connection.
        assert_eq!(p.kind, ConnectionKind::Mysql);
        assert!(p.mcp_allowed());
    }

    #[test]
    fn redis_profiles_round_trip_and_stay_out_of_mcp() {
        let mut p = ConnectionProfile::new_redis("cache", "10.0.0.5");
        p.redis_db = 3;
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""kind":"redis""#) && json.contains(r#""redis_db":3"#));
        assert_eq!(serde_json::from_str::<ConnectionProfile>(&json).unwrap(), p);
        assert_eq!(p.port, 6379);
        assert!(!p.mcp_allowed());
        assert_eq!(p.server_label(), "10.0.0.5:6379");
        // db 0 is the default and not written out.
        let plain = ConnectionProfile::new("x", "h", "u");
        assert!(!serde_json::to_string(&plain).unwrap().contains("redis_db"));
    }
}
