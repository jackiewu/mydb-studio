import { useCallback, useEffect, useRef, useState } from "react";
import { api, type ExecutionOutcome, type PinnedTab } from "./api";
import { statementRanges } from "./editor/statements";

/** An editor tab. Pinned tabs are saved per connection and come back with it. */
export interface EditorTab {
  key: string;
  /** Row id in the store once pinned. */
  pinnedId: number | null;
  sql: string;
  /** Run results are stored only while this is on. */
  recording: boolean;
  outcome: ExecutionOutcome | null;
  /** Bumped to replace the editor text with `sql`. */
  seed: number;
}

let nextKey = 1;
const newKey = () => `tab-${nextKey++}`;

/** A short name for a tab: the start of its first statement. */
export function tabTitle(sql: string, fallback: string) {
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
 * Editor tabs for one connection. Unpinned tabs live only in memory;
 * pinned ones are written to the store (debounced while typing) until
 * they are closed or unpinned.
 */
export function useTabs(connectionId: string | null, recordByDefault: boolean) {
  const [tabs, setTabs] = useState<EditorTab[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const pending = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  // Latest tabs for the save timers, which outlive renders.
  const latest = useRef(tabs);
  latest.current = tabs;

  const blank = useCallback(
    (sql = ""): EditorTab => ({
      key: newKey(),
      pinnedId: null,
      sql,
      recording: recordByDefault,
      outcome: null,
      seed: 0,
    }),
    [recordByDefault],
  );

  const persist = useCallback(
    async (key: string) => {
      const list = latest.current;
      const index = list.findIndex((t) => t.key === key);
      const tab = list[index];
      if (!tab || tab.pinnedId == null || !connectionId) return;
      await api.savePinnedTab({
        id: tab.pinnedId,
        connection_id: connectionId,
        title: tabTitle(tab.sql, `查詢 ${index + 1}`),
        sql: tab.sql,
        position: index,
      });
    },
    [connectionId],
  );

  const flush = useCallback(() => {
    for (const [key, timer] of pending.current) {
      clearTimeout(timer);
      persist(key).catch(() => {});
    }
    pending.current.clear();
  }, [persist]);

  // Open the connection's pinned tabs, or one empty tab.
  useEffect(() => {
    let cancelled = false;
    setTabs([]);
    setActiveKey(null);
    if (!connectionId) return;
    api
      .listPinnedTabs(connectionId)
      .catch(() => [] as PinnedTab[])
      .then((pinned) => {
        if (cancelled) return;
        const restored = pinned.map((p) => ({
          key: newKey(),
          pinnedId: p.id,
          sql: p.sql,
          recording: true,
          outcome: null,
          seed: 0,
        }));
        const list = restored.length ? restored : [blank("SELECT VERSION();")];
        setTabs(list);
        setActiveKey(list[0].key);
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
      if (!latest.current.find((t) => t.key === key)?.pinnedId) return;
      clearTimeout(pending.current.get(key));
      pending.current.set(
        key,
        setTimeout(() => {
          pending.current.delete(key);
          persist(key).catch(() => {});
        }, SAVE_DELAY_MS),
      );
    },
    [update, persist],
  );

  const add = useCallback(
    (sql = "") => {
      const tab = blank(sql);
      setTabs((list) => [...list, tab]);
      setActiveKey(tab.key);
      return tab.key;
    },
    [blank],
  );

  const close = useCallback(
    async (key: string) => {
      const list = latest.current;
      const index = list.findIndex((t) => t.key === key);
      const tab = list[index];
      if (!tab) return;
      if (tab.pinnedId != null) {
        if (!confirm("關閉釘選的分頁？內容不會再保留。")) return;
        clearTimeout(pending.current.get(key));
        pending.current.delete(key);
        await api.deletePinnedTab(tab.pinnedId).catch(() => {});
      }
      const rest = list.filter((t) => t.key !== key);
      const next = rest.length ? rest : [blank()];
      setTabs(next);
      setActiveKey((current) =>
        current === key ? next[Math.min(index, next.length - 1)].key : current,
      );
    },
    [blank],
  );

  const togglePin = useCallback(
    async (key: string) => {
      const list = latest.current;
      const index = list.findIndex((t) => t.key === key);
      const tab = list[index];
      if (!tab || !connectionId) return;
      if (tab.pinnedId != null) {
        await api.deletePinnedTab(tab.pinnedId).catch(() => {});
        update(key, { pinnedId: null });
        return;
      }
      const saved = await api.savePinnedTab({
        id: 0,
        connection_id: connectionId,
        title: tabTitle(tab.sql, `查詢 ${index + 1}`),
        sql: tab.sql,
        position: index,
      });
      // Pinned tabs record their runs by default.
      update(key, { pinnedId: saved.id, recording: true });
    },
    [connectionId, update],
  );

  /** Closes every unpinned tab except `key`. */
  const closeOthers = useCallback((key: string) => {
    setTabs((list) => list.filter((t) => t.key === key || t.pinnedId != null));
    setActiveKey(key);
  }, []);

  /** Puts `sql` in a tab's editor, replacing what was there. */
  const replaceSql = useCallback(
    (key: string, sql: string) => {
      setSql(key, sql);
      setTabs((list) => list.map((t) => (t.key === key ? { ...t, seed: t.seed + 1 } : t)));
    },
    [setSql],
  );

  const active = tabs.find((t) => t.key === activeKey) ?? null;
  return { tabs, active, setActive: setActiveKey, add, close, closeOthers, togglePin, setSql, replaceSql, update, flush };
}
