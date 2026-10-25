import { useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  api,
  describeError,
  KIND_LABEL,
  newProfile,
  serverLabel,
  type ConnectionKind,
  type ConnectionProfile,
  type McpAccess,
  type SslConfig,
  type SshAuth,
  type SslMode,
} from "../api";

interface Props {
  initial: ConnectionProfile;
  onSaved: (p: ConnectionProfile) => void;
  onCancel: () => void;
  onDelete?: () => void;
}

type Tab = "general" | "ssl" | "ssh";

const SSL_MODES: { value: SslMode; label: string }[] = [
  { value: "disabled", label: "不加密" },
  { value: "required", label: "加密，不驗證憑證" },
  { value: "verify_ca", label: "加密，驗證 CA" },
  { value: "verify_identity", label: "加密，驗證 CA 與主機名稱" },
];

type Cloud = NonNullable<SslConfig["cloud"]>;

const CLOUDS: { value: Cloud; label: string; mode: SslMode; hint: string }[] = [
  {
    value: "aws",
    label: "AWS RDS / Aurora",
    mode: "verify_identity",
    hint: "已套用 app 內建的 AWS RDS CA（所有區域）。「主機」請填 RDS 的 endpoint（xxx.rds.amazonaws.com），不要填 IP，否則主機名稱驗證會失敗。",
  },
  {
    value: "gcp",
    label: "Google Cloud SQL",
    mode: "verify_ca",
    hint: "到 Cloud SQL 主控台 → 連線 → 安全性，下載伺服器 CA（server-ca.pem），在下面選這個檔案。用 IP 連線所以不驗證主機名稱。如果執行個體要求用戶端憑證，再選 client-cert.pem 和 client-key.pem。",
  },
  {
    value: "azure",
    label: "Azure Database for MySQL",
    mode: "verify_identity",
    hint: "Azure 的憑證由 DigiCert 簽發，app 內建的根憑證就能驗證，CA 欄位留空即可。「主機」填 xxx.mysql.database.azure.com。",
  },
];

/** TLS presets for managed Redis. */
const REDIS_CLOUDS: { value: Cloud; label: string; mode: SslMode; hint: string }[] = [
  {
    value: "aws",
    label: "AWS ElastiCache / MemoryDB",
    mode: "verify_identity",
    hint: "需要開啟「傳輸中加密」。憑證由 Amazon 簽發，系統內建的根憑證就能驗證，CA 欄位留空即可。「主機」請填 endpoint（xxx.cache.amazonaws.com），不要填 IP。ElastiCache 只能從 VPC 內連線，通常要搭配「SSH 通道」經由同一個 VPC 的 EC2 跳板機。",
  },
  {
    value: "gcp",
    label: "GCP Memorystore for Redis",
    mode: "verify_ca",
    hint: "到 Memorystore 執行個體頁面下載伺服器 CA（server-ca.pem），在下面選這個檔案。用 IP 連線所以不驗證主機名稱。有開啟 AUTH 的話，把 AUTH 字串填在「一般」頁的密碼欄。",
  },
];

/** The cloud a host name points at, to suggest its settings. */
function cloudOf(host: string, kind: ConnectionKind): Cloud | null {
  const h = host.trim().toLowerCase();
  if (kind === "redis") return h.endsWith(".cache.amazonaws.com") || h.includes(".memorydb.") ? "aws" : null;
  if (h.endsWith(".rds.amazonaws.com") || h.endsWith(".rds.amazonaws.com.cn")) return "aws";
  if (h.endsWith(".database.azure.com")) return "azure";
  return null;
}

/** The parts of a profile that decide which server and login a list is for. */
const loginKey = (p: ConnectionProfile) => JSON.stringify([p.host, p.port, p.user, p.password, p.ssl, p.ssh]);

