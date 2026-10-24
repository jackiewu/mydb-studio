import { useEffect, useRef, type RefObject } from "react";
import { EditorView, basicSetup } from "codemirror";
import { keymap } from "@codemirror/view";
import { Compartment, EditorState, Prec } from "@codemirror/state";
import { completeFromList, snippet, snippetCompletion } from "@codemirror/autocomplete";
import { sql, MySQL, MariaSQL, type SQLNamespace } from "@codemirror/lang-sql";
import type { Snippet } from "../api";
import { columnSource, type SchemaMap } from "../editor/columns";
import { statementAt } from "../editor/statements";
import type { EditorTheme } from "../editor/themes";

/** Lets the app insert text into the editor. */
export interface EditorHandle {
  /** Inserts a snippet at the cursor (replacing the selection). */
  insertSnippet: (body: string) => void;
  /** The selection if there is one, otherwise the whole text. */
  runText: () => string;
  /** The selection if there is one, otherwise the statement at the cursor. */
  statementText: () => string;
}

function selectionOr(v: EditorView, fallback: "all" | "statement") {
  const { from, to, head } = v.state.selection.main;
  if (from !== to) return v.state.sliceDoc(from, to);
  const doc = v.state.doc.toString();
  return fallback === "all" ? doc : statementAt(doc, head);
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
  /** The editor wants the columns of `db` for autocomplete. */
  onNeedColumns?: (db: string) => void;
  onChange: (value: string) => void;
  /** ⌘Enter: the selection or the whole text; ⌘⇧Enter: the statement at the cursor. */
  onRun: (sql: string) => void;
  /** ⌘E: EXPLAIN the selection or the statement at the cursor. */
  onExplain?: (sql: string) => void;
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
  return [
    theme.extension,
    EditorView.theme({ "&": { fontSize: `${fontSize}px` } }, { dark: theme.dark }),
  ];
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

  useEffect(() => {
    const run = (v: EditorView) => {
      handlers.current.onRun(selectionOr(v, "all"));
      return true;
    };
    const runStatement = (v: EditorView) => {
      handlers.current.onRun(selectionOr(v, "statement"));
      return true;
    };
    const explain = (v: EditorView) => {
      handlers.current.onExplain?.(selectionOr(v, "statement"));
      return true;
    };
    const p = parts.current;
    view.current = new EditorView({
      doc: initialValue,
      parent: host.current!,
      extensions: [
        basicSetup,
        p.language.of(language(mariadb, schema, defaultSchema, needColumns)),
        p.snippets.of(snippetSource(snippets)),
        p.look.of(look(theme, fontSize)),
        Prec.highest(keymap.of([
            { key: "Mod-Enter", run },
            { key: "Mod-Shift-Enter", run: runStatement },
            { key: "Mod-e", run: explain },
          ])),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) handlers.current.onChange(u.state.doc.toString());
        }),
        EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { overflow: "auto" } }),
      ],
    });
    view.current.focus();
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

  useEffect(() => {
    view.current?.dispatch({ effects: parts.current.look.reconfigure(look(theme, fontSize)) });
  }, [theme, fontSize]);

  useEffect(() => {
    view.current?.dispatch({ effects: parts.current.snippets.reconfigure(snippetSource(snippets)) });
  }, [snippets]);

  useEffect(() => {
    if (!handle) return;
    handle.current = {
      runText: () => (view.current ? selectionOr(view.current, "all") : ""),
      statementText: () => (view.current ? selectionOr(view.current, "statement") : ""),
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

  return <div className="editor" ref={host} />;
}
