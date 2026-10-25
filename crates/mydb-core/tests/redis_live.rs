//! Tests against a real Redis server. They are skipped unless
//! `MYDB_TEST_REDIS` is set to `host:port` (e.g. `127.0.0.1:6379`), with
//! `MYDB_TEST_REDIS_PASSWORD` when the server wants one.
//!
//! Optional: `MYDB_TEST_REDIS_TLS` (`host:port` of a TLS port whose
//! certificate names that host) with `MYDB_TEST_REDIS_CA`, and the
//! `MYDB_TEST_SSH_*` variables of `live.rs` for tunnel tests. They use
//! db 9 and keys starting with `mydbtest:`.

use std::sync::Arc;
use std::time::Duration;

use mydb_core::redis::{KeyName, Reply};
use mydb_core::{
    ConnectionKind, ConnectionProfile, CoreError, RedisSession, SshAuth, SshConfig, SslMode,
};

const DB: u8 = 9;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn profile_at(addr: &str) -> ConnectionProfile {
    let (host, port) = addr.rsplit_once(':').expect("host:port");
    let mut p = ConnectionProfile::new_redis("redis test", host);
    p.port = port.parse().unwrap();
    p.password = env("MYDB_TEST_REDIS_PASSWORD");
    p.redis_db = DB;
    p
}

macro_rules! profile_or_skip {
    () => {
        match env("MYDB_TEST_REDIS") {
            Some(addr) => profile_at(&addr),
            None => {
                eprintln!("MYDB_TEST_REDIS not set, skipping");
                return;
            }
        }
    };
}

fn key(k: &str) -> KeyName {
    KeyName {
        key: k.into(),
        binary: false,
    }
}

/// Runs `text`, asserting it succeeded.
async fn run(s: &RedisSession, text: &str) -> Vec<Reply> {
    let out = s.execute(text).await;
    assert!(out.error.is_none(), "{text}: {:?}", out.error);
    out.results.into_iter().map(|r| r.reply).collect()
}

async fn clean(s: &RedisSession, prefix: &str) {
    let mut cursor = "0".to_string();
    loop {
        let page = s.scan(DB, &cursor, &format!("{prefix}*")).await.unwrap();
        for chunk in page.keys.chunks(50) {
            let names: Vec<&str> = chunk.iter().map(|k| k.name.key.as_str()).collect();
            run(s, &format!("UNLINK {}", names.join(" "))).await;
        }
        if page.cursor == "0" {
            break;
        }
        cursor = page.cursor;
    }
}

