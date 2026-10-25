mod commands;
mod keychain;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use mydb_core::Store;
use tauri::Manager;

pub use commands::AppState;

/// Present while the app runs; left behind when it doesn't exit normally.
const RUNNING_FLAG: &str = "running.flag";
const PANIC_LOG: &str = "last-panic.txt";

/// Finds how the previous run ended, and sets up this run's tracking: the
/// running flag, and a panic hook that writes the panic to a file.
fn crash_watch(dir: &Path) -> Option<commands::CrashInfo> {
    let flag = dir.join(RUNNING_FLAG);
    let log = dir.join(PANIC_LOG);
    let unclean_exit = flag.exists();
    let panic = std::fs::read_to_string(&log).ok();
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::write(&flag, std::process::id().to_string());

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = format!("{info}\n\n{}", std::backtrace::Backtrace::force_capture());
        let _ = std::fs::write(&log, text);
        default_hook(info);
    }));

    (unclean_exit || panic.is_some()).then_some(commands::CrashInfo {
        unclean_exit,
        panic,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let mut store = Store::open(dir.join("mydb-studio.db"))?;
            if let Some(vault) = keychain::vault() {
                store.use_vault(vault)?;
            }
            let crash = crash_watch(&dir);
            let ai = mydb_core::ai::AiRuntime::new(dir.join("llama-server.pid"));
            ai.stop_leftover();
            app.manage(AppState {
                store: Mutex::new(store),
                sessions: tokio::sync::RwLock::new(HashMap::new()),
                print_html: Mutex::new(String::new()),
                ai,
                crash: Mutex::new(crash),
            });
            // Stop the AI model once it sits unused for the configured time.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    let state = handle.state::<AppState>();
                    let minutes = state
                        .store
                        .lock()
                        .unwrap()
                        .get_settings()
                        .map_or(10, |s| s.ai_idle_minutes);
                    state
                        .ai
                        .stop_if_idle(Duration::from_secs(u64::from(minutes.max(1)) * 60))
                        .await;
                }
            });
            Ok(())
        })
        .register_uri_scheme_protocol(commands::PRINT_PROTOCOL, |ctx, _request| {
            let html = ctx
                .app_handle()
                .state::<AppState>()
                .print_html
                .lock()
                .unwrap()
                .clone();
            tauri::http::Response::builder()
                .header("Content-Type", "text/html; charset=utf-8")
                .body(html.into_bytes())
                .unwrap()
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connections,
            commands::save_connection,
            commands::delete_connection,
            commands::set_favorite,
            commands::test_connection,
            commands::list_server_databases,
            commands::aws_rds_ca_path,
            commands::open_session,
            commands::close_session,
            commands::execute,
            commands::list_databases,
            commands::list_tables,
            commands::list_records,
            commands::get_record,
            commands::set_record_note,
            commands::delete_records,
            commands::generate_report,
            commands::save_text_file,
            commands::print_report,
            commands::check_sql,
            commands::ai_status,
            commands::ai_context,
            commands::ai_chat,
            commands::ai_cancel,
            commands::save_ai_chat,
            commands::list_ai_chats,
            commands::get_ai_chat,
            commands::delete_ai_chat,
            commands::read_text_file,
            commands::take_crash_info,
            commands::bug_report_body,
            commands::open_bug_report,
            commands::use_database,
            commands::list_all_tables,
            commands::list_columns,
            commands::get_settings,
            commands::save_settings,
            commands::list_snippets,
            commands::save_snippet,
            commands::delete_snippet,
            commands::list_pinned_tabs,
            commands::save_pinned_tab,
            commands::delete_pinned_tab,
            commands::delete_draft_tabs,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<AppState>().ai.kill_now();
                if let Ok(dir) = app.path().app_data_dir() {
                    let _ = std::fs::remove_file(dir.join(RUNNING_FLAG));
                }
            }
        });
}
