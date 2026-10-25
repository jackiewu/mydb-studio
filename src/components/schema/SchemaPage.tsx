import { useCallback, useEffect, useState } from "react";
import {
  api,
  describeError,
  type DatabaseInfo,
  type DatabaseOptions,
  type ObjectKind,
  type ObjectList,
  type SchemaApplied,
  type SchemaChange,
  type SchemaMeta,
  type SchemaObject,
  type TableInfo,
} from "../../api";
import { openMenu } from "../../contextMenu";
import { OBJECT_LABEL, objectTemplate } from "../../ddl";
import type { EditorTheme } from "../../editor/themes";
import { SYSTEM_DBS } from "../../schema";
import { CodeArea } from "./CodeArea";
import { DdlPlanDialog } from "./DdlPlanDialog";
import { TableDesigner } from "./TableDesigner";

/** Where the page opens: a database, and maybe a table to design. */
export interface SchemaTarget {
  database: string;
  table?: string;
  nonce: number;
}

interface Props {
  sessionId: string;
  mariadb: boolean;
  databases: DatabaseInfo[] | null;
  showSystem: boolean;
  currentDb: string | null;
  target: SchemaTarget | null;
  theme: EditorTheme;
  fontSize: number;
  /** A change ran: databases, tables or objects may differ now. */
  onChanged: (currentDb: string | null) => void;
  onOpenSql: (sql: string) => void;
}

type Tab = "tables" | ObjectKind;

const TABS: { id: Tab; label: string }[] = [
  { id: "tables", label: "資料表" },
  { id: "view", label: "檢視" },
  { id: "procedure", label: "預存程序" },
  { id: "function", label: "函式" },
  { id: "trigger", label: "觸發器" },
  { id: "event", label: "事件" },
];

interface Pending {
  title: string;
  change: SchemaChange;
  confirmName?: string;
  after?: (result: SchemaApplied) => void;
}

