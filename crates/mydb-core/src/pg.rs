//! PostgreSQL connections.
//!
//! A [`PgSession`] behaves like the MySQL [`crate::Session`]: it reconnects
//! by itself (re-opening the SSH tunnel), can be cancelled, and returns the
//! same result types, so the editor, records and reports don't need to know
//! which server they talk to.
//!
//! PostgreSQL connects to one database. What the UI calls a "database" on a
//! PostgreSQL connection is a schema of that database: choosing one puts it
//! first in `search_path`, which is kept across reconnects.
//!
//! Statements run one at a time. Each is first prepared (which refuses a
//! text holding more than one statement, and gives the column types), then
//! run with the simple protocol so every value comes back as text, the way
//! `psql` shows it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_postgres::types::Type;
use tokio_postgres::{AsyncMessage, CancelToken, Client, NoTls, SimpleQueryMessage};

use crate::error::{CoreError, Result};
use crate::profile::{ConnectionProfile, SslMode};
use crate::session::{
    ColumnDef, ColumnInfo, DatabaseInfo, ExecutionOutcome, SessionInfo, StatementResult, TableInfo,
    TextRows,
};
use crate::split::split_statements_pg;
use crate::tunnel::SshTunnel;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Schemas PostgreSQL keeps for itself, hidden from the sidebar.
const SYSTEM_SCHEMAS: &str = "n.nspname NOT IN ('information_schema', 'pg_catalog', 'pg_toast') AND n.nspname NOT LIKE 'pg\\_temp\\_%' AND n.nspname NOT LIKE 'pg\\_toast\\_temp\\_%'";

pub struct PgSession {
    profile: ConnectionProfile,
    link: Mutex<Option<Link>>,
    info: SessionInfo,
    /// `search_path` as last seen, put back after a reconnect.
    search_path: StdMutex<Option<String>>,
    /// `current_schema()` as last seen.
    schema: StdMutex<Option<String>>,
    /// How to reach the server for a cancel request, while a link is open.
    canceller: StdMutex<Option<Canceller>>,
    busy: AtomicBool,
    cancelled: AtomicBool,
}

struct Link {
    client: Client,
    /// NOTICE and WARNING messages since the last statement started.
    notices: Arc<StdMutex<Vec<String>>>,
    driver: JoinHandle<()>,
    // Dropped after the client, so the tunnel outlives the connection.
    _tunnel: Option<SshTunnel>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

#[derive(Clone)]
struct Canceller {
    token: CancelToken,
    host: String,
    port: u16,
    tls: Option<RustlsConnect>,
}

impl PgSession {
    pub async fn connect(profile: &ConnectionProfile) -> Result<Self> {
        let (link, info, canceller) = open_link(profile).await?;
        let session = Self {
            profile: profile.with_pinned_host_key(info.ssh_host_fingerprint.as_deref()),
            link: Mutex::new(Some(link)),
            info,
            search_path: StdMutex::new(None),
            schema: StdMutex::new(None),
            canceller: StdMutex::new(Some(canceller)),
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        };
        {
            let mut guard = session.link.lock().await;
            let client = &guard.as_mut().unwrap().client;
            session.refresh_path(client).await;
        }
        Ok(session)
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    pub fn profile(&self) -> &ConnectionProfile {
        &self.profile
    }

    /// The database this connection is on.
    pub fn database_name(&self) -> String {
        self.profile
            .database
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| "postgres".into())
    }

    /// The schema in use (first existing one in `search_path`).
    pub fn current_database(&self) -> Option<String> {
        self.schema.lock().unwrap().clone()
    }

    /// `search_path` as last seen.
    pub fn search_path(&self) -> Option<String> {
        self.search_path.lock().unwrap().clone()
    }

    async fn refresh_path(&self, client: &Client) {
        if let Ok(rows) = client
            .query_one(
                "SELECT current_schema()::text, current_setting('search_path')",
                &[],
            )
            .await
        {
            *self.schema.lock().unwrap() = rows.get::<_, Option<String>>(0);
            *self.search_path.lock().unwrap() = rows.get::<_, Option<String>>(1);
        }
    }

