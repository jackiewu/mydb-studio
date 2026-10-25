use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
///
/// If the server drops the connection (for example after `wait_timeout`),
/// the next call reconnects transparently, re-opening the tunnel if needed
/// and returning to the database selected with `USE`.
pub struct Session {
    profile: ConnectionProfile,
    link: Mutex<Option<Link>>,
    info: SessionInfo,
    /// Database chosen with `USE`, restored after a reconnect.
    current_db: std::sync::Mutex<Option<String>>,
    /// Server thread id of the live connection, which changes on reconnect.
    thread_id: AtomicU32,
    /// A statement is running (holding the link).
    busy: AtomicBool,
    /// [`Session::cancel`] was called during the current run.
    cancelled: AtomicBool,
}

struct Link {
    conn: Conn,
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
    /// The LIMIT the app added to this query, when it added one; `sql` is
    /// the statement as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_added: Option<u64>,
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
    /// The connection had been lost and was re-established before running.
    pub reconnected: bool,
    /// The user stopped the run; `error` says where.
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DatabaseInfo {
    pub name: String,
    pub table_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableInfo {
    pub name: String,
    /// `BASE TABLE`, `VIEW`, `SYSTEM VIEW`, ...
    pub kind: String,
    pub engine: Option<String>,
    /// InnoDB's estimate, not an exact count.
    pub rows_estimate: Option<u64>,
    /// Data plus index size in bytes.
    pub size_bytes: Option<u64>,
    pub comment: String,
}

/// Column names and rows of a small query, every cell as text.
#[derive(Debug, Clone, Serialize)]
pub struct TextRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// A column, as needed for autocomplete and table details.
#[derive(Debug, Clone, Serialize)]
pub struct ColumnDef {
    pub table: String,
    pub name: String,
    /// Full type, e.g. `varchar(64)` or `int(10) unsigned`.
    pub column_type: String,
    /// `PRI`, `UNI`, `MUL` or empty.
    pub key: String,
    pub nullable: bool,
    pub comment: String,
}

impl Session {
    pub async fn connect(profile: &ConnectionProfile) -> Result<Self> {
        let (link, info) = open_link(profile, None).await?;
        Ok(Self {
            profile: profile.with_pinned_host_key(info.ssh_host_fingerprint.as_deref()),
            link: Mutex::new(Some(link)),
            thread_id: AtomicU32::new(info.connection_id),
            info,
            current_db: std::sync::Mutex::new(None),
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        })
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    pub fn profile(&self) -> &ConnectionProfile {
        &self.profile
    }

    /// The database in use: the last `USE`, else the profile default.
    pub fn current_database(&self) -> Option<String> {
        self.current_db
            .lock()
            .unwrap()
            .clone()
            .or_else(|| self.profile.database.clone())
            .filter(|d| !d.is_empty())
    }

    /// Returns a live connection, reconnecting if the old one is gone.
    /// The bool is true when a reconnect happened.
    async fn live<'a>(&self, link: &'a mut Option<Link>) -> Result<(&'a mut Conn, bool)> {
        let alive = match link.as_mut() {
            Some(l) => l.conn.ping().await.is_ok(),
            None => false,
        };
        if !alive {
            // Drop the dead link (and its tunnel) before opening a new one.
            *link = None;
            let db = self.current_db.lock().unwrap().clone();
            let (new_link, info) = open_link(&self.profile, db).await?;
            self.thread_id.store(info.connection_id, Ordering::SeqCst);
            *link = Some(new_link);
        }
        Ok((&mut link.as_mut().unwrap().conn, !alive))
    }

    /// Round trip of a `COM_PING`, for the status bar. `None` while a
    /// statement holds the connection; never reconnects, so session state
    /// is left alone until the next real run.
    pub async fn ping(&self) -> Result<Option<f64>> {
        if self.busy.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let Ok(mut guard) = self.link.try_lock() else {
            return Ok(None);
        };
        let Some(l) = guard.as_mut() else {
            return Err(CoreError::Connection {
                message: "連線已中斷".into(),
            });
        };
        let timer = Instant::now();
        l.conn.ping().await?;
        Ok(Some(timer.elapsed().as_secs_f64() * 1000.0))
    }

    /// `@@wait_timeout`: seconds the server keeps this connection while it
    /// is idle. Needs no privileges.
    pub async fn wait_timeout(&self) -> Result<Option<u64>> {
        let rows = self.query_text("SELECT @@wait_timeout").await?;
        Ok(rows
            .rows
            .first()
            .and_then(|r| r.first().cloned().flatten())
            .and_then(|v| v.parse().ok()))
    }

    /// Runs `sql` (one or more statements) and collects every result set,
    /// keeping at most `max_rows` rows of each.
    pub async fn execute(&self, sql: &str, max_rows: usize) -> ExecutionOutcome {
        self.execute_limited(sql, max_rows, None).await
    }

    /// Like [`Session::execute`], adding `LIMIT auto_limit` to queries that
    /// have none (see [`crate::advisor::add_limit`]).
    pub async fn execute_limited(
        &self,
        sql: &str,
        max_rows: usize,
        auto_limit: Option<u64>,
    ) -> ExecutionOutcome {
        let started_at = Utc::now();
        let timer = Instant::now();
        let mut results = Vec::new();
        let mut failed_statement = None;
        let mut reconnected = false;
        let mut guard = self.link.lock().await;
        let _busy = self.start_run();

        let error = match self.live(&mut guard).await {
            Err(e) => Some(e),
            Ok((conn, r)) => {
                reconnected = r;
                let res = run(
                    conn,
                    sql,
                    max_rows,
                    auto_limit,
                    &self.cancelled,
                    &mut results,
                    &mut failed_statement,
                )
                .await;
                if results.iter().any(|r| is_use(&r.sql))
                    && let Ok(db) = conn
                        .query_first::<Option<String>, _>("SELECT DATABASE()")
                        .await
                {
                    *self.current_db.lock().unwrap() = db.flatten();
                }
                res.err()
            }
        };
        // Anything but a server-reported error may have broken the link;
        // the ping on the next call decides whether to reconnect.
        ExecutionOutcome {
            started_at,
            duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
            results,
            error,
            failed_statement,
            reconnected,
            cancelled: self.cancelled.load(Ordering::SeqCst),
        }
    }

    /// Marks a run as started until the guard drops.
    fn start_run(&self) -> BusyGuard<'_> {
        self.cancelled.store(false, Ordering::SeqCst);
        self.busy.store(true, Ordering::SeqCst);
        BusyGuard(&self.busy)
    }

    /// Stops the statement that is running, if any, with `KILL QUERY` sent
    /// over a second connection (the first one is busy waiting for the
    /// result). Later statements of the same run are skipped. Returns
    /// whether something was running.
    pub async fn cancel(&self) -> Result<bool> {
        if !self.busy.load(Ordering::SeqCst) {
            return Ok(false);
        }
        self.cancelled.store(true, Ordering::SeqCst);
        let (mut link, _) = open_link(&self.profile, None).await?;
        let id = self.thread_id.load(Ordering::SeqCst);
        let killed = link.conn.query_drop(format!("KILL QUERY {id}")).await;
        let _ = link.conn.disconnect().await;
        match killed {
            // 1094: the statement had just finished.
            Err(mysql_async::Error::Server(e)) if e.code == 1094 => Ok(true),
            Err(e) => Err(e.into()),
            Ok(()) => Ok(true),
        }
    }

    /// Runs one query and hands its columns, then each row, to the
    /// callbacks, without keeping rows in memory. For exports; can be
    /// stopped with [`Session::cancel`]. Returns the number of rows.
    pub async fn stream_rows(
        &self,
        sql: &str,
        on_columns: impl FnOnce(&[ColumnInfo]) -> std::io::Result<()>,
        on_row: impl FnMut(Vec<serde_json::Value>) -> std::io::Result<()>,
    ) -> Result<u64> {
        let mut guard = self.link.lock().await;
        let _busy = self.start_run();
        let streamed = self.stream_on(&mut guard, sql, on_columns, on_row).await;
        match streamed {
            Err(_) if self.cancelled.load(Ordering::SeqCst) => Err(CoreError::Cancelled),
            other => other,
        }
    }

    async fn stream_on(
        &self,
        link: &mut Option<Link>,
        sql: &str,
        on_columns: impl FnOnce(&[ColumnInfo]) -> std::io::Result<()>,
        mut on_row: impl FnMut(Vec<serde_json::Value>) -> std::io::Result<()>,
    ) -> Result<u64> {
        let (conn, _) = self.live(link).await?;
        let mut result = conn.query_iter(sql).await?;
        on_columns(&column_infos(&result))?;
        let mut count = 0u64;
        let mut failed: Option<std::io::Error> = None;
        result
            .for_each(|row: Row| {
                if failed.is_none() {
                    count += 1;
                    if let Err(e) = on_row(row.unwrap().into_iter().map(to_json).collect()) {
                        failed = Some(e);
                    }
                }
            })
            .await?;
        // Any further result sets must be read before the connection is reused.
        result.drop_result().await?;
        match failed {
            Some(e) => Err(e.into()),
            None => Ok(count),
        }
    }

    /// Databases visible to this user, with their table counts.
    pub async fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        let rows: Vec<(String, u64)> = conn
            .query(
                "SELECT s.SCHEMA_NAME, COUNT(t.TABLE_NAME)
                 FROM information_schema.SCHEMATA s
                 LEFT JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = s.SCHEMA_NAME
                 GROUP BY s.SCHEMA_NAME
                 ORDER BY s.SCHEMA_NAME",
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|(name, table_count)| DatabaseInfo { name, table_count })
            .collect())
    }

    /// Tables and views in `database`.
    pub async fn list_tables(&self, database: &str) -> Result<Vec<TableInfo>> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        type Row = (
            String,
            String,
            Option<String>,
            Option<u64>,
            Option<u64>,
            String,
        );
        let rows: Vec<Row> = conn
            .exec(
                "SELECT TABLE_NAME, TABLE_TYPE, ENGINE, TABLE_ROWS,
                        DATA_LENGTH + INDEX_LENGTH, TABLE_COMMENT
                 FROM information_schema.TABLES
                 WHERE TABLE_SCHEMA = ?
                 ORDER BY TABLE_NAME",
                (database,),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(name, kind, engine, rows_estimate, size_bytes, comment)| TableInfo {
                    name,
                    kind,
                    engine,
                    rows_estimate,
                    size_bytes,
                    comment,
                },
            )
            .collect())
    }

    /// `SHOW CREATE TABLE` for `table` (a view works too), in `database` or
    /// the current one.
    pub async fn show_create_table(&self, database: Option<&str>, table: &str) -> Result<String> {
        let quote = |n: &str| format!("`{}`", n.replace('`', "``"));
        let name = match database {
            Some(db) => format!("{}.{}", quote(db), quote(table)),
            None => quote(table),
        };
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        let row: Option<mysql_async::Row> = conn
            .query_first(format!("SHOW CREATE TABLE {name}"))
            .await?;
        Ok(row.and_then(|r| r.get::<String, _>(1)).unwrap_or_default())
    }

    /// Switches to `database`, as `USE` would. It stays selected after an
    /// automatic reconnect.
    pub async fn use_database(&self, database: &str) -> Result<()> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        conn.query_drop(format!("USE `{}`", database.replace('`', "``")))
            .await?;
        *self.current_db.lock().unwrap() = Some(database.to_string());
        Ok(())
    }

    /// Table names of every user database, as `(database, table)` pairs.
    /// Cheap enough to load once per connection for autocomplete.
    pub async fn list_all_tables(&self) -> Result<Vec<(String, String)>> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        Ok(conn
            .query(
                "SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.TABLES
                 WHERE TABLE_SCHEMA NOT IN ('information_schema', 'performance_schema')
                 ORDER BY TABLE_SCHEMA, TABLE_NAME",
            )
            .await?)
    }

    /// Columns of every table in `database`, in table and column order.
    pub async fn list_columns(&self, database: &str) -> Result<Vec<ColumnDef>> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        let rows: Vec<(String, String, String, String, String, String)> = conn
            .exec(
                "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, COLUMN_KEY, IS_NULLABLE,
                        COLUMN_COMMENT
                 FROM information_schema.COLUMNS
                 WHERE TABLE_SCHEMA = ?
                 ORDER BY TABLE_NAME, ORDINAL_POSITION",
                (database,),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(table, name, column_type, key, nullable, comment)| ColumnDef {
                    table,
                    name,
                    column_type,
                    key,
                    nullable: nullable == "YES",
                    comment,
                },
            )
            .collect())
    }

    /// Runs one statement and returns its column names and rows as text.
    /// Meant for SHOW statements and small admin queries, not user SQL.
    pub async fn query_text(&self, sql: &str) -> Result<TextRows> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        let mut result = conn.query_iter(sql).await?;
        let columns = result
            .columns_ref()
            .iter()
            .map(|c| c.name_str().into_owned())
            .collect();
        let rows = result
            .map(|row: Row| row.unwrap().into_iter().map(text_cell).collect())
            .await?;
        Ok(TextRows { columns, rows })
    }

    /// Like [`Session::query_text`], with `?` placeholders bound to
    /// `params`. For information_schema lookups by name.
    pub async fn query_params(&self, sql: &str, params: &[&str]) -> Result<TextRows> {
        let mut guard = self.link.lock().await;
        let (conn, _) = self.live(&mut guard).await?;
        let params: Vec<Value> = params.iter().map(|p| Value::from(*p)).collect();
        let mut result = conn.exec_iter(sql, params).await?;
        let columns = result
            .columns_ref()
            .iter()
            .map(|c| c.name_str().into_owned())
            .collect();
        let rows = result
            .map(|row: Row| row.unwrap().into_iter().map(text_cell).collect())
            .await?;
        Ok(TextRows { columns, rows })
    }

    /// Runs schema changes one statement at a time, each sent whole so a
    /// routine body needs no `DELIMITER`. With `database`, they run there
    /// and the database selected before is put back afterwards (when there
    /// was none, `database` stays selected). Stops at the first error.
    pub async fn run_ddl(&self, database: Option<&str>, statements: &[String]) -> ExecutionOutcome {
        let started_at = Utc::now();
        let timer = Instant::now();
        let mut results = Vec::new();
        let mut failed_statement = None;
        let mut reconnected = false;
        let mut guard = self.link.lock().await;
        let _busy = self.start_run();

        let error = match self.live(&mut guard).await {
            Err(e) => Some(e),
            Ok((conn, r)) => {
                reconnected = r;
                let before = self.current_database();
                let quote = |n: &str| format!("USE `{}`", n.replace('`', "``"));
                let mut error = None;
                if let Some(db) = database
                    && before.as_deref() != Some(db)
                    && let Err(e) = conn.query_drop(quote(db)).await
                {
                    error = Some(e.into());
                }
                if error.is_none() {
                    for sql in statements {
                        let step = Instant::now();
                        match conn.query_iter(sql.as_str()).await {
                            Ok(result) => {
                                let stats = (
                                    result.affected_rows(),
                                    result.warnings(),
                                    result.info().into_owned(),
                                );
                                if let Err(e) = result.drop_result().await {
                                    failed_statement = Some(sql.clone());
                                    error = Some(e.into());
                                    break;
                                }
                                results.push(StatementResult {
                                    sql: sql.clone(),
                                    duration_ms: step.elapsed().as_secs_f64() * 1000.0,
                                    columns: Vec::new(),
                                    rows: Vec::new(),
                                    total_rows: 0,
                                    truncated: false,
                                    affected_rows: stats.0,
                                    last_insert_id: None,
                                    warnings: stats.1,
                                    info: stats.2,
                                    limit_added: None,
                                });
                            }
                            Err(e) => {
                                failed_statement = Some(sql.clone());
                                error = Some(e.into());
                                break;
                            }
                        }
                    }
                }
                if let Some(db) = database {
                    match before.as_deref() {
                        Some(prev) if prev != db => {
                            let _ = conn.query_drop(quote(prev)).await;
                        }
                        Some(_) => {}
                        None => *self.current_db.lock().unwrap() = Some(db.to_string()),
                    }
                }
                error
            }
        };
        ExecutionOutcome {
            started_at,
            duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
            results,
            error,
            failed_statement,
            reconnected,
            cancelled: false,
        }
    }

    /// Runs `statements` in order, stopping at the first error. Returns how
    /// many succeeded along with that error. Nothing is recorded.
    pub async fn run_each(&self, statements: &[String]) -> (usize, Option<CoreError>) {
        let mut guard = self.link.lock().await;
        let conn = match self.live(&mut guard).await {
            Ok((conn, _)) => conn,
            Err(e) => return (0, Some(e)),
        };
        for (i, sql) in statements.iter().enumerate() {
            if let Err(e) = conn.query_drop(sql.as_str()).await {
                return (i, Some(e.into()));
            }
        }
        (statements.len(), None)
    }

    pub async fn close(self) -> Result<()> {
        if let Some(link) = self.link.into_inner() {
            link.conn.disconnect().await?;
        }
        Ok(())
    }
}

