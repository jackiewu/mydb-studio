//! Redis connections: browsing keys with `SCAN`, reading values a page at a
//! time, and running commands typed in the editor.
//!
//! Like the MySQL [`Session`](crate::Session), a lost connection is
//! re-established on the next call, SSH tunnel included. There are two
//! connections to the server: one for browsing and one for the command
//! editor, so a blocking command there doesn't hold up the key tree.

pub mod command;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use ::redis::aio::MultiplexedConnection;
use ::redis::io::AsyncDNSResolver;
use ::redis::{
    AsyncConnectionConfig, Client, ClientTlsConfig, ConnectionAddr, IntoConnectionInfo,
    RedisConnectionInfo, TlsCertificates, Value,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};

pub use command::{Reply, check_command, command_lines, mask_text, masked_line, split_args};

use crate::error::{CoreError, Result};
use crate::profile::{ConnectionKind, ConnectionProfile, SslMode};
use crate::records::{ExecutionRecord, RECORD_PREVIEW_ROWS, RecordedStatement};
use crate::session::SessionInfo;
use crate::tunnel::SshTunnel;

/// Databases shown in the key tree.
pub const DATABASES: u8 = 16;
/// Keys loaded per "load more" in the key tree.
pub const SCAN_BATCH: usize = 500;
/// Elements of a collection read per page.
pub const VALUE_PAGE: usize = 200;
/// Strings longer than this are cut when shown.
pub const MAX_STRING_BYTES: usize = 1024 * 1024;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const BROWSE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct RedisSession {
    profile: ConnectionProfile,
    info: SessionInfo,
    link: Mutex<Option<Link>>,
    /// The command editor's connection, opened on first use.
    cmd: Mutex<Option<MultiplexedConnection>>,
    /// Database selected with `SELECT` in the editor, else the profile's.
    current_db: AtomicU8,
    busy: AtomicBool,
    cancelled: AtomicBool,
    stop: Notify,
}

struct Link {
    client: Client,
    browse: MultiplexedConnection,
    /// Database the browse connection has selected.
    browse_db: u8,
    // Last, so the connections close before the tunnel.
    _tunnel: Option<SshTunnel>,
}

/// Sends every connection to the tunnel, while TLS still checks the
/// certificate against the real host name.
struct ViaTunnel(SocketAddr);

impl AsyncDNSResolver for ViaTunnel {
    fn resolve<'a, 'b: 'a>(
        &'a self,
        _host: &'b str,
        _port: u16,
    ) -> ::redis::RedisFuture<'a, Box<dyn Iterator<Item = SocketAddr> + Send + 'a>> {
        let addr = self.0;
        Box::pin(async move {
            Ok(Box::new(std::iter::once(addr)) as Box<dyn Iterator<Item = SocketAddr> + Send>)
        })
    }
}

/// A key as it travels to and from the UI: UTF-8 text, or hex for binary
/// keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyName {
    pub key: String,
    #[serde(default)]
    pub binary: bool,
}

