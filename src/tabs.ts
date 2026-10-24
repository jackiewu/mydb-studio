import { useCallback, useEffect, useRef, useState } from "react";
import { ask } from "./ask";
import { api, type ExecutionOutcome, type PinnedTab } from "./api";
import { statementRanges } from "./editor/statements";

/**
 * An editor tab. Every tab with text is kept in the store while open, so a
 * crash doesn't lose it: pinned tabs for good, unpinned ones as drafts that
 * are dropped once the user closes them.
 */
export interface EditorTab {
  key: string;
  /** Row id in the store once saved there. */
  rowId: number | null;
  pinned: boolean;
  /** The .sql file the tab was opened from or saved to. */
  filePath: string | null;
  /** The text last written to (or read from) `filePath`. */
  savedSql: string | null;
  sql: string;
  /** Run results are stored only while this is on. */
  recording: boolean;
  outcome: ExecutionOutcome | null;
  /** Bumped to replace the editor text with `sql`. */
  seed: number;
}

/** Text that only lives in this tab: not pinned and not saved to its file. */
export function isUnsaved(t: EditorTab) {
  return !t.pinned && t.sql.trim() !== "" && (t.filePath == null || t.sql !== t.savedSql);
}

/** The file name of a path. */
export function fileName(path: string) {
  return path.split(/[\\/]/).pop() ?? path;
}

let nextKey = 1;
const newKey = () => `tab-${nextKey++}`;

