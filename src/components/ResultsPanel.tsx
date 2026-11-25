import { useEffect, useRef, useState } from "react";
import { api, describeError, quickReportOptions, type ExecutionOutcome, type StatementResult } from "../api";
import { openMenu } from "../contextMenu";
import { copyText, formatBlock } from "../copy";
import { isExplain, readPlan } from "../explain";
import { ResultGrid } from "./ResultGrid";

/** A statement on one line, for the statement list (CSS cuts it to fit). */
function oneLine(sql: string) {
  return sql.replace(/\s+/g, " ").trim();
}

function ms(n: number) {
  return n < 1000 ? `${n.toFixed(1)} ms` : `${(n / 1000).toFixed(2)} s`;
}

function summary(r: StatementResult) {
  if (r.columns.length > 0) {
    const shown = r.truncated ? `（顯示前 ${r.rows.length.toLocaleString()} 筆）` : "";
    return `回傳 ${r.total_rows.toLocaleString()} 筆${shown}`;
  }
  return `影響 ${r.affected_rows.toLocaleString()} 筆`;
}

/** Short status for the statement list. */
function brief(r: StatementResult) {
  if (r.columns.length > 0) return `${r.total_rows.toLocaleString()} 筆`;
  return r.affected_rows > 0 ? `影響 ${r.affected_rows.toLocaleString()}` : "OK";
}

const MD_ROWS = 20;

/** A run as a Markdown report, for runs that were not recorded. */
export function outcomeMarkdown(outcome: ExecutionOutcome) {
  const out = [
    "# SQL 執行報表",
    "",
    `- 時間：${new Date(outcome.started_at).toLocaleString()}`,
    `- 陳述式：${outcome.results.length} 個，共 ${ms(outcome.duration_ms)}`,
    "",
  ];
  outcome.results.forEach((r, i) => {
    out.push(`## #${i + 1}`, "", "```sql", r.sql.trim(), "```", "", `${summary(r)} · ${ms(r.duration_ms)}`, "");
    if (r.columns.length > 0) {
      out.push(formatBlock({ columns: r.columns, rows: r.rows.slice(0, MD_ROWS) }, "markdown", true), "");
      if (r.rows.length > MD_ROWS) out.push(`（只列出前 ${MD_ROWS} 筆）`, "");
    }
  });
  if (outcome.error) {
    out.push(`## ${outcome.cancelled ? "已停止" : "錯誤"}`, "");
    if (outcome.failed_statement) out.push("```sql", outcome.failed_statement.trim(), "```", "");
    if (!outcome.cancelled) out.push(describeError(outcome.error), "");
  }
  return out.join("\n");
}

/** A result set as a `|`-separated text table, for the AI. */
function textTable(r: StatementResult) {
  const cell = (v: unknown) => (v == null ? "NULL" : typeof v === "string" ? v : JSON.stringify(v));
  return [r.columns.map((c) => c.name).join(" | "), ...r.rows.map((row) => row.map(cell).join(" | "))].join("\n");
}

export interface AskAi {
  sql: string;
  error?: string | null;
  explain?: string | null;
  prompt?: string;
}

