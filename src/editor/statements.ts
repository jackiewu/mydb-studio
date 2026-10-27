/**
 * Statement boundaries in editor text, for running and marking the
 * statement at the cursor. Mirrors the quote and comment rules of
 * `split.rs`; text with a `DELIMITER` command is treated as one block.
 *
 * A range runs from its first non-blank character to its `;`, plus a
 * comment that follows on the same line. So a comment on lines of its own
 * belongs to the statement after it, while `SELECT 1; -- note` keeps its
 * note.
 */
export interface Range {
  from: number;
  to: number;
}

const SPACE = /\s/;

/** End of the comment starting at `i`, or -1 when none starts there. */
function commentEnd(sql: string, i: number): number {
  if (sql[i] === "#" || (sql.startsWith("--", i) && /\s|^$/.test(sql[i + 2] ?? ""))) {
    const end = sql.indexOf("\n", i);
    return end < 0 ? sql.length : end;
  }
  if (sql.startsWith("/*", i)) {
    const end = sql.indexOf("*/", i + 2);
    return end < 0 ? sql.length : end + 2;
  }
  return -1;
}

function hasCode(text: string): boolean {
  for (let i = 0; i < text.length; ) {
    const end = commentEnd(text, i);
    if (end >= 0) i = end;
    else if (SPACE.test(text[i])) i++;
    else return true;
  }
  return false;
}

export function statementRanges(sql: string): Range[] {
  if (/^\s*delimiter\s/im.test(sql)) {
    const from = sql.search(/\S/);
    return from < 0 ? [] : [{ from, to: sql.trimEnd().length }];
  }
  const out: Range[] = [];
  let start = -1;
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    if (start < 0 && !SPACE.test(c)) start = i;
    const comment = commentEnd(sql, i);
    if (comment >= 0) {
      i = comment;
    } else if (c === "'" || c === '"' || c === "`") {
      i++;
      while (i < sql.length) {
        if (sql[i] === "\\" && c !== "`") i += 2;
        else if (sql[i] === c && sql[i + 1] === c) i += 2;
        else if (sql[i] === c) break;
        else i++;
      }
      i++;
    } else if (c === ";") {
      let end = i + 1;
      // A comment on the same line after the `;` belongs to this statement.
      let j = end;
      while (sql[j] === " " || sql[j] === "\t") j++;
      const trailing = commentEnd(sql, j);
      if (trailing >= 0) end = trailing;
      out.push({ from: start, to: end });
      start = -1;
      i = end;
    } else {
      i++;
    }
  }
  if (start >= 0) out.push({ from: start, to: sql.trimEnd().length });
  return out.filter((r) => hasCode(sql.slice(r.from, r.to)));
}

/**
 * Index of the statement for a cursor at `pos`: the one containing it, or
 * in the blank space between two, the one before (the next one when the
 * cursor is above the first statement).
 */
export function statementIndexAt(ranges: Range[], pos: number): number {
  if (ranges.length === 0) return -1;
  const inside = ranges.findIndex((r) => pos >= r.from && pos <= r.to);
  if (inside >= 0) return inside;
  const next = ranges.findIndex((r) => r.from > pos);
  return next < 0 ? ranges.length - 1 : Math.max(0, next - 1);
}

/**
 * Whether `text` can't be the start of a statement: its first code (after
 * blanks and comments) is something like `33,` or `'abc'` instead of a word
 * or `(`. Such a selection was almost surely meant as part of a bigger
 * statement, and would only fail with a syntax error on its own.
 */
export function startsMidStatement(text: string): boolean {
  for (let i = 0; i < text.length; ) {
    // `/*! ... */` is executable, so it counts as code.
    if (text.startsWith("/*!", i) || text.startsWith("/*M!", i)) return false;
    const end = commentEnd(text, i);
    if (end >= 0) i = end;
    else if (SPACE.test(text[i])) i++;
    else return !/[\p{L}_(]/u.test(text[i]);
  }
  return false;
}

export function statementAt(sql: string, pos: number): string {
  const ranges = statementRanges(sql);
  const i = statementIndexAt(ranges, pos);
  return i < 0 ? "" : sql.slice(ranges[i].from, ranges[i].to);
}
