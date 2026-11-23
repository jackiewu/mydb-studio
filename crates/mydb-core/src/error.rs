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
    /// `gcloud compute start-iap-tunnel` failed, with what gcloud said.
    #[error("GCP IAP error: {message}")]
    Iap { message: String },
    #[error("I/O error: {message}")]
    Io { message: String },
    #[error("storage error: {message}")]
    Store { message: String },
    #[error("invalid configuration: {message}")]
    Config { message: String },
    #[error("not connected: {id}")]
    NotConnected { id: String },
    /// An error reported by a PostgreSQL server, with its SQLSTATE. The
    /// server's DETAIL and HINT lines follow the message.
    #[error("PostgreSQL error {state}: {message}")]
    Postgres { state: String, message: String },
    /// An error reply from a Redis server, e.g. `WRONGTYPE …`.
    #[error("Redis error: {message}")]
    Redis { message: String },
    /// The user stopped the run before this statement started.
    #[error("cancelled")]
    Cancelled,
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

impl From<::redis::RedisError> for CoreError {
    fn from(err: ::redis::RedisError) -> Self {
        if err.is_io_error()
            || err.is_connection_dropped()
            || err.is_connection_refusal()
            || err.is_timeout()
        {
            CoreError::Connection {
                message: err.to_string(),
            }
        } else {
            // `WRONGTYPE Operation against a key …`, as redis-cli shows it.
            let message = match (err.code(), err.detail()) {
                (Some(code), Some(detail)) => format!("{code} {detail}"),
                _ => err.to_string(),
            };
            CoreError::Redis { message }
        }
    }
}

impl From<tokio_postgres::Error> for CoreError {
    fn from(err: tokio_postgres::Error) -> Self {
        if let Some(db) = err.as_db_error() {
            let mut message = db.message().to_string();
            if let Some(d) = db.detail() {
                message.push_str(&format!("\nDETAIL: {d}"));
            }
            if let Some(h) = db.hint() {
                message.push_str(&format!("\nHINT: {h}"));
            }
            return CoreError::Postgres {
                state: db.code().code().to_string(),
                message,
            };
        }
        let text = match std::error::Error::source(&err) {
            Some(src) => format!("{err}: {src}"),
            None => err.to_string(),
        };
        if err.is_closed()
            || std::error::Error::source(&err).is_some_and(|s| s.is::<std::io::Error>())
        {
            CoreError::Connection { message: text }
        } else {
            CoreError::Driver { message: text }
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
