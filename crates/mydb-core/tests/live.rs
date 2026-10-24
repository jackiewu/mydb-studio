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

async fn connection_id(session: &Session) -> String {
    let out = session.execute("SELECT CONNECTION_ID()", 1).await;
    out.results[0].rows[0][0].as_str().unwrap().to_string()
}

/// Simulates the server dropping an idle connection (`wait_timeout`).
async fn kill_from_outside(profile: &ConnectionProfile, id: &str) {
    let admin = Session::connect(profile).await.unwrap();
    let out = admin.execute(&format!("KILL {id}"), 1).await;
    assert!(out.error.is_none(), "{:?}", out.error);
    admin.close().await.unwrap();
}

#[tokio::test]
async fn reconnects_after_server_drops_connection() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    let out = session.execute("USE demo", 1).await;
    assert!(out.error.is_none() && !out.reconnected);
    let before = connection_id(&session).await;

    kill_from_outside(&profile, &before).await;

    let out = session.execute("SELECT DATABASE()", 1).await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.reconnected);
    // The database chosen with USE survives the reconnect.
    assert_eq!(out.results[0].rows[0][0], serde_json::json!("demo"));
    assert_ne!(connection_id(&session).await, before);
}

#[tokio::test]
async fn reconnects_through_ssh_tunnel() {
    let mut profile = profile_or_skip!();
    let Some(ssh) = ssh_config() else {
        return;
    };
    let direct = profile.clone();
    profile.ssh = Some(ssh);
    let session = Session::connect(&profile).await.unwrap();
    let before = connection_id(&session).await;
    kill_from_outside(&direct, &before).await;
    let out = session.execute("SELECT 1", 1).await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.reconnected);
}

#[tokio::test]
async fn lists_databases_and_tables() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    session
        .execute(
            "CREATE TABLE IF NOT EXISTS demo.mydb_list_a (id INT PRIMARY KEY);
             CREATE OR REPLACE VIEW demo.mydb_list_v AS SELECT 1 AS x;",
            1,
        )
        .await;
    let dbs = session.list_databases().await.unwrap();
    let demo = dbs.iter().find(|d| d.name == "demo").expect("demo listed");
    assert!(demo.table_count >= 2);
    assert!(dbs.iter().any(|d| d.name == "information_schema"));

    let tables = session.list_tables("demo").await.unwrap();
    let a = tables.iter().find(|t| t.name == "mydb_list_a").unwrap();
    assert_eq!(a.kind, "BASE TABLE");
    assert!(a.engine.is_some());
    let v = tables.iter().find(|t| t.name == "mydb_list_v").unwrap();
    assert_eq!(v.kind, "VIEW");
    session
        .execute("DROP VIEW demo.mydb_list_v; DROP TABLE demo.mydb_list_a", 1)
        .await;
}

