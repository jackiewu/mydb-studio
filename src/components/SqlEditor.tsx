import { useEffect, useRef, type RefObject } from "react";
import { EditorView, basicSetup } from "codemirror";
import { keymap } from "@codemirror/view";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { completeFromList, snippet, snippetCompletion } from "@codemirror/autocomplete";
import { sql, MySQL, MariaSQL, type SQLNamespace } from "@codemirror/lang-sql";
import { selectAll, toggleComment } from "@codemirror/commands";
import { readText } from "@tauri-apps/plugin-clipboard-manager";
import type { Snippet } from "../api";
import { columnSource, type SchemaMap } from "../editor/columns";
import { statementAt } from "../editor/statements";
import { allStatements, runTarget, statementMarks, type RunTarget } from "../editor/statementMarks";
import type { EditorTheme } from "../editor/themes";
import { openMenu } from "../contextMenu";
import { copyText } from "../copy";

/** Lets the app insert text into the editor. */
export interface EditorHandle {
  /** Inserts a snippet at the cursor (replacing the selection). */
  insertSnippet: (body: string) => void;
  /** What ▶ runs: the selection if there is one, otherwise the statement at the cursor. */
  runTarget: () => RunTarget | null;
  /** Everything in the editor. */
  allText: () => RunTarget | null;
  /** The selection if there is one, otherwise the statement at the cursor. */
  statementText: () => string;
}

function selectionOr(v: EditorView) {
  const { from, to, head } = v.state.selection.main;
  if (from !== to) return v.state.sliceDoc(from, to);
  return statementAt(v.state.doc.toString(), head);
}

interface Props {
  initialValue: string;
  mariadb: boolean;
  /** Databases, tables and columns to autocomplete; null turns that off. */
  schema: SchemaMap | null;
  /** Tables of this database complete without a prefix. */
  defaultSchema?: string | null;
  theme: EditorTheme;
  fontSize: number;
  snippets: Snippet[];
  handle?: RefObject<EditorHandle | null>;
  /** False while another tab is shown; the editor stays mounted to keep its state. */
  active?: boolean;
  /** The editor wants the columns of `db` for autocomplete. */
  onNeedColumns?: (db: string) => void;
  onChange: (value: string) => void;
  /** ⌘Enter: the selection, or the statement at the cursor; ⌘⇧Enter: everything. */
  onRun: (sql: string) => void;
  /** What ⌘Enter would run, whenever that changes. */
  onRunTargetChange?: (target: RunTarget) => void;
  /** ⌘E: EXPLAIN the selection or the statement at the cursor. */
  onExplain?: (sql: string) => void;
  /** Opens the snippet editor with `body` as a new snippet. */
  onSaveSnippet?: (body: string) => void;
}

function language(
  mariadb: boolean,
  schema: SchemaMap | null,
  defaultSchema: string | null | undefined,
  needColumns: (db: string) => void,
) {
  const dialect = mariadb ? MariaSQL : MySQL;
  if (!schema) return sql({ dialect, upperCaseKeywords: true });
  const columns = [{ autocomplete: columnSource(schema, defaultSchema, needColumns) }];
  return [
    sql({
      dialect,
      upperCaseKeywords: true,
      schema: schema as SQLNamespace,
      defaultSchema: defaultSchema ?? undefined,
    }),
    EditorState.languageData.of(() => columns),
  ];
}

function snippetSource(snippets: Snippet[]) {
  const options = snippets
    .filter((s) => s.prefix.trim())
    .map((s) =>
      snippetCompletion(s.body, {
        label: s.prefix.trim(),
        detail: s.name,
        info: s.body.replace(/\$\{([^}]*)\}/g, "$1"),
        type: "text",
        // Above keywords; a section would sort them to the bottom instead.
        boost: 50,
      }),
    );
  // Built once: autocomplete tracks sources by identity, so a new function
  // per lookup would reset it on every keystroke.
  const data = [{ autocomplete: completeFromList(options) }];
  return EditorState.languageData.of(() => data);
}

function look(theme: EditorTheme, fontSize: number) {
  return [theme.extension, EditorView.theme({ "&": { fontSize: `${fontSize}px` } }, { dark: theme.dark })];
}

