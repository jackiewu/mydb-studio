import { useCallback, useMemo, useState } from "react";
import {
  api,
  describeError,
  type CommandStat,
  type CoreError,
  type RedisClient,
  type RedisOverview,
  type SlowEntry,
} from "../../api";
import { openMenu } from "../../contextMenu";
import { copyText } from "../../copy";
import { bytes, duration, pct, usePolling } from "./ServerPage";

type Tab = "overview" | "clients" | "slowlog" | "commands" | "config" | "info";

const TABS: { id: Tab; label: string }[] = [
  { id: "overview", label: "總覽" },
  { id: "clients", label: "用戶端" },
  { id: "slowlog", label: "慢查詢" },
  { id: "commands", label: "指令統計" },
  { id: "config", label: "設定" },
  { id: "info", label: "INFO" },
];

interface Props {
  sessionId: string;
  /** Asks before a dangerous command, as the editor does (RiskDialog). */
  confirmRisk: (text: string) => Promise<boolean>;
}

/** The Redis counterpart of the MySQL server page; only CLIENT KILL changes anything. */
export function RedisServerPage({ sessionId, confirmRisk }: Props) {
  const [tab, setTab] = useState<Tab>("overview");
  return (
    <div className="server-page">
      <div className="tabs server-tabs">
        {TABS.map((t) => (
          <button key={t.id} className={tab === t.id ? "active" : undefined} onClick={() => setTab(t.id)}>
            {t.label}
          </button>
        ))}
      </div>
      <div className="server-body">
        {(tab === "overview" || tab === "info") && <Overview sessionId={sessionId} tab={tab} />}
        {tab === "clients" && <Clients sessionId={sessionId} confirmRisk={confirmRisk} />}
        {tab === "slowlog" && <Slowlog sessionId={sessionId} />}
        {tab === "commands" && <CommandStats sessionId={sessionId} />}
        {tab === "config" && <Config sessionId={sessionId} />}
      </div>
    </div>
  );
}

/** An error reply means the server (often a managed service) refuses the command. */
type LoadError = { refused: boolean; text: string };

function loadError(e: unknown): LoadError {
  const err = e as CoreError;
  return { refused: err?.kind === "redis", text: err?.kind === "redis" ? err.message : describeError(e) };
}

/** `load` result of one tab, with its error. */
function useLoad<T>(fetch: () => Promise<T>) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<LoadError | null>(null);
  const load = useCallback(() => {
    fetch().then(
      (d) => {
        setData(d);
        setError(null);
      },
      (e) => {
        const err = loadError(e);
        // A refusal is not a hiccup: nothing of the tab is shown.
        if (err.refused) setData(null);
        setError(err);
      },
    );
  }, [fetch]);
  return { data, error, load };
}

/** The error of a tab: a quiet note when the service doesn't allow the command. */
function ErrorNote({ error, what }: { error: LoadError | null; what: string }) {
  if (!error) return null;
  if (error.refused)
    return (
      <p className="empty-state">
        這個服務不允許 {what}，例如 ElastiCache 和部分雲端 Redis 會封鎖這個指令。
        <br />
        <span className="muted small-text">伺服器回覆：{error.text}</span>
      </p>
    );
  return <div className="error-box">{error.text}</div>;
}

function Refresh({
  auto,
  onAuto,
  every,
  onLoad,
}: {
  auto: boolean;
  onAuto: (v: boolean) => void;
  every: number;
  onLoad: () => void;
}) {
  return (
    <>
      <label className="inline check">
        <input type="checkbox" checked={auto} onChange={(e) => onAuto(e.target.checked)} /> 每 {every} 秒更新
      </label>
      <button onClick={onLoad}>↻ 重新整理</button>
    </>
  );
}

function ago(unixSeconds: number) {
  const s = Math.max(0, Math.round(Date.now() / 1000 - unixSeconds));
  return s < 60 ? `${s} 秒前` : `${duration(s)}前`;
}

function count(n: number) {
  return n.toLocaleString();
}

interface Card {
  title: string;
  value: string;
  detail?: string;
  level?: "good" | "warn" | "bad";
  hint: string;
}

