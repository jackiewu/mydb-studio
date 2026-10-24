//! Offline advice: what an error means and how to fix it, and checks that
//! run before SQL is sent (risky statements, patterns that defeat indexes).
//!
//! Everything here is rule based and instant; AI suggestions build on it.

use serde::Serialize;

use crate::error::CoreError;
use crate::split::split_statements;

/// Explanation and fixes for an error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Advice {
    pub title: String,
    pub explanation: String,
    pub fixes: Vec<String>,
}

fn advice(title: &str, explanation: &str, fixes: &[&str]) -> Advice {
    Advice {
        title: title.into(),
        explanation: explanation.into(),
        fixes: fixes.iter().map(|f| f.to_string()).collect(),
    }
}

/// The first 'quoted' part of a server message, such as the column in
/// "Unknown column 'x' in 'field list'".
fn quoted(message: &str) -> Option<&str> {
    let start = message.find('\'')? + 1;
    let len = message[start..].find('\'')?;
    Some(&message[start..start + len])
}

/// Advice for an error, when it is one we know.
pub fn error_advice(err: &CoreError) -> Option<Advice> {
    match err {
        CoreError::Server { code, message, .. } => server_advice(*code, message),
        CoreError::Connection { .. } => Some(advice(
            "連線中斷",
            "跟伺服器的網路連線失敗或被關閉。",
            &[
                "確認主機、埠號、防火牆或 VPN。",
                "連線閒置太久會被伺服器關閉（wait_timeout），再執行一次會自動重新連線。",
            ],
        )),
        CoreError::Ssh { .. } => Some(advice(
            "SSH 通道失敗",
            "無法透過 SSH 跳板連到資料庫。",
            &[
                "確認 SSH 主機、帳號和金鑰或密碼。",
                "確認跳板主機連得到資料庫的位址和埠號。",
            ],
        )),
        _ => None,
    }
}

