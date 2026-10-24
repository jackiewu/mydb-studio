import { useEffect, useRef } from "react";
import { EditorView, basicSetup } from "codemirror";
import { keymap } from "@codemirror/view";
import { Prec } from "@codemirror/state";
import { sql, MySQL, MariaSQL } from "@codemirror/lang-sql";

interface Props {
  initialValue: string;
  mariadb: boolean;
  onChange: (value: string) => void;
  /** Called with the selection if there is one, otherwise the whole text. */
  onRun: (sql: string) => void;
}

export function SqlEditor({ initialValue, mariadb, onChange, onRun }: Props) {
  const host = useRef<HTMLDivElement>(null);
  // Keep the latest callbacks without rebuilding the editor.
  const handlers = useRef({ onChange, onRun });
  handlers.current = { onChange, onRun };

  useEffect(() => {
    const run = (view: EditorView) => {
      const { from, to } = view.state.selection.main;
      const text = from === to ? view.state.doc.toString() : view.state.sliceDoc(from, to);
      handlers.current.onRun(text);
      return true;
    };
    const view = new EditorView({
      doc: initialValue,
      parent: host.current!,
      extensions: [
        basicSetup,
        sql({ dialect: mariadb ? MariaSQL : MySQL, upperCaseKeywords: true }),
        Prec.highest(keymap.of([{ key: "Mod-Enter", run }])),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) handlers.current.onChange(u.state.doc.toString());
        }),
        EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { overflow: "auto" } }),
      ],
    });
    return () => view.destroy();
    // The editor is created once per session; `initialValue` is only the seed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mariadb]);

  return <div className="editor" ref={host} />;
}
