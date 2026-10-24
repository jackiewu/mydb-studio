use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Utc};
use mysql_async::prelude::Queryable;
use mysql_async::{ClientIdentity, Conn, OptsBuilder, Row, SslOpts, Value};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::error::{CoreError, Result};
use crate::profile::{ConnectionProfile, SslMode};
use crate::split::split_statements;
use crate::tunnel::SshTunnel;

/// Rows kept per result set unless the caller asks otherwise. The full row
/// count is still reported.
pub const DEFAULT_MAX_ROWS: usize = 1000;

/// An open connection, possibly running through an SSH tunnel.
pub struct Session {
    conn: Mutex<Conn>,
    info: SessionInfo,
    // Declared after `conn` so the tunnel outlives the connection on drop.
    _tunnel: Option<SshTunnel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub server_version: String,
    pub connection_id: u32,
    pub tls: bool,
    /// Set when the connection went through SSH; the UI saves it into the
    /// profile on first use.
    pub ssh_host_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub table: String,
    pub type_name: String,
}

/// Result of one statement in a (possibly multi-statement) query.
#[derive(Debug, Clone, Serialize)]
pub struct StatementResult {
    pub sql: String,
    pub duration_ms: f64,
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Rows returned by the server, which can exceed `rows.len()`.
    pub total_rows: u64,
    pub truncated: bool,
    pub affected_rows: u64,
    pub last_insert_id: Option<u64>,
    pub warnings: u16,
    pub info: String,
}

/// Everything about one run of the editor contents. On error, `results`
/// still holds the statements that completed before it.
#[derive(Debug, Serialize)]
pub struct ExecutionOutcome {
    pub started_at: DateTime<Utc>,
    pub duration_ms: f64,
    pub results: Vec<StatementResult>,
    pub error: Option<CoreError>,
    /// The statement that raised `error`, when it came from one.
    pub failed_statement: Option<String>,
}

