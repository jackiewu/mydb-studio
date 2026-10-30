import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import {
  api,
  describeError,
  KIND_LABEL,
  serverLabel,
  type ConnectionProfile,
  type ImportAction,
  type ImportCandidate,
  type ImportPreview,
} from "../api";

const NO_SECRETS =
  "匯出檔不含資料庫密碼、SSH 密碼和金鑰密語，也不含金鑰檔本身（只有路徑）。主機位址和帳號仍在檔案裡，請妥善保管。";

/** Picks connections and writes them to a file, without their passwords. */
export function ExportDialog({
  connections,
  onClose,
  onDone,
}: {
  connections: ConnectionProfile[];
  onClose: () => void;
  onDone: (message: string) => void;
}) {
  const [picked, setPicked] = useState(() => new Set(connections.map((c) => c.id)));
  const [error, setError] = useState<string | null>(null);
  const all = picked.size === connections.length;

  const toggle = (id: string) =>
    setPicked((s) => {
      const next = new Set(s);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const run = async () => {
    const day = new Date().toISOString().slice(0, 10);
    const path = await save({
      defaultPath: `mydb-connections-${day}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (!path) return;
    try {
      // Keep the list's order rather than the order of clicks.
      const ids = connections.filter((c) => picked.has(c.id)).map((c) => c.id);
      const n = await api.exportConnections(ids, path);
      onDone(`已匯出 ${n} 個連線到 ${path}`);
    } catch (e) {
      setError(describeError(e));
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal transfer-dialog" role="dialog" aria-label="匯出連線">
        <h2>匯出連線</h2>
        <p className="muted">{NO_SECRETS}</p>
        <label className="check">
          <input
            type="checkbox"
            checked={all}
            onChange={() => setPicked(all ? new Set() : new Set(connections.map((c) => c.id)))}
          />
          全選（{picked.size}/{connections.length}）
        </label>
        <ul className="transfer-list">
          {connections.map((c) => (
            <li key={c.id}>
              <label className="check">
                <input type="checkbox" checked={picked.has(c.id)} onChange={() => toggle(c.id)} />
                <ConnectionLine c={c} />
              </label>
            </li>
          ))}
        </ul>
        {error && <p className="error">{error}</p>}
        <div className="modal-actions">
          <button onClick={onClose}>取消</button>
          <button className="primary" disabled={picked.size === 0} onClick={run}>
            匯出 {picked.size} 個連線…
          </button>
        </div>
      </div>
    </div>
  );
}

function ConnectionLine({ c }: { c: ConnectionProfile }) {
  return (
    <span className="transfer-line">
      <strong>{c.name}</strong>
      <span className="muted">{serverLabel(c)}</span>
      <span className={`badge kind-${c.kind ?? "mysql"}`}>{KIND_LABEL[c.kind ?? "mysql"]}</span>
      {c.ssh && <span className="badge good">SSH</span>}
      {c.ssl.mode !== "disabled" && <span className="badge good">TLS</span>}
    </span>
  );
}

type Choice = ImportAction | "skip";

function defaultChoice(c: ImportCandidate): Choice {
  if (!c.clash) return "new";
  // Already here (exported from this app, or imported before): leave it.
  return c.clash.same_id ? "skip" : "copy";
}

/** What ticking a skipped connection means: updating it when it's already here. */
function includedChoice(c: ImportCandidate): Choice {
  const d = defaultChoice(c);
  return d === "skip" ? "replace" : d;
}

/**
 * Shows what an export file holds, lets the user settle name clashes, saves
 * the picked connections and then points at the ones needing a password.
 */
export function ImportDialog({
  preview,
  onClose,
  onImported,
  onEdit,
}: {
  preview: ImportPreview;
  onClose: () => void;
  /** Saved: the home list should reload. */
  onImported: () => void;
  /** Opens the connection form for an imported connection. */
  onEdit: (c: ConnectionProfile) => void;
}) {
  const [choices, setChoices] = useState<Choice[]>(() => preview.connections.map(defaultChoice));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [imported, setImported] = useState<ConnectionProfile[] | null>(null);
  const [visited, setVisited] = useState<Set<string>>(new Set());
  const count = choices.filter((c) => c !== "skip").length;

  const setChoice = (i: number, c: Choice) => setChoices((cs) => cs.map((x, j) => (j === i ? c : x)));

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const items = preview.connections.flatMap((c, i) => {
        const action = choices[i];
        return action === "skip"
          ? []
          : [{ profile: c.profile, action, target: action === "replace" ? c.clash?.id : undefined }];
      });
      setImported(await api.importConnections(items));
      onImported();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };

  if (imported) {
    return (
      <div className="modal-backdrop">
        <div className="modal transfer-dialog" role="dialog" aria-label="匯入連線">
          <h2>已匯入 {imported.length} 個連線</h2>
          {imported.length > 0 && (
            <>
              <p className="muted">
                匯出檔不含密碼。需要密碼的連線，請按「輸入密碼」補上；取代現有連線且伺服器沒變的，原本存的密碼會保留。
              </p>
              <ul className="transfer-list">
                {imported.map((c) => (
                  <li key={c.id} className="transfer-line">
                    <ConnectionLine c={c} />
                    <span className="spacer" />
                    {visited.has(c.id) && <span className="muted small-text">已開啟</span>}
                    <button
                      className="small"
                      onClick={() => {
                        setVisited((v) => new Set(v).add(c.id));
                        onEdit(c);
                      }}
                    >
                      輸入密碼…
                    </button>
                  </li>
                ))}
              </ul>
            </>
          )}
          <div className="modal-actions">
            <button className="primary" onClick={onClose}>
              完成
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="modal-backdrop">
      <div className="modal transfer-dialog" role="dialog" aria-label="匯入連線">
        <h2>匯入連線</h2>
        {preview.connections.length === 0 && <p>檔案裡沒有可以匯入的連線。</p>}
        <ul className="transfer-list">
          {preview.connections.map((c, i) => (
            <li key={i}>
              <div className="transfer-line">
                <label className="check">
                  <input
                    type="checkbox"
                    checked={choices[i] !== "skip"}
                    onChange={(e) => setChoice(i, e.target.checked ? includedChoice(c) : "skip")}
                  />
                  <ConnectionLine c={c.profile} />
                </label>
              </div>
              {c.clash && (
                <div className="transfer-note">
                  {c.clash.same_id ? "這個連線已經在這台電腦上" : `已經有同名的連線「${c.clash.name}」`}：
                  <select value={choices[i]} onChange={(e) => setChoice(i, e.target.value as Choice)}>
                    <option value="skip">略過</option>
                    <option value="replace">取代現有的</option>
                    <option value="copy">兩個都留（新的加上編號）</option>
                  </select>
                </div>
              )}
              {c.missing_files.length > 0 && choices[i] !== "skip" && (
                <div className="transfer-note warn">
                  這台電腦找不到：{c.missing_files.join("、")}。匯入後請把檔案複製過來，或在連線設定裡改路徑。
                </div>
              )}
            </li>
          ))}
        </ul>
        {preview.skipped.length > 0 && (
          <p className="muted small-text">有 {preview.skipped.length} 筆讀不懂，已略過：{preview.skipped.join("；")}</p>
        )}
        <p className="muted small-text">不會匯入任何密碼，匯入後再輸入。</p>
        {error && <p className="error">{error}</p>}
        <div className="modal-actions">
          <button onClick={onClose}>取消</button>
          <button className="primary" disabled={count === 0 || busy} onClick={run}>
            匯入 {count} 個連線
          </button>
        </div>
      </div>
    </div>
  );
}
