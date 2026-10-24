import { useEffect, useRef, type RefObject } from "react";
import { EditorView, basicSetup } from "codemirror";
import { keymap } from "@codemirror/view";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { autocompletion, type Completion, type CompletionContext } from "@codemirror/autocomplete";
import { selectAll, toggleLineComment } from "@codemirror/commands";
import { readText } from "@tauri-apps/plugin-clipboard-manager";
import { openMenu } from "../contextMenu";
import { copyText } from "../copy";
import type { RunTarget } from "../editor/statementMarks";
import type { EditorTheme } from "../editor/themes";
import { REDIS_COMMANDS } from "../redis/commands";
import type { EditorHandle } from "./SqlEditor";

/** A line that runs: not blank and not a `#` or `//` comment. */
const isCommand = (line: string) => {
  const t = line.trim();
  return t !== "" && !t.startsWith("#") && !t.startsWith("//");
};

/**
 * What ⌘Enter runs: the lines the selection touches, or the line at the
 * cursor. `count` is how many of them are commands.
 */
function runTarget(state: EditorState): RunTarget {
  const doc = state.doc;
  const total = doc.toString().split("\n").filter(isCommand).length;
  const { from, to, head } = state.selection.main;
  if (from !== to) {
    const first = doc.lineAt(from);
    // A selection ending at the start of a line doesn't take that line.
    const last = doc.lineAt(to > from && doc.lineAt(to).from === to ? to - 1 : to);
    const text = doc.sliceString(first.from, last.to);
    const all = state.sliceDoc(0, first.from).trim() === "" && state.sliceDoc(last.to).trim() === "";
    return { kind: all ? "all" : "selection", count: text.split("\n").filter(isCommand).length, total, text };
  }
  const line = doc.lineAt(head).text;
  return { kind: "statement", count: isCommand(line) ? 1 : 0, total, text: isCommand(line) ? line : "" };
}

const options: Completion[] = REDIS_COMMANDS.map(([name, args]) => ({
  label: name,
  detail: args,
  type: "keyword",
}));

/** Command names, at the start of a line only. */
function completeCommand(ctx: CompletionContext) {
  const line = ctx.state.doc.lineAt(ctx.pos);
  const before = line.text.slice(0, ctx.pos - line.from);
  const m = /^\s*([A-Za-z]+(?: [A-Za-z]*)?)$/.exec(before);
  if (!m && !ctx.explicit) return null;
  const word = m?.[1] ?? "";
  // A second word only completes container commands (CONFIG GET …).
  if (word.includes(" ") && !options.some((o) => o.label.startsWith(word.toUpperCase().split(" ")[0] + " ")))
    return null;
  return {
    from: ctx.pos - word.length,
    options,
    validFor: /^[A-Za-z]*( [A-Za-z]*)?$/,
  };
}

function look(theme: EditorTheme, fontSize: number) {
  return [theme.extension, EditorView.theme({ "&": { fontSize: `${fontSize}px` } }, { dark: theme.dark })];
}

interface Props {
  initialValue: string;
  theme: EditorTheme;
  fontSize: number;
  handle?: RefObject<EditorHandle | null>;
  active?: boolean;
  onChange: (value: string) => void;
  /** ⌘Enter: the line at the cursor, or the selected lines. */
  onRun: (text: string) => void;
  onRunTargetChange?: (target: RunTarget) => void;
}

