//! Server administration: status and variables, running queries, user
//! accounts and their privileges.
//!
//! Statements that change anything are built here from a structured
//! [`AdminChange`] rather than taken from the UI as text, so the preview the
//! user confirms and the SQL that runs come from the same code. Passwords are
//! masked in the preview and never leave this module otherwise.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::session::Session;

/// Everything the overview page needs, fetched in one go.
#[derive(Debug, Clone, Serialize)]
pub struct ServerOverview {
    pub version: String,
    pub current_user: String,
    /// `SHOW GLOBAL STATUS`, by variable name.
    pub status: BTreeMap<String, String>,
    /// `SHOW GLOBAL VARIABLES`, by variable name.
    pub variables: BTreeMap<String, String>,
}

/// One row of `SHOW FULL PROCESSLIST`.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub id: u64,
    pub user: String,
    pub host: String,
    pub db: Option<String>,
    pub command: String,
    pub time: u64,
    pub state: Option<String>,
    pub info: Option<String>,
}

/// A login account (roles are left out).
#[derive(Debug, Clone, Serialize)]
pub struct UserAccount {
    pub user: String,
    pub host: String,
    pub plugin: String,
    pub locked: bool,
}

/// One line of `SHOW GRANTS`, parsed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grant {
    /// Upper case, e.g. `SELECT`, `ALL PRIVILEGES`, `UPDATE (a, b)`.
    pub privileges: Vec<String>,
    /// Where the privileges apply; `None` when the line is something else,
    /// such as a role or a PROXY grant.
    pub target: Option<GrantTarget>,
    pub grant_option: bool,
    /// The line itself, with any password hash removed.
    pub text: String,
    /// Whether the privilege editor can change this grant: global, database
    /// or table level, without column lists or routines.
    pub editable: bool,
}

/// `*.*` (both `None`), `db.*` or `db.table`. Names are as the server
/// prints them, so wildcard escapes such as `shop\_%` round-trip.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GrantTarget {
    pub database: Option<String>,
    pub table: Option<String>,
}

/// The privileges of one account on one target, before and after an edit.
#[derive(Debug, Clone, Deserialize)]
pub struct PrivilegeEdit {
    pub target: GrantTarget,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub before_grant_option: bool,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub after_grant_option: bool,
}

/// A change the user asked for, turned into SQL by [`plan`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdminChange {
    CreateUser {
        user: String,
        host: String,
        password: String,
        #[serde(default)]
        privileges: Vec<PrivilegeEdit>,
    },
    SetPassword {
        user: String,
        host: String,
        password: String,
    },
    SetLocked {
        user: String,
        host: String,
        locked: bool,
    },
    DropUser {
        user: String,
        host: String,
    },
    Privileges {
        user: String,
        host: String,
        edits: Vec<PrivilegeEdit>,
    },
    Kill {
        id: u64,
        /// Stop only the running statement and keep the connection.
        query_only: bool,
    },
}

/// Shown instead of a password in previews.
pub const MASK: &str = "********";

/// How string literals must be written for the server's `sql_mode`.
#[derive(Debug, Clone, Copy)]
pub struct Dialect {
    /// False under `NO_BACKSLASH_ESCAPES`.
    pub backslash_escapes: bool,
}