impl Session {
    pub async fn connect(profile: &ConnectionProfile) -> Result<Self> {
        let tunnel = match &profile.ssh {
            Some(ssh) => Some(SshTunnel::open(ssh, &profile.host, profile.port).await?),
            None => None,
        };
        let (host, port) = match &tunnel {
            Some(t) => ("127.0.0.1".to_string(), t.local_port()),
            None => (profile.host.clone(), profile.port),
        };

        let mut opts = OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(port)
            .user(Some(profile.user.clone()))
            .pass(profile.password.clone())
            .db_name(profile.database.clone().filter(|d| !d.is_empty()))
            .prefer_socket(false);
        let tls = profile.ssl.mode != SslMode::Disabled;
        if tls {
            opts = opts.ssl_opts(ssl_opts(profile)?);
        }

        let mut conn = Conn::new(opts).await?;
        let server_version: Option<String> = conn.query_first("SELECT VERSION()").await?;
        let info = SessionInfo {
            server_version: server_version.unwrap_or_default(),
            connection_id: conn.id(),
            tls,
            ssh_host_fingerprint: tunnel.as_ref().map(|t| t.host_fingerprint().to_string()),
        };
        Ok(Self {
            conn: Mutex::new(conn),
            info,
            _tunnel: tunnel,
        })
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    /// Runs `sql` (one or more statements) and collects every result set,
    /// keeping at most `max_rows` rows of each.
    pub async fn execute(&self, sql: &str, max_rows: usize) -> ExecutionOutcome {
        let started_at = Utc::now();
        let timer = Instant::now();
        let mut results = Vec::new();
        let mut conn = self.conn.lock().await;
        let mut failed_statement = None;
        let error = run(
            &mut conn,
            sql,
            max_rows,
            &mut results,
            &mut failed_statement,
        )
        .await
        .err();
        ExecutionOutcome {
            started_at,
            duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
            results,
            error,
            failed_statement,
        }
    }

    pub async fn close(self) -> Result<()> {
        self.conn.into_inner().disconnect().await?;
        Ok(())
    }
}

async fn run(
    conn: &mut Conn,
    sql: &str,
    max_rows: usize,
    out: &mut Vec<StatementResult>,
    failed: &mut Option<String>,
) -> Result<()> {
    // Statements run one at a time so each gets its own timing and stats,
    // and an error points at the statement that caused it.
    for stmt in split_statements(sql) {
        *failed = Some(stmt.clone());
        let timer = Instant::now();
        let mut result = conn.query_iter(stmt.as_str()).await?;
        // A statement can still yield several result sets (e.g. CALL).
        loop {
            let columns: Vec<ColumnInfo> = result
                .columns_ref()
                .iter()
                .map(|c| ColumnInfo {
                    name: c.name_str().into_owned(),
                    table: c.table_str().into_owned(),
                    type_name: format!("{:?}", c.column_type())
                        .trim_start_matches("MYSQL_TYPE_")
                        .to_string(),
                })
                .collect();
            let mut rows = Vec::new();
            let mut total_rows = 0u64;
            result
                .for_each(|row: Row| {
                    total_rows += 1;
                    if rows.len() < max_rows {
                        rows.push(row.unwrap().into_iter().map(to_json).collect());
                    }
                })
                .await?;
            out.push(StatementResult {
                sql: stmt.clone(),
                duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
                columns,
                truncated: total_rows > rows.len() as u64,
                rows,
                total_rows,
                affected_rows: result.affected_rows(),
                last_insert_id: result.last_insert_id(),
                warnings: result.warnings(),
                info: result.info().into_owned(),
            });
            if result.is_empty() {
                break;
            }
        }
    }
    *failed = None;
    Ok(())
}

fn ssl_opts(profile: &ConnectionProfile) -> Result<SslOpts> {
    let ssl = &profile.ssl;
    let mut opts = SslOpts::default();
    if let Some(ca) = ssl.ca_path.as_deref().filter(|p| !p.is_empty()) {
        opts = opts.with_root_certs(vec![PathBuf::from(ca).into()]);
    }
    match (
        ssl.cert_path.as_deref().filter(|p| !p.is_empty()),
        ssl.key_path.as_deref().filter(|p| !p.is_empty()),
    ) {
        (Some(cert), Some(key)) => {
            opts = opts.with_client_identity(Some(ClientIdentity::new(
                PathBuf::from(cert).into(),
                PathBuf::from(key).into(),
            )));
        }
        (None, None) => {}
        _ => {
            return Err(CoreError::Config {
                message: "client certificate and key must be set together".into(),
            });
        }
    }
    opts = match ssl.mode {
        SslMode::Disabled | SslMode::Required => opts.with_danger_accept_invalid_certs(true),
        SslMode::VerifyCa => opts.with_danger_skip_domain_validation(true),
        SslMode::VerifyIdentity => opts,
    };
    // Through a tunnel we dial 127.0.0.1, but the certificate names the real host.
    if profile.ssh.is_some() {
        opts = opts.with_danger_tls_hostname_override(Some(profile.host.clone()));
    }
    Ok(opts)
}

/// Converts a cell to JSON. Integers outside JavaScript's safe range become
/// strings so the UI does not silently round them.
fn to_json(value: Value) -> serde_json::Value {
    const SAFE: i64 = (1 << 53) - 1;
    match value {
        Value::NULL => serde_json::Value::Null,
        Value::Bytes(bytes) => match String::from_utf8(bytes) {
            Ok(s) => s.into(),
            Err(e) => format!("0x{}", hex(e.as_bytes())).into(),
        },
        Value::Int(i) if (-SAFE..=SAFE).contains(&i) => i.into(),
        Value::Int(i) => i.to_string().into(),
        Value::UInt(u) if u <= SAFE as u64 => u.into(),
        Value::UInt(u) => u.to_string().into(),
        Value::Float(f) => f.into(),
        Value::Double(d) => d.into(),
        other => other.as_sql(true).trim_matches('\'').to_string().into(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_integers_become_strings() {
        assert_eq!(to_json(Value::Int(42)), serde_json::json!(42));
        assert_eq!(
            to_json(Value::UInt(u64::MAX)),
            serde_json::json!("18446744073709551615")
        );
    }

    #[test]
    fn binary_cells_are_hex() {
        assert_eq!(
            to_json(Value::Bytes(vec![0xff, 0x00])),
            serde_json::json!("0xFF00")
        );
        assert_eq!(
            to_json(Value::Bytes(b"abc".to_vec())),
            serde_json::json!("abc")
        );
    }

    #[test]
    fn dates_are_formatted() {
        assert_eq!(
            to_json(Value::Date(2026, 10, 24, 11, 0, 0, 0)),
            serde_json::json!("2026-10-24 11:00:00")
        );
    }
}
