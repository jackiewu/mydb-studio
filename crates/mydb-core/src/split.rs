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
            if rest.len() >= 10 && rest[..10].eq_ignore_ascii_case("delimiter ") {
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
}
