import { useEffect, useState, type ReactNode } from "react";
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

/** `text` with the part matching `filter` marked. */
function highlight(text: string, filter: string): ReactNode {
  const at = filter ? text.toLowerCase().indexOf(filter) : -1;
  if (at < 0) return text;
  return (
    <>
      {text.slice(0, at)}
      <mark>{text.slice(at, at + filter.length)}</mark>
      {text.slice(at + filter.length)}
    </>
  );
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
  const [filterText, setFilterText] = useState("");
  const filter = filterText.trim().toLowerCase();

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

  const shown = (dbs ?? []).filter((d) => showSystem || !SYSTEM_DBS.has(d.name));
  const hidden = (dbs?.length ?? 0) - shown.length;
  // With a filter: databases whose name matches, plus databases that have
  // matching tables (opened, showing only those tables).
  const tableHits = (db: string) =>
    filter ? (schema.tableNames[db] ?? []).filter((t) => t.toLowerCase().includes(filter)) : [];
  const visible = filter
    ? shown.filter((d) => d.name.toLowerCase().includes(filter) || tableHits(d.name).length > 0)
    : shown;
  const forcedOpen = visible
    .filter((d) => !d.name.toLowerCase().includes(filter) && tableHits(d.name).length > 0)
    .map((d) => d.name);
  const forcedKey = filter ? forcedOpen.join("\n") : "";

  // Load the tables of databases opened by the filter.
  useEffect(() => {
    for (const db of forcedKey ? forcedKey.split("\n") : []) if (!tables[db]) schema.loadTables(db);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [forcedKey]);

  return (
    <div className="schema">
      <div className="sidebar-head">
        <span>
          資料庫 {dbs && <small className="muted">({filter ? `${visible.length}/${shown.length}` : shown.length})</small>}
        </span>
        <button className="small ghost" title="重新整理" onClick={reload}>
          ⟳
        </button>
      </div>
      <div className="schema-filter">
        <input
          type="search"
          placeholder="篩選資料庫或資料表…"
          value={filterText}
          spellCheck={false}
          onChange={(e) => setFilterText(e.target.value)}
          onKeyDown={(e) => e.key === "Escape" && setFilterText("")}
        />
      </div>
      {filter && dbs && visible.length === 0 && <div className="hint pad">沒有符合「{filterText.trim()}」的資料庫或資料表</div>}
      {error && <div className="schema-error">{error}</div>}
      {!dbs && !error && <div className="hint pad">載入中…</div>}
      <ul className="tree">
        {visible.map((d) => {
          const onlyTables = forcedOpen.includes(d.name);
          const loaded = tables[d.name];
          const list =
            onlyTables && Array.isArray(loaded)
              ? loaded.filter((t) => t.name.toLowerCase().includes(filter))
              : loaded;
          const isOpen = open.has(d.name) || onlyTables;
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
                <span className="tree-name">{highlight(d.name, filter)}</span>
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
                          <span className="tree-name">{highlight(t.name, filter)}</span>
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