    /// A live client, reconnecting if the old connection is gone. The bool
    /// is true when a reconnect happened.
    async fn live<'a>(&self, link: &'a mut Option<Link>) -> Result<(&'a mut Link, bool)> {
        let alive = match link.as_ref() {
            Some(l) => !l.client.is_closed() && l.client.simple_query("").await.is_ok(),
            None => false,
        };
        if !alive {
            *link = None;
            *self.canceller.lock().unwrap() = None;
            let (new_link, _, canceller) = open_link(&self.profile).await?;
            let path = self.search_path.lock().unwrap().clone();
            if let Some(path) = path {
                let _ = new_link
                    .client
                    .execute("SELECT set_config('search_path', $1, false)", &[&path])
                    .await;
            }
            *self.canceller.lock().unwrap() = Some(canceller);
            *link = Some(new_link);
        }
        Ok((link.as_mut().unwrap(), !alive))
    }

    /// Round trip of an empty query, for the status bar; `None` while busy.
    pub async fn ping(&self) -> Result<Option<f64>> {
        if self.busy.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let Ok(guard) = self.link.try_lock() else {
            return Ok(None);
        };
        let Some(l) = guard.as_ref() else {
            return Err(CoreError::Connection {
                message: "連線已中斷".into(),
            });
        };
        let timer = Instant::now();
        l.client.simple_query("").await?;
        Ok(Some(timer.elapsed().as_secs_f64() * 1000.0))
    }

    /// `idle_session_timeout` in seconds, when the server sets one.
    pub async fn idle_timeout(&self) -> Result<Option<u64>> {
        let rows = self
            .query_text("SELECT setting FROM pg_settings WHERE name = 'idle_session_timeout'")
            .await?;
        Ok(rows
            .rows
            .first()
            .and_then(|r| r.first().cloned().flatten())
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map(|ms| ms / 1000))
    }

    pub async fn execute(&self, sql: &str, max_rows: usize) -> ExecutionOutcome {
        self.execute_limited(sql, max_rows, None).await
    }

