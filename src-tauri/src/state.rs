use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::parser::discover::{self, CodexSessionInfo};
use crate::settings::Settings;
use crate::watcher::WatcherHandle;

/// A Server-Sent Event destined for browser clients.
#[derive(Clone, Debug)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// In-memory discovery state for the session picker.
///
/// Holds the progressively-enriched session list plus the bookkeeping needed to
/// deduplicate/restart the background enrichment job when the sessions directory
/// changes or a file event invalidates the cached list.
#[derive(Default)]
struct DiscoveryState {
    dir: String,
    generation: u64,
    sessions: Vec<CodexSessionInfo>,
    dirty: bool,
}

pub struct AppState {
    pub session_watcher: Mutex<Option<WatcherHandle>>,
    pub picker_watcher: Mutex<Option<WatcherHandle>>,
    pub settings: Mutex<Settings>,
    pub watched_session_ongoing: Mutex<Option<(String, bool)>>,
    pub event_tx: broadcast::Sender<SseEvent>,
    discovery: Mutex<DiscoveryState>,
}

/// Broadcast an event to both the SSE stream (web clients) and the Tauri event bus
/// (desktop webview). `data` is the JSON payload; both channels deliver the same shape.
fn broadcast(state: &AppState, app: &Option<AppHandle>, event: &str, data: &str) {
    state.broadcast(event, data);
    if let Some(handle) = app {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
            let _ = handle.emit(event, value);
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            session_watcher: Mutex::new(None),
            picker_watcher: Mutex::new(None),
            settings: Mutex::new(crate::settings::load_settings()),
            watched_session_ongoing: Mutex::new(None),
            event_tx,
            discovery: Mutex::new(DiscoveryState::default()),
        }
    }

    pub fn stop_session_watcher(&self) -> Result<(), String> {
        let mut guard = self.session_watcher.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = guard.take() {
            handle.stop();
        }
        Ok(())
    }

    pub fn set_session_watcher(&self, handle: WatcherHandle) -> Result<(), String> {
        let mut guard = self.session_watcher.lock().map_err(|e| e.to_string())?;
        *guard = Some(handle);
        Ok(())
    }

    pub fn stop_picker_watcher(&self) -> Result<(), String> {
        let mut guard = self.picker_watcher.lock().map_err(|e| e.to_string())?;
        if let Some(handle) = guard.take() {
            handle.stop();
        }
        Ok(())
    }

    pub fn set_picker_watcher(&self, handle: WatcherHandle) -> Result<(), String> {
        let mut guard = self.picker_watcher.lock().map_err(|e| e.to_string())?;
        *guard = Some(handle);
        Ok(())
    }

    pub fn set_watched_ongoing(&self, path: String, ongoing: bool) {
        if let Ok(mut guard) = self.watched_session_ongoing.lock() {
            *guard = Some((path, ongoing));
        }
    }

    pub fn clear_watched_ongoing(&self) {
        if let Ok(mut guard) = self.watched_session_ongoing.lock() {
            *guard = None;
        }
    }

    pub fn apply_watched_ongoing(&self, sessions: &mut [CodexSessionInfo]) {
        let guard = match self.watched_session_ongoing.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some((ref path, ongoing)) = *guard {
            if let Some(s) = sessions.iter_mut().find(|s| s.path == *path) {
                s.is_ongoing = ongoing;
            }
        }
    }

    /// Fast session discovery plus a background enrichment job.
    ///
    /// Returns the header-only session list immediately (no full-file scans), and spawns
    /// a background thread that enriches each session and streams `picker-progress` and
    /// `session-enriched` events. Subsequent calls for the same `dir` return the current
    /// in-memory snapshot without rescanning, unless the watcher marked it dirty.
    pub fn discover_sessions(
        self: &Arc<Self>,
        dir: &str,
        app: &Option<AppHandle>,
    ) -> Result<Vec<CodexSessionInfo>, String> {
        let (sessions, generation) = {
            let mut guard = self.discovery.lock().map_err(|e| e.to_string())?;
            if guard.dir == dir && !guard.dirty {
                return Ok(guard.sessions.clone());
            }

            let path = std::path::Path::new(dir);
            let sessions = discover::discover_sessions_fast(path)?;
            let generation = guard.generation + 1;
            guard.dir = dir.to_string();
            guard.generation = generation;
            guard.sessions = sessions.clone();
            guard.dirty = false;
            (sessions, generation)
        };

        self.spawn_enrichment(dir.to_string(), app.clone(), generation);
        Ok(sessions)
    }

    /// Mark the cached discovery list stale so the next [`Self::discover_sessions`] call
    /// rescans. Called by the picker watcher when session files change on disk.
    pub fn mark_discovery_dirty(&self) {
        if let Ok(mut guard) = self.discovery.lock() {
            guard.dirty = true;
        }
    }

    fn spawn_enrichment(self: &Arc<Self>, _dir: String, app: Option<AppHandle>, generation: u64) {
        let state = self.clone();
        let sessions = {
            let guard = match self.discovery.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            guard.sessions.clone()
        };

        let total = sessions.len();

        std::thread::spawn(move || {
            let scanned = Arc::new(AtomicUsize::new(0));

            sessions.par_iter().for_each(|info| {
                let mut enriched = info.clone();
                let path = PathBuf::from(&info.path);
                discover::enrich_session_info(&mut enriched, &path);

                let done = scanned.fetch_add(1, Ordering::Relaxed) + 1;

                {
                    let mut guard = match state.discovery.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    if guard.generation != generation {
                        return;
                    }
                    if let Some(entry) = guard.sessions.iter_mut().find(|s| s.path == enriched.path)
                    {
                        *entry = enriched.clone();
                    }
                }

                if let Ok(data) = serde_json::to_string(&enriched) {
                    broadcast(&state, &app, "session-enriched", &data);
                }

                // Throttle progress broadcasts to keep the event stream light.
                if done == total || done % 25 == 0 {
                    let progress = serde_json::json!({ "scanned": done, "total": total });
                    broadcast(&state, &app, "picker-progress", &progress.to_string());
                }
            });

            // Finalize: mark inline workers across the now-fully-enriched list, then tell
            // clients to re-fetch for a consistent view.
            {
                let mut guard = match state.discovery.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if guard.generation == generation {
                    discover::mark_inline_workers(&mut guard.sessions);
                }
            }
            broadcast(&state, &app, "picker-refresh", "{}");
        });
    }

    pub fn broadcast(&self, event: &str, data: &str) {
        let _ = self.event_tx.send(SseEvent {
            event: event.to_string(),
            data: data.to_string(),
        });
    }

    /// Broadcast to both SSE (web) and the Tauri event bus (desktop webview).
    pub fn broadcast_all(&self, app: &Option<AppHandle>, event: &str, data: &str) {
        broadcast(self, app, event, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> Arc<AppState> {
        Arc::new(AppState::new())
    }

    #[test]
    fn discover_sessions_returns_empty_for_nonexistent_dir() {
        let state = make_state();
        let result = state.discover_sessions("/nonexistent/path/that/does/not/exist", &None);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn discover_sessions_returns_snapshot_without_rescanning() {
        let state = make_state();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        let first = state.discover_sessions(path, &None).unwrap();
        assert!(first.is_empty());

        let second = state.discover_sessions(path, &None).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn mark_discovery_dirty_forces_rescan() {
        let state = make_state();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        assert!(state.discover_sessions(path, &None).unwrap().is_empty());

        let day_dir = dir.path().join("2026/05/07");
        std::fs::create_dir_all(&day_dir).unwrap();
        let file = day_dir.join("rollout-2026-05-07T00-00-00-abc.jsonl");
        std::fs::write(
            &file,
            r#"{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{"id":"abc","timestamp":"2026-05-07T00:00:00Z","cwd":"/tmp"}}"#,
        )
        .unwrap();

        state.mark_discovery_dirty();
        let result = state.discover_sessions(path, &None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "abc");
    }
}
