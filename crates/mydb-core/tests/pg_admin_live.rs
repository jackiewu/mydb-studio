//! Server administration against a real PostgreSQL server. Skipped unless
//! `MYDB_PG_HOST` is set; see `pg_live.rs` for the other variables. Needs a
//! user that may create roles.

use mydb_core::pg_admin::{self, PgAdminChange, PgObjectKind, PgTarget};
use mydb_core::{ConnectionProfile, PgSession};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn base_profile() -> Option<ConnectionProfile> {
    let host = env("MYDB_PG_HOST")?;
    let mut p = ConnectionProfile::new_postgres("pg-admin-test", host);
    p.port = env("MYDB_PG_PORT").map_or(5432, |v| v.parse().unwrap());
    p.user = env("MYDB_PG_USER").unwrap_or("postgres".into());
    p.password = env("MYDB_PG_PASSWORD");
    if let Some(db) = env("MYDB_PG_DATABASE") {
        p.database = Some(db);
    }
    Some(p)
}

#[tokio::test]
async fn overview_settings_and_activity() {
    let Some(p) = base_profile() else { return };
    let s = PgSession::connect(&p).await.unwrap();
    let o = pg_admin::overview(&s).await.unwrap();
    assert!(o.version.starts_with("PostgreSQL"));
    assert!(o.connections >= 1 && o.max_connections > 0);
    assert!(
        o.databases
            .iter()
            .any(|d| d.name == o.current_database && d.size_bytes.is_some())
    );

    let settings = pg_admin::settings(&s).await.unwrap();
    let wm = settings.iter().find(|x| x.name == "work_mem").unwrap();
    assert_eq!(wm.unit, "kB");

    let me = s.info().connection_id as i32;
    let list = pg_admin::activity(&s).await.unwrap();
    assert!(list.iter().any(|a| a.pid == me && a.state == "active"));
    // Its own connection is refused.
    assert!(
        pg_admin::apply(&s, &PgAdminChange::Terminate { pid: me })
            .await
            .is_err()
    );

    // A second connection with a long query, cancelled from the first.
    let other = PgSession::connect(&p).await.unwrap();
    let pid = other.info().connection_id as i32;
    let run = tokio::spawn(async move { other.execute("SELECT pg_sleep(30)", 0).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    pg_admin::apply(&s, &PgAdminChange::Cancel { pid })
        .await
        .unwrap();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
    assert!(out.error.is_some());

    // `None` without the extension, a list with it.
    let top = pg_admin::top_statements(&s).await.unwrap();
    assert_eq!(top.is_some(), o.has_pg_stat_statements);
}

#[tokio::test]
async fn roles_and_privileges() {
    let Some(p) = base_profile() else { return };
    let s = PgSession::connect(&p).await.unwrap();
    s.execute(
        "DROP SCHEMA IF EXISTS a_priv CASCADE; DROP ROLE IF EXISTS a_app; DROP ROLE IF EXISTS a_readers;
         CREATE SCHEMA a_priv; CREATE TABLE a_priv.t (id int); CREATE SEQUENCE a_priv.s;
         CREATE FUNCTION a_priv.f(n int) RETURNS int LANGUAGE sql AS 'SELECT n';
         CREATE PROCEDURE a_priv.p(n int) LANGUAGE sql AS 'SELECT n';
         CREATE ROLE a_readers NOLOGIN;",
        0,
    )
    .await;
    let ok = |c: PgAdminChange| {
        let s = &s;
        async move { pg_admin::apply(s, &c).await.unwrap() }
    };
    let ran = ok(PgAdminChange::CreateRole {
        name: "a_app".into(),
        password: Some("s3cret pw".into()),
        login: true,
        create_db: false,
        create_role: false,
        connection_limit: Some(5),
        valid_until: None,
        member_of: vec!["a_readers".into()],
    })
    .await;
    assert!(ran[0].contains("PASSWORD '********'"), "{ran:?}");
    // The stored password is the SCRAM verifier, and it works.
    let stored = s
        .query_text("SELECT rolpassword FROM pg_authid WHERE rolname = 'a_app'")
        .await
        .unwrap();
    assert!(
        stored.rows[0][0]
            .as_deref()
            .unwrap()
            .starts_with("SCRAM-SHA-256$")
    );
    let mut login = p.clone();
    login.user = "a_app".into();
    login.password = Some("s3cret pw".into());
    PgSession::connect(&login).await.unwrap();

    let roles = pg_admin::roles(&s).await.unwrap();
    let app = roles.iter().find(|r| r.name == "a_app").unwrap();
    assert!(app.can_login && app.connection_limit == 5);
    assert_eq!(app.member_of, ["a_readers"]);

    let target = |kind, schema: Option<&str>, name: Option<&str>| PgTarget {
        kind,
        schema: schema.map(Into::into),
        name: name.map(Into::into),
    };
    let grant = |t: PgTarget, privs: &[&str], opt: bool| PgAdminChange::Grant {
        role: "a_app".into(),
        target: t,
        privileges: privs.iter().map(|s| s.to_string()).collect(),
        grant_option: opt,
    };
    let db = o_db(&s).await;
    ok(grant(
        target(PgObjectKind::Database, None, Some(&db)),
        &["CONNECT", "TEMP"],
        false,
    ))
    .await;
    ok(grant(
        target(PgObjectKind::Schema, None, Some("a_priv")),
        &["USAGE"],
        false,
    ))
    .await;
    ok(grant(
        target(PgObjectKind::AllTables, Some("a_priv"), None),
        &["SELECT", "INSERT"],
        true,
    ))
    .await;
    ok(grant(
        target(PgObjectKind::Sequence, Some("a_priv"), Some("s")),
        &["USAGE"],
        false,
    ))
    .await;
    ok(grant(
        target(PgObjectKind::Function, Some("a_priv"), Some("f(integer)")),
        &["EXECUTE"],
        false,
    ))
    .await;
    // A procedure, named as the schema page lists it.
    ok(grant(
        target(
            PgObjectKind::Function,
            Some("a_priv"),
            Some("p(IN n integer)"),
        ),
        &["EXECUTE"],
        false,
    ))
    .await;
    ok(PgAdminChange::DefaultPrivileges {
        role: "a_app".into(),
        schema: Some("a_priv".into()),
        object: "tables".into(),
        privileges: vec!["SELECT".into()],
        revoke: false,
        for_role: None,
    })
    .await;

    let privs = pg_admin::privileges(&s, "a_app").await.unwrap();
    let find = |kind: PgObjectKind, name: &str| {
        privs
            .grants
            .iter()
            .find(|g| g.target.kind == kind && g.target.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{kind:?} {name}: {:?}", privs.grants))
    };
    assert_eq!(
        find(PgObjectKind::Database, &db).privileges,
        ["CONNECT", "TEMPORARY"]
    );
    assert_eq!(find(PgObjectKind::Schema, "a_priv").privileges, ["USAGE"]);
    let t = find(PgObjectKind::Table, "t");
    assert_eq!(t.privileges, ["INSERT", "SELECT"]);
    assert_eq!(t.grantable, ["INSERT", "SELECT"]);
    assert_eq!(find(PgObjectKind::Sequence, "s").privileges, ["USAGE"]);
    assert_eq!(
        find(PgObjectKind::Function, "f(n integer)").privileges,
        ["EXECUTE"]
    );
    assert_eq!(privs.defaults.len(), 1);
    assert_eq!(privs.defaults[0].object, "tables");
    assert_eq!(privs.defaults[0].schema.as_deref(), Some("a_priv"));

    // A function named as it is listed (with argument names) resolves too.
    ok(PgAdminChange::Revoke {
        role: "a_app".into(),
        target: target(PgObjectKind::Function, Some("a_priv"), Some("f(n integer)")),
        privileges: vec![],
    })
    .await;
    ok(PgAdminChange::Revoke {
        role: "a_app".into(),
        target: target(PgObjectKind::Table, Some("a_priv"), Some("t")),
        privileges: vec!["INSERT".into()],
    })
    .await;
    let privs = pg_admin::privileges(&s, "a_app").await.unwrap();
    let funcs: Vec<_> = privs
        .grants
        .iter()
        .filter(|g| g.target.kind == PgObjectKind::Function)
        .map(|g| g.target.name.clone().unwrap())
        .collect();
    assert_eq!(funcs, ["p(IN n integer)"]);
    let t = privs
        .grants
        .iter()
        .find(|g| g.target.name.as_deref() == Some("t"))
        .unwrap();
    assert_eq!(t.privileges, ["SELECT"]);

    ok(PgAdminChange::AlterRole {
        name: "a_app".into(),
        login: false,
        create_db: true,
        create_role: false,
        connection_limit: -1,
        valid_until: Some("2099-01-01".into()),
    })
    .await;
    ok(PgAdminChange::RevokeRole {
        role: "a_readers".into(),
        member: "a_app".into(),
    })
    .await;
    let roles = pg_admin::roles(&s).await.unwrap();
    let app = roles.iter().find(|r| r.name == "a_app").unwrap();
    assert!(!app.can_login && app.create_db && app.member_of.is_empty());
    assert!(
        app.valid_until
            .as_deref()
            .unwrap()
            .starts_with("2099-01-01")
    );

    // It owns a table: a plain DROP ROLE fails and changes nothing...
    s.execute("ALTER TABLE a_priv.t OWNER TO a_app", 0).await;
    assert_eq!(pg_admin::privileges(&s, "a_app").await.unwrap().owned, 1);
    let err = pg_admin::apply(
        &s,
        &PgAdminChange::DropRole {
            name: "a_app".into(),
            reassign_to: None,
        },
    )
    .await;
    assert!(err.is_err());
    // ...handing its objects over first works.
    ok(PgAdminChange::DropRole {
        name: "a_app".into(),
        reassign_to: Some(p.user.clone()),
    })
    .await;
    assert!(
        !pg_admin::roles(&s)
            .await
            .unwrap()
            .iter()
            .any(|r| r.name == "a_app")
    );
    s.execute("DROP SCHEMA a_priv CASCADE; DROP ROLE a_readers", 0)
        .await;
}

async fn o_db(s: &PgSession) -> String {
    pg_admin::overview(s).await.unwrap().current_database
}

/// Needs `shared_preload_libraries = 'pg_stat_statements'`; skipped
/// otherwise.
#[tokio::test]
async fn top_statements_when_installed() {
    let Some(p) = base_profile() else { return };
    let s = PgSession::connect(&p).await.unwrap();
    let preload = s
        .query_text("SELECT current_setting('shared_preload_libraries')")
        .await
        .unwrap();
    if !preload.rows[0][0]
        .as_deref()
        .unwrap_or("")
        .contains("pg_stat_statements")
    {
        eprintln!("pg_stat_statements not preloaded, skipping");
        return;
    }
    s.execute("CREATE EXTENSION IF NOT EXISTS pg_stat_statements", 0)
        .await;
    s.execute("SELECT count(*) FROM pg_class", 0).await;
    let list = pg_admin::top_statements(&s).await.unwrap().unwrap();
    assert!(list.iter().any(|x| x.calls > 0 && !x.query.is_empty()));
}
