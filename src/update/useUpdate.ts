import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, describeError, type AppInfo, type UpdateInfo } from "../api";

/** After the app opens, so the check doesn't compete with the first connection. */
const FIRST_CHECK_MS = 8_000;
const CHECK_EVERY_MS = 12 * 60 * 60 * 1000;

export type UpdatePhase =
  | { phase: "idle" }
  | { phase: "checking" }
  | { phase: "latest"; checkedAt: Date }
  | { phase: "available"; update: UpdateInfo }
  | { phase: "downloading"; update: UpdateInfo; downloaded: number; total: number | null }
  | { phase: "installing"; update: UpdateInfo }
  | { phase: "ready"; version: string }
  | { phase: "error"; message: string; update?: UpdateInfo };

export interface Updates {
  info: AppInfo | null;
  state: UpdatePhase;
  /** A newer release is out (whatever is happening with it now). */
  newer: string | null;
  check: () => void;
  install: () => void;
  restart: () => void;
}

/** "0.4.1" or "0.4.1 IAP". */
export function versionLabel(info: AppInfo) {
  return info.flavor ? `${info.version} ${info.flavor}` : info.version;
}

/** The app's version, and checking for and installing a newer release. */
export function useUpdate(autoCheck: boolean, settingsLoaded: boolean): Updates {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [state, setState] = useState<UpdatePhase>({ phase: "idle" });
  const busy = useRef(false);

  useEffect(() => {
    api.appInfo().then(setInfo, () => {});
  }, []);

  const check = useCallback((quiet = false) => {
    if (busy.current) return;
    busy.current = true;
    if (!quiet) setState({ phase: "checking" });
    api
      .checkUpdate()
      .then(
        (update) =>
          setState((s) =>
            update ? { phase: "available", update } : quiet && s.phase !== "idle" ? s : { phase: "latest", checkedAt: new Date() },
          ),
        // A failed background check (offline, say) stays quiet.
        (e) => !quiet && setState({ phase: "error", message: describeError(e) }),
      )
      .finally(() => (busy.current = false));
  }, []);

  // Only the first window checks on its own; others show what you ask for.
  useEffect(() => {
    if (!settingsLoaded || !autoCheck || getCurrentWindow().label !== "main") return;
    const first = setTimeout(() => check(true), FIRST_CHECK_MS);
    const timer = setInterval(() => check(true), CHECK_EVERY_MS);
    return () => {
      clearTimeout(first);
      clearInterval(timer);
    };
  }, [autoCheck, settingsLoaded, check]);

  const install = useCallback(() => {
    const update =
      state.phase === "available" ? state.update : state.phase === "error" ? state.update : undefined;
    if (!update || busy.current) return;
    busy.current = true;
    setState({ phase: "downloading", update, downloaded: 0, total: null });
    api
      .installUpdate((e) =>
        setState(
          e.event === "installing"
            ? { phase: "installing", update }
            : { phase: "downloading", update, downloaded: e.downloaded, total: e.total },
        ),
      )
      .then(
        (version) => setState({ phase: "ready", version }),
        (e) => setState({ phase: "error", message: describeError(e), update }),
      )
      .finally(() => (busy.current = false));
  }, [state]);

  const restart = useCallback(() => {
    api.restartApp().catch(() => {});
  }, []);

  const newer =
    state.phase === "available" || state.phase === "downloading" || state.phase === "installing"
      ? state.update.version
      : state.phase === "ready"
        ? state.version
        : state.phase === "error" && state.update
          ? state.update.version
          : null;

  return { info, state, newer, check: () => check(false), install, restart };
}