fn server_advice(code: u16, message: &str) -> Option<Advice> {
    let name = quoted(message).unwrap_or("");
    let a = match code {
        1044 | 1142 | 1143 | 1227 | 1370 => advice(
            "權限不足",
            "這個帳號沒有執行這個動作的權限。",
            &[
                "用 SHOW GRANTS; 查看目前帳號的權限。",
                "請 DBA 授權需要的權限，或換一個有權限的帳號。",
            ],
        ),
        1045 => advice(
            "帳號或密碼錯誤",
            "伺服器拒絕了這組帳號密碼，或這個帳號不允許從你的位址連線。",
            &[
                "確認帳號、密碼。",
                "帳號是以「使用者@來源位址」區分的，確認有允許你的來源位址（例如 'user'@'%'）。",
            ],
        ),
        1046 => advice(
            "還沒有選資料庫",
            "SQL 裡的資料表沒有指定資料庫，目前也沒有選資料庫。",
            &[
                "在左側雙擊資料庫，或用工具列的「資料庫」選單切換。",
                "或在前面加 USE 資料庫名稱;，或寫成 資料庫.資料表。",
            ],
        ),
        1048 => advice(
            "欄位不能是 NULL",
            &format!("欄位 {name} 設定為 NOT NULL，但這次寫入的是 NULL。"),
            &[
                "給這個欄位一個值。",
                "如果應該允許空值，用 ALTER TABLE … MODIFY 把欄位改成可為 NULL。",
            ],
        ),
        1049 => advice(
            "資料庫不存在",
            &format!("找不到資料庫 {name}。"),
            &[
                "檢查名稱拼字和大小寫（Linux 上的 MySQL 區分大小寫）。",
                "用 SHOW DATABASES; 看有哪些資料庫。",
            ],
        ),
        1050 => advice(
            "資料表已經存在",
            &format!("資料表 {name} 已經存在。"),
            &[
                "改用 CREATE TABLE IF NOT EXISTS。",
                "或先確認是不是要修改既有的表（ALTER TABLE）。",
            ],
        ),
        1052 => advice(
            "欄位名稱不明確",
            &format!("欄位 {name} 在多個 JOIN 的表裡都有，伺服器不知道是哪一個。"),
            &["在欄位前加上表名或別名，例如 o.id。"],
        ),
        1054 => advice(
            "找不到欄位",
            &format!("欄位 {name} 不存在於這些資料表中。"),
            &[
                "檢查拼字，或在左側展開資料表看實際的欄位名稱。",
                "字串值要用單引號 'value'；用雙引號或沒加引號會被當成欄位名稱。",
                "SELECT 裡取的別名不能用在同一層的 WHERE。",
            ],
        ),
        1055 => advice(
            "GROUP BY 不完整（ONLY_FULL_GROUP_BY）",
            "SELECT 了沒有在 GROUP BY 裡、也沒有用聚合函式包起來的欄位。",
            &[
                "把該欄位加進 GROUP BY。",
                "或用 MAX()、MIN()、ANY_VALUE() 包住它。",
            ],
        ),
        1060 => advice(
            "欄位名稱重複",
            &format!("欄位 {name} 已經存在。"),
            &["換一個名稱，或確認是不是重複執行了 ALTER TABLE。"],
        ),
        1062 => advice(
            "重複的鍵值",
            "寫入的值跟主鍵或唯一索引上已有的資料重複。",
            &[
                "先 SELECT 確認那筆資料是否已存在。",
                "要「有就更新、沒有就新增」可用 INSERT … ON DUPLICATE KEY UPDATE。",
                "要略過重複的資料可用 INSERT IGNORE（會連其他錯誤一起略過，小心使用）。",
            ],
        ),
        1064 => advice(
            "SQL 語法錯誤",
            "伺服器看不懂這句 SQL。錯誤訊息中 near '…' 後面就是出錯的位置。",
            &[
                "檢查 near 後面那段附近：少了逗號、括號不成對、引號沒關。",
                "欄位或表名跟保留字相同（例如 order、key、group）時，要用反引號 `order` 包起來。",
                "多句 SQL 之間要用 ; 分開。",
            ],
        ),
        1067 => advice(
            "預設值不合法",
            &format!("欄位 {name} 的 DEFAULT 值不符合它的型別或 sql_mode。"),
            &["常見於 DATETIME 預設 '0000-00-00'，改成 NULL 或 CURRENT_TIMESTAMP。"],
        ),
        1071 => advice(
            "索引鍵太長",
            "索引欄位的總長度超過上限（utf8mb4 每個字最多 4 bytes）。",
            &[
                "對長字串欄位建前綴索引，例如 INDEX (title(191))。",
                "或縮短 VARCHAR 長度。",
            ],
        ),
        1091 => advice(
            "要刪除的欄位或索引不存在",
            &format!("找不到 {name}。"),
            &[
                "用 SHOW CREATE TABLE 確認實際的名稱。",
                "MariaDB 可用 DROP … IF EXISTS。",
            ],
        ),
        1093 => advice(
            "不能在子查詢裡讀取正在修改的表",
            "MySQL 不允許 UPDATE/DELETE 一張表時，在同一句的子查詢裡讀同一張表。",
            &[
                "改寫成 JOIN：UPDATE t JOIN (SELECT …) x ON … SET …。",
                "或把子查詢再包一層：WHERE id IN (SELECT id FROM (SELECT id FROM t …) AS x)。",
            ],
        ),
        1114 => advice(
            "資料表已滿",
            "資料表或暫存表的空間用完了。",
            &[
                "檢查磁碟空間。",
                "若是暫存表，調整 tmp_table_size / max_heap_table_size，或減少查詢的資料量。",
            ],
        ),
        1118 => advice(
            "單列資料太大",
            "一列所有欄位加起來超過 InnoDB 的列大小上限。",
            &["把大的 VARCHAR 改成 TEXT。", "使用 ROW_FORMAT=DYNAMIC。"],
        ),
        1136 => advice(
            "欄位數量跟值的數量不一致",
            "INSERT 列出的欄位數和 VALUES 裡的值數量不同。",
            &["寫明欄位清單 INSERT INTO t (a, b) VALUES (1, 2)，並逐一對齊。"],
        ),
        1146 => advice(
            "資料表不存在",
            &format!("找不到資料表 {name}。"),
            &[
                "檢查名稱拼字和大小寫。",
                "確認目前選的資料庫對不對，或寫成 資料庫.資料表。",
                "用 SHOW TABLES; 看這個資料庫有哪些表。",
            ],
        ),
        1175 => advice(
            "安全更新模式擋下了這句",
            "sql_safe_updates 開啟時，UPDATE/DELETE 必須用 WHERE 鎖定主鍵或有索引的欄位，或加 LIMIT。這是在防止誤改整張表。",
            &[
                "加上用主鍵或索引欄位的 WHERE，例如 WHERE id = 123。",
                "先用相同條件 SELECT 確認會影響哪些列。",
                "確定要改整張表時，才在這個連線執行 SET SQL_SAFE_UPDATES = 0;。",
            ],
        ),
        1205 => advice(
            "等鎖逾時",
            "另一個交易鎖住了要修改的資料，等太久（innodb_lock_wait_timeout）放棄了。",
            &[
                "稍後重試。",
                "找出持有鎖的交易：SELECT * FROM information_schema.innodb_trx;。",
                "交易盡量短，修改條件要用到索引，避免鎖住太多列。",
            ],
        ),
        1213 => advice(
            "死結（deadlock）",
            "兩個交易互相等對方的鎖，伺服器取消了這一個。",
            &[
                "重新執行一次通常就會成功。",
                "讓各處以相同順序修改資料表和資料列。",
                "用 SHOW ENGINE INNODB STATUS; 查看最近一次死結的細節。",
            ],
        ),
        1215 => advice(
            "無法建立外鍵",
            "外鍵兩邊的欄位型別、長度、字元集要一致，被參照的欄位要有索引。",
            &[
                "比對兩邊欄位定義（含 UNSIGNED、字元集）。",
                "確認被參照的欄位是主鍵或有索引。",
            ],
        ),
        1216 | 1452 => advice(
            "外鍵找不到對應的資料",
            "寫入的值在被參照的父表中不存在。",
            &["先新增父表的資料，或改用存在的值。"],
        ),
        1217 | 1451 => advice(
            "還有資料參照這一列",
            "要刪除或修改的資料，被其他表的外鍵參照著。",
            &[
                "先處理子表的資料。",
                "或在外鍵上設定 ON DELETE CASCADE / SET NULL（會連帶影響子表，小心）。",
            ],
        ),
        1241 => advice(
            "子查詢回傳了多個欄位",
            "這裡只能放一個欄位的子查詢。",
            &["子查詢只 SELECT 一個欄位。"],
        ),
        1242 => advice(
            "子查詢回傳了多列",
            "用 = 比較的子查詢只能回傳一列。",
            &["改用 IN (…)。", "或在子查詢加條件、LIMIT 1 讓它只有一列。"],
        ),
        1248 => advice(
            "衍生表需要別名",
            "FROM 裡的子查詢必須取一個別名。",
            &["在子查詢後面加 AS x，例如 FROM (SELECT …) AS x。"],
        ),
        1264 => advice(
            "數值超出範圍",
            &format!("寫入 {name} 的數值超過欄位型別能存的範圍。"),
            &[
                "檢查資料是否正確。",
                "需要的話把欄位改大，例如 INT → BIGINT，或 DECIMAL 加大位數。",
            ],
        ),
        1265 | 1406 => advice(
            "資料太長被截斷",
            &format!("寫入 {name} 的值超過欄位長度。"),
            &[
                "縮短資料。",
                "或把欄位改長，例如 VARCHAR(50) → VARCHAR(255)。",
            ],
        ),
        1290 => advice(
            "伺服器是唯讀的",
            "伺服器設定了 read_only（常見於從庫或維護中的主機）。",
            &["確認連到的是可寫入的主庫。"],
        ),
        1292 => advice(
            "日期或數值格式不正確",
            "值的格式不符合欄位型別，例如日期寫成 '2026/13/01' 或空字串。",
            &[
                "日期用 'YYYY-MM-DD HH:MM:SS' 格式。",
                "空值用 NULL 而不是 ''。",
            ],
        ),
        1317 => advice(
            "查詢被中斷",
            "查詢被 KILL 或因逾時而中止。",
            &["如果是逾時，檢查執行計畫（Explain）看是否需要索引。"],
        ),
        1364 => advice(
            "欄位沒有預設值",
            &format!("欄位 {name} 是 NOT NULL 且沒有 DEFAULT，INSERT 時沒有給它值。"),
            &["在 INSERT 裡給這個欄位值。", "或替欄位加上 DEFAULT。"],
        ),
        1366 => advice(
            "字元無法存入",
            "字串裡有欄位字元集存不下的字，最常見是 emoji 存進 utf8（3 bytes）欄位。",
            &[
                "把資料表和欄位改成 utf8mb4。",
                "連線的字元集也要是 utf8mb4。",
            ],
        ),
        1040 => advice(
            "連線數已滿",
            "伺服器已達 max_connections 上限。",
            &["關閉不用的連線，或請 DBA 調高上限。"],
        ),
        1969 | 3024 => advice(
            "查詢執行逾時",
            "查詢超過伺服器設定的最長執行時間。",
            &[
                "用 Explain 檢查是否有全表掃描，補上索引。",
                "縮小查詢範圍或加 LIMIT。",
            ],
        ),
        2006 | 2013 => advice(
            "跟伺服器的連線中斷",
            "執行中連線斷了，可能是逾時、封包太大（max_allowed_packet）或伺服器重啟。",
            &["再執行一次會自動重新連線。", "大量資料寫入時分批執行。"],
        ),
        _ => return None,
    };
    Some(a)
}

