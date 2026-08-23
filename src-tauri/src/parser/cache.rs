use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::discover::CodexSessionInfo;

const CACHE_VERSION: u32 = 1;

/// One cached session, keyed by its absolute file path.
#[derive(Serialize, Deserialize)]
struct CachedEntry {
    mtime_ns: u128,
    size: u64,
    info: CodexSessionInfo,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, CachedEntry>,
}

impl Default for CacheFile {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            entries: HashMap::new(),
        }
    }
}

/// On-disk cache of enriched session metadata, keyed by (path, mtime, size).
///
/// Session files are immutable once Codex stops writing them, so an unchanged
/// (mtime, size) pair means the previously-computed [`CodexSessionInfo`] is still
/// valid. This lets a cold start skip re-reading gigabytes of JSONL: only new or
/// modified files are re-enriched, the rest are served from the cache.
pub struct PersistentCache {
    path: PathBuf,
    data: CacheFile,
}

impl PersistentCache {
    pub fn load() -> Self {
        Self::load_from(cache_path())
    }

    fn load_from(path: PathBuf) -> Self {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<CacheFile>(&s).ok())
            .filter(|c| c.version == CACHE_VERSION)
            .unwrap_or_default();
        Self { path, data }
    }

    /// Return the cached info for `path` if the file is unchanged (same mtime + size).
    ///
    /// `is_ongoing` is cleared: it depends on the file being freshly written (mtime
    /// within the last minute), which can't hold across a cache round-trip.
    pub fn get(&self, path: &str, mtime: SystemTime, size: u64) -> Option<CodexSessionInfo> {
        let entry = self.data.entries.get(path)?;
        if entry.mtime_ns != mtime_nanos(mtime) || entry.size != size {
            return None;
        }
        let mut info = entry.info.clone();
        info.is_ongoing = false;
        Some(info)
    }

    pub fn put(&mut self, path: &str, mtime: SystemTime, size: u64, info: CodexSessionInfo) {
        self.data.entries.insert(
            path.to_string(),
            CachedEntry {
                mtime_ns: mtime_nanos(mtime),
                size,
                info,
            },
        );
    }

    pub fn save(&self) {
        let json = match serde_json::to_string(&self.data) {
            Ok(j) => j,
            Err(_) => return,
        };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, json);
    }
}

fn cache_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEXTRACE_CACHE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("sessions-cache.json");
        }
    }
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("codex-trace")
        .join("sessions-cache.json")
}

fn mtime_nanos(t: SystemTime) -> u128 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_info(id: &str) -> CodexSessionInfo {
        CodexSessionInfo {
            id: id.to_string(),
            path: "/tmp/rollout.jsonl".to_string(),
            cwd: None,
            git_branch: None,
            originator: None,
            model: None,
            cli_version: None,
            thread_name: None,
            turn_count: 0,
            start_time: String::new(),
            end_time: None,
            total_tokens: None,
            is_ongoing: true,
            is_external_worker: false,
            is_inline_worker: false,
            is_headless: false,
            is_archived: false,
            worker_nickname: None,
            worker_role: None,
            spawned_worker_ids: vec![],
            date_group: String::new(),
            ai_title: None,
            approval_mode: None,
            history_base_thread_id: None,
        }
    }

    #[test]
    fn cache_hit_returns_info_with_ongoing_cleared() {
        let mut cache = PersistentCache {
            path: tempdir().unwrap().path().join("c.json"),
            data: CacheFile {
                version: CACHE_VERSION,
                entries: Default::default(),
            },
        };
        let mtime = UNIX_EPOCH + std::time::Duration::from_secs(100);
        cache.put("/x/rollout.jsonl", mtime, 42, sample_info("abc"));

        let hit = cache.get("/x/rollout.jsonl", mtime, 42).unwrap();
        assert_eq!(hit.id, "abc");
        assert!(!hit.is_ongoing, "cached entries are never ongoing");
    }

    #[test]
    fn cache_miss_when_mtime_or_size_differs() {
        let mut cache = PersistentCache {
            path: tempdir().unwrap().path().join("c.json"),
            data: CacheFile {
                version: CACHE_VERSION,
                entries: Default::default(),
            },
        };
        let mtime = UNIX_EPOCH + std::time::Duration::from_secs(100);
        cache.put("/x/rollout.jsonl", mtime, 42, sample_info("abc"));

        assert!(cache.get("/x/rollout.jsonl", mtime, 43).is_none());
        assert!(cache
            .get(
                "/x/rollout.jsonl",
                mtime + std::time::Duration::from_secs(1),
                42
            )
            .is_none());
        assert!(cache.get("/other.jsonl", mtime, 42).is_none());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sub").join("c.json");
        let mtime = UNIX_EPOCH + std::time::Duration::from_secs(100);
        {
            let mut cache = PersistentCache::load_from(path.clone());
            assert!(cache.get("/x/rollout.jsonl", mtime, 42).is_none());
            cache.put("/x/rollout.jsonl", mtime, 42, sample_info("abc"));
            cache.save();
        }
        assert!(path.exists());

        // Reload from disk: the fresh cache must be written with the current version
        // and the entry must round-trip (with is_ongoing cleared).
        let cache = PersistentCache::load_from(path);
        let hit = cache.get("/x/rollout.jsonl", mtime, 42).expect("cache hit");
        assert_eq!(hit.id, "abc");
        assert!(!hit.is_ongoing);
    }
}
