use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

use crate::parser::cache::PersistentCache;
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
    /// When the directory was last actually rescanned; used to rate-limit watcher-driven
    /// rescans while a Codex process is actively appending to session files.
    last_rescan: Option<std::time::Instant>,
}

/// Minimum interval between full header rescans of the sessions directory. An actively
/// written rollout file triggers the picker watcher on every append; without this bound
/// each append would re-scan every file and re-parse the (potentially huge) active one.
#[cfg(test)]
const MIN_RESCAN_INTERVAL: std::time::Duration = std::time::Duration::ZERO;
#[cfg(not(test))]
const MIN_RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// One session that needs its deferred metadata recomputed.
enum EnrichJob {
    /// No usable previous scan — parse from the beginning.
    Full(CodexSessionInfo),
    /// A previous scan exists; resume parsing just past its bookmark.
    Incremental(CodexSessionInfo, discover::ScanBookmark),
}

pub struct AppState {
    pub session_watcher: Mutex<Option<WatcherHandle>>,
    pub picker_watcher: Mutex<Option<WatcherHandle>>,
    pub settings: Mutex<Settings>,
    pub watched_session_ongoing: Mutex<Option<(String, bool)>>,
    pub event_tx: broadcast::Sender<SseEvent>,
    discovery: Mutex<DiscoveryState>,
    /// On-disk cache of enriched session metadata keyed by (path, mtime, size).
    cache: Mutex<PersistentCache>,
    /// Dedicated, reduced-size thread pool for background enrichment. The default rayon
    /// global pool spans every logical CPU, so a 7 GB enrichment scan would saturate all
    /// cores and starve the webview UI. This pool leaves headroom for rendering + tokio.
    enrich_pool: Arc<rayon::ThreadPool>,
}

/// How many threads the background enrichment job may use. We deliberately leave at least
/// half the logical CPUs idle so the webview (a separate process scheduled by the OS) and
/// the tokio runtime stay responsive while session files are parsed in the background.
fn enrich_thread_count() -> usize {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cpus / 2).max(1)
}