// ---------- Automatic LIMIT ----------

/// `stmt` with `LIMIT n` added when it is a query that reads rows from a
/// table without a LIMIT of its own; otherwise `None`.
///
/// Locking reads (`FOR UPDATE`, `LOCK IN SHARE MODE`) and `SELECT … INTO`
/// are left alone, since a LIMIT would change what they lock or write.
pub fn add_limit(stmt: &str, n: u64) -> Option<String> {
    let toks = tokens(stmt);
    let top: Vec<&Tok> = toks
        .iter()
        .filter(|(_, d)| *d == 0)
        .map(|(t, _)| t)
        .collect();
    let starts_query = matches!(top.first(), Some(Tok::Word(w)) if w == "SELECT" || w == "WITH")
        || matches!(top.first(), Some(Tok::Punct('(')));
    let has = |w: &str| top.iter().any(|t| is_word(t, w));
    if !starts_query
        || !toks.iter().any(|(t, _)| is_word(t, "FROM"))
        || ["LIMIT", "INTO", "FOR", "LOCK", "PROCEDURE"]
            .iter()
            .any(|w| has(w))
    {
        return None;
    }
    // On its own line, so a trailing `-- comment` cannot swallow it.
    Some(format!("{}\nLIMIT {n}", stmt.trim_end()))
}