/** A short name for a tab: its file, or the start of its first statement. */
export function tabTitle(sql: string, fallback: string, filePath?: string | null) {
  if (filePath) return fileName(filePath);
  const first = statementRanges(sql)[0];
  if (!first) return fallback;
  const code = sql
    .slice(first.from, first.to)
    .replace(/\/\*[\s\S]*?\*\/|(--\s|#).*$/gm, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!code) return fallback;
  return code.length > 24 ? code.slice(0, 23) + "…" : code;
}

const SAVE_DELAY_MS = 500;

/**
 * Editor tabs for one connection. Each change is written to the store
 * (debounced while typing) so tabs survive a crash; see {@link EditorTab}.
 */
export function useTabs(connectionId: string | null, recordByDefault: boolean) {
  const [tabs, setTabs] = useState<EditorTab[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  /** How many unsaved tabs came back from last time (after a crash). */
  const [restored, setRestored] = useState(0);
  const pending = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  // One save at a time per tab, so a new tab isn't inserted twice.
  const saving = useRef(new Map<string, Promise<void>>());
  // Latest tabs for the save timers, which outlive renders.
  const latest = useRef(tabs);
  latest.current = tabs;

  const blank = useCallback(
    (sql = "", filePath: string | null = null): EditorTab => ({
      key: newKey(),
      rowId: null,
      pinned: false,
      filePath,
      savedSql: filePath ? sql : null,
      sql,
      recording: recordByDefault,
      outcome: null,
      seed: 0,
    }),
    [recordByDefault],
  );

  const saveNow = useCallback(
    async (key: string) => {
      const list = latest.current;
      const index = list.findIndex((t) => t.key === key);
      const tab = list[index];
      if (!tab || !connectionId) return;
      if (tab.rowId == null && !tab.pinned && !tab.sql.trim()) return;
      const saved = await api.savePinnedTab({
        id: tab.rowId ?? 0,
        connection_id: connectionId,
        title: tabTitle(tab.sql, `查詢 ${index + 1}`, tab.filePath),
        sql: tab.sql,
        position: index,
        pinned: tab.pinned,
        file_path: tab.filePath,
      });
      if (tab.rowId == null) {
        setTabs((l) => l.map((t) => (t.key === key ? { ...t, rowId: saved.id } : t)));
        // The tab may have closed while it was being saved.
        if (!latest.current.some((t) => t.key === key)) await api.deletePinnedTab(saved.id).catch(() => {});
        latest.current = latest.current.map((t) => (t.key === key ? { ...t, rowId: saved.id } : t));
      }
    },
    [connectionId],
  );

  const persist = useCallback(
    (key: string) => {
      const next = (saving.current.get(key) ?? Promise.resolve())
        .then(() => saveNow(key))
        .catch(() => {});
      saving.current.set(key, next);
      return next;
    },
    [saveNow],
  );

  const schedule = useCallback(
    (key: string) => {
      clearTimeout(pending.current.get(key));
      pending.current.set(
        key,
        setTimeout(() => {
          pending.current.delete(key);
          persist(key);
        }, SAVE_DELAY_MS),
      );
    },
    [persist],
  );

  const flush = useCallback(() => {
    for (const [key, timer] of pending.current) {
      clearTimeout(timer);
      persist(key);
    }
    pending.current.clear();
  }, [persist]);

  // Open the connection's saved tabs (pinned, and drafts left by a crash),
  // or one starter tab.
  useEffect(() => {
    let cancelled = false;
    setTabs([]);
    setActiveKey(null);
    setRestored(0);
    if (!connectionId) return;
    api
      .listPinnedTabs(connectionId)
      .catch(() => [] as PinnedTab[])
      .then((saved) => {
        if (cancelled) return;
        const list: EditorTab[] = saved.map((p) => ({
          key: newKey(),
          rowId: p.id,
          pinned: p.pinned,
          filePath: p.file_path ?? null,
          savedSql: null,
          sql: p.sql,
          recording: p.pinned || recordByDefault,
          outcome: null,
          seed: 0,
        }));
        const shown = list.length ? list : [blank("SELECT VERSION();")];
        setTabs(shown);
        setActiveKey(shown[0].key);
        setRestored(list.filter((t) => !t.pinned).length);
      });
    return () => {
      cancelled = true;
      flush();
    };
    // `blank` only carries the record default; reloading tabs when the
    // setting changes would drop unpinned work.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId]);

  const update = useCallback((key: string, patch: Partial<EditorTab>) => {
    setTabs((list) => list.map((t) => (t.key === key ? { ...t, ...patch } : t)));
  }, []);

  const setSql = useCallback(
    (key: string, sql: string) => {
      update(key, { sql });
      latest.current = latest.current.map((t) => (t.key === key ? { ...t, sql } : t));
      schedule(key);
    },
    [update, schedule],
  );

  const add = useCallback(
    (sql = "", filePath: string | null = null) => {
      const tab = blank(sql, filePath);
      setTabs((list) => [...list, tab]);
      latest.current = [...latest.current, tab];
      setActiveKey(tab.key);
      if (sql.trim()) persist(tab.key);
      return tab.key;
    },
    [blank, persist],
  );

  const drop = useCallback(async (tab: EditorTab) => {
    clearTimeout(pending.current.get(tab.key));
    pending.current.delete(tab.key);
    await saving.current.get(tab.key);
    saving.current.delete(tab.key);
    const rowId = latest.current.find((t) => t.key === tab.key)?.rowId ?? tab.rowId;
    if (rowId != null) await api.deletePinnedTab(rowId).catch(() => {});
  }, []);

  const close = useCallback(
    async (key: string) => {
      const list = latest.current;
      const index = list.findIndex((t) => t.key === key);
      const tab = list[index];
      if (!tab) return;
      if (tab.pinned && !(await ask("關閉釘選的分頁？內容不會再保留。", "關閉"))) return;
      if (isUnsaved(tab) && !(await ask("這個分頁還沒存檔，關閉後內容會消失。確定關閉？", "關閉"))) return;
      const rest = list.filter((t) => t.key !== key);
      const next = rest.length ? rest : [blank()];
      latest.current = next;
      setTabs(next);
      setActiveKey((current) =>
        current === key ? next[Math.min(index, next.length - 1)].key : current,
      );
      await drop(tab);
    },
    [blank, drop],
  );

  const togglePin = useCallback(
    async (key: string) => {
      const tab = latest.current.find((t) => t.key === key);
      if (!tab) return;
      // Pinned tabs record their runs by default.
      const patch = tab.pinned ? { pinned: false } : { pinned: true, recording: true };
      update(key, patch);
      latest.current = latest.current.map((t) => (t.key === key ? { ...t, ...patch } : t));
      await persist(key);
    },
    [update, persist],
  );

  /** Closes every unpinned tab except `key`. */
  const closeOthers = useCallback(
    async (key: string) => {
      const gone = latest.current.filter((t) => t.key !== key && !t.pinned);
      const unsaved = gone.filter(isUnsaved).length;
      if (unsaved && !(await ask(`其中 ${unsaved} 個分頁還沒存檔，關閉後內容會消失。確定關閉？`, "關閉"))) return;
      latest.current = latest.current.filter((t) => t.key === key || t.pinned);
      setTabs(latest.current);
      setActiveKey(key);
      await Promise.all(gone.map(drop));
    },
    [drop],
  );

  /** Puts `sql` in a tab's editor, replacing what was there. */
  const replaceSql = useCallback(
    (key: string, sql: string) => {
      setSql(key, sql);
      setTabs((list) => list.map((t) => (t.key === key ? { ...t, seed: t.seed + 1 } : t)));
    },
    [setSql],
  );

  /** Records that a tab's text was written to `filePath`. */
  const markSaved = useCallback(
    (key: string, filePath: string, sql: string) => {
      update(key, { filePath, savedSql: sql });
      latest.current = latest.current.map((t) => (t.key === key ? { ...t, filePath, savedSql: sql } : t));
      persist(key);
    },
    [update, persist],
  );

  /** Lets go of unpinned tabs for good, once the user agreed to leave them. */
  const discardDrafts = useCallback(async () => {
    for (const timer of pending.current.values()) clearTimeout(timer);
    pending.current.clear();
    await Promise.all(saving.current.values());
    if (connectionId) await api.deleteDraftTabs(connectionId).catch(() => {});
  }, [connectionId]);

  const active = tabs.find((t) => t.key === activeKey) ?? null;
  return {
    tabs,
    active,
    restored,
    setActive: setActiveKey,
    add,
    close,
    closeOthers,
    togglePin,
    setSql,
    replaceSql,
    markSaved,
    update,
    flush,
    discardDrafts,
  };
}