/** The numbers worth a glance, with a color when they are a problem. */
function cards(o: RedisOverview): Card[] {
  const s = o.summary;
  const info = (k: string) => o.info[k] ?? "";
  const out: Card[] = [];
  out.push({
    title: "版本",
    value: s.version,
    detail: [s.mode, info("os")].filter(Boolean).join("，"),
    hint: "伺服器版本、執行模式（standalone／cluster／sentinel）與作業系統。",
  });
  out.push({
    title: "已運行",
    value: duration(s.uptime_seconds),
    detail: info("tcp_port") ? `連接埠 ${info("tcp_port")}` : undefined,
    hint: "距離上次重新啟動的時間。",
  });

  const ratio = s.maxclients ? s.connected_clients / s.maxclients : 0;
  out.push({
    title: "用戶端",
    value: s.maxclients ? `${count(s.connected_clients)} / ${count(s.maxclients)}` : count(s.connected_clients),
    detail: `等待中（blocked）${count(s.blocked_clients)}`,
    level: ratio > 0.85 ? "bad" : ratio > 0.6 ? "warn" : "good",
    hint: "connected_clients／maxclients。到達上限後新連線會收到 max number of clients reached。",
  });

  const mem = s.maxmemory ? s.used_memory / s.maxmemory : 0;
  out.push({
    title: "記憶體",
    value: s.maxmemory ? `${bytes(s.used_memory)} / ${bytes(s.maxmemory)}` : bytes(s.used_memory),
    detail: `峰值 ${bytes(s.used_memory_peak)}，${s.maxmemory ? `淘汰策略 ${s.maxmemory_policy ?? "?"}` : "沒有設定 maxmemory 上限"}`,
    level: !s.maxmemory ? undefined : mem > 0.9 ? "bad" : mem > 0.75 ? "warn" : "good",
    hint: "used_memory／maxmemory。超過上限時依 maxmemory-policy 淘汰 key，noeviction 則拒絕寫入。",
  });

  out.push({
    title: "每秒指令",
    value: s.ops_per_sec.toLocaleString(undefined, { maximumFractionDigits: 1 }),
    detail: `啟動以來共 ${count(Number(info("total_commands_processed")) || 0)} 個`,
    hint: "instantaneous_ops_per_sec：伺服器最近每秒處理的指令數。",
  });

  const reads = s.keyspace_hits + s.keyspace_misses;
  if (s.hit_ratio != null) {
    const few = reads < 1000;
    out.push({
      title: "key 命中率",
      value: pct(s.hit_ratio),
      detail: `命中 ${count(s.keyspace_hits)}，未命中 ${count(s.keyspace_misses)}${few ? "，樣本還少" : ""}`,
      level: few ? undefined : s.hit_ratio < 0.5 ? "bad" : s.hit_ratio < 0.8 ? "warn" : "good",
      hint: "讀取 key 時 key 存在的比例：hits ÷（hits + misses）。當快取用時偏低代表常常查不到，可能是 TTL 太短或記憶體不夠被淘汰。",
    });
  }

  out.push({
    title: "過期的 key",
    value: count(s.expired_keys),
    detail: "啟動以來因 TTL 到期刪除",
    hint: "expired_keys：TTL 到期後被刪除的 key 數，屬於正常現象。",
  });
  out.push({
    title: "被淘汰的 key",
    value: count(s.evicted_keys),
    detail: s.evicted_keys > 0 ? "記憶體不夠，key 被提早刪除" : "沒有因記憶體不足刪除 key",
    level: s.evicted_keys > 0 ? "warn" : "good",
    hint: "evicted_keys：記憶體到達 maxmemory 時被淘汰的 key 數。大於 0 代表資料可能在 TTL 前就消失。",
  });

  const replica = s.role === "slave";
  const linkDown = replica && info("master_link_status") === "down";
  out.push({
    title: "主從角色",
    value: replica ? "副本（replica）" : "主節點（master）",
    detail: replica
      ? `主節點 ${info("master_host")}:${info("master_port")}，連線 ${info("master_link_status") || "?"}`
      : `副本數 ${s.connected_slaves}`,
    level: linkDown ? "bad" : undefined,
    hint: "role 與 connected_slaves。副本和主節點的連線中斷時資料不會再同步。",
  });

  const saveFailed = s.rdb_last_bgsave_status === "err";
  out.push({
    title: "持久化",
    value: s.rdb_last_save_time ? `RDB ${ago(s.rdb_last_save_time)}` : "RDB —",
    detail: `AOF ${s.aof_enabled ? "開啟" : "關閉"}，${count(s.rdb_changes_since_last_save)} 筆變更尚未存檔${saveFailed ? "，上次 BGSAVE 失敗" : ""}`,
    level: saveFailed ? "bad" : undefined,
    hint: "rdb_last_save_time 是上次 RDB 快照的時間；aof_enabled 表示是否開啟 AOF 日誌。兩者都沒有時重新啟動會遺失資料。",
  });

  const used = o.keyspace.filter((d) => d.keys > 0);
  out.push({
    title: "key 總數",
    value: count(used.reduce((n, d) => n + d.keys, 0)),
    detail: used.length ? used.map((d) => `db${d.index} ${count(d.keys)}`).join("，") : "沒有任何 key",
    hint: "INFO keyspace 裡每個資料庫的 key 數。",
  });
  return out;
}

