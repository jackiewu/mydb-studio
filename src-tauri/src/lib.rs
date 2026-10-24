mod commands;

use std::collections::HashMap;
use std::sync::Mutex;

use mydb_core::Store;
use tauri::Manager;

pub use commands::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = Store::open(dir.join("mydb-studio.db"))?;
            app.manage(AppState {
                store: Mutex::new(store),
                sessions: tokio::sync::RwLock::new(HashMap::new()),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_connections,
            commands::save_connection,
            commands::delete_connection,
            commands::test_connection,
            commands::open_session,
            commands::close_session,
            commands::execute,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
