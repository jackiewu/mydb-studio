import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ask } from "./ask";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  api,
  recordsByDefault,
  DEFAULT_SETTINGS,
  describeError,
  newProfile,
  withoutSecrets,
  type AppSettings,
  type ConnectionProfile,
  type ExecutionRecord,
  type Finding,
  type ImportPreview,
  type KeyName,
  KIND_LABEL,
  type OpenedSession,
  quoteIdent,
  type Snippet,
} from "./api";
import { openMenu } from "./contextMenu";
import { copyText } from "./copy";
import { AiDrawer, type AiRequest } from "./components/AiDrawer";
import { BugReportDialog, type BugInfo } from "./components/BugReportDialog";
import { ConnectionForm } from "./components/ConnectionForm";
import { ExportDialog, ImportDialog } from "./components/ConnectionTransfer";
import { Home } from "./components/Home";
import { LockScreen } from "./components/LockScreen";
import { RecordsPage } from "./components/RecordsPage";
import { RedisEditor } from "./components/RedisEditor";
import { RedisKeyTree } from "./components/RedisKeyTree";
import { CONNECTION_STATE_RESET, RedisPanel } from "./components/RedisPanel";
import { SchemaTree } from "./components/SchemaTree";
import { ResultsPanel, type AskAi } from "./components/ResultsPanel";
import { RiskDialog } from "./components/RiskDialog";
import { RedisServerPage } from "./components/server/RedisServerPage";
import { ServerPage } from "./components/server/ServerPage";
import { SchemaPage, type SchemaTarget } from "./components/schema/SchemaPage";
import { SettingsDialog, type Tab as SettingsTab } from "./components/SettingsDialog";
import { SnippetsDialog } from "./components/SnippetsDialog";
import { Splitter, usePaneSize } from "./components/Splitter";
import { StatusBar, type Problem } from "./components/StatusBar";
import { SqlEditor, type EditorHandle } from "./components/SqlEditor";
import type { RunTarget } from "./editor/statementMarks";
import { statementRanges } from "./editor/statements";
import { editorTheme } from "./editor/themes";
import { useSchema } from "./schema";
import { useSessionStatus } from "./status/useSessionStatus";
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
  const [transfer, setTransfer] = useState<{ kind: "export" } | { kind: "import"; preview: ImportPreview } | null>(
    null,
  );
  const [session, setSession] = useState<OpenedSession | null>(null);
  const [connecting, setConnecting] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; kind: "error" | "info" } | null>(null);
  // Pane sizes the user dragged to; null keeps the default layout.
  const [sidebarWidth, setSidebarWidth] = usePaneSize("mydb.sidebarWidth");
  const [editorHeight, setEditorHeight] = usePaneSize("mydb.editorHeight");
  const splitRef = useRef<HTMLDivElement>(null);
  const [running, setRunning] = useState(false);
  const [view, setView] = useState<"main" | "records" | "server" | "schema">("main");
  // Where the schema page opens, set from the tree.
  const [schemaTarget, setSchemaTarget] = useState<SchemaTarget | null>(null);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [dialog, setDialog] = useState<"settings" | "snippets" | null>(null);
  const [settingsTab, setSettingsTab] = useState<SettingsTab | undefined>(undefined);
  // The AI drawer, and what it was last opened about.
  const [aiOpen, setAiOpen] = useState(false);
  const [aiRequest, setAiRequest] = useState<AiRequest | null>(null);
  // The open problem report, and a crash or unexpected error waiting behind ⚠ in the status bar.
  const [bug, setBug] = useState<BugInfo | null>(null);
  const [lastError, setLastError] = useState<(BugInfo & Problem) | null>(null);
  // SQL from the editor to start a new snippet with.
  const [snippetDraft, setSnippetDraft] = useState<Snippet | null>(null);
  // Database selected with USE (double-click in the tree, the picker, or SQL).
  const [currentDb, setCurrentDb] = useState<string | null>(null);
  const editor = useRef<EditorHandle | null>(null);
  // What ▶ would run; drives its label.
  const [runTarget, setRunTarget] = useState<RunTarget | null>(null);
  const isRedis = session?.kind === "redis";
  // Redis: the editor's database (SELECT), the key in the value view, and
  // which of the two the lower panel shows.
  const [redisDb, setRedisDb] = useState(0);
  const [openKey, setOpenKey] = useState<{ db: number; key: KeyName } | null>(null);
  const [redisView, setRedisView] = useState<"result" | "key">("result");
  const schema = useSchema(isRedis ? null : (session?.session_id ?? null));
  const recordMode = connections.find((c) => c.id === session?.connection_id)?.record_mode;
  const recordForced = recordMode === "forced";
  const tabs = useTabs(
    session?.connection_id ?? null,
    recordsByDefault(recordMode, settings.record_by_default),
    isRedis,
    recordForced,
  );
  const tab = tabs.active;
  // SQL to open in a new tab once a connection's tabs have loaded.
  const pendingSql = useRef<string | null>(null);
  const systemDark = useSystemDark();
  const sessionStatus = useSessionStatus(session?.session_id ?? null, running);

  const reload = useCallback(async () => {
    setConnections(await api.listConnections());
  }, []);

  /** Picks an export file and shows what importing it would do. */
  const importConnections = async () => {
    const path = await openDialog({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
    if (typeof path !== "string") return;
    try {
      setTransfer({ kind: "import", preview: await api.readConnectionImport(path) });
    } catch (e) {
      setNotice({ text: describeError(e), kind: "error" });
    }
  };

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
    Promise.all([api.crashInfo(), api.getSettings()])
      .then(([c, s]) => {
        if (!c) return;
        const reason = c.panic ? "MyDB Studio 上次發生錯誤而關閉。" : "MyDB Studio 上次沒有正常關閉。";
        const details = c.panic ?? "（app 被強制結束或當掉，沒有留下錯誤訊息）";
        // Only a real crash may open the report by itself, and only when asked to;
        // a forced quit (e.g. restarting the dev app) just shows ⚠.
        if (c.panic && s.report_crash_on_start) {
          setBug({ reason, details });
          api.clearCrashInfo().catch(() => {});
        }
        else setLastError({ reason, details, summary: reason, severity: c.panic ? "error" : "notice" });
      })
      .catch(() => {});
    const onError = (message: string, stack?: string) => {
      if (/ResizeObserver loop/.test(message)) return;
      const reason = "app 發生了未預期的錯誤。";
      setLastError({ reason, details: `${message}\n${stack ?? ""}`, summary: reason, severity: "error" });
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
    api.saveSettings(next).catch((e) => {
      setNotice({ text: describeError(e), kind: "error" });
      // Show what is actually saved (e.g. a lock change that was refused).
      api.getSettings().then(setSettings, () => {});
    });
  };

  /**
   * Asks before leaving a connection with tabs that would be lost, then lets
   * go of their drafts. False when the user wants to stay.
   */
  const leaveTabs = async (what: string) => {
    const unsaved = tabs.tabs.filter(isUnsaved);
    if (unsaved.length) {
      const names = unsaved.map((t) => `・${tabTitle(t.sql, "查詢", t.filePath, isRedis)}`).join("\n");
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
      setRedisDb(opened.redis_db ?? 0);
      setOpenKey(null);
      setRedisView("result");
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

  // App lock: covered until Touch ID / the login password.
  const [locked, setLocked] = useState(false);
  // Locked since the app started (ask at once), rather than idle or Lock now (wait for the owner).
  const [lockedAtStart, setLockedAtStart] = useState(false);
  const [lockAvailable, setLockAvailable] = useState(false);
  const lockOn = settings.lock_mode !== "off";
  useEffect(() => {
    api.lockStatus().then(
      (s) => {
        setLocked(s.locked);
        setLockedAtStart(s.locked);
        setLockAvailable(s.available);
      },
      () => {},
    );
    const off = listen("app-locked", () => setLocked(true));
    return () => {
      off.then((f) => f());
    };
  }, []);
  const lockNow = useCallback(async () => {
    if (!lockOn) return;
    await api.lockApp().catch(() => {});
    setLocked(true);
  }, [lockOn]);
  // Lock after idle time, and after the computer slept (the timer jumps).
  useEffect(() => {
    if (!lockOn || locked) return;
    let lastInput = Date.now();
    let lastTick = Date.now();
    const touch = () => (lastInput = Date.now());
    const events = ["pointerdown", "pointermove", "keydown", "wheel"] as const;
    events.forEach((e) => window.addEventListener(e, touch, { passive: true }));
    const timer = setInterval(() => {
      const now = Date.now();
      const slept = now - lastTick > 60_000;
      lastTick = now;
      const idleMs = settings.lock_idle_minutes * 60_000;
      if ((slept && settings.lock_on_sleep) || (idleMs > 0 && now - lastInput >= idleMs)) lockNow();
    }, 5000);
    return () => {
      events.forEach((e) => window.removeEventListener(e, touch));
      clearInterval(timer);
    };
  }, [lockOn, locked, settings.lock_idle_minutes, settings.lock_on_sleep, lockNow]);
  // ⌘/Ctrl+Shift+L locks now.
  useEffect(() => {
    if (!lockOn) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "l") {
        e.preventDefault();
        lockNow();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [lockOn, lockNow]);
  // Optionally let go of connections while locked.
  const sessionId = session?.session_id;
  useEffect(() => {
    if (locked && settings.lock_disconnect && sessionId) {
      api.closeSession(sessionId).catch(() => {});
      setSession(null);
      setView("main");
    }
  }, [locked, settings.lock_disconnect, sessionId]);
  // Everything under the lock screen is inert: no focus, clicks or keys.
  useEffect(() => {
    const root = document.getElementById("root");
    if (root) root.inert = locked;
  }, [locked]);
  const lockScreen =
    locked &&
    createPortal(
      <LockScreen
        askNow={lockedAtStart}
        onUnlocked={() => {
          setLocked(false);
          setLockedAtStart(false);
          // Snippets can't be read while locked, so a start behind the lock has none yet.
          reloadSnippets().catch(() => {});
        }}
      />,
      document.body,
    );

  // Risky statements waiting for the user's go-ahead.
  const [risk, setRisk] = useState<{ findings: Finding[]; answer: (run: boolean) => void } | null>(null);

  const confirmRisk = async (text: string) => {
    if (!settings.confirm_dangerous) return true;
    const found = isRedis ? api.redisCheck(text) : api.checkSql(text);
    const dangers = (await found.catch(() => [] as Finding[])).filter((f) => f.level === "danger");
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

  /** Runs Redis command lines; none of the SQL extras (LIMIT, Explain…) apply. */
  const runRedis = async (text: string, record: boolean) => {
    if (!session || !tab) return;
    const key = tab.key;
    setRunning(true);
    try {
      const result = await api.redisExecute(session.session_id, text, record);
      sessionStatus.record(result);
      tabs.update(key, { redisOutcome: result });
      setRedisDb(result.db);
      setRedisView("result");
      setNotice(
        result.reconnected ? { text: `指令列的連線曾中斷，已重新連線。${CONNECTION_STATE_RESET}`, kind: "info" } : null,
      );
    } catch (e) {
      sessionStatus.recordError(e);
      setNotice({ text: describeError(e), kind: "error" });
    } finally {
      setRunning(false);
    }
  };

  const run = async (text: string, record = tab?.recording ?? false, noLimit = false) => {
    if (!session || !tab || running || !text.trim()) return;
    if (!(await confirmRisk(text))) return;
    if (isRedis) return runRedis(text, record);
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
      sessionStatus.record(result, text);
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
      sessionStatus.recordError(e);
      setNotice({ text: describeError(e), kind: "error" });
    } finally {
      setRunning(false);
    }
  };

  /**
   * Runs what ▶ / ⌘Enter targets. A selection that starts mid-statement
   * (say from a value line of a multi-line INSERT) would only fail with a
   * syntax error, so offer the whole statement instead.
   */
  const runEditorTarget = async (t: RunTarget | null | undefined) => {
    if (!t) return;
    if (t.whole === undefined) return run(t.text);
    const head = t.text.trim().split("\n")[0].slice(0, 40);
    const ok = await ask(
      `選取的內容（開頭是「${head}」）只是一句 SQL 的中間片段，單獨執行會出現語法錯誤。\n\n要改成執行它所在的整句嗎？`,
      "執行整句",
    );
    if (ok) return run(t.whole);
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

  /** Points the Redis command editor at database `db`. */
  const selectRedisDb = async (db: number) => {
    if (!session || running) return;
    try {
      const result = await api.redisExecute(session.session_id, `SELECT ${db}`, false);
      if (result.error) throw result.error;
      setRedisDb(result.db);
    } catch (e) {
      setNotice({ text: describeError(e), kind: "error" });
    }
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
    if (!session || isRedis) return;
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
  }, [session, isRedis]);

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
          lockAvailable={lockAvailable}
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
        {lockScreen}
      </div>
    );
  }

  const databaseNames = (schema.databases ?? []).map((d) => d.name);
  const runLabel = isRedis
    ? runTarget && runTarget.count > 1
      ? { text: `執行 ${runTarget.count} 行`, hint: "依序執行選取的指令" }
      : { text: "執行這一行", hint: "執行游標所在的那一行指令" }
    : runTarget?.kind === "all"
      ? { text: `執行全部（${runTarget.count} 句）`, hint: "執行全部" }
      : runTarget && runTarget.count > 1
        ? { text: `執行多行（${runTarget.count} 句）`, hint: "執行選取的部分" }
        : runTarget?.kind === "selection"
          ? { text: "執行選取", hint: "執行選取的部分" }
          : { text: "執行單行", hint: "執行游標所在的那一句（淡色底的那段）" };

  return (
    <div className="app with-status">
      {session && active && (
        <aside className="sidebar" style={sidebarWidth ? { width: sidebarWidth } : undefined}>
          <Splitter
            dir="col"
            className="sidebar-splitter"
            onDrag={(x) => setSidebarWidth(Math.min(Math.max(x, 160), window.innerWidth * 0.6))}
            onReset={() => setSidebarWidth(null)}
          />
          <div className="sidebar-head">
            <button className="small ghost" onClick={() => disconnect()} title="中斷連線並回到首頁">
              ← 所有連線
            </button>
            <span className={`badge kind-${session.kind}`}>{KIND_LABEL[session.kind]}</span>
          </div>
          {isRedis ? (
            <RedisKeyTree
              key={session.session_id}
              sessionId={session.session_id}
              currentDb={redisDb}
              openKey={openKey}
              onOpenKey={(db, key) => {
                setOpenKey({ db, key });
                setRedisView("key");
              }}
              onSelectDb={selectRedisDb}
            />
          ) : (
          <SchemaTree
            key={session.session_id}
            schema={schema}
            currentDb={currentDb}
            showSystem={settings.show_system_databases}
            onShowSystemChange={(show) => updateSettings({ ...settings, show_system_databases: show })}
            onUseDatabase={switchDatabase}
            onOpenTable={openTable}
            onManage={(database, table) => {
              setSchemaTarget({ database, table, nonce: Date.now() });
              setView("schema");
            }}
          />
          )}
        </aside>
      )}

      <main className="workspace">
        {session && active && (
          <header className="topbar">
            <strong>{active.name}</strong>
            {isRedis ? (
              <label className="db-picker" title="指令列使用的資料庫（SELECT）。左側雙擊資料庫也可以切換。">
                資料庫
                <select value={redisDb} disabled={running} onChange={(e) => selectRedisDb(Number(e.target.value))}>
                  {Array.from({ length: 16 }, (_, i) => (
                    <option key={i} value={i}>
                      db{i}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
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
            )}
            <span className="spacer" />
            {!isRedis && (
              <button
                className={view === "schema" ? "active" : undefined}
                onClick={() => setView(view === "schema" ? "main" : "schema")}
                title="資料庫、資料表結構與索引、檢視、預存程序、觸發器、事件"
              >
                🗂 結構
              </button>
            )}
            <button
              className={view === "server" ? "active" : undefined}
              onClick={() => setView(view === "server" ? "main" : "server")}
              title={isRedis ? "伺服器狀態、用戶端、慢查詢、指令統計與設定" : "伺服器狀態、執行中的查詢、使用者與權限"}
            >
              🖥 伺服器
            </button>
            {view === "server" || view === "schema" ? (
              <button className="primary" onClick={() => setView("main")}>
                ← 回到編輯器
              </button>
            ) : (
              <>
                <label
                  className={tab?.recording ? "record-toggle on" : "record-toggle"}
                  title={
                    recordForced
                      ? `這個連線設定為強制紀錄：每次執行的${isRedis ? "指令" : " SQL "}都會存進「紀錄」，不能關閉（在連線設定裡修改）`
                      : `打開後，這個分頁每次執行的${isRedis ? "指令" : " SQL "}與統計都會存進「紀錄」（釘選的分頁預設打開）`
                  }
                >
                  <input
                    type="checkbox"
                    checked={!!tab?.recording}
                    disabled={recordForced}
                    onChange={(e) => tab && tabs.update(tab.key, { recording: e.target.checked })}
                  />
                  📝 紀錄{recordForced && "（強制）"}
                </label>
                <button onClick={() => setView("records")}>紀錄清單</button>
                {!isRedis && (
                <button onClick={() => (setSnippetDraft(null), setDialog("snippets"))} title="Snippet：常用 SQL 範本">
                  {"{ }"}
                </button>
                )}
                <button onClick={() => (setSettingsTab(undefined), setDialog("settings"))} title="設定">
                  ⚙
                </button>
                {!isRedis && (
                <>
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
                </>
                )}
                {(runTarget?.total ?? 0) > 1 && runTarget?.kind !== "all" && (
                  <button
                    disabled={running}
                    onClick={() => run(editor.current?.allText()?.text ?? "")}
                    title={`依序執行編輯器裡的每一${isRedis ? "行指令" : "句"}（⌘／Ctrl + Shift + Enter）`}
                  >
                    全部（{runTarget?.total} {isRedis ? "行" : "句"}）
                  </button>
                )}
                {running && (
                  <button className="danger" onClick={stop} title={`停止執行中的${isRedis ? "指令" : "查詢"}（⌘／Ctrl + .）`}>
                    ■ 停止
                  </button>
                )}
                <button
                  className="primary"
                  disabled={running || runTarget?.count === 0}
                  onClick={() => runEditorTarget(editor.current?.runTarget())}
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

        {session && view === "server" && isRedis && (
          <RedisServerPage key={session.session_id} sessionId={session.session_id} confirmRisk={confirmRisk} />
        )}
        {session && view === "server" && !isRedis && (
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
        {session && view === "schema" && !isRedis && (
          <SchemaPage
            key={session.session_id}
            sessionId={session.session_id}
            mariadb={isMaria}
            databases={schema.databases}
            showSystem={settings.show_system_databases}
            currentDb={currentDb}
            target={schemaTarget}
            theme={editorTheme(settings.editor_theme, systemDark)}
            fontSize={settings.editor_font_size}
            onChanged={(db) => {
              setCurrentDb(db);
              schema.reload();
            }}
            onOpenSql={(sql) => {
              setView("main");
              tabs.add(sql);
            }}
          />
        )}
        {session ? (
          <div
            className="split"
            hidden={view === "server" || view === "schema"}
            ref={splitRef}
            style={editorHeight ? { gridTemplateRows: `${editorHeight}px minmax(0, 1fr)` } : undefined}
          >
            <Splitter
              dir="row"
              className="editor-splitter"
              style={editorHeight ? { top: editorHeight } : undefined}
              onDrag={(y) => {
                const box = splitRef.current?.getBoundingClientRect();
                if (box) setEditorHeight(Math.min(Math.max(y - box.top, 80), box.height - 80));
              }}
              onReset={() => setEditorHeight(null)}
            />
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
                          label: recordForced
                            ? "這個連線強制紀錄"
                            : t.recording
                              ? "停止記錄這個分頁"
                              : "記錄這個分頁的執行",
                          disabled: recordForced,
                          onClick: () => tabs.update(t.key, { recording: !t.recording }),
                        },
                        {
                          label: isRedis ? "複製分頁內的指令" : "複製分頁內的 SQL",
                          disabled: !t.sql.trim(),
                          onClick: () => copyText(t.sql),
                        },
                        ...(isRedis
                          ? []
                          : [
                              "sep" as const,
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
                            ]),
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
                    <span className="tab-title">{tabTitle(t.sql, `${isRedis ? "指令" : "查詢"} ${i + 1}`, t.filePath, isRedis)}</span>
                    {t.filePath && t.savedSql != null && t.sql !== t.savedSql && (
                      <span className="tab-dirty" title="有修改還沒存檔（⌘S）">
                        •
                      </span>
                    )}
                    {t.recording && (
                      <span className="tab-rec" title={recordForced ? "這個連線強制紀錄執行" : "這個分頁會記錄執行"}>
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
              {isRedis && tab && tab.sql.includes("****") && (
                <div className="editor-hint">密碼已遮蔽，請重新輸入後再執行。</div>
              )}
              {tabs.tabs.map((t) =>
                isRedis ? (
                  <RedisEditor
                    key={`${session.session_id}:${t.key}:${t.seed}`}
                    active={t.key === tab?.key}
                    initialValue={t.sql}
                    theme={editorTheme(settings.editor_theme, systemDark)}
                    fontSize={settings.editor_font_size}
                    handle={t.key === tab?.key ? editor : undefined}
                    onRunTargetChange={setRunTarget}
                    onChange={(value) => tabs.setSql(t.key, value)}
                    onRun={run}
                  />
                ) : (
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
                  onRunTarget={runEditorTarget}
                />
                ),
              )}
            </div>
            {isRedis ? (
              <RedisPanel
                sessionId={session.session_id}
                outcome={tab?.redisOutcome ?? null}
                openKey={openKey}
                view={redisView}
                onViewChange={setRedisView}
              />
            ) : (
            <ResultsPanel
              outcome={tab?.outcome ?? null}
              onRunWithoutLimit={(sql) => run(sql, undefined, true)}
              sessionId={session.session_id}
              onAskAi={askAi}
            />
            )}
          </div>
        ) : (
          <Home
            connections={connections}
            connecting={connecting}
            doubleClick={settings.connect_on_double_click}
            onConnect={connect}
            onEdit={(c) =>
              // The list has no passwords; this reads them (from the keychain).
              api.getConnection(c.id).then(setEditing, (e) => setNotice({ text: describeError(e), kind: "error" }))
            }
            onNew={() => setEditing({ ...newProfile(), kind: undefined })}
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
            onExport={() => setTransfer({ kind: "export" })}
            onImport={importConnections}
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
      <StatusBar
        session={session}
        profile={active}
        database={isRedis ? `db${redisDb}` : currentDb}
        running={running}
        outcome={(isRedis ? tab?.redisOutcome : tab?.outcome) ?? null}
        status={sessionStatus.status}
        aiEnabled={settings.ai_enabled}
        problem={lastError}
        onLock={lockOn ? lockNow : undefined}
        onOpenProblem={() => {
          if (!lastError) return;
          setBug({ reason: lastError.reason, details: lastError.details });
          setLastError(null);
          api.clearCrashInfo().catch(() => {});
        }}
      />
      {bug && <BugReportDialog info={bug} onClose={() => setBug(null)} />}
      {lockScreen}
      {dialogs}
      {risk && (
        <RiskDialog
          findings={risk.findings}
          connectionName={active?.name ?? ""}
          intro={isRedis ? "這些指令可能會刪除資料、拖慢伺服器或影響其他用戶端。" : undefined}
          onAnswer={risk.answer}
        />
      )}
      {transfer?.kind === "export" && (
        <ExportDialog
          connections={connections}
          onClose={() => setTransfer(null)}
          onDone={(text) => {
            setTransfer(null);
            setNotice({ text, kind: "info" });
          }}
        />
      )}
      {transfer?.kind === "import" && (
        <ImportDialog
          preview={transfer.preview}
          onClose={() => setTransfer(null)}
          onImported={() => reload().catch(() => {})}
          onEdit={(c) =>
            api.getConnection(c.id).then(setEditing, (e) => setNotice({ text: describeError(e), kind: "error" }))
          }
        />
      )}
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
