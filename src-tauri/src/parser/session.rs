use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::entry::{extract_session_id, RawEntry};
use super::toolcall::ToolKind;
use super::turn::{build_turns, CodexTurn, TokenInfo, TurnStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInfo {
    pub commit_hash: Option<String>,
    pub branch: Option<String>,
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSession {
    pub id: String,
    pub timestamp: String,
    pub cwd: Option<String>,
    pub originator: Option<String>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub git: Option<GitInfo>,
    pub instructions: Option<String>,
    pub turns: Vec<CodexTurn>,
    pub is_ongoing: bool,
    pub total_tokens: Option<TokenInfo>,
    pub thread_name: Option<String>,
    pub spawned_worker_ids: Vec<String>,
    pub path: String,
    pub ai_title: Option<String>,
    /// true when the session was started via `codex remote-control` (Codex v0.130.0+, PR #21424).
    /// Detected from originator == "remote-control" or source == "remote-control" in session_meta.
    pub is_headless: bool,
}

/// Parse a Codex JSONL session file into a CodexSession.
pub fn parse_session(path: &Path) -> Result<CodexSession, String> {
    let mut visited = HashSet::new();
    parse_session_inner(path, &mut visited)
}

fn parse_session_inner(
    path: &Path,
    visited: &mut HashSet<PathBuf>,
) -> Result<CodexSession, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical_path.clone()) {
        return Err(format!(
            "recursive session reference detected: {}",
            path.display()
        ));
    }

    let entries: Vec<RawEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(RawEntry::parse)
        .collect();

    let mut session = CodexSession {
        id: String::new(),
        timestamp: String::new(),
        cwd: None,
        originator: None,
        cli_version: None,
        model_provider: None,
        git: None,
        instructions: None,
        turns: Vec::new(),
        is_ongoing: false,
        total_tokens: None,
        thread_name: None,
        spawned_worker_ids: Vec::new(),
        path: path.to_string_lossy().to_string(),
        ai_title: None,
        is_headless: false,
    };

    // Parse session_meta from first matching entry
    for entry in &entries {
        match entry.entry_type.as_str() {
            "session_meta" => {
                parse_session_meta_new(&mut session, &entry.payload, &entry.raw);
                break;
            }
            "session_meta_root" => {
                parse_session_meta_root(&mut session, &entry.raw);
                break;
            }
            _ => {}
        }
    }

    // Check for explicit session_end marker (Codex v0.128.0+).
    // When present the session is definitively closed regardless of file freshness.
    let has_session_end = entries.iter().any(|e| e.entry_type == "session_end");

    // Build turns from remaining entries
    let mut turns = build_turns(&entries);

    // Extract thread_name from last thread_name_updated
    let thread_name = turns.iter().rev().find_map(|t| t.thread_name.clone());

    // Collect spawned_worker_ids from all turns
    let spawned_worker_ids: Vec<String> = turns
        .iter()
        .flat_map(|t| t.collab_spawns.iter().map(|s| s.new_session_id.clone()))
        .collect();

    // Determine total tokens from last turn's token info
    let total_tokens = turns.iter().rev().find_map(|t| t.total_tokens.clone());

    // Determine is_ongoing: last turn must be Ongoing AND file must have been
    // modified within 60 seconds (same threshold as source repo). Sessions older
    // than that have no live CLI writing to them — task_complete was simply missed
    // (crash, kill, older CLI that never emitted the event).
    // A session_end marker (v0.128.0+) overrides both heuristics: the session
    // is definitively closed even if the file is still fresh.
    let turn_ongoing = turns
        .last()
        .map(|t| t.status == super::turn::TurnStatus::Ongoing)
        .unwrap_or(false);
    let file_fresh = fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mt| {
            SystemTime::now()
                .duration_since(mt)
                .map(|e| e.as_secs() <= 60)
                .unwrap_or(true)
        })
        .unwrap_or(true);
    let is_ongoing = !has_session_end && turn_ongoing && file_fresh;

    // If the file is stale (or session_end present) and the last turn never got
    // a completion event, mark it as Aborted so the UI doesn't show an ongoing
    // indicator.
    if turn_ongoing && (!file_fresh || has_session_end) {
        if let Some(last) = turns.last_mut() {
            last.status = TurnStatus::Aborted;
        }
    }

    embed_worker_sessions(path, &mut turns, visited);

    session.turns = turns;
    session.thread_name = thread_name;
    session.spawned_worker_ids = spawned_worker_ids;
    session.total_tokens = total_tokens;
    session.is_ongoing = is_ongoing;

    visited.remove(&canonical_path);
    Ok(session)
}

