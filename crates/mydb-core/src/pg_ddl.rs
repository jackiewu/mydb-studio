//! Schema management on PostgreSQL: the counterpart of [`crate::ddl`],
//! with the same types so the schema page and table designer work on both.
//!
//! What the UI calls a database is a schema here. Tables, views,
//! functions, procedures and triggers are read from the catalog, and every
//! change runs as one transaction ([`PgSession::run_in_transaction`]):
//! PostgreSQL can undo schema changes, so a failed step leaves nothing
//! half done.

use crate::ddl::{
    ColumnDesign, DatabaseOptions, DdlPlan, DdlRun, Flavor, ForeignKeyDesign, IndexDesign,
    IndexKind, IndexPart, ObjectDefinition, ObjectKind, ObjectList, SchemaChange, SchemaMeta,
    SchemaObject, TableDesign, TableOptions, invalid,
};
use crate::error::Result;
use crate::pg::{PgSession, quote_ident, quote_literal};

fn q(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

pub async fn meta(s: &PgSession) -> Result<SchemaMeta> {
    let rows = s
        .query("SELECT current_setting('server_version_num')::int", &[])
        .await?;
    let num: i32 = rows.first().map(|r| r.get(0)).unwrap_or(0);
    let encoding = s
        .query("SELECT current_setting('server_encoding')", &[])
        .await?
        .first()
        .map(|r| r.get::<_, String>(0))
        .unwrap_or_default();
    Ok(SchemaMeta {
        flavor: Flavor {
            mariadb: false,
            version: (
                (num / 10000) as u32,
                (num % 10000 / 100) as u32,
                (num % 100) as u32,
            ),
            postgres: true,
        },
        charsets: Vec::new(),
        engines: Vec::new(),
        server_charset: encoding,
        server_collation: String::new(),
    })
}

/// A schema has no character set; its owner is shown instead.
pub async fn database_options(s: &PgSession, schema: &str) -> Result<DatabaseOptions> {
    let rows = s
        .query(
            "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname = $1",
            &[&schema],
        )
        .await?;
    Ok(DatabaseOptions {
        name: schema.to_string(),
        charset: rows.first().map(|r| r.get(0)).unwrap_or_default(),
        collation: String::new(),
    })
}

pub async fn objects(s: &PgSession, schema: &str) -> Result<ObjectList> {
    let mut objects = Vec::new();
    for r in s
        .query(
            "SELECT c.relname::text, c.relkind = 'm', pg_get_userbyid(c.relowner)::text,
                    COALESCE(obj_description(c.oid, 'pg_class'), '')
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relkind IN ('v', 'm') ORDER BY 1",
            &[&schema],
        )
        .await?
    {
        objects.push(SchemaObject {
            kind: ObjectKind::View,
            name: r.get(0),
            detail: if r.get(1) {
                "MATERIALIZED".into()
            } else {
                String::new()
            },
            definer: r.get(2),
            created: None,
            modified: None,
            comment: r.get(3),
            enabled: None,
        });
    }
    for r in s
        .query(
            "SELECT p.prokind = 'p',
                    p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')',
                    CASE WHEN p.prokind = 'p' THEN '' ELSE 'RETURNS ' || pg_get_function_result(p.oid) END,
                    pg_get_userbyid(p.proowner)::text, l.lanname::text,
                    COALESCE(obj_description(p.oid, 'pg_proc'), '')
             FROM pg_proc p
             JOIN pg_namespace n ON n.oid = p.pronamespace
             JOIN pg_language l ON l.oid = p.prolang
             WHERE n.nspname = $1 AND p.prokind IN ('f', 'p')
               AND NOT EXISTS (SELECT 1 FROM pg_depend d
                               WHERE d.objid = p.oid AND d.deptype = 'e')
             ORDER BY 2",
            &[&schema],
        )
        .await?
    {
        let lang: String = r.get(4);
        let result: String = r.get(2);
        objects.push(SchemaObject {
            kind: if r.get(0) {
                ObjectKind::Procedure
            } else {
                ObjectKind::Function
            },
            name: r.get(1),
            detail: [result, lang].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(" · "),
            definer: r.get(3),
            created: None,
            modified: None,
            comment: r.get(5),
            enabled: None,
        });
    }
    for r in s
        .query(
            "SELECT t.tgname::text, c.relname::text, t.tgenabled <> 'D',
                    COALESCE(obj_description(t.oid, 'pg_trigger'), '')
             FROM pg_trigger t
             JOIN pg_class c ON c.oid = t.tgrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND NOT t.tgisinternal ORDER BY 1",
            &[&schema],
        )
        .await?
    {
        let table: String = r.get(1);
        objects.push(SchemaObject {
            kind: ObjectKind::Trigger,
            name: r.get(0),
            detail: format!("ON {table}"),
            definer: String::new(),
            created: None,
            modified: None,
            comment: r.get(3),
            enabled: Some(r.get(2)),
        });
    }
    Ok(ObjectList {
        objects,
        event_scheduler: None,
    })
}

/// The oid of the function or procedure listed as `signature`
/// (`name(arg types)`).
async fn routine_oid(s: &PgSession, schema: &str, signature: &str) -> Result<u32> {
    s.query(
        "SELECT p.oid FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
         WHERE n.nspname = $1
           AND p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = $2",
        &[&schema, &signature],
    )
    .await?
    .first()
    .map(|r| r.get(0))
    .ok_or_else(|| invalid(&format!("找不到 {schema}.{signature}")))
}

/// The table a trigger is on.
async fn trigger_table(s: &PgSession, schema: &str, name: &str) -> Result<(u32, String)> {
    s.query(
        "SELECT t.oid, c.relname::text FROM pg_trigger t
         JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = $1 AND t.tgname = $2 AND NOT t.tgisinternal LIMIT 1",
        &[&schema, &name],
    )
    .await?
    .first()
    .map(|r| (r.get(0), r.get(1)))
    .ok_or_else(|| invalid(&format!("找不到觸發器 {schema}.{name}")))
}

async fn is_materialized(s: &PgSession, schema: &str, name: &str) -> Result<bool> {
    Ok(s.query(
        "SELECT c.relkind = 'm' FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = $1 AND c.relname = $2",
        &[&schema, &name],
    )
    .await?
    .first()
    .is_some_and(|r| r.get(0)))
}

pub async fn definition(
    s: &PgSession,
    schema: &str,
    kind: ObjectKind,
    name: &str,
) -> Result<ObjectDefinition> {
    let definition = match kind {
        ObjectKind::View => {
            let mat = is_materialized(s, schema, name).await?;
            let body: String = s
                .query(
                    "SELECT pg_get_viewdef(c.oid, true) FROM pg_class c
                     JOIN pg_namespace n ON n.oid = c.relnamespace
                     WHERE n.nspname = $1 AND c.relname = $2",
                    &[&schema, &name],
                )
                .await?
                .first()
                .map(|r| r.get(0))
                .ok_or_else(|| invalid(&format!("找不到檢視 {schema}.{name}")))?;
            let head = if mat {
                "CREATE MATERIALIZED VIEW"
            } else {
                "CREATE OR REPLACE VIEW"
            };
            format!(
                "{head} {} AS\n{}",
                q(schema, name),
                body.trim_end().trim_end_matches(';')
            )
        }
        ObjectKind::Function | ObjectKind::Procedure => {
            let oid = routine_oid(s, schema, name).await?;
            let def: String = s
                .query("SELECT pg_get_functiondef($1::oid)", &[&oid])
                .await?
                .first()
                .map(|r| r.get(0))
                .unwrap_or_default();
            def.trim_end().to_string()
        }
        ObjectKind::Trigger => {
            let (oid, _) = trigger_table(s, schema, name).await?;
            s.query("SELECT pg_get_triggerdef($1::oid, true)", &[&oid])
                .await?
                .first()
                .map(|r| r.get(0))
                .unwrap_or_default()
        }
        ObjectKind::Event => {
            return Err(invalid(
                "PostgreSQL 沒有事件（EVENT）。可以用 pg_cron 之類的擴充。",
            ));
        }
    };
    Ok(ObjectDefinition {
        definition,
        sql_mode: None,
    })
}

async fn table_oid(s: &PgSession, schema: &str, table: &str) -> Result<u32> {
    s.query(
        "SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p')",
        &[&schema, &table],
    )
    .await?
    .first()
    .map(|r| r.get(0))
    .ok_or_else(|| invalid(&format!("找不到資料表 {schema}.{table}")))
}

pub async fn table_design(s: &PgSession, schema: &str, table: &str) -> Result<TableDesign> {
    let oid = table_oid(s, schema, table).await?;
    let mut columns = Vec::new();
    for r in s
        .query(
            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull,
                    pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text,
                    COALESCE(col_description(a.attrelid, a.attnum), '')
             FROM pg_attribute a
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE a.attrelid = $1::oid AND a.attnum > 0 AND NOT a.attisdropped
             ORDER BY a.attnum",
            &[&oid],
        )
        .await?
    {
        let name: String = r.get(0);
        let expr: Option<String> = r.get(3);
        let identity: String = r.get(4);
        let generated = r.get::<_, String>(5) == "s";
        columns.push(ColumnDesign {
            original: Some(name.clone()),
            name,
            column_type: r.get(1),
            nullable: r.get(2),
            default: if generated { None } else { expr.clone() },
            auto_increment: !identity.is_empty(),
            generated: if generated { expr } else { None },
            stored: generated,
            comment: r.get(6),
            ..Default::default()
        });
    }

    let mut indexes = Vec::new();
    for r in s
        .query(
            "SELECT i.relname::text, ix.indisprimary, ix.indisunique, am.amname::text,
                    ARRAY(SELECT CASE WHEN ix.indkey[k] = 0 THEN NULL ELSE
                                   (SELECT attname::text FROM pg_attribute
                                    WHERE attrelid = ix.indrelid AND attnum = ix.indkey[k]) END
                          FROM generate_series(0, ix.indnkeyatts - 1) k ORDER BY k),
                    ARRAY(SELECT pg_get_indexdef(ix.indexrelid, k + 1, true)
                          FROM generate_series(0, ix.indnkeyatts - 1) k ORDER BY k),
                    ARRAY(SELECT (ix.indoption[k] & 1) = 1
                          FROM generate_series(0, ix.indnkeyatts - 1) k ORDER BY k),
                    COALESCE(obj_description(i.oid, 'pg_class'), '')
             FROM pg_index ix
             JOIN pg_class i ON i.oid = ix.indexrelid
             JOIN pg_am am ON am.oid = i.relam
             WHERE ix.indrelid = $1::oid
             ORDER BY ix.indisprimary DESC, i.relname",
            &[&oid],
        )
        .await?
    {
        let real: String = r.get(0);
        let primary: bool = r.get(1);
        let unique: bool = r.get(2);
        let method: String = r.get(3);
        let cols: Vec<Option<String>> = r.get(4);
        let defs: Vec<String> = r.get(5);
        let desc: Vec<bool> = r.get(6);
        let parts = cols
            .into_iter()
            .zip(defs)
            .zip(desc)
            .map(|((col, def), descending)| match col {
                Some(column) => IndexPart {
                    column,
                    length: None,
                    descending,
                    expression: None,
                },
                None => IndexPart {
                    column: String::new(),
                    length: None,
                    descending,
                    expression: Some(def),
                },
            })
            .collect();
        indexes.push(IndexDesign {
            original: Some(real.clone()),
            name: if primary { "PRIMARY".into() } else { real },
            kind: if primary {
                IndexKind::Primary
            } else if unique {
                IndexKind::Unique
            } else {
                IndexKind::Index
            },
            parts,
            using: (method != "btree").then_some(method),
            comment: r.get(7),
        });
    }

    let mut foreign_keys = Vec::new();
    for r in s
        .query(
            "SELECT con.conname::text,
                    ARRAY(SELECT attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, o)
                          JOIN pg_attribute ON attrelid = con.conrelid AND attnum = k.n ORDER BY k.o),
                    rn.nspname::text, rc.relname::text,
                    ARRAY(SELECT attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, o)
                          JOIN pg_attribute ON attrelid = con.confrelid AND attnum = k.n ORDER BY k.o),
                    con.confdeltype::text, con.confupdtype::text
             FROM pg_constraint con
             JOIN pg_class rc ON rc.oid = con.confrelid
             JOIN pg_namespace rn ON rn.oid = rc.relnamespace
             WHERE con.conrelid = $1::oid AND con.contype = 'f'
             ORDER BY 1",
            &[&oid],
        )
        .await?
    {
        let ref_schema: String = r.get(2);
        foreign_keys.push(ForeignKeyDesign {
            original: Some(r.get(0)),
            name: r.get(0),
            columns: r.get(1),
            ref_database: (ref_schema != schema).then_some(ref_schema),
            ref_table: r.get(3),
            ref_columns: r.get(4),
            on_delete: fk_rule(&r.get::<_, String>(5)).into(),
            on_update: fk_rule(&r.get::<_, String>(6)).into(),
        });
    }

    let comment = s
        .query(
            "SELECT COALESCE(obj_description($1::oid, 'pg_class'), '')",
            &[&oid],
        )
        .await?
        .first()
        .map(|r| r.get(0))
        .unwrap_or_default();
    Ok(TableDesign {
        name: table.to_string(),
        columns,
        indexes,
        foreign_keys,
        options: TableOptions {
            comment,
            ..Default::default()
        },
    })
}

