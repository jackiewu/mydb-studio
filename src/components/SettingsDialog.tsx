import { useEffect, useState, type ReactNode } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api, type AiStatus, type AppSettings, type ConnectionProfile, type McpSetup, type ModelFile } from "../api";
import { copyText } from "../copy";
import { EDITOR_THEMES } from "../editor/themes";

interface Props {
  settings: AppSettings;
  /** Applied (and saved) right away, so changes show while the dialog is open. */
  onChange: (settings: AppSettings) => void;
  onClose: () => void;
  onOpenSnippets: () => void;
  /** Section to open on. */
  initialTab?: Tab;
  /** Opens the GitHub bug report. */
  onReportBug?: () => void;
}

export type Tab = "editor" | "run" | "ai" | "mcp" | "system";

const TABS: { id: Tab; label: string; icon: string }[] = [
  { id: "editor", label: "編輯器", icon: "✎" },
  { id: "run", label: "執行與安全", icon: "▶" },
  { id: "ai", label: "AI 建議", icon: "✨" },
  { id: "mcp", label: "AI 工具（MCP）", icon: "⇄" },
  { id: "system", label: "系統", icon: "⚙" },
];

/** One setting: a title, a grey caption under it, and its control on the right. */
function Row({ title, caption, children }: { title: string; caption?: ReactNode; children: ReactNode }) {
  return (
    <div className="set-row">
      <div className="set-text">
        <div className="set-title">{title}</div>
        {caption && <div className="set-caption">{caption}</div>}
      </div>
      <div className="set-control">{children}</div>
    </div>
  );
}

function Switch({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="switch">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span />
    </label>
  );
}

