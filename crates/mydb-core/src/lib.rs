//! Core of MyDB Studio: connection profiles, SSH tunnels, query execution and
//! the local app database. Kept free of any UI code so the desktop app and a
//! future MCP server can share it.

pub mod admin;
pub mod advisor;
pub mod ai;
pub mod ddl;
pub mod error;
pub mod export;
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
pub use profile::{
    ConnectionKind, ConnectionProfile, McpAccess, RecordMode, SshAuth, SshConfig, SslConfig,
    SslMode,
};
pub use records::{ExecutionRecord, RecordContext, RecordFilter, RecordedStatement};
pub use redis::RedisSession;
pub use report::{ReportFormat, ReportOptions};
pub use session::{
    AnySession, ColumnDef, DEFAULT_MAX_ROWS, DatabaseInfo, ExecutionOutcome, Session, SessionInfo,
    StatementResult, TableInfo, TextRows,
};
pub use settings::{AppSettings, LOCK_FLAG, PinnedTab, RUNNING_FLAG, Snippet};
pub use split::split_statements;
pub use store::Store;
