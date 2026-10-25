import { useEffect, useRef, useState } from "react";
import { ask } from "../ask";
import {
  api,
  describeError,
  type AiChat,
  type AiChatSummary,
  type AiContext,
  type AppSettings,
  type ChatMessage,
} from "../api";
import { copyText } from "../copy";

/** What the drawer was opened about. */
export interface AiRequest {
  sql: string;
  error?: string | null;
  explain?: string | null;
  /** Sent right away when set (e.g. "怎麼修" from an error). */
  prompt?: string;
  /** Changes on every open, so the same SQL can start a new conversation. */
  nonce: number;
}

interface Props {
  request: AiRequest | null;
  sessionId: string | null;
  connectionId: string | null;
  connectionName: string;
  settings: AppSettings;
  onClose: () => void;
  onOpenSettings: () => void;
  onNewTab: (sql: string) => void;
}

type Status = "idle" | "gathering" | "starting" | "thinking" | "streaming";

/** Context parts the user can leave out, by key. */
function parts(ctx: AiContext): { key: string; label: string; detail: string }[] {
  const out = [];
  if (ctx.sql.trim()) out.push({ key: "sql", label: "SQL", detail: ctx.sql });
  if (ctx.error) out.push({ key: "error", label: "錯誤訊息", detail: ctx.error });
  if (ctx.explain) out.push({ key: "explain", label: "EXPLAIN 執行計畫", detail: ctx.explain });
  for (const t of ctx.tables) out.push({ key: `table:${t.name}`, label: `資料表結構 ${t.name}`, detail: t.ddl });
  if (ctx.other_tables.length)
    out.push({
      key: "other_tables",
      label: `${ctx.database ?? ""} 的資料表清單（${ctx.other_tables.length}）`,
      detail: ctx.other_tables.join(", "),
    });
  if (ctx.findings.length) out.push({ key: "findings", label: "規則檢查結果", detail: ctx.findings.join("\n") });
  return out;
}

function without(ctx: AiContext, excluded: Set<string>): AiContext {
  return {
    ...ctx,
    sql: excluded.has("sql") ? "" : ctx.sql,
    error: excluded.has("error") ? null : ctx.error,
    explain: excluded.has("explain") ? null : ctx.explain,
    tables: ctx.tables.filter((t) => !excluded.has(`table:${t.name}`)),
    other_tables: excluded.has("other_tables") ? [] : ctx.other_tables,
    findings: excluded.has("findings") ? [] : ctx.findings,
  };
}

/** SQLite's UTC `YYYY-MM-DD HH:MM:SS` in local time. */
function localTime(utc: string) {
  const d = new Date(utc.replace(" ", "T") + "Z");
  if (isNaN(d.getTime())) return utc;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function titleFor(text: string) {
  const s = text.replace(/\s+/g, " ").trim();
  return s.length > 40 ? s.slice(0, 39) + "…" : s;
}

/** An answer with its ```sql blocks made copyable and openable in a tab. */
function Answer({ text, onNewTab }: { text: string; onNewTab: (sql: string) => void }) {
  const pieces = text.split(/```([a-zA-Z]*)\n?([\s\S]*?)(?:```|$)/g);
  const out = [];
  for (let i = 0; i < pieces.length; i += 3) {
    const prose = pieces[i].replace(/^\n+|\n+$/g, "");
    if (prose) out.push(<Prose key={i} text={prose} />);
    const code = pieces[i + 2];
    if (code != null && code.trim()) {
      const lang = (pieces[i + 1] || "").toLowerCase();
      const isSql = lang === "sql" || lang === "mysql" || (!lang && /\b(SELECT|UPDATE|INSERT|DELETE|ALTER|CREATE)\b/i.test(code));
      out.push(
        <div key={`c${i}`} className="ai-code">
          <pre>{code.trimEnd()}</pre>
          <div className="ai-code-actions">
            <button className="small" onClick={() => copyText(code.trim())}>
              複製
            </button>
            {isSql && (
              <button className="small" onClick={() => onNewTab(code.trim())}>
                開新分頁
              </button>
            )}
          </div>
        </div>,
      );
    }
  }
  return <>{out}</>;
}