/// An open connection of either kind, as the app keeps them.
#[derive(Clone)]
pub enum AnySession {
    Mysql(std::sync::Arc<Session>),
    Redis(std::sync::Arc<crate::redis::RedisSession>),
}

impl AnySession {
    pub fn info(&self) -> &SessionInfo {
        match self {
            AnySession::Mysql(s) => s.info(),
            AnySession::Redis(s) => s.info(),
        }
    }

    /// Round trip to the server in milliseconds; `None` while busy.
    pub async fn ping(&self) -> Result<Option<f64>> {
        match self {
            AnySession::Mysql(s) => s.ping().await,
            AnySession::Redis(s) => s.ping().await,
        }
    }

    /// Disconnects, unless a call still holds the session.
    pub async fn close(self) -> Result<()> {
        match self {
            AnySession::Mysql(s) => match std::sync::Arc::try_unwrap(s) {
                Ok(s) => s.close().await,
                Err(_) => Ok(()),
            },
            AnySession::Redis(s) => {
                if let Ok(s) = std::sync::Arc::try_unwrap(s) {
                    s.close().await;
                }
                Ok(())
            }
        }
    }
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn column_infos<P: mysql_async::prelude::Protocol>(
    result: &mysql_async::QueryResult<'_, '_, P>,
) -> Vec<ColumnInfo> {
    result
        .columns_ref()
        .iter()
        .map(|c| ColumnInfo {
            name: c.name_str().into_owned(),
            table: c.table_str().into_owned(),
            type_name: format!("{:?}", c.column_type())
                .trim_start_matches("MYSQL_TYPE_")
                .to_string(),
        })
        .collect()
}

/// Opens the tunnel (if any) and the MySQL connection. `database` overrides
/// the profile's default database.
async fn open_link(
    profile: &ConnectionProfile,
    database: Option<String>,
) -> Result<(Link, SessionInfo)> {
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
        .db_name(
            database
                .or_else(|| profile.database.clone())
                .filter(|d| !d.is_empty()),
        )
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
    Ok((
        Link {
            conn,
            _tunnel: tunnel,
        },
        info,
    ))
}

/// A cell as text, as [`Session::query_text`] returns it.
fn text_cell(v: Value) -> Option<String> {
    match v {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(&b).into_owned()),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

/// Whether a statement (as split, so line comments are already gone) is
/// `USE db`, possibly after block comments.
fn is_use(stmt: &str) -> bool {
    let mut rest = stmt.trim_start();
    while let Some(after) = rest.strip_prefix("/*") {
        match after.find("*/") {
            Some(end) => rest = after[end + 2..].trim_start(),
            None => return false,
        }
    }
    rest.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("use"))
        && rest[3..].starts_with(|c: char| c.is_whitespace() || c == '`')
}

async fn run(
    conn: &mut Conn,
    sql: &str,
    max_rows: usize,
    auto_limit: Option<u64>,
    cancelled: &AtomicBool,
    out: &mut Vec<StatementResult>,
    failed: &mut Option<String>,
) -> Result<()> {
    // Statements run one at a time so each gets its own timing and stats,
    // and an error points at the statement that caused it.
    for stmt in split_statements(sql) {
        *failed = Some(stmt.clone());
        if cancelled.load(Ordering::SeqCst) {
            return Err(CoreError::Cancelled);
        }
        let timer = Instant::now();
        let limited = auto_limit.and_then(|n| crate::advisor::add_limit(&stmt, n));
        let mut result = conn
            .query_iter(limited.as_deref().unwrap_or(stmt.as_str()))
            .await?;
        // A statement can still yield several result sets (e.g. CALL).
        loop {
            let columns = column_infos(&result);
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
                limit_added: limited.as_ref().and(auto_limit),
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
    fn recognizes_use() {
        for yes in [
            "USE shop",
            "use `my db`",
            "  Use\nshop",
            "/* go */ USE shop",
            "use\tshop",
        ] {
            assert!(is_use(yes), "{yes}");
        }
        for no in [
            "SELECT 1",
            "user_id",
            "USER()",
            "/* use x */ SELECT 1",
            "used",
        ] {
            assert!(!is_use(no), "{no}");
        }
    }

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
