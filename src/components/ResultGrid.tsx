import { useEffect, useMemo, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { api, describeError, type StatementResult } from "../api";
import { COPY_FORMATS, copyText, fileExtension, formatBlock, type Block, type CopyFormat } from "../copy";

interface Pos {
  r: number;
  c: number;
}

// Statements safe to run a second time for an export (checked again in Rust).
const READ_ONLY = /^\s*(\/\*[\s\S]*?\*\/\s*)*(select|with|show|desc|describe|explain|table|values|\()/i;

// Column resizing: narrowest width, widest double-click fit, rows a fit looks at.
const MIN_COL = 32;
const MAX_FIT = 1200;
const FIT_ROWS = 1000;

// Remembered across results so a chosen format sticks.
let lastFormat: CopyFormat = "tsv";
let lastHeader = false;

/**
 * Result rows with spreadsheet-style selection: click a cell, drag or
 * shift-click for a range, click a row number or column header for whole
 * rows or columns. ⌘/Ctrl+C copies the selection (or everything) in the
 * chosen format; right-click offers the other formats. The search box
 * keeps only the loaded rows with a matching cell; selection and copying
 * then work on those rows. Dragging the right edge of a column header
 * resizes the column; double-clicking it fits the column to its contents.
 */
export function ResultGrid({
  result,
  cellLevel,
  sessionId,
}: {
  result: StatementResult;
  /** When set, rows the grid did not load can be exported by running the query again. */
  sessionId?: string;
  /** Highlights cells, e.g. the problems in an EXPLAIN. */
  cellLevel?: (row: number, col: number) => "bad" | "warn" | undefined;
}) {
  const { columns, rows: allRows } = result;
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();
  // Original row numbers of the rows on screen.
  const shown = useMemo(
    () =>
      allRows.flatMap((row, i) =>
        !needle || row.some((v) => (v === null ? "null" : String(v)).toLowerCase().includes(needle)) ? [i] : [],
      ),
    [allRows, needle],
  );
  const rows = useMemo(() => (needle ? shown.map((i) => allRows[i]) : allRows), [allRows, shown, needle]);
  const searchBox = useRef<HTMLInputElement>(null);
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
  // Pixel widths of the row-number column and each result column, once the
  // user has resized one; until then the table sizes itself.
  const [widths, setWidths] = useState<number[] | null>(null);
  const resizing = useRef(false);

  const columnKey = columns.map((c) => c.name).join("\u0000");
  useEffect(() => setWidths(null), [columnKey]);

  /** Current widths: the ones set, or what the browser laid out. */
  const measured = () =>
    widths ??
    Array.from(wrap.current?.querySelectorAll<HTMLElement>("thead th") ?? [], (th) => th.getBoundingClientRect().width);

  const startResize = (col: number, e: React.MouseEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const base = measured();
    const startX = e.clientX;
    const startW = base[col];
    resizing.current = true;
    document.body.classList.add("col-resizing");
    const move = (ev: MouseEvent) => {
      const next = [...base];
      next[col] = Math.max(MIN_COL, Math.round(startW + ev.clientX - startX));
      setWidths(next);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("col-resizing");
      // Let the click that ends the drag pass without selecting the column.
      setTimeout(() => (resizing.current = false));
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  /** Widens or narrows a column to its longest loaded value (header included). */
  const autoFit = (col: number) => {
    const table = wrap.current?.querySelector("table");
    if (!table) return;
    const cells = [
      table.querySelector<HTMLElement>(`thead th:nth-child(${col + 1})`),
      ...Array.from(table.querySelectorAll<HTMLElement>(`tbody tr:nth-child(-n + ${FIT_ROWS}) td:nth-child(${col + 1})`)),
    ];
    // A range around the cell's text measures it even where the cell clips it.
    const range = document.createRange();
    let fit = MIN_COL;
    for (const cell of cells) {
      if (!cell) continue;
      range.selectNodeContents(cell);
      const handle = cell.querySelector(".col-resize");
      if (handle) range.setEndBefore(handle);
      const pad = parseFloat(getComputedStyle(cell).paddingLeft) + parseFloat(getComputedStyle(cell).paddingRight);
      fit = Math.max(fit, Math.ceil(range.getBoundingClientRect().width + pad + 2));
    }
    const base = measured();
    const next = [...base];
    next[col] = Math.min(MAX_FIT, fit);
    setWidths(next);
  };

  useEffect(() => {
    lastFormat = format;
    lastHeader = header;
  }, [format, header]);

  useEffect(() => {
    const up = () => (dragging.current = false);
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);

  // A new search starts with nothing selected.
  useEffect(() => {
    setAnchor(null);
    setFocus(null);
  }, [needle]);

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
    `${b.rows.length} 列 × ${b.columns.length} 欄${
      range ? "" : needle ? "（搜尋結果）" : result.truncated ? "（只含已載入的部分）" : ""
    }`;

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

  // Rows exist beyond what was loaded: the grid's cap, or the automatic LIMIT.
  const more = result.truncated || result.limit_added != null;
  const canExportAll = !!sessionId && more && READ_ONLY.test(result.sql);
  const [exporting, setExporting] = useState(false);

  const exportAll = async () => {
    if (!sessionId) return;
    const ext = fileExtension(format);
    const path = await save({
      defaultPath: `${columns[0]?.table || "result"}.${ext}`,
      filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
    });
    if (!path) return;
    setExporting(true);
    setFlash("匯出中…（會重新執行查詢）");
    try {
      const n = await api.exportQuery(sessionId, result.sql, { format, header, table: null }, path);
      setFlash(`已匯出全部 ${n.toLocaleString()} 列`);
    } catch (e) {
      setFlash((e as { kind?: string })?.kind === "cancelled" ? "已停止匯出，沒有留下檔案" : describeError(e));
    } finally {
      setExporting(false);
    }
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "c") {
      e.preventDefault();
      copy();
    } else if (mod && e.key.toLowerCase() === "f") {
      e.preventDefault();
      searchBox.current?.focus();
      searchBox.current?.select();
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
        <input
          ref={searchBox}
          className="grid-search"
          type="search"
          placeholder="搜尋結果…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setQuery("");
              wrap.current?.focus();
            } else if (e.key === "Enter" || e.key === "ArrowDown") {
              e.preventDefault();
              wrap.current?.focus();
            }
          }}
          title="只顯示有符合儲存格的列（⌘/Ctrl + F），Esc 清除"
        />
        {needle && (
          <span className={shown.length ? "muted" : "no-match"}>
            {shown.length} / {allRows.length} 列
          </span>
        )}
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
          複製{range ? "選取" : needle ? "符合的列" : "全部"}
        </button>
        <button className="small" onClick={saveFile} title="存下已載入的列">
          存檔…
        </button>
        {canExportAll &&
          (exporting ? (
            <button className="small danger" onClick={() => sessionId && api.cancelQuery(sessionId)}>
              ■ 停止匯出
            </button>
          ) : (
            <button
              className="small"
              onClick={exportAll}
              title={`畫面上只載入 ${result.rows.length.toLocaleString()} 列。重新執行這句查詢${
                result.limit_added != null ? "（不加自動 LIMIT）" : ""
              }，把所有結果直接寫進檔案。`}
            >
              匯出全部…
            </button>
          ))}
      </div>
      <div
        className="grid-wrap"
        ref={wrap}
        tabIndex={0}
        onKeyDown={onKeyDown}
        onClick={() => setMenu(null)}
      >
        <table
          className={widths ? "grid selectable sized" : "grid selectable"}
          style={widths ? { width: widths.reduce((a, b) => a + b, 0) } : undefined}
        >
          {widths && (
            <colgroup>
              {widths.map((w, i) => (
                <col key={i} style={{ width: w }} />
              ))}
            </colgroup>
          )}
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
                  onClick={(e) => !resizing.current && select({ r: 0, c: i }, { r: lastRow, c: i }, e.shiftKey)}
                >
                  {c.name}
                  <span
                    className="col-resize"
                    title="拖曳調整欄寬，雙擊依內容自動調整"
                    onMouseDown={(e) => startResize(i + 1, e)}
                    onClick={(e) => e.stopPropagation()}
                    onDoubleClick={(e) => {
                      e.stopPropagation();
                      autoFit(i + 1);
                    }}
                  />
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
                  {(needle ? shown[r] : r) + 1}
                </td>
                {row.map((cell, c) => (
                  <td
                    key={c}
                    className={[
                      cell === null ? "null" : "",
                      inRange(r, c) ? "sel" : "",
                      cellLevel?.(needle ? shown[r] : r, c) ?? "",
                    ].join(" ")}
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
                    {highlight(cell === null ? "NULL" : String(cell), needle)}
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

/** Marks each case-insensitive occurrence of `needle` in `text`. */
function highlight(text: string, needle: string) {
  if (!needle) return text;
  const lower = text.toLowerCase();
  const parts: React.ReactNode[] = [];
  let at = 0;
  for (let i = lower.indexOf(needle); i >= 0; i = lower.indexOf(needle, at)) {
    if (i > at) parts.push(text.slice(at, i));
    parts.push(<mark key={i}>{text.slice(i, i + needle.length)}</mark>);
    at = i + needle.length;
  }
  if (!parts.length) return text;
  if (at < text.length) parts.push(text.slice(at));
  return parts;
}
