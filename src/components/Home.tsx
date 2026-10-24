import type { ConnectionProfile } from "../api";

interface Props {
  connections: ConnectionProfile[];
  connecting: string | null;
  onConnect: (c: ConnectionProfile) => void;
  onEdit: (c: ConnectionProfile) => void;
  onNew: () => void;
  onToggleFavorite: (c: ConnectionProfile) => void;
  onOpenRecords: () => void;
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

function Card({ c, connecting, onConnect, onEdit, onToggleFavorite }: Omit<Props, "connections" | "onNew" | "onOpenRecords"> & { c: ConnectionProfile }) {
  return (
    <div className="card" onClick={() => onConnect(c)} role="button" tabIndex={0}>
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
        {c.user}@{c.host}:{c.port}
        {c.database ? ` / ${c.database}` : ""}
      </div>
      <div className="card-foot">
        {c.ssl.mode !== "disabled" && <span className="badge good">TLS</span>}
        {c.ssh && <span className="badge good">SSH {c.ssh.host}</span>}
        <span className="muted small-text">{lastUsed(c.last_used_at)}</span>
        <span className="spacer" />
        {connecting === c.id ? (
          <span className="spinner" />
        ) : (
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
        )}
      </div>
    </div>
  );
}

export function Home(props: Props) {
  const { connections, onNew, onOpenRecords } = props;
  const favorites = connections.filter((c) => c.favorite);
  const others = connections.filter((c) => !c.favorite);

  return (
    <div className="home">
      <div className="home-head">
        <h1>連線</h1>
        <div className="home-actions">
          <button onClick={onOpenRecords}>📋 執行紀錄與報表</button>
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
        </div>
      )}
      {favorites.length > 0 && (
        <>
          <h2>★ 最愛</h2>
          <div className="cards">
            {favorites.map((c) => (
              <Card key={c.id} c={c} {...props} />
            ))}
          </div>
        </>
      )}
      {others.length > 0 && (
        <>
          {favorites.length > 0 && <h2>其他連線</h2>}
          <div className="cards">
            {others.map((c) => (
              <Card key={c.id} c={c} {...props} />
            ))}
          </div>
        </>
      )}
    </div>
  );
}
