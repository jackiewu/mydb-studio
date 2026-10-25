import { useCallback, useEffect, useRef, useState } from "react";
import { ask } from "./ask";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  api,
  DEFAULT_SETTINGS,
  describeError,
  newProfile,
  withoutSecrets,
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
import { AiDrawer, type AiRequest } from "./components/AiDrawer";
import { BugReportDialog, type BugInfo } from "./components/BugReportDialog";
import { ConnectionForm } from "./components/ConnectionForm";
import { Home } from "./components/Home";
import { RecordsPage } from "./components/RecordsPage";
import { SchemaTree } from "./components/SchemaTree";
import { ResultsPanel, type AskAi } from "./components/ResultsPanel";
import { RiskDialog } from "./components/RiskDialog";
import { ServerPage } from "./components/server/ServerPage";
import { SettingsDialog, type Tab as SettingsTab } from "./components/SettingsDialog";
import { SnippetsDialog } from "./components/SnippetsDialog";
import { SqlEditor, type EditorHandle } from "./components/SqlEditor";
import type { RunTarget } from "./editor/statementMarks";
import { statementRanges } from "./editor/statements";
import { editorTheme } from "./editor/themes";
import { useSchema } from "./schema";
import { fileName, isUnsaved, tabTitle, useTabs, type EditorTab } from "./tabs";
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
  const [view, setView] = useState<"main" | "records" | "server">("main");
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [dialog, setDialog] = useState<"settings" | "snippets" | null>(null);
  const [settingsTab, setSettingsTab] = useState<SettingsTab | undefined>(undefined);
  // The AI drawer, and what it was last opened about.
  const [aiOpen, setAiOpen] = useState(false);
  const [aiRequest, setAiRequest] = useState<AiRequest | null>(null);
  // A crash or unexpected error to offer reporting, and one waiting for the user to ask.
  const [bug, setBug] = useState<BugInfo | null>(null);
  const [lastError, setLastError] = useState<BugInfo | null>(null);
  // SQL from the editor to start a new snippet with.
  const [snippetDraft, setSnippetDraft] = useState<Snippet | null>(null);
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

  // Offer to report last run's crash, and catch unexpected errors in this one.
  useEffect(() => {
    api
      .takeCrashInfo()
      .then((c) => {
        if (!c) return;
        setBug({
          reason: c.panic ? "MyDB Studio 上次發生錯誤而關閉。" : "MyDB Studio 上次沒有正常關閉。",
          details: c.panic ?? "（app 被強制結束或當掉，沒有留下錯誤訊息）",
        });
      })
      .catch(() => {});
    const onError = (message: string, stack?: string) => {
      if (/ResizeObserver loop/.test(message)) return;
      setLastError({ reason: "app 發生了未預期的錯誤。", details: `${message}\n${stack ?? ""}` });
    };
    const onWindowError = (e: ErrorEvent) => onError(e.message, e.error?.stack);
    const onRejection = (e: PromiseRejectionEvent) => {
      const r = e.reason;
      // Errors from commands are expected and shown where they happen.
      if (r && typeof r === "object" && "kind" in r) return;
      onError(r instanceof Error ? r.message : String(r), r instanceof Error ? r.stack : undefined);
    };
    window.addEventListener("error", onWindowError);
    window.addEventListener("unhandledrejection", onRejection);
    return () => {
      window.removeEventListener("error", onWindowError);
      window.removeEventListener("unhandledrejection", onRejection);
    };
  }, []);

  // Columns of the database in use feed autocomplete.
  const { loadColumns } = schema;
  useEffect(() => {
    if (currentDb) loadColumns(currentDb);
  }, [currentDb, loadColumns]);

  const updateSettings = (next: AppSettings) => {
    setSettings(next);
    api.saveSettings(next).catch((e) => setNotice({ text: describeError(e), kind: "error" }));
  };

  /**
   * Asks before leaving a connection with tabs that would be lost, then lets
   * go of their drafts. False when the user wants to stay.
   */
  const leaveTabs = async (what: string) => {
    const unsaved = tabs.tabs.filter(isUnsaved);
    if (unsaved.length) {
      const names = unsaved.map((t) => `・${tabTitle(t.sql, "查詢", t.filePath)}`).join("\n");
      if (!(await ask(`有 ${unsaved.length} 個分頁沒有釘選也沒有存檔，${what}後內容會消失：\n${names}\n\n確定要${what}？`, what)))
        return false;
    }
    await tabs.discardDrafts();
    return true;
  };

  const connect = async (c: ConnectionProfile) => {
    if (session && !(await leaveTabs("切換連線"))) return false;
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

  const disconnect = async (ask = true) => {
    if (!session) return;
    if (ask && !(await leaveTabs("中斷連線"))) return;
    await api.closeSession(session.session_id).catch(() => {});
    setSession(null);
    setView("main");
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

  const run = async (text: string, record = tab?.recording ?? false, noLimit = false) => {
    if (!session || !tab || running || !text.trim()) return;
    if (!(await confirmRisk(text))) return;
    // Results go to the tab that ran, even if another one is shown by then.
    const key = tab.key;
    setRunning(true);
    try {
      const result = await api.execute(
        session.session_id,
        text,
        record,
        settings.max_rows,
        noLimit ? 0 : settings.auto_limit,
      );
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

  const stop = () => {
    if (session) api.cancelQuery(session.session_id).catch((e) => setNotice({ text: describeError(e), kind: "error" }));
  };

  // ⌘/Ctrl+. stops the running query.
  const stopRef = useRef(stop);
  stopRef.current = stop;
  useEffect(() => {
    if (!running) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === ".") {
        e.preventDefault();
        stopRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [running]);

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
    if (!(await ask(`刪除連線「${c.name}」？`, "刪除"))) return;
    await api.deleteConnection(c.id);
    if (session?.connection_id === c.id) await disconnect(false);
    setEditing(null);
    await reload();
  };

  // Unsaved tabs come back after the app was closed unexpectedly.
  useEffect(() => {
    if (tabs.restored > 0)
      setNotice({
        text: `已還原 ${tabs.restored} 個上次沒存的分頁（app 上次沒有正常關閉）。`,
        kind: "info",
      });
  }, [tabs.restored]);

  // Closing the window: ask about unsaved tabs, then let their drafts go.
  const leaveRef = useRef(leaveTabs);
  leaveRef.current = leaveTabs;
  useEffect(() => {
    const unlisten = getCurrentWindow().onCloseRequested(async (e) => {
      if (!(await leaveRef.current("關閉 app"))) e.preventDefault();
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const saveToFile = async (t: EditorTab, saveAs = false) => {
    let path = t.filePath;
    if (!path || saveAs) {
      const suggested =
        tabTitle(t.sql, "query", t.filePath)
          .replace(/[^\p{L}\p{N} _.-]+/gu, " ")
          .replace(/\s+/g, " ")
          .trim() || "query";
      path = await saveDialog({
        defaultPath: t.filePath ?? `${suggested.replace(/\.sql$/i, "")}.sql`,
        filters: [{ name: "SQL", extensions: ["sql"] }],
      });
      if (!path) return;
    }
    try {
      await api.saveTextFile(path, t.sql);
      tabs.markSaved(t.key, path, t.sql);
      setNotice({ text: `已存檔：${fileName(path)}`, kind: "info" });
    } catch (e) {
      setNotice({ text: describeError(e), kind: "error" });
    }
  };

  const openSqlFiles = async () => {
    const picked = await openDialog({
      multiple: true,
      filters: [
        { name: "SQL", extensions: ["sql", "txt"] },
        { name: "所有檔案", extensions: ["*"] },
      ],
    });
    const paths = picked == null ? [] : Array.isArray(picked) ? picked : [picked];
    for (const path of paths) {
      const already = tabs.tabs.find((t) => t.filePath === path);
      if (already) {
        tabs.setActive(already.key);
        continue;
      }
      try {
        const text = await api.readTextFile(path);
        tabs.add(text, path);
      } catch (e) {
        setNotice({ text: describeError(e), kind: "error" });
      }
    }
  };

  // ⌘S saves the tab to its file (asking where the first time), ⌘⇧S saves
  // as, ⌘O opens .sql files in new tabs.
  const fileKeys = useRef({ save: saveToFile, open: openSqlFiles, tab });
  fileKeys.current = { save: saveToFile, open: openSqlFiles, tab };
  useEffect(() => {
    if (!session) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey)) return;
      const k = e.key.toLowerCase();
      if (k === "s" && fileKeys.current.tab) {
        e.preventDefault();
        fileKeys.current.save(fileKeys.current.tab, e.shiftKey);
      } else if (k === "o") {
        e.preventDefault();
        fileKeys.current.open();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [session]);

  const askAi = (ask: AskAi) => {
    setAiRequest({ ...ask, nonce: Date.now() });
    setAiOpen(true);
  };

  const openTable = (db: string, table: string) =>
    run(`SELECT * FROM ${quoteIdent(db)}.${quoteIdent(table)} LIMIT 100;`);

  const active = connections.find((c) => c.id === session?.connection_id);
  const isMaria = !!session?.info.server_version.toLowerCase().includes("mariadb");

  const dialogs = dialog && (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
      {dialog === "settings" ? (
        <SettingsDialog
          initialTab={settingsTab}
          settings={settings}
          onChange={updateSettings}
          onClose={() => setDialog(null)}
          onOpenSnippets={() => (setSnippetDraft(null), setDialog("snippets"))}
          onReportBug={() => {
            setDialog(null);
            setBug({ reason: "", details: lastError?.details ?? "（沒有錯誤紀錄）" });
          }}
        />
      ) : (
        <SnippetsDialog
          snippets={snippets}
          initial={snippetDraft}
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
            <button className="small ghost" onClick={() => disconnect()} title="中斷連線並回到首頁">
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
            <button
              className={view === "server" ? "active" : undefined}
              onClick={() => setView(view === "server" ? "main" : "server")}
              title="伺服器狀態、執行中的查詢、使用者與權限"
            >
              🖥 伺服器
            </button>
            {view === "server" ? (
              <button className="primary" onClick={() => setView("main")}>
                ← 回到編輯器
              </button>
            ) : (
              <>
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
                <button onClick={() => (setSnippetDraft(null), setDialog("snippets"))} title="Snippet：常用 SQL 範本">
                  {"{ }"}
                </button>
                <button onClick={() => (setSettingsTab(undefined), setDialog("settings"))} title="設定">
                  ⚙
                </button>
                <button
                  className={aiOpen ? "active" : undefined}
                  onClick={() => {
                    if (aiOpen) return setAiOpen(false);
                    const sql = editor.current?.statementText().trim() ?? "";
                    if (!aiRequest && sql) askAi({ sql });
                    else setAiOpen(true);
                  }}
                  title="AI 分析（⌘／Ctrl + I 分析選取的部分或游標所在的那一句）"
                >
                  ✨ AI
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
                {running && (
                  <button className="danger" onClick={stop} title="停止執行中的查詢（⌘／Ctrl + .）">
                    ■ 停止
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
              </>
            )}
          </header>
        )}

        {notice && (
          <div className={`notice ${notice.kind}`} onClick={() => setNotice(null)} title="點一下關閉">
            {notice.text}
          </div>
        )}

        {session && view === "server" && (
          <ServerPage
            key={session.session_id}
            sessionId={session.session_id}
            connectionId={session.info.connection_id}
            mariadb={isMaria}
            onOpenSql={(sql) => {
              setView("main");
              tabs.add(sql);
            }}
          />
        )}
        {session ? (
          <div className="split" hidden={view === "server"}>
            <div className="editor-pane">
              <div className="tab-bar" role="tablist">
                {tabs.tabs.map((t, i) => (
                  <div
                    key={t.key}
                    role="tab"
                    aria-selected={t.key === tab?.key}
                    className={["tab", t.key === tab?.key ? "active" : "", t.pinned ? "pinned" : ""].join(
                      " ",
                    )}
                    onClick={() => tabs.setActive(t.key)}
                    onAuxClick={(e) => e.button === 1 && tabs.close(t.key)}
                    onContextMenu={(e) =>
                      openMenu(e, [
                        {
                          label: t.pinned ? "取消釘選" : "釘選（下次打開這個連線時還原）",
                          onClick: () => tabs.togglePin(t.key),
                        },
                        {
                          label: t.recording ? "停止記錄這個分頁" : "記錄這個分頁的執行",
                          onClick: () => tabs.update(t.key, { recording: !t.recording }),
                        },
                        { label: "複製分頁內的 SQL", disabled: !t.sql.trim(), onClick: () => copyText(t.sql) },
                        "sep",
                        {
                          label: t.filePath ? `儲存到 ${fileName(t.filePath)}` : "存成 .sql 檔…",
                          hint: "⌘S",
                          onClick: () => saveToFile(t),
                        },
                        { label: "另存新檔…", hint: "⇧⌘S", onClick: () => saveToFile(t, true) },
                        { label: "開啟 .sql 檔…", hint: "⌘O", onClick: openSqlFiles },
                        ...(t.filePath
                          ? [{ label: "複製檔案路徑", onClick: () => copyText(t.filePath ?? "") }]
                          : []),
                        "sep",
                        { label: "新分頁", hint: "⌘T", onClick: () => tabs.add() },
                        { label: "關閉", onClick: () => tabs.close(t.key) },
                        {
                          label: "關閉其他未釘選的分頁",
                          disabled: !tabs.tabs.some((o) => o.key !== t.key && !o.pinned),
                          onClick: () => tabs.closeOthers(t.key),
                        },
                      ])
                    }
                    title={(t.filePath ? `${t.filePath}\n\n` : "") + t.sql.slice(0, 300)}
                  >
                    <button
                      className="pin"
                      title={t.pinned ? "取消釘選" : "釘選：內容會保留，下次打開這個連線時還原，預設打開紀錄"}
                      onClick={(e) => {
                        e.stopPropagation();
                        tabs.togglePin(t.key);
                      }}
                    >
                      📌
                    </button>
                    <span className="tab-title">{tabTitle(t.sql, `查詢 ${i + 1}`, t.filePath)}</span>
                    {t.filePath && t.savedSql != null && t.sql !== t.savedSql && (
                      <span className="tab-dirty" title="有修改還沒存檔（⌘S）">
                        •
                      </span>
                    )}
                    {t.recording && (
                      <span className="tab-rec" title="這個分頁會記錄執行">
                        ●
                      </span>
                    )}
                    <button
                      className="close"
                      title={t.pinned ? "關閉並取消保留" : "關閉"}
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
                  autoLimit={settings.auto_limit}
                  onSaveSnippet={(body) => {
                    setSnippetDraft({ id: 0, prefix: "", name: tabTitle(body, ""), body });
                    setDialog("snippets");
                  }}
                  onAskAi={(sql) => askAi({ sql })}
                  onRunTargetChange={setRunTarget}
                  onChange={(value) => tabs.setSql(t.key, value)}
                  onRun={run}
                />
              ))}
            </div>
            <ResultsPanel
              outcome={tab?.outcome ?? null}
              onRunWithoutLimit={(sql) => run(sql, undefined, true)}
              sessionId={session.session_id}
              onAskAi={askAi}
            />
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
              setEditing({
                ...withoutSecrets(c),
                id: "",
                name: `${c.name} 的副本`,
                favorite: false,
                last_used_at: null,
              })
            }
            onDelete={deleteConnection}
            onOpenRecords={() => setView("records")}
            onOpenSettings={() => (setSettingsTab(undefined), setDialog("settings"))}
          />
        )}
      </main>

      {aiOpen && session && (
        <AiDrawer
          request={aiRequest}
          sessionId={session.session_id}
          connectionId={session.connection_id}
          connectionName={active?.name ?? ""}
          settings={settings}
          onClose={() => setAiOpen(false)}
          onOpenSettings={() => {
            setSettingsTab("ai");
            setDialog("settings");
          }}
          onNewTab={(sql) => tabs.add(sql)}
        />
      )}
      {lastError && !bug && (
        <div className="toast">
          發生未預期的錯誤。
          <button className="link report-link" onClick={() => (setBug(lastError), setLastError(null))}>
            回報問題…
          </button>
          <button className="link" onClick={() => setLastError(null)}>
            ✕
          </button>
        </div>
      )}
      {bug && <BugReportDialog info={bug} onClose={() => setBug(null)} />}
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
