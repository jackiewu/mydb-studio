//! The Redis server page: `INFO`, `CLIENT LIST`, `SLOWLOG`, command
//! statistics and `CONFIG GET`, read on the browse connection.
//!
//! Everything here only reads, except [`RedisSession::kill_client`], which
//! the UI confirms first (rule `redis_client_kill`). Managed services such
//! as ElastiCache refuse `CONFIG` and `SLOWLOG`; those calls then fail with
//! a [`CoreError::Redis`] the page shows in place of the tab.

use std::collections::BTreeMap;

use ::redis::Value;
use serde::Serialize;

use super::{DbInfo, RedisSession, command, parse_keyspace, value_int, value_text};
use crate::error::Result;

/// Entries read from the slow log.
pub const SLOWLOG_ENTRIES: usize = 128;

/// Shown instead of a password in `CONFIG GET`.
const MASK: &str = "****";

/// The numbers of the overview cards, taken from `INFO`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RedisSummary {
    /// `Redis 7.2.4` or `Valkey 8.0.1`.
    pub version: String,
    /// `standalone`, `cluster` or `sentinel`.
    pub mode: Option<String>,
    pub uptime_seconds: u64,
    pub connected_clients: u64,
    pub maxclients: Option<u64>,
    pub blocked_clients: u64,
    pub used_memory: u64,
    pub used_memory_peak: u64,
    /// 0 means no limit.
    pub maxmemory: u64,
    pub maxmemory_policy: Option<String>,
    pub ops_per_sec: f64,
    pub keyspace_hits: u64,
    pub keyspace_misses: u64,
    /// hits ÷ (hits + misses); `None` before any key was read.
    pub hit_ratio: Option<f64>,
    pub expired_keys: u64,
    pub evicted_keys: u64,
    /// `master` or `slave`.
    pub role: String,
    pub connected_slaves: u64,
    /// Unix time of the last RDB save.
    pub rdb_last_save_time: Option<i64>,
    /// `ok` or `err`.
    pub rdb_last_bgsave_status: Option<String>,
    pub rdb_changes_since_last_save: u64,
    pub aof_enabled: bool,
}

/// Everything the overview needs, in one round trip.
#[derive(Debug, Clone, Serialize)]
pub struct RedisOverview {
    pub summary: RedisSummary,
    pub keyspace: Vec<DbInfo>,
    /// Every `INFO` field by name, for the details list.
    pub info: BTreeMap<String, String>,
}

/// One line of `CLIENT LIST`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientInfo {
    pub id: u64,
    pub addr: String,
    pub name: String,
    pub user: String,
    /// Seconds connected.
    pub age: u64,
    /// Seconds since the last command.
    pub idle: u64,
    pub flags: String,
    pub db: u64,
    /// The last command, e.g. `client|list`.
    pub cmd: String,
    /// This app's own browse connection.
    pub own: bool,
}

/// One entry of `SLOWLOG GET`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlowEntry {
    pub id: i64,
    /// Unix time it ran.
    pub time: i64,
    pub duration_us: i64,
    /// The command, passwords masked.
    pub command: String,
    pub client: String,
    pub client_name: String,
}

/// One line of `INFO commandstats`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandStat {
    /// Lower case, `client|list` for subcommands.
    pub name: String,
    pub calls: u64,
    pub usec: u64,
    pub usec_per_call: f64,
    pub rejected_calls: u64,
    pub failed_calls: u64,
}

impl RedisSession {
    /// `INFO` (the default sections) and the key counts per database.
    pub async fn server_overview(&self) -> Result<RedisOverview> {
        let text = self.info_section(None).await?;
        let info = parse_info(&text);
        Ok(RedisOverview {
            summary: summarize(&info),
            keyspace: parse_keyspace(&text),
            info,
        })
    }

