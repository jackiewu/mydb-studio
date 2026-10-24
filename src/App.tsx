import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  DEFAULT_SETTINGS,
  describeError,
  newProfile,
  type AppSettings,
  type ConnectionProfile,
  type ExecutionOutcome,
  type ExecutionRecord,
  type OpenedSession,
  quoteIdent,
  type Snippet,
} from "./api";
import { ConnectionForm } from "./components/ConnectionForm";
import { Home } from "./components/Home";
import { RecordsPage } from "./components/RecordsPage";
import { SchemaTree } from "./components/SchemaTree";
import { ResultsPanel } from "./components/ResultsPanel";
import { SettingsDialog } from "./components/SettingsDialog";
import { SnippetsDialog } from "./components/SnippetsDialog";
import { SqlEditor, type EditorHandle } from "./components/SqlEditor";
import { editorTheme } from "./editor/themes";
import { useSchema } from "./schema";
import "./styles.css";

const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");

function useSystemDark() {
  const [dark, setDark] = useState(darkQuery.matches);
  useEffect(() => {
    const onChange = () => setDark(darkQuery.matches);
    darkQuery.addEventListener("change", onChange);
    return () => darkQuery.removeEventListener("change", onChange);
  }, []);
  return dark;
}

export default function App() {
  const [connections, setConnections] = useState<ConnectionProfile[]>([]);
  const [editing, setEditing] = useState<ConnectionProfile | null>(null);
  const [session, setSession] = useState<OpenedSession | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; kind: "error" | "info" } | null>(null);
  const [sql, setSql] = useState("SELECT VERSION();");
  const [running, setRunning] = useState(false);
  const [outcome, setOutcome] = useState<ExecutionOutcome | null>(null);
  const [view, setView] = useState<"main" | "records">("main");
  // Off by default: a run is stored only when the user asks for it.
  const [recording, setRecording] = useState(false);
  // Bumped to reseed the (uncontrolled) editor with `sql`.
  const [editorSeed, setEditorSeed] = useState(0);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [dialog, setDialog] = useState<"settings" | "snippets" | null>(null);
  // Database selected with USE (double-click in the tree, the picker, or SQL).
  const [currentDb, setCurrentDb] = useState<string | null>(null);
  const editor = useRef<EditorHandle | null>(null);
  const schema = useSchema(session?.session_id ?? null);
  const systemDark = useSystemDark();

  const reload = useCallback(async () => {
    setConnections(await api.listConnections());
  }, []);

  const reloadSnippets = useCallback(async () => {
    setSnippets(await api.listSnippets());
  }, []);

  useEffect(() => {
    reload().catch((e) => setNotice({ text: describeError(e), kind: "error" }));
    api.getSettings().then(setSettings, () => {});
    reloadSnippets().catch(() => {});
  }, [reload, reloadSnippets]);

  // Columns of the database in use feed autocomplete.
  const { loadColumns } = schema;
  useEffect(() => {
    if (currentDb) loadColumns(currentDb);
  }, [currentDb, loadColumns]);

  const updateSettings = (next: AppSettings) => {
    setSettings(next);
    api.saveSettings(next).catch((e) => setNotice({ text: describeError(e), kind: "error" }));
  };

  const connect = async (c: ConnectionProfile) => {
    setNotice(null);
    setConnecting(c.id);
    try {
      if (session) await api.closeSession(session.session_id);
      const opened = await api.openSession(c.id);
      setSession(opened);
      setCurrentDb(opened.current_database ?? null);
      setOutcome(null);
      setRecording(settings.record_by_default);
      // Refresh last-used ordering (and a first-time SSH host fingerprint).
      await reload();
      return true;
    } catch (e) {
      setSession(null);
      setNotice({ text: describeError(e), kind: "error" });
      return false;
    } finally {
      setConnecting(null);
    }
  };

  const openRecordInEditor = async (rec: ExecutionRecord) => {
    setView("main");
    const target = connections.find((c) => c.id === rec.connection_id);
    if (target && session?.connection_id !== target.id && !(await connect(target))) return;
    setSql(rec.sql);
    setEditorSeed((n) => n + 1);
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
      const result = await api.execute(session.session_id, text, recording, settings.max_rows);
      setOutcome(result);
      setCurrentDb(result.current_database ?? null);
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

  const switchDatabase = async (db: string) => {
    if (!session) return;
    try {
      await api.useDatabase(session.session_id, db);
      setCurrentDb(db);
    } catch (e) {
      setNotice({ text: describeError(e), kind: "error" });
    }
  };

  const openTable = (db: string, table: string) =>
    run(`SELECT * FROM ${quoteIdent(db)}.${quoteIdent(table)} LIMIT 100;`);

  const active = connections.find((c) => c.id === session?.connection_id);
  const isMaria = !!session?.info.server_version.toLowerCase().includes("mariadb");

  const dialogs = dialog && (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
      {dialog === "settings" ? (
        <SettingsDialog
          settings={settings}
          onChange={updateSettings}
          onClose={() => setDialog(null)}
          onOpenSnippets={() => setDialog("snippets")}
        />
      ) : (
        <SnippetsDialog
          snippets={snippets}
          onChanged={reloadSnippets}
          onInsert={
            session && view === "main"
              ? (s) => {
                  setDialog(null);
                  editor.current?.insertSnippet(s.body);
                }
              : undefined
          }
          onClose={() => setDialog(null)}
        />
      )}
    </div>
  );

  if (view === "records") {
    return (
      <div className="app">
        <RecordsPage connections={connections} onBack={() => setView("main")} onOpenInEditor={openRecordInEditor} />
        {dialogs}
      </div>
    );
  }

  const databaseNames = (schema.databases ?? []).map((d) => d.name);

  return (
    <div className="app">
      {session && active && (
        <aside className="sidebar">
          <div className="sidebar-head">
            <button className="small ghost" onClick={disconnect} title="中斷連線並回到首頁">
              ← 所有連線
            </button>
          </div>
          <SchemaTree
            key={session.session_id}
            schema={schema}
            currentDb={currentDb}
            showSystem={settings.show_system_databases}
            onShowSystemChange={(show) => updateSettings({ ...settings, show_system_databases: show })}
            onUseDatabase={switchDatabase}
            onOpenTable={openTable}
          />
        </aside>
      )}

      <main className="workspace">
        {session && active && (
          <header className="topbar">
            <strong>{active.name}</strong>
            <span className="badge">{session.info.server_version}</span>
            {session.info.tls && <span className="badge good">TLS</span>}
            {active.ssh && <span className="badge good">SSH</span>}
            <label className="db-picker" title="目前資料庫（USE）。左側雙擊資料庫也可以切換。">
              資料庫
              <select value={currentDb ?? ""} onChange={(e) => e.target.value && switchDatabase(e.target.value)}>
                {!currentDb && <option value="">（未選擇）</option>}
                {currentDb && !databaseNames.includes(currentDb) && <option value={currentDb}>{currentDb}</option>}
                {databaseNames.map((n) => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
              </select>
            </label>
            <span className="spacer" />
            <label className={recording ? "record-toggle on" : "record-toggle"} title="打開後，每次執行的 SQL 與統計都會存進「紀錄」">
              <input type="checkbox" checked={recording} onChange={(e) => setRecording(e.target.checked)} />
              📝 紀錄
            </label>
            <button onClick={() => setView("records")}>紀錄清單</button>
            <button onClick={() => setDialog("snippets")} title="Snippet：常用 SQL 範本">
              {"{ }"}
            </button>
            <button onClick={() => setDialog("settings")} title="設定">
              ⚙
            </button>
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
            <SqlEditor
              key={`${session.session_id}:${editorSeed}`}
              initialValue={sql}
              mariadb={isMaria}
              schema={settings.schema_autocomplete ? schema.namespace : null}
              defaultSchema={currentDb}
              theme={editorTheme(settings.editor_theme, systemDark)}
              fontSize={settings.editor_font_size}
              snippets={snippets}
              handle={editor}
              onNeedColumns={loadColumns}
              onChange={setSql}
              onRun={run}
            />
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
            onOpenRecords={() => setView("records")}
            onOpenSettings={() => setDialog("settings")}
          />
        )}
      </main>

      {dialogs}
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
