import { useEffect, useRef } from "react";
import { sql, MariaSQL, MySQL, PostgreSQL } from "@codemirror/lang-sql";
import { EditorState } from "@codemirror/state";
import { EditorView, basicSetup } from "codemirror";
import { zhTwPhrases } from "../../editor/phrases";
import type { EditorTheme } from "../../editor/themes";
import { multiCursor } from "../../editor/multiCursor";

interface Props {
  /** The text to start with; a new value replaces what is in the editor. */
  value: string;
  mariadb: boolean;
  postgres?: boolean;
  theme: EditorTheme;
  fontSize: number;
  onChange: (value: string) => void;
}

/** A plain SQL editor for one definition: no run keys, no autocomplete of tables. */
export function CodeArea({ value, mariadb, postgres, theme, fontSize, onChange }: Props) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const changed = useRef(onChange);
  changed.current = onChange;

  useEffect(() => {
    const v = new EditorView({
      parent: host.current!,
      state: EditorState.create({
        doc: value,
        extensions: [
          basicSetup,
          zhTwPhrases,
          multiCursor(),
          EditorView.lineWrapping,
          sql({ dialect: postgres ? PostgreSQL : mariadb ? MariaSQL : MySQL, upperCaseKeywords: true }),
          theme.extension,
          EditorView.theme({ "&": { height: "100%", fontSize: `${fontSize}px` }, ".cm-scroller": { overflow: "auto" } }),
          EditorView.updateListener.of((u) => u.docChanged && changed.current(u.state.doc.toString())),
        ],
      }),
    });
    view.current = v;
    return () => v.destroy();
    // The editor is rebuilt only when its look changes; text goes through the effect below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mariadb, postgres, theme, fontSize]);

  useEffect(() => {
    const v = view.current;
    if (v && v.state.doc.toString() !== value)
      v.dispatch({ changes: { from: 0, to: v.state.doc.length, insert: value } });
  }, [value]);

  return <div className="code-area" ref={host} />;
}
