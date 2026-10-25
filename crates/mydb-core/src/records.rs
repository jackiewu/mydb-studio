//! Execution records: what ran, where, how long it took and what it changed.
//!
//! A record is written only when the user turns recording on for a run.
//! Reports are generated from a selection of records (see `report`), so
//! there is a single data model for both.

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::profile::ConnectionKind;
use crate::session::{ColumnInfo, ExecutionOutcome};

/// Rows of each result set kept in a record, for reports.
pub const RECORD_PREVIEW_ROWS: usize = 50;

/// Per-statement stats kept in a record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordedStatement {
    pub sql: String,
    pub duration_ms: f64,
    pub affected_rows: u64,
    /// Rows returned by a query; `None` for statements without a result set.
    pub returned_rows: Option<u64>,
    pub warnings: u16,
    pub info: String,
    #[serde(default)]
    pub columns: Vec<String>,
    /// First `RECORD_PREVIEW_ROWS` rows of the result set.
    #[serde(default)]
    pub preview: Vec<Vec<serde_json::Value>>,
}

/// A stored execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionRecord {
    pub id: i64,
    pub connection_id: Option<String>,
    pub connection_name: String,
    /// MySQL or Redis; records from before Redis support are MySQL.
    #[serde(default)]
    pub connection_kind: ConnectionKind,
    /// `user@host:port`, kept so records stay readable if the profile goes.
    pub server: String,
    pub server_version: String,
    pub database: Option<String>,
    pub started_at: String,
    pub duration_ms: f64,
    pub sql: String,
    pub statements: Vec<RecordedStatement>,
    pub statement_count: u32,
    pub affected_rows: u64,
    pub returned_rows: u64,
    pub error_code: Option<u16>,
    pub error_message: Option<String>,
    pub failed_statement: Option<String>,
    pub note: String,
}

/// Where a run happened, for building a record.
pub struct RecordContext<'a> {
    pub connection_id: &'a str,
    pub connection_name: &'a str,
    pub server: String,
    pub server_version: &'a str,
    pub database: Option<String>,
}

impl ExecutionRecord {
    /// Builds an unsaved record (`id` 0) from a finished run.
    pub fn from_outcome(ctx: RecordContext<'_>, sql: &str, outcome: &ExecutionOutcome) -> Self {
        let statements: Vec<RecordedStatement> = outcome
            .results
            .iter()
            .map(|r| RecordedStatement {
                sql: r.sql.clone(),
                duration_ms: r.duration_ms,
                affected_rows: r.affected_rows,
                returned_rows: (!r.columns.is_empty()).then_some(r.total_rows),
                warnings: r.warnings,
                info: r.info.clone(),
                columns: r
                    .columns
                    .iter()
                    .map(|c: &ColumnInfo| c.name.clone())
                    .collect(),
                preview: r.rows.iter().take(RECORD_PREVIEW_ROWS).cloned().collect(),
            })
            .collect();
        let (error_code, error_message) = match &outcome.error {
            None => (None, None),
            Some(CoreError::Server { code, message, .. }) => (Some(*code), Some(message.clone())),
            Some(other) => (None, Some(other.to_string())),
        };
        Self {
            id: 0,
            connection_id: Some(ctx.connection_id.to_string()),
            connection_name: ctx.connection_name.to_string(),
            connection_kind: ConnectionKind::Mysql,
            server: ctx.server,
            server_version: ctx.server_version.to_string(),
            database: ctx.database,
            started_at: outcome.started_at.to_rfc3339(),
            duration_ms: outcome.duration_ms,
            sql: sql.trim().to_string(),
            statement_count: statements.len() as u32
                + u32::from(outcome.failed_statement.is_some()),
            affected_rows: statements.iter().map(|s| s.affected_rows).sum(),
            returned_rows: statements.iter().filter_map(|s| s.returned_rows).sum(),
            statements,
            error_code,
            error_message,
            failed_statement: outcome.failed_statement.clone(),
            note: String::new(),
        }
    }

    pub fn succeeded(&self) -> bool {
        self.error_message.is_none()
    }
}

/// Filters for listing records. Empty fields do not filter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordFilter {
    #[serde(default)]
    pub connection_id: Option<String>,
    /// Only records of this kind of connection.
    #[serde(default)]
    pub connection_kind: Option<ConnectionKind>,
    /// Matched against the SQL, the note and the error message.
    #[serde(default)]
    pub text: Option<String>,
    /// Inclusive lower bound, RFC 3339 or `YYYY-MM-DD`.
    #[serde(default)]
    pub from: Option<String>,
    /// Exclusive upper bound, RFC 3339 or `YYYY-MM-DD`.
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub only_errors: bool,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}