    /// Runs `sql` statement by statement, adding `LIMIT auto_limit` to
    /// queries without one.
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
            Ok((link, r)) => {
                reconnected = r;
                let mut error = None;
                let statements = split_statements_pg(sql);
                for stmt in &statements {
                    failed_statement = Some(stmt.clone());
                    if self.cancelled.load(Ordering::SeqCst) {
                        error = Some(CoreError::Cancelled);
                        break;
                    }
                    let limited = auto_limit.and_then(|n| crate::advisor::add_limit(stmt, n));
                    let text = limited.as_deref().unwrap_or(stmt);
                    match run_one(link, text, max_rows).await {
                        Ok(mut r) => {
                            r.sql = stmt.clone();
                            r.limit_added = limited.as_ref().and(auto_limit);
                            results.push(r);
                        }
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                }
                if error.is_none() {
                    failed_statement = None;
                }
                if statements.iter().any(|s| touches_path(s)) {
                    self.refresh_path(&link.client).await;
                }
                error
            }
        };
        let cancelled = self.cancelled.load(Ordering::SeqCst);
        ExecutionOutcome {
            started_at,
            duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
            results,
            error: match error {
                // The server's "canceling statement due to user request".
                Some(CoreError::Postgres { state, .. }) if cancelled && state == "57014" => {
                    Some(CoreError::Cancelled)
                }
                other => other,
            },
            failed_statement,
            reconnected,
            cancelled,
        }
    }

    fn start_run(&self) -> BusyGuard<'_> {
        self.cancelled.store(false, Ordering::SeqCst);
        self.busy.store(true, Ordering::SeqCst);
        BusyGuard(&self.busy)
    }

    /// Asks the server to stop the running statement (PostgreSQL's cancel
    /// request, over a new connection through the same tunnel). Later
    /// statements of the run are skipped.
    pub async fn cancel(&self) -> Result<bool> {
        if !self.busy.load(Ordering::SeqCst) {
            return Ok(false);
        }
        self.cancelled.store(true, Ordering::SeqCst);
        let Some(c) = self.canceller.lock().unwrap().clone() else {
            return Ok(true);
        };
        let stream = tcp(&c.host, c.port).await?;
        match c.tls {
            Some(tls) => c.token.cancel_query_raw(stream, tls).await?,
            None => c.token.cancel_query_raw(stream, NoTls).await?,
        }
        Ok(true)
    }

    /// Runs one query and hands its columns, then each row, to the
    /// callbacks without keeping rows in memory. For exports.
    pub async fn stream_rows(
        &self,
        sql: &str,
        on_columns: impl FnOnce(&[ColumnInfo]) -> std::io::Result<()>,
        mut on_row: impl FnMut(Vec<serde_json::Value>) -> std::io::Result<()>,
    ) -> Result<u64> {
        let mut guard = self.link.lock().await;
        let _busy = self.start_run();
        let streamed = async {
            let (link, _) = self.live(&mut guard).await?;
            // Refuses more than one statement.
            let prepared = link.client.prepare(sql).await?;
            let types: Vec<Type> = prepared
                .columns()
                .iter()
                .map(|c| c.type_().clone())
                .collect();
            on_columns(&column_infos(&prepared))?;
            let stream = link.client.simple_query_raw(sql).await?;
            futures_util::pin_mut!(stream);
            let mut count = 0u64;
            while let Some(msg) = stream.next().await {
                if let SimpleQueryMessage::Row(row) = msg? {
                    count += 1;
                    on_row(row_json(&row, &types))?;
                }
            }
            Ok::<u64, CoreError>(count)
        }
        .await;
        match streamed {
            Err(_) if self.cancelled.load(Ordering::SeqCst) => Err(CoreError::Cancelled),
            other => other,
        }
    }

    /// Schemas of this database the user can use, with table counts.
    pub async fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let rows = self
            .query(
                &format!(
                    "SELECT n.nspname::text, COUNT(c.oid)
                     FROM pg_namespace n
                     LEFT JOIN pg_class c ON c.relnamespace = n.oid
                          AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition
                     WHERE {SYSTEM_SCHEMAS} AND has_schema_privilege(n.oid, 'USAGE')
                     GROUP BY n.nspname ORDER BY n.nspname"
                ),
                &[],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| DatabaseInfo {
                name: r.get(0),
                table_count: r.get::<_, i64>(1) as u64,
            })
            .collect())
    }

    /// Databases on the server this user may connect to, for the
    /// connection form.
    pub async fn list_server_databases(&self) -> Result<Vec<String>> {
        let rows = self
            .query(
                "SELECT datname::text FROM pg_database
                 WHERE NOT datistemplate AND has_database_privilege(datname, 'CONNECT')
                 ORDER BY datname",
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    /// Tables, views, materialized views and foreign tables in `schema`.
    pub async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>> {
        let rows = self
            .query(
                "SELECT c.relname::text,
                        CASE c.relkind WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW'
                             WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'BASE TABLE' END,
                        CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END,
                        CASE WHEN c.relkind IN ('r', 'p', 'm') THEN pg_total_relation_size(c.oid) END,
                        COALESCE(obj_description(c.oid, 'pg_class'), '')
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
                       AND NOT c.relispartition
                 ORDER BY c.relname",
                &[&schema],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| TableInfo {
                name: r.get(0),
                kind: r.get(1),
                engine: None,
                rows_estimate: r.get::<_, Option<i64>>(2).map(|n| n.max(0) as u64),
                size_bytes: r.get::<_, Option<i64>>(3).map(|n| n.max(0) as u64),
                comment: r.get(4),
            })
            .collect())
    }

    /// `(schema, table)` for every user table, for autocomplete.
    pub async fn list_all_tables(&self) -> Result<Vec<(String, String)>> {
        let rows = self
            .query(
                &format!(
                    "SELECT n.nspname::text, c.relname::text
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                     WHERE {SYSTEM_SCHEMAS} AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
                           AND NOT c.relispartition
                     ORDER BY 1, 2"
                ),
                &[],
            )
            .await?;
        Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    /// Columns of every table in `schema`, in table and column order.
    pub async fn list_columns(&self, schema: &str) -> Result<Vec<ColumnDef>> {
        let rows = self
            .query(
                "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod),
                        CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid
                                          AND i.indisprimary AND a.attnum = ANY(i.indkey)) THEN 'PRI'
                             WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid
                                          AND i.indisunique AND i.indkey[0] = a.attnum) THEN 'UNI'
                             WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid
                                          AND i.indkey[0] = a.attnum) THEN 'MUL'
                             ELSE '' END,
                        NOT a.attnotnull,
                        COALESCE(col_description(c.oid, a.attnum), '')
                 FROM pg_attribute a
                 JOIN pg_class c ON c.oid = a.attrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
                       AND NOT c.relispartition AND a.attnum > 0 AND NOT a.attisdropped
                 ORDER BY c.relname, a.attnum",
                &[&schema],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| ColumnDef {
                table: r.get(0),
                name: r.get(1),
                column_type: r.get(2),
                key: r.get(3),
                nullable: r.get(4),
                comment: r.get(5),
            })
            .collect())
    }

    /// Puts `schema` first in `search_path` (keeping `public` after it),
    /// the PostgreSQL counterpart of `USE`.
    pub async fn use_database(&self, schema: &str) -> Result<()> {
        let mut path = quote_ident(schema);
        if schema != "public" {
            path.push_str(", public");
        }
        let mut guard = self.link.lock().await;
        let (link, _) = self.live(&mut guard).await?;
        link.client
            .execute("SELECT set_config('search_path', $1, false)", &[&path])
            .await?;
        self.refresh_path(&link.client).await;
        Ok(())
    }

    /// A `CREATE TABLE` (or `CREATE VIEW`) statement rebuilt from the
    /// catalog: PostgreSQL has no `SHOW CREATE TABLE`. Columns, defaults,
    /// identity, constraints, other indexes and comments.
    pub async fn show_create_table(&self, schema: Option<&str>, table: &str) -> Result<String> {
        let found = match schema {
            Some(s) => {
                self.query(
                    "SELECT c.oid, c.relkind::text, quote_ident(n.nspname) || '.' || quote_ident(c.relname)
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                     WHERE n.nspname = $1 AND c.relname = $2",
                    &[&s, &table],
                )
                .await?
            }
            None => {
                self.query(
                    "SELECT c.oid, c.relkind::text, quote_ident(n.nspname) || '.' || quote_ident(c.relname)
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                     WHERE c.oid = to_regclass(quote_ident($1))",
                    &[&table],
                )
                .await?
            }
        };
        let Some(row) = found.first() else {
            return Ok(String::new());
        };
        let oid: u32 = row.get(0);
        let kind: String = row.get(1);
        let name: String = row.get(2);

        if kind == "v" || kind == "m" {
            let def = self
                .query("SELECT pg_get_viewdef($1::oid, true)", &[&oid])
                .await?;
            let body: String = def.first().map(|r| r.get(0)).unwrap_or_default();
            let what = if kind == "m" {
                "MATERIALIZED VIEW"
            } else {
                "VIEW"
            };
            return Ok(format!(
                "CREATE {what} {name} AS\n{}",
                body.trim_end().trim_end_matches(';')
            ) + ";");
        }

        let mut lines = Vec::new();
        for r in self
            .query(
                "SELECT quote_ident(a.attname), format_type(a.atttypid, a.atttypmod),
                        a.attnotnull, pg_get_expr(d.adbin, d.adrelid), a.attidentity::text,
                        a.attgenerated::text
                 FROM pg_attribute a
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                 WHERE a.attrelid = $1::oid AND a.attnum > 0 AND NOT a.attisdropped
                 ORDER BY a.attnum",
                &[&oid],
            )
            .await?
        {
            let mut line = format!("  {} {}", r.get::<_, String>(0), r.get::<_, String>(1));
            let default: Option<String> = r.get(3);
            let identity: String = r.get(4);
            let generated: String = r.get(5);
            match (identity.as_str(), generated.as_str(), default) {
                ("a", _, _) => line.push_str(" GENERATED ALWAYS AS IDENTITY"),
                ("d", _, _) => line.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
                (_, "s", Some(expr)) => {
                    line.push_str(&format!(" GENERATED ALWAYS AS ({expr}) STORED"))
                }
                (_, _, Some(expr)) => line.push_str(&format!(" DEFAULT {expr}")),
                _ => {}
            }
            if r.get::<_, bool>(2) {
                line.push_str(" NOT NULL");
            }
            lines.push(line);
        }
        for r in self
            .query(
                "SELECT quote_ident(conname), pg_get_constraintdef(oid, true)
                 FROM pg_constraint WHERE conrelid = $1::oid AND contype <> 'n'
                 ORDER BY CASE contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'f' THEN 2 ELSE 3 END, conname",
                &[&oid],
            )
            .await?
        {
            lines.push(format!(
                "  CONSTRAINT {} {}",
                r.get::<_, String>(0),
                r.get::<_, String>(1)
            ));
        }
        let mut out = format!("CREATE TABLE {name} (\n{}\n);", lines.join(",\n"));
        for r in self
            .query(
                "SELECT pg_get_indexdef(i.indexrelid) FROM pg_index i
                 WHERE i.indrelid = $1::oid
                   AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid
                                   AND c.conrelid = i.indrelid)
                 ORDER BY i.indexrelid",
                &[&oid],
            )
            .await?
        {
            out.push_str(&format!("\n{};", r.get::<_, String>(0)));
        }
        for r in self
            .query(
                "SELECT NULL::text, obj_description($1::oid, 'pg_class')
                 UNION ALL
                 SELECT quote_ident(a.attname), col_description($1::oid, a.attnum)
                 FROM pg_attribute a
                 WHERE a.attrelid = $1::oid AND a.attnum > 0 AND NOT a.attisdropped",
                &[&oid],
            )
            .await?
        {
            let Some(comment) = r.get::<_, Option<String>>(1) else {
                continue;
            };
            let lit = quote_literal(&comment);
            match r.get::<_, Option<String>>(0) {
                None => out.push_str(&format!("\nCOMMENT ON TABLE {name} IS {lit};")),
                Some(col) => out.push_str(&format!("\nCOMMENT ON COLUMN {name}.{col} IS {lit};")),
            }
        }
        Ok(out)
    }

    /// Runs one statement and returns its columns and rows as text. For the
    /// app's own catalog queries, not user SQL.
    pub async fn query_text(&self, sql: &str) -> Result<TextRows> {
        let mut guard = self.link.lock().await;
        let (link, _) = self.live(&mut guard).await?;
        simple_text(&link.client, sql).await
    }

    /// Runs `statements` as one transaction: all of them, or none if one
    /// fails (PostgreSQL can undo schema changes too). Nothing is recorded.
    pub async fn run_in_transaction(&self, statements: &[String]) -> ExecutionOutcome {
        let started_at = Utc::now();
        let timer = Instant::now();
        let mut results = Vec::new();
        let mut failed_statement = None;
        let mut reconnected = false;
        let mut guard = self.link.lock().await;
        let _busy = self.start_run();
        let error = match self.live(&mut guard).await {
            Err(e) => Some(e),
            Ok((link, r)) => {
                reconnected = r;
                let mut error = None;
                if let Err(e) = link.client.batch_execute("BEGIN").await {
                    error = Some(e.into());
                }
                if error.is_none() {
                    for sql in statements {
                        let step = Instant::now();
                        match link.client.simple_query(sql).await {
                            Ok(msgs) => results.push(StatementResult {
                                sql: sql.clone(),
                                duration_ms: step.elapsed().as_secs_f64() * 1000.0,
                                columns: Vec::new(),
                                rows: Vec::new(),
                                total_rows: 0,
                                truncated: false,
                                affected_rows: msgs
                                    .iter()
                                    .filter_map(|m| match m {
                                        SimpleQueryMessage::CommandComplete(n) => Some(*n),
                                        _ => None,
                                    })
                                    .sum(),
                                last_insert_id: None,
                                warnings: 0,
                                info: take_notices(link),
                                limit_added: None,
                            }),
                            Err(e) => {
                                failed_statement = Some(sql.clone());
                                error = Some(e.into());
                                break;
                            }
                        }
                    }
                    let end = if error.is_some() {
                        "ROLLBACK"
                    } else {
                        "COMMIT"
                    };
                    if let Err(e) = link.client.batch_execute(end).await
                        && error.is_none()
                    {
                        error = Some(e.into());
                    }
                }
                self.refresh_path(&link.client).await;
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

    /// Typed query over the live connection, for the catalog lookups here.
    async fn query(
        &self,
        sql: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Vec<tokio_postgres::Row>> {
        let mut guard = self.link.lock().await;
        let (link, _) = self.live(&mut guard).await?;
        Ok(link.client.query(sql, params).await?)
    }

    pub async fn close(self) {
        // Dropping the link ends the connection and the tunnel.
        drop(self.link.into_inner());
    }
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Prepares (to check it is one statement and learn its columns), then
/// runs `sql` with the simple protocol, keeping `max_rows` rows.
async fn run_one(link: &mut Link, sql: &str, max_rows: usize) -> Result<StatementResult> {
    if is_copy_stdio(sql) {
        return Err(CoreError::Config {
            message:
                "編輯器不支援 COPY … FROM STDIN / TO STDOUT。請改用 \\copy（psql）或匯出功能。"
                    .into(),
        });
    }
    take_notices(link);
    let timer = Instant::now();
    let prepared = link.client.prepare(sql).await?;
    let columns = column_infos(&prepared);
    let types: Vec<Type> = prepared
        .columns()
        .iter()
        .map(|c| c.type_().clone())
        .collect();
    let stream = link.client.simple_query_raw(sql).await?;
    futures_util::pin_mut!(stream);
    let mut rows = Vec::new();
    let mut total_rows = 0u64;
    let mut affected = 0u64;
    while let Some(msg) = stream.next().await {
        match msg? {
            SimpleQueryMessage::Row(row) => {
                total_rows += 1;
                if rows.len() < max_rows {
                    rows.push(row_json(&row, &types));
                }
            }
            SimpleQueryMessage::CommandComplete(n) => affected = n,
            _ => {}
        }
    }
    Ok(StatementResult {
        sql: sql.to_string(),
        duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
        truncated: total_rows > rows.len() as u64,
        rows,
        total_rows,
        // For a query the count is the rows returned, not changed.
        affected_rows: if columns.is_empty() { affected } else { 0 },
        columns,
        last_insert_id: None,
        warnings: 0,
        info: take_notices(link),
        limit_added: None,
    })
}

fn take_notices(link: &Link) -> String {
    std::mem::take(&mut *link.notices.lock().unwrap()).join("\n")
}

async fn simple_text(client: &Client, sql: &str) -> Result<TextRows> {
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    for msg in client.simple_query(sql).await? {
        match msg {
            SimpleQueryMessage::RowDescription(cols) if columns.is_empty() => {
                columns = cols.iter().map(|c| c.name().to_string()).collect();
            }
            SimpleQueryMessage::Row(r) => {
                rows.push((0..r.len()).map(|i| r.get(i).map(str::to_string)).collect());
            }
            _ => {}
        }
    }
    Ok(TextRows { columns, rows })
}

fn column_infos(stmt: &tokio_postgres::Statement) -> Vec<ColumnInfo> {
    stmt.columns()
        .iter()
        .map(|c| ColumnInfo {
            name: c.name().to_string(),
            table: String::new(),
            type_name: c.type_().name().to_string(),
        })
        .collect()
}

/// A row as JSON cells: integers and floats become numbers (big integers
/// stay text, as for MySQL), everything else stays as PostgreSQL prints it.
fn row_json(row: &tokio_postgres::SimpleQueryRow, types: &[Type]) -> Vec<serde_json::Value> {
    (0..row.len())
        .map(|i| match row.get(i) {
            None => serde_json::Value::Null,
            Some(text) => cell_json(text, types.get(i)),
        })
        .collect()
}

fn cell_json(text: &str, ty: Option<&Type>) -> serde_json::Value {
    const SAFE: i64 = (1 << 53) - 1;
    match ty {
        Some(t) if *t == Type::INT2 || *t == Type::INT4 || *t == Type::INT8 || *t == Type::OID => {
            match text.parse::<i64>() {
                Ok(n) if (-SAFE..=SAFE).contains(&n) => n.into(),
                _ => text.into(),
            }
        }
        Some(t) if *t == Type::FLOAT4 || *t == Type::FLOAT8 => match text.parse::<f64>() {
            Ok(f) if f.is_finite() => f.into(),
            _ => text.into(),
        },
        Some(t) if *t == Type::BOOL => match text {
            "t" => "true".into(),
            "f" => "false".into(),
            other => other.into(),
        },
        _ => text.into(),
    }
}

/// Whether a statement may change `search_path` (so it is read again).
fn touches_path(stmt: &str) -> bool {
    let up = stmt.to_ascii_uppercase();
    up.contains("SEARCH_PATH")
        || up.contains("SCHEMA")
        || up.trim_start().starts_with("RESET")
        || up.trim_start().starts_with("DISCARD")
        || up.trim_start().starts_with("ROLLBACK")
}

fn is_copy_stdio(stmt: &str) -> bool {
    let up = stmt.to_ascii_uppercase();
    up.trim_start().starts_with("COPY")
        && up
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|w| w == "STDIN" || w == "STDOUT")
}

/// `"name"`, quoted the way PostgreSQL needs it.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `'text'` as a standard SQL string literal.
pub fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

async fn tcp(host: &str, port: u16) -> Result<TcpStream> {
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port))).await {
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            Ok(s)
        }
        Ok(Err(e)) => Err(CoreError::Connection {
            message: format!("{host}:{port}: {e}"),
        }),
        Err(_) => Err(CoreError::Connection {
            message: format!("{host}:{port}: 連線逾時"),
        }),
    }
}