fn fk_rule(code: &str) -> &'static str {
    match code {
        "r" => "RESTRICT",
        "c" => "CASCADE",
        "n" => "SET NULL",
        "d" => "SET DEFAULT",
        _ => "NO ACTION",
    }
}

// ---------------------------------------------------------------------------
// Planning

fn name(n: &str, what: &str) -> Result<String> {
    let n = n.trim();
    if n.is_empty() {
        return Err(invalid(&format!("請輸入{what}")));
    }
    Ok(n.to_string())
}

fn column_sql(c: &ColumnDesign) -> Result<String> {
    let mut sql = format!(
        "{} {}",
        quote_ident(&name(&c.name, "欄位名稱")?),
        name(&c.column_type, &format!("欄位 {} 的型別", c.name))?
    );
    if let Some(expr) = c.generated.as_deref().filter(|e| !e.trim().is_empty()) {
        sql.push_str(&format!(" GENERATED ALWAYS AS ({expr}) STORED"));
    } else if c.auto_increment {
        sql.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    } else if let Some(d) = c.default.as_deref().filter(|d| !d.trim().is_empty()) {
        sql.push_str(&format!(" DEFAULT {d}"));
    }
    if !c.nullable {
        sql.push_str(" NOT NULL");
    }
    Ok(sql)
}