export function ConnectionForm({ initial, onSaved, onCancel, onDelete }: Props) {
  // A new connection starts by picking what kind of server it is.
  const [p, setP] = useState<ConnectionProfile>(initial.kind || initial.id ? initial : newProfile());
  const [choosing, setChoosing] = useState(!initial.id && !initial.kind);
  const redis = p.kind === "redis";
  const clouds = redis ? REDIS_CLOUDS : CLOUDS;
  const [tab, setTab] = useState<Tab>("general");
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const set = (patch: Partial<ConnectionProfile>) => setP((old) => ({ ...old, ...patch }));
  const setSsl = (patch: Partial<ConnectionProfile["ssl"]>) =>
    setP((old) => ({ ...old, ssl: { ...old.ssl, ...patch } }));
  const setSsh = (patch: Partial<NonNullable<ConnectionProfile["ssh"]>>) =>
    setP((old) => (old.ssh ? { ...old, ssh: { ...old.ssh, ...patch } } : old));
  const setAuth = (auth: SshAuth) => setSsh({ auth });

  // Databases on the server, offered for the default database.
  const [databases, setDatabases] = useState<{ key: string; names: string[]; error?: string } | null>(null);
  const loading = useRef<string | null>(null);
  const loadDatabases = async (force = false) => {
    const key = loginKey(p);
    if (!p.host.trim() || !p.user.trim() || loading.current === key) return;
    if (!force && databases?.key === key) return;
    loading.current = key;
    try {
      const names = await api.listServerDatabases(p);
      setDatabases({ key, names });
    } catch (e) {
      setDatabases({ key, names: [], error: describeError(e) });
    } finally {
      loading.current = null;
    }
  };
  const dbList = databases?.key === loginKey(p) ? databases : null;
  const [dbOpen, setDbOpen] = useState(false);
  const typed = (p.database ?? "").trim().toLowerCase();
  const allNames = dbList?.names ?? [];
  // Everything while the field holds a full name, otherwise what matches.
  const dbChoices =
    !typed || allNames.some((n) => n.toLowerCase() === typed)
      ? allNames
      : allNames.filter((n) => n.toLowerCase().includes(typed));

  const applyCloud = async (cloud: Cloud | null) => {
    if (!cloud) return setSsl({ cloud: null });
    const preset = clouds.find((c) => c.value === cloud)!;
    const ca_path = redis
      ? cloud === "aws"
        ? null
        : p.ssl.ca_path?.includes("aws-rds-global-bundle")
          ? null
          : (p.ssl.ca_path ?? null)
      : cloud === "aws"
        ? await api.awsRdsCaPath().catch(() => p.ssl.ca_path ?? null)
        : cloud === "azure"
          ? null
          : p.ssl.ca_path?.includes("aws-rds-global-bundle")
            ? null
            : (p.ssl.ca_path ?? null);
    setSsl({ cloud, mode: preset.mode, ca_path });
  };
  const suggestedCloud = !p.ssl.cloud ? cloudOf(p.host, p.kind ?? "mysql") : null;

  const pickFile = async (field: "ca_path" | "cert_path" | "key_path") => {
    const picked = await openDialog({
      multiple: false,
      filters: [
        { name: "憑證 / 金鑰", extensions: ["pem", "crt", "cer", "key", "der"] },
        { name: "所有檔案", extensions: ["*"] },
      ],
    });
    if (typeof picked === "string") setSsl({ [field]: picked });
  };

  const test = async () => {
    setBusy(true);
    setStatus(null);
    try {
      const info = await api.testConnection(p);
      const extras = [info.tls && "TLS", p.ssh && "SSH"].filter(Boolean).join("、");
      setStatus({ ok: true, text: `連線成功：${info.server_version}${extras ? `（${extras}）` : ""}` });
      if (!redis) loadDatabases();
    } catch (e) {
      setStatus({ ok: false, text: describeError(e) });
    } finally {
      setBusy(false);
    }
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      onSaved(await api.saveConnection({ ...p, name: p.name.trim() || serverLabel(p) }));
    } catch (err) {
      setStatus({ ok: false, text: describeError(err) });
    }
  };

  if (choosing) {
    return (
      <div className="conn-form">
        <h2>新增連線</h2>
        <p className="hint">先選擇要連線的資料庫類型：</p>
        <div className="kind-choices">
          <button
            type="button"
            className="kind-choice"
            autoFocus
            onClick={() => (setP(newProfile("mysql")), setChoosing(false))}
          >
            <strong>MySQL / MariaDB</strong>
            <span className="muted">SQL 編輯器、資料表瀏覽、Explain、伺服器管理</span>
          </button>
          <button type="button" className="kind-choice" onClick={() => (setP(newProfile("redis")), setChoosing(false))}>
            <strong>Redis</strong>
            <span className="muted">Key 瀏覽（SCAN）、值的檢視、指令列</span>
          </button>
        </div>
        <div className="actions">
          <span className="spacer" />
          <button type="button" onClick={onCancel}>
            取消
          </button>
        </div>
      </div>
    );
  }

  return (
    <form className="conn-form" onSubmit={save}>
      <h2>
        {initial.id ? "編輯連線" : "新增連線"} <span className={`badge kind-${p.kind ?? "mysql"}`}>{KIND_LABEL[p.kind ?? "mysql"]}</span>
        {!initial.id && !initial.kind && (
          <button type="button" className="link small-text" onClick={() => setChoosing(true)}>
            換類型
          </button>
        )}
      </h2>
      <div className="tabs">
        {(["general", "ssl", "ssh"] as Tab[]).map((t) => (
          <button type="button" key={t} className={tab === t ? "active" : ""} onClick={() => setTab(t)}>
            {t === "general" ? "一般" : t === "ssl" ? (redis ? "TLS" : "SSL/TLS") : "SSH 通道"}
          </button>
        ))}
      </div>

      {tab === "general" && (
        <div className="fields">
          <label>
            名稱
            <input value={p.name} placeholder={serverLabel(p)} onChange={(e) => set({ name: e.target.value })} />
          </label>
          <div className="row">
            <label className="grow">
              主機
              <input required value={p.host} onChange={(e) => set({ host: e.target.value })} />
            </label>
            <label className="port">
              埠號
              <input
                type="number"
                required
                value={p.port}
                onChange={(e) => set({ port: Number(e.target.value) })}
              />
            </label>
          </div>
          {redis ? (
            <label>
              使用者名稱（選填）
              <input value={p.user} placeholder="default" onChange={(e) => set({ user: e.target.value })} />
              <span className="field-note">Redis 6 以上的 ACL 使用者；只設了 requirepass 的話留空，只填密碼。</span>
            </label>
          ) : (
          <label>
            使用者
            <input required value={p.user} onChange={(e) => set({ user: e.target.value })} />
          </label>
          )}
          <label>
            密碼
            <input type="password" value={p.password ?? ""} onChange={(e) => set({ password: e.target.value })} />
            <span className="field-note">macOS 與 Windows 上存在系統鑰匙圈，不會寫進 app 的資料庫檔案。</span>
          </label>
          {redis ? (
            <label>
              預設資料庫
              <select value={p.redis_db ?? 0} onChange={(e) => set({ redis_db: Number(e.target.value) })}>
                {Array.from({ length: 16 }, (_, i) => (
                  <option key={i} value={i}>
                    db{i}
                  </option>
                ))}
              </select>
              <span className="field-note">連線後先選這個資料庫（SELECT）。叢集模式只有 db0。</span>
            </label>
          ) : (
          <>
          <div className="field-group db-field">
            <span>預設資料庫（選填）</span>
            <div className="input-with-button">
              <input
                value={p.database ?? ""}
                placeholder="點一下載入伺服器上的資料庫，或直接輸入"
                onFocus={() => {
                  setDbOpen(true);
                  loadDatabases();
                }}
                onBlur={() => setTimeout(() => setDbOpen(false), 150)}
                onChange={(e) => (set({ database: e.target.value }), setDbOpen(true))}
              />
              <button type="button" className="small" onClick={() => loadDatabases(true)} title="重新讀取資料庫清單">
                ↻
              </button>
            </div>
            {dbOpen && dbChoices.length > 0 && (
              <ul className="db-suggest">
                {dbChoices.map((n) => (
                  <li key={n}>
                    <button
                      type="button"
                      className={n === p.database ? "active" : undefined}
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => (set({ database: n }), setDbOpen(false))}
                    >
                      {n}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {dbList?.error && <span className="field-note bad">讀不到資料庫清單：{dbList.error}</span>}
            {dbList && !dbList.error && (
              <span className="field-note">伺服器上有 {dbList.names.length} 個資料庫，點輸入框就能選。</span>
            )}
          </div>
          <label>
            AI 工具（MCP）
            <select value={p.mcp ?? "read_only"} onChange={(e) => set({ mcp: e.target.value as McpAccess })}>
              <option value="read_only">唯讀：只能查詢（SELECT、SHOW、EXPLAIN）</option>
              <option value="read_write">可寫入：可以新增、修改、刪除資料和改結構</option>
              <option value="off">關閉：AI 工具看不到這個連線</option>
            </select>
            <span className="field-note">
              {p.mcp === "read_write"
                ? "⚠️ 接上 MCP 的 AI 工具（例如 Claude）可以直接修改這個資料庫。每次執行都會留在紀錄清單。"
                : "接上 MCP 的 AI 工具（例如 Claude）能用這個連線做什麼。查詢結果會傳給該 AI 工具；每次執行都會留在紀錄清單。"}
            </span>
          </label>
          </>
          )}
        </div>
      )}

      {tab === "ssl" && (
        <div className="fields">
          <label>
            雲端服務
            <select value={p.ssl.cloud ?? ""} onChange={(e) => applyCloud((e.target.value || null) as Cloud | null)}>
              <option value="">自己架設 / 其他</option>
              {clouds.map((c) => (
                <option key={c.value} value={c.value}>
                  {c.label}
                </option>
              ))}
            </select>
          </label>
          {suggestedCloud && (
            <p className="hint">
              主機看起來是 {clouds.find((c) => c.value === suggestedCloud)!.label}。
              <button type="button" className="link" onClick={() => applyCloud(suggestedCloud)}>
                套用建議設定
              </button>
            </p>
          )}
          {p.ssl.cloud && <p className="hint">{clouds.find((c) => c.value === p.ssl.cloud)?.hint}</p>}
          <label>
            模式
            <select value={p.ssl.mode} onChange={(e) => setSsl({ mode: e.target.value as SslMode })}>
              {SSL_MODES.map((m) => (
                <option key={m.value} value={m.value}>
                  {m.label}
                </option>
              ))}
            </select>
          </label>
          {p.ssl.mode !== "disabled" && (
            <>
              <label>
                CA 憑證路徑
                <div className="input-with-button">
                  <input
                    value={p.ssl.ca_path ?? ""}
                    placeholder="/path/to/ca.pem（不填則使用內建的公開根憑證）"
                    onChange={(e) => setSsl({ ca_path: e.target.value })}
                  />
                  <button type="button" className="small" onClick={() => pickFile("ca_path")}>
                    選擇…
                  </button>
                </div>
              </label>
              <label>
                用戶端憑證路徑（選填）
                <div className="input-with-button">
                  <input value={p.ssl.cert_path ?? ""} onChange={(e) => setSsl({ cert_path: e.target.value })} />
                  <button type="button" className="small" onClick={() => pickFile("cert_path")}>
                    選擇…
                  </button>
                </div>
              </label>
              <label>
                用戶端私鑰路徑（選填）
                <div className="input-with-button">
                  <input value={p.ssl.key_path ?? ""} onChange={(e) => setSsl({ key_path: e.target.value })} />
                  <button type="button" className="small" onClick={() => pickFile("key_path")}>
                    選擇…
                  </button>
                </div>
              </label>
            </>
          )}
        </div>
      )}

      {tab === "ssh" && (
        <div className="fields">
          <label className="check">
            <input
              type="checkbox"
              checked={!!p.ssh}
              onChange={(e) =>
                set({
                  ssh: e.target.checked
                    ? { host: "", port: 22, user: "", auth: { method: "key", key_path: "~/.ssh/id_ed25519" } }
                    : null,
                })
              }
            />
            透過 SSH 跳板機連線
          </label>
          {p.ssh && (
            <>
              <p className="hint">
                上方「主機」與「埠號」是從跳板機看出去的{redis ? " Redis " : "資料庫"}位址，例如{" "}
                {redis ? "127.0.0.1:6379 或 ElastiCache 的 endpoint" : "127.0.0.1:3306"}。使用 TLS 時會用這個主機名稱驗證憑證。
              </p>
              <div className="row">
                <label className="grow">
                  SSH 主機
                  <input required value={p.ssh.host} onChange={(e) => setSsh({ host: e.target.value })} />
                </label>
                <label className="port">
                  埠號
                  <input
                    type="number"
                    value={p.ssh.port}
                    onChange={(e) => setSsh({ port: Number(e.target.value) })}
                  />
                </label>
              </div>
              <label>
                SSH 使用者
                <input required value={p.ssh.user} onChange={(e) => setSsh({ user: e.target.value })} />
              </label>
              <label>
                驗證方式
                <select
                  value={p.ssh.auth.method}
                  onChange={(e) =>
                    setAuth(
                      e.target.value === "password"
                        ? { method: "password", password: "" }
                        : { method: "key", key_path: "~/.ssh/id_ed25519" },
                    )
                  }
                >
                  <option value="key">私鑰</option>
                  <option value="password">密碼</option>
                </select>
              </label>
              {p.ssh.auth.method === "password" ? (
                <label>
                  SSH 密碼
                  <input
                    type="password"
                    value={p.ssh.auth.password}
                    onChange={(e) => setAuth({ method: "password", password: e.target.value })}
                  />
                </label>
              ) : (
                <>
                  <label>
                    私鑰路徑
                    <input
                      value={p.ssh.auth.key_path}
                      onChange={(e) =>
                        setAuth({ ...(p.ssh!.auth as Extract<SshAuth, { method: "key" }>), key_path: e.target.value })
                      }
                    />
                  </label>
                  <label>
                    私鑰密碼（選填）
                    <input
                      type="password"
                      value={p.ssh.auth.passphrase ?? ""}
                      onChange={(e) =>
                        setAuth({ ...(p.ssh!.auth as Extract<SshAuth, { method: "key" }>), passphrase: e.target.value })
                      }
                    />
                  </label>
                </>
              )}
              <div className="fingerprint">
                主機金鑰指紋：
                {p.ssh.known_host_fingerprint ? (
                  <>
                    <code>{p.ssh.known_host_fingerprint}</code>
                    <button type="button" className="link" onClick={() => setSsh({ known_host_fingerprint: null })}>
                      清除
                    </button>
                  </>
                ) : (
                  <span>尚未記錄，第一次連線時會自動記住</span>
                )}
              </div>
            </>
          )}
        </div>
      )}

      {status && <div className={status.ok ? "status ok" : "status bad"}>{status.text}</div>}

      <div className="actions">
        {onDelete && (
          <button type="button" className="danger" onClick={onDelete}>
            刪除
          </button>
        )}
        <span className="spacer" />
        <button type="button" onClick={test} disabled={busy}>
          {busy ? "測試中…" : "測試連線"}
        </button>
        <button type="button" onClick={onCancel}>
          取消
        </button>
        <button type="submit" className="primary">
          儲存
        </button>
      </div>
    </form>
  );
}
