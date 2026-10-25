//! Schema management: databases and their character sets, table structure,
//! indexes and foreign keys, views, routines, triggers and events.
//!
//! As in [`crate::admin`], a change arrives as a structured
//! [`SchemaChange`] and is turned into SQL here, so the preview the user
//! confirms and the statements that run come from the same code. Every run
//! is kept as an execution record, together with the definition an object
//! had before it was replaced or dropped.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::admin::{self, Dialect, first_cell, ident, string_literal};
use crate::error::{CoreError, Result};
use crate::session::{ExecutionOutcome, Session, TextRows};

/// Which server it is, for syntax that differs between MySQL and MariaDB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Flavor {
    pub mariadb: bool,
    pub version: (u32, u32, u32),
}

impl Flavor {
    /// From a version string such as `8.0.36` or `10.11.6-MariaDB-1:10.11.6+maria~ubu2204`.
    pub fn parse(version: &str) -> Self {
        let mut nums = version
            .split(|c: char| !c.is_ascii_digit())
            .take(3)
            .map(|n| n.parse().unwrap_or(0));
        let mut next = || nums.next().unwrap_or(0);
        Self {
            mariadb: version.to_ascii_lowercase().contains("mariadb"),
            version: (next(), next(), next()),
        }
    }

    fn at_least(&self, v: (u32, u32, u32)) -> bool {
        self.version >= v
    }

    /// `CREATE OR REPLACE` for procedures, functions, triggers and events.
    fn or_replace(&self) -> bool {
        self.mariadb && self.at_least((10, 1, 4))
    }

    /// `RENAME INDEX` and `RENAME COLUMN`.
    fn renames(&self) -> bool {
        if self.mariadb {
            self.at_least((10, 5, 2))
        } else {
            self.at_least((8, 0, 0))
        }
    }

    /// `information_schema.COLUMNS.COLUMN_DEFAULT` is written as SQL
    /// (strings quoted, `NULL` spelled out).
    fn sql_defaults(&self) -> bool {
        self.mariadb && self.at_least((10, 2, 7))
    }
}

/// What a plan needs to know about the connection.
#[derive(Debug, Clone, Copy)]
pub struct Ctx {
    pub dialect: Dialect,
    pub flavor: Flavor,
}

/// A character set and its collations.
#[derive(Debug, Clone, Serialize)]
pub struct CharsetInfo {
    pub name: String,
    pub description: String,
    pub default_collation: String,
    pub collations: Vec<String>,
}

/// Lists for the pickers, and server defaults.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaMeta {
    pub flavor: Flavor,
    pub charsets: Vec<CharsetInfo>,
    /// Engines that can be used, the default first.
    pub engines: Vec<String>,
    pub server_charset: String,
    pub server_collation: String,
}

/// A database's defaults.
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseOptions {
    pub name: String,
    pub charset: String,
    pub collation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    View,
    Procedure,
    Function,
    Trigger,
    Event,
}

impl ObjectKind {
    pub fn keyword(self) -> &'static str {
        match self {
            ObjectKind::View => "VIEW",
            ObjectKind::Procedure => "PROCEDURE",
            ObjectKind::Function => "FUNCTION",
            ObjectKind::Trigger => "TRIGGER",
            ObjectKind::Event => "EVENT",
        }
    }

    fn from_keyword(word: &str) -> Option<Self> {
        Some(match word.to_ascii_uppercase().as_str() {
            "VIEW" => ObjectKind::View,
            "PROCEDURE" => ObjectKind::Procedure,
            "FUNCTION" => ObjectKind::Function,
            "TRIGGER" => ObjectKind::Trigger,
            "EVENT" => ObjectKind::Event,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            ObjectKind::View => "檢視",
            ObjectKind::Procedure => "預存程序",
            ObjectKind::Function => "函式",
            ObjectKind::Trigger => "觸發器",
            ObjectKind::Event => "事件",
        }
    }
}

/// A view, routine, trigger or event, as listed.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaObject {
    pub kind: ObjectKind,
    pub name: String,
    /// Kind-specific summary: a trigger's timing and table, an event's
    /// schedule, a function's return type.
    pub detail: String,
    pub definer: String,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub comment: String,
    /// Events only.
    pub enabled: Option<bool>,
}

/// The objects of a database, with the server's event scheduler state.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectList {
    pub objects: Vec<SchemaObject>,
    /// `ON`, `OFF` or `DISABLED`; `None` if it could not be read.
    pub event_scheduler: Option<String>,
}

/// `SHOW CREATE …` of an object.
#[derive(Debug, Clone, Serialize)]
pub struct ObjectDefinition {
    pub definition: String,
    /// The `sql_mode` it was created under (not for views).
    pub sql_mode: Option<String>,
}

/// One column of a table, as read or as edited.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ColumnDesign {
    /// Its name on the server; `None` for a column being added.
    #[serde(default)]
    pub original: Option<String>,
    pub name: String,
    /// Full type as written in SQL, e.g. `varchar(64)` or `int unsigned`.
    pub column_type: String,
    /// Text columns only.
    #[serde(default)]
    pub charset: Option<String>,
    #[serde(default)]
    pub collation: Option<String>,
    #[serde(default)]
    pub nullable: bool,
    /// The default as SQL: `'abc'`, `0`, `CURRENT_TIMESTAMP`, `(uuid())`.
    /// `None` for no default (or the implicit `NULL`).
    #[serde(default)]
    pub default: Option<String>,
    /// E.g. `CURRENT_TIMESTAMP`.
    #[serde(default)]
    pub on_update: Option<String>,
    #[serde(default)]
    pub auto_increment: bool,
    /// Expression of a generated column.
    #[serde(default)]
    pub generated: Option<String>,
    /// A generated column is stored rather than virtual.
    #[serde(default)]
    pub stored: bool,
    #[serde(default)]
    pub invisible: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    Primary,
    Unique,
    Index,
    Fulltext,
    Spatial,
}

/// One column (or expression) of an index.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct IndexPart {
    #[serde(default)]
    pub column: String,
    /// Prefix length.
    #[serde(default)]
    pub length: Option<u32>,
    #[serde(default)]
    pub descending: bool,
    /// A functional key part (MySQL 8), as SQL without the parentheses.
    #[serde(default)]
    pub expression: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDesign {
    #[serde(default)]
    pub original: Option<String>,
    /// `PRIMARY` for the primary key.
    pub name: String,
    pub kind: IndexKind,
    pub parts: Vec<IndexPart>,
    /// `HASH` and the like; `None` for the engine's default.
    #[serde(default)]
    pub using: Option<String>,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyDesign {
    #[serde(default)]
    pub original: Option<String>,
    pub name: String,
    pub columns: Vec<String>,
    /// `None` for the table's own database.
    #[serde(default)]
    pub ref_database: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `RESTRICT`, `CASCADE`, `SET NULL`, `NO ACTION` or `SET DEFAULT`.
    #[serde(default)]
    pub on_delete: String,
    #[serde(default)]
    pub on_update: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TableOptions {
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub charset: Option<String>,
    #[serde(default)]
    pub collation: Option<String>,
    #[serde(default)]
    pub comment: String,
    /// Next value; changes on every insert, so it is only set when edited.
    #[serde(default)]
    pub auto_increment: Option<u64>,
}

/// A table's structure: what the designer shows and edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDesign {
    pub name: String,
    pub columns: Vec<ColumnDesign>,
    #[serde(default)]
    pub indexes: Vec<IndexDesign>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyDesign>,
    #[serde(default)]
    pub options: TableOptions,
}

/// A change the user asked for, turned into SQL by [`plan`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchemaChange {
    CreateDatabase {
        name: String,
        #[serde(default)]
        charset: Option<String>,
        #[serde(default)]
        collation: Option<String>,
    },
    AlterDatabase {
        name: String,
        #[serde(default)]
        charset: Option<String>,
        #[serde(default)]
        collation: Option<String>,
        /// Also convert every table (and its text columns).
        #[serde(default)]
        convert_tables: bool,
    },
    DropDatabase {
        name: String,
    },
    CreateTable {
        database: String,
        design: Box<TableDesign>,
    },
    AlterTable {
        database: String,
        /// The structure as loaded; the change is refused if the table no
        /// longer looks like this.
        original: Box<TableDesign>,
        design: Box<TableDesign>,
        /// Convert existing text columns to the table's character set.
        #[serde(default)]
        convert_charset: bool,
    },
    DropTable {
        database: String,
        table: String,
    },
    /// Creates a view, routine, trigger or event from `definition`, or
    /// replaces the one named `original`.
    SaveObject {
        database: String,
        object_kind: ObjectKind,
        #[serde(default)]
        original: Option<String>,
        definition: String,
    },
    DropObject {
        database: String,
        object_kind: ObjectKind,
        name: String,
    },
    SetEventEnabled {
        database: String,
        name: String,
        enabled: bool,
    },
    SetEventScheduler {
        on: bool,
    },
}

impl SchemaChange {
    /// The database the statements run in, if any.
    fn database(&self) -> Option<&str> {
        match self {
            SchemaChange::CreateDatabase { .. }
            | SchemaChange::DropDatabase { .. }
            | SchemaChange::SetEventScheduler { .. } => None,
            SchemaChange::AlterDatabase { name, .. } => Some(name),
            SchemaChange::CreateTable { database, .. }
            | SchemaChange::AlterTable { database, .. }
            | SchemaChange::DropTable { database, .. }
            | SchemaChange::SaveObject { database, .. }
            | SchemaChange::DropObject { database, .. }
            | SchemaChange::SetEventEnabled { database, .. } => Some(database),
        }
    }
}

/// The statements a change runs, with what the user should know first.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DdlPlan {
    pub statements: Vec<String>,
    pub notes: Vec<String>,
}