fn part_sql(p: &IndexPart) -> Result<String> {
    let mut out = match p.expression.as_deref().filter(|e| !e.trim().is_empty()) {
        // A lone function call needs no extra parentheses, anything else does.
        Some(e) => format!("({e})"),
        None => quote_ident(&name(&p.column, "索引欄位")?),
    };
    if p.descending {
        out.push_str(" DESC");
    }
    Ok(out)
}

fn parts_sql(ix: &IndexDesign) -> Result<String> {
    if ix.parts.is_empty() {
        return Err(invalid(&format!("索引 {} 沒有欄位", ix.name)));
    }
    Ok(ix
        .parts
        .iter()
        .map(part_sql)
        .collect::<Result<Vec<_>>>()?
        .join(", "))
}

fn create_index_sql(schema: &str, table: &str, ix: &IndexDesign) -> Result<String> {
    if matches!(ix.kind, IndexKind::Fulltext | IndexKind::Spatial) {
        return Err(invalid(
            "PostgreSQL 沒有 FULLTEXT / SPATIAL 索引；請改用一般索引並把方法設為 gin 或 gist。",
        ));
    }
    let using = match ix.using.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) if u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            format!(" USING {u}")
        }
        Some(u) => return Err(invalid(&format!("索引方法 {u} 不正確"))),
        None => String::new(),
    };
    Ok(format!(
        "CREATE {}INDEX {} ON {}{using} ({})",
        if ix.kind == IndexKind::Unique {
            "UNIQUE "
        } else {
            ""
        },
        quote_ident(&name(&ix.name, "索引名稱")?),
        q(schema, table),
        parts_sql(ix)?
    ))
}

