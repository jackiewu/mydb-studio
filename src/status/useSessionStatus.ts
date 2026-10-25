import { useCallback, useEffect, useRef, useState } from "react";
import { api, type CoreError } from "../api";
import {
  addToday,
  isConnectionError,
  light,
  mayBeDropped,
  shouldPing,
  statementCount,
  todayCount,
  type Light,
  type RunSummary,
} from "./usage";

/** How often the status bar measures latency while idle. */
const PING_EVERY_MS = 30_000;
/** How often the idle time and its warning are brought up to date. */
const CLOCK_MS = 10_000;

export interface SessionStatus {
  light: Light;
  /** The light says so because the last run reconnected. */
  reconnected: boolean;
  /** Last measured round trip; null before the first ping. */
  latencyMs: number | null;
  /** Statements (or commands) run on this connection. */
  sessionCount: number;
  /** Statements run in this app today, any connection. */
  today: number;
  /** Since the last run, or since connecting. */
  idleMs: number;
  /** MySQL's `wait_timeout` in seconds. */
  waitTimeout: number | null;
  /** Idle long enough that the server may have closed the connection. */
  mayBeDropped: boolean;
}

function storage(): Storage | null {
  try {
    return localStorage;
  } catch {
    return null;
  }
}

/**
 * Connection health and usage for the status bar. `record` takes each run
 * of the editor; `recordError` a run that could not start.
 */
export function useSessionStatus(sessionId: string | null, running: boolean) {
  const [now, setNow] = useState(() => Date.now());
  const [latencyMs, setLatency] = useState<number | null>(null);
  const [sessionCount, setSessionCount] = useState(0);
  const [today, setToday] = useState(() => {
    const s = storage();
    return s ? todayCount(s, new Date()) : 0;
  });
  const [reconnected, setReconnected] = useState(false);
  const [connectionError, setConnectionError] = useState(false);
  const [waitTimeout, setWaitTimeout] = useState<number | null>(null);
  // Last run (or connect), and last time anything reached the server.
  const [lastActivity, setLastActivity] = useState(() => Date.now());
  const [lastContact, setLastContact] = useState(() => Date.now());

  const lastPing = useRef(0);
  const ping = useCallback(() => {
    if (!sessionId) return;
    lastPing.current = Date.now();
    api.sessionPing(sessionId).then(
      (ms) => {
        if (ms == null) return;
        setLatency(ms);
        setLastContact(Date.now());
        setConnectionError(false);
      },
      (e: CoreError) => {
        if (isConnectionError(e)) {
          setConnectionError(true);
          setLatency(null);
        }
      },
    );
  }, [sessionId]);

  useEffect(() => {
    const t = Date.now();
    setSessionCount(0);
    setLatency(null);
    setReconnected(false);
    setConnectionError(false);
    setWaitTimeout(null);
    setLastActivity(t);
    setLastContact(t);
    if (!sessionId) return;
    let live = true;
    api.idleTimeout(sessionId).then(
      (s) => live && setWaitTimeout(s ?? null),
      () => {},
    );
    // Right after connecting, the schema tree is still loading on the
    // connection; a ping then would only step aside.
    const first = setTimeout(ping, 2000);
    return () => {
      live = false;
      clearTimeout(first);
    };
  }, [sessionId, ping]);

  // The clock for the idle time, and a ping now and then while idle.
  const state = useRef({ running, lastActivity });
  state.current = { running, lastActivity };
  useEffect(() => {
    const id = setInterval(() => {
      const t = Date.now();
      setNow(t);
      const s = storage();
      if (s) setToday(todayCount(s, new Date(t)));
      if (
        t - lastPing.current >= PING_EVERY_MS &&
        document.visibilityState === "visible" &&
        shouldPing({ ...state.current, now: t })
      )
        ping();
    }, CLOCK_MS);
    return () => clearInterval(id);
  }, [ping]);

  /** `text` is what ran; a reconnect or a `SET … wait_timeout` changes the timeout. */
  const record = useCallback(
    (o: RunSummary, text = "") => {
      const t = Date.now();
      const n = statementCount(o);
      setSessionCount((c) => c + n);
      const s = storage();
      setToday((c) => (s ? addToday(s, n, new Date(t)) : c + n));
      setLastActivity(t);
      setLastContact(t);
      setNow(t);
      setReconnected(o.reconnected);
      const lost = !o.cancelled && isConnectionError(o.error);
      setConnectionError(lost);
      if (lost) setLatency(null);
      if (sessionId && (o.reconnected || /wait_timeout/i.test(text)))
        api.idleTimeout(sessionId).then(
          (s) => setWaitTimeout(s ?? null),
          () => {},
        );
    },
    [sessionId],
  );

  const recordError = useCallback((e: unknown) => {
    if (isConnectionError(e as CoreError)) setConnectionError(true);
  }, []);

  const shownNow = Math.max(now, lastActivity);
  const status: SessionStatus = {
    light: light({ running, reconnected, connectionError }),
    reconnected,
    latencyMs,
    sessionCount,
    today,
    idleMs: running ? 0 : shownNow - lastActivity,
    waitTimeout,
    mayBeDropped: !running && !connectionError && mayBeDropped(lastContact, shownNow, waitTimeout),
  };
  return { status, record, recordError };
}
