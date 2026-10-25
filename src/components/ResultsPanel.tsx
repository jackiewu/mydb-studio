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

export function ResultsPanel({ outcome }: { outcome: ExecutionOutcome | null }) {
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
      </div>
    );
  }

  const current = outcome.results[tab];
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
            <button
              key={i}
              className={i === tab ? "active" : ""}
              title={r.sql}
              onClick={() => setTab(i)}
            >
              <span className="tab-no">#{i + 1}</span> {label(r.sql)}
            </button>
          ))}
        </div>
      </div>

      {outcome.error && (
        <div className="error-box">
          <strong>{describeError(outcome.error)}</strong>
          {outcome.failed_statement && <pre>{outcome.failed_statement}</pre>}
        </div>
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
          {plan && (
            <div className="plan-notes">
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
            <ResultGrid key={`${outcome.started_at}:${tab}`} result={current} cellLevel={plan?.cellLevel} />
          )}
        </>
      )}
    </div>
  );
}