pub async fn overview(session: &Session) -> Result<ServerOverview> {
    let pairs = |rows: crate::session::TextRows| {
        rows.rows
            .into_iter()
            .filter_map(|mut r| {
                let value = r.get_mut(1).and_then(Option::take).unwrap_or_default();
                Some((r.first()?.clone()?, value))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let status = pairs(session.query_text("SHOW GLOBAL STATUS").await?);
    let variables = pairs(session.query_text("SHOW GLOBAL VARIABLES").await?);
    let current_user = first_cell(session, "SELECT CURRENT_USER()").await?;
    Ok(ServerOverview {
        version: variables
            .get("version")
            .cloned()
            .unwrap_or_else(|| session.info().server_version.clone()),
        current_user,
        status,
        variables,
    })
}

pub async fn processes(session: &Session) -> Result<Vec<ProcessInfo>> {
    let rows = session.query_text("SHOW FULL PROCESSLIST").await?;
    let num = |v: &Option<String>| v.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0);
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    Ok(rows
        .rows
        .iter()
        .filter(|r| r.len() >= 8)
        .map(|r| ProcessInfo {
            id: num(&r[0]),
            user: text(&r[1]),
            host: text(&r[2]),
            db: r[3].clone(),
            command: text(&r[4]),
            time: num(&r[5]),
            state: r[6].clone().filter(|s| !s.is_empty()),
            info: r[7].clone(),
        })
        .collect())
}

/// Login accounts, sorted by user then host. Needs read access to the
/// `mysql` schema.
pub async fn users(session: &Session) -> Result<Vec<UserAccount>> {
    // MariaDB keeps roles in the same table; MySQL has no is_role column.
    let rows = match session
        .query_text("SELECT User, Host, plugin FROM mysql.user WHERE is_role <> 'Y'")
        .await
    {
        Ok(rows) => rows,
        Err(CoreError::Server { code: 1054, .. }) => {
            session
                .query_text("SELECT User, Host, plugin FROM mysql.user")
                .await?
        }
        Err(e) => return Err(e),
    };
    let mut users = Vec::new();
    for r in rows.rows {
        let user = r.first().cloned().flatten().unwrap_or_default();
        let host = r.get(1).cloned().flatten().unwrap_or_default();
        let create = first_cell(
            session,
            &format!("SHOW CREATE USER {}", account(&user, &host)),
        )
        .await
        .unwrap_or_default();
        users.push(UserAccount {
            plugin: r.get(2).cloned().flatten().unwrap_or_default(),
            locked: top_level_find(&create.to_ascii_uppercase(), " ACCOUNT LOCK").is_some(),
            user,
            host,
        });
    }
    users.sort_by(|a, b| (&a.user, &a.host).cmp(&(&b.user, &b.host)));
    Ok(users)
}

pub async fn grants(session: &Session, user: &str, host: &str) -> Result<Vec<Grant>> {
    let rows = session
        .query_text(&format!("SHOW GRANTS FOR {}", account(user, host)))
        .await?;
    Ok(rows
        .rows
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .map(|line| parse_grant(&line))
        .collect())
}

/// The string-literal rules of this connection.
pub async fn dialect(session: &Session) -> Result<Dialect> {
    let mode = first_cell(session, "SELECT @@SESSION.sql_mode").await?;
    Ok(Dialect {
        backslash_escapes: !mode.to_ascii_uppercase().contains("NO_BACKSLASH_ESCAPES"),
    })
}

/// The statements that carry out `change`, in order. With `mask`, passwords
/// are replaced by [`MASK`] for display.
pub fn plan(change: &AdminChange, dialect: Dialect, mask: bool) -> Result<Vec<String>> {
    let password = |p: &str| {
        if mask {
            format!("'{MASK}'")
        } else {
            string_literal(p, dialect)
        }
    };
    Ok(match change {
        AdminChange::CreateUser {
            user,
            host,
            password: pw,
            privileges,
        } => {
            if user.trim().is_empty() {
                return Err(invalid("請輸入使用者名稱"));
            }
            let acct = account(user, host);
            let mut out = vec![if pw.is_empty() {
                format!("CREATE USER {acct}")
            } else {
                format!("CREATE USER {acct} IDENTIFIED BY {}", password(pw))
            }];
            for edit in privileges {
                out.extend(privilege_sql(&acct, edit)?);
            }
            out
        }
        AdminChange::SetPassword {
            user,
            host,
            password: pw,
        } => vec![format!(
            "ALTER USER {} IDENTIFIED BY {}",
            account(user, host),
            password(pw)
        )],
        AdminChange::SetLocked { user, host, locked } => vec![format!(
            "ALTER USER {} ACCOUNT {}",
            account(user, host),
            if *locked { "LOCK" } else { "UNLOCK" }
        )],
        AdminChange::DropUser { user, host } => {
            vec![format!("DROP USER {}", account(user, host))]
        }
        AdminChange::Privileges { user, host, edits } => {
            let acct = account(user, host);
            let mut out = Vec::new();
            for edit in edits {
                out.extend(privilege_sql(&acct, edit)?);
            }
            out
        }
        AdminChange::Kill { id, query_only } => vec![format!(
            "KILL {}{id}",
            if *query_only { "QUERY " } else { "CONNECTION " }
        )],
    })
}

/// Runs `change` and returns the statements that ran (masked), or the error
/// along with how many had succeeded before it.
pub async fn apply(session: &Session, change: &AdminChange) -> Result<Vec<String>> {
    let dialect = dialect(session).await?;
    let statements = plan(change, dialect, false)?;
    let shown = plan(change, dialect, true)?;
    match session.run_each(&statements).await {
        (_, None) => Ok(shown),
        (done, Some(e)) => Err(match e {
            CoreError::Server {
                code,
                state,
                message,
            } => CoreError::Server {
                code,
                state,
                message: format!(
                    "{message}\n（第 {} 句失敗：{}；之前的 {done} 句已執行）",
                    done + 1,
                    shown[done]
                ),
            },
            other => other,
        }),
    }
}

/// `GRANT` and `REVOKE` statements that turn `before` into `after`.
pub fn privilege_sql(account: &str, edit: &PrivilegeEdit) -> Result<Vec<String>> {
    let before = privilege_set(&edit.before)?;
    let after = privilege_set(&edit.after)?;
    let on = target_sql(&edit.target);
    const ALL: &str = "ALL PRIVILEGES";

    let mut revoke: Vec<&str> = Vec::new();
    let mut grant: Vec<&str> = Vec::new();
    match (before.contains(ALL), after.contains(ALL)) {
        (true, true) => {}
        (false, true) => grant.push(ALL),
        (true, false) => {
            revoke.push(ALL);
            grant.extend(after.iter().map(String::as_str));
        }
        (false, false) => {
            revoke.extend(before.difference(&after).map(String::as_str));
            grant.extend(after.difference(&before).map(String::as_str));
        }
    }

    let mut out = Vec::new();
    if !revoke.is_empty() {
        out.push(format!(
            "REVOKE {} ON {on} FROM {account}",
            revoke.join(", ")
        ));
    }
    if edit.before_grant_option && !edit.after_grant_option {
        out.push(format!("REVOKE GRANT OPTION ON {on} FROM {account}"));
    }
    let add_option = edit.after_grant_option && !edit.before_grant_option;
    if !grant.is_empty() || add_option {
        let privs = if grant.is_empty() {
            "USAGE".to_string()
        } else {
            grant.join(", ")
        };
        let option = if add_option { " WITH GRANT OPTION" } else { "" };
        out.push(format!("GRANT {privs} ON {on} TO {account}{option}"));
    }
    Ok(out)
}

/// Normalizes privilege names: upper case, single spaces, `ALL` spelled out
/// and `USAGE` (no privilege) dropped. Anything that is not a plain
/// privilege name is refused, since it goes into the SQL as is.
fn privilege_set(names: &[String]) -> Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    for name in names {
        let name = name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_uppercase();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == ' ' || c == '_')
        {
            return Err(invalid(&format!("無法辨識的權限：{name}")));
        }
        match name.as_str() {
            "USAGE" => {}
            "ALL" => {
                set.insert("ALL PRIVILEGES".to_string());
            }
            _ => {
                set.insert(name);
            }
        }
    }
    Ok(set)
}

