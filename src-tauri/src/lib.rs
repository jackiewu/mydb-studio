mod commands;
mod keychain;
mod lock;
mod mcp;
mod windows;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use mydb_core::Store;
use tauri::Manager;

pub use commands::AppState;

use mydb_core::RUNNING_FLAG;
const PANIC_LOG: &str = "last-panic.txt";

/// Finds how the previous run ended, and sets up this run's tracking: the
/// running flag, and a panic hook that writes the panic to a file.
fn crash_watch(dir: &Path) -> Option<commands::CrashInfo> {
    let flag = dir.join(RUNNING_FLAG);
    let log = dir.join(PANIC_LOG);
    // A clean exit empties the flag (see `run`).
    let unclean_exit = std::fs::metadata(&flag).is_ok_and(|m| m.len() > 0);
    let panic = std::fs::read_to_string(&log).ok();
    let _ = std::fs::remove_file(&log);
    // Held for the whole run: the MCP server checks it to tell whether the
    // app is running, and the OS lets go when the process ends. Taken on a
    // thread, since another copy of the app may hold it for a while.
    std::thread::spawn(move || {
        loop {
            match mydb_core::running::hold(&flag) {
                Ok(held) => break std::mem::forget(held),
                Err(e) => {
                    eprintln!("running flag: {e}");
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
            }
        }
    });

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
    if std::env::args().skip(1).any(|a| a == "--mcp") {
        std::process::exit(mcp::main());
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let mut store = Store::open(dir.join("mydb-studio.db"))?;
            if let Some(vault) = keychain::vault() {
                store.use_vault(vault)?;
                store.trust_unbound_secrets();
            }
            // Locked before the running flag says the app runs, so the MCP
            // server never sees a running, unlocked app that should be locked.
            let lock = lock::LockState::new(
                dir.join(mydb_core::LOCK_FLAG),
                &store.get_settings().unwrap_or_default(),
            );
            let crash = crash_watch(&dir);
            let ai = mydb_core::ai::AiRuntime::new(dir.join("llama-server.pid"));
            ai.stop_leftover();
            app.manage(AppState {
                store: Mutex::new(store),
                sessions: tokio::sync::RwLock::new(HashMap::new()),
                print_html: Mutex::new(String::new()),
                ai,
                lock,
                crash: Mutex::new(crash),
                windows: windows::WindowSessions::default(),
            });
            #[cfg(target_os = "macos")]
            windows::install_menu(app)?;
            lock::watch_screen_lock(app.handle());
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
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                windows::window_destroyed(window.app_handle(), window.label());
            }
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
            commands::get_connection,
            commands::cancel_query,
            commands::ai_running,
            commands::session_ping,
            commands::idle_timeout,
            commands::redis_server_overview,
            commands::redis_clients,
            commands::redis_slowlog,
            commands::redis_command_stats,
            commands::redis_config,
            commands::redis_kill_client,
            lock::lock_status,
            lock::lock_app,
            lock::unlock_app,
            lock::verify_owner,
            lock::owner_verified,
            commands::redis_databases,
            commands::redis_scan,
            commands::redis_key_info,
            commands::redis_read_value,
            commands::redis_check,
            commands::redis_execute,
            commands::mcp_setup,
            commands::export_query,
            commands::server_overview,
            commands::server_processes,
            commands::server_users,
            commands::user_grants,
            commands::admin_plan,
            commands::admin_apply,
            commands::schema_meta,
            commands::database_options,
            commands::table_design,
            commands::show_create_table,
            commands::schema_objects,
            commands::object_definition,
            commands::schema_plan,
            commands::schema_apply,
            commands::save_connection,
            commands::delete_connection,
            commands::export_connections,
            commands::read_connection_import,
            commands::import_connections,
            commands::set_favorite,
            commands::test_connection,
            commands::list_server_databases,
            commands::aws_rds_ca_path,
            commands::open_session,
            windows::new_window,
            windows::focus_connection_window,
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
            commands::crash_info,
            commands::clear_crash_info,
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
                let state = app.state::<AppState>();
                state.ai.kill_now();
                // Emptied, not removed: another copy of the app may be
                // waiting for its lock, and must get the same file the MCP
                // server checks.
                if let Ok(dir) = app.path().app_data_dir() {
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .open(dir.join(RUNNING_FLAG))
                        .and_then(|f| f.set_len(0));
                }
            }
        });
}