fn fk_sql(schema: &str, fk: &ForeignKeyDesign) -> Result<String> {
    if fk.columns.is_empty() || fk.columns.len() != fk.ref_columns.len() {
        return Err(invalid(&format!(
            "外鍵 {} 的欄位和參照欄位數量要一樣",
            fk.name
        )));
    }
    let list = |cols: &[String]| {
        cols.iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let rule = |r: &str, what: &str| -> Result<String> {
        let r = r.trim().to_ascii_uppercase();
        match r.as_str() {
            "" => Ok(String::new()),
            "RESTRICT" | "CASCADE" | "SET NULL" | "NO ACTION" | "SET DEFAULT" => {
                Ok(format!(" {what} {r}"))
            }
            _ => Err(invalid(&format!("外鍵規則 {r} 不正確"))),
        }
    };
    let ref_schema = fk
        .ref_database
        .as_deref()
        .filter(|d| !d.is_empty())
        .unwrap_or(schema);
    Ok(format!(
        "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}){}{}",
        quote_ident(&name(&fk.name, "外鍵名稱")?),
        list(&fk.columns),
        q(ref_schema, &name(&fk.ref_table, "參照資料表")?),
        list(&fk.ref_columns),
        rule(&fk.on_delete, "ON DELETE")?,
        rule(&fk.on_update, "ON UPDATE")?
    ))
}

fn comment_sql(target: &str, comment: &str) -> String {
    let value = if comment.is_empty() {
        "NULL".to_string()
    } else {
        quote_literal(comment)
    };
    format!("COMMENT ON {target} IS {value}")
}

/// `CREATE TABLE` with its primary key and foreign keys, then the other
/// indexes and the comments.
pub fn create_table_sql(schema: &str, design: &TableDesign) -> Result<Vec<String>> {
    let table = name(&design.name, "資料表名稱")?;
    if design.columns.is_empty() {
        return Err(invalid("資料表至少要有一個欄位"));
    }
    let mut lines = design
        .columns
        .iter()
        .map(column_sql)
        .collect::<Result<Vec<_>>>()?;
    for ix in design
        .indexes
        .iter()
        .filter(|i| i.kind == IndexKind::Primary)
    {
        lines.push(format!("PRIMARY KEY ({})", parts_sql(ix)?));
    }
    for fk in &design.foreign_keys {
        lines.push(fk_sql(schema, fk)?);
    }
    let mut out = vec![format!(
        "CREATE TABLE {} (\n  {}\n)",
        q(schema, &table),
        lines.join(",\n  ")
    )];
    for ix in design
        .indexes
        .iter()
        .filter(|i| i.kind != IndexKind::Primary)
    {
        out.push(create_index_sql(schema, &table, ix)?);
    }
    if !design.options.comment.is_empty() {
        out.push(comment_sql(
            &format!("TABLE {}", q(schema, &table)),
            &design.options.comment,
        ));
    }
    for c in design.columns.iter().filter(|c| !c.comment.is_empty()) {
        out.push(comment_sql(
            &format!("COLUMN {}.{}", q(schema, &table), quote_ident(&c.name)),
            &c.comment,
        ));
    }
    Ok(out)
}

/// Same index apart from its name and comment.
fn same_index(a: &IndexDesign, b: &IndexDesign) -> bool {
    let norm = |u: &Option<String>| {
        u.as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty() && s != "btree")
    };
    a.kind == b.kind && a.parts == b.parts && norm(&a.using) == norm(&b.using)
}