pub fn target_sql(target: &GrantTarget) -> String {
    match (&target.database, &target.table) {
        (None, _) => "*.*".to_string(),
        (Some(db), None) => format!("{}.*", ident(db)),
        (Some(db), Some(t)) => format!("{}.{}", ident(db), ident(t)),
    }
}

/// `` `user`@`host` ``. Backticks work for accounts on MySQL and MariaDB
/// and need no care about `sql_mode`.
pub fn account(user: &str, host: &str) -> String {
    format!("{}@{}", ident(user), ident(host))
}

fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn string_literal(s: &str, dialect: Dialect) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' if dialect.backslash_escapes => out.push_str("\\\\"),
            '\0' if dialect.backslash_escapes => out.push_str("\\0"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn invalid(message: &str) -> CoreError {
    CoreError::Config {
        message: message.to_string(),
    }
}

async fn first_cell(session: &Session, sql: &str) -> Result<String> {
    Ok(session
        .query_text(sql)
        .await?
        .rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten())
        .unwrap_or_default())
}

/// Parses one line of `SHOW GRANTS`. Lines it cannot read are kept with
/// `target: None` so the UI can still show them.
pub fn parse_grant(line: &str) -> Grant {
    let text = scrub_identified(line.trim());
    let other = |text: String| Grant {
        privileges: Vec::new(),
        target: None,
        grant_option: false,
        editable: false,
        text,
    };
    let upper = text.to_ascii_uppercase();
    if !upper.starts_with("GRANT ") {
        return other(text);
    }
    let body = &text[6..];
    let (Some(on), Some(to)) = (
        top_level_find(&body.to_ascii_uppercase(), " ON "),
        top_level_find(&body.to_ascii_uppercase(), " TO "),
    ) else {
        return other(text);
    };
    if to < on {
        return other(text);
    }
    let privileges: Vec<String> = split_top_level(&body[..on])
        .into_iter()
        .map(|p| {
            // Keep column lists as written; upper-case the name before them.
            match p.find('(') {
                Some(i) => format!("{} {}", p[..i].trim().to_ascii_uppercase(), p[i..].trim()),
                None => p.to_ascii_uppercase(),
            }
        })
        .collect();
    let object = body[on + 4..to].trim();
    let grant_option = top_level_find(&upper, " WITH GRANT OPTION").is_some();
    let target = parse_target(object);
    let editable = target.is_some() && privileges.iter().all(|p| !p.contains('('));
    Grant {
        privileges,
        target,
        grant_option,
        editable,
        text,
    }
}

