//! An MCP server (Model Context Protocol, JSON-RPC over stdio) that lets AI
//! tools such as Claude Desktop or Claude Code use the connections saved in
//! MyDB Studio.
//!
//! Each connection has an access level ([`McpAccess`]): off, read-only (the
//! default) or read-write. Read-only connections only take statements that
//! [`advisor::is_plain_read`] accepts, and run them inside a read-only
//! transaction as a second guard. Every query is saved to the app's
//! execution records with a note, so the user can see what an AI tool ran.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mydb_core::export::json_value;
use mydb_core::records::{ExecutionRecord, RecordContext};
use mydb_core::session::StatementResult;
use mydb_core::{ConnectionProfile, CoreError, McpAccess, Session, Store, advisor};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Note attached to records of queries an AI tool ran.
pub const RECORD_NOTE: &str = "由 AI 工具（MCP）執行";

const DEFAULT_ROWS: usize = 100;
const MAX_ROWS: usize = 1000;
/// Longer cells are cut, so one big column cannot flood the AI's context.
const MAX_CELL_CHARS: usize = 2000;

const INSTRUCTIONS: &str = "Tools for the MySQL/MariaDB connections saved in MyDB Studio. \
Start with list_connections; pass a connection's name or id to the other tools. \
Connections marked read_only accept only SELECT, SHOW, DESCRIBE and EXPLAIN. \
Results are capped (max_rows, default 100); add LIMIT or aggregate instead of fetching whole tables. \
Every query is saved in MyDB Studio's execution records.";

pub struct Server {
    store: Mutex<Store>,
    /// Open sessions by connection id, reused across calls.
    sessions: tokio::sync::Mutex<HashMap<String, Arc<Session>>>,
    /// Present while MyDB Studio is locked; then every tool is refused.
    lock_flag: Option<PathBuf>,
}

/// A tool failure, shown to the AI as the tool's result.
struct ToolError(String);

impl From<CoreError> for ToolError {
    fn from(e: CoreError) -> Self {
        ToolError(e.to_string())
    }
}

type ToolResult = Result<Value, ToolError>;

impl Server {
    pub fn new(store: Store) -> Self {
        Self {
            store: Mutex::new(store),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            lock_flag: None,
        }
    }

    /// Refuses all tools while `flag` exists (the app is locked).
    pub fn with_lock_flag(mut self, flag: PathBuf) -> Self {
        self.lock_flag = Some(flag);
        self
    }

