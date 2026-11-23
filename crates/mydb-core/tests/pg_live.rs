//! Tests against a real PostgreSQL server. Skipped unless `MYDB_PG_HOST`
//! is set (`MYDB_PG_PORT`, `MYDB_PG_USER`, `MYDB_PG_PASSWORD`,
//! `MYDB_PG_DATABASE` optional). `MYDB_PG_CA` turns on the TLS tests: the
//! CA that signed the server's certificate.

use std::sync::Arc;
use std::time::Duration;

use mydb_core::{ConnectionProfile, CoreError, PgSession, SqlSession, SslMode};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn base_profile() -> Option<ConnectionProfile> {
    let host = env("MYDB_PG_HOST")?;
    let mut p = ConnectionProfile::new_postgres("pg-test", host);
    p.port = env("MYDB_PG_PORT").map_or(5432, |v| v.parse().unwrap());
    p.user = env("MYDB_PG_USER").unwrap_or("postgres".into());
    p.password = env("MYDB_PG_PASSWORD");
    if let Some(db) = env("MYDB_PG_DATABASE") {
        p.database = Some(db);
    }
    Some(p)
}

macro_rules! profile_or_skip {
    () => {
        match base_profile() {
            Some(p) => p,
            None => {
                eprintln!("MYDB_PG_HOST not set, skipping");
                return;
            }
        }
    };
}

async fn setup(s: &PgSession, schema: &str) {
    let out = s
        .execute(
            &format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema};
                 CREATE TABLE {schema}.items (
                   id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                   name text NOT NULL UNIQUE,
                   price numeric(10,2) DEFAULT 0,
                   big bigint, ok boolean, tags text[], meta jsonb
                 );
                 COMMENT ON TABLE {schema}.items IS 'it''s items';
                 CREATE INDEX items_price ON {schema}.items (price);
                 CREATE VIEW {schema}.cheap AS SELECT * FROM {schema}.items WHERE price < 10;"
            ),
            10,
        )
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
}

#[tokio::test]
async fn runs_statements_and_reports_types() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    assert!(s.info().server_version.starts_with("PostgreSQL "));
    setup(&s, "t_run").await;

    let out = s
        .execute(
            "INSERT INTO t_run.items (name, price, big, ok, tags, meta)
             VALUES ('a', 1.5, 9007199254740993, true, '{x,y}', '{\"k\": 1}'), ('b', 20, 1, false, NULL, NULL);
             SELECT id, name, price, big, ok, tags, meta FROM t_run.items ORDER BY id;
             DO $$ BEGIN RAISE NOTICE 'hello; world'; END $$;",
            1,
        )
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.results.len(), 3);
    assert_eq!(out.results[0].affected_rows, 2);
    let sel = &out.results[1];
    assert_eq!(sel.total_rows, 2);
    assert!(sel.truncated);
    assert_eq!(sel.rows.len(), 1);
    let types: Vec<&str> = sel.columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(
        types,
        ["int4", "text", "numeric", "int8", "bool", "_text", "jsonb"]
    );
    let row = &sel.rows[0];
    assert_eq!(row[0], serde_json::json!(1));
    assert_eq!(row[2], serde_json::json!("1.50"));
    assert_eq!(row[3], serde_json::json!("9007199254740993"));
    assert_eq!(row[4], serde_json::json!("true"));
    assert_eq!(row[5], serde_json::json!("{x,y}"));
    assert_eq!(row[6], serde_json::json!("{\"k\": 1}"));
    assert!(
        out.results[2].info.contains("hello; world"),
        "{}",
        out.results[2].info
    );
}

