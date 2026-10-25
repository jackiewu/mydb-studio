// Run with `npm test` (Node's own test runner; Node 22 strips the types).
import { test } from "node:test";
import assert from "node:assert/strict";
import { defaultSql, indexName, moveColumn, newColumn, newTable, removeColumn, renameColumn } from "./ddl.ts";

function table() {
  const t = newTable("InnoDB", "utf8mb4", null);
  t.name = "users";
  t.columns.push({ ...newColumn("email"), original: "email" }, { ...newColumn("team_id"), column_type: "int" });
  t.indexes.push({
    original: "uq",
    name: "uq_email",
    kind: "unique",
    parts: [
      { column: "email", length: 10, descending: false, expression: null },
      { column: "id", length: null, descending: false, expression: null },
    ],
    using: null,
    comment: "",
  });
  t.foreign_keys.push({
    original: null,
    name: "fk_team",
    columns: ["team_id"],
    ref_database: null,
    ref_table: "teams",
    ref_columns: ["id"],
    on_delete: "",
    on_update: "",
  });
  return t;
}

test("defaults typed as text get quotes only where needed", () => {
  assert.equal(defaultSql("", "int"), null);
  assert.equal(defaultSql("0", "int"), "0");
  assert.equal(defaultSql("abc", "varchar(10)"), "'abc'");
  assert.equal(defaultSql("it's", "text"), "'it''s'");
  assert.equal(defaultSql("'kept'", "varchar(10)"), "'kept'");
  assert.equal(defaultSql("12", "varchar(10)"), "'12'");
  assert.equal(defaultSql("null", "varchar(10)"), null);
  assert.equal(defaultSql("CURRENT_TIMESTAMP(3)", "datetime(3)"), "CURRENT_TIMESTAMP(3)");
  assert.equal(defaultSql("2020-01-01", "date"), "'2020-01-01'");
  assert.equal(defaultSql("(uuid())", "char(36)"), "(uuid())");
  assert.equal(defaultSql("b'1'", "bit(1)"), "b'1'");
  assert.equal(defaultSql("uuid()", "char(36)"), "uuid()");
});

test("renaming a column carries its indexes and keys along", () => {
  const t = renameColumn(table(), 1, "mail");
  assert.equal(t.columns[1].name, "mail");
  assert.equal(t.columns[1].original, "email");
  assert.deepEqual(t.indexes[1].parts.map((p) => p.column), ["mail", "id"]);
  const u = renameColumn(t, 2, "team");
  assert.deepEqual(u.foreign_keys[0].columns, ["team"]);
});

test("removing a column trims indexes and drops keys on it", () => {
  const t = removeColumn(table(), 1);
  assert.deepEqual(t.indexes.map((i) => i.parts.map((p) => p.column)), [["id"], ["id"]]);
  const u = removeColumn(t, 1);
  assert.equal(u.foreign_keys.length, 0);
  const v = removeColumn(u, 0);
  assert.equal(v.indexes.length, 0);
});

test("moves stay in range and index names stay unique", () => {
  const t = table();
  assert.deepEqual(moveColumn(t, 0, 1).columns.map((c) => c.name), ["email", "id", "team_id"]);
  assert.equal(moveColumn(t, 0, -1), t);
  assert.equal(indexName(t, "unique", ["email"]), "uq_email_2");
  assert.equal(indexName(t, "index", ["team_id"]), "idx_team_id");
});
