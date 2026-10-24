//! Local app database (SQLite): saved connections and execution records.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;
use crate::profile::ConnectionProfile;
use crate::records::{ExecutionRecord, RecordFilter};
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
];

const RECORD_COLUMNS: &str = "id, connection_id, connection_name, server, server_version, database,
    started_at, duration_ms, sql, statements, statement_count, affected_rows, returned_rows,
    error_code, error_message, failed_statement, note";

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
        },
        r.get(9)?,
    ))
}

pub struct Store {
    db: Connection,
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
        let mut store = Self { db };
        store.migrate()?;
        Ok(store)
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
    /// name.
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
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    /// Inserts or replaces a profile by id.
    pub fn save_connection(&self, profile: &ConnectionProfile) -> Result<()> {
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
                affected_rows, returned_rows, error_code, error_message, failed_statement, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
    /// Pinned tabs of a connection, in tab order.
    pub fn list_pinned_tabs(&self, connection_id: &str) -> Result<Vec<PinnedTab>> {
        let mut stmt = self.db.prepare(
            "SELECT id, connection_id, title, sql, position FROM pinned_tabs
             WHERE connection_id = ?1 ORDER BY position, id",
        )?;
        let rows = stmt.query_map([connection_id], |r| {
            Ok(PinnedTab {
                id: r.get(0)?,
                connection_id: r.get(1)?,
                title: r.get(2)?,
                sql: r.get(3)?,
                position: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Inserts a tab with id 0, otherwise updates it. Returns it with its id.
    pub fn save_pinned_tab(&self, tab: &PinnedTab) -> Result<PinnedTab> {
        let mut saved = tab.clone();
        if tab.id == 0 {
            self.db.execute(
                "INSERT INTO pinned_tabs (connection_id, title, sql, position) VALUES (?1, ?2, ?3, ?4)",
                params![tab.connection_id, tab.title, tab.sql, tab.position],
            )?;
            saved.id = self.db.last_insert_rowid();
        } else {
            self.db.execute(
                "UPDATE pinned_tabs SET title = ?2, sql = ?3, position = ?4, updated_at = datetime('now')
                 WHERE id = ?1",
                params![tab.id, tab.title, tab.sql, tab.position],
            )?;
        }
        Ok(saved)
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
    fn pinned_tabs_per_connection() {
        let store = Store::open_in_memory().unwrap();
        let tab = |conn: &str, title: &str, position| PinnedTab {
            id: 0,
            connection_id: conn.into(),
            title: title.into(),
            sql: format!("SELECT '{title}';"),
            position,
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
