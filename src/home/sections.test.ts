// Run with `npm test` (Node's own test runner; Node 22 strips the types).
import { test } from "node:test";
import assert from "node:assert/strict";
import { homeSections } from "./sections.ts";

type C = { name: string; favorite?: boolean; kind?: "mysql" | "redis" };
const label = (k: "mysql" | "redis") => (k === "mysql" ? "MySQL" : "Redis");

/** Sections as `title: names` lines. */
function shape(list: C[], grouped: boolean) {
  return homeSections(list, grouped, label).map((s) => `${s.title}: ${s.list.map((c) => c.name).join(",")}`);
}

test("grouping splits favorites by type when everything is a favorite", () => {
  const all: C[] = [
    { name: "a", favorite: true },
    { name: "r", favorite: true, kind: "redis" },
    { name: "b", favorite: true, kind: "mysql" },
  ];
  assert.deepEqual(shape(all, false), ["★ 最愛: a,r,b"]);
  assert.deepEqual(shape(all, true), ["★ 最愛 · MySQL: a,b", "★ 最愛 · Redis: r"]);
});

test("grouped favorites come before the grouped rest", () => {
  const list: C[] = [
    { name: "r1", kind: "redis" },
    { name: "m1", favorite: true },
    { name: "m2" },
    { name: "r2", favorite: true, kind: "redis" },
  ];
  assert.deepEqual(shape(list, true), ["★ 最愛 · MySQL: m1", "★ 最愛 · Redis: r2", "MySQL: m2", "Redis: r1"]);
  assert.deepEqual(shape(list, false), ["★ 最愛: m1,r2", "其他連線: r1,m2"]);
});

test("one type leaves no empty sections", () => {
  const mysqlOnly: C[] = [{ name: "a", favorite: true }, { name: "b" }];
  assert.deepEqual(shape(mysqlOnly, true), ["★ 最愛 · MySQL: a", "MySQL: b"]);
  assert.deepEqual(shape([{ name: "x", kind: "redis" }], true), ["Redis: x"]);
});

test("no favorites: one untitled list unless grouped", () => {
  const list: C[] = [{ name: "a" }, { name: "r", kind: "redis" }];
  assert.deepEqual(shape(list, false), [": a,r"]);
  assert.deepEqual(shape(list, true), ["MySQL: a", "Redis: r"]);
  assert.deepEqual(shape([], true), []);
});
