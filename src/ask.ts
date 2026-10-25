import { confirm } from "@tauri-apps/plugin-dialog";

/**
 * Asks a yes/no question in a native dialog and waits for the answer.
 * (`window.confirm` doesn't wait in the app's webview: it returns at once.)
 */
export function ask(message: string, okLabel = "確定"): Promise<boolean> {
  return confirm(message, { title: "MyDB Studio", kind: "warning", okLabel, cancelLabel: "取消" });
}
