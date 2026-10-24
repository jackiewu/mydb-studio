/**
 * Statement boundaries in editor text, for "run the statement at the
 * cursor". Mirrors the quote and comment rules of `split.rs`; text with a
 * `DELIMITER` command is treated as one block.
 */
export interface Range {
  from: number;
  to: number;
}

export function statementRanges(sql: string): Range[] {
  if (/^\s*delimiter\s/im.test(sql)) return [{ from: 0, to: sql.length }];
  const out: Range[] = [];
  let start = 0;
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    if (c === "'" || c === '"' || c === "`") {
      i++;
      while (i < sql.length) {
        if (sql[i] === "\\" && c !== "`") i += 2;
        else if (sql[i] === c && sql[i + 1] === c) i += 2;
        else if (sql[i] === c) break;
        else i++;
      }
      i++;
    } else if (c === "#" || (c === "-" && sql.startsWith("--", i) && /\s|$/.test(sql[i + 2] ?? ""))) {
      const end = sql.indexOf("\n", i);
      i = end < 0 ? sql.length : end + 1;
    } else if (c === "/" && sql[i + 1] === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end < 0 ? sql.length : end + 2;
    } else if (c === ";") {
      out.push({ from: start, to: i });
      start = ++i;
    } else {
      i++;
    }
  }
  out.push({ from: start, to: sql.length });
  return out.filter((r) => sql.slice(r.from, r.to).trim() !== "");
}

/**
 * The statement around `pos`. A cursor in blank space after a statement's
 * `;` belongs to that statement.
 */
export function statementAt(sql: string, pos: number): string {
  const ranges = statementRanges(sql);
  const hit =
    ranges.find((r) => pos >= r.from && pos <= r.to + 1) ??
    [...ranges].reverse().find((r) => r.to < pos) ??
    ranges[0];
  return hit ? sql.slice(hit.from, hit.to).trim() : "";
}