export function ResultsPanel({
  outcome,
  onRunWithoutLimit,
  onAskAi,
  sessionId,
}: {
  outcome: ExecutionOutcome | null;
  /** Lets the grid export every row by running a query again. */
  sessionId?: string;
  /** Runs one statement again without the automatic LIMIT. */
  onRunWithoutLimit?: (sql: string) => void;
  /** Opens the AI drawer about part of this outcome. */
  onAskAi?: (ask: AskAi) => void;
}) {
  const [tab, setTab] = useState(0);
  const [listOpen, setListOpen] = useState(true);
  const [copied, setCopied] = useState<string | null>(null);
  const list = useRef<HTMLDivElement>(null);

  // Keep the chosen statement in view in the list.
  useEffect(() => {
    list.current?.querySelector(".statement-row.active")?.scrollIntoView({ block: "nearest" });
  }, [tab, outcome, listOpen]);

  const copyMarkdown = async () => {
    if (!outcome) return;
    try {
      const md =
        outcome.record_id != null
          ? await api.generateReport([outcome.record_id], quickReportOptions("markdown"))
          : outcomeMarkdown(outcome);
      await copyText(md);
      setCopied("已複製 Markdown 報表");
    } catch (e) {
      setCopied(describeError(e));
    }
    setTimeout(() => setCopied(null), 2500);
  };

  // Jump to the last result set (usually the one the user cares about).
  useEffect(() => {
    if (outcome) setTab(Math.max(0, outcome.results.length - 1));
  }, [outcome]);

  if (!outcome) {
    return (
      <div className="results empty">
        <p>⌘／Ctrl + Enter：執行游標所在的那一句（淡色底的那段）；有選取就執行選取的部分。</p>
        <p>⌘／Ctrl + Shift + Enter：依序執行全部（多句用 ; 分開）。</p>
        <p>⌘／Ctrl + E：Explain 選取的部分或游標所在的那一句。</p>
        <p>⌘／Ctrl + I：用 AI 分析選取的部分或游標所在的那一句。</p>
      </div>
    );
  }

  const current = outcome.results[tab];
  // Danger was already confirmed before running; show what is still useful.
  const notes = (outcome.findings ?? []).filter((f) => f.level !== "danger");
  const plan = current && isExplain(current) ? readPlan(current) : null;
  const multi = outcome.results.length > 1 || (outcome.results.length > 0 && !!outcome.failed_statement);
  return (
    <div className="results sql-results">
      <div className="results-bar">
        <span>
          完成 {outcome.results.length} 個陳述式，共 {ms(outcome.duration_ms)}
        </span>
        {outcome.record_id != null && <span className="recorded">📝 已記錄 #{outcome.record_id}</span>}
        {multi && (
          <button className="link" onClick={() => setListOpen((o) => !o)}>
            {listOpen ? "收合清單" : "展開清單"}
          </button>
        )}
        <span className="spacer" />
        {copied && <span className="copied-note">{copied}</span>}
        <button
          className="small"
          title={
            outcome.record_id != null
              ? "用預設選項（隱藏連線資訊、含查詢結果）產生這次執行的報表並複製 Markdown"
              : "把這次執行的語句與結果（每個結果前 20 筆）複製成 Markdown"
          }
          onClick={copyMarkdown}
        >
          複製 MD
        </button>
      </div>
      {multi && listOpen && (
        <div className="statement-list" ref={list} role="listbox" aria-label="陳述式">
          {outcome.results.map((r, i) => (
            <div
              key={i}
              role="option"
              aria-selected={i === tab}
              className={`statement-row${i === tab ? " active" : ""}`}
              title={`${r.sql}\n\n右鍵：複製 SQL`}
              onClick={() => setTab(i)}
              onContextMenu={(e) => openMenu(e, [{ label: "複製 SQL", onClick: () => copyText(r.sql) }])}
            >
              <span className="stmt-no">#{i + 1}</span>
              <span className="stmt-status ok">✓</span>
              <span className="stmt-sql">{oneLine(r.sql)}</span>
              <span className={`stmt-rows${r.warnings > 0 ? " warn" : ""}`}>
                {brief(r)}
                {r.warnings > 0 ? ` · ${r.warnings} 警告` : ""}
              </span>
              <span className="stmt-time">{ms(r.duration_ms)}</span>
            </div>
          ))}
          {outcome.error && outcome.failed_statement && (
            <div className="statement-row failed" title={outcome.failed_statement}>
              <span className="stmt-no">#{outcome.results.length + 1}</span>
              <span className={`stmt-status ${outcome.cancelled ? "warn" : "bad"}`}>{outcome.cancelled ? "⏹" : "✗"}</span>
              <span className="stmt-sql">{oneLine(outcome.failed_statement)}</span>
              <span className="stmt-rows">{outcome.cancelled ? "已停止" : "失敗"}</span>
              <span className="stmt-time" />
            </div>
          )}
        </div>
      )}

      {outcome.error && outcome.cancelled && (
        <div className="cancelled-box">
          ⏹ 已停止
          {outcome.failed_statement
            ? `：停在下面這句，${outcome.results.length ? `之前的 ${outcome.results.length} 句已完成。` : "前面沒有已完成的語句。"}`
            : "。"}
          {outcome.failed_statement && <pre>{outcome.failed_statement}</pre>}
        </div>
      )}
      {outcome.error && !outcome.cancelled && (
        <div className="error-box">
          <strong>{describeError(outcome.error)}</strong>
          {outcome.failed_statement && <pre>{outcome.failed_statement}</pre>}
          {outcome.advice && (
            <div className="advice">
              <div className="advice-title">💡 {outcome.advice.title}</div>
              <div>{outcome.advice.explanation}</div>
              <ul>
                {outcome.advice.fixes.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            </div>
          )}
          {onAskAi && (
            <button
              className="small ai-ask"
              onClick={() =>
                onAskAi({
                  sql: outcome.failed_statement ?? outcome.results.map((r) => r.sql).join(";\n"),
                  error: describeError(outcome.error),
                  prompt: "這個錯誤怎麼修？",
                })
              }
            >
              ✨ 問 AI 怎麼修
            </button>
          )}
        </div>
      )}

      {notes.length > 0 && (
        <details className="findings-box" open>
          <summary>
            💡 {notes.length} 個建議
            {notes.some((f) => f.level === "warn") && <span className="warn">（可能影響效能）</span>}
            {onAskAi && (
              <button
                className="small ai-ask"
                onClick={(e) => {
                  e.preventDefault();
                  onAskAi({
                    sql: outcome.results.map((r) => r.sql).join(";\n"),
                    prompt: "幫我檢查這段 SQL 有沒有問題",
                  });
                }}
              >
                ✨ 問 AI
              </button>
            )}
          </summary>
          <ul className="findings">
            {notes.map((f, i) => (
              <li key={i} className={f.level}>
                {outcome.results.length > 1 && (
                  <code>
                    #{f.statement + 1} {f.sql}
                  </code>
                )}
                <div>{f.message}</div>
                <div className="suggestion">建議：{f.suggestion}</div>
              </li>
            ))}
          </ul>
        </details>
      )}

      {current && (
        <>
          <div className="statement-meta">
            {multi && <span className="tab-no">#{tab + 1}</span>}
            <span>{summary(current)}</span>
            <span>{ms(current.duration_ms)}</span>
            {current.warnings > 0 && <span className="warn">{current.warnings} 個警告</span>}
            {current.last_insert_id ? <span>last insert id {current.last_insert_id}</span> : null}
            {current.limit_added != null && (
              <span className="limit-note" title="可在 ⚙ 設定調整自動 LIMIT 的數量或關閉">
                {current.total_rows >= current.limit_added ? (
                  <span className="warn">已達自動 LIMIT {current.limit_added.toLocaleString()}，可能還有更多資料</span>
                ) : (
                  <span>已自動加上 LIMIT {current.limit_added.toLocaleString()}</span>
                )}
                {onRunWithoutLimit && (
                  <button className="link" onClick={() => onRunWithoutLimit(current.sql)}>
                    不限制筆數重新執行
                  </button>
                )}
              </span>
            )}
            {current.info && <span>{current.info}</span>}
          </div>
          {plan && (
            <div className="plan-notes">
              {onAskAi && (
                <button
                  className="small ai-ask"
                  onClick={() =>
                    onAskAi({
                      sql: current.sql.replace(/^\s*explain\s+(analyze\s+|format\s*=\s*\w+\s+)?/i, ""),
                      explain: textTable(current),
                      prompt: "怎麼讓它跑得更快？",
                    })
                  }
                >
                  ✨ 問 AI 怎麼加速
                </button>
              )}
              {plan.notes.length === 0 ? (
                <div className="ok">✓ 執行計畫沒有明顯問題（有用到索引，沒有全表掃描、filesort 或暫存表）</div>
              ) : (
                plan.notes.map((n, i) => (
                  <div key={i} className={n.level}>
                    {n.level === "bad" ? "⛔" : "⚠️"} {n.text}
                  </div>
                ))
              )}
            </div>
          )}
          {current.columns.length > 0 && (
            <ResultGrid
              key={`${outcome.started_at}:${tab}`}
              result={current}
              cellLevel={plan?.cellLevel}
              sessionId={sessionId}
            />
          )}
        </>
      )}
    </div>
  );
}
