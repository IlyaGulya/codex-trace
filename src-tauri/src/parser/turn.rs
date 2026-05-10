use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::entry::{parse_timestamp_secs, RawEntry};
use super::spawn::parse_spawn_agent_output;
use super::toolcall::{ToolCall, ToolCallBuilder};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Complete,
    Aborted,
    Cancelled,
    Ongoing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMsg {
    pub text: String,
    pub phase: Option<String>,
    pub timestamp: String,
    pub is_reasoning: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    pub context_window_tokens: Option<u64>,
    pub model_context_window: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollabSpawn {
    pub call_id: String,
    pub new_thread_id: String,
    pub agent_nickname: String,
    pub agent_role: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub prompt_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextElement {
    pub placeholder: String,
    pub byte_range: ByteRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTurn {
    pub turn_id: String,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub status: TurnStatus,
    pub user_message: Option<String>,
    pub text_elements: Vec<TextElement>,
    pub agent_messages: Vec<AgentMsg>,
    pub tool_calls: Vec<ToolCall>,
    pub final_answer: Option<String>,
    pub total_tokens: Option<TokenInfo>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub reasoning_effort: Option<String>,
    pub error: Option<String>,
    pub aborted_reason: Option<String>,
    pub has_compaction: bool,
    pub thread_name: Option<String>,
    pub collab_spawns: Vec<CollabSpawn>,
}

impl CodexTurn {
    pub fn new(turn_id: String) -> Self {
        CodexTurn {
            turn_id,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            status: TurnStatus::Ongoing,
            user_message: None,
            text_elements: Vec::new(),
            agent_messages: Vec::new(),
            tool_calls: Vec::new(),
            final_answer: None,
            total_tokens: None,
            model: None,
            cwd: None,
            reasoning_effort: None,
            error: None,
            aborted_reason: None,
            has_compaction: false,
            thread_name: None,
            collab_spawns: Vec::new(),
        }
    }
}

/// Build turns from a sequence of raw entries.
/// Handles both new format (task_started/task_complete) and old format (user_message-bounded).
pub fn build_turns(entries: &[RawEntry]) -> Vec<CodexTurn> {
    let mut turns: indexmap::IndexMap<String, CodexTurn> = indexmap::IndexMap::new();
    let mut current_turn_id: Option<String> = None;
    let mut tool_builders: HashMap<String, ToolCallBuilder> = HashMap::new();

    // Detect format: new (has task_started) vs old (user_message-bounded)
    let has_task_started = entries.iter().any(|e| {
        e.entry_type == "event_msg"
            && e.payload.get("type").and_then(|t| t.as_str()) == Some("task_started")
    });

    let mut synthetic_turn_counter = 0u32;

    for entry in entries {
        match entry.entry_type.as_str() {
            "event_msg" => {
                handle_event_msg(
                    entry,
                    &mut turns,
                    &mut current_turn_id,
                    &mut tool_builders,
                    has_task_started,
                    &mut synthetic_turn_counter,
                );
            }
            "response_item"
            | "function_call"
            | "function_call_output"
            | "message"
            | "reasoning" => {
                handle_response_item(entry, &mut turns, &current_turn_id, &mut tool_builders);
            }
            "turn_context" => {
                handle_turn_context(entry, &mut turns, &current_turn_id);
            }
            "compacted" => {
                if let Some(ref tid) = current_turn_id {
                    if let Some(turn) = turns.get_mut(tid) {
                        turn.has_compaction = true;
                    }
                }
            }
            _ => {}
        }
    }

    // Finalize all tool builders
    for (turn_id, mut builder) in tool_builders {
        builder.drain_pending();
        if let Some(turn) = turns.get_mut(&turn_id) {
            turn.tool_calls.extend(builder.finalized);
        }
    }

    let mut result: Vec<CodexTurn> = turns.into_values().collect();
    result.sort_by_key(|t| t.started_at.unwrap_or(0));
    result
}

fn handle_event_msg(
    entry: &RawEntry,
    turns: &mut indexmap::IndexMap<String, CodexTurn>,
    current_turn_id: &mut Option<String>,
    tool_builders: &mut HashMap<String, ToolCallBuilder>,
    has_task_started: bool,
    synthetic_counter: &mut u32,
) {
    let payload = &entry.payload;
    let msg_type = match payload.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return,
    };
    let ts = entry.timestamp.as_deref().unwrap_or("");

    match msg_type {
        "task_started" => {
            let turn_id = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if turn_id.is_empty() {
                return;
            }
            // Prefer turn_start_timestamp from payload (added in Codex v0.128.0 via #19473).
            // Fall back to the outer JSONL line timestamp for sessions captured before that.
            let started_at = payload
                .get("turn_start_timestamp")
                .and_then(|v| v.as_f64())
                .map(|v| v as u64)
                .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
            let mut turn = CodexTurn::new(turn_id.clone());
            turn.started_at = started_at;
            turns.insert(turn_id.clone(), turn);
            *current_turn_id = Some(turn_id.clone());
            tool_builders
                .entry(turn_id)
                .or_insert_with(ToolCallBuilder::new);
        }

        "user_message" => {
            let message = payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if !has_task_started {
                // Old format: each user_message starts a new turn
                *synthetic_counter += 1;
                let turn_id = format!("turn-{synthetic_counter}");
                let started_at = entry.timestamp.as_deref().and_then(parse_timestamp_secs);
                let mut turn = CodexTurn::new(turn_id.clone());
                turn.started_at = started_at;
                turn.user_message = Some(message.clone());
                turns.insert(turn_id.clone(), turn);
                *current_turn_id = Some(turn_id.clone());
                tool_builders
                    .entry(turn_id)
                    .or_insert_with(ToolCallBuilder::new);
            } else if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    if turn.user_message.is_none() {
                        turn.user_message = Some(message);
                    }
                }
            }
        }

        "agent_message" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let text = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        let phase = payload
                            .get("phase")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let is_final = phase.as_deref() == Some("final_answer");
                        if is_final && turn.final_answer.is_none() {
                            turn.final_answer = Some(text.clone());
                        }
                        turn.agent_messages.push(AgentMsg {
                            text,
                            phase,
                            timestamp: ts.to_string(),
                            is_reasoning: false,
                        });
                    }
                }
            }
        }

        "agent_reasoning" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let text = payload
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        turn.agent_messages.push(AgentMsg {
                            text,
                            phase: None,
                            timestamp: ts.to_string(),
                            is_reasoning: true,
                        });
                    }
                }
            }
        }

        "task_complete" => {
            let turn_id = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            if let Some(turn) = turns.get_mut(&turn_id) {
                turn.status = TurnStatus::Complete;
                // Prefer task_complete.last_agent_message as final_answer
                if let Some(last_msg) = payload
                    .get("last_agent_message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    turn.final_answer = Some(last_msg.to_string());
                }
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
                // Codex v0.128.0 adds prompt_tokens/completion_tokens/total_tokens to task_complete.
                // Use these only when no richer token_count event has already populated the turn.
                if turn.total_tokens.is_none() {
                    let prompt_tokens = payload.get("prompt_tokens").and_then(|v| v.as_u64());
                    let completion_tokens =
                        payload.get("completion_tokens").and_then(|v| v.as_u64());
                    let total = payload
                        .get("total_tokens")
                        .and_then(|v| v.as_u64())
                        .or_else(|| prompt_tokens.zip(completion_tokens).map(|(p, c)| p + c));
                    if let Some(total_tokens) = total {
                        turn.total_tokens = Some(TokenInfo {
                            input_tokens: prompt_tokens.unwrap_or(0),
                            cached_input_tokens: 0,
                            output_tokens: completion_tokens.unwrap_or(0),
                            reasoning_output_tokens: 0,
                            total_tokens,
                            context_window_tokens: None,
                            model_context_window: 0,
                        });
                    }
                }
            }
        }

        "turn_aborted" => {
            let turn_id_field = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            let target_id = if !turn_id_field.is_empty() {
                turn_id_field
            } else {
                current_turn_id.clone().unwrap_or_default()
            };
            if let Some(turn) = turns.get_mut(&target_id) {
                turn.status = TurnStatus::Aborted;
                turn.aborted_reason = payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
            }
        }

        "inference_stream_cancelled" => {
            let turn_id_field = payload
                .get("turn_id")
                .and_then(|v| v.as_str())
                .unwrap_or(current_turn_id.as_deref().unwrap_or(""))
                .to_string();
            let target_id = if !turn_id_field.is_empty() {
                turn_id_field
            } else {
                current_turn_id.clone().unwrap_or_default()
            };
            if let Some(turn) = turns.get_mut(&target_id) {
                turn.status = TurnStatus::Cancelled;
                turn.completed_at = payload
                    .get("completed_at")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u64)
                    .or_else(|| entry.timestamp.as_deref().and_then(parse_timestamp_secs));
                turn.duration_ms = payload.get("duration_ms").and_then(|v| v.as_u64());
            }
        }

        "token_count" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    if let Some(info) = payload.get("info").filter(|v| !v.is_null()) {
                        if let Some(total) = info.get("total_token_usage") {
                            let context_window_tokens = info
                                .get("last_token_usage")
                                .and_then(|last| last.get("total_tokens"))
                                .and_then(|v| v.as_u64());
                            turn.total_tokens = Some(TokenInfo {
                                input_tokens: u64_field(total, "input_tokens"),
                                cached_input_tokens: u64_field(total, "cached_input_tokens"),
                                output_tokens: u64_field(total, "output_tokens"),
                                reasoning_output_tokens: u64_field(
                                    total,
                                    "reasoning_output_tokens",
                                ),
                                total_tokens: u64_field(total, "total_tokens"),
                                context_window_tokens,
                                model_context_window: u64_field(info, "model_context_window"),
                            });
                        }
                    }
                }
            }
        }

        "error" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    let msg = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    turn.status = TurnStatus::Error;
                    turn.error = Some(msg);
                }
            }
        }

        "exec_command_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_exec(msg_type, payload);
            }
        }

        "mcp_tool_call_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_mcp(msg_type, payload);
            }
        }

        "patch_apply_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_patch(msg_type, payload);
            }
        }

        "web_search_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.add_web_search(msg_type, payload);
            }
        }

        "collab_agent_spawn_end" => {
            if let Some(ref tid) = current_turn_id {
                // Record collab spawn metadata
                if let Some(turn) = turns.get_mut(tid) {
                    let call_id = str_field(payload, "call_id");
                    let new_thread_id = str_field(payload, "new_thread_id");
                    let agent_nickname = str_field(payload, "new_agent_nickname");
                    let agent_role = str_field(payload, "new_agent_role");
                    let model = payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let reasoning_effort = payload
                        .get("reasoning_effort")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let prompt = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                    let prompt_preview = prompt.chars().take(200).collect();

                    turn.collab_spawns.push(CollabSpawn {
                        call_id: call_id.clone(),
                        new_thread_id,
                        agent_nickname,
                        agent_role,
                        model,
                        reasoning_effort,
                        prompt_preview,
                    });
                }

                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_spawn(msg_type, payload);
            }
        }

        "collab_waiting_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_wait(msg_type, payload);
            }
        }

        "collab_close_end" => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_close(msg_type, payload);
            }
        }

        other if other.ends_with("_end") => {
            if let Some(ref tid) = current_turn_id {
                let builder = tool_builders
                    .entry(tid.clone())
                    .or_insert_with(ToolCallBuilder::new);
                builder.finalize_unknown_end(other, payload);
            }
        }

        "thread_name_updated" => {
            if let Some(ref tid) = current_turn_id {
                if let Some(turn) = turns.get_mut(tid) {
                    turn.thread_name = payload
                        .get("thread_name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }
        }

        _ => {}
    }
}

fn handle_response_item(
    entry: &RawEntry,
    turns: &mut indexmap::IndexMap<String, CodexTurn>,
    current_turn_id: &Option<String>,
    tool_builders: &mut HashMap<String, ToolCallBuilder>,
) {
    let payload = if entry.entry_type == "response_item" {
        &entry.payload
    } else {
        &entry.raw
    };

    let item_type = match payload.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return,
    };

    let tid = match current_turn_id {
        Some(t) => t,
        None => return,
    };

    let builder = tool_builders
        .entry(tid.clone())
        .or_insert_with(ToolCallBuilder::new);

    match item_type {
        "function_call" => {
            let call_id = str_field(payload, "call_id");
            let name = str_field(payload, "name");
            let arguments_str = payload
                .get("arguments")
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let namespace = payload
                .get("namespace")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            builder.add_function_call(call_id, name, arguments_str, namespace);
        }

        "function_call_output" => {
            let call_id = str_field(payload, "call_id");
            let output = match payload.get("output") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            if let Some(spawn) = spawn_from_function_call_output(builder, &call_id, &output) {
                if let Some(turn) = turns.get_mut(tid) {
                    turn.collab_spawns.push(spawn);
                }
            }
            builder.add_function_call_output(&call_id, &output);
        }

        "custom_tool_call" => {
            let call_id = str_field(payload, "call_id");
            let name = str_field(payload, "name");
            let input = payload
                .get("input")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            builder.add_custom_tool_call(call_id, name, input);
        }

        "custom_tool_call_output" => {
            let call_id = str_field(payload, "call_id");
            // output field is a JSON string: {"output":"...","metadata":{"exit_code":N,...}}
            let raw_output = payload.get("output").and_then(|v| v.as_str()).unwrap_or("");
            let output = serde_json::from_str::<Value>(raw_output)
                .ok()
                .and_then(|v| {
                    v.get("output")
                        .and_then(|o| o.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| raw_output.to_string());
            let exit_code = serde_json::from_str::<Value>(raw_output)
                .ok()
                .and_then(|v| {
                    v.get("metadata")
                        .and_then(|m| m.get("exit_code"))
                        .and_then(|c| c.as_i64())
                        .map(|c| c as i32)
                });
            builder.finalize_custom_tool_output(&call_id, &output, exit_code);
        }

        // Codex v0.129.0 (PR #20540): apply_patch file changes moved from the
        // patch_apply_end event_msg into this turn item. Backfill the result onto the
        // PatchApply call that custom_tool_call_output already finalized.
        "apply_patch_end" => {
            let call_id = str_field(payload, "call_id");
            let success = payload.get("success").and_then(|v| v.as_bool());
            let changes = payload.get("changes").cloned();
            builder.backfill_patch_result(&call_id, success, changes);
        }

        _ => {}
    }
}

fn handle_turn_context(
    entry: &RawEntry,
    turns: &mut indexmap::IndexMap<String, CodexTurn>,
    current_turn_id: &Option<String>,
) {
    let payload = &entry.payload;
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let effort = payload
        .get("effort")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(ref tid) = current_turn_id {
        if let Some(turn) = turns.get_mut(tid) {
            if model.is_some() {
                turn.model = model;
            }
            if cwd.is_some() {
                turn.cwd = cwd;
            }
            if effort.is_some() {
                turn.reasoning_effort = effort;
            }
        }
    }
}

fn u64_field(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn spawn_from_function_call_output(
    builder: &ToolCallBuilder,
    call_id: &str,
    output: &str,
) -> Option<CollabSpawn> {
    let pending = builder.pending.get(call_id)?;
    if pending.name != "spawn_agent" {
        return None;
    }

    let parsed = parse_spawn_agent_output(output)?;
    let message = pending
        .arguments
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt_preview = message.chars().take(200).collect();
    let agent_role = pending
        .arguments
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let model = pending
        .arguments
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let reasoning_effort = pending
        .arguments
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(CollabSpawn {
        call_id: call_id.to_string(),
        new_thread_id: parsed.agent_id,
        agent_nickname: parsed.nickname,
        agent_role,
        model,
        reasoning_effort,
        prompt_preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::toolcall::ToolKind;

    fn entries(lines: &[&str]) -> Vec<RawEntry> {
        lines
            .iter()
            .filter_map(|line| RawEntry::parse(line))
            .collect()
    }

    #[test]
    fn links_spawn_agent_from_sdk_function_call_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:52:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:52:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\",\"model\":\"gpt-5.4-mini\",\"reasoning_effort\":\"medium\"}","call_id":"call_spawn"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Parfit\"}"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].collab_spawns.len(), 1);
        assert_eq!(turns[0].collab_spawns[0].call_id, "call_spawn");
        assert_eq!(turns[0].collab_spawns[0].new_thread_id, "worker-session");
        assert_eq!(turns[0].collab_spawns[0].agent_nickname, "Parfit");
        assert_eq!(turns[0].collab_spawns[0].agent_role, "worker");
        assert_eq!(
            turns[0].collab_spawns[0].model.as_deref(),
            Some("gpt-5.4-mini")
        );
        assert_eq!(
            turns[0].collab_spawns[0].reasoning_effort.as_deref(),
            Some("medium")
        );
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].kind, ToolKind::SpawnAgent);
    }

    #[test]
    fn records_context_window_tokens_from_last_token_usage() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:52:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:52:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:52:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":38000,"cached_input_tokens":12000,"output_tokens":2000,"reasoning_output_tokens":500,"total_tokens":40000},"last_token_usage":{"input_tokens":24000,"cached_input_tokens":8000,"output_tokens":1500,"reasoning_output_tokens":200,"total_tokens":26000},"model_context_window":100000}}}"#,
            r#"{"timestamp":"2026-04-27T04:52:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279923.0}}"#,
        ]);

        let turns = build_turns(&entries);

        let tokens = turns[0].total_tokens.as_ref().expect("token count");
        assert_eq!(tokens.total_tokens, 40000);
        assert_eq!(tokens.context_window_tokens, Some(26000));
        assert_eq!(tokens.model_context_window, 100000);
    }

    #[test]
    fn marks_failed_spawn_agent_output_without_child_link() {
        let failure =
            "Full-history forked agents inherit the parent agent type, model, and reasoning effort; omit agent_type, model, and reasoning_effort, or spawn without a full-history fork.";
        let line = format!(
            r#"{{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"call_spawn","output":{failure:?}}}}}"#
        );
        let lines = [
            r#"{"timestamp":"2026-04-27T04:52:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:52:00Z"}}"#.to_string(),
            r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#.to_string(),
            r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"fork_context\":true,\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#.to_string(),
            line,
            r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#.to_string(),
        ];
        let entries: Vec<RawEntry> = lines
            .iter()
            .filter_map(|line| RawEntry::parse(line))
            .collect();

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert!(turns[0].collab_spawns.is_empty());
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::SpawnAgent);
        assert_eq!(tool.status, "failed");
        assert_eq!(tool.output.as_deref(), Some(failure));
    }

    #[test]
    fn classifies_sdk_exec_command_function_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"printf hello\",\"workdir\":\"/tmp\",\"yield_time_ms\":1000}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"Chunk ID: abc123\nWall time: 0.2500 seconds\nProcess exited with code 0\nOriginal token count: 1\nOutput:\nhello\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279984.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.output.as_deref(), Some("hello\n"));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.status, "completed");
        assert_eq!(
            tool.command.as_ref().unwrap(),
            &vec!["printf hello".to_string()]
        );
        assert_eq!(tool.cwd.as_deref(), Some("/tmp"));
    }

    #[test]
    fn folds_write_stdin_output_into_running_sdk_exec_command() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"node slack.js history --channel '#ai-tools-on-call'\",\"workdir\":\"/workspace\",\"yield_time_ms\":1000}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"Chunk ID: e6e3cc\nWall time: 1.0020 seconds\nProcess running with session ID 72266\nOriginal token count: 0\nOutput:\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"response_item","payload":{"type":"function_call","name":"write_stdin","arguments":"{\"session_id\":72266,\"chars\":\"\",\"yield_time_ms\":1000,\"max_output_tokens\":30000}","call_id":"call_poll"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_poll","output":"Chunk ID: 507212\nWall time: 0.0000 seconds\nProcess exited with code 1\nOriginal token count: 19\nOutput:\n{\n  \"ok\": false,\n  \"error\": \"Slack API error: enterprise_is_restricted\"\n}\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279986.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.call_id, "call_exec");
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(tool.name, "exec_command");
        assert_eq!(tool.exit_code, Some(1));
        assert_eq!(tool.status, "failed");
        assert!(tool
            .output
            .as_deref()
            .unwrap()
            .contains("Slack API error: enterprise_is_restricted"));
        assert_eq!(
            tool.command.as_ref().unwrap(),
            &vec!["node slack.js history --channel '#ai-tools-on-call'".to_string()]
        );
        assert_eq!(tool.cwd.as_deref(), Some("/workspace"));
    }

    #[test]
    fn preserves_unwrapped_sdk_exec_output() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"tool with changed output shape\",\"workdir\":\"/tmp\"}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"plain future transport output\nstill visible\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279984.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert_eq!(
            tool.output.as_deref(),
            Some("plain future transport output\nstill visible\n")
        );
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn folds_single_running_exec_without_session_id_mapping() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-27T04:53:00Z","type":"session_meta","payload":{"id":"worker","timestamp":"2026-04-27T04:53:00Z"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:02Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"long command\",\"workdir\":\"/workspace\"}","call_id":"call_exec"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_exec","output":"still running under a future transport shape\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:04Z","type":"response_item","payload":{"type":"function_call","name":"write_stdin","arguments":"{\"session_id\":123,\"chars\":\"\"}","call_id":"call_poll"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_poll","output":"final chunk under a future transport shape\n"}}"#,
            r#"{"timestamp":"2026-04-27T04:53:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279986.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tool = &turns[0].tool_calls[0];
        assert_eq!(tool.call_id, "call_exec");
        assert_eq!(tool.kind, ToolKind::ExecCommand);
        assert!(tool
            .output
            .as_deref()
            .unwrap()
            .contains("final chunk under a future transport shape"));
        assert_eq!(tool.status, "completed");
    }

    #[test]
    fn links_spawn_agent_from_collab_spawn_end_event() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-16T11:48:00Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-16T11:48:00Z"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect graph\"}","call_id":"call_spawn"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:03Z","type":"event_msg","payload":{"type":"collab_agent_spawn_end","call_id":"call_spawn","sender_thread_id":"parent","new_thread_id":"worker-session","new_agent_nickname":"Noether","new_agent_role":"worker","prompt":"Collect graph","model":"gpt-5.4-mini","reasoning_effort":"medium","status":"pending_init"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Noether\"}"}}"#,
            r#"{"timestamp":"2026-04-16T11:48:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1776335285.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].collab_spawns.len(), 1);
        assert_eq!(turns[0].collab_spawns[0].new_thread_id, "worker-session");
        assert_eq!(turns[0].collab_spawns[0].agent_nickname, "Noether");
        assert_eq!(turns[0].tool_calls.len(), 1);
        assert_eq!(turns[0].tool_calls[0].kind, ToolKind::SpawnAgent);
    }

    // Codex v0.128.0 (#19473): task_started now includes turn_start_timestamp in the payload.
    // It should be used as started_at in preference to the outer JSONL line timestamp.
    #[test]
    fn turn_start_timestamp_in_payload_is_used_as_started_at() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            // turn_start_timestamp = 1746000050.0 (earlier than outer line timestamp 1746000060)
            r#"{"timestamp":"2026-04-30T10:01:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","turn_start_timestamp":1746000050.0}}"#,
            r#"{"timestamp":"2026-04-30T10:02:00Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746000120.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // started_at should come from turn_start_timestamp (1746000050), not the line timestamp
        assert_eq!(turns[0].started_at, Some(1746000050));
    }

    // Codex v0.128.0 (#19620): turn metadata headers are now ASCII-escaped JSON.
    // serde_json handles \uXXXX sequences natively; verify non-ASCII in payloads parses correctly.
    #[test]
    fn ascii_escaped_unicode_in_task_started_payload_is_parsed_correctly() {
        // Chinese characters ASCII-escaped as Codex v0.128.0 emits
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:01:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","turn_start_timestamp":1746000050.0}}"#,
            "{\"timestamp\":\"2026-04-30T10:01:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"\\u4e2d\\u6587\"}}",
            r#"{"timestamp":"2026-04-30T10:02:00Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746000120.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // The ASCII-escaped Unicode must be decoded to its actual UTF-8 string value
        assert_eq!(turns[0].user_message.as_deref(), Some("\u{4e2d}\u{6587}"));
    }

    #[test]
    fn reads_token_usage_from_task_complete_v0128() {
        // Codex v0.128.0 adds prompt_tokens/completion_tokens/total_tokens to task_complete.
        // These should populate turn.total_tokens when no prior token_count event exists.
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746007210.0,"prompt_tokens":1500,"completion_tokens":300,"total_tokens":1800}}"#,
        ]);

        let turns = build_turns(&entries);

        let tokens = turns[0]
            .total_tokens
            .as_ref()
            .expect("token info from task_complete");
        assert_eq!(tokens.input_tokens, 1500);
        assert_eq!(tokens.output_tokens, 300);
        assert_eq!(tokens.total_tokens, 1800);
        assert_eq!(tokens.cached_input_tokens, 0);
        assert_eq!(tokens.context_window_tokens, None);
    }

    #[test]
    fn inference_stream_cancelled_marks_turn_cancelled() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-1","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"Working on it...","phase":"commentary"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"event_msg","payload":{"type":"inference_stream_cancelled","turn_id":"turn-1","completed_at":1746007203.0,"duration_ms":2000}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Cancelled);
        assert_eq!(turns[0].completed_at, Some(1746007203));
        assert_eq!(turns[0].duration_ms, Some(2000));
    }

    #[test]
    fn inference_stream_cancelled_falls_back_to_entry_timestamp() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-2","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:05Z","type":"event_msg","payload":{"type":"inference_stream_cancelled","turn_id":"turn-2"}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Cancelled);
        assert!(turns[0].completed_at.is_some());
    }

    #[test]
    fn inference_stream_cancelled_uses_current_turn_when_no_turn_id() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-3","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:04Z","type":"event_msg","payload":{"type":"inference_stream_cancelled"}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Cancelled);
    }

    // Codex v0.130.0 (PR #21566): multi-page thread completeness.
    // The thread turns endpoint now paginates large threads and writes "unloaded"
    // stub entries as placeholders between pages.  build_turns must ignore all
    // non-full stubs so every real turn is present in the parsed output.
    #[test]
    fn multi_page_thread_all_turns_present_stubs_ignored() {
        let entries = entries(&[
            // session header
            r#"{"timestamp":"2026-05-08T10:00:00Z","type":"session_meta","payload":{"id":"long-session","timestamp":"2026-05-08T10:00:00Z"}}"#,
            // page 1 — turn 1 (full entries, no view_mode = legacy compat)
            r#"{"timestamp":"2026-05-08T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1","turn_start_timestamp":1746691201.0}}"#,
            r#"{"timestamp":"2026-05-08T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746691202.0}}"#,
            // unloaded stub that would appear between pages (view_mode:unloaded) — must be skipped
            r#"{"timestamp":"2026-05-08T10:00:03Z","type":"event_msg","view_mode":"unloaded","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            // summary stub — also must be skipped
            r#"{"timestamp":"2026-05-08T10:00:04Z","type":"event_msg","view_mode":"summary","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            // page 2 — turn 2 (full view_mode explicit)
            r#"{"timestamp":"2026-05-08T10:00:05Z","type":"event_msg","view_mode":"full","payload":{"type":"task_started","turn_id":"turn-2","turn_start_timestamp":1746691205.0}}"#,
            r#"{"timestamp":"2026-05-08T10:00:06Z","type":"event_msg","view_mode":"full","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1746691206.0}}"#,
            // page 3 — turn 3 (legacy, no view_mode)
            r#"{"timestamp":"2026-05-08T10:00:07Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3","turn_start_timestamp":1746691207.0}}"#,
            r#"{"timestamp":"2026-05-08T10:00:08Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-3","completed_at":1746691208.0}}"#,
        ]);

        let turns = build_turns(&entries);

        // All three real turns must be present; stubs must not create phantom turns
        assert_eq!(
            turns.len(),
            3,
            "expected exactly 3 complete turns, got {}",
            turns.len()
        );
        assert_eq!(turns[0].turn_id, "turn-1");
        assert_eq!(turns[1].turn_id, "turn-2");
        assert_eq!(turns[2].turn_id, "turn-3");
        assert!(turns.iter().all(|t| t.status == TurnStatus::Complete));
    }

    #[test]
    fn unknown_event_types_are_ignored_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-04-30T10:00:00Z","type":"session_meta","payload":{"id":"sess-4","timestamp":"2026-04-30T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-4"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:02Z","type":"event_msg","payload":{"type":"some_future_unknown_event","data":"whatever"}}"#,
            r#"{"timestamp":"2026-04-30T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-4","completed_at":1746007203.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
    }

    // Codex v0.129.0 (PR #20471) removed `item/fileChange` and `outputDelta` notification
    // events from the app-server event stream. codex-trace is unaffected because it reads
    // JSONL session files from disk — it never connects to the Codex app-server. Even if
    // these types appeared as event_msg entries in older JSONL files, they would be silently
    // dropped by the wildcard arm, and file-change data is already read from patch_apply_end
    // events via patch_changes. This test guards against regressions that would re-introduce
    // a dependency on these removed notification types.
    #[test]
    fn v0129_removed_item_file_change_and_output_delta_events_ignored_gracefully() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"sess-v0129","timestamp":"2026-05-07T10:00:00Z","cli_version":"0.129.0"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"event_msg","payload":{"type":"item/fileChange","path":"/tmp/foo.txt","action":"modified"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"event_msg","payload":{"type":"outputDelta","delta":"partial output text"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746604804.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert!(turns[0].tool_calls.is_empty());
    }

    // Codex v0.129.0 (PR #20540): apply_patch file changes moved from patch_apply_end
    // event into an apply_patch_end response_item (turn item). The PatchApply call is
    // finalized by custom_tool_call_output (exit_code, output) and the file changes are
    // backfilled by the subsequent apply_patch_end turn item.
    #[test]
    fn apply_patch_end_turn_item_backfills_file_changes_onto_finalized_call() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"v0129","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_patch","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/main.rs\n..."}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_patch","output":"{\"output\":\"Applied patch successfully\",\"metadata\":{\"exit_code\":0}}"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"response_item","payload":{"type":"apply_patch_end","call_id":"call_patch","success":true,"changes":[{"path":"src/main.rs","type":"modified"}]}}"#,
            r#"{"timestamp":"2026-05-07T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746614405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::PatchApply);
        assert_eq!(tc.name, "apply_patch");
        assert_eq!(tc.status, "completed");
        assert_eq!(tc.patch_success, Some(true));
        let changes = tc
            .patch_changes
            .as_ref()
            .expect("patch_changes should be set");
        assert!(changes.is_array());
        assert_eq!(changes.as_array().unwrap().len(), 1);
        assert_eq!(changes[0]["path"], "src/main.rs");
        assert_eq!(changes[0]["type"], "modified");
    }

    // Codex v0.129.0 (PR #20463): ApplyPatchEnd is now explicitly stored in limited
    // history mode, so a patch_apply_end event_msg may coexist with custom_tool_call_output
    // in the same session. Verify we get exactly one PatchApply entry (no duplicate) and
    // that patch_changes are backfilled from the event rather than lost.
    #[test]
    fn patch_apply_end_event_after_custom_tool_call_output_does_not_create_duplicate() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"v0129b","timestamp":"2026-05-07T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_patch","name":"apply_patch","input":"*** Begin Patch\n..."}}"#,
            r#"{"timestamp":"2026-05-07T10:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call_patch","output":"{\"output\":\"Patch applied\",\"metadata\":{\"exit_code\":0}}"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:04Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call_patch","success":true,"changes":[{"path":"lib.rs","type":"modified"}],"status":"completed"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746614405.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        // Must have exactly one tool call — no duplicate from the event.
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::PatchApply);
        assert_eq!(tc.patch_success, Some(true));
        let changes = tc
            .patch_changes
            .as_ref()
            .expect("patch_changes backfilled from event");
        assert_eq!(changes[0]["path"], "lib.rs");
    }

    // Old-format sessions (pre-v0.129.0): custom_tool_call + patch_apply_end event with no
    // custom_tool_call_output. The event still finalizes the call normally.
    #[test]
    fn patch_apply_end_event_finalizes_pending_call_in_old_format() {
        let entries = entries(&[
            r#"{"timestamp":"2026-01-01T10:00:00Z","type":"session_meta","payload":{"id":"old-fmt","timestamp":"2026-01-01T10:00:00Z"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call_old","name":"apply_patch","input":"*** Begin Patch\n..."}}"#,
            r#"{"timestamp":"2026-01-01T10:00:03Z","type":"event_msg","payload":{"type":"patch_apply_end","call_id":"call_old","success":true,"changes":[{"path":"old.rs","type":"created"}],"stdout":"Applied 1 hunk","status":"completed"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1735725604.0}}"#,
        ]);

        let turns = build_turns(&entries);

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].tool_calls.len(), 1);
        let tc = &turns[0].tool_calls[0];
        assert_eq!(tc.kind, ToolKind::PatchApply);
        assert_eq!(tc.name, "apply_patch");
        assert_eq!(tc.patch_success, Some(true));
        assert_eq!(tc.output.as_deref(), Some("Applied 1 hunk"));
        let changes = tc.patch_changes.as_ref().expect("patch_changes from event");
        assert_eq!(changes[0]["path"], "old.rs");
    }

    // Codex v0.129.0 (PRs #20502/#20682): persist_extended_history disabled; app-server
    // always returns a limited history window. codex-trace reads JSONL session files from
    // disk — it never fetches history from the app-server — so all turns recorded in the
    // rollout file are available regardless of the server-side history window. When Codex
    // compacts context in response to the limited window it writes a `compacted` entry,
    // which codex-trace detects via has_compaction. No turns are silently dropped.
    #[test]
    fn v0129_persist_extended_history_disabled_all_turns_captured_compaction_flagged() {
        let entries = entries(&[
            r#"{"timestamp":"2026-05-07T10:00:00Z","type":"session_meta","payload":{"id":"long-session","timestamp":"2026-05-07T10:00:00Z","cli_version":"0.129.0"}}"#,
            // Turn 1
            r#"{"timestamp":"2026-05-07T10:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
            r#"{"timestamp":"2026-05-07T10:00:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746597610.0}}"#,
            // Turn 2
            r#"{"timestamp":"2026-05-07T10:01:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-2"}}"#,
            r#"{"timestamp":"2026-05-07T10:01:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-2","completed_at":1746597670.0}}"#,
            // Turn 3 — Codex hits the limited history window and compacts context mid-turn.
            // The compacted entry records that history was summarised; all three turns are
            // still fully present in the JSONL file and captured by codex-trace.
            r#"{"timestamp":"2026-05-07T10:02:00Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-3"}}"#,
            r#"{"timestamp":"2026-05-07T10:02:01Z","type":"compacted","payload":{"summary":"Summarised previous turns due to history window limit"}}"#,
            r#"{"timestamp":"2026-05-07T10:02:10Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-3","completed_at":1746597730.0}}"#,
        ]);

        let turns = build_turns(&entries);

        // All three turns must be present — no silent truncation even though
        // the app-server only returned a limited history window to Codex CLI.
        assert_eq!(
            turns.len(),
            3,
            "all turns captured despite app-server history limit"
        );
        assert_eq!(turns[0].status, TurnStatus::Complete);
        assert_eq!(turns[1].status, TurnStatus::Complete);
        assert_eq!(turns[2].status, TurnStatus::Complete);

        // Compaction is correctly attributed to turn-3 (where the compacted entry appeared).
        assert!(!turns[0].has_compaction);
        assert!(!turns[1].has_compaction);
        assert!(
            turns[2].has_compaction,
            "turn-3 has_compaction set from compacted entry"
        );
    }
}
