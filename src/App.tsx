import { useCallback, useEffect, useState } from "react";
import {
  api,
  describeError,
  newProfile,
  type ConnectionProfile,
  type ExecutionOutcome,
  type OpenedSession,
  quoteIdent,
} from "./api";
import { ConnectionForm } from "./components/ConnectionForm";
import { Home } from "./components/Home";
import { SchemaTree } from "./components/SchemaTree";
import { ResultsPanel } from "./components/ResultsPanel";
import { SqlEditor } from "./components/SqlEditor";
import "./styles.css";

export default function App() {
  const [connections, setConnections] = useState<ConnectionProfile[]>([]);
  const [editing, setEditing] = useState<ConnectionProfile | null>(null);
  const [session, setSession] = useState<OpenedSession | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; kind: "error" | "info" } | null>(null);
  const [sql, setSql] = useState("SELECT VERSION();");
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<ExecutionOutcome | null>(null);

  const reload = useCallback(async () => {
    setConnections(await api.listConnections());
  }, []);

  useEffect(() => {
    reload().catch((e) => setNotice({ text: describeError(e), kind: "error" }));
  }, [reload]);

  const connect = async (c: ConnectionProfile) => {
    setNotice(null);
    setConnecting(c.id);
    try {
      if (session) await api.closeSession(session.session_id);
      setSession(await api.openSession(c.id));
      setOutcome(null);
      // Refresh last-used ordering (and a first-time SSH host fingerprint).
      await reload();
    } catch (e) {
      setSession(null);
      setNotice({ text: describeError(e), kind: "error" });
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
      const result = await api.execute(session.session_id, text);
      setOutcome(result);
      setNotice(
        result.reconnected
          ? {
              text: "連線曾中斷，已自動重新連線。session 變數、暫存表和未提交的交易都已重設。",
              kind: "info",
            }
          : null,
      );
    } catch (e) {
      setNotice({ text: describeError(e), kind: "error" });
    } finally {
      setRunning(false);
    }
  };

  const toggleFavorite = async (c: ConnectionProfile) => {
    await api.setFavorite(c.id, !c.favorite);
    await reload();
  };

  const openTable = (db: string, table: string) =>
    run(`SELECT * FROM ${quoteIdent(db)}.${quoteIdent(table)} LIMIT 100;`);

  const active = connections.find((c) => c.id === session?.connection_id);
  const isMaria = !!session?.info.server_version.toLowerCase().includes("mariadb");

  return (
    <div className="app">
      {session && active && (
        <aside className="sidebar">
          <div className="sidebar-head">
            <button className="small ghost" onClick={disconnect} title="中斷連線並回到首頁">
              ← 所有連線
            </button>
          </div>
          <SchemaTree key={session.session_id} sessionId={session.session_id} onOpenTable={openTable} />
        </aside>
      )}

      <main className="workspace">
        {session && active && (
          <header className="topbar">
            <strong>{active.name}</strong>
            <span className="badge">{session.info.server_version}</span>
            {session.info.tls && <span className="badge good">TLS</span>}
            {active.ssh && <span className="badge good">SSH</span>}
            <span className="spacer" />
            <button className="primary" disabled={running} onClick={() => run(sql)}>
              {running ? "執行中…" : "▶ 執行"}
            </button>
            <button onClick={disconnect}>中斷連線</button>
          </header>
        )}

        {notice && (
          <div className={`notice ${notice.kind}`} onClick={() => setNotice(null)} title="點一下關閉">
            {notice.text}
          </div>
        )}

        {session ? (
          <div className="split">
            <SqlEditor key={session.session_id} initialValue={sql} mariadb={isMaria} onChange={setSql} onRun={run} />
            <ResultsPanel outcome={outcome} />
          </div>
        ) : (
          <Home
            connections={connections}
            connecting={connecting}
            onConnect={connect}
            onEdit={setEditing}
            onNew={() => setEditing(newProfile())}
            onToggleFavorite={toggleFavorite}
          />
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
