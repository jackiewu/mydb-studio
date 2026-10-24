//! Core of MyDB Studio: connection profiles, SSH tunnels, query execution and
//! the local app database. Kept free of any UI code so the desktop app and a
//! future MCP server can share it.

pub mod error;
pub mod profile;
pub mod session;
pub mod split;
pub mod store;
pub mod tunnel;

pub use error::{CoreError, Result};
pub use profile::{ConnectionProfile, SshAuth, SshConfig, SslConfig, SslMode};
pub use session::{DEFAULT_MAX_ROWS, ExecutionOutcome, Session, SessionInfo, StatementResult};
pub use split::split_statements;
pub use store::Store;
