import { useCallback, useEffect, useMemo, useState } from "react";
import {
  api,
  describeError,
  type PgActivity,
  type PgAdminChange,
  type PgGrant,
  type PgObjectKind,
  type PgOverview,
  type PgPrivileges,
  type PgRole,
  type PgSetting,
  type PgStatement,
  type PgTarget,
} from "../../api";
import { openMenu } from "../../contextMenu";
import { copyText } from "../../copy";
import { PlanDialog } from "./PlanDialog";
import { bytes, duration, pct, usePolling } from "./ServerPage";
import { PasswordFields } from "./UsersPanel";

type Tab = "overview" | "activity" | "roles" | "statements" | "settings";

const TABS: { id: Tab; label: string }[] = [
  { id: "overview", label: "總覽" },
  { id: "activity", label: "執行中的查詢" },
  { id: "roles", label: "角色與權限" },
  { id: "statements", label: "慢查詢" },
  { id: "settings", label: "設定" },
];

interface Props {
  sessionId: string;
  /** This app's own backend pid, which must not be cancelled from here. */
  connectionId: number;
  onOpenSql: (sql: string) => void;
}

interface Pending {
  title: string;
  change: PgAdminChange;
  warning?: string;
  danger?: boolean;
  /** Role to select once it ran; null clears the selection. */
  select?: string | null;
}

/** The PostgreSQL 伺服器 page: what the MySQL one shows, in PostgreSQL terms. */
export function PgServerPage({ sessionId, connectionId, onOpenSql }: Props) {
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
        {tab === "overview" && <Overview sessionId={sessionId} />}
        {tab === "activity" && <Activity sessionId={sessionId} ownPid={connectionId} onOpenSql={onOpenSql} />}
        {tab === "roles" && <Roles sessionId={sessionId} />}
        {tab === "statements" && <Statements sessionId={sessionId} onOpenSql={onOpenSql} />}
        {tab === "settings" && <Settings sessionId={sessionId} />}
      </div>
    </div>
  );
}

interface Card {
  title: string;
  value: string;
  detail?: string;
  level?: "good" | "warn" | "bad";
  hint: string;
}

function hitRatio(read: number, hit: number) {
  return read + hit ? hit / (read + hit) : null;
}

function cards(o: PgOverview): Card[] {
  const out: Card[] = [];
  out.push({
    title: "版本",
    // Distribution builds append their own tag, e.g. "16.4 (Ubuntu …)".
    value: `PostgreSQL ${o.server_version.split(" ")[0]}`,
    detail: o.version.replace(/^PostgreSQL \S+\s*/, ""),
    hint: "伺服器版本與編譯平台。",
  });
  out.push({
    title: "已運行",
    value: duration(Math.floor(o.uptime_seconds)),
    detail: `目前登入：${o.current_user}（${o.current_database}）`,
    hint: "距離上次重新啟動的時間。",
  });
  const ratio = o.max_connections ? o.connections / o.max_connections : 0;
  out.push({
    title: "連線數",
    value: `${o.connections} / ${o.max_connections}`,
    detail: `執行中 ${o.active}`,
    level: ratio > 0.85 ? "bad" : ratio > 0.6 ? "warn" : "good",
    hint: "用戶端連線數／max_connections。接近上限時新連線會被拒絕（too many clients）。RDS 也會保留幾條給 rdsadmin。",
  });
  out.push({
    title: "交易中閒置",
    value: String(o.idle_in_transaction),
    detail: o.oldest_transaction_seconds ? `最久的交易已開 ${duration(Math.floor(o.oldest_transaction_seconds))}` : "沒有開著的交易",
    level: o.oldest_transaction_seconds > 600 ? "bad" : o.idle_in_transaction > 0 || o.oldest_transaction_seconds > 60 ? "warn" : "good",
    hint: "idle in transaction：開了交易卻沒有結束的連線。會擋住 VACUUM、讓資料表膨脹，也可能一直鎖著資料列。",
  });
  const cur = o.databases.find((d) => d.name === o.current_database);
  if (cur) {
    const hit = hitRatio(cur.blocks_read, cur.blocks_hit);
    if (hit != null)
      out.push({
        title: "快取命中率",
        value: pct(hit),
        detail: `${o.current_database}${cur.blocks_read + cur.blocks_hit < 100000 ? "，樣本還少" : ""}`,
        level: cur.blocks_read + cur.blocks_hit < 100000 ? undefined : hit < 0.95 ? "bad" : hit < 0.99 ? "warn" : "good",
        hint: "讀取時資料已在 shared_buffers 的比例。偏低代表記憶體可能不夠，常常要讀硬碟。",
      });
    const xacts = cur.commits + cur.rollbacks;
    if (xacts)
      out.push({
        title: "交易回復比例",
        value: pct(cur.rollbacks / xacts),
        detail: `${cur.rollbacks.toLocaleString()} / ${xacts.toLocaleString()}`,
        level: cur.rollbacks / xacts > 0.1 ? "warn" : undefined,
        hint: "ROLLBACK 的交易比例。偏高通常是程式出錯後回復，值得查一下錯誤紀錄。",
      });
    out.push({
      title: "死結",
      value: cur.deadlocks.toLocaleString(),
      detail: `暫存檔 ${bytes(cur.temp_bytes)}`,
      level: cur.deadlocks > 0 ? "warn" : "good",
      hint: "deadlocks：互相等待而被中止的交易數。暫存檔是排序或雜湊放不進 work_mem 時寫到硬碟的量。",
    });
  }
  out.push({
    title: "角色",
    value: o.in_recovery ? "唯讀副本" : "主要",
    level: o.in_recovery ? "warn" : undefined,
    hint: "唯讀副本（read replica / hot standby）只能查詢，不能寫入。",
  });
  return out;
}

