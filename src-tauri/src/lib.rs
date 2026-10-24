mod commands;

use std::collections::HashMap;
use std::sync::Mutex;

use mydb_core::Store;
use tauri::Manager;

pub use commands::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = Store::open(dir.join("mydb-studio.db"))?;
            app.manage(AppState {
                store: Mutex::new(store),
                sessions: tokio::sync::RwLock::new(HashMap::new()),
                print_html: Mutex::new(String::new()),
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