/// A finished run, for the execution record.
#[derive(Debug)]
pub struct DdlRun {
    pub outcome: ExecutionOutcome,
    /// What ran, as one script.
    pub sql: String,
    pub database: Option<String>,
    /// Describes the change, with the definition replaced or dropped.
    pub note: String,
}

// ---------------------------------------------------------------------------
// Reading

pub async fn context(session: &Session) -> Result<Ctx> {
    Ok(Ctx {
        dialect: admin::dialect(session).await?,
        flavor: Flavor::parse(&session.info().server_version),
    })
}

pub async fn meta(session: &Session) -> Result<SchemaMeta> {
    let charsets = Rows(session.query_text("SHOW CHARACTER SET").await?);
    let collations = Rows(session.query_text("SHOW COLLATION").await?);
    let mut by_charset: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in 0..collations.len() {
        if let (Some(c), Some(cs)) = (collations.get(r, "Collation"), collations.get(r, "Charset"))
        {
            by_charset.entry(cs).or_default().push(c);
        }
    }
    let mut list: Vec<CharsetInfo> = (0..charsets.len())
        .filter_map(|r| {
            let name = charsets.get(r, "Charset")?;
            let mut collations = by_charset.remove(&name).unwrap_or_default();
            collations.sort();
            Some(CharsetInfo {
                description: charsets.get(r, "Description").unwrap_or_default(),
                default_collation: charsets.get(r, "Default collation").unwrap_or_default(),
                collations,
                name,
            })
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));

    let engines = Rows(session.query_text("SHOW ENGINES").await?);
    let mut engine_list = Vec::new();
    for r in 0..engines.len() {
        let (Some(name), Some(support)) = (engines.get(r, "Engine"), engines.get(r, "Support"))
        else {
            continue;
        };
        match support.to_ascii_uppercase().as_str() {
            "DEFAULT" => engine_list.insert(0, name),
            "YES" => engine_list.push(name),
            _ => {}
        }
    }
    let defaults = session
        .query_text("SELECT @@character_set_server, @@collation_server")
        .await?;
    let cell = |i: usize| {
        defaults
            .rows
            .first()
            .and_then(|r| r.get(i).cloned().flatten())
            .unwrap_or_default()
    };
    Ok(SchemaMeta {
        flavor: Flavor::parse(&session.info().server_version),
        charsets: list,
        engines: engine_list,
        server_charset: cell(0),
        server_collation: cell(1),
    })
}

pub async fn database_options(session: &Session, database: &str) -> Result<DatabaseOptions> {
    let rows = session
        .query_params(
            "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME
             FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
            &[database],
        )
        .await?;
    let row = rows
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| CoreError::Config {
            message: format!("找不到資料庫 {database}"),
        })?;
    let cell = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
    Ok(DatabaseOptions {
        name: database.to_string(),
        charset: cell(0),
        collation: cell(1),
    })
}

/// Views, procedures, functions, triggers and events of `database`.
pub async fn objects(session: &Session, database: &str) -> Result<ObjectList> {
    let mut objects = Vec::new();

    let views = Rows(
        session
            .query_params(
                "SELECT * FROM information_schema.VIEWS WHERE TABLE_SCHEMA = ?
                 ORDER BY TABLE_NAME",
                &[database],
            )
            .await?,
    );
    for r in 0..views.len() {
        objects.push(SchemaObject {
            kind: ObjectKind::View,
            name: views.text(r, "TABLE_NAME"),
            detail: match views.text(r, "IS_UPDATABLE").as_str() {
                "YES" => "可更新".into(),
                _ => String::new(),
            },
            definer: views.text(r, "DEFINER"),
            created: None,
            modified: None,
            comment: String::new(),
            enabled: None,
        });
    }

    let routines = Rows(
        session
            .query_params(
                "SELECT * FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ?
                 ORDER BY ROUTINE_TYPE DESC, ROUTINE_NAME",
                &[database],
            )
            .await?,
    );
    for r in 0..routines.len() {
        let kind = match routines.text(r, "ROUTINE_TYPE").as_str() {
            "PROCEDURE" => ObjectKind::Procedure,
            "FUNCTION" => ObjectKind::Function,
            // MariaDB packages are left out.
            _ => continue,
        };
        objects.push(SchemaObject {
            kind,
            name: routines.text(r, "ROUTINE_NAME"),
            detail: match kind {
                ObjectKind::Function => {
                    let returns = routines.text(r, "DTD_IDENTIFIER");
                    if returns.is_empty() {
                        String::new()
                    } else {
                        format!("RETURNS {returns}")
                    }
                }
                _ => String::new(),
            },
            definer: routines.text(r, "DEFINER"),
            created: routines.get(r, "CREATED"),
            modified: routines.get(r, "LAST_ALTERED"),
            comment: routines.text(r, "ROUTINE_COMMENT"),
            enabled: None,
        });
    }

    let triggers = Rows(
        session
            .query_params(
                "SELECT * FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ?
                 ORDER BY EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORDER",
                &[database],
            )
            .await?,
    );
    for r in 0..triggers.len() {
        objects.push(SchemaObject {
            kind: ObjectKind::Trigger,
            name: triggers.text(r, "TRIGGER_NAME"),
            detail: format!(
                "{} {} ON {}",
                triggers.text(r, "ACTION_TIMING"),
                triggers.text(r, "EVENT_MANIPULATION"),
                triggers.text(r, "EVENT_OBJECT_TABLE"),
            ),
            definer: triggers.text(r, "DEFINER"),
            created: triggers.get(r, "CREATED"),
            modified: None,
            comment: String::new(),
            enabled: None,
        });
    }

    let events = Rows(
        session
            .query_params(
                "SELECT * FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?
                 ORDER BY EVENT_NAME",
                &[database],
            )
            .await?,
    );
    for r in 0..events.len() {
        let schedule = if events.text(r, "EVENT_TYPE") == "ONE TIME" {
            format!("一次：{}", events.text(r, "EXECUTE_AT"))
        } else {
            let mut s = format!(
                "每 {} {}",
                events.text(r, "INTERVAL_VALUE").trim_matches('\''),
                events.text(r, "INTERVAL_FIELD")
            );
            if let Some(starts) = events.get(r, "STARTS") {
                s.push_str(&format!("，從 {starts}"));
            }
            if let Some(ends) = events.get(r, "ENDS") {
                s.push_str(&format!(" 到 {ends}"));
            }
            s
        };
        let last = events
            .get(r, "LAST_EXECUTED")
            .map(|t| format!("（上次執行 {t}）"))
            .unwrap_or_default();
        objects.push(SchemaObject {
            kind: ObjectKind::Event,
            name: events.text(r, "EVENT_NAME"),
            detail: format!("{schedule}{last}"),
            definer: events.text(r, "DEFINER"),
            created: events.get(r, "CREATED"),
            modified: events.get(r, "LAST_ALTERED"),
            comment: events.text(r, "EVENT_COMMENT"),
            enabled: Some(events.text(r, "STATUS") == "ENABLED"),
        });
    }

    let event_scheduler = first_cell(session, "SELECT @@GLOBAL.event_scheduler")
        .await
        .ok();
    Ok(ObjectList {
        objects,
        event_scheduler,
    })
}

pub async fn definition(
    session: &Session,
    database: &str,
    kind: ObjectKind,
    name: &str,
) -> Result<ObjectDefinition> {
    let rows = Rows(
        session
            .query_text(&format!(
                "SHOW CREATE {} {}",
                kind.keyword(),
                qualified(database, name)
            ))
            .await?,
    );
    let column = match kind {
        ObjectKind::View => "Create View",
        ObjectKind::Procedure => "Create Procedure",
        ObjectKind::Function => "Create Function",
        ObjectKind::Trigger => "SQL Original Statement",
        ObjectKind::Event => "Create Event",
    };
    let definition = rows.get(0, column).ok_or_else(|| CoreError::Config {
        message: format!(
            "讀不到 {} 的定義：可能沒有權限（需要是 definer，或有 SHOW_ROUTINE／SELECT ON mysql.proc 等權限）。",
            name
        ),
    })?;
    Ok(ObjectDefinition {
        definition,
        sql_mode: rows.get(0, "sql_mode"),
    })
}

