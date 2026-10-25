import { useEffect, useRef, useState } from "react";
import { api, describeError } from "../api";

/** After a prompt ends, focus changes are the prompt closing, not the owner coming back. */
const SETTLE_MS = 1500;

/**
 * Covers the whole window until the owner confirms with Touch ID or the login password.
 * The system prompt never shows up by itself while the window is in the background:
 * it asks when the owner comes back to the window, or on the button (or Enter).
 */
export function LockScreen({ askNow, onUnlocked }: { askNow: boolean; onUnlocked: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busyRef = useRef(false);
  const settledAt = useRef(0);
  const button = useRef<HTMLButtonElement>(null);

  const unlock = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      await api.unlockApp();
      onUnlocked();
    } catch (e) {
      setError(describeError(e));
    } finally {
      busyRef.current = false;
      settledAt.current = Date.now();
      setBusy(false);
      button.current?.focus();
    }
  };

  useEffect(() => {
    button.current?.focus();
    // Coming back to the window (after it was in the background) asks.
    let away = !document.hasFocus();
    const onBlur = () => {
      if (!busyRef.current && Date.now() - settledAt.current > SETTLE_MS) away = true;
    };
    const onFocus = () => {
      if (!away || busyRef.current || Date.now() - settledAt.current < SETTLE_MS) return;
      away = false;
      unlock();
    };
    window.addEventListener("blur", onBlur);
    window.addEventListener("focus", onFocus);
    // On start the owner is right here, so ask at once if the window is in front.
    if (askNow && document.hasFocus()) {
      away = false;
      unlock();
    }
    return () => {
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  return (
    <div className="lock-screen" role="dialog" aria-label="MyDB Studio 已鎖定">
      <div className="lock-box">
        <div className="lock-icon">🔒</div>
        <h2>MyDB Studio 已鎖定</h2>
        <p className="muted">用 Touch ID 或這台 Mac 的登入密碼解鎖。鎖定期間 AI 工具（MCP）也不能查詢。</p>
        <button ref={button} className="primary" onClick={unlock} disabled={busy}>
          {busy ? "等待驗證…" : "解鎖"}
        </button>
        {error && <p className="lock-error">{error}</p>}
      </div>
    </div>
  );
}