function gb(bytes: number) {
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

/** Suggested models: a coder model that follows instructions. */
const MODELS = [
  {
    file: "Qwen2.5-Coder-7B-Instruct-Q4_K_M.gguf",
    repo: "bartowski/Qwen2.5-Coder-7B-Instruct-GGUF",
    label: "Qwen2.5-Coder 7B（建議）",
    size: "約 4.7 GB",
    note: "SQL 建議較準，使用時約佔 5–6 GB 記憶體。",
  },
  {
    file: "Qwen2.5-Coder-3B-Instruct-Q4_K_M.gguf",
    repo: "bartowski/Qwen2.5-Coder-3B-Instruct-GGUF",
    label: "Qwen2.5-Coder 3B（省記憶體）",
    size: "約 1.9 GB",
    note: "記憶體吃緊時用，使用時約佔 2.5 GB，建議較簡單。",
  },
];

function downloadCommand(dir: string, m: (typeof MODELS)[number]) {
  const q = (s: string) => `"${s.replace(/"/g, '\\"')}"`;
  return `mkdir -p ${q(dir)} && curl -L --fail -C - -o ${q(`${dir}/${m.file}`)} https://huggingface.co/${m.repo}/resolve/main/${m.file}`;
}

/** A setup step with its state and what to do about it. */
function Step({ ok, title, children }: { ok: boolean; title: string; children: ReactNode }) {
  return (
    <div className={ok ? "setup-step ok" : "setup-step"}>
      <span className="setup-mark">{ok ? "✓" : "○"}</span>
      <div className="setup-body">
        <div className="set-title">{title}</div>
        {children}
      </div>
    </div>
  );
}

const ACCESS_LABEL = { off: "關閉", read_only: "唯讀", read_write: "可寫入" } as const;

/** How to connect Claude and other AI tools, and what each connection allows. */
function McpSection() {
  const [setup, setSetup] = useState<McpSetup | null>(null);
  const [connections, setConnections] = useState<ConnectionProfile[]>([]);
  useEffect(() => {
    api.mcpSetup().then(setSetup, () => {});
    api.listConnections().then(setConnections, () => {});
  }, []);

  return (
    <>
      <div className="set-caption mcp-intro">
        MCP 讓 Claude Desktop、Claude Code 等 AI 工具透過 MyDB Studio 存好的連線查詢資料庫：列出資料庫和資料表、看表結構、執行
        SQL、看 EXPLAIN。AI 工具會直接執行本程式，不需要開著 app。
        <strong> 查詢結果會傳給該 AI 工具</strong>，所以每個連線可以設定開放到什麼程度，每次執行也都會記在「紀錄清單」。
      </div>

      <div className="set-subhead">Claude Code</div>
      <div className="set-caption">在終端機執行一次：</div>
      {setup && <Command text={setup.claude_code} />}

      <div className="set-subhead">Claude Desktop</div>
      <div className="set-caption">
        Claude Desktop → Settings → Developer → Edit Config，把下面的 mydb-studio 加進 <code>mcpServers</code>
        ，存檔後重開 Claude Desktop。
      </div>
      {setup && <Command text={setup.config_json} />}

      <div className="set-subhead">各連線的權限</div>
      <div className="set-caption">預設是唯讀。要更改請到連線的「編輯 → 一般 → AI 工具（MCP）」。</div>
      <ul className="mcp-connections">
        {connections.map((c) => (
          <li key={c.id}>
            <span>{c.name || `${c.user}@${c.host}`}</span>
            <span className={`mcp-access ${c.mcp ?? "read_only"}`}>{ACCESS_LABEL[c.mcp ?? "read_only"]}</span>
          </li>
        ))}
      </ul>
    </>
  );
}

function Command({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="command">
      <code>{text}</code>
      <button
        type="button"
        className="small"
        onClick={async () => {
          await copyText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
      >
        {copied ? "已複製" : "複製指令"}
      </button>
    </div>
  );
}

function AiSection({ settings, set }: { settings: AppSettings; set: (p: Partial<AppSettings>) => void }) {
  const [status, setStatus] = useState<AiStatus | null>(null);
  const [checking, setChecking] = useState(false);

  const refresh = async () => {
    setChecking(true);
    try {
      setStatus(await api.aiStatus());
    } finally {
      setChecking(false);
    }
  };

  useEffect(() => {
    refresh();
    // Re-check when the model choice changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.ai_model_path, settings.ai_llama_server]);

  const pickModel = async () => {
    const path = await open({ multiple: false, filters: [{ name: "GGUF 模型", extensions: ["gguf"] }] });
    if (typeof path === "string") set({ ai_model_path: path });
  };

  const model = status?.model ?? null;
  const usable = (m: ModelFile) => m.instruct;
  const others = (status?.found_models ?? []).filter((m) => m.path !== model?.path);
  const ready = settings.ai_use_external || (!!status?.llama_server && !!model);
  const threads = settings.ai_threads || Math.max(1, Math.floor((status?.cpu_threads ?? 8) / 2));

  return (
    <>
      <Row
        title="啟用 AI 建議（本機模型）"
        caption={
          <>
            錯誤和 Explain 結果旁會出現「問 AI」。模型只在你按下時才在背景啟動，
            {settings.ai_idle_minutes} 分鐘沒用自動關閉，完全在這台電腦執行、不連網。 只送
            SQL、錯誤訊息、執行計畫和表結構給模型，不送資料內容。
          </>
        }
      >
        <Switch checked={settings.ai_enabled} onChange={(v) => set({ ai_enabled: v })} />
      </Row>

      <div className="set-subhead">
        準備工作
        <button type="button" className="link small-text" onClick={refresh} disabled={checking}>
          {checking ? "檢查中…" : "重新檢查"}
        </button>
        {status && (
          <span className={ready ? "ok small-text" : "warn small-text"}>{ready ? "✓ 已可使用" : "還沒準備好"}</span>
        )}
      </div>

      {!settings.ai_use_external && (
        <>
          <Step ok={!!status?.llama_server} title="1. 安裝 llama.cpp（執行模型的程式）">
            {status?.llama_server ? (
              <div className="set-caption">已找到：{status.llama_server}</div>
            ) : (
              <>
                <div className="set-caption">在終端機執行下面的指令（需要 Homebrew），裝好後按「重新檢查」。</div>
                <Command text="brew install llama.cpp" />
              </>
            )}
          </Step>

          <Step ok={!!model && usable(model)} title="2. 模型檔（.gguf）">
            {model ? (
              <div className="set-caption">
                使用中：{model.name}（{gb(model.size_bytes)}）
                {!usable(model) && (
                  <div className="warn">⚠️ 這看起來是 Base 模型，不太會回答問題，建議改用名稱有 Instruct 的模型。</div>
                )}
              </div>
            ) : settings.ai_model_path ? (
              <div className="set-caption warn">找不到 {settings.ai_model_path}，檔案可能被移走了。</div>
            ) : (
              <div className="set-caption">選一個下載指令貼到終端機執行，下載完按「重新檢查」就會出現在下面。</div>
            )}
            {!model &&
              status &&
              MODELS.map((m) => (
                <div key={m.file} className="model-option">
                  <div>
                    <strong>{m.label}</strong> <span className="muted">{m.size}</span>
                    <div className="set-caption">{m.note}</div>
                  </div>
                  <Command text={downloadCommand(status.models_dir, m)} />
                </div>
              ))}
            {others.length > 0 && (
              <div className="found-models">
                <div className="set-caption">這台電腦上找到的模型：</div>
                {others.map((m) => (
                  <button
                    key={m.path}
                    type="button"
                    className="found-model"
                    onClick={() => set({ ai_model_path: m.path })}
                    title={m.path}
                  >
                    <span>{m.name}</span>
                    <span className="muted">{gb(m.size_bytes)}</span>
                    {!usable(m) && <span className="warn small-text">Base 模型，不建議</span>}
                    <span className="link">使用</span>
                  </button>
                ))}
              </div>
            )}
            <button type="button" className="small" onClick={pickModel}>
              選擇模型檔…
            </button>
          </Step>
        </>
      )}

      <div className="set-subhead">進階</div>
      <Row title="閒置多久後關閉模型" caption="關閉後釋放記憶體，下次使用要再等幾秒載入。">
        <select value={settings.ai_idle_minutes} onChange={(e) => set({ ai_idle_minutes: Number(e.target.value) })}>
          {[3, 5, 10, 20, 30].map((n) => (
            <option key={n} value={n}>
              {n} 分鐘
            </option>
          ))}
        </select>
      </Row>
      <Row
        title="使用的 CPU 執行緒"
        caption={`模型以較低優先權執行，限制執行緒可以讓打字和其他程式更順。這台電腦有 ${status?.cpu_threads ?? "?"} 個。`}
      >
        <select value={settings.ai_threads} onChange={(e) => set({ ai_threads: Number(e.target.value) })}>
          <option value={0}>自動（{threads}）</option>
          {[2, 4, 6, 8].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
      </Row>
      <Row
        title="改用已經在執行的伺服器"
        caption="如果你自己開著 llama-server、Ollama 或 LM Studio，填它的 OpenAI 相容位址，app 就不會自己啟動模型。"
      >
        <Switch checked={settings.ai_use_external} onChange={(v) => set({ ai_use_external: v })} />
      </Row>
      {settings.ai_use_external && (
        <Row
          title="伺服器位址"
          caption="例如 http://127.0.0.1:39282（llama-server）或 http://127.0.0.1:11434（Ollama）。"
        >
          <input
            className="url-input"
            value={settings.ai_server_url}
            spellCheck={false}
            onChange={(e) => set({ ai_server_url: e.target.value })}
          />
        </Row>
      )}
    </>
  );
}

export function SettingsDialog({
  settings,
  onChange,
  onClose,
  onOpenSnippets,
  onReportBug,
  initialTab = "editor",
}: Props) {
  const [tab, setTab] = useState<Tab>(initialTab);
  const set = (patch: Partial<AppSettings>) => onChange({ ...settings, ...patch });
  const themes = [{ id: "auto", label: "跟隨系統" }, ...EDITOR_THEMES];

  return (
    <div className="settings-dialog" role="dialog">
      <nav className="settings-tabs">
        <h2>設定</h2>
        {TABS.map((t) => (
          <button key={t.id} type="button" className={tab === t.id ? "active" : ""} onClick={() => setTab(t.id)}>
            <span className="tab-icon">{t.icon}</span>
            {t.label}
          </button>
        ))}
      </nav>
      <div className="settings-page">
        {tab === "editor" && (
          <>
            <div className="set-subhead">主題</div>
            <div className="theme-swatch">
              {themes.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className={settings.editor_theme === t.id ? "active" : ""}
                  onClick={() => set({ editor_theme: t.id })}
                >
                  {t.label}
                </button>
              ))}
            </div>
            <Row title="字型大小">
              <select
                value={settings.editor_font_size}
                onChange={(e) => set({ editor_font_size: Number(e.target.value) })}
              >
                {[11, 12, 13, 14, 15, 16, 18, 20].map((n) => (
                  <option key={n} value={n}>
                    {n}px
                  </option>
                ))}
              </select>
            </Row>
            <Row title="自動完成名稱" caption="打字時建議資料庫、資料表、欄位名稱。">
              <Switch checked={settings.schema_autocomplete} onChange={(v) => set({ schema_autocomplete: v })} />
            </Row>
            <Row
              title="Snippet（常用 SQL）"
              caption={
                <>
                  輸入縮寫（例如 <code>sel</code>）按 Enter 展開，Tab 跳到下一個要填的地方。編輯器右鍵也能把 SQL 存成
                  Snippet。
                </>
              }
            >
              <button type="button" className="small" onClick={onOpenSnippets}>
                管理…
              </button>
            </Row>
          </>
        )}

        {tab === "run" && (
          <>
            <Row
              title="危險語句先確認"
              caption="執行沒有 WHERE 的 UPDATE／DELETE、DROP、TRUNCATE 之前跳出確認。編輯器裡也會用紅色底線標出來。"
            >
              <Switch checked={settings.confirm_dangerous} onChange={(v) => set({ confirm_dangerous: v })} />
            </Row>
            <Row
              title="自動加上 LIMIT"
              caption="SELECT 沒寫 LIMIT 時，送出前自動加上，避免一次撈整張表。結果上可以一鍵取消限制重跑。"
            >
              <div className="inline-controls">
                {settings.auto_limit > 0 && (
                  <input
                    className="limit-input"
                    type="number"
                    min={1}
                    step={100}
                    value={settings.auto_limit}
                    onChange={(e) => set({ auto_limit: Math.max(1, Math.floor(Number(e.target.value) || 1)) })}
                  />
                )}
                <Switch checked={settings.auto_limit > 0} onChange={(v) => set({ auto_limit: v ? 1000 : 0 })} />
              </div>
            </Row>
            <Row title="每個結果最多顯示筆數" caption="超過的部分不會載入畫面，複製和存檔也只含已載入的列。">
              <select value={settings.max_rows} onChange={(e) => set({ max_rows: Number(e.target.value) })}>
                {[100, 500, 1000, 5000, 10000].map((n) => (
                  <option key={n} value={n}>
                    {n.toLocaleString()}
                  </option>
                ))}
              </select>
            </Row>
            <Row
              title="連線後預設打開紀錄"
              caption="打開時每次執行的 SQL 和統計都會存進「紀錄清單」。釘選的分頁一律會記錄。"
            >
              <Switch checked={settings.record_by_default} onChange={(v) => set({ record_by_default: v })} />
            </Row>
          </>
        )}

        {tab === "ai" && <AiSection settings={settings} set={set} />}

        {tab === "mcp" && <McpSection />}

        {tab === "system" && (
          <>
            <Row title="點兩下才連線" caption="首頁點一下連線卡片只會選取，點兩下或按 Enter 才連線。關掉時點一下就連線。">
              <Switch
                checked={settings.connect_on_double_click}
                onChange={(v) => set({ connect_on_double_click: v })}
              />
            </Row>
            <Row title="顯示系統資料庫" caption="左側清單顯示 mysql、information_schema、performance_schema、sys。">
              <Switch checked={settings.show_system_databases} onChange={(v) => set({ show_system_databases: v })} />
            </Row>
            {onReportBug && (
              <Row
                title="回報問題"
                caption="在瀏覽器打開填好的 GitHub issue。連線資訊、IP 和資料名稱會先遮掉，送出前你可以看過內容。"
              >
                <button onClick={onReportBug}>回報問題…</button>
              </Row>
            )}
            <p className="set-footer">設定即時生效，存在這台電腦上。</p>
          </>
        )}
      </div>
      <div className="settings-actions">
        <button className="primary" onClick={onClose}>
          完成
        </button>
      </div>
    </div>
  );
}
