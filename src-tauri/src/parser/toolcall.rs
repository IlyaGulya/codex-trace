use super::spawn::parse_spawn_agent_output;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    ExecCommand,
    McpTool,
    PatchApply,
    WebSearch,
    ImageGeneration,
    SpawnAgent,
    WaitAgent,
    CloseAgent,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub kind: ToolKind,
    pub name: String,
    pub arguments: Value,
    pub input_text: Option<String>,
    pub output: Option<String>,
    pub exit_code: Option<i32>,
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub duration_secs: Option<f64>,
    pub mcp_server: Option<String>,
    pub mcp_tool: Option<String>,
    pub patch_success: Option<bool>,
    pub patch_changes: Option<Value>,
    pub web_query: Option<String>,
    pub web_url: Option<String>,
    pub image_prompt: Option<String>,
    pub worker_session: Option<Box<super::session::CodexSession>>,
    pub status: String,
}

/// A pending (not yet finalized) tool call — waiting for its end event.
#[derive(Debug, Clone)]
pub struct PendingCall {
    pub name: String,
    pub arguments: Value,
    pub input_text: Option<String>,
    /// Raw namespace from the function_call payload (e.g. "mcp__codex_apps__github").
    pub namespace: Option<String>,
}

/// Builder that collects function_call / custom_tool_call entries and finalizes
/// them when the corresponding end event arrives.
pub struct ToolCallBuilder {
    pub pending: HashMap<String, PendingCall>,
    pub finalized: Vec<ToolCall>,
    pty_sessions: HashMap<String, String>,
    running_exec_call_ids: Vec<String>,
}

