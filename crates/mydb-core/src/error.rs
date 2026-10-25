use serde::Serialize;

/// Errors surfaced by the core library.
///
/// Server errors keep their MySQL error code and SQLSTATE so the UI (and,
/// later, the error advisor) can look them up.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoreError {
    #[error("MySQL error {code} ({state}): {message}")]
    Server {
        code: u16,
        state: String,
        message: String,
    },
    #[error("driver error: {message}")]
    Driver { message: String },
    /// The network connection to the server failed or was closed.
    #[error("connection error: {message}")]
    Connection { message: String },
    #[error("SSH error: {message}")]
    Ssh { message: String },
    #[error("SSH host key for {host} changed (expected {expected}, got {actual})")]
    HostKeyMismatch {
        host: String,
        expected: String,
        actual: String,
    },
    #[error("I/O error: {message}")]
    Io { message: String },
    #[error("storage error: {message}")]
    Store { message: String },
    #[error("invalid configuration: {message}")]
    Config { message: String },
    #[error("not connected: {id}")]
    NotConnected { id: String },
}

pub type Result<T> = std::result::Result<T, CoreError>;

impl From<mysql_async::Error> for CoreError {
    fn from(err: mysql_async::Error) -> Self {
        match err {
            mysql_async::Error::Server(e) => CoreError::Server {
                code: e.code,
                state: e.state,
                message: e.message,
            },
            // The driver's Display repeats "Input/output error" for both
            // layers, so report the innermost message only.
            mysql_async::Error::Io(mysql_async::IoError::Io(e)) => CoreError::Connection {
                message: e.to_string(),
            },
            mysql_async::Error::Io(e) => CoreError::Connection {
                message: e.to_string(),
            },
            other => CoreError::Driver {
                message: other.to_string(),
            },
        }
    }
}

impl From<russh::Error> for CoreError {
    fn from(err: russh::Error) -> Self {
        CoreError::Ssh {
            message: err.to_string(),
        }
    }
}

impl From<std::io::Error> for CoreError {
    fn from(err: std::io::Error) -> Self {
        CoreError::Io {
            message: err.to_string(),
        }
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(err: rusqlite::Error) -> Self {
        CoreError::Store {
            message: err.to_string(),
        }
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(err: serde_json::Error) -> Self {
        CoreError::Store {
            message: err.to_string(),
        }
    }
}
