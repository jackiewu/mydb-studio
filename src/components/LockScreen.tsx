import { useEffect, useRef, useState } from "react";
import { api, describeError } from "../api";

/** Covers the whole window until the owner confirms with Touch ID or the login password. */
export function LockScreen({ onUnlocked }: { onUnlocked: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const asked = useRef(false);

  const unlock = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.unlockApp();
      onUnlocked();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };

  // Ask right away once; after that only on the button.
  useEffect(() => {
    if (asked.current) return;
    asked.current = true;
    unlock();
  }, []);

  return (
    <div className="lock-screen" role="dialog" aria-label="MyDB Studio 已鎖定">
      <div className="lock-box">
        <div className="lock-icon">🔒</div>
        <h2>MyDB Studio 已鎖定</h2>
        <p className="muted">用 Touch ID 或這台 Mac 的登入密碼解鎖。鎖定期間 AI 工具（MCP）也不能查詢。</p>
        <button className="primary" onClick={unlock} disabled={busy}>
          {busy ? "等待驗證…" : "解鎖"}
        </button>
        {error && <p className="lock-error">{error}</p>}
      </div>
    </div>
  );
}