/// Opens the tunnel (if any) and the connection, and starts the task that
/// drives it.
async fn open_link(profile: &ConnectionProfile) -> Result<(Link, SessionInfo, Canceller)> {
    let tunnel = match &profile.ssh {
        Some(ssh) => Some(SshTunnel::open(ssh, &profile.host, profile.port).await?),
        None => None,
    };
    let (host, port) = match &tunnel {
        Some(t) => ("127.0.0.1".to_string(), t.local_port()),
        None => (profile.host.clone(), profile.port),
    };

    let mut cfg = tokio_postgres::Config::new();
    cfg.user(&profile.user)
        .dbname(
            profile
                .database
                .as_deref()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or("postgres"),
        )
        .application_name("MyDB Studio");
    if let Some(pw) = profile.password.as_deref() {
        cfg.password(pw);
    }
    let tls = match profile.ssl.mode {
        SslMode::Disabled => None,
        _ => Some(RustlsConnect::new(profile)?),
    };
    cfg.ssl_mode(if tls.is_some() {
        tokio_postgres::config::SslMode::Require
    } else {
        tokio_postgres::config::SslMode::Disable
    });

    let notices = Arc::new(StdMutex::new(Vec::new()));
    let stream = tcp(&host, port).await?;
    let connecting = async {
        match tls.clone() {
            Some(t) => {
                let (client, conn) = cfg.connect_raw(stream, t).await?;
                Ok::<_, CoreError>((client, drive(conn, notices.clone())))
            }
            None => {
                let (client, conn) = cfg.connect_raw(stream, NoTls).await?;
                Ok((client, drive(conn, notices.clone())))
            }
        }
    };
    let (client, driver) = match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(CoreError::Connection {
                message: "PostgreSQL 沒有回應（逾時）".into(),
            });
        }
    };

    let row = client
        .query_one(
            "SELECT current_setting('server_version'), pg_backend_pid()",
            &[],
        )
        .await?;
    let version: String = row.get(0);
    let pid: i32 = row.get(1);
    let info = SessionInfo {
        server_version: format!("PostgreSQL {version}"),
        connection_id: pid as u32,
        tls: tls.is_some(),
        ssh_host_fingerprint: tunnel.as_ref().map(|t| t.host_fingerprint().to_string()),
    };
    let canceller = Canceller {
        token: client.cancel_token(),
        host,
        port,
        tls,
    };
    Ok((
        Link {
            client,
            notices,
            driver,
            _tunnel: tunnel,
        },
        info,
        canceller,
    ))
}