    pub async fn clients(&self) -> Result<Vec<ClientInfo>> {
        let mut guard = self.link.lock().await;
        let (l, _) = self.live(&mut guard).await?;
        let own: Option<u64> = ::redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut l.browse)
            .await
            .ok();
        let text: String = ::redis::cmd("CLIENT")
            .arg("LIST")
            .query_async(&mut l.browse)
            .await?;
        Ok(parse_client_list(&text, own))
    }

    /// The newest [`SLOWLOG_ENTRIES`] entries of the slow log.
    pub async fn slowlog(&self) -> Result<Vec<SlowEntry>> {
        let mut guard = self.link.lock().await;
        let (l, _) = self.live(&mut guard).await?;
        let reply: Value = ::redis::cmd("SLOWLOG")
            .arg("GET")
            .arg(SLOWLOG_ENTRIES)
            .query_async(&mut l.browse)
            .await?;
        Ok(parse_slowlog(&reply))
    }

    /// `INFO commandstats`, most called first.
    pub async fn command_stats(&self) -> Result<Vec<CommandStat>> {
        let text = self.info_section(Some("commandstats")).await?;
        Ok(parse_command_stats(&text))
    }

    /// `CONFIG GET *`, sorted by name, passwords shown as `****`.
    pub async fn config(&self) -> Result<Vec<(String, String)>> {
        let mut guard = self.link.lock().await;
        let (l, _) = self.live(&mut guard).await?;
        let reply: Value = ::redis::cmd("CONFIG")
            .arg("GET")
            .arg("*")
            .query_async(&mut l.browse)
            .await?;
        Ok(parse_config(&reply))
    }

    /// `CLIENT KILL ID id`. The UI asks first; returns whether a client
    /// was disconnected.
    pub async fn kill_client(&self, id: u64) -> Result<bool> {
        let mut guard = self.link.lock().await;
        let (l, _) = self.live(&mut guard).await?;
        let killed: i64 = ::redis::cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(id)
            .query_async(&mut l.browse)
            .await?;
        Ok(killed > 0)
    }

    async fn info_section(&self, section: Option<&str>) -> Result<String> {
        let mut guard = self.link.lock().await;
        let (l, _) = self.live(&mut guard).await?;
        let mut c = ::redis::cmd("INFO");
        if let Some(s) = section {
            c.arg(s);
        }
        Ok(c.query_async(&mut l.browse).await?)
    }
}

/// `name:value` lines of `INFO`; `# Section` headers and blank lines are
/// skipped.
pub fn parse_info(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The overview numbers from parsed `INFO`. Missing fields count as zero,
/// since older servers and managed services leave some out.
pub fn summarize(info: &BTreeMap<String, String>) -> RedisSummary {
    let text = |k: &str| info.get(k).map(|v| v.trim().to_string());
    let num = |k: &str| text(k).and_then(|v| v.parse::<u64>().ok());
    let n = |k: &str| num(k).unwrap_or(0);
    let version = match (text("valkey_version"), text("redis_version")) {
        (Some(v), _) => format!("Valkey {v}"),
        (None, Some(v)) => format!("Redis {v}"),
        (None, None) => "Redis".into(),
    };
    let hits = n("keyspace_hits");
    let misses = n("keyspace_misses");
    RedisSummary {
        version,
        mode: text("redis_mode").or_else(|| text("server_mode")),
        uptime_seconds: n("uptime_in_seconds"),
        connected_clients: n("connected_clients"),
        maxclients: num("maxclients"),
        blocked_clients: n("blocked_clients"),
        used_memory: n("used_memory"),
        used_memory_peak: n("used_memory_peak"),
        maxmemory: n("maxmemory"),
        maxmemory_policy: text("maxmemory_policy"),
        ops_per_sec: text("instantaneous_ops_per_sec")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        keyspace_hits: hits,
        keyspace_misses: misses,
        hit_ratio: (hits + misses > 0).then(|| hits as f64 / (hits + misses) as f64),
        expired_keys: n("expired_keys"),
        evicted_keys: n("evicted_keys"),
        role: text("role").unwrap_or_else(|| "master".into()),
        connected_slaves: n("connected_slaves"),
        rdb_last_save_time: text("rdb_last_save_time").and_then(|v| v.parse().ok()),
        rdb_last_bgsave_status: text("rdb_last_bgsave_status"),
        rdb_changes_since_last_save: n("rdb_changes_since_last_save"),
        aof_enabled: text("aof_enabled").as_deref() == Some("1"),
    }
}

/// `id=3 addr=… name= …` lines of `CLIENT LIST`. `own` is this app's
/// connection id, if known.
pub fn parse_client_list(text: &str, own: Option<u64>) -> Vec<ClientInfo> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            // Values have no spaces except `name`, which can't contain any.
            let fields: BTreeMap<&str, &str> = line
                .split_whitespace()
                .filter_map(|kv| kv.split_once('='))
                .collect();
            let s = |k: &str| fields.get(k).copied().unwrap_or("").to_string();
            let n = |k: &str| fields.get(k).and_then(|v| v.parse().ok()).unwrap_or(0);
            let id = n("id");
            ClientInfo {
                id,
                addr: s("addr"),
                name: s("name"),
                user: s("user"),
                age: n("age"),
                idle: n("idle"),
                flags: s("flags"),
                db: n("db"),
                cmd: s("cmd"),
                own: own == Some(id),
            }
        })
        .collect()
}

