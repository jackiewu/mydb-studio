//! User preferences and SQL snippets, both kept in the local store.

use serde::{Deserialize, Serialize};

use crate::session::DEFAULT_MAX_ROWS;

/// App-wide preferences. Missing fields take their defaults, so older saved
/// settings keep loading as new options are added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Editor color theme id; `auto` follows the system light/dark mode.
    pub editor_theme: String,
    pub editor_font_size: u32,
    /// Rows kept per result set.
    pub max_rows: usize,
    /// Whether the record toggle starts switched on after connecting.
    pub record_by_default: bool,
    pub show_system_databases: bool,
    /// Home page: a click selects a connection and a double-click opens it;
    /// off, a single click opens it.
    pub connect_on_double_click: bool,
    /// Suggest database, table and column names while typing.
    pub schema_autocomplete: bool,
    /// Ask before running statements that can destroy data.
    pub confirm_dangerous: bool,
    /// LIMIT added to queries that have none; 0 turns it off.
    pub auto_limit: u64,
    /// AI suggestions from a local model (off until set up).
    pub ai_enabled: bool,
    /// `llama-server` to use; empty finds it on PATH or in Homebrew.
    pub ai_llama_server: String,
    /// The `.gguf` model file the app starts when AI is first used.
    pub ai_model_path: String,
    /// Use an already running OpenAI-compatible server instead of starting one.
    pub ai_use_external: bool,
    pub ai_server_url: String,
    /// Stop the model after this many idle minutes.
    pub ai_idle_minutes: u32,
    /// CPU threads the model may use; 0 picks half of them.
    pub ai_threads: u32,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            editor_theme: "auto".into(),
            editor_font_size: 13,
            max_rows: DEFAULT_MAX_ROWS,
            record_by_default: false,
            show_system_databases: false,
            connect_on_double_click: true,
            schema_autocomplete: true,
            confirm_dangerous: true,
            auto_limit: 1000,
            ai_enabled: false,
            ai_llama_server: String::new(),
            ai_model_path: String::new(),
            ai_use_external: false,
            ai_server_url: "http://127.0.0.1:39282".into(),
            ai_idle_minutes: 10,
            ai_threads: 0,
        }
    }
}

/// A reusable piece of SQL. Typing `prefix` in the editor offers `body`,
/// where `${name}` marks a field to fill in (Tab moves to the next one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snippet {
    /// 0 for a snippet that has not been saved yet.
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub body: String,
}

/// An editor tab the user pinned to a connection. Pinned tabs are saved as
/// they change and reopen with the connection until closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinnedTab {
    /// 0 for a tab that has not been saved yet.
    #[serde(default)]
    pub id: i64,
    pub connection_id: String,
    pub title: String,
    pub sql: String,
    /// Order among the connection's pinned tabs.
    #[serde(default)]
    pub position: i64,
    /// False for a draft: an unpinned tab kept only so a crash doesn't
    /// lose it. Drafts are dropped when the user closes them.
    #[serde(default = "yes")]
    pub pinned: bool,
    /// The .sql file the tab was opened from or saved to.
    #[serde(default)]
    pub file_path: Option<String>,
}

fn yes() -> bool {
    true
}