impl KeyName {
    fn from_bytes(bytes: &[u8]) -> Self {
        let (key, binary) = command::bytes_text(bytes);
        Self { key, binary }
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        if !self.binary {
            return Ok(self.key.as_bytes().to_vec());
        }
        (0..self.key.len())
            .step_by(2)
            .map(|i| {
                self.key
                    .get(i..i + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| CoreError::Config {
                message: "invalid binary key".into(),
            })
    }
}

/// A key in the tree.
#[derive(Debug, Clone, Serialize)]
pub struct KeyEntry {
    #[serde(flatten)]
    pub name: KeyName,
    /// `string`, `hash`, `list`, `set`, `zset`, `stream`, or `none` when it
    /// expired meanwhile.
    pub kind: String,
    /// Seconds to live; -1 when it never expires.
    pub ttl: i64,
}

/// One batch of `SCAN`.
#[derive(Debug, Clone, Serialize)]
pub struct ScanPage {
    /// Cursor for the next batch; `"0"` when the scan is complete.
    pub cursor: String,
    pub keys: Vec<KeyEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DbInfo {
    pub index: u8,
    pub keys: u64,
    pub expires: u64,
}

/// Facts about one key, shown above its value.
#[derive(Debug, Clone, Serialize)]
pub struct KeyInfo {
    pub kind: String,
    pub ttl: i64,
    pub encoding: Option<String>,
    /// `MEMORY USAGE`, when the server allows it.
    pub memory_bytes: Option<u64>,
    /// Characters of a string, elements of a collection.
    pub length: Option<u64>,
}

/// A page of a value: text for strings, rows for collections.
#[derive(Debug, Clone, Serialize)]
pub struct ValuePage {
    pub kind: String,
    /// The string, for strings.
    pub text: Option<String>,
    pub binary: bool,
    /// The string was longer than [`MAX_STRING_BYTES`] and was cut.
    pub truncated: bool,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Where the next page starts (a cursor, offset or stream id); `None`
    /// at the end.
    pub next: Option<String>,
}

/// One command line that ran.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    /// 0-based line in the editor text.
    pub line: usize,
    /// The command, passwords masked.
    pub command: String,
    pub duration_ms: f64,
    pub reply: Reply,
}

/// Everything about one run of the command editor.
#[derive(Debug, Serialize)]
pub struct RedisOutcome {
    pub started_at: DateTime<Utc>,
    pub duration_ms: f64,
    pub results: Vec<CommandResult>,
    /// Why the run stopped early: an error reply, a lost connection, a line
    /// that doesn't parse, or the user pressing stop.
    pub error: Option<CoreError>,
    /// The command (masked) that raised `error`.
    pub failed_command: Option<String>,
    pub reconnected: bool,
    pub cancelled: bool,
    /// The database in use after the run.
    pub db: u8,
}

impl RedisSession {
    pub async fn connect(profile: &ConnectionProfile) -> Result<Self> {
        let db = profile.redis_db.min(DATABASES - 1);
        let mut link = open_link(profile, db).await?;
        let info = server_info(profile, &mut link).await?;
        Ok(Self {
            profile: profile.clone(),
            info,
            link: Mutex::new(Some(link)),
            cmd: Mutex::new(None),
            current_db: AtomicU8::new(db),
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            stop: Notify::new(),
        })
    }

    pub fn info(&self) -> &SessionInfo {
        &self.info
    }

    pub fn profile(&self) -> &ConnectionProfile {
        &self.profile
    }

    pub fn current_db(&self) -> u8 {
        self.current_db.load(Ordering::SeqCst)
    }

    /// The browse connection, reconnecting if it is gone. The bool is true
    /// when a reconnect happened.
    async fn live<'a>(&self, link: &'a mut Option<Link>) -> Result<(&'a mut Link, bool)> {
        let alive = match link.as_mut() {
            Some(l) => ::redis::cmd("PING")
                .query_async::<Value>(&mut l.browse)
                .await
                .is_ok(),
            None => false,
        };
        if !alive {
            // The editor's connection keeps working through the old tunnel's
            // session, or notices itself that it is gone.
            *link = None;
            *link = Some(open_link(&self.profile, self.current_db()).await?);
        }
        Ok((link.as_mut().unwrap(), !alive))
    }

    /// The browse connection with `db` selected.
    async fn browse<'a>(
        &self,
        link: &'a mut Option<Link>,
        db: u8,
    ) -> Result<&'a mut MultiplexedConnection> {
        let (l, _) = self.live(link).await?;
        if l.browse_db != db {
            ::redis::cmd("SELECT")
                .arg(db)
                .query_async::<()>(&mut l.browse)
                .await?;
            l.browse_db = db;
        }
        Ok(&mut l.browse)
    }

    /// Key counts per database, from `INFO keyspace`. Always lists db0 to
    /// db15, plus any higher database that has keys.
    pub async fn databases(&self) -> Result<Vec<DbInfo>> {
        let mut guard = self.link.lock().await;
        let (l, _) = self.live(&mut guard).await?;
        let text: String = ::redis::cmd("INFO")
            .arg("keyspace")
            .query_async(&mut l.browse)
            .await?;
        Ok(parse_keyspace(&text))
    }

    /// The next batch of keys matching `pattern` (all when empty), starting
    /// at `cursor` ("0" for the first batch). Calls `SCAN` until about
    /// [`SCAN_BATCH`] keys are found, the scan ends, or a couple of seconds
    /// pass. Never uses `KEYS`.
    pub async fn scan(&self, db: u8, cursor: &str, pattern: &str) -> Result<ScanPage> {
        let mut guard = self.link.lock().await;
        let conn = self.browse(&mut guard, db).await?;
        let mut cursor: u64 = cursor.parse().unwrap_or(0);
        let mut found: Vec<Vec<u8>> = Vec::new();
        let started = Instant::now();
        loop {
            let mut c = ::redis::cmd("SCAN");
            c.arg(cursor);
            if !pattern.is_empty() && pattern != "*" {
                c.arg("MATCH").arg(pattern);
            }
            c.arg("COUNT").arg(SCAN_BATCH);
            let (next, keys): (u64, Vec<Vec<u8>>) = c.query_async(conn).await?;
            found.extend(keys);
            cursor = next;
            if cursor == 0
                || found.len() >= SCAN_BATCH
                || started.elapsed() > Duration::from_secs(2)
            {
                break;
            }
        }
        // SCAN may return a key twice across calls.
        found.sort();
        found.dedup();

        let mut pipe = ::redis::pipe();
        for k in &found {
            pipe.cmd("TYPE").arg(k).cmd("TTL").arg(k);
        }
        let facts: Vec<Value> = if found.is_empty() {
            Vec::new()
        } else {
            pipe.query_async(conn).await?
        };
        let keys = found
            .iter()
            .zip(facts.chunks(2))
            .map(|(k, f)| KeyEntry {
                name: KeyName::from_bytes(k),
                kind: value_text(&f[0]),
                ttl: value_int(&f[1]).unwrap_or(-1),
            })
            .collect();
        Ok(ScanPage {
            cursor: cursor.to_string(),
            keys,
        })
    }

    pub async fn key_info(&self, db: u8, key: &KeyName) -> Result<KeyInfo> {
        let k = key.bytes()?;
        let mut guard = self.link.lock().await;
        let conn = self.browse(&mut guard, db).await?;
        let kind: String = ::redis::cmd("TYPE").arg(&k).query_async(conn).await?;
        let ttl: i64 = ::redis::cmd("TTL").arg(&k).query_async(conn).await?;
        // Both can be refused (ACLs, managed services); then they're left out.
        let encoding: Option<String> = ::redis::cmd("OBJECT")
            .arg("ENCODING")
            .arg(&k)
            .query_async(conn)
            .await
            .ok()
            .flatten();
        let memory_bytes: Option<u64> = ::redis::cmd("MEMORY")
            .arg("USAGE")
            .arg(&k)
            .query_async(conn)
            .await
            .ok()
            .flatten();
        let length_cmd = match kind.as_str() {
            "string" => Some("STRLEN"),
            "hash" => Some("HLEN"),
            "list" => Some("LLEN"),
            "set" => Some("SCARD"),
            "zset" => Some("ZCARD"),
            "stream" => Some("XLEN"),
            _ => None,
        };
        let length = match length_cmd {
            Some(c) => ::redis::cmd(c).arg(&k).query_async(conn).await.ok(),
            None => None,
        };
        Ok(KeyInfo {
            kind,
            ttl,
            encoding,
            memory_bytes,
            length,
        })
    }

    /// A page of `key`'s value, starting at `from` (the `next` of the
    /// previous page; empty for the first).
    pub async fn read_value(&self, db: u8, key: &KeyName, from: &str) -> Result<ValuePage> {
        let k = key.bytes()?;
        let mut guard = self.link.lock().await;
        let conn = self.browse(&mut guard, db).await?;
        let kind: String = ::redis::cmd("TYPE").arg(&k).query_async(conn).await?;
        let mut page = ValuePage {
            kind: kind.clone(),
            text: None,
            binary: false,
            truncated: false,
            columns: Vec::new(),
            rows: Vec::new(),
            next: None,
        };
        let cursor: u64 = from.parse().unwrap_or(0);
        let text = |v: &Value| serde_json::Value::String(value_text(v));
        match kind.as_str() {
            "string" => {
                let len: usize = ::redis::cmd("STRLEN").arg(&k).query_async(conn).await?;
                let bytes: Vec<u8> = if len > MAX_STRING_BYTES {
                    page.truncated = true;
                    ::redis::cmd("GETRANGE")
                        .arg(&k)
                        .arg(0)
                        .arg(MAX_STRING_BYTES - 1)
                        .query_async(conn)
                        .await?
                } else {
                    ::redis::cmd("GET").arg(&k).query_async(conn).await?
                };
                let (t, binary) = if page.truncated {
                    // A cut can split a character; that is not binary data.
                    (String::from_utf8_lossy(&bytes).into_owned(), false)
                } else {
                    command::bytes_text(&bytes)
                };
                page.text = Some(t);
                page.binary = binary;
            }
            "hash" | "set" | "zset" => {
                let scan = match kind.as_str() {
                    "hash" => "HSCAN",
                    "set" => "SSCAN",
                    _ => "ZSCAN",
                };
                let (next, items): (u64, Vec<Value>) = ::redis::cmd(scan)
                    .arg(&k)
                    .arg(cursor)
                    .arg("COUNT")
                    .arg(VALUE_PAGE)
                    .query_async(conn)
                    .await?;
                page.columns = match kind.as_str() {
                    "hash" => vec!["field".into(), "value".into()],
                    "set" => vec!["member".into()],
                    _ => vec!["member".into(), "score".into()],
                };
                let width = page.columns.len();
                page.rows = items
                    .chunks(width)
                    .map(|c| c.iter().map(text).collect())
                    .collect();
                page.next = (next != 0).then(|| next.to_string());
            }
            "list" => {
                let start = cursor as usize;
                let items: Vec<Value> = ::redis::cmd("LRANGE")
                    .arg(&k)
                    .arg(start)
                    .arg(start + VALUE_PAGE - 1)
                    .query_async(conn)
                    .await?;
                let len: usize = ::redis::cmd("LLEN").arg(&k).query_async(conn).await?;
                page.columns = vec!["index".into(), "value".into()];
                page.rows = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| vec![serde_json::json!(start + i), text(v)])
                    .collect();
                page.next = (start + VALUE_PAGE < len).then(|| (start + VALUE_PAGE).to_string());
            }
            "stream" => {
                // Exclusive start after the last id of the previous page.
                let start = if from.is_empty() {
                    "-".to_string()
                } else {
                    format!("({from}")
                };
                let entries: Vec<(String, Vec<Value>)> = ::redis::cmd("XRANGE")
                    .arg(&k)
                    .arg(&start)
                    .arg("+")
                    .arg("COUNT")
                    .arg(VALUE_PAGE)
                    .query_async(conn)
                    .await?;
                page.columns = vec!["id".into(), "fields".into()];
                page.next = (entries.len() == VALUE_PAGE)
                    .then(|| entries.last().map(|(id, _)| id.clone()))
                    .flatten();
                page.rows = entries
                    .into_iter()
                    .map(|(id, fields)| {
                        let map: serde_json::Map<String, serde_json::Value> = fields
                            .chunks(2)
                            .map(|f| (value_text(&f[0]), text(f.get(1).unwrap_or(&Value::Nil))))
                            .collect();
                        vec![id.into(), serde_json::Value::Object(map).to_string().into()]
                    })
                    .collect();
            }
            _ => {}
        }
        Ok(page)
    }

    /// Runs the command lines in `text` one by one on the editor's
    /// connection, stopping at the first error. Lines must have been
    /// confirmed already (see [`check_command`]).
    pub async fn execute(&self, text: &str) -> RedisOutcome {
        let started_at = Utc::now();
        let timer = Instant::now();
        let mut results = Vec::new();
        let mut error = None;
        let mut failed_command = None;
        let mut reconnected = false;
        let mut guard = self.cmd.lock().await;
        let _busy = self.start_run();

        for (line, raw) in command_lines(text) {
            let args = match split_args(raw) {
                Ok(a) if !a.is_empty() => a,
                Ok(_) => continue,
                Err(e) => {
                    error = Some(CoreError::Config {
                        message: format!("第 {} 行：{e}", line + 1),
                    });
                    failed_command = Some(mask_text(raw));
                    break;
                }
            };
            let shown = masked_line(&args);
            if self.cancelled.load(Ordering::SeqCst) {
                error = Some(CoreError::Cancelled);
                failed_command = Some(shown);
                break;
            }
            let conn = match self.cmd_conn(&mut guard).await {
                Ok((c, r)) => {
                    reconnected |= r;
                    c
                }
                Err(e) => {
                    error = Some(e);
                    failed_command = Some(shown);
                    break;
                }
            };
            let mut c = ::redis::Cmd::new();
            for a in &args {
                c.arg(a.as_bytes());
            }
            let cmd_timer = Instant::now();
            let stopped = self.stop.notified();
            tokio::pin!(stopped);
            stopped.as_mut().enable();
            let replied = if self.cancelled.load(Ordering::SeqCst) {
                None
            } else {
                tokio::select! {
                    r = c.query_async::<Value>(conn) => Some(r),
                    _ = &mut stopped => None,
                }
            };
            let duration_ms = cmd_timer.elapsed().as_secs_f64() * 1000.0;
            let name = command::command_name(&args);
            match replied {
                None => {
                    // The reply may still come; drop the connection so it
                    // can't be mistaken for the next command's.
                    *guard = None;
                    error = Some(CoreError::Cancelled);
                    failed_command = Some(shown);
                    break;
                }
                Some(Ok(v)) => {
                    if name == "SELECT"
                        && let Some(db) = args.get(1).and_then(|d| d.parse::<u8>().ok())
                    {
                        self.current_db.store(db, Ordering::SeqCst);
                    }
                    results.push(CommandResult {
                        line,
                        command: shown,
                        duration_ms,
                        reply: v.into(),
                    });
                    // The connection now streams commands or messages
                    // instead of replies; let it go.
                    if matches!(
                        name.as_str(),
                        "MONITOR" | "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE"
                    ) {
                        *guard = None;
                    }
                }
                Some(Err(e)) => {
                    let e = CoreError::from(e);
                    if let CoreError::Redis { message } = &e {
                        results.push(CommandResult {
                            line,
                            command: shown.clone(),
                            duration_ms,
                            reply: Reply::Error {
                                message: message.clone(),
                            },
                        });
                    } else {
                        *guard = None;
                    }
                    error = Some(e);
                    failed_command = Some(shown);
                    break;
                }
            }
        }
        RedisOutcome {
            started_at,
            duration_ms: timer.elapsed().as_secs_f64() * 1000.0,
            results,
            error,
            failed_command,
            reconnected,
            cancelled: self.cancelled.load(Ordering::SeqCst),
            db: self.current_db(),
        }
    }

    /// The editor's connection, opening or re-opening it as needed.
    async fn cmd_conn<'a>(
        &self,
        cmd: &'a mut Option<MultiplexedConnection>,
    ) -> Result<(&'a mut MultiplexedConnection, bool)> {
        if let Some(c) = cmd.as_mut()
            && ::redis::cmd("PING").query_async::<Value>(c).await.is_ok()
        {
            return Ok((cmd.as_mut().unwrap(), false));
        }
        let lost = cmd.take().is_some();
        let client = {
            let mut guard = self.link.lock().await;
            let (l, _) = self.live(&mut guard).await?;
            l.client.clone()
        };
        // No response timeout: commands like BLPOP wait as long as asked.
        let config = connection_config(self.link_tunnel_addr().await).set_response_timeout(None);
        let mut conn = client
            .get_multiplexed_async_connection_with_config(&config)
            .await?;
        ::redis::cmd("SELECT")
            .arg(self.current_db())
            .query_async::<()>(&mut conn)
            .await?;
        *cmd = Some(conn);
        Ok((cmd.as_mut().unwrap(), lost))
    }

    async fn link_tunnel_addr(&self) -> Option<SocketAddr> {
        let guard = self.link.lock().await;
        guard
            .as_ref()
            .and_then(|l| l._tunnel.as_ref())
            .map(|t| SocketAddr::from(([127, 0, 0, 1], t.local_port())))
    }

    fn start_run(&self) -> BusyGuard<'_> {
        self.cancelled.store(false, Ordering::SeqCst);
        self.busy.store(true, Ordering::SeqCst);
        BusyGuard(&self.busy)
    }

    /// Stops the command that is running, if any: the editor stops waiting
    /// and its connection is dropped, which also ends blocking commands
    /// like `BLPOP` on the server. Returns whether something was running.
    pub fn cancel(&self) -> bool {
        if !self.busy.load(Ordering::SeqCst) {
            return false;
        }
        self.cancelled.store(true, Ordering::SeqCst);
        self.stop.notify_waiters();
        true
    }

    /// A record of a run: the commands with passwords masked, each reply's
    /// type and size, and a short preview.
    pub fn record(&self, text: &str, outcome: &RedisOutcome) -> ExecutionRecord {
        let p = &self.profile;
        let statements: Vec<RecordedStatement> = outcome
            .results
            .iter()
            .map(|r| RecordedStatement {
                sql: r.command.clone(),
                duration_ms: r.duration_ms,
                affected_rows: 0,
                returned_rows: r.reply.element_count().map(|n| n as u64),
                warnings: 0,
                info: r.reply.type_name().into(),
                columns: vec!["value".into()],
                preview: preview(&r.reply),
            })
            .collect();
        let (error_code, error_message) = match &outcome.error {
            None => (None, None),
            Some(CoreError::Redis { message }) => (None, Some(message.clone())),
            Some(other) => (None, Some(other.to_string())),
        };
        ExecutionRecord {
            id: 0,
            connection_id: Some(p.id.clone()),
            connection_name: p.name.clone(),
            connection_kind: ConnectionKind::Redis,
            server: p.server_label(),
            server_version: self.info.server_version.clone(),
            database: Some(format!("db{}", outcome.db)),
            started_at: outcome.started_at.to_rfc3339(),
            duration_ms: outcome.duration_ms,
            sql: mask_text(text.trim()),
            statement_count: statements.len() as u32
                + u32::from(
                    outcome.failed_command.is_some()
                        && outcome
                            .results
                            .iter()
                            .all(|r| !matches!(r.reply, Reply::Error { .. })),
                ),
            affected_rows: 0,
            returned_rows: statements.iter().filter_map(|s| s.returned_rows).sum(),
            statements,
            error_code,
            error_message,
            failed_statement: outcome.failed_command.clone(),
            note: String::new(),
        }
    }

    pub async fn close(self) {
        // Dropping the connections closes them, then the tunnel.
        drop(self.cmd.into_inner());
        drop(self.link.into_inner());
    }
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// What a record keeps of a reply.
fn preview(reply: &Reply) -> Vec<Vec<serde_json::Value>> {
    let cut = |s: String| -> serde_json::Value {
        if s.chars().count() > 500 {
            format!("{}…", s.chars().take(500).collect::<String>()).into()
        } else {
            s.into()
        }
    };
    match reply {
        Reply::Array { items } => items
            .iter()
            .take(RECORD_PREVIEW_ROWS)
            .map(|i| vec![cut(i.to_text())])
            .collect(),
        Reply::Map { entries } => entries
            .iter()
            .take(RECORD_PREVIEW_ROWS)
            .map(|(k, v)| vec![cut(format!("{}: {}", k.to_text(), v.to_text()))])
            .collect(),
        other => vec![vec![cut(other.to_text())]],
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::BulkString(b) => {
            let (t, binary) = command::bytes_text(b);
            if binary { format!("0x{t}") } else { t }
        }
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".into(),
        Value::Int(i) => i.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Nil => String::new(),
        other => Reply::from(other.clone()).to_text(),
    }
}

