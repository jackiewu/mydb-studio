import { useState } from "react";
import { api, describeError, type ConnectionProfile, type SshAuth, type SslMode } from "../api";

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

export function ConnectionForm({ initial, onSaved, onCancel, onDelete }: Props) {
  const [p, setP] = useState<ConnectionProfile>(initial);
  const [tab, setTab] = useState<Tab>("general");
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const set = (patch: Partial<ConnectionProfile>) => setP((old) => ({ ...old, ...patch }));
  const setSsl = (patch: Partial<ConnectionProfile["ssl"]>) =>
    setP((old) => ({ ...old, ssl: { ...old.ssl, ...patch } }));
  const setSsh = (patch: Partial<NonNullable<ConnectionProfile["ssh"]>>) =>
    setP((old) => (old.ssh ? { ...old, ssh: { ...old.ssh, ...patch } } : old));
  const setAuth = (auth: SshAuth) => setSsh({ auth });

  const test = async () => {
    setBusy(true);
    setStatus(null);
    try {
      const info = await api.testConnection(p);
      const extras = [info.tls && "TLS", p.ssh && "SSH"].filter(Boolean).join("、");
      setStatus({ ok: true, text: `連線成功：${info.server_version}${extras ? `（${extras}）` : ""}` });
    } catch (e) {
      setStatus({ ok: false, text: describeError(e) });
    } finally {
      setBusy(false);
    }
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      onSaved(await api.saveConnection({ ...p, name: p.name.trim() || `${p.user}@${p.host}` }));
    } catch (err) {
      setStatus({ ok: false, text: describeError(err) });
    }
  };

  return (
    <form className="conn-form" onSubmit={save}>
      <h2>{initial.id ? "編輯連線" : "新增連線"}</h2>
      <div className="tabs">
        {(["general", "ssl", "ssh"] as Tab[]).map((t) => (
          <button type="button" key={t} className={tab === t ? "active" : ""} onClick={() => setTab(t)}>
            {t === "general" ? "一般" : t === "ssl" ? "SSL/TLS" : "SSH 通道"}
          </button>
        ))}
      </div>

      {tab === "general" && (
        <div className="fields">
          <label>
            名稱
            <input value={p.name} placeholder={`${p.user}@${p.host}`} onChange={(e) => set({ name: e.target.value })} />
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
          <label>
            使用者
            <input required value={p.user} onChange={(e) => set({ user: e.target.value })} />
          </label>
          <label>
            密碼
            <input type="password" value={p.password ?? ""} onChange={(e) => set({ password: e.target.value })} />
          </label>
          <label>
            預設資料庫（選填）
            <input value={p.database ?? ""} onChange={(e) => set({ database: e.target.value })} />
          </label>
        </div>
      )}

      {tab === "ssl" && (
        <div className="fields">
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
                <input
                  value={p.ssl.ca_path ?? ""}
                  placeholder="/path/to/ca.pem（不填則使用系統內建根憑證）"
                  onChange={(e) => setSsl({ ca_path: e.target.value })}
                />
              </label>
              <label>
                用戶端憑證路徑（選填）
                <input value={p.ssl.cert_path ?? ""} onChange={(e) => setSsl({ cert_path: e.target.value })} />
              </label>
              <label>
                用戶端私鑰路徑（選填）
                <input value={p.ssl.key_path ?? ""} onChange={(e) => setSsl({ key_path: e.target.value })} />
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
              <p className="hint">上方「主機」與「埠號」是從跳板機看出去的資料庫位址，例如 127.0.0.1:3306。</p>
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