/// Runs the connection in the background, keeping NOTICE / WARNING
/// messages (e.g. from `RAISE NOTICE`) for the result's info line.
fn drive<S, T>(
    mut conn: tokio_postgres::Connection<S, T>,
    notices: Arc<StdMutex<Vec<String>>>,
) -> JoinHandle<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| conn.poll_message(cx));
        while let Some(msg) = messages.next().await {
            match msg {
                Ok(AsyncMessage::Notice(n)) => {
                    let mut list = notices.lock().unwrap();
                    if list.len() < 100 {
                        list.push(format!("{}: {}", n.severity(), n.message()));
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    })
}

// ---------- TLS through rustls ----------

#[derive(Clone)]
struct RustlsConnect {
    config: Arc<rustls::ClientConfig>,
    server_name: rustls::pki_types::ServerName<'static>,
}

impl RustlsConnect {
    fn new(profile: &ConnectionProfile) -> Result<Self> {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

        let config_err = |m: String| CoreError::Config { message: m };
        let ssl = &profile.ssl;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

        let mut roots = rustls::RootCertStore::empty();
        match ssl.ca_path.as_deref().filter(|p| !p.is_empty()) {
            Some(ca) => {
                for cert in CertificateDer::pem_file_iter(ca)
                    .map_err(|e| config_err(format!("CA certificate {ca}: {e}")))?
                {
                    let cert = cert.map_err(|e| config_err(format!("CA certificate {ca}: {e}")))?;
                    roots
                        .add(cert)
                        .map_err(|e| config_err(format!("CA certificate {ca}: {e}")))?;
                }
            }
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = {
            let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
                Arc::new(roots),
                provider.clone(),
            )
            .build()
            .map_err(|e| config_err(format!("TLS: {e}")))?;
            match ssl.mode {
                SslMode::Disabled | SslMode::Required => Arc::new(AcceptAny(provider.clone())),
                SslMode::VerifyCa => Arc::new(SkipHostname(webpki)),
                SslMode::VerifyIdentity => webpki,
            }
        };
        let builder = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| config_err(format!("TLS: {e}")))?
            .dangerous()
            .with_custom_certificate_verifier(verifier);
        let config = match (
            ssl.cert_path.as_deref().filter(|p| !p.is_empty()),
            ssl.key_path.as_deref().filter(|p| !p.is_empty()),
        ) {
            (Some(cert), Some(key)) => {
                let chain = CertificateDer::pem_file_iter(cert)
                    .map_err(|e| config_err(format!("client certificate {cert}: {e}")))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| config_err(format!("client certificate {cert}: {e}")))?;
                let key = PrivateKeyDer::from_pem_file(key)
                    .map_err(|e| config_err(format!("client key {key}: {e}")))?;
                builder
                    .with_client_auth_cert(chain, key)
                    .map_err(|e| config_err(format!("client certificate: {e}")))?
            }
            (None, None) => builder.with_no_client_auth(),
            _ => {
                return Err(config_err(
                    "client certificate and key must be set together".into(),
                ));
            }
        };
        // Through a tunnel we dial 127.0.0.1, but the certificate names the
        // real host.
        let server_name = ServerName::try_from(profile.host.clone())
            .map_err(|e| config_err(format!("host {}: {e}", profile.host)))?;
        Ok(Self {
            config: Arc::new(config),
            server_name,
        })
    }
}