/// `*.*`, `` `db`.* ``, `` db.`t` `` and the like. Routines, proxies and
/// anything else give `None`.
fn parse_target(object: &str) -> Option<GrantTarget> {
    let (first, rest) = read_name(object)?;
    let rest = rest.strip_prefix('.')?;
    let (second, rest) = read_name(rest)?;
    if !rest.trim().is_empty() {
        return None;
    }
    match (first, second) {
        (None, None) => Some(GrantTarget {
            database: None,
            table: None,
        }),
        (Some(db), table) => Some(GrantTarget {
            database: Some(db),
            table,
        }),
        (None, Some(_)) => None,
    }
}

/// Reads `*` (as `None`), a backticked name or a bare word.
fn read_name(s: &str) -> Option<(Option<String>, &str)> {
    if let Some(rest) = s.strip_prefix('*') {
        return Some((None, rest));
    }
    if let Some(inner) = s.strip_prefix('`') {
        let mut name = String::new();
        let mut chars = inner.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '`' {
                if chars.peek().is_some_and(|&(_, n)| n == '`') {
                    chars.next();
                    name.push('`');
                } else {
                    return Some((Some(name), &inner[i + 1..]));
                }
            } else {
                name.push(c);
            }
        }
        return None;
    }
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .unwrap_or(s.len());
    (end > 0).then(|| (Some(s[..end].to_string()), &s[end..]))
}

