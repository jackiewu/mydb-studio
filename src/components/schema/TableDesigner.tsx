import { useEffect, useMemo, useState } from "react";
import {
  api,
  describeError,
  type ColumnDef,
  type ColumnDesign,
  type ForeignKeyDesign,
  type IndexDesign,
  type IndexKind,
  type SchemaApplied,
  type SchemaChange,
  type SchemaMeta,
  type TableDesign,
} from "../../api";
import { copyText } from "../../copy";
import { Loading, LoadError } from "./Loading";
import {
  COMMON_TYPES,
  defaultSql,
  FK_RULES,
  INDEX_LABEL,
  indexName,
  isTextType,
  moveColumn,
  newColumn,
  newTable,
  part,
  removeColumn,
  renameColumn,
} from "../../ddl";
import { DdlPlanDialog } from "./DdlPlanDialog";

interface Props {
  sessionId: string;
  database: string;
  /** null designs a new table. */
  table: string | null;
  meta: SchemaMeta;
  /** The database's defaults, for a new table. */
  dbCharset: string | null;
  dbCollation: string | null;
  /** After a change ran; `table` is the table's name now. */
  onSaved: (table: string, result: SchemaApplied) => void;
  onClose: () => void;
}

type Tab = "columns" | "indexes" | "keys" | "options" | "ddl";

/**
 * Edits a table's columns, indexes, foreign keys and options, then shows
 * the one ALTER TABLE (or CREATE TABLE) that makes the change.
 */
