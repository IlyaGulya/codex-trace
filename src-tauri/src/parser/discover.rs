use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use super::entry::{extract_session_id, RawEntry};
use super::spawn::parse_spawn_agent_output;

/// Lightweight session info for the picker list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexSessionInfo {
    pub id: String,
    pub path: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub originator: Option<String>,
    pub model: Option<String>,
    pub cli_version: Option<String>,
    pub thread_name: Option<String>,
    pub turn_count: u32,
    pub start_time: String,
    pub end_time: Option<String>,
    pub total_tokens: Option<u64>,
    pub is_ongoing: bool,
    /// true when session_meta.source.subagent is set (system-spawned: review, memory_consolidation)
    pub is_external_worker: bool,
    /// true when this session's id appears in another session's spawned_worker_ids (inline collab worker)
    pub is_inline_worker: bool,
    pub worker_nickname: Option<String>,
    pub worker_role: Option<String>,
    pub spawned_worker_ids: Vec<String>,
    /// "YYYY/MM/DD" derived from the file path
    pub date_group: String,
    /// Optional AI-generated title from external agent sessions (Codex v0.128.0+)
    pub ai_title: Option<String>,
}

/// Scan a sessions directory recursively for all rollout-*.jsonl files.
/// Returns CodexSessionInfo sorted by filename descending (newest first).
pub fn discover_sessions(sessions_dir: &Path) -> Result<Vec<CodexSessionInfo>, String> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut infos: Vec<CodexSessionInfo> = Vec::new();
    collect_jsonl_files(sessions_dir, &mut infos)?;

    // Sort newest first (ISO timestamp in filename is lexicographically sortable)
    infos.sort_by(|a, b| {
        let fa = Path::new(&a.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let fb = Path::new(&b.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        fb.cmp(fa)
    });

    // Second pass: mark inline workers — sessions whose id appears in any parent's spawned_worker_ids.
    use std::collections::HashSet;
    let inline_worker_ids: HashSet<String> = infos
        .iter()
        .flat_map(|s| s.spawned_worker_ids.iter().cloned())
        .collect();
    for info in &mut infos {
        if inline_worker_ids.contains(&info.id) {
            info.is_inline_worker = true;
        }
    }

    Ok(infos)
}

fn collect_jsonl_files(dir: &Path, infos: &mut Vec<CodexSessionInfo>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, infos)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("rollout-") {
                if let Some(info) = scan_session_file(&path) {
                    infos.push(info);
                }
            }
        }
    }

    Ok(())
}

/// Extract date group (YYYY/MM/DD) from the file path.
/// Path structure: .../sessions/YYYY/MM/DD/rollout-*.jsonl
fn date_group_from_path(path: &Path) -> String {
    path.parent()
        .and_then(|p| {
            let dd = p.file_name()?.to_str()?;
            let mm = p.parent()?.file_name()?.to_str()?;
            let yyyy = p.parent()?.parent()?.file_name()?.to_str()?;
            Some(format!("{yyyy}/{mm}/{dd}"))
        })
        .unwrap_or_default()
}

