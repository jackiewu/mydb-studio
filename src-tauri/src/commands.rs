//! Tauri commands: a thin layer over `mydb-core`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mydb_core::admin;
use mydb_core::advisor::{self, Advice, Finding};
use mydb_core::ai::{AiChat, AiChatSummary, AiContext, ChatMessage, ServerConfig, TableDdl};
use mydb_core::export::{ExportOptions, RowWriter};
use mydb_core::report::{self, ConnectionSecrets, ReportOptions};
use mydb_core::{
    AppSettings, ColumnDef, ConnectionProfile, CoreError, DEFAULT_MAX_ROWS, DatabaseInfo,
    ExecutionOutcome, ExecutionRecord, PinnedTab, RecordContext, RecordFilter, Result, Session,
    SessionInfo, Snippet, Store, TableInfo,
};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::RwLock;

pub struct AppState {
    pub store: Mutex<Store>,
    /// Open sessions by session id. One connection profile can have several.
    pub sessions: RwLock<HashMap<String, Arc<Session>>>,
    /// The report shown in the print window, served by the `mydbreport:`
    /// protocol.
    pub print_html: Mutex<String>,
    /// The local AI model, started on demand.
    pub ai: mydb_core::ai::AiRuntime,
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

    async fn session(&self, id: &str) -> Result<Arc<Session>> {
        self.sessions
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| CoreError::NotConnected { id: id.to_string() })
    }
}

#[tauri::command]
pub fn list_connections(state: State<'_, AppState>) -> Result<Vec<ConnectionProfile>> {
    state.store.lock().unwrap().list_connections()
}

#[tauri::command]
pub fn save_connection(
    state: State<'_, AppState>,
    mut profile: ConnectionProfile,
) -> Result<ConnectionProfile> {
    if profile.id.is_empty() {
        profile.id = uuid_v4();
    }
    state.store.lock().unwrap().save_connection(&profile)?;
    Ok(profile)
}

#[tauri::command]
pub fn set_favorite(state: State<'_, AppState>, id: String, favorite: bool) -> Result<()> {
    state.store.lock().unwrap().set_favorite(&id, favorite)
}

#[tauri::command]
pub fn delete_connection(state: State<'_, AppState>, id: String) -> Result<()> {
    state.store.lock().unwrap().delete_connection(&id)
}

/// Connects with an unsaved profile and disconnects again.
#[tauri::command]
pub async fn test_connection(profile: ConnectionProfile) -> Result<SessionInfo> {
    let session = Session::connect(&profile).await?;
    let info = session.info().clone();
    session.close().await?;
    Ok(info)
}

/// Database names on the server `profile` points at, for the form's
/// default database list. Connects just for this.
#[tauri::command]
pub async fn list_server_databases(profile: ConnectionProfile) -> Result<Vec<String>> {
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
    info: SessionInfo,
    current_database: Option<String>,
}

