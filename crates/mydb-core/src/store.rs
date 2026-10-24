//! Local app database (SQLite): saved connections now, execution records next.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;
use crate::profile::ConnectionProfile;

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
];

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

    pub fn list_connections(&self) -> Result<Vec<ConnectionProfile>> {
        let mut stmt = self
            .db
            .prepare("SELECT profile FROM connections ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
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
            params![profile.id, profile.name, serde_json::to_string(profile)?],
        )?;
        Ok(())
    }

    pub fn delete_connection(&self, id: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM connections WHERE id = ?1", [id])?;
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
