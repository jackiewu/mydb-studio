import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import type { Cell, ColumnInfo } from "./api";

export type CopyFormat = "tsv" | "csv" | "json" | "insert" | "insert_batch" | "markdown";

export const COPY_FORMATS: { id: CopyFormat; label: string; header: boolean }[] = [
  { id: "tsv", label: "TSV（貼到試算表）", header: true },
  { id: "csv", label: "CSV", header: true },
  { id: "json", label: "JSON", header: false },
  { id: "insert", label: "INSERT SQL（每列一句）", header: false },
  { id: "insert_batch", label: "INSERT SQL（多列合成一句）", header: false },
  { id: "markdown", label: "Markdown 表格", header: false },
];

/** Selected part of a result: a range of rows and a list of columns. */
export interface Block {
  columns: ColumnInfo[];
  rows: Cell[][];
}

// Driver type names (see session.rs) that hold numbers and need no quotes.
const NUMERIC = new Set(["TINY", "SHORT", "LONG", "INT24", "LONGLONG", "FLOAT", "DOUBLE", "DECIMAL", "NEWDECIMAL", "YEAR", "BIT"]);

/**
 * Text that starts like a formula (=, +, -, @, tab, CR) gets a leading ', so a
 * spreadsheet shows it instead of running it. Numbers such as -5 are left alone.
 */
export function defuseFormula(s: string) {
  return /^[=+\-@\t\r]/.test(s) && Number.isNaN(Number(s.trim())) ? `'${s}` : s;
}

function tsvCell(v: Cell) {
  if (v === null) return "NULL";
  // Tabs and newlines would break the grid when pasted.
  return defuseFormula(String(v)).replace(/[\t\r\n]+/g, " ");
}

function csvCell(v: Cell) {
  if (v === null) return "";
  const s = defuseFormula(String(v));
  return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function sqlValue(v: Cell, col: ColumnInfo) {
  if (v === null) return "NULL";
  if (typeof v === "number") return String(v);
  if (NUMERIC.has(col.type_name) && /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i.test(v)) return v;
  // Binary cells arrive as 0x hex (see to_json in session.rs).
  if (/BLOB|STRING/.test(col.type_name) && /^0x[0-9A-F]+$/.test(v)) return v;
  return `'${v.replace(/\\/g, "\\\\").replace(/'/g, "''").replace(/\n/g, "\\n").replace(/\r/g, "\\r")}'`;
}

const INTEGER = new Set(["TINY", "SHORT", "LONG", "INT24", "LONGLONG", "YEAR"]);

/** Integers and floats as JSON numbers; DECIMAL stays a string to keep its exact digits. */
function jsonValue(v: Cell, col: ColumnInfo) {
  if (typeof v !== "string") return v;
  if (INTEGER.has(col.type_name) && /^-?\d+$/.test(v) && Number.isSafeInteger(Number(v))) return Number(v);
  if ((col.type_name === "FLOAT" || col.type_name === "DOUBLE") && v.trim() !== "" && Number.isFinite(Number(v))) return Number(v);
  return v;
}

function ident(name: string) {
  return "`" + name.replace(/`/g, "``") + "`";
}

/** Table the columns come from, when they all come from one. */
export function sourceTable(columns: ColumnInfo[]) {
  const tables = new Set(columns.map((c) => c.table).filter(Boolean));
  return tables.size === 1 ? [...tables][0] : null;
}

export function formatBlock(block: Block, format: CopyFormat, header: boolean, table?: string): string {
  const { columns, rows } = block;
  const names = columns.map((c) => c.name);
  switch (format) {
    case "tsv": {
      const lines = rows.map((r) => r.map(tsvCell).join("\t"));
      return (header ? [names.map((n) => tsvCell(n)).join("\t"), ...lines] : lines).join("\n");
    }
    case "csv": {
      const lines = rows.map((r) => r.map(csvCell).join(","));
      return (header ? [names.map(csvCell).join(","), ...lines] : lines).join("\r\n");
    }
    case "json":
      return JSON.stringify(
        rows.map((r) => Object.fromEntries(names.map((n, i) => [n, jsonValue(r[i], columns[i])]))),
        null,
        2,
      );
    case "insert":
    case "insert_batch": {
      const into = `INSERT INTO ${ident(table || sourceTable(columns) || "table_name")} (${names.map(ident).join(", ")})`;
      const values = rows.map((r) => `(${r.map((v, i) => sqlValue(v, columns[i])).join(", ")})`);
      if (format === "insert") return values.map((v) => `${into} VALUES ${v};`).join("\n");
      return values.length ? `${into} VALUES\n  ${values.join(",\n  ")};` : "";
    }
    case "markdown": {
      const cell = (v: Cell) => (v === null ? "NULL" : String(v).replace(/\|/g, "\\|").replace(/\r?\n/g, " "));
      return [
        `| ${names.map(cell).join(" | ")} |`,
        `|${names.map(() => "---").join("|")}|`,
        ...rows.map((r) => `| ${r.map(cell).join(" | ")} |`),
      ].join("\n");
    }
  }
}

/** File extension for saving a block in `format`. */
export function fileExtension(format: CopyFormat) {
  return { tsv: "tsv", csv: "csv", json: "json", insert: "sql", insert_batch: "sql", markdown: "md" }[format];
}

export async function copyText(text: string) {
  try {
    await writeText(text);
  } catch {
    // Older webviews: fall back to a temporary textarea.
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}
