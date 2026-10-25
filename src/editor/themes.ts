import type { Extension } from "@codemirror/state";
import { oneDark } from "@codemirror/theme-one-dark";
import { dracula } from "@uiw/codemirror-theme-dracula";
import { githubDark, githubLight } from "@uiw/codemirror-theme-github";
import { materialDark, materialLight } from "@uiw/codemirror-theme-material";
import { nord } from "@uiw/codemirror-theme-nord";
import { solarizedDark } from "@uiw/codemirror-theme-solarized/dark";
import { solarizedLight } from "@uiw/codemirror-theme-solarized/light";
import { tokyoNight } from "@uiw/codemirror-theme-tokyo-night";

export interface EditorTheme {
  id: string;
  label: string;
  dark: boolean;
  extension: Extension;
}

export const EDITOR_THEMES: EditorTheme[] = [
  { id: "light", label: "預設淺色", dark: false, extension: [] },
  { id: "github-light", label: "GitHub Light", dark: false, extension: githubLight },
  { id: "solarized-light", label: "Solarized Light", dark: false, extension: solarizedLight },
  { id: "material-light", label: "Material Light", dark: false, extension: materialLight },
  { id: "one-dark", label: "One Dark", dark: true, extension: oneDark },
  { id: "github-dark", label: "GitHub Dark", dark: true, extension: githubDark },
  { id: "dracula", label: "Dracula", dark: true, extension: dracula },
  { id: "nord", label: "Nord", dark: true, extension: nord },
  { id: "tokyo-night", label: "Tokyo Night", dark: true, extension: tokyoNight },
  { id: "solarized-dark", label: "Solarized Dark", dark: true, extension: solarizedDark },
  { id: "material-dark", label: "Material Dark", dark: true, extension: materialDark },
];

/** Resolves a saved theme id; "auto" (or an unknown id) follows the system. */
export function editorTheme(id: string, systemDark: boolean): EditorTheme {
  return (
    EDITOR_THEMES.find((t) => t.id === id) ??
    EDITOR_THEMES.find((t) => t.id === (systemDark ? "one-dark" : "light"))!
  );
}
