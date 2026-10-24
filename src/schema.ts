import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Completion } from "@codemirror/autocomplete";
import { api, describeError, type ColumnDef, type DatabaseInfo, type TableInfo } from "./api";
import type { SchemaMap } from "./editor/columns";

export const SYSTEM_DBS = new Set(["information_schema", "performance_schema", "mysql", "sys"]);

/**
 * What the app knows about the server's schema: databases, tables per
 * database (for the tree and autocomplete) and columns of the databases
 * that have been used or expanded.
 */
export interface Schema {
  databases: DatabaseInfo[] | null;
  tables: Record<string, TableInfo[] | "loading">;
  error: string | null;
  reload: () => Promise<void>;
  loadTables: (db: string) => Promise<void>;
  loadColumns: (db: string) => Promise<void>;
  /** Databases → tables → columns, for autocomplete. */
  namespace: SchemaMap;
}

export function useSchema(sessionId: string | null): Schema {
  const [databases, setDatabases] = useState<DatabaseInfo[] | null>(null);
  const [tables, setTables] = useState<Record<string, TableInfo[] | "loading">>({});
  const [tableNames, setTableNames] = useState<Record<string, string[]>>({});
  const [columns, setColumns] = useState<Record<string, ColumnDef[]>>({});
  const [error, setError] = useState<string | null>(null);
  // Databases whose columns were requested, so each loads once.
  const requested = useRef(new Set<string>());

  const reload = useCallback(async () => {
    setError(null);
    setTables({});
    setColumns({});
    requested.current.clear();
    if (!sessionId) {
      setDatabases(null);
      setTableNames({});
      return;
    }
    try {
      const [dbs, all] = await Promise.all([api.listDatabases(sessionId), api.listAllTables(sessionId)]);
      const names: Record<string, string[]> = {};
      for (const [db, table] of all) (names[db] ??= []).push(table);
      setDatabases(dbs);
      setTableNames(names);
    } catch (e) {
      setError(describeError(e));
    }
  }, [sessionId]);

  useEffect(() => {
    setDatabases(null);
    reload();
  }, [reload]);

  const loadTables = useCallback(
    async (db: string) => {
      if (!sessionId) return;
      setTables((t) => (t[db] ? t : { ...t, [db]: "loading" }));
      try {
        const list = await api.listTables(sessionId, db);
        setTables((t) => ({ ...t, [db]: list }));
      } catch (e) {
        setError(describeError(e));
        setTables(({ [db]: _, ...rest }) => rest);
      }
    },
    [sessionId],
  );

  const loadColumns = useCallback(
    async (db: string) => {
      if (!sessionId || requested.current.has(db)) return;
      requested.current.add(db);
      try {
        const list = await api.listColumns(sessionId, db);
        setColumns((c) => ({ ...c, [db]: list }));
      } catch {
        // Autocomplete just goes without columns.
        requested.current.delete(db);
      }
    },
    [sessionId],
  );

  const namespace = useMemo(() => {
    const ns: SchemaMap = {};
    for (const db of databases ?? []) {
      const byTable: { [table: string]: Completion[] } = {};
      for (const t of tableNames[db.name] ?? []) byTable[t] = [];
      for (const c of columns[db.name] ?? []) {
        (byTable[c.table] ??= []).push({
          label: c.name,
          type: "property",
          detail: c.key === "PRI" ? `${c.column_type} · PK` : c.column_type,
          info: c.comment || undefined,
        });
      }
      ns[db.name] = byTable;
    }
    return ns;
  }, [databases, tableNames, columns]);

  return { databases, tables, error, reload, loadTables, loadColumns, namespace };
}