impl ToolCallBuilder {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            finalized: Vec::new(),
            pty_sessions: HashMap::new(),
            running_exec_call_ids: Vec::new(),
        }
    }

    /// Register a function_call (response_item).
    pub fn add_function_call(
        &mut self,
        call_id: String,
        name: String,
        arguments_str: &str,
        namespace: Option<String>,
    ) {
        let arguments = serde_json::from_str(arguments_str).unwrap_or(Value::Null);
        self.pending.insert(
            call_id,
            PendingCall {
                name,
                arguments,
                input_text: None,
                namespace,
            },
        );
    }

    /// Register a custom_tool_call (apply_patch etc).
    pub fn add_custom_tool_call(&mut self, call_id: String, name: String, input: Option<String>) {
        self.pending.insert(
            call_id,
            PendingCall {
                name,
                arguments: Value::Object(serde_json::Map::new()),
                input_text: input,
                namespace: None,
            },
        );
    }

    /// Finalize a custom_tool_call (apply_patch etc) with its output.
    pub fn finalize_custom_tool_output(
        &mut self,
        call_id: &str,
        output: &str,
        exit_code: Option<i32>,
    ) {
        if let Some(pending) = self.pending.remove(call_id) {
            self.finalized.push(ToolCall {
                call_id: call_id.to_string(),
                kind: ToolKind::PatchApply,
                name: pending.name,
                arguments: pending.arguments,
                input_text: pending.input_text,
                output: Some(output.to_string()),
                exit_code,
                command: None,
                cwd: None,
                duration_secs: None,
                mcp_server: None,
                mcp_tool: None,
                patch_success: exit_code.map(|c| c == 0),
                patch_changes: None,
                web_query: None,
                web_url: None,
                image_prompt: None,
                worker_session: None,
                status: if exit_code.unwrap_or(1) == 0 {
                    "completed".to_string()
                } else {
                    "failed".to_string()
                },
            });
        }
    }

    /// Register a function_call_output (no typed end event).
    /// If the pending call has an MCP namespace, classify as McpTool. Built-in
    /// collaboration calls are also typed here because newer Codex SDK logs do
    /// not emit the older collab_*_end events.
    pub fn add_function_call_output(&mut self, call_id: &str, output: &str) {
        if let Some(pending) = self.pending.remove(call_id) {
            if pending.name == "exec_command" {
                let parsed_output = parse_exec_function_output(output);
                if let Some(session_id) = &parsed_output.running_session_id {
                    self.pty_sessions
                        .insert(session_id.clone(), call_id.to_string());
                }
                if parsed_output.status == "running" {
                    push_unique(&mut self.running_exec_call_ids, call_id.to_string());
                }
                self.finalized.push(exec_tool_call_from_pending(
                    call_id.to_string(),
                    pending,
                    parsed_output,
                ));
                return;
            }

            if pending.name == "write_stdin" {
                let parsed_output = parse_exec_function_output(output);
                let original_call_id = session_id_from_arguments(&pending.arguments)
                    .and_then(|session_id| {
                        self.pty_sessions
                            .get(&session_id)
                            .cloned()
                            .map(|call_id| (call_id, Some(session_id)))
                    })
                    .or_else(|| {
                        self.single_running_exec_call_id()
                            .map(|call_id| (call_id, None))
                    });

                if let Some((original_call_id, session_id)) = original_call_id {
                    self.merge_pty_output(&original_call_id, parsed_output);
                    if !self
                        .finalized
                        .iter()
                        .any(|tc| tc.call_id == original_call_id && tc.status == "running")
                    {
                        self.running_exec_call_ids
                            .retain(|call_id| call_id != &original_call_id);
                        if let Some(session_id) = session_id {
                            self.pty_sessions.remove(&session_id);
                        }
                    }
                    return;
                }

                self.finalized.push(exec_tool_call_from_pending(
                    call_id.to_string(),
                    pending,
                    parsed_output,
                ));
                return;
            }

            if pending.name == "spawn_agent" {
                self.finalized.push(ToolCall {
                    call_id: call_id.to_string(),
                    kind: ToolKind::SpawnAgent,
                    name: pending.name,
                    arguments: pending.arguments,
                    input_text: pending.input_text,
                    output: Some(output.to_string()),
                    exit_code: None,
                    command: None,
                    cwd: None,
                    duration_secs: None,
                    mcp_server: None,
                    mcp_tool: None,
                    patch_success: None,
                    patch_changes: None,
                    web_query: None,
                    web_url: None,
                    image_prompt: None,
                    worker_session: None,
                    status: spawn_agent_status(output),
                });
                return;
            }

            let (kind, mcp_server, mcp_tool) = match &pending.namespace {
                Some(ns) if ns.starts_with("mcp__") => {
                    let (server, tool) = parse_mcp_namespace(ns, &pending.name);
                    (ToolKind::McpTool, server, tool)
                }
                _ if pending.name == "wait_agent" => (ToolKind::WaitAgent, None, None),
                _ if pending.name == "close_agent" => (ToolKind::CloseAgent, None, None),
                _ => (ToolKind::Unknown, None, None),
            };
            self.finalized.push(ToolCall {
                call_id: call_id.to_string(),
                kind,
                name: pending.name,
                arguments: pending.arguments,
                input_text: pending.input_text,
                output: Some(output.to_string()),
                exit_code: None,
                command: None,
                cwd: None,
                duration_secs: None,
                mcp_server,
                mcp_tool,
                patch_success: None,
                patch_changes: None,
                web_query: None,
                web_url: None,
                image_prompt: None,
                worker_session: None,
                status: "completed".to_string(),
            });
        }
    }

    fn merge_pty_output(&mut self, original_call_id: &str, output: ExecFunctionOutput) {
        let Some(tool_call) = self
            .finalized
            .iter_mut()
            .find(|tc| tc.call_id == original_call_id)
        else {
            return;
        };

        append_output(&mut tool_call.output, output.output);
        if output.exit_code.is_some() {
            tool_call.exit_code = output.exit_code;
        }
        if output.duration_secs.is_some() {
            tool_call.duration_secs =
                Some(tool_call.duration_secs.unwrap_or(0.0) + output.duration_secs.unwrap_or(0.0));
        }
        tool_call.status = output.status;
    }

    fn single_running_exec_call_id(&self) -> Option<String> {
        let mut running = self.running_exec_call_ids.iter();
        let first = running.next()?;
        if running.next().is_none() {
            Some(first.clone())
        } else {
            None
        }
    }

    /// Finalize with exec_command_end event.
    pub fn finalize_exec(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
            });

        let command: Option<Vec<String>> = payload
            .get("command")
            .and_then(|c| serde_json::from_value(c.clone()).ok());
        let exit_code = payload
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32);
        let cwd = payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let duration_secs = parse_duration(payload);
        // aggregated_output carries the actual command output; stdout is often empty.
        let output = ["aggregated_output", "stdout", "formatted_output"]
            .iter()
            .find_map(|key| {
                payload
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            });
        let status = str_field(payload, "status");

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::ExecCommand,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output,
            exit_code,
            command,
            cwd,
            duration_secs,
            mcp_server: None,
            mcp_tool: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status,
        });
    }

    /// Finalize with mcp_tool_call_end event.
    pub fn finalize_mcp(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
            });

        // Extract server + tool from invocation field, then namespace, then name.
        // namespace format: "mcp__<server>" (no trailing __, no tool name).
        let (mcp_server, mcp_tool) = if let Some(inv) = payload.get("invocation") {
            let server = inv
                .get("server")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let tool = inv
                .get("tool")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            (server, tool)
        } else if let Some(ns) = &pending.namespace {
            parse_mcp_namespace(ns, &pending.name)
        } else {
            parse_mcp_name(&pending.name)
        };

        // Extract output text from result.Ok.content[].text
        let output = extract_mcp_output(payload);
        let duration_secs = parse_duration(payload);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::McpTool,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs,
            mcp_server,
            mcp_tool,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status: "completed".to_string(),
        });
    }

    /// Finalize with patch_apply_end event.
    pub fn finalize_patch(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
            });

        let patch_success = payload.get("success").and_then(|v| v.as_bool());
        let patch_changes = payload.get("changes").cloned();
        let stdout = payload
            .get("stdout")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::PatchApply,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output: stdout,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            patch_success,
            patch_changes,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status: str_field(payload, "status"),
        });
    }

    /// Finalize with collab_agent_spawn_end event.
    pub fn finalize_spawn(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending_name = self.pending.remove(&call_id).map(|p| p.name);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::SpawnAgent,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status: str_field(payload, "status"),
        });
    }

    /// Finalize with collab_waiting_end event.
    pub fn finalize_wait(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending_name = self.pending.remove(&call_id).map(|p| p.name);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::WaitAgent,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status: "completed".to_string(),
        });
    }

    /// Finalize with collab_close_end event.
    pub fn finalize_close(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending_name = self.pending.remove(&call_id).map(|p| p.name);

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::CloseAgent,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status: "completed".to_string(),
        });
    }

    /// Finalize web_search (no call_id pairing — best-effort).
    pub fn add_web_search(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending_name = self.pending.remove(&call_id).map(|p| p.name);
        let query = payload
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let web_url = payload
            .get("action")
            .and_then(|a| a.get("url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::WebSearch,
            name: pending_name.unwrap_or_else(|| kind_name(event_type)),
            arguments: payload.clone(),
            input_text: None,
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            patch_success: None,
            patch_changes: None,
            web_query: query,
            web_url,
            image_prompt: None,
            worker_session: None,
            status: "completed".to_string(),
        });
    }

    /// Catch-all for any unrecognised *_end event — preserves name from pending.
    pub fn finalize_unknown_end(&mut self, event_type: &str, payload: &Value) {
        let call_id = str_field(payload, "call_id");
        let pending = self
            .pending
            .remove(&call_id)
            .unwrap_or_else(|| PendingCall {
                name: kind_name(event_type),
                arguments: Value::Null,
                input_text: None,
                namespace: None,
            });
        let output = ["output", "aggregated_output", "stdout"]
            .iter()
            .find_map(|key| {
                payload
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            });
        self.finalized.push(ToolCall {
            call_id,
            kind: ToolKind::Unknown,
            name: pending.name,
            arguments: pending.arguments,
            input_text: pending.input_text,
            output,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: parse_duration(payload),
            mcp_server: None,
            mcp_tool: None,
            patch_success: None,
            patch_changes: None,
            web_query: None,
            web_url: None,
            image_prompt: None,
            worker_session: None,
            status: str_field(payload, "status"),
        });
    }

    /// Drain any remaining pending calls as Unknown (no end event arrived).
    pub fn drain_pending(&mut self) {
        let pending: Vec<(String, PendingCall)> = self.pending.drain().collect();
        for (call_id, p) in pending {
            self.finalized.push(ToolCall {
                call_id,
                kind: ToolKind::Unknown,
                name: p.name,
                arguments: p.arguments,
                input_text: p.input_text,
                output: None,
                exit_code: None,
                command: None,
                cwd: None,
                duration_secs: None,
                mcp_server: None,
                mcp_tool: None,
                patch_success: None,
                patch_changes: None,
                web_query: None,
                web_url: None,
                image_prompt: None,
                worker_session: None,
                status: "unknown".to_string(),
            });
        }
        // Remove Unknown entries that share a call_id with a properly classified end-event entry.
        // This happens when function_call_output arrives before exec_command_end for the same
        // call_id — the output is finalized as Unknown first, then the end event adds the real entry.
        let paired: HashSet<String> = self
            .finalized
            .iter()
            .filter(|tc| tc.kind != ToolKind::Unknown)
            .map(|tc| tc.call_id.clone())
            .collect();
        self.finalized
            .retain(|tc| tc.kind != ToolKind::Unknown || !paired.contains(&tc.call_id));
    }
}

