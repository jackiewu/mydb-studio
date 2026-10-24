import { useState } from "react";
import { api, describeError, type Snippet } from "../api";

interface Props {
  snippets: Snippet[];
  /** Called after any save or delete so the app reloads the list. */
  onChanged: () => Promise<void>;
  /** Present when an editor is open. */
  onInsert?: (s: Snippet) => void;
  onClose: () => void;
  /** Starts with this unsaved snippet, e.g. SQL picked in the editor. */
  initial?: Snippet | null;
}

const BLANK: Snippet = { id: 0, name: "", prefix: "", body: "" };

export function SnippetsDialog({ snippets, onChanged, onInsert, onClose, initial }: Props) {
  const [draft, setDraft] = useState<Snippet>(initial ?? snippets[0] ?? BLANK);
  const [error, setError] = useState<string | null>(null);
  const saved = snippets.find((s) => s.id === draft.id);
  const dirty = !saved || saved.name !== draft.name || saved.prefix !== draft.prefix || saved.body !== draft.body;
  const set = (patch: Partial<Snippet>) => setDraft((d) => ({ ...d, ...patch }));

  const save = async () => {
    setError(null);
    if (!draft.prefix.trim() || !draft.body.trim()) {
      setError("縮寫和內容都要填。");
      return;
    }
    if (snippets.some((s) => s.id !== draft.id && s.prefix === draft.prefix.trim())) {
      setError(`縮寫「${draft.prefix.trim()}」已經有了。`);
      return;
    }
    try {
      const s = await api.saveSnippet({ ...draft, prefix: draft.prefix.trim(), name: draft.name.trim() });
      await onChanged();
      setDraft(s);
    } catch (e) {
      setError(describeError(e));
    }
  };

  const remove = async () => {
    if (!saved || !confirm(`刪除 snippet「${saved.prefix}」？`)) return;
    await api.deleteSnippet(saved.id);
    await onChanged();
    setDraft(snippets.find((s) => s.id !== saved.id) ?? BLANK);
  };

  return (
    <div className="snippet-dialog" role="dialog">
      <h2>Snippet</h2>
      <div className="snippet-body">
        <ul className="snippet-list">
          {snippets.map((s) => (
            <li key={s.id}>
              <button className={s.id === draft.id ? "active" : ""} onClick={() => setDraft(s)} title={s.body}>
                <code>{s.prefix}</code>
                <span className="name">{s.name}</span>
              </button>
            </li>
          ))}
          <li>
            <button className={draft.id === 0 ? "active" : ""} onClick={() => setDraft(BLANK)}>
              ＋ 新增
            </button>
          </li>
        </ul>
        <div className="snippet-edit">
          <div className="fields-row row">
            <label style={{ width: 140 }}>
              縮寫
              <input
                value={draft.prefix}
                placeholder="例如 sel"
                autoFocus={!!initial}
                onChange={(e) => set({ prefix: e.target.value })}
              />
            </label>
            <label className="grow">
              名稱
              <input value={draft.name} placeholder="例如 查詢資料" onChange={(e) => set({ name: e.target.value })} />
            </label>
          </div>
          <label className="grow-label">
            內容（用 {"${名稱}"} 標記要填的地方）
            <textarea
              value={draft.body}
              spellCheck={false}
              placeholder={"SELECT *\nFROM ${table}\nWHERE ${condition};"}
              onChange={(e) => set({ body: e.target.value })}
            />
          </label>
          {error && <div className="status bad">{error}</div>}
        </div>
      </div>
      <div className="actions">
        {saved && (
          <button className="danger" onClick={remove}>
            刪除
          </button>
        )}
        <span className="spacer" />
        {onInsert && (
          <button disabled={!draft.body.trim()} onClick={() => onInsert(draft)}>
            插入編輯器
          </button>
        )}
        <button disabled={!dirty} onClick={save}>
          儲存
        </button>
        <button className="primary" onClick={onClose}>
          完成
        </button>
      </div>
    </div>
  );
}