/// The structure of a base table.
pub async fn table_design(session: &Session, database: &str, table: &str) -> Result<TableDesign> {
    let flavor = Flavor::parse(&session.info().server_version);
    let dialect = admin::dialect(session).await?;

    let info = Rows(
        session
            .query_params(
                "SELECT ENGINE, TABLE_COLLATION, TABLE_COMMENT, AUTO_INCREMENT, TABLE_TYPE
                 FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
                &[database, table],
            )
            .await?,
    );
    if info.len() == 0 {
        return Err(CoreError::Config {
            message: format!("找不到資料表 {database}.{table}"),
        });
    }
    let collation = info.get(0, "TABLE_COLLATION");
    let options = TableOptions {
        engine: info.get(0, "ENGINE"),
        charset: collation.as_deref().map(charset_of),
        collation,
        comment: info.text(0, "TABLE_COMMENT"),
        auto_increment: info.get(0, "AUTO_INCREMENT").and_then(|v| v.parse().ok()),
    };

    let cols = Rows(
        session
            .query_params(
                "SELECT * FROM information_schema.COLUMNS
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
                &[database, table],
            )
            .await?,
    );
    let columns = (0..cols.len())
        .map(|r| read_column(&cols, r, flavor, dialect))
        .collect();

    let stats = Rows(
        session
            .query_text(&format!("SHOW INDEX FROM {}", qualified(database, table)))
            .await?,
    );
    let mut indexes: Vec<IndexDesign> = Vec::new();
    for r in 0..stats.len() {
        let name = stats.text(r, "Key_name");
        let index_type = stats.text(r, "Index_type").to_ascii_uppercase();
        let part = IndexPart {
            column: stats.text(r, "Column_name"),
            length: stats.get(r, "Sub_part").and_then(|v| v.parse().ok()),
            descending: stats.text(r, "Collation") == "D",
            expression: stats.get(r, "Expression"),
        };
        match indexes.iter_mut().find(|i| i.name == name) {
            Some(ix) => ix.parts.push(part),
            None => indexes.push(IndexDesign {
                original: Some(name.clone()),
                kind: if name == "PRIMARY" {
                    IndexKind::Primary
                } else if index_type == "FULLTEXT" {
                    IndexKind::Fulltext
                } else if index_type == "SPATIAL" {
                    IndexKind::Spatial
                } else if stats.text(r, "Non_unique") == "0" {
                    IndexKind::Unique
                } else {
                    IndexKind::Index
                },
                name,
                parts: vec![part],
                using: (!matches!(index_type.as_str(), "BTREE" | "FULLTEXT" | "SPATIAL" | ""))
                    .then_some(index_type),
                comment: stats.text(r, "Index_comment"),
            }),
        }
    }

    let keys = Rows(
        session
            .query_params(
                "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA,
                        REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME
                 FROM information_schema.KEY_COLUMN_USAGE
                 WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL
                 ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
                &[database, table],
            )
            .await?,
    );
    let rules = Rows(
        session
            .query_params(
                "SELECT CONSTRAINT_NAME, UPDATE_RULE, DELETE_RULE
                 FROM information_schema.REFERENTIAL_CONSTRAINTS
                 WHERE CONSTRAINT_SCHEMA = ? AND TABLE_NAME = ?",
                &[database, table],
            )
            .await?,
    );
    let mut foreign_keys: Vec<ForeignKeyDesign> = Vec::new();
    for r in 0..keys.len() {
        let name = keys.text(r, "CONSTRAINT_NAME");
        let column = keys.text(r, "COLUMN_NAME");
        let ref_column = keys.text(r, "REFERENCED_COLUMN_NAME");
        if let Some(fk) = foreign_keys.iter_mut().find(|f| f.name == name) {
            fk.columns.push(column);
            fk.ref_columns.push(ref_column);
            continue;
        }
        let rule = (0..rules.len()).find(|&i| rules.text(i, "CONSTRAINT_NAME") == name);
        let ref_db = keys.text(r, "REFERENCED_TABLE_SCHEMA");
        foreign_keys.push(ForeignKeyDesign {
            original: Some(name.clone()),
            name,
            columns: vec![column],
            ref_database: (ref_db != database).then_some(ref_db),
            ref_table: keys.text(r, "REFERENCED_TABLE_NAME"),
            ref_columns: vec![ref_column],
            on_delete: rule
                .map(|i| rules.text(i, "DELETE_RULE"))
                .unwrap_or_default(),
            on_update: rule
                .map(|i| rules.text(i, "UPDATE_RULE"))
                .unwrap_or_default(),
        });
    }

    Ok(TableDesign {
        name: table.to_string(),
        columns,
        indexes,
        foreign_keys,
        options,
    })
}

fn read_column(cols: &Rows, r: usize, flavor: Flavor, dialect: Dialect) -> ColumnDesign {
    let name = cols.text(r, "COLUMN_NAME");
    let extra = cols.text(r, "EXTRA");
    let extra_upper = extra.to_ascii_uppercase();
    let expression = cols
        .get(r, "GENERATION_EXPRESSION")
        .filter(|e| !e.is_empty());
    let generated = expression.filter(|_| {
        cols.text(r, "IS_GENERATED") == "ALWAYS"
            || extra_upper.contains("VIRTUAL")
            || extra_upper.contains("STORED")
            || extra_upper.contains("PERSISTENT")
    });
    let nullable = cols.text(r, "IS_NULLABLE") == "YES";
    let data_type = cols.text(r, "DATA_TYPE");
    let default = if generated.is_some() {
        None
    } else {
        default_sql(
            cols.get(r, "COLUMN_DEFAULT").as_deref(),
            &extra,
            &data_type,
            flavor,
            dialect,
        )
    };
    ColumnDesign {
        original: Some(name.clone()),
        name,
        column_type: cols.text(r, "COLUMN_TYPE"),
        charset: cols.get(r, "CHARACTER_SET_NAME"),
        collation: cols.get(r, "COLLATION_NAME"),
        nullable,
        default,
        on_update: on_update(&extra),
        auto_increment: extra_upper.contains("AUTO_INCREMENT"),
        stored: generated.is_some()
            && (extra_upper.contains("STORED") || extra_upper.contains("PERSISTENT")),
        generated,
        invisible: extra_upper.contains("INVISIBLE"),
        comment: cols.text(r, "COLUMN_COMMENT"),
    }
}

/// `COLUMN_DEFAULT` turned into SQL that can follow `DEFAULT`.
pub fn default_sql(
    raw: Option<&str>,
    extra: &str,
    data_type: &str,
    flavor: Flavor,
    dialect: Dialect,
) -> Option<String> {
    let raw = raw?;
    if flavor.sql_defaults() {
        // Already SQL; NULL is the same as no default.
        return (!raw.eq_ignore_ascii_case("NULL")).then(|| raw.to_string());
    }
    let upper = raw.to_ascii_uppercase();
    let is_now = upper.starts_with("CURRENT_TIMESTAMP")
        && upper["CURRENT_TIMESTAMP".len()..]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '(' || c == ')');
    if extra.to_ascii_uppercase().contains("DEFAULT_GENERATED") {
        return Some(if is_now {
            raw.to_string()
        } else {
            format!("({raw})")
        });
    }
    let data_type = data_type.to_ascii_lowercase();
    if is_now && (data_type == "timestamp" || data_type == "datetime") {
        return Some(raw.to_string());
    }
    if data_type == "bit" && raw.starts_with("b'") {
        return Some(raw.to_string());
    }
    let numeric = matches!(
        data_type.as_str(),
        "tinyint"
            | "smallint"
            | "mediumint"
            | "int"
            | "integer"
            | "bigint"
            | "decimal"
            | "numeric"
            | "float"
            | "double"
            | "real"
    );
    if numeric && raw.parse::<f64>().is_ok() {
        return Some(raw.to_string());
    }
    Some(string_literal(raw, dialect))
}

/// The expression after `on update` in `EXTRA`.
fn on_update(extra: &str) -> Option<String> {
    let lower = extra.to_ascii_lowercase();
    let at = lower.find("on update ")? + "on update ".len();
    let rest = &extra[at..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].to_string()).filter(|s| !s.is_empty())
}

/// A collation's character set: the part before the first `_`.
fn charset_of(collation: &str) -> String {
    collation.split('_').next().unwrap_or(collation).to_string()
}

// ---------------------------------------------------------------------------
// Planning

/// The statements `change` would run, and what to know before running it.
pub async fn plan(session: &Session, change: &SchemaChange) -> Result<DdlPlan> {
    let ctx = context(session).await?;
    let mut plan = DdlPlan::default();
    match change {
        SchemaChange::CreateDatabase {
            name,
            charset,
            collation,
        } => {
            let mut sql = format!("CREATE DATABASE {}", ident(&non_empty(name, "資料庫名稱")?));
            sql.push_str(&charset_clause(charset, collation, " ")?);
            plan.statements.push(sql);
        }
        SchemaChange::AlterDatabase {
            name,
            charset,
            collation,
            convert_tables,
        } => {
            let clause = charset_clause(charset, collation, " ")?;
            if clause.is_empty() {
                return Err(invalid("請選擇字元集或定序"));
            }
            plan.statements
                .push(format!("ALTER DATABASE {}{clause}", ident(name)));
            if *convert_tables {
                let convert = charset_clause(charset, collation, " ")?;
                for table in base_tables(session, name).await? {
                    plan.statements.push(format!(
                        "ALTER TABLE {} CONVERT TO{convert}",
                        qualified(name, &table)
                    ));
                }
                plan.notes.push(
                    "CONVERT TO 會改寫每個資料表所有文字欄位的字元集，大資料表需要很久，期間會鎖住寫入。建議先備份。"
                        .into(),
                );
            } else {
                plan.notes.push(
                    "只會改變之後新建資料表的預設值；現有的資料表與欄位維持原本的字元集。".into(),
                );
            }
        }
        SchemaChange::DropDatabase { name } => {
            plan.statements
                .push(format!("DROP DATABASE {}", ident(name)));
            let tables = base_tables(session, name).await.unwrap_or_default();
            plan.notes.push(format!(
                "資料庫裡的 {} 個資料表、所有資料與預存程序都會刪除，無法復原。執行紀錄只會留下資料表清單，不會備份資料。",
                tables.len()
            ));
        }
        SchemaChange::CreateTable { database, design } => {
            plan.statements
                .push(create_table_sql(database, design, ctx)?);
        }
        SchemaChange::AlterTable {
            database,
            original,
            design,
            convert_charset,
        } => {
            plan.statements = alter_table_sql(database, original, design, *convert_charset, ctx)?;
            if !plan.statements.is_empty() {
                plan.notes.push(
                    "大資料表的 ALTER TABLE 可能需要一段時間，部分變更（例如改型別、CONVERT）會複製整個資料表並暫停寫入。"
                        .into(),
                );
            }
        }
        SchemaChange::DropTable { database, table } => {
            plan.statements
                .push(format!("DROP TABLE {}", qualified(database, table)));
            plan.notes.push(
                "資料表與裡面的資料都會刪除，無法復原。執行紀錄會留下原本的 CREATE TABLE，但不含資料。"
                    .into(),
            );
        }
        SchemaChange::SaveObject {
            database,
            object_kind,
            original,
            definition,
        } => {
            let saved =
                save_object_sql(database, *object_kind, original.as_deref(), definition, ctx)?;
            plan.statements = saved.statements;
            plan.notes = saved.notes;
            if let Some(orig) = original
                && *object_kind != ObjectKind::View
                && let Ok(old) = self::definition(session, database, *object_kind, orig).await
                && let Some(old_mode) = old.sql_mode
            {
                let now = first_cell(session, "SELECT @@SESSION.sql_mode").await?;
                if !same_mode(&old_mode, &now) {
                    plan.notes.push(format!(
                        "原本建立時的 sql_mode 是「{old_mode}」，目前連線是「{now}」；重新建立後會改用目前的 sql_mode。"
                    ));
                }
            }
        }
        SchemaChange::DropObject {
            database,
            object_kind,
            name,
        } => {
            plan.statements.push(format!(
                "DROP {} {}",
                object_kind.keyword(),
                qualified(database, name)
            ));
            plan.notes.push(
                "刪除後無法復原。原本的定義會存進執行紀錄，需要時可以從那裡重新建立。".into(),
            );
        }
        SchemaChange::SetEventEnabled {
            database,
            name,
            enabled,
        } => {
            plan.statements.push(format!(
                "ALTER EVENT {} {}",
                qualified(database, name),
                if *enabled { "ENABLE" } else { "DISABLE" }
            ));
        }
        SchemaChange::SetEventScheduler { on } => {
            plan.statements.push(format!(
                "SET GLOBAL event_scheduler = {}",
                if *on { "ON" } else { "OFF" }
            ));
            plan.notes.push(
                "需要 SUPER（或 MySQL 8 的 SYSTEM_VARIABLES_ADMIN）權限。SET GLOBAL 在伺服器重新啟動後會回到設定檔的值；要永久生效請修改 my.cnf 的 event_scheduler。如果伺服器以 event_scheduler=DISABLED 啟動，就無法在執行中打開。"
                    .into(),
            );
        }
    }
    Ok(plan)
}

