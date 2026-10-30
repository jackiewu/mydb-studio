import { useEffect, useState, type ReactNode } from "react";
import { quoteIdent } from "../api";
import { openMenu } from "../contextMenu";
import { copyText } from "../copy";
import { SYSTEM_DBS, type Schema } from "../schema";

interface Props {
  schema: Schema;
  /** Database selected for the session (with USE). */
  currentDb: string | null;
  showSystem: boolean;
  onShowSystemChange: (show: boolean) => void;
  /** Click on a database. */
  onUseDatabase: (database: string) => void;
  /** Called when a table is clicked. */
  onOpenTable: (database: string, table: string) => void;
  /** Opens the schema page on a database, or the designer of a table. */
  onManage: (database: string, table?: string) => void;
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

export function SchemaTree({ schema, currentDb, showSystem, onShowSystemChange, onUseDatabase, onOpenTable, onManage }: Props) {
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
          資料庫{" "}
          {dbs && <small className="muted">({filter ? `${visible.length}/${shown.length}` : shown.length})</small>}
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
      {filter && dbs && visible.length === 0 && (
        <div className="hint pad">沒有符合「{filterText.trim()}」的資料庫或資料表</div>
      )}
      {error && <div className="schema-error">{error}</div>}
      {!dbs && !error && <div className="hint pad">載入中…</div>}
      <ul className="tree">
        {visible.map((d) => {
          const onlyTables = forcedOpen.includes(d.name);
          const loaded = tables[d.name];
          const list =
            onlyTables && Array.isArray(loaded) ? loaded.filter((t) => t.name.toLowerCase().includes(filter)) : loaded;
          const isOpen = open.has(d.name) || onlyTables;
          const isCurrent = d.name === currentDb;
          return (
            <li key={d.name}>
              <button
                className={isCurrent ? "tree-db current" : "tree-db"}
                title={isCurrent ? "目前使用中的資料庫" : "點一下切換到這個資料庫（USE）"}
                onClick={() => {
                  if (!isCurrent) onUseDatabase(d.name);
                }}
                onContextMenu={(e) =>
                  openMenu(e, [
                    {
                      label: "使用這個資料庫（USE）",
                      disabled: isCurrent,
                      onClick: () => onUseDatabase(d.name),
                    },
                    { label: isOpen ? "收合" : "展開", disabled: onlyTables, onClick: () => expand(d.name, !isOpen) },
                    "sep",
                    { label: "管理結構（資料表、程序、事件…）", onClick: () => onManage(d.name) },
                    "sep",
                    { label: "複製名稱", onClick: () => copyText(d.name) },
                    "sep",
                    { label: "重新整理清單", onClick: reload },
                  ])
                }
              >
                <span
                  className="caret"
                  role="button"
                  aria-label={isOpen ? "收合資料表" : "展開資料表"}
                  aria-expanded={isOpen}
                  title={onlyTables ? undefined : isOpen ? "收合資料表" : "展開資料表"}
                  onClick={(e) => {
                    // Only the chevron opens the table list; the row selects.
                    e.stopPropagation();
                    if (!onlyTables) expand(d.name, !isOpen);
                  }}
                  onDoubleClick={(e) => e.stopPropagation()}
                >
                  {isOpen ? "▾" : "▸"}
                </span>
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
                          onContextMenu={(e) => {
                            const full = `${quoteIdent(d.name)}.${quoteIdent(t.name)}`;
                            openMenu(e, [
                              { label: "查詢前 100 筆", onClick: () => onOpenTable(d.name, t.name) },
                              ...(t.kind === "BASE TABLE"
                                ? [{ label: "設計資料表（欄位、索引）…", onClick: () => onManage(d.name, t.name) }]
                                : []),
                              "sep",
                              { label: "複製名稱", onClick: () => copyText(t.name) },
                              { label: `複製完整名稱（${d.name}.${t.name}）`, onClick: () => copyText(full) },
                              {
                                label: "複製 SELECT 語句",
                                onClick: () => copyText(`SELECT * FROM ${full} LIMIT 100;`),
                              },
                            ]);
                          }}
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
