import { useState } from "react";

/** Reads a remembered pane size; null when none is kept. */
function load(key: string): number | null {
  try {
    const n = Number(localStorage.getItem(key));
    return Number.isFinite(n) && n > 0 ? n : null;
  } catch {
    return null;
  }
}

/** A pane size in pixels, remembered across restarts. `null` means the default layout. */
export function usePaneSize(key: string): [number | null, (size: number | null) => void] {
  const [size, setSize] = useState(() => load(key));
  const set = (next: number | null) => {
    setSize(next);
    try {
      if (next === null) localStorage.removeItem(key);
      else localStorage.setItem(key, String(Math.round(next)));
    } catch {
      // Not kept; the size still applies until the app closes.
    }
  };
  return [size, set];
}

/**
 * A handle to drag between two panes. `onDrag` gets the pointer position
 * (clientX for "col", clientY for "row"); double-click restores the default.
 */
export function Splitter({
  dir,
  onDrag,
  onReset,
  className = "",
  style,
}: {
  dir: "col" | "row";
  onDrag: (pos: number) => void;
  onReset: () => void;
  className?: string;
  style?: React.CSSProperties;
}) {
  return (
    <div
      className={`splitter splitter-${dir} ${className}`}
      style={style}
      role="separator"
      aria-orientation={dir === "col" ? "vertical" : "horizontal"}
      title="拖曳調整大小，按兩下恢復預設"
      onDoubleClick={onReset}
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        const el = e.currentTarget;
        el.setPointerCapture(e.pointerId);
        document.body.classList.add(`resizing-${dir}`);
        const move = (ev: PointerEvent) => onDrag(dir === "col" ? ev.clientX : ev.clientY);
        const up = () => {
          el.removeEventListener("pointermove", move);
          el.removeEventListener("pointerup", up);
          el.removeEventListener("pointercancel", up);
          document.body.classList.remove(`resizing-${dir}`);
        };
        el.addEventListener("pointermove", move);
        el.addEventListener("pointerup", up);
        el.addEventListener("pointercancel", up);
      }}
    />
  );
}