#[tokio::test]
async fn errors_carry_sqlstate_and_stop_the_run() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    let out = s
        .execute("SELECT 1; SELECT * FROM no_such_table; SELECT 3", 10)
        .await;
    assert_eq!(out.results.len(), 1);
    match &out.error {
        Some(CoreError::Postgres { state, message }) => {
            assert_eq!(state, "42P01");
            assert!(message.contains("no_such_table"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        out.failed_statement.as_deref(),
        Some("SELECT * FROM no_such_table")
    );
    assert!(mydb_core::advisor::error_advice(out.error.as_ref().unwrap()).is_some());

    // In a transaction, a failure aborts the rest until ROLLBACK.
    let out = s.execute("BEGIN; SELECT 1/0; SELECT 1", 10).await;
    assert!(matches!(&out.error, Some(CoreError::Postgres { state, .. }) if state == "22012"));
    let out = s.execute("SELECT 1", 10).await;
    assert!(matches!(&out.error, Some(CoreError::Postgres { state, .. }) if state == "25P02"));
    assert!(s.execute("ROLLBACK", 10).await.error.is_none());
}

#[tokio::test]
async fn auto_limit_and_copy_refusal() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    let out = s
        .execute_limited("SELECT g FROM generate_series(1, 50) g", 1000, Some(5))
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.results[0].total_rows, 5);
    assert_eq!(out.results[0].limit_added, Some(5));
    let out = s.execute("COPY (SELECT 1) TO STDOUT", 10).await;
    assert!(matches!(out.error, Some(CoreError::Config { .. })));
    // The connection is still fine afterwards.
    assert!(s.execute("SELECT 1", 10).await.error.is_none());
}

#[tokio::test]
async fn catalog_lookups() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    setup(&s, "t_cat").await;

    let schemas = s.list_databases().await.unwrap();
    let mine = schemas.iter().find(|d| d.name == "t_cat").unwrap();
    assert_eq!(mine.table_count, 2);
    assert!(!schemas.iter().any(|d| d.name == "pg_catalog"));

    let tables = s.list_tables("t_cat").await.unwrap();
    let kinds: Vec<(&str, &str)> = tables
        .iter()
        .map(|t| (t.name.as_str(), t.kind.as_str()))
        .collect();
    assert_eq!(kinds, [("cheap", "VIEW"), ("items", "BASE TABLE")]);
    assert_eq!(tables[1].comment, "it's items");

    let cols = s.list_columns("t_cat").await.unwrap();
    let id = cols
        .iter()
        .find(|c| c.table == "items" && c.name == "id")
        .unwrap();
    assert_eq!(id.key, "PRI");
    assert!(!id.nullable);
    let name = cols
        .iter()
        .find(|c| c.table == "items" && c.name == "name")
        .unwrap();
    assert_eq!(name.key, "UNI");
    let price = cols
        .iter()
        .find(|c| c.table == "items" && c.name == "price")
        .unwrap();
    assert_eq!(price.column_type, "numeric(10,2)");
    assert_eq!(price.key, "MUL");

    let ddl = s.show_create_table(Some("t_cat"), "items").await.unwrap();
    assert!(ddl.starts_with("CREATE TABLE t_cat.items ("), "{ddl}");
    assert!(ddl.contains("GENERATED ALWAYS AS IDENTITY"), "{ddl}");
    assert!(ddl.contains("PRIMARY KEY (id)"), "{ddl}");
    assert!(ddl.contains("CREATE INDEX items_price"), "{ddl}");
    assert!(
        ddl.contains("COMMENT ON TABLE t_cat.items IS 'it''s items';"),
        "{ddl}"
    );
    let view = s.show_create_table(Some("t_cat"), "cheap").await.unwrap();
    assert!(view.starts_with("CREATE VIEW t_cat.cheap AS"), "{view}");

    let all = s.list_all_tables().await.unwrap();
    assert!(all.contains(&("t_cat".into(), "items".into())));
}