/// Whether `stmt` only reads: a query, SHOW, DESCRIBE or EXPLAIN, without
/// `INTO`, locking clauses or a data-changing part (`WITH … DELETE`). Used
/// before running a statement again, e.g. to export all of its rows.
pub fn is_plain_read(stmt: &str) -> bool {
    let toks = tokens(stmt);
    let top: Vec<&Tok> = toks
        .iter()
        .filter(|(_, d)| *d == 0)
        .map(|(t, _)| t)
        .collect();
    let starts = match top.first() {
        Some(Tok::Word(w)) => [
            "SELECT", "WITH", "SHOW", "DESC", "DESCRIBE", "EXPLAIN", "TABLE", "VALUES",
        ]
        .contains(&w.as_str()),
        Some(Tok::Punct('(')) => true,
        _ => false,
    };
    starts
        && ![
            "INTO",
            "FOR",
            "LOCK",
            "ANALYZE",
            "INSERT",
            "UPDATE",
            "DELETE",
            "REPLACE",
            "PROCEDURE",
        ]
        .iter()
        .any(|w| top.iter().any(|t| is_word(t, w)))
}

// ---------- Pre-run checks ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Could destroy data: ask before running.
    Danger,
    /// Likely slow or surprising.
    Warn,
    /// Worth knowing.
    Info,
}

/// Something a check noticed about one statement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// Index of the statement among those in the SQL.
    pub statement: usize,
    /// The statement's first line, to show which one is meant.
    pub sql: String,
    pub level: Level,
    /// Stable id of the rule, e.g. `update_without_where`.
    pub rule: &'static str,
    pub message: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// Upper-cased keyword or identifier.
    Word(String),
    /// A quoted string's contents.
    Str(String),
    /// A backtick-quoted identifier's name.
    Ident(String),
    Punct(char),
}

/// Tokens of one statement with their parenthesis depth. Comments are dropped.
fn tokens(sql: &str) -> Vec<(Tok, usize)> {
    tokens_cased(sql, true)
}

/// Like [`tokens`]; words keep their case unless `upper`.
fn tokens_cased(sql: &str, upper: bool) -> Vec<(Tok, usize)> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '#' => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for d in chars.by_ref() {
                    if prev == '*' && d == '/' {
                        break;
                    }
                    prev = d;
                }
            }
            '\'' | '"' | '`' => {
                let mut s = String::new();
                while let Some(d) = chars.next() {
                    if d == '\\' && c != '`' {
                        if let Some(e) = chars.next() {
                            s.push(e);
                        }
                    } else if d == c {
                        if chars.peek() == Some(&c) {
                            s.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    } else {
                        s.push(d);
                    }
                }
                let tok = if c == '`' { Tok::Ident(s) } else { Tok::Str(s) };
                out.push((tok, depth));
            }
            c if c.is_alphanumeric() || c == '_' || c == '$' || c == '@' => {
                let mut w = String::from(c);
                while let Some(&d) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' || d == '$' || d == '.' {
                        w.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let w = if upper { w.to_uppercase() } else { w };
                out.push((Tok::Word(w), depth));
            }
            '(' => {
                out.push((Tok::Punct('('), depth));
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                out.push((Tok::Punct(')'), depth));
            }
            c => out.push((Tok::Punct(c), depth)),
        }
    }
    out
}

/// Words that end a table reference (so they aren't read as an alias).
const AFTER_TABLE: &[&str] = &[
    "WHERE",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "CROSS",
    "NATURAL",
    "STRAIGHT_JOIN",
    "ON",
    "USING",
    "GROUP",
    "ORDER",
    "LIMIT",
    "HAVING",
    "UNION",
    "FOR",
    "LOCK",
    "WINDOW",
    "SET",
    "USE",
    "FORCE",
    "IGNORE",
    "PARTITION",
    "INTO",
    "VALUES",
    "VALUE",
    "SELECT",
    "WITH",
];

