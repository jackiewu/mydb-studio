//! Tauri commands: a thin layer over `mydb-core`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mydb_core::{
    ConnectionProfile, CoreError, DEFAULT_MAX_ROWS, ExecutionOutcome, Result, Session, SessionInfo,
    Store,
};
use serde::Serialize;
use tauri::State;
use tokio::sync::RwLock;

pub struct AppState {
    pub store: Mutex<Store>,
    /// Open sessions by session id. One connection profile can have several.
    pub sessions: RwLock<HashMap<String, Arc<Session>>>,
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
}

#[tauri::command]
pub async fn open_session(
    state: State<'_, AppState>,
    connection_id: String,
) -> Result<OpenedSession> {
    let mut profile = state.connection(&connection_id)?;
    let session = Session::connect(&profile).await?;
    let info = session.info().clone();

    // Trust on first use: remember the SSH host key for next time.
    if let (Some(ssh), Some(fp)) = (profile.ssh.as_mut(), &info.ssh_host_fingerprint)
        && ssh.known_host_fingerprint.is_none()
    {
        ssh.known_host_fingerprint = Some(fp.clone());
        state.store.lock().unwrap().save_connection(&profile)?;
    }

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

#[tauri::command]
pub async fn execute(
    state: State<'_, AppState>,
    session_id: String,
    sql: String,
    max_rows: Option<usize>,
) -> Result<ExecutionOutcome> {
    let session = state.session(&session_id).await?;
    Ok(session
        .execute(&sql, max_rows.unwrap_or(DEFAULT_MAX_ROWS))
        .await)
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}