/// Quickly scan a JSONL file for session metadata without full parsing.
fn scan_session_file(path: &Path) -> Option<CodexSessionInfo> {
    let content = fs::read_to_string(path).ok()?;
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());

    let first_line = lines.next()?;
    let first: Value = serde_json::from_str(first_line).ok()?;

    // Skip state placeholders
    if first.get("record_type").and_then(|t| t.as_str()) == Some("state") {
        return None;
    }

    let entry = RawEntry::parse(first_line)?;
    let payload = &entry.payload;
    let raw = &entry.raw;

    let (
        id,
        start_time,
        cwd,
        originator,
        cli_version,
        git_branch,
        _instructions,
        is_external_worker,
        worker_nickname,
        worker_role,
        ai_title,
    ) = match entry.entry_type.as_str() {
        "session_meta" => {
            let id = extract_session_id(payload);
            let start_time = str_field(payload, "timestamp");
            let cwd = opt_str(payload, "cwd");
            let originator = opt_str(payload, "originator");
            let cli_version = opt_str(payload, "cli_version");
            let git_branch = payload
                .get("git")
                .and_then(|g| g.get("branch"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let instructions: Option<String> = None; // not needed for picker
                                                     // source.subagent present ⟹ this is a system-spawned external worker
            let is_external_worker = payload
                .get("source")
                .and_then(|s| s.get("subagent"))
                .is_some();
            let (worker_nickname, worker_role) = worker_metadata(payload);
            let ai_title = payload
                .get("ai-title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            (
                id,
                start_time,
                cwd,
                originator,
                cli_version,
                git_branch,
                instructions,
                is_external_worker,
                worker_nickname,
                worker_role,
                ai_title,
            )
        }
        "session_meta_root" => {
            let id = str_field(raw, "id");
            let start_time = str_field(raw, "timestamp");
            let git_branch = raw
                .get("git")
                .and_then(|g| g.get("branch"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (
                id, start_time, None, None, None, git_branch, None, false, None, None, None,
            )
        }
        _ => return None,
    };

    if id.is_empty() {
        return None;
    }

    // Quick scan remaining lines for turn count, model, thread_name, tokens, end_time
    let mut turn_count: u32 = 0;
    let mut model: Option<String> = None;
    let mut thread_name: Option<String> = None;
    let mut total_tokens: Option<u64> = None;
    let mut end_time: Option<String> = None;
    let mut spawned_worker_ids: Vec<String> = Vec::new();
    let mut pending_spawn_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut is_ongoing = true;
    let mut has_session_end = false;

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let t = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match t {
            "session_end" => {
                has_session_end = true;
                is_ongoing = false;
                if end_time.is_none() {
                    end_time = v
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
            "event_msg" => {
                let pt = v
                    .get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match pt {
                    "task_started" => {
                        turn_count += 1;
                        if !has_session_end {
                            is_ongoing = true;
                        }
                        end_time = None;
                    }
                    "user_message" if turn_count == 0 => {
                        turn_count += 1;
                        if !has_session_end {
                            is_ongoing = true;
                        }
                        end_time = None;
                    }
                    "task_complete" => {
                        is_ongoing = false;
                        let payload = v.get("payload").unwrap_or(&Value::Null);
                        end_time = payload
                            .get("completed_at")
                            .and_then(|v| v.as_f64())
                            .map(|ts| {
                                use chrono::{DateTime, Utc};
                                DateTime::<Utc>::from_timestamp(ts as i64, 0)
                                    .map(|dt| dt.to_rfc3339())
                                    .unwrap_or_default()
                            })
                            .or_else(|| {
                                v.get("timestamp")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                            });
                        // Codex v0.128.0: task_complete may carry prompt_tokens/completion_tokens/total_tokens.
                        // Use as fallback when no token_count event was seen.
                        if total_tokens.is_none() {
                            total_tokens = payload
                                .get("total_tokens")
                                .and_then(|v| v.as_u64())
                                .or_else(|| {
                                    let p =
                                        payload.get("prompt_tokens").and_then(|v| v.as_u64())?;
                                    let c = payload
                                        .get("completion_tokens")
                                        .and_then(|v| v.as_u64())?;
                                    Some(p + c)
                                });
                        }
                    }
                    "turn_aborted" => {
                        is_ongoing = false;
                        end_time = v
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    "token_count" => {
                        if let Some(info) = v
                            .get("payload")
                            .and_then(|p| p.get("info"))
                            .filter(|v| !v.is_null())
                        {
                            if let Some(ttu) = info.get("total_token_usage") {
                                total_tokens = ttu.get("total_tokens").and_then(|v| v.as_u64());
                            }
                        }
                    }
                    "thread_name_updated" => {
                        thread_name = v
                            .get("payload")
                            .and_then(|p| p.get("thread_name"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                    }
                    "collab_agent_spawn_end" => {
                        if let Some(new_id) = v
                            .get("payload")
                            .and_then(|p| p.get("new_thread_id"))
                            .and_then(|v| v.as_str())
                        {
                            push_unique(&mut spawned_worker_ids, new_id.to_string());
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                let payload = v.get("payload").unwrap_or(&Value::Null);
                match payload.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "function_call"
                        if payload.get("name").and_then(|v| v.as_str()) == Some("spawn_agent") =>
                    {
                        if let Some(call_id) = payload.get("call_id").and_then(|v| v.as_str()) {
                            pending_spawn_call_ids.insert(call_id.to_string());
                        }
                    }
                    "function_call_output" => {
                        let Some(call_id) = payload.get("call_id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if !pending_spawn_call_ids.contains(call_id) {
                            continue;
                        }
                        if let Some(output) = output_text(payload) {
                            if let Some(spawn) = parse_spawn_agent_output(&output) {
                                push_unique(&mut spawned_worker_ids, spawn.agent_id);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "turn_context" => {
                // Always overwrite — spec says "most recent turn_context.payload.model"
                let m = v
                    .get("payload")
                    .and_then(|p| p.get("model"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if m.is_some() {
                    model = m;
                }
            }
            _ => {}
        }
    }

    // Also count user_message events for old format (no task_started)
    // We already handle that above by incrementing on first "user_message"
    // For accuracy, re-scan and count task_started events specifically
    // (already done in the loop above — task_started increments turn_count)

    // Sessions with no turns have no active task — not ongoing regardless of event stream.
    if turn_count == 0 {
        is_ongoing = false;
    }

    // Validate with file mtime: sessions last modified more than 60 seconds ago
    // cannot be actively processing a turn, regardless of missing task_complete events.
    // Many older CLI versions didn't emit task_complete, causing false positives otherwise.
    if is_ongoing && !has_session_end {
        const ONGOING_THRESHOLD_SECS: u64 = 60;
        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                    if elapsed.as_secs() > ONGOING_THRESHOLD_SECS {
                        is_ongoing = false;
                    }
                }
            }
        }
    }

    let date_group = date_group_from_path(path);

    Some(CodexSessionInfo {
        id,
        path: path.to_string_lossy().to_string(),
        cwd,
        git_branch,
        originator,
        model,
        cli_version,
        thread_name,
        turn_count,
        start_time,
        end_time,
        total_tokens,
        is_ongoing,
        is_external_worker,
        is_inline_worker: false, // set by discover_sessions second pass
        worker_nickname,
        worker_role,
        spawned_worker_ids,
        date_group,
        ai_title,
    })
}

fn worker_metadata(payload: &Value) -> (Option<String>, Option<String>) {
    let thread_spawn = payload
        .get("source")
        .and_then(|source| source.get("subagent"))
        .and_then(|subagent| subagent.get("thread_spawn"));

    let nickname = thread_spawn
        .and_then(|spawn| opt_str(spawn, "agent_nickname").or_else(|| opt_str(spawn, "nickname")));
    let role = thread_spawn
        .and_then(|spawn| opt_str(spawn, "agent_role").or_else(|| opt_str(spawn, "agent_type")));

    (nickname, role)
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

fn output_text(payload: &Value) -> Option<String> {
    match payload.get("output") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(arr)) => Some(
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn discover_sessions_reads_id_from_session_id_field() {
        // v0.129.0+ PR #20437: session_id field in session_meta payload
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/07");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-07T00-00-00-newsessid.jsonl");
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
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "new-sess-id").unwrap();
        assert_eq!(session.id, "new-sess-id");
    }

    #[test]
    fn discover_sessions_reads_id_from_thread_session_id() {
        // v0.129.0+ PR #21336: sessionId moved onto Thread object
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/05/07");
        std::fs::create_dir_all(&day_dir).unwrap();
        let path = day_dir.join("rollout-2026-05-07T00-01-00-threadsessid.jsonl");
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
        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "thread-sess-id").unwrap();
        assert_eq!(session.id, "thread-sess-id");
    }

    #[test]
    fn date_group_from_path_test() {
        let path = PathBuf::from("/home/user/.codex/sessions/2026/04/25/rollout-abc.jsonl");
        let dg = date_group_from_path(&path);
        assert_eq!(dg, "2026/04/25");
    }

    #[test]
    fn discover_sessions_links_sdk_spawn_agent_output() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/27");
        std::fs::create_dir_all(&day_dir).unwrap();

        let parent_path = day_dir.join("rollout-2026-04-27T16-50-45-019dcd46-parent.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-04-27T04:50:45Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:50:45Z","source":"exec"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker\",\"nickname\":\"Parfit\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let child_path = day_dir.join("rollout-2026-04-27T16-52-43-worker.jsonl");
        std::fs::write(
            &child_path,
            r#"{"timestamp":"2026-04-27T04:52:43Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:52:43Z","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1,"agent_nickname":"Parfit","agent_role":"worker"}}}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let parent = sessions.iter().find(|s| s.id == "parent").unwrap();
        let child = sessions.iter().find(|s| s.id == "worker").unwrap();

        assert_eq!(parent.spawned_worker_ids, vec!["worker"]);
        assert!(child.is_external_worker);
        assert!(child.is_inline_worker);
        assert_eq!(child.worker_nickname.as_deref(), Some("Parfit"));
        assert_eq!(child.worker_role.as_deref(), Some("worker"));
    }

    #[test]
    fn discover_sessions_links_collab_spawn_end_event() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/16");
        std::fs::create_dir_all(&day_dir).unwrap();

        let parent_path = day_dir.join("rollout-2026-04-16T11-38-08-019d9382-parent.jsonl");
        std::fs::write(
            &parent_path,
            [
                r#"{"timestamp":"2026-04-16T11:38:08Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-16T11:38:08Z","source":"cli"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect graph\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call_spawn","sender_thread_id":"parent","new_thread_id":"worker","new_agent_nickname":"Noether","new_agent_role":"worker","prompt":"Collect graph","status":"pending_init"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker\",\"nickname\":\"Noether\"}"}}"#,
                r#"{"timestamp":"2026-04-16T11:48:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1776335285.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let child_path = day_dir.join("rollout-2026-04-16T11-48-09-worker.jsonl");
        std::fs::write(
            &child_path,
            r#"{"timestamp":"2026-04-16T11:48:09Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-16T11:48:09Z","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1,"agent_nickname":"Noether","agent_role":"worker"}}}}}"#,
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let parent = sessions.iter().find(|s| s.id == "parent").unwrap();
        let child = sessions.iter().find(|s| s.id == "worker").unwrap();

        assert_eq!(parent.spawned_worker_ids, vec!["worker"]);
        assert!(child.is_external_worker);
        assert!(child.is_inline_worker);
        assert_eq!(child.worker_nickname.as_deref(), Some("Noether"));
        assert_eq!(child.worker_role.as_deref(), Some("worker"));
    }

    #[test]
    fn discover_sessions_marks_later_started_turn_ongoing_after_completed_turn() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/27");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-27T17-10-00-active.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-27T05:10:00Z","type":"session_meta","payload":{"id":"active","timestamp":"2026-04-27T05:10:00Z","source":"cli"}}"#,
                r#"{"timestamp":"2026-04-27T05:10:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T05:10:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777281002.0}}"#,
                r#"{"timestamp":"2026-04-27T05:10:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "active").unwrap();

        assert_eq!(session.turn_count, 2);
        assert!(session.is_ongoing);
        assert_eq!(session.end_time, None);
    }

    #[test]
    fn discover_sessions_marks_completed_latest_turn_not_ongoing() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/27");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-27T17-20-00-complete.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-27T05:20:00Z","type":"session_meta","payload":{"id":"complete","timestamp":"2026-04-27T05:20:00Z","source":"cli"}}"#,
                r#"{"timestamp":"2026-04-27T05:20:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T05:20:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777281602.0}}"#,
                r#"{"timestamp":"2026-04-27T05:20:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
                r#"{"timestamp":"2026-04-27T05:20:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1777281604.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "complete").unwrap();

        assert_eq!(session.turn_count, 2);
        assert!(!session.is_ongoing);
        assert!(session.end_time.is_some());
    }

    #[test]
    fn discover_sessions_reads_ai_title() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-30T10-00-00-aititle.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"aititle-session","timestamp":"2026-04-30T10:00:00Z","ai-title":"Refactor the auth module"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007202.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "aititle-session").unwrap();
        assert_eq!(
            session.ai_title.as_deref(),
            Some("Refactor the auth module")
        );
    }

    #[test]
    fn discover_sessions_session_end_marks_not_ongoing() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-30T10-01-00-ended.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:01:00Z","type":"session_meta","payload":{"id":"ended-session","timestamp":"2026-04-30T10:01:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:01:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007262.0}}"#,
                r#"{"timestamp":"2026-04-30T10:01:03Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "ended-session").unwrap();
        assert!(!session.is_ongoing);
    }

    #[test]
    fn discover_sessions_session_end_overrides_ongoing_turn() {
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();

        let session_path = day_dir.join("rollout-2026-04-30T10-02-00-endmarker.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:02:00Z","type":"session_meta","payload":{"id":"endmarker-session","timestamp":"2026-04-30T10:02:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:02:02Z","type":"session_end","payload":{}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.id == "endmarker-session")
            .unwrap();
        assert!(!session.is_ongoing);
    }

    #[test]
    fn reads_total_tokens_from_task_complete_v0128() {
        // Codex v0.128.0 adds prompt_tokens/completion_tokens/total_tokens to task_complete.
        // The discover scanner should use these as a fallback when no token_count event exists.
        let tmp = tempdir().unwrap();
        let day_dir = tmp.path().join("2026/04/30");
        std::fs::create_dir_all(&day_dir).unwrap();
        let session_path = day_dir.join("rollout-2026-04-30T10-00-00-s1.jsonl");
        std::fs::write(
            &session_path,
            [
                r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-30T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007210.0,"prompt_tokens":1500,"completion_tokens":300,"total_tokens":1800}}"#,
            ]
            .join("\n"),
        )
        .unwrap();

        let sessions = discover_sessions(tmp.path()).unwrap();
        let session = sessions.iter().find(|s| s.id == "s1").unwrap();

        assert_eq!(session.total_tokens, Some(1800));
    }
}