/// The reply of `SLOWLOG GET`: per entry an id, a time, microseconds, the
/// arguments and, since Redis 4.0, the client's address and name.
pub fn parse_slowlog(reply: &Value) -> Vec<SlowEntry> {
    let Value::Array(entries) = reply else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let Value::Array(f) = e else { return None };
            let int = |i: usize| f.get(i).and_then(value_int).unwrap_or(0);
            let text = |i: usize| f.get(i).map(value_text).unwrap_or_default();
            let args: Vec<Vec<u8>> = match f.get(3) {
                Some(Value::Array(a)) => a.iter().map(arg_bytes).collect(),
                _ => Vec::new(),
            };
            Some(SlowEntry {
                id: int(0),
                time: int(1),
                duration_us: int(2),
                command: command::masked_line(&args),
                client: text(4),
                client_name: text(5),
            })
        })
        .collect()
}

fn arg_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::BulkString(b) => b.clone(),
        other => value_text(other).into_bytes(),
    }
}

/// `cmdstat_get:calls=10,usec=30,usec_per_call=3.00,…` lines, most called
/// first.
pub fn parse_command_stats(text: &str) -> Vec<CommandStat> {
    let mut stats: Vec<CommandStat> = text
        .lines()
        .filter_map(|l| {
            let (name, values) = l.trim().strip_prefix("cmdstat_")?.split_once(':')?;
            let field = |key: &str| {
                values
                    .split(',')
                    .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            };
            let n = |key: &str| field(key).and_then(|v| v.parse().ok()).unwrap_or(0);
            Some(CommandStat {
                name: name.to_lowercase(),
                calls: n("calls"),
                usec: n("usec"),
                usec_per_call: field("usec_per_call")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0),
                rejected_calls: n("rejected_calls"),
                failed_calls: n("failed_calls"),
            })
        })
        .collect();
    stats.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));
    stats
}

/// Whether a `CONFIG` value is a password: `requirepass`, `masterauth`,
/// and the TLS key passphrases (`tls-key-file-pass`, …).
fn is_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "requirepass" || name == "masterauth" || name.ends_with("-pass")
}

