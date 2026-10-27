import { RangeSetBuilder, StateField, type EditorState, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";
import { startsMidStatement, statementIndexAt, statementRanges, type Range } from "./statements";

/** What ▶ / ⌘Enter would run right now. */
export interface RunTarget {
  /** `statement`: the one at the cursor; `selection`: part of the text; `all`: everything is selected. */
  kind: "statement" | "selection" | "all";
  /** Statements that would run. */
  count: number;
  /** Statements in the whole editor. */
  total: number;
  text: string;
  /**
   * Set when the selection starts in the middle of a statement (see
   * `startsMidStatement`): the whole statements it touches, to run instead.
   */
  whole?: string;
}

const ranges = StateField.define<Range[]>({
  create: (state) => statementRanges(state.doc.toString()),
  update: (value, tr) => (tr.docChanged ? statementRanges(tr.state.doc.toString()) : value),
});

export function runTarget(state: EditorState): RunTarget {
  const { from, to, head } = state.selection.main;
  const list = state.field(ranges);
  if (from !== to) {
    const text = state.sliceDoc(from, to);
    const all = state.sliceDoc(0, from).trim() === "" && state.sliceDoc(to).trim() === "";
    const target: RunTarget = {
      kind: all ? "all" : "selection",
      count: statementRanges(text).length,
      total: list.length,
      text,
    };
    const touched = list.filter((r) => r.to > from && r.from < to);
    if (touched.length > 0 && startsMidStatement(text)) {
      target.whole = state.sliceDoc(touched[0].from, touched[touched.length - 1].to);
    }
    return target;
  }
  const i = statementIndexAt(list, head);
  return {
    kind: "statement",
    count: i < 0 ? 0 : 1,
    total: list.length,
    text: i < 0 ? "" : state.sliceDoc(list[i].from, list[i].to),
  };
}

export function allStatements(state: EditorState): RunTarget {
  const count = state.field(ranges).length;
  return { kind: "all", count, total: count, text: state.doc.toString() };
}

const separator = Decoration.line({ class: "cm-stmt-start" });
const current = Decoration.line({ class: "cm-stmt-current" });

function decorate(state: EditorState): DecorationSet {
  const list = state.field(ranges);
  const { from, to, head } = state.selection.main;
  const active = from === to ? statementIndexAt(list, head) : -1;
  const builder = new RangeSetBuilder<Decoration>();
  list.forEach((r, i) => {
    const first = state.doc.lineAt(r.from).number;
    const last = state.doc.lineAt(r.to).number;
    for (let n = first; n <= last; n++) {
      const line = state.doc.line(n);
      const classes = [];
      if (n === first && i > 0) classes.push(separator);
      if (i === active) classes.push(current);
      // Two line decorations at one position must be added in order.
      for (const d of classes) builder.add(line.from, line.from, d);
    }
  });
  return builder.finish();
}

const marks = StateField.define<DecorationSet>({
  create: decorate,
  update: (value, tr) => (tr.docChanged || tr.selection ? decorate(tr.state) : value),
  provide: (f) => EditorView.decorations.from(f),
});

/** Lines between statements, and a tint on the statement ▶ would run. */
export function statementMarks(): Extension {
  return [
    ranges,
    marks,
    EditorView.baseTheme({
      ".cm-stmt-start": { borderTop: "1px dashed rgba(127, 127, 127, 0.45)" },
      "&light .cm-stmt-current": { backgroundColor: "rgba(47, 111, 222, 0.07)" },
      "&dark .cm-stmt-current": { backgroundColor: "rgba(120, 160, 255, 0.10)" },
    }),
  ];
}