export function SchemaPage(props: Props) {
  const { sessionId, mariadb, databases, showSystem, currentDb, target, onChanged } = props;
  const [db, setDb] = useState<string | null>(target?.database ?? currentDb);
  const [filter, setFilter] = useState("");
  const [meta, setMeta] = useState<SchemaMeta | null>(null);
  const [options, setOptions] = useState<DatabaseOptions | null>(null);
  const [tab, setTab] = useState<Tab>("tables");
  const [tables, setTables] = useState<TableInfo[] | null>(null);
  const [objects, setObjects] = useState<ObjectList | null>(null);
  // The table in the designer: a name, "" for a new one, null for none.
  const [designing, setDesigning] = useState<string | null>(target?.table ?? null);
  // Bumped after a change so the designer reloads the table.
  const [designVersion, setDesignVersion] = useState(0);
  const [selected, setSelected] = useState<{ kind: ObjectKind; name: string | null } | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [dbDialog, setDbDialog] = useState<"create" | "alter" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    api.schemaMeta(sessionId).then(setMeta, (e) => setError(describeError(e)));
  }, [sessionId]);

  useEffect(() => {
    if (!target) return;
    setDb(target.database);
    setTab("tables");
    setDesigning(target.table ?? null);
    setSelected(null);
  }, [target]);

  const loadDb = useCallback(() => {
    setError(null);
    if (!db) return;
    api.databaseOptions(sessionId, db).then(setOptions, (e) => setError(describeError(e)));
    api.listTables(sessionId, db).then(setTables, (e) => setError(describeError(e)));
    api.schemaObjects(sessionId, db).then(setObjects, (e) => setError(describeError(e)));
  }, [sessionId, db]);

  useEffect(() => {
    setOptions(null);
    setTables(null);
    setObjects(null);
    loadDb();
  }, [loadDb]);

  const ran = (result: SchemaApplied, text: string) => {
    setMessage(`${text}${result.record_id != null ? `（已存進紀錄 #${result.record_id}）` : ""}`);
    onChanged(result.current_database);
    loadDb();
  };

  const run = (p: Pending) => setPending(p);

  const f = filter.trim().toLowerCase();
  const shown = (databases ?? []).filter(
    (d) => (showSystem || !SYSTEM_DBS.has(d.name)) && (!f || d.name.toLowerCase().includes(f)),
  );
  const kindObjects = (kind: ObjectKind) => (objects?.objects ?? []).filter((o) => o.kind === kind);
  const baseTables = (tables ?? []).filter((t) => t.kind === "BASE TABLE");

  return (
    <div className="users-panel schema-page">
      <div className="users-list">
        <div className="users-list-head">
          <input type="search" placeholder="篩選資料庫…" value={filter} onChange={(e) => setFilter(e.target.value)} />
          <button className="small" onClick={() => setDbDialog("create")} title="建立資料庫">
            ＋
          </button>
        </div>
        <ul>
          {shown.map((d) => (
            <li
              key={d.name}
              className={d.name === db ? "current" : undefined}
              onClick={() => {
                setDb(d.name);
                setDesigning(null);
                setSelected(null);
                setMessage(null);
              }}
            >
              <span className="user-name">{d.name}</span>
              {d.name === currentDb && <span className="current-mark">USE</span>}
              <span className="spacer" />
              <span className="muted small-text">{d.table_count}</span>
            </li>
          ))}
        </ul>
      </div>

      <div className="user-detail">
        {message && (
          <div className="notice info" onClick={() => setMessage(null)} title="點一下關閉">
            {message}
          </div>
        )}
        {error && <div className="error-box">{error}</div>}
        {!db && <div className="hint pad">選擇左邊的資料庫。</div>}
        {db && (
          <div className="schema-detail">
            <div className="user-head schema-head">
              <h2>{db}</h2>
              {options && (
                <span className="badge" title="資料庫預設字元集與定序">
                  {options.charset} · {options.collation}
                </span>
              )}
              <span className="spacer" />
              <button onClick={() => setDbDialog("alter")} disabled={!options || !meta}>
                字元集／定序…
              </button>
              <button
                className="danger"
                disabled={SYSTEM_DBS.has(db)}
                onClick={() =>
                  run({
                    title: `刪除資料庫 ${db}`,
                    change: { kind: "drop_database", name: db },
                    confirmName: db,
                    after: (r) => {
                      ran(r, `已刪除資料庫 ${db}`);
                      setDb(null);
                    },
                  })
                }
              >
                刪除資料庫
              </button>
            </div>
            <div className="tabs schema-tabs">
              {TABS.map((t) => (
                <button
                  key={t.id}
                  className={tab === t.id ? "active" : undefined}
                  onClick={() => {
                    setTab(t.id);
                    setSelected(null);
                  }}
                >
                  {t.label}
                  <small className="muted">
                    {" "}
                    {t.id === "tables" ? (tables ? baseTables.length : "") : objects ? kindObjects(t.id).length : ""}
                  </small>
                </button>
              ))}
            </div>

            {tab === "tables" && designing != null && meta && (
              <TableDesigner
                key={`${db}/${designing}/${designVersion}`}
                sessionId={sessionId}
                database={db}
                table={designing || null}
                meta={meta}
                dbCharset={options?.charset ?? null}
                dbCollation={options?.collation ?? null}
                onClose={() => setDesigning(null)}
                onSaved={(name, r) => {
                  ran(r, `已更新資料表 ${name}`);
                  setDesigning(name);
                  setDesignVersion((v) => v + 1);
                }}
              />
            )}
            {tab === "tables" && designing == null && (
              <TableList
                tables={baseTables}
                onDesign={setDesigning}
                onNew={() => setDesigning("")}
                onDrop={(t) =>
                  run({
                    title: `刪除資料表 ${db}.${t}`,
                    change: { kind: "drop_table", database: db, table: t },
                    confirmName: t,
                    after: (r) => ran(r, `已刪除資料表 ${t}`),
                  })
                }
                onOpenSql={props.onOpenSql}
                database={db}
              />
            )}

            {tab !== "tables" && (
              <div className="objects-panel">
                <div className="objects-list">
                  {tab === "event" && objects && (
                    <div className="scheduler">
                      <span>
                        event_scheduler：
                        <strong className={objects.event_scheduler === "ON" ? "good" : "warn"}>
                          {objects.event_scheduler ?? "?"}
                        </strong>
                      </span>
                      {objects.event_scheduler !== "DISABLED" && (
                        <button
                          className="small"
                          onClick={() =>
                            run({
                              title: objects.event_scheduler === "ON" ? "關閉事件排程器" : "開啟事件排程器",
                              change: { kind: "set_event_scheduler", on: objects.event_scheduler !== "ON" },
                              after: (r) => ran(r, "已變更 event_scheduler"),
                            })
                          }
                        >
                          {objects.event_scheduler === "ON" ? "關閉" : "開啟"}
                        </button>
                      )}
                      {objects.event_scheduler !== "ON" && kindObjects("event").length > 0 && (
                        <div className="field-note bad">排程器沒有開，事件不會執行。</div>
                      )}
                    </div>
                  )}
                  <ul>
                    {kindObjects(tab).map((o) => (
                      <li
                        key={o.name}
                        className={selected?.name === o.name ? "current" : undefined}
                        onClick={() => setSelected({ kind: tab, name: o.name })}
                        title={[o.detail, o.definer && `DEFINER ${o.definer}`, o.modified && `修改於 ${o.modified}`, o.comment]
                          .filter(Boolean)
                          .join("\n")}
                      >
                        <span className="user-name">{o.name}</span>
                        {o.enabled === false && <span className="badge">停用</span>}
                        <span className="muted small-text object-detail">{o.detail}</span>
                      </li>
                    ))}
                    {objects && kindObjects(tab).length === 0 && <li className="hint">沒有{OBJECT_LABEL[tab]}</li>}
                  </ul>
                  <button className="link" onClick={() => setSelected({ kind: tab, name: null })}>
                    ＋ 新增{OBJECT_LABEL[tab]}
                  </button>
                </div>
                {selected && (
                  <ObjectEditor
                    key={`${db}/${selected.kind}/${selected.name ?? ""}`}
                    {...props}
                    database={db}
                    kind={selected.kind}
                    name={selected.name}
                    object={kindObjects(selected.kind).find((o) => o.name === selected.name) ?? null}
                    tables={baseTables.map((t) => t.name)}
                    mariadb={mariadb}
                    onRun={(p) => run(p)}
                    onSaved={(name, r) => {
                      ran(r, `已儲存${OBJECT_LABEL[selected.kind]} ${name}`);
                      setSelected({ kind: selected.kind, name });
                    }}
                    onDropped={(r) => {
                      ran(r, `已刪除${OBJECT_LABEL[selected.kind]} ${selected.name}`);
                      setSelected(null);
                    }}
                  />
                )}
              </div>
            )}
          </div>
        )}
      </div>

      {dbDialog && meta && (
        <DatabaseDialog
          mode={dbDialog}
          meta={meta}
          options={dbDialog === "alter" ? options : null}
          onCancel={() => setDbDialog(null)}
          onNext={(change) => {
            setDbDialog(null);
            const name = "name" in change ? change.name : "";
            run({
              title: change.kind === "create_database" ? `建立資料庫 ${name}` : `變更資料庫 ${name} 的字元集`,
              change,
              after: (r) => {
                ran(r, change.kind === "create_database" ? `已建立資料庫 ${name}` : `已變更 ${name} 的字元集`);
                setDb(name);
              },
            });
          }}
        />
      )}
      {pending && (
        <DdlPlanDialog
          sessionId={sessionId}
          title={pending.title}
          change={pending.change}
          confirmName={pending.confirmName}
          onCancel={() => setPending(null)}
          onDone={(r) => {
            const p = pending;
            setPending(null);
            if (r.error) {
              // It may still have changed something (and is in the records).
              onChanged(r.current_database);
              loadDb();
            } else p.after?.(r);
          }}
        />
      )}
    </div>
  );
}