    /// Reads JSON-RPC messages, one per line, until `input` ends.
    pub async fn serve<R, W>(&self, input: R, mut output: W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = input.lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(&line).await {
                output.write_all(reply.to_string().as_bytes()).await?;
                output.write_all(b"\n").await?;
                output.flush().await?;
            }
        }
        for (_, session) in self.sessions.lock().await.drain() {
            if let Ok(s) = Arc::try_unwrap(session) {
                let _ = s.close().await;
            }
        }
        Ok(())
    }

    /// Handles one message; `None` for notifications, which get no reply.
    pub async fn handle_line(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let id = id?;
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => Ok(self.call(&params).await),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => rpc_error(id, code, &message),
        })
    }

    async fn call(&self, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let locked = self.lock_flag.as_ref().is_some_and(|f| f.exists());
        let result = match name {
            _ if locked => Err(ToolError(
                "MyDB Studio is locked. Ask the user to unlock it, then try again.".into(),
            )),
            "list_connections" => self.list_connections(),
            "list_databases" => self.list_databases(&args).await,
            "list_tables" => self.list_tables(&args).await,
            "describe_table" => self.describe_table(&args).await,
            "run_query" => self.run_query(&args).await,
            "explain_query" => self.explain_query(&args).await,
            _ => Err(ToolError(format!("unknown tool: {name}"))),
        };
        match result {
            Ok(value) => json!({
                "content": [{ "type": "text", "text": pretty(&value) }],
                "structuredContent": value,
            }),
            Err(ToolError(message)) => json!({
                "content": [{ "type": "text", "text": message }],
                "isError": true,
            }),
        }
    }

    fn list_connections(&self) -> ToolResult {
        let list = self.store.lock().unwrap().list_connections()?;
        let connections: Vec<Value> = list
            .iter()
            .filter(|p| p.mcp_allowed())
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "server": format!("{}@{}:{}", p.user, p.host, p.port),
                    "default_database": p.database,
                    "access": access_name(p.mcp),
                })
            })
            .collect();
        Ok(json!({ "connections": connections }))
    }

    async fn list_databases(&self, args: &Value) -> ToolResult {
        let (_, session) = self.open(args).await?;
        let dbs = session.list_databases().await?;
        Ok(json!({ "databases": dbs }))
    }

    async fn list_tables(&self, args: &Value) -> ToolResult {
        let (_, session) = self.open(args).await?;
        let db = self.database_arg(args, &session)?;
        let tables = session.list_tables(&db).await?;
        Ok(json!({ "database": db, "tables": tables }))
    }

    async fn describe_table(&self, args: &Value) -> ToolResult {
        let (_, session) = self.open(args).await?;
        let table = required(args, "table")?;
        let db = str_arg(args, "database").or_else(|| session.current_database());
        let ddl = session.show_create_table(db.as_deref(), &table).await?;
        Ok(json!({ "database": db, "table": table, "create_table": ddl }))
    }

    async fn run_query(&self, args: &Value) -> ToolResult {
        let (profile, session) = self.open(args).await?;
        let sql = required(args, "sql")?;
        let max_rows = args
            .get("max_rows")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_ROWS, |n| (n as usize).clamp(1, MAX_ROWS));
        if let Some(db) = str_arg(args, "database") {
            session.use_database(&db).await?;
        }
        let read_only = profile.mcp == McpAccess::ReadOnly;
        if read_only {
            check_read_only(&sql)?;
            transaction(&session, "START TRANSACTION READ ONLY").await?;
        }
        let outcome = session.execute(&sql, max_rows).await;
        if read_only {
            transaction(&session, "ROLLBACK").await?;
        }
        self.record(&profile, &session, &sql, &outcome);

        let statements: Vec<Value> = outcome.results.iter().map(statement_json).collect();
        let mut out = json!({
            "statements": statements,
            "database": session.current_database(),
        });
        if let Some(e) = &outcome.error {
            out["error"] = json!(e.to_string());
            if let Some(stmt) = &outcome.failed_statement {
                out["failed_statement"] = json!(stmt);
            }
            if let Some(advice) = advisor::error_advice(e) {
                out["advice"] = json!(advice);
            }
        }
        Ok(out)
    }

    async fn explain_query(&self, args: &Value) -> ToolResult {
        let (profile, session) = self.open(args).await?;
        let sql = required(args, "sql")?;
        let stmt = mydb_core::split_statements(&sql)
            .into_iter()
            .next()
            .ok_or_else(|| ToolError("sql is empty".into()))?;
        if let Some(db) = str_arg(args, "database") {
            session.use_database(&db).await?;
        }
        let explain = format!("EXPLAIN {stmt}");
        // EXPLAIN never runs the statement, so it is fine on read-only connections.
        let outcome = session.execute(&explain, MAX_ROWS).await;
        self.record(&profile, &session, &explain, &outcome);
        if let Some(e) = outcome.error {
            return Err(e.into());
        }
        Ok(json!({
            "plan": outcome.results.iter().map(statement_json).collect::<Vec<_>>(),
            "findings": advisor::check_sql(&stmt),
        }))
    }

    /// The connection named by `args.connection` (name or id), opened.
    async fn open(&self, args: &Value) -> Result<(ConnectionProfile, Arc<Session>), ToolError> {
        let wanted = required(args, "connection")?;
        let profile = {
            let store = self.store.lock().unwrap();
            match store
                .list_connections()?
                .into_iter()
                .find(|p| p.id == wanted || p.name.eq_ignore_ascii_case(&wanted))
            {
                // With its passwords this time.
                Some(found) => store.get_connection(&found.id)?,
                None => None,
            }
        }
        .filter(|p| p.mcp_allowed())
        .ok_or_else(|| {
            ToolError(format!(
                "no connection named \"{wanted}\" is available to AI tools; call list_connections"
            ))
        })?;
        let mut sessions = self.sessions.lock().await;
        let session = match sessions.get(&profile.id) {
            Some(s) => s.clone(),
            None => {
                let s = Arc::new(Session::connect(&profile).await?);
                sessions.insert(profile.id.clone(), s.clone());
                s
            }
        };
        Ok((profile, session))
    }

    fn database_arg(&self, args: &Value, session: &Session) -> Result<String, ToolError> {
        str_arg(args, "database")
            .or_else(|| session.current_database())
            .ok_or_else(|| ToolError("pass database: this connection has no default".into()))
    }

    /// Saves the run in the execution records, marked as coming from MCP.
    fn record(
        &self,
        profile: &ConnectionProfile,
        session: &Session,
        sql: &str,
        outcome: &mydb_core::ExecutionOutcome,
    ) {
        let rec = ExecutionRecord::from_outcome(
            RecordContext {
                connection_id: &profile.id,
                connection_name: &profile.name,
                server: format!("{}@{}:{}", profile.user, profile.host, profile.port),
                server_version: &session.info().server_version,
                database: session.current_database(),
            },
            sql,
            outcome,
        );
        let store = self.store.lock().unwrap();
        match store.insert_record(&rec) {
            Ok(id) => {
                let _ = store.set_record_note(id, RECORD_NOTE);
            }
            Err(e) => eprintln!("mydb-mcp: could not save record: {e}"),
        }
    }
}