/// Runs `change`. The statements are planned again here, so what runs is
/// what [`plan`] showed (unless the server changed in between, which the
/// table designer checks for).
pub async fn apply(session: &Session, change: &SchemaChange) -> Result<DdlRun> {
    let ctx = context(session).await?;
    let database = change.database().map(str::to_string);
    let mut note = String::new();
    let mut restore: Option<String> = None;

    match change {
        SchemaChange::AlterTable {
            database, original, ..
        } => {
            let now = table_design(session, database, &original.name).await?;
            if !same_structure(&now, original) {
                return Err(invalid(
                    "這個資料表在你開啟設計器之後被變更了。請重新載入，再重新修改。",
                ));
            }
            let create = session
                .show_create_table(Some(database), &original.name)
                .await
                .unwrap_or_default();
            note = format!(
                "結構變更：修改資料表 {}.{}\n\n-- 變更前的結構\n{create};",
                database, original.name
            );
        }
        SchemaChange::DropTable { database, table } => {
            let create = session
                .show_create_table(Some(database), table)
                .await
                .unwrap_or_default();
            note = format!("結構變更：刪除資料表 {database}.{table}\n\n-- 刪除前的結構\n{create};");
        }
        SchemaChange::DropDatabase { name } => {
            let tables = base_tables(session, name).await.unwrap_or_default();
            note = format!(
                "結構變更：刪除資料庫 {name}\n\n刪除前有 {} 個資料表：{}",
                tables.len(),
                tables.join(", ")
            );
        }
        SchemaChange::SaveObject {
            database,
            object_kind,
            original: Some(orig),
            definition: def,
        } => {
            let old = definition(session, database, *object_kind, orig).await?;
            note = format!(
                "結構變更：修改{} {database}.{orig}\n\n-- 原本的定義\n{};",
                object_kind.label(),
                old.definition
            );
            let parsed = parse_create(def)?;
            let same_name = names_equal(*object_kind, &parsed.name, orig);
            if same_name && !(*object_kind == ObjectKind::View || ctx.flavor.or_replace()) {
                restore = Some(old.definition);
            }
        }
        SchemaChange::SaveObject {
            database,
            object_kind,
            original: None,
            ..
        } => {
            note = format!("結構變更：新增{}（{database}）", object_kind.label());
        }
        SchemaChange::DropObject {
            database,
            object_kind,
            name,
        } => {
            let old = definition(session, database, *object_kind, name).await?;
            note = format!(
                "結構變更：刪除{} {database}.{name}\n\n-- 刪除前的定義\n{};",
                object_kind.label(),
                old.definition
            );
        }
        _ => {}
    }

    let planned = plan(session, change).await?;
    let statements = planned.statements;
    if statements.is_empty() {
        return Err(invalid("沒有需要變更的地方。"));
    }
    let _ = ctx;
    let mut outcome = session.run_ddl(database.as_deref(), &statements).await;
    let mut sql = script(&statements);

    // MySQL replaced the object with DROP then CREATE; if CREATE failed,
    // put the old one back.
    if let (Some(old), Some(_)) = (restore, &outcome.error)
        && outcome.results.len() == 1
    {
        let again = session
            .run_ddl(database.as_deref(), std::slice::from_ref(&old))
            .await;
        let restored = match &again.error {
            None => "已自動用原本的定義還原。".to_string(),
            Some(e) => format!("自動還原也失敗了：{e}。原本的定義在這筆紀錄的備註裡。"),
        };
        sql.push_str(&format!("\n\n-- 建立失敗，還原原本的定義\n{old};"));
        outcome.results.extend(again.results);
        if let Some(CoreError::Server { message, .. }) = outcome.error.as_mut() {
            message.push_str(&format!("\n（{restored}）"));
        }
        note.push_str(&format!("\n\n{restored}"));
    }

    if note.is_empty() {
        note = "結構變更".into();
    }
    Ok(DdlRun {
        outcome,
        sql,
        database,
        note,
    })
}

async fn base_tables(session: &Session, database: &str) -> Result<Vec<String>> {
    Ok(session
        .query_params(
            "SELECT TABLE_NAME FROM information_schema.TABLES
             WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME",
            &[database],
        )
        .await?
        .rows
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect())
}

/// The statements as one script, each ending in `;`.
fn script(statements: &[String]) -> String {
    statements
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn same_mode(a: &str, b: &str) -> bool {
    let set = |s: &str| {
        let mut v: Vec<String> = s
            .split(',')
            .map(|m| m.trim().to_ascii_uppercase())
            .filter(|m| !m.is_empty())
            .collect();
        v.sort();
        v
    };
    set(a) == set(b)
}

/// Equal apart from the next AUTO_INCREMENT value.
fn same_structure(a: &TableDesign, b: &TableDesign) -> bool {
    let strip = |t: &TableDesign| {
        let mut t = t.clone();
        t.options.auto_increment = None;
        t
    };
    strip(a) == strip(b)
}

/// `` CHARACTER SET x COLLATE y ``, each part only when given, led by `sep`.
fn charset_clause(
    charset: &Option<String>,
    collation: &Option<String>,
    sep: &str,
) -> Result<String> {
    let mut out = String::new();
    if let Some(cs) = charset.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("{sep}CHARACTER SET {}", word(cs, "字元集")?));
    }
    if let Some(co) = collation.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("{sep}COLLATE {}", word(co, "定序")?));
    }
    Ok(out)
}

pub fn create_table_sql(database: &str, design: &TableDesign, ctx: Ctx) -> Result<String> {
    validate_design(design)?;
    let mut lines = Vec::new();
    for c in &design.columns {
        lines.push(column_sql(c, ctx)?);
    }
    for ix in &design.indexes {
        lines.push(index_sql(ix)?);
    }
    for fk in &design.foreign_keys {
        lines.push(foreign_key_sql(database, fk)?);
    }
    let mut sql = format!(
        "CREATE TABLE {} (\n  {}\n)",
        qualified(database, &non_empty(&design.name, "資料表名稱")?),
        lines.join(",\n  ")
    );
    let o = &design.options;
    if let Some(engine) = o.engine.as_deref().filter(|e| !e.is_empty()) {
        sql.push_str(&format!(" ENGINE={}", word(engine, "儲存引擎")?));
    }
    if let Some(cs) = o.charset.as_deref().filter(|s| !s.is_empty()) {
        sql.push_str(&format!(" DEFAULT CHARSET={}", word(cs, "字元集")?));
    }
    if let Some(co) = o.collation.as_deref().filter(|s| !s.is_empty()) {
        sql.push_str(&format!(" COLLATE={}", word(co, "定序")?));
    }
    if let Some(n) = o.auto_increment.filter(|n| *n > 1) {
        sql.push_str(&format!(" AUTO_INCREMENT={n}"));
    }
    if !o.comment.is_empty() {
        sql.push_str(&format!(
            " COMMENT={}",
            string_literal(&o.comment, ctx.dialect)
        ));
    }
    Ok(sql)
}

