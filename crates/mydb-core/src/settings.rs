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
    /// Suggest database, table and column names while typing.
    pub schema_autocomplete: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            editor_theme: "auto".into(),
            editor_font_size: 13,
            max_rows: DEFAULT_MAX_ROWS,
            record_by_default: false,
            show_system_databases: false,
            schema_autocomplete: true,
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