/// Removes ` IDENTIFIED BY PASSWORD '…'` and similar, which carry the
/// password hash.
fn scrub_identified(line: &str) -> String {
    let upper = line.to_ascii_uppercase();
    let Some(start) = top_level_find(&upper, " IDENTIFIED ") else {
        return line.to_string();
    };
    let tail = &upper[start + 1..];
    let end = [
        " REQUIRE ",
        " WITH ",
        " ACCOUNT ",
        " PASSWORD EXPIRE",
        " PASSWORD HISTORY",
        " PASSWORD REUSE",
        " FAILED_LOGIN",
    ]
    .iter()
    .filter_map(|k| top_level_find(tail, k))
    .min()
    .map(|i| start + 1 + i)
    .unwrap_or(line.len());
    format!("{}{}", &line[..start], &line[end..])
}

/// Byte offset of `needle` outside quotes and parentheses.
fn top_level_find(haystack: &str, needle: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let mut quote: Option<u8> = None;
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' && q == b'\'' {
                    i += 1;
                } else if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' | b'`' => quote = Some(b),
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                _ if depth == 0 && haystack[i..].starts_with(needle) => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Splits on commas outside quotes and parentheses, trimming each part.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest = s;
    while let Some(i) = top_level_find(rest, ",") {
        parts.push(rest[..i].trim().to_string());
        rest = &rest[i + 1..];
    }
    parts.push(rest.trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Dialect = Dialect {
        backslash_escapes: true,
    };

    fn target(db: Option<&str>, table: Option<&str>) -> GrantTarget {
        GrantTarget {
            database: db.map(String::from),
            table: table.map(String::from),
        }
    }

    #[test]
    fn parses_grant_lines() {
        let g = parse_grant(
            "GRANT ALL PRIVILEGES ON *.* TO `tester`@`%` IDENTIFIED BY PASSWORD '*14E65567ABDB5135D0CFD9A70B3032C179A49EE7'",
        );
        assert_eq!(g.privileges, ["ALL PRIVILEGES"]);
        assert_eq!(g.target, Some(target(None, None)));
        assert!(!g.grant_option && g.editable);
        assert_eq!(g.text, "GRANT ALL PRIVILEGES ON *.* TO `tester`@`%`");

        let g =
            parse_grant("GRANT SELECT, INSERT ON `shop\\_%`.* TO 'app'@'10.%' WITH GRANT OPTION");
        assert_eq!(g.privileges, ["SELECT", "INSERT"]);
        assert_eq!(g.target, Some(target(Some("shop\\_%"), None)));
        assert!(g.grant_option);

        let g =
            parse_grant("GRANT SELECT (`id`, name), UPDATE (name) ON `shop`.`users` TO `app`@`%`");
        assert_eq!(g.privileges, ["SELECT (`id`, name)", "UPDATE (name)"]);
        assert_eq!(g.target, Some(target(Some("shop"), Some("users"))));
        assert!(!g.editable);

        let g = parse_grant("GRANT EXECUTE ON PROCEDURE `shop`.`tidy` TO `app`@`%`");
        assert_eq!(g.target, None);
        let g = parse_grant("GRANT PROXY ON ''@'%' TO 'root'@'localhost' WITH GRANT OPTION");
        assert_eq!(g.target, None);
        let g = parse_grant("GRANT `reader` TO `app`@`%`");
        assert_eq!(g.target, None);
        assert_eq!(g.text, "GRANT `reader` TO `app`@`%`");

        let g = parse_grant(
            "GRANT USAGE ON *.* TO `x`@`%` IDENTIFIED VIA ed25519 USING 'abc' OR unix_socket WITH MAX_QUERIES_PER_HOUR 5",
        );
        assert_eq!(
            g.text,
            "GRANT USAGE ON *.* TO `x`@`%` WITH MAX_QUERIES_PER_HOUR 5"
        );
        let g = parse_grant("GRANT SELECT ON `a``b`.`t` TO `x`@`%`");
        assert_eq!(g.target, Some(target(Some("a`b"), Some("t"))));
    }

    #[test]
    fn plans_privilege_changes() {
        let acct = account("app", "%");
        let edit = |before: &[&str], after: &[&str], bo, ao| PrivilegeEdit {
            target: target(Some("shop"), None),
            before: before.iter().map(|s| s.to_string()).collect(),
            before_grant_option: bo,
            after: after.iter().map(|s| s.to_string()).collect(),
            after_grant_option: ao,
        };
        assert_eq!(
            privilege_sql(
                &acct,
                &edit(&["SELECT", "INSERT"], &["select", "UPDATE"], false, false)
            )
            .unwrap(),
            [
                "REVOKE INSERT ON `shop`.* FROM `app`@`%`",
                "GRANT UPDATE ON `shop`.* TO `app`@`%`",
            ]
        );
        assert_eq!(
            privilege_sql(&acct, &edit(&["ALL PRIVILEGES"], &["SELECT"], true, false)).unwrap(),
            [
                "REVOKE ALL PRIVILEGES ON `shop`.* FROM `app`@`%`",
                "REVOKE GRANT OPTION ON `shop`.* FROM `app`@`%`",
                "GRANT SELECT ON `shop`.* TO `app`@`%`",
            ]
        );
        assert_eq!(
            privilege_sql(&acct, &edit(&["SELECT"], &["SELECT", "ALL"], false, true)).unwrap(),
            ["GRANT ALL PRIVILEGES ON `shop`.* TO `app`@`%` WITH GRANT OPTION"]
        );
        assert_eq!(
            privilege_sql(&acct, &edit(&["SELECT"], &["SELECT", "USAGE"], false, true)).unwrap(),
            ["GRANT USAGE ON `shop`.* TO `app`@`%` WITH GRANT OPTION"]
        );
        assert!(privilege_sql(&acct, &edit(&[], &["SELECT; DROP"], false, false)).is_err());
        assert!(
            privilege_sql(&acct, &edit(&["SELECT"], &["SELECT"], false, false))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn plans_user_changes_with_masked_passwords() {
        let create = AdminChange::CreateUser {
            user: "o'neil".into(),
            host: "%".into(),
            password: "it's\\secret".into(),
            privileges: vec![PrivilegeEdit {
                target: target(None, None),
                before: vec![],
                before_grant_option: false,
                after: vec!["PROCESS".into()],
                after_grant_option: false,
            }],
        };
        assert_eq!(
            plan(&create, PLAIN, false).unwrap(),
            [
                "CREATE USER `o'neil`@`%` IDENTIFIED BY 'it''s\\\\secret'",
                "GRANT PROCESS ON *.* TO `o'neil`@`%`",
            ]
        );
        let shown = plan(&create, PLAIN, true).unwrap();
        assert_eq!(
            shown[0],
            "CREATE USER `o'neil`@`%` IDENTIFIED BY '********'"
        );
        assert!(!shown.join("").contains("secret"));
        let raw = Dialect {
            backslash_escapes: false,
        };
        assert_eq!(
            plan(
                &AdminChange::SetPassword {
                    user: "a".into(),
                    host: "localhost".into(),
                    password: "x\\y".into(),
                },
                raw,
                false
            )
            .unwrap(),
            ["ALTER USER `a`@`localhost` IDENTIFIED BY 'x\\y'"]
        );
        assert_eq!(
            plan(
                &AdminChange::Kill {
                    id: 42,
                    query_only: true
                },
                PLAIN,
                false
            )
            .unwrap(),
            ["KILL QUERY 42"]
        );
        let empty = AdminChange::CreateUser {
            user: " ".into(),
            host: "%".into(),
            password: String::new(),
            privileges: vec![],
        };
        assert!(plan(&empty, PLAIN, false).is_err());
    }

    #[test]
    fn changes_come_from_json() {
        let change: AdminChange =
            serde_json::from_str(r#"{"kind":"set_locked","user":"app","host":"%","locked":true}"#)
                .unwrap();
        assert_eq!(
            plan(&change, PLAIN, false).unwrap(),
            ["ALTER USER `app`@`%` ACCOUNT LOCK"]
        );
    }
}
