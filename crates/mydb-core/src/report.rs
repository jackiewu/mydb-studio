//! Reports generated from execution records.

use serde::{Deserialize, Serialize};

use crate::records::{ExecutionRecord, RecordedStatement};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportFormat {
    Markdown,
    Html,
    /// One row per statement, for spreadsheets.
    Csv,
}

impl ReportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ReportFormat::Markdown => "md",
            ReportFormat::Html => "html",
            ReportFormat::Csv => "csv",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportOptions {
    pub format: ReportFormat,
    #[serde(default)]
    pub title: Option<String>,
    /// Include each result set's preview rows.
    #[serde(default = "yes")]
    pub include_results: bool,
    /// Rows of each preview to include.
    #[serde(default = "default_rows")]
    pub max_result_rows: usize,
    /// Leave out server addresses and accounts; see [`redact`].
    #[serde(default = "yes")]
    pub hide_connection: bool,
}

fn yes() -> bool {
    true
}

fn default_rows() -> usize {
    20
}

/// Renders `records` (in the given order) as a report.
pub fn render(records: &[ExecutionRecord], opts: &ReportOptions, generated_at: &str) -> String {
    match opts.format {
        ReportFormat::Markdown => markdown(records, opts, generated_at),
        ReportFormat::Html => html(records, opts, generated_at),
        ReportFormat::Csv => csv(records),
    }
}

fn title(opts: &ReportOptions) -> &str {
    opts.title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("SQL 執行報表")
}

fn ms(v: f64) -> String {
    if v < 1000.0 {
        format!("{v:.1} ms")
    } else {
        format!("{:.2} s", v / 1000.0)
    }
}

fn status(r: &ExecutionRecord) -> String {
    match (&r.error_code, &r.error_message) {
        (_, None) => "成功".into(),
        (Some(code), Some(_)) => format!("錯誤 {code}"),
        (None, Some(_)) => "錯誤".into(),
    }
}

/// Rows touched by a run: returned rows for queries, affected rows for
/// changes (the server reports 0 affected rows for a SELECT).
fn rows(r: &ExecutionRecord) -> String {
    match (r.returned_rows, r.affected_rows) {
        (0, 0) => "0".into(),
        (n, 0) => format!("回傳 {n}"),
        (0, n) => format!("影響 {n}"),
        (q, a) => format!("影響 {a}，回傳 {q}"),
    }
}

/// Server address (when known) and version.
fn server_info(r: &ExecutionRecord) -> String {
    if r.server.is_empty() {
        r.server_version.clone()
    } else {
        format!("{}，{}", r.server, r.server_version)
    }
}

fn target(r: &ExecutionRecord) -> String {
    match &r.database {
        Some(db) if !db.is_empty() => format!("{} / {db}", r.connection_name),
        _ => r.connection_name.clone(),
    }
}

/// `YYYY-MM-DD HH:MM:SS` in local time, falling back to the raw value.
fn when(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

fn cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".into(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn summary_line(records: &[ExecutionRecord]) -> String {
    let failed = records.iter().filter(|r| !r.succeeded()).count();
    let total_ms: f64 = records.iter().map(|r| r.duration_ms).sum();
    let affected: u64 = records.iter().map(|r| r.affected_rows).sum();
    format!(
        "共 {} 筆紀錄（{} 筆失敗），總耗時 {}，共影響 {} 筆資料",
        records.len(),
        failed,
        ms(total_ms),
        affected
    )
}

// ---------- Markdown ----------

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn md_statement_table(out: &mut String, stmts: &[RecordedStatement]) {
    out.push_str("| # | 陳述式 | 耗時 | 影響筆數 | 回傳筆數 | 警告 |\n|---|---|---|---|---|---|\n");
    for (i, s) in stmts.iter().enumerate() {
        let first_line: String = s
            .sql
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} |\n",
            i + 1,
            md_cell(&first_line).replace('`', "'"),
            ms(s.duration_ms),
            s.affected_rows,
            s.returned_rows.map_or("—".into(), |n| n.to_string()),
            s.warnings
        ));
    }
}

