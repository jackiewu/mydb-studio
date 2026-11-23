//! Saves an HTML report straight to a PDF file, without the print dialog.
//!
//! macOS renders the report in a hidden window and asks WebKit for a PDF of
//! the whole page (one long page, like a scrolling screenshot with real text).
//! Elsewhere there is no such call, so the report opens in the print window.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mydb_core::{CoreError, Result};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::commands::AppState;

pub const PDF_WINDOW: &str = "report-pdf";
/// Path the `mydbreport:` protocol serves the PDF's HTML on.
pub const PDF_PATH: &str = "/pdf";

fn config_error(message: impl Into<String>) -> CoreError {
    CoreError::Config {
        message: message.into(),
    }
}

/// `dir/name.pdf`, or `dir/name (2).pdf` and so on when that is taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(format!("{name}.pdf"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{name} ({n}).pdf")))
        .find(|p| !p.exists())
        .expect("some free file name")
}

/// Keeps a file name to letters, digits, `-` and `_`.
fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "sql-report".into() } else { s }
}

/// Writes `html` as a PDF named `name` into the Downloads folder and returns
/// its path. Returns `None` where only the print window is available.
#[tauri::command]
pub async fn save_report_pdf(
    app: AppHandle,
    state: State<'_, AppState>,
    html: String,
    name: String,
) -> Result<Option<String>> {
    state.lock.ensure_unlocked()?;
    if !cfg!(target_os = "macos") {
        crate::commands::print_report(app, state, html)?;
        return Ok(None);
    }
    let dir = app
        .path()
        .download_dir()
        .map_err(|e| config_error(format!("找不到「下載」資料夾：{e}")))?;
    let path = unique_path(&dir, &safe_name(&name));

    *state.pdf_html.lock().unwrap() = html;
    // One PDF at a time: a window left from an earlier try is replaced.
    if let Some(old) = app.get_webview_window(PDF_WINDOW) {
        let _ = old.destroy();
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<std::result::Result<Vec<u8>, String>>();
    let tx = std::sync::Mutex::new(Some(tx));
    let url = format!(
        "{}{}",
        crate::commands::PRINT_URL.trim_end_matches('/'),
        PDF_PATH
    )
    .parse()
    .expect("valid PDF URL");
    let window = WebviewWindowBuilder::new(&app, PDF_WINDOW, WebviewUrl::CustomProtocol(url))
        .title("PDF")
        // About A4 width at 96 dpi, so the layout matches a printed page.
        .inner_size(794.0, 1123.0)
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .on_page_load(move |window, payload| {
            if !matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                return;
            }
            let Some(tx) = tx.lock().unwrap().take() else {
                return;
            };
            create_pdf(&window, tx);
        })
        .build()
        .map_err(|e| config_error(format!("無法產生 PDF：{e}")))?;

    let result = tokio::time::timeout(Duration::from_secs(30), rx).await;
    let _ = window.destroy();
    state.pdf_html.lock().unwrap().clear();
    let bytes = match result {
        Ok(Ok(Ok(bytes))) => bytes,
        Ok(Ok(Err(e))) => return Err(config_error(format!("無法產生 PDF：{e}"))),
        Ok(Err(_)) => return Err(config_error("無法產生 PDF：視窗已關閉")),
        Err(_) => return Err(config_error("產生 PDF 逾時")),
    };
    std::fs::write(&path, bytes)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

type PdfSender = tokio::sync::oneshot::Sender<std::result::Result<Vec<u8>, String>>;

#[cfg(target_os = "macos")]
fn create_pdf(window: &tauri::WebviewWindow, tx: PdfSender) {
    use block2::RcBlock;
    use objc2_foundation::{NSData, NSError};
    use objc2_web_kit::WKWebView;

    let tx = std::sync::Mutex::new(Some(tx));
    let send = move |r: std::result::Result<Vec<u8>, String>| {
        if let Some(tx) = tx.lock().unwrap().take() {
            let _ = tx.send(r);
        }
    };
    let res = window.with_webview(move |wv| {
        // SAFETY: on macOS `inner` is the window's WKWebView, and
        // `with_webview` runs this on the main thread.
        let webview: &WKWebView = unsafe { &*(wv.inner() as *const WKWebView) };
        let done = RcBlock::new(move |data: *mut NSData, err: *mut NSError| {
            // SAFETY: WebKit passes valid NSData on success, else an NSError.
            let r = match unsafe { data.as_ref() } {
                Some(d) => Ok(d.to_vec()),
                None => Err(unsafe { err.as_ref() }
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "unknown error".into())),
            };
            send(r);
        });
        // A nil configuration captures the whole page.
        unsafe { webview.createPDFWithConfiguration_completionHandler(None, &done) };
    });
    if let Err(e) = res {
        // The closure, and with it the sender, was dropped, which ends
        // the wait with an error.
        eprintln!("PDF webview unavailable: {e}");
    }
}

#[cfg(not(target_os = "macos"))]
fn create_pdf(_window: &tauri::WebviewWindow, tx: PdfSender) {
    let _ = tx.send(Err("目前只支援 macOS".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_and_unique() {
        assert_eq!(safe_name("sql-report-67"), "sql-report-67");
        assert_eq!(safe_name("../a b"), "___a_b");
        assert_eq!(safe_name(""), "sql-report");
        let dir = std::env::temp_dir().join(format!("mydb-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "r"), dir.join("r.pdf"));
        std::fs::write(dir.join("r.pdf"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "r"), dir.join("r (2).pdf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
