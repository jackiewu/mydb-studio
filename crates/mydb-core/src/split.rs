//! Splits editor text into statements, the way the `mysql` CLI does.
//!
//! Understands quotes, backticks, the three comment styles and the
//! `DELIMITER` command, so stored routine bodies can be pasted as-is.

/// Splits `sql` into trimmed statements without their delimiter. Statements
/// that contain only comments are dropped.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut delimiter = ";".to_string();
    let mut current = String::new();
    let mut has_code = false;
    let mut chars = sql.char_indices().peekable();
    let mut at_line_start = true;

    while let Some((i, c)) = chars.next() {
        // `DELIMITER xx` is a client command and only valid at the start of a line.
        if at_line_start && !has_code {
            let rest = &sql[i..];
            // Compared as bytes: slicing the str at 10 could split a character.
            if rest.len() >= 10 && rest.as_bytes()[..10].eq_ignore_ascii_case(b"delimiter ") {
                let line_end = rest.find('\n').unwrap_or(rest.len());
                let new_delim = rest[10..line_end].trim();
                if !new_delim.is_empty() {
                    delimiter = new_delim.to_string();
                }
                current.clear();
                while chars.peek().is_some_and(|(j, _)| *j < i + line_end) {
                    chars.next();
                }
                at_line_start = true;
                continue;
            }
        }
        at_line_start = c == '\n';

        match c {
            '\'' | '"' | '`' => {
                has_code = true;
                current.push(c);
                while let Some((_, d)) = chars.next() {
                    current.push(d);
                    if d == '\\' && c != '`' {
                        if let Some((_, e)) = chars.next() {
                            current.push(e);
                        }
                    } else if d == c {
                        // A doubled quote is an escaped quote.
                        if chars.peek().is_some_and(|(_, n)| *n == c) {
                            current.push(chars.next().unwrap().1);
                        } else {
                            break;
                        }
                    }
                }
            }
            '#' => skip_line(&mut chars, &mut current, &mut at_line_start),
            '-' if sql[i..].starts_with("--")
                && sql[i + 2..]
                    .chars()
                    .next()
                    .is_none_or(|n| n.is_whitespace()) =>
            {
                skip_line(&mut chars, &mut current, &mut at_line_start)
            }
            '/' if sql[i..].starts_with("/*") => {
                // `/*! ... */` is executable on MySQL/MariaDB, so it counts as code.
                if sql[i..].starts_with("/*!") || sql[i..].starts_with("/*M!") {
                    has_code = true;
                }
                current.push(c);
                let mut prev = '\0';
                for (_, d) in chars.by_ref() {
                    current.push(d);
                    if prev == '*' && d == '/' {
                        break;
                    }
                    prev = d;
                }
            }
            _ => {
                current.push(c);
                if !c.is_whitespace() {
                    has_code = true;
                }
                if current.ends_with(delimiter.as_str()) {
                    current.truncate(current.len() - delimiter.len());
                    push_statement(&mut out, &mut current, &mut has_code);
                }
            }
        }
    }
    push_statement(&mut out, &mut current, &mut has_code);
    out
}

