import type { KeyEntry } from "../api";

/**
 * A folder or key in the Redis key tree. Keys are grouped by the parts
 * between colons: `user:123:profile` sits in `user` → `123`. A key can also
 * be a folder when other keys continue its name (`user` and `user:1`).
 */
export interface KeyTreeNode {
  /** The part of the name this level shows. */
  name: string;
  /** Prefix of every key below, e.g. `user:123:`; the key itself for leaves. */
  path: string;
  /** The key whose full name ends here, if any. */
  key: KeyEntry | null;
  children: KeyTreeNode[];
  /** Keys at or below this node. */
  count: number;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function sortNodes(nodes: KeyTreeNode[]) {
  // Folders first, then keys; numbers in names sort as numbers.
  nodes.sort((a, b) => {
    const fa = a.children.length > 0 ? 0 : 1;
    const fb = b.children.length > 0 ? 0 : 1;
    return fa - fb || collator.compare(a.name, b.name);
  });
  for (const n of nodes) sortNodes(n.children);
}

/** Groups `keys` into a tree by `separator`. Binary keys are not split. */
export function buildKeyTree(keys: KeyEntry[], separator = ":"): KeyTreeNode[] {
  const root: KeyTreeNode = { name: "", path: "", key: null, children: [], count: 0 };
  // Children by name, per node, while building.
  const index = new Map<KeyTreeNode, Map<string, KeyTreeNode>>();
  const child = (parent: KeyTreeNode, name: string, path: string) => {
    let byName = index.get(parent);
    if (!byName) index.set(parent, (byName = new Map()));
    let node = byName.get(name);
    if (!node) {
      node = { name, path, key: null, children: [], count: 0 };
      byName.set(name, node);
      parent.children.push(node);
    }
    return node;
  };
  for (const k of keys) {
    const parts = k.binary ? [k.key] : k.key.split(separator);
    let node = root;
    root.count++;
    let prefix = "";
    parts.forEach((part, i) => {
      const last = i === parts.length - 1;
      prefix += part + (last ? "" : separator);
      node = child(node, last ? `\u0000${part}` : part, prefix);
      node.count++;
    });
    node.key = k;
  }
  // Leaves were stored under "\0name" so `a` (a key) and `a:` (a folder)
  // stay apart while building; merge a key into its folder of the same name.
  const finish = (nodes: KeyTreeNode[]): KeyTreeNode[] => {
    const folders = new Map(nodes.filter((n) => !n.name.startsWith("\u0000")).map((n) => [n.name, n]));
    const out: KeyTreeNode[] = [];
    for (const n of nodes) {
      if (!n.name.startsWith("\u0000")) {
        n.children = finish(n.children);
        out.push(n);
        continue;
      }
      const name = n.name.slice(1);
      const folder = folders.get(name);
      if (folder) {
        folder.key = n.key;
        folder.count += 1;
      } else {
        out.push({ ...n, name });
      }
    }
    return out;
  };
  const tree = finish(root.children);
  sortNodes(tree);
  return tree;
}

/** Paths of every folder in `nodes`, for "expand all". */
export function folderPaths(nodes: KeyTreeNode[]): string[] {
  return nodes.flatMap((n) => (n.children.length ? [n.path, ...folderPaths(n.children)] : []));
}

/** Short TTL text: 永久, 45 秒, 12 分, 3 小時, 2 天. */
export function ttlText(ttl: number) {
  if (ttl < 0) return ttl === -2 ? "已過期" : "永久";
  if (ttl < 60) return `${ttl} 秒`;
  if (ttl < 3600) return `${Math.floor(ttl / 60)} 分`;
  if (ttl < 86400) return `${Math.floor(ttl / 3600)} 小時`;
  return `${Math.floor(ttl / 86400)} 天`;
}
