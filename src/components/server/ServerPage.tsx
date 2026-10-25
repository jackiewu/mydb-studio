import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, describeError, type AdminChange, type ProcessInfo, type ServerOverview } from "../../api";
import { openMenu } from "../../contextMenu";
import { copyText } from "../../copy";
import { PlanDialog } from "./PlanDialog";
import { UsersPanel } from "./UsersPanel";

type Tab = "overview" | "status" | "variables" | "processes" | "users";

const TABS: { id: Tab; label: string }[] = [
  { id: "overview", label: "總覽" },
  { id: "processes", label: "執行中的查詢" },
  { id: "users", label: "使用者與權限" },
  { id: "status", label: "狀態變數" },
  { id: "variables", label: "系統變數" },
];

interface Props {
  sessionId: string;
  /** This app's own connection, which must not be killed from here. */
  connectionId: number;
  mariadb: boolean;
  onOpenSql: (sql: string) => void;
}

export function ServerPage({ sessionId, connectionId, mariadb, onOpenSql }: Props) {
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
        {(tab === "overview" || tab === "status" || tab === "variables") && (
          <OverviewTabs sessionId={sessionId} tab={tab} />
        )}
        {tab === "processes" && <Processes sessionId={sessionId} ownId={connectionId} onOpenSql={onOpenSql} />}
        {tab === "users" && <UsersPanel sessionId={sessionId} mariadb={mariadb} />}
      </div>
    </div>
  );
}

/** Calls `load` now and, while `on`, every `ms`. */
export function usePolling(load: () => void, on: boolean, ms: number) {
  const ref = useRef(load);
  ref.current = load;
  useEffect(() => {
    ref.current();
  }, []);
  useEffect(() => {
    if (!on) return;
    const id = setInterval(() => ref.current(), ms);
    return () => clearInterval(id);
  }, [on, ms]);
}

export function duration(seconds: number) {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d) return `${d} 天 ${h} 小時`;
  if (h) return `${h} 小時 ${m} 分`;
  return `${m} 分 ${seconds % 60} 秒`;
}

