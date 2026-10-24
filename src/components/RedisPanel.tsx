import { useCallback, useEffect, useState } from "react";
import {
  api,
  describeError,
  type Cell,
  type KeyInfo,
  type KeyName,
  type RedisOutcome,
  type Reply,
  type StatementResult,
  type ValuePage,
} from "../api";
import { copyText } from "../copy";
import { ttlText } from "../redis/keyTree";
import { ResultGrid } from "./ResultGrid";
import { TYPE_LABEL } from "./RedisKeyTree";

function ms(n: number) {
  return n < 1000 ? `${n.toFixed(1)} ms` : `${(n / 1000).toFixed(2)} s`;
}

function bytes(n: number) {
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i > 0 && v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/** Rows as the result grid takes them. */
function grid(columns: string[], rows: Cell[][], sql = ""): StatementResult {
  return {
    sql,
    duration_ms: 0,
    columns: columns.map((name) => ({ name, table: "", type_name: "" })),
    rows,
    total_rows: rows.length,
    truncated: false,
    affected_rows: 0,
    warnings: 0,
    info: "",
  };
}

/** One-line text of a reply, as redis-cli prints scalars. */
export function replyText(r: Reply): string {
  switch (r.type) {
    case "nil":
      return "(nil)";
    case "integer":
    case "double":
      return String(r.value);
    case "bool":
      return r.value ? "true" : "false";
    case "text":
      return r.binary ? `0x${r.value}` : r.value;
    case "status":
      return r.value;
    case "error":
      return `(error) ${r.message}`;
    case "array":
      return `[${r.items.map(replyText).join(", ")}]`;
    case "map":
      return `{${r.entries.map(([k, v]) => `${replyText(k)}: ${replyText(v)}`).join(", ")}}`;
  }
}

/** `text` pretty-printed when it is a JSON object or array. */
export function prettyJson(text: string): string | null {
  const t = text.trim();
  if (!(t.startsWith("{") || t.startsWith("["))) return null;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    return null;
  }
}

function TextValue({ text, binary }: { text: string; binary?: boolean }) {
  const pretty = binary ? null : prettyJson(text);
  const [raw, setRaw] = useState(false);
  return (
    <div className="redis-text">
      <div className="redis-text-bar">
        {pretty && <span className="badge">JSON</span>}
        {binary && <span className="badge">二進位（以十六進位顯示）</span>}
        {pretty && (
          <button className="link small-text" onClick={() => setRaw(!raw)}>
            {raw ? "格式化顯示" : "顯示原始內容"}
          </button>
        )}
        <span className="spacer" />
        <button className="small" onClick={() => copyText(text)}>
          複製
        </button>
      </div>
      <pre>{pretty && !raw ? pretty : binary ? `0x${text}` : text}</pre>
    </div>
  );
}

function ReplyView({ reply, command }: { reply: Reply; command: string }) {
  switch (reply.type) {
    case "nil":
      return <div className="reply muted">(nil)</div>;
    case "integer":
      return <div className="reply">(integer) {reply.value}</div>;
    case "double":
      return <div className="reply">(double) {reply.value}</div>;
    case "bool":
      return <div className="reply">{reply.value ? "true" : "false"}</div>;
    case "status":
      return <div className="reply">{reply.value}</div>;
    case "error":
      return <div className="reply bad">(error) {reply.message}</div>;
    case "text":
      return <TextValue text={reply.value} binary={reply.binary} />;
    case "array":
      if (reply.items.length === 0) return <div className="reply muted">(empty array)</div>;
      return (
        <ResultGrid
          result={grid(
            ["#", "value"],
            reply.items.map((it, i) => [i + 1, replyText(it)]),
            command,
          )}
        />
      );
    case "map":
      if (reply.entries.length === 0) return <div className="reply muted">(empty map)</div>;
      return (
        <ResultGrid
          result={grid(
            ["key", "value"],
            reply.entries.map(([k, v]) => [replyText(k), replyText(v)]),
            command,
          )}
        />
      );
  }
}

