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

export function StatusBar({ session, profile, database, running, outcome, aiEnabled, problem, onOpenProblem }: Props) {
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
      {problem && (
        <button
          className={`status-problem ${problem.severity}`}
          onClick={onOpenProblem}
          title={`${problem.summary}按一下查看或回報。`}
        >
          ⚠
        </button>
      )}
    </footer>
  );
}