#[tokio::test]
async fn execution_becomes_a_record() {
    use mydb_core::{ExecutionRecord, RecordContext, RecordFilter, Store};
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    let sql = "SELECT 1 AS a UNION SELECT 2; SELECT * FROM no_such_db.t";
    let out = session.execute(sql, 10).await;
    let rec = ExecutionRecord::from_outcome(
        RecordContext {
            connection_id: &profile.id,
            connection_name: &profile.name,
            server: format!("{}@{}:{}", profile.user, profile.host, profile.port),
            server_version: &session.info().server_version,
            database: session.current_database(),
        },
        sql,
        &out,
    );
    assert_eq!(rec.statement_count, 2);
    assert_eq!(rec.returned_rows, 2);
    assert!(rec.error_code.is_some());
    assert_eq!(rec.statements[0].columns, ["a"]);
    assert_eq!(rec.statements[0].preview.len(), 2);

    let store = Store::open_in_memory().unwrap();
    let id = store.insert_record(&rec).unwrap();
    let back = store.get_record(id).unwrap().unwrap();
    assert_eq!(back.statements, rec.statements);
    assert_eq!(
        store
            .list_records(&RecordFilter {
                only_errors: true,
                ..Default::default()
            })
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn use_database_survives_reconnect_and_lists_columns() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    session
        .execute(
            "CREATE TABLE IF NOT EXISTS demo.mydb_cols (
                id BIGINT UNSIGNED PRIMARY KEY,
                name VARCHAR(40) NULL COMMENT '名稱'
             )",
            1,
        )
        .await;

    session.use_database("demo").await.unwrap();
    assert_eq!(session.current_database().as_deref(), Some("demo"));
    let before = connection_id(&session).await;
    kill_from_outside(&profile, &before).await;
    let out = session.execute("SELECT DATABASE()", 1).await;
    assert!(out.reconnected);
    assert_eq!(out.results[0].rows[0][0], serde_json::json!("demo"));

    let all = session.list_all_tables().await.unwrap();
    assert!(all.contains(&("demo".into(), "mydb_cols".into())));
    assert!(!all.iter().any(|(db, _)| db == "information_schema"));

    let cols: Vec<_> = session
        .list_columns("demo")
        .await
        .unwrap()
        .into_iter()
        .filter(|c| c.table == "mydb_cols")
        .collect();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0].name, "id");
    assert_eq!(cols[0].key, "PRI");
    assert!(cols[0].column_type.contains("unsigned"));
    assert!(!cols[0].nullable && cols[1].nullable);
    assert_eq!(cols[1].comment, "名稱");

    assert!(session.use_database("no_such_db_xyz").await.is_err());
    assert_eq!(session.current_database().as_deref(), Some("demo"));
    session.execute("DROP TABLE demo.mydb_cols", 1).await;
}

#[tokio::test]
async fn automatic_limit_caps_plain_queries() {
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    let sql = "SELECT COLLATION_NAME FROM information_schema.COLLATIONS; SELECT COLLATION_NAME FROM information_schema.COLLATIONS LIMIT 20; SELECT 1";
    let out = session.execute_limited(sql, 100, Some(5)).await;
    assert!(out.error.is_none(), "{:?}", out.error);
    let r = &out.results;
    assert_eq!((r[0].total_rows, r[0].limit_added), (5, Some(5)));
    assert_eq!(
        r[0].sql,
        "SELECT COLLATION_NAME FROM information_schema.COLLATIONS"
    );
    assert_eq!((r[1].total_rows, r[1].limit_added), (20, None));
    assert_eq!(r[2].limit_added, None);
    session.close().await.unwrap();
}

#[tokio::test]
async fn server_admin_overview_users_and_privileges() {
    use mydb_core::admin::{self, AdminChange, GrantTarget, PrivilegeEdit};
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();

    let overview = admin::overview(&session).await.unwrap();
    assert!(overview.status.contains_key("Uptime"));
    assert!(overview.variables.contains_key("max_connections"));
    assert!(!overview.current_user.is_empty());
    let me = session.info().connection_id as u64;
    assert!(
        admin::processes(&session)
            .await
            .unwrap()
            .iter()
            .any(|p| p.id == me)
    );

    let (user, host) = ("mydb_admin_test", "%");
    let _ = admin::apply(
        &session,
        &AdminChange::DropUser {
            user: user.into(),
            host: host.into(),
        },
    )
    .await;
    let shop = GrantTarget {
        database: Some("demo".into()),
        table: None,
    };
    let shown = admin::apply(
        &session,
        &AdminChange::CreateUser {
            user: user.into(),
            host: host.into(),
            password: "p'w\\1".into(),
            privileges: vec![PrivilegeEdit {
                target: shop.clone(),
                before: vec![],
                before_grant_option: false,
                after: vec!["SELECT".into(), "INSERT".into()],
                after_grant_option: false,
            }],
        },
    )
    .await
    .unwrap();
    assert!(shown[0].contains("'********'"));

    // The new password works.
    let mut login = profile.clone();
    login.user = user.into();
    login.password = Some("p'w\\1".into());
    Session::connect(&login)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();

    let grants = admin::grants(&session, user, host).await.unwrap();
    let on_demo = grants
        .iter()
        .find(|g| g.target.as_ref() == Some(&shop))
        .unwrap();
    let mut privs = on_demo.privileges.clone();
    privs.sort();
    assert_eq!(privs, ["INSERT", "SELECT"]);
    assert!(grants.iter().all(|g| !g.text.contains("PASSWORD '")));

    admin::apply(
        &session,
        &AdminChange::Privileges {
            user: user.into(),
            host: host.into(),
            edits: vec![PrivilegeEdit {
                target: shop.clone(),
                before: privs,
                before_grant_option: false,
                after: vec!["SELECT".into(), "UPDATE".into()],
                after_grant_option: true,
            }],
        },
    )
    .await
    .unwrap();
    let grants = admin::grants(&session, user, host).await.unwrap();
    let on_demo = grants
        .iter()
        .find(|g| g.target.as_ref() == Some(&shop))
        .unwrap();
    let mut privs = on_demo.privileges.clone();
    privs.sort();
    assert_eq!(privs, ["SELECT", "UPDATE"]);
    assert!(on_demo.grant_option);

    admin::apply(
        &session,
        &AdminChange::SetLocked {
            user: user.into(),
            host: host.into(),
            locked: true,
        },
    )
    .await
    .unwrap();
    let users = admin::users(&session).await.unwrap();
    let account = users.iter().find(|u| u.user == user).unwrap();
    assert!(account.locked);
    assert!(Session::connect(&login).await.is_err());

    admin::apply(
        &session,
        &AdminChange::DropUser {
            user: user.into(),
            host: host.into(),
        },
    )
    .await
    .unwrap();
    assert!(
        !admin::users(&session)
            .await
            .unwrap()
            .iter()
            .any(|u| u.user == user)
    );
}