#[derive(Debug, Clone, Default)]
struct ExecFunctionOutput {
    output: Option<String>,
    exit_code: Option<i32>,
    duration_secs: Option<f64>,
    running_session_id: Option<String>,
    status: String,
}

fn exec_tool_call_from_pending(
    call_id: String,
    pending: PendingCall,
    parsed_output: ExecFunctionOutput,
) -> ToolCall {
    let command = command_from_arguments(&pending.arguments);
    let cwd = cwd_from_arguments(&pending.arguments);

    ToolCall {
        call_id,
        kind: ToolKind::ExecCommand,
        name: pending.name,
        arguments: pending.arguments,
        input_text: pending.input_text,
        output: parsed_output.output,
        exit_code: parsed_output.exit_code,
        command,
        cwd,
        duration_secs: parsed_output.duration_secs,
        mcp_server: None,
        mcp_tool: None,
        patch_success: None,
        patch_changes: None,
        web_query: None,
        web_url: None,
        image_prompt: None,
        worker_session: None,
        status: parsed_output.status,
    }
}

fn spawn_agent_status(output: &str) -> String {
    if parse_spawn_agent_output(output).is_some() {
        "completed"
    } else if output.trim().is_empty() {
        "unknown"
    } else {
        "failed"
    }
    .to_string()
}

