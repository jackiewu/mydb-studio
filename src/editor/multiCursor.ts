import { Prec, type EditorState, type Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { selectNextOccurrence, selectSelectionMatches } from "@codemirror/search";

/**
 * Multiple cursors and column selection. `basicSetup` already brings most of
 * it: ⌘-click adds a cursor, ⌥-drag selects a column (box), ⌘⌥↑/↓ adds a
 * cursor above/below, ⌘D picks the next match, Esc goes back to one cursor.
 * This adds what the app needs on top:
 *
 * - "select every match" moves to ⌃⌘G, because ⌘⇧L locks the app; inside the
 *   editor ⌘⇧L does nothing else, so the lock still gets it.
 * - the extra cursors are drawn as clearly as the main one.
 */
export function multiCursor(): Extension {
  return [
    Prec.highest(
      keymap.of([
        { key: "Mod-Shift-l", run: () => true },
        { key: "Mod-Alt-Shift-l", mac: "Ctrl-Cmd-g", run: selectSelectionMatches, preventDefault: true },
      ]),
    ),
    EditorView.theme({
      ".cm-cursor-secondary": { borderLeftWidth: "2px", borderLeftColor: "#e8912d" },
    }),
  ];
}

export { selectNextOccurrence, selectSelectionMatches };

/** Every selected text, in document order, one per line (what ⌘C copies). */
export function selectedTexts(state: EditorState): string {
  return state.selection.ranges
    .filter((r) => !r.empty)
    .map((r) => state.sliceDoc(r.from, r.to))
    .join(state.lineBreak);
}

/** Whether `pos` is inside (or at the edge of) any selection range or cursor. */
export function inSelection(state: EditorState, pos: number): boolean {
  return state.selection.ranges.some((r) => pos >= r.from && pos <= r.to);
}