fn embed_worker_sessions(
    parent_path: &Path,
    turns: &mut [CodexTurn],
    visited: &mut HashSet<PathBuf>,
) {
    for turn in turns {
        for tool in &mut turn.tool_calls {
            if tool.kind != ToolKind::SpawnAgent {
                continue;
            }

            let Some(spawn) = turn
                .collab_spawns
                .iter()
                .find(|spawn| spawn.call_id == tool.call_id)
            else {
                continue;
            };

            let Some(worker_path) = find_session_file_by_id(parent_path, &spawn.new_session_id)
            else {
                continue;
            };

            let canonical_worker_path =
                fs::canonicalize(&worker_path).unwrap_or_else(|_| worker_path.clone());
            if visited.contains(&canonical_worker_path) {
                continue;
            }

            if let Ok(worker_session) = parse_session_inner(&worker_path, visited) {
                tool.worker_session = Some(Box::new(worker_session));
            }
        }
    }
}

fn find_session_file_by_id(anchor_path: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }

    let dir = anchor_path.parent()?;
    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(session_id))
        })
        .collect();

    candidates.sort();
    candidates
        .iter()
        .find(|path| session_file_id(path).as_deref() == Some(session_id))
        .cloned()
        .or_else(|| candidates.into_iter().next())
}

fn session_file_id(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    BufReader::new(file).lines().take(20).find_map(|line| {
        let line = line.ok()?;
        let entry = RawEntry::parse(&line)?;
        match entry.entry_type.as_str() {
            "session_meta" => {
                let id = extract_session_id(&entry.payload);
                if id.is_empty() {
                    None
                } else {
                    Some(id)
                }
            }
            "session_meta_root" => entry
                .raw
                .get("id")
                .and_then(|id| id.as_str())
                .map(|id| id.to_string()),
            _ => None,
        }
    })
}

fn parse_session_meta_new(session: &mut CodexSession, payload: &Value, _raw: &Value) {
    session.id = extract_session_id(payload);
    session.timestamp = str_field(payload, "timestamp");
    session.cwd = opt_str(payload, "cwd");
    session.originator = opt_str(payload, "originator");
    session.cli_version = opt_str(payload, "cli_version");
    session.model_provider = opt_str(payload, "model_provider");
    // ai-title is an optional field added in Codex v0.128.0 for external agent sessions
    session.ai_title = payload
        .get("ai-title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // Codex v0.130.0 (PR #21424): `codex remote-control` starts headless app-server sessions.
    // Detected from originator == "remote-control" or source == "remote-control".
    session.is_headless = payload
        .get("originator")
        .and_then(|v| v.as_str())
        .map(|s| s == "remote-control")
        .unwrap_or(false)
        || payload
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s == "remote-control")
            .unwrap_or(false);

    if let Some(git) = payload.get("git") {
        session.git = Some(GitInfo {
            commit_hash: opt_str(git, "commit_hash"),
            branch: opt_str(git, "branch"),
            repository_url: opt_str(git, "repository_url"),
        });
    }

    // Instructions: prefer base_instructions.text, fall back to instructions (flat string)
    session.instructions = payload
        .get("base_instructions")
        .and_then(|bi| bi.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| opt_str(payload, "instructions"));
}

