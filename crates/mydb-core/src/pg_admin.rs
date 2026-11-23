//! PostgreSQL server administration: overview, running queries, settings,
//! roles and privileges, and `pg_stat_statements`.
//!
//! As in [`crate::admin`], statements that change anything are built here
//! from a structured [`PgAdminChange`], so the preview the user confirms and
//! what runs come from the same code. A password is sent as a SCRAM-SHA-256
//! verifier computed here (like psql's `\password`), so the plain text never
//! reaches the server or its logs; previews show it masked.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::admin::MASK;
use crate::ddl::invalid;
use crate::error::Result;
use crate::pg::{PgSession, quote_ident, quote_literal};

#[derive(Debug, Clone, Serialize)]
pub struct PgOverview {
    /// `version()`, e.g. `PostgreSQL 16.4 on aarch64-unknown-linux-gnu …`.
    pub version: String,
    pub server_version: String,
    pub current_user: String,
    pub current_database: String,
    pub uptime_seconds: f64,
    pub connections: i64,
    pub max_connections: i64,
    pub active: i64,
    pub idle_in_transaction: i64,
    /// Age of the oldest open transaction of another connection.
    pub oldest_transaction_seconds: f64,
    /// A read replica (hot standby).
    pub in_recovery: bool,
    pub databases: Vec<PgDatabaseStat>,
    /// Whether the `pg_stat_statements` extension is installed here.
    pub has_pg_stat_statements: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgDatabaseStat {
    pub name: String,
    /// `None` without CONNECT on it (e.g. `rdsadmin` on RDS).
    pub size_bytes: Option<i64>,
    pub connections: i64,
    pub commits: i64,
    pub rollbacks: i64,
    pub blocks_read: i64,
    pub blocks_hit: i64,
    pub deadlocks: i64,
    pub temp_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgSetting {
    pub name: String,
    pub setting: String,
    pub unit: String,
    pub category: String,
    pub description: String,
    /// Where the value came from: `default`, `configuration file`, …
    pub source: String,
    /// When a change takes effect: `postmaster` (restart), `sighup`, `user`, …
    pub context: String,
    pub pending_restart: bool,
}

/// One row of `pg_stat_activity`.
#[derive(Debug, Clone, Serialize)]
pub struct PgActivity {
    pub pid: i32,
    pub user: String,
    pub application: String,
    pub client: String,
    pub database: Option<String>,
    /// `active`, `idle`, `idle in transaction`, …; empty for background workers.
    pub state: String,
    pub wait: Option<String>,
    pub backend_type: String,
    pub query_seconds: Option<f64>,
    pub transaction_seconds: Option<f64>,
    pub query: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgRole {
    pub name: String,
    pub can_login: bool,
    pub superuser: bool,
    pub create_db: bool,
    pub create_role: bool,
    pub replication: bool,
    /// -1 for no limit.
    pub connection_limit: i32,
    pub valid_until: Option<String>,
    /// Roles this one is a member of.
    pub member_of: Vec<String>,
}

/// What a role may do on one object of the connected database.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PgGrant {
    pub target: PgTarget,
    /// Upper case, e.g. `SELECT`, `USAGE`.
    pub privileges: Vec<String>,
    /// The ones it may pass on (WITH GRANT OPTION).
    pub grantable: Vec<String>,
}

/// `ALTER DEFAULT PRIVILEGES`: what the role gets on objects created later.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PgDefaultGrant {
    /// Objects created by this role get the privileges.
    pub for_role: String,
    /// `None` for every schema.
    pub schema: Option<String>,
    /// `tables`, `sequences`, `functions`, `types` or `schemas`.
    pub object: String,
    pub privileges: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PgPrivileges {
    pub grants: Vec<PgGrant>,
    pub defaults: Vec<PgDefaultGrant>,
    /// Objects the role owns here (it has every privilege on them).
    pub owned: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PgObjectKind {
    Database,
    Schema,
    Table,
    Sequence,
    Function,
    AllTables,
    AllSequences,
    AllFunctions,
}

/// Where a privilege applies. `schema` is set for objects in a schema and
/// for the `All*` kinds; `name` for everything but the `All*` kinds. A
/// function's name includes its argument types, e.g. `f(integer)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PgTarget {
    pub kind: PgObjectKind,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// One statement of `pg_stat_statements`.
#[derive(Debug, Clone, Serialize)]
pub struct PgStatement {
    pub query: String,
    pub calls: i64,
    pub total_ms: f64,
    pub mean_ms: f64,
    pub rows: i64,
    pub database: String,
    pub user: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PgAdminChange {
    CreateRole {
        name: String,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        login: bool,
        #[serde(default)]
        create_db: bool,
        #[serde(default)]
        create_role: bool,
        #[serde(default)]
        connection_limit: Option<i32>,
        #[serde(default)]
        valid_until: Option<String>,
        #[serde(default)]
        member_of: Vec<String>,
    },
    AlterRole {
        name: String,
        login: bool,
        create_db: bool,
        create_role: bool,
        /// -1 for no limit.
        connection_limit: i32,
        /// `None` (or empty) for no expiry.
        #[serde(default)]
        valid_until: Option<String>,
    },
    SetPassword {
        name: String,
        password: String,
    },
    DropRole {
        name: String,
        /// Hand what it owns in this database to this role first, and drop
        /// its privileges here (REASSIGN OWNED + DROP OWNED).
        #[serde(default)]
        reassign_to: Option<String>,
    },
    GrantRole {
        /// The group role.
        role: String,
        member: String,
    },
    RevokeRole {
        role: String,
        member: String,
    },
    Grant {
        role: String,
        target: PgTarget,
        privileges: Vec<String>,
        #[serde(default)]
        grant_option: bool,
    },
    /// Takes `privileges` away (all of them when empty).
    Revoke {
        role: String,
        target: PgTarget,
        #[serde(default)]
        privileges: Vec<String>,
    },
    DefaultPrivileges {
        role: String,
        #[serde(default)]
        schema: Option<String>,
        /// `tables`, `sequences` or `functions`.
        object: String,
        privileges: Vec<String>,
        /// Revoke instead.
        #[serde(default)]
        revoke: bool,
        /// Objects created by this role (default: the current user).
        #[serde(default)]
        for_role: Option<String>,
    },
    /// `pg_cancel_backend`: stop the running statement, keep the connection.
    Cancel {
        pid: i32,
    },
    /// `pg_terminate_backend`: end the connection.
    Terminate {
        pid: i32,
    },
}

// ---------------------------------------------------------------------------
// Reading

pub async fn overview(s: &PgSession) -> Result<PgOverview> {
    let rows = s
        .query(
            "SELECT version(), current_setting('server_version'), current_user::text,
                    current_database()::text,
                    extract(epoch FROM now() - pg_postmaster_start_time())::float8,
                    (SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend'),
                    current_setting('max_connections')::int8,
                    (SELECT count(*) FROM pg_stat_activity
                      WHERE backend_type = 'client backend' AND state = 'active'),
                    (SELECT count(*) FROM pg_stat_activity
                      WHERE state LIKE 'idle in transaction%'),
                    (SELECT coalesce(max(extract(epoch FROM now() - xact_start)), 0)::float8
                       FROM pg_stat_activity
                      WHERE xact_start IS NOT NULL AND pid <> pg_backend_pid()),
                    pg_is_in_recovery(),
                    EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')",
            &[],
        )
        .await?;
    let r = &rows[0];
    let databases = s
        .query(
            "SELECT d.datname::text,
                    CASE WHEN has_database_privilege(d.datname, 'CONNECT')
                         THEN pg_database_size(d.oid) END,
                    coalesce(st.numbackends, 0)::int8, coalesce(st.xact_commit, 0)::int8,
                    coalesce(st.xact_rollback, 0)::int8, coalesce(st.blks_read, 0)::int8,
                    coalesce(st.blks_hit, 0)::int8, coalesce(st.deadlocks, 0)::int8,
                    coalesce(st.temp_bytes, 0)::int8
               FROM pg_database d LEFT JOIN pg_stat_database st ON st.datid = d.oid
              WHERE NOT d.datistemplate
              ORDER BY 1",
            &[],
        )
        .await?
        .iter()
        .map(|d| PgDatabaseStat {
            name: d.get(0),
            size_bytes: d.get(1),
            connections: d.get(2),
            commits: d.get(3),
            rollbacks: d.get(4),
            blocks_read: d.get(5),
            blocks_hit: d.get(6),
            deadlocks: d.get(7),
            temp_bytes: d.get(8),
        })
        .collect();
    Ok(PgOverview {
        version: r.get(0),
        server_version: r.get(1),
        current_user: r.get(2),
        current_database: r.get(3),
        uptime_seconds: r.get(4),
        connections: r.get(5),
        max_connections: r.get(6),
        active: r.get(7),
        idle_in_transaction: r.get(8),
        oldest_transaction_seconds: r.get(9),
        in_recovery: r.get(10),
        databases,
        has_pg_stat_statements: r.get(11),
    })
}

pub async fn settings(s: &PgSession) -> Result<Vec<PgSetting>> {
    Ok(s
        .query(
            "SELECT name::text, coalesce(setting, ''), coalesce(unit, ''), coalesce(category, ''),
                    coalesce(short_desc, ''), coalesce(source, ''), coalesce(context, ''),
                    coalesce(pending_restart, false)
               FROM pg_settings ORDER BY name",
            &[],
        )
        .await?
        .iter()
        .map(|r| PgSetting {
            name: r.get(0),
            setting: r.get(1),
            unit: r.get(2),
            category: r.get(3),
            description: r.get(4),
            source: r.get(5),
            context: r.get(6),
            pending_restart: r.get(7),
        })
        .collect())
}

pub async fn activity(s: &PgSession) -> Result<Vec<PgActivity>> {
    Ok(s
        .query(
            "SELECT pid, coalesce(usename::text, ''), coalesce(application_name, ''),
                    coalesce(host(client_addr), CASE WHEN client_port = -1 THEN 'local' ELSE '' END),
                    datname::text, coalesce(state, ''),
                    CASE WHEN wait_event IS NOT NULL THEN wait_event_type || ': ' || wait_event END,
                    coalesce(backend_type, ''),
                    extract(epoch FROM now() - query_start)::float8,
                    extract(epoch FROM now() - xact_start)::float8,
                    coalesce(query, '')
               FROM pg_stat_activity ORDER BY pid",
            &[],
        )
        .await?
        .iter()
        .map(|r| PgActivity {
            pid: r.get(0),
            user: r.get(1),
            application: r.get(2),
            client: r.get(3),
            database: r.get(4),
            state: r.get(5),
            wait: r.get(6),
            backend_type: r.get(7),
            query_seconds: r.get(8),
            transaction_seconds: r.get(9),
            query: r.get(10),
        })
        .collect())
}

pub async fn roles(s: &PgSession) -> Result<Vec<PgRole>> {
    Ok(s
        .query(
            "SELECT r.rolname::text, r.rolcanlogin, r.rolsuper, r.rolcreatedb, r.rolcreaterole,
                    r.rolreplication, r.rolconnlimit, r.rolvaliduntil::text,
                    array(SELECT b.rolname::text FROM pg_auth_members m
                            JOIN pg_roles b ON b.oid = m.roleid
                           WHERE m.member = r.oid ORDER BY 1)
               FROM pg_roles r
              WHERE r.rolname !~ '^pg_'
              ORDER BY 1",
            &[],
        )
        .await?
        .iter()
        .map(|r| PgRole {
            name: r.get(0),
            can_login: r.get(1),
            superuser: r.get(2),
            create_db: r.get(3),
            create_role: r.get(4),
            replication: r.get(5),
            connection_limit: r.get(6),
            valid_until: r.get(7),
            member_of: r.get(8),
        })
        .collect())
}

/// What `role` was granted in the connected database.
pub async fn privileges(s: &PgSession, role: &str) -> Result<PgPrivileges> {
    let rows = s
        .query(
            "WITH r AS (SELECT oid FROM pg_roles WHERE rolname = $1)
             SELECT 'database', NULL::text, d.datname::text, a.privilege_type, a.is_grantable
               FROM pg_database d, aclexplode(d.datacl) a, r
              WHERE d.datname = current_database() AND a.grantee = r.oid
             UNION ALL
             SELECT 'schema', NULL, n.nspname::text, a.privilege_type, a.is_grantable
               FROM pg_namespace n, aclexplode(n.nspacl) a, r
              WHERE a.grantee = r.oid
             UNION ALL
             SELECT CASE WHEN c.relkind = 'S' THEN 'sequence' ELSE 'table' END,
                    n.nspname::text, c.relname::text, a.privilege_type, a.is_grantable
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace,
                    aclexplode(c.relacl) a, r
              WHERE c.relkind IN ('r', 'p', 'v', 'm', 'S', 'f') AND a.grantee = r.oid
             UNION ALL
             SELECT 'function', n.nspname::text,
                    p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')',
                    a.privilege_type, a.is_grantable
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace,
                    aclexplode(p.proacl) a, r
              WHERE a.grantee = r.oid AND n.nspname NOT IN ('pg_catalog', 'information_schema')
             ORDER BY 1, 2, 3, 4",
            &[&role],
        )
        .await?;
    let mut grants: BTreeMap<PgTarget, PgGrant> = BTreeMap::new();
    for r in &rows {
        let kind = match r.get::<_, &str>(0) {
            "database" => PgObjectKind::Database,
            "schema" => PgObjectKind::Schema,
            "sequence" => PgObjectKind::Sequence,
            "function" => PgObjectKind::Function,
            _ => PgObjectKind::Table,
        };
        let target = PgTarget {
            kind,
            schema: r.get(1),
            name: r.get(2),
        };
        let g = grants.entry(target.clone()).or_insert_with(|| PgGrant {
            target,
            privileges: Vec::new(),
            grantable: Vec::new(),
        });
        let p: String = r.get(3);
        if r.get::<_, bool>(4) {
            g.grantable.push(p.clone());
        }
        g.privileges.push(p);
    }
    let defaults = s
        .query(
            "WITH r AS (SELECT oid FROM pg_roles WHERE rolname = $1)
             SELECT pg_get_userbyid(d.defaclrole)::text, n.nspname::text,
                    CASE d.defaclobjtype WHEN 'r' THEN 'tables' WHEN 'S' THEN 'sequences'
                         WHEN 'f' THEN 'functions' WHEN 'T' THEN 'types' ELSE 'schemas' END,
                    array_agg(a.privilege_type ORDER BY a.privilege_type)
               FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid = d.defaclnamespace,
                    aclexplode(d.defaclacl) a, r
              WHERE a.grantee = r.oid
              GROUP BY 1, 2, 3 ORDER BY 1, 2, 3",
            &[&role],
        )
        .await?
        .iter()
        .map(|r| PgDefaultGrant {
            for_role: r.get(0),
            schema: r.get(1),
            object: r.get(2),
            privileges: r.get(3),
        })
        .collect();
    let owned: i64 = s
        .query(
            "WITH r AS (SELECT oid FROM pg_roles WHERE rolname = $1)
             SELECT (SELECT count(*) FROM pg_class c, r WHERE c.relowner = r.oid
                        AND c.relnamespace NOT IN ('pg_catalog'::regnamespace,
                                                   'information_schema'::regnamespace))
                  + (SELECT count(*) FROM pg_namespace n, r WHERE n.nspowner = r.oid)
                  + (SELECT count(*) FROM pg_proc p, r WHERE p.proowner = r.oid)",
            &[&role],
        )
        .await?
        .first()
        .map_or(0, |r| r.get::<_, Option<i64>>(0).unwrap_or(0));
    Ok(PgPrivileges {
        grants: grants.into_values().collect(),
        defaults,
        owned,
    })
}

/// The heaviest statements by total time; `None` when the extension is not
/// installed in this database.
pub async fn top_statements(s: &PgSession) -> Result<Option<Vec<PgStatement>>> {
    let r = s
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements'),
                    current_setting('server_version_num')::int",
            &[],
        )
        .await?;
    if !r[0].get::<_, bool>(0) {
        return Ok(None);
    }
    // Renamed in PostgreSQL 13.
    let (total, mean) = if r[0].get::<_, i32>(1) >= 130000 {
        ("total_exec_time", "mean_exec_time")
    } else {
        ("total_time", "mean_time")
    };
    let rows = s
        .query(
            &format!(
                "SELECT coalesce(s.query, ''), s.calls::int8, s.{total}::float8, s.{mean}::float8,
                        s.rows::int8, coalesce(d.datname::text, ''),
                        coalesce(pg_get_userbyid(s.userid)::text, '')
                   FROM pg_stat_statements s LEFT JOIN pg_database d ON d.oid = s.dbid
                  ORDER BY s.{total} DESC LIMIT 100"
            ),
            &[],
        )
        .await?;
    Ok(Some(
        rows.iter()
            .map(|r| PgStatement {
                query: r.get(0),
                calls: r.get(1),
                total_ms: r.get(2),
                mean_ms: r.get(3),
                rows: r.get(4),
                database: r.get(5),
                user: r.get(6),
            })
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// Changes

fn role_name(n: &str) -> Result<String> {
    let n = n.trim();
    if n.is_empty() {
        return Err(invalid("請輸入角色名稱"));
    }
    if n.to_ascii_lowercase().starts_with("pg_") {
        return Err(invalid("pg_ 開頭的是 PostgreSQL 保留的角色名稱。"));
    }
    Ok(quote_ident(n))
}

const DATABASE_PRIVS: &[&str] = &["CREATE", "CONNECT", "TEMPORARY"];
const SCHEMA_PRIVS: &[&str] = &["USAGE", "CREATE"];
const TABLE_PRIVS: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
    "MAINTAIN",
];
const SEQUENCE_PRIVS: &[&str] = &["USAGE", "SELECT", "UPDATE"];
const FUNCTION_PRIVS: &[&str] = &["EXECUTE"];

/// `privileges` checked against what `kind` takes; empty means ALL.
fn privilege_list(kind: &PgObjectKind, privileges: &[String]) -> Result<String> {
    let allowed = match kind {
        PgObjectKind::Database => DATABASE_PRIVS,
        PgObjectKind::Schema => SCHEMA_PRIVS,
        PgObjectKind::Table | PgObjectKind::AllTables => TABLE_PRIVS,
        PgObjectKind::Sequence | PgObjectKind::AllSequences => SEQUENCE_PRIVS,
        PgObjectKind::Function | PgObjectKind::AllFunctions => FUNCTION_PRIVS,
    };
    let mut out = Vec::new();
    for p in privileges {
        let p = p.trim().to_ascii_uppercase();
        let p = if p == "TEMP" { "TEMPORARY".to_string() } else { p };
        if p == "ALL" || p == "ALL PRIVILEGES" {
            return Ok("ALL PRIVILEGES".into());
        }
        if !allowed.contains(&p.as_str()) {
            return Err(invalid(&format!("這種物件沒有 {p} 權限")));
        }
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(if out.is_empty() {
        "ALL PRIVILEGES".into()
    } else {
        out.join(", ")
    })
}

fn need<'a>(v: &'a Option<String>, what: &str) -> Result<&'a str> {
    v.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid(&format!("請選擇{what}")))
}

/// `ON …` of a GRANT or REVOKE.
async fn target_sql(s: &PgSession, t: &PgTarget) -> Result<String> {
    Ok(match t.kind {
        PgObjectKind::Database => format!("DATABASE {}", quote_ident(need(&t.name, "資料庫")?)),
        PgObjectKind::Schema => format!("SCHEMA {}", quote_ident(need(&t.name, "schema")?)),
        PgObjectKind::Table => format!(
            "TABLE {}.{}",
            quote_ident(need(&t.schema, "schema")?),
            quote_ident(need(&t.name, "資料表")?)
        ),
        PgObjectKind::Sequence => format!(
            "SEQUENCE {}.{}",
            quote_ident(need(&t.schema, "schema")?),
            quote_ident(need(&t.name, "序列")?)
        ),
        PgObjectKind::Function => {
            // Resolved by the server, which also quotes it. The name is as
            // listed (`f(n integer)`, with argument names) or typed with
            // types only (`f(integer)`).
            let schema = need(&t.schema, "schema")?;
            let name = need(&t.name, "函式")?;
            let rows = s
                .query(
                    "SELECT coalesce(
                       (SELECT p.oid::regprocedure::text FROM pg_proc p
                         WHERE p.pronamespace = to_regnamespace($1)::oid
                           AND p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = $2
                         LIMIT 1),
                       to_regprocedure(quote_ident($1) || '.' || $2)::text)",
                    &[&schema, &name],
                )
                .await?;
            let sig: Option<String> = rows.first().and_then(|r| r.get(0));
            let sig = sig.ok_or_else(|| invalid(&format!("找不到函式 {schema}.{name}")))?;
            // A schema not on search_path comes back qualified already.
            // ROUTINE covers procedures too (PostgreSQL 11+).
            if sig.contains('.') {
                format!("ROUTINE {sig}")
            } else {
                format!("ROUTINE {}.{sig}", quote_ident(schema))
            }
        }
        PgObjectKind::AllTables => format!(
            "ALL TABLES IN SCHEMA {}",
            quote_ident(need(&t.schema, "schema")?)
        ),
        PgObjectKind::AllSequences => format!(
            "ALL SEQUENCES IN SCHEMA {}",
            quote_ident(need(&t.schema, "schema")?)
        ),
        PgObjectKind::AllFunctions => format!(
            "ALL FUNCTIONS IN SCHEMA {}",
            quote_ident(need(&t.schema, "schema")?)
        ),
    })
}

fn password_sql(password: &str, masked: bool) -> Result<String> {
    if password.is_empty() {
        return Err(invalid("請輸入密碼"));
    }
    Ok(if masked {
        format!("PASSWORD '{MASK}'")
    } else {
        let verifier = postgres_protocol::password::scram_sha_256(password.as_bytes());
        format!("PASSWORD {}", quote_literal(&verifier))
    })
}

fn valid_until_sql(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| format!("VALID UNTIL {}", quote_literal(v)))
}

fn flag(on: bool, word: &str) -> String {
    if on {
        word.to_string()
    } else {
        format!("NO{word}")
    }
}

/// The statements `change` runs; with `masked`, passwords are replaced by
/// `********` for showing.
pub async fn plan(s: &PgSession, change: &PgAdminChange, masked: bool) -> Result<Vec<String>> {
    Ok(match change {
        PgAdminChange::CreateRole {
            name,
            password,
            login,
            create_db,
            create_role,
            connection_limit,
            valid_until,
            member_of,
        } => {
            let role = role_name(name)?;
            let mut opts = vec![
                flag(*login, "LOGIN"),
                flag(*create_db, "CREATEDB"),
                flag(*create_role, "CREATEROLE"),
            ];
            if let Some(n) = connection_limit {
                opts.push(format!("CONNECTION LIMIT {n}"));
            }
            if let Some(p) = password.as_deref().filter(|p| !p.is_empty()) {
                opts.push(password_sql(p, masked)?);
            }
            opts.extend(valid_until_sql(valid_until));
            let mut out = vec![format!("CREATE ROLE {role} WITH {}", opts.join(" "))];
            for g in member_of.iter().filter(|g| !g.trim().is_empty()) {
                out.push(format!("GRANT {} TO {role}", quote_ident(g.trim())));
            }
            out
        }
        PgAdminChange::AlterRole {
            name,
            login,
            create_db,
            create_role,
            connection_limit,
            valid_until,
        } => {
            let role = role_name(name)?;
            let until = valid_until_sql(valid_until).unwrap_or("VALID UNTIL 'infinity'".into());
            vec![format!(
                "ALTER ROLE {role} WITH {} {} {} CONNECTION LIMIT {connection_limit} {until}",
                flag(*login, "LOGIN"),
                flag(*create_db, "CREATEDB"),
                flag(*create_role, "CREATEROLE"),
            )]
        }
        PgAdminChange::SetPassword { name, password } => vec![format!(
            "ALTER ROLE {} WITH {}",
            role_name(name)?,
            password_sql(password, masked)?
        )],
        PgAdminChange::DropRole { name, reassign_to } => {
            let role = role_name(name)?;
            let mut out = Vec::new();
            if let Some(to) = reassign_to.as_deref().filter(|t| !t.trim().is_empty()) {
                out.push(format!(
                    "REASSIGN OWNED BY {role} TO {}",
                    quote_ident(to.trim())
                ));
                out.push(format!("DROP OWNED BY {role}"));
            }
            out.push(format!("DROP ROLE {role}"));
            out
        }
        PgAdminChange::GrantRole { role, member } => vec![format!(
            "GRANT {} TO {}",
            quote_ident(role.trim()),
            role_name(member)?
        )],
        PgAdminChange::RevokeRole { role, member } => vec![format!(
            "REVOKE {} FROM {}",
            quote_ident(role.trim()),
            role_name(member)?
        )],
        PgAdminChange::Grant {
            role,
            target,
            privileges,
            grant_option,
        } => {
            if privileges.is_empty() {
                return Err(invalid("請勾選要給的權限"));
            }
            vec![format!(
                "GRANT {} ON {} TO {}{}",
                privilege_list(&target.kind, privileges)?,
                target_sql(s, target).await?,
                role_name(role)?,
                if *grant_option {
                    " WITH GRANT OPTION"
                } else {
                    ""
                }
            )]
        }
        PgAdminChange::Revoke {
            role,
            target,
            privileges,
        } => vec![format!(
            "REVOKE {} ON {} FROM {}",
            privilege_list(&target.kind, privileges)?,
            target_sql(s, target).await?,
            role_name(role)?
        )],
        PgAdminChange::DefaultPrivileges {
            role,
            schema,
            object,
            privileges,
            revoke,
            for_role,
        } => {
            let kind = match object.as_str() {
                "tables" => PgObjectKind::AllTables,
                "sequences" => PgObjectKind::AllSequences,
                "functions" => PgObjectKind::AllFunctions,
                _ => return Err(invalid("預設權限只支援資料表、序列與函式")),
            };
            let mut head = "ALTER DEFAULT PRIVILEGES".to_string();
            if let Some(f) = for_role.as_deref().filter(|f| !f.trim().is_empty()) {
                head.push_str(&format!(" FOR ROLE {}", quote_ident(f.trim())));
            }
            if let Some(sc) = schema.as_deref().filter(|v| !v.trim().is_empty()) {
                head.push_str(&format!(" IN SCHEMA {}", quote_ident(sc.trim())));
            }
            let privs = privilege_list(&kind, privileges)?;
            let what = object.to_ascii_uppercase();
            let role = role_name(role)?;
            vec![if *revoke {
                format!("{head} REVOKE {privs} ON {what} FROM {role}")
            } else {
                format!("{head} GRANT {privs} ON {what} TO {role}")
            }]
        }
        PgAdminChange::Cancel { pid } => vec![format!("SELECT pg_cancel_backend({pid})")],
        PgAdminChange::Terminate { pid } => vec![format!("SELECT pg_terminate_backend({pid})")],
    })
}

/// Runs `change` (in one transaction), returning what ran with passwords
/// masked. Not recorded, like MySQL account changes.
pub async fn apply(s: &PgSession, change: &PgAdminChange) -> Result<Vec<String>> {
    let shown = plan(s, change, true).await?;
    if let PgAdminChange::Cancel { pid } | PgAdminChange::Terminate { pid } = change {
        if *pid as u32 == s.info().connection_id {
            return Err(invalid("這是 MyDB Studio 自己的連線。"));
        }
        let func = if matches!(change, PgAdminChange::Cancel { .. }) {
            "pg_cancel_backend"
        } else {
            "pg_terminate_backend"
        };
        let ok: bool = s
            .query(&format!("SELECT {func}($1)"), &[pid])
            .await?
            .first()
            .is_some_and(|r| r.get::<_, Option<bool>>(0).unwrap_or(false));
        if !ok {
            return Err(invalid(
                "沒有成功：連線可能已經結束，或沒有權限（需要同一個角色，或 pg_signal_backend）。",
            ));
        }
        return Ok(shown);
    }
    let real = plan(s, change, false).await?;
    let out = s.run_in_transaction(&real).await;
    if let Some(e) = out.error {
        return Err(e);
    }
    Ok(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_lists() {
        let p = |k, v: &[&str]| {
            privilege_list(&k, &v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            p(PgObjectKind::Table, &["select", "insert"]).unwrap(),
            "SELECT, INSERT"
        );
        assert_eq!(p(PgObjectKind::Schema, &[]).unwrap(), "ALL PRIVILEGES");
        assert_eq!(p(PgObjectKind::Database, &["temp"]).unwrap(), "TEMPORARY");
        assert!(p(PgObjectKind::Schema, &["SELECT"]).is_err());
        assert!(p(PgObjectKind::Table, &["SELECT; DROP TABLE x"]).is_err());
    }

    #[test]
    fn passwords_are_hashed_or_masked() {
        assert_eq!(password_sql("pw", true).unwrap(), "PASSWORD '********'");
        let real = password_sql("pw", false).unwrap();
        assert!(real.starts_with("PASSWORD 'SCRAM-SHA-256$4096:"), "{real}");
        assert!(!real.contains("'pw'"));
        assert!(role_name("pg_monitor").is_err());
        assert_eq!(role_name("app\"x").unwrap(), "\"app\"\"x\"");
    }
}