/** Plain text with **bold**, `code` and # headings. */
function Prose({ text }: { text: string }) {
  return (
    <div className="ai-prose">
      {text.split("\n").map((line, i) => {
        const heading = /^#{1,4}\s+/.test(line);
        const body = line.replace(/^#{1,4}\s+/, "");
        const spans = body.split(/(\*\*[^*]+\*\*|`[^`]+`)/g).map((p, j) =>
          p.startsWith("**") && p.endsWith("**") && p.length > 4 ? (
            <strong key={j}>{p.slice(2, -2)}</strong>
          ) : p.startsWith("`") && p.endsWith("`") && p.length > 2 ? (
            <code key={j}>{p.slice(1, -1)}</code>
          ) : (
            p
          ),
        );
        return heading ? (
          <div key={i} className="ai-heading">
            {spans}
          </div>
        ) : (
          <div key={i}>{line.trim() ? spans : " "}</div>
        );
      })}
    </div>
  );
}

const STATUS_TEXT: Record<Status, string> = {
  idle: "",
  gathering: "正在整理 SQL 與資料表結構…",
  starting: "正在啟動本機模型…（閒置後第一次約需 10–30 秒）",
  thinking: "模型思考中…",
  streaming: "",
};

/**
 * The AI side panel: a conversation about the SQL it was opened with. The
 * SQL, error, EXPLAIN and table structure go along automatically (the user
 * can leave any of them out); conversations are kept in the local store.
 */
export function AiDrawer(props: Props) {
  const { request, sessionId, connectionId, connectionName, settings } = props;
  const [view, setView] = useState<"chat" | "history">("chat");
  const [chat, setChat] = useState<AiChat | null>(null);
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [input, setInput] = useState("");
  const [status, setStatus] = useState<Status>("idle");
  const [streamed, setStreamed] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [history, setHistory] = useState<AiChatSummary[]>([]);
  const [setup, setSetup] = useState<string | null>(null);
  const [showContext, setShowContext] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const busy = status !== "idle";

  // Whether a model is set up at all.
  useEffect(() => {
    if (!settings.ai_enabled) {
      setSetup("AI 建議還沒開啟。");
      return;
    }
    if (settings.ai_use_external) {
      setSetup(null);
      return;
    }
    api
      .aiStatus()
      .then((s) =>
        setSetup(
          !s.llama_server
            ? "這台電腦還沒裝 llama.cpp。"
            : !s.model
              ? "還沒選擇模型檔。"
              : null,
        ),
      )
      .catch(() => setSetup(null));
  }, [settings.ai_enabled, settings.ai_use_external, settings.ai_model_path, settings.ai_llama_server]);

  const send = async (text: string, base: AiChat | null = chat) => {
    const question = text.trim();
    if (!question || !base || busy) return;
    setError(null);
    setInput("");
    const context = base.id === 0 ? without(base.context, excluded) : base.context;
    const messages: ChatMessage[] = [...base.messages, { role: "user", content: question }];
    let next: AiChat = { ...base, context, messages, title: base.title || titleFor(question) };
    setChat(next);
    setStreamed("");
    setStatus("thinking");
    let answer = "";
    try {
      answer = await api.aiChat(context, messages, (e) => {
        if (e.kind === "starting") setStatus("starting");
        else if (e.kind === "thinking") setStatus("thinking");
        else {
          setStatus("streaming");
          setStreamed((s) => s + e.text);
        }
      });
    } catch (e) {
      setError(describeError(e));
    }
    if (answer) next = { ...next, messages: [...messages, { role: "assistant", content: answer }] };
    setStreamed("");
    setStatus("idle");
    try {
      next = { ...next, id: await api.saveAiChat(next) };
    } catch {
      // Keeping the history is best effort.
    }
    setChat(next);
    inputRef.current?.focus();
  };

  // A new request starts a new conversation.
  useEffect(() => {
    if (!request) return;
    let cancelled = false;
    setView("chat");
    setError(null);
    setExcluded(new Set());
    setChat(null);
    setStatus("gathering");
    api
      .aiContext(sessionId, request.sql, request.error ?? null, request.explain ?? null)
      .catch(
        (): AiContext => ({
          server: "",
          database: null,
          sql: request.sql,
          error: request.error ?? null,
          explain: request.explain ?? null,
          tables: [],
          other_tables: [],
          findings: [],
        }),
      )
      .then((context) => {
        if (cancelled) return;
        const fresh: AiChat = {
          id: 0,
          title: "",
          connection_id: connectionId,
          connection_name: connectionName,
          context,
          messages: [],
          updated_at: "",
        };
        setChat(fresh);
        setStatus("idle");
        if (request.prompt) send(request.prompt, fresh);
        else inputRef.current?.focus();
      });
    return () => {
      cancelled = true;
    };
    // Only a new request (nonce) restarts the conversation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [request?.nonce]);

  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight });
  }, [chat?.messages.length, streamed, status]);

  const openHistory = async () => {
    setView("history");
    setHistory(await api.listAiChats().catch(() => []));
  };

  const openChat = async (id: number) => {
    const loaded = await api.getAiChat(id).catch(() => null);
    if (!loaded) return;
    setChat(loaded);
    setExcluded(new Set());
    setView("chat");
  };

  const ctxParts = chat ? parts(chat.context) : [];
  const fresh = chat?.id === 0 && chat.messages.length === 0;
  const quick = chat
    ? [
        ...(chat.context.error ? ["這個錯誤怎麼修？"] : []),
        "幫我檢查這段 SQL 有沒有問題",
        "怎麼讓它跑得更快？",
        "解釋這段 SQL 在做什麼",
      ]
    : [];

  return (
    <aside className="ai-drawer" onKeyDown={(e) => e.key === "Escape" && !busy && props.onClose()}>
      <header className="ai-head">
        <strong>✨ AI 分析</strong>
        <span className="muted">本機模型，資料不會離開這台電腦</span>
        <span className="spacer" />
        {view === "chat" ? (
          <button className="small ghost" onClick={openHistory} title="之前的 AI 對話">
            歷史
          </button>
        ) : (
          <button className="small ghost" onClick={() => setView("chat")}>
            ← 回到對話
          </button>
        )}
        <button className="small ghost" onClick={props.onClose} title="關閉（Esc）">
          ✕
        </button>
      </header>

      {view === "history" ? (
        <div className="ai-body">
          {history.length === 0 && <p className="muted">還沒有 AI 對話。</p>}
          <ul className="ai-history">
            {history.map((h) => (
              <li key={h.id} onClick={() => openChat(h.id)}>
                <div className="ai-history-title">{h.title || "（未命名）"}</div>
                <div className="muted">
                  {h.connection_name} · {localTime(h.updated_at)} · {h.message_count} 則
                </div>
                <button
                  className="small ghost"
                  title="刪除這段對話"
                  onClick={async (e) => {
                    e.stopPropagation();
                    if (!(await ask("刪除這段 AI 對話？", "刪除"))) return;
                    await api.deleteAiChat(h.id).catch(() => {});
                    setHistory((l) => l.filter((x) => x.id !== h.id));
                    if (chat?.id === h.id) setChat(null);
                  }}
                >
                  刪除
                </button>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <>
          <div className="ai-body" ref={scroller}>
            {setup && (
              <div className="ai-setup">
                <div>
                  {setup}設定好本機模型後就能使用 AI 分析。不用的時候模型不會啟動，也不會占記憶體。
                </div>
                <button className="small primary" onClick={props.onOpenSettings}>
                  開啟 AI 設定
                </button>
              </div>
            )}
            {!chat && status === "idle" && (
              <p className="muted">在編輯器選取 SQL 後按右鍵「✨ AI 分析」（⌘I），或從「歷史」打開之前的對話。</p>
            )}
            {chat && ctxParts.length > 0 && (
              <div className="ai-context">
                <button className="link" onClick={() => setShowContext((v) => !v)}>
                  {showContext ? "▾" : "▸"} 會一起附上：
                  {ctxParts.filter((p) => !excluded.has(p.key)).map((p) => p.label.split(" ")[0]).join("、") || "（無）"}
                </button>
                {showContext && (
                  <ul>
                    {ctxParts.map((p) => (
                      <li key={p.key}>
                        <label>
                          <input
                            type="checkbox"
                            disabled={!fresh}
                            checked={!excluded.has(p.key)}
                            onChange={(e) => {
                              const next = new Set(excluded);
                              if (e.target.checked) next.delete(p.key);
                              else next.add(p.key);
                              setExcluded(next);
                            }}
                          />
                          <span>{p.label}</span>
                        </label>
                        <pre>{p.detail.length > 1500 ? p.detail.slice(0, 1500) + "…" : p.detail}</pre>
                      </li>
                    ))}
                    <li className="muted">只會附上 SQL、錯誤、執行計畫與資料表結構，不會附上任何資料列。</li>
                  </ul>
                )}
              </div>
            )}
            {chat?.messages.map((m, i) => (
              <div key={i} className={`ai-msg ${m.role}`}>
                {m.role === "assistant" ? <Answer text={m.content} onNewTab={props.onNewTab} /> : m.content}
              </div>
            ))}
            {streamed && (
              <div className="ai-msg assistant">
                <Answer text={streamed} onNewTab={props.onNewTab} />
              </div>
            )}
            {STATUS_TEXT[status] && <div className="ai-status">{STATUS_TEXT[status]}</div>}
            {error && (
              <div className="ai-error">
                {error}
                {/llama|模型|AI 建議/.test(error) && (
                  <button className="link" onClick={props.onOpenSettings}>
                    開啟 AI 設定
                  </button>
                )}
              </div>
            )}
            {chat && fresh && !busy && (
              <div className="ai-quick">
                {quick.map((q) => (
                  <button key={q} className="small" disabled={!!setup} onClick={() => send(q)}>
                    {q}
                  </button>
                ))}
              </div>
            )}
          </div>
          <footer className="ai-input">
            <textarea
              ref={inputRef}
              value={input}
              rows={3}
              disabled={!chat || !!setup}
              placeholder={chat ? "輸入你想問的…（Enter 送出，Shift + Enter 換行）" : ""}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  send(input);
                }
              }}
            />
            {busy && status !== "gathering" ? (
              <button onClick={() => api.aiCancel()}>停止</button>
            ) : (
              <button className="primary" disabled={!chat || !input.trim() || !!setup} onClick={() => send(input)}>
                送出
              </button>
            )}
          </footer>
        </>
      )}
    </aside>
  );
}