fn value_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        _ => value_text(v).parse().ok(),
    }
}

/// `db0:keys=1,expires=0,avg_ttl=0` lines of `INFO keyspace`.
fn parse_keyspace(text: &str) -> Vec<DbInfo> {
    let mut dbs: Vec<DbInfo> = (0..DATABASES)
        .map(|index| DbInfo {
            index,
            keys: 0,
            expires: 0,
        })
        .collect();
    for line in text.lines() {
        let Some((name, stats)) = line.trim().split_once(':') else {
            continue;
        };
        let Some(index) = name.strip_prefix("db").and_then(|n| n.parse::<u8>().ok()) else {
            continue;
        };
        let stat = |key: &str| {
            stats
                .split(',')
                .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0)
        };
        let info = DbInfo {
            index,
            keys: stat("keys"),
            expires: stat("expires"),
        };
        match dbs.iter_mut().find(|d| d.index == index) {
            Some(d) => *d = info,
            None => dbs.push(info),
        }
    }
    dbs
}

fn connection_config(tunnel: Option<SocketAddr>) -> AsyncConnectionConfig {
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(Some(BROWSE_TIMEOUT));
    match tunnel {
        Some(addr) => config.set_dns_resolver(ViaTunnel(addr)),
        None => config,
    }
}

fn read_file(path: &str, what: &str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| CoreError::Config {
        message: format!("cannot read {what} {path}: {e}"),
    })
}

