use serde_json::Value;

/// A raw JSONL line from a Codex session file, loosely typed.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub entry_type: String,
    pub timestamp: Option<String>,
    pub payload: Value,
    /// The raw line value (useful for oldest-format session_meta where fields are at root)
    pub raw: Value,
}

impl RawEntry {
    /// Parse a single JSONL line into a RawEntry.
    pub fn parse(line: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(line).ok()?;

        // Skip "state" placeholder entries
        if v.get("record_type").and_then(|t| t.as_str()) == Some("state") {
            return None;
        }

        // Skip non-full view mode entries (Codex v0.130.0+, PR #21566).
        // The thread turns endpoint now exposes three view modes: "unloaded"
        // (metadata-only stub), "summary" (partial), and "full" (complete).
        // Only absent (legacy) or "full" entries carry complete turn data;
        // any other view_mode is a placeholder and must be skipped so callers
        // never receive silently truncated turn content.
        if let Some(vm) = v.get("view_mode").and_then(|t| t.as_str()) {
            if vm != "full" {
                return None;
            }
        }

        let entry_type = detect_entry_type(&v);
        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);

        Some(RawEntry {
            entry_type,
            timestamp,
            payload,
            raw: v,
        })
    }
}

fn detect_entry_type(v: &Value) -> String {
    // Check explicit type field first
    if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
        return t.to_string();
    }

    // Mid format: has payload but no type
    if v.get("payload").is_some() {
        return "session_meta".to_string();
    }

    // Oldest format: has id + timestamp at root
    if v.get("id").is_some() && v.get("timestamp").is_some() {
        return "session_meta_root".to_string();
    }

    // Bare old-format entries (cli_version < 0.44): function_call, function_call_output, message, reasoning
    if v.get("call_id").is_some() && v.get("arguments").is_some() && v.get("name").is_some() {
        return "function_call".to_string();
    }
    if v.get("call_id").is_some() && v.get("output").is_some() {
        return "function_call_output".to_string();
    }
    if v.get("role").is_some() && v.get("content").is_some() {
        return "message".to_string();
    }
    if v.get("encrypted_content").is_some() {
        return "reasoning".to_string();
    }

    "unknown".to_string()
}

/// Extract the event_msg payload type (e.g. "task_started", "user_message", etc.)
pub fn event_msg_type(payload: &Value) -> Option<&str> {
    payload.get("type").and_then(|t| t.as_str())
}

