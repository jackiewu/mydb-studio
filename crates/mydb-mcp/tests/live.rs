//! Runs the MCP tools against a real server; skipped unless
//! `MYDB_TEST_HOST` is set (see mydb-core's live tests).

use mydb_core::{ConnectionProfile, McpAccess, Store};
use mydb_mcp::{RECORD_NOTE, Server};
use serde_json::{Value, json};

fn profile(name: &str, access: McpAccess) -> Option<ConnectionProfile> {
    let host = std::env::var("MYDB_TEST_HOST")
        .ok()
        .filter(|h| !h.is_empty())?;
    let user = std::env::var("MYDB_TEST_USER").unwrap_or("root".into());
    let mut p = ConnectionProfile::new(name, host, user);
    p.port = std::env::var("MYDB_TEST_PORT").map_or(3306, |v| v.parse().unwrap());
    p.password = std::env::var("MYDB_TEST_PASSWORD").ok();
    p.database = Some("demo".into());
    p.mcp = access;
    Some(p)
}

async fn call(s: &Server, tool: &str, args: Value) -> Value {
    let msg = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    s.handle_line(&msg.to_string()).await.unwrap()["result"].clone()
}

#[tokio::test]
async fn read_only_and_read_write_connections() {
    let (Some(ro), Some(rw)) = (
        profile("reader", McpAccess::ReadOnly),
        profile("writer", McpAccess::ReadWrite),
    ) else {
        eprintln!("MYDB_TEST_HOST not set, skipping");
        return;
    };
    let path = std::env::temp_dir().join(format!("mydb-mcp-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = Store::open(&path).unwrap();
    store.save_connection(&ro).unwrap();
    store.save_connection(&rw).unwrap();
    let s = Server::new(store);

    let setup = call(
        &s,
        "run_query",
        json!({"connection":"writer","sql":"DROP TABLE IF EXISTS mcp_t; CREATE TABLE mcp_t (id INT PRIMARY KEY, v TEXT); INSERT INTO mcp_t VALUES (1,'a'),(2,'b'),(3,'c')"}),
    )
    .await;
    assert!(setup.get("isError").is_none(), "{setup}");
    assert_eq!(
        setup["structuredContent"]["statements"][2]["affected_rows"],
        3
    );

    let read = call(
        &s,
        "run_query",
        json!({"connection":"READER","sql":"SELECT id, v FROM mcp_t ORDER BY id","max_rows":2}),
    )
    .await;
    let st = &read["structuredContent"]["statements"][0];
    assert_eq!(st["rows"], json!([[1, "a"], [2, "b"]]));
    assert_eq!(st["total_rows"], 3);
    assert_eq!(st["truncated"], true);

    let refused = call(
        &s,
        "run_query",
        json!({"connection":"reader","sql":"DELETE FROM mcp_t"}),
    )
    .await;
    assert_eq!(refused["isError"], true);
    // Subqueries are still plain reads.
    let sub = call(
        &s,
        "run_query",
        json!({"connection":"reader","sql":"SELECT 1 FROM mcp_t WHERE id = 1 AND (SELECT COUNT(*) FROM mcp_t) > 0"}),
    )
    .await;
    assert!(sub.get("isError").is_none());

    let tables = call(&s, "list_tables", json!({"connection":"reader"})).await;
    assert!(
        tables["structuredContent"]["tables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "mcp_t")
    );
    let ddl = call(
        &s,
        "describe_table",
        json!({"connection":"reader","table":"mcp_t"}),
    )
    .await;
    assert!(
        ddl["structuredContent"]["create_table"]
            .as_str()
            .unwrap()
            .contains("PRIMARY KEY")
    );
    let plan = call(
        &s,
        "explain_query",
        json!({"connection":"reader","sql":"SELECT * FROM mcp_t WHERE v = 'a'"}),
    )
    .await;
    assert!(plan.get("isError").is_none(), "{plan}");

    let err = call(
        &s,
        "run_query",
        json!({"connection":"reader","sql":"SELECT nope FROM mcp_t"}),
    )
    .await;
    assert!(
        err["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("1054")
    );

    call(
        &s,
        "run_query",
        json!({"connection":"writer","sql":"DROP TABLE mcp_t"}),
    )
    .await;
    drop(s);

    // Every run is in the records, marked as coming from an AI tool.
    let store = Store::open(&path).unwrap();
    let recs = store.list_records(&Default::default()).unwrap();
    assert!(recs.len() >= 6);
    assert!(recs.iter().all(|r| r.note == RECORD_NOTE));
    let _ = std::fs::remove_file(&path);
}