export function TableDesigner({ sessionId, database, table, meta, dbCharset, dbCollation, onSaved, onClose }: Props) {
  const [original, setOriginal] = useState<TableDesign | null>(null);
  const [design, setDesign] = useState<TableDesign | null>(null);
  const [ddl, setDdl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("columns");
  const [convert, setConvert] = useState(false);
  const [pending, setPending] = useState<SchemaChange | null>(null);
  const [expanded, setExpanded] = useState<number | null>(null);
  // Columns of every table in the database, for foreign keys.
  const [dbColumns, setDbColumns] = useState<ColumnDef[]>([]);

  const load = () => {
    setError(null);
    if (table == null) {
      const t = newTable(meta.engines[0] ?? null, dbCharset, dbCollation);
      setOriginal(null);
      setDesign(t);
      setDdl("");
      return;
    }
    api.tableDesign(sessionId, database, table).then(
      (t) => {
        setOriginal(t);
        setDesign(t);
        setConvert(false);
      },
      (e) => setError(describeError(e)),
    );
    api.showCreateTable(sessionId, database, table).then(setDdl, () => setDdl(""));
  };
  useEffect(load, [sessionId, database, table]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    api.listColumns(sessionId, database).then(setDbColumns, () => {});
  }, [sessionId, database]);

  const origCols = useMemo(() => new Map((original?.columns ?? []).map((c) => [c.name, c])), [original]);
  const changes = useMemo(() => (design ? countChanges(original, design, convert) : 0), [original, design, convert]);

  if (error && !design) return <LoadError what={`資料表 ${table} 的欄位與索引`} error={error} onRetry={load} />;
  if (!design) return <Loading what={`資料表 ${table} 的欄位與索引`} />;

  const set = (next: TableDesign) => setDesign(next);
  const setCol = (i: number, patch: Partial<ColumnDesign>) =>
    set({ ...design, columns: design.columns.map((c, j) => (j === i ? { ...c, ...patch } : c)) });
  const setIndex = (i: number, patch: Partial<IndexDesign>) =>
    set({ ...design, indexes: design.indexes.map((x, j) => (j === i ? { ...x, ...patch } : x)) });
  const setFk = (i: number, patch: Partial<ForeignKeyDesign>) =>
    set({ ...design, foreign_keys: design.foreign_keys.map((x, j) => (j === i ? { ...x, ...patch } : x)) });
  const columnNames = design.columns.map((c) => c.name).filter(Boolean);
  const charset = meta.charsets.find((c) => c.name === design.options.charset);
  const tables = [...new Set(dbColumns.map((c) => c.table))];

  const colState = (c: ColumnDesign) => {
    if (c.original == null) return "added";
    const o = origCols.get(c.original);
    return o && JSON.stringify({ ...o, original: null }) !== JSON.stringify({ ...c, original: null }) ? "changed" : "";
  };

  const preview = () => {
    if (!design.name.trim()) {
      setTab("options");
      setError("請輸入資料表名稱");
      return;
    }
    setError(null);
    setPending(
      original
        ? { kind: "alter_table", database, original, design, convert_charset: convert }
        : { kind: "create_table", database, design },
    );
  };

  return (
    <div className="designer">
      <div className="designer-head">
        <label className="designer-name">
          資料表
          <input
            value={design.name}
            placeholder="新資料表名稱"
            spellCheck={false}
            onChange={(e) => set({ ...design, name: e.target.value })}
          />
        </label>
        {original && design.name !== original.name && <span className="badge">改名自 {original.name}</span>}
        <span className="spacer" />
        <span className="muted small-text">{changes ? `${changes} 處變更` : original ? "沒有變更" : ""}</span>
        <button onClick={load} disabled={!changes} title="放棄還沒執行的修改">
          還原
        </button>
        <button onClick={onClose}>關閉</button>
        <button className="primary" disabled={original != null && !changes} onClick={preview}>
          {original ? "預覽 ALTER TABLE…" : "預覽 CREATE TABLE…"}
        </button>
      </div>
      <div className="tabs designer-tabs">
        {(
          [
            ["columns", `欄位（${design.columns.length}）`],
            ["indexes", `索引（${design.indexes.length}）`],
            ["keys", `外鍵（${design.foreign_keys.length}）`],
            ["options", "資料表選項"],
            ...(original ? [["ddl", "DDL"]] : []),
          ] as [Tab, string][]
        ).map(([id, label]) => (
          <button key={id} className={tab === id ? "active" : undefined} onClick={() => setTab(id)}>
            {label}
          </button>
        ))}
      </div>
      {error && <div className="error-box">{error}</div>}

      <div className="designer-body">
        {tab === "columns" && (
          <>
            <datalist id="column-types">
              {COMMON_TYPES.map((t) => (
                <option key={t} value={t} />
              ))}
            </datalist>
            <table className="grid design-grid">
              <thead>
                <tr>
                  <th />
                  <th>名稱</th>
                  <th>型別</th>
                  <th title="可以是 NULL">NULL</th>
                  <th title="以 SQL 表示：'文字'、0、CURRENT_TIMESTAMP、(uuid())。文字欄位直接打字會自動加上引號。">預設值</th>
                  <th title="AUTO_INCREMENT">自動遞增</th>
                  <th>註解</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {design.columns.map((c, i) => {
                  const state = colState(c);
                  return [
                    <tr key={`c${i}`} className={state}>
                      <td className="row-handle">
                        <button className="small ghost" disabled={i === 0} onClick={() => set(moveColumn(design, i, -1))} title="上移">
                          ↑
                        </button>
                        <button
                          className="small ghost"
                          disabled={i === design.columns.length - 1}
                          onClick={() => set(moveColumn(design, i, 1))}
                          title="下移"
                        >
                          ↓
                        </button>
                      </td>
                      <td>
                        <input
                          value={c.name}
                          spellCheck={false}
                          onChange={(e) => set(renameColumn(design, i, e.target.value))}
                          title={c.original && c.original !== c.name ? `原本是 ${c.original}` : undefined}
                        />
                      </td>
                      <td>
                        <input
                          list="column-types"
                          value={c.column_type}
                          spellCheck={false}
                          onChange={(e) => {
                            const t = e.target.value;
                            setCol(i, isTextType(t) ? { column_type: t } : { column_type: t, charset: null, collation: null });
                          }}
                        />
                      </td>
                      <td className="center">
                        <input type="checkbox" checked={c.nullable} onChange={(e) => setCol(i, { nullable: e.target.checked })} />
                      </td>
                      <td>
                        <DefaultInput
                          value={c.default}
                          disabled={c.generated != null || c.auto_increment}
                          onChange={(v) => setCol(i, { default: v === null ? null : defaultSql(v, c.column_type) })}
                        />
                      </td>
                      <td className="center">
                        <input
                          type="checkbox"
                          checked={c.auto_increment}
                          onChange={(e) => setCol(i, { auto_increment: e.target.checked, default: e.target.checked ? null : c.default })}
                        />
                      </td>
                      <td>
                        <input value={c.comment} onChange={(e) => setCol(i, { comment: e.target.value })} />
                      </td>
                      <td className="row-actions">
                        <button
                          className={expanded === i ? "small active" : "small ghost"}
                          onClick={() => setExpanded(expanded === i ? null : i)}
                          title="字元集、ON UPDATE、產生欄位…"
                        >
                          ⋯
                        </button>
                        <button className="small ghost danger" onClick={() => set(removeColumn(design, i))} title="刪除這個欄位">
                          ✕
                        </button>
                      </td>
                    </tr>,
                    expanded === i && (
                      <tr key={`x${i}`} className="column-more">
                        <td />
                        <td colSpan={7}>
                          <ColumnMore column={c} meta={meta} onChange={(patch) => setCol(i, patch)} />
                        </td>
                      </tr>
                    ),
                  ];
                })}
              </tbody>
            </table>
            <div className="designer-foot">
              <button onClick={() => set({ ...design, columns: [...design.columns, newColumn(`column_${design.columns.length + 1}`)] })}>
                ＋ 新增欄位
              </button>
              {original &&
                original.columns
                  .filter((o) => !design.columns.some((c) => c.original === o.name))
                  .map((o) => (
                    <span key={o.name} className="badge bad-badge" title="執行後會刪除這個欄位與它的資料">
                      刪除 {o.name}
                    </span>
                  ))}
            </div>
          </>
        )}

        {tab === "indexes" && (
          <>
            <table className="grid design-grid">
              <thead>
                <tr>
                  <th>名稱</th>
                  <th>種類</th>
                  <th>欄位（依序）</th>
                  <th>註解</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {design.indexes.map((ix, i) => (
                  <tr key={i} className={ix.original == null ? "added" : indexChanged(original, ix) ? "changed" : ""}>
                    <td>
                      <input
                        value={ix.kind === "primary" ? "PRIMARY" : ix.name}
                        disabled={ix.kind === "primary"}
                        spellCheck={false}
                        onChange={(e) => setIndex(i, { name: e.target.value })}
                      />
                    </td>
                    <td>
                      <select
                        value={ix.kind}
                        onChange={(e) => {
                          const kind = e.target.value as IndexKind;
                          setIndex(i, {
                            kind,
                            name: kind === "primary" ? "PRIMARY" : ix.kind === "primary" ? indexName(design, kind, ix.parts.map((p) => p.column)) : ix.name,
                          });
                        }}
                      >
                        {(Object.keys(INDEX_LABEL) as IndexKind[])
                          .filter((k) => k !== "primary" || ix.kind === "primary" || !design.indexes.some((x) => x.kind === "primary"))
                          .map((k) => (
                            <option key={k} value={k}>
                              {INDEX_LABEL[k]}
                            </option>
                          ))}
                      </select>
                    </td>
                    <td>
                      <div className="index-parts">
                        {ix.parts.map((p, j) => (
                          <span key={j} className="index-part">
                            {p.expression != null ? (
                              <code title="運算式索引">({p.expression})</code>
                            ) : (
                              <select
                                value={p.column}
                                onChange={(e) =>
                                  setIndex(i, { parts: ix.parts.map((q, k) => (k === j ? { ...q, column: e.target.value } : q)) })
                                }
                              >
                                {!columnNames.includes(p.column) && <option value={p.column}>{p.column}</option>}
                                {columnNames.map((n) => (
                                  <option key={n} value={n}>
                                    {n}
                                  </option>
                                ))}
                              </select>
                            )}
                            <input
                              className="part-length"
                              placeholder="長度"
                              title="前綴長度（只索引前 N 個字元）"
                              value={p.length ?? ""}
                              onChange={(e) => {
                                const n = parseInt(e.target.value, 10);
                                setIndex(i, { parts: ix.parts.map((q, k) => (k === j ? { ...q, length: n > 0 ? n : null } : q)) });
                              }}
                            />
                            <label className="check small-text" title="遞減排序（MySQL 8／MariaDB 10.8 以上）">
                              <input
                                type="checkbox"
                                checked={p.descending}
                                onChange={(e) =>
                                  setIndex(i, { parts: ix.parts.map((q, k) => (k === j ? { ...q, descending: e.target.checked } : q)) })
                                }
                              />
                              DESC
                            </label>
                            <button
                              className="small ghost"
                              onClick={() => setIndex(i, { parts: ix.parts.filter((_, k) => k !== j) })}
                              title="從索引拿掉"
                            >
                              ✕
                            </button>
                          </span>
                        ))}
                        <select
                          value=""
                          onChange={(e) => e.target.value && setIndex(i, { parts: [...ix.parts, part(e.target.value)] })}
                        >
                          <option value="">＋ 欄位</option>
                          {columnNames
                            .filter((n) => !ix.parts.some((p) => p.column === n))
                            .map((n) => (
                              <option key={n} value={n}>
                                {n}
                              </option>
                            ))}
                        </select>
                      </div>
                    </td>
                    <td>
                      <input value={ix.comment} onChange={(e) => setIndex(i, { comment: e.target.value })} />
                    </td>
                    <td className="row-actions">
                      <button
                        className="small ghost danger"
                        onClick={() => set({ ...design, indexes: design.indexes.filter((_, j) => j !== i) })}
                        title="刪除這個索引"
                      >
                        ✕
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            <div className="designer-foot">
              {(["index", "unique", "fulltext", "primary"] as IndexKind[])
                .filter((k) => k !== "primary" || !design.indexes.some((x) => x.kind === "primary"))
                .map((k) => (
                  <button
                    key={k}
                    onClick={() =>
                      set({
                        ...design,
                        indexes: [
                          ...design.indexes,
                          {
                            original: null,
                            name: indexName(design, k, columnNames.slice(0, 1)),
                            kind: k,
                            parts: columnNames[0] ? [part(columnNames[0])] : [],
                            using: null,
                            comment: "",
                          },
                        ],
                      })
                    }
                  >
                    ＋ {INDEX_LABEL[k]}索引
                  </button>
                ))}
              <span className="muted small-text">同一個 ALTER TABLE 裡會先刪除再建立變更過的索引；只改名稱時用 RENAME INDEX。</span>
            </div>
          </>
        )}

        {tab === "keys" && (
          <>
            <table className="grid design-grid">
              <thead>
                <tr>
                  <th>名稱</th>
                  <th>欄位</th>
                  <th>參照資料表</th>
                  <th>參照欄位</th>
                  <th>ON DELETE</th>
                  <th>ON UPDATE</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {design.foreign_keys.map((fk, i) => {
                  const refCols = dbColumns.filter((c) => c.table === fk.ref_table).map((c) => c.name);
                  return (
                    <tr key={i} className={fk.original == null ? "added" : fkChanged(original, fk) ? "changed" : ""}>
                      <td>
                        <input value={fk.name} spellCheck={false} onChange={(e) => setFk(i, { name: e.target.value })} />
                      </td>
                      <td>
                        <ListPicker values={fk.columns} options={columnNames} onChange={(columns) => setFk(i, { columns })} />
                      </td>
                      <td>
                        {fk.ref_database ? (
                          <code>
                            {fk.ref_database}.{fk.ref_table}
                          </code>
                        ) : (
                          <select value={fk.ref_table} onChange={(e) => setFk(i, { ref_table: e.target.value, ref_columns: [] })}>
                            {!tables.includes(fk.ref_table) && <option value={fk.ref_table}>{fk.ref_table || "（選擇）"}</option>}
                            {tables.map((t) => (
                              <option key={t} value={t}>
                                {t}
                              </option>
                            ))}
                          </select>
                        )}
                      </td>
                      <td>
                        <ListPicker
                          values={fk.ref_columns}
                          options={refCols.length ? refCols : fk.ref_columns}
                          onChange={(ref_columns) => setFk(i, { ref_columns })}
                        />
                      </td>
                      {(["on_delete", "on_update"] as const).map((k) => (
                        <td key={k}>
                          <select value={fk[k] || "RESTRICT"} onChange={(e) => setFk(i, { [k]: e.target.value })}>
                            {FK_RULES.map((r) => (
                              <option key={r}>{r}</option>
                            ))}
                          </select>
                        </td>
                      ))}
                      <td className="row-actions">
                        <button
                          className="small ghost danger"
                          onClick={() => set({ ...design, foreign_keys: design.foreign_keys.filter((_, j) => j !== i) })}
                          title="刪除這個外鍵"
                        >
                          ✕
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            <div className="designer-foot">
              <button
                onClick={() =>
                  set({
                    ...design,
                    foreign_keys: [
                      ...design.foreign_keys,
                      {
                        original: null,
                        name: `fk_${design.name || "table"}_${design.foreign_keys.length + 1}`,
                        columns: [],
                        ref_database: null,
                        ref_table: "",
                        ref_columns: [],
                        on_delete: "RESTRICT",
                        on_update: "RESTRICT",
                      },
                    ],
                  })
                }
              >
                ＋ 新增外鍵
              </button>
              <span className="muted small-text">變更過的外鍵會先用另一句 ALTER TABLE 刪除，再在主要的 ALTER TABLE 裡重建。</span>
            </div>
          </>
        )}

        {tab === "options" && (
          <div className="admin-form designer-options">
            <div className="row">
              <label>
                儲存引擎
                <select
                  value={design.options.engine ?? ""}
                  onChange={(e) => set({ ...design, options: { ...design.options, engine: e.target.value || null } })}
                >
                  {design.options.engine && !meta.engines.includes(design.options.engine) && (
                    <option value={design.options.engine}>{design.options.engine}</option>
                  )}
                  {meta.engines.map((e, i) => (
                    <option key={e} value={e}>
                      {e}
                      {i === 0 ? "（預設）" : ""}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                下一個 AUTO_INCREMENT
                <input
                  value={design.options.auto_increment ?? ""}
                  placeholder="自動"
                  onChange={(e) => {
                    const n = parseInt(e.target.value, 10);
                    set({ ...design, options: { ...design.options, auto_increment: n > 0 ? n : null } });
                  }}
                />
              </label>
            </div>
            <div className="row">
              <label>
                預設字元集
                <select
                  value={design.options.charset ?? ""}
                  onChange={(e) => {
                    const cs = meta.charsets.find((c) => c.name === e.target.value);
                    set({
                      ...design,
                      options: { ...design.options, charset: cs?.name ?? null, collation: cs?.default_collation ?? null },
                    });
                  }}
                >
                  <option value="">（資料庫預設）</option>
                  {meta.charsets.map((c) => (
                    <option key={c.name} value={c.name}>
                      {c.name} — {c.description}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                預設定序
                <select
                  value={design.options.collation ?? ""}
                  onChange={(e) => set({ ...design, options: { ...design.options, collation: e.target.value || null } })}
                >
                  <option value="">（字元集預設）</option>
                  {(charset?.collations ?? []).map((c) => (
                    <option key={c} value={c}>
                      {c}
                      {c === charset?.default_collation ? "（預設）" : ""}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            {original && (
              <label className="check">
                <input type="checkbox" checked={convert} onChange={(e) => setConvert(e.target.checked)} />
                同時把現有的文字欄位轉成這個字元集（CONVERT TO，會改寫整個資料表）
              </label>
            )}
            {original && !convert && (design.options.charset !== original.options.charset || design.options.collation !== original.options.collation) && (
              <p className="field-note">只改預設值的話，現有欄位維持原本的字元集，只有之後新增的欄位會用新的設定。</p>
            )}
            <label>
              註解
              <input
                value={design.options.comment}
                onChange={(e) => set({ ...design, options: { ...design.options, comment: e.target.value } })}
              />
            </label>
          </div>
        )}

        {tab === "ddl" && (
          <div className="designer-ddl">
            <div className="actions">
              <button className="small" onClick={() => copyText(ddl + ";")}>
                複製
              </button>
            </div>
            <pre className="plan-sql">{ddl}</pre>
          </div>
        )}
      </div>

      {pending && (
        <DdlPlanDialog
          sessionId={sessionId}
          title={original ? `修改資料表 ${database}.${original.name}` : `建立資料表 ${database}.${design.name}`}
          change={pending}
          onCancel={() => setPending(null)}
          onDone={(result) => {
            setPending(null);
            if (!result.error) onSaved(design.name, result);
            else load();
          }}
        />
      )}
    </div>
  );
}

/** The default field: shows the SQL, and turns typed text into SQL on blur. */
function DefaultInput({ value, disabled, onChange }: { value: string | null; disabled: boolean; onChange: (v: string | null) => void }) {
  const [text, setText] = useState(value ?? "");
  useEffect(() => setText(value ?? ""), [value]);
  return (
    <input
      value={disabled ? "" : text}
      disabled={disabled}
      placeholder={disabled ? "" : "無"}
      spellCheck={false}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== (value ?? "") && onChange(text.trim() ? text : null)}
      onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
    />
  );
}

function ColumnMore({ column: c, meta, onChange }: { column: ColumnDesign; meta: SchemaMeta; onChange: (p: Partial<ColumnDesign>) => void }) {
  const charset = meta.charsets.find((x) => x.name === c.charset);
  const text = isTextType(c.column_type);
  return (
    <div className="column-more-body">
      {text && (
        <>
          <label>
            字元集
            <select
              value={c.charset ?? ""}
              onChange={(e) => {
                const cs = meta.charsets.find((x) => x.name === e.target.value);
                onChange({ charset: cs?.name ?? null, collation: cs?.default_collation ?? null });
              }}
            >
              <option value="">（資料表預設）</option>
              {meta.charsets.map((x) => (
                <option key={x.name} value={x.name}>
                  {x.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            定序
            <select value={c.collation ?? ""} onChange={(e) => onChange({ collation: e.target.value || null })}>
              <option value="">（字元集預設）</option>
              {(charset?.collations ?? []).map((x) => (
                <option key={x} value={x}>
                  {x}
                </option>
              ))}
            </select>
          </label>
        </>
      )}
      <label title="例如 CURRENT_TIMESTAMP">
        ON UPDATE
        <input value={c.on_update ?? ""} spellCheck={false} onChange={(e) => onChange({ on_update: e.target.value || null })} />
      </label>
      <label title="產生欄位（generated column）的運算式；留空表示一般欄位">
        產生欄位運算式
        <input
          value={c.generated ?? ""}
          spellCheck={false}
          placeholder="例：price * qty"
          onChange={(e) => onChange({ generated: e.target.value || null, default: e.target.value ? null : c.default })}
        />
      </label>
      {c.generated != null && (
        <label className="check">
          <input type="checkbox" checked={c.stored} onChange={(e) => onChange({ stored: e.target.checked })} />
          STORED（存起來，否則每次讀取時計算）
        </label>
      )}
      <label className="check" title="SELECT * 不會列出這個欄位（MySQL 8.0.23／MariaDB 10.3 以上）">
        <input type="checkbox" checked={c.invisible} onChange={(e) => onChange({ invisible: e.target.checked })} />
        INVISIBLE
      </label>
    </div>
  );
}

/** Ordered list of names picked from `options`. */
function ListPicker({ values, options, onChange }: { values: string[]; options: string[]; onChange: (v: string[]) => void }) {
  return (
    <div className="index-parts">
      {values.map((v, i) => (
        <span key={i} className="index-part">
          <code>{v}</code>
          <button className="small ghost" onClick={() => onChange(values.filter((_, j) => j !== i))}>
            ✕
          </button>
        </span>
      ))}
      <select value="" onChange={(e) => e.target.value && onChange([...values, e.target.value])}>
        <option value="">＋</option>
        {options
          .filter((o) => !values.includes(o))
          .map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
      </select>
    </div>
  );
}

function indexChanged(original: TableDesign | null, ix: IndexDesign) {
  const o = original?.indexes.find((x) => x.name === ix.original);
  return !o || JSON.stringify(o) !== JSON.stringify(ix);
}

function fkChanged(original: TableDesign | null, fk: ForeignKeyDesign) {
  const o = original?.foreign_keys.find((x) => x.name === fk.original);
  return !o || JSON.stringify(o) !== JSON.stringify(fk);
}

/** Roughly how many things the user changed, for the header. */
function countChanges(original: TableDesign | null, d: TableDesign, convert: boolean): number {
  if (!original) return 1;
  let n = 0;
  const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
  for (const c of d.columns) {
    const o = original.columns.find((x) => x.name === c.original);
    if (!o || !same(o, c)) n++;
  }
  n += original.columns.filter((o) => !d.columns.some((c) => c.original === o.name)).length;
  const order = (t: TableDesign) => t.columns.map((c) => c.original ?? "").join("\n");
  if (n === 0 && order(original) !== order(d)) n++;
  for (const ix of d.indexes) if (indexChanged(original, ix)) n++;
  n += original.indexes.filter((o) => !d.indexes.some((x) => x.original === o.name)).length;
  for (const fk of d.foreign_keys) if (fkChanged(original, fk)) n++;
  n += original.foreign_keys.filter((o) => !d.foreign_keys.some((x) => x.original === o.name)).length;
  if (d.name !== original.name) n++;
  if (!same(d.options, original.options)) n++;
  if (convert) n++;
  return n;
}
