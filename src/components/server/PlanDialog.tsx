import { useEffect, useState } from "react";
import { api, describeError, type AdminChange, type PgAdminChange } from "../../api";

interface Props {
  sessionId: string;
  title: string;
  change: AdminChange | PgAdminChange;
  /** A PostgreSQL change (`PgAdminChange`). */
  postgres?: boolean;
  /** Shown above the SQL, e.g. what dropping a user means. */
  warning?: string;
  danger?: boolean;
  onDone: (ran: string[]) => void;
  onCancel: () => void;
}

/**
 * Shows the SQL a change will run (passwords masked), and runs it once the
 * user agrees. The statements are built again on the Rust side when they
 * run, so what is shown is what runs.
 */
export function PlanDialog({ sessionId, title, change, postgres, warning, danger, onDone, onCancel }: Props) {
  const [plan, setPlan] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);

  useEffect(() => {
    const p = postgres
      ? api.pgAdminPlan(sessionId, change as PgAdminChange)
      : api.adminPlan(sessionId, change as AdminChange);
    p.then(setPlan, (e) => setError(describeError(e)));
  }, [sessionId, change, postgres]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !running && onCancel();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel, running]);

  const runIt = async () => {
    setRunning(true);
    setError(null);
    try {
      onDone(
        await (postgres
          ? api.pgAdminApply(sessionId, change as PgAdminChange)
          : api.adminApply(sessionId, change as AdminChange)),
      );
    } catch (e) {
      setError(describeError(e));
      setRunning(false);
    }
  };

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && !running && onCancel()}>
      <div className="risk-dialog plan-dialog" role="dialog" aria-labelledby="plan-title">
        <h2 id="plan-title">{title}</h2>
        {warning && <p className={danger ? "plan-warning danger" : "plan-warning"}>{warning}</p>}
        {plan && (
          <>
            <p className="muted">
              會執行下面 {plan.length} 句 SQL{"password" in change ? "（密碼以 ******** 表示，不會寫進紀錄）" : ""}：
            </p>
            <pre className="plan-sql">{plan.map((s) => s + ";").join("\n")}</pre>
          </>
        )}
        {plan?.length === 0 && <p className="muted">沒有需要變更的地方。</p>}
        {error && <div className="error-box">{error}</div>}
        <div className="actions">
          <span className="spacer" />
          <button onClick={onCancel} disabled={running} autoFocus={danger}>
            取消
          </button>
          <button
            className={danger ? "danger" : "primary"}
            autoFocus={!danger}
            disabled={!plan || plan.length === 0 || running}
            onClick={runIt}
          >
            {running ? "執行中…" : "執行"}
          </button>
        </div>
      </div>
    </div>
  );
}
