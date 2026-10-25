import type { ConnectionKind } from "../api";

export interface Section<T> {
  /** Empty when the list needs no heading. */
  title: string;
  list: T[];
}

const KINDS: ConnectionKind[] = ["mysql", "redis"];

/**
 * The home page's headed lists, in order: favorites first, then the rest.
 * Grouped by type, favorites are split by type too ("★ 最愛 · MySQL"), so
 * grouping shows even when every connection is a favorite. Empty sections
 * are left out.
 */
export function homeSections<T extends { favorite?: boolean; kind?: ConnectionKind }>(
  shown: T[],
  grouped: boolean,
  label: (k: ConnectionKind) => string,
): Section<T>[] {
  const kindOf = (c: T) => c.kind ?? "mysql";
  const favorites = shown.filter((c) => c.favorite);
  const others = shown.filter((c) => !c.favorite);
  const sections: Section<T>[] = [];
  if (grouped) {
    for (const k of KINDS)
      sections.push({ title: `★ 最愛 · ${label(k)}`, list: favorites.filter((c) => kindOf(c) === k) });
    for (const k of KINDS) sections.push({ title: label(k), list: others.filter((c) => kindOf(c) === k) });
  } else {
    sections.push({ title: "★ 最愛", list: favorites });
    sections.push({ title: favorites.length > 0 ? "其他連線" : "", list: others });
  }
  return sections.filter((s) => s.list.length > 0);
}