fn same_fk(a: &ForeignKeyDesign, b: &ForeignKeyDesign) -> bool {
    let up = |s: &str| s.trim().to_ascii_uppercase();
    a.columns == b.columns
        && a.ref_database == b.ref_database
        && a.ref_table == b.ref_table
        && a.ref_columns == b.ref_columns
        && up(&a.on_delete) == up(&b.on_delete)
        && up(&a.on_update) == up(&b.on_update)
}

/// The statements that turn `original` into `design`, plus notes.
/// `constraints` are the names of the table's primary key and unique
/// constraints (dropped with `DROP CONSTRAINT`, not `DROP INDEX`).
pub fn alter_table_sql(
    schema: &str,
    original: &TableDesign,
    design: &TableDesign,
    constraints: &[String],
) -> Result<(Vec<String>, Vec<String>)> {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    let new_name = name(&design.name, "資料表名稱")?;
    if new_name != original.name {
        out.push(format!(
            "ALTER TABLE {} RENAME TO {}",
            q(schema, &original.name),
            quote_ident(&new_name)
        ));
    }
    let t = q(schema, &new_name);

    // Foreign keys going away or changing.
    for fk in &original.foreign_keys {
        let kept = design
            .foreign_keys
            .iter()
            .find(|d| d.original.as_deref() == fk.original.as_deref());
        match kept {
            Some(d) if same_fk(d, fk) => {
                if d.name != fk.name {
                    out.push(format!(
                        "ALTER TABLE {t} RENAME CONSTRAINT {} TO {}",
                        quote_ident(&fk.name),
                        quote_ident(&name(&d.name, "外鍵名稱")?)
                    ));
                }
            }
            _ => out.push(format!(
                "ALTER TABLE {t} DROP CONSTRAINT {}",
                quote_ident(fk.original.as_deref().unwrap_or(&fk.name))
            )),
        }
    }

    // Indexes going away or changing.
    let drop_index = |ix: &IndexDesign| {
        let real = ix.original.clone().unwrap_or_else(|| ix.name.clone());
        if constraints.contains(&real) {
            format!("ALTER TABLE {t} DROP CONSTRAINT {}", quote_ident(&real))
        } else {
            format!("DROP INDEX {}", q(schema, &real))
        }
    };
    for ix in &original.indexes {
        let kept = design
            .indexes
            .iter()
            .find(|d| d.original.is_some() && d.original == ix.original);
        match kept {
            Some(d) if same_index(d, ix) => {
                let real = ix.original.clone().unwrap_or_default();
                if d.kind != IndexKind::Primary && d.name != ix.name {
                    out.push(format!(
                        "ALTER INDEX {} RENAME TO {}",
                        q(schema, &real),
                        quote_ident(&name(&d.name, "索引名稱")?)
                    ));
                }
                if d.comment != ix.comment {
                    let now = if d.kind == IndexKind::Primary {
                        real
                    } else {
                        d.name.clone()
                    };
                    out.push(comment_sql(
                        &format!("INDEX {}", q(schema, &now)),
                        &d.comment,
                    ));
                }
            }
            _ => out.push(drop_index(ix)),
        }
    }

    // Columns.
    for c in &original.columns {
        if !design
            .columns
            .iter()
            .any(|d| d.original.as_deref() == Some(c.name.as_str()))
        {
            out.push(format!(
                "ALTER TABLE {t} DROP COLUMN {}",
                quote_ident(&c.name)
            ));
        }
    }
    let kept_order: Vec<&str> = design
        .columns
        .iter()
        .filter_map(|c| c.original.as_deref())
        .collect();
    let orig_order: Vec<&str> = original
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .filter(|n| kept_order.contains(n))
        .collect();
    if kept_order != orig_order {
        notes.push("PostgreSQL 不能調整既有欄位的順序，順序的變更會被忽略。".into());
    }
    for d in &design.columns {
        let Some(orig_name) = d.original.as_deref() else {
            out.push(format!("ALTER TABLE {t} ADD COLUMN {}", column_sql(d)?));
            if !d.comment.is_empty() {
                out.push(comment_sql(
                    &format!("COLUMN {t}.{}", quote_ident(&d.name)),
                    &d.comment,
                ));
            }
            continue;
        };
        let Some(o) = original.columns.iter().find(|c| c.name == orig_name) else {
            continue;
        };
        let col_name = name(&d.name, "欄位名稱")?;
        if col_name != o.name {
            out.push(format!(
                "ALTER TABLE {t} RENAME COLUMN {} TO {}",
                quote_ident(&o.name),
                quote_ident(&col_name)
            ));
        }
        let c = quote_ident(&col_name);
        if d.generated != o.generated || d.stored != o.stored {
            return Err(invalid(&format!(
                "欄位 {col_name}：PostgreSQL 不能直接修改產生欄位，請刪除這個欄位再新增。"
            )));
        }
        let new_type = name(&d.column_type, &format!("欄位 {col_name} 的型別"))?;
        if new_type != o.column_type {
            out.push(format!(
                "ALTER TABLE {t} ALTER COLUMN {c} TYPE {new_type} USING {c}::{new_type}"
            ));
        }
        if d.auto_increment != o.auto_increment {
            if d.auto_increment {
                if o.default.is_some() {
                    out.push(format!("ALTER TABLE {t} ALTER COLUMN {c} DROP DEFAULT"));
                }
                out.push(format!(
                    "ALTER TABLE {t} ALTER COLUMN {c} ADD GENERATED BY DEFAULT AS IDENTITY"
                ));
            } else {
                out.push(format!(
                    "ALTER TABLE {t} ALTER COLUMN {c} DROP IDENTITY IF EXISTS"
                ));
            }
        }
        if !d.auto_increment && d.generated.is_none() && d.default != o.default {
            match d.default.as_deref().filter(|x| !x.trim().is_empty()) {
                Some(v) => out.push(format!("ALTER TABLE {t} ALTER COLUMN {c} SET DEFAULT {v}")),
                None => out.push(format!("ALTER TABLE {t} ALTER COLUMN {c} DROP DEFAULT")),
            }
        }
        if d.nullable != o.nullable {
            out.push(format!(
                "ALTER TABLE {t} ALTER COLUMN {c} {} NOT NULL",
                if d.nullable { "DROP" } else { "SET" }
            ));
        }
        if d.comment != o.comment {
            out.push(comment_sql(&format!("COLUMN {t}.{c}"), &d.comment));
        }
    }

    // Indexes and keys being added (or re-added after a change).
    for ix in &design.indexes {
        let unchanged = ix.original.is_some()
            && original
                .indexes
                .iter()
                .any(|o| o.original == ix.original && same_index(o, ix));
        if unchanged {
            continue;
        }
        if ix.kind == IndexKind::Primary {
            out.push(format!(
                "ALTER TABLE {t} ADD PRIMARY KEY ({})",
                parts_sql(ix)?
            ));
        } else {
            out.push(create_index_sql(schema, &new_name, ix)?);
            if !ix.comment.is_empty() {
                out.push(comment_sql(
                    &format!("INDEX {}", q(schema, &ix.name)),
                    &ix.comment,
                ));
            }
        }
    }
    for fk in &design.foreign_keys {
        let unchanged = fk.original.is_some()
            && original
                .foreign_keys
                .iter()
                .any(|o| o.original == fk.original && same_fk(o, fk));
        if !unchanged {
            out.push(format!("ALTER TABLE {t} ADD {}", fk_sql(schema, fk)?));
        }
    }
    if design.options.comment != original.options.comment {
        out.push(comment_sql(&format!("TABLE {t}"), &design.options.comment));
    }
    Ok((out, notes))
}

