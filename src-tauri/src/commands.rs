//! Tauri commands: a thin layer over `mydb-core`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mydb_core::report::{self, ConnectionSecrets, ReportOptions};
use mydb_core::{
    AppSettings, ColumnDef, ConnectionProfile, CoreError, DEFAULT_MAX_ROWS, DatabaseInfo,
    ExecutionOutcome, ExecutionRecord, PinnedTab, RecordContext, RecordFilter, Result, Session,
    SessionInfo, Snippet, Store, TableInfo,
};
use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::RwLock;

pub struct AppState {
    pub store: Mutex<Store>,
    /// Open sessions by session id. One connection profile can have several.
    pub sessions: RwLock<HashMap<String, Arc<Session>>>,
    /// The report shown in the print window, served by the `mydbreport:`
    /// protocol.
    pub print_html: Mutex<String>,
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
}

#[tauri::command]
pub async fn execute(
    state: State<'_, AppState>,
    session_id: String,
    sql: String,
    max_rows: Option<usize>,
    record: Option<bool>,
) -> Result<ExecuteResponse> {
    let session = state.session(&session_id).await?;
    let outcome = session
        .execute(&sql, max_rows.unwrap_or(DEFAULT_MAX_ROWS))
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
    Ok(ExecuteResponse {
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
        let known: Vec<ConnectionSecrets> = store
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
            .collect();
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

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}
