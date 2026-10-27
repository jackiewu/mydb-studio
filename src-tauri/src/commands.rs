//! Tauri commands: a thin layer over `mydb-core`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mydb_core::admin;
use mydb_core::advisor::{self, Advice, Finding};
use mydb_core::ai::{AiChat, AiChatSummary, AiContext, ChatMessage, ServerConfig, TableDdl};
use mydb_core::ddl;
use mydb_core::export::{ExportOptions, RowWriter};
use mydb_core::redis::server::{ClientInfo, CommandStat, RedisOverview, SlowEntry};
use mydb_core::redis::{self as kv, DbInfo, KeyInfo, KeyName, RedisOutcome, ScanPage, ValuePage};
use mydb_core::report::{self, ConnectionSecrets, ReportOptions};
use mydb_core::{
    AnySession, AppSettings, ColumnDef, ConnectionKind, ConnectionProfile, CoreError,
    DEFAULT_MAX_ROWS, DatabaseInfo, ExecutionOutcome, ExecutionRecord, PinnedTab, RecordContext,
    RecordFilter, RedisSession, Result, Session, SessionInfo, Snippet, Store, TableInfo,
};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::RwLock;

pub struct AppState {
    pub store: Mutex<Store>,
    /// Open sessions by session id. One connection profile can have several.
    pub sessions: RwLock<HashMap<String, AnySession>>,
    /// The report shown in the print window, served by the `mydbreport:`
    /// protocol.
    pub print_html: Mutex<String>,
    /// The local AI model, started on demand.
    pub ai: mydb_core::ai::AiRuntime,
    pub lock: crate::lock::LockState,
    /// How the previous run ended, until the UI asks.
    pub crash: Mutex<Option<CrashInfo>>,
}

impl AppState {
    fn connection(&self, id: &str) -> Result<ConnectionProfile> {
        self.store
            .lock()
            .unwrap()
            .get_connection(id)?
            .ok_or_else(|| CoreError::Config {
                message: format!("unknown connection {id}"),
            })
    }

    async fn any_session(&self, id: &str) -> Result<AnySession> {
        self.sessions
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::NotConnected { id: id.to_string() })
    }

    /// A MySQL session; SQL commands refuse Redis ones.
    async fn session(&self, id: &str) -> Result<Arc<Session>> {
        match self.any_session(id).await? {
            AnySession::Mysql(s) => Ok(s),
            AnySession::Redis(_) => Err(CoreError::Config {
                message: "這是 Redis 連線，不能執行 SQL。".into(),
            }),
        }
    }

    async fn redis(&self, id: &str) -> Result<Arc<RedisSession>> {
        match self.any_session(id).await? {
            AnySession::Redis(s) => Ok(s),
            AnySession::Mysql(_) => Err(CoreError::Config {
                message: "這不是 Redis 連線。".into(),
            }),
        }
    }
}

/// Connects to the server `profile` points at, whichever kind it is.
async fn connect(profile: &ConnectionProfile) -> Result<AnySession> {
    Ok(match profile.kind {
        ConnectionKind::Mysql => AnySession::Mysql(Arc::new(Session::connect(profile).await?)),
        ConnectionKind::Redis => AnySession::Redis(Arc::new(RedisSession::connect(profile).await?)),
    })
}

#[tauri::command]
pub fn list_connections(state: State<'_, AppState>) -> Result<Vec<ConnectionProfile>> {
    state.store.lock().unwrap().list_connections()
}

/// One connection with its passwords, for editing.
#[tauri::command]
pub fn get_connection(state: State<'_, AppState>, id: String) -> Result<ConnectionProfile> {
    state.lock.ensure_unlocked()?;
    state.connection(&id)
}

#[tauri::command]
pub fn save_connection(
    state: State<'_, AppState>,
    mut profile: ConnectionProfile,
) -> Result<ConnectionProfile> {
    state.lock.ensure_unlocked()?;
    if profile.id.is_empty() {
        profile.id = uuid_v4();
    }
    state.store.lock().unwrap().save_connection(&profile)?;
    Ok(profile)
}

#[tauri::command]
pub fn set_favorite(state: State<'_, AppState>, id: String, favorite: bool) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().set_favorite(&id, favorite)
}

#[tauri::command]
pub fn delete_connection(state: State<'_, AppState>, id: String) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().delete_connection(&id)
}

/// Connects with an unsaved profile and disconnects again.
#[tauri::command]
pub async fn test_connection(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
) -> Result<SessionInfo> {
    state.lock.ensure_unlocked()?;
    let session = connect(&profile).await?;
    let info = session.info().clone();
    session.close().await?;
    Ok(info)
}

