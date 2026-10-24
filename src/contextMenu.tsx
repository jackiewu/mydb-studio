import { useEffect, useState } from "react";
import { readText } from "@tauri-apps/plugin-clipboard-manager";
import { copyText } from "./copy";

export type MenuItem =
  | { label: string; onClick: () => void; disabled?: boolean; danger?: boolean; hint?: string }
  | "sep";

interface Open {
  x: number;
  y: number;
  items: MenuItem[];
}

let show: (menu: Open | null) => void = () => {};
// The native event a component already answered, so the fallback skips it.
let handled: Event | null = null;

/**
 * Shows `items` at the pointer in place of the webview's own menu (which
 * offers Inspect Element in development builds).
 */
export function openMenu(e: React.MouseEvent | MouseEvent, items: MenuItem[]) {
  e.preventDefault();
  e.stopPropagation();
  handled = "nativeEvent" in e ? e.nativeEvent : e;
  show({ x: e.clientX, y: e.clientY, items });
}

type Editable = HTMLInputElement | HTMLTextAreaElement;

function editableAt(target: EventTarget | null): Editable | HTMLElement | null {
  const el = target instanceof Element ? target : null;
  const field = el?.closest("input, textarea");
  if (field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement) {
    const textual =
      field instanceof HTMLTextAreaElement || /^(text|search|password|number|email|url|)$/.test(field.type);
    return textual ? field : null;
  }
  const editable = el?.closest("[contenteditable='true']");
  return editable instanceof HTMLElement ? editable : null;
}

/** Cut, copy, paste and select all for a text field. */
function fieldItems(field: Editable | HTMLElement): MenuItem[] {
  const isField = field instanceof HTMLInputElement || field instanceof HTMLTextAreaElement;
  const selected = isField
    ? field.value.slice(field.selectionStart ?? 0, field.selectionEnd ?? 0)
    : String(getSelection() ?? "");
  const readOnly = isField && (field.readOnly || field.disabled);
  const secret = field instanceof HTMLInputElement && field.type === "password";
  return [
    {
      label: "剪下",
      disabled: !selected || readOnly || secret,
      onClick: async () => {
        await copyText(selected);
        field.focus();
        document.execCommand("delete");
      },
    },
    { label: "複製", disabled: !selected || secret, onClick: () => copyText(selected) },
    {
      label: "貼上",
      disabled: readOnly,
      onClick: async () => {
        const text = await readText().catch(() => "");
        field.focus();
        if (text) document.execCommand("insertText", false, text);
      },
    },
    "sep",
    {
      label: "全選",
      onClick: () => {
        field.focus();
        if (isField) field.select();
        else document.execCommand("selectAll");
      },
    },
  ];
}

/**
 * Renders the open menu, and answers right-clicks no component handled:
 * text fields get cut / copy / paste, selected text gets copy, and
 * everywhere else nothing opens.
 */
export function ContextMenuHost() {
  const [menu, setMenu] = useState<Open | null>(null);

  useEffect(() => {
    show = setMenu;
    const onContextMenu = (e: MouseEvent) => {
      // Answered by a component (its own menu) already.
      if (handled === e || e.defaultPrevented) return;
      e.preventDefault();
      const field = editableAt(e.target);
      if (field) {
        setMenu({ x: e.clientX, y: e.clientY, items: fieldItems(field) });
        return;
      }
      const selected = String(getSelection() ?? "");
      if (selected.trim()) {
        setMenu({ x: e.clientX, y: e.clientY, items: [{ label: "複製", onClick: () => copyText(selected) }] });
      } else {
        setMenu(null);
      }
    };
    const close = () => setMenu(null);
    window.addEventListener("contextmenu", onContextMenu);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    return () => {
      show = () => {};
      window.removeEventListener("contextmenu", onContextMenu);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
    };
  }, []);

  useEffect(() => {
    if (!menu) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setMenu(null);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [menu]);

  if (!menu) return null;
  const height = menu.items.reduce((h, i) => h + (i === "sep" ? 9 : 30), 8);
  return (
    <div
      className="menu-backdrop"
      onMouseDown={() => setMenu(null)}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu(null);
      }}
    >
      <ul
        className="context-menu"
        style={{
          left: Math.min(menu.x, window.innerWidth - 240),
          top: Math.max(0, Math.min(menu.y, window.innerHeight - height)),
        }}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {menu.items.map((item, i) =>
          item === "sep" ? (
            <li key={i} className="sep" />
          ) : (
            <li key={i}>
              <button
                className={item.danger ? "danger-item" : undefined}
                disabled={item.disabled}
                onClick={() => {
                  setMenu(null);
                  item.onClick();
                }}
              >
                <span>{item.label}</span>
                {item.hint && <span className="hint">{item.hint}</span>}
              </button>
            </li>
          ),
        )}
      </ul>
    </div>
  );
}
