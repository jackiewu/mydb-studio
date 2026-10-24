import { useEffect } from "react";
import type { Finding } from "../api";

/** Asks before running statements that can destroy data. */
export function RiskDialog({
  findings,
  connectionName,
  onAnswer,
}: {
  findings: Finding[];
  connectionName: string;
  onAnswer: (run: boolean) => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onAnswer(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onAnswer]);

  return (
    <div className="modal-backdrop">
      <div className="risk-dialog" role="alertdialog" aria-labelledby="risk-title">
        <h2 id="risk-title">⚠️ 確定要執行嗎？</h2>
        <p className="muted">
          連線：<strong>{connectionName}</strong>。這些語句可能會毀損資料，而且通常無法復原。
        </p>
        <ul className="findings">
          {findings.map((f, i) => (
            <li key={i} className={f.level}>
              <code>{f.sql}</code>
              <div>{f.message}</div>
              <div className="suggestion">建議：{f.suggestion}</div>
            </li>
          ))}
        </ul>
        <div className="actions">
          <span className="muted small-text">可以在 ⚙ 設定裡關掉這個確認。</span>
          <span className="spacer" />
          <button className="primary" autoFocus onClick={() => onAnswer(false)}>
            取消
          </button>
          <button className="danger" onClick={() => onAnswer(true)}>
            仍要執行
          </button>
        </div>
      </div>
    </div>
  );
}
