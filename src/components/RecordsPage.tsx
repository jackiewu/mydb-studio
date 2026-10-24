import { useCallback, useEffect, useState } from "react";
import { ask } from "../ask";
import { save } from "@tauri-apps/plugin-dialog";
import {
  api,
  describeError,
  type ConnectionKind,
  type ConnectionProfile,
  type ExecutionRecord,
  type RecordFilter,
  type ReportFormat,
  type ReportOptions,
} from "../api";
import { openMenu } from "../contextMenu";
import { copyText } from "../copy";

interface Props {
  connections: ConnectionProfile[];
  onBack: () => void;
  /** Loads a record's SQL into the editor. */
  onOpenInEditor: (rec: ExecutionRecord) => void;
}

function ms(n: number) {
  return n < 1000 ? `${n.toFixed(1)} ms` : `${(n / 1000).toFixed(2)} s`;
}

function when(iso: string) {
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** Local `YYYY-MM-DD` to a UTC instant; `nextDay` makes an exclusive end. */
function dayToIso(day: string, nextDay = false) {
  if (!day) return null;
  const d = new Date(`${day}T00:00:00`);
  if (nextDay) d.setDate(d.getDate() + 1);
  return d.toISOString();
}

/** Returned rows for queries, affected rows for changes (a SELECT affects 0). */
function rowCount(r: ExecutionRecord) {
  const parts = [];
  if (r.affected_rows) parts.push(`影響 ${r.affected_rows.toLocaleString()}`);
  if (r.returned_rows) parts.push(`回傳 ${r.returned_rows.toLocaleString()}`);
  return parts.join("，") || "0";
}

function firstLine(sql: string) {
  const line = sql.split("\n").find((l) => l.trim()) ?? "";
  return line.length > 90 ? line.slice(0, 90) + "…" : line;
}

/** PDF is the HTML report, printed. */
type ExportFormat = ReportFormat | "pdf";

const FORMATS: { value: ExportFormat; label: string }[] = [
  { value: "markdown", label: "Markdown (.md)" },
  { value: "html", label: "HTML (.html)" },
  { value: "pdf", label: "PDF（列印／存成 PDF）" },
  { value: "csv", label: "CSV (.csv)" },
];

export function RecordsPage({ connections, onBack, onOpenInEditor }: Props) {
  const [connectionId, setConnectionId] = useState("");
  const [kind, setKind] = useState<ConnectionKind | "">("");
  const [text, setText] = useState("");
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [onlyErrors, setOnlyErrors] = useState(false);
  const [records, setRecords] = useState<ExecutionRecord[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [detail, setDetail] = useState<ExecutionRecord | null>(null);
  const [note, setNote] = useState("");
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);
  const [reportOpen, setReportOpen] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 1800);
    return () => clearTimeout(t);
  }, [toast]);

  const copySql = async (sql: string, what = "SQL") => {
    await copyText(sql);
    setToast(`已複製 ${what}`);
  };

  const filter: RecordFilter = {
    connection_id: connectionId || null,
    connection_kind: kind || null,
    text: text || null,
    from: dayToIso(from),
    to: dayToIso(to, true),
    only_errors: onlyErrors,
    limit: 500,
  };

  const load = useCallback(async () => {
    try {
      setRecords(await api.listRecords(filter));
    } catch (e) {
      setMessage({ ok: false, text: describeError(e) });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId, kind, text, from, to, onlyErrors]);

  useEffect(() => {
    const t = setTimeout(load, 200);
    return () => clearTimeout(t);
  }, [load]);

  const openDetail = async (id: number) => {
    const rec = await api.getRecord(id);
    setDetail(rec);
    setNote(rec?.note ?? "");
  };

  const toggle = (id: number) => {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setSelected(next);
  };

  const allSelected = records.length > 0 && records.every((r) => selected.has(r.id));
  const toggleAll = () => setSelected(allSelected ? new Set() : new Set(records.map((r) => r.id)));

  const saveNote = async () => {
    if (!detail) return;
    await api.setRecordNote(detail.id, note);
    setDetail({ ...detail, note });
    setRecords((rs) => rs.map((r) => (r.id === detail.id ? { ...r, note } : r)));
  };

  const remove = async () => {
    if (!(await ask(`刪除選取的 ${selected.size} 筆紀錄？刪除後無法復原。`, "刪除"))) return;
    await api.deleteRecords([...selected]);
    if (detail && selected.has(detail.id)) setDetail(null);
    setSelected(new Set());
    await load();
  };

  return (
    <div className="records">
      <header className="topbar">
        <button className="small ghost" onClick={onBack}>
          ← 返回
        </button>
        <strong>執行紀錄</strong>
        <span className="muted small-text">{records.length} 筆</span>
        <span className="spacer" />
        {selected.size > 0 && (
          <>
            <span className="small-text">已選 {selected.size} 筆</span>
            <button className="danger" onClick={remove}>
              刪除
            </button>
          </>
        )}
        <button className="primary" disabled={selected.size === 0} onClick={() => setReportOpen(true)}>
          產生報表
        </button>
      </header>

      <div className="filters">
        <select
          value={kind}
          onChange={(e) => (setKind(e.target.value as ConnectionKind | ""), setConnectionId(""))}
          title="連線類型"
        >
          <option value="">所有類型</option>
          <option value="mysql">MySQL / MariaDB</option>
          <option value="redis">Redis</option>
        </select>
        <select value={connectionId} onChange={(e) => setConnectionId(e.target.value)}>
          <option value="">所有連線</option>
          {connections.filter((c) => !kind || (c.kind ?? "mysql") === kind).map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        <input placeholder="搜尋 SQL／指令、備註、錯誤訊息" value={text} onChange={(e) => setText(e.target.value)} />
        <label className="inline">
          從 <input type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
        </label>
        <label className="inline">
          到 <input type="date" value={to} onChange={(e) => setTo(e.target.value)} />
        </label>
        <label className="inline check">
          <input type="checkbox" checked={onlyErrors} onChange={(e) => setOnlyErrors(e.target.checked)} />
          只看失敗
        </label>
      </div>

      {message && (
        <div className={`notice ${message.ok ? "info ok" : "error"}`} onClick={() => setMessage(null)}>
          {message.text}
        </div>
      )}

      <div className="records-body">
        <div className="records-list">
          {records.length === 0 ? (
            <div className="empty-state">
              <p>還沒有符合條件的紀錄。</p>
              <p className="small-text">在 SQL 編輯器上方打開「📝 紀錄」再執行，這次執行就會被記下來。</p>
            </div>
          ) : (
            <table className="grid records-table">
              <thead>
                <tr>
                  <th>
                    <input type="checkbox" checked={allSelected} onChange={toggleAll} />
                  </th>
                  <th>#</th>
                  <th>時間</th>
                  <th>連線 / 資料庫</th>
                  <th>SQL</th>
                  <th>耗時</th>
                  <th>筆數</th>
                  <th>狀態</th>
                </tr>
              </thead>
              <tbody>
                {records.map((r) => (
                  <tr
                    key={r.id}
                    className={detail?.id === r.id ? "current" : ""}
                    onClick={() => openDetail(r.id)}
                    onContextMenu={(e) =>
                      openMenu(e, [
                        { label: "複製 SQL", onClick: () => copySql(r.sql) },
                        {
                          label: "在編輯器中開啟",
                          onClick: async () => {
                            const rec = await api.getRecord(r.id);
                            if (rec) onOpenInEditor(rec);
                          },
                        },
                        { label: "查看詳細內容", onClick: () => openDetail(r.id) },
                        "sep",
                        { label: selected.has(r.id) ? "取消選取" : "選取", onClick: () => toggle(r.id) },
                        {
                          label: "用這筆產生報表…",
                          onClick: () => {
                            setSelected(new Set([r.id]));
                            setReportOpen(true);
                          },
                        },
                        ...(r.error_message
                          ? [{ label: "複製錯誤訊息", onClick: () => copySql(r.error_message ?? "", "錯誤訊息") }]
                          : []),
                      ])
                    }
                  >
                    <td onClick={(e) => e.stopPropagation()}>
                      <input type="checkbox" checked={selected.has(r.id)} onChange={() => toggle(r.id)} />
                    </td>
                    <td className="rownum">{r.id}</td>
                    <td>{when(r.started_at)}</td>
                    <td>
                      {r.connection_kind === "redis" && <span className="badge kind-redis">Redis</span>}{" "}
                      {r.connection_name}
                      {r.database ? ` / ${r.database}` : ""}
                    </td>
                    <td
                      className="sql-cell copyable"
                      title={`${r.sql}\n\n（點一下複製 SQL）`}
                      onClick={() => copySql(r.sql)}
                    >
                      {firstLine(r.sql)}
                      {r.statement_count > 1 && (
                        <span className="muted">
                          {" "}
                          （{r.statement_count} {r.connection_kind === "redis" ? "個指令" : "句"}）
                        </span>
                      )}
                      {r.note && <div className="note-line">📝 {r.note}</div>}
                    </td>
                    <td className="num">{ms(r.duration_ms)}</td>
                    <td className="num">{rowCount(r)}</td>
                    <td className={r.error_message ? "bad" : "ok"}>
                      {r.error_message ? `錯誤${r.error_code ? " " + r.error_code : ""}` : "成功"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        {detail && (
          <aside className="record-detail">
            <div className="detail-head">
              <strong>
                #{detail.id} · {when(detail.started_at)}
              </strong>
              <button className="small ghost" onClick={() => setDetail(null)}>
                ✕
              </button>
            </div>
            <div className="muted small-text">
              {detail.connection_name}
              {detail.database ? ` / ${detail.database}` : ""} · {detail.server} · {detail.server_version}
            </div>
            <div className="stat-row">
              <span>耗時 {ms(detail.duration_ms)}</span>
              <span>影響 {detail.affected_rows} 筆</span>
              <span>回傳 {detail.returned_rows} 筆</span>
            </div>
            <pre className="sql-block copyable" title="點一下複製 SQL" onClick={() => copySql(detail.sql)}>
              {detail.sql}
            </pre>
            <div className="row-actions">
              <button onClick={() => copySql(detail.sql)}>複製 SQL</button>
              <button onClick={() => onOpenInEditor(detail)}>在編輯器中開啟</button>
            </div>

            {detail.error_message && (
              <div className="error-box">
                <strong>
                  {detail.error_code ? `錯誤 ${detail.error_code}：` : ""}
                  {detail.error_message}
                </strong>
                {detail.failed_statement && (
                  <pre
                    className="copyable"
                    title="點一下複製這一句"
                    onClick={() => copySql(detail.failed_statement!, "出錯的陳述式")}
                  >
                    {detail.failed_statement}
                  </pre>
                )}
              </div>
            )}

            <h4>各陳述式</h4>
            <table className="grid">
              <thead>
                <tr>
                  <th>#</th>
                  <th>陳述式</th>
                  <th>耗時</th>
                  <th>影響</th>
                  <th>回傳</th>
                </tr>
              </thead>
              <tbody>
                {detail.statements.map((s, i) => (
                  <tr key={i}>
                    <td className="rownum">{i + 1}</td>
                    <td
                      className="sql-cell copyable"
                      title={`${s.sql}\n\n（點一下複製這一句）`}
                      onClick={() => copySql(s.sql, `第 ${i + 1} 句`)}
                    >
                      {firstLine(s.sql)}
                    </td>
                    <td className="num">{ms(s.duration_ms)}</td>
                    <td className="num">{s.affected_rows}</td>
                    <td className="num">
                      {detail.connection_kind === "redis"
                        ? [s.returned_rows, s.info].filter((v) => v != null && v !== "").join(" · ")
                        : (s.returned_rows ?? "—")}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>

            <h4>備註</h4>
            <textarea
              value={note}
              placeholder="例如：每月對帳、修正訂單狀態…（會出現在報表中）"
              onChange={(e) => setNote(e.target.value)}
              onBlur={saveNote}
            />
          </aside>
        )}
      </div>

      {toast && <div className="toast">{toast}</div>}

      {reportOpen && (
        <ReportDialog
          ids={[...selected]}
          onClose={() => setReportOpen(false)}
          onSaved={(path) => {
            setReportOpen(false);
            setMessage({ ok: true, text: `報表已儲存到 ${path}` });
          }}
          onCopied={() => setToast("已複製報表內容")}
        />
      )}
    </div>
  );
}

function ReportDialog({
  ids,
  onClose,
  onSaved,
  onCopied,
}: {
  ids: number[];
  onClose: () => void;
  onSaved: (path: string) => void;
  onCopied: () => void;
}) {
  const [format, setFormat] = useState<ExportFormat>("markdown");
  const [hideConnection, setHideConnection] = useState(true);
  const [title, setTitle] = useState("SQL 執行報表");
  const [includeResults, setIncludeResults] = useState(true);
  const [preview, setPreview] = useState("");
  const [error, setError] = useState<string | null>(null);

  const options: ReportOptions = {
    format: format === "pdf" ? "html" : format,
    title,
    include_results: includeResults,
    max_result_rows: 20,
    hide_connection: hideConnection,
  };

  useEffect(() => {
    api
      .generateReport(ids, options)
      .then(setPreview)
      .catch((e) => setError(describeError(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [format, title, includeResults, hideConnection]);

  const saveAs = async () => {
    if (format === "pdf") {
      try {
        await api.printReport(preview);
      } catch (e) {
        setError(describeError(e));
      }
      return;
    }
    const day = new Date().toISOString().slice(0, 10);
    const path = await save({
      defaultPath: `sql-report-${day}.${format === "markdown" ? "md" : format}`,
      filters: [{ name: format.toUpperCase(), extensions: [format === "markdown" ? "md" : format] }],
    });
    if (!path) return;
    try {
      await api.saveTextFile(path, preview);
      onSaved(path);
    } catch (e) {
      setError(describeError(e));
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="report-dialog">
        <h2>產生報表（{ids.length} 筆紀錄）</h2>
        <div className="fields">
          <div className="row">
            <label className="grow">
              標題
              <input value={title} onChange={(e) => setTitle(e.target.value)} disabled={format === "csv"} />
            </label>
            <label>
              格式
              <select value={format} onChange={(e) => setFormat(e.target.value as ExportFormat)}>
                {FORMATS.map((f) => (
                  <option key={f.value} value={f.value}>
                    {f.label}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {format !== "csv" && (
            <label className="check">
              <input type="checkbox" checked={includeResults} onChange={(e) => setIncludeResults(e.target.checked)} />
              包含查詢結果（每個結果最多 20 筆）
            </label>
          )}
          <label className="check">
            <input type="checkbox" checked={hideConnection} onChange={(e) => setHideConnection(e.target.checked)} />
            隱藏連線資訊（主機、帳號、IP 改成 ***，連線只顯示名稱）
          </label>
        </div>
        <div className="report-preview">
          {format === "html" || format === "pdf" ? (
            <iframe title="預覽" sandbox="" srcDoc={preview} />
          ) : (
            <pre>{preview}</pre>
          )}
        </div>
        {error && <div className="status bad">{error}</div>}
        <div className="actions">
          <button
            onClick={async () => {
              await copyText(preview);
              onCopied();
            }}
          >
            複製內容
          </button>
          <span className="spacer" />
          <button onClick={onClose}>取消</button>
          <button className="primary" onClick={saveAs}>
            {format === "pdf" ? "列印／存成 PDF…" : "另存新檔…"}
          </button>
        </div>
      </div>
    </div>
  );
}
