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

import { sqlDialect } from "../dialect.ts";

const SPACE = /\s/;

/**
 * Which server's rules to split by. PostgreSQL has `$tag$ … $tag$` bodies,
 * no `#` comments, no backslash escapes outside `E'…'` and no `DELIMITER`.
 * Set by the app for the window's connection.
 */
export { setSqlDialect } from "../dialect.ts";

/** End of the comment starting at `i`, or -1 when none starts there. */
function commentEnd(sql: string, i: number): number {
  if (sqlDialect() === "postgres") {
    if (sql.startsWith("--", i)) {
      const end = sql.indexOf("\n", i);
      return end < 0 ? sql.length : end;
    }
    if (sql.startsWith("/*", i)) {
      let depth = 0;
      let j = i;
      while (j < sql.length) {
        if (sql.startsWith("/*", j)) (depth++, (j += 2));
        else if (sql.startsWith("*/", j)) {
          depth--;
          j += 2;
          if (depth === 0) return j;
        } else j++;
      }
      return sql.length;
    }
    return -1;
  }
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

/** End (exclusive) of the quoted part starting at `i` on PostgreSQL, or -1. */
function pgQuoteEnd(sql: string, i: number): number {
  const c = sql[i];
  if (c === "'" || c === '"') {
    const escapes = c === "'" && /[eE]/.test(sql[i - 1] ?? "") && !/[\p{L}\d_]/u.test(sql[i - 2] ?? "");
    let j = i + 1;
    while (j < sql.length) {
      if (escapes && sql[j] === "\\") j += 2;
      else if (sql[j] === c && sql[j + 1] === c) j += 2;
      else if (sql[j] === c) return j + 1;
      else j++;
    }
    return sql.length;
  }
  if (c === "$" && !/[\p{L}\d_]/u.test(sql[i - 1] ?? "")) {
    const m = /^\$(?:[\p{L}_][\p{L}\d_]*)?\$/u.exec(sql.slice(i, i + 64));
    if (!m) return -1;
    const close = sql.indexOf(m[0], i + m[0].length);
    return close < 0 ? sql.length : close + m[0].length;
  }
  return -1;
}

export function statementRanges(sql: string): Range[] {
  if (sqlDialect() === "postgres") return pgRanges(sql);
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

function pgRanges(sql: string): Range[] {
  const out: Range[] = [];
  let start = -1;
  let depth = 0;
  let i = 0;
  while (i < sql.length) {
    const c = sql[i];
    if (start < 0 && !SPACE.test(c)) start = i;
    const comment = commentEnd(sql, i);
    const quote = comment < 0 ? pgQuoteEnd(sql, i) : -1;
    if (comment >= 0) i = comment;
    else if (quote >= 0) i = quote;
    else if (c === "(") (depth++, i++);
    else if (c === ")") ((depth = Math.max(0, depth - 1)), i++);
    else if (c === ";" && depth === 0) {
      let end = i + 1;
      let j = end;
      while (sql[j] === " " || sql[j] === "\t") j++;
      const trailing = commentEnd(sql, j);
      if (trailing >= 0) end = trailing;
      out.push({ from: start, to: end });
      start = -1;
      i = end;
    } else i++;
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