/// Names of the table's primary key and unique constraints.
async fn constraint_names(s: &PgSession, schema: &str, table: &str) -> Result<Vec<String>> {
    let oid = table_oid(s, schema, table).await?;
    Ok(s.query(
        "SELECT conname::text FROM pg_constraint
             WHERE conrelid = $1::oid AND contype IN ('p', 'u', 'x')",
        &[&oid],
    )
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect())
}

/// Refuses statements that would run as more than one.
fn single(statements: &[String]) -> Result<()> {
    for st in statements {
        if crate::split::split_statements_pg(st).len() > 1 {
            return Err(invalid(&format!(
                "這段 SQL 裡有分號，會被當成好幾句：{}",
                st.lines().next().unwrap_or(st)
            )));
        }
    }
    Ok(())
}

pub async fn plan(s: &PgSession, change: &SchemaChange) -> Result<DdlPlan> {
    let mut notes = Vec::new();
    let statements = match change {
        SchemaChange::CreateDatabase { name: n, .. } => {
            vec![format!(
                "CREATE SCHEMA {}",
                quote_ident(&name(n, "schema 名稱")?)
            )]
        }
        SchemaChange::AlterDatabase { .. } => {
            return Err(invalid("PostgreSQL 的 schema 沒有字元集設定。"));
        }
        SchemaChange::DropDatabase { name: n } => {
            notes.push(format!(
                "會一併刪除 schema {n} 裡的所有資料表、檢視、函式與資料（CASCADE），無法復原。"
            ));
            vec![format!("DROP SCHEMA {} CASCADE", quote_ident(n))]
        }
        SchemaChange::CreateTable { database, design } => create_table_sql(database, design)?,
        SchemaChange::AlterTable {
            database,
            original,
            design,
            ..
        } => {
            let constraints = constraint_names(s, database, &original.name).await?;
            let (st, n) = alter_table_sql(database, original, design, &constraints)?;
            notes.extend(n);
            st
        }
        SchemaChange::DropTable { database, table } => {
            notes.push("參照這個資料表的檢視或外鍵會讓刪除失敗；不會用 CASCADE。".into());
            vec![format!("DROP TABLE {}", q(database, table))]
        }
        SchemaChange::SaveObject {
            database,
            object_kind,
            original,
            definition: def,
        } => {
            let def = def.trim().trim_end_matches(';').trim_end().to_string();
            if def.is_empty() {
                return Err(invalid("定義是空的"));
            }
            let replaces = def.to_ascii_uppercase().contains("OR REPLACE");
            // Unqualified names in the definition mean this schema; only for
            // this transaction.
            let mut st = vec![format!(
                "SET LOCAL search_path TO {}, public",
                quote_ident(database)
            )];
            if let Some(orig) = original {
                match object_kind {
                    ObjectKind::View => {
                        if is_materialized(s, database, orig).await? {
                            st.push(format!("DROP MATERIALIZED VIEW {}", q(database, orig)));
                        } else if !replaces {
                            st.push(format!("DROP VIEW {}", q(database, orig)));
                        }
                    }
                    ObjectKind::Function | ObjectKind::Procedure => {
                        if replaces {
                            notes.push("CREATE OR REPLACE：如果改了名稱或參數型別，會變成另一個新的函式，原本的還在。".into());
                        } else {
                            st.push(drop_object_sql(s, database, *object_kind, orig).await?);
                        }
                    }
                    ObjectKind::Trigger => {
                        if !replaces {
                            st.push(drop_object_sql(s, database, *object_kind, orig).await?);
                        }
                    }
                    ObjectKind::Event => return Err(invalid("PostgreSQL 沒有事件（EVENT）。")),
                }
            }
            st.push(def);
            notes.push("全部在同一個交易裡執行，任何一句失敗都會整批還原。".into());
            st
        }
        SchemaChange::DropObject {
            database,
            object_kind,
            name: n,
        } => vec![drop_object_sql(s, database, *object_kind, n).await?],
        SchemaChange::SetEventEnabled { .. } | SchemaChange::SetEventScheduler { .. } => {
            return Err(invalid("PostgreSQL 沒有事件（EVENT）。"));
        }
    };
    single(&statements)?;
    Ok(DdlPlan { statements, notes })
}

