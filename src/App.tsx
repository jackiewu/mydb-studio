import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  DEFAULT_SETTINGS,
  describeError,
  newProfile,
  type AppSettings,
  type ConnectionProfile,
  type ExecutionRecord,
  type Finding,
  type OpenedSession,
  quoteIdent,
  type Snippet,
} from "./api";
import { openMenu } from "./contextMenu";
import { copyText } from "./copy";
import { ConnectionForm } from "./components/ConnectionForm";
import { Home } from "./components/Home";
import { RecordsPage } from "./components/RecordsPage";
import { SchemaTree } from "./components/SchemaTree";
import { ResultsPanel } from "./components/ResultsPanel";
import { RiskDialog } from "./components/RiskDialog";
import { SettingsDialog } from "./components/SettingsDialog";
import { SnippetsDialog } from "./components/SnippetsDialog";
import { SqlEditor, type EditorHandle } from "./components/SqlEditor";
import type { RunTarget } from "./editor/statementMarks";
import { statementRanges } from "./editor/statements";
import { editorTheme } from "./editor/themes";
import { useSchema } from "./schema";
import { tabTitle, useTabs } from "./tabs";
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
  const [running, setRunning] = useState(false);
  const [view, setView] = useState<"main" | "records">("main");
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [dialog, setDialog] = useState<"settings" | "snippets" | null>(null);
  // Database selected with USE (double-click in the tree, the picker, or SQL).
  const [currentDb, setCurrentDb] = useState<string | null>(null);
  const editor = useRef<EditorHandle | null>(null);
  // What ▶ would run; drives its label.
  const [runTarget, setRunTarget] = useState<RunTarget | null>(null);
  const schema = useSchema(session?.session_id ?? null);
  const tabs = useTabs(session?.connection_id ?? null, settings.record_by_default);
  const tab = tabs.active;
  // SQL to open in a new tab once a connection's tabs have loaded.
  const pendingSql = useRef<string | null>(null);
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
    if (target && session?.connection_id !== target.id) {
      pendingSql.current = rec.sql;
      await connect(target);
    } else {
      tabs.add(rec.sql);
    }
  };

  const { add: addTab } = tabs;
  const tabsLoaded = tabs.tabs.length > 0;
  useEffect(() => {
    if (tabsLoaded && pendingSql.current != null) {
      addTab(pendingSql.current);
      pendingSql.current = null;
    }
  }, [tabsLoaded, addTab]);

  // ⌘/Ctrl+T opens a tab.
  useEffect(() => {
    if (!session) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "t") {
        e.preventDefault();
        addTab();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [session, addTab]);

  const disconnect = async () => {
    if (!session) return;
    await api.closeSession(session.session_id).catch(() => {});
    setSession(null);
  };

  // Risky statements waiting for the user's go-ahead.
  const [risk, setRisk] = useState<{ findings: Finding[]; answer: (run: boolean) => void } | null>(null);

  const confirmRisk = async (text: string) => {
    if (!settings.confirm_dangerous) return true;
    const dangers = (await api.checkSql(text).catch(() => [] as Finding[])).filter((f) => f.level === "danger");
    if (dangers.length === 0) return true;
    return new Promise<boolean>((resolve) =>
      setRisk({
        findings: dangers,
        answer: (ok) => {
          setRisk(null);
          resolve(ok);
        },
      }),
    );
  };

  const run = async (text: string, record = tab?.recording ?? false) => {
    if (!session || !tab || running || !text.trim()) return;
    if (!(await confirmRisk(text))) return;
    // Results go to the tab that ran, even if another one is shown by then.
    const key = tab.key;
    setRunning(true);
    try {
      const result = await api.execute(session.session_id, text, record, settings.max_rows);
      tabs.update(key, { outcome: result });
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

  // EXPLAIN the first statement of the selection (or of the statement at the cursor).
  const explain = (text: string) => {
    const first = statementRanges(text)[0];
    if (!first) return;
    const stmt = text
      .slice(first.from, first.to)
      .trim()
      .replace(/^(explain|analyze)\s+/i, "");
    run(`EXPLAIN ${stmt}`, false);
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

  const deleteConnection = async (c: ConnectionProfile) => {
    if (!confirm(`刪除連線「${c.name}」？`)) return;
    await api.deleteConnection(c.id);
    if (session?.connection_id === c.id) await disconnect();
    setEditing(null);
    await reload();
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
            session && tab && view === "main"
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
  const runLabel =
    runTarget?.kind === "all"
      ? { text: `執行全部（${runTarget.count} 句）`, hint: "執行全部" }
      : runTarget && runTarget.count > 1
        ? { text: `執行多行（${runTarget.count} 句）`, hint: "執行選取的部分" }
        : runTarget?.kind === "selection"
          ? { text: "執行單行", hint: "執行選取的那一句" }
          : { text: "執行單行", hint: "執行游標所在的那一句（淡色底的那段）" };

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
            <label
              className={tab?.recording ? "record-toggle on" : "record-toggle"}
              title="打開後，這個分頁每次執行的 SQL 與統計都會存進「紀錄」（釘選的分頁預設打開）"
            >
              <input
                type="checkbox"
                checked={!!tab?.recording}
                onChange={(e) => tab && tabs.update(tab.key, { recording: e.target.checked })}
              />
              📝 紀錄
            </label>
            <button onClick={() => setView("records")}>紀錄清單</button>
            <button onClick={() => setDialog("snippets")} title="Snippet：常用 SQL 範本">
              {"{ }"}
            </button>
            <button onClick={() => setDialog("settings")} title="設定">
              ⚙
            </button>
            <button
              disabled={running}
              onClick={() => explain(editor.current?.statementText() ?? "")}
              title="Explain 選取的部分或游標所在的那一句（⌘／Ctrl + E）"
            >
              Explain
            </button>
            {(runTarget?.total ?? 0) > 1 && runTarget?.kind !== "all" && (
              <button
                disabled={running}
                onClick={() => run(editor.current?.allText()?.text ?? "")}
                title="依序執行編輯器裡的每一句（⌘／Ctrl + Shift + Enter）"
              >
                全部（{runTarget?.total} 句）
              </button>
            )}
            <button
              className="primary"
              disabled={running || runTarget?.count === 0}
              onClick={() => run(editor.current?.runTarget()?.text ?? "")}
              title={`${runLabel.hint}（⌘／Ctrl + Enter）\n${runTarget?.text.trim().slice(0, 200) ?? ""}`}
            >
              {running ? "執行中…" : `▶ ${runLabel.text}`}
            </button>
          </header>
        )}

        {notice && (
          <div className={`notice ${notice.kind}`} onClick={() => setNotice(null)} title="點一下關閉">
            {notice.text}
          </div>
        )}

        {session ? (
          <div className="split">
            <div className="editor-pane">
              <div className="tab-bar" role="tablist">
                {tabs.tabs.map((t, i) => (
                  <div
                    key={t.key}
                    role="tab"
                    aria-selected={t.key === tab?.key}
                    className={["tab", t.key === tab?.key ? "active" : "", t.pinnedId != null ? "pinned" : ""].join(
                      " ",
                    )}
                    onClick={() => tabs.setActive(t.key)}
                    onAuxClick={(e) => e.button === 1 && tabs.close(t.key)}
                    onContextMenu={(e) =>
                      openMenu(e, [
                        {
                          label: t.pinnedId != null ? "取消釘選" : "釘選（下次打開這個連線時還原）",
                          onClick: () => tabs.togglePin(t.key),
                        },
                        {
                          label: t.recording ? "停止記錄這個分頁" : "記錄這個分頁的執行",
                          onClick: () => tabs.update(t.key, { recording: !t.recording }),
                        },
                        { label: "複製分頁內的 SQL", disabled: !t.sql.trim(), onClick: () => copyText(t.sql) },
                        "sep",
                        { label: "新分頁", hint: "⌘T", onClick: () => tabs.add() },
                        { label: "關閉", onClick: () => tabs.close(t.key) },
                        {
                          label: "關閉其他未釘選的分頁",
                          disabled: !tabs.tabs.some((o) => o.key !== t.key && o.pinnedId == null),
                          onClick: () => tabs.closeOthers(t.key),
                        },
                      ])
                    }
                    title={t.sql.slice(0, 300)}
                  >
                    <button
                      className="pin"
                      title={t.pinnedId != null ? "取消釘選" : "釘選：內容會保留，下次打開這個連線時還原，預設打開紀錄"}
                      onClick={(e) => {
                        e.stopPropagation();
                        tabs.togglePin(t.key);
                      }}
                    >
                      📌
                    </button>
                    <span className="tab-title">{tabTitle(t.sql, `查詢 ${i + 1}`)}</span>
                    {t.recording && (
                      <span className="tab-rec" title="這個分頁會記錄執行">
                        ●
                      </span>
                    )}
                    <button
                      className="close"
                      title={t.pinnedId != null ? "關閉並取消保留" : "關閉"}
                      onClick={(e) => {
                        e.stopPropagation();
                        tabs.close(t.key);
                      }}
                    >
                      ×
                    </button>
                  </div>
                ))}
                <button className="tab-add" title="新分頁（⌘／Ctrl + T）" onClick={() => tabs.add()}>
                  ＋
                </button>
              </div>
              {tabs.tabs.map((t) => (
                <SqlEditor
                  key={`${session.session_id}:${t.key}:${t.seed}`}
                  active={t.key === tab?.key}
                  initialValue={t.sql}
                  mariadb={isMaria}
                  schema={settings.schema_autocomplete ? schema.namespace : null}
                  defaultSchema={currentDb}
                  theme={editorTheme(settings.editor_theme, systemDark)}
                  fontSize={settings.editor_font_size}
                  snippets={snippets}
                  handle={t.key === tab?.key ? editor : undefined}
                  onNeedColumns={loadColumns}
                  onExplain={explain}
                  onRunTargetChange={setRunTarget}
                  onChange={(value) => tabs.setSql(t.key, value)}
                  onRun={run}
                />
              ))}
            </div>
            <ResultsPanel outcome={tab?.outcome ?? null} />
          </div>
        ) : (
          <Home
            connections={connections}
            connecting={connecting}
            onConnect={connect}
            onEdit={setEditing}
            onNew={() => setEditing(newProfile())}
            onToggleFavorite={toggleFavorite}
            onDuplicate={(c) =>
              setEditing({ ...c, id: "", name: `${c.name} 的副本`, favorite: false, last_used_at: null })
            }
            onDelete={deleteConnection}
            onOpenRecords={() => setView("records")}
            onOpenSettings={() => setDialog("settings")}
          />
        )}
      </main>

      {dialogs}
      {risk && <RiskDialog findings={risk.findings} connectionName={active?.name ?? ""} onAnswer={risk.answer} />}
      {editing && (
        <div className="modal-backdrop">
          <ConnectionForm
            initial={editing}
            onCancel={() => setEditing(null)}
            onSaved={async () => {
              setEditing(null);
              await reload();
            }}
            onDelete={editing.id ? () => deleteConnection(editing) : undefined}
          />
        </div>
      )}
    </div>
  );
}
