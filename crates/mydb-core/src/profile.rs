use serde::{Deserialize, Serialize};

/// A saved connection to a MySQL or MariaDB server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
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
    /// Starred connections sort first on the home screen and sidebar.
    #[serde(default)]
    pub favorite: bool,
    /// When this connection was last opened (RFC 3339). Maintained by the
    /// store; ignored when saving.
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// What AI tools may do with this connection through the MCP server.
    #[serde(default)]
    pub mcp: McpAccess,
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

impl ConnectionProfile {
    pub fn new(name: impl Into<String>, host: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            host: host.into(),
            port: 3306,
            user: user.into(),
            password: None,
            database: None,
            ssl: SslConfig::default(),
            ssh: None,
            favorite: false,
            last_used_at: None,
            mcp: McpAccess::default(),
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
        });
        let json = serde_json::to_string(&p).unwrap();
        let back: ConnectionProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn minimal_json_uses_defaults() {
        let p: ConnectionProfile = serde_json::from_str(
            r#"{"id":"1","name":"local","host":"127.0.0.1","port":3306,"user":"root"}"#,
        )
        .unwrap();
        assert_eq!(p.ssl.mode, SslMode::Disabled);
        assert!(p.ssh.is_none());
    }
}