/** Results of a run of the command editor. */
function CommandResults({ outcome }: { outcome: RedisOutcome | null }) {
  const [shown, setShown] = useState(0);
  useEffect(() => {
    if (outcome) setShown(Math.max(0, outcome.results.length - 1));
  }, [outcome]);

  if (!outcome) {
    return (
      <div className="results empty">
        <p>每行一個指令，例如 <code>GET user:1</code>、<code>HGETALL user:1:profile</code>。</p>
        <p>⌘／Ctrl + Enter：執行游標所在的那一行；有選取時依序執行選取的多行。</p>
        <p>⌘／Ctrl + Shift + Enter：依序執行全部。以 # 開頭的行是註解。</p>
        <p>左側點 key 可以檢視內容（唯讀）。</p>
      </div>
    );
  }
  const current = outcome.results[shown];
  const hasErrorReply = outcome.results.some((r) => r.reply.type === "error");
  return (
    <div className="results">
      <div className="results-bar">
        <span>
          完成 {outcome.results.length} 個指令，共 {ms(outcome.duration_ms)} · db{outcome.db}
        </span>
        {outcome.record_id != null && <span className="recorded">📝 已記錄 #{outcome.record_id}</span>}
        <div className="result-tabs">
          {outcome.results.map((r, i) => (
            <button
              key={i}
              className={[i === shown ? "active" : "", r.reply.type === "error" ? "bad" : ""].join(" ")}
              title={r.command}
              onClick={() => setShown(i)}
            >
              <span className="tab-no">#{i + 1}</span> {r.command.length > 28 ? r.command.slice(0, 27) + "…" : r.command}
            </button>
          ))}
        </div>
      </div>
      {outcome.error && outcome.cancelled && (
        <div className="cancelled-box">
          ⏹ 已停止
          {outcome.failed_command ? "：停在下面這個指令。" : "。"}
          {outcome.failed_command && <pre>{outcome.failed_command}</pre>}
        </div>
      )}
      {outcome.error && !outcome.cancelled && !hasErrorReply && (
        <div className="error-box">
          <strong>{describeError(outcome.error)}</strong>
          {outcome.failed_command && <pre>{outcome.failed_command}</pre>}
        </div>
      )}
      {current && (
        <>
          <div className="statement-meta">
            <code className="redis-command">&gt; {current.command}</code>
            <span>第 {current.line + 1} 行</span>
            <span>{ms(current.duration_ms)}</span>
            {(current.reply.type === "array" || current.reply.type === "map") && (
              <span>
                {current.reply.type === "array" ? current.reply.items.length : current.reply.entries.length} 筆
              </span>
            )}
            {current.reply.type !== "text" && current.reply.type !== "error" && (
              <button className="link small-text" onClick={() => copyText(replyText(current.reply))}>
                複製結果
              </button>
            )}
          </div>
          <ReplyView key={`${outcome.started_at}:${shown}`} reply={current.reply} command={current.command} />
        </>
      )}
    </div>
  );
}

/** A key's value, read-only, a page at a time for collections. */
function ValueView({ sessionId, db, keyName }: { sessionId: string; db: number; keyName: KeyName }) {
  const [info, setInfo] = useState<KeyInfo | null>(null);
  const [page, setPage] = useState<ValuePage | null>(null);
  const [rows, setRows] = useState<Cell[][]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [i, p] = await Promise.all([
        api.redisKeyInfo(sessionId, db, keyName),
        api.redisReadValue(sessionId, db, keyName, ""),
      ]);
      setInfo(i);
      setPage(p);
      setRows(p.rows);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setLoading(false);
    }
  }, [sessionId, db, keyName]);

  useEffect(() => {
    load();
  }, [load]);

  const more = async () => {
    if (!page?.next) return;
    setLoading(true);
    try {
      const p = await api.redisReadValue(sessionId, db, keyName, page.next);
      setPage(p);
      setRows((r) => [...r, ...p.rows]);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setLoading(false);
    }
  };

  const shownName = keyName.binary ? `0x${keyName.key}` : keyName.key;
  return (
    <div className="results redis-value">
      <div className="results-bar redis-key-head">
        <code className="redis-key-name" title={shownName}>
          {shownName}
        </code>
        <button className="small" onClick={() => copyText(keyName.key)} title="複製 key 名稱">
          複製名稱
        </button>
        <button className="small ghost" onClick={load} title="重新讀取">
          ⟳
        </button>
      </div>
      {error && <div className="error-box"><strong>{error}</strong></div>}
      {info && (
        <div className="statement-meta">
          <span className={`key-type t-${info.kind}`}>{TYPE_LABEL[info.kind] ?? info.kind}</span>
          <span>TTL：{ttlText(info.ttl)}</span>
          {info.length != null && <span>{info.kind === "string" ? `長度 ${info.length.toLocaleString()} bytes` : `${info.length.toLocaleString()} 筆`}</span>}
          {info.encoding && <span>編碼：{info.encoding}</span>}
          {info.memory_bytes != null && <span>記憶體：{bytes(info.memory_bytes)}</span>}
          <span className="muted">唯讀</span>
        </div>
      )}
      {info?.kind === "none" && <div className="hint pad">這個 key 已不存在（可能已過期或被刪除）。</div>}
      {page?.text != null && (
        <>
          {page.truncated && <div className="limit-note">內容太大，只顯示前 1 MB。</div>}
          <TextValue key={shownName} text={page.text} binary={page.binary} />
        </>
      )}
      {page && page.columns.length > 0 && (
        <>
          <div className="statement-meta">
            <span>
              已載入 {rows.length.toLocaleString()}
              {info?.length != null ? ` / ${info.length.toLocaleString()}` : ""} 筆
            </span>
            {page.next && (
              <button className="link small-text" disabled={loading} onClick={more}>
                {loading ? "載入中…" : "載入更多"}
              </button>
            )}
          </div>
          <ResultGrid key={`${shownName}:${rows.length > 0}`} result={grid(page.columns, rows)} />
        </>
      )}
      {loading && !page && <div className="hint pad">載入中…</div>}
    </div>
  );
}

/** The lower half of a Redis tab: command results, or the key being viewed. */
export function RedisPanel({
  sessionId,
  outcome,
  openKey,
  view,
  onViewChange,
}: {
  sessionId: string;
  outcome: RedisOutcome | null;
  openKey: { db: number; key: KeyName } | null;
  view: "result" | "key";
  onViewChange: (v: "result" | "key") => void;
}) {
  return (
    <div className="redis-panel">
      {openKey && (
        <div className="tabs redis-panel-tabs">
          <button className={view === "result" ? "active" : ""} onClick={() => onViewChange("result")}>
            指令結果
          </button>
          <button className={view === "key" ? "active" : ""} onClick={() => onViewChange("key")}>
            Key：db{openKey.db} / {openKey.key.binary ? `0x${openKey.key.key}` : openKey.key.key}
          </button>
        </div>
      )}
      {view === "key" && openKey ? (
        <ValueView
          key={`${openKey.db}:${openKey.key.binary ? 1 : 0}:${openKey.key.key}`}
          sessionId={sessionId}
          db={openKey.db}
          keyName={openKey.key}
        />
      ) : (
        <CommandResults outcome={outcome} />
      )}
    </div>
  );
}