/// Tables a statement names, as `(database, table)`, in order of first use.
/// Used to give the AI their structure.
pub fn referenced_tables(sql: &str) -> Vec<(Option<String>, String)> {
    let mut out: Vec<(Option<String>, String)> = Vec::new();
    for stmt in crate::split::split_statements(sql) {
        let toks = tokens_cased(&stmt, false);
        let upper = |i: usize| match toks.get(i) {
            Some((Tok::Word(w), _)) => w.to_uppercase(),
            _ => String::new(),
        };
        // The name starting at `j`, and the index after it.
        let name_at = |j: usize| -> Option<((Option<String>, String), usize)> {
            let part = |k: usize| match toks.get(k) {
                Some((Tok::Word(w), _)) if !w.starts_with('@') => Some(w.clone()),
                Some((Tok::Ident(w), _)) => Some(w.clone()),
                _ => None,
            };
            let first = part(j)?;
            if matches!(toks.get(j + 1), Some((Tok::Punct('.'), _)))
                && let Some(second) = part(j + 2)
            {
                return Some(((Some(first), second), j + 3));
            }
            Some(match first.split_once('.') {
                Some((db, t)) if !db.is_empty() && !t.is_empty() => {
                    ((Some(db.to_string()), t.to_string()), j + 1)
                }
                _ => ((None, first), j + 1),
            })
        };
        let mut push = |n: (Option<String>, String)| {
            if !n.1.is_empty()
                && !AFTER_TABLE.contains(&n.1.to_uppercase().as_str())
                && !out.contains(&n)
            {
                out.push(n);
            }
        };
        for i in 0..toks.len() {
            let kw = upper(i);
            let starts = match kw.as_str() {
                "FROM" | "JOIN" | "UPDATE" | "INTO" => true,
                "TABLE" => true,
                "DESCRIBE" | "DESC" => i == 0,
                _ => false,
            };
            if !starts {
                continue;
            }
            let mut j = i + 1;
            // CREATE TABLE IF NOT EXISTS t / DROP TABLE IF EXISTS t
            if upper(j) == "IF" {
                j += 1;
                if upper(j) == "NOT" {
                    j += 1;
                }
                if upper(j) == "EXISTS" {
                    j += 1;
                }
            }
            let depth = toks[i].1;
            while let Some((name, next)) = name_at(j) {
                push(name);
                if kw != "FROM" {
                    break;
                }
                // Skip an alias, then follow `FROM a, b`.
                j = next;
                if upper(j) == "AS" {
                    j += 2;
                } else if let Some((Tok::Word(w), _)) = toks.get(j)
                    && !AFTER_TABLE.contains(&w.to_uppercase().as_str())
                {
                    j += 1;
                }
                match toks.get(j) {
                    Some((Tok::Punct(','), d)) if *d == depth => j += 1,
                    _ => break,
                }
            }
        }
    }
    out
}

fn is_word(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Word(x) if x == w)
}

