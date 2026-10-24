import type { Completion, CompletionSource } from "@codemirror/autocomplete";

/** database → table → column completions. */
export type SchemaMap = { [db: string]: { [table: string]: Completion[] } };

const TABLE_REF = /\b(?:from|join|update|into|table)\s+((?:`[^`]+`|\w+)(?:\s*\.\s*(?:`[^`]+`|\w+))?)/gi;

/**
 * Completes bare column names from the tables the query mentions
 * (`FROM orders`, `JOIN shop.customers`, `UPDATE t`), which lang-sql only
 * does for qualified names. Columns of a database load on first use, so
 * `needColumns` asks for them and the next keystroke picks them up.
 */
export function columnSource(
  schema: SchemaMap,
  defaultSchema: string | null | undefined,
  needColumns: (db: string) => void,
): CompletionSource {
  return (ctx) => {
    const word = ctx.matchBefore(/\w*/);
    if (!word || (word.from === word.to && !ctx.explicit)) return null;
    // Qualified names (`t.col`, `db.t`) are lang-sql's job.
    if (ctx.state.sliceDoc(word.from - 1, word.from) === ".") return null;

    const options: Completion[] = [];
    const seen = new Set<string>();
    for (const m of ctx.state.doc.toString().matchAll(TABLE_REF)) {
      const parts = m[1].split(/\s*\.\s*/).map((p) => p.replace(/^`|`$/g, ""));
      const [db, table] = parts.length === 2 ? parts : [defaultSchema, parts[0]];
      const columns = db ? schema[db]?.[table] : undefined;
      if (!db || !columns) continue;
      if (columns.length === 0) needColumns(db);
      for (const c of columns) {
        if (seen.has(c.label)) continue;
        seen.add(c.label);
        options.push({ ...c, detail: `${table} · ${c.detail ?? ""}`, boost: 5 });
      }
    }
    return options.length ? { from: word.from, options, validFor: /^\w*$/ } : null;
  };
}
