//! Tests against a real MySQL/MariaDB server. They are skipped unless
//! `MYDB_TEST_HOST` is set; see `scripts/test-env.sh` for a local setup.
//!
//! Optional: `MYDB_TEST_CA` (TLS tests) and `MYDB_TEST_SSH_*` (tunnel tests).

use mydb_core::{ConnectionProfile, CoreError, Session, SshAuth, SshConfig, SslMode};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn base_profile() -> Option<ConnectionProfile> {
    let host = env("MYDB_TEST_HOST")?;
    let mut p =
        ConnectionProfile::new("test", host, env("MYDB_TEST_USER").unwrap_or("root".into()));
    p.port = env("MYDB_TEST_PORT").map_or(3306, |v| v.parse().unwrap());
    p.password = env("MYDB_TEST_PASSWORD");
    Some(p)
}

macro_rules! profile_or_skip {
    () => {
        match base_profile() {
            Some(p) => p,
            None => {
                eprintln!("MYDB_TEST_HOST not set, skipping");
                return;
            }
        }
    };
}

#[tokio::test]
async fn select_dml_and_multi_statement() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    assert!(!session.info().server_version.is_empty());

    let out = session
        .execute(
            "DROP TABLE IF EXISTS demo.mydb_t;
             CREATE TABLE demo.mydb_t (id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(20), big BIGINT UNSIGNED);
             INSERT INTO demo.mydb_t (name, big) VALUES ('a', 1), ('b', 18446744073709551615), ('c', NULL);
             UPDATE demo.mydb_t SET name = 'z' WHERE id > 1;
             SELECT id, name, big FROM demo.mydb_t ORDER BY id;",
            2,
        )
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.results.len(), 5);
    assert!(out.results[2].sql.starts_with("INSERT"));
    assert_eq!(out.results[2].affected_rows, 3);
    assert_eq!(out.results[3].affected_rows, 2);

    let select = &out.results[4];
    let names: Vec<_> = select.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "big"]);
    assert_eq!(select.total_rows, 3);
    assert_eq!(select.rows.len(), 2);
    assert!(select.truncated);
    assert_eq!(select.rows[1][2], serde_json::json!("18446744073709551615"));
    assert!(out.duration_ms > 0.0);

    session.execute("DROP TABLE demo.mydb_t", 10).await;
    session.close().await.unwrap();
}

#[tokio::test]
async fn stored_procedure_with_delimiter() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    let out = session
        .execute(
            "DROP PROCEDURE IF EXISTS demo.mydb_p;
DELIMITER //
CREATE PROCEDURE demo.mydb_p()
BEGIN
  SELECT 1 AS a;
  SELECT 2 AS b, 3 AS c;
END//
DELIMITER ;
CALL demo.mydb_p();
SELECT 'after';",
            10,
        )
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    let sets: Vec<usize> = out.results.iter().map(|r| r.columns.len()).collect();
    // DROP, CREATE, CALL's two result sets, final SELECT.
    assert_eq!(sets, [0, 0, 1, 2, 1]);
    assert_eq!(out.results[4].rows[0][0], serde_json::json!("after"));
    session.execute("DROP PROCEDURE demo.mydb_p", 10).await;
}

#[tokio::test]
async fn error_keeps_completed_statements_and_server_code() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    let out = session
        .execute(
            "SELECT 1; SELECT * FROM no_such_db.no_such_table; SELECT 3",
            10,
        )
        .await;
    assert_eq!(out.results.len(), 1);
    match out.error {
        Some(CoreError::Server { code, .. }) => assert!(code == 1049 || code == 1146),
        other => panic!("expected server error, got {other:?}"),
    }
    assert_eq!(
        out.failed_statement.as_deref(),
        Some("SELECT * FROM no_such_db.no_such_table")
    );
    // The connection is still usable afterwards.
    let again = session.execute("SELECT 2", 10).await;
    assert!(again.error.is_none());
}

#[tokio::test]
async fn bad_password_is_a_server_error() {
    let mut profile = profile_or_skip!();
    profile.password = Some("definitely-wrong".into());
    match Session::connect(&profile).await {
        Err(CoreError::Server { code: 1045, .. }) => {}
        Err(e) => panic!("unexpected error {e:?}"),
        Ok(_) => panic!("connected with a wrong password"),
    }
}

#[tokio::test]
async fn tls_verify_identity_with_ca() {
    let mut profile = profile_or_skip!();
    let Some(ca) = env("MYDB_TEST_CA") else {
        eprintln!("MYDB_TEST_CA not set, skipping");
        return;
    };
    profile.ssl.mode = SslMode::VerifyIdentity;
    profile.ssl.ca_path = Some(ca);
    let session = Session::connect(&profile).await.unwrap();
    assert!(session.info().tls);
    let out = session
        .execute("SHOW SESSION STATUS LIKE 'Ssl_version'", 10)
        .await;
    let version = out.results[0].rows[0][1].as_str().unwrap();
    assert!(version.starts_with("TLS"), "Ssl_version = {version}");
}

#[tokio::test]
async fn tls_verify_ca_rejects_unknown_ca() {
    let mut profile = profile_or_skip!();
    if env("MYDB_TEST_CA").is_none() {
        return;
    }
    // Built-in web roots do not include the test CA.
    profile.ssl.mode = SslMode::VerifyCa;
    assert!(Session::connect(&profile).await.is_err());
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
async fn ssh_tunnel_learns_then_enforces_host_key() {
    let mut profile = profile_or_skip!();
    let Some(ssh) = ssh_config() else {
        eprintln!("MYDB_TEST_SSH_HOST not set, skipping");
        return;
    };
    profile.ssh = Some(ssh);

    let session = Session::connect(&profile).await.unwrap();
    let fingerprint = session.info().ssh_host_fingerprint.clone().unwrap();
    assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
    let out = session.execute("SELECT 42", 10).await;
    assert_eq!(out.results[0].rows[0][0], serde_json::json!("42"));
    session.close().await.unwrap();

    // Remembered key: connects again.
    profile.ssh.as_mut().unwrap().known_host_fingerprint = Some(fingerprint);
    Session::connect(&profile).await.unwrap();

    // Different key on record: refused.
    profile.ssh.as_mut().unwrap().known_host_fingerprint = Some("SHA256:not-this-one".into());
    match Session::connect(&profile).await {
        Err(CoreError::HostKeyMismatch { .. }) => {}
        Err(e) => panic!("expected host key mismatch, got {e:?}"),
        Ok(_) => panic!("connected despite a changed host key"),
    }
}

#[tokio::test]
async fn ssh_tunnel_with_tls_checks_real_host_name() {
    let mut profile = profile_or_skip!();
    let (Some(ssh), Some(ca)) = (ssh_config(), env("MYDB_TEST_CA")) else {
        return;
    };
    profile.ssh = Some(ssh);
    profile.ssl.mode = SslMode::VerifyIdentity;
    profile.ssl.ca_path = Some(ca);
    let session = Session::connect(&profile).await.unwrap();
    assert!(session.info().tls);
}