/// Splits PostgreSQL text into statements, the way `psql` does: `;` ends a
/// statement except inside quotes, `"identifiers"`, `$tag$ … $tag$` bodies
/// and comments (`--` and nestable `/* */`). There is no `DELIMITER`, and
/// `#` is an operator, not a comment.
pub fn split_statements_pg(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut has_code = false;
    let b = sql.as_bytes();
    let mut i = 0;
    // Parentheses: a `;` inside them (e.g. in CREATE RULE) doesn't end it.
    let mut depth = 0i32;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\'' | b'"' => {
                // E'…' strings take backslash escapes.
                let escapes = c == b'\''
                    && i > 0
                    && (b[i - 1] == b'E' || b[i - 1] == b'e')
                    && (i < 2 || !is_ident_byte(b[i - 2]));
                let start = i;
                i += 1;
                while i < b.len() {
                    if escapes && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == c {
                        if b.get(i + 1) == Some(&c) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let end = i.min(b.len());
                current.push_str(&sql[start..end]);
                has_code = true;
                continue;
            }
            b'$' if !(i > 0 && is_ident_byte(b[i - 1])) => {
                if let Some(tag_len) = dollar_tag(&b[i..]) {
                    let tag = &sql[i..i + tag_len];
                    let body_start = i + tag_len;
                    let end = sql[body_start..]
                        .find(tag)
                        .map_or(b.len(), |p| body_start + p + tag_len);
                    current.push_str(&sql[i..end]);
                    has_code = true;
                    i = end;
                    continue;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                let end = sql[i..].find('\n').map_or(b.len(), |p| i + p);
                // Comments before the statement are dropped, like the
                // MySQL splitter does; later ones stay with the statement.
                if has_code {
                    current.push_str(&sql[i..end]);
                }
                i = end;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut level = 0;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        level += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        level -= 1;
                        i += 2;
                        if level == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                current.push_str(&sql[start..i.min(b.len())]);
                continue;
            }
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            b';' if depth == 0 => {
                push_statement(&mut out, &mut current, &mut has_code);
                i += 1;
                continue;
            }
            _ => {}
        }
        // Copy one whole character.
        let len = utf8_len(c);
        let ch = &sql[i..(i + len).min(b.len())];
        if !ch.trim().is_empty() {
            has_code = true;
        }
        current.push_str(ch);
        i += len;
    }
    push_statement(&mut out, &mut current, &mut has_code);
    out
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Length of a `$tag$` (or `$$`) at the start of `b`, if there is one.
fn dollar_tag(b: &[u8]) -> Option<usize> {
    let mut j = 1;
    while j < b.len() && b[j] != b'$' {
        let ok = if j == 1 {
            b[j].is_ascii_alphabetic() || b[j] == b'_' || b[j] >= 0x80
        } else {
            is_ident_byte(b[j])
        };
        if !ok {
            return None;
        }
        j += 1;
    }
    (j < b.len()).then_some(j + 1)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

fn skip_line(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    current: &mut String,
    at_line_start: &mut bool,
) {
    for (_, d) in chars.by_ref() {
        if d == '\n' {
            current.push('\n');
            *at_line_start = true;
            return;
        }
    }
}

fn push_statement(out: &mut Vec<String>, current: &mut String, has_code: &mut bool) {
    let stmt = current.trim();
    if *has_code && !stmt.is_empty() {
        out.push(stmt.to_string());
    }
    current.clear();
    *has_code = false;
}

#[cfg(test)]
mod tests {
    use super::split_statements as split;

    #[test]
    fn basic() {
        assert_eq!(split("SELECT 1; SELECT 2"), ["SELECT 1", "SELECT 2"]);
        assert_eq!(split("SELECT 1;;\n ; "), ["SELECT 1"]);
        assert!(split("").is_empty());
    }

    #[test]
    fn delimiters_inside_quotes_and_identifiers() {
        assert_eq!(
            split(r#"SELECT ';', "a;b", `c;d`; SELECT 'it''s; ok', 'x\';y'"#),
            [
                r#"SELECT ';', "a;b", `c;d`"#,
                r"SELECT 'it''s; ok', 'x\';y'"
            ]
        );
    }

    #[test]
    fn comments() {
        assert_eq!(
            split("-- intro; still comment\nSELECT 1; # trailing; comment\n/* a; b */ SELECT 2;"),
            ["SELECT 1", "/* a; b */ SELECT 2"]
        );
        // `--` without a following space is an operator, not a comment.
        assert_eq!(split("SELECT 5--1; SELECT 2"), ["SELECT 5--1", "SELECT 2"]);
        assert!(split("-- only a comment").is_empty());
    }

    #[test]
    fn executable_comment_is_kept() {
        assert_eq!(
            split("/*!40101 SET NAMES utf8mb4 */;"),
            ["/*!40101 SET NAMES utf8mb4 */"]
        );
    }

    #[test]
    fn delimiter_command() {
        let sql = "DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND//\nDELIMITER ;\nCALL p();";
        assert_eq!(
            split(sql),
            [
                "CREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND",
                "CALL p()"
            ]
        );
    }

    #[test]
    fn non_ascii_at_line_start() {
        // The `DELIMITER` check used to slice inside a multi-byte character.
        assert_eq!(
            split("-- 本月各客戶的訂單\nSELECT 1;\n中文欄位;"),
            ["SELECT 1", "中文欄位"]
        );
    }

    #[test]
    fn postgres_dollar_quotes_and_comments() {
        use super::split_statements_pg as pg;
        let sql = "CREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql;\nSELECT $tag$a;b$tag$, 'x;y', \"c;d\";";
        assert_eq!(
            pg(sql),
            [
                "CREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql",
                "SELECT $tag$a;b$tag$, 'x;y', \"c;d\""
            ]
        );
        assert_eq!(
            pg("-- intro; x\nSELECT 1--c;\n; /* a /* nested; */ b; */ SELECT 2"),
            ["SELECT 1--c;", "/* a /* nested; */ b; */ SELECT 2"]
        );
        // `#` is an operator, and a backslash only escapes in E'' strings.
        assert_eq!(
            pg("SELECT 5 # 3; SELECT 'a\\'; SELECT E'b\\';c'"),
            ["SELECT 5 # 3", "SELECT 'a\\'", "SELECT E'b\\';c'"]
        );
        assert_eq!(
            pg("SELECT $1, a$b; SELECT 2"),
            ["SELECT $1, a$b", "SELECT 2"]
        );
        assert_eq!(pg("中文 ;\n-- 只有註解"), ["中文"]);
        assert!(pg("-- only a comment").is_empty());
    }
}