fn command_from_arguments(arguments: &Value) -> Option<Vec<String>> {
    if let Some(cmd) = arguments.get("cmd").and_then(|v| v.as_str()) {
        return Some(vec![cmd.to_string()]);
    }
    arguments
        .get("command")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

fn cwd_from_arguments(arguments: &Value) -> Option<String> {
    ["workdir", "cwd"].iter().find_map(|key| {
        arguments
            .get(*key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })
}

fn session_id_from_arguments(arguments: &Value) -> Option<String> {
    arguments
        .get("session_id")
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
}

fn parse_exec_function_output(output: &str) -> ExecFunctionOutput {
    let duration_secs = parse_wall_time(output);
    let exit_code = parse_process_exit_code(output);
    let running_session_id = parse_running_session_id(output);
    let tool_output = display_output(output);
    let status = if exit_code.map(|code| code != 0).unwrap_or(false) {
        "failed"
    } else if running_session_id.is_some() || likely_running_output(output, exit_code) {
        "running"
    } else {
        "completed"
    }
    .to_string();

    ExecFunctionOutput {
        output: tool_output,
        exit_code,
        duration_secs,
        running_session_id,
        status,
    }
}

fn display_output(output: &str) -> Option<String> {
    if output.is_empty() {
        return None;
    }

    Some(
        payload_after_output_marker(output)
            .filter(|payload| !payload.is_empty())
            .unwrap_or(output)
            .to_string(),
    )
}

fn payload_after_output_marker(output: &str) -> Option<&str> {
    let mut offset = 0;
    for line in output.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if line.trim_end_matches('\n').trim_end_matches('\r').trim() == "Output:" {
            return Some(&output[offset..]);
        }
        if line_start == output.len() {
            break;
        }
    }
    None
}

fn parse_wall_time(output: &str) -> Option<f64> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("wall") && lower.contains("time") {
            parse_first_f64(line)
        } else {
            None
        }
    })
}