/// The statements that turn `original` into `design`: usually a single
/// `ALTER TABLE`, preceded by one that drops foreign keys being changed
/// (MySQL cannot drop and re-add a foreign key of the same name at once).
pub fn alter_table_sql(
    database: &str,
    original: &TableDesign,
    design: &TableDesign,
    convert_charset: bool,
    ctx: Ctx,
) -> Result<Vec<String>> {
    validate_design(design)?;
    let target = qualified(database, &original.name);
    let orig_cols: HashMap<&str, &ColumnDesign> = original
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    let mut kept = HashSet::new();
    for c in &design.columns {
        if let Some(o) = &c.original {
            if !orig_cols.contains_key(o.as_str()) {
                return Err(invalid(&format!("找不到原本的欄位 {o}")));
            }
            if !kept.insert(o.as_str()) {
                return Err(invalid(&format!("欄位 {o} 出現了兩次")));
            }
        }
    }
    // Old column name → new name, for indexes and keys that follow a rename.
    let renamed: HashMap<&str, &str> = design
        .columns
        .iter()
        .filter_map(|c| Some((c.original.as_deref()?, c.name.as_str())))
        .collect();
    let follow = |name: &str| renamed.get(name).copied().unwrap_or(name).to_string();

    let mut pre = Vec::new();
    let mut drops = Vec::new();
    let mut changes = Vec::new();
    let mut adds = Vec::new();
    let mut options = Vec::new();

    // Foreign keys.
    for ofk in &original.foreign_keys {
        let moved = ForeignKeyDesign {
            columns: ofk.columns.iter().map(|c| follow(c)).collect(),
            ..ofk.clone()
        };
        match design
            .foreign_keys
            .iter()
            .find(|f| f.original.as_deref() == Some(&ofk.name))
        {
            Some(f) if same_fk(f, &moved) => {}
            other => {
                pre.push(format!("DROP FOREIGN KEY {}", ident(&ofk.name)));
                if let Some(f) = other {
                    adds.push(format!("ADD {}", foreign_key_sql(database, f)?));
                }
            }
        }
    }
    for f in design.foreign_keys.iter().filter(|f| f.original.is_none()) {
        adds.push(format!("ADD {}", foreign_key_sql(database, f)?));
    }

    // Indexes.
    for oix in &original.indexes {
        let moved = IndexDesign {
            parts: oix
                .parts
                .iter()
                .map(|p| IndexPart {
                    column: follow(&p.column),
                    ..p.clone()
                })
                .collect(),
            ..oix.clone()
        };
        let drop = || {
            if oix.kind == IndexKind::Primary {
                "DROP PRIMARY KEY".to_string()
            } else {
                format!("DROP INDEX {}", ident(&oix.name))
            }
        };
        match design
            .indexes
            .iter()
            .find(|i| i.original.as_deref() == Some(&oix.name))
        {
            None => drops.push(drop()),
            Some(ix) if same_index(ix, &moved) => {
                if ix.name != oix.name {
                    if ctx.flavor.renames() && ix.kind != IndexKind::Primary {
                        changes.push(format!(
                            "RENAME INDEX {} TO {}",
                            ident(&oix.name),
                            ident(&ix.name)
                        ));
                    } else {
                        drops.push(drop());
                        adds.push(format!("ADD {}", index_sql(ix)?));
                    }
                }
            }
            Some(ix) => {
                drops.push(drop());
                adds.push(format!("ADD {}", index_sql(ix)?));
            }
        }
    }
    for ix in design.indexes.iter().filter(|i| i.original.is_none()) {
        adds.push(format!("ADD {}", index_sql(ix)?));
    }

    // Columns: drops, then adds and changes in the new order. `order`
    // tracks the server's column order as the clauses apply, by new name.
    for oc in &original.columns {
        if !kept.contains(oc.name.as_str()) {
            drops.push(format!("DROP COLUMN {}", ident(&oc.name)));
        }
    }
    let mut order: Vec<String> = original
        .columns
        .iter()
        .filter(|c| kept.contains(c.name.as_str()))
        .map(|c| follow(&c.name))
        .collect();
    for (i, c) in design.columns.iter().enumerate() {
        let position = match i {
            0 => " FIRST".to_string(),
            _ => format!(" AFTER {}", ident(&design.columns[i - 1].name)),
        };
        match &c.original {
            None => {
                let at_end = i >= order.len();
                changes.push(format!(
                    "ADD COLUMN {}{}",
                    column_sql(c, ctx)?,
                    if at_end { "" } else { &position }
                ));
                order.insert(i.min(order.len()), c.name.clone());
            }
            Some(o) => {
                let oc = orig_cols[o.as_str()];
                let now = order.iter().position(|n| *n == c.name).unwrap_or(i);
                let moved = now != i;
                let same_def = same_column(oc, c);
                if moved || !same_def || oc.name != c.name {
                    let pos = if moved { position.as_str() } else { "" };
                    let clause = if same_def && !moved && ctx.flavor.renames() {
                        format!("RENAME COLUMN {} TO {}", ident(o), ident(&c.name))
                    } else if oc.name != c.name {
                        format!("CHANGE COLUMN {} {}{pos}", ident(o), column_sql(c, ctx)?)
                    } else {
                        format!("MODIFY COLUMN {}{pos}", column_sql(c, ctx)?)
                    };
                    changes.push(clause);
                    if moved {
                        let name = order.remove(now);
                        order.insert(i, name);
                    }
                }
            }
        }
    }

    // Table options.
    let (o, n) = (&original.options, &design.options);
    if design.name != original.name {
        options.push(format!(
            "RENAME TO {}",
            qualified(database, &non_empty(&design.name, "資料表名稱")?)
        ));
    }
    if let Some(engine) = n.engine.as_deref().filter(|e| !e.is_empty())
        && n.engine != o.engine
    {
        options.push(format!("ENGINE = {}", word(engine, "儲存引擎")?));
    }
    let charset_changed = n.charset != o.charset || n.collation != o.collation;
    if convert_charset {
        let cs = n.charset.clone().or_else(|| o.charset.clone());
        let clause = charset_clause(&cs, &n.collation, " ")?;
        if clause.is_empty() {
            return Err(invalid("請選擇要轉換成的字元集"));
        }
        options.push(format!("CONVERT TO{clause}"));
    } else if charset_changed {
        let clause = charset_clause(&n.charset, &n.collation, " ")?;
        if !clause.is_empty() {
            options.push(format!("DEFAULT{clause}"));
        }
    }
    if n.comment != o.comment {
        options.push(format!(
            "COMMENT = {}",
            string_literal(&n.comment, ctx.dialect)
        ));
    }
    if let Some(next) = n.auto_increment
        && n.auto_increment != o.auto_increment
    {
        options.push(format!("AUTO_INCREMENT = {next}"));
    }

    let mut out = Vec::new();
    if !pre.is_empty() {
        out.push(format!("ALTER TABLE {target}\n  {}", pre.join(",\n  ")));
    }
    let clauses: Vec<String> = drops
        .into_iter()
        .chain(changes)
        .chain(adds)
        .chain(options)
        .collect();
    if !clauses.is_empty() {
        out.push(format!("ALTER TABLE {target}\n  {}", clauses.join(",\n  ")));
    }
    Ok(out)
}

fn validate_design(design: &TableDesign) -> Result<()> {
    non_empty(&design.name, "資料表名稱")?;
    if design.columns.is_empty() {
        return Err(invalid("資料表至少要有一個欄位"));
    }
    let mut seen = HashSet::new();
    for c in &design.columns {
        let name = non_empty(&c.name, "欄位名稱")?;
        if !seen.insert(name.to_lowercase()) {
            return Err(invalid(&format!("欄位名稱重複：{name}")));
        }
    }
    let mut seen = HashSet::new();
    for ix in &design.indexes {
        if ix.parts.is_empty() {
            return Err(invalid(&format!("索引 {} 沒有欄位", ix.name)));
        }
        let name = if ix.kind == IndexKind::Primary {
            "PRIMARY".to_string()
        } else {
            non_empty(&ix.name, "索引名稱")?
        };
        if !seen.insert(name.to_lowercase()) {
            return Err(invalid(&format!("索引名稱重複：{name}")));
        }
        for p in &ix.parts {
            if p.expression.is_none() && !design.columns.iter().any(|c| c.name == p.column) {
                return Err(invalid(&format!(
                    "索引 {} 用到不存在的欄位 {}",
                    ix.name, p.column
                )));
            }
        }
    }
    for fk in &design.foreign_keys {
        if fk.columns.is_empty() || fk.columns.len() != fk.ref_columns.len() {
            return Err(invalid(&format!(
                "外鍵 {} 的欄位與參照欄位數量要一樣",
                fk.name
            )));
        }
    }
    Ok(())
}

/// Same definition, ignoring the name on the server.
fn same_column(a: &ColumnDesign, b: &ColumnDesign) -> bool {
    let strip = |c: &ColumnDesign| ColumnDesign {
        original: None,
        name: String::new(),
        ..c.clone()
    };
    strip(a) == strip(b)
}

/// Same definition, ignoring the name.
fn same_index(a: &IndexDesign, b: &IndexDesign) -> bool {
    a.kind == b.kind && a.parts == b.parts && a.using == b.using && a.comment == b.comment
}

fn same_fk(a: &ForeignKeyDesign, b: &ForeignKeyDesign) -> bool {
    let rule = |r: &str| match r.to_ascii_uppercase().as_str() {
        "" | "NO ACTION" => "RESTRICT".to_string(),
        other => other.to_string(),
    };
    a.name == b.name
        && a.columns == b.columns
        && a.ref_database == b.ref_database
        && a.ref_table == b.ref_table
        && a.ref_columns == b.ref_columns
        && rule(&a.on_delete) == rule(&b.on_delete)
        && rule(&a.on_update) == rule(&b.on_update)
}

fn is_text_type(column_type: &str) -> bool {
    let t = column_type.trim_start().to_ascii_lowercase();
    let base = t
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("");
    matches!(
        base,
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set"
    )
}