/// A client for `profile`. TLS is checked against the profile's host name
/// even through an SSH tunnel, since the tunnel is reached by resolving
/// that name to the tunnel's port.
fn client(profile: &ConnectionProfile, db: u8) -> Result<Client> {
    let ssl = &profile.ssl;
    let host = profile.host.trim().to_string();
    let addr = match ssl.mode {
        SslMode::Disabled => ConnectionAddr::Tcp(host, profile.port),
        mode => ConnectionAddr::TcpTls {
            host,
            port: profile.port,
            insecure: mode == SslMode::Required,
            tls_params: None,
        },
    };
    let mut redis = RedisConnectionInfo::default().set_db(i64::from(db));
    if !profile.user.trim().is_empty() {
        redis = redis.set_username(profile.user.trim());
    }
    if let Some(pw) = profile.password.as_deref().filter(|p| !p.is_empty()) {
        redis = redis.set_password(pw);
    }
    let info = addr.into_connection_info()?.set_redis_settings(redis);
    if matches!(ssl.mode, SslMode::Disabled | SslMode::Required) {
        return Ok(Client::open(info)?);
    }
    let client_tls = match (
        ssl.cert_path.as_deref().filter(|p| !p.is_empty()),
        ssl.key_path.as_deref().filter(|p| !p.is_empty()),
    ) {
        (Some(cert), Some(key)) => Some(ClientTlsConfig {
            client_cert: read_file(cert, "client certificate")?,
            client_key: read_file(key, "client key")?,
        }),
        (None, None) => None,
        _ => {
            return Err(CoreError::Config {
                message: "client certificate and key must be set together".into(),
            });
        }
    };
    let root_cert = match ssl.ca_path.as_deref().filter(|p| !p.is_empty()) {
        Some(ca) => Some(read_file(ca, "CA certificate")?),
        None => None,
    };
    let client = Client::build_with_tls(
        info,
        TlsCertificates {
            client_tls,
            root_cert,
        },
    )?;
    if ssl.mode == SslMode::VerifyCa {
        // Chain only; e.g. GCP Memorystore is reached by IP address.
        let info = client.get_connection_info().clone();
        let mut addr = info.addr().clone();
        addr.set_danger_accept_invalid_hostnames(true);
        return Ok(Client::open(info.set_addr(addr))?);
    }
    Ok(client)
}

