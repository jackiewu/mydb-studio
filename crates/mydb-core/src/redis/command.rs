//! Redis command lines: splitting them into arguments the way `redis-cli`
//! does, spotting the dangerous ones, hiding passwords, and turning replies
//! into something the UI can show.

use ::redis::Value;
use serde::Serialize;

use crate::advisor::{Finding, Level};

/// `DEL` / `UNLINK` with more keys than this asks first.
pub const MANY_KEYS: usize = 100;
/// `SCAN` (and `HSCAN` …) with a larger `COUNT` asks first.
pub const LARGE_SCAN_COUNT: u64 = 10_000;

/// Splits one command line into arguments, like `redis-cli`: spaces
/// separate, `"…"` allows `\n`, `\t`, `\"`, `\\` and `\xHH`, and `'…'`
/// is literal apart from `\'`. Errors on an unterminated quote.
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return Ok(args);
        };
        let mut arg = String::new();
        if first == '"' || first == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == first {
                    closed = true;
                    break;
                }
                if c != '\\' {
                    arg.push(c);
                    continue;
                }
                let Some(e) = chars.next() else { break };
                if first == '\'' {
                    if e != '\'' {
                        arg.push('\\');
                    }
                    arg.push(e);
                    continue;
                }
                match e {
                    'n' => arg.push('\n'),
                    'r' => arg.push('\r'),
                    't' => arg.push('\t'),
                    'b' => arg.push('\u{8}'),
                    'a' => arg.push('\u{7}'),
                    'x' => {
                        let hex: String =
                            [chars.next(), chars.next()].into_iter().flatten().collect();
                        match u8::from_str_radix(&hex, 16) {
                            Ok(b) if hex.len() == 2 => arg.push(char::from(b)),
                            _ => {
                                arg.push_str("\\x");
                                arg.push_str(&hex);
                            }
                        }
                    }
                    other => arg.push(other),
                }
            }
            if !closed {
                return Err("引號沒有成對".into());
            }
            if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                return Err("引號後面要接空白".into());
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                arg.push(c);
            }
        }
        args.push(arg);
    }
}

/// The command lines of an editor text: one per line, skipping blank
/// lines and `#` / `//` comments. Each keeps its 0-based line number.
pub fn command_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i, l.trim()))
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"))
        .collect()
}

fn upper(args: &[String], i: usize) -> String {
    args.get(i)
        .map(|a| a.to_ascii_uppercase())
        .unwrap_or_default()
}

/// The command name, with the subcommand for container commands
/// (`CONFIG SET`, `CLIENT KILL` …).
pub fn command_name(args: &[String]) -> String {
    let name = upper(args, 0);
    let container = [
        "CONFIG", "CLIENT", "ACL", "MEMORY", "OBJECT", "SCRIPT", "FUNCTION", "CLUSTER", "XINFO",
        "XGROUP", "COMMAND", "SLOWLOG", "LATENCY", "MODULE",
    ];
    if container.contains(&name.as_str()) && args.len() > 1 {
        format!("{name} {}", upper(args, 1))
    } else {
        name
    }
}

const MASK: &str = "****";

/// `args` as one line with every password replaced by `****`, for
/// showing, logging and records.
pub fn masked_line(args: &[String]) -> String {
    let mut shown: Vec<String> = args.iter().map(|a| quote(a)).collect();
    let mut hide = |i: usize| {
        if let Some(s) = shown.get_mut(i) {
            *s = MASK.into();
        }
    };
    match upper(args, 0).as_str() {
        // AUTH password | AUTH user password
        "AUTH" => hide(args.len().saturating_sub(1).max(1)),
        // HELLO 3 AUTH user password SETNAME x
        "HELLO" => {
            if let Some(i) = args.iter().position(|a| a.eq_ignore_ascii_case("AUTH")) {
                hide(i + 2);
            }
        }
        // MIGRATE … AUTH password | AUTH2 user password
        "MIGRATE" => {
            for (i, a) in args.iter().enumerate() {
                if a.eq_ignore_ascii_case("AUTH") {
                    hide(i + 1);
                } else if a.eq_ignore_ascii_case("AUTH2") {
                    hide(i + 2);
                }
            }
        }
        "CONFIG" if upper(args, 1) == "SET" => {
            for i in (2..args.len()).step_by(2) {
                let name = upper(args, i);
                if name.contains("PASS") || name.contains("AUTH") {
                    hide(i + 1);
                }
            }
        }
        // ACL SETUSER name on >password #hash <password
        "ACL" if upper(args, 1) == "SETUSER" => {
            for (i, a) in args.iter().enumerate().skip(3) {
                if let Some(first) = a.chars().next()
                    && matches!(first, '>' | '<' | '#' | '!')
                {
                    shown[i] = format!("{first}{MASK}");
                }
            }
        }
        _ => {}
    }
    shown.join(" ")
}

