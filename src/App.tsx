import { useCallback, useEffect, useState } from "react";
import {
  api,
  describeError,
  newProfile,
  type ConnectionProfile,
  type ExecutionOutcome,
  type OpenedSession,
} from "./api";
import { ConnectionForm } from "./components/ConnectionForm";
import { ResultsPanel } from "./components/ResultsPanel";
import { SqlEditor } from "./components/SqlEditor";
import "./styles.css";

export default function App() {
  const [connections, setConnections] = useState<ConnectionProfile[]>([]);
  const [editing, setEditing] = useState<ConnectionProfile | null>(null);
  const [session, setSession] = useState<OpenedSession | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [sql, setSql] = useState("SELECT VERSION();");
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<ExecutionOutcome | null>(null);

  const reload = useCallback(async () => {
    setConnections(await api.listConnections());
  }, []);

  useEffect(() => {
    reload().catch((e) => setNotice(describeError(e)));
  }, [reload]);

  const connect = async (c: ConnectionProfile) => {
    setNotice(null);
    setConnecting(c.id);
    try {
      if (session) await api.closeSession(session.session_id);
      setSession(await api.openSession(c.id));
      setOutcome(null);
      // A first SSH connect stores the host fingerprint in the profile.
      if (c.ssh && !c.ssh.known_host_fingerprint) await reload();
    } catch (e) {
      setSession(null);
      setNotice(describeError(e));
    } finally {
      setConnecting(null);
    }
  };

  const disconnect = async () => {
    if (!session) return;
    await api.closeSession(session.session_id).catch(() => {});
    setSession(null);
    setOutcome(null);
  };

  const run = async (text: string) => {
    if (!session || running || !text.trim()) return;
    setRunning(true);
    try {
      setOutcome(await api.execute(session.session_id, text));
    } catch (e) {
      setNotice(describeError(e));
    } finally {
      setRunning(false);
    }
  };

  const active = connections.find((c) => c.id === session?.connection_id);
  const isMaria = !!session?.info.server_version.toLowerCase().includes("mariadb");

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-head">
          <span>連線</span>
          <button className="small" onClick={() => setEditing(newProfile())}>
            ＋ 新增
          </button>
        </div>
        <ul className="conn-list">
          {connections.map((c) => (
            <li key={c.id} className={c.id === session?.connection_id ? "active" : ""}>
              <button className="conn-name" onClick={() => connect(c)}>
                <span>{c.name}</span>
                <small>
                  {c.user}@{c.host}:{c.port}
                  {c.ssl.mode !== "disabled" && " · TLS"}
                  {c.ssh && " · SSH"}
                </small>
              </button>
              <button className="small ghost" title="編輯" onClick={() => setEditing(c)}>
                ✎
              </button>
              {connecting === c.id && <span className="spinner" />}
            </li>
          ))}
          {connections.length === 0 && <li className="hint">還沒有連線，按「＋ 新增」建立一個。</li>}
        </ul>
      </aside>

      <main className="workspace">
        <header className="topbar">
          {session && active ? (
            <>
              <strong>{active.name}</strong>
              <span className="badge">{session.info.server_version}</span>
              {session.info.tls && <span className="badge good">TLS</span>}
              {active.ssh && <span className="badge good">SSH</span>}
              <span className="spacer" />
              <button className="primary" disabled={running} onClick={() => run(sql)}>
                {running ? "執行中…" : "▶ 執行"}
              </button>
              <button onClick={disconnect}>中斷連線</button>
            </>
          ) : (
            <span className="muted">點左側的連線開始。</span>
          )}
        </header>

        {notice && (
          <div className="notice" onClick={() => setNotice(null)}>
            {notice}
          </div>
        )}

        {session ? (
          <div className="split">
            <SqlEditor key={session.session_id} initialValue={sql} mariadb={isMaria} onChange={setSql} onRun={run} />
            <ResultsPanel outcome={outcome} />
          </div>
        ) : (
          <div className="welcome">
            <h1>MyDB Studio</h1>
            <p>MySQL / MariaDB 用戶端，支援 SSL/TLS 與 SSH 通道。</p>
          </div>
        )}
      </main>

      {editing && (
        <div className="modal-backdrop">
          <ConnectionForm
            initial={editing}
            onCancel={() => setEditing(null)}
            onSaved={async () => {
              setEditing(null);
              await reload();
            }}
            onDelete={
              editing.id
                ? async () => {
                    if (!confirm(`刪除連線「${editing.name}」？`)) return;
                    await api.deleteConnection(editing.id);
                    if (session?.connection_id === editing.id) await disconnect();
                    setEditing(null);
                    await reload();
                  }
                : undefined
            }
          />
        </div>
      )}
    </div>
  );
}