/// Parse an ISO timestamp string to Unix seconds (u64).
pub fn parse_timestamp_secs(ts: &str) -> Option<u64> {
    use chrono::DateTime;
    let dt = ts.parse::<DateTime<chrono::Utc>>().ok()?;
    Some(dt.timestamp() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_new_session_meta() {
        let line = r#"{"timestamp":"2026-04-25T10:00:00Z","type":"session_meta","payload":{"id":"abc","cwd":"/tmp"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["id"], "abc");
    }

    #[test]
    fn parse_state_placeholder_returns_none() {
        let line = r#"{"record_type":"state"}"#;
        assert!(RawEntry::parse(line).is_none());
    }

    #[test]
    fn parse_event_msg() {
        let line = r#"{"timestamp":"2026-04-25T10:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "event_msg");
        assert_eq!(event_msg_type(&e.payload), Some("user_message"));
    }

    #[test]
    fn parse_timestamp() {
        assert!(parse_timestamp_secs("2026-04-25T10:00:00Z").is_some());
    }

    fn parse_response_item() {
        let line = r#"{"timestamp":"2026-04-25T10:00:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call_1"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["type"], "function_call");
    }

    // `response_item` is a JSONL log entry type written by the Codex CLI into session
    // files. It is entirely unrelated to the `codex responses` CLI subcommand that was
    // removed in Codex v0.128.0 (PR #19640). This test guards against that confusion
    // and ensures all expected response_item payload types continue to parse correctly.
    #[test]
    fn response_item_payload_types_parsed_from_jsonl_not_cli_subcommand() {
        let cases = [
            (
                r#"{"timestamp":"2026-04-25T10:00:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#,
                "function_call",
            ),
            (
                r#"{"timestamp":"2026-04-25T10:00:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#,
                "function_call_output",
            ),
            (
                r#"{"timestamp":"2026-04-25T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"hello"}}"#,
                "message",
            ),
            (
                r#"{"timestamp":"2026-04-25T10:00:03Z","type":"response_item","payload":{"type":"reasoning","encrypted_content":"..."}}"#,
                "reasoning",
            ),
        ];
        for (line, expected_payload_type) in cases {
            let e = RawEntry::parse(line).unwrap();
            assert_eq!(e.entry_type, "response_item");
            assert_eq!(e.payload["type"], expected_payload_type);
        }
    }

    /// Codex CLI flags boundary: codex-trace never invokes `codex` at runtime.
    #[test]
    fn codex_cli_flags_read_as_jsonl_data_not_invoked() {
        let line = r#"{"timestamp":"2026-04-30T12:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/home/user","permission_profile":"full-auto"}}"#;
        let e = RawEntry::parse(line).unwrap();
        assert_eq!(e.entry_type, "session_meta");
        assert_eq!(e.payload["permission_profile"], "full-auto");
    }

    // Codex v0.130.0 (PR #21566): thread turns endpoint now exposes three view
    // modes. "unloaded" and "summary" entries are placeholders / partial stubs;
    // only absent (legacy) or "full" entries contain complete turn data.

    #[test]
    fn view_mode_unloaded_returns_none() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","view_mode":"unloaded","payload":{"type":"function_call","name":"exec_command","call_id":"c1"}}"#;
        assert!(RawEntry::parse(line).is_none());
    }

    #[test]
    fn view_mode_summary_returns_none() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","view_mode":"summary","payload":{"type":"message","role":"assistant","content":"partial"}}"#;
        assert!(RawEntry::parse(line).is_none());
    }

    #[test]
    fn view_mode_full_is_parsed_normally() {
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","view_mode":"full","payload":{"type":"function_call","name":"exec_command","call_id":"c2"}}"#;
        let e = RawEntry::parse(line).expect("view_mode:full must parse");
        assert_eq!(e.entry_type, "response_item");
        assert_eq!(e.payload["name"], "exec_command");
    }

    #[test]
    fn absent_view_mode_is_parsed_normally() {
        // Legacy entries (pre-v0.130.0) have no view_mode field; they must still parse.
        let line = r#"{"timestamp":"2026-05-08T10:00:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"hello"}}"#;
        let e = RawEntry::parse(line).expect("legacy entry without view_mode must parse");
        assert_eq!(e.entry_type, "response_item");
    }

    #[test]
    fn log_db_log_writer_refactor_does_not_affect_jsonl_session_parser() {
        // Codex v0.128.0 PRs #19234/#19959 refactored the internal log DB into a
        // LogWriter interface and fixed its batch flush timing. That subsystem is a
        // SQLite-backed telemetry store — entirely separate from the JSONL session
        // files at ~/.codex/sessions/ that codex-trace reads. Verify all four
        // standard entry types produced by a v0.128.0 session parse correctly.
        let lines = [
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"v0128-session","timestamp":"2026-04-30T10:00:00Z","cwd":"/tmp","cli_version":"0.128.0","model_provider":"openai"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"Hello"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"turn_context","payload":{"model":"gpt-5.4","cwd":"/tmp"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007204.0}}"#,
        ];
        let expected_types = [
            "session_meta",
            "event_msg",
            "response_item",
            "turn_context",
            "event_msg",
        ];
        for (line, expected) in lines.iter().zip(expected_types.iter()) {
            let entry = RawEntry::parse(line).expect("parse failed");
            assert_eq!(entry.entry_type, *expected, "wrong type for: {line}");
        }
        let meta = RawEntry::parse(lines[0]).unwrap();
        assert_eq!(meta.payload["cli_version"], "0.128.0");
    }
}
