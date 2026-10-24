// Run with `npm test` (Node's own test runner; Node 22 strips the types).
import { test } from "node:test";
import assert from "node:assert/strict";
import { buildKeyTree, folderPaths, ttlText } from "./keyTree.ts";

const key = (k: string, binary = false) => ({ key: k, binary, kind: "string", ttl: -1 });

/** The tree as nested `name(count)` text, keys marked with `*`. */
function shape(nodes: ReturnType<typeof buildKeyTree>): string[] {
  return nodes.map((n) => {
    const inner = n.children.length ? ` [${shape(n.children).join(", ")}]` : "";
    return `${n.name}${n.key ? "*" : ""}(${n.count})${inner}`;
  });
}

test("groups keys by colon prefixes", () => {
  const tree = buildKeyTree([
    key("user:123:profile"),
    key("user:123:cart"),
    key("user:9:profile"),
    key("session:abc"),
    key("plain"),
  ]);
  assert.deepEqual(shape(tree), [
    "session(1) [abc*(1)]",
    "user(3) [9(1) [profile*(1)], 123(2) [cart*(1), profile*(1)]]",
    "plain*(1)",
  ]);
  const user = tree.find((n) => n.name === "user")!;
  assert.equal(user.path, "user:");
  assert.equal(user.children[1].path, "user:123:");
  assert.equal(user.children[1].children[0].path, "user:123:cart");
  assert.equal(user.children[1].children[0].key?.key, "user:123:cart");
});

test("a key that is also a prefix is both key and folder", () => {
  const tree = buildKeyTree([key("user:1"), key("user"), key("user:2")]);
  assert.deepEqual(shape(tree), ["user*(3) [1*(1), 2*(1)]"]);
  assert.equal(tree[0].key?.key, "user");
});

test("numbers sort as numbers, folders before keys", () => {
  const tree = buildKeyTree(["k:10", "k:9", "k:100", "k:2:x", "a"].map((k) => key(k)));
  assert.deepEqual(shape(tree), ["k(4) [2(1) [x*(1)], 9*(1), 10*(1), 100*(1)]", "a*(1)"]);
});

test("empty parts and binary keys", () => {
  const tree = buildKeyTree([key("a::b"), key("FF3A00", true), key(":lead")]);
  assert.deepEqual(shape(tree), ["(1) [lead*(1)]", "a(1) [(1) [b*(1)]]", "FF3A00*(1)"]);
  assert.deepEqual(folderPaths(tree), [":", "a:", "a::"]);
});

test("ttl text", () => {
  assert.equal(ttlText(-1), "永久");
  assert.equal(ttlText(-2), "已過期");
  assert.equal(ttlText(45), "45 秒");
  assert.equal(ttlText(3600 * 5), "5 小時");
  assert.equal(ttlText(86400 * 3), "3 天");
});
