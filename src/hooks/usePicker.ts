import { useState, useEffect, useCallback } from "react";
import { invoke } from "../lib/invoke";
import type { CodexSessionInfo, SettingsResponse } from "../../shared/types";
import { useTauriEvent } from "./useTauriEvent";

export interface PickerProgress {
  scanned: number;
  total: number;
}

interface PickerState {
  sessions: CodexSessionInfo[];
  loading: boolean;
  searchQuery: string;
  sessionsDir: string;
}

export function usePicker() {
  const [state, setState] = useState<PickerState>({
    sessions: [],
    loading: false,
    searchQuery: "",
    sessionsDir: "",
  });
  const [progress, setProgress] = useState<PickerProgress | null>(null);

  const discoverSessions = useCallback(async (sessionsDir: string) => {
    if (!sessionsDir) return;
    setState((prev) => ({ ...prev, loading: true, sessionsDir }));
    setProgress(null);
    try {
      const sessions = await invoke<CodexSessionInfo[]>("list_sessions", { sessionsDir });
      setState((prev) => ({ ...prev, sessions, loading: false }));
      try {
        await invoke<void>("watch_picker", { sessionsDir });
      } catch {
        // watcher is optional
      }
    } catch (err) {
      console.error("Failed to discover sessions:", err);
      setState((prev) => ({ ...prev, loading: false }));
    }
  }, []);

  const setSearchQuery = useCallback((query: string) => {
    setState((prev) => ({ ...prev, searchQuery: query }));
  }, []);

  const updateSessionOngoing = useCallback((path: string, ongoing: boolean) => {
    setState((prev) => {
      const idx = prev.sessions.findIndex((s) => s.path === path);
      if (idx === -1 || prev.sessions[idx].is_ongoing === ongoing) return prev;
      const sessions = [...prev.sessions];
      sessions[idx] = { ...sessions[idx], is_ongoing: ongoing };
      return { ...prev, sessions };
    });
  }, []);

  // Progress from the background enrichment job. The final event carries
  // scanned === total, at which point we clear the indicator.
  useTauriEvent<PickerProgress>("picker-progress", (p) => {
    if (p.scanned >= p.total) {
      setProgress(null);
    } else {
      setProgress({ scanned: p.scanned, total: p.total });
    }
  });

  // A single session finished enrichment — replace it in place so the list fills
  // in live without re-fetching the whole directory.
  useTauriEvent<CodexSessionInfo>("session-enriched", (session) => {
    setState((prev) => {
      const idx = prev.sessions.findIndex((s) => s.path === session.path);
      if (idx === -1) return prev;
      const sessions = [...prev.sessions];
      sessions[idx] = session;
      return { ...prev, sessions };
    });
  });

  // picker-refresh carries no session data — the watcher sends only a lightweight
  // signal (also emitted when enrichment completes, after inline-worker links are
  // resolved). Re-fetch via the API so the expensive scan runs only on demand.
  useTauriEvent("picker-refresh", () => {
    setState((prev) => {
      if (!prev.sessionsDir) return prev;
      invoke<CodexSessionInfo[]>("list_sessions", { sessionsDir: prev.sessionsDir })
        .then((sessions) => setState((s) => ({ ...s, sessions, loading: false })))
        .catch(() => {});
      return prev;
    });
  });

  useEffect(() => {
    return () => {
      invoke<void>("unwatch_picker").catch(() => {});
    };
  }, []);

  const filteredSessions = state.searchQuery
    ? state.sessions.filter(
        (s) =>
          (s.thread_name ?? "").toLowerCase().includes(state.searchQuery.toLowerCase()) ||
          s.id.toLowerCase().includes(state.searchQuery.toLowerCase()) ||
          (s.cwd ?? "").toLowerCase().includes(state.searchQuery.toLowerCase()),
      )
    : state.sessions;

  return {
    sessions: filteredSessions,
    allSessions: state.sessions,
    loading: state.loading,
    searchQuery: state.searchQuery,
    sessionsDir: state.sessionsDir,
    progress,
    setSearchQuery,
    discoverSessions,
    updateSessionOngoing,
  };
}

export async function resolveSessionsDir(): Promise<string> {
  const settings = await invoke<SettingsResponse>("get_settings");
  return settings.sessions_dir ?? settings.default_dir;
}