impl<S> tokio_postgres::tls::TlsConnect<S> for RustlsConnect
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<RustlsStream<S>>> + Send>,
    >;

    fn connect(self, stream: S) -> Self::Future {
        Box::pin(async move {
            let s = tokio_rustls::TlsConnector::from(self.config)
                .connect(self.server_name, stream)
                .await?;
            Ok(RustlsStream(s))
        })
    }
}

pub struct RustlsStream<S>(tokio_rustls::client::TlsStream<S>);

impl<S> tokio_postgres::tls::TlsStream for RustlsStream<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    fn channel_binding(&self) -> tokio_postgres::tls::ChannelBinding {
        tokio_postgres::tls::ChannelBinding::none()
    }
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> tokio::io::AsyncRead
    for RustlsStream<S>
{
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite
    for RustlsStream<S>
{
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// `required`: encrypt, accept any certificate (signatures are still
/// checked, so the handshake itself is sound).
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `verify_ca`: the chain must lead to a trusted CA; the host name isn't
/// checked (cloud servers are often reached by IP).
#[derive(Debug)]
struct SkipHostname(Arc<rustls::client::WebPkiServerVerifier>);

impl rustls::client::danger::ServerCertVerifier for SkipHostname {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
        {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                | rustls::CertificateError::NotValidForNameContext { .. },
            )) => Ok(rustls::client::danger::ServerCertVerified::assertion()),
            other => other,
        }
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells() {
        assert_eq!(cell_json("42", Some(&Type::INT4)), serde_json::json!(42));
        assert_eq!(
            cell_json("9223372036854775807", Some(&Type::INT8)),
            serde_json::json!("9223372036854775807")
        );
        assert_eq!(
            cell_json("NaN", Some(&Type::FLOAT8)),
            serde_json::json!("NaN")
        );
        assert_eq!(cell_json("t", Some(&Type::BOOL)), serde_json::json!("true"));
        assert_eq!(
            cell_json("1.50", Some(&Type::NUMERIC)),
            serde_json::json!("1.50")
        );
    }

    #[test]
    fn copy_stdio_is_refused() {
        assert!(is_copy_stdio("COPY t FROM STDIN"));
        assert!(is_copy_stdio("copy (select 1) to stdout with csv"));
        assert!(!is_copy_stdio("COPY t TO '/tmp/x'"));
        assert!(!is_copy_stdio("SELECT 'stdin'"));
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_literal("it's"), "'it''s'");
    }
}
