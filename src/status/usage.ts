import type { CoreError } from "../api";

/** What the status bar needs of a run, MySQL or Redis. */
export interface RunSummary {
  results: unknown[];
  error?: CoreError | null;
  cancelled?: boolean;
  reconnected: boolean;
  /** MySQL: the statement that failed, which is not in `results`. */
  failed_statement?: string | null;
  /** Redis: the command that failed; an error reply is in `results` already. */
  failed_command?: string | null;
}

/** Statements (or Redis commands) a run sent to the server. */
export function statementCount(o: RunSummary) {
  if (o.cancelled && o.results.length === 0) return 0;
  // A server error on a statement counts; it ran. Redis error replies are
  // already among the results.
  const failedMysql = o.failed_statement != null && o.error?.kind === "server";
  return o.results.length + (failedMysql ? 1 : 0);
}

/** Errors that mean the connection itself is in trouble. */
export function isConnectionError(e: CoreError | null | undefined) {
  return !!e && ["connection", "ssh", "not_connected", "host_key_mismatch"].includes(e.kind);
}

export type Light = "ok" | "busy" | "bad";

/** Red when the last failure was the connection, yellow while running or right after a reconnect. */
export function light(s: { running: boolean; reconnected: boolean; connectionError: boolean }): Light {
  if (s.running) return "busy";
  if (s.connectionError) return "bad";
  return s.reconnected ? "busy" : "ok";
}

/** Share of `wait_timeout` after which the server may have dropped the connection. */
export const IDLE_WARN_RATIO = 0.8;

/**
 * Whether the server may already have closed an idle connection: nothing
 * reached it (a run or a ping) for more than 80% of `wait_timeout`.
 */
export function mayBeDropped(lastContact: number, now: number, waitTimeoutSec: number | null) {
  if (!waitTimeoutSec) return false;
  return now - lastContact > waitTimeoutSec * 1000 * IDLE_WARN_RATIO;
}

/** Pings stop after this much idle time, so `wait_timeout` still applies. */
export const PING_WHILE_IDLE_MS = 5 * 60_000;

/** Whether to measure latency now: idle, but not idle for long. */
export function shouldPing(s: { running: boolean; lastActivity: number; now: number }) {
  return !s.running && s.now - s.lastActivity < PING_WHILE_IDLE_MS;
}

/** `閒置 3 分鐘`, or null under a minute. */
export function idleText(ms: number) {
  const mins = Math.floor(ms / 60_000);
  if (mins < 1) return null;
  if (mins < 60) return `閒置 ${mins} 分鐘`;
  const h = Math.floor(mins / 60);
  return `閒置 ${h} 小時${mins % 60 ? ` ${mins % 60} 分` : ""}`;
}

/** The local date, `2026-10-25`. */
export function localDate(now: Date) {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

type Store = Pick<Storage, "getItem" | "setItem">;

export const TODAY_KEY = "mydb.usage.today";

/** Statements run in this app today; a stored count from another day is 0. */
export function todayCount(store: Store, now: Date) {
  try {
    const saved = JSON.parse(store.getItem(TODAY_KEY) ?? "null") as { date?: string; count?: number } | null;
    return saved?.date === localDate(now) && typeof saved.count === "number" ? saved.count : 0;
  } catch {
    return 0;
  }
}

/** Adds `n` to today's count and returns the new total. */
export function addToday(store: Store, n: number, now: Date) {
  const count = todayCount(store, now) + n;
  try {
    store.setItem(TODAY_KEY, JSON.stringify({ date: localDate(now), count }));
  } catch {
    // Kept in memory by the caller until the app closes.
  }
  return count;
}
