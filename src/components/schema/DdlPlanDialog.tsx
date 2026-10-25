import { useEffect, useState } from "react";
import { api, describeError, type DdlPlan, type SchemaApplied, type SchemaChange } from "../../api";

interface Props {
  sessionId: string;
  title: string;
  change: SchemaChange;
  /**
   * For drops: the name the user must type before it runs, since a drop
   * cannot be undone.
   */
  confirmName?: string;
  onDone: (result: SchemaApplied) => void;
  onCancel: () => void;
}

/**
 * Shows the DDL a schema change will run and what to know about it, and
 * runs it once the user agrees. As with GRANT, the statements are built
 * again on the Rust side when they run. Every run goes into the records.
 */
export function DdlPlanDialog({ sessionId, title, change, confirmName, onDone, onCancel }: Props) {
  const [plan, setPlan] = useState<DdlPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [failed, setFailed] = useState<SchemaApplied | null>(null);
  const [running, setRunning] = useState(false);
  const [typed, setTyped] = useState("");
  const danger = confirmName != null;

  useEffect(() => {
    api.schemaPlan(sessionId, change).then(setPlan, (e) => setError(describeError(e)));
  }, [sessionId, change]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !running && onCancel();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, running]);

  const runIt = async () => {
    setRunning(true);
    setError(null);
    try {
      const result = await api.schemaApply(sessionId, change);
      if (result.error) {
        setFailed(result);
        setError(describeError(result.error));
        setRunning(false);
      } else onDone(result);
    } catch (e) {
      setError(describeError(e));
      setRunning(false);
    }
  };

  const ready = !!plan && plan.statements.length > 0 && !failed && (!danger || typed === confirmName);

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && !running && onCancel()}>
      <div className="risk-dialog plan-dialog ddl-dialog" role="dialog" aria-labelledby="ddl-title">
        <h2 id="ddl-title">{title}</h2>
        {plan && plan.statements.length > 0 && (
          <>
            <p className="muted">會依序執行下面 {plan.statements.length} 句，執行結果會存進「紀錄」：</p>
            <pre className="plan-sql">{plan.statements.map((s) => s + ";").join("\n\n")}</pre>
          </>
        )}
        {plan?.notes.map((n) => (
          <p key={n} className={danger ? "plan-warning danger" : "plan-warning"}>
            {n}
          </p>
        ))}
        {plan?.statements.length === 0 && <p className="muted">沒有需要變更的地方。</p>}
        {danger && plan && !failed && (
          <label className="confirm-name">
            確定要刪除的話，請輸入名稱 <code>{confirmName}</code>：
            <input value={typed} onChange={(e) => setTyped(e.target.value)} spellCheck={false} autoFocus />
          </label>
        )}
        {error && (
          <div className="error-box">
            {error}
            {failed?.record_id != null && <div className="muted small-text">這次執行已存進紀錄（#{failed.record_id}）。</div>}
          </div>
        )}
        <div className="actions">
          <span className="spacer" />
          <button onClick={failed ? () => onDone(failed) : onCancel} disabled={running}>
            {failed ? "關閉" : "取消"}
          </button>
          {!failed && (
            <button className={danger ? "danger" : "primary"} disabled={!ready || running} onClick={runIt}>
              {running ? "執行中…" : danger ? "刪除" : "執行"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
