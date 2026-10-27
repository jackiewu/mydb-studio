//! Local app database (SQLite): saved connections and execution records.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::ai::{AiChat, AiChatSummary};
use crate::error::Result;
use crate::profile::{ConnectionKind, ConnectionProfile, RecordMode};
use crate::records::{ExecutionRecord, RecordFilter};
use crate::secrets::{self, ConnectionSecrets, SecretVault, Secrets};
use crate::settings::{AppSettings, PinnedTab, Snippet};

/// Schema migrations, applied in order. Never edit a shipped entry; append.
const MIGRATIONS: &[&str] = &[
    // 1: saved connections
    "CREATE TABLE connections (
        id         TEXT PRIMARY KEY,
        name       TEXT NOT NULL,
        profile    TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    );",
    // 2: recently used ordering
    "ALTER TABLE connections ADD COLUMN last_used_at TEXT;",
    // 3: execution records
    "CREATE TABLE executions (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        connection_id    TEXT,
        connection_name  TEXT NOT NULL,
        server           TEXT NOT NULL,
        server_version   TEXT NOT NULL,
        database         TEXT,
        started_at       TEXT NOT NULL,
        duration_ms      REAL NOT NULL,
        sql              TEXT NOT NULL,
        statements       TEXT NOT NULL,
        statement_count  INTEGER NOT NULL,
        affected_rows    INTEGER NOT NULL,
        returned_rows    INTEGER NOT NULL,
        error_code       INTEGER,
        error_message    TEXT,
        failed_statement TEXT,
        note             TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX executions_started_at ON executions (started_at);
    CREATE INDEX executions_connection ON executions (connection_id, started_at);",
    // 4: preferences and snippets, with a few starter snippets
    "CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE snippets (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        name       TEXT NOT NULL,
        prefix     TEXT NOT NULL,
        body       TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT (datetime('now'))
    );
    INSERT INTO snippets (name, prefix, body) VALUES
      ('查詢資料', 'sel', 'SELECT *
FROM ${table}
WHERE ${condition}
LIMIT 100;'),
      ('計算筆數', 'cnt', 'SELECT COUNT(*) FROM ${table};'),
      ('新增資料', 'ins', 'INSERT INTO ${table} (${columns})
VALUES (${values});'),
      ('更新資料', 'upd', 'UPDATE ${table}
SET ${column} = ${value}
WHERE ${id_column} = ${id};'),
      ('刪除資料', 'del', 'DELETE FROM ${table}
WHERE ${condition};'),
      ('資料表結構', 'ddl', 'SHOW CREATE TABLE ${table};'),
      ('索引', 'idx', 'SHOW INDEX FROM ${table};'),
      ('執行中的查詢', 'proc', 'SHOW FULL PROCESSLIST;'),
      ('資料表大小', 'size', 'SELECT TABLE_NAME,
       ROUND((DATA_LENGTH + INDEX_LENGTH) / 1024 / 1024, 1) AS size_mb,
       TABLE_ROWS AS rows_estimate
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = DATABASE()
ORDER BY size_mb DESC;');",
    // 5: pinned editor tabs
    "CREATE TABLE pinned_tabs (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        connection_id TEXT NOT NULL,
        title         TEXT NOT NULL,
        sql           TEXT NOT NULL,
        position      INTEGER NOT NULL DEFAULT 0,
        updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE INDEX pinned_tabs_connection ON pinned_tabs (connection_id, position);",
    // 6: AI conversations
    "CREATE TABLE ai_chats (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        title           TEXT NOT NULL,
        connection_id   TEXT,
        connection_name TEXT NOT NULL DEFAULT '',
        context         TEXT NOT NULL,
        messages        TEXT NOT NULL,
        created_at      TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
    );
    CREATE INDEX ai_chats_updated ON ai_chats (updated_at);",
    // 7: every tab is kept while open (unpinned ones as drafts), with its file
    "ALTER TABLE pinned_tabs ADD COLUMN pinned INTEGER NOT NULL DEFAULT 1;
    ALTER TABLE pinned_tabs ADD COLUMN file_path TEXT;",
    // 8: records of Redis commands, told apart from SQL
    "ALTER TABLE executions ADD COLUMN connection_kind TEXT NOT NULL DEFAULT 'mysql';",
];

const RECORD_COLUMNS: &str = "id, connection_id, connection_name, server, server_version, database,
    started_at, duration_ms, sql, statements, statement_count, affected_rows, returned_rows,
    error_code, error_message, failed_statement, note, connection_kind";

fn record_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(ExecutionRecord, String)> {
    Ok((
        ExecutionRecord {
            id: r.get(0)?,
            connection_id: r.get(1)?,
            connection_name: r.get(2)?,
            server: r.get(3)?,
            server_version: r.get(4)?,
            database: r.get(5)?,
            started_at: r.get(6)?,
            duration_ms: r.get(7)?,
            sql: r.get(8)?,
            statements: Vec::new(),
            statement_count: r.get(10)?,
            affected_rows: r.get::<_, i64>(11)? as u64,
            returned_rows: r.get::<_, i64>(12)? as u64,
            error_code: r.get(13)?,
            error_message: r.get(14)?,
            failed_statement: r.get(15)?,
            note: r.get(16)?,
            connection_kind: match r.get::<_, String>(17)?.as_str() {
                "redis" => ConnectionKind::Redis,
                _ => ConnectionKind::Mysql,
            },
        },
        r.get(9)?,
    ))
}

pub struct Store {
    db: Connection,
    /// Where passwords go instead of `db`, when set.
    secrets: Option<std::cell::RefCell<Secrets>>,
    /// Whether secrets saved before they knew their destination may be
    /// used (and then bound); see [`Store::trust_unbound_secrets`].
    unbound_ok: bool,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self> {
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        // Overwrite deleted content, so a replaced profile (or anything
        // else removed) doesn't linger in free pages.
        db.pragma_update(None, "secure_delete", "ON")?;
        let mut store = Self {
            db,
            secrets: None,
            unbound_ok: false,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Keeps passwords in `vault` from now on, moving any saved in the
    /// database there.
    pub fn use_vault(&mut self, vault: Box<dyn SecretVault>) -> Result<()> {
        self.secrets = Some(std::cell::RefCell::new(Secrets::new(vault)));
        let mut stmt = self.db.prepare("SELECT profile FROM connections")?;
        let saved: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let mut moved = false;
        for json in saved {
            let profile: ConnectionProfile = serde_json::from_str(&json)?;
            let (stripped, found) = secrets::strip(&profile);
            if found.is_empty() {
                continue;
            }
            // Passwords in the file for somewhere other than the ones in the
            // vault were put there behind the app's back: drop them rather
            // than let them replace the vault's.
            let kept = self
                .secrets
                .as_ref()
                .and_then(|s| s.borrow_mut().get(&profile.id));
            if kept.is_some_and(|k| k.destination != found.destination) {
                self.write_connection(&stripped)?;
            } else {
                self.save_connection(&profile)?;
            }
            moved = true;
        }
        // Rewrite the file once so passwords saved before the vault are
        // gone from free pages and the WAL too.
        let scrubbed: Option<String> = self
            .db
            .query_row(
                "SELECT value FROM settings WHERE key = 'vault_scrubbed'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if moved || scrubbed.is_none() {
            self.db.execute_batch(
                "VACUUM;
                 PRAGMA wal_checkpoint(TRUNCATE);
                 INSERT INTO settings (key, value) VALUES ('vault_scrubbed', '1')
                   ON CONFLICT(key) DO NOTHING;",
            )?;
        }
        Ok(())
    }

    /// Lets secrets saved before they knew their destination be used, binding
    /// them to the profile on first use. Only the app does this, where the
    /// owner sees which server a connection points at; the MCP server
    /// leaves them alone, so a changed database can't claim them.
    pub fn trust_unbound_secrets(&mut self) {
        self.unbound_ok = true;
    }

    /// `profile` with its passwords from the vault, if they were saved for
    /// the servers it points at.
    fn with_secrets(&self, mut profile: ConnectionProfile) -> ConnectionProfile {
        let Some(s) = &self.secrets else {
            return profile;
        };
        let mut s = s.borrow_mut();
        let Some(found) = s.get(&profile.id) else {
            return profile;
        };
        if !found.fits(&profile, self.unbound_ok) {
            return profile;
        }
        let here = secrets::destination(&profile);
        if found.destination.as_ref() != Some(&here) {
            // Saved before secrets knew their whole destination: bind them now.
            let bound = ConnectionSecrets {
                destination: Some(here),
                ..found.clone()
            };
            s.put(&profile.id, bound);
        }
        secrets::fill(&mut profile, &found);
        profile
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .db
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tx = self.db.transaction()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Saved connections: favorites first, then most recently used, then by
    /// name. Without their passwords, so listing doesn't read the keychain
    /// (and ask for it on macOS); [`Store::get_connection`] has them.
    pub fn list_connections(&self) -> Result<Vec<ConnectionProfile>> {
        let mut stmt = self
            .db
            .prepare("SELECT profile, last_used_at FROM connections")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (json, last_used_at) = row?;
            let mut profile: ConnectionProfile = serde_json::from_str(&json)?;
            profile.last_used_at = last_used_at;
            out.push(profile);
        }
        out.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                // RFC 3339 strings sort chronologically; None sorts last.
                .then_with(|| b.last_used_at.cmp(&a.last_used_at))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    /// Records that a connection was just opened.
    pub fn touch_connection(&self, id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE connections SET last_used_at = ?2 WHERE id = ?1",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<()> {
        if let Some(mut profile) = self.get_connection(id)? {
            profile.favorite = favorite;
            self.save_connection(&profile)?;
        }
        Ok(())
    }

    pub fn get_connection(&self, id: &str) -> Result<Option<ConnectionProfile>> {
        let json: Option<String> = self
            .db
            .query_row("SELECT profile FROM connections WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let profile: Option<ConnectionProfile> =
            json.map(|j| serde_json::from_str(&j)).transpose()?;
        Ok(profile.map(|p| self.with_secrets(p)))
    }

    /// Whether the saved profile `id` forces recording. Read from the file
    /// as saved, so a change made while connected applies at once, and
    /// without touching the keychain.
    pub fn record_forced(&self, id: &str) -> Result<bool> {
        let json: Option<String> = self
            .db
            .query_row("SELECT profile FROM connections WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match json {
            Some(j) => {
                serde_json::from_str::<ConnectionProfile>(&j)?.record_mode == RecordMode::Forced
            }
            None => false,
        })
    }

    /// Inserts or replaces a profile by id.
    pub fn save_connection(&self, profile: &ConnectionProfile) -> Result<()> {
        // Passwords go to the vault; the database keeps them only when
        // there is no vault or it can't be used.
        let (stripped, found) = secrets::strip(profile);
        let profile = match &self.secrets {
            Some(s) if s.borrow_mut().put(&profile.id, found) => &stripped,
            _ => profile,
        };
        self.write_connection(profile)
    }

    /// Writes a profile to the file as it is.
    fn write_connection(&self, profile: &ConnectionProfile) -> Result<()> {
        self.db.execute(
            "INSERT INTO connections (id, name, profile) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               profile = excluded.profile,
               updated_at = datetime('now')",
            params![
                profile.id,
                profile.name,
                serde_json::to_string(&ConnectionProfile {
                    last_used_at: None,
                    ..profile.clone()
                })?
            ],
        )?;
        Ok(())
    }

    /// Saves a record and returns its id. The record's own `id` is ignored.
    pub fn insert_record(&self, rec: &ExecutionRecord) -> Result<i64> {
        self.db.execute(
            "INSERT INTO executions (connection_id, connection_name, server, server_version,
                database, started_at, duration_ms, sql, statements, statement_count,
                affected_rows, returned_rows, error_code, error_message, failed_statement, note,
                connection_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                rec.connection_id,
                rec.connection_name,
                rec.server,
                rec.server_version,
                rec.database,
                rec.started_at,
                rec.duration_ms,
                rec.sql,
                serde_json::to_string(&rec.statements)?,
                rec.statement_count,
                rec.affected_rows as i64,
                rec.returned_rows as i64,
                rec.error_code,
                rec.error_message,
                rec.failed_statement,
                rec.note,
                rec.connection_kind.as_str(),
            ],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Records matching `filter`, newest first. `statements` is left empty;
    /// use `get_record` or `get_records` for the full record.
    pub fn list_records(&self, filter: &RecordFilter) -> Result<Vec<ExecutionRecord>> {
        let mut sql = format!("SELECT {RECORD_COLUMNS} FROM executions WHERE 1 = 1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let non_empty = |v: &Option<String>| v.as_ref().filter(|s| !s.trim().is_empty()).cloned();
        if let Some(id) = non_empty(&filter.connection_id) {
            args.push(Box::new(id));
            sql += &format!(" AND connection_id = ?{}", args.len());
        }
        if let Some(kind) = filter.connection_kind {
            args.push(Box::new(kind.as_str()));
            sql += &format!(" AND connection_kind = ?{}", args.len());
        }
        if let Some(text) = non_empty(&filter.text) {
            args.push(Box::new(format!("%{}%", text.trim())));
            let n = args.len();
            sql += &format!(
                " AND (sql LIKE ?{n} OR note LIKE ?{n} OR IFNULL(error_message, '') LIKE ?{n})"
            );
        }
        if let Some(from) = non_empty(&filter.from) {
            args.push(Box::new(from));
            sql += &format!(" AND started_at >= ?{}", args.len());
        }
        if let Some(to) = non_empty(&filter.to) {
            args.push(Box::new(to));
            sql += &format!(" AND started_at < ?{}", args.len());
        }
        if filter.only_errors {
            sql += " AND error_message IS NOT NULL";
        }
        sql += &format!(
            " ORDER BY started_at DESC, id DESC LIMIT {} OFFSET {}",
            filter.limit.unwrap_or(200),
            filter.offset.unwrap_or(0)
        );
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            record_from_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.0);
        }
        Ok(out)
    }

    pub fn get_record(&self, id: i64) -> Result<Option<ExecutionRecord>> {
        let row = self
            .db
            .query_row(
                &format!("SELECT {RECORD_COLUMNS} FROM executions WHERE id = ?1"),
                [id],
                record_from_row,
            )
            .optional()?;
        Ok(match row {
            Some((mut rec, statements)) => {
                rec.statements = serde_json::from_str(&statements)?;
                Some(rec)
            }
            None => None,
        })
    }

    /// Full records for `ids`, oldest first (report order).
    pub fn get_records(&self, ids: &[i64]) -> Result<Vec<ExecutionRecord>> {
        let mut out = Vec::new();
        for id in ids {
            if let Some(rec) = self.get_record(*id)? {
                out.push(rec);
            }
        }
        out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    pub fn set_record_note(&self, id: i64, note: &str) -> Result<()> {
        self.db.execute(
            "UPDATE executions SET note = ?2 WHERE id = ?1",
            params![id, note],
        )?;
        Ok(())
    }

    pub fn delete_records(&self, ids: &[i64]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for id in ids {
            tx.execute("DELETE FROM executions WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_connection(&self, id: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM connections WHERE id = ?1", [id])?;
        if let Some(s) = &self.secrets {
            s.borrow_mut().remove(id);
        }
        Ok(())
    }

    /// Saved preferences, or the defaults if none were saved.
    pub fn get_settings(&self) -> Result<AppSettings> {
        let json: Option<String> = self
            .db
            .query_row("SELECT value FROM settings WHERE key = 'app'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(match json {
            Some(j) => serde_json::from_str(&j)?,
            None => AppSettings::default(),
        })
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.db.execute(
            "INSERT INTO settings (key, value) VALUES ('app', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [serde_json::to_string(settings)?],
        )?;
        Ok(())
    }

    /// All snippets, by prefix.
    pub fn list_snippets(&self) -> Result<Vec<Snippet>> {
        let mut stmt = self
            .db
            .prepare("SELECT id, name, prefix, body FROM snippets ORDER BY prefix, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Snippet {
                id: r.get(0)?,
                name: r.get(1)?,
                prefix: r.get(2)?,
                body: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inserts a snippet with id 0, otherwise updates it. Returns it with its id.
    pub fn save_snippet(&self, snippet: &Snippet) -> Result<Snippet> {
        let mut saved = snippet.clone();
        if snippet.id == 0 {
            self.db.execute(
                "INSERT INTO snippets (name, prefix, body) VALUES (?1, ?2, ?3)",
                params![snippet.name, snippet.prefix, snippet.body],
            )?;
            saved.id = self.db.last_insert_rowid();
        } else {
            self.db.execute(
                "UPDATE snippets SET name = ?2, prefix = ?3, body = ?4, updated_at = datetime('now')
                 WHERE id = ?1",
                params![snippet.id, snippet.name, snippet.prefix, snippet.body],
            )?;
        }
        Ok(saved)
    }

    pub fn delete_snippet(&self, id: i64) -> Result<()> {
        self.db
            .execute("DELETE FROM snippets WHERE id = ?1", [id])?;
        Ok(())
    }
    /// Inserts a conversation with id 0, otherwise updates it. Returns its id.
    pub fn save_ai_chat(&self, chat: &AiChat) -> Result<i64> {
        let context = serde_json::to_string(&chat.context)?;
        let messages = serde_json::to_string(&chat.messages)?;
        if chat.id == 0 {
            self.db.execute(
                "INSERT INTO ai_chats (title, connection_id, connection_name, context, messages)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    chat.title,
                    chat.connection_id,
                    chat.connection_name,
                    context,
                    messages
                ],
            )?;
            Ok(self.db.last_insert_rowid())
        } else {
            self.db.execute(
                "UPDATE ai_chats SET title = ?2, context = ?3, messages = ?4,
                 updated_at = datetime('now') WHERE id = ?1",
                params![chat.id, chat.title, context, messages],
            )?;
            Ok(chat.id)
        }
    }

    /// Conversations, newest first.
    pub fn list_ai_chats(&self, limit: usize) -> Result<Vec<AiChatSummary>> {
        let mut stmt = self.db.prepare(
            "SELECT id, title, connection_name, updated_at, messages FROM ai_chats
             ORDER BY updated_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            let messages: String = r.get(4)?;
            Ok(AiChatSummary {
                id: r.get(0)?,
                title: r.get(1)?,
                connection_name: r.get(2)?,
                updated_at: r.get(3)?,
                message_count: serde_json::from_str::<Vec<serde_json::Value>>(&messages)
                    .map_or(0, |m| m.len()),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_ai_chat(&self, id: i64) -> Result<Option<AiChat>> {
        let row = self
            .db
            .query_row(
                "SELECT id, title, connection_id, connection_name, context, messages, updated_at
                 FROM ai_chats WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, title, connection_id, connection_name, context, messages, updated_at)) = row
        else {
            return Ok(None);
        };
        Ok(Some(AiChat {
            id,
            title,
            connection_id,
            connection_name,
            context: serde_json::from_str(&context).unwrap_or_default(),
            messages: serde_json::from_str(&messages)?,
            updated_at,
        }))
    }

    pub fn delete_ai_chat(&self, id: i64) -> Result<()> {
        self.db
            .execute("DELETE FROM ai_chats WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Pinned tabs of a connection, in tab order.
    pub fn list_pinned_tabs(&self, connection_id: &str) -> Result<Vec<PinnedTab>> {
        let mut stmt = self.db.prepare(
            "SELECT id, connection_id, title, sql, position, pinned, file_path FROM pinned_tabs
             WHERE connection_id = ?1 ORDER BY position, id",
        )?;
        let rows = stmt.query_map([connection_id], |r| {
            Ok(PinnedTab {
                id: r.get(0)?,
                connection_id: r.get(1)?,
                title: r.get(2)?,
                sql: r.get(3)?,
                position: r.get(4)?,
                pinned: r.get(5)?,
                file_path: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inserts a tab with id 0, otherwise updates it. Returns it with its id.
    pub fn save_pinned_tab(&self, tab: &PinnedTab) -> Result<PinnedTab> {
        let mut saved = tab.clone();
        if tab.id == 0 {
            self.db.execute(
                "INSERT INTO pinned_tabs (connection_id, title, sql, position, pinned, file_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    tab.connection_id,
                    tab.title,
                    tab.sql,
                    tab.position,
                    tab.pinned,
                    tab.file_path
                ],
            )?;
            saved.id = self.db.last_insert_rowid();
        } else {
            self.db.execute(
                "UPDATE pinned_tabs SET title = ?2, sql = ?3, position = ?4, pinned = ?5,
                 file_path = ?6, updated_at = datetime('now')
                 WHERE id = ?1",
                params![
                    tab.id,
                    tab.title,
                    tab.sql,
                    tab.position,
                    tab.pinned,
                    tab.file_path
                ],
            )?;
        }
        Ok(saved)
    }

    /// Drops a connection's unpinned (draft) tabs, once the user let them go.
    pub fn delete_draft_tabs(&self, connection_id: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM pinned_tabs WHERE connection_id = ?1 AND pinned = 0",
            [connection_id],
        )?;
        Ok(())
    }

    pub fn delete_pinned_tab(&self, id: i64) -> Result<()> {
        self.db
            .execute("DELETE FROM pinned_tabs WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_list_update_delete() {
        let store = Store::open_in_memory().unwrap();
        let mut b = ConnectionProfile::new("b-staging", "10.0.0.2", "app");
        let a = ConnectionProfile::new("a-local", "127.0.0.1", "root");
        store.save_connection(&b).unwrap();
        store.save_connection(&a).unwrap();

        let names: Vec<_> = store
            .list_connections()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["a-local", "b-staging"]);

        b.port = 3307;
        store.save_connection(&b).unwrap();
        assert_eq!(store.get_connection(&b.id).unwrap().unwrap().port, 3307);

        store.delete_connection(&a.id).unwrap();
        assert_eq!(store.list_connections().unwrap().len(), 1);
        assert!(store.get_connection(&a.id).unwrap().is_none());
    }

    #[test]
    fn record_forced_follows_saved_profile() {
        let store = Store::open_in_memory().unwrap();
        let mut p = ConnectionProfile::new("prod", "10.0.0.9", "app");
        assert!(!store.record_forced(&p.id).unwrap());
        store.save_connection(&p).unwrap();
        assert!(!store.record_forced(&p.id).unwrap());
        p.record_mode = RecordMode::Forced;
        store.save_connection(&p).unwrap();
        assert!(store.record_forced(&p.id).unwrap());
        // Profiles saved before the setting existed follow the app setting.
        let old: ConnectionProfile =
            serde_json::from_str(r#"{"id":"x","name":"x","host":"h","port":3306,"user":"u"}"#)
                .unwrap();
        assert_eq!(old.record_mode, RecordMode::Auto);
    }

    #[test]
    fn favorites_then_recent_then_name() {
        let store = Store::open_in_memory().unwrap();
        let a = ConnectionProfile::new("a", "h", "u");
        let b = ConnectionProfile::new("b", "h", "u");
        let c = ConnectionProfile::new("c", "h", "u");
        let d = ConnectionProfile::new("d", "h", "u");
        for p in [&a, &b, &c, &d] {
            store.save_connection(p).unwrap();
        }
        store.touch_connection(&c.id).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.touch_connection(&b.id).unwrap();
        store.set_favorite(&d.id, true).unwrap();

        let list = store.list_connections().unwrap();
        let names: Vec<_> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["d", "b", "c", "a"]);
        assert!(list[0].favorite);
        assert!(list[1].last_used_at.is_some());

        // Saving an edited profile keeps its usage time.
        let mut b2 = list[1].clone();
        b2.port = 3307;
        store.save_connection(&b2).unwrap();
        assert_eq!(store.list_connections().unwrap()[1].name, "b");
    }

    fn record(name: &str, started_at: &str, sql: &str, error: Option<&str>) -> ExecutionRecord {
        ExecutionRecord {
            id: 0,
            connection_id: Some(name.into()),
            connection_name: name.into(),
            connection_kind: ConnectionKind::Mysql,
            server: "u@h:3306".into(),
            server_version: "11.4".into(),
            database: None,
            started_at: started_at.into(),
            duration_ms: 1.0,
            sql: sql.into(),
            statements: vec![crate::RecordedStatement {
                sql: sql.into(),
                duration_ms: 1.0,
                affected_rows: 3,
                returned_rows: None,
                warnings: 0,
                info: String::new(),
                columns: vec![],
                preview: vec![],
            }],
            statement_count: 1,
            affected_rows: 3,
            returned_rows: 0,
            error_code: error.map(|_| 1146),
            error_message: error.map(String::from),
            failed_statement: None,
            note: String::new(),
        }
    }

    #[test]
    fn records_round_trip_and_filter() {
        let store = Store::open_in_memory().unwrap();
        let a = store
            .insert_record(&record(
                "prod",
                "2026-10-01T10:00:00+00:00",
                "UPDATE t SET x = 1",
                None,
            ))
            .unwrap();
        let b = store
            .insert_record(&record(
                "prod",
                "2026-10-20T10:00:00+00:00",
                "SELECT * FROM nope",
                Some("no table"),
            ))
            .unwrap();
        let c = store
            .insert_record(&record(
                "dev",
                "2026-10-21T10:00:00+00:00",
                "DELETE FROM t",
                None,
            ))
            .unwrap();

        let all = store.list_records(&RecordFilter::default()).unwrap();
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), [c, b, a]);
        assert!(all[0].statements.is_empty(), "list leaves statements out");

        let ids = |f: RecordFilter| -> Vec<i64> {
            store
                .list_records(&f)
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect()
        };
        assert_eq!(
            ids(RecordFilter {
                connection_id: Some("prod".into()),
                ..Default::default()
            }),
            [b, a]
        );
        assert_eq!(
            ids(RecordFilter {
                text: Some("update".into()),
                ..Default::default()
            }),
            [a]
        );
        assert_eq!(
            ids(RecordFilter {
                text: Some("no table".into()),
                ..Default::default()
            }),
            [b]
        );
        assert_eq!(
            ids(RecordFilter {
                only_errors: true,
                ..Default::default()
            }),
            [b]
        );
        assert_eq!(
            ids(RecordFilter {
                from: Some("2026-10-15".into()),
                to: Some("2026-10-21".into()),
                ..Default::default()
            }),
            [b]
        );
        assert_eq!(
            ids(RecordFilter {
                limit: Some(1),
                offset: Some(1),
                ..Default::default()
            }),
            [b]
        );

        let full = store.get_record(a).unwrap().unwrap();
        assert_eq!(full.statements.len(), 1);
        assert_eq!(full.statements[0].affected_rows, 3);

        store.set_record_note(a, "月底結帳").unwrap();
        assert_eq!(store.get_record(a).unwrap().unwrap().note, "月底結帳");
        assert_eq!(
            ids(RecordFilter {
                text: Some("結帳".into()),
                ..Default::default()
            }),
            [a]
        );

        // Report order is oldest first.
        let picked = store.get_records(&[c, a]).unwrap();
        assert_eq!(picked.iter().map(|r| r.id).collect::<Vec<_>>(), [a, c]);

        let mut redis = record("cache", "2026-10-22T10:00:00+00:00", "GET k", None);
        redis.connection_kind = ConnectionKind::Redis;
        let d = store.insert_record(&redis).unwrap();
        assert_eq!(
            ids(RecordFilter {
                connection_kind: Some(ConnectionKind::Redis),
                ..Default::default()
            }),
            [d]
        );
        assert_eq!(
            ids(RecordFilter {
                connection_kind: Some(ConnectionKind::Mysql),
                ..Default::default()
            }),
            [c, b, a]
        );
        assert_eq!(
            store.get_record(d).unwrap().unwrap().connection_kind,
            ConnectionKind::Redis
        );
        store.delete_records(&[d]).unwrap();

        store.delete_records(&[a, c]).unwrap();
        assert_eq!(ids(RecordFilter::default()), [b]);
    }

    #[test]
    fn settings_default_then_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_settings().unwrap(), AppSettings::default());
        let s = AppSettings {
            editor_theme: "dracula".into(),
            max_rows: 200,
            ..Default::default()
        };
        store.save_settings(&s).unwrap();
        assert_eq!(store.get_settings().unwrap(), s);
        // Unknown or missing fields from another version still load.
        store
            .db
            .execute(
                "UPDATE settings SET value = '{\"editor_font_size\": 16, \"gone\": 1}'",
                [],
            )
            .unwrap();
        let loaded = store.get_settings().unwrap();
        assert_eq!(loaded.editor_font_size, 16);
        assert_eq!(loaded.max_rows, crate::DEFAULT_MAX_ROWS);
    }

    #[test]
    fn ai_chats_are_kept() {
        use crate::ai::{AiContext, ChatMessage};
        let store = Store::open_in_memory().unwrap();
        let mut chat = AiChat {
            id: 0,
            title: "為什麼慢".into(),
            connection_id: Some("c1".into()),
            connection_name: "本機".into(),
            context: AiContext {
                sql: "SELECT 1".into(),
                ..Default::default()
            },
            messages: vec![ChatMessage::new("user", "為什麼慢")],
            updated_at: String::new(),
        };
        chat.id = store.save_ai_chat(&chat).unwrap();
        chat.messages.push(ChatMessage::new("assistant", "加索引"));
        store.save_ai_chat(&chat).unwrap();
        let list = store.list_ai_chats(10).unwrap();
        assert_eq!((list.len(), list[0].message_count), (1, 2));
        let loaded = store.get_ai_chat(chat.id).unwrap().unwrap();
        assert_eq!(
            (loaded.messages, loaded.context),
            (chat.messages, chat.context)
        );
        store.delete_ai_chat(chat.id).unwrap();
        assert!(store.get_ai_chat(chat.id).unwrap().is_none());
    }

    #[test]
    fn snippets_start_seeded_and_can_be_edited() {
        let store = Store::open_in_memory().unwrap();
        let seeded = store.list_snippets().unwrap();
        assert!(seeded.iter().any(|s| s.prefix == "sel"));

        let mut mine = store
            .save_snippet(&Snippet {
                id: 0,
                name: "最近訂單".into(),
                prefix: "aaa".into(),
                body: "SELECT * FROM orders ORDER BY id DESC LIMIT ${n};".into(),
            })
            .unwrap();
        assert!(mine.id > 0);
        assert_eq!(store.list_snippets().unwrap()[0], mine);

        mine.body = "SELECT 1;".into();
        store.save_snippet(&mine).unwrap();
        assert_eq!(store.list_snippets().unwrap()[0].body, "SELECT 1;");

        store.delete_snippet(mine.id).unwrap();
        assert_eq!(store.list_snippets().unwrap().len(), seeded.len());
    }

    #[test]
    fn passwords_only_go_where_they_were_saved_for() {
        let mut store = Store::open_in_memory().unwrap();
        let vault = crate::secrets::MemoryVault::default();
        store.use_vault(Box::new(vault.clone())).unwrap();
        let mut p = ConnectionProfile::new("prod", "db.example", "app");
        p.password = Some("s3cret".into());
        p.ssl.mode = crate::profile::SslMode::Required;
        store.save_connection(&p).unwrap();

        // The database is changed behind the app's back to point elsewhere.
        let mut planted = p.clone();
        planted.host = "attacker.example".into();
        planted.password = None;
        store
            .db
            .execute(
                "UPDATE connections SET profile = ?1",
                [serde_json::to_string(&planted).unwrap()],
            )
            .unwrap();
        assert_eq!(store.get_connection(&p.id).unwrap().unwrap().password, None);

        // Turning TLS off or clearing the SSH pin counts as elsewhere too.
        let mut weaker = p.clone();
        weaker.ssl.mode = crate::profile::SslMode::Disabled;
        weaker.password = None;
        store
            .db
            .execute(
                "UPDATE connections SET profile = ?1",
                [serde_json::to_string(&weaker).unwrap()],
            )
            .unwrap();
        assert_eq!(store.get_connection(&p.id).unwrap().unwrap().password, None);
        // Saving that profile (it has no password) keeps the real one.
        store.save_connection(&weaker).unwrap();
        assert!(
            vault
                .0
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("s3cret")
        );

        // Secrets saved before they knew their destination: the MCP server
        // leaves them alone; the app uses them and binds them.
        *vault.0.lock().unwrap() = Some(format!(r#"{{"{}":{{"password":"old"}}}}"#, p.id));
        let mut mcp = Store::open_in_memory().unwrap();
        mcp.save_connection(&planted).unwrap();
        mcp.use_vault(Box::new(vault.clone())).unwrap();
        assert_eq!(mcp.get_connection(&p.id).unwrap().unwrap().password, None);
        let mut app = Store::open_in_memory().unwrap();
        app.save_connection(&ConnectionProfile {
            password: None,
            ..p.clone()
        })
        .unwrap();
        app.use_vault(Box::new(vault.clone())).unwrap();
        app.trust_unbound_secrets();
        assert_eq!(
            app.get_connection(&p.id)
                .unwrap()
                .unwrap()
                .password
                .as_deref(),
            Some("old")
        );
        assert!(
            vault
                .0
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("db.example")
        );

        // A different CA file counts as elsewhere.
        let mut other_ca = p.clone();
        other_ca.ssl.ca_path = Some("/tmp/evil-ca.pem".into());
        other_ca.password = None;
        app.db
            .execute(
                "UPDATE connections SET profile = ?1",
                [serde_json::to_string(&other_ca).unwrap()],
            )
            .unwrap();
        assert_eq!(app.get_connection(&p.id).unwrap().unwrap().password, None);

        // Saving a profile without passwords leaves unbound ones alone.
        *vault.0.lock().unwrap() = Some(format!(r#"{{"{}":{{"password":"old"}}}}"#, p.id));
        let mut mcp = Store::open_in_memory().unwrap();
        mcp.use_vault(Box::new(vault.clone())).unwrap();
        mcp.save_connection(&planted).unwrap();
        assert!(vault.0.lock().unwrap().as_deref().unwrap().contains("old"));

        // A password planted in the file for elsewhere doesn't replace the
        // vault's when the vault is opened.
        let mut real = Store::open_in_memory().unwrap();
        real.use_vault(Box::new(vault.clone())).unwrap();
        real.trust_unbound_secrets();
        real.save_connection(&p).unwrap();
        let mut dummy = planted.clone();
        dummy.password = Some("planted".into());
        let mut again = Store::open_in_memory().unwrap();
        again
            .db
            .execute(
                "INSERT INTO connections (id, name, profile) VALUES (?1, ?2, ?3)",
                params![p.id, p.name, serde_json::to_string(&dummy).unwrap()],
            )
            .unwrap();
        again.use_vault(Box::new(vault.clone())).unwrap();
        let kept = vault.0.lock().unwrap().clone().unwrap();
        assert!(kept.contains("s3cret") && !kept.contains("planted"));
        let json: String = again
            .db
            .query_row("SELECT profile FROM connections", [], |r| r.get(0))
            .unwrap();
        assert!(!json.contains("planted"));
    }

    #[test]
    fn passwords_live_in_the_vault() {
        use crate::profile::{SshAuth, SshConfig};
        use crate::secrets::MemoryVault;
        let mut store = Store::open_in_memory().unwrap();
        // Saved before there was a vault: moved over when one is set.
        let mut p = ConnectionProfile::new("prod", "db", "app");
        p.password = Some("s3cret".into());
        p.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "me".into(),
            auth: SshAuth::Password {
                password: "sshpw".into(),
            },
            known_host_fingerprint: None,
        });
        store.save_connection(&p).unwrap();
        let vault = MemoryVault::default();
        store.use_vault(Box::new(vault.clone())).unwrap();

        let raw: String = store
            .db
            .query_row("SELECT profile FROM connections", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("s3cret") && !raw.contains("sshpw"), "{raw}");
        let kept = vault.0.lock().unwrap().clone().unwrap();
        assert!(kept.contains("s3cret") && kept.contains("sshpw"));

        // Reading one fills them back in, so a copy carries them too; the
        // list leaves them out.
        assert_eq!(store.get_connection(&p.id).unwrap().unwrap(), p);
        assert_eq!(store.list_connections().unwrap()[0].password, None);
        let copy = ConnectionProfile {
            id: "copy".into(),
            ..store.get_connection(&p.id).unwrap().unwrap()
        };
        store.save_connection(&copy).unwrap();
        assert_eq!(
            store
                .get_connection("copy")
                .unwrap()
                .unwrap()
                .password
                .as_deref(),
            Some("s3cret")
        );

        // Clearing the password removes it; deleting drops the rest.
        let mut cleared = p.clone();
        cleared.password = Some(String::new());
        store.save_connection(&cleared).unwrap();
        assert_eq!(store.get_connection(&p.id).unwrap().unwrap().password, None);
        store.delete_connection(&p.id).unwrap();
        store.delete_connection("copy").unwrap();
        assert_eq!(vault.0.lock().unwrap().as_deref(), Some("{}"));
    }

    #[test]
    fn moved_passwords_leave_no_trace_in_the_file() {
        let dir = std::env::temp_dir().join(format!("mydb-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.db");
        let mut p = ConnectionProfile::new("prod", "db", "app");
        p.password = Some("Zq7-unique-secret".into());
        Store::open(&path).unwrap().save_connection(&p).unwrap();

        let mut store = Store::open(&path).unwrap();
        store
            .use_vault(Box::new(crate::secrets::MemoryVault::default()))
            .unwrap();
        // A later password change must not linger either.
        p.password = Some("Zq7-second-secret".into());
        store.save_connection(&p).unwrap();
        // Read the files while the app still has them open (WAL included).

        for entry in std::fs::read_dir(&dir).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("Zq7-"),
                "password left in the database files"
            );
        }
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pinned_tabs_per_connection() {
        let store = Store::open_in_memory().unwrap();
        let tab = |conn: &str, title: &str, position| PinnedTab {
            id: 0,
            connection_id: conn.into(),
            title: title.into(),
            sql: format!("SELECT '{title}';"),
            position,
            pinned: true,
            file_path: None,
        };
        let b = store.save_pinned_tab(&tab("c1", "b", 1)).unwrap();
        let a = store.save_pinned_tab(&tab("c1", "a", 0)).unwrap();
        store.save_pinned_tab(&tab("c2", "other", 0)).unwrap();

        let titles = |c: &str| -> Vec<String> {
            store
                .list_pinned_tabs(c)
                .unwrap()
                .into_iter()
                .map(|t| t.title)
                .collect()
        };
        assert_eq!(titles("c1"), ["a", "b"]);

        let mut a2 = a.clone();
        a2.sql = "SELECT 42;".into();
        store.save_pinned_tab(&a2).unwrap();
        assert_eq!(store.list_pinned_tabs("c1").unwrap()[0].sql, "SELECT 42;");

        store.delete_pinned_tab(b.id).unwrap();
        assert_eq!(titles("c1"), ["a"]);
        assert_eq!(titles("c2"), ["other"]);

        // Drafts come back after a crash and go once let go.
        let draft = PinnedTab {
            pinned: false,
            file_path: Some("/tmp/q.sql".into()),
            ..tab("c1", "draft", 2)
        };
        store.save_pinned_tab(&draft).unwrap();
        assert_eq!(titles("c1"), ["a", "draft"]);
        assert_eq!(
            store.list_pinned_tabs("c1").unwrap()[1]
                .file_path
                .as_deref(),
            Some("/tmp/q.sql")
        );
        store.delete_draft_tabs("c1").unwrap();
        assert_eq!(titles("c1"), ["a"]);
    }

    #[test]
    fn reopening_does_not_rerun_migrations() {
        let dir = std::env::temp_dir().join(format!("mydb-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.db");
        let p = ConnectionProfile::new("x", "h", "u");
        Store::open(&path).unwrap().save_connection(&p).unwrap();
        let again = Store::open(&path).unwrap();
        assert_eq!(again.list_connections().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