#[tokio::test]
async fn browse_keys_and_read_every_type() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    assert!(
        s.info().server_version.starts_with("Redis")
            || s.info().server_version.starts_with("Valkey")
    );
    assert_eq!(s.current_db(), DB);
    let p = "mydbtest:types:";
    clean(&s, p).await;

    run(
        &s,
        &format!(
            r#"SET {p}user:1:profile "{{\"name\":\"Jackie\",\"tags\":[1,2]}}"
HSET {p}user:1:meta plan pro seats 3
RPUSH {p}queue a b c
SADD {p}set x y
ZADD {p}rank 1.5 alice 3 bob
XADD {p}events 1-1 kind login
EXPIRE {p}queue 3600"#
        ),
    )
    .await;

    let page = s.scan(DB, "0", &format!("{p}*")).await.unwrap();
    assert_eq!(page.cursor, "0");
    let mut found: Vec<(String, String, bool)> = page
        .keys
        .iter()
        .map(|k| {
            (
                k.name.key.trim_start_matches(p).to_string(),
                k.kind.clone(),
                k.ttl > 0,
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            ("events".into(), "stream".into(), false),
            ("queue".into(), "list".into(), true),
            ("rank".into(), "zset".into(), false),
            ("set".into(), "set".into(), false),
            ("user:1:meta".into(), "hash".into(), false),
            ("user:1:profile".into(), "string".into(), false),
        ]
    );

    let v = s
        .read_value(DB, &key(&format!("{p}user:1:profile")), "")
        .await
        .unwrap();
    assert_eq!(v.kind, "string");
    assert_eq!(v.text.as_deref(), Some(r#"{"name":"Jackie","tags":[1,2]}"#));

    let v = s
        .read_value(DB, &key(&format!("{p}user:1:meta")), "")
        .await
        .unwrap();
    assert_eq!(v.columns, ["field", "value"]);
    let mut rows = v.rows.clone();
    rows.sort_by_key(|r| r[0].to_string());
    assert_eq!(
        rows,
        [
            vec!["plan".into(), "pro".into()],
            vec!["seats".into(), serde_json::json!("3")]
        ]
    );

    let v = s
        .read_value(DB, &key(&format!("{p}rank")), "")
        .await
        .unwrap();
    assert_eq!(v.columns, ["member", "score"]);
    assert!(
        v.rows.contains(&vec!["alice".into(), "1.5".into()]),
        "{:?}",
        v.rows
    );

    let v = s
        .read_value(DB, &key(&format!("{p}set")), "")
        .await
        .unwrap();
    assert_eq!(v.rows.len(), 2);

    let v = s
        .read_value(DB, &key(&format!("{p}events")), "")
        .await
        .unwrap();
    assert_eq!(
        v.rows,
        [vec![
            serde_json::json!("1-1"),
            serde_json::json!(r#"{"kind":"login"}"#)
        ]]
    );
    assert_eq!(v.next, None);

    let info = s.key_info(DB, &key(&format!("{p}queue"))).await.unwrap();
    assert_eq!((info.kind.as_str(), info.length), ("list", Some(3)));
    assert!(info.ttl > 3000);
    assert!(info.encoding.is_some());
    assert!(info.memory_bytes.unwrap_or(0) > 0);

    let dbs = s.databases().await.unwrap();
    assert!(dbs.len() >= 16);
    assert!(dbs[DB as usize].keys >= 6);

    clean(&s, p).await;
}

#[tokio::test]
async fn large_values_and_key_lists_come_in_pages() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let p = "mydbtest:pages:";
    clean(&s, p).await;

    // 1 200 keys: several SCAN batches.
    let mut lines = Vec::new();
    for chunk in (0..1200).collect::<Vec<_>>().chunks(100) {
        let pairs: Vec<String> = chunk.iter().map(|i| format!("{p}k:{i} v")).collect();
        lines.push(format!("MSET {}", pairs.join(" ")));
    }
    let items: Vec<String> = (0..450).map(|i| i.to_string()).collect();
    lines.push(format!("RPUSH {p}list {}", items.join(" ")));
    run(&s, &lines.join("\n")).await;

    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = "0".to_string();
    let mut batches = 0;
    loop {
        let page = s.scan(DB, &cursor, &format!("{p}k:*")).await.unwrap();
        assert!(page.keys.len() <= 1200);
        seen.extend(page.keys.into_iter().map(|k| k.name.key));
        batches += 1;
        if page.cursor == "0" {
            break;
        }
        cursor = page.cursor;
    }
    assert_eq!(seen.len(), 1200);
    assert!(batches >= 2, "loaded in {batches} batch(es)");

    let list = key(&format!("{p}list"));
    let first = s.read_value(DB, &list, "").await.unwrap();
    assert_eq!(first.rows.len(), 200);
    assert_eq!(
        first.rows[0],
        vec![serde_json::json!(0), serde_json::json!("0")]
    );
    let third = s.read_value(DB, &list, "400").await.unwrap();
    assert_eq!(third.rows.len(), 50);
    assert_eq!(third.next, None);
    assert_eq!(first.next.as_deref(), Some("200"));

    clean(&s, p).await;
}

#[tokio::test]
async fn command_lines_run_in_order_and_stop_at_errors() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let out = s
        .execute(
            "# a comment\nSET mydbtest:cmd 1\n\nINCR mydbtest:cmd\nGET mydbtest:nope\nLRANGE mydbtest:cmd 0 -1\nGET mydbtest:cmd",
        )
        .await;
    let replies: Vec<&Reply> = out.results.iter().map(|r| &r.reply).collect();
    assert_eq!(replies.len(), 4);
    assert_eq!(replies[0], &Reply::Status { value: "OK".into() });
    assert_eq!(replies[1], &Reply::Integer { value: 2 });
    assert_eq!(replies[2], &Reply::Nil);
    assert!(matches!(replies[3], Reply::Error { message } if message.contains("WRONGTYPE")));
    assert!(matches!(out.error, Some(CoreError::Redis { .. })));
    assert_eq!(
        out.failed_command.as_deref(),
        Some("LRANGE mydbtest:cmd 0 -1")
    );
    assert_eq!(out.results[1].line, 3);

    // SELECT moves the editor (and records) to another database.
    let out = s.execute("SELECT 10\nDBSIZE").await;
    assert!(out.error.is_none());
    assert_eq!(out.db, 10);
    assert_eq!(s.current_db(), 10);
    run(&s, "SELECT 9\nDEL mydbtest:cmd").await;

    // Unbalanced quotes are refused before anything runs.
    let out = s.execute("GET \"open").await;
    assert!(matches!(out.error, Some(CoreError::Config { .. })));
    assert!(out.results.is_empty());
}

#[tokio::test]
async fn monitor_and_subscribe_leave_the_editor_usable() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let replies = run(
        &s,
        "MONITOR\nPING\nSUBSCRIBE mydbtest:ch\nGET mydbtest:none",
    )
    .await;
    assert_eq!(replies[0], Reply::Status { value: "OK".into() });
    assert_eq!(
        replies[1],
        Reply::Status {
            value: "PONG".into()
        }
    );
    assert_eq!(replies[3], Reply::Nil);
}