/// Database names on the server `profile` points at, for the form's
/// default database list. Connects just for this.
#[tauri::command]
pub async fn list_server_databases(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
) -> Result<Vec<String>> {
    state.lock.ensure_unlocked()?;
    let profile = ConnectionProfile {
        database: None,
        ..profile
    };
    let session = Session::connect(&profile).await?;
    let names = session
        .list_databases()
        .await
        .map(|dbs| dbs.into_iter().map(|d| d.name).collect());
    let _ = session.close().await;
    names
}

/// Writes the AWS RDS CA bundle that ships with the app to the app's data
/// folder and returns its path, for the SSL settings.
#[tauri::command]
pub fn aws_rds_ca_path(app: AppHandle) -> Result<String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| CoreError::Config {
            message: e.to_string(),
        })?
        .join("certs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("aws-rds-global-bundle.pem");
    let bundle = mydb_core::profile::AWS_RDS_CA_BUNDLE;
    if std::fs::read_to_string(&path).ok().as_deref() != Some(bundle) {
        std::fs::write(&path, bundle)?;
    }
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Serialize)]
pub struct OpenedSession {
    session_id: String,
    connection_id: String,
    kind: ConnectionKind,
    info: SessionInfo,
    current_database: Option<String>,
    /// Redis: the database selected.
    redis_db: Option<u8>,
}

#[tauri::command]
pub async fn open_session(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<OpenedSession> {
    state.lock.ensure_unlocked()?;
    let mut profile = state.connection(&connection_id)?;
    let session = connect(&profile).await?;
    let info = session.info().clone();
    let (current_database, redis_db) = match &session {
        AnySession::Mysql(s) => (s.current_database(), None),
        AnySession::Redis(s) => (None, Some(s.current_db())),
    };

    // Trust on first use: remember the SSH host key for next time.
    if let (Some(ssh), Some(fp)) = (profile.ssh.as_mut(), &info.ssh_host_fingerprint)
        && ssh.known_host_fingerprint.is_none()
    {
        ssh.known_host_fingerprint = Some(fp.clone());
        state.store.lock().unwrap().save_connection(&profile)?;
    }

    state
        .store
        .lock()
        .unwrap()
        .touch_connection(&connection_id)?;

    let session_id = uuid_v4();
    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);
    Ok(OpenedSession {
        session_id,
        connection_id,
        kind: profile.kind,
        info,
        current_database,
        redis_db,
    })
}

