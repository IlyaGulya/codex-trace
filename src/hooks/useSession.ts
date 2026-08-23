import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "../lib/invoke";
import type { CodexSession } from "../../shared/types";
import { useTauriEvent } from "./useTauriEvent";

export interface SessionLoadProgress {
  path: string;
  done: number;
  total: number;
}

interface SessionState {
  session: CodexSession | null;
  loading: boolean;
  sessionPath: string;
}

export function useSession() {
  const [state, setState] = useState<SessionState>({
    session: null,
    loading: false,
    sessionPath: "",
  });
  const [loadProgress, setLoadProgress] = useState<SessionLoadProgress | null>(null);
  const loadingPathRef = useRef<string | null>(null);

  const loadSession = useCallback(async (path: string) => {
    loadingPathRef.current = path;
    setLoadProgress(null);
    setState((prev) => ({ ...prev, loading: true }));
    try {
      try {
        await invoke<void>("unwatch_session");
      } catch {
        // ignore
      }
      const session = await invoke<CodexSession>("load_session", { path });
      setState({ session, loading: false, sessionPath: path });
      try {
        await invoke<void>("watch_session", { path });
      } catch {
        // watcher is optional
      }
    } catch (err) {
      console.error("Failed to load session:", err);
      setState((prev) => ({ ...prev, loading: false }));
    } finally {
      loadingPathRef.current = null;
      setLoadProgress(null);
    }
  }, []);

  useTauriEvent<{ session: CodexSession }>("session-update", (payload) => {
    setState((prev) => ({ ...prev, session: payload.session }));
  });

  // Parse progress for the session currently being loaded.
  useTauriEvent<SessionLoadProgress>("session-load-progress", (p) => {
    if (loadingPathRef.current === p.path) {
      setLoadProgress({ path: p.path, done: p.done, total: p.total });
    }
  });

  useEffect(() => {
    return () => {
      invoke<void>("unwatch_session").catch(() => {});
    };
  }, []);

  return {
    ...state,
    loadProgress,
    loadSession,
  };
}