function Overview({ sessionId }: { sessionId: string }) {
  const [o, setO] = useState<PgOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [auto, setAuto] = useState(false);
  const load = useCallback(() => {
    api.pgServerOverview(sessionId).then(
      (x) => {
        setO(x);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  usePolling(load, auto, 5000);

  return (
    <>
      <div className="filters">
        <span className="muted small-text">滑到卡片上可以看說明。</span>
        <span className="spacer" />
        <label className="inline check">
          <input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} /> 每 5 秒更新
        </label>
        <button onClick={load}>↻ 重新整理</button>
      </div>
      {error && <div className="error-box">{error}</div>}
      {o && (
        <>
          <div className="server-cards">
            {cards(o).map((c) => (
              <div key={c.title} className={`server-card ${c.level ?? ""}`} title={c.hint}>
                <div className="server-card-title">{c.title}</div>
                <div className="server-card-value">{c.value || "—"}</div>
                {c.detail && <div className="server-card-detail">{c.detail}</div>}
              </div>
            ))}
          </div>
          <div className="records-list">
            <table className="grid records-table">
              <thead>
                <tr>
                  <th>資料庫</th>
                  <th>大小</th>
                  <th>連線</th>
                  <th>快取命中率</th>
                  <th>交易（提交／回復）</th>
                  <th>死結</th>
                </tr>
              </thead>
              <tbody>
                {o.databases.map((d) => {
                  const hit = hitRatio(d.blocks_read, d.blocks_hit);
                  return (
                    <tr key={d.name} className={d.name === o.current_database ? "current" : undefined}>
                      <td>{d.name}</td>
                      <td className="num" title={d.size_bytes == null ? "沒有 CONNECT 權限，看不到大小" : undefined}>
                        {d.size_bytes == null ? "—" : bytes(d.size_bytes)}
                      </td>
                      <td className="num">{d.connections}</td>
                      <td className="num">{hit == null ? "" : pct(hit)}</td>
                      <td className="num">
                        {d.commits.toLocaleString()} / {d.rollbacks.toLocaleString()}
                      </td>
                      <td className="num">{d.deadlocks || ""}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <p className="hint server-foot">
            這個連線只連到 {o.current_database}；結構頁與權限都是這個資料庫裡的。要管理其他資料庫，請另外新增一個連線。
          </p>
        </>
      )}
    </>
  );
}

function Activity({ sessionId, ownPid, onOpenSql }: { sessionId: string; ownPid: number; onOpenSql: (sql: string) => void }) {
  const [list, setList] = useState<PgActivity[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [auto, setAuto] = useState(true);
  const [hideIdle, setHideIdle] = useState(true);
  const [filter, setFilter] = useState("");
  const [pending, setPending] = useState<Pending | null>(null);

  const load = useCallback(() => {
    api.pgServerActivity(sessionId).then(
      (l) => {
        setList(l);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  usePolling(load, auto && !pending, 3000);

  // Clock differences can make a just-started query a little negative.
  const seconds = (a: PgActivity) => Math.max(0, (a.state === "active" ? a.query_seconds : a.transaction_seconds) ?? 0);
  const f = filter.trim().toLowerCase();
  const shown = (list ?? [])
    .filter((a) => !hideIdle || (a.backend_type === "client backend" && a.state !== "idle"))
    .filter((a) => !f || [a.user, a.client, a.application, a.database ?? "", a.query, a.state].some((x) => x.toLowerCase().includes(f)))
    .sort((a, b) => seconds(b) - seconds(a));

  const ask = (a: PgActivity, terminate: boolean) =>
    setPending(
      terminate
        ? {
            title: "中斷連線",
            change: { kind: "terminate", pid: a.pid },
            danger: true,
            warning: `中斷 ${a.user}（pid ${a.pid}）的整個連線。未提交的交易會回復，對方程式會收到連線中斷的錯誤。`,
          }
        : {
            title: "中止查詢",
            change: { kind: "cancel", pid: a.pid },
            warning: `中止 ${a.user}（pid ${a.pid}）正在執行的語句。若在交易中，整個交易會變成失敗、需要 ROLLBACK；連線保留。`,
          },
    );

  return (
    <>
      <div className="filters">
        <input placeholder="搜尋使用者、來源、程式、資料庫或 SQL" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <label className="inline check">
          <input type="checkbox" checked={hideIdle} onChange={(e) => setHideIdle(e.target.checked)} /> 隱藏閒置與背景程序
        </label>
        <label className="inline check">
          <input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} /> 每 3 秒更新
        </label>
        <button onClick={load}>↻ 重新整理</button>
      </div>
      {error && <div className="error-box">{error}</div>}
      <div className="records-list">
        <table className="grid records-table process-table">
          <thead>
            <tr>
              <th>pid</th>
              <th>使用者</th>
              <th>來源</th>
              <th>資料庫</th>
              <th>狀態</th>
              <th title="執行中：這句已跑的秒數；其他：交易已開的秒數">秒數</th>
              <th>SQL</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {shown.map((a) => {
              const own = a.pid === ownPid;
              const secs = seconds(a);
              const client = a.backend_type === "client backend";
              return (
                <tr
                  key={a.pid}
                  className={own ? "current" : undefined}
                  onContextMenu={(e) =>
                    openMenu(e, [
                      { label: "複製 SQL", disabled: !a.query, onClick: () => copyText(a.query) },
                      { label: "在新分頁開啟 SQL", disabled: !a.query, onClick: () => onOpenSql(a.query) },
                      "sep",
                      { label: "中止這個查詢（pg_cancel_backend）", disabled: own || a.state !== "active", onClick: () => ask(a, false) },
                      { label: "中斷這個連線（pg_terminate_backend）", danger: true, disabled: own || !client, onClick: () => ask(a, true) },
                    ])
                  }
                >
                  <td className="num">{a.pid}</td>
                  <td>{a.user || <span className="muted">{a.backend_type}</span>}</td>
                  <td title={a.application}>
                    {a.client}
                    {a.application && <span className="muted small-text"> {a.application}</span>}
                  </td>
                  <td>{a.database ?? ""}</td>
                  <td className={a.state.startsWith("idle in transaction") ? "warn" : undefined} title={a.wait ?? ""}>
                    {a.state || a.backend_type}
                    {a.wait && <span className="muted small-text"> {a.wait}</span>}
                  </td>
                  <td className={secs > 60 && a.state !== "idle" ? "num warn" : "num"}>{secs ? Math.floor(secs) : ""}</td>
                  <td className="process-sql" title={a.query}>
                    {own ? <span className="muted">（MyDB Studio 本身）</span> : a.query}
                  </td>
                  <td>
                    {!own && a.state === "active" && client && (
                      <button className="small" onClick={() => ask(a, false)} title="pg_cancel_backend：只中止這句，連線保留">
                        中止
                      </button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {list && shown.length === 0 && <p className="empty-state">{hideIdle ? "目前沒有正在執行的查詢" : "沒有符合的連線"}</p>}
      </div>
      <p className="hint server-foot">
        只看得到別人的 SQL 需要 pg_read_all_stats（RDS 的 rds_superuser 有）；中止別人的查詢需要同一個角色或 pg_signal_backend。
      </p>
      {pending && (
        <PlanDialog
          postgres
          sessionId={sessionId}
          title={pending.title}
          change={pending.change}
          danger={pending.danger}
          warning={pending.warning}
          onCancel={() => setPending(null)}
          onDone={() => {
            setPending(null);
            load();
          }}
        />
      )}
    </>
  );
}

function Settings({ sessionId }: { sessionId: string }) {
  const [list, setList] = useState<PgSetting[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [changedOnly, setChangedOnly] = useState(false);
  const load = useCallback(() => {
    api.pgServerSettings(sessionId).then(
      (l) => {
        setList(l);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  useEffect(load, [load]);

  const f = filter.trim().toLowerCase();
  const shown = (list ?? []).filter(
    (s) =>
      (!changedOnly || (s.source !== "default" && s.source !== "override")) &&
      (!f || [s.name, s.setting, s.description, s.category].some((x) => x.toLowerCase().includes(f))),
  );
  return (
    <>
      <div className="filters">
        <input autoFocus placeholder="搜尋設定，例如 work_mem、timeout、log" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <label className="inline check">
          <input type="checkbox" checked={changedOnly} onChange={(e) => setChangedOnly(e.target.checked)} /> 只看改過的
        </label>
        <span className="spacer" />
        <button onClick={load}>↻ 重新整理</button>
      </div>
      {error && <div className="error-box">{error}</div>}
      <div className="records-list">
        <table className="grid records-table vars-table">
          <thead>
            <tr>
              <th>名稱</th>
              <th>值</th>
              <th>來源</th>
              <th>生效方式</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((s) => (
              <tr
                key={s.name}
                title={`${s.description}\n分類：${s.category}`}
                onContextMenu={(e) =>
                  openMenu(e, [
                    { label: "複製名稱", onClick: () => copyText(s.name) },
                    { label: "複製值", onClick: () => copyText(s.setting) },
                  ])
                }
              >
                <td>{s.name}</td>
                <td>
                  {s.setting}
                  {s.unit && <span className="muted small-text"> {s.unit}</span>}
                  {s.pending_restart && <span className="badge bad-badge">待重新啟動</span>}
                </td>
                <td className="muted">{s.source}</td>
                <td className="muted">{CONTEXT[s.context] ?? s.context}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {list && shown.length === 0 && <p className="empty-state">沒有符合的設定</p>}
      </div>
      <p className="hint server-foot">
        AWS RDS 的設定要在參數群組（parameter group）裡改；「需要重新啟動」的要重開執行個體才會生效。
      </p>
    </>
  );
}

const CONTEXT: Record<string, string> = {
  internal: "唯讀",
  postmaster: "需要重新啟動",
  sighup: "重新載入設定",
  "superuser-backend": "新連線（超級使用者）",
  backend: "新連線",
  superuser: "SET（超級使用者）",
  user: "SET",
};

function Statements({ sessionId, onOpenSql }: { sessionId: string; onOpenSql: (sql: string) => void }) {
  const [list, setList] = useState<PgStatement[] | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const load = useCallback(() => {
    api.pgTopStatements(sessionId).then(
      (l) => {
        setList(l);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  useEffect(load, [load]);
  const f = filter.trim().toLowerCase();
  const shown = (list ?? []).filter((s) => !f || [s.query, s.database, s.user].some((x) => x.toLowerCase().includes(f)));
  return (
    <>
      <div className="filters">
        <input placeholder="搜尋 SQL、資料庫或使用者" value={filter} onChange={(e) => setFilter(e.target.value)} />
        <span className="spacer" />
        <button onClick={load}>↻ 重新整理</button>
      </div>
      {error && <div className="error-box">{error}</div>}
      {list === null && (
        <div className="hint pad">
          <p>這個資料庫還沒有安裝 pg_stat_statements，所以看不到哪些查詢最花時間。</p>
          <p>
            AWS RDS 預設已經在 shared_preload_libraries 裡載入它，只要用有權限的帳號在這個資料庫執行一次：
          </p>
          <pre className="plan-sql">CREATE EXTENSION pg_stat_statements;</pre>
          <button onClick={() => onOpenSql("CREATE EXTENSION IF NOT EXISTS pg_stat_statements;\n")}>在編輯器開啟</button>
        </div>
      )}
      {list && (
        <div className="records-list">
          <table className="grid records-table process-table">
            <thead>
              <tr>
                <th title="累計執行時間">總時間</th>
                <th>次數</th>
                <th title="每次平均">平均</th>
                <th>筆數</th>
                <th>資料庫</th>
                <th>使用者</th>
                <th>SQL</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((s, i) => (
                <tr
                  key={i}
                  onContextMenu={(e) =>
                    openMenu(e, [
                      { label: "複製 SQL", onClick: () => copyText(s.query) },
                      { label: "在新分頁開啟 SQL", onClick: () => onOpenSql(s.query) },
                    ])
                  }
                >
                  <td className="num">{ms(s.total_ms)}</td>
                  <td className="num">{s.calls.toLocaleString()}</td>
                  <td className={s.mean_ms > 1000 ? "num warn" : "num"}>{ms(s.mean_ms)}</td>
                  <td className="num">{s.rows.toLocaleString()}</td>
                  <td>{s.database}</td>
                  <td>{s.user}</td>
                  <td className="process-sql" title={s.query}>
                    {s.query}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {shown.length === 0 && <p className="empty-state">沒有紀錄</p>}
        </div>
      )}
      {list && (
        <p className="hint server-foot">
          依總時間排序的前 100 句（自從上次 pg_stat_statements_reset()）。$1、$2 是被抽掉的常數。
        </p>
      )}
    </>
  );
}

function ms(n: number) {
  if (n >= 60000) return `${(n / 60000).toFixed(1)} 分`;
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)} 秒`;
  return `${n.toFixed(n < 10 ? 2 : 0)} ms`;
}

// ---------------------------------------------------------------------------
// Roles

/** Built-in roles worth handing out (PostgreSQL 14+ for the data ones). */
const PREDEFINED = ["pg_read_all_data", "pg_write_all_data", "pg_monitor", "pg_read_all_stats", "pg_signal_backend"];

const KIND_LABEL: Record<PgObjectKind, string> = {
  database: "資料庫",
  schema: "Schema",
  table: "資料表／檢視",
  sequence: "序列",
  function: "函式／程序",
  all_tables: "schema 裡所有資料表",
  all_sequences: "schema 裡所有序列",
  all_functions: "schema 裡所有函式",
};

const PRIVS: Record<"database" | "schema" | "table" | "sequence" | "function", string[]> = {
  database: ["CONNECT", "CREATE", "TEMPORARY"],
  schema: ["USAGE", "CREATE"],
  table: ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"],
  sequence: ["USAGE", "SELECT", "UPDATE"],
  function: ["EXECUTE"],
};

function privsFor(kind: PgObjectKind) {
  if (kind === "all_tables") return PRIVS.table;
  if (kind === "all_sequences") return PRIVS.sequence;
  if (kind === "all_functions") return PRIVS.function;
  return PRIVS[kind];
}

function targetText(t: PgTarget) {
  if (t.kind.startsWith("all_")) return `${t.schema}.*`;
  return t.schema ? `${t.schema}.${t.name}` : (t.name ?? "");
}

function Roles({ sessionId }: { sessionId: string }) {
  const [roles, setRoles] = useState<PgRole[] | null>(null);
  const [me, setMe] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [creating, setCreating] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [version, setVersion] = useState(0);

  const load = useCallback(() => {
    api.pgRoles(sessionId).then(
      (r) => {
        setRoles(r);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  useEffect(() => {
    load();
    api.pgServerOverview(sessionId).then(
      (o) => setMe(o.current_user),
      () => {},
    );
  }, [load, sessionId]);

  const f = filter.trim().toLowerCase();
  const shown = (roles ?? []).filter((r) => !f || r.name.toLowerCase().includes(f));
  const role = roles?.find((r) => r.name === selected) ?? null;

  return (
    <div className="users-panel">
      <div className="users-list">
        <div className="users-list-head">
          <input placeholder="搜尋角色" value={filter} onChange={(e) => setFilter(e.target.value)} />
          <button className="primary" onClick={() => setCreating(true)} title="新增角色">
            ＋
          </button>
        </div>
        {error && <div className="error-box">{error}</div>}
        <ul>
          {shown.map((r) => (
            <li key={r.name} className={r.name === selected ? "current" : undefined} onClick={() => (setSelected(r.name), setMessage(null))}>
              <span className="user-name">{r.name}</span>
              {!r.can_login && <span className="muted small-text">群組</span>}
              {r.superuser && <span className="badge bad-badge">superuser</span>}
              {r.name === me && <span className="badge">你</span>}
            </li>
          ))}
        </ul>
        {roles && shown.length === 0 && <p className="empty-state">沒有角色</p>}
      </div>
      <div className="user-detail">
        {message && (
          <div className="notice ok" onClick={() => setMessage(null)}>
            {message}
          </div>
        )}
        {role ? (
          <RoleDetail
            key={`${role.name}:${version}`}
            sessionId={sessionId}
            role={role}
            roles={roles ?? []}
            me={me}
            onPlan={setPending}
          />
        ) : (
          <p className="empty-state">
            選一個角色查看與修改權限，或按 ＋ 新增。PostgreSQL 的使用者就是可以登入（LOGIN）的角色；群組是不能登入、用來分派權限的角色。
          </p>
        )}
      </div>
      {creating && (
        <CreateRoleDialog
          roles={roles ?? []}
          onCancel={() => setCreating(false)}
          onNext={(p) => {
            setCreating(false);
            setPending(p);
          }}
        />
      )}
      {pending && (
        <PlanDialog
          postgres
          sessionId={sessionId}
          title={pending.title}
          change={pending.change}
          warning={pending.warning}
          danger={pending.danger}
          onCancel={() => setPending(null)}
          onDone={() => {
            const done = pending;
            setPending(null);
            setMessage(`已完成：${done.title}`);
            if (done.select !== undefined) setSelected(done.select);
            setVersion((v) => v + 1);
            load();
          }}
        />
      )}
    </div>
  );
}

function RoleDetail({
  sessionId,
  role,
  roles,
  me,
  onPlan,
}: {
  sessionId: string;
  role: PgRole;
  roles: PgRole[];
  me: string;
  onPlan: (p: Pending) => void;
}) {
  const [privs, setPrivs] = useState<PgPrivileges | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [login, setLogin] = useState(role.can_login);
  const [createDb, setCreateDb] = useState(role.create_db);
  const [createRole, setCreateRole] = useState(role.create_role);
  const [limit, setLimit] = useState(role.connection_limit < 0 ? "" : String(role.connection_limit));
  const [until, setUntil] = useState(role.valid_until && role.valid_until !== "infinity" ? role.valid_until : "");
  const [password, setPassword] = useState(false);
  const [dropping, setDropping] = useState(false);
  const [granting, setGranting] = useState(false);
  const [addGroup, setAddGroup] = useState("");

  useEffect(() => {
    api.pgRolePrivileges(sessionId, role.name).then(setPrivs, (e) => setError(describeError(e)));
  }, [sessionId, role.name]);

  const attrsChanged =
    login !== role.can_login ||
    createDb !== role.create_db ||
    createRole !== role.create_role ||
    (limit === "" ? -1 : Number(limit)) !== role.connection_limit ||
    until !== (role.valid_until && role.valid_until !== "infinity" ? role.valid_until : "");
  const isMe = role.name === me;
  const groups = useMemo(
    () => [...roles.filter((r) => r.name !== role.name).map((r) => r.name), ...PREDEFINED].filter((n) => !role.member_of.includes(n)),
    [roles, role],
  );

  const revoke = (g: PgGrant) =>
    onPlan({
      title: `撤銷 ${role.name} 在 ${targetText(g.target)} 的權限`,
      change: { kind: "revoke", role: role.name, target: g.target, privileges: g.privileges },
    });

  return (
    <div className="user-detail-body">
      <div className="user-head">
        <h2>{role.name}</h2>
        <span className="badge">{role.can_login ? "可登入" : "群組（不能登入）"}</span>
        {role.superuser && <span className="badge bad-badge">superuser</span>}
        {role.replication && <span className="badge">replication</span>}
        <span className="spacer" />
        <button onClick={() => setPassword(true)}>變更密碼…</button>
        <button className="danger" disabled={isMe} onClick={() => setDropping(true)} title={isMe ? "不能刪除目前登入的角色" : undefined}>
          刪除角色…
        </button>
      </div>

      <h3>屬性</h3>
      <div className="admin-form">
        <div className="row">
          <label className="check">
            <input type="checkbox" checked={login} onChange={(e) => setLogin(e.target.checked)} /> 可以登入（LOGIN）
          </label>
          <label className="check">
            <input type="checkbox" checked={createDb} onChange={(e) => setCreateDb(e.target.checked)} /> 可建立資料庫（CREATEDB）
          </label>
          <label className="check">
            <input type="checkbox" checked={createRole} onChange={(e) => setCreateRole(e.target.checked)} /> 可建立角色（CREATEROLE）
          </label>
        </div>
        <div className="row">
          <label>
            連線數上限
            <input value={limit} placeholder="不限" onChange={(e) => setLimit(e.target.value.replace(/\D/g, ""))} />
          </label>
          <label>
            密碼有效期限
            <input value={until} placeholder="永久，例如 2026-12-31" onChange={(e) => setUntil(e.target.value)} />
          </label>
        </div>
        <div className="actions">
          <span className="spacer" />
          <button
            className="primary"
            disabled={!attrsChanged}
            onClick={() =>
              onPlan({
                title: `修改角色 ${role.name} 的屬性`,
                change: {
                  kind: "alter_role",
                  name: role.name,
                  login,
                  create_db: createDb,
                  create_role: createRole,
                  connection_limit: limit === "" ? -1 : Number(limit),
                  valid_until: until.trim() || null,
                },
                warning: isMe && !login ? "這是你目前登入的角色；取消 LOGIN 之後就不能再用它連線。" : undefined,
              })
            }
          >
            預覽並儲存…
          </button>
        </div>
      </div>

      <h3>所屬群組</h3>
      <div className="index-parts">
        {role.member_of.map((g) => (
          <span key={g} className="index-part">
            <code>{g}</code>
            <button
              className="small ghost"
              title="移出這個群組"
              onClick={() =>
                onPlan({
                  title: `把 ${role.name} 移出 ${g}`,
                  change: { kind: "revoke_role", role: g, member: role.name },
                })
              }
            >
              ✕
            </button>
          </span>
        ))}
        <select
          value={addGroup}
          onChange={(e) => {
            const g = e.target.value;
            setAddGroup("");
            if (g)
              onPlan({
                title: `把 ${role.name} 加入 ${g}`,
                change: { kind: "grant_role", role: g, member: role.name },
              });
          }}
        >
          <option value="">＋ 加入群組</option>
          {groups.map((g) => (
            <option key={g} value={g}>
              {g}
            </option>
          ))}
        </select>
      </div>
      <p className="muted small-text">加入群組就有群組的所有權限。pg_read_all_data／pg_write_all_data 是內建的全部讀取／寫入（PostgreSQL 14 以上）。</p>

      <div className="user-head">
        <h3>在這個資料庫的權限</h3>
        <span className="spacer" />
        <button className="primary" onClick={() => setGranting(true)}>
          ＋ 授權…
        </button>
      </div>
      {error && <div className="error-box">{error}</div>}
      {privs && (
        <>
          <table className="grid records-table">
            <thead>
              <tr>
                <th>種類</th>
                <th>物件</th>
                <th>權限</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {privs.grants.map((g) => (
                <tr key={`${g.target.kind}:${targetText(g.target)}`}>
                  <td>{KIND_LABEL[g.target.kind]}</td>
                  <td>
                    <code>{targetText(g.target)}</code>
                  </td>
                  <td>
                    {g.privileges.map((p) => (
                      <span key={p} className="badge" title={g.grantable.includes(p) ? "可以再授權給別人（WITH GRANT OPTION）" : undefined}>
                        {p}
                        {g.grantable.includes(p) ? " ✱" : ""}
                      </span>
                    ))}
                  </td>
                  <td className="row-actions">
                    <button className="small ghost danger" onClick={() => revoke(g)} title="撤銷這些權限">
                      撤銷
                    </button>
                  </td>
                </tr>
              ))}
              {privs.defaults.map((d) => (
                <tr key={`default:${d.for_role}:${d.schema}:${d.object}`}>
                  <td>預設權限</td>
                  <td title={`${d.for_role} 之後建立的物件會自動給這些權限`}>
                    <code>
                      {d.schema ? `${d.schema}.` : "所有 schema 的"}
                      {DEFAULT_LABEL[d.object] ?? d.object}
                    </code>
                    <span className="muted small-text">（{d.for_role} 新建的）</span>
                  </td>
                  <td>
                    {d.privileges.map((p) => (
                      <span key={p} className="badge">
                        {p}
                      </span>
                    ))}
                  </td>
                  <td className="row-actions">
                    {(d.object === "tables" || d.object === "sequences" || d.object === "functions") && (
                      <button
                        className="small ghost danger"
                        onClick={() =>
                          onPlan({
                            title: `撤銷 ${role.name} 的預設權限`,
                            change: {
                              kind: "default_privileges",
                              role: role.name,
                              schema: d.schema,
                              object: d.object as "tables" | "sequences" | "functions",
                              privileges: d.privileges,
                              revoke: true,
                              for_role: d.for_role,
                            },
                          })
                        }
                      >
                        撤銷
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {privs.grants.length === 0 && privs.defaults.length === 0 && (
            <p className="empty-state">在這個資料庫沒有個別授權{role.member_of.length ? "（可能透過群組取得權限）" : ""}。</p>
          )}
          {privs.owned > 0 && <p className="muted small-text">另外擁有 {privs.owned} 個物件（擁有者有全部權限）。</p>}
        </>
      )}

      {password && (
        <PasswordDialog
          name={role.name}
          onCancel={() => setPassword(false)}
          onNext={(pw) => {
            setPassword(false);
            onPlan({
              title: `變更 ${role.name} 的密碼`,
              change: { kind: "set_password", name: role.name, password: pw },
              warning: isMe ? "這是你目前登入的角色。改完後，連線設定裡記住的密碼也要一起更新，否則下次會連不上。" : undefined,
            });
          }}
        />
      )}
      {dropping && (
        <DropRoleDialog
          role={role}
          roles={roles}
          me={me}
          owned={privs?.owned ?? 0}
          onCancel={() => setDropping(false)}
          onNext={(p) => {
            setDropping(false);
            onPlan(p);
          }}
        />
      )}
      {granting && (
        <GrantDialog
          sessionId={sessionId}
          role={role.name}
          onCancel={() => setGranting(false)}
          onNext={(p) => {
            setGranting(false);
            onPlan(p);
          }}
        />
      )}
    </div>
  );
}

const DEFAULT_LABEL: Record<string, string> = {
  tables: "資料表",
  sequences: "序列",
  functions: "函式",
  types: "型別",
  schemas: "schema",
};

function PasswordDialog({ name, onNext, onCancel }: { name: string; onNext: (p: string) => void; onCancel: () => void }) {
  const [password, setPassword] = useState("");
  const [valid, setValid] = useState(true);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <form
        className="conn-form admin-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (password && valid) onNext(password);
        }}
      >
        <h2>變更 {name} 的密碼</h2>
        <PasswordFields password={password} setPassword={setPassword} required onValid={setValid} />
        <p className="muted small-text">密碼會先在本機轉成 SCRAM-SHA-256 雜湊才送出，伺服器與它的紀錄檔都不會看到明文。</p>
        <div className="actions">
          <span className="spacer" />
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button className="primary" type="submit" disabled={!password || !valid}>
            下一步：預覽
          </button>
        </div>
      </form>
    </div>
  );
}

function CreateRoleDialog({ roles, onNext, onCancel }: { roles: PgRole[]; onNext: (p: Pending) => void; onCancel: () => void }) {
  const [name, setName] = useState("");
  const [login, setLogin] = useState(true);
  const [password, setPassword] = useState("");
  const [valid, setValid] = useState(true);
  const [createDb, setCreateDb] = useState(false);
  const [limit, setLimit] = useState("");
  const [groups, setGroups] = useState<string[]>([]);
  const options = [...roles.filter((r) => !r.can_login).map((r) => r.name), ...PREDEFINED];
  const toggle = (g: string) => setGroups((cur) => (cur.includes(g) ? cur.filter((x) => x !== g) : [...cur, g]));
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <form
        className="conn-form admin-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (!name.trim() || !valid) return;
          onNext({
            title: `新增角色 ${name.trim()}`,
            change: {
              kind: "create_role",
              name: name.trim(),
              password: password || null,
              login,
              create_db: createDb,
              create_role: false,
              connection_limit: limit ? Number(limit) : null,
              valid_until: null,
              member_of: groups,
            },
            select: name.trim(),
          });
        }}
      >
        <h2>新增角色</h2>
        <label>
          名稱
          <input value={name} onChange={(e) => setName(e.target.value)} spellCheck={false} autoFocus />
        </label>
        <div className="row">
          <label className="check">
            <input type="checkbox" checked={login} onChange={(e) => setLogin(e.target.checked)} /> 可以登入（使用者）
          </label>
          <label className="check">
            <input type="checkbox" checked={createDb} onChange={(e) => setCreateDb(e.target.checked)} /> 可建立資料庫
          </label>
        </div>
        {login && <PasswordFields password={password} setPassword={setPassword} required={false} onValid={setValid} />}
        <label>
          連線數上限
          <input value={limit} placeholder="不限" onChange={(e) => setLimit(e.target.value.replace(/\D/g, ""))} />
        </label>
        <div>
          <span>加入群組</span>
          <div className="pg-priv-checks">
            {options.map((g) => (
              <label key={g} className="check">
                <input type="checkbox" checked={groups.includes(g)} onChange={() => toggle(g)} /> {g}
              </label>
            ))}
          </div>
          <p className="muted small-text">
            要開唯讀帳號：PostgreSQL 14 以上可以直接加入 pg_read_all_data；否則建立後在「授權…」給 schema 的 USAGE 與所有資料表的 SELECT。
          </p>
        </div>
        <div className="actions">
          <span className="spacer" />
          <button type="button" onClick={onCancel}>
            取消
          </button>
          <button className="primary" type="submit" disabled={!name.trim() || !valid}>
            下一步：預覽
          </button>
        </div>
      </form>
    </div>
  );
}

function DropRoleDialog({
  role,
  roles,
  me,
  owned,
  onNext,
  onCancel,
}: {
  role: PgRole;
  roles: PgRole[];
  me: string;
  owned: number;
  onNext: (p: Pending) => void;
  onCancel: () => void;
}) {
  const [reassign, setReassign] = useState(true);
  const [to, setTo] = useState(me);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="risk-dialog plan-dialog admin-form" role="dialog">
        <h2>刪除角色 {role.name}</h2>
        <label className="check">
          <input type="checkbox" checked={reassign} onChange={(e) => setReassign(e.target.checked)} />
          先把它擁有的物件轉給別的角色，並清掉它在這個資料庫的權限（REASSIGN OWNED + DROP OWNED）
        </label>
        {reassign && (
          <label>
            轉給
            <select value={to} onChange={(e) => setTo(e.target.value)}>
              {roles
                .filter((r) => r.name !== role.name)
                .map((r) => (
                  <option key={r.name} value={r.name}>
                    {r.name}
                  </option>
                ))}
            </select>
          </label>
        )}
        <p className="field-note">
          {owned > 0 ? `它在這個資料庫擁有 ${owned} 個物件。` : ""}
          角色在其他資料庫還有物件或權限的話，刪除會失敗；請連到那些資料庫做一樣的處理。
        </p>
        <div className="actions">
          <span className="spacer" />
          <button onClick={onCancel}>取消</button>
          <button
            className="danger"
            disabled={reassign && !to}
            onClick={() =>
              onNext({
                title: `刪除角色 ${role.name}`,
                change: { kind: "drop_role", name: role.name, reassign_to: reassign ? to : null },
                danger: true,
                warning: reassign
                  ? `${role.name} 擁有的物件會轉給 ${to}，它的權限會被清掉，然後刪除角色。全部在同一個交易裡。`
                  : `刪除角色 ${role.name}。`,
                select: null,
              })
            }
          >
            下一步：預覽
          </button>
        </div>
      </div>
    </div>
  );
}

type GrantKind = PgObjectKind | "default_tables" | "default_sequences" | "default_functions";

const GRANT_KINDS: { id: GrantKind; label: string }[] = [
  { id: "database", label: "這個資料庫（連線、建立 schema）" },
  { id: "schema", label: "Schema（使用、建立物件）" },
  { id: "all_tables", label: "schema 裡現有的所有資料表與檢視" },
  { id: "default_tables", label: "schema 裡之後新建的資料表（預設權限）" },
  { id: "table", label: "單一資料表或檢視" },
  { id: "all_sequences", label: "schema 裡所有序列" },
  { id: "default_sequences", label: "schema 裡之後新建的序列（預設權限）" },
  { id: "all_functions", label: "schema 裡所有函式" },
  { id: "function", label: "單一函式或程序" },
];

function GrantDialog({
  sessionId,
  role,
  onNext,
  onCancel,
}: {
  sessionId: string;
  role: string;
  onNext: (p: Pending) => void;
  onCancel: () => void;
}) {
  const [kind, setKind] = useState<GrantKind>("all_tables");
  const [database, setDatabase] = useState("");
  const [schemas, setSchemas] = useState<string[]>([]);
  const [schema, setSchema] = useState("");
  const [names, setNames] = useState<string[]>([]);
  const [name, setName] = useState("");
  const [privs, setPrivs] = useState<string[]>(["SELECT"]);
  const [grantOption, setGrantOption] = useState(false);

  useEffect(() => {
    api.listDatabases(sessionId).then(
      (d) => {
        setSchemas(d.map((x) => x.name));
        setSchema((cur) => cur || d.find((x) => x.name === "public")?.name || d[0]?.name || "");
      },
      () => {},
    );
    api.pgServerOverview(sessionId).then(
      (o) => setDatabase(o.current_database),
      () => {},
    );
  }, [sessionId]);

  const objectKind: PgObjectKind = kind.startsWith("default_")
    ? (kind.replace("default_", "all_") as PgObjectKind)
    : (kind as PgObjectKind);
  const available = privsFor(objectKind);

  useEffect(() => {
    setName("");
    setNames([]);
    if (!schema) return;
    if (kind === "table")
      api.listTables(sessionId, schema).then(
        (t) => setNames(t.map((x) => x.name)),
        () => {},
      );
    if (kind === "function")
      api.schemaObjects(sessionId, schema).then(
        (o) => setNames(o.objects.filter((x) => x.kind === "function" || x.kind === "procedure").map((x) => x.name)),
        () => {},
      );
  }, [sessionId, kind, schema]);

  useEffect(() => {
    setPrivs((cur) => {
      const kept = cur.filter((p) => available.includes(p));
      return kept.length ? kept : [available[0]];
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind]);

  const needsSchema = kind !== "database" && kind !== "schema";
  const target: PgTarget = {
    kind: objectKind,
    schema: needsSchema ? schema : null,
    name: kind === "database" ? database : kind === "schema" ? schema : kind === "table" || kind === "function" ? name : null,
  };
  const ready = privs.length > 0 && (needsSchema ? !!schema : true) && (kind !== "table" && kind !== "function" ? true : !!name);
  const toggle = (p: string) => setPrivs((cur) => (cur.includes(p) ? cur.filter((x) => x !== p) : [...cur, p]));

  const submit = () => {
    if (kind.startsWith("default_"))
      onNext({
        title: `設定 ${role} 的預設權限`,
        change: {
          kind: "default_privileges",
          role,
          schema,
          object: kind.replace("default_", "") as "tables" | "sequences" | "functions",
          privileges: privs,
          revoke: false,
          for_role: null,
        },
        warning: "只對「你目前登入的角色」之後在這個 schema 建立的物件有效；別的角色建立的物件不受影響。",
      });
    else
      onNext({
        title: `授權給 ${role}`,
        change: { kind: "grant", role, target, privileges: privs, grant_option: grantOption },
        warning:
          kind.startsWith("all_")
            ? "只套用到現在已經有的物件；之後新建的請另外設定「預設權限」。別忘了也要給 schema 的 USAGE。"
            : undefined,
      });
  };

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="risk-dialog plan-dialog admin-form" role="dialog">
        <h2>授權給 {role}</h2>
        <label>
          範圍
          <select value={kind} onChange={(e) => setKind(e.target.value as GrantKind)}>
            {GRANT_KINDS.map((k) => (
              <option key={k.id} value={k.id}>
                {k.label}
              </option>
            ))}
          </select>
        </label>
        {kind !== "database" && (
          <label>
            Schema
            <select value={schema} onChange={(e) => setSchema(e.target.value)}>
              {schemas.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
        )}
        {(kind === "table" || kind === "function") && (
          <label>
            {kind === "table" ? "資料表或檢視" : "函式或程序"}
            <select value={name} onChange={(e) => setName(e.target.value)}>
              <option value="">（選擇）</option>
              {names.map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </label>
        )}
        <div>
          <span>權限</span>
          {(objectKind === "table" || objectKind === "all_tables") && (
            <span className="priv-presets">
              <button type="button" className="small" onClick={() => setPrivs(["SELECT"])}>
                唯讀
              </button>
              <button type="button" className="small" onClick={() => setPrivs(["SELECT", "INSERT", "UPDATE", "DELETE"])}>
                讀寫
              </button>
              <button type="button" className="small" onClick={() => setPrivs([...available])}>
                全部
              </button>
            </span>
          )}
          <div className="pg-priv-checks">
            {available.map((p) => (
              <label key={p} className="check">
                <input type="checkbox" checked={privs.includes(p)} onChange={() => toggle(p)} /> {p}
              </label>
            ))}
          </div>
        </div>
        {!kind.startsWith("default_") && (
          <label className="check">
            <input type="checkbox" checked={grantOption} onChange={(e) => setGrantOption(e.target.checked)} />
            可以再授權給別人（WITH GRANT OPTION）
          </label>
        )}
        <div className="actions">
          <span className="spacer" />
          <button onClick={onCancel}>取消</button>
          <button className="primary" disabled={!ready} onClick={submit}>
            下一步：預覽
          </button>
        </div>
      </div>
    </div>
  );
}
