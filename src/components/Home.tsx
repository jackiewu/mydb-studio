import { useState } from "react";
import { KIND_LABEL, serverLabel, type ConnectionKind, type ConnectionProfile } from "../api";
import { openMenu } from "../contextMenu";
import { copyText } from "../copy";
import { homeSections } from "../home/sections";

interface Props {
  connections: ConnectionProfile[];
  connecting: string | null;
  /** A click selects a card and a double-click connects; else a click connects. */
  doubleClick: boolean;
  onConnect: (c: ConnectionProfile) => void;
  onEdit: (c: ConnectionProfile) => void;
  onNew: () => void;
  onToggleFavorite: (c: ConnectionProfile) => void;
  /** Opens the form with a copy of `c`, saved as a new connection. */
  onDuplicate: (c: ConnectionProfile) => void;
  onDelete: (c: ConnectionProfile) => void;
  /** Opens another window, connected to `c` when given. */
  onOpenInNewWindow: (c?: ConnectionProfile) => void;
  onOpenRecords: () => void;
  onOpenSettings: () => void;
  onExport: () => void;
  onImport: () => void;
}

function lastUsed(iso?: string | null) {
  if (!iso) return "尚未使用";
  const mins = Math.round((Date.now() - new Date(iso).getTime()) / 60000);
  if (mins < 1) return "剛剛使用";
  if (mins < 60) return `${mins} 分鐘前使用`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours} 小時前使用`;
  return `${Math.round(hours / 24)} 天前使用`;
}

type CardProps = Pick<
  Props,
  "connecting" | "doubleClick" | "onConnect" | "onEdit" | "onToggleFavorite" | "onDuplicate" | "onDelete" | "onOpenInNewWindow"
> & { selected: boolean; onSelect: (c: ConnectionProfile) => void };

function Card({
  c,
  connecting,
  doubleClick,
  selected,
  onSelect,
  onConnect,
  onEdit,
  onToggleFavorite,
  onDuplicate,
  onDelete,
  onOpenInNewWindow,
}: CardProps & { c: ConnectionProfile }) {
  const address = serverLabel(c);
  const kind = c.kind ?? "mysql";
  return (
    <div
      className={selected ? "card selected" : "card"}
      onClick={() => (doubleClick ? onSelect(c) : onConnect(c))}
      onDoubleClick={() => doubleClick && onConnect(c)}
      onFocus={() => onSelect(c)}
      onKeyDown={(e) => {
        if (e.key === "Enter" && e.target === e.currentTarget) onConnect(c);
      }}
      role="button"
      tabIndex={0}
      onContextMenu={(e) =>
        openMenu(e, [
          { label: "連線", onClick: () => onConnect(c) },
          { label: "在新視窗連線", onClick: () => onOpenInNewWindow(c) },
          { label: "編輯…", onClick: () => onEdit(c) },
          { label: "複製一份連線…", onClick: () => onDuplicate(c) },
          { label: c.favorite ? "取消最愛" : "加入最愛", onClick: () => onToggleFavorite(c) },
          "sep",
          { label: `複製位址（${address}）`, onClick: () => copyText(address) },
          { label: "複製名稱", onClick: () => copyText(c.name) },
          "sep",
          { label: "刪除…", danger: true, onClick: () => onDelete(c) },
        ])
      }
    >
      <div className="card-head">
        <strong>{c.name}</strong>
        <button
          className={c.favorite ? "star on" : "star"}
          title={c.favorite ? "取消最愛" : "加入最愛"}
          onClick={(e) => {
            e.stopPropagation();
            onToggleFavorite(c);
          }}
        >
          {c.favorite ? "★" : "☆"}
        </button>
      </div>
      <div className="card-addr">
        {address}
        {kind === "redis" ? ` / db${c.redis_db ?? 0}` : c.database ? ` / ${c.database}` : ""}
      </div>
      <div className="card-foot">
        <span className={`badge kind-${kind}`}>{KIND_LABEL[kind]}</span>
        {c.ssl.mode !== "disabled" && <span className="badge good">TLS</span>}
        {c.iap && (
          <span className="badge good" title={`經由 GCP IAP 連到 VM ${c.iap.instance}`}>
            IAP
          </span>
        )}
        {c.ssh && (
          <span className="badge good" title={`經由 SSH ${c.ssh.user}@${c.ssh.host}`}>
            SSH
          </span>
        )}
        <span className="muted small-text last-used">{lastUsed(c.last_used_at)}</span>
        <span className="spacer" />
        {connecting === c.id ? (
          <span className="spinner" />
        ) : (
          <>
            <button
              className="small ghost"
              title="複製一份連線"
              onClick={(e) => {
                e.stopPropagation();
                onDuplicate(c);
              }}
            >
              ⧉
            </button>
            <button
              className="small ghost"
              title="編輯"
              onClick={(e) => {
                e.stopPropagation();
                onEdit(c);
              }}
            >
              ✎
            </button>
          </>
        )}
      </div>
    </div>
  );
}

type SortBy = "name" | "recent";
type KindFilter = "all" | ConnectionKind;

/** A remembered home-page choice; falls back when nothing (valid) is kept. */
function useChoice<T extends string>(key: string, allowed: readonly T[], fallback: T): [T, (v: T) => void] {
  const [value, setValue] = useState<T>(() => {
    try {
      const v = localStorage.getItem(key) as T | null;
      return v && allowed.includes(v) ? v : fallback;
    } catch {
      return fallback;
    }
  });
  const set = (v: T) => {
    setValue(v);
    try {
      localStorage.setItem(key, v);
    } catch {
      // Kept until the app closes.
    }
  };
  return [value, set];
}

function matches(c: ConnectionProfile, text: string) {
  if (!text) return true;
  const hay = [c.name, c.host, c.user, c.database ?? "", String(c.port), c.ssh?.host ?? "", c.iap?.instance ?? "", c.iap?.project ?? ""].join(" ").toLowerCase();
  return text
    .toLowerCase()
    .split(/\s+/)
    .every((w) => hay.includes(w));
}

function sorted(list: ConnectionProfile[], by: SortBy) {
  return [...list].sort((a, b) =>
    by === "recent"
      ? (b.last_used_at ?? "").localeCompare(a.last_used_at ?? "") || a.name.localeCompare(b.name)
      : a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }),
  );
}

export function Home(props: Props) {

  const { connections, onNew, onOpenRecords, onOpenSettings, onOpenInNewWindow, onExport, onImport } = props;
  const [text, setText] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [sortBy, setSortBy] = useChoice<SortBy>("mydb.home.sort", ["name", "recent"], "recent");
  const [kind, setKind] = useChoice<KindFilter>("mydb.home.kind", ["all", "mysql", "redis"], "all");
  const [grouped, setGrouped] = useChoice<"yes" | "no">("mydb.home.grouped", ["yes", "no"], "no");

  const kindOf = (c: ConnectionProfile) => c.kind ?? "mysql";
  const count = (k: ConnectionKind) => connections.filter((c) => kindOf(c) === k).length;
  const shown = sorted(
    connections.filter((c) => (kind === "all" || kindOf(c) === kind) && matches(c, text.trim())),
    sortBy,
  );
  const sections = homeSections(shown, grouped === "yes", (k) => KIND_LABEL[k]);

  return (
    <div className="home">
      <div className="home-head">
        <h1>連線</h1>
        <div className="home-actions">
          <button onClick={onOpenRecords}>📋 執行紀錄與報表</button>
          <button
            title="匯出或匯入連線，例如搬到另一台電腦"
            onClick={(e) =>
              openMenu(e, [
                { label: "匯出連線…", onClick: onExport, disabled: connections.length === 0 },
                { label: "從檔案匯入連線…", onClick: onImport },
              ])
            }
          >
            ⇅ 匯入／匯出
          </button>
          <button onClick={onOpenSettings} title="設定">
            ⚙ 設定
          </button>
          <button onClick={() => onOpenInNewWindow()} title="開新視窗，同時連另一台（⌘／Ctrl + Shift + N）">
            ⧉ 新視窗
          </button>
          <button className="primary" onClick={onNew}>
            ＋ 新增連線
          </button>
        </div>
      </div>
      {connections.length === 0 && (
        <div className="empty-state">
          <p>還沒有任何連線。</p>
          <button className="primary" onClick={onNew}>
            建立第一個連線
          </button>
          <button onClick={onImport}>從另一台電腦匯入連線…</button>
        </div>
      )}
      {connections.length > 0 && (
        <div className="home-filters">
          <input
            className="home-search"
            type="search"
            placeholder="搜尋名稱、主機、使用者、資料庫…"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setText("");
              if (e.key === "Enter" && shown.length === 1) props.onConnect(shown[0]);
            }}
          />
          <div className="segmented" role="group" aria-label="連線類型">
            {(["all", "mysql", "redis"] as const).map((k) => (
              <button key={k} className={kind === k ? "on" : ""} onClick={() => setKind(k)}>
                {k === "all" ? `全部 ${connections.length}` : `${KIND_LABEL[k]} ${count(k)}`}
              </button>
            ))}
          </div>
          <select value={sortBy} onChange={(e) => setSortBy(e.target.value as SortBy)} title="排序">
            <option value="recent">最近使用</option>
            <option value="name">名稱</option>
          </select>
          <label className="check">
            <input
              type="checkbox"
              checked={grouped === "yes"}
              onChange={(e) => setGrouped(e.target.checked ? "yes" : "no")}
            />
            依類型分組
          </label>
        </div>
      )}
      {connections.length > 0 && shown.length === 0 && <p className="muted">沒有符合的連線。</p>}
      {sections.map((sec) => (
        <div key={sec.title || "all"}>
          {sec.title && <h2>{sec.title}</h2>}
          <div className="cards">
            {sec.list.map((c) => (
              <Card key={c.id} c={c} {...props} selected={selectedId === c.id} onSelect={(c) => setSelectedId(c.id)} />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