/// Demote the calling thread to background QoS so the OS scheduler always favours the
/// webview's interactive work over enrichment parsing. Without this, even a few
/// CPU-bound rayon threads can starve the UI process and cause visible jank.
#[cfg(target_os = "macos")]
fn lower_background_priority() {
    unsafe {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        // QOS_CLASS_UTILITY (0x11): run eagerly on spare cores but yield to UI I/O + CPU.
        pthread_set_qos_class_self_np(0x11, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn lower_background_priority() {}

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
        let enrich_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(enrich_thread_count())
            .thread_name(|i| format!("codex-enrich-{i}"))
            .start_handler(|_| lower_background_priority())
            .build()
            .expect("failed to build enrichment thread pool");
        Self {
            session_watcher: Mutex::new(None),
            picker_watcher: Mutex::new(None),
            settings: Mutex::new(crate::settings::load_settings()),
            watched_session_ongoing: Mutex::new(None),
            event_tx,
            discovery: Mutex::new(DiscoveryState::default()),
            cache: Mutex::new(PersistentCache::load()),
            enrich_pool: Arc::new(enrich_pool),
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
    /// Returns the session list immediately: unchanged files are served from the on-disk
    /// cache (full metadata), and only new/modified files are left as headers to be
    /// enriched by a background thread that streams `picker-progress` and
    /// `session-enriched` events. Subsequent calls for the same `dir` return the current
    /// in-memory snapshot without rescanning, unless the watcher marked it dirty.
    pub fn discover_sessions(
        self: &Arc<Self>,
        dir: &str,
        app: &Option<AppHandle>,
    ) -> Result<Vec<CodexSessionInfo>, String> {
        let (sessions, jobs, generation) = {
            let mut guard = self.discovery.lock().map_err(|e| e.to_string())?;
            if guard.dir == dir && !guard.dirty {
                return Ok(guard.sessions.clone());
            }

            let path = std::path::Path::new(dir);
            // Scope the header scan to the reduced, low-priority pool so it never
            // saturates every core while the UI is rendering.
            let headers = self
                .enrich_pool
                .install(|| discover::discover_sessions_fast(path))?;

            // Classify each file: unchanged files are served straight from cache,
            // actively-growing files resume incrementally from their scan bookmark,
            // and everything else gets a full background scan.
            let cache = self.cache.lock().map_err(|e| e.to_string())?;
            let mut sessions = Vec::with_capacity(headers.len());
            let mut jobs: Vec<EnrichJob> = Vec::new();
            for header in headers {
                let meta = std::fs::metadata(&header.path).ok();
                if let Some(m) = meta.as_ref() {
                    if let Ok(mtime) = m.modified() {
                        if let Some(full) = cache.get(&header.path, mtime, m.len()) {
                            sessions.push(full);
                            continue;
                        }
                    }
                }

                let resumable = meta.as_ref().and_then(|m| {
                    let stale = cache.get_stale(&header.path)?;
                    // Only plain (seekable) files can skip their scanned prefix, and
                    // the file must not have shrunk below that position.
                    (header.path.ends_with(".jsonl") && m.len() >= stale.1.bytes).then_some(stale)
                });

                match resumable {
                    Some((stale_info, bookmark)) => {
                        let mut shown = stale_info.clone();
                        shown.is_ongoing = false;
                        sessions.push(shown);
                        jobs.push(EnrichJob::Incremental(stale_info, bookmark));
                    }
                    None => {
                        sessions.push(header.clone());
                        jobs.push(EnrichJob::Full(header));
                    }
                }
            }
            drop(cache);

            // Cached entries already carry spawned_worker_ids, so resolve inline-worker
            // links now for a correct initial view (the enrichment pass re-runs this
            // after it fills in the newly-scanned files).
            discover::mark_inline_workers(&mut sessions);

            let generation = guard.generation + 1;
            guard.dir = dir.to_string();
            guard.generation = generation;
            guard.sessions = sessions.clone();
            guard.dirty = false;
            guard.last_rescan = Some(std::time::Instant::now());
            (sessions, jobs, generation)
        };

        // Everything was served from the cache — nothing to scan, no refresh needed.
        if jobs.is_empty() {
            return Ok(sessions);
        }

        self.spawn_enrichment(app.clone(), generation, jobs);
        Ok(sessions)
    }

    /// Mark the cached discovery list stale so the next [`Self::discover_sessions`] call
    /// rescans. Called by the picker watcher when session files change on disk.
    pub fn mark_discovery_dirty(&self) {
        if let Ok(mut guard) = self.discovery.lock() {
            guard.dirty = true;
        }
    }

    fn spawn_enrichment(
        self: &Arc<Self>,
        app: Option<AppHandle>,
        generation: u64,
        jobs: Vec<EnrichJob>,
    ) {
        let state = self.clone();
        let pool = self.enrich_pool.clone();
        let total = jobs.len();

        std::thread::spawn(move || {
            let scanned = Arc::new(AtomicUsize::new(0));

            // `install` scopes the parallel loop to the reduced-size pool so enrichment
            // never saturates all logical CPUs.
            pool.install(|| {
                jobs.into_par_iter().for_each(|job| {
                    // Resume from the previous scan position when possible so an
                    // actively-appended rollout only has its new tail parsed.
                    let (enriched, bookmark) = match job {
                        EnrichJob::Full(header) => {
                            let path = PathBuf::from(&header.path);
                            let mut info = header;
                            let bm = discover::enrich_session_info(&mut info, &path);
                            (info, bm)
                        }
                        EnrichJob::Incremental(seed, bookmark) => {
                            let path = PathBuf::from(&seed.path);
                            let mut info = seed;
                            let bm =
                                discover::enrich_session_incremental(&mut info, &path, &bookmark)
                                    .unwrap_or_else(|| {
                                        discover::enrich_session_info(&mut info, &path)
                                    });
                            (info, bm)
                        }
                    };

                    let done = scanned.fetch_add(1, Ordering::Relaxed) + 1;

                    {
                        let mut guard = match state.discovery.lock() {
                            Ok(g) => g,
                            Err(_) => return,
                        };
                        if guard.generation != generation {
                            return;
                        }
                        if let Some(entry) =
                            guard.sessions.iter_mut().find(|s| s.path == enriched.path)
                        {
                            *entry = enriched.clone();
                        }
                    }

                    // Persist this file's result (and its resume position) so both the
                    // next cold start and the next incremental pass skip what's done.
                    let path = PathBuf::from(&enriched.path);
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(mtime) = meta.modified() {
                            if let Ok(mut cache) = state.cache.lock() {
                                cache.put(
                                    &enriched.path,
                                    mtime,
                                    meta.len(),
                                    bookmark,
                                    enriched.clone(),
                                );
                            }
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
            });

            // Finalize: mark inline workers across the (cached + freshly-enriched) list.
            {
                let mut guard = match state.discovery.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if guard.generation == generation {
                    discover::mark_inline_workers(&mut guard.sessions);
                }
            }

            // Persist the cache so the next cold start skips unchanged files.
            if let Ok(cache) = state.cache.lock() {
                cache.save();
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