fn markdown(records: &[ExecutionRecord], opts: &ReportOptions, generated_at: &str) -> String {
    let mut out = format!(
        "# {}\n\n產生時間：{}  \n{}\n\n",
        title(opts),
        when(generated_at),
        summary_line(records)
    );
    out.push_str("| 紀錄 | 時間 | 連線 / 資料庫 | 陳述式 | 耗時 | 筆數 | 狀態 |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");
    for r in records {
        out.push_str(&format!(
            "| #{} | {} | {} | {} | {} | {} | {} |\n",
            r.id,
            when(&r.started_at),
            md_cell(&target(r)),
            r.statement_count,
            ms(r.duration_ms),
            rows(r),
            status(r)
        ));
    }

    for r in records {
        out.push_str(&format!(
            "\n---\n\n## #{} · {}\n\n- 連線：{}（{}）\n- 耗時：{}，影響 {} 筆，回傳 {} 筆\n",
            r.id,
            when(&r.started_at),
            target(r),
            server_info(r),
            ms(r.duration_ms),
            r.affected_rows,
            r.returned_rows
        ));
        if !r.note.trim().is_empty() {
            out.push_str(&format!("- 備註：{}\n", r.note.trim()));
        }
        out.push_str(&format!("\n```sql\n{}\n```\n\n", r.sql));
        if !r.statements.is_empty() {
            md_statement_table(&mut out, &r.statements);
        }
        if let Some(msg) = &r.error_message {
            out.push_str(&format!(
                "\n> **{}**：{}\n",
                status(r),
                msg.replace('\n', " ")
            ));
            if let Some(stmt) = &r.failed_statement {
                out.push_str(&format!(
                    ">\n> 出錯的陳述式：`{}`\n",
                    stmt.replace('`', "'")
                ));
            }
        }
        if opts.include_results {
            for (i, s) in r.statements.iter().enumerate() {
                if s.columns.is_empty() {
                    continue;
                }
                out.push_str(&format!("\n**結果 {}**", i + 1));
                let shown = s.preview.len().min(opts.max_result_rows);
                if let Some(total) = s.returned_rows
                    && total as usize > shown
                {
                    out.push_str(&format!("（顯示前 {shown} 筆，共 {total} 筆）"));
                }
                out.push_str("\n\n| ");
                out.push_str(
                    &s.columns
                        .iter()
                        .map(|c| md_cell(c))
                        .collect::<Vec<_>>()
                        .join(" | "),
                );
                out.push_str(" |\n|");
                out.push_str(&"---|".repeat(s.columns.len()));
                out.push('\n');
                for row in s.preview.iter().take(opts.max_result_rows) {
                    out.push_str("| ");
                    out.push_str(
                        &row.iter()
                            .map(|v| md_cell(&cell(v)))
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                    out.push_str(" |\n");
                }
            }
        }
    }
    out
}

// ---------- HTML ----------

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn html(records: &[ExecutionRecord], opts: &ReportOptions, generated_at: &str) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<h1>{}</h1><p class=\"meta\">產生時間：{}<br>{}</p>",
        esc(title(opts)),
        esc(&when(generated_at)),
        esc(&summary_line(records))
    ));
    body.push_str("<table><thead><tr><th>紀錄</th><th>時間</th><th>連線 / 資料庫</th><th>陳述式</th><th>耗時</th><th>筆數</th><th>狀態</th></tr></thead><tbody>");
    for r in records {
        body.push_str(&format!(
            "<tr><td><a href=\"#r{id}\">#{id}</a></td><td>{}</td><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"{}\">{}</td></tr>",
            esc(&when(&r.started_at)),
            esc(&target(r)),
            r.statement_count,
            ms(r.duration_ms),
            esc(&rows(r)),
            if r.succeeded() { "ok" } else { "bad" },
            esc(&status(r)),
            id = r.id,
        ));
    }
    body.push_str("</tbody></table>");

    for r in records {
        body.push_str(&format!(
            "<section id=\"r{}\"><h2>#{} · {}</h2><p class=\"meta\">{}（{}）· 耗時 {} · 影響 {} 筆 · 回傳 {} 筆</p>",
            r.id,
            r.id,
            esc(&when(&r.started_at)),
            esc(&target(r)),
            esc(&server_info(r)),
            ms(r.duration_ms),
            r.affected_rows,
            r.returned_rows
        ));
        if !r.note.trim().is_empty() {
            body.push_str(&format!("<p class=\"note\">{}</p>", esc(r.note.trim())));
        }
        body.push_str(&format!("<pre>{}</pre>", esc(&r.sql)));
        if !r.statements.is_empty() {
            body.push_str("<table><thead><tr><th>#</th><th>陳述式</th><th>耗時</th><th>影響筆數</th><th>回傳筆數</th><th>警告</th></tr></thead><tbody>");
            for (i, s) in r.statements.iter().enumerate() {
                body.push_str(&format!(
                    "<tr><td class=\"n\">{}</td><td><code>{}</code></td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>",
                    i + 1,
                    esc(s.sql.lines().next().unwrap_or("")),
                    ms(s.duration_ms),
                    s.affected_rows,
                    s.returned_rows.map_or("—".into(), |n| n.to_string()),
                    s.warnings
                ));
            }
            body.push_str("</tbody></table>");
        }
        if let Some(msg) = &r.error_message {
            body.push_str(&format!(
                "<div class=\"error\"><strong>{}</strong>：{}",
                esc(&status(r)),
                esc(msg)
            ));
            if let Some(stmt) = &r.failed_statement {
                body.push_str(&format!("<pre>{}</pre>", esc(stmt)));
            }
            body.push_str("</div>");
        }
        if opts.include_results {
            for (i, s) in r.statements.iter().enumerate() {
                if s.columns.is_empty() {
                    continue;
                }
                let shown = s.preview.len().min(opts.max_result_rows);
                let more = match s.returned_rows {
                    Some(total) if total as usize > shown => {
                        format!("（顯示前 {shown} 筆，共 {total} 筆）")
                    }
                    _ => String::new(),
                };
                body.push_str(&format!(
                    "<h3>結果 {}{}</h3><table class=\"data\"><thead><tr>",
                    i + 1,
                    more
                ));
                for c in &s.columns {
                    body.push_str(&format!("<th>{}</th>", esc(c)));
                }
                body.push_str("</tr></thead><tbody>");
                for row in s.preview.iter().take(opts.max_result_rows) {
                    body.push_str("<tr>");
                    for v in row {
                        if v.is_null() {
                            body.push_str("<td class=\"null\">NULL</td>");
                        } else {
                            body.push_str(&format!("<td>{}</td>", esc(&cell(v))));
                        }
                    }
                    body.push_str("</tr>");
                }
                body.push_str("</tbody></table>");
            }
        }
        body.push_str("</section>");
    }

    format!(
        r#"<!doctype html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
body {{ font-family: -apple-system, "Segoe UI", "Noto Sans TC", "PingFang TC", "Microsoft JhengHei", sans-serif; color: #1d2330; max-width: 1100px; margin: 32px auto; padding: 0 16px; font-size: 14px; }}
h1 {{ font-size: 24px; }} h2 {{ font-size: 18px; margin-top: 36px; }} h3 {{ font-size: 14px; }}
.meta {{ color: #6b7380; }}
table {{ border-collapse: collapse; margin: 8px 0; }}
th, td {{ border: 1px solid #dcdfe4; padding: 4px 8px; text-align: left; vertical-align: top; }}
th {{ background: #f1f3f5; }}
td.n {{ text-align: right; font-variant-numeric: tabular-nums; }}
td.ok {{ color: #1e8a4c; }} td.bad {{ color: #c43c3c; }}
td.null {{ color: #8b93a1; font-style: italic; }}
table.data td {{ font-family: ui-monospace, Menlo, Consolas, monospace; font-size: 12px; }}
pre {{ background: #f6f7f9; padding: 10px 12px; border-radius: 6px; white-space: pre-wrap; font-size: 12px; }}
code {{ font-size: 12px; }}
.note {{ border-left: 3px solid #2f6fde; padding-left: 10px; }}
.error {{ border-left: 3px solid #c43c3c; padding: 6px 10px; background: #fdf3f3; margin: 8px 0; }}
section {{ border-top: 1px solid #dcdfe4; }}
@media print {{
  body {{ margin: 0; max-width: none; font-size: 12px; }}
  section {{ break-before: auto; }}
  h2, h3 {{ break-after: avoid; }}
  tr, pre, .error {{ break-inside: avoid; }}
  a {{ color: inherit; text-decoration: none; }}
}}
</style>
</head>
<body>
{body}
</body>
</html>
"#,
        title = esc(title(opts)),
    )
}

// ---------- Hiding connection details ----------

/// What one saved connection could reveal in a report.
#[derive(Debug, Clone, Default)]
pub struct ConnectionSecrets {
    pub id: String,
    /// The connection's current name, used in place of the recorded one.
    pub name: String,
    /// Database and SSH hosts.
    pub hosts: Vec<String>,
    /// Database and SSH accounts.
    pub users: Vec<String>,
}

const HIDDEN: &str = "***";

/// Removes where records ran before they go into a report:
///
/// - the server address is dropped;
/// - the connection is shown by its current name, or as 「連線 N」 when that
///   name is itself an address such as `user@host`;
/// - hosts, `user@` accounts and IPv4 addresses in SQL, errors and notes
///   become `***`.
///
/// Result rows are left alone: they are data the user chose to include.
pub fn redact(records: &mut [ExecutionRecord], known: &[ConnectionSecrets]) {
    let mut aliases: Vec<String> = Vec::new();
    for r in records.iter_mut() {
        let profile = known
            .iter()
            .find(|k| Some(&k.id) == r.connection_id.as_ref());
        let mut hosts: Vec<String> = profile.map(|p| p.hosts.clone()).unwrap_or_default();
        let mut users: Vec<String> = profile.map(|p| p.users.clone()).unwrap_or_default();
        // The recorded `user@host:port`, which also covers deleted connections.
        if let Some((user, rest)) = r.server.rsplit_once('@') {
            users.push(user.to_string());
            hosts.push(rest.rsplit_once(':').map_or(rest, |(h, _)| h).to_string());
        }
        hosts.retain(|h| !h.trim().is_empty());
        users.retain(|u| !u.trim().is_empty());

        let name = profile.map_or(r.connection_name.as_str(), |p| p.name.as_str());
        r.connection_name = if name.trim().is_empty()
            || name.contains('@')
            || hosts.iter().any(|h| name.contains(h.as_str()))
        {
            let key = r
                .connection_id
                .clone()
                .unwrap_or_else(|| r.connection_name.clone());
            let n = match aliases.iter().position(|a| *a == key) {
                Some(i) => i + 1,
                None => {
                    aliases.push(key);
                    aliases.len()
                }
            };
            format!("連線 {n}")
        } else {
            name.to_string()
        };
        r.server.clear();

        let hide = |text: &str| hide_in(text, &hosts, &users);
        r.sql = hide(&r.sql);
        r.note = hide(&r.note);
        r.error_message = r.error_message.as_deref().map(|m| hide_ipv4(&hide(m)));
        r.failed_statement = r.failed_statement.as_deref().map(hide);
        for s in &mut r.statements {
            s.sql = hide(&s.sql);
            s.info = hide_ipv4(&hide(&s.info));
        }
    }
}

fn hide_in(text: &str, hosts: &[String], users: &[String]) -> String {
    let mut out = text.to_string();
    // Longest first, so `db.example.com` goes before `example.com`.
    let mut hosts: Vec<&String> = hosts.iter().collect();
    hosts.sort_by_key(|h| std::cmp::Reverse(h.len()));
    for h in hosts {
        out = replace_whole(&out, h, HIDDEN, true);
    }
    for u in users {
        for q in ["", "'", "`", "\""] {
            out = replace_whole(
                &out,
                &format!("{q}{u}{q}@"),
                &format!("{q}{HIDDEN}{q}@"),
                false,
            );
        }
    }
    out
}

/// Replaces `needle` where it is not part of a longer name, so a host
/// called `db` leaves `db_name` alone.
fn replace_whole(text: &str, needle: &str, with: &str, check_after: bool) -> String {
    let name = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '$');
    let mut out = String::with_capacity(text.len());
    let mut rest = 0;
    for (at, _) in text.match_indices(needle) {
        if at < rest {
            continue;
        }
        let end = at + needle.len();
        let before = text[..at].chars().next_back();
        let after = text[end..].chars().next();
        let first = needle.chars().next();
        if (first.is_some_and(name) && before.is_some_and(name))
            || (check_after && after.is_some_and(name))
        {
            continue;
        }
        out.push_str(&text[rest..at]);
        out.push_str(with);
        rest = end;
    }
    out.push_str(&text[rest..]);
    out
}

/// Replaces dotted IPv4 addresses, such as the client address in
/// "Access denied for user 'x'@'10.0.0.5'".
fn hide_ipv4(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        let starts = b[i].is_ascii_digit()
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.'));
        if starts && let Some(len) = ipv4_len(&b[i..]) {
            out.push_str(&text[copied..i]);
            out.push_str(HIDDEN);
            i += len;
            copied = i;
            continue;
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// Length of the IPv4 address at the start of `b`, if there is one.
fn ipv4_len(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    for part in 0..4 {
        if part > 0 {
            if b.get(i) != Some(&b'.') {
                return None;
            }
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() && i - start < 3 {
            i += 1;
        }
        if i == start
            || std::str::from_utf8(&b[start..i])
                .ok()?
                .parse::<u8>()
                .is_err()
        {
            return None;
        }
    }
    let next = b.get(i);
    if next.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'.') {
        return None;
    }
    Some(i)
}

// ---------- CSV ----------

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv(records: &[ExecutionRecord]) -> String {
    // UTF-8 BOM so Excel opens Chinese text correctly.
    let mut out = String::from("\u{feff}");
    out.push_str("record_id,started_at,connection,database,statement_no,statement,duration_ms,affected_rows,returned_rows,warnings,error_code,error_message,note\r\n");
    for r in records {
        let mut rows: Vec<[String; 7]> = r
            .statements
            .iter()
            .enumerate()
            .map(|(i, s)| {
                [
                    (i + 1).to_string(),
                    s.sql.clone(),
                    format!("{:.3}", s.duration_ms),
                    s.affected_rows.to_string(),
                    s.returned_rows.map_or(String::new(), |n| n.to_string()),
                    s.warnings.to_string(),
                    String::new(),
                ]
            })
            .collect();
        if let Some(stmt) = &r.failed_statement {
            rows.push([
                (rows.len() + 1).to_string(),
                stmt.clone(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                "error".into(),
            ]);
        }
        for row in rows {
            let failed = row[6] == "error";
            let fields = [
                r.id.to_string(),
                r.started_at.clone(),
                r.connection_name.clone(),
                r.database.clone().unwrap_or_default(),
                row[0].clone(),
                row[1].clone(),
                row[2].clone(),
                row[3].clone(),
                row[4].clone(),
                row[5].clone(),
                if failed {
                    r.error_code.map_or(String::new(), |c| c.to_string())
                } else {
                    String::new()
                },
                if failed {
                    r.error_message.clone().unwrap_or_default()
                } else {
                    String::new()
                },
                r.note.clone(),
            ];
            out.push_str(
                &fields
                    .iter()
                    .map(|f| csv_field(f))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push_str("\r\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hiding_connection_details() {
        let mut records = sample();
        records[0].server = "dev@34.150.28.17:3306".into();
        records[0].connection_name = "dev@34.150.28.17".into();
        records[0].sql = "SELECT db_name FROM t WHERE host = '34.150.28.17';".into();
        records[0].error_message =
            Some("Access denied for user 'dev'@'10.1.2.3' (using password: YES)".into());
        records[1].connection_id = Some("c2".into());
        records[1].connection_name = "old name".into();
        let known = vec![
            ConnectionSecrets {
                id: "c1".into(),
                name: "dev@34.150.28.17".into(),
                hosts: vec!["34.150.28.17".into(), "db".into()],
                users: vec!["dev".into()],
            },
            ConnectionSecrets {
                id: "c2".into(),
                name: "fgpay 正式".into(),
                hosts: vec!["db.internal".into()],
                users: vec!["app".into()],
            },
        ];
        redact(&mut records, &known);
        let r = &records[0];
        assert_eq!(r.connection_name, "連線 1");
        assert_eq!(r.server, "");
        assert_eq!(r.sql, "SELECT db_name FROM t WHERE host = '***';");
        assert_eq!(
            r.error_message.as_deref(),
            Some("Access denied for user '***'@'***' (using password: YES)")
        );
        assert_eq!(records[1].connection_name, "fgpay 正式");

        let mut o = opts(ReportFormat::Markdown);
        o.hide_connection = true;
        let md = render(&records, &o, "2026-10-24T03:00:00+00:00");
        assert!(!md.contains("34.150"), "{md}");
        assert!(!md.contains("dev@"), "{md}");
        assert!(md.contains("（10.11.6-MariaDB）"), "{md}");
    }

    #[test]
    fn ipv4_only_whole_addresses() {
        assert_eq!(
            hide_ipv4("from 10.0.0.5 and v1.2.3.4"),
            "from *** and v1.2.3.4"
        );
        assert_eq!(
            hide_ipv4("1.2.3.4.5 999.1.1.1 1.2.3"),
            "1.2.3.4.5 999.1.1.1 1.2.3"
        );
    }

    #[test]
    fn row_counts_read_by_kind_of_statement() {
        let mut r = sample().remove(0);
        for (returned, affected, text) in [
            (0, 0, "0"),
            (12, 0, "回傳 12"),
            (0, 3, "影響 3"),
            (12, 3, "影響 3，回傳 12"),
        ] {
            r.returned_rows = returned;
            r.affected_rows = affected;
            assert_eq!(rows(&r), text);
        }
    }

    fn sample() -> Vec<ExecutionRecord> {
        let ok = ExecutionRecord {
            id: 1,
            connection_id: Some("c1".into()),
            connection_name: "prod".into(),
            server: "app@db:3306".into(),
            server_version: "10.11.6-MariaDB".into(),
            database: Some("shop".into()),
            started_at: "2026-10-24T03:00:00+00:00".into(),
            duration_ms: 12.5,
            sql: "UPDATE orders SET note = 'a|b' WHERE id = 1;\nSELECT id, note FROM orders;"
                .into(),
            statements: vec![
                RecordedStatement {
                    sql: "UPDATE orders SET note = 'a|b' WHERE id = 1".into(),
                    duration_ms: 2.0,
                    affected_rows: 1,
                    returned_rows: None,
                    warnings: 0,
                    info: String::new(),
                    columns: vec![],
                    preview: vec![],
                },
                RecordedStatement {
                    sql: "SELECT id, note FROM orders".into(),
                    duration_ms: 10.5,
                    affected_rows: 0,
                    returned_rows: Some(2),
                    warnings: 0,
                    info: String::new(),
                    columns: vec!["id".into(), "note".into()],
                    preview: vec![
                        vec![json!("1"), json!("a|b")],
                        vec![json!("2"), json!(null)],
                    ],
                },
            ],
            statement_count: 2,
            affected_rows: 1,
            returned_rows: 2,
            error_code: None,
            error_message: None,
            failed_statement: None,
            note: "每月對帳".into(),
        };
        let failed = ExecutionRecord {
            id: 2,
            started_at: "2026-10-24T04:00:00+00:00".into(),
            sql: "UPDATE orders SET x = 1".into(),
            statements: vec![],
            statement_count: 1,
            affected_rows: 0,
            returned_rows: 0,
            error_code: Some(1175),
            error_message: Some("You are using safe update mode".into()),
            failed_statement: Some("UPDATE orders SET x = 1".into()),
            note: String::new(),
            ..ok.clone()
        };
        vec![ok, failed]
    }

    fn opts(format: ReportFormat) -> ReportOptions {
        ReportOptions {
            format,
            title: Some("月報".into()),
            include_results: true,
            max_result_rows: 20,
            hide_connection: false,
        }
    }

    #[test]
    fn markdown_has_summary_details_and_escaped_cells() {
        let md = render(
            &sample(),
            &opts(ReportFormat::Markdown),
            "2026-10-24T05:00:00+00:00",
        );
        assert!(md.starts_with("# 月報\n"));
        assert!(md.contains("共 2 筆紀錄（1 筆失敗）"));
        assert!(md.contains("| 錯誤 1175 |"));
        assert!(md.contains("- 備註：每月對帳"));
        assert!(md.contains("| 1 | a\\|b |"));
        assert!(md.contains("| 2 | NULL |"));
        assert!(md.contains("出錯的陳述式：`UPDATE orders SET x = 1`"));
    }

    #[test]
    fn html_is_escaped_and_self_contained() {
        let mut recs = sample();
        recs[0].note = "<script>alert(1)</script>".into();
        let html = render(
            &recs,
            &opts(ReportFormat::Html),
            "2026-10-24T05:00:00+00:00",
        );
        assert!(html.starts_with("<!doctype html>"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("<td class=\"null\">NULL</td>"));
    }

    #[test]
    fn results_can_be_left_out() {
        let mut o = opts(ReportFormat::Markdown);
        o.include_results = false;
        let md = render(&sample(), &o, "2026-10-24T05:00:00+00:00");
        assert!(!md.contains("**結果"));
    }

    #[test]
    fn csv_has_one_row_per_statement_and_quotes() {
        let csv = render(&sample(), &opts(ReportFormat::Csv), "x");
        let lines: Vec<_> = csv
            .trim_start_matches('\u{feff}')
            .split("\r\n")
            .filter(|l| !l.is_empty())
            .collect();
        // header + 2 statements + 1 failed statement
        assert_eq!(lines.len(), 4);
        assert!(lines[1].starts_with(
            "1,2026-10-24T03:00:00+00:00,prod,shop,1,UPDATE orders SET note = 'a|b' WHERE id = 1,"
        ));
        assert!(lines[3].contains(",1175,You are using safe update mode,"));
    }
}
