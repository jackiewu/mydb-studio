import type { StatementResult } from "./api";

export type Level = "bad" | "warn";

export interface PlanNote {
  level: Level;
  text: string;
}

/** Rows a step may scan before it is worth a warning. */
const MANY_ROWS = 100_000;

export function isExplain(r: StatementResult) {
  return /^\s*(explain|analyze)\b/i.test(r.sql) && r.columns.some((c) => c.name === "type");
}

/**
 * Reads a classic (tabular) EXPLAIN: what to look at, and which cells show
 * it. Covers what most often makes a query slow: full scans, no usable
 * index, filesort, temporary tables and very large row estimates.
 */
export function readPlan(r: StatementResult) {
  const col = (name: string) => r.columns.findIndex((c) => c.name.toLowerCase() === name);
  const [table, type, key, rows, extra] = ["table", "type", "key", "rows", "extra"].map(col);
  const notes: PlanNote[] = [];
  const cells = new Map<string, Level>();
  const mark = (row: number, c: number, level: Level) => c >= 0 && cells.set(`${row}:${c}`, level);

  r.rows.forEach((row, i) => {
    const name = String(row[table] ?? `第 ${i + 1} 步`);
    const t = String(row[type] ?? "");
    const ex = String(row[extra] ?? "");
    const n = Number(row[rows] ?? 0);
    if (t === "ALL") {
      notes.push({ level: "bad", text: `${name}：全表掃描（type=ALL），WHERE／JOIN 的欄位可能需要索引` });
      mark(i, type, "bad");
    } else if (t === "index") {
      notes.push({ level: "warn", text: `${name}：掃描整個索引（type=index）` });
      mark(i, type, "warn");
    }
    if (key >= 0 && row[key] === null && t && !["ALL", "system", "const", "NULL"].includes(t)) {
      notes.push({ level: "warn", text: `${name}：沒有用到索引` });
      mark(i, key, "warn");
    }
    if (/filesort/i.test(ex)) {
      notes.push({ level: "warn", text: `${name}：排序沒用到索引（Using filesort）` });
      mark(i, extra, "warn");
    }
    if (/temporary/i.test(ex)) {
      notes.push({ level: "warn", text: `${name}：需要暫存表（Using temporary），常見於 GROUP BY／DISTINCT` });
      mark(i, extra, "warn");
    }
    if (n >= MANY_ROWS) {
      notes.push({ level: "warn", text: `${name}：預估要掃描 ${n.toLocaleString()} 筆` });
      mark(i, rows, "warn");
    }
  });
  return { notes, cellLevel: (row: number, c: number) => cells.get(`${row}:${c}`) };
}