#[tauri::command]
pub async fn close_session(state: State<'_, AppState>, session_id: String) -> Result<()> {
    let session = state.sessions.write().await.remove(&session_id);
    // Only disconnect cleanly if no query is still holding the session.
    if let Some(session) = session {
        session.close().await?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct ExecuteResponse {
    #[serde(flatten)]
    outcome: ExecutionOutcome,
    /// Id of the saved record when recording was on.
    record_id: Option<i64>,
    /// The database in use after the run (a `USE` may have changed it).
    current_database: Option<String>,
    /// What the error means and how to fix it, when known.
    advice: Option<Advice>,
    /// Pre-run check results for the SQL that ran.
    findings: Vec<Finding>,
}

/// Whether llama.cpp and a model are available for AI suggestions.
#[tauri::command]
pub fn ai_status(app: AppHandle, state: State<'_, AppState>) -> Result<mydb_core::ai::AiStatus> {
    let settings = state.store.lock().unwrap().get_settings()?;
    let models = app
        .path()
        .app_data_dir()
        .map_err(|e| CoreError::Config {
            message: e.to_string(),
        })?
        .join("models");
    Ok(mydb_core::ai::status(
        &settings.ai_llama_server,
        &settings.ai_model_path,
        &models,
    ))
}

/// Whether the local model is loaded, for the status bar.
#[tauri::command]
pub async fn ai_running(state: State<'_, AppState>) -> Result<bool> {
    Ok(state.ai.is_running().await)
}

/// Checks SQL before it runs: risky statements and slow patterns.
#[tauri::command]
pub fn check_sql(sql: String) -> Vec<Finding> {
    advisor::check_sql(&sql)
}

#[tauri::command]
pub async fn execute(
    state: State<'_, AppState>,
    session_id: String,
    sql: String,
    max_rows: Option<usize>,
    auto_limit: Option<u64>,
    record: Option<bool>,
) -> Result<ExecuteResponse> {
    state.lock.ensure_unlocked()?;
    let session = state.session(&session_id).await?;
    let outcome = session
        .execute_limited(
            &sql,
            max_rows.unwrap_or(DEFAULT_MAX_ROWS),
            auto_limit.filter(|n| *n > 0),
        )
        .await;
    let forced = state
        .store
        .lock()
        .unwrap()
        .record_forced(&session.profile().id)?;
    let record_id = if forced || record.unwrap_or(false) {
        let p = session.profile();
        let rec = ExecutionRecord::from_outcome(
            RecordContext {
                connection_id: &p.id,
                connection_name: &p.name,
                server: p.server_label(),
                server_version: &session.info().server_version,
                database: session.current_database(),
            },
            &sql,
            &outcome,
        );
        Some(state.store.lock().unwrap().insert_record(&rec)?)
    } else {
        None
    };
    let advice = outcome
        .error
        .as_ref()
        .filter(|_| !outcome.cancelled)
        .and_then(advisor::error_advice);
    Ok(ExecuteResponse {
        advice,
        findings: advisor::check_sql(&sql)
            .into_iter()
            // The automatic LIMIT already covers it.
            .filter(|f| !(auto_limit > Some(0) && f.rule == "select_all_rows"))
            .collect(),
        outcome,
        record_id,
        current_database: session.current_database(),
    })
}

#[tauri::command]
pub fn list_records(
    state: State<'_, AppState>,
    filter: RecordFilter,
) -> Result<Vec<ExecutionRecord>> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().list_records(&filter)
}

#[tauri::command]
pub fn get_record(state: State<'_, AppState>, id: i64) -> Result<Option<ExecutionRecord>> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().get_record(id)
}

#[tauri::command]
pub fn set_record_note(state: State<'_, AppState>, id: i64, note: String) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().set_record_note(id, &note)
}

#[tauri::command]
pub fn delete_records(state: State<'_, AppState>, ids: Vec<i64>) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().delete_records(&ids)
}

#[tauri::command]
pub fn generate_report(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    options: ReportOptions,
) -> Result<String> {
    state.lock.ensure_unlocked()?;
    let store = state.store.lock().unwrap();
    let mut records = store.get_records(&ids)?;
    if options.hide_connection {
        let known = known_secrets(&store)?;
        report::redact(&mut records, &known);
    }
    Ok(report::render(
        &records,
        &options,
        &chrono::Utc::now().to_rfc3339(),
    ))
}

/// Opens an HTML report in its own window and starts printing it, which on
/// every platform offers saving as PDF.
#[tauri::command]
pub fn print_report(app: AppHandle, state: State<'_, AppState>, html: String) -> Result<()> {
    state.lock.ensure_unlocked()?;
    *state.print_html.lock().unwrap() = html;
    let failed = |e: tauri::Error| CoreError::Config {
        message: format!("無法開啟列印視窗：{e}"),
    };
    if let Some(window) = app.get_webview_window(PRINT_WINDOW) {
        window.eval("location.reload()").map_err(failed)?;
        window.set_focus().map_err(failed)?;
        return Ok(());
    }
    let url = PRINT_URL.parse().expect("valid print URL");
    WebviewWindowBuilder::new(&app, PRINT_WINDOW, WebviewUrl::CustomProtocol(url))
        .title("報表（列印／存成 PDF）")
        .inner_size(900.0, 1000.0)
        // No webview menu (and its Inspect Element) on the report.
        .initialization_script(
            "document.addEventListener('contextmenu', (e) => e.preventDefault());",
        )
        .on_page_load(|window, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                let _ = window.print();
            }
        })
        .build()
        .map_err(failed)?;
    Ok(())
}

pub const PRINT_WINDOW: &str = "report-print";
pub const PRINT_PROTOCOL: &str = "mydbreport";
#[cfg(any(windows, target_os = "android"))]
const PRINT_URL: &str = "http://mydbreport.localhost/";
#[cfg(not(any(windows, target_os = "android")))]
const PRINT_URL: &str = "mydbreport://localhost/";

/// Writes a generated report to the path the user picked in the save dialog.
#[tauri::command]
pub fn save_text_file(state: State<'_, AppState>, path: String, contents: String) -> Result<()> {
    state.lock.ensure_unlocked()?;
    std::fs::write(&path, contents)?;
    Ok(())
}