function TableList(props: {
  database: string;
  tables: TableInfo[];
  onDesign: (t: string) => void;
  onNew: () => void;
  onDrop: (t: string) => void;
  onOpenSql: (sql: string) => void;
}) {
  const { tables } = props;
  return (
    <div className="schema-tables">
      <div className="designer-foot">
        <button className="primary" onClick={props.onNew}>
          ＋ 新資料表
        </button>
        <span className="muted small-text">點兩下資料表開啟設計器；右鍵有更多動作。</span>
      </div>
      <table className="grid records-table">
        <thead>
          <tr>
            <th>名稱</th>
            <th>引擎</th>
            <th>筆數（估計）</th>
            <th>大小</th>
            <th>註解</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {tables.map((t) => (
            <tr
              key={t.name}
              onDoubleClick={() => props.onDesign(t.name)}
              onContextMenu={(e) =>
                openMenu(e, [
                  { label: "設計資料表…", onClick: () => props.onDesign(t.name) },
                  {
                    label: "在編輯器查詢前 100 筆",
                    onClick: () =>
                      props.onOpenSql(`SELECT * FROM \`${props.database.replace(/`/g, "``")}\`.\`${t.name.replace(/`/g, "``")}\` LIMIT 100;`),
                  },
                  "sep",
                  { label: "刪除資料表…", danger: true, onClick: () => props.onDrop(t.name) },
                ])
              }
            >
              <td>{t.name}</td>
              <td>{t.engine}</td>
              <td className="num">{t.rows_estimate?.toLocaleString() ?? ""}</td>
              <td className="num">{t.size_bytes != null ? sizeText(t.size_bytes) : ""}</td>
              <td className="muted">{t.comment}</td>
              <td className="row-actions">
                <button className="small" onClick={() => props.onDesign(t.name)}>
                  設計
                </button>
                <button className="small ghost danger" onClick={() => props.onDrop(t.name)} title="刪除資料表">
                  ✕
                </button>
              </td>
            </tr>
          ))}
          {tables.length === 0 && (
            <tr>
              <td colSpan={6} className="hint">
                沒有資料表
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

function sizeText(n: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n < 10 && i ? 1 : 0)} ${units[i]}`;
}

function ObjectEditor(props: {
  sessionId: string;
  database: string;
  kind: ObjectKind;
  /** null for a new object. */
  name: string | null;
  object: SchemaObject | null;
  tables: string[];
  mariadb: boolean;
  theme: EditorTheme;
  fontSize: number;
  onRun: (p: Pending) => void;
  onSaved: (name: string, r: SchemaApplied) => void;
  onDropped: (r: SchemaApplied) => void;
  onOpenSql: (sql: string) => void;
}) {
  const { sessionId, database, kind, name, object } = props;
  const [loaded, setLoaded] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [sqlMode, setSqlMode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (name == null) {
      const t = objectTemplate(kind, props.tables);
      setLoaded(t);
      setText(t);
      return;
    }
    api.objectDefinition(sessionId, database, kind, name).then(
      (d) => {
        setLoaded(d.definition);
        setText(d.definition);
        setSqlMode(d.sql_mode);
      },
      (e) => setError(describeError(e)),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, database, kind, name]);

  const label = OBJECT_LABEL[kind];
  const save = () =>
    props.onRun({
      title: name ? `儲存${label} ${name}` : `建立${label}`,
      change: { kind: "save_object", database, object_kind: kind, original: name, definition: text },
      after: (r) => props.onSaved(nameIn(text) ?? name ?? "", r),
    });

  return (
    <div className="object-editor">
      <div className="object-editor-head">
        <strong>{name ?? `新${label}`}</strong>
        {object?.detail && <span className="muted small-text">{object.detail}</span>}
        <span className="spacer" />
        {kind === "event" && object && (
          <button
            onClick={() =>
              props.onRun({
                title: `${object.enabled ? "停用" : "啟用"}事件 ${object.name}`,
                change: { kind: "set_event_enabled", database, name: object.name, enabled: !object.enabled },
                after: (r) => props.onSaved(object.name, r),
              })
            }
          >
            {object.enabled ? "停用" : "啟用"}
          </button>
        )}
        <button onClick={() => props.onOpenSql(text + "\n")} title="複製到新的查詢分頁">
          在編輯器開啟
        </button>
        {name && (
          <button
            className="danger"
            onClick={() =>
              props.onRun({
                title: `刪除${label} ${name}`,
                change: { kind: "drop_object", database, object_kind: kind, name },
                confirmName: name,
                after: props.onDropped,
              })
            }
          >
            刪除
          </button>
        )}
        <button onClick={() => loaded != null && setText(loaded)} disabled={text === loaded}>
          還原
        </button>
        <button className="primary" onClick={save} disabled={loaded == null || (name != null && text === loaded)}>
          預覽並儲存…
        </button>
      </div>
      {error && <div className="error-box">{error}</div>}
      <p className="muted small-text object-hint">
        整段 CREATE 語句會當成一句送出，不需要 DELIMITER。
        {name &&
          (props.mariadb || kind === "view"
            ? "儲存時用 CREATE OR REPLACE，失敗的話原本的不受影響。"
            : "MySQL 會先刪除再建立；建立失敗時會自動用原本的定義還原。")}
        {sqlMode && ` 建立時的 sql_mode：${sqlMode}`}
      </p>
      {loaded != null && (
        <div className="object-code">
          <CodeArea value={text} mariadb={props.mariadb} theme={props.theme} fontSize={props.fontSize} onChange={setText} />
        </div>
      )}
    </div>
  );
}

/** The object name in a CREATE statement, to select it after saving. */
function nameIn(sql: string): string | null {
  const id = "(`(?:[^`]|``)+`|[\\w$]+)";
  const m = new RegExp(`\\b(?:VIEW|PROCEDURE|FUNCTION|TRIGGER|EVENT)\\s+(?:IF\\s+NOT\\s+EXISTS\\s+)?${id}(?:\\s*\\.\\s*${id})?`, "i").exec(sql);
  if (!m) return null;
  const name = m[2] ?? m[1];
  return name.startsWith("`") ? name.slice(1, -1).replace(/``/g, "`") : name;
}

function DatabaseDialog(props: {
  mode: "create" | "alter";
  meta: SchemaMeta;
  options: DatabaseOptions | null;
  onCancel: () => void;
  onNext: (change: SchemaChange) => void;
}) {
  const { meta, options } = props;
  const [name, setName] = useState("");
  const [charset, setCharset] = useState(options?.charset ?? meta.server_charset);
  const [collation, setCollation] = useState(options?.collation ?? meta.server_collation);
  const [convert, setConvert] = useState(false);
  const cs = meta.charsets.find((c) => c.name === charset);
  const create = props.mode === "create";
  const unchanged = !create && options?.charset === charset && options?.collation === collation && !convert;

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onCancel()}>
      <div className="risk-dialog plan-dialog admin-form" role="dialog">
        <h2>{create ? "建立資料庫" : `${options?.name} 的字元集與定序`}</h2>
        {create && (
          <label>
            名稱
            <input value={name} onChange={(e) => setName(e.target.value)} spellCheck={false} autoFocus />
          </label>
        )}
        <div className="row">
          <label>
            字元集
            <select
              value={charset}
              onChange={(e) => {
                setCharset(e.target.value);
                setCollation(meta.charsets.find((c) => c.name === e.target.value)?.default_collation ?? "");
              }}
            >
              {meta.charsets.map((c) => (
                <option key={c.name} value={c.name}>
                  {c.name} — {c.description}
                </option>
              ))}
            </select>
          </label>
          <label>
            定序
            <select value={collation} onChange={(e) => setCollation(e.target.value)}>
              {(cs?.collations ?? []).map((c) => (
                <option key={c} value={c}>
                  {c}
                  {c === cs?.default_collation ? "（預設）" : ""}
                </option>
              ))}
            </select>
          </label>
        </div>
        {!charset.startsWith("utf8mb4") && <p className="field-note bad">建議使用 utf8mb4，才能存 emoji 與所有中文字。</p>}
        {!create && (
          <label className="check">
            <input type="checkbox" checked={convert} onChange={(e) => setConvert(e.target.checked)} />
            同時轉換資料庫裡所有資料表與文字欄位（每個資料表一句 ALTER TABLE … CONVERT TO）
          </label>
        )}
        <div className="actions">
          <span className="spacer" />
          <button onClick={props.onCancel}>取消</button>
          <button
            className="primary"
            disabled={(create && !name.trim()) || unchanged}
            onClick={() =>
              props.onNext(
                create
                  ? { kind: "create_database", name: name.trim(), charset, collation }
                  : { kind: "alter_database", name: options!.name, charset, collation, convert_tables: convert },
              )
            }
          >
            預覽 SQL…
          </button>
        </div>
      </div>
    </div>
  );
}