pub fn column_sql(c: &ColumnDesign, ctx: Ctx) -> Result<String> {
    let column_type = fragment(&c.column_type, "型別")?;
    let mut s = format!("{} {column_type}", ident(&non_empty(&c.name, "欄位名稱")?));
    let lower = column_type.to_ascii_lowercase();
    if is_text_type(&column_type) {
        if let Some(cs) = c.charset.as_deref().filter(|s| !s.is_empty())
            && !lower.contains("character set")
            && !lower.contains("charset")
        {
            s.push_str(&format!(" CHARACTER SET {}", word(cs, "字元集")?));
        }
        if let Some(co) = c.collation.as_deref().filter(|s| !s.is_empty())
            && !lower.contains("collate")
        {
            s.push_str(&format!(" COLLATE {}", word(co, "定序")?));
        }
    }
    if let Some(expr) = c.generated.as_deref().filter(|e| !e.trim().is_empty()) {
        s.push_str(&format!(
            " GENERATED ALWAYS AS ({}) {}",
            fragment(expr, "產生欄位的運算式")?,
            if c.stored { "STORED" } else { "VIRTUAL" }
        ));
        // MariaDB does not take NULL / NOT NULL on generated columns.
        if !c.nullable && !ctx.flavor.mariadb {
            s.push_str(" NOT NULL");
        }
    } else {
        s.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
        if let Some(d) = c.default.as_deref().filter(|d| !d.trim().is_empty()) {
            s.push_str(&format!(" DEFAULT {}", fragment(d, "預設值")?));
        }
        if let Some(u) = c.on_update.as_deref().filter(|u| !u.trim().is_empty()) {
            s.push_str(&format!(" ON UPDATE {}", fragment(u, "ON UPDATE")?));
        }
    }
    if c.invisible {
        s.push_str(" INVISIBLE");
    }
    if c.auto_increment {
        s.push_str(" AUTO_INCREMENT");
    }
    if !c.comment.is_empty() {
        s.push_str(&format!(
            " COMMENT {}",
            string_literal(&c.comment, ctx.dialect)
        ));
    }
    Ok(s)
}

pub fn index_sql(ix: &IndexDesign) -> Result<String> {
    let parts = ix
        .parts
        .iter()
        .map(|p| {
            let mut s = match &p.expression {
                Some(e) => format!("({})", fragment(e, "索引運算式")?),
                None => ident(&non_empty(&p.column, "索引欄位")?),
            };
            if let Some(n) = p.length.filter(|n| *n > 0) {
                s.push_str(&format!("({n})"));
            }
            if p.descending {
                s.push_str(" DESC");
            }
            Ok(s)
        })
        .collect::<Result<Vec<_>>>()?
        .join(", ");
    let name = || -> Result<String> { Ok(ident(&non_empty(&ix.name, "索引名稱")?)) };
    let mut s = match ix.kind {
        IndexKind::Primary => format!("PRIMARY KEY ({parts})"),
        IndexKind::Unique => format!("UNIQUE INDEX {} ({parts})", name()?),
        IndexKind::Index => format!("INDEX {} ({parts})", name()?),
        IndexKind::Fulltext => format!("FULLTEXT INDEX {} ({parts})", name()?),
        IndexKind::Spatial => format!("SPATIAL INDEX {} ({parts})", name()?),
    };
    if let Some(using) = ix.using.as_deref().filter(|u| !u.is_empty()) {
        s.push_str(&format!(" USING {}", word(using, "索引類型")?));
    }
    if !ix.comment.is_empty() {
        // Comments of indexes take no sql_mode care beyond quotes.
        s.push_str(&format!(
            " COMMENT '{}'",
            ix.comment.replace('\'', "''").replace('\\', "\\\\")
        ));
    }
    Ok(s)
}

pub fn foreign_key_sql(database: &str, fk: &ForeignKeyDesign) -> Result<String> {
    let cols = |names: &[String]| -> Result<String> {
        Ok(names
            .iter()
            .map(|n| Ok(ident(&non_empty(n, "外鍵欄位")?)))
            .collect::<Result<Vec<_>>>()?
            .join(", "))
    };
    let table = match fk
        .ref_database
        .as_deref()
        .filter(|d| !d.is_empty() && *d != database)
    {
        Some(db) => qualified(db, &fk.ref_table),
        None => ident(&non_empty(&fk.ref_table, "參照的資料表")?),
    };
    let mut s = String::new();
    if !fk.name.is_empty() {
        s.push_str(&format!("CONSTRAINT {} ", ident(&fk.name)));
    }
    s.push_str(&format!(
        "FOREIGN KEY ({}) REFERENCES {table} ({})",
        cols(&fk.columns)?,
        cols(&fk.ref_columns)?
    ));
    for (what, rule) in [("DELETE", &fk.on_delete), ("UPDATE", &fk.on_update)] {
        let rule = rule.trim().to_ascii_uppercase();
        match rule.as_str() {
            "" => {}
            "RESTRICT" | "CASCADE" | "SET NULL" | "NO ACTION" | "SET DEFAULT" => {
                s.push_str(&format!(" ON {what} {rule}"))
            }
            _ => return Err(invalid(&format!("無法辨識的外鍵規則：{rule}"))),
        }
    }
    Ok(s)
}

/// Statements and notes for [`SchemaChange::SaveObject`].
pub fn save_object_sql(
    database: &str,
    kind: ObjectKind,
    original: Option<&str>,
    definition: &str,
    ctx: Ctx,
) -> Result<DdlPlan> {
    let def = definition
        .trim()
        .trim_end_matches(';')
        .trim_end()
        .to_string();
    if def
        .lines()
        .any(|l| l.trim_start().to_ascii_uppercase().starts_with("DELIMITER"))
    {
        return Err(invalid(
            "不需要 DELIMITER：整段定義會當成一句送出，直接寫 CREATE … BEGIN … END 即可。",
        ));
    }
    let parsed = parse_create(&def)?;
    if parsed.kind != kind {
        return Err(invalid(&format!(
            "這段是 CREATE {}，不是 {}",
            parsed.kind.keyword(),
            kind.keyword()
        )));
    }
    if let Some(db) = &parsed.database
        && db != database
    {
        return Err(invalid(&format!(
            "名稱指定了資料庫 {db}，但這裡是 {database}。請拿掉資料庫名稱，或到 {db} 裡建立。"
        )));
    }
    let mut plan = DdlPlan::default();
    let drop = |name: &str| format!("DROP {} {}", kind.keyword(), qualified(database, name));
    match original {
        None => plan.statements.push(def),
        Some(orig) => {
            let same = names_equal(kind, &parsed.name, orig);
            if kind == ObjectKind::View || ctx.flavor.or_replace() {
                plan.statements.push(with_or_replace(&def, &parsed));
                if !same {
                    plan.statements.push(drop(orig));
                }
            } else if same {
                plan.statements.push(format!(
                    "DROP {} IF EXISTS {}",
                    kind.keyword(),
                    qualified(database, orig)
                ));
                plan.statements.push(def);
                let mut note = format!(
                    "MySQL 不支援 CREATE OR REPLACE {}，所以會先刪除再建立。如果建立失敗，會自動用原本的定義還原。",
                    kind.keyword()
                );
                if matches!(kind, ObjectKind::Procedure | ObjectKind::Function) {
                    note.push_str("刪除時，個別授予這個程序的 EXECUTE 權限也會一起移除。");
                }
                plan.notes.push(note);
            } else {
                plan.statements.push(def);
                plan.statements.push(drop(orig));
            }
            if !same {
                plan.notes.push(format!(
                    "名稱改了：會建立 {}，再刪除原本的 {orig}。",
                    parsed.name
                ));
            }
        }
    }
    Ok(plan)
}

fn names_equal(kind: ObjectKind, a: &str, b: &str) -> bool {
    match kind {
        // Routine and event names are not case sensitive.
        ObjectKind::Procedure | ObjectKind::Function | ObjectKind::Event => {
            a.eq_ignore_ascii_case(b)
        }
        ObjectKind::View | ObjectKind::Trigger => a == b,
    }
}

/// What [`parse_create`] reads from the head of a `CREATE` statement.
#[derive(Debug, PartialEq)]
pub struct CreateHead {
    pub kind: ObjectKind,
    pub database: Option<String>,
    pub name: String,
    /// Byte offset just after the `CREATE` keyword.
    create_end: usize,
    or_replace: bool,
}

/// Reads `CREATE [OR REPLACE] [ALGORITHM=…] [DEFINER=…] [SQL SECURITY …]
/// [AGGREGATE] {VIEW|PROCEDURE|FUNCTION|TRIGGER|EVENT} [IF NOT EXISTS] name`.
pub fn parse_create(sql: &str) -> Result<CreateHead> {
    let mut lex = Lexer { s: sql, at: 0 };
    let bad = || {
        invalid("看不懂這段定義：需要以 CREATE VIEW／PROCEDURE／FUNCTION／TRIGGER／EVENT 開頭。")
    };
    match lex.next() {
        Some(Tok::Word(w)) if w.eq_ignore_ascii_case("CREATE") => {}
        _ => return Err(bad()),
    }
    let create_end = lex.at;
    let mut or_replace = false;
    let is_kw = |w: &str| {
        ObjectKind::from_keyword(w).is_some()
            || ["SQL", "ALGORITHM", "AGGREGATE", "DEFINER"]
                .iter()
                .any(|k| k.eq_ignore_ascii_case(w))
    };
    loop {
        let Some(Tok::Word(w)) = lex.next() else {
            return Err(bad());
        };
        let upper = w.to_ascii_uppercase();
        if let Some(kind) = ObjectKind::from_keyword(&upper) {
            let save = lex.at;
            if matches!(lex.next(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case("IF")) {
                lex.next();
                lex.next();
            } else {
                lex.at = save;
            }
            let first = match lex.next() {
                Some(Tok::Ident(n)) => n,
                Some(Tok::Word(n)) => n.to_string(),
                _ => return Err(bad()),
            };
            let (database, name) = if lex.next() == Some(Tok::Punct('.')) {
                match lex.next() {
                    Some(Tok::Ident(n)) => (Some(first), n),
                    Some(Tok::Word(n)) => (Some(first), n.to_string()),
                    _ => return Err(bad()),
                }
            } else {
                (None, first)
            };
            return Ok(CreateHead {
                kind,
                database,
                name,
                create_end,
                or_replace,
            });
        }
        match upper.as_str() {
            "OR" => {
                lex.next();
                or_replace = true;
            }
            "ALGORITHM" => {
                lex.next();
                lex.next();
            }
            "SQL" => {
                lex.next();
                lex.next();
            }
            "AGGREGATE" => {}
            "DEFINER" => {
                if lex.next() != Some(Tok::Punct('=')) {
                    return Err(bad());
                }
                // The account: `u`@`h`, 'u'@'h', u@h or CURRENT_USER[()].
                loop {
                    let save = lex.at;
                    match lex.next() {
                        Some(Tok::Word(w)) if is_kw(w) => {
                            lex.at = save;
                            break;
                        }
                        None => return Err(bad()),
                        _ => {}
                    }
                }
            }
            _ => return Err(bad()),
        }
    }
}

/// `def` with `OR REPLACE` after `CREATE`, unless it has it already.
fn with_or_replace(def: &str, head: &CreateHead) -> String {
    if head.or_replace {
        return def.to_string();
    }
    format!(
        "{} OR REPLACE{}",
        &def[..head.create_end],
        &def[head.create_end..]
    )
}

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    /// A quoted identifier, unquoted.
    Ident(String),
    Str,
    Punct(char),
}

