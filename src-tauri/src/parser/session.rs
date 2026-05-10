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
        .flat_map(|t| t.collab_spawns.iter().map(|s| s.new_thread_id.clone()))
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

            let Some(worker_path) = find_session_file_by_id(parent_path, &spawn.new_thread_id)
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
            session.turns[0].collab_spawns[0].new_thread_id,
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
}