fn parse_session_meta_root(session: &mut CodexSession, raw: &Value) {
    session.id = str_field(raw, "id");
    session.timestamp = str_field(raw, "timestamp");
    // Oldest format: no cwd, originator, cli_version
    if let Some(git) = raw.get("git") {
        session.git = Some(GitInfo {
            commit_hash: opt_str(git, "commit_hash"),
            branch: opt_str(git, "branch"),
            repository_url: opt_str(git, "repository_url"),
        });
    }
    session.instructions = opt_str(raw, "instructions");
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Returns the default Codex sessions directory: ~/.codex/sessions
pub fn default_sessions_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}

/// Resolve the sessions directory from settings or default.
pub fn resolve_sessions_dir(configured: Option<&str>) -> Result<std::path::PathBuf, String> {
    if let Some(p) = configured.filter(|s| !s.is_empty()) {
        return Ok(std::path::PathBuf::from(p));
    }
    default_sessions_dir().ok_or_else(|| "cannot determine home directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn parse_session_reads_id_from_session_id_field() {
        // v0.129.0+ PR #20437: session_id field in session_meta payload
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-07T00-00-00-newsessid.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{"session_id":"new-sess-id","timestamp":"2026-05-07T00:00:00Z","cwd":"/tmp"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746576002.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "new-sess-id");
    }

    #[test]
    fn parse_session_reads_id_from_thread_session_id() {
        // v0.129.0+ PR #21336: sessionId moved onto Thread object
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-07T00-01-00-threadsessid.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-07T00:01:00Z","type":"session_meta","payload":{"thread":{"sessionId":"thread-sess-id"},"timestamp":"2026-05-07T00:01:00Z","cwd":"/tmp"}}"#,
                r#"{"timestamp":"2026-05-07T00:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-07T00:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746576062.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "thread-sess-id");
    }

    #[test]
    fn default_sessions_dir_exists() {
        let dir = default_sessions_dir();
        assert!(dir.is_some());
    }

    fn find_first_jsonl(dir: &PathBuf) -> Option<PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        let mut children: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        children.sort();
        for child in &children {
            if child.is_file() && child.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                return Some(child.clone());
            }
            if child.is_dir() {
                if let Some(found) = find_first_jsonl(child) {
                    return Some(found);
                }
            }
        }
        None
    }

    #[test]
    fn parse_real_session_does_not_panic() {
        let home = std::env::var("HOME").expect("HOME not set");
        let sessions_root = PathBuf::from(home).join(".codex/sessions");
        if !sessions_root.exists() {
            return;
        }
        let Some(path) = find_first_jsonl(&sessions_root) else {
            return;
        };
        let result = parse_session(&path);
        assert!(result.is_ok(), "parse_session failed: {:?}", result.err());
        let session = result.unwrap();
        assert!(!session.id.is_empty(), "session id should not be empty");
    }

    #[test]
    fn parse_session_collects_sdk_spawn_agent_output_workers() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-27T16-50-45-parent.jsonl");
        let worker_path = tmp
            .path()
            .join("rollout-2026-04-27T16-50-46-worker-session.jsonl");
        let nested_worker_path = tmp
            .path()
            .join("rollout-2026-04-27T16-50-47-nested-worker-session.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-27T04:50:45Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:50:45Z"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Parfit\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &worker_path,
            [
                r#"{"timestamp":"2026-04-27T04:50:46Z","type":"session_meta","payload":{"id":"worker-session","timestamp":"2026-04-27T04:50:46Z","cwd":"/tmp/worker"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:05Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-worker"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:06Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo worker\"}","call_id":"call_exec"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:07Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"worker output"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:08Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Go deeper\"}","call_id":"call_nested"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:09Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_nested","output":"{\"agent_id\":\"nested-worker-session\",\"nickname\":\"Nested\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-worker","completed_at":1777279930.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &nested_worker_path,
            [
                r#"{"timestamp":"2026-04-27T04:50:47Z","type":"session_meta","payload":{"id":"nested-worker-session","timestamp":"2026-04-27T04:50:47Z","cwd":"/tmp/nested"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:11Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-nested"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:12Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"echo nested\"}","call_id":"call_nested_exec"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:13Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_nested_exec","output":"nested output"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:14Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-nested","completed_at":1777279934.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();

        assert_eq!(session.spawned_worker_ids, vec!["worker-session"]);
        assert_eq!(
            session.turns[0].collab_spawns[0].new_session_id,
            "worker-session"
        );

        let worker_session = session.turns[0].tool_calls[0]
            .worker_session
            .as_ref()
            .expect("spawn_agent tool should embed worker session");
        assert_eq!(worker_session.id, "worker-session");
        assert_eq!(worker_session.turns[0].tool_calls[0].name, "exec_command");

        let nested_worker_session = worker_session.turns[0].tool_calls[1]
            .worker_session
            .as_ref()
            .expect("nested spawn_agent tool should embed nested worker session");
        assert_eq!(nested_worker_session.id, "nested-worker-session");
        assert_eq!(
            nested_worker_session.turns[0].tool_calls[0].output,
            Some("nested output".to_string())
        );
    }

    #[test]
    fn parse_session_reads_ai_title_from_session_meta() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-30T10-00-00-ext.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"ext-session","timestamp":"2026-04-30T10:00:00Z","ai-title":"Fix the login bug"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007202.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.ai_title.as_deref(), Some("Fix the login bug"));
        assert_eq!(session.id, "ext-session");
    }

    #[test]
    fn parse_session_end_marker_closes_session() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-30T10-01-00-ended.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:01:00Z","type":"session_meta","payload":{"id":"ended-session","timestamp":"2026-04-30T10:01:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007262.0}}"#,
                r#"{"timestamp":"2026-04-30T10:01:03Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert!(!session.is_ongoing);
    }

    #[test]
    fn parse_session_end_marker_overrides_ongoing_turn() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-04-30T10-02-00-endmarker.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:02:00Z","type":"session_meta","payload":{"id":"endmarker-session","timestamp":"2026-04-30T10:02:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:02Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        // session_end overrides the ongoing turn — session must not appear live
        assert!(!session.is_ongoing);
    }

    // Codex v0.130.0 (PR #21424): `codex remote-control` starts headless app-server sessions.

    #[test]
    fn parse_session_detects_headless_via_originator() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-08T10-00-00-headless.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"headless-session","timestamp":"2026-05-08T10:00:00Z","originator":"remote-control","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698402.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert!(
            session.is_headless,
            "originator:remote-control must set is_headless"
        );
        assert_eq!(session.id, "headless-session");
        assert_eq!(session.turns.len(), 1);
    }

    #[test]
    fn parse_session_detects_headless_via_source_string() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-08T10-01-00-headless2.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:01:00Z","type":"session_meta","payload":{"id":"headless-session-2","timestamp":"2026-05-08T10:01:00Z","source":"remote-control","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698462.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert!(
            session.is_headless,
            "source:remote-control must set is_headless"
        );
    }

    // Codex v0.133.0 (PRs #23300, #23685, #23696, #23732): Goals feature enabled by default.
    // Goal lifecycle events are interleaved in the session JSONL turn stream. Verify full
    // session parse handles them gracefully and produces the correct turn structure.

    #[test]
    fn parse_session_with_goal_events_produces_correct_turns() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-21T10-00-00-goals.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"goals-session","timestamp":"2026-05-21T10:00:00Z","cwd":"/tmp","cli_version":"0.133.0"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:02Z","type":"event_msg","payload":{"type":"goal_created","goal_id":"goal-abc","title":"Implement feature X","status":"active"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:03Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"goal-abc","progress":0.25}}"#,
                r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"goal_updated","goal_id":"goal-abc","progress":0.75}}"#,
                r#"{"timestamp":"2026-05-21T10:00:05Z","type":"event_msg","payload":{"type":"goal_completed","goal_id":"goal-abc","outcome":"success"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167206.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "goals-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.133.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn parse_session_regular_exec_session_is_not_headless() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-08T10-02-00-exec.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-08T10:02:00Z","type":"session_meta","payload":{"id":"exec-session","timestamp":"2026-05-08T10:02:00Z","source":"exec","cli_version":"0.130.0"}}"#,
                r#"{"timestamp":"2026-05-08T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-08T10:02:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746698522.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let session = parse_session(&path).unwrap();
        assert!(!session.is_headless, "exec session must not be headless");
    }

    #[test]
    fn parse_session_unknown_record_types_are_skipped() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-30T10-03-00-unknown.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-30T10:03:00Z","type":"session_meta","payload":{"id":"unknown-types-session","timestamp":"2026-04-30T10:03:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:03:01Z","type":"future_record_type_v999","payload":{"data":"some future data"}}"#,
                r#"{"timestamp":"2026-04-30T10:03:02Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:03:03Z","type":"another_unknown_type","payload":{}}"#,
                r#"{"timestamp":"2026-04-30T10:03:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007384.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "unknown-types-session");
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.131.0 (PRs #22594, #22647, #22724): profile-v2 layered config format.
    //
    // codex-trace reads JSONL session files, not Codex CLI TOML config files. The profile-v2
    // changes alter what Codex writes into session_meta: a `profile` field may appear naming
    // the active profile, and instructions now come via `base_instructions.text` from the
    // profile's system_prompt (the `instructions_file` config key is gone from Codex config).
    // All cases below must parse without panics and produce correct field values.
    //
    // Note: As of Codex v0.134.0 (PRs #23883, #24051, #24055, #24059), --profile-v2 was
    // renamed to --profile and all legacy profile v1 support was removed. See the v0134_*
    // tests below for the corresponding v0.134.0 verification.

    #[test]
    fn v0131_profile_v2_session_parses_correctly() {
        // session_meta from v0.131.0 with --profile-v2 active (renamed to --profile in
        // v0.134.0): carries `profile` field and instructions sourced from the profile's
        // system_prompt via base_instructions.text.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-18T10-00-00-profilev2.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:00:00Z","type":"session_meta","payload":{"id":"v0131-profile-v2","timestamp":"2026-05-18T10:00:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai","profile":"work","base_instructions":{"text":"You are a helpful assistant."}}}"#,
                r#"{"timestamp":"2026-05-18T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562402.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0131-profile-v2");
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        // Instructions arrive from base_instructions.text (profile system_prompt).
        assert_eq!(
            session.instructions.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0131_session_without_instructions_file_parses_correctly() {
        // v0.131.0 removed `instructions_file` from the Codex config (PR #22724). Sessions
        // started without a profile providing instructions will have no `instructions` or
        // `base_instructions` in session_meta. The parser must return None, not panic.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-18T10-01-00-noinstr.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:01:00Z","type":"session_meta","payload":{"id":"v0131-no-instructions","timestamp":"2026-05-18T10:01:00Z","cwd":"/home/user","cli_version":"0.131.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-18T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562462.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0131-no-instructions");
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        // instructions_file is gone — no instructions in this session.
        assert!(session.instructions.is_none());
        assert_eq!(session.turns.len(), 1);
    }

    #[test]
    fn v0131_legacy_profiles_section_absent_does_not_affect_session_parsing() {
        // PR #22647: Codex now rejects legacy [profiles] TOML when profile-v2 is active.
        // codex-trace reads only JSONL session files — it never touches Codex TOML config.
        // This test confirms standard v0.131.0 session files parse correctly regardless of
        // which config format the CLI was configured with.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-18T10-02-00-v0131.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-18T10:02:00Z","type":"session_meta","payload":{"id":"v0131-standard","timestamp":"2026-05-18T10:02:00Z","cwd":"/workspace","cli_version":"0.131.0","model_provider":"openai","profile":"default"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/workspace"}}"#,
                r#"{"timestamp":"2026-05-18T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562524.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0131-standard");
        assert_eq!(session.cli_version.as_deref(), Some("0.131.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.131.0 (PR #22268): collab_agent_spawn_end uses new_session_id instead of
    // new_thread_id. Verify end-to-end: parse_session reads new_session_id, populates
    // spawned_worker_ids, and embed_worker_sessions correctly stitches the worker session.
    #[test]
    fn v0131_parse_session_stitches_worker_via_new_session_id() {
        let tmp = tempdir().unwrap();
        let parent_path = tmp
            .path()
            .join("rollout-2026-05-18T10-03-00-parent-v131.jsonl");
        let worker_path = tmp
            .path()
            .join("rollout-2026-05-18T10-03-09-worker-v131.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-05-18T10:03:00Z","type":"session_meta","payload":{"id":"parent-v131","timestamp":"2026-05-18T10:03:00Z","cli_version":"0.131.0"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Gather data\"}","call_id":"call-spawn-v131"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call-spawn-v131","sender_session_id":"parent-v131","new_session_id":"worker-v131","new_agent_nickname":"Hypatia","new_agent_role":"worker","prompt":"Gather data","status":"pending_init"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-spawn-v131","output":"{\"agent_id\":\"worker-v131\",\"nickname\":\"Hypatia\"}"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1747562585.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &worker_path,
            [
                r#"{"timestamp":"2026-05-18T10:03:09Z","type":"session_meta","payload":{"id":"worker-v131","timestamp":"2026-05-18T10:03:09Z","cli_version":"0.131.0","cwd":"/tmp/worker"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:10Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-worker"}}"#,
                r#"{"timestamp":"2026-05-18T10:03:11Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-worker","completed_at":1747562591.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&parent_path).unwrap();
        assert_eq!(session.id, "parent-v131");
        assert_eq!(session.spawned_worker_ids, vec!["worker-v131"]);
        assert_eq!(
            session.turns[0].collab_spawns[0].new_session_id,
            "worker-v131"
        );
        assert_eq!(session.turns[0].collab_spawns[0].agent_nickname, "Hypatia");
        let worker = session.turns[0].tool_calls[0]
            .worker_session
            .as_ref()
            .expect("spawn_agent tool call should embed worker session");
        assert_eq!(worker.id, "worker-v131");
    }

    // Codex v0.134.0 (PRs #23883, #24051, #24055, #24059): --profile-v2 renamed to --profile;
    // legacy profile v1 support removed entirely.
    //
    // codex-trace reads JSONL session files only — it never invokes `codex` or reads Codex
    // TOML config. Sessions from v0.134.0+ carry the same `profile` field in session_meta
    // as v0.131.0+ sessions. The parser is unaffected; these tests confirm v0.134.0
    // sessions parse correctly and produce the expected field values.

    #[test]
    fn v0134_profile_session_parses_correctly() {
        // session_meta from v0.134.0 with --profile active (flag renamed from --profile-v2).
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-26T10-00-00-profile.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:00:00Z","type":"session_meta","payload":{"id":"v0134-profile","timestamp":"2026-05-26T10:00:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai","profile":"work","base_instructions":{"text":"You are a helpful assistant."}}}"#,
                r#"{"timestamp":"2026-05-26T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254802.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-profile");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(
            session.instructions.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0134_session_without_profile_parses_correctly() {
        // v0.134.0 session started without --profile: no `profile` field in session_meta.
        // parse_session must return None for instructions, not panic.
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-26T10-01-00-noprofile.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:01:00Z","type":"session_meta","payload":{"id":"v0134-no-profile","timestamp":"2026-05-26T10:01:00Z","cwd":"/home/user","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254862.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-no-profile");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert!(session.instructions.is_none());
        assert_eq!(session.turns.len(), 1);
    }

    #[test]
    fn v0134_legacy_profile_v1_absent_does_not_affect_session_parsing() {
        // v0.134.0 removed legacy profile v1 support entirely. Since codex-trace reads only
        // JSONL session files (never Codex TOML config), the removal has no effect on
        // parsing. Standard v0.134.0 sessions must parse correctly regardless.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-26T10-02-00-v0134.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-26T10:02:00Z","type":"session_meta","payload":{"id":"v0134-standard","timestamp":"2026-05-26T10:02:00Z","cwd":"/workspace","cli_version":"0.134.0","model_provider":"openai","profile":"default"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:03Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/workspace"}}"#,
                r#"{"timestamp":"2026-05-26T10:02:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748254924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-standard");
        assert_eq!(session.cli_version.as_deref(), Some("0.134.0"));
        assert_eq!(session.turns.len(), 1);
        assert!(!session.is_ongoing);
    }

    // Codex v0.133.0 (PR #22709): TurnContextItem fields trimmed.
    // turn_context payloads now carry only the model field; cwd and effort are no longer
    // emitted. Sessions from v0.133.0+ must parse correctly with the reduced payload.

    #[test]
    fn v0133_turn_context_trimmed_fields_session_parses_correctly() {
        // v0.133.0 session where turn_context has only model — cwd and effort are absent.
        // Verifies the parser extracts model from the trimmed payload and does not panic
        // on the missing fields.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-21T10-00-00-v0133.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-21T10:00:00Z","type":"session_meta","payload":{"id":"v0133-turn-ctx","timestamp":"2026-05-21T10:00:00Z","cwd":"/workspace","cli_version":"0.133.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Done"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5"}}"#,
                r#"{"timestamp":"2026-05-21T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748167204.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0133-turn-ctx");
        assert_eq!(session.cli_version.as_deref(), Some("0.133.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].model.as_deref(), Some("gpt-5"));
        // cwd and effort absent in turn_context — must not panic
        assert!(session.turns[0].reasoning_effort.is_none());
        assert!(!session.is_ongoing);
    }

    // Codex v0.135.0 (PR #24591): memory state moved from file-based storage to a dedicated
    // SQLite DB. Active memories are injected into context at turn start and written into the
    // turn_context JSONL event. parse_session must expose them on each CodexTurn.

    #[test]
    fn v0135_session_with_memories_in_turn_context() {
        let tmp = tempdir().unwrap();
        let path = tmp
            .path()
            .join("rollout-2026-05-28T10-00-00-v0135mem.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-28T10:00:00Z","type":"session_meta","payload":{"id":"v0135-mem-session","timestamp":"2026-05-28T10:00:00Z","cwd":"/project","cli_version":"0.135.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-28T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-28T10:00:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","memories":["User prefers terse output","Project uses TypeScript strict mode"]}}"#,
                r#"{"timestamp":"2026-05-28T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
                r#"{"timestamp":"2026-05-28T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426404.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0135-mem-session");
        assert_eq!(session.cli_version.as_deref(), Some("0.135.0"));
        assert_eq!(session.turns.len(), 1);
        assert_eq!(
            session.turns[0].memories,
            vec![
                "User prefers terse output",
                "Project uses TypeScript strict mode"
            ]
        );
        assert!(!session.is_ongoing);
    }

    #[test]
    fn v0135_session_without_memories_produces_empty_vec() {
        // Pre-v0.135.0 sessions must parse normally with an empty memories Vec.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-05-28T10-01-00-nomem.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-28T10:01:00Z","type":"session_meta","payload":{"id":"v0134-no-memories","timestamp":"2026-05-28T10:01:00Z","cwd":"/project","cli_version":"0.134.0","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-05-28T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-28T10:01:02Z","type":"turn_context","payload":{"model":"gpt-5","cwd":"/project","effort":"high"}}"#,
                r#"{"timestamp":"2026-05-28T10:01:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1748426463.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        assert_eq!(session.id, "v0134-no-memories");
        assert!(session.turns[0].memories.is_empty());
    }
}
