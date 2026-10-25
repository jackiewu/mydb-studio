//! App lock: the window is covered until the owner confirms with Touch ID or
//! the login password. While locked, a flag file tells the MCP server (a
//! separate process) to refuse every tool.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mydb_core::{AppSettings, CoreError, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::commands::AppState;

/// How long one confirmation allows changing the lock settings.
const VERIFIED_FOR: Duration = Duration::from_secs(120);

pub struct LockState {
    locked: AtomicBool,
    /// [`mydb_core::LOCK_FLAG`] in the app data folder.
    flag: PathBuf,
    verified_at: Mutex<Option<Instant>>,
}

impl LockState {
    /// Starts locked when the settings ask for it.
    pub fn new(flag: PathBuf, settings: &AppSettings) -> Self {
        let state = Self {
            locked: AtomicBool::new(false),
            flag,
            verified_at: Mutex::new(None),
        };
        state.set_locked(settings.lock_enabled() && settings.lock_on_start);
        state
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
        if locked {
            let _ = std::fs::write(&self.flag, "");
        } else {
            let _ = std::fs::remove_file(&self.flag);
        }
    }

    fn recently_verified(&self) -> bool {
        self.verified_at
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() < VERIFIED_FOR)
    }

    /// Refuses changes to the lock settings without a recent confirmation,
    /// so nobody can turn the lock off while the owner is away.
    pub fn check_settings_change(&self, old: &AppSettings, new: &AppSettings) -> Result<()> {
        if old.lock_changed(new) && !self.recently_verified() {
            return Err(CoreError::Config {
                message: "變更 App 鎖定設定前，請先通過系統驗證。".into(),
            });
        }
        Ok(())
    }
}

/// Shows the system prompt off the async runtime.
async fn system_auth(reason: &str) -> Result<()> {
    let reason = reason.to_string();
    tokio::task::spawn_blocking(move || mydb_sysauth::authenticate(&reason))
        .await
        .map_err(|e| CoreError::Config {
            message: e.to_string(),
        })?
        .map_err(|e| CoreError::Config {
            message: e.to_string(),
        })
}

#[derive(Serialize)]
pub struct LockStatus {
    locked: bool,
    /// Whether this computer can do system authentication.
    available: bool,
}

#[tauri::command]
pub fn lock_status(state: State<'_, AppState>) -> LockStatus {
    LockStatus {
        locked: state.lock.is_locked(),
        available: mydb_sysauth::available(),
    }
}

#[tauri::command]
pub fn lock_app(app: AppHandle, state: State<'_, AppState>) -> Result<()> {
    let enabled = state.store.lock().unwrap().get_settings()?.lock_enabled();
    if enabled {
        state.lock.set_locked(true);
        let _ = app.emit("app-locked", ());
    }
    Ok(())
}

#[tauri::command]
pub async fn unlock_app(state: State<'_, AppState>) -> Result<()> {
    system_auth("解鎖 MyDB Studio").await?;
    *state.lock.verified_at.lock().unwrap() = Some(Instant::now());
    state.lock.set_locked(false);
    Ok(())
}

/// Confirms the owner is here, e.g. before changing the lock settings.
#[tauri::command]
pub async fn verify_owner(state: State<'_, AppState>, reason: String) -> Result<()> {
    system_auth(&reason).await?;
    *state.lock.verified_at.lock().unwrap() = Some(Instant::now());
    Ok(())
}

/// Locks when the screen locks, if the settings ask for it.
pub fn watch_screen_lock(app: &AppHandle) {
    let app = app.clone();
    mydb_sysauth::watch_screen_lock(move || {
        let state = tauri::Manager::state::<AppState>(&app);
        let lock = state
            .store
            .lock()
            .unwrap()
            .get_settings()
            .is_ok_and(|s| s.lock_enabled() && s.lock_on_sleep);
        if lock {
            state.lock.set_locked(true);
            let _ = app.emit("app-locked", ());
        }
    });
}
