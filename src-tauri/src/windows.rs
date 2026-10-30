//! Several app windows, each connected to its own server. Sessions belong to
//! the window that opened them and are closed when it goes away; one
//! connection is open in at most one window, so its saved tabs have a single
//! owner.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use mydb_core::{CoreError, Result};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::commands::AppState;

/// Which window opened a session, and for which connection.
struct Owner {
    window: String,
    connection_id: String,
}

#[derive(Default)]
pub struct WindowSessions {
    owners: Mutex<HashMap<String, Owner>>,
}

impl WindowSessions {
    pub fn add(&self, session_id: &str, window: &str, connection_id: &str) {
        self.owners.lock().unwrap().insert(
            session_id.to_string(),
            Owner {
                window: window.to_string(),
                connection_id: connection_id.to_string(),
            },
        );
    }

    pub fn remove(&self, session_id: &str) {
        self.owners.lock().unwrap().remove(session_id);
    }

    /// Lets go of a window's sessions, returning their ids.
    fn take_window(&self, window: &str) -> Vec<String> {
        let mut owners = self.owners.lock().unwrap();
        let ids: Vec<String> = owners
            .iter()
            .filter(|(_, o)| o.window == window)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            owners.remove(id);
        }
        ids
    }

    /// Another window that has `connection_id` open.
    fn window_with(&self, connection_id: &str, except: &str) -> Option<String> {
        self.owners
            .lock()
            .unwrap()
            .values()
            .find(|o| o.connection_id == connection_id && o.window != except)
            .map(|o| o.window.clone())
    }
}

/// App windows other than the first ("main") are labelled with this prefix.
const WINDOW_PREFIX: &str = "win-";
static NEXT_WINDOW: AtomicU32 = AtomicU32::new(1);

/// Opens another app window, a little below and right of the focused one.
/// With `connection_id`, the window connects to it once it loads.
pub fn open_window(app: &AppHandle, connection_id: Option<&str>) -> Result<()> {
    let failed = |e: tauri::Error| CoreError::Config {
        message: format!("無法開啟新視窗：{e}"),
    };
    let label = format!(
        "{WINDOW_PREFIX}{}",
        NEXT_WINDOW.fetch_add(1, Ordering::Relaxed)
    );
    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App("index.html".into()))
        .title("MyDB Studio")
        .inner_size(1280.0, 800.0)
        .min_inner_size(900.0, 560.0);
    if let Some(id) = connection_id {
        let id = serde_json::to_string(id).expect("a string serializes");
        builder = builder.initialization_script(format!("window.__MYDB_CONNECT__ = {id};"));
    }
    let from = app
        .webview_windows()
        .into_values()
        .find(|w| w.is_focused().unwrap_or(false) && is_app_window(w.label()));
    if let Some(from) = from
        && let (Ok(pos), Ok(scale)) = (from.outer_position(), from.scale_factor())
    {
        let pos = pos.to_logical::<f64>(scale);
        builder = builder.position(pos.x + 28.0, pos.y + 28.0);
    }
    builder.build().map_err(failed)?;
    Ok(())
}

fn is_app_window(label: &str) -> bool {
    label == "main" || label.starts_with(WINDOW_PREFIX)
}

#[tauri::command]
pub fn new_window(app: AppHandle, connection_id: Option<String>) -> Result<()> {
    open_window(&app, connection_id.as_deref())
}

/// Brings forward the other window that has `connection_id` open. False
/// when no other window has it.
#[tauri::command]
pub fn focus_connection_window(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    connection_id: String,
) -> bool {
    let Some(label) = state.windows.window_with(&connection_id, window.label()) else {
        return false;
    };
    let Some(other) = app.get_webview_window(&label) else {
        return false;
    };
    let _ = other.unminimize();
    let _ = other.set_focus();
    true
}

/// Closes the sessions of a window that went away.
pub fn window_destroyed(app: &AppHandle, label: &str) {
    if !is_app_window(label) {
        return;
    }
    let ids = app.state::<AppState>().windows.take_window(label);
    if ids.is_empty() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        for id in ids {
            let session = state.sessions.write().await.remove(&id);
            if let Some(session) = session {
                let _ = session.close().await;
            }
        }
    });
}

/// Tells every window that shared data changed, so they show the same.
pub fn changed(app: &AppHandle, what: &str) {
    let _ = app.emit(&format!("{what}-changed"), ());
}

/// Adds "New Window" (⌘⇧N) to the File menu of the standard macOS menu.
#[cfg(target_os = "macos")]
pub fn install_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app.handle())?;
    let item = MenuItem::with_id(app, "new-window", "新視窗", true, Some("CmdOrCtrl+Shift+N"))?;
    let file = menu.items()?.into_iter().find_map(|i| match i {
        MenuItemKind::Submenu(s) if s.text().is_ok_and(|t| t == "File") => Some(s),
        _ => None,
    });
    if let Some(file) = file {
        file.insert(&item, 0)?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id() == "new-window" {
            let _ = open_window(app, None);
        }
    });
    Ok(())
}
