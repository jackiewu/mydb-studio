import type { AppSettings } from "../api";
import { EDITOR_THEMES } from "../editor/themes";

interface Props {
  settings: AppSettings;
  /** Applied (and saved) right away, so changes show while the dialog is open. */
  onChange: (settings: AppSettings) => void;
  onClose: () => void;
  onOpenSnippets: () => void;
}

export function SettingsDialog({ settings, onChange, onClose, onOpenSnippets }: Props) {
  const set = (patch: Partial<AppSettings>) => onChange({ ...settings, ...patch });
  const themes = [{ id: "auto", label: "跟隨系統" }, ...EDITOR_THEMES];

  return (
    <div className="settings-dialog" role="dialog">
      <h2>設定</h2>
      <div className="fields">
        <div className="field-group">
          <span>編輯器主題</span>
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
        </div>
        <div className="row">
          <label className="grow">
            編輯器字型大小
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
          </label>
          <label className="grow">
            每個結果最多顯示筆數
            <select value={settings.max_rows} onChange={(e) => set({ max_rows: Number(e.target.value) })}>
              {[100, 500, 1000, 5000, 10000].map((n) => (
                <option key={n} value={n}>
                  {n.toLocaleString()}
                </option>
              ))}
            </select>
          </label>
        </div>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.schema_autocomplete}
            onChange={(e) => set({ schema_autocomplete: e.target.checked })}
          />
          自動完成資料庫、資料表、欄位名稱
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.record_by_default}
            onChange={(e) => set({ record_by_default: e.target.checked })}
          />
          連線後預設打開「📝 紀錄」
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={settings.show_system_databases}
            onChange={(e) => set({ show_system_databases: e.target.checked })}
          />
          左側顯示系統資料庫（mysql、information_schema…）
        </label>
        <p className="hint">
          Snippet：在編輯器輸入縮寫（例如 <code>sel</code>）會出現在自動完成清單，按 Enter 展開，Tab 跳到下一個欄位。
          <button type="button" className="link" onClick={onOpenSnippets}>
            管理 Snippet…
          </button>
        </p>
      </div>
      <div className="actions">
        <span className="spacer" />
        <button className="primary" onClick={onClose}>
          完成
        </button>
      </div>
    </div>
  );
}