fn parse_process_exit_code(output: &str) -> Option<i32> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("exit") && lower.contains("code") {
            parse_first_i32(line)
        } else {
            None
        }
    })
}

fn parse_running_session_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let marker = "session id";
        let marker_index = lower.find(marker)?;
        let after_marker = &line[marker_index + marker.len()..];
        let id: String = after_marker
            .chars()
            .skip_while(|c| c.is_whitespace() || matches!(c, ':' | '='))
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        if id.is_empty() {
            None
        } else {
            Some(id)
        }
    })
}

fn likely_running_output(output: &str, exit_code: Option<i32>) -> bool {
    exit_code.is_none() && output.to_ascii_lowercase().contains("running")
}

fn parse_first_f64(text: &str) -> Option<f64> {
    let mut number = String::new();
    let mut started = false;
    for c in text.chars() {
        if c.is_ascii_digit() || (started && c == '.') {
            number.push(c);
            started = true;
        } else if started {
            break;
        }
    }
    number.parse().ok()
}

fn parse_first_i32(text: &str) -> Option<i32> {
    let mut number = String::new();
    let mut started = false;
    for c in text.chars() {
        if c.is_ascii_digit() || (!started && c == '-') {
            number.push(c);
            started = true;
        } else if started {
            break;
        }
    }
    number.parse().ok()
}