#[tauri::command]
pub async fn open_session(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<OpenedSession> {
    let mut profile = state.connection(&connection_id)?;
    let session = Session::connect(&profile).await?;
    let info = session.info().clone();
    let current_database = session.current_database();

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
        .insert(session_id.clone(), Arc::new(session));
    Ok(OpenedSession {
        session_id,
        connection_id,
        info,
        current_database,
    })
}

#[tauri::command]
pub async fn close_session(state: State<'_, AppState>, session_id: String) -> Result<()> {
    let session = state.sessions.write().await.remove(&session_id);
    // Only disconnect cleanly if no query is still holding the session.
    if let Some(session) = session.and_then(|s| Arc::try_unwrap(s).ok()) {
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
    let session = state.session(&session_id).await?;
    let outcome = session
        .execute_limited(
            &sql,
            max_rows.unwrap_or(DEFAULT_MAX_ROWS),
            auto_limit.filter(|n| *n > 0),
        )
        .await;
    let record_id = if record.unwrap_or(false) {
        let p = session.profile();
        let rec = ExecutionRecord::from_outcome(
            RecordContext {
                connection_id: &p.id,
                connection_name: &p.name,
                server: format!("{}@{}:{}", p.user, p.host, p.port),
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
    state.store.lock().unwrap().list_records(&filter)
}

#[tauri::command]
pub fn get_record(state: State<'_, AppState>, id: i64) -> Result<Option<ExecutionRecord>> {
    state.store.lock().unwrap().get_record(id)
}

#[tauri::command]
pub fn set_record_note(state: State<'_, AppState>, id: i64, note: String) -> Result<()> {
    state.store.lock().unwrap().set_record_note(id, &note)
}

#[tauri::command]
pub fn delete_records(state: State<'_, AppState>, ids: Vec<i64>) -> Result<()> {
    state.store.lock().unwrap().delete_records(&ids)
}

#[tauri::command]
pub fn generate_report(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    options: ReportOptions,
) -> Result<String> {
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
pub fn save_text_file(path: String, contents: String) -> Result<()> {
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

/// The previous run's crash, once: later calls return nothing.
#[tauri::command]
pub fn take_crash_info(state: State<'_, AppState>) -> Option<CrashInfo> {
    state.crash.lock().unwrap().take()
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
pub fn read_text_file(path: String) -> Result<String> {
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
    state.store.lock().unwrap().save_ai_chat(&chat)
}

#[tauri::command]
pub fn list_ai_chats(state: State<'_, AppState>) -> Result<Vec<AiChatSummary>> {
    state.store.lock().unwrap().list_ai_chats(300)
}

#[tauri::command]
pub fn get_ai_chat(state: State<'_, AppState>, id: i64) -> Result<Option<AiChat>> {
    state.store.lock().unwrap().get_ai_chat(id)
}

#[tauri::command]
pub fn delete_ai_chat(state: State<'_, AppState>, id: i64) -> Result<()> {
    state.store.lock().unwrap().delete_ai_chat(id)
}

#[tauri::command]
pub async fn list_databases(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<DatabaseInfo>> {
    state.session(&session_id).await?.list_databases().await
}

#[tauri::command]
pub async fn list_tables(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<Vec<TableInfo>> {
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
    state.session(&session_id).await?.list_all_tables().await
}

#[tauri::command]
pub async fn list_columns(
    state: State<'_, AppState>,
    session_id: String,
    database: String,
) -> Result<Vec<ColumnDef>> {
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
    state.store.lock().unwrap().save_settings(&settings)
}

#[tauri::command]
pub fn list_snippets(state: State<'_, AppState>) -> Result<Vec<Snippet>> {
    state.store.lock().unwrap().list_snippets()
}

#[tauri::command]
pub fn save_snippet(state: State<'_, AppState>, snippet: Snippet) -> Result<Snippet> {
    state.store.lock().unwrap().save_snippet(&snippet)
}

#[tauri::command]
pub fn delete_snippet(state: State<'_, AppState>, id: i64) -> Result<()> {
    state.store.lock().unwrap().delete_snippet(id)
}

#[tauri::command]
pub fn list_pinned_tabs(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<Vec<PinnedTab>> {
    state.store.lock().unwrap().list_pinned_tabs(&connection_id)
}

#[tauri::command]
pub fn save_pinned_tab(state: State<'_, AppState>, tab: PinnedTab) -> Result<PinnedTab> {
    state.store.lock().unwrap().save_pinned_tab(&tab)
}

#[tauri::command]
pub fn delete_pinned_tab(state: State<'_, AppState>, id: i64) -> Result<()> {
    state.store.lock().unwrap().delete_pinned_tab(id)
}

#[tauri::command]
pub fn delete_draft_tabs(state: State<'_, AppState>, connection_id: String) -> Result<()> {
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
    admin::overview(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn server_processes(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<admin::ProcessInfo>> {
    admin::processes(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn server_users(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<admin::UserAccount>> {
    admin::users(&*state.session(&session_id).await?).await
}

#[tauri::command]
pub async fn user_grants(
    state: State<'_, AppState>,
    session_id: String,
    user: String,
    host: String,
) -> Result<Vec<admin::Grant>> {
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
    admin::apply(&*state.session(&session_id).await?, &change).await
}

/// Stops the statement running on this session, if any.
#[tauri::command]
pub async fn cancel_query(state: State<'_, AppState>, session_id: String) -> Result<bool> {
    state.session(&session_id).await?.cancel().await
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