function Overview({ sessionId, tab }: { sessionId: string; tab: "overview" | "info" }) {
  const fetch = useCallback(() => api.redisServerOverview(sessionId), [sessionId]);
  const { data, error, load } = useLoad(fetch);
  const [auto, setAuto] = useState(false);
  const [filter, setFilter] = useState("");
  usePolling(load, auto, 5000);

  const rows = useMemo(() => {
    if (!data || tab === "overview") return [];
    const f = filter.trim().toLowerCase();
    return Object.entries(data.info).filter(
      ([k, v]) => !f || k.toLowerCase().includes(f) || v.toLowerCase().includes(f),
    );
  }, [data, tab, filter]);

  return (
    <>
      <div className="filters">
        {tab === "info" ? (
          <input
            autoFocus
            placeholder="搜尋 INFO 欄位，例如 memory、keyspace"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        ) : (
          <span className="muted small-text">滑到卡片上可以看說明。</span>
        )}
        <span className="spacer" />
        <Refresh auto={auto} onAuto={setAuto} every={5} onLoad={load} />
      </div>
      <ErrorNote error={error} what="INFO" />
      {data && tab === "overview" && (
        <div className="server-cards">
          {cards(data).map((c) => (
            <div key={c.title} className={`server-card ${c.level ?? ""}`} title={c.hint}>
              <div className="server-card-title">{c.title}</div>
              <div className="server-card-value">{c.value || "—"}</div>
              {c.detail && <div className="server-card-detail">{c.detail}</div>}
            </div>
          ))}
        </div>
      )}
      {data && tab === "info" && (
        <div className="records-list">
          <table className="grid records-table vars-table">
            <thead>
              <tr>
                <th>名稱</th>
                <th>值</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(([k, v]) => (
                <tr key={k} onContextMenu={(e) => openMenu(e, copyItems(k, v))}>
                  <td>{k}</td>
                  <td className={/^\d+$/.test(v) ? "num" : undefined} title={v}>
                    {/^\d+$/.test(v) ? Number(v).toLocaleString() : v}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length === 0 && <p className="empty-state">沒有符合的欄位</p>}
        </div>
      )}
    </>
  );
}

function copyItems(name: string, value: string) {
  return [
    { label: "複製名稱", onClick: () => copyText(name) },
    { label: "複製值", onClick: () => copyText(value) },
  ];
}

function Clients({ sessionId, confirmRisk }: Props) {
  const fetch = useCallback(() => api.redisClients(sessionId), [sessionId]);
  const { data, error, load } = useLoad(fetch);
  const [auto, setAuto] = useState(true);
  const [filter, setFilter] = useState("");
  const [killing, setKilling] = useState(false);
  const [killError, setKillError] = useState<string | null>(null);
  usePolling(load, auto && !killing, 3000);

  const f = filter.trim().toLowerCase();
  const shown = (data ?? [])
    .filter((c) => !f || [c.addr, c.name, c.user, c.cmd, c.flags].some((x) => x.toLowerCase().includes(f)))
    .sort((a, b) => Number(b.own) - Number(a.own) || a.id - b.id);

  const kill = async (c: RedisClient) => {
    setKilling(true);
    try {
      if (!(await confirmRisk(`CLIENT KILL ID ${c.id}`))) return;
      await api.redisKillClient(sessionId, c.id);
      setKillError(null);
      load();
    } catch (e) {
      setKillError(describeError(e));
    } finally {
      setKilling(false);
    }
  };

  return (
    <>
      <div className="filters">
        <input placeholder="搜尋位址、名稱、使用者或指令" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <span className="spacer" />
        <Refresh auto={auto} onAuto={setAuto} every={3} onLoad={load} />
      </div>
      <ErrorNote error={error} what="CLIENT LIST" />
      {killError && <div className="error-box">{killError}</div>}
      {data && (
        <div className="records-list">
          <table className="grid records-table process-table">
            <thead>
              <tr>
                <th>Id</th>
                <th>位址</th>
                <th>名稱</th>
                <th>使用者</th>
                <th>db</th>
                <th>已連線</th>
                <th>閒置秒數</th>
                <th>flags</th>
                <th>最近的指令</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {shown.map((c) => (
                <tr
                  key={c.id}
                  className={c.own ? "current" : undefined}
                  onContextMenu={(e) =>
                    openMenu(e, [
                      { label: "複製位址", onClick: () => copyText(c.addr) },
                      "sep",
                      { label: "斷開這個用戶端（CLIENT KILL）", danger: true, disabled: c.own, onClick: () => kill(c) },
                    ])
                  }
                >
                  <td className="num">{c.id}</td>
                  <td>{c.addr}</td>
                  <td>{c.own ? <span className="muted">（MyDB Studio 本身）</span> : c.name}</td>
                  <td>{c.user}</td>
                  <td className="num">{c.db}</td>
                  <td>{duration(c.age)}</td>
                  <td className="num">{count(c.idle)}</td>
                  <td>{c.flags}</td>
                  <td className="process-sql">{c.cmd}</td>
                  <td>
                    {!c.own && (
                      <button
                        className="small"
                        disabled={killing}
                        onClick={() => kill(c)}
                        title="CLIENT KILL ID：斷開這個用戶端"
                      >
                        斷開
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {shown.length === 0 && <p className="empty-state">沒有符合的用戶端</p>}
        </div>
      )}
      <p className="hint server-foot">
        flags：N 一般、b 等待中（BLPOP 等）、M 主節點、S 副本、P 訂閱中、x 在交易中。斷開前會先確認。
      </p>
    </>
  );
}

function Slowlog({ sessionId }: { sessionId: string }) {
  const fetch = useCallback(() => api.redisSlowlog(sessionId), [sessionId]);
  const { data, error, load } = useLoad<SlowEntry[]>(fetch);
  const [auto, setAuto] = useState(false);
  usePolling(load, auto, 5000);
  return (
    <>
      <div className="filters">
        <span className="muted small-text">最新 128 筆，門檻由 slowlog-log-slower-than 決定（微秒）。</span>
        <span className="spacer" />
        <Refresh auto={auto} onAuto={setAuto} every={5} onLoad={load} />
      </div>
      <ErrorNote error={error} what="SLOWLOG" />
      {data && (
        <div className="records-list">
          <table className="grid records-table process-table">
            <thead>
              <tr>
                <th>Id</th>
                <th>時間</th>
                <th>耗時</th>
                <th>指令</th>
                <th>用戶端</th>
              </tr>
            </thead>
            <tbody>
              {data.map((e) => (
                <tr
                  key={e.id}
                  onContextMenu={(ev) => openMenu(ev, [{ label: "複製指令", onClick: () => copyText(e.command) }])}
                >
                  <td className="num">{e.id}</td>
                  <td>{new Date(e.time * 1000).toLocaleString()}</td>
                  <td className={e.duration_us > 100_000 ? "num warn" : "num"}>
                    {(e.duration_us / 1000).toLocaleString(undefined, { maximumFractionDigits: 2 })} ms
                  </td>
                  <td className="process-sql" title={e.command}>
                    {e.command}
                  </td>
                  <td>{[e.client, e.client_name].filter(Boolean).join(" ")}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {data.length === 0 && <p className="empty-state">慢查詢紀錄是空的</p>}
        </div>
      )}
    </>
  );
}

type StatSort = "calls" | "usec" | "usec_per_call";

const STAT_SORTS: { id: StatSort; label: string }[] = [
  { id: "calls", label: "呼叫次數" },
  { id: "usec", label: "總耗時" },
  { id: "usec_per_call", label: "平均耗時" },
];

function CommandStats({ sessionId }: { sessionId: string }) {
  const fetch = useCallback(() => api.redisCommandStats(sessionId), [sessionId]);
  const { data, error, load } = useLoad<CommandStat[]>(fetch);
  const [sortBy, setSortBy] = useState<StatSort>("calls");
  const [filter, setFilter] = useState("");
  usePolling(load, false, 0);
  const f = filter.trim().toLowerCase();
  const rows = (data ?? []).filter((c) => !f || c.name.includes(f)).sort((a, b) => b[sortBy] - a[sortBy]);
  return (
    <>
      <div className="filters">
        <input placeholder="搜尋指令" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <div className="segmented" role="group" aria-label="排序">
          {STAT_SORTS.map((s) => (
            <button key={s.id} className={sortBy === s.id ? "on" : ""} onClick={() => setSortBy(s.id)}>
              依{s.label}
            </button>
          ))}
        </div>
        <span className="spacer" />
        <button onClick={load}>↻ 重新整理</button>
      </div>
      <ErrorNote error={error} what="INFO commandstats" />
      {data && (
        <div className="records-list">
          <table className="grid records-table vars-table">
            <thead>
              <tr>
                <th>指令</th>
                <th>呼叫次數</th>
                <th>總耗時</th>
                <th>平均（µs）</th>
                <th>被拒絕</th>
                <th>失敗</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.name}>
                  <td>{c.name}</td>
                  <td className="num">{count(c.calls)}</td>
                  <td className="num">{(c.usec / 1000).toLocaleString(undefined, { maximumFractionDigits: 1 })} ms</td>
                  <td className="num">{c.usec_per_call.toLocaleString(undefined, { maximumFractionDigits: 2 })}</td>
                  <td className="num">{count(c.rejected_calls)}</td>
                  <td className={c.failed_calls > 0 ? "num warn" : "num"}>{count(c.failed_calls)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length === 0 && <p className="empty-state">沒有符合的指令</p>}
        </div>
      )}
      <p className="hint server-foot">啟動（或 CONFIG RESETSTAT）以來的累計數字。</p>
    </>
  );
}

function Config({ sessionId }: { sessionId: string }) {
  const fetch = useCallback(() => api.redisConfig(sessionId), [sessionId]);
  const { data, error, load } = useLoad(fetch);
  const [filter, setFilter] = useState("");
  usePolling(load, false, 0);
  const f = filter.trim().toLowerCase();
  const rows = (data ?? []).filter(([k, v]) => !f || k.toLowerCase().includes(f) || v.toLowerCase().includes(f));
  return (
    <>
      <div className="filters">
        <input
          autoFocus
          placeholder="搜尋設定，例如 maxmemory、timeout"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <span className="spacer" />
        <button onClick={load}>↻ 重新整理</button>
      </div>
      <ErrorNote error={error} what="CONFIG GET" />
      {data && (
        <div className="records-list">
          <table className="grid records-table vars-table">
            <thead>
              <tr>
                <th>名稱</th>
                <th>值</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(([k, v]) => (
                <tr key={k} onContextMenu={(e) => openMenu(e, copyItems(k, v))}>
                  <td>{k}</td>
                  <td title={v}>{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length === 0 && <p className="empty-state">沒有符合的設定</p>}
        </div>
      )}
      <p className="hint server-foot">唯讀。requirepass、masterauth 等密碼顯示為 ****。</p>
    </>
  );
}
