import { useEffect, useState } from "react";
import { api, describeError } from "../api";

export interface BugInfo {
  /** Why the dialog opened, shown at the top. */
  reason: string;
  /** Log, stack or error text; scrubbed before it is shown. */
  details: string;
}

/**
 * Prepares a GitHub issue about a crash or error. Anything identifying is
 * hidden first, the user reads and can edit the text, and the issue only
 * opens in the browser (to submit there) when they ask.
 */
export function BugReportDialog({ info, onClose }: { info: BugInfo; onClose: () => void }) {
  const [title, setTitle] = useState(info.reason.replace(/。$/, ""));
  const [what, setWhat] = useState("");
  const [body, setBody] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .bugReportBody(what.trim() || "（請描述發生前你在做什麼）", info.details)
      .then(setBody, (e) => setError(describeError(e)));
    // Rebuilt only when the user's description settles (on blur).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [info]);

  const rebuild = () =>
    api.bugReportBody(what.trim() || "（請描述發生前你在做什麼）", info.details).then(setBody, () => {});

  return (
    <div className="modal-backdrop">
      <div className="modal bug-report" role="dialog" aria-label="回報問題">
        <h2>回報問題</h2>
        <p>{info.reason}要把這個問題回報到 GitHub 嗎？</p>
        <label>
          標題
          <input value={title} onChange={(e) => setTitle(e.target.value)} />
        </label>
        <label>
          發生前你在做什麼？（選填）
          <textarea rows={2} value={what} onChange={(e) => setWhat(e.target.value)} onBlur={rebuild} />
        </label>
        <label>
          會送出的內容（可以修改）
          <textarea className="mono" rows={12} value={body} onChange={(e) => setBody(e.target.value)} />
        </label>
        <p className="muted">
          GitHub 的 issue 是公開的。連線位址、帳號、IP、連線名稱，以及引號裡的資料表、資料庫名稱和值都已經自動遮成
          ***，也不會附上 SQL。送出前請再看一次。按下後會在瀏覽器打開填好的 issue 頁面，在那裡按 Submit 才會送出。
        </p>
        {error && <p className="error">{error}</p>}
        <div className="modal-actions">
          <button onClick={onClose}>不用了</button>
          <button
            className="primary"
            disabled={!body}
            onClick={async () => {
              try {
                await api.openBugReport(title.trim() || "錯誤回報", body);
                onClose();
              } catch (e) {
                setError(describeError(e));
              }
            }}
          >
            在瀏覽器開啟 GitHub issue
          </button>
        </div>
      </div>
    </div>
  );
}
