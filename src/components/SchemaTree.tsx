import { useState } from "react";
import { SYSTEM_DBS, type Schema } from "../schema";

interface Props {
  schema: Schema;
  /** Database selected for the session (with USE). */
  currentDb: string | null;
  showSystem: boolean;
  onShowSystemChange: (show: boolean) => void;
  /** Double-click on a database. */
  onUseDatabase: (database: string) => void;
  /** Called when a table is clicked. */
  onOpenTable: (database: string, table: string) => void;
}

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

export function SchemaTree({ schema, currentDb, showSystem, onShowSystemChange, onUseDatabase, onOpenTable }: Props) {
  const { databases: dbs, tables, error } = schema;
  const [open, setOpen] = useState<Set<string>>(new Set());

  const expand = (db: string, isOpen: boolean) => {
    setOpen((prev) => {
      const next = new Set(prev);
      if (isOpen) next.add(db);
      else next.delete(db);
      return next;
    });
    if (isOpen && !tables[db]) schema.loadTables(db);
    // Columns too, so autocomplete knows the tables just looked at.
    if (isOpen) schema.loadColumns(db);
  };

  const reload = () => {
    setOpen(new Set());
    schema.reload();
  };

  const visible = (dbs ?? []).filter((d) => showSystem || !SYSTEM_DBS.has(d.name));
  const hidden = (dbs?.length ?? 0) - visible.length;

  return (
    <div className="schema">
      <div className="sidebar-head">
        <span>資料庫 {dbs && <small className="muted">({visible.length})</small>}</span>
        <button className="small ghost" title="重新整理" onClick={reload}>
          ⟳
        </button>
      </div>
      {error && <div className="schema-error">{error}</div>}
      {!dbs && !error && <div className="hint pad">載入中…</div>}
      <ul className="tree">
        {visible.map((d) => {
          const list = tables[d.name];
          const isOpen = open.has(d.name);
          const isCurrent = d.name === currentDb;
          return (
            <li key={d.name}>
              <button
                className={isCurrent ? "tree-db current" : "tree-db"}
                title={isCurrent ? "目前使用中的資料庫" : "雙擊切換到這個資料庫（USE）"}
                onClick={(e) => {
                  // The second click of a double-click is handled below.
                  if (e.detail <= 1) expand(d.name, !isOpen);
                }}
                onDoubleClick={() => {
                  expand(d.name, true);
                  onUseDatabase(d.name);
                }}
              >
                <span className="caret">{isOpen ? "▾" : "▸"}</span>
                <span className="tree-name">{d.name}</span>
                {isCurrent && <span className="current-mark">USE</span>}
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
                            t.rows_estimate != null ? `約 ${t.rows_estimate.toLocaleString()} 筆（估計值）` : "",
                            size(t.size_bytes),
                            t.comment,
                          ]
                            .filter(Boolean)
                            .join(" · ")}
                          onClick={() => onOpenTable(d.name, t.name)}
                        >
                          <span className="icon">{t.kind.includes("VIEW") ? "◇" : "▤"}</span>
                          <span className="tree-name">{t.name}</span>
                          {t.rows_estimate != null && !t.kind.includes("VIEW") && (
                            <span className="count">{t.rows_estimate ? t.rows_estimate.toLocaleString() : "—"}</span>
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
        <button className="link small-text" onClick={() => onShowSystemChange(!showSystem)}>
          {showSystem ? "隱藏系統資料庫" : `顯示系統資料庫（${hidden}）`}
        </button>
      ) : null}
    </div>
  );
}