/** Editor for Redis commands, one per line. */
export function RedisEditor(props: Props) {
  const { initialValue, theme, fontSize, handle } = props;
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const lookPart = useRef(new Compartment());
  const handlers = useRef(props);
  handlers.current = props;
  const lastTarget = useRef("");
  const reportRef = useRef<(state: EditorState, force?: boolean) => void>(() => {});

  useEffect(() => {
    const run = (v: EditorView) => {
      handlers.current.onRun(runTarget(v.state).text);
      return true;
    };
    const runAll = (v: EditorView) => {
      handlers.current.onRun(v.state.doc.toString());
      return true;
    };
    const report = (state: EditorState, force = false) => {
      if (handlers.current.active === false) return;
      const t = runTarget(state);
      const key = `${t.kind}:${t.count}:${t.total}:${t.text.slice(0, 60)}`;
      if (key === lastTarget.current && !force) return;
      lastTarget.current = key;
      handlers.current.onRunTargetChange?.(t);
    };
    reportRef.current = report;
    const contextMenu = (e: MouseEvent, v: EditorView) => {
      const at = v.posAtCoords({ x: e.clientX, y: e.clientY });
      const sel = v.state.selection.main;
      if (at != null && (at < sel.from || at > sel.to)) v.dispatch({ selection: { anchor: at } });
      v.focus();
      const target = runTarget(v.state);
      const { from, to } = v.state.selection.main;
      const selected = v.state.sliceDoc(from, to);
      openMenu(e, [
        {
          label: target.count > 1 ? `▶ 執行選取的 ${target.count} 行` : "▶ 執行這一行",
          hint: "⌘↵",
          disabled: target.count === 0,
          onClick: () => run(v),
        },
        ...(target.total > 1 && target.kind !== "all"
          ? [{ label: `依序執行全部（${target.total} 行）`, hint: "⌘⇧↵", onClick: () => runAll(v) }]
          : []),
        "sep",
        {
          label: "剪下",
          hint: "⌘X",
          disabled: !selected,
          onClick: async () => {
            await copyText(selected);
            v.dispatch(v.state.replaceSelection(""));
            v.focus();
          },
        },
        { label: "複製", hint: "⌘C", disabled: !selected, onClick: () => copyText(selected) },
        {
          label: "貼上",
          hint: "⌘V",
          onClick: async () => {
            const text = await readText().catch(() => "");
            if (text) v.dispatch(v.state.replaceSelection(text), { scrollIntoView: true });
            v.focus();
          },
        },
        { label: "全選", hint: "⌘A", onClick: () => (selectAll(v), v.focus()) },
        "sep",
        { label: "註解／取消註解（#）", hint: "⌘/", onClick: () => (toggleLineComment(v), v.focus()) },
        { label: "複製這一行", disabled: target.count === 0, onClick: () => copyText(target.text.trim()) },
      ]);
      return true;
    };
    view.current = new EditorView({
      doc: initialValue,
      parent: host.current!,
      extensions: [
        basicSetup,
        lookPart.current.of(look(theme, fontSize)),
        // `#` comments, for ⌘/.
        EditorState.languageData.of(() => [{ commentTokens: { line: "#" } }]),
        Prec.highest(autocompletion({ override: [completeCommand] })),
        EditorView.domEventHandlers({ contextmenu: contextMenu }),
        Prec.highest(
          keymap.of([
            { key: "Mod-Enter", run },
            { key: "Mod-Shift-Enter", run: runAll },
          ]),
        ),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) handlers.current.onChange(u.state.doc.toString());
          if (u.docChanged || u.selectionSet) report(u.state);
        }),
        EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { overflow: "auto" } }),
      ],
    });
    if (props.active !== false) view.current.focus();
    report(view.current.state);
    return () => {
      view.current?.destroy();
      view.current = null;
    };
    // Created once; `initialValue` is only the seed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const v = view.current;
    if (!v || props.active === false) return;
    v.requestMeasure();
    v.focus();
    reportRef.current(v.state, true);
  }, [props.active]);

  useEffect(() => {
    view.current?.dispatch({ effects: lookPart.current.reconfigure(look(theme, fontSize)) });
  }, [theme, fontSize]);

  useEffect(() => {
    if (!handle) return;
    handle.current = {
      runTarget: () => (view.current ? runTarget(view.current.state) : null),
      allText: () => {
        const v = view.current;
        if (!v) return null;
        const text = v.state.doc.toString();
        const count = text.split("\n").filter(isCommand).length;
        return { kind: "all", count, total: count, text };
      },
      statementText: () => (view.current ? runTarget(view.current.state).text : ""),
      insertSnippet: (body) => {
        const v = view.current;
        if (!v) return;
        v.dispatch(v.state.replaceSelection(body));
        v.focus();
      },
    };
    return () => {
      handle.current = null;
    };
  }, [handle]);

  return <div className="editor" ref={host} hidden={props.active === false} />;
}
