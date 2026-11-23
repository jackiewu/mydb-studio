// Run with `npm test` (Node's own test runner; Node 22 strips the types).
import { test } from "node:test";
import assert from "node:assert/strict";
import { startsMidStatement, statementAt, statementRanges } from "./statements.ts";

const insert = `insert into channels (code, name, providerId, url, mode)
value (
    'QQ01',
    'QQ01',
    33,
    'https://api.example.com/v1/place-order',
    1,
    2
);`;

test("a multi-line INSERT with URLs is one statement", () => {
  assert.equal(statementRanges(insert).length, 1);
  assert.equal(statementAt(insert, insert.indexOf("33")), insert);
});

test("a selection from a value line starts mid-statement", () => {
  const from = insert.indexOf("    33,");
  const to = insert.indexOf("    2\n");
  assert.equal(startsMidStatement(insert.slice(from, to)), true);
  assert.equal(startsMidStatement("'QQ01',\n    33"), true);
  assert.equal(startsMidStatement(", 2)"), true);
});

test("selections that can start a statement are left alone", () => {
  assert.equal(startsMidStatement("SELECT 1"), false);
  assert.equal(startsMidStatement("  -- note\n  select id from t"), false);
  assert.equal(startsMidStatement("(SELECT 1) UNION (SELECT 2)"), false);
  assert.equal(startsMidStatement("/*!40101 SET NAMES utf8 */"), false);
  assert.equal(startsMidStatement("/* note */ UPDATE t SET a = 1"), false);
  assert.equal(startsMidStatement("   "), false);
});

test("postgres: dollar quotes, nested comments and # are handled", async () => {
  const { setSqlDialect } = await import("./statements.ts");
  setSqlDialect("postgres");
  try {
    const fn = "CREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql;";
    const sql = `${fn}\nSELECT 5 # 3; /* a /* b; */ c; */ SELECT 'x\\'; SELECT 2;`;
    const parts = statementRanges(sql).map((r) => sql.slice(r.from, r.to));
    // A comment after `;` on the same line stays with that statement.
    assert.deepEqual(parts, [fn, "SELECT 5 # 3; /* a /* b; */ c; */", "SELECT 'x\\';", "SELECT 2;"]);
    assert.equal(statementAt(sql, sql.indexOf("RETURN")), fn);
  } finally {
    setSqlDialect("mysql");
  }
});
