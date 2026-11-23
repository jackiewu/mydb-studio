/**
 * The SQL dialect of the window's connection: how names are quoted and
 * statements split. One window has one connection, so the app sets this
 * when a session opens.
 */
export type SqlDialect = "mysql" | "postgres";

let current: SqlDialect = "mysql";

export function setSqlDialect(d: SqlDialect) {
  current = d;
}

export function sqlDialect(): SqlDialect {
  return current;
}

/** `name` quoted as an identifier: `name` for MySQL, "name" for PostgreSQL. */
export function quoteName(name: string, dialect: SqlDialect = current): string {
  return dialect === "postgres" ? '"' + name.replace(/"/g, '""') + '"' : "`" + name.replace(/`/g, "``") + "`";
}