export function bytes(n: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n < 10 && i ? 1 : 0)} ${units[i]}`;
}

export function pct(n: number) {
  return `${(n * 100).toFixed(n > 0.999 && n < 1 ? 2 : 1)}%`;
}

interface Card {
  title: string;
  value: string;
  detail?: string;
  level?: "good" | "warn" | "bad";
  hint: string;
}

/** The numbers worth a glance, with a word on when they are a problem. */
function cards(o: ServerOverview, prev: { at: number; o: ServerOverview } | null, now: number): Card[] {
  const s = (k: string) => Number(o.status[k] ?? o.status[k.toUpperCase()] ?? 0);
  const v = (k: string) => o.variables[k] ?? "";
  const out: Card[] = [];
  const uptime = s("Uptime");
  out.push({
    title: "版本",
    value: o.version,
    detail: v("version_comment"),
    hint: "伺服器版本與發行版。",
  });
  out.push({
    title: "已運行",
    value: duration(uptime),
    detail: `目前登入：${o.current_user}`,
    hint: "距離上次重新啟動的時間。",
  });

  const conns = s("Threads_connected");
  const max = Number(v("max_connections")) || 0;
  const ratio = max ? conns / max : 0;
  out.push({
    title: "連線數",
    value: `${conns} / ${max}`,
    detail: `歷史最高 ${s("Max_used_connections")}，執行中 ${s("Threads_running")}`,
    level: ratio > 0.85 ? "bad" : ratio > 0.6 ? "warn" : "good",
    hint: "目前連線數／max_connections。接近上限時新連線會被拒絕（Too many connections）。",
  });

  let qps = uptime ? s("Questions") / uptime : 0;
  let qpsNote = "啟動以來平均";
  if (prev) {
    const dt = (now - prev.at) / 1000;
    const dq = s("Questions") - Number(prev.o.status["Questions"] ?? 0);
    if (dt > 0 && dq >= 0) {
      qps = dq / dt;
      qpsNote = `最近 ${Math.round(dt)} 秒`;
    }
  }
  out.push({
    title: "每秒查詢",
    value: qps.toFixed(qps < 10 ? 1 : 0),
    detail: qpsNote,
    hint: "Questions 的增加速度，也就是 client 每秒送進來的語句數。",
  });

  const slow = s("Slow_queries");
  out.push({
    title: "慢查詢",
    value: slow.toLocaleString(),
    detail: `slow_query_log ${v("slow_query_log") || "?"}，門檻 ${Number(v("long_query_time") || 0)} 秒`,
    level: slow > 0 ? "warn" : "good",
    hint: "執行超過 long_query_time 的查詢數。打開 slow_query_log 才會記下是哪些語句。",
  });

  const reads = s("Innodb_buffer_pool_reads");
  const requests = s("Innodb_buffer_pool_read_requests");
  if (requests) {
    const hit = 1 - reads / requests;
    out.push({
      title: "InnoDB 快取命中率",
      value: pct(hit),
      detail: `buffer pool ${bytes(Number(v("innodb_buffer_pool_size")) || 0)}${requests < 100000 ? "，樣本還少" : ""}`,
      // Right after a restart the cache is cold and the ratio means little.
      level: requests < 100000 ? undefined : hit < 0.95 ? "bad" : hit < 0.99 ? "warn" : "good",
      hint: "讀取時資料已在記憶體的比例。偏低代表 innodb_buffer_pool_size 可能太小，常常要讀硬碟。",
    });
  }

  const tmp = s("Created_tmp_tables");
  const tmpDisk = s("Created_tmp_disk_tables");
  if (tmp) {
    const r = tmpDisk / tmp;
    out.push({
      title: "暫存表寫到硬碟",
      value: pct(r),
      detail: `${tmpDisk.toLocaleString()} / ${tmp.toLocaleString()}`,
      level: r > 0.25 ? "warn" : "good",
      hint: "GROUP BY、ORDER BY、子查詢產生的暫存表有多少放不進記憶體。偏高時可檢查查詢或調大 tmp_table_size。",
    });
  }

  const aborted = s("Aborted_connects");
  out.push({
    title: "連線失敗",
    value: aborted.toLocaleString(),
    detail: `中斷的連線 ${s("Aborted_clients").toLocaleString()}`,
    level: aborted > 100 ? "warn" : undefined,
    hint: "Aborted_connects 是登入失敗（密碼錯、權限不足、逾時）；Aborted_clients 是沒好好關閉的連線。",
  });

  out.push({
    title: "流量",
    value: `↓ ${bytes(s("Bytes_received"))}　↑ ${bytes(s("Bytes_sent"))}`,
    detail: "啟動以來收到／送出",
    hint: "伺服器收到與送出的位元組總數。",
  });

  out.push({
    title: "字元集",
    value: v("character_set_server"),
    detail: v("collation_server"),
    level: v("character_set_server").startsWith("utf8mb4") ? undefined : "warn",
    hint: "新資料庫預設的字元集。建議 utf8mb4，才能存 emoji 和所有中文字。",
  });

  out.push({
    title: "時區",
    value: v("time_zone") === "SYSTEM" ? `SYSTEM（${v("system_time_zone")}）` : v("time_zone"),
    hint: "NOW() 與 TIMESTAMP 欄位使用的時區。",
  });

  const ro = v("read_only");
  if (ro)
    out.push({
      title: "唯讀",
      value: ro,
      detail: v("log_bin") === "ON" ? "binlog 已開啟" : "binlog 未開啟",
      level: ro === "ON" ? "warn" : undefined,
      hint: "read_only 為 ON 時只有 SUPER 權限可以寫入，通常是複寫的從庫。",
    });
  return out;
}

function OverviewTabs({ sessionId, tab }: { sessionId: string; tab: "overview" | "status" | "variables" }) {
  const [data, setData] = useState<{ at: number; o: ServerOverview } | null>(null);
  const [prev, setPrev] = useState<{ at: number; o: ServerOverview } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [auto, setAuto] = useState(false);
  const [filter, setFilter] = useState("");

  const load = useCallback(() => {
    api.serverOverview(sessionId).then(
      (o) => {
        setData((cur) => {
          setPrev(cur);
          return { at: Date.now(), o };
        });
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  usePolling(load, auto, 5000);

  const rows = useMemo(() => {
    if (!data || tab === "overview") return [];
    const map = tab === "status" ? data.o.status : data.o.variables;
    const f = filter.trim().toLowerCase();
    return Object.entries(map).filter(([k, v]) => !f || k.toLowerCase().includes(f) || v.toLowerCase().includes(f));
  }, [data, tab, filter]);

  return (
    <>
      <div className="filters">
        {tab !== "overview" && (
          <input
            autoFocus
            placeholder={tab === "status" ? "搜尋狀態變數，例如 innodb、threads" : "搜尋系統變數，例如 timeout、buffer"}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        )}
        {tab === "overview" && <span className="muted small-text">滑到卡片上可以看說明。</span>}
        <span className="spacer" />
        <label className="inline check">
          <input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} /> 每 5 秒更新
        </label>
        <button onClick={load}>↻ 重新整理</button>
      </div>
      {error && <div className="error-box">{error}</div>}
      {data && tab === "overview" && (
        <div className="server-cards">
          {cards(data.o, prev, data.at).map((c) => (
            <div key={c.title} className={`server-card ${c.level ?? ""}`} title={c.hint}>
              <div className="server-card-title">{c.title}</div>
              <div className="server-card-value">{c.value || "—"}</div>
              {c.detail && <div className="server-card-detail">{c.detail}</div>}
            </div>
          ))}
        </div>
      )}
      {data && tab !== "overview" && (
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
                <tr
                  key={k}
                  onContextMenu={(e) =>
                    openMenu(e, [
                      { label: "複製名稱", onClick: () => copyText(k) },
                      { label: "複製值", onClick: () => copyText(v) },
                    ])
                  }
                >
                  <td>{k}</td>
                  {/* Counters read better grouped; settings are shown exactly as set. */}
                  <td className={tab === "status" && /^\d+$/.test(v) ? "num" : undefined} title={v}>
                    {tab === "status" && /^\d+$/.test(v) ? Number(v).toLocaleString() : v}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {rows.length === 0 && <p className="empty-state">沒有符合的變數</p>}
        </div>
      )}
    </>
  );
}

function Processes({
  sessionId,
  ownId,
  onOpenSql,
}: {
  sessionId: string;
  ownId: number;
  onOpenSql: (sql: string) => void;
}) {
  const [list, setList] = useState<ProcessInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [auto, setAuto] = useState(true);
  const [hideSleep, setHideSleep] = useState(true);
  const [filter, setFilter] = useState("");
  const [kill, setKill] = useState<{
    p: ProcessInfo;
    change: AdminChange;
  } | null>(null);

  const load = useCallback(() => {
    api.serverProcesses(sessionId).then(
      (l) => {
        setList(l);
        setError(null);
      },
      (e) => setError(describeError(e)),
    );
  }, [sessionId]);
  usePolling(load, auto && !kill, 3000);

  const f = filter.trim().toLowerCase();
  const shown = (list ?? [])
    .filter((p) => !hideSleep || (p.command !== "Sleep" && p.command !== "Daemon"))
    .filter(
      (p) => !f || [p.user, p.host, p.db ?? "", p.info ?? "", p.state ?? ""].some((x) => x.toLowerCase().includes(f)),
    )
    .sort((a, b) => b.time - a.time);

  const askKill = (p: ProcessInfo, queryOnly: boolean) =>
    setKill({ p, change: { kind: "kill", id: p.id, query_only: queryOnly } });

  return (
    <>
      <div className="filters">
        <input
          placeholder="搜尋使用者、來源、資料庫或 SQL"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <label className="inline check">
          <input type="checkbox" checked={hideSleep} onChange={(e) => setHideSleep(e.target.checked)} /> 隱藏閒置連線
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
              <th>Id</th>
              <th>使用者</th>
              <th>來源</th>
              <th>資料庫</th>
              <th>狀態</th>
              <th>秒數</th>
              <th>SQL</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {shown.map((p) => {
              const own = p.id === ownId;
              return (
                <tr
                  key={p.id}
                  className={own ? "current" : undefined}
                  onContextMenu={(e) =>
                    openMenu(e, [
                      {
                        label: "複製 SQL",
                        disabled: !p.info,
                        onClick: () => copyText(p.info ?? ""),
                      },
                      {
                        label: "在新分頁開啟 SQL",
                        disabled: !p.info,
                        onClick: () => onOpenSql(p.info ?? ""),
                      },
                      "sep",
                      {
                        label: "中止這個查詢（KILL QUERY）",
                        disabled: own || p.command === "Sleep",
                        onClick: () => askKill(p, true),
                      },
                      {
                        label: "中斷這個連線（KILL）",
                        danger: true,
                        disabled: own,
                        onClick: () => askKill(p, false),
                      },
                    ])
                  }
                >
                  <td className="num">{p.id}</td>
                  <td>{p.user}</td>
                  <td>{p.host}</td>
                  <td>{p.db ?? ""}</td>
                  <td title={p.state ?? ""}>{p.command === "Query" ? p.state || "Query" : p.command}</td>
                  <td className={p.time > 60 && p.command === "Query" ? "num warn" : "num"}>{p.time}</td>
                  <td className="process-sql" title={p.info ?? ""}>
                    {own ? <span className="muted">（MyDB Studio 本身）</span> : (p.info ?? "")}
                  </td>
                  <td>
                    {!own && p.command === "Query" && (
                      <button
                        className="small"
                        onClick={() => askKill(p, true)}
                        title="KILL QUERY：只中止這句，連線保留"
                      >
                        中止
                      </button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {list && shown.length === 0 && (
          <p className="empty-state">{hideSleep ? "目前沒有正在執行的查詢" : "沒有符合的連線"}</p>
        )}
      </div>
      <p className="hint server-foot">沒有 PROCESS 權限時只看得到自己帳號的連線。右鍵可以複製 SQL 或中斷整個連線。</p>
      {kill && (
        <PlanDialog
          sessionId={sessionId}
          title={kill.change.kind === "kill" && kill.change.query_only ? "中止查詢" : "中斷連線"}
          change={kill.change}
          danger={kill.change.kind === "kill" && !kill.change.query_only}
          warning={
            kill.change.kind === "kill" && kill.change.query_only
              ? `中止 ${kill.p.user}@${kill.p.host} 正在執行的語句（已跑 ${kill.p.time} 秒）。若在交易中，該語句的變更會回復，連線保留。`
              : `中斷 ${kill.p.user}@${kill.p.host} 的整個連線。未提交的交易會回復，對方程式會收到連線中斷的錯誤。`
          }
          onCancel={() => setKill(null)}
          onDone={() => {
            setKill(null);
            load();
          }}
        />
      )}
    </>
  );
}
