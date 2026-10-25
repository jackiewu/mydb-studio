import { useEffect, useState } from "react";
import { api, type ConnectionProfile, type ExecutionOutcome, type OpenedSession, type RedisOutcome } from "../api";

/** Something to offer reporting: a crash (error) or an unclean exit (notice). */
export interface Problem {
  severity: "error" | "notice";
  summary: string;
}

interface Props {
  session: OpenedSession | null;
  profile: ConnectionProfile | undefined;
  database: string | null;
  running: boolean;
  outcome: ExecutionOutcome | RedisOutcome | null;
  aiEnabled: boolean;
  problem: Problem | null;
  onOpenProblem: () => void;
  /** Locks the app now; absent when the lock is off. */
  onLock?: () => void;
}

/** Line icons, so they match each other and the text color (an emoji lock is colored on macOS). */
function LockIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
      <rect x="3" y="7" width="10" height="7.5" rx="1.5" />
      <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />
    </svg>
  );
}

function WarnIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden>
      <path d="M8 1.8 15 14H1z" strokeLinejoin="round" />
      <path d="M8 6.5v3.5" strokeLinecap="round" />
      <circle cx="8" cy="12" r="0.4" fill="currentColor" />
    </svg>
  );
}

function ms(n: number) {
  return n < 1000 ? `${Math.round(n)} ms` : `${(n / 1000).toFixed(2)} 秒`;
}

function isRedis(o: ExecutionOutcome | RedisOutcome): o is RedisOutcome {
  return "db" in o && "failed_command" in o;
}

/** Last run, in a few words. */
function lastRun(o: ExecutionOutcome | RedisOutcome) {
  const parts = [`${ms(o.duration_ms)}`];
  if (isRedis(o)) {
    parts.push(`${o.results.length} 個指令`);
  } else {
    parts.push(`${o.results.length} 句`);
    const rows = o.results.reduce((n, r) => n + (r.columns.length > 0 ? r.total_rows : 0), 0);
    const affected = o.results.reduce((n, r) => n + r.affected_rows, 0);
    if (rows > 0) parts.push(`${rows.toLocaleString()} 筆`);
    if (affected > 0) parts.push(`影響 ${affected.toLocaleString()} 筆`);
  }
  if (o.cancelled) parts.push("已停止");
  else if (o.error) parts.push("失敗");
  return `上次執行：${parts.join(" · ")}`;
}

/** Whether the local model is loaded, checked now and then while AI is on. */
function useAiRunning(enabled: boolean) {
  const [up, setUp] = useState(false);
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const check = () =>
      api.aiRunning().then(
        (v) => live && setUp(v),
        () => {},
      );
    check();
    const timer = setInterval(check, 15000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [enabled]);
  return enabled && up;
}

export function StatusBar({
  session,
  profile,
  database,
  running,
  outcome,
  aiEnabled,
  problem,
  onOpenProblem,
  onLock,
}: Props) {
  const aiUp = useAiRunning(aiEnabled);
  return (
    <footer className="status-bar">
      {session && profile ? (
        <span className="status-conn" title={session.info.server_version}>
          <span className="status-dot" />
          {profile.name}
          <span className="muted">{session.info.server_version}</span>
          {session.info.tls && <span className="good">TLS</span>}
          {profile.ssh && <span className="good">SSH</span>}
          {database && <span className="muted">{database}</span>}
        </span>
      ) : (
        <span className="muted">未連線</span>
      )}
      <span className="spacer" />
      {running ? (
        <span className="muted">執行中…</span>
      ) : (
        session &&
        outcome && <span className={outcome.error && !outcome.cancelled ? "bad" : "muted"}>{lastRun(outcome)}</span>
      )}
      {aiEnabled && (
        <span className="muted" title="本機 AI 模型用到時才啟動，閒置一段時間後自動關閉">
          AI：{aiUp ? "已載入" : "未啟動"}
        </span>
      )}
      {onLock && (
        <button className="status-icon" onClick={onLock} title="立即鎖定（⌘⇧L）" aria-label="立即鎖定">
          <LockIcon />
        </button>
      )}
      {problem && (
        <button
          className={`status-icon ${problem.severity === "error" ? "bad" : ""}`}
          onClick={onOpenProblem}
          title={`${problem.summary}按一下查看或回報。`}
        >
          <WarnIcon />
        </button>
      )}
    </footer>
  );
}