#[tokio::test]
async fn schema_choice_survives_a_reconnect() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    setup(&s, "t_path").await;
    s.use_database("t_path").await.unwrap();
    assert_eq!(s.current_database().as_deref(), Some("t_path"));
    let out = s.execute("SELECT count(*) FROM items", 10).await;
    assert!(out.error.is_none(), "{:?}", out.error);

    // Kill the connection from outside.
    let pid = s.info().connection_id;
    let other = PgSession::connect(&profile).await.unwrap();
    let out = other
        .execute(&format!("SELECT pg_terminate_backend({pid})"), 10)
        .await;
    assert!(out.error.is_none(), "{:?}", out.error);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let out = s.execute("SELECT count(*) FROM items", 10).await;
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.reconnected);
    assert_eq!(s.current_database().as_deref(), Some("t_path"));

    // SET search_path in the editor is picked up too.
    let out = s.execute("SET search_path TO public", 10).await;
    assert!(out.error.is_none());
    assert_eq!(s.current_database().as_deref(), Some("public"));
}

#[tokio::test]
async fn cancel_stops_a_long_statement() {
    let profile = profile_or_skip!();
    let s = Arc::new(PgSession::connect(&profile).await.unwrap());
    let runner = s.clone();
    let run =
        tokio::spawn(async move { runner.execute("SELECT pg_sleep(30); SELECT 2", 10).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(s.cancel().await.unwrap());
    let out = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("cancel took too long")
        .unwrap();
    assert!(out.cancelled);
    assert!(
        matches!(out.error, Some(CoreError::Cancelled)),
        "{:?}",
        out.error
    );
    assert!(out.results.is_empty());
    assert!(s.execute("SELECT 1", 10).await.error.is_none());
}

#[tokio::test]
async fn export_stream_takes_one_statement() {
    let profile = profile_or_skip!();
    let s = SqlSession::connect(&profile).await.unwrap();
    let mut rows = Vec::new();
    let n = s
        .stream_rows(
            "SELECT g, g * 2 AS d FROM generate_series(1, 3) g",
            |c| {
                assert_eq!(c.len(), 2);
                Ok(())
            },
            |r| {
                rows.push(r);
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(n, 3);
    assert_eq!(rows[2], vec![serde_json::json!(3), serde_json::json!(6)]);
    // A second statement hidden after `;` is refused by the server.
    let err = s
        .stream_rows("SELECT 1; SELECT 2", |_| Ok(()), |_| Ok(()))
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Postgres { .. }), "{err:?}");
}

#[tokio::test]
async fn read_only_transaction_blocks_writes() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    setup(&s, "t_ro").await;
    assert!(
        s.execute("START TRANSACTION READ ONLY", 0)
            .await
            .error
            .is_none()
    );
    let out = s
        .execute("INSERT INTO t_ro.items (name) VALUES ('x')", 0)
        .await;
    assert!(matches!(&out.error, Some(CoreError::Postgres { state, .. }) if state == "25006"));
    assert!(s.execute("ROLLBACK", 0).await.error.is_none());
}

#[tokio::test]
async fn transaction_runs_all_or_nothing() {
    let profile = profile_or_skip!();
    let s = PgSession::connect(&profile).await.unwrap();
    setup(&s, "t_tx").await;
    let out = s
        .run_in_transaction(&[
            "ALTER TABLE t_tx.items ADD COLUMN extra int".into(),
            "ALTER TABLE t_tx.items ADD COLUMN extra int".into(),
        ])
        .await;
    assert!(out.error.is_some());
    let cols = s.list_columns("t_tx").await.unwrap();
    assert!(!cols.iter().any(|c| c.name == "extra"));
}

#[tokio::test]
async fn tls_modes() {
    let Some(ca) = env("MYDB_PG_CA") else {
        eprintln!("MYDB_PG_CA not set, skipping");
        return;
    };
    let mut profile = profile_or_skip!();
    profile.ssl.mode = SslMode::Required;
    let s = PgSession::connect(&profile).await.unwrap();
    assert!(s.info().tls);
    let out = s
        .execute(
            "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            10,
        )
        .await;
    assert_eq!(out.results[0].rows[0][0], serde_json::json!("true"));

    profile.ssl.mode = SslMode::VerifyCa;
    profile.ssl.ca_path = Some(ca.clone());
    PgSession::connect(&profile).await.unwrap();

    // Without the CA, verification fails.
    profile.ssl.ca_path = None;
    assert!(PgSession::connect(&profile).await.is_err());
}