/// Masks every command line in `text`, keeping other lines as they are.
/// Lines that don't parse are kept unless they start like `AUTH`, which
/// is then hidden whole.
pub fn mask_text(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let head = trimmed
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            let sensitive = matches!(
                head.as_str(),
                "AUTH" | "HELLO" | "MIGRATE" | "CONFIG" | "ACL"
            );
            if !sensitive {
                return line.to_string();
            }
            let indent = &line[..line.len() - trimmed.len()];
            match split_args(trimmed) {
                Ok(args) => format!("{indent}{}", masked_line(&args)),
                Err(_) => format!("{indent}{head} {MASK}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Quotes an argument for display when it needs it.
fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c.is_control())
    {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Why a command should be confirmed before it runs, if it should.
pub fn check_command(index: usize, args: &[String]) -> Option<Finding> {
    let name = command_name(args);
    let finding = |rule: &'static str, message: String, suggestion: &str| Finding {
        statement: index,
        sql: masked_line(args),
        level: Level::Danger,
        rule,
        message,
        suggestion: suggestion.into(),
    };
    match name.as_str() {
        "KEYS" => Some(finding(
            "redis_keys",
            "KEYS 會一次掃過整個資料庫，key 多的時候會讓 Redis 停住，其他用戶端都要等它跑完。".into(),
            &format!(
                "改用 SCAN 分批讀取：SCAN 0 MATCH {} COUNT 500",
                args.get(1).map_or("*".into(), |p| quote(p))
            ),
        )),
        "FLUSHALL" => Some(finding(
            "redis_flushall",
            "FLUSHALL 會刪掉所有資料庫（db0～db15）裡的每一個 key，無法復原。".into(),
            "確定是正確的伺服器；只想清一個資料庫的話用 FLUSHDB。",
        )),
        "FLUSHDB" => Some(finding(
            "redis_flushdb",
            "FLUSHDB 會刪掉目前資料庫裡的每一個 key，無法復原。".into(),
            "確定目前選的是正確的資料庫（上方「資料庫」）。",
        )),
        "SHUTDOWN" => Some(finding(
            "redis_shutdown",
            "SHUTDOWN 會關掉 Redis 伺服器，所有用戶端都會斷線。".into(),
            "通常應該由維運流程處理，不要在這裡執行。",
        )),
        "CONFIG SET" => Some(finding(
            "redis_config_set",
            "CONFIG SET 會直接改變執行中伺服器的設定，影響所有用戶端。".into(),
            "先用 CONFIG GET 看目前的值，並記下來以便改回。",
        )),
        "DEBUG" => Some(finding(
            "redis_debug",
            "DEBUG 是除錯用的指令，部分子指令會讓伺服器停住或當掉（例如 DEBUG SLEEP、DEBUG SEGFAULT）。".into(),
            "正式環境不要使用。",
        )),
        "MONITOR" => Some(finding(
            "redis_monitor",
            "MONITOR 會讓伺服器把每一個指令都送過來，忙碌的伺服器效能會明顯下降。指令列不會持續顯示輸出，執行後會立即結束。".into(),
            "需要觀察流量時，改用 SLOWLOG GET 或 INFO commandstats。",
        )),
        "DEL" | "UNLINK" if args.len() - 1 > MANY_KEYS => Some(finding(
            "redis_many_keys",
            format!("一次刪除 {} 個 key。", args.len() - 1),
            "確認清單正確；大量刪除建議用 UNLINK 分批進行。",
        )),
        "SCAN" | "SSCAN" | "HSCAN" | "ZSCAN" => {
            let count = args
                .iter()
                .position(|a| a.eq_ignore_ascii_case("COUNT"))
                .and_then(|i| args.get(i + 1))
                .and_then(|n| n.parse::<u64>().ok())?;
            (count > LARGE_SCAN_COUNT).then(|| {
                finding(
                    "redis_large_scan",
                    format!("{name} 的 COUNT 是 {count}，一次掃太多會讓伺服器短暫停住。"),
                    "COUNT 用 100～1000，多呼叫幾次。",
                )
            })
        }
        _ => None,
    }
}

/// A reply, ready to show.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Nil,
    Integer {
        value: i64,
    },
    /// A bulk string; `binary` when it wasn't UTF-8 and `value` is hex.
    Text {
        value: String,
        binary: bool,
    },
    /// `OK`, `PONG` and other simple strings.
    Status {
        value: String,
    },
    Double {
        value: f64,
    },
    Bool {
        value: bool,
    },
    Array {
        items: Vec<Reply>,
    },
    Map {
        entries: Vec<(Reply, Reply)>,
    },
    Error {
        message: String,
    },
}

impl Reply {
    /// The kind of reply, for records.
    pub fn type_name(&self) -> &'static str {
        match self {
            Reply::Nil => "nil",
            Reply::Integer { .. } => "integer",
            Reply::Text { .. } => "string",
            Reply::Status { .. } => "status",
            Reply::Double { .. } => "double",
            Reply::Bool { .. } => "boolean",
            Reply::Array { .. } => "array",
            Reply::Map { .. } => "map",
            Reply::Error { .. } => "error",
        }
    }

    /// Elements of an array or map; `None` for single values.
    pub fn element_count(&self) -> Option<usize> {
        match self {
            Reply::Array { items } => Some(items.len()),
            Reply::Map { entries } => Some(entries.len()),
            _ => None,
        }
    }

    /// One-line text, as `redis-cli` would print a scalar.
    pub fn to_text(&self) -> String {
        match self {
            Reply::Nil => "(nil)".into(),
            Reply::Integer { value } => value.to_string(),
            Reply::Text { value, binary } => {
                if *binary {
                    format!("0x{value}")
                } else {
                    value.clone()
                }
            }
            Reply::Status { value } => value.clone(),
            Reply::Double { value } => value.to_string(),
            Reply::Bool { value } => value.to_string(),
            Reply::Array { items } => {
                let parts: Vec<String> = items.iter().map(Reply::to_text).collect();
                format!("[{}]", parts.join(", "))
            }
            Reply::Map { entries } => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.to_text(), v.to_text()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Reply::Error { message } => format!("(error) {message}"),
        }
    }
}