async fn drop_object_sql(s: &PgSession, schema: &str, kind: ObjectKind, n: &str) -> Result<String> {
    Ok(match kind {
        ObjectKind::View => {
            if is_materialized(s, schema, n).await? {
                format!("DROP MATERIALIZED VIEW {}", q(schema, n))
            } else {
                format!("DROP VIEW {}", q(schema, n))
            }
        }
        ObjectKind::Function | ObjectKind::Procedure => {
            let oid = routine_oid(s, schema, n).await?;
            let sig: String = s
                .query(
                    "SELECT quote_ident(proname) || '(' || pg_get_function_identity_arguments(oid) || ')'
                     FROM pg_proc WHERE oid = $1::oid",
                    &[&oid],
                )
                .await?
                .first()
                .map(|r| r.get(0))
                .unwrap_or_default();
            let word = if kind == ObjectKind::Procedure {
                "PROCEDURE"
            } else {
                "FUNCTION"
            };
            format!("DROP {word} {}.{sig}", quote_ident(schema))
        }
        ObjectKind::Trigger => {
            let (_, table) = trigger_table(s, schema, n).await?;
            format!("DROP TRIGGER {} ON {}", quote_ident(n), q(schema, &table))
        }
        ObjectKind::Event => return Err(invalid("PostgreSQL 沒有事件（EVENT）。")),
    })
}

