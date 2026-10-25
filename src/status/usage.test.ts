// Run with `npm test` (Node's own test runner; Node 22 strips the types).
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  addToday,
  idleText,
  isConnectionError,
  light,
  mayBeDropped,
  shouldPing,
  statementCount,
  todayCount,
  TODAY_KEY,
} from "./usage.ts";

function memory() {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
}

test("statements counted per run", () => {
  assert.equal(statementCount({ results: [1, 2], reconnected: false }), 2);
  // MySQL: the failing statement ran too.
  const server = { kind: "server" as const, code: 1064, state: "42000", message: "x" };
  assert.equal(statementCount({ results: [1], error: server, failed_statement: "SELEC", reconnected: false }), 2);
  // …but not when the connection was gone before it ran.
  const lost = { kind: "connection" as const, message: "gone" };
  assert.equal(statementCount({ results: [], error: lost, failed_statement: "SELECT 1", reconnected: false }), 0);
  // Redis: the error reply is already a result.
  const reply = { kind: "redis" as const, message: "WRONGTYPE" };
  assert.equal(statementCount({ results: [1, 2], error: reply, failed_command: "GET h", reconnected: false }), 2);
  assert.equal(statementCount({ results: [], cancelled: true, reconnected: false }), 0);
});

test("the light", () => {
  assert.equal(light({ running: false, reconnected: false, connectionError: false }), "ok");
  assert.equal(light({ running: true, reconnected: false, connectionError: true }), "busy");
  assert.equal(light({ running: false, reconnected: true, connectionError: false }), "busy");
  assert.equal(light({ running: false, reconnected: true, connectionError: true }), "bad");
  assert.ok(isConnectionError({ kind: "connection", message: "" }));
  assert.ok(isConnectionError({ kind: "ssh", message: "" }));
  assert.ok(!isConnectionError({ kind: "server", code: 1, state: "", message: "" }));
  assert.ok(!isConnectionError(null));
});

test("idle warning after 80% of wait_timeout", () => {
  const t0 = 1_000_000;
  assert.ok(!mayBeDropped(t0, t0 + 470_000, 600));
  assert.ok(mayBeDropped(t0, t0 + 481_000, 600));
  assert.ok(!mayBeDropped(t0, t0 + 10_000_000, null));
  assert.ok(!mayBeDropped(t0, t0 + 10_000_000, 0));
});

test("pings only while recently active and not running", () => {
  assert.ok(shouldPing({ running: false, lastActivity: 0, now: 60_000 }));
  assert.ok(!shouldPing({ running: true, lastActivity: 0, now: 60_000 }));
  assert.ok(!shouldPing({ running: false, lastActivity: 0, now: 5 * 60_000 }));
});

test("idle text", () => {
  assert.equal(idleText(59_000), null);
  assert.equal(idleText(3 * 60_000 + 5_000), "閒置 3 分鐘");
  assert.equal(idleText(60 * 60_000), "閒置 1 小時");
  assert.equal(idleText(135 * 60_000), "閒置 2 小時 15 分");
});

test("today's count resets with the date", () => {
  const s = memory();
  const day = new Date(2026, 9, 25, 23, 59);
  assert.equal(todayCount(s, day), 0);
  assert.equal(addToday(s, 3, day), 3);
  assert.equal(addToday(s, 2, day), 5);
  assert.equal(todayCount(s, day), 5);
  const next = new Date(2026, 9, 26, 0, 1);
  assert.equal(todayCount(s, next), 0);
  assert.equal(addToday(s, 1, next), 1);
  s.m.set(TODAY_KEY, "not json");
  assert.equal(todayCount(s, next), 0);
});