/// Text for bytes: UTF-8 when possible, otherwise hex.
pub fn bytes_text(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (bytes.iter().map(|b| format!("{b:02X}")).collect(), true),
    }
}

impl From<Value> for Reply {
    fn from(v: Value) -> Self {
        match v {
            Value::Nil => Reply::Nil,
            Value::Int(value) => Reply::Integer { value },
            Value::BulkString(b) => {
                let (value, binary) = bytes_text(&b);
                Reply::Text { value, binary }
            }
            Value::Array(items) | Value::Set(items) | Value::Push { data: items, .. } => {
                Reply::Array {
                    items: items.into_iter().map(Reply::from).collect(),
                }
            }
            Value::SimpleString(value) => Reply::Status { value },
            Value::Okay => Reply::Status { value: "OK".into() },
            Value::Map(pairs) => Reply::Map {
                entries: pairs
                    .into_iter()
                    .map(|(k, v)| (Reply::from(k), Reply::from(v)))
                    .collect(),
            },
            Value::Attribute { data, .. } => Reply::from(*data),
            Value::Double(value) => Reply::Double { value },
            Value::Boolean(value) => Reply::Bool { value },
            Value::VerbatimString { text, .. } => Reply::Text {
                value: text,
                binary: false,
            },
            Value::BigNumber(n) => Reply::Status {
                value: String::from_utf8_lossy(&n).into_owned(),
            },
            Value::ServerError(e) => Reply::Error {
                message: e.to_string(),
            },
            other => Reply::Status {
                value: format!("{other:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        split_args(line).unwrap()
    }

    #[test]
    fn splits_like_redis_cli() {
        assert_eq!(args("  SET  user:1  hello "), ["SET", "user:1", "hello"]);
        assert_eq!(args(r#"SET k "a b\n\x41\"""#), ["SET", "k", "a b\nA\""]);
        assert_eq!(args(r"SET k 'it\'s \n'"), ["SET", "k", "it's \\n"]);
        assert_eq!(args(r#"SET k """#), ["SET", "k", ""]);
        assert!(split_args(r#"SET k "open"#).is_err());
        assert!(split_args(r#"SET k "a"b"#).is_err());
        assert!(args("").is_empty());
    }

    #[test]
    fn command_lines_skip_comments_and_blanks() {
        let text = "GET a\n\n# note\n  // also a note\n  HGETALL h  ";
        assert_eq!(command_lines(text), [(0, "GET a"), (4, "HGETALL h")]);
    }

    #[test]
    fn auth_is_masked() {
        assert_eq!(masked_line(&args("AUTH s3cret")), "AUTH ****");
        assert_eq!(masked_line(&args("auth admin s3cret")), "auth admin ****");
        assert_eq!(masked_line(&args("AUTH")), "AUTH");
        assert_eq!(
            masked_line(&args("HELLO 3 AUTH admin s3cret SETNAME me")),
            "HELLO 3 AUTH admin **** SETNAME me"
        );
        assert_eq!(
            masked_line(&args("MIGRATE h 6379 k 0 5000 AUTH2 u s3cret")),
            "MIGRATE h 6379 k 0 5000 AUTH2 u ****"
        );
        assert_eq!(
            masked_line(&args("MIGRATE h 6379 k 0 5000 AUTH s3cret")),
            "MIGRATE h 6379 k 0 5000 AUTH ****"
        );
        assert_eq!(
            masked_line(&args(
                "CONFIG SET requirepass s3cret maxmemory 1gb masterauth s3cret"
            )),
            "CONFIG SET requirepass **** maxmemory 1gb masterauth ****"
        );
        assert_eq!(
            masked_line(&args("ACL SETUSER bob on >s3cret ~* +@read")),
            "ACL SETUSER bob on >**** ~* +@read"
        );
        assert_eq!(masked_line(&args(r#"SET k "a b""#)), r#"SET k "a b""#);
        assert_eq!(masked_line(&args(r#"AUTH "pass word""#)), "AUTH ****");

        let text = "GET a\n  AUTH s3cret\nauth \"unterminated s3cret\nSET pw s3cret";
        let masked = mask_text(text);
        assert_eq!(masked, "GET a\n  AUTH ****\nAUTH ****\nSET pw s3cret");
    }

    #[test]
    fn dangerous_commands_are_flagged() {
        let rule = |line: &str| check_command(0, &args(line)).map(|f| f.rule);
        assert_eq!(rule("KEYS user:*"), Some("redis_keys"));
        assert_eq!(rule("keys *"), Some("redis_keys"));
        assert!(
            check_command(0, &args("KEYS user:*"))
                .unwrap()
                .suggestion
                .contains("SCAN 0 MATCH user:*")
        );
        assert_eq!(rule("FLUSHALL ASYNC"), Some("redis_flushall"));
        assert_eq!(rule("flushdb"), Some("redis_flushdb"));
        assert_eq!(rule("SHUTDOWN NOSAVE"), Some("redis_shutdown"));
        assert_eq!(rule("config set maxmemory 1gb"), Some("redis_config_set"));
        assert_eq!(rule("CONFIG GET maxmemory"), None);
        assert_eq!(rule("DEBUG SLEEP 1"), Some("redis_debug"));
        assert_eq!(rule("MONITOR"), Some("redis_monitor"));

        let keys: Vec<String> = (0..=MANY_KEYS).map(|i| format!("k{i}")).collect();
        assert_eq!(
            rule(&format!("DEL {}", keys.join(" "))),
            Some("redis_many_keys")
        );
        assert_eq!(
            rule(&format!("UNLINK {}", keys.join(" "))),
            Some("redis_many_keys")
        );
        assert_eq!(rule(&format!("DEL {}", keys[..MANY_KEYS].join(" "))), None);

        assert_eq!(
            rule("SCAN 0 MATCH * COUNT 100000"),
            Some("redis_large_scan")
        );
        assert_eq!(rule("HSCAN h 0 count 50000"), Some("redis_large_scan"));
        assert_eq!(rule("SCAN 0 COUNT 500"), None);
        assert_eq!(rule("SCAN 0"), None);
        assert_eq!(rule("GET k"), None);
        // The finding never shows a password.
        assert_eq!(
            check_command(0, &args("CONFIG SET requirepass s3cret"))
                .unwrap()
                .sql,
            "CONFIG SET requirepass ****"
        );
    }

    #[test]
    fn replies_convert() {
        let v = Value::Array(vec![
            Value::BulkString(b"a".to_vec()),
            Value::BulkString(vec![0xff]),
            Value::Nil,
            Value::Int(3),
            Value::Okay,
        ]);
        let r = Reply::from(v);
        assert_eq!(r.element_count(), Some(5));
        assert_eq!(r.to_text(), "[a, 0xFF, (nil), 3, OK]");
        assert_eq!(Reply::from(Value::Nil).type_name(), "nil");
    }
}