fn append_output(current: &mut Option<String>, next: Option<String>) {
    let Some(next) = next else {
        return;
    };

    match current {
        Some(current) if !current.is_empty() => {
            if !current.ends_with('\n') && !next.starts_with('\n') {
                current.push('\n');
            }
            current.push_str(&next);
        }
        _ => *current = Some(next),
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// Reconstruct MCP server + tool from the `namespace` field and function `name`.
///
/// OpenAI encodes MCP tools as: namespace = `mcp__<server>__[ns_suffix]`, name = `[_suffix]`.
/// `server` = full namespace without `mcp__` prefix (e.g. `codex_apps__github`).
/// `tool`   = reconstructed full tool name (ns_suffix concatenated with name).
///
/// Examples:
///   namespace="mcp__codex_apps__github", name="_get_pr_info"
///     → server="codex_apps__github", tool="github_get_pr_info"
///   namespace="mcp__computer_use__", name="screenshot"
///     → server="computer_use", tool="screenshot"
fn parse_mcp_namespace(namespace: &str, name: &str) -> (Option<String>, Option<String>) {
    let after_mcp = match namespace.strip_prefix("mcp__") {
        Some(s) => s,
        None => return (None, None),
    };
    // Use the full namespace segment (minus mcp__ and any trailing __) as the server identifier.
    let server = after_mcp.trim_end_matches("__");
    if server.is_empty() {
        return (None, None);
    }
    // Reconstruct full tool name: ns_suffix (after first __) concatenated with name.
    let full_tool = if let Some((_, ns_suffix)) = after_mcp.split_once("__") {
        format!("{ns_suffix}{name}")
    } else {
        name.to_string()
    };
    (Some(server.to_string()), Some(full_tool))
}

fn kind_name(event_type: &str) -> String {
    event_type
        .strip_suffix("_end")
        .unwrap_or(event_type)
        .to_string()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn parse_duration(v: &Value) -> Option<f64> {
    let dur = v.get("duration")?;
    let secs = dur.get("secs")?.as_f64()?;
    let nanos = dur.get("nanos").and_then(|n| n.as_f64()).unwrap_or(0.0);
    Some(secs + nanos / 1_000_000_000.0)
}

fn parse_mcp_name(name: &str) -> (Option<String>, Option<String>) {
    let parts: Vec<&str> = name.split("__").collect();
    if parts.len() >= 3 && parts[0] == "mcp" {
        (Some(parts[1].to_string()), Some(parts[2..].join("__")))
    } else {
        (Some("codex".to_string()), Some(name.to_string()))
    }
}

fn extract_mcp_output(payload: &Value) -> Option<String> {
    let content = payload
        .get("result")
        .and_then(|r| r.get("Ok"))
        .and_then(|ok| ok.get("content"))
        .and_then(|c| c.as_array())?;

    let texts: Vec<&str> = content
        .iter()
        .filter(|item| item.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
        .collect();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_exec_function_output, parse_mcp_namespace};

    #[test]
    fn namespace_with_tool_prefix_keeps_full_namespace_as_server() {
        // namespace="mcp__codex_apps__github", name="_get_pr_info"
        // server = "codex_apps__github" (full namespace without mcp__)
        // tool   = "github_get_pr_info" (ns_suffix + name)
        let (server, tool) = parse_mcp_namespace("mcp__codex_apps__github", "_get_pr_info");
        assert_eq!(server.as_deref(), Some("codex_apps__github"));
        assert_eq!(tool.as_deref(), Some("github_get_pr_info"));
    }

    #[test]
    fn namespace_with_trailing_double_underscore() {
        // namespace="mcp__computer_use__", name="screenshot"
        // trailing __ is trimmed → server="computer_use", tool="screenshot"
        let (server, tool) = parse_mcp_namespace("mcp__computer_use__", "screenshot");
        assert_eq!(server.as_deref(), Some("computer_use"));
        assert_eq!(tool.as_deref(), Some("screenshot"));
    }

    #[test]
    fn namespace_without_trailing_separator() {
        // namespace="mcp__my_server", name="do_thing"
        let (server, tool) = parse_mcp_namespace("mcp__my_server", "do_thing");
        assert_eq!(server.as_deref(), Some("my_server"));
        assert_eq!(tool.as_deref(), Some("do_thing"));
    }

    #[test]
    fn non_mcp_namespace_returns_none() {
        let (server, tool) = parse_mcp_namespace("other__ns__tool", "fn_name");
        assert_eq!(server, None);
        assert_eq!(tool, None);
    }

    #[test]
    fn exec_output_parsing_unaffected_by_codex_v0_130_0_banner_change() {
        // Codex v0.130.0 (PR #21683) removed "research preview" from the `codex exec`
        // startup banner. codex-trace must not pattern-match on banner wording — only
        // stable structural markers (Output:, exit code, wall time, session id) are used.
        let old_banner = "Codex - a coding agent (research preview)\nOutput:\nhello\nExit code: 0\nWall time: 0.5s\n";
        let new_banner = "Codex - a coding agent\nOutput:\nhello\nExit code: 0\nWall time: 0.5s\n";

        let old = parse_exec_function_output(old_banner);
        let new = parse_exec_function_output(new_banner);

        assert_eq!(
            old.status, new.status,
            "status differs between banner formats"
        );
        assert_eq!(old.exit_code, new.exit_code);
        assert_eq!(old.duration_secs, new.duration_secs);
        let old_out = old.output.unwrap_or_default();
        let new_out = new.output.unwrap_or_default();
        assert!(old_out.contains("hello"), "old banner output missing hello");
        assert!(new_out.contains("hello"), "new banner output missing hello");
        assert!(
            !old_out.contains("research preview"),
            "banner text leaked into output"
        );
    }
}
