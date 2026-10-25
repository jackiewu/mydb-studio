// Helpers for the schema pages: table designs as the designer edits them,
// and starting points for new views, routines, triggers and events.
// Kept free of React so `npm test` can cover them.
import type { ColumnDesign, IndexKind, ObjectKind, TableDesign } from "./api";

export const OBJECT_LABEL: Record<ObjectKind, string> = {
  view: "檢視",
  procedure: "預存程序",
  function: "函式",
  trigger: "觸發器",
  event: "事件",
};

export const INDEX_LABEL: Record<IndexKind, string> = {
  primary: "主鍵",
  unique: "唯一",
  index: "一般",
  fulltext: "全文",
  spatial: "空間",
};

/** Suggestions for the type field; anything else can be typed. */
export const COMMON_TYPES = [
  "int",
  "int unsigned",
  "bigint",
  "bigint unsigned",
  "tinyint(1)",
  "smallint",
  "decimal(10,2)",
  "double",
  "varchar(255)",
  "varchar(64)",
  "char(36)",
  "text",
  "mediumtext",
  "longtext",
  "json",
  "date",
  "datetime",
  "timestamp",
  "time",
  "blob",
  "enum('a','b')",
];

export const FK_RULES = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"];

/** Types that take a character set and collation. */
export function isTextType(type: string): boolean {
  const base = type.trim().toLowerCase().split(/[\s(]/)[0];
  return ["char", "varchar", "tinytext", "text", "mediumtext", "longtext", "enum", "set"].includes(base);
}

function isNumericType(type: string): boolean {
  const base = type.trim().toLowerCase().split(/[\s(]/)[0];
  return [
    "tinyint", "smallint", "mediumint", "int", "integer", "bigint",
    "decimal", "numeric", "float", "double", "real", "bit", "bool", "boolean",
  ].includes(base);
}

/**
 * What was typed in the default field, as SQL. Plain text for a text or
 * date column gets quotes; NULL, numbers, CURRENT_TIMESTAMP, b'…' and
 * (expressions) are kept as typed. Empty means no default.
 */
export function defaultSql(input: string, type: string): string | null {
  const v = input.trim();
  if (!v) return null;
  if (/^'.*'$/s.test(v) || /^\(.*\)$/s.test(v) || /^b'[01]*'$/i.test(v)) return v;
  // NULL is what a nullable column without a default gets anyway.
  if (/^null$/i.test(v)) return null;
  if (/^(current_timestamp|localtime|localtimestamp)(\(\d*\))?$/i.test(v)) return v;
  // A function call such as uuid() or now(), as MariaDB writes them.
  if (/^[a-z_][a-z0-9_]*\(.*\)$/is.test(v)) return v;
  if (/^[+-]?\d+(\.\d+)?$/.test(v) && (isNumericType(type) || !isTextType(type))) return v;
  if (isNumericType(type) && /^(true|false)$/i.test(v)) return v;
  return `'${v.replace(/\\/g, "\\\\").replace(/'/g, "''")}'`;
}

export function newColumn(name = ""): ColumnDesign {
  return {
    original: null,
    name,
    column_type: "varchar(255)",
    charset: null,
    collation: null,
    nullable: true,
    default: null,
    on_update: null,
    auto_increment: false,
    generated: null,
    stored: false,
    invisible: false,
    comment: "",
  };
}

/** A new table with an `id` primary key, in the database's defaults. */
export function newTable(engine: string | null, charset: string | null, collation: string | null): TableDesign {
  return {
    name: "",
    columns: [
      { ...newColumn("id"), column_type: "bigint unsigned", nullable: false, auto_increment: true },
    ],
    indexes: [
      { original: null, name: "PRIMARY", kind: "primary", parts: [part("id")], using: null, comment: "" },
    ],
    foreign_keys: [],
    options: { engine, charset, collation, comment: "", auto_increment: null },
  };
}

export function part(column: string) {
  return { column, length: null, descending: false, expression: null };
}

/** A name for a new index on `columns` that is not taken yet. */
export function indexName(design: TableDesign, kind: IndexKind, columns: string[]): string {
  if (kind === "primary") return "PRIMARY";
  const prefix = kind === "unique" ? "uq" : kind === "fulltext" ? "ft" : "idx";
  const base = `${prefix}_${columns.join("_") || "new"}`.slice(0, 60);
  const taken = new Set(design.indexes.map((i) => i.name.toLowerCase()));
  let name = base;
  for (let n = 2; taken.has(name.toLowerCase()); n++) name = `${base}_${n}`;
  return name;
}

/** Renames column `i`, along with the indexes and foreign keys that use it. */
export function renameColumn(design: TableDesign, i: number, name: string): TableDesign {
  const old = design.columns[i].name;
  return {
    ...design,
    columns: design.columns.map((c, j) => (j === i ? { ...c, name } : c)),
    indexes: design.indexes.map((ix) => ({
      ...ix,
      parts: ix.parts.map((p) => (p.expression == null && p.column === old ? { ...p, column: name } : p)),
    })),
    foreign_keys: design.foreign_keys.map((fk) => ({
      ...fk,
      columns: fk.columns.map((c) => (c === old ? name : c)),
    })),
  };
}

/**
 * Removes column `i`. Indexes lose it (and go when nothing is left), as
 * the server would do; foreign keys on it go.
 */
export function removeColumn(design: TableDesign, i: number): TableDesign {
  const name = design.columns[i].name;
  return {
    ...design,
    columns: design.columns.filter((_, j) => j !== i),
    indexes: design.indexes
      .map((ix) => ({ ...ix, parts: ix.parts.filter((p) => p.expression != null || p.column !== name) }))
      .filter((ix) => ix.parts.length > 0),
    foreign_keys: design.foreign_keys.filter((fk) => !fk.columns.includes(name)),
  };
}

/** Moves column `i` by `delta` places. */
export function moveColumn(design: TableDesign, i: number, delta: number): TableDesign {
  const j = i + delta;
  if (j < 0 || j >= design.columns.length) return design;
  const columns = [...design.columns];
  const [c] = columns.splice(i, 1);
  columns.splice(j, 0, c);
  return { ...design, columns };
}

/** A starting definition for a new object. */
export function objectTemplate(kind: ObjectKind, tables: string[]): string {
  const table = tables[0] ?? "my_table";
  switch (kind) {
    case "view":
      return `CREATE VIEW \`v_new\` AS\nSELECT *\nFROM \`${table}\``;
    case "procedure":
      return `CREATE PROCEDURE \`p_new\`(IN p_id INT)\nBEGIN\n  SELECT * FROM \`${table}\` WHERE id = p_id;\nEND`;
    case "function":
      return `CREATE FUNCTION \`f_new\`(p_value INT)\nRETURNS INT\nDETERMINISTIC\nBEGIN\n  RETURN p_value * 2;\nEND`;
    case "trigger":
      return `CREATE TRIGGER \`trg_${table}_before_insert\`\nBEFORE INSERT ON \`${table}\`\nFOR EACH ROW\nBEGIN\n  -- SET NEW.created_at = NOW();\nEND`;
    case "event":
      return `CREATE EVENT \`ev_new\`\nON SCHEDULE EVERY 1 DAY\nSTARTS CURRENT_TIMESTAMP + INTERVAL 1 HOUR\nDO\n  DELETE FROM \`${table}\` WHERE created_at < NOW() - INTERVAL 30 DAY`;
  }
}