#[tokio::test]
async fn cancel_stops_the_running_statement_and_the_rest() {
    let profile = profile_or_skip!();
    let session = std::sync::Arc::new(Session::connect(&profile).await.unwrap());
    assert!(!session.cancel().await.unwrap(), "nothing to cancel yet");

    let runner = session.clone();
    let run = tokio::spawn(async move {
        runner
            .execute("SELECT 1; SELECT SLEEP(30); SELECT 3;", 10)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let started = std::time::Instant::now();
    assert!(session.cancel().await.unwrap());
    let out = run.await.unwrap();
    assert!(started.elapsed().as_secs() < 5);
    assert!(out.cancelled);
    // SLEEP returns 1 when interrupted rather than failing; either way the
    // third statement must not run.
    assert!(out.results.len() <= 2, "{:?}", out.results);
    assert!(out.error.is_some());

    // The session is usable again and a new run is not marked cancelled.
    let again = session.execute("SELECT 42", 10).await;
    assert!(again.error.is_none() && !again.cancelled);
}

#[tokio::test]
async fn export_streams_every_row() {
    use mydb_core::export::{ExportFormat, ExportOptions, RowWriter};
    let profile = profile_or_skip!();
    let session = Session::connect(&profile).await.unwrap();
    let export = |sql: &'static str| {
        let session = &session;
        async move {
            let mut buf = Vec::new();
            let writer = std::sync::Mutex::new(RowWriter::new(
                &mut buf,
                ExportOptions {
                    format: ExportFormat::Csv,
                    header: true,
                    table: None,
                },
            ));
            let n = session
                .stream_rows(
                    sql,
                    |c| writer.lock().unwrap().begin(c),
                    |r| writer.lock().unwrap().row(&r),
                )
                .await
                .unwrap();
            assert_eq!(writer.into_inner().unwrap().finish().unwrap(), n);
            (n, String::from_utf8(buf).unwrap())
        }
    };
    // More rows than the grid keeps (DEFAULT_MAX_ROWS).
    let (n, csv) =
        export("SELECT seq AS n, CONCAT('row ', seq) AS label FROM demo.seq_1_to_5000").await;
    assert_eq!(n, 5000);
    assert!(csv.ends_with("\r\n5000,row 5000"));
    let (_, csv) =
        export("SELECT seq AS n, CONCAT('row ', seq) AS label FROM demo.seq_1_to_3").await;
    assert_eq!(csv, "n,label\r\n1,row 1\r\n2,row 2\r\n3,row 3");
}
