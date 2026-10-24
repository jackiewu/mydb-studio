import { useEffect, useState } from "react";
import { describeError, type ExecutionOutcome, type StatementResult } from "../api";
import { isExplain, readPlan } from "../explain";
import { ResultGrid } from "./ResultGrid";

/** Start of a statement, for its result tab. */
function label(sql: string) {
  const s = sql.replace(/\s+/g, " ").trim();
  return s.length > 28 ? s.slice(0, 27) + "…" : s;
}

function ms(n: number) {
  return n < 1000 ? `${n.toFixed(1)} ms` : `${(n / 1000).toFixed(2)} s`;
}

function summary(r: StatementResult) {
  if (r.columns.length > 0) {
    const shown = r.truncated ? `（顯示前 ${r.rows.length} 筆）` : "";
    return `回傳 ${r.total_rows} 筆${shown}`;
  }
  return `影響 ${r.affected_rows} 筆`;
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
  return (
    <div className="results">
      <div className="results-bar">
        <span>
          完成 {outcome.results.length} 個陳述式，共 {ms(outcome.duration_ms)}
        </span>
        {outcome.record_id != null && <span className="recorded">📝 已記錄 #{outcome.record_id}</span>}
        <div className="result-tabs">
          {outcome.results.map((r, i) => (
            <button key={i} className={i === tab ? "active" : ""} title={r.sql} onClick={() => setTab(i)}>
              <span className="tab-no">#{i + 1}</span> {label(r.sql)}
            </button>
          ))}
        </div>
      </div>

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
            <span>{summary(current)}</span>
            <span>{ms(current.duration_ms)}</span>
            {current.warnings > 0 && <span className="warn">{current.warnings} 個警告</span>}
            {current.last_insert_id ? <span>last insert id {current.last_insert_id}</span> : null}
            {current.info && <span>{current.info}</span>}
          </div>
          {current.limit_added != null && (
            <div className="limit-note">
              已自動加上 LIMIT {current.limit_added.toLocaleString()}
              {current.total_rows >= current.limit_added ? "，可能還有更多資料。" : "。"}
              {onRunWithoutLimit && (
                <button className="link" onClick={() => onRunWithoutLimit(current.sql)}>
                  不限制筆數重新執行
                </button>
              )}
              <span className="muted">（可在 ⚙ 設定調整數量或關閉）</span>
            </div>
          )}
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
