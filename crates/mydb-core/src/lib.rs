//! Core of MyDB Studio: connection profiles, SSH tunnels, query execution and
//! the local app database. Kept free of any UI code so the desktop app and a
//! future MCP server can share it.

pub mod admin;
pub mod advisor;
pub mod ai;
pub mod ddl;
pub mod error;
pub mod export;
pub mod pg;
pub mod pg_admin;
pub mod pg_ddl;
pub mod profile;
pub mod records;
pub mod redis;
pub mod report;
pub mod running;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod split;
pub mod store;
pub mod transfer;
pub mod tunnel;

pub use error::{CoreError, Result};

/// Picks aws-lc-rs as rustls' process-wide crypto provider. Call once at
/// startup, before any TLS connection. Other crates (the updater pulls in
/// ring) enable a second provider, and with two compiled in rustls can't
/// choose one itself: `ClientConfig::builder()` in mysql_async, redis and
/// reqwest would panic.
pub fn install_tls_provider() {
    // Err only means one is already installed, which is fine.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
pub use pg::PgSession;
pub use profile::{
    ConnectionKind, ConnectionProfile, McpAccess, RecordMode, SshAuth, SshConfig, SslConfig,
    SslMode,
};
pub use records::{ExecutionRecord, RecordContext, RecordFilter, RecordedStatement};
pub use redis::RedisSession;
pub use report::{ReportFormat, ReportOptions};
pub use session::{
    AnySession, ColumnDef, DEFAULT_MAX_ROWS, DatabaseInfo, ExecutionOutcome, Session, SessionInfo,
    SqlSession, StatementResult, TableInfo, TextRows,
};
pub use settings::{AppSettings, LOCK_FLAG, PinnedTab, RUNNING_FLAG, Snippet};
pub use split::{split_statements, split_statements_pg};
pub use store::Store;
