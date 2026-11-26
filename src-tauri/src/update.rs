//! The app's version, and updating it from GitHub Releases.
//!
//! The updater fetches `latest.json` from the newest release, downloads the
//! `.app.tar.gz` it names, and checks its minisign signature against the
//! public key in tauri.conf.json before replacing the app. Releases are
//! signed on the maintainer's Mac (scripts/release-mac.sh); the private key
//! never leaves it.

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Runtime};
use tauri_plugin_updater::UpdaterExt;

use mydb_core::CoreError;

type Result<T> = std::result::Result<T, CoreError>;

/// Build variant shown next to the version; empty for releases.
///
/// A branch that carries features never released (the IAP branch) sets its
/// name here. Such a build doesn't install updates: a release would replace
/// it with a build that lacks those features.
pub const FLAVOR: &str = match option_env!("MYDB_FLAVOR") {
    Some(f) => f,
    None => "IAP",
};

#[derive(Serialize)]
pub struct AppInfo {
    pub version: String,
    /// Empty for releases, e.g. "IAP" for a local IAP build.
    pub flavor: &'static str,
    /// Why this build can't install updates; None when it can.
    pub update_blocked: Option<String>,
}

fn update_blocked() -> Option<String> {
    if !FLAVOR.is_empty() {
        return Some(format!(
            "這是含 {FLAVOR} 的本機版本，從 GitHub 更新會換成沒有 {FLAVOR} 的正式版，所以不在 app 內安裝。請在 Mac 上合併 main 後重新建置。"
        ));
    }
    if cfg!(debug_assertions) {
        return Some(
            "開發模式（npm run tauri dev）不會安裝更新，請用 git pull 更新原始碼。".into(),
        );
    }
    None
}

#[tauri::command]
pub fn app_info<R: Runtime>(app: AppHandle<R>) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        flavor: FLAVOR,
        update_blocked: update_blocked(),
    }
}

#[derive(Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    /// Release notes, Markdown.
    pub notes: Option<String>,
    /// RFC 3339.
    pub date: Option<String>,
}

fn updater_error(e: tauri_plugin_updater::Error) -> CoreError {
    CoreError::Config {
        message: format!("檢查更新失敗：{e}"),
    }
}

/// The newer release, if there is one. Works in every build, so a blocked
/// build can still tell that a release is out.
#[tauri::command]
pub async fn check_update<R: Runtime>(app: AppHandle<R>) -> Result<Option<UpdateInfo>> {
    let update = app
        .updater()
        .map_err(updater_error)?
        .check()
        .await
        .map_err(updater_error)?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version,
        current_version: u.current_version,
        notes: u.body.filter(|b| !b.trim().is_empty()),
        date: u.date.and_then(|d| {
            d.format(&time::format_description::well_known::Rfc3339)
                .ok()
        }),
    }))
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DownloadEvent {
    Progress { downloaded: u64, total: Option<u64> },
    Installing,
}

/// Downloads the newest release, checks its signature and replaces the app.
/// The new version runs after `restart_app`.
#[tauri::command]
pub async fn install_update<R: Runtime>(
    app: AppHandle<R>,
    on_event: Channel<DownloadEvent>,
) -> Result<String> {
    if let Some(why) = update_blocked() {
        return Err(CoreError::Config { message: why });
    }
    let update = app
        .updater()
        .map_err(updater_error)?
        .check()
        .await
        .map_err(updater_error)?
        .ok_or_else(|| CoreError::Config {
            message: "已經是最新版本。".into(),
        })?;
    let mut downloaded = 0u64;
    update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = on_event.send(DownloadEvent::Progress { downloaded, total });
            },
            || {
                let _ = on_event.send(DownloadEvent::Installing);
            },
        )
        .await
        .map_err(|e| CoreError::Config {
            message: format!("更新失敗，目前的版本沒有變動：{e}"),
        })?;
    Ok(update.version)
}

/// Restarts through the normal exit, so the run loop's exit handling (the
/// running flag, the AI model) happens as on a quit. `restart()` would skip
/// it when called on the main thread, and the next start would report an
/// unclean exit.
#[tauri::command]
pub fn restart_app<R: Runtime>(app: AppHandle<R>) {
    app.request_restart();
}