/// Hosts, users and names of the saved connections, to hide in exports.
fn known_secrets(store: &Store) -> Result<Vec<ConnectionSecrets>> {
    Ok(store
        .list_connections()?
        .into_iter()
        .map(|p| {
            let mut hosts = vec![p.host];
            let mut users = vec![p.user];
            if let Some(ssh) = p.ssh {
                hosts.push(ssh.host);
                users.push(ssh.user);
            }
            if let Some(iap) = p.iap {
                hosts.extend(
                    [iap.instance, iap.project]
                        .into_iter()
                        .filter(|s| !s.trim().is_empty()),
                );
            }
            ConnectionSecrets {
                id: p.id,
                name: p.name,
                hosts,
                users,
            }
        })
        .collect())
}

/// What went wrong last time, found at startup.
#[derive(Clone, Serialize)]
pub struct CrashInfo {
    /// The app didn't get to shut down normally.
    pub unclean_exit: bool,
    /// A Rust panic message and backtrace, when there was one.
    pub panic: Option<String>,
}

/// The previous run's crash, until [`clear_crash_info`]: the page may load
/// more than once (a reload), and each load should still offer it.
#[tauri::command]
pub fn crash_info(state: State<'_, AppState>) -> Option<CrashInfo> {
    state.crash.lock().unwrap().clone()
}

/// The previous run's crash was shown in the report dialog.
#[tauri::command]
pub fn clear_crash_info(state: State<'_, AppState>) {
    state.crash.lock().unwrap().take();
}

/// A bug report for GitHub with anything identifying scrubbed: see
/// [`report::redact_text`].
#[tauri::command]
pub fn bug_report_body(
    app: AppHandle,
    state: State<'_, AppState>,
    what: String,
    details: String,
) -> Result<String> {
    let known = known_secrets(&state.store.lock().unwrap())?;
    let what = report::redact_text(&what, &known);
    let details = report::redact_text(&details, &known);
    let details: String = details.chars().take(5000).collect();
    Ok(format!(
        "### 發生什麼事\n{what}\n\n### 環境\n- MyDB Studio {}\n- {} {}\n\n### 細節\n```\n{}\n```\n",
        app.package_info().version,
        std::env::consts::OS,
        std::env::consts::ARCH,
        details.trim()
    ))
}

const ISSUES_URL: &str = "https://github.com/jackiewu/mydb-studio/issues/new";