#[tokio::test]
async fn records_never_keep_passwords() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let text = "PING\nAUTH someone Zq7-secret-pw";
    let out = s.execute(text).await;
    assert_eq!(out.results.len(), 2);
    let rec = s.record(text, &out);
    let json = serde_json::to_string(&rec).unwrap();
    assert!(!json.contains("Zq7-secret-pw"), "{json}");
    assert_eq!(rec.connection_kind, ConnectionKind::Redis);
    assert_eq!(rec.database.as_deref(), Some("db9"));
    assert_eq!(rec.sql, "PING\nAUTH someone ****");
    assert_eq!(rec.statements[0].info, "status");
    assert!(rec.error_message.is_some());
    assert!(!format!("{:?}", out.error).contains("Zq7-secret-pw"));
}

#[tokio::test]
async fn stop_ends_a_blocking_command() {
    let profile = profile_or_skip!();
    let s = Arc::new(RedisSession::connect(&profile).await.unwrap());
    let running = {
        let s = s.clone();
        tokio::spawn(async move { s.execute("BLPOP mydbtest:never 30\nPING").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.cancel());
    let out = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("stopped in time")
        .unwrap();
    assert!(out.cancelled);
    assert!(matches!(out.error, Some(CoreError::Cancelled)));
    assert_eq!(
        out.failed_command.as_deref(),
        Some("BLPOP mydbtest:never 30")
    );
    assert!(!s.cancel(), "nothing is running any more");
    // The next run gets a fresh connection.
    assert_eq!(
        run(&s, "PING").await,
        [Reply::Status {
            value: "PONG".into()
        }]
    );
}

#[tokio::test]
async fn reconnects_after_the_server_drops_the_connection() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let editor_id = run(&s, "CLIENT ID").await[0].to_text();
    let browse_id = s.info().connection_id;
    // Both connections of the session, as a server timeout would. Only
    // these, since other tests run at the same time.
    let admin = RedisSession::connect(&profile).await.unwrap();
    run(
        &admin,
        &format!("CLIENT KILL ID {browse_id}\nCLIENT KILL ID {editor_id}"),
    )
    .await;
    // Browsing reconnects by itself.
    s.databases().await.unwrap();

    // The editor reports the lost connection and doesn't send the command
    // again on a new one: it may have run already.
    run(&admin, "DEL mydbtest:resend").await;
    let out = s.execute("INCR mydbtest:resend\nPING").await;
    match &out.error {
        Some(CoreError::Connection { message }) => {
            assert!(message.contains("沒有自動重送"), "{message}")
        }
        other => panic!("expected a connection error, got {other:?}"),
    }
    assert!(out.results.is_empty());
    assert_eq!(out.failed_command.as_deref(), Some("INCR mydbtest:resend"));

    // The next run opens a new connection, in the profile's database.
    let out = s.execute("GET mydbtest:resend\nCLIENT INFO").await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.reconnected);
    assert_eq!(out.results[0].reply, Reply::Nil, "INCR was sent again");
    let client = out.results[1].reply.to_text();
    assert!(client.contains(&format!("db={DB}")), "{client}");
}

#[tokio::test]
async fn transactions_get_only_the_typed_commands() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let replies = run(
        &s,
        "DEL mydbtest:tx\nMULTI\nSET mydbtest:tx 1\nINCR mydbtest:tx\nEXEC\nDEL mydbtest:tx",
    )
    .await;
    assert_eq!(replies[1], Reply::Status { value: "OK".into() });
    assert_eq!(
        replies[2],
        Reply::Status {
            value: "QUEUED".into()
        }
    );
    assert_eq!(
        replies[3],
        Reply::Status {
            value: "QUEUED".into()
        }
    );
    assert_eq!(
        replies[4],
        Reply::Array {
            items: vec![
                Reply::Status { value: "OK".into() },
                Reply::Integer { value: 2 }
            ]
        }
    );
}

#[tokio::test]
async fn hex_escapes_send_single_bytes() {
    let profile = profile_or_skip!();
    let s = RedisSession::connect(&profile).await.unwrap();
    let replies = run(
        &s,
        r#"SET mydbtest:hex "\xff"
STRLEN mydbtest:hex
GET mydbtest:hex
DEL mydbtest:hex"#,
    )
    .await;
    assert_eq!(replies[1], Reply::Integer { value: 1 });
    assert_eq!(
        replies[2],
        Reply::Text {
            value: "FF".into(),
            binary: true
        }
    );
}

