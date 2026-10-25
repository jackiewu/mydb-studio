//! `mydb-studio --mcp`: the app's executable doubles as an MCP server for AI
//! tools, without opening a window. Using the same executable means it
//! reads the same app database and, on macOS, may read the same keychain
//! item without another prompt.

use std::path::PathBuf;

use mydb_core::Store;

/// Must match `identifier` in tauri.conf.json, which names the folder.
const IDENTIFIER: &str = "com.jackiewu.mydbstudio";

/// The folder Tauri's `app_data_dir` points at, worked out without Tauri.
/// In debug builds `MYDB_STUDIO_DATA_DIR` overrides it, for tests. Release
/// builds ignore it: another folder's database could point saved
/// connections at other servers.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(debug_assertions)
        && let Some(dir) = std::env::var_os("MYDB_STUDIO_DATA_DIR")
    {
        return Some(dir.into());
    }
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home()?.join("Library/Application Support")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| Some(home()?.join(".local/share")))?
    };
    Some(base.join(IDENTIFIER))
}

/// Serves MCP on stdin/stdout; returns the process exit code.
pub fn main() -> i32 {
    let Some(dir) = data_dir() else {
        eprintln!("mydb-studio --mcp: cannot find the app data folder");
        return 1;
    };
    let store = (|| {
        std::fs::create_dir_all(&dir)?;
        let mut store = Store::open(dir.join("mydb-studio.db"))?;
        if let Some(vault) = crate::keychain::vault() {
            store.use_vault(vault)?;
        }
        Ok::<_, mydb_core::CoreError>(store)
    })();
    let store = match store {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mydb-studio --mcp: {e}");
            return 1;
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mydb-studio --mcp: {e}");
            return 1;
        }
    };
    match runtime.block_on(mydb_mcp::serve_stdio(store, Some(dir))) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mydb-studio --mcp: {e}");
            1
        }
    }
}
