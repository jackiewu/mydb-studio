import { EditorState } from "@codemirror/state";

/** CodeMirror's built-in UI text (find/replace panel, go to line, folding) in Traditional Chinese. */
export const zhTwPhrases = EditorState.phrases.of({
  // Find / replace panel (⌘F)
  Find: "搜尋",
  Replace: "取代為",
  next: "下一個",
  previous: "上一個",
  all: "全部選取",
  "match case": "大小寫須相符",
  regexp: "正規表示式",
  "by word": "全字相符",
  replace: "取代",
  "replace all": "全部取代",
  close: "關閉",
  "current match": "目前的符合項目",
  "on line": "行",
  "replaced match on line $": "已取代第 $ 行的符合項目",
  "replaced $ matches": "已取代 $ 個符合項目",
  // Go to line
  "Go to line": "跳到第幾行",
  go: "前往",
  // Folding, autocomplete, lint
  "Folded lines": "已摺疊的行",
  "Unfolded lines": "已展開的行",
  "folded code": "已摺疊的程式碼",
  unfold: "展開",
  to: "到",
  "Fold line": "摺疊",
  "Unfold line": "展開",
  Completions: "自動完成",
  Diagnostics: "檢查結果",
  "No diagnostics": "沒有檢查結果",
  "Control character": "控制字元",
});
