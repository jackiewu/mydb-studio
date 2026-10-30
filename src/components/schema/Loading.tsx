import { useEffect, useState } from "react";

/** Seconds before the "still waiting" hint shows. */
const SLOW_AFTER = 10;

/**
 * A spinner that says what is loading and for how long, so a slow
 * information_schema query does not look like an empty page.
 */
export function Loading({ what, inline }: { what: string; inline?: boolean }) {
  const [start] = useState(() => Date.now());
  const [now, setNow] = useState(start);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  const secs = Math.floor((now - start) / 1000);
  return (
    <div className={inline ? "loading-state inline" : "loading-state"} role="status">
      <span className="spinner" />
      <span>讀取{what}…</span>
      {secs >= 1 && <span className="muted small-text">{secs} 秒</span>}
      {!inline && secs >= SLOW_AFTER && (
        <div className="hint loading-slow">
          仍在等伺服器回應。資料表很多時 information_schema 查詢會比較慢；若一直沒有結果，請檢查連線狀態。
        </div>
      )}
    </div>
  );
}

/** A failed load: what failed, why, and a retry button. */
export function LoadError({ what, error, onRetry }: { what: string; error: string; onRetry?: () => void }) {
  return (
    <div className="error-box load-error">
      <strong>讀取{what}失敗</strong>
      <pre>{error}</pre>
      {onRetry && (
        <button className="small" onClick={onRetry}>
          重試
        </button>
      )}
    </div>
  );
}