/// The name/value pairs of `CONFIG GET` (an array in RESP2, a map in
/// RESP3), sorted, with set passwords replaced by `****`.
pub fn parse_config(reply: &Value) -> Vec<(String, String)> {
    let pairs: Vec<(String, String)> = match reply {
        Value::Array(items) => items
            .chunks(2)
            .map(|c| {
                (
                    value_text(&c[0]),
                    c.get(1).map(value_text).unwrap_or_default(),
                )
            })
            .collect(),
        Value::Map(entries) => entries
            .iter()
            .map(|(k, v)| (value_text(k), value_text(v)))
            .collect(),
        _ => Vec::new(),
    };
    let mut pairs: Vec<(String, String)> = pairs
        .into_iter()
        .map(|(k, v)| {
            let v = if is_secret(&k) && !v.is_empty() {
                MASK.to_string()
            } else {
                v
            };
            (k, v)
        })
        .collect();
    pairs.sort();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "# Server\r\nredis_version:7.2.4\r\nredis_mode:standalone\r\nuptime_in_seconds:93784\r\n\r\n# Clients\r\nconnected_clients:12\r\nmaxclients:10000\r\nblocked_clients:1\r\n\r\n# Memory\r\nused_memory:1048576\r\nused_memory_human:1.00M\r\nused_memory_peak:2097152\r\nmaxmemory:4194304\r\nmaxmemory_policy:allkeys-lru\r\n\r\n# Persistence\r\nrdb_changes_since_last_save:7\r\nrdb_last_save_time:1700000000\r\nrdb_last_bgsave_status:ok\r\naof_enabled:0\r\n\r\n# Stats\r\ninstantaneous_ops_per_sec:42\r\nkeyspace_hits:90\r\nkeyspace_misses:10\r\nexpired_keys:5\r\nevicted_keys:3\r\n\r\n# Replication\r\nrole:master\r\nconnected_slaves:2\r\n\r\n# Keyspace\r\ndb0:keys=12,expires=3,avg_ttl=100\r\n";

    #[test]
    fn info_fields_by_name() {
        let info = parse_info(INFO);
        assert_eq!(info["redis_version"], "7.2.4");
        assert_eq!(info["used_memory_human"], "1.00M");
        // Keyspace lines keep everything after the first colon.
        assert_eq!(info["db0"], "keys=12,expires=3,avg_ttl=100");
        assert!(!info.keys().any(|k| k.starts_with('#') || k.is_empty()));
    }

    #[test]
    fn summary_of_info() {
        let s = summarize(&parse_info(INFO));
        assert_eq!(s.version, "Redis 7.2.4");
        assert_eq!(s.mode.as_deref(), Some("standalone"));
        assert_eq!(s.uptime_seconds, 93784);
        assert_eq!((s.connected_clients, s.maxclients), (12, Some(10000)));
        assert_eq!(
            (s.used_memory, s.used_memory_peak, s.maxmemory),
            (1 << 20, 2 << 20, 4 << 20)
        );
        assert_eq!(s.maxmemory_policy.as_deref(), Some("allkeys-lru"));
        assert_eq!(s.ops_per_sec, 42.0);
        assert_eq!(s.hit_ratio, Some(0.9));
        assert_eq!((s.expired_keys, s.evicted_keys), (5, 3));
        assert_eq!((s.role.as_str(), s.connected_slaves), ("master", 2));
        assert_eq!(s.rdb_last_save_time, Some(1_700_000_000));
        assert_eq!(s.rdb_changes_since_last_save, 7);
        assert!(!s.aof_enabled);
    }

    #[test]
    fn summary_without_fields_or_reads() {
        let s = summarize(&parse_info(
            "# Server\r\nvalkey_version:8.0.1\r\nredis_version:7.2.4\r\n",
        ));
        assert_eq!(s.version, "Valkey 8.0.1");
        assert_eq!(s.hit_ratio, None);
        assert_eq!(s.maxclients, None);
        assert_eq!(s.role, "master");
        assert_eq!(summarize(&BTreeMap::new()).version, "Redis");
    }

    #[test]
    fn client_list_lines() {
        let text = "id=3 addr=127.0.0.1:52734 laddr=127.0.0.1:6379 fd=8 name= age=120 idle=5 flags=N db=0 sub=0 psub=0 cmd=client|list user=default\n\
                    id=7 addr=10.0.0.2:40000 fd=9 name=worker age=3 idle=0 flags=b db=2 cmd=blpop user=app\n";
        let list = parse_client_list(text, Some(3));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, 3);
        assert_eq!(list[0].name, "");
        assert_eq!(list[0].cmd, "client|list");
        assert!(list[0].own);
        assert_eq!(
            (list[1].name.as_str(), list[1].db, list[1].user.as_str()),
            ("worker", 2, "app")
        );
        assert!(!list[1].own);
        assert_eq!(
            (list[1].age, list[1].idle, list[1].flags.as_str()),
            (3, 0, "b")
        );
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn slowlog_entries_with_passwords_masked() {
        let reply = Value::Array(vec![
            Value::Array(vec![
                Value::Int(14),
                Value::Int(1_700_000_000),
                Value::Int(25_000),
                Value::Array(vec![bulk("KEYS"), bulk("*")]),
                bulk("127.0.0.1:5000"),
                bulk("app"),
            ]),
            // Before Redis 4.0: no client fields.
            Value::Array(vec![
                Value::Int(13),
                Value::Int(1_699_999_999),
                Value::Int(12_000),
                Value::Array(vec![bulk("AUTH"), bulk("hunter2")]),
            ]),
        ]);
        let log = parse_slowlog(&reply);
        assert_eq!(log.len(), 2);
        assert_eq!((log[0].id, log[0].duration_us), (14, 25_000));
        assert_eq!(log[0].command, "KEYS *");
        assert_eq!(log[0].client_name, "app");
        assert!(!log[1].command.contains("hunter2"), "{}", log[1].command);
        assert_eq!(log[1].client, "");
        assert!(parse_slowlog(&Value::Nil).is_empty());
    }

    #[test]
    fn command_stats_most_called_first() {
        let text = "# Commandstats\r\ncmdstat_get:calls=10,usec=30,usec_per_call=3.00,rejected_calls=0,failed_calls=1\r\ncmdstat_client|list:calls=25,usec=500,usec_per_call=20.00,rejected_calls=2,failed_calls=0\r\ncmdstat_set:calls=10,usec=40,usec_per_call=4.00\r\n";
        let stats = parse_command_stats(text);
        let names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["client|list", "get", "set"]);
        assert_eq!(stats[0].usec, 500);
        assert_eq!(stats[0].usec_per_call, 20.0);
        assert_eq!(stats[0].rejected_calls, 2);
        assert_eq!(stats[1].failed_calls, 1);
        // Older servers have no rejected/failed counts.
        assert_eq!(stats[2].failed_calls, 0);
    }

    #[test]
    fn config_hides_passwords() {
        let reply = Value::Array(vec![
            bulk("requirepass"),
            bulk("s3cret"),
            bulk("maxmemory"),
            bulk("0"),
            bulk("masterauth"),
            bulk(""),
            bulk("tls-key-file-pass"),
            bulk("pp"),
        ]);
        let config = parse_config(&reply);
        let get = |k: &str| config.iter().find(|(n, _)| n == k).unwrap().1.as_str();
        assert_eq!(get("requirepass"), "****");
        assert_eq!(get("tls-key-file-pass"), "****");
        // Unset stays visibly unset.
        assert_eq!(get("masterauth"), "");
        assert_eq!(get("maxmemory"), "0");
        assert_eq!(config[0].0, "masterauth");

        let map = Value::Map(vec![
            (bulk("requirepass"), bulk("x")),
            (bulk("port"), bulk("6379")),
        ]);
        assert_eq!(
            parse_config(&map),
            vec![
                ("port".into(), "6379".into()),
                ("requirepass".into(), "****".into())
            ]
        );
    }
}