/// Refuses anything but plain reads, statement by statement.
fn check_read_only(sql: &str) -> Result<(), ToolError> {
    let statements = mydb_core::split_statements(sql);
    if statements.is_empty() {
        return Err(ToolError("sql is empty".into()));
    }
    match statements.iter().find(|s| !advisor::is_plain_read(s)) {
        Some(s) => Err(ToolError(format!(
            "this connection is read-only for AI tools; only SELECT, SHOW, DESCRIBE and EXPLAIN \
             are allowed. Refused: {}. The user can allow changes in MyDB Studio's connection \
             settings (AI 工具 → 可寫入).",
            s.lines().next().unwrap_or(s)
        ))),
        None => Ok(()),
    }
}

async fn transaction(session: &Session, sql: &str) -> Result<(), ToolError> {
    match session.execute(sql, 0).await.error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

fn statement_json(r: &StatementResult) -> Value {
    let rows: Vec<Vec<Value>> = r
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .zip(&r.columns)
                .map(|(v, c)| clip_cell(&json_value(v, c)))
                .collect()
        })
        .collect();
    let mut v = json!({
        "sql": r.sql,
        "columns": r.columns.iter().map(|c| &c.name).collect::<Vec<_>>(),
        "duration_ms": (r.duration_ms * 10.0).round() / 10.0,
    });
    if !r.columns.is_empty() {
        v["rows"] = json!(rows);
        v["total_rows"] = json!(r.total_rows);
        if r.truncated {
            v["truncated"] = json!(true);
        }
    } else {
        v["affected_rows"] = json!(r.affected_rows);
        if let Some(id) = r.last_insert_id.filter(|id| *id > 0) {
            v["last_insert_id"] = json!(id);
        }
    }
    if r.warnings > 0 {
        v["warnings"] = json!(r.warnings);
    }
    v
}

fn clip_cell(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > MAX_CELL_CHARS => {
            let cut: String = s.chars().take(MAX_CELL_CHARS).collect();
            json!(format!("{cut}…（截斷，共 {} 字）", s.chars().count()))
        }
        other => other.clone(),
    }
}

fn access_name(a: McpAccess) -> &'static str {
    match a {
        McpAccess::Off => "off",
        McpAccess::ReadOnly => "read_only",
        McpAccess::ReadWrite => "read_write",
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn required(args: &Value, key: &str) -> Result<String, ToolError> {
    str_arg(args, key).ok_or_else(|| ToolError(format!("missing argument: {key}")))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mydb-studio", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let connection = json!({
        "type": "string",
        "description": "Connection name or id, from list_connections",
    });
    let database = json!({
        "type": "string",
        "description": "Database to use; defaults to the connection's default database",
    });
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            "name": "list_connections",
            "description": "List the MySQL/MariaDB connections saved in MyDB Studio that AI tools may use, with their access level (read_only or read_write).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only,
        },
        {
            "name": "list_databases",
            "description": "List the databases on a connection, with table counts.",
            "inputSchema": {
                "type": "object",
                "properties": { "connection": connection },
                "required": ["connection"],
            },
            "annotations": read_only,
        },
        {
            "name": "list_tables",
            "description": "List tables and views in a database, with engine, estimated rows, size and comment.",
            "inputSchema": {
                "type": "object",
                "properties": { "connection": connection, "database": database },
                "required": ["connection"],
            },
            "annotations": read_only,
        },
        {
            "name": "describe_table",
            "description": "Get SHOW CREATE TABLE for a table or view: columns, types, indexes and foreign keys.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "connection": connection,
                    "database": database,
                    "table": { "type": "string" },
                },
                "required": ["connection", "table"],
            },
            "annotations": read_only,
        },
        {
            "name": "run_query",
            "description": "Run SQL (one or more statements separated by ;) and return the results. On read_only connections only SELECT, SHOW, DESCRIBE and EXPLAIN are accepted. Rows per result are capped by max_rows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "connection": connection,
                    "sql": { "type": "string" },
                    "database": database,
                    "max_rows": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_ROWS,
                        "description": "Rows to return per result (default 100)",
                    },
                },
                "required": ["connection", "sql"],
            },
            "annotations": { "destructiveHint": true, "openWorldHint": false },
        },
        {
            "name": "explain_query",
            "description": "Show the execution plan of a statement (EXPLAIN, without running it) plus MyDB Studio's checks for full scans and risky statements.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "connection": connection,
                    "sql": { "type": "string" },
                    "database": database,
                },
                "required": ["connection", "sql"],
            },
            "annotations": read_only,
        },
    ])
}