#[tokio::test]
async fn wrong_password_is_refused() {
    let mut profile = profile_or_skip!();
    profile.password = Some("definitely-wrong".into());
    profile.user = "nobody".into();
    match RedisSession::connect(&profile).await {
        Err(CoreError::Redis { message }) | Err(CoreError::Connection { message }) => {
            assert!(!message.contains("definitely-wrong"), "{message}");
        }
        Err(e) => panic!("unexpected error {e:?}"),
        Ok(_) => panic!("connected with a wrong password"),
    }
}

fn tls_profile() -> Option<ConnectionProfile> {
    let mut p = profile_at(&env("MYDB_TEST_REDIS_TLS")?);
    p.ssl.ca_path = Some(env("MYDB_TEST_REDIS_CA")?);
    p.ssl.mode = SslMode::VerifyIdentity;
    Some(p)
}

#[tokio::test]
async fn tls_modes() {
    let Some(profile) = tls_profile() else {
        eprintln!("MYDB_TEST_REDIS_TLS not set, skipping");
        return;
    };
    let s = RedisSession::connect(&profile).await.unwrap();
    assert!(s.info().tls);
    assert_eq!(
        run(&s, "PING").await,
        [Reply::Status {
            value: "PONG".into()
        }]
    );

    // Without the CA the certificate isn't trusted…
    let mut no_ca = profile.clone();
    no_ca.ssl.ca_path = None;
    assert!(RedisSession::connect(&no_ca).await.is_err());
    // … unless only encryption is asked for.
    no_ca.ssl.mode = SslMode::Required;
    RedisSession::connect(&no_ca).await.unwrap();

    // By IP the name doesn't match: verify_ca still connects, verify_identity doesn't.
    let (_, port) = env("MYDB_TEST_REDIS_TLS")
        .unwrap()
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.to_string()))
        .unwrap();
    let mut by_ip = profile.clone();
    by_ip.host = "127.0.0.1".into();
    by_ip.port = port.parse().unwrap();
    assert!(RedisSession::connect(&by_ip).await.is_err());
    by_ip.ssl.mode = SslMode::VerifyCa;
    RedisSession::connect(&by_ip).await.unwrap();

    // Plain TCP to a TLS port fails instead of hanging.
    let mut plain = profile.clone();
    plain.ssl.mode = SslMode::Disabled;
    assert!(
        tokio::time::timeout(Duration::from_secs(40), RedisSession::connect(&plain))
            .await
            .unwrap()
            .is_err()
    );
}

fn ssh_config() -> Option<SshConfig> {
    let host = env("MYDB_TEST_SSH_HOST")?;
    let auth = match env("MYDB_TEST_SSH_KEY") {
        Some(key_path) => SshAuth::Key {
            key_path,
            passphrase: None,
        },
        None => SshAuth::Password {
            password: env("MYDB_TEST_SSH_PASSWORD")?,
        },
    };
    Some(SshConfig {
        host,
        port: env("MYDB_TEST_SSH_PORT").map_or(22, |v| v.parse().unwrap()),
        user: env("MYDB_TEST_SSH_USER")?,
        auth,
        known_host_fingerprint: None,
    })
}

#[tokio::test]
async fn through_an_ssh_tunnel() {
    let mut profile = profile_or_skip!();
    let Some(ssh) = ssh_config() else {
        eprintln!("MYDB_TEST_SSH_HOST not set, skipping");
        return;
    };
    profile.ssh = Some(ssh);
    let s = RedisSession::connect(&profile).await.unwrap();
    assert!(
        s.info()
            .ssh_host_fingerprint
            .as_deref()
            .unwrap()
            .starts_with("SHA256:")
    );
    run(&s, "SET mydbtest:ssh 1").await;
    let page = s.scan(DB, "0", "mydbtest:ssh").await.unwrap();
    assert_eq!(page.keys.len(), 1);
    run(&s, "DEL mydbtest:ssh").await;
}

#[tokio::test]
async fn ssh_tunnel_with_tls_checks_real_host_name() {
    let (Some(mut profile), Some(ssh)) = (tls_profile(), ssh_config()) else {
        return;
    };
    profile.ssh = Some(ssh);
    // The certificate names the host as seen from the jump host, while the
    // app connects to its local tunnel port.
    let s = RedisSession::connect(&profile).await.unwrap();
    assert!(s.info().tls);
    run(&s, "PING").await;
    let mut wrong = profile.clone();
    wrong.host = "127.0.0.1".into();
    assert!(RedisSession::connect(&wrong).await.is_err());
}
