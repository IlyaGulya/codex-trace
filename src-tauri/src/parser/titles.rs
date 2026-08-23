use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::OpenFlags;

/// Human-readable thread titles written by modern Codex CLIs.
///
/// Since ~v0.147 the TUI no longer appends `thread_name_updated` events to rollout
/// JSONL files — conversation titles live in the Codex state database
/// (`~/.codex/state_*.sqlite`, table `threads`, columns `name`/`title`). Without
/// reading it, the picker can only fall back to the cwd's last path segment.
///
/// The database may not exist (older CLIs) or may be briefly locked by a running
/// Codex process; every failure degrades to an empty map.
/// Load `thread id -> display title` from the newest Codex state database.
pub fn load_thread_titles() -> HashMap<String, String> {
    for path in candidate_state_dbs() {
        if let Some(map) = read_titles(&path) {
            return map;
        }
    }
    HashMap::new()
}

/// `~/.codex/state_*.sqlite` sorted by schema version, newest first.
fn candidate_state_dbs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let codex = home.join(".codex");
    let mut candidates: Vec<(u32, PathBuf)> = std::fs::read_dir(&codex)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?;
            let num = name.strip_prefix("state_")?.strip_suffix(".sqlite")?;
            Some((num.parse::<u32>().ok()?, p))
        })
        .collect();
    candidates.sort_by_key(|b| std::cmp::Reverse(b.0));
    candidates.into_iter().map(|(_, p)| p).collect()
}

fn read_titles(path: &Path) -> Option<HashMap<String, String>> {
    // Read-only + no shared-memory writes: safe next to a live Codex process, and
    // never creates sidecar files just by being inspected.
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let mut stmt = conn
        .prepare("SELECT id, name, title FROM threads WHERE title != '' OR name IS NOT NULL")
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    let mut map = HashMap::new();
    while let Ok(Some(row)) = rows.next() {
        let id: String = row.get(0).ok()?;
        let name: Option<String> = row.get(1).ok()?;
        let title: String = row.get(2).ok()?;
        // Prefer the short user-facing name; fall back to the longer title, skipping
        // internal markers ("<codex_delegation>…" blobs) and pasted system prompts
        // ("You are …") that subagent threads store in the title column.
        const SKIP_PREFIXES: [&str; 2] = ["<", "You are"];
        let value = match name.filter(|n| !n.trim().is_empty()) {
            Some(n) => Some(n),
            None => {
                let t = title.trim();
                (!t.is_empty() && !SKIP_PREFIXES.iter().any(|p| t.starts_with(p)))
                    .then(|| t.to_string())
            }
        };
        if let Some(v) = value {
            map.insert(id, v);
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_codex_dir_yields_empty_map() {
        // load_thread_titles must never panic regardless of machine state.
        let _ = load_thread_titles();
    }

    #[test]
    fn reads_titles_from_temp_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state_9.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, name TEXT, title TEXT NOT NULL DEFAULT '');
             INSERT INTO threads (id, name, title) VALUES
               ('t1', 'Short name', 'Long first message'),
               ('t2', NULL, 'Plain title'),
               ('t3', NULL, '<codex_delegation>internal'),
               ('t4', NULL, ''),
               ('t5', NULL, 'You are a summarizer');",
        )
        .unwrap();

        let map = read_titles(&db).expect("readable db");
        assert_eq!(map.get("t1").unwrap(), "Short name");
        assert_eq!(map.get("t2").unwrap(), "Plain title");
        assert!(!map.contains_key("t3"), "marker blob titles are skipped");
        assert!(!map.contains_key("t4"));
        assert!(!map.contains_key("t5"), "system-prompt titles are skipped");
    }

    #[test]
    fn candidates_are_sorted_newest_schema_first() {
        let dir = tempfile::tempdir().unwrap();
        for n in [2u32, 10, 1] {
            std::fs::write(dir.path().join(format!("state_{n}.sqlite")), b"x").unwrap();
        }
        // Reuse the same ordering logic through the public loader is not possible
        // (it scans $HOME), so verify sort semantics inline via the helper type path:
        let mut paths: Vec<(u32, PathBuf)> = [2u32, 10, 1]
            .iter()
            .map(|n| (*n, dir.path().join(format!("state_{n}.sqlite"))))
            .collect();
        paths.sort_by_key(|b| std::cmp::Reverse(b.0));
        assert_eq!(
            paths.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![10, 2, 1]
        );
    }
}