async fn open_link(profile: &ConnectionProfile, db: u8) -> Result<Link> {
    let tunnel = match &profile.ssh {
        Some(ssh) => Some(SshTunnel::open(ssh, profile.host.trim(), profile.port).await?),
        None => None,
    };
    let via = tunnel
        .as_ref()
        .map(|t| SocketAddr::from(([127, 0, 0, 1], t.local_port())));
    let client = client(profile, db)?;
    let browse = client
        .get_multiplexed_async_connection_with_config(&connection_config(via))
        .await?;
    Ok(Link {
        client,
        browse,
        browse_db: db,
        _tunnel: tunnel,
    })
}

async fn server_info(profile: &ConnectionProfile, link: &mut Link) -> Result<SessionInfo> {
    let text: String = ::redis::cmd("INFO")
        .arg("server")
        .query_async(&mut link.browse)
        .await?;
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(name)?.strip_prefix(':'))
            .map(str::to_string)
    };
    let server_version = match (field("valkey_version"), field("redis_version")) {
        (Some(v), _) => format!("Valkey {v}"),
        (None, Some(v)) => format!("Redis {v}"),
        (None, None) => "Redis".into(),
    };
    let id: Option<i64> = ::redis::cmd("CLIENT")
        .arg("ID")
        .query_async(&mut link.browse)
        .await
        .ok();
    Ok(SessionInfo {
        server_version,
        connection_id: id.unwrap_or(0) as u32,
        tls: profile.ssl.mode != SslMode::Disabled,
        ssh_host_fingerprint: link
            ._tunnel
            .as_ref()
            .map(|t| t.host_fingerprint().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyspace_lists_sixteen_databases() {
        let dbs = parse_keyspace(
            "# Keyspace\r\ndb0:keys=12,expires=3,avg_ttl=100\r\ndb5:keys=1,expires=0,avg_ttl=0\r\ndb20:keys=2,expires=0,avg_ttl=0\r\n",
        );
        assert_eq!(dbs.len(), 17);
        assert_eq!((dbs[0].keys, dbs[0].expires), (12, 3));
        assert_eq!(dbs[5].keys, 1);
        assert_eq!(dbs[1].keys, 0);
        assert_eq!(dbs[16].index, 20);
    }

    #[test]
    fn binary_keys_round_trip_as_hex() {
        let k = KeyName::from_bytes(&[0xff, 0x00, b'a']);
        assert!(k.binary);
        assert_eq!(k.key, "FF0061");
        assert_eq!(k.bytes().unwrap(), vec![0xff, 0x00, b'a']);
        let plain = KeyName::from_bytes(b"user:1");
        assert_eq!((plain.key.as_str(), plain.binary), ("user:1", false));
    }
}