struct Lexer<'a> {
    s: &'a str,
    at: usize,
}

impl<'a> Lexer<'a> {
    fn skip_space(&mut self) {
        loop {
            let rest = &self.s[self.at..];
            let trimmed = rest.trim_start();
            self.at += rest.len() - trimmed.len();
            if trimmed.starts_with("--") || trimmed.starts_with('#') {
                self.at += trimmed.find('\n').unwrap_or(trimmed.len());
            } else if let Some(after) = trimmed.strip_prefix("/*") {
                self.at += 2 + after.find("*/").map(|i| i + 2).unwrap_or(after.len());
            } else {
                return;
            }
        }
    }

    fn next(&mut self) -> Option<Tok<'a>> {
        self.skip_space();
        let rest = &self.s[self.at..];
        let c = rest.chars().next()?;
        if c.is_alphanumeric() || c == '_' || c == '$' {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(rest.len());
            self.at += end;
            return Some(Tok::Word(&rest[..end]));
        }
        if c == '`' || c == '"' || c == '\'' {
            let mut out = String::new();
            let mut chars = rest.char_indices().skip(1).peekable();
            while let Some((i, ch)) = chars.next() {
                if ch == c {
                    if chars.peek().is_some_and(|&(_, n)| n == c) {
                        chars.next();
                        out.push(c);
                        continue;
                    }
                    self.at += i + 1;
                    return Some(if c == '\'' { Tok::Str } else { Tok::Ident(out) });
                }
                if ch == '\\' && c == '\'' {
                    chars.next();
                    continue;
                }
                out.push(ch);
            }
            self.at = self.s.len();
            return None;
        }
        self.at += c.len_utf8();
        Some(Tok::Punct(c))
    }
}

// ---------------------------------------------------------------------------
// Helpers

/// `` `db`.`name` ``.
pub fn qualified(database: &str, name: &str) -> String {
    format!("{}.{}", ident(database), ident(name))
}

fn invalid(message: &str) -> CoreError {
    CoreError::Config {
        message: message.to_string(),
    }
}

fn non_empty(s: &str, what: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        Err(invalid(&format!("請輸入{what}")))
    } else {
        Ok(s.to_string())
    }
}

/// A charset, collation, engine or similar bare word.
fn word(s: &str, what: &str) -> Result<String> {
    let t = s.trim();
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(t.to_string())
    } else {
        Err(invalid(&format!("{what}「{s}」不正確")))
    }
}

