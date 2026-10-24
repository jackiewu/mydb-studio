import { useCallback, useEffect, useState } from "react";
import { api, describeError, type DatabaseInfo, type TableInfo } from "../api";

interface Props {
  sessionId: string;
  /** Called when a table is clicked. */
  onOpenTable: (database: string, table: string) => void;
}

const SYSTEM_DBS = new Set(["information_schema", "performance_schema", "mysql", "sys"]);

function size(bytes?: number | null) {
  if (bytes == null) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n < 10 && i > 0 ? n.toFixed(1) : Math.round(n)} ${units[i]}`;
}

export function SchemaTree({ sessionId, onOpenTable }: Props) {
  const [dbs, setDbs] = useState<DatabaseInfo[] | null>(null);
  const [tables, setTables] = useState<Record<string, TableInfo[] | "loading">>({});
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [showSystem, setShowSystem] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setDbs(await api.listDatabases(sessionId));
      setTables({});
      setOpen(new Set());
    } catch (e) {
      setError(describeError(e));
    }
  }, [sessionId]);

  useEffect(() => {
    load();
  }, [load]);

  const toggle = async (db: string) => {
    const next = new Set(open);
    if (next.has(db)) {
      next.delete(db);
      setOpen(next);
      return;
    }
    next.add(db);
    setOpen(next);
    if (!tables[db]) {
      setTables((t) => ({ ...t, [db]: "loading" }));
      try {
        const list = await api.listTables(sessionId, db);
        setTables((t) => ({ ...t, [db]: list }));
      } catch (e) {
        setError(describeError(e));
        setTables((t) => {
          const { [db]: _, ...rest } = t;
          return rest;
        });
      }
    }
  };

  const visible = (dbs ?? []).filter((d) => showSystem || !SYSTEM_DBS.has(d.name));
  const hidden = (dbs?.length ?? 0) - visible.length;

  return (
    <div className="schema">
      <div className="sidebar-head">
        <span>資料庫 {dbs && <small className="muted">({visible.length})</small>}</span>
        <button className="small ghost" title="重新整理" onClick={load}>
          ⟳
        </button>
      </div>
      {error && <div className="schema-error">{error}</div>}
      {!dbs && !error && <div className="hint pad">載入中…</div>}
      <ul className="tree">
        {visible.map((d) => {
          const list = tables[d.name];
          const isOpen = open.has(d.name);
          return (
            <li key={d.name}>
              <button className="tree-db" onClick={() => toggle(d.name)}>
                <span className="caret">{isOpen ? "▾" : "▸"}</span>
                <span className="tree-name">{d.name}</span>
                <span className="count">{d.table_count}</span>
              </button>
              {isOpen && (
                <ul>
                  {list === "loading" && <li className="hint pad">載入中…</li>}
                  {Array.isArray(list) && list.length === 0 && <li className="hint pad">沒有資料表</li>}
                  {Array.isArray(list) &&
                    list.map((t) => (
                      <li key={t.name}>
                        <button
                          className="tree-table"
                          title={[
                            t.kind,
                            t.engine,
                            t.rows_estimate != null ? `約 ${t.rows_estimate.toLocaleString()} 筆` : "",
                            size(t.size_bytes),
                            t.comment,
                          ]
                            .filter(Boolean)
                            .join(" · ")}
                          onClick={() => onOpenTable(d.name, t.name)}
                        >
                          <span className="icon">{t.kind.includes("VIEW") ? "◇" : "▤"}</span>
                          <span className="tree-name">{t.name}</span>
                          {t.rows_estimate != null && (
                            <span className="count">{t.rows_estimate.toLocaleString()}</span>
                          )}
                        </button>
                      </li>
                    ))}
                </ul>
              )}
            </li>
          );
        })}
      </ul>
      {hidden > 0 || showSystem ? (
        <button className="link small-text" onClick={() => setShowSystem(!showSystem)}>
          {showSystem ? "隱藏系統資料庫" : `顯示系統資料庫（${hidden}）`}
        </button>
      ) : null}
    </div>
  );
}