export function SqlEditor(props: Props) {
  const { initialValue, mariadb, schema, defaultSchema, theme, fontSize, snippets, handle } = props;
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  // Parts that change while the editor is open are swapped in place, so the
  // text, cursor and undo history stay.
  const parts = useRef({
    language: new Compartment(),
    look: new Compartment(),
    snippets: new Compartment(),
  });
  // Keep the latest callbacks without rebuilding the editor.
  const handlers = useRef(props);
  handlers.current = props;
  const needColumns = (db: string) => handlers.current.onNeedColumns?.(db);
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
    const explain = (v: EditorView) => {
      handlers.current.onExplain?.(selectionOr(v));
      return true;
    };
    const contextMenu = (e: MouseEvent, v: EditorView) => {
      // Right-clicking outside the selection moves the cursor there first.
      const at = v.posAtCoords({ x: e.clientX, y: e.clientY });
      const sel = v.state.selection.main;
      if (at != null && (at < sel.from || at > sel.to)) v.dispatch({ selection: { anchor: at } });
      v.focus();
      const target = runTarget(v.state);
      const { from, to } = v.state.selection.main;
      const selected = v.state.sliceDoc(from, to);
      const runLabel =
        target.kind === "all"
          ? `執行全部（${target.count} 句）`
          : target.count > 1
            ? `執行多行（${target.count} 句）`
            : "執行單行";
      openMenu(e, [
        { label: `▶ ${runLabel}`, hint: "⌘↵", disabled: target.count === 0, onClick: () => run(v) },
        ...(target.total > 1 && target.kind !== "all"
          ? [{ label: `執行全部（${target.total} 句）`, hint: "⌘⇧↵", onClick: () => runAll(v) }]
          : []),
        { label: "Explain", hint: "⌘E", disabled: target.count === 0, onClick: () => explain(v) },
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
        { label: "註解／取消註解", hint: "⌘/", onClick: () => (toggleComment(v), v.focus()) },
        {
          label: "複製這一句",
          disabled: target.count === 0,
          onClick: () => copyText(target.text.trim()),
        },
        {
          label: selected ? "把選取的內容存成 Snippet…" : "把這一句存成 Snippet…",
          disabled: !selected.trim() && target.count === 0,
          onClick: () => handlers.current.onSaveSnippet?.((selected.trim() ? selected : target.text).trim()),
        },
      ]);
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
    const p = parts.current;
    view.current = new EditorView({
      doc: initialValue,
      parent: host.current!,
      extensions: [
        basicSetup,
        p.language.of(language(mariadb, schema, defaultSchema, needColumns)),
        p.snippets.of(snippetSource(snippets)),
        p.look.of(look(theme, fontSize)),
        statementMarks(),
        EditorView.domEventHandlers({ contextmenu: contextMenu }),
        Prec.highest(
          keymap.of([
            { key: "Mod-Enter", run },
            { key: "Mod-Shift-Enter", run: runAll },
            { key: "Mod-e", run: explain },
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
    // The editor is created once; `initialValue` is only the seed and the
    // other props are applied by the effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    view.current?.dispatch({
      effects: parts.current.language.reconfigure(language(mariadb, schema, defaultSchema, needColumns)),
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mariadb, schema, defaultSchema]);

  // Coming back to a tab: re-measure (it was hidden), focus, and say what ▶ runs.
  useEffect(() => {
    const v = view.current;
    if (!v || props.active === false) return;
    v.requestMeasure();
    v.focus();
    reportRef.current(v.state, true);
  }, [props.active]);

  useEffect(() => {
    view.current?.dispatch({ effects: parts.current.look.reconfigure(look(theme, fontSize)) });
  }, [theme, fontSize]);

  useEffect(() => {
    view.current?.dispatch({ effects: parts.current.snippets.reconfigure(snippetSource(snippets)) });
  }, [snippets]);

  useEffect(() => {
    if (!handle) return;
    handle.current = {
      runTarget: () => (view.current ? runTarget(view.current.state) : null),
      allText: () => (view.current ? allStatements(view.current.state) : null),
      statementText: () => (view.current ? selectionOr(view.current) : ""),
      insertSnippet: (body) => {
        const v = view.current;
        if (!v) return;
        const { from, to } = v.state.selection.main;
        snippet(body)(v, null, from, to);
        v.focus();
      },
    };
    return () => {
      handle.current = null;
    };
  }, [handle]);

  return <div className="editor" ref={host} hidden={props.active === false} />;
}
