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
/// is literal apart from `\'`. Arguments are bytes, as sent: `"\xff"` is
/// the single byte 0xFF. Errors on an unterminated quote.
pub fn split_args(line: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    let push = |arg: &mut Vec<u8>, c: char| {
        let mut buf = [0; 4];
        arg.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    };
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else {
            return Ok(args);
        };
        let mut arg = Vec::new();
        if first == '"' || first == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == first {
                    closed = true;
                    break;
                }
                if c != '\\' {
                    push(&mut arg, c);
                    continue;
                }
                let Some(e) = chars.next() else { break };
                if first == '\'' {
                    if e != '\'' {
                        arg.push(b'\\');
                    }
                    push(&mut arg, e);
                    continue;
                }
                match e {
                    'n' => arg.push(b'\n'),
                    'r' => arg.push(b'\r'),
                    't' => arg.push(b'\t'),
                    'b' => arg.push(0x08),
                    'a' => arg.push(0x07),
                    'x' => {
                        let hex: String =
                            [chars.next(), chars.next()].into_iter().flatten().collect();
                        match u8::from_str_radix(&hex, 16) {
                            Ok(b) if hex.len() == 2 => arg.push(b),
                            _ => {
                                arg.extend_from_slice(b"\\x");
                                arg.extend_from_slice(hex.as_bytes());
                            }
                        }
                    }
                    other => push(&mut arg, other),
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
                push(&mut arg, c);
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

/// An argument as text, for keyword checks.
fn text(arg: &[u8]) -> String {
    String::from_utf8_lossy(arg).into_owned()
}

fn upper(args: &[Vec<u8>], i: usize) -> String {
    args.get(i)
        .map(|a| text(a).to_ascii_uppercase())
        .unwrap_or_default()
}

/// The command name, with the subcommand for container commands
/// (`CONFIG SET`, `CLIENT KILL` …).
pub fn command_name(args: &[Vec<u8>]) -> String {
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
pub fn masked_line(args: &[Vec<u8>]) -> String {
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
            if let Some(i) = args.iter().position(|a| a.eq_ignore_ascii_case(b"AUTH")) {
                hide(i + 2);
            }
        }
        // MIGRATE … AUTH password | AUTH2 user password
        "MIGRATE" => {
            for (i, a) in args.iter().enumerate() {
                if a.eq_ignore_ascii_case(b"AUTH") {
                    hide(i + 1);
                } else if a.eq_ignore_ascii_case(b"AUTH2") {
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
                if let Some(&first) = a.first()
                    && matches!(first, b'>' | b'<' | b'#' | b'!')
                {
                    shown[i] = format!("{}{MASK}", first as char);
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

/// An argument for display: as is when it is plain text, otherwise
/// quoted the way `split_args` reads it back (bytes that aren't UTF-8 as
/// `\xHH`).
fn quote(arg: &[u8]) -> String {
    let (shown, binary) = bytes_text(arg);
    if !binary
        && !shown.is_empty()
        && !shown
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c.is_control())
    {
        return shown;
    }
    let mut out = String::from("\"");
    for chunk in arg.utf8_chunks() {
        for c in chunk.valid().chars() {
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
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out.push('"');
    out
}

/// Why a command should be confirmed before it runs, if it should.
pub fn check_command(index: usize, args: &[Vec<u8>]) -> Option<Finding> {
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
        "REPLICAOF" | "SLAVEOF" => {
            let stop = upper(args, 1) == "NO" && upper(args, 2) == "ONE";
            Some(if stop {
                finding(
                    "redis_replicaof",
                    format!("{name} NO ONE 會讓這台停止複寫、變成主節點，應用程式可能會寫到錯的那台。"),
                    "確認這是計畫中的切換。",
                )
            } else {
                finding(
                    "redis_replicaof",
                    format!("{name} 會讓這台變成另一台的副本：這台目前的資料會全部清掉，換成主節點的資料。"),
                    "確認主機和埠號正確，而且這台的資料可以丟掉。",
                )
            })
        }
        "SWAPDB" => Some(finding(
            "redis_swapdb",
            format!(
                "SWAPDB 會把 db{} 和 db{} 的內容整個對調，正在使用這兩個資料庫的用戶端會立刻看到另一邊的資料。",
                text(args.get(1).map_or(b"?" as &[u8], |a| a)),
                text(args.get(2).map_or(b"?" as &[u8], |a| a))
            ),
            "確認兩個資料庫編號正確。",
        )),
        "SCRIPT FLUSH" => Some(finding(
            "redis_script_flush",
            "SCRIPT FLUSH 會清掉伺服器快取的所有 Lua 腳本，之後用 EVALSHA 的應用程式會出現 NOSCRIPT 錯誤。".into(),
            "確認應用程式在 NOSCRIPT 時會改用 EVAL 重新載入。",
        )),
        "FUNCTION FLUSH" => Some(finding(
            "redis_function_flush",
            "FUNCTION FLUSH 會刪掉伺服器上所有的函式庫，用 FCALL 呼叫的地方都會失敗。".into(),
            "先用 FUNCTION DUMP 備份。",
        )),
        "SAVE" => Some(finding(
            "redis_save",
            "SAVE 會在主執行緒寫出整個資料集，寫完之前所有用戶端都要等。".into(),
            "改用 BGSAVE，在背景寫出。",
        )),
        "CLIENT KILL" => Some(finding(
            "redis_client_kill",
            "CLIENT KILL 會把符合條件的用戶端連線斷掉，可能包括正在服務的應用程式。".into(),
            "先用 CLIENT LIST 確認要斷的是哪些連線。",
        )),
        "CONFIG REWRITE" => Some(finding(
            "redis_config_rewrite",
            "CONFIG REWRITE 會把目前的設定（包括用 CONFIG SET 改過的）寫回伺服器的設定檔，重啟後也會生效。".into(),
            "確認目前的設定都是要保留的。",
        )),
        "CONFIG RESETSTAT" => Some(finding(
            "redis_config_resetstat",
            "CONFIG RESETSTAT 會把 INFO 的統計數字（指令次數、命中率、慢查詢紀錄等）歸零，監控會看到數字突然掉下來。".into(),
            "需要的話先用 INFO 記下目前的數字。",
        )),
        "ACL DELUSER" => Some(finding(
            "redis_acl_deluser",
            "ACL DELUSER 會刪掉使用者，並斷開用這個帳號登入的所有連線。".into(),
            "確認沒有應用程式還在用這個帳號。",
        )),
        "MODULE LOAD" => Some(finding(
            "redis_module_load",
            "MODULE LOAD 會把一個動態函式庫載入伺服器執行，模組有問題時可能讓伺服器當掉。".into(),
            "只載入信任的模組，並先在測試環境試過。",
        )),
        "MODULE UNLOAD" => Some(finding(
            "redis_module_unload",
            "MODULE UNLOAD 會卸載模組，用到這個模組的指令和資料型別都會無法使用。".into(),
            "確認沒有應用程式還在用這個模組。",
        )),
        "CLUSTER RESET" => Some(finding(
            "redis_cluster_reset",
            "CLUSTER RESET 會讓這個節點忘掉叢集設定；HARD 還會換掉節點 ID。".into(),
            "通常只在重建叢集時使用。",
        )),
        "CLUSTER FAILOVER" => Some(finding(
            "redis_cluster_failover",
            "CLUSTER FAILOVER 會讓這個副本接手變成主節點，原本的主節點降為副本。".into(),
            "確認這是計畫中的切換。",
        )),
        // The read-only variants (EVAL_RO …) can't change anything.
        "EVAL" | "EVALSHA" | "FCALL" => Some(finding(
            "redis_script",
            format!(
                "{name} 會在伺服器上執行腳本。腳本裡呼叫的指令不會逐一檢查，例如腳本裡可以執行 FLUSHALL；執行期間伺服器也無法處理其他用戶端。"
            ),
            "確認腳本內容；不確定時先在測試環境執行。",
        )),
        "DEL" | "UNLINK" if args.len() - 1 > MANY_KEYS => Some(finding(
            "redis_many_keys",
            format!("一次刪除 {} 個 key。", args.len() - 1),
            "確認清單正確；大量刪除建議用 UNLINK 分批進行。",
        )),
        "SCAN" | "SSCAN" | "HSCAN" | "ZSCAN" => {
            let count = args
                .iter()
                .position(|a| a.eq_ignore_ascii_case(b"COUNT"))
                .and_then(|i| args.get(i + 1))
                .and_then(|n| text(n).parse::<u64>().ok())?;
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

    fn args(line: &str) -> Vec<Vec<u8>> {
        split_args(line).unwrap()
    }

    fn strs(line: &str) -> Vec<String> {
        args(line).iter().map(|a| text(a)).collect()
    }

    #[test]
    fn splits_like_redis_cli() {
        assert_eq!(strs("  SET  user:1  hello "), ["SET", "user:1", "hello"]);
        assert_eq!(strs(r#"SET k "a b\n\x41\"""#), ["SET", "k", "a b\nA\""]);
        assert_eq!(strs(r"SET k 'it\'s \n'"), ["SET", "k", "it's \\n"]);
        assert_eq!(strs(r#"SET k """#), ["SET", "k", ""]);
        assert_eq!(strs("SET 名稱 值"), ["SET", "名稱", "值"]);
        assert!(split_args(r#"SET k "open"#).is_err());
        assert!(split_args(r#"SET k "a"b"#).is_err());
        assert!(args("").is_empty());
    }

    #[test]
    fn hex_escapes_are_single_bytes() {
        // As redis-cli sends them: one byte each, not UTF-8.
        assert_eq!(args(r#"SET k "\xff\x00a""#)[2], vec![0xff, 0x00, b'a']);
        assert_eq!(args(r#"SET k "\xc3\xa9""#)[2], "é".as_bytes());
        assert_eq!(args(r#"SET k "\xzz""#)[2], b"\\xzz");
        // Shown so they read back the same.
        let line = masked_line(&args(r#"SET k "\xff a""#));
        assert_eq!(line, r#"SET k "\xff a""#);
        assert_eq!(args(&line), args(r#"SET k "\xff a""#));
    }

    #[test]
    fn subcommands_are_part_of_the_name() {
        assert_eq!(command_name(&args("cluster reset hard")), "CLUSTER RESET");
        assert_eq!(command_name(&args("Module Load /x.so")), "MODULE LOAD");
        assert_eq!(command_name(&args("client kill ID 3")), "CLIENT KILL");
        assert_eq!(command_name(&args("CONFIG")), "CONFIG");
        assert_eq!(command_name(&args("get k")), "GET");
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
        assert_eq!(rule("REPLICAOF 10.0.0.1 6379"), Some("redis_replicaof"));
        assert_eq!(rule("slaveof 10.0.0.1 6379"), Some("redis_replicaof"));
        assert!(
            check_command(0, &args("REPLICAOF NO ONE"))
                .unwrap()
                .message
                .contains("停止複寫")
        );
        assert!(
            check_command(0, &args("REPLICAOF h 6379"))
                .unwrap()
                .message
                .contains("清掉")
        );
        assert_eq!(rule("SWAPDB 0 1"), Some("redis_swapdb"));
        assert_eq!(rule("SCRIPT FLUSH"), Some("redis_script_flush"));
        assert_eq!(rule("script exists abc"), None);
        assert_eq!(rule("FUNCTION FLUSH ASYNC"), Some("redis_function_flush"));
        assert_eq!(rule("FUNCTION LIST"), None);
        assert_eq!(rule("SAVE"), Some("redis_save"));
        assert!(
            check_command(0, &args("save"))
                .unwrap()
                .suggestion
                .contains("BGSAVE")
        );
        assert_eq!(rule("BGSAVE"), None);
        assert_eq!(rule("CLIENT KILL ID 5"), Some("redis_client_kill"));
        assert_eq!(rule("CLIENT LIST"), None);
        assert_eq!(rule("CONFIG REWRITE"), Some("redis_config_rewrite"));
        assert_eq!(rule("ACL DELUSER bob"), Some("redis_acl_deluser"));
        assert_eq!(rule("ACL WHOAMI"), None);
        assert_eq!(rule("MODULE LOAD /x.so"), Some("redis_module_load"));
        assert_eq!(rule("MODULE LIST"), None);
        assert_eq!(rule("CLUSTER RESET HARD"), Some("redis_cluster_reset"));
        assert_eq!(rule("CLUSTER FAILOVER"), Some("redis_cluster_failover"));
        assert_eq!(rule("CLUSTER INFO"), None);
        assert_eq!(rule("CONFIG RESETSTAT"), Some("redis_config_resetstat"));
        assert_eq!(rule("MODULE UNLOAD search"), Some("redis_module_unload"));
        for ro in ["EVAL_RO \"return 1\" 0", "evalsha_ro abc 0", "FCALL_RO f 0"] {
            assert_eq!(rule(ro), None, "{ro}");
        }
        for script in ["EVAL \"return 1\" 0", "evalsha abc 0", "FCALL myfunc 0"] {
            let f = check_command(0, &args(script)).unwrap();
            assert_eq!(f.rule, "redis_script", "{script}");
            assert!(f.message.contains("FLUSHALL"), "{script}");
        }

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
