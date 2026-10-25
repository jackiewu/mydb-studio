import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, describeError, type DbInfo, type KeyEntry, type KeyName } from "../api";
import { openMenu } from "../contextMenu";
import { copyText } from "../copy";
import { buildKeyTree, ttlText, type KeyTreeNode } from "../redis/keyTree";

/** Short labels for key types. */
export const TYPE_LABEL: Record<string, string> = {
  string: "STR",
  hash: "HASH",
  list: "LIST",
  set: "SET",
  zset: "ZSET",
  stream: "STRM",
  none: "—",
};

/** Keys loaded so far for one database. */
interface DbKeys {
  keys: KeyEntry[];
  /** "0" once the scan has seen every key. */
  cursor: string;
  loading: boolean;
  error: string | null;
}

/** A MATCH pattern from what was typed: as is with wildcards, else "contains". */
export function matchPattern(text: string) {
  const t = text.trim();
  if (!t) return "";
  return /[*?[]/.test(t) ? t : `*${t}*`;
}

interface Props {
  sessionId: string;
  /** Database the command editor uses (SELECT). */
  currentDb: number;
  /** The key shown in the value view. */
  openKey: { db: number; key: KeyName } | null;
  onOpenKey: (db: number, key: KeyName) => void;
  onSelectDb: (db: number) => void;
}

/**
 * db0–db15, each opening to its keys grouped by colon prefixes. Keys come
 * from SCAN, about 500 at a time, with "load more" for the rest; KEYS is
 * never used.
 */
export function RedisKeyTree({ sessionId, currentDb, openKey, onOpenKey, onSelectDb }: Props) {
  const [dbs, setDbs] = useState<DbInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filterText, setFilterText] = useState("");
  const [pattern, setPattern] = useState("");
  const [openDbs, setOpenDbs] = useState<Set<number>>(new Set([currentDb]));
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [loaded, setLoaded] = useState<Record<number, DbKeys>>({});

  const loadDbs = useCallback(() => {
    setError(null);
    api.redisDatabases(sessionId).then(setDbs, (e) => setError(describeError(e)));
  }, [sessionId]);

  useEffect(loadDbs, [loadDbs]);

  // Latest keys for loadKeys, and a counter so a scan started before the
  // filter changed can't land in the new list.
  const loadedRef = useRef(loaded);
  loadedRef.current = loaded;
  const generation = useRef(0);

  const loadKeys = useCallback(
    async (db: number, more: boolean, pat: string) => {
      const gen = generation.current;
      const prev = more ? loadedRef.current[db] : undefined;
      const cursor = prev?.cursor ?? "0";
      const set = (v: DbKeys) => {
        if (gen !== generation.current) return;
        loadedRef.current = { ...loadedRef.current, [db]: v };
        setLoaded(loadedRef.current);
      };
      set({ keys: prev?.keys ?? [], cursor, loading: true, error: null });
      try {
        const page = await api.redisScan(sessionId, db, cursor, pat);
        const before = prev?.keys ?? [];
        const seen = new Set(before.map((k) => `${k.binary ? 1 : 0}:${k.key}`));
        const fresh = page.keys.filter((k) => !seen.has(`${k.binary ? 1 : 0}:${k.key}`));
        set({ keys: [...before, ...fresh], cursor: page.cursor, loading: false, error: null });
      } catch (e) {
        set({ keys: prev?.keys ?? [], cursor, loading: false, error: describeError(e) });
      }
    },
    [sessionId],
  );

  const resetKeys = () => {
    generation.current++;
    loadedRef.current = {};
    setLoaded({});
  };

  // Open databases (re)load when the filter changes.
  const openKeyList = [...openDbs].join(",");
  useEffect(() => {
    resetKeys();
    for (const db of openKeyList ? openKeyList.split(",").map(Number) : []) loadKeys(db, false, pattern);
    // Only a new pattern reloads everything; opening a database loads it below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pattern, loadKeys]);

  const toggleDb = (db: number) => {
    const next = new Set(openDbs);
    if (next.has(db)) next.delete(db);
    else {
      next.add(db);
      if (!loadedRef.current[db]) loadKeys(db, false, pattern);
    }
    setOpenDbs(next);
  };

  const toggle = (path: string) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  const reload = () => {
    loadDbs();
    resetKeys();
    for (const db of openDbs) loadKeys(db, false, pattern);
  };

  const trees = useMemo(() => {
    const out: Record<number, KeyTreeNode[]> = {};
    for (const [db, v] of Object.entries(loaded)) out[Number(db)] = buildKeyTree(v.keys);
    return out;
  }, [loaded]);

  const renderNodes = (db: number, nodes: KeyTreeNode[]) => (
    <ul>
      {nodes.map((n) => {
        const folder = n.children.length > 0;
        const isOpen = open.has(`${db}/${n.path}`);
        const k = n.key;
        const selected = !!k && openKey?.db === db && openKey.key.key === k.key && !!openKey.key.binary === !!k.binary;
        return (
          <li key={n.path + (folder ? "/" : "")}>
            <button
              className={["tree-table", "redis-key", selected ? "selected" : ""].join(" ")}
              title={k ? `${k.key}\n${k.kind} · TTL ${ttlText(k.ttl)}` : `${n.path}*（${n.count} 個 key）`}
              onClick={() => (folder ? toggle(`${db}/${n.path}`) : k && onOpenKey(db, k))}
              onDoubleClick={() => folder && k && onOpenKey(db, k)}
              onContextMenu={(e) =>
                openMenu(e, [
                  ...(k ? [{ label: "檢視內容", onClick: () => onOpenKey(db, k) }] : []),
                  ...(folder ? [{ label: isOpen ? "收合" : "展開", onClick: () => toggle(`${db}/${n.path}`) }] : []),
                  "sep",
                  ...(k ? [{ label: "複製 key 名稱", onClick: () => copyText(k.key) }] : []),
                  ...(folder
                    ? [
                        { label: `複製前綴（${n.path}）`, onClick: () => copyText(n.path) },
                        {
                          label: `只看 ${n.path}*`,
                          onClick: () => {
                            setFilterText(`${n.path}*`);
                            setPattern(`${n.path}*`);
                          },
                        },
                      ]
                    : []),
                ])
              }
            >
              <span className="caret">{folder ? (isOpen ? "▾" : "▸") : ""}</span>
              <span className="tree-name">{n.name === "" ? <em className="muted">（空）</em> : n.name}</span>
              {k && <span className={`key-type t-${k.kind}`}>{TYPE_LABEL[k.kind] ?? k.kind}</span>}
              {k && k.ttl >= 0 && <span className="count">{ttlText(k.ttl)}</span>}
              {folder && <span className="count">{n.count}</span>}
            </button>
            {folder && isOpen && renderNodes(db, n.children)}
          </li>
        );
      })}
    </ul>
  );

  const shown = dbs ?? [];
  return (
    <div className="schema">
      <div className="sidebar-head">
        <span>
          Key {pattern && <small className="muted">（{pattern}）</small>}
        </span>
        <button className="small ghost" title="重新整理" onClick={reload}>
          ⟳
        </button>
      </div>
      <div className="schema-filter">
        <input
          type="search"
          placeholder="篩選 key（MATCH，例如 user:*），Enter 套用"
          value={filterText}
          spellCheck={false}
          onChange={(e) => {
            setFilterText(e.target.value);
            if (!e.target.value) setPattern("");
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") setPattern(matchPattern(filterText));
            if (e.key === "Escape") (setFilterText(""), setPattern(""));
          }}
        />
      </div>
      {error && <div className="schema-error">{error}</div>}
      {!dbs && !error && <div className="hint pad">載入中…</div>}
      <ul className="tree">
        {shown.map((d) => {
          const isOpen = openDbs.has(d.index);
          const isCurrent = d.index === currentDb;
          const keys = loaded[d.index];
          return (
            <li key={d.index}>
              <button
                className={isCurrent ? "tree-db current" : "tree-db"}
                title={isCurrent ? "指令列目前使用的資料庫" : "雙擊讓指令列切換到這個資料庫（SELECT）"}
                onClick={(e) => e.detail <= 1 && toggleDb(d.index)}
                onDoubleClick={() => onSelectDb(d.index)}
                onContextMenu={(e) =>
                  openMenu(e, [
                    { label: `指令列使用 db${d.index}（SELECT）`, disabled: isCurrent, onClick: () => onSelectDb(d.index) },
                    { label: isOpen ? "收合" : "展開", onClick: () => toggleDb(d.index) },
                    "sep",
                    { label: "重新整理", onClick: () => loadKeys(d.index, false, pattern) },
                  ])
                }
              >
                <span className="caret">{isOpen ? "▾" : "▸"}</span>
                <span className="tree-name">db{d.index}</span>
                {isCurrent && <span className="current-mark">SELECT</span>}
                <span className="count">{d.keys.toLocaleString()}</span>
              </button>
              {isOpen && keys && (
                <>
                  {keys.keys.length > 0 && renderNodes(d.index, trees[d.index] ?? [])}
                  {!keys.loading && keys.keys.length === 0 && !keys.error && (
                    <div className="hint pad">{pattern ? `沒有符合 ${pattern} 的 key` : "沒有 key"}</div>
                  )}
                  {keys.error && <div className="schema-error">{keys.error}</div>}
                  {keys.loading && <div className="hint pad">載入中…</div>}
                  {!keys.loading && keys.cursor !== "0" && (
                    <button className="link small-text load-more" onClick={() => loadKeys(d.index, true, pattern)}>
                      載入更多（已載入 {keys.keys.length.toLocaleString()} 個）
                    </button>
                  )}
                </>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
