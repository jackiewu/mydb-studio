import { useEffect, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { api, describeError, type StatementResult } from "../api";
import { COPY_FORMATS, copyText, fileExtension, formatBlock, type Block, type CopyFormat } from "../copy";

interface Pos {
  r: number;
  c: number;
}

// Remembered across results so a chosen format sticks.
let lastFormat: CopyFormat = "tsv";
let lastHeader = false;

/**
 * Result rows with spreadsheet-style selection: click a cell, drag or
 * shift-click for a range, click a row number or column header for whole
 * rows or columns. ⌘/Ctrl+C copies the selection (or everything) in the
 * chosen format; right-click offers the other formats.
 */
export function ResultGrid({
  result,
  cellLevel,
}: {
  result: StatementResult;
  /** Highlights cells, e.g. the problems in an EXPLAIN. */
  cellLevel?: (row: number, col: number) => "bad" | "warn" | undefined;
}) {
  const { columns, rows } = result;
  const [anchor, setAnchor] = useState<Pos | null>(null);
  const [focus, setFocus] = useState<Pos | null>(null);
  const [format, setFormat] = useState<CopyFormat>(lastFormat);
  const [header, setHeader] = useState(lastHeader);
  const [menu, setMenu] = useState<{ x: number; y: number; cell: Pos } | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const dragging = useRef(false);
  const wrap = useRef<HTMLDivElement>(null);
  const lastRow = rows.length - 1;
  const lastCol = columns.length - 1;

  useEffect(() => {
    lastFormat = format;
    lastHeader = header;
  }, [format, header]);

  useEffect(() => {
    const up = () => (dragging.current = false);
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);

  useEffect(() => {
    if (!flash) return;
    const t = setTimeout(() => setFlash(null), 2000);
    return () => clearTimeout(t);
  }, [flash]);

  const range =
    anchor && focus
      ? {
          r1: Math.min(anchor.r, focus.r),
          r2: Math.max(anchor.r, focus.r),
          c1: Math.min(anchor.c, focus.c),
          c2: Math.max(anchor.c, focus.c),
        }
      : null;
  const inRange = (r: number, c: number) =>
    !!range && r >= range.r1 && r <= range.r2 && c >= range.c1 && c <= range.c2;

  const select = (a: Pos, f: Pos, extend: boolean) => {
    if (!extend || !anchor) setAnchor(a);
    setFocus(f);
    wrap.current?.focus();
  };

  const block = (): Block =>
    range
      ? {
          columns: columns.slice(range.c1, range.c2 + 1),
          rows: rows.slice(range.r1, range.r2 + 1).map((row) => row.slice(range.c1, range.c2 + 1)),
        }
      : { columns, rows };

  const describe = (b: Block) =>
    `${b.rows.length} 列 × ${b.columns.length} 欄${!range && result.truncated ? "（只含已載入的部分）" : ""}`;

  const copy = async (f: CopyFormat = format) => {
    const b = block();
    await copyText(formatBlock(b, f, header));
    setFlash(`已複製 ${describe(b)}`);
  };

  const saveFile = async () => {
    const b = block();
    const ext = fileExtension(format);
    const path = await save({
      defaultPath: `${b.columns[0]?.table || "result"}.${ext}`,
      filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
    });
    if (!path) return;
    try {
      await api.saveTextFile(path, formatBlock(b, format, header));
      setFlash(`已存檔 ${describe(b)}`);
    } catch (e) {
      setFlash(describeError(e));
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "c") {
      e.preventDefault();
      copy();
    } else if (mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      select({ r: 0, c: 0 }, { r: lastRow, c: lastCol }, false);
    } else if (e.key === "Escape") {
      setAnchor(null);
      setFocus(null);
      setMenu(null);
    } else if (focus && e.key.startsWith("Arrow")) {
      e.preventDefault();
      const d = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[e.key];
      if (!d) return;
      const next = {
        r: Math.min(lastRow, Math.max(0, focus.r + d[0])),
        c: Math.min(lastCol, Math.max(0, focus.c + d[1])),
      };
      select(next, next, e.shiftKey);
      wrap.current
        ?.querySelector(`tbody tr:nth-child(${next.r + 1}) td:nth-child(${next.c + 2})`)
        ?.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  };

  const cellValue = (p: Pos) => {
    const v = rows[p.r]?.[p.c];
    return v === null || v === undefined ? "NULL" : String(v);
  };

  return (
    <>
      <div className="copy-bar">
        <span className="muted">{range ? `已選取 ${describe(block())}` : "點儲存格、列號或欄名來選取"}</span>
        <span className="spacer" />
        {flash && <span className="flash">{flash}</span>}
        <select value={format} onChange={(e) => setFormat(e.target.value as CopyFormat)} title="複製格式">
          {COPY_FORMATS.map((f) => (
            <option key={f.id} value={f.id}>
              {f.label}
            </option>
          ))}
        </select>
        {COPY_FORMATS.find((f) => f.id === format)?.header && (
          <label className="check-inline">
            <input type="checkbox" checked={header} onChange={(e) => setHeader(e.target.checked)} />
            含標題
          </label>
        )}
        <button className="small" onClick={() => copy()} title="⌘/Ctrl + C">
          複製{range ? "選取" : "全部"}
        </button>
        <button className="small" onClick={saveFile}>
          存檔…
        </button>
      </div>
      <div
        className="grid-wrap"
        ref={wrap}
        tabIndex={0}
        onKeyDown={onKeyDown}
        onClick={() => setMenu(null)}
      >
        <table className="grid selectable">
          <thead>
            <tr>
              <th
                className="rownum"
                title="全選"
                onClick={() => select({ r: 0, c: 0 }, { r: lastRow, c: lastCol }, false)}
              >
                #
              </th>
              {columns.map((c, i) => (
                <th
                  key={i}
                  className={range && range.r1 === 0 && range.r2 === lastRow && i >= range.c1 && i <= range.c2 ? "sel" : ""}
                  title={`${c.table ? c.table + "." : ""}${c.name} · ${c.type_name}`}
                  onClick={(e) => select({ r: 0, c: i }, { r: lastRow, c: i }, e.shiftKey)}
                >
                  {c.name}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, r) => (
              <tr key={r}>
                <td
                  className={inRange(r, 0) && range!.c1 === 0 && range!.c2 === lastCol ? "rownum sel" : "rownum"}
                  onClick={(e) => select({ r, c: 0 }, { r, c: lastCol }, e.shiftKey)}
                >
                  {r + 1}
                </td>
                {row.map((cell, c) => (
                  <td
                    key={c}
                    className={[cell === null ? "null" : "", inRange(r, c) ? "sel" : "", cellLevel?.(r, c) ?? ""].join(" ")}
                    onMouseDown={(e) => {
                      if (e.button !== 0) return;
                      dragging.current = true;
                      select({ r, c }, { r, c }, e.shiftKey);
                    }}
                    onMouseEnter={() => dragging.current && setFocus({ r, c })}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      if (!inRange(r, c)) select({ r, c }, { r, c }, false);
                      setMenu({ x: e.clientX, y: e.clientY, cell: { r, c } });
                    }}
                  >
                    {cell === null ? "NULL" : String(cell)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {menu && (
        <div className="menu-backdrop" onClick={() => setMenu(null)} onContextMenu={(e) => (e.preventDefault(), setMenu(null))}>
          <ul className="context-menu" style={{ left: Math.min(menu.x, window.innerWidth - 260), top: Math.min(menu.y, window.innerHeight - 340) }} onClick={(e) => e.stopPropagation()}>
            <li>
              <button
                onClick={async () => {
                  await copyText(cellValue(menu.cell));
                  setFlash("已複製這一格");
                  setMenu(null);
                }}
              >
                複製這一格的值
              </button>
            </li>
            <li className="sep" />
            {COPY_FORMATS.map((f) => (
              <li key={f.id}>
                <button
                  onClick={() => {
                    setFormat(f.id);
                    copy(f.id);
                    setMenu(null);
                  }}
                >
                  複製為 {f.label}
                </button>
              </li>
            ))}
            <li className="sep" />
            <li>
              <label className="check-inline">
                <input type="checkbox" checked={header} onChange={(e) => setHeader(e.target.checked)} />
                TSV／CSV 含標題列
              </label>
            </li>
          </ul>
        </div>
      )}
    </>
  );
}