/// Serves MCP on stdin/stdout until stdin closes.
pub async fn serve_stdio(store: Store, lock_flag: Option<PathBuf>) -> std::io::Result<()> {
    let mut server = Server::new(store);
    if let Some(flag) = lock_flag {
        server = server.with_lock_flag(flag);
    }
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    server.serve(stdin, tokio::io::stdout()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        let store = Store::open_in_memory().unwrap();
        let mut off = ConnectionProfile::new("hidden", "db.example", "u");
        off.mcp = McpAccess::Off;
        store.save_connection(&off).unwrap();
        store
            .save_connection(&ConnectionProfile::new("Shop", "127.0.0.1", "reader"))
            .unwrap();
        // The MCP server only speaks SQL: Redis stays out even when allowed.
        let mut cache = ConnectionProfile::new_redis("cache", "127.0.0.1");
        cache.mcp = McpAccess::ReadWrite;
        store.save_connection(&cache).unwrap();
        Server::new(store)
    }

    async fn ask(s: &Server, msg: Value) -> Value {
        s.handle_line(&msg.to_string()).await.unwrap()
    }

    #[tokio::test]
    async fn handshake_and_tool_list() {
        let s = server();
        let init = ask(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
        )
        .await;
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "mydb-studio");
        assert!(
            s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .await
                .is_none()
        );
        let list = ask(&s, json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await;
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"run_query") && names.contains(&"list_connections"));
        let unknown = ask(&s, json!({"jsonrpc":"2.0","id":3,"method":"nope"})).await;
        assert_eq!(unknown["error"]["code"], -32601);
        let bad = s.handle_line("{not json").await.unwrap();
        assert_eq!(bad["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn hidden_connections_stay_hidden() {
        let s = server();
        let out = ask(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_connections","arguments":{}}}),
        )
        .await;
        let conns = &out["result"]["structuredContent"]["connections"];
        assert_eq!(conns.as_array().unwrap().len(), 1);
        assert_eq!(conns[0]["name"], "Shop");
        assert_eq!(conns[0]["access"], "read_only");
        // No password or other secret is listed.
        assert!(!out.to_string().contains("password"));

        for name in ["hidden", "cache"] {
            let out = ask(
                &s,
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"run_query","arguments":{"connection":name,"sql":"SELECT 1"}}}),
            )
            .await;
            assert_eq!(out["result"]["isError"], true, "{name}");
        }
    }

    #[test]
    fn read_only_refuses_changes() {
        assert!(check_read_only("SELECT 1; SHOW TABLES").is_ok());
        for sql in [
            "DELETE FROM t",
            "SELECT 1; DROP TABLE t",
            "SELECT * FROM t FOR UPDATE",
            "SET GLOBAL read_only = 0",
            "",
        ] {
            assert!(check_read_only(sql).is_err(), "{sql}");
        }
    }

    #[tokio::test]
    async fn locked_app_refuses_every_tool() {
        let dir = std::env::temp_dir().join(format!("mydb-mcp-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let flag = dir.join(mydb_core::LOCK_FLAG);
        let s = server().with_lock_flag(flag.clone());
        let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_connections","arguments":{}}});

        std::fs::write(&flag, "").unwrap();
        let out = ask(&s, list.clone()).await;
        assert_eq!(out["result"]["isError"], true);
        assert!(
            out["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("locked")
        );

        std::fs::remove_file(&flag).unwrap();
        let out = ask(&s, list).await;
        assert!(out["result"]["isError"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