/// Runs `change` in one transaction, with a note holding what it replaced.
pub async fn apply(s: &PgSession, change: &SchemaChange) -> Result<DdlRun> {
    let mut note = String::new();
    let database = match change {
        SchemaChange::CreateDatabase { .. } | SchemaChange::DropDatabase { .. } => None,
        SchemaChange::AlterDatabase { name, .. } => Some(name.clone()),
        SchemaChange::CreateTable { database, .. }
        | SchemaChange::AlterTable { database, .. }
        | SchemaChange::DropTable { database, .. }
        | SchemaChange::SaveObject { database, .. }
        | SchemaChange::DropObject { database, .. }
        | SchemaChange::SetEventEnabled { database, .. } => Some(database.clone()),
        SchemaChange::SetEventScheduler { .. } => None,
    };
    match change {
        SchemaChange::AlterTable {
            database, original, ..
        } => {
            let now = table_design(s, database, &original.name).await?;
            if now != **original {
                return Err(invalid(
                    "這個資料表在你開啟設計器之後被變更了。請重新載入，再重新修改。",
                ));
            }
            let create = s
                .show_create_table(Some(database), &original.name)
                .await
                .unwrap_or_default();
            note = format!(
                "結構變更：修改資料表 {database}.{}\n\n-- 變更前的結構\n{create}",
                original.name
            );
        }
        SchemaChange::DropTable { database, table } => {
            let create = s
                .show_create_table(Some(database), table)
                .await
                .unwrap_or_default();
            note = format!("結構變更：刪除資料表 {database}.{table}\n\n-- 刪除前的結構\n{create}");
        }
        SchemaChange::DropDatabase { name } => {
            let tables = s.list_tables(name).await.unwrap_or_default();
            note = format!(
                "結構變更：刪除 schema {name}\n\n刪除前有 {} 個資料表或檢視：{}",
                tables.len(),
                tables
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        SchemaChange::SaveObject {
            database,
            object_kind,
            original: Some(orig),
            ..
        } => {
            let old = definition(s, database, *object_kind, orig).await?;
            note = format!(
                "結構變更：修改 {database}.{orig}\n\n-- 原本的定義\n{};",
                old.definition
            );
        }
        SchemaChange::DropObject {
            database,
            object_kind,
            name,
        } => {
            let old = definition(s, database, *object_kind, name).await?;
            note = format!(
                "結構變更：刪除 {database}.{name}\n\n-- 刪除前的定義\n{};",
                old.definition
            );
        }
        _ => {}
    }
    let planned = plan(s, change).await?;
    if planned.statements.is_empty() {
        return Err(invalid("沒有需要變更的地方。"));
    }
    let outcome = s.run_in_transaction(&planned.statements).await;
    let sql = format!(
        "BEGIN;\n\n{}\n\nCOMMIT;",
        planned
            .statements
            .iter()
            .map(|st| format!("{st};"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str) -> ColumnDesign {
        ColumnDesign {
            original: Some(name.into()),
            name: name.into(),
            column_type: ty.into(),
            nullable: true,
            ..Default::default()
        }
    }

    fn table() -> TableDesign {
        TableDesign {
            name: "items".into(),
            columns: vec![
                ColumnDesign {
                    auto_increment: true,
                    nullable: false,
                    ..col("id", "integer")
                },
                col("name", "text"),
            ],
            indexes: vec![IndexDesign {
                original: Some("items_pkey".into()),
                name: "PRIMARY".into(),
                kind: IndexKind::Primary,
                parts: vec![IndexPart {
                    column: "id".into(),
                    ..Default::default()
                }],
                using: None,
                comment: String::new(),
            }],
            foreign_keys: vec![],
            options: TableOptions::default(),
        }
    }

    #[test]
    fn create_table() {
        let mut t = table();
        t.options.comment = "it's".into();
        let sql = create_table_sql("app", &t).unwrap();
        assert_eq!(
            sql[0],
            "CREATE TABLE \"app\".\"items\" (\n  \"id\" integer GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n  \"name\" text,\n  PRIMARY KEY (\"id\")\n)"
        );
        assert_eq!(sql[1], "COMMENT ON TABLE \"app\".\"items\" IS 'it''s'");
    }

    #[test]
    fn alter_table_diff() {
        let orig = table();
        let mut d = orig.clone();
        d.name = "goods".into();
        d.columns[1].name = "title".into();
        d.columns[1].nullable = false;
        d.columns[1].column_type = "varchar(100)".into();
        d.columns.push(ColumnDesign {
            original: None,
            default: Some("0".into()),
            ..col("qty", "integer")
        });
        d.indexes.push(IndexDesign {
            original: None,
            name: "goods_title".into(),
            kind: IndexKind::Unique,
            parts: vec![IndexPart {
                column: "title".into(),
                ..Default::default()
            }],
            using: None,
            comment: String::new(),
        });
        let (sql, notes) = alter_table_sql("app", &orig, &d, &["items_pkey".into()]).unwrap();
        assert!(notes.is_empty());
        assert_eq!(
            sql,
            [
                "ALTER TABLE \"app\".\"items\" RENAME TO \"goods\"",
                "ALTER TABLE \"app\".\"goods\" RENAME COLUMN \"name\" TO \"title\"",
                "ALTER TABLE \"app\".\"goods\" ALTER COLUMN \"title\" TYPE varchar(100) USING \"title\"::varchar(100)",
                "ALTER TABLE \"app\".\"goods\" ALTER COLUMN \"title\" SET NOT NULL",
                "ALTER TABLE \"app\".\"goods\" ADD COLUMN \"qty\" integer DEFAULT 0",
                "CREATE UNIQUE INDEX \"goods_title\" ON \"app\".\"goods\" (\"title\")",
            ]
        );
    }

    #[test]
    fn primary_key_change_drops_the_constraint() {
        let orig = table();
        let mut d = orig.clone();
        d.indexes[0].parts.push(IndexPart {
            column: "name".into(),
            ..Default::default()
        });
        let (sql, _) = alter_table_sql("app", &orig, &d, &["items_pkey".into()]).unwrap();
        assert_eq!(
            sql,
            [
                "ALTER TABLE \"app\".\"items\" DROP CONSTRAINT \"items_pkey\"",
                "ALTER TABLE \"app\".\"items\" ADD PRIMARY KEY (\"id\", \"name\")",
            ]
        );
    }

    #[test]
    fn semicolons_are_refused() {
        assert!(single(&["ALTER TABLE t ADD COLUMN a int; DROP TABLE x".into()]).is_err());
        assert!(single(&["COMMENT ON TABLE t IS 'a;b'".into()]).is_ok());
    }
}