/// Opens the new-issue page, filled in, in the default browser. Nothing is
/// sent until the user submits it there.
#[tauri::command]
pub fn open_bug_report(title: String, body: String) -> Result<()> {
    fn encode(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    let url = format!(
        "{ISSUES_URL}?labels=bug&title={}&body={}",
        encode(&title),
        encode(&body)
    );
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(&url).spawn()?;
    Ok(())
}

/// Reads a text file the user picked in the open dialog.
#[tauri::command]
pub fn read_text_file(state: State<'_, AppState>, path: String) -> Result<String> {
    state.lock.ensure_unlocked()?;
    let bytes = std::fs::read(&path)?;
    if bytes.len() > 20 * 1024 * 1024 {
        return Err(CoreError::Config {
            message: "檔案太大（超過 20 MB）。".into(),
        });
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Formats a result set as a `|`-separated text table.
fn text_table(r: &mydb_core::session::StatementResult) -> String {
    let mut out = r
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    for row in &r.rows {
        out.push('\n');
        let cells: Vec<String> = row
            .iter()
            .map(|v| match v {
                serde_json::Value::Null => "NULL".to_string(),
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        out.push_str(&cells.join(" | "));
    }
    out
}

/// Gathers what the AI is told about `sql`: the server, the database, the
/// structure of the tables it names, and EXPLAIN for its queries. Never row
/// data. Works without a session too, with only what was passed in.
#[tauri::command]
pub async fn ai_context(
    state: State<'_, AppState>,
    session_id: Option<String>,
    sql: String,
    error: Option<String>,
    explain: Option<String>,
) -> Result<AiContext> {
    state.lock.ensure_unlocked()?;
    let mut ctx = AiContext {
        sql: sql.clone(),
        error,
        findings: advisor::check_sql(&sql)
            .into_iter()
            .map(|f| format!("第 {} 句：{}", f.statement + 1, f.message))
            .collect(),
        ..Default::default()
    };
    let session = match session_id {
        Some(id) => state.session(&id).await.ok(),
        None => None,
    };
    let Some(session) = session else {
        ctx.explain = explain;
        return Ok(ctx);
    };
    ctx.server = session.info().server_version.clone();
    ctx.database = session.current_database();

    for (db, table) in advisor::referenced_tables(&sql).into_iter().take(6) {
        if let Ok(ddl) = session.show_create_table(db.as_deref(), &table).await
            && !ddl.is_empty()
        {
            let name = match db {
                Some(db) => format!("{db}.{table}"),
                None => table,
            };
            ctx.tables.push(TableDdl { name, ddl });
        }
    }
    if let Some(db) = &ctx.database
        && let Ok(tables) = session.list_tables(db).await
    {
        ctx.other_tables = tables
            .into_iter()
            .map(|t| t.name)
            .filter(|n| !ctx.tables.iter().any(|t| t.name.ends_with(n.as_str())))
            .take(200)
            .collect();
    }

    // EXPLAIN only reads the plan; limited to plain queries all the same.
    ctx.explain = match explain {
        Some(e) => Some(e),
        None => {
            let mut plans = Vec::new();
            for stmt in mydb_core::split::split_statements(&sql).iter().take(3) {
                let head = stmt.trim_start().to_uppercase();
                if !(head.starts_with("SELECT") || head.starts_with("WITH")) {
                    continue;
                }
                let outcome = session.execute(&format!("EXPLAIN {stmt}"), 50).await;
                if let Some(r) = outcome.results.first()
                    && outcome.error.is_none()
                {
                    plans.push(text_table(r));
                }
            }
            (!plans.is_empty()).then(|| plans.join("\n\n"))
        }
    };
    Ok(ctx)
}

fn ai_config(settings: &AppSettings) -> ServerConfig {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    ServerConfig {
        llama_server: settings.ai_llama_server.clone(),
        model_path: settings.ai_model_path.clone(),
        threads: if settings.ai_threads == 0 {
            (cores / 2).max(1)
        } else {
            settings.ai_threads as usize
        },
        context: 8192,
        external_url: settings
            .ai_use_external
            .then(|| settings.ai_server_url.clone()),
    }
}

/// Progress of an AI answer, streamed to the drawer.
#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiEvent {
    /// The model is being loaded (the first question after a while).
    Starting,
    /// The model has the question.
    Thinking,
    Token {
        text: String,
    },
}

/// Asks the local model, streaming the answer through `on_event`. Starts
/// the model when needed.
#[tauri::command]
pub async fn ai_chat(
    state: State<'_, AppState>,
    context: AiContext,
    messages: Vec<ChatMessage>,
    on_event: Channel<AiEvent>,
) -> Result<String> {
    state.lock.ensure_unlocked()?;
    let settings = state.store.lock().unwrap().get_settings()?;
    if !settings.ai_enabled {
        return Err(CoreError::Config {
            message: "AI 建議還沒開啟，請到設定的「AI 建議」開啟並選擇模型。".into(),
        });
    }
    let cfg = ai_config(&settings);
    if !state.ai.is_ready(&cfg).await {
        let _ = on_event.send(AiEvent::Starting);
    }
    let base = state.ai.ensure(&cfg).await?;
    let _ = on_event.send(AiEvent::Thinking);
    let msgs = mydb_core::ai::build_messages(&context, &messages);
    state
        .ai
        .chat(&base, &msgs, 1024, |piece| {
            let _ = on_event.send(AiEvent::Token {
                text: piece.to_string(),
            });
        })
        .await
}

/// Stops the answer being generated.
#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>) {
    state.ai.cancel();
}

#[tauri::command]
pub fn save_ai_chat(state: State<'_, AppState>, chat: AiChat) -> Result<i64> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().save_ai_chat(&chat)
}

#[tauri::command]
pub fn list_ai_chats(state: State<'_, AppState>) -> Result<Vec<AiChatSummary>> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().list_ai_chats(300)
}

#[tauri::command]
pub fn get_ai_chat(state: State<'_, AppState>, id: i64) -> Result<Option<AiChat>> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().get_ai_chat(id)
}

#[tauri::command]
pub fn delete_ai_chat(state: State<'_, AppState>, id: i64) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().delete_ai_chat(id)
}

#[tauri::command]
pub async fn list_databases(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<DatabaseInfo>> {
    state.lock.ensure_unlocked()?;
    state.session(&session_id).await?.list_databases().await
}

#[tauri::command]
pub async fn list_tables(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<Vec<TableInfo>> {
    state.lock.ensure_unlocked()?;
    state
        .session(&session_id)
        .await?
        .list_tables(&database)
        .await
}

#[tauri::command]
pub async fn use_database(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state
        .session(&session_id)
        .await?
        .use_database(&database)
        .await
}

/// Table names per database, for autocomplete.
#[tauri::command]
pub async fn list_all_tables(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<(String, String)>> {
    state.lock.ensure_unlocked()?;
    state.session(&session_id).await?.list_all_tables().await
}

#[tauri::command]
pub async fn list_columns(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<Vec<ColumnDef>> {
    state.lock.ensure_unlocked()?;
    state
        .session(&session_id)
        .await?
        .list_columns(&database)
        .await
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings> {
    state.store.lock().unwrap().get_settings()
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: AppSettings) -> Result<()> {
    // Settings include where AI prompts go and what program runs the model.
    state.lock.ensure_unlocked()?;
    let store = state.store.lock().unwrap();
    state
        .lock
        .check_settings_change(&store.get_settings()?, &settings)?;
    store.save_settings(&settings)
}

#[tauri::command]
pub fn list_snippets(state: State<'_, AppState>) -> Result<Vec<Snippet>> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().list_snippets()
}

#[tauri::command]
pub fn save_snippet(state: State<'_, AppState>, snippet: Snippet) -> Result<Snippet> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().save_snippet(&snippet)
}

#[tauri::command]
pub fn delete_snippet(state: State<'_, AppState>, id: i64) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().delete_snippet(id)
}

#[tauri::command]
pub fn list_pinned_tabs(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<Vec<PinnedTab>> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().list_pinned_tabs(&connection_id)
}

#[tauri::command]
pub fn save_pinned_tab(state: State<'_, AppState>, mut tab: PinnedTab) -> Result<PinnedTab> {
    state.lock.ensure_unlocked()?;
    let store = state.store.lock().unwrap();
    // A Redis tab may hold AUTH lines; their passwords are never kept.
    // (Only the kind is needed, so the passwords stay where they are.)
    if store
        .list_connections()?
        .iter()
        .any(|p| p.id == tab.connection_id && p.kind == ConnectionKind::Redis)
    {
        tab.sql = kv::mask_text(&tab.sql);
    }
    store.save_pinned_tab(&tab)
}

#[tauri::command]
pub fn delete_pinned_tab(state: State<'_, AppState>, id: i64) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state.store.lock().unwrap().delete_pinned_tab(id)
}

#[tauri::command]
pub fn delete_draft_tabs(state: State<'_, AppState>, connection_id: String) -> Result<()> {
    state.lock.ensure_unlocked()?;
    state
        .store
        .lock()
        .unwrap()
        .delete_draft_tabs(&connection_id)
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[tauri::command]
pub async fn server_overview(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<admin::ServerOverview> {
    state.lock.ensure_unlocked()?;
    admin::overview(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn server_processes(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<admin::ProcessInfo>> {
    state.lock.ensure_unlocked()?;
    admin::processes(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn server_users(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<admin::UserAccount>> {
    state.lock.ensure_unlocked()?;
    admin::users(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn user_grants(
    state: State<'_, AppState>,
    session_id: String,
    user: String,
    host: String,
) -> Result<Vec<admin::Grant>> {
    state.lock.ensure_unlocked()?;
    admin::grants(&*state.session(&session_id).await?, &user, &host).await
}

/// The statements `change` would run, passwords masked, for the user to
/// confirm.
#[tauri::command]
pub async fn admin_plan(
    state: State<'_, AppState>,
    session_id: String,
    change: admin::AdminChange,
) -> Result<Vec<String>> {
    state.lock.ensure_unlocked()?;
    let session = state.session(&session_id).await?;
    admin::plan(&change, admin::dialect(&session).await?, true)
}

/// Runs `change`. Admin statements are not recorded, so passwords never
/// reach the history.
#[tauri::command]
pub async fn admin_apply(
    state: State<'_, AppState>,
    session_id: String,
    change: admin::AdminChange,
) -> Result<Vec<String>> {
    state.lock.ensure_unlocked()?;
    admin::apply(&*state.session(&session_id).await?, &change).await
}

#[tauri::command]
pub async fn schema_meta(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<ddl::SchemaMeta> {
    state.lock.ensure_unlocked()?;
    ddl::meta(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn database_options(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<ddl::DatabaseOptions> {
    state.lock.ensure_unlocked()?;
    ddl::database_options(&*state.session(&session_id).await?, &database).await
}

#[tauri::command]
pub async fn table_design(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
    table: String,
) -> Result<ddl::TableDesign> {
    state.lock.ensure_unlocked()?;
    ddl::table_design(&*state.session(&session_id).await?, &database, &table).await
}

/// `SHOW CREATE TABLE` (or VIEW) text.
#[tauri::command]
pub async fn show_create_table(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
    table: String,
) -> Result<String> {
    state.lock.ensure_unlocked()?;
    state
        .session(&session_id)
        .await?
        .show_create_table(Some(&database), &table)
        .await
}

#[tauri::command]
pub async fn schema_objects(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<ddl::ObjectList> {
    state.lock.ensure_unlocked()?;
    ddl::objects(&*state.session(&session_id).await?, &database).await
}

#[tauri::command]
pub async fn object_definition(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
    kind: ddl::ObjectKind,
    name: String,
) -> Result<ddl::ObjectDefinition> {
    state.lock.ensure_unlocked()?;
    ddl::definition(&*state.session(&session_id).await?, &database, kind, &name).await
}

/// The statements a schema change would run, for the user to confirm.
#[tauri::command]
pub async fn schema_plan(
    state: State<'_, AppState>,
    session_id: String,
    change: ddl::SchemaChange,
) -> Result<ddl::DdlPlan> {
    state.lock.ensure_unlocked()?;
    ddl::plan(&*state.session(&session_id).await?, &change).await
}

#[derive(Serialize)]
pub struct SchemaApplied {
    /// What ran, including a restore after a failed re-create.
    statements: Vec<String>,
    error: Option<CoreError>,
    record_id: Option<i64>,
    /// The session's database afterwards, which a change may have selected.
    current_database: Option<String>,
}

/// Runs a schema change. Every run is recorded, whether or not recording is
/// on, with the definition it replaced in the note.
#[tauri::command]
pub async fn schema_apply(
    state: State<'_, AppState>,
    session_id: String,
    change: ddl::SchemaChange,
) -> Result<SchemaApplied> {
    state.lock.ensure_unlocked()?;
    let session = state.session(&session_id).await?;
    let run = ddl::apply(&session, &change).await?;
    let p = session.profile();
    let mut rec = ExecutionRecord::from_outcome(
        RecordContext {
            connection_id: &p.id,
            connection_name: &p.name,
            server: p.server_label(),
            server_version: &session.info().server_version,
            database: run.database.clone().or_else(|| session.current_database()),
        },
        &run.sql,
        &run.outcome,
    );
    rec.note = run.note;
    let record_id = state.store.lock().unwrap().insert_record(&rec).ok();
    Ok(SchemaApplied {
        statements: run.outcome.results.iter().map(|r| r.sql.clone()).collect(),
        error: run.outcome.error,
        record_id,
        current_database: session.current_database(),
    })
}

/// Stops the statement (or Redis command) running on this session, if any.
#[tauri::command]
pub async fn cancel_query(state: State<'_, AppState>, session_id: String) -> Result<bool> {
    match state.any_session(&session_id).await? {
        AnySession::Mysql(s) => s.cancel().await,
        AnySession::Redis(s) => Ok(s.cancel()),
    }
}

/// Round trip to the server in milliseconds, for the status bar; `None`
/// when the connection is busy. Never reconnects.
#[tauri::command]
pub async fn session_ping(state: State<'_, AppState>, session_id: String) -> Result<Option<f64>> {
    state.any_session(&session_id).await?.ping().await
}

/// Seconds an idle connection is kept by the server: MySQL's
/// `@@wait_timeout`; `None` for Redis.
#[tauri::command]
pub async fn idle_timeout(state: State<'_, AppState>, session_id: String) -> Result<Option<u64>> {
    match state.any_session(&session_id).await? {
        AnySession::Mysql(s) => s.wait_timeout().await,
        AnySession::Redis(_) => Ok(None),
    }
}

#[tauri::command]
pub async fn redis_server_overview(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<RedisOverview> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.server_overview().await
}

#[tauri::command]
pub async fn redis_clients(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<ClientInfo>> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.clients().await
}

#[tauri::command]
pub async fn redis_slowlog(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<SlowEntry>> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.slowlog().await
}

#[tauri::command]
pub async fn redis_command_stats(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<CommandStat>> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.command_stats().await
}

/// `CONFIG GET *`, passwords shown as `****`.
#[tauri::command]
pub async fn redis_config(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<(String, String)>> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.config().await
}

/// `CLIENT KILL ID id`, after the UI confirmed it (rule
/// `redis_client_kill`).
#[tauri::command]
pub async fn redis_kill_client(
    state: State<'_, AppState>,
    session_id: String,
    id: u64,
) -> Result<bool> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.kill_client(id).await
}

#[tauri::command]
pub async fn redis_databases(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<DbInfo>> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.databases().await
}

/// The next batch of keys; only ever `SCAN`, never `KEYS`.
#[tauri::command]
pub async fn redis_scan(
    state: State<'_, AppState>,
    session_id: String,
    db: u8,
    cursor: String,
    pattern: String,
) -> Result<ScanPage> {
    state.lock.ensure_unlocked()?;
    state
        .redis(&session_id)
        .await?
        .scan(db, &cursor, pattern.trim())
        .await
}

#[tauri::command]
pub async fn redis_key_info(
    state: State<'_, AppState>,
    session_id: String,
    db: u8,
    key: KeyName,
) -> Result<KeyInfo> {
    state.lock.ensure_unlocked()?;
    state.redis(&session_id).await?.key_info(db, &key).await
}

#[tauri::command]
pub async fn redis_read_value(
    state: State<'_, AppState>,
    session_id: String,
    db: u8,
    key: KeyName,
    from: String,
) -> Result<ValuePage> {
    state.lock.ensure_unlocked()?;
    state
        .redis(&session_id)
        .await?
        .read_value(db, &key, &from)
        .await
}

/// Commands in `text` to confirm before running.
#[tauri::command]
pub fn redis_check(text: String) -> Vec<Finding> {
    kv::command_lines(&text)
        .into_iter()
        .filter_map(|(line, raw)| kv::check_command(line, &kv::split_args(raw).ok()?))
        .collect()
}

#[derive(Serialize)]
pub struct RedisExecuteResponse {
    #[serde(flatten)]
    outcome: RedisOutcome,
    record_id: Option<i64>,
}

/// Runs the command lines in `text`, after the UI confirmed any that
/// [`redis_check`] flagged.
#[tauri::command]
pub async fn redis_execute(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
    record: Option<bool>,
) -> Result<RedisExecuteResponse> {
    state.lock.ensure_unlocked()?;
    let session = state.redis(&session_id).await?;
    let outcome = session.execute(&text).await;
    let forced = state
        .store
        .lock()
        .unwrap()
        .record_forced(&session.profile().id)?;
    let record_id = if forced || record.unwrap_or(false) {
        let rec = session.record(&text, &outcome);
        Some(state.store.lock().unwrap().insert_record(&rec)?)
    } else {
        None
    };
    Ok(RedisExecuteResponse { outcome, record_id })
}

/// Runs `sql` again and writes every row to `path`, without the row limit of
/// the result grid. Only statements that just read are allowed, since the
/// statement runs a second time. Returns the number of rows written; a
/// failed or stopped export leaves no file behind.
#[tauri::command]
pub async fn export_query(
    state: State<'_, AppState>,
    session_id: String,
    sql: String,
    options: ExportOptions,
    path: String,
) -> Result<u64> {
    state.lock.ensure_unlocked()?;
    if !advisor::is_plain_read(&sql) {
        return Err(CoreError::Config {
            message: "只能匯出查詢（SELECT、SHOW 等）的結果，這句會修改資料或鎖定資料列。".into(),
        });
    }
    let session = state.session(&session_id).await?;
    let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let writer = Mutex::new(Some(RowWriter::new(file, options)));
    let streamed = session
        .stream_rows(
            &sql,
            |columns| writer.lock().unwrap().as_mut().unwrap().begin(columns),
            |row| writer.lock().unwrap().as_mut().unwrap().row(&row),
        )
        .await;
    let finished = streamed.and_then(|_| {
        let w = writer.lock().unwrap().take().unwrap();
        Ok(w.finish()?)
    });
    if finished.is_err() {
        drop(writer);
        let _ = std::fs::remove_file(&path);
    }
    finished
}

/// How to add this app as an MCP server in an AI tool.
#[derive(Serialize)]
pub struct McpSetup {
    /// The executable to run with `--mcp`.
    pub command: String,
    /// `mcpServers` entry for Claude Desktop and similar clients.
    pub config_json: String,
    /// One line for Claude Code.
    pub claude_code: String,
}

#[tauri::command]
pub fn mcp_setup() -> Result<McpSetup> {
    let exe = std::env::current_exe()?;
    let command = exe.to_string_lossy().into_owned();
    let config = serde_json::json!({
        "mcpServers": { "mydb-studio": { "command": command, "args": ["--mcp"] } }
    });
    let quoted = if command.contains(' ') {
        format!("\"{command}\"")
    } else {
        command.clone()
    };
    Ok(McpSetup {
        config_json: serde_json::to_string_pretty(&config)?,
        claude_code: format!("claude mcp add mydb-studio -- {quoted} --mcp"),
        command,
    })
}
