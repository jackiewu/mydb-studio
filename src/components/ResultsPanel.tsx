import { useEffect, useState } from "react";
import { describeError, type ExecutionOutcome, type StatementResult } from "../api";

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
    return <div className="results empty">按 Ctrl/⌘ + Enter 執行。有選取範圍時只執行選取的部分。</div>;
  }

  const current = outcome.results[tab];
  return (
    <div className="results">
      <div className="results-bar">
        <span>
          完成 {outcome.results.length} 個陳述式，共 {ms(outcome.duration_ms)}
        </span>
        <div className="result-tabs">
          {outcome.results.map((r, i) => (
            <button
              key={i}
              className={i === tab ? "active" : ""}
              title={r.sql}
              onClick={() => setTab(i)}
            >
              #{i + 1} {r.columns.length > 0 ? "結果" : "OK"}
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
          {current.columns.length > 0 && (
            <div className="grid-wrap">
              <table className="grid">
                <thead>
                  <tr>
                    <th className="rownum">#</th>
                    {current.columns.map((c, i) => (
                      <th key={i} title={`${c.table ? c.table + "." : ""}${c.name} · ${c.type_name}`}>
                        {c.name}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {current.rows.map((row, i) => (
                    <tr key={i}>
                      <td className="rownum">{i + 1}</td>
                      {row.map((cell, j) =>
                        cell === null ? (
                          <td key={j} className="null">NULL</td>
                        ) : (
                          <td key={j}>{String(cell)}</td>
                        ),
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </div>
  );
}