/// A piece of SQL that goes into a statement as written (a type, a
/// default, an expression). Refused if it could end the statement or
/// comment out the rest: a `;`, a comment, unbalanced parentheses or an
/// unclosed quote.
fn fragment(s: &str, what: &str) -> Result<String> {
    let t = s.trim();
    let bad = || invalid(&format!("{what}「{t}」不正確"));
    if t.is_empty() {
        return Err(invalid(&format!("請輸入{what}")));
    }
    let mut quote: Option<char> = None;
    let mut depth = 0i32;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' && q != '`' {
                    chars.next();
                } else if c == q {
                    if chars.peek() == Some(&q) {
                        chars.next();
                    } else {
                        quote = None;
                    }
                }
            }
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth < 0 {
                        return Err(bad());
                    }
                }
                ';' | '#' => return Err(bad()),
                '-' if chars.peek() == Some(&'-') => return Err(bad()),
                '/' if chars.peek() == Some(&'*') => return Err(bad()),
                _ => {}
            },
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(bad());
    }
    Ok(t.to_string())
}

/// Text rows read by column name, ignoring case.
struct Rows(TextRows);

impl Rows {
    fn len(&self) -> usize {
        self.0.rows.len()
    }

    fn get(&self, row: usize, column: &str) -> Option<String> {
        let i = self
            .0
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(column))?;
        self.0.rows.get(row)?.get(i).cloned().flatten()
    }

    fn text(&self, row: usize, column: &str) -> String {
        self.get(row, column).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MYSQL: Ctx = Ctx {
        dialect: Dialect {
            backslash_escapes: true,
        },
        flavor: Flavor {
            mariadb: false,
            version: (8, 0, 36),
        },
    };
    const MARIA: Ctx = Ctx {
        dialect: Dialect {
            backslash_escapes: true,
        },
        flavor: Flavor {
            mariadb: true,
            version: (10, 11, 6),
        },
    };

    fn col(name: &str, ty: &str) -> ColumnDesign {
        ColumnDesign {
            original: Some(name.into()),
            name: name.into(),
            column_type: ty.into(),
            ..Default::default()
        }
    }

    fn index(name: &str, kind: IndexKind, cols: &[&str]) -> IndexDesign {
        IndexDesign {
            original: Some(name.into()),
            name: name.into(),
            kind,
            parts: cols
                .iter()
                .map(|c| IndexPart {
                    column: c.to_string(),
                    ..Default::default()
                })
                .collect(),
            using: None,
            comment: String::new(),
        }
    }

    fn users() -> TableDesign {
        TableDesign {
            name: "users".into(),
            columns: vec![
                ColumnDesign {
                    auto_increment: true,
                    ..col("id", "int unsigned")
                },
                ColumnDesign {
                    charset: Some("utf8mb4".into()),
                    collation: Some("utf8mb4_0900_ai_ci".into()),
                    ..col("name", "varchar(64)")
                },
                ColumnDesign {
                    nullable: true,
                    ..col("email", "varchar(255)")
                },
            ],
            indexes: vec![
                index("PRIMARY", IndexKind::Primary, &["id"]),
                index("idx_name", IndexKind::Index, &["name"]),
            ],
            foreign_keys: vec![],
            options: TableOptions {
                engine: Some("InnoDB".into()),
                charset: Some("utf8mb4".into()),
                collation: Some("utf8mb4_0900_ai_ci".into()),
                comment: String::new(),
                auto_increment: Some(42),
            },
        }
    }

    #[test]
    fn parses_versions() {
        assert_eq!(
            Flavor::parse("10.11.6-MariaDB-1:10.11.6+maria~ubu2204"),
            Flavor {
                mariadb: true,
                version: (10, 11, 6)
            }
        );
        assert_eq!(Flavor::parse("8.0.36").version, (8, 0, 36));
        assert!(!Flavor::parse("5.7.44-log").renames());
    }

    #[test]
    fn unchanged_table_needs_nothing() {
        let t = users();
        assert!(
            alter_table_sql("shop", &t, &t, false, MYSQL)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn builds_one_alter_for_many_changes() {
        let orig = users();
        let mut d = orig.clone();
        // Rename name → full_name and make it longer; drop email; add two.
        d.columns[1].name = "full_name".into();
        d.columns[1].column_type = "varchar(128)".into();
        d.columns.remove(2);
        d.columns.insert(
            1,
            ColumnDesign {
                original: None,
                default: Some("CURRENT_TIMESTAMP".into()),
                ..col("created_at", "datetime")
            },
        );
        d.columns.push(ColumnDesign {
            original: None,
            nullable: true,
            comment: "it's".into(),
            ..col("note", "text")
        });
        // The index follows the rename by itself; add a unique one.
        d.indexes[1].parts[0].column = "full_name".into();
        d.indexes.push(IndexDesign {
            original: None,
            ..index("uq_created", IndexKind::Unique, &["created_at"])
        });
        d.options.comment = "people".into();
        let sql = alter_table_sql("shop", &orig, &d, false, MYSQL).unwrap();
        assert_eq!(
            sql,
            ["ALTER TABLE `shop`.`users`\n  \
              DROP COLUMN `email`,\n  \
              ADD COLUMN `created_at` datetime NOT NULL DEFAULT CURRENT_TIMESTAMP AFTER `id`,\n  \
              CHANGE COLUMN `name` `full_name` varchar(128) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci NOT NULL,\n  \
              ADD COLUMN `note` text NULL COMMENT 'it''s',\n  \
              ADD UNIQUE INDEX `uq_created` (`created_at`),\n  \
              COMMENT = 'people'"]
        );
    }

    #[test]
    fn moves_and_renames() {
        let orig = users();
        let mut d = orig.clone();
        d.columns.swap(1, 2);
        let sql = alter_table_sql("shop", &orig, &d, false, MYSQL).unwrap();
        assert_eq!(
            sql,
            ["ALTER TABLE `shop`.`users`\n  MODIFY COLUMN `email` varchar(255) NULL AFTER `id`"]
        );

        let mut d = orig.clone();
        d.columns[2].name = "mail".into();
        d.indexes[1].name = "by_name".into();
        assert_eq!(
            alter_table_sql("shop", &orig, &d, false, MYSQL).unwrap(),
            ["ALTER TABLE `shop`.`users`\n  \
              RENAME INDEX `idx_name` TO `by_name`,\n  \
              RENAME COLUMN `email` TO `mail`"]
        );
        let old = Ctx {
            flavor: Flavor {
                mariadb: true,
                version: (10, 3, 0),
            },
            ..MARIA
        };
        assert_eq!(
            alter_table_sql("shop", &orig, &d, false, old).unwrap(),
            ["ALTER TABLE `shop`.`users`\n  \
              DROP INDEX `idx_name`,\n  \
              CHANGE COLUMN `email` `mail` varchar(255) NULL,\n  \
              ADD INDEX `by_name` (`name`)"]
        );
    }

    #[test]
    fn changes_keys_and_options() {
        let mut orig = users();
        orig.foreign_keys.push(ForeignKeyDesign {
            original: Some("fk_team".into()),
            name: "fk_team".into(),
            columns: vec!["id".into()],
            ref_database: None,
            ref_table: "teams".into(),
            ref_columns: vec!["id".into()],
            on_delete: "RESTRICT".into(),
            on_update: "RESTRICT".into(),
        });
        let mut d = orig.clone();
        d.foreign_keys[0].on_delete = "CASCADE".into();
        d.indexes[0].parts.push(IndexPart {
            column: "name".into(),
            length: Some(10),
            ..Default::default()
        });
        d.name = "people".into();
        d.options.collation = Some("utf8mb4_bin".into());
        d.options.auto_increment = Some(1000);
        let sql = alter_table_sql("shop", &orig, &d, true, MYSQL).unwrap();
        assert_eq!(
            sql,
            [
                "ALTER TABLE `shop`.`users`\n  DROP FOREIGN KEY `fk_team`",
                "ALTER TABLE `shop`.`users`\n  \
                 DROP PRIMARY KEY,\n  \
                 ADD CONSTRAINT `fk_team` FOREIGN KEY (`id`) REFERENCES `teams` (`id`) ON DELETE CASCADE ON UPDATE RESTRICT,\n  \
                 ADD PRIMARY KEY (`id`, `name`(10)),\n  \
                 RENAME TO `shop`.`people`,\n  \
                 CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_bin,\n  \
                 AUTO_INCREMENT = 1000"
            ]
        );
    }

    #[test]
    fn creates_tables() {
        let mut t = users();
        t.options.auto_increment = None;
        t.columns[2].default = Some("NULL".into());
        assert_eq!(
            create_table_sql("shop", &t, MYSQL).unwrap(),
            "CREATE TABLE `shop`.`users` (\n  \
             `id` int unsigned NOT NULL AUTO_INCREMENT,\n  \
             `name` varchar(64) CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci NOT NULL,\n  \
             `email` varchar(255) NULL DEFAULT NULL,\n  \
             PRIMARY KEY (`id`),\n  \
             INDEX `idx_name` (`name`)\n\
             ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci"
        );
    }

    #[test]
    fn refuses_bad_designs_and_fragments() {
        let mut t = users();
        t.columns[0].column_type = "int; DROP TABLE x".into();
        assert!(create_table_sql("shop", &t, MYSQL).is_err());
        let mut t = users();
        t.columns[1].default = Some("'a' -- b".into());
        assert!(create_table_sql("shop", &t, MYSQL).is_err());
        let mut t = users();
        t.columns[1].name = "ID".into();
        assert!(create_table_sql("shop", &t, MYSQL).is_err());
        let mut t = users();
        t.indexes[1].parts[0].column = "missing".into();
        assert!(create_table_sql("shop", &t, MYSQL).is_err());
        assert_eq!(fragment("enum('a;b','c')", "x").unwrap(), "enum('a;b','c')");
        assert_eq!(fragment("'it''s'", "x").unwrap(), "'it''s'");
        assert!(fragment("(a", "x").is_err());
        assert!(fragment("'abc", "x").is_err());
    }

    #[test]
    fn generated_columns_follow_the_server() {
        let c = ColumnDesign {
            original: None,
            generated: Some("`a` + 1".into()),
            stored: true,
            ..col("b", "int")
        };
        assert_eq!(
            column_sql(&c, MYSQL).unwrap(),
            "`b` int GENERATED ALWAYS AS (`a` + 1) STORED NOT NULL"
        );
        assert_eq!(
            column_sql(&c, MARIA).unwrap(),
            "`b` int GENERATED ALWAYS AS (`a` + 1) STORED"
        );
    }

    #[test]
    fn reads_defaults() {
        let d = MYSQL.dialect;
        let my = MYSQL.flavor;
        let ma = MARIA.flavor;
        assert_eq!(default_sql(None, "", "int", my, d), None);
        assert_eq!(
            default_sql(Some("0"), "", "int", my, d).as_deref(),
            Some("0")
        );
        assert_eq!(
            default_sql(Some("it's"), "", "varchar", my, d).as_deref(),
            Some("'it''s'")
        );
        assert_eq!(
            default_sql(
                Some("CURRENT_TIMESTAMP"),
                "DEFAULT_GENERATED",
                "datetime",
                my,
                d
            )
            .as_deref(),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(
            default_sql(Some("uuid()"), "DEFAULT_GENERATED", "char", my, d).as_deref(),
            Some("(uuid())")
        );
        assert_eq!(
            default_sql(Some("b'1'"), "", "bit", my, d).as_deref(),
            Some("b'1'")
        );
        assert_eq!(default_sql(Some("NULL"), "", "int", ma, d), None);
        assert_eq!(
            default_sql(Some("'abc'"), "", "varchar", ma, d).as_deref(),
            Some("'abc'")
        );
        assert_eq!(
            default_sql(Some("current_timestamp()"), "", "datetime", ma, d).as_deref(),
            Some("current_timestamp()")
        );
        assert_eq!(
            on_update("DEFAULT_GENERATED on update CURRENT_TIMESTAMP(3)").as_deref(),
            Some("CURRENT_TIMESTAMP(3)")
        );
        assert_eq!(on_update("auto_increment"), None);
        assert_eq!(charset_of("utf8mb4_0900_ai_ci"), "utf8mb4");
        assert_eq!(charset_of("binary"), "binary");
    }

    #[test]
    fn parses_create_heads() {
        let h = parse_create(
            "CREATE DEFINER=`root`@`%` PROCEDURE `tidy`(IN n INT)\nBEGIN\n  SELECT n;\nEND",
        )
        .unwrap();
        assert_eq!(
            (h.kind, h.database, h.name.as_str()),
            (ObjectKind::Procedure, None, "tidy")
        );
        let h = parse_create(
            "-- note\nCREATE OR REPLACE ALGORITHM=UNDEFINED DEFINER='a'@'localhost' SQL SECURITY DEFINER VIEW shop.v AS SELECT 1",
        )
        .unwrap();
        assert_eq!(h.kind, ObjectKind::View);
        assert_eq!(h.database.as_deref(), Some("shop"));
        assert_eq!(h.name, "v");
        assert!(h.or_replace);
        let h = parse_create("create definer = current_user() trigger `t``x` before insert on t for each row set new.a = 1").unwrap();
        assert_eq!((h.kind, h.name.as_str()), (ObjectKind::Trigger, "t`x"));
        let h = parse_create("CREATE EVENT IF NOT EXISTS e ON SCHEDULE EVERY 1 DAY DO SELECT 1")
            .unwrap();
        assert_eq!((h.kind, h.name.as_str()), (ObjectKind::Event, "e"));
        let h =
            parse_create("CREATE AGGREGATE FUNCTION f() RETURNS INT BEGIN RETURN 1; END").unwrap();
        assert_eq!(h.kind, ObjectKind::Function);
        assert!(parse_create("SELECT 1").is_err());
        assert!(parse_create("CREATE TABLE t (a int)").is_err());
    }

    #[test]
    fn replaces_objects_per_server() {
        let def = "CREATE DEFINER=`root`@`%` PROCEDURE `tidy`()\nBEGIN\n  DELETE FROM t;\nEND;";
        let my = save_object_sql("shop", ObjectKind::Procedure, Some("tidy"), def, MYSQL).unwrap();
        assert_eq!(
            my.statements,
            [
                "DROP PROCEDURE IF EXISTS `shop`.`tidy`",
                "CREATE DEFINER=`root`@`%` PROCEDURE `tidy`()\nBEGIN\n  DELETE FROM t;\nEND"
            ]
        );
        assert!(!my.notes.is_empty());
        let ma = save_object_sql("shop", ObjectKind::Procedure, Some("TIDY"), def, MARIA).unwrap();
        assert_eq!(
            ma.statements,
            [
                "CREATE OR REPLACE DEFINER=`root`@`%` PROCEDURE `tidy`()\nBEGIN\n  DELETE FROM t;\nEND"
            ]
        );
        let renamed = save_object_sql(
            "shop",
            ObjectKind::View,
            Some("old_v"),
            "CREATE VIEW new_v AS SELECT 1",
            MYSQL,
        )
        .unwrap();
        assert_eq!(
            renamed.statements,
            [
                "CREATE OR REPLACE VIEW new_v AS SELECT 1",
                "DROP VIEW `shop`.`old_v`"
            ]
        );
        assert!(
            save_object_sql(
                "shop",
                ObjectKind::View,
                None,
                "CREATE VIEW other.v AS SELECT 1",
                MYSQL
            )
            .is_err()
        );
        assert!(
            save_object_sql(
                "shop",
                ObjectKind::Function,
                None,
                "CREATE PROCEDURE p() SELECT 1",
                MYSQL
            )
            .is_err()
        );
        assert!(
            save_object_sql(
                "shop",
                ObjectKind::Procedure,
                None,
                "DELIMITER $$\nCREATE PROCEDURE p() SELECT 1$$",
                MYSQL
            )
            .is_err()
        );
    }

    #[test]
    fn changes_come_from_json() {
        let change: SchemaChange = serde_json::from_str(
            r#"{"kind":"save_object","database":"shop","object_kind":"view","original":null,"definition":"CREATE VIEW v AS SELECT 1"}"#,
        )
        .unwrap();
        assert!(matches!(
            change,
            SchemaChange::SaveObject {
                object_kind: ObjectKind::View,
                ..
            }
        ));
    }
}