fn first_line(sql: &str) -> String {
    let line = sql
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(79).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Functions that, wrapped around a column in WHERE, stop an index on it
/// from being used.
const INDEX_BREAKERS: &[&str] = &[
    "DATE",
    "YEAR",
    "MONTH",
    "DAY",
    "LOWER",
    "UPPER",
    "TRIM",
    "SUBSTRING",
    "SUBSTR",
    "LEFT",
    "RIGHT",
    "CONCAT",
    "IFNULL",
    "COALESCE",
    "CAST",
    "CONVERT",
    "DATE_FORMAT",
    "UNIX_TIMESTAMP",
];

/// Checks each statement of `sql` before it runs.
pub fn check_sql(sql: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, stmt) in split_statements(sql).iter().enumerate() {
        let toks = tokens(stmt);
        let mut add = |level, rule, message: String, suggestion: &str| {
            out.push(Finding {
                statement: i,
                sql: first_line(stmt),
                level,
                rule,
                message,
                suggestion: suggestion.into(),
            })
        };
        let top: Vec<&Tok> = toks
            .iter()
            .filter(|(_, d)| *d == 0)
            .map(|(t, _)| t)
            .collect();
        let Some(Tok::Word(verb)) = top.first() else {
            continue;
        };
        let has_top = |w: &str| top.iter().any(|t| is_word(t, w));
        let where_at = top.iter().position(|t| is_word(t, "WHERE"));

        match verb.as_str() {
            "UPDATE" | "DELETE" => {
                let what = if verb == "UPDATE" { "修改" } else { "刪除" };
                match where_at {
                    None => add(
                        Level::Danger,
                        if verb == "UPDATE" {
                            "update_without_where"
                        } else {
                            "delete_without_where"
                        },
                        format!("{verb} 沒有 WHERE，會{what}整張表的每一列。"),
                        "加上 WHERE 條件（例如用主鍵 WHERE id = …），並先用相同條件 SELECT 確認影響範圍。",
                    ),
                    Some(w) => {
                        // WHERE 1, WHERE TRUE, WHERE 1=1 and nothing else.
                        let cond: Vec<&&Tok> = top[w + 1..]
                            .iter()
                            .take_while(|t| !is_word(t, "ORDER") && !is_word(t, "LIMIT"))
                            .collect();
                        let always = matches!(cond.as_slice(), [Tok::Word(a)] if a == "1" || a == "TRUE")
                            || matches!(cond.as_slice(), [Tok::Word(a), Tok::Punct('='), Tok::Word(b)] if a == b);
                        if always {
                            add(
                                Level::Danger,
                                "always_true_where",
                                format!("WHERE 條件永遠成立，會{what}整張表的每一列。"),
                                "改成真正篩選資料的條件。",
                            );
                        }
                    }
                }
                if has_top("LIMIT") && !has_top("ORDER") {
                    add(
                        Level::Info,
                        "limit_without_order",
                        format!("{verb} … LIMIT 沒有 ORDER BY，被{what}的是哪幾列不固定。"),
                        "加上 ORDER BY（例如 ORDER BY id）讓結果可預期。",
                    );
                }
            }
            "TRUNCATE" => add(
                Level::Danger,
                "truncate",
                "TRUNCATE 會清空整張表，而且無法用 ROLLBACK 復原。".into(),
                "確認連線和資料表正確，必要時先備份。",
            ),
            "DROP" => {
                let object = top.get(1).map(|t| match t {
                    Tok::Word(w) => w.as_str(),
                    _ => "",
                });
                match object {
                    Some("DATABASE" | "SCHEMA") => add(
                        Level::Danger,
                        "drop_database",
                        "DROP DATABASE 會刪除整個資料庫和裡面所有的表，無法復原。".into(),
                        "確認連線和資料庫名稱，先備份（mysqldump）。",
                    ),
                    Some("TABLE") => add(
                        Level::Danger,
                        "drop_table",
                        "DROP TABLE 會刪除資料表和所有資料，無法復原。".into(),
                        "確認連線和資料表名稱，先備份；只是想清資料可以考慮 RENAME 保留舊表。",
                    ),
                    Some("USER") => add(
                        Level::Danger,
                        "drop_user",
                        "DROP USER 會刪除帳號，使用它的程式會無法連線。".into(),
                        "確認沒有服務還在使用這個帳號。",
                    ),
                    _ => add(
                        Level::Warn,
                        "drop",
                        "DROP 會刪除物件，無法復原。".into(),
                        "確認名稱正確。",
                    ),
                }
            }
            "ALTER" if top.get(1).is_some_and(|t| is_word(t, "TABLE")) => {
                if has_top("DROP") {
                    add(
                        Level::Danger,
                        "alter_drop",
                        "ALTER TABLE … DROP 會刪除欄位、索引或分割區，欄位資料無法復原。".into(),
                        "確認要刪除的名稱；正式環境先備份。",
                    );
                }
                add(
                    Level::Warn,
                    "alter_table",
                    "ALTER TABLE 在大表上可能要重建整張表，期間可能鎖表。".into(),
                    "大表請在離峰時段執行，可加 ALGORITHM=INPLACE, LOCK=NONE 讓伺服器不支援時直接報錯，或使用 pt-online-schema-change / gh-ost。",
                );
            }
            "GRANT" => {
                if has_top("ALL")
                    && top.windows(3).any(|w| {
                        matches!(w, [Tok::Punct('*'), Tok::Punct('.'), Tok::Punct('*')])
                            || matches!(w[0], Tok::Word(x) if x == "*.*")
                    })
                {
                    add(
                        Level::Warn,
                        "grant_all",
                        "GRANT ALL ON *.* 給了這個帳號所有資料庫的全部權限。".into(),
                        "只授權需要的資料庫和動作，例如 GRANT SELECT, INSERT ON shop.* TO …。",
                    );
                }
            }
            "INSERT" | "REPLACE" => {
                // INSERT INTO t VALUES … without a column list.
                let mut j = 1;
                while top.get(j).is_some_and(|t| {
                    ["INTO", "IGNORE", "LOW_PRIORITY", "DELAYED", "HIGH_PRIORITY"]
                        .iter()
                        .any(|w| is_word(t, w))
                }) {
                    j += 1;
                }
                if top
                    .get(j + 1)
                    .is_some_and(|t| is_word(t, "VALUES") || is_word(t, "VALUE"))
                {
                    add(
                        Level::Info,
                        "insert_without_columns",
                        "INSERT 沒有列出欄位，資料表欄位一變動這句就會出錯或寫錯欄位。".into(),
                        "寫明欄位清單：INSERT INTO t (a, b) VALUES (…)。",
                    );
                }
            }
            "SELECT" if !has_top("WHERE") && !has_top("LIMIT") && has_top("FROM") => {
                let star = matches!(top.get(1), Some(Tok::Punct('*')));
                let grouped = has_top("GROUP") || toks.iter().any(|(t, _)| is_word(t, "COUNT"));
                if star && !grouped {
                    add(
                        Level::Info,
                        "select_all_rows",
                        "SELECT * 沒有 WHERE 也沒有 LIMIT，伺服器會讀出整張表。".into(),
                        "大表請加 WHERE 或 LIMIT，只取需要的欄位。",
                    );
                }
            }
            _ => {}
        }

        // Patterns anywhere in the statement.
        for (j, (t, _)) in toks.iter().enumerate() {
            let next = toks.get(j + 1).map(|(t, _)| t);
            if is_word(t, "LIKE")
                && let Some(Tok::Str(s)) = next
                && (s.starts_with('%') || s.starts_with('_'))
            {
                add(
                    Level::Warn,
                    "leading_wildcard",
                    format!("LIKE '{s}' 以萬用字元開頭，無法使用索引，會掃描整張表。"),
                    "盡量改成前綴比對 LIKE 'abc%'；需要全文搜尋可用 FULLTEXT 索引。",
                );
            }
            if is_word(t, "RAND")
                && j >= 2
                && is_word(&toks[j - 1].0, "BY")
                && is_word(&toks[j - 2].0, "ORDER")
            {
                add(
                    Level::Warn,
                    "order_by_rand",
                    "ORDER BY RAND() 要替每一列產生亂數再排序，大表會很慢。".into(),
                    "先隨機挑主鍵範圍（例如 WHERE id >= 隨機值 LIMIT 1），或在程式端抽樣。",
                );
            }
            if (is_word(t, "WHERE") || is_word(t, "AND") || is_word(t, "OR"))
                && let Some(Tok::Word(f)) = next
                && INDEX_BREAKERS.contains(&f.as_str())
                && matches!(toks.get(j + 2), Some((Tok::Punct('('), _)))
                && matches!(toks.get(j + 3), Some((Tok::Word(_) | Tok::Ident(_), _)))
            {
                add(
                    Level::Warn,
                    "function_on_column",
                    format!("WHERE 裡用 {f}(欄位) 包住欄位，這個欄位上的索引就用不到。"),
                    "把函式移到另一邊，例如 DATE(created_at) = '2026-10-24' 改成 created_at >= '2026-10-24' AND created_at < '2026-10-25'。",
                );
            }
            if is_word(t, "NOT")
                && next.is_some_and(|n| is_word(n, "IN"))
                && matches!(toks.get(j + 2), Some((Tok::Punct('('), _)))
                && toks.get(j + 3).is_some_and(|(n, _)| is_word(n, "SELECT"))
            {
                add(
                    Level::Info,
                    "not_in_subquery",
                    "NOT IN (SELECT …) 只要子查詢有一個 NULL，整個條件就不成立，結果會是空的。"
                        .into(),
                    "改用 NOT EXISTS (SELECT 1 FROM … WHERE …)，語意清楚也較好最佳化。",
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(sql: &str) -> Vec<&'static str> {
        check_sql(sql).into_iter().map(|f| f.rule).collect()
    }

    #[test]
    fn limit_is_added_only_to_plain_queries() {
        assert_eq!(
            add_limit("SELECT * FROM t", 100).as_deref(),
            Some("SELECT * FROM t\nLIMIT 100")
        );
        assert_eq!(
            add_limit("SELECT a FROM t WHERE b = 1 -- note", 5).as_deref(),
            Some("SELECT a FROM t WHERE b = 1 -- note\nLIMIT 5")
        );
        assert!(add_limit("WITH x AS (SELECT 1 FROM t) SELECT * FROM x", 5).is_some());
        // A LIMIT inside a subquery does not count.
        assert!(add_limit("SELECT * FROM (SELECT * FROM t LIMIT 3) x", 5).is_some());
        for sql in [
            "SELECT * FROM t LIMIT 10",
            "SELECT * FROM t LIMIT 10 OFFSET 5",
            "SELECT 1",
            "SELECT NOW()",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t LOCK IN SHARE MODE",
            "SELECT a INTO @x FROM t",
            "UPDATE t SET a = 1",
            "SHOW TABLES",
            "EXPLAIN SELECT * FROM t",
        ] {
            assert_eq!(add_limit(sql, 5), None, "{sql}");
        }
    }

    #[test]
    fn risky_writes_are_dangerous() {
        assert_eq!(
            rules("UPDATE orders SET note = 'x'"),
            ["update_without_where"]
        );
        assert_eq!(rules("delete from orders;"), ["delete_without_where"]);
        assert_eq!(rules("DELETE FROM t WHERE 1=1"), ["always_true_where"]);
        assert_eq!(
            rules("UPDATE t SET a = 1 WHERE TRUE"),
            ["always_true_where"]
        );
        assert_eq!(rules("TRUNCATE TABLE t"), ["truncate"]);
        assert_eq!(rules("drop table t"), ["drop_table"]);
        assert_eq!(rules("DROP DATABASE shop"), ["drop_database"]);
        assert_eq!(
            rules("ALTER TABLE t DROP COLUMN a"),
            ["alter_drop", "alter_table"]
        );
        assert!(
            check_sql("DROP TABLE t")
                .iter()
                .all(|f| f.level == Level::Danger)
        );
    }

    #[test]
    fn safe_statements_pass() {
        assert!(rules("UPDATE orders SET note = 'x' WHERE id = 1").is_empty());
        // A WHERE inside a subquery does not count for the outer UPDATE.
        assert_eq!(
            rules("UPDATE t SET a = (SELECT 1 FROM u WHERE u.id = 2)"),
            ["update_without_where"]
        );
        assert!(rules("SELECT id FROM orders WHERE id = 1").is_empty());
        assert!(rules("SELECT COUNT(*) FROM orders").is_empty());
        // Keywords inside strings and comments are ignored.
        assert!(rules("SELECT 'DELETE FROM x' AS s -- DROP TABLE y").is_empty());
        assert!(rules("INSERT INTO t (a) VALUES (1)").is_empty());
    }

    #[test]
    fn slow_patterns_warn() {
        assert_eq!(
            rules("SELECT id FROM t WHERE name LIKE '%abc'"),
            ["leading_wildcard"]
        );
        assert!(rules("SELECT id FROM t WHERE name LIKE 'abc%'").is_empty());
        assert_eq!(
            rules("SELECT id FROM t WHERE a = 1 ORDER BY RAND() LIMIT 1"),
            ["order_by_rand"]
        );
        assert_eq!(
            rules("SELECT id FROM t WHERE DATE(created_at) = '2026-10-24'"),
            ["function_on_column"]
        );
        assert_eq!(
            rules("SELECT id FROM t WHERE id NOT IN (SELECT x FROM u)"),
            ["not_in_subquery"]
        );
        assert_eq!(rules("SELECT * FROM t"), ["select_all_rows"]);
        assert_eq!(
            rules("INSERT INTO t VALUES (1, 2)"),
            ["insert_without_columns"]
        );
        assert_eq!(rules("GRANT ALL ON *.* TO 'a'@'%'"), ["grant_all"]);
        assert_eq!(
            rules("DELETE FROM t WHERE a = 1 LIMIT 10"),
            ["limit_without_order"]
        );
    }

    #[test]
    fn findings_point_at_their_statement() {
        let f = check_sql("SELECT 1;\nUPDATE t SET a = 1;");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].statement, 1);
        assert_eq!(f[0].sql, "UPDATE t SET a = 1");
    }

    #[test]
    fn finds_referenced_tables() {
        let t = |db: Option<&str>, n: &str| (db.map(str::to_string), n.to_string());
        assert_eq!(
            referenced_tables(
                "SELECT * FROM shop.Orders o, items AS i JOIN `shop`.`users` u ON u.id = o.user_id \
                 WHERE o.id IN (SELECT order_id FROM refunds);\n\
                 UPDATE products SET x = 1;\nINSERT INTO logs VALUES (1);\n\
                 CREATE TABLE IF NOT EXISTS t2 (id INT); SELECT @a INTO @b"
            ),
            [
                t(Some("shop"), "Orders"),
                t(None, "items"),
                t(Some("shop"), "users"),
                t(None, "refunds"),
                t(None, "products"),
                t(None, "logs"),
                t(None, "t2"),
            ]
        );
        assert!(referenced_tables("SELECT 1 ORDER BY 1 DESC LIMIT 1").is_empty());
    }

    #[test]
    fn errors_get_advice() {
        let err = CoreError::Server {
            code: 1054,
            state: "42S22".into(),
            message: "Unknown column 'nmae' in 'field list'".into(),
        };
        let a = error_advice(&err).unwrap();
        assert_eq!(a.title, "找不到欄位");
        assert!(a.explanation.contains("nmae"));
        let unknown = CoreError::Server {
            code: 9999,
            state: "HY000".into(),
            message: "x".into(),
        };
        assert!(error_advice(&unknown).is_none());
    }

    #[test]
    fn plain_reads() {
        for yes in [
            "SELECT * FROM t",
            "  /* x */ select a from t where b = 'DELETE'",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "SHOW PROCESSLIST",
            "DESCRIBE t",
            "EXPLAIN SELECT 1",
            "(SELECT 1) UNION (SELECT 2)",
        ] {
            assert!(is_plain_read(yes), "{yes}");
        }
        for no in [
            "DELETE FROM t",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * INTO OUTFILE '/tmp/x' FROM t",
            "WITH c AS (SELECT 1) DELETE FROM t",
            "EXPLAIN ANALYZE DELETE FROM t",
            "CALL p()",
            "",
        ] {
            assert!(!is_plain_read(no), "{no}");
        }
    }
}
