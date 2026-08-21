use claude_replay_engine::seam::{
    epoch_secs, parse_marker, parse_path_timed_for, relativize, AgentStatus, AssistantPhase,
    Attachment, AttachmentContent, AttachmentKind, Block, CompactTrigger, LinePreprocessor,
    LoadedAttachment, Message, Metrics, PreprocessedLine, Shaping, SpanHint, SubAgent, TaskOp,
    Todo, ToolDuration, ToolExecution, ToolStatus, UsdCost,
};
#[cfg(test)]
use claude_replay_engine::seam::{
    replay, stamp_user_turns, BlockIndex, EpochSeconds, SessionAccumulator,
};
use serde_json::Value;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const AGENT_PATH_KEY_PREFIX: &str = "codex-agent-";
const SUBAGENT_THREAD_RESULT_PREFIX: &str = "\0codex-subagent-thread:";
// Adapter-private L2 names. `semantic_exec_messages` uses them to carry Codex's structured
// `parsed_cmd` actions through the ordinary ToolUse join; `codex_finish` consumes every one and
// emits only the canonical Read/Grep/LS activity vocabulary, with the command detail attached to
// the final action's output.
// They must never reach a Session or a presenter.
const EXPLORE_READ: &str = "__codex_explore_read";
const EXPLORE_SEARCH: &str = "__codex_explore_search";
const EXPLORE_LIST: &str = "__codex_explore_list";
const EXPLORE_DETAIL: &str = "__codex_explore_detail";

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CodexLinePreprocessor {
    child: Option<ChildRollout>,
    /// Codex 0.147+ records the model-facing `functions.exec` wrapper as a
    /// `custom_tool_call`, then records the operation the TUI actually shows as an
    /// `event_msg/item_completed` (`CommandExecution`, `FileChange`, …). The session's
    /// CLI version selects that schema; older wrappers have no semantic mirror and must
    /// remain visible.
    #[serde(default)]
    semantic_exec: bool,
    /// Wrapper call ids whose result is transport noise. This must ride the cursor:
    /// a durable resume can land between the call and its output.
    #[serde(default)]
    transport_calls: HashSet<String>,
    /// Latest accepted session cwd, used to relativize paths in `FileChange` events.
    #[serde(default)]
    cwd: String,
    /// Latest per-interaction context occupancy. Codex writes the compaction boundary before
    /// its post-compaction usage snapshot, so the adapter retains the preceding snapshot here
    /// to populate the canonical divider's `before → after` fields.
    #[serde(default)]
    last_context_tokens: Option<u64>,
    /// The pre-compaction occupancy waiting for the next `token_count`. This rides the durable
    /// cursor because a live/cache split can land in the few records between the two halves.
    #[serde(default)]
    pending_compaction_pre_tokens: Option<u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ChildRollout {
    session_started_at: f64,
    agent_path: String,
    skipping_parent_snapshot: bool,
}

impl LinePreprocessor for CodexLinePreprocessor {
    /// Cursor state (#14): the whole self. The child-rollout boundary machine is learned from
    /// the transcript's FIRST lines, which a resumed fold never re-reads — without this, a
    /// resume inside the parent-snapshot region would replay the cloned parent as child
    /// history, the exact bug the preprocessor exists to prevent.
    fn state(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn restore(&mut self, state: &Value) {
        if let Ok(p) = serde_json::from_value::<CodexLinePreprocessor>(state.clone()) {
            *self = p;
        }
    }

    fn process(&mut self, line: &str) -> PreprocessedLine {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return PreprocessedLine::Include;
        };

        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(spawn) = value.pointer("/payload/source/subagent/thread_spawn") {
                if self.child.is_none() {
                    self.child = Some(ChildRollout {
                        session_started_at: value
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .and_then(epoch_secs)
                            .unwrap_or_default(),
                        agent_path: spawn
                            .get("agent_path")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        skipping_parent_snapshot: false,
                    });
                }
                update_preprocessor_session(&value, &mut self.cwd, &mut self.semantic_exec);
                return PreprocessedLine::Include;
            }
            // A second session_meta in a child rollout starts a physical copy of the parent's
            // transcript. It is bootstrap context, not authored child history.
            if let Some(child) = self.child.as_mut() {
                child.skipping_parent_snapshot = true;
                return PreprocessedLine::Ignore;
            }
            update_preprocessor_session(&value, &mut self.cwd, &mut self.semantic_exec);
        }

        if let Some(child) = self.child.as_mut() {
            if child.skipping_parent_snapshot {
                let starts_this_child = value.get("type").and_then(Value::as_str)
                    == Some("event_msg")
                    && value.pointer("/payload/type").and_then(Value::as_str)
                        == Some("task_started")
                    && value
                        .pointer("/payload/started_at")
                        .and_then(Value::as_f64)
                        .is_some_and(|started| (started - child.session_started_at).abs() <= 5.0);
                if starts_this_child {
                    child.skipping_parent_snapshot = false;
                    return PreprocessedLine::Include;
                }
                return PreprocessedLine::Ignore;
            }
        }

        if value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
        {
            if let Some(post_tokens) = codex_context_tokens(&value) {
                self.last_context_tokens = Some(post_tokens);
                if let Some(pre_tokens) = self.pending_compaction_pre_tokens.take() {
                    return PreprocessedLine::Messages(vec![Message::CompactUsage {
                        pre_tokens,
                        post_tokens,
                    }]);
                }
            }
        }

        if value.get("type").and_then(Value::as_str) == Some("compacted") {
            let pre_tokens = self.last_context_tokens.unwrap_or(0);
            self.pending_compaction_pre_tokens = self.last_context_tokens;
            let ts = value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_secs);
            let mut messages = vec![
                Message::LineStart(ts),
                Message::CompactBoundary {
                    trigger: CompactTrigger::Auto,
                    pre_tokens,
                    post_tokens: 0,
                },
            ];
            if let Some(text) = value
                .pointer("/payload/message")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            {
                messages.push(Message::CompactSummary {
                    text: text.to_string(),
                });
            }
            return PreprocessedLine::Messages(messages);
        }

        // The modern Codex tool transport is two-layered:
        //
        //   response_item/custom_tool_call name=exec   (JavaScript orchestration)
        //   event_msg/item_completed                  (the command/edit Codex renders)
        //   response_item/custom_tool_call_output      (wrapper receipt)
        //
        // Normalize only here, in the Codex adapter. The engine and every presenter keep
        // consuming their existing Claude-shaped `Bash`/`Edit` vocabulary.
        if value.get("type").and_then(Value::as_str) == Some("response_item") {
            let payload = &value["payload"];
            let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
            let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");

            if matches!(kind, "function_call_output" | "custom_tool_call_output")
                && self.transport_calls.remove(call_id)
            {
                return PreprocessedLine::Ignore;
            }

            if self.semantic_exec && is_wait_transport(payload) {
                if !call_id.is_empty() {
                    self.transport_calls.insert(call_id.to_string());
                }
                return PreprocessedLine::Ignore;
            }

            if let Some((raw_name, input)) = orchestrated_task_call(payload) {
                if !call_id.is_empty() {
                    self.transport_calls.insert(call_id.to_string());
                }
                let ts = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(epoch_secs);
                let mut messages = vec![
                    Message::LineStart(ts),
                    Message::ToolUse {
                        id: call_id.to_string(),
                        name: raw_name.to_string(),
                        input: input.clone(),
                        cwd: self.cwd.clone(),
                    },
                ];
                if let Some(op) = codex_task_op(raw_name, &input) {
                    messages.push(Message::TaskOp(op));
                }
                if raw_name == "update_plan" {
                    messages.push(Message::ToolResult {
                        tool_use_id: call_id.to_string(),
                        text: codex_plan_update_text(&input),
                        tur: Value::Null,
                        is_error: None,
                    });
                }
                return PreprocessedLine::Messages(messages);
            }

            if self.semantic_exec && is_semantic_exec_transport(payload) {
                if !call_id.is_empty() {
                    self.transport_calls.insert(call_id.to_string());
                }
                return PreprocessedLine::Ignore;
            }
        }

        if self.semantic_exec {
            if let Some(messages) = semantic_exec_messages(&value, &self.cwd) {
                return PreprocessedLine::Messages(messages);
            }
        }

        if value.get("type").and_then(Value::as_str) == Some("response_item")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some("agent_message")
        {
            let payload = &value["payload"];
            let recipient = payload
                .get("recipient")
                .and_then(Value::as_str)
                .unwrap_or("");
            let author = payload.get("author").and_then(Value::as_str).unwrap_or("");
            let addressed_to_this_child = self
                .child
                .as_ref()
                .is_some_and(|child| !child.agent_path.is_empty() && recipient == child.agent_path);
            // A resumed fold starts above session_meta, so it cannot restore the child path.
            // Parent→descendant direction is an equivalent fallback for incoming assignments;
            // descendant→parent replies remain invisible in the parent's ordinary transcript.
            let incoming_from_parent = !author.is_empty()
                && recipient
                    .strip_prefix(author)
                    .is_some_and(|suffix| suffix.starts_with('/'));
            if addressed_to_this_child || incoming_from_parent {
                let text = payload
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("input_text"))
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .filter(|text| !text.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    let ts = value
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(epoch_secs);
                    return PreprocessedLine::Messages(vec![
                        Message::LineStart(ts),
                        Message::UserText { text },
                    ]);
                }
            }
        }

        PreprocessedLine::Include
    }
}

fn codex_context_tokens(value: &Value) -> Option<u64> {
    value
        .pointer("/payload/info/last_token_usage/total_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .pointer("/payload/info/last_token_usage/input_tokens")
                .and_then(Value::as_u64)
        })
}

fn update_preprocessor_session(value: &Value, cwd: &mut String, semantic_exec: &mut bool) {
    if let Some(next) = value
        .pointer("/payload/cwd")
        .and_then(Value::as_str)
        .filter(|next| !next.is_empty())
    {
        *cwd = next.to_string();
    }
    if let Some(version) = value
        .pointer("/payload/cli_version")
        .and_then(Value::as_str)
    {
        *semantic_exec = codex_version_at_least(version, (0, 147, 0));
    }
}

fn codex_version_at_least(version: &str, minimum: (u64, u64, u64)) -> bool {
    let mut parts = version.split('.').map(|part| {
        part.split_once('-')
            .map_or(part, |(number, _)| number)
            .parse::<u64>()
            .unwrap_or(0)
    });
    let found = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    found >= minimum
}

/// The orchestration calls whose outer `exec` item carries no user-facing meaning. Their
/// corresponding semantic `item_completed` record is mapped below. `write_stdin` is included:
/// a long-running command may have many polls, but Codex shows one completed command, not one
/// shell entry per poll.
fn is_semantic_exec_transport(payload: &Value) -> bool {
    if payload.get("type").and_then(Value::as_str) != Some("custom_tool_call")
        || payload.get("name").and_then(Value::as_str) != Some("exec")
    {
        return false;
    }
    let code = payload.get("input").and_then(Value::as_str).unwrap_or("");
    [
        "tools.exec_command(",
        "tools.write_stdin(",
        "tools.apply_patch(",
        "tools.web__run(",
    ]
    .iter()
    .any(|needle| code.contains(needle))
}

/// `functions.wait` polls an already-yielded orchestration cell. It is transport lifecycle,
/// not an agent action (the underlying command's `CommandExecution` event is the action).
fn is_wait_transport(payload: &Value) -> bool {
    payload.get("type").and_then(Value::as_str) == Some("function_call")
        && payload.get("name").and_then(Value::as_str) == Some("wait")
        && call_input(payload).get("cell_id").is_some()
}

/// Task/plan calls do not currently get a semantic `item_completed` mirror, so unwrap the
/// small JavaScript object literal from the outer exec and feed the existing task vocabulary.
fn orchestrated_task_call(payload: &Value) -> Option<(&'static str, Value)> {
    if payload.get("type").and_then(Value::as_str) != Some("custom_tool_call")
        || payload.get("name").and_then(Value::as_str) != Some("exec")
    {
        return None;
    }
    let code = payload.get("input").and_then(Value::as_str)?;
    for name in ["update_plan", "create_goal", "update_goal"] {
        if let Some(input) = js_tool_object(code, name) {
            return Some((name, input));
        }
    }
    None
}

/// Parse the JSON-like object passed to `tools.<name>(…)`. Codex-generated orchestration uses
/// JSON strings/arrays/values but occasionally leaves identifier-shaped object keys unquoted;
/// quote only those keys, outside strings, then let serde_json do the real parsing.
fn js_tool_object(code: &str, name: &str) -> Option<Value> {
    let marker = format!("tools.{name}(");
    let rest = code.split_once(&marker)?.1;
    let object = balanced_js_argument(rest)?;
    serde_json::from_str(&quote_bare_js_keys(object)).ok()
}

fn balanced_js_argument(input: &str) -> Option<&str> {
    let start = input.find('{')?;
    let mut depth = 0usize;
    let mut quote = None::<u8>;
    let mut escaped = false;
    for (offset, byte) in input.as_bytes()[start..].iter().copied().enumerate() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == q {
                quote = None;
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return input.get(start..=start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn quote_bare_js_keys(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len() + 16);
    let mut i = 0usize;
    let mut quote = None::<u8>;
    let mut escaped = false;
    let mut key_position = true;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(q) = quote {
            out.push(byte as char);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                out.push(byte as char);
                i += 1;
            }
            b'{' | b',' => {
                key_position = true;
                out.push(byte as char);
                i += 1;
            }
            b':' => {
                key_position = false;
                out.push(':');
                i += 1;
            }
            b if key_position && (b.is_ascii_alphabetic() || b == b'_') => {
                let start = i;
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let mut after = i;
                while after < bytes.len() && bytes[after].is_ascii_whitespace() {
                    after += 1;
                }
                if after < bytes.len() && bytes[after] == b':' {
                    out.push('"');
                    out.push_str(&input[start..i]);
                    out.push('"');
                } else {
                    out.push_str(&input[start..i]);
                }
            }
            _ => {
                out.push(byte as char);
                i += 1;
            }
        }
    }
    out
}

/// Map the TUI-facing Codex event vocabulary onto the canonical Claude-shaped tool
/// vocabulary. This is deliberately adapter-local: presenters only ever see Bash/Edit blocks
/// or canonical Read/Grep/LS activity spans.
fn semantic_exec_messages(value: &Value, fallback_cwd: &str) -> Option<Vec<Message>> {
    if value.get("type").and_then(Value::as_str) != Some("event_msg")
        || value.pointer("/payload/type").and_then(Value::as_str) != Some("item_completed")
    {
        return None;
    }
    let item = value.pointer("/payload/item")?;
    let ts = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(epoch_secs);
    match item.get("type").and_then(Value::as_str)? {
        "CommandExecution" => {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let command = command_execution_text(item)?;
            let cwd = command_execution_cwd(item).unwrap_or_else(|| fallback_cwd.to_string());
            if let Some(mut exploration) = command_exploration_messages(item, id, &command, &cwd) {
                exploration.insert(0, Message::LineStart(ts));
                return Some(exploration);
            }
            let mut messages = vec![
                Message::LineStart(ts),
                Message::ToolUse {
                    id: id.to_string(),
                    name: "exec_command".to_string(),
                    input: command_execution_input(serde_json::json!({ "cmd": command }), item),
                    cwd,
                },
            ];
            let output = command_execution_output(item);
            if !output.trim().is_empty() {
                messages.push(Message::ToolResult {
                    tool_use_id: id.to_string(),
                    text: output,
                    tur: Value::Null,
                    is_error: Some(command_execution_failed(item)),
                });
            }
            Some(messages)
        }
        "FileChange" => {
            let changes = item.get("changes").and_then(Value::as_object)?;
            if changes.is_empty() {
                return None;
            }
            let event_id = item.get("id").and_then(Value::as_str).unwrap_or("edit");
            let mut messages = vec![Message::LineStart(ts)];
            for (index, (path, change)) in changes.iter().enumerate() {
                let kind = change
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("update");
                let header = match kind {
                    "add" => "*** Add File: ",
                    "delete" => "*** Delete File: ",
                    _ => "*** Update File: ",
                };
                let diff = change
                    .get("unified_diff")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let patch = format!("{header}{path}\n{diff}");
                messages.push(Message::ToolUse {
                    id: format!("{event_id}:{index}"),
                    name: "apply_patch".to_string(),
                    input: Value::String(patch),
                    cwd: fallback_cwd.to_string(),
                });
            }
            Some(messages)
        }
        "Extension" if item.get("kind").and_then(Value::as_str) == Some("web.search") => {
            let id = item.get("id").and_then(Value::as_str).unwrap_or("web");
            let action = item.pointer("/action/type").and_then(Value::as_str);
            let action_url = item.pointer("/action/url").and_then(Value::as_str);
            let first_result_url = item
                .get("results")
                .and_then(Value::as_array)
                .and_then(|results| results.first())
                .and_then(|result| result.get("url"))
                .and_then(Value::as_str);
            let (name, input) = if action == Some("search") {
                let query = item
                    .pointer("/action/queries")
                    .and_then(Value::as_array)
                    .map(|queries| {
                        queries
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .filter(|query| !query.is_empty())
                    .or_else(|| {
                        item.get("query")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                ("WebSearch", serde_json::json!({ "query": query }))
            } else {
                (
                    "WebFetch",
                    serde_json::json!({
                        "description": action_url
                            .or(first_result_url)
                            .unwrap_or("open web result")
                    }),
                )
            };
            let mut messages = vec![
                Message::LineStart(ts),
                Message::ToolUse {
                    id: id.to_string(),
                    name: name.to_string(),
                    input,
                    cwd: fallback_cwd.to_string(),
                },
            ];
            let output = extension_results_text(item);
            if !output.is_empty() {
                messages.push(Message::ToolResult {
                    tool_use_id: id.to_string(),
                    text: output,
                    tur: Value::Null,
                    is_error: None,
                });
            }
            Some(messages)
        }
        _ => None,
    }
}

/// Codex already parsed the shell script into the exact actions its TUI calls “Explored”. An
/// exploration is non-empty and contains only read/list/search actions; one unknown action makes
/// the whole command an ordinary “Ran” command. Preserve that decision instead of trying to
/// reverse-engineer it later from a lossy shell string.
fn command_exploration_messages(
    item: &Value,
    id: &str,
    command: &str,
    cwd: &str,
) -> Option<Vec<Message>> {
    let parsed = item.get("parsed_cmd").and_then(Value::as_array)?;
    if parsed.is_empty()
        || parsed.iter().any(|action| {
            !matches!(
                action.get("type").and_then(Value::as_str),
                Some("read" | "search" | "list_files")
            )
        })
    {
        return None;
    }

    let mut messages = Vec::with_capacity(parsed.len() + 2);
    for (index, action) in parsed.iter().enumerate() {
        let kind = action.get("type").and_then(Value::as_str)?;
        let cmd = action.get("cmd").and_then(Value::as_str).unwrap_or("");
        let path = action
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty());
        let (name, input) = match kind {
            "read" => (
                EXPLORE_READ,
                serde_json::json!({ "file_path": path.unwrap_or(cmd) }),
            ),
            "search" => {
                let query = action
                    .get("query")
                    .and_then(Value::as_str)
                    .filter(|query| !query.is_empty());
                let target = match (query, path) {
                    (Some(query), Some(path)) => {
                        format!("{query} in {}", relativize(path, cwd))
                    }
                    (Some(query), None) => query.to_string(),
                    _ => cmd.to_string(),
                };
                (EXPLORE_SEARCH, serde_json::json!({ "query": target }))
            }
            "list_files" => match path {
                Some(path) => (EXPLORE_LIST, serde_json::json!({ "path": path })),
                None => (EXPLORE_LIST, serde_json::json!({ "description": cmd })),
            },
            _ => unreachable!("validated parsed_cmd kind"),
        };
        messages.push(Message::ToolUse {
            id: format!("{id}:action:{index}"),
            name: name.to_string(),
            input,
            cwd: cwd.to_string(),
        });
    }

    // The parsed actions drive the collapsed semantic summary. The exact composite command and
    // its aggregate output remain available when the span is expanded; `codex_finish` moves this
    // private detail tool onto the final canonical action and therefore adds no fake Bash count.
    let detail_id = format!("{id}:detail");
    messages.push(Message::ToolUse {
        id: detail_id.clone(),
        name: EXPLORE_DETAIL.to_string(),
        input: command_execution_input(serde_json::json!({ "cmd": command }), item),
        cwd: cwd.to_string(),
    });
    let output = command_execution_output(item);
    if !output.trim().is_empty() {
        messages.push(Message::ToolResult {
            tool_use_id: detail_id,
            text: output,
            tur: Value::Null,
            is_error: Some(command_execution_failed(item)),
        });
    }
    Some(messages)
}

fn command_execution_input(mut input: Value, item: &Value) -> Value {
    if let Some(execution) = command_execution_metadata(item) {
        input.as_object_mut().expect("tool input object").insert(
            "__execution".to_string(),
            serde_json::to_value(execution).expect("ToolExecution serializes"),
        );
    }
    input
}

fn command_execution_metadata(item: &Value) -> Option<ToolExecution> {
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .map(|status| match status {
            "completed" => ToolStatus::Completed,
            "failed" => ToolStatus::Failed,
            "declined" => ToolStatus::Declined,
            "cancelled" | "canceled" | "aborted" => ToolStatus::Cancelled,
            _ => ToolStatus::Unknown,
        });
    let exit_code = item
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    let duration = item.get("duration").and_then(|duration| {
        let secs = duration.get("secs").and_then(Value::as_u64)?;
        let nanos = duration
            .get("nanos")
            .and_then(Value::as_u64)
            .and_then(|nanos| u32::try_from(nanos).ok())?;
        Some(ToolDuration { secs, nanos })
    });
    (status.is_some() || exit_code.is_some() || duration.is_some()).then_some(ToolExecution {
        status,
        exit_code,
        duration,
    })
}

fn extension_results_text(item: &Value) -> String {
    let Some(results) = item.get("results").and_then(Value::as_array) else {
        return String::new();
    };
    results
        .iter()
        .filter_map(|result| {
            let title = result.get("title").and_then(Value::as_str).unwrap_or("");
            let url = result.get("url").and_then(Value::as_str).unwrap_or("");
            let snippet = result.get("snippet").and_then(Value::as_str).unwrap_or("");
            let text = [title, url, snippet]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn command_execution_text(item: &Value) -> Option<String> {
    match item.get("command")? {
        Value::String(command) => Some(command.clone()),
        Value::Array(parts) => {
            let parts = parts.iter().filter_map(Value::as_str).collect::<Vec<_>>();
            if parts.len() >= 3 && matches!(parts.get(1), Some(&"-lc" | &"-c")) {
                parts.last().map(|command| (*command).to_string())
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
    .filter(|command| !command.trim().is_empty())
}

fn command_execution_cwd(item: &Value) -> Option<String> {
    item.get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(|cwd| cwd.strip_prefix("file://").unwrap_or(cwd).to_string())
}

fn command_execution_output(item: &Value) -> String {
    for key in ["formatted_output", "aggregated_output"] {
        if let Some(output) = item.get(key).and_then(Value::as_str) {
            return output.to_string();
        }
    }
    let stdout = item.get("stdout").and_then(Value::as_str).unwrap_or("");
    let stderr = item.get("stderr").and_then(Value::as_str).unwrap_or("");
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}{stderr}"),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (true, true) => String::new(),
    }
}

fn command_execution_failed(item: &Value) -> bool {
    item.get("exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
        || item
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "completed")
}

/// Whether this raw rollout line says the TURN is over (#194), Codex-format: the turn
/// lifecycle is explicit — `event_msg`/`task_complete` ends it, `task_started` opens
/// it, and any `response_item` is by definition inside a turn. Anchored on the payload
/// envelope like the liveness scan, so quoted transcripts in tool output can't fake it.
pub(crate) fn turn_ended(raw_line: &str) -> Option<bool> {
    const EVENT: &str = "\"payload\":{\"type\":\"";
    let rest = &raw_line[raw_line.find(EVENT)? + EVENT.len()..];
    let kind = &rest[..rest.find('"')?];
    match kind {
        "task_complete" | "turn_aborted" => Some(true),
        "task_started" => Some(false),
        _ if raw_line.contains("\"type\":\"response_item\"") => Some(false),
        _ => None,
    }
}

pub(crate) fn encode_agent_path(path: &str) -> String {
    let encoded = path
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{AGENT_PATH_KEY_PREFIX}{encoded}")
}

pub(crate) fn decode_agent_path(key: &str) -> Option<String> {
    let encoded = key.strip_prefix(AGENT_PATH_KEY_PREFIX)?;
    if encoded.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
fn parse_codex(jsonl: &str) -> Vec<Block> {
    // In-memory batch entry on the shared engine (L1 `tokenize` → L2 `replay`). The
    // streaming path (the shared `SessionAccumulator`) also runs on the engine now, per line
    // via `decode_line` + `Replayer` (M9).
    replay(&tokenize(jsonl.lines()), &mut Vec::new(), &CODEX_SHAPING)
}

/// Codex's back-patch is simpler than Claude's — no `toolUseResult` metadata, and the
/// output is skipped for Edit/Write. Shim it into `Shaping::apply`'s `(&mut Block, &str,
/// &Value)` signature (the `Value` is always Null for Codex).
fn apply_output_shaping(block: &mut Block, text: &str, _tur: &Value) {
    apply_output(block, text.to_string());
}
fn codex_keep_orphan(text: &str) -> bool {
    // A child rollout id arrives as a Codex-only activity event. It is shaped as a
    // ToolResult so the shared Replayer can join it to the spawn call, but an event
    // mirrored into a child transcript has no matching call and must stay invisible.
    !text.starts_with(SUBAGENT_THREAD_RESULT_PREFIX)
}
fn codex_finish(blocks: Vec<Block>) -> Vec<Block> {
    fn flush(actions: &mut Vec<Block>, out: &mut Vec<Block>) {
        if actions.is_empty() {
            return;
        }
        out.push(Block::Thinking {
            text: String::new(),
            duration_secs: None,
            tools: std::mem::take(actions),
        });
    }

    fn canonical_activity(name: &str) -> Option<&'static str> {
        match name {
            EXPLORE_READ => Some("Read"),
            EXPLORE_SEARCH => Some("Grep"),
            EXPLORE_LIST => Some("LS"),
            _ => None,
        }
    }

    let mut out = Vec::with_capacity(blocks.len());
    let mut actions = Vec::new();
    for mut block in blocks {
        let name = match &block {
            Block::ToolUse { name, .. } => name.clone(),
            _ => String::new(),
        };
        if let Some(canonical) = canonical_activity(&name) {
            if let Block::ToolUse { name, .. } = &mut block {
                *name = canonical.to_string();
            }
            actions.push(block);
            continue;
        }
        if name == EXPLORE_DETAIL && !actions.is_empty() {
            let Block::ToolUse {
                target,
                output,
                execution,
                ..
            } = block
            else {
                unreachable!("detail name belongs to ToolUse")
            };
            let mut detail = format!("$ {target}");
            if let Some(output) = output.filter(|output| !output.is_empty()) {
                detail.push('\n');
                detail.push_str(&output);
            }
            if let Some(Block::ToolUse {
                output,
                execution: action_execution,
                ..
            }) = actions.last_mut()
            {
                *output = Some(detail);
                *action_execution = execution;
            }
            continue;
        }

        // A malformed/copy-trimmed stream could theoretically retain the detail without its
        // preceding actions. Degrade to the ordinary Bash representation instead of leaking an
        // adapter-private name into the canonical Session.
        flush(&mut actions, &mut out);
        if name == EXPLORE_DETAIL {
            if let Block::ToolUse { name, .. } = &mut block {
                *name = "Bash".to_string();
            }
        }
        out.push(block);
    }
    flush(&mut actions, &mut out);
    out
}

/// Codex's `build_tool`: collaboration spawns use the shared sub-agent block; every
/// other call follows the ordinary Codex tool shaping.
fn codex_build_tool(id: &str, raw_name: &str, input: &Value, cwd: &str) -> Block {
    if raw_name == "spawn_agent" {
        let field = |name| {
            input
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let description = field("task_name");
        return Block::SubAgent(SubAgent {
            agent_id: String::new(),
            tool_use_id: id.to_string(),
            agent_type: "agent".to_string(),
            description,
            prompt: field("message"),
            status: AgentStatus::Running,
            result: None,
            output_file: None,
            blocks: Vec::new(),
            subtree_cost: None,
        });
    }
    let (name, target, diffs) = call_details(raw_name, input, cwd);
    let execution = input
        .get("__execution")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    Block::ToolUse {
        name,
        target,
        diffs,
        output: None,
        patch: None,
        read_lines: None,
        cwd: cwd.to_string(),
        execution,
    }
}

/// Codex's L2 shaping: bare output back-patch, keep all orphans, and turn only the adapter's
/// structured `parsed_cmd` markers into canonical pure activity spans.
pub(crate) const CODEX_SHAPING: Shaping = Shaping {
    build_tool: codex_build_tool,
    join_result: apply_output_shaping,
    keep_orphan: codex_keep_orphan,
    finish_turns: codex_finish,
};

/// **Layer 1 — Codex tokenize.** Map Codex's `response_item` line shapes to the canonical
/// message log (design §3.2). Pure line-shaping — no back-patch / grouping (that is the
/// shared `replay`). `replay(tokenize(x), &CODEX_SHAPING)` is asserted bit-identical to
/// `parse_lines(x)`.
#[cfg(test)]
fn tokenize<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> Vec<Message> {
    let mut msgs: Vec<Message> = Vec::new();
    let mut cwd = String::new();
    for line in lines {
        decode_line(line.as_ref(), &mut cwd, &mut msgs);
    }
    msgs
}

/// **Layer 1 — Codex decode, per line** (the streaming unit). One raw `response_item` line →
/// 0+ canonical messages appended to `msgs`; `cwd` is threaded across lines (set from
/// `session_meta`). `tokenize` is this over every line; the streaming driver (M9) calls it
/// one line at a time so no whole-file `Vec<Message>` is built.
pub(crate) fn decode_line(line: &str, cwd: &mut String, msgs: &mut Vec<Message>) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let ts = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(epoch_secs);
    // Codex writes event_msg mirrors immediately before their canonical response_item
    // records, often at the exact same timestamp. Only canonical timeline records may
    // advance the replay clock; otherwise every reasoning duration is measured from its
    // duplicate agent_reasoning event and rounds down to 0s.
    if is_timeline_event(&value) {
        msgs.push(Message::LineStart(ts));
    }
    match value.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            // Running-current (#173): a later `session_meta` (a spawned thread) moves the
            // anchor forward; an absent/empty payload cwd keeps the previous value.
            if let Some(c) = value.pointer("/payload/cwd").and_then(Value::as_str) {
                if !c.is_empty() {
                    *cwd = c.to_string();
                }
            }
        }
        Some("response_item") => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                    if matches!(role, "user" | "assistant") {
                        for item in payload
                            .get("content")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                        {
                            match item.get("type").and_then(Value::as_str) {
                                Some("input_text") if role == "user" => {
                                    if let Some(text) =
                                        item.get("text").and_then(Value::as_str).filter(|text| {
                                            !text.trim().is_empty() && !is_host_context(text)
                                        })
                                    {
                                        msgs.push(Message::UserText {
                                            text: text.to_string(),
                                        });
                                    }
                                }
                                Some("output_text") if role == "assistant" => {
                                    if let Some(text) = item
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .filter(|text| !text.trim().is_empty())
                                    {
                                        if let Some(phase) = assistant_phase(payload) {
                                            msgs.push(Message::AssistantMessage {
                                                text: text.to_string(),
                                                phase,
                                            });
                                        } else {
                                            msgs.push(Message::AssistantText(text.to_string()));
                                        }
                                    }
                                }
                                Some("input_image") if role == "user" => {
                                    if let Some(attachment) = input_image_attachment(item) {
                                        msgs.push(Message::Attachment(attachment));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some("reasoning") => {
                    let text = payload
                        .get("summary")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|item| {
                            item.get("type").and_then(Value::as_str) == Some("summary_text")
                        })
                        .filter_map(|item| item.get("text").and_then(Value::as_str))
                        .filter(|text| !text.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        msgs.push(Message::Thinking { text, ts });
                    }
                }
                Some("function_call" | "custom_tool_call") => {
                    let raw_name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    // Raw fields only — the block is shaped in L2 via `codex_build_tool`.
                    let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                    let input = call_input(payload);
                    msgs.push(Message::ToolUse {
                        id: call_id.to_string(),
                        name: raw_name.to_string(),
                        input: input.clone(),
                        cwd: cwd.to_string(),
                    });
                    if let Some(op) = codex_task_op(raw_name, &input) {
                        msgs.push(Message::TaskOp(op));
                    }
                }
                Some("function_call_output" | "custom_tool_call_output") => {
                    let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                    let output = output_text(payload.get("output").unwrap_or(&Value::Null));
                    msgs.push(Message::ToolResult {
                        tool_use_id: call_id.to_string(),
                        text: output,
                        tur: Value::Null,
                        // No structural failure signal exists in observed rollouts (#23):
                        // no is_error/success/exit_code anywhere in a year of stores, and
                        // prose like "Script failed" is content, not a signal. Honest
                        // unknown, for every codex result shape below too.
                        is_error: None,
                    });
                }
                Some("tool_search_call") => {
                    let id = specialized_call_id(payload);
                    let input = payload.get("arguments").cloned().unwrap_or(Value::Null);
                    msgs.push(Message::ToolUse {
                        id,
                        name: "ToolSearch".to_string(),
                        input,
                        cwd: cwd.to_string(),
                    });
                }
                Some("tool_search_output") => msgs.push(Message::ToolResult {
                    tool_use_id: specialized_call_id(payload),
                    text: display_value(payload.get("tools").unwrap_or(&Value::Null)),
                    tur: Value::Null,
                    is_error: None,
                }),
                Some("web_search_call") => {
                    let action = payload.get("action").cloned().unwrap_or(Value::Null);
                    msgs.push(Message::ToolUse {
                        id: specialized_call_id(payload),
                        name: "WebSearch".to_string(),
                        input: action,
                        cwd: cwd.to_string(),
                    });
                }
                Some("image_generation_call") => {
                    let input = serde_json::json!({
                        "description": payload
                            .get("revised_prompt")
                            .and_then(Value::as_str)
                            .unwrap_or("generate image")
                    });
                    let id = specialized_call_id(payload);
                    msgs.push(Message::ToolUse {
                        id: id.clone(),
                        name: "ImageGeneration".to_string(),
                        input,
                        cwd: cwd.to_string(),
                    });
                    if let Some(result) = payload
                        .get("result")
                        .and_then(Value::as_str)
                        .filter(|result| !result.trim().is_empty())
                    {
                        if let Some((mime, _)) = encoded_image(result) {
                            msgs.push(Message::Attachment(deferred_image(mime)));
                        } else {
                            msgs.push(Message::ToolResult {
                                tool_use_id: id,
                                text: result.to_string(),
                                tur: Value::Null,
                                is_error: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        Some("event_msg") => {
            let Some(payload) = value.get("payload") else {
                return;
            };
            match payload.get("type").and_then(Value::as_str) {
                Some("turn_aborted") => msgs.push(Message::SystemNote {
                    text: match payload.get("reason").and_then(Value::as_str) {
                        Some("interrupted") | None => "Turn interrupted.".to_string(),
                        Some(reason) => format!("Turn aborted: {reason}"),
                    },
                }),
                Some("task_complete") => {
                    if let Some(error) = payload.get("error").and_then(codex_error_text) {
                        msgs.push(Message::SystemNote {
                            text: format!("Turn failed: {error}"),
                        });
                    }
                }
                Some("sub_agent_activity") => {
                    let call_id = payload
                        .get("event_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let thread_id = payload
                        .get("agent_thread_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    match payload.get("kind").and_then(Value::as_str) {
                        Some("started") if !call_id.is_empty() && !thread_id.is_empty() => {
                            msgs.push(Message::ToolResult {
                                tool_use_id: call_id.to_string(),
                                text: format!("{SUBAGENT_THREAD_RESULT_PREFIX}{thread_id}"),
                                tur: Value::Null,
                                is_error: None,
                            });
                        }
                        Some("interrupted") if !thread_id.is_empty() => {
                            msgs.push(Message::Completion {
                                tool_use_id: call_id.to_string(),
                                task_id: thread_id.to_string(),
                                status: Some(AgentStatus::Stopped),
                                description: payload
                                    .get("agent_path")
                                    .and_then(Value::as_str)
                                    .unwrap_or("agent")
                                    .to_string(),
                                result: None,
                            });
                        }
                        // `interacted` is activity, not a lifecycle transition. Keeping the spawn
                        // running is the accurate representation for a reusable Codex agent.
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Some("compacted") => {
            msgs.push(Message::CompactBoundary {
                trigger: CompactTrigger::Auto,
                pre_tokens: 0,
                post_tokens: 0,
            });
            if let Some(text) = value
                .pointer("/payload/message")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            {
                msgs.push(Message::CompactSummary {
                    text: text.to_string(),
                });
            }
        }
        _ => {}
    }
}

fn codex_error_text(error: &Value) -> Option<String> {
    match error {
        Value::String(text) => (!text.trim().is_empty()).then(|| text.to_string()),
        Value::Object(_) => error
            .get("message")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn specialized_call_id(payload: &Value) -> String {
    payload
        .get("call_id")
        .or_else(|| payload.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn codex_task_op(raw_name: &str, input: &Value) -> Option<TaskOp> {
    match raw_name {
        "update_plan" => Some(TaskOp::Snapshot {
            todos: input
                .get("plan")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    let text = item.get("step").and_then(Value::as_str)?.trim();
                    (!text.is_empty()).then(|| Todo {
                        text: text.to_string(),
                        status: item
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("pending")
                            .to_string(),
                        active_form: String::new(),
                    })
                })
                .collect(),
        }),
        "create_goal" => input
            .get("objective")
            .and_then(Value::as_str)
            .filter(|objective| !objective.trim().is_empty())
            .map(|objective| TaskOp::Snapshot {
                todos: vec![Todo {
                    text: objective.trim().to_string(),
                    status: "in_progress".to_string(),
                    active_form: String::new(),
                }],
            }),
        "update_goal" => Some(TaskOp::Update {
            task_id: "0".to_string(),
            status: input.get("status").and_then(Value::as_str).map(|status| {
                match status {
                    "complete" | "blocked" => "completed",
                    _ => "pending",
                }
                .to_string()
            }),
            subject: None,
            description: input
                .get("status")
                .and_then(Value::as_str)
                .filter(|status| *status == "blocked")
                .map(|_| "blocked".to_string()),
            active_form: None,
            add_blocks: Vec::new(),
            add_blocked_by: Vec::new(),
        }),
        _ => None,
    }
}

/// Codex's live plan cell contains more than the final task sidecar: it shows the optional
/// explanation and every status transition at the point it happened. Keep that timeline body
/// on the canonical TodoWrite block while `TaskOp::Snapshot` continues to drive session tasks.
fn codex_plan_update_text(input: &Value) -> String {
    let mut lines = vec!["Updated Plan".to_string()];
    if let Some(explanation) = input
        .get("explanation")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        lines.push(explanation.trim().to_string());
    }
    for item in input
        .get("plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(step) = item
            .get("step")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|step| !step.is_empty())
        else {
            continue;
        };
        let status = match item.get("status").and_then(Value::as_str) {
            Some("completed") => "Completed",
            Some("in_progress") => "InProgress",
            Some("pending") | None => "Pending",
            Some(other) => other,
        };
        lines.push(format!("{status}: {step}"));
    }
    lines.join("\n")
}

fn input_image_attachment(item: &Value) -> Option<Attachment> {
    let url = item.get("image_url").and_then(Value::as_str)?;
    if let Some((mime, _)) = data_image(url) {
        // #193: the kept prefix carries the whole `data:<mime>;base64,` header, so an
        // elided payload classifies exactly as a raw one; the marker becomes the hint.
        // The hint's frame reconstructs the LOADED content (`Base64.b64` is payload-only),
        // so the header is stripped here — it is always inside the kept 64 bytes. A prefix
        // that unexpectedly fails to strip yields no hint, and the walk handles it.
        let header = format!("data:{mime};base64,");
        let span = parse_marker(url).and_then(|m| {
            let payload_prefix = m.prefix.strip_prefix(&header)?;
            Some(SpanHint {
                off: m.off,
                len: m.len,
                prefix: payload_prefix.to_string(),
                postfix: m.postfix,
                mime: Some(mime.to_string()),
            })
        });
        let mut a = deferred_image(mime);
        a.content = AttachmentContent::Deferred {
            at: 0,
            index: 0,
            span,
        };
        return Some(a);
    }
    (!url.trim().is_empty()).then(|| Attachment {
        kind: AttachmentKind::Image,
        name: "remote-image".to_string(),
        path: None,
        content: AttachmentContent::None,
    })
}

fn deferred_image(mime: &str) -> Attachment {
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    };
    Attachment {
        kind: AttachmentKind::Image,
        name: format!("image.{ext}"),
        path: None,
        content: AttachmentContent::Deferred {
            at: 0,
            index: 0,
            span: None,
        },
    }
}

fn data_image(url: &str) -> Option<(&str, &str)> {
    let (meta, b64) = url.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    (mime.starts_with("image/") && !b64.is_empty()).then_some((mime, b64))
}

fn encoded_image(value: &str) -> Option<(&'static str, &str)> {
    if let Some((mime, b64)) = data_image(value) {
        let mime = match mime {
            "image/png" => "image/png",
            "image/jpeg" => "image/jpeg",
            "image/gif" => "image/gif",
            "image/webp" => "image/webp",
            _ => return None,
        };
        return Some((mime, b64));
    }
    let mime = if value.starts_with("iVBOR") {
        "image/png"
    } else if value.starts_with("/9j/") {
        "image/jpeg"
    } else if value.starts_with("R0lGOD") {
        "image/gif"
    } else if value.starts_with("UklGR") {
        "image/webp"
    } else {
        return None;
    };
    Some((mime, value))
}

pub(crate) fn nth_loaded_attachment(line: &str, index: usize) -> Option<LoadedAttachment> {
    let value = serde_json::from_str::<Value>(line).ok()?;
    let payload = value.get("payload")?;
    let (mime, b64) = match payload.get("type").and_then(Value::as_str) {
        Some("message") => payload
            .get("content")?
            .as_array()?
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("input_image"))
            .filter_map(|item| item.get("image_url").and_then(Value::as_str))
            .filter_map(data_image)
            .nth(index)?,
        Some("image_generation_call") if index == 0 => {
            encoded_image(payload.get("result")?.as_str()?)?
        }
        _ => return None,
    };
    Some(LoadedAttachment::Base64 {
        mime: mime.to_string(),
        b64: b64.to_string(),
    })
}

fn is_timeline_event(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("session_meta" | "response_item" | "compacted") => true,
        Some("event_msg") => match value.pointer("/payload/type").and_then(Value::as_str) {
            Some("turn_aborted") => true,
            Some("task_complete") => value.pointer("/payload/error").is_some(),
            _ => false,
        },
        _ => false,
    }
}

pub(crate) fn enrich_tree(path: &Path, blocks: &mut [Block]) {
    let mut seen = HashSet::new();
    seen.insert(normalized_path(path));
    let Some(relationships) = crate::agents::codex::discover::CodexRelationshipIndex::load(path)
    else {
        return;
    };
    enrich_descendants(path, blocks, &relationships, &mut seen);
}

fn enrich_descendants(
    root: &Path,
    blocks: &mut [Block],
    relationships: &crate::agents::codex::discover::CodexRelationshipIndex,
    seen: &mut HashSet<PathBuf>,
) {
    for block in blocks {
        let Block::SubAgent(agent) = block else {
            continue;
        };
        let Some(child_path) = relationships.subagent_source(root, &agent.agent_id) else {
            continue;
        };
        if !seen.insert(normalized_path(&child_path)) {
            continue;
        }
        let Ok((mut child_blocks, _, metrics)) =
            parse_path_timed_for(&crate::adapters::CodexAdapter, &child_path)
        else {
            continue;
        };
        enrich_descendants(&child_path, &mut child_blocks, relationships, seen);
        agent.subtree_cost = subtree_cost(&metrics, &child_blocks);
        agent.blocks = child_blocks;
    }
}

fn subtree_cost(metrics: &Metrics, blocks: &[Block]) -> Option<UsdCost> {
    let descendants: UsdCost = blocks
        .iter()
        .filter_map(|block| match block {
            Block::SubAgent(agent) => agent.subtree_cost,
            _ => None,
        })
        .sum();
    match metrics.cost_usd {
        Some(own) => Some(own + descendants),
        None if descendants > 0.0 => Some(descendants),
        None => None,
    }
}

fn normalized_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// **Frozen golden reference** (M9): production parses Codex through the streaming engine;
/// this pre-engine parser is retained only to pin the shared `replay` bit-identical in
/// `codex_replay_matches_parse_lines`.
#[cfg(test)]
fn parse_lines<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    user_times: &mut Vec<Option<EpochSeconds>>,
) -> Vec<Block> {
    let mut out = Vec::new();
    // Stamp the previous canonical event's user turns on the next canonical event
    // so an ignored event_msg mirror cannot move the replay timeline.
    let mut pending_ts: Option<EpochSeconds> = None;
    let mut stamped = 0usize;
    let mut slots: HashMap<String, BlockIndex> = HashMap::new();
    let mut cwd = String::new();
    // The previous canonical event's ts: a thinking's duration is `its ts − this`
    // (mirrors the engine's `prev_ts` after `decode_line` filters LineStart events).
    let mut prev_ts = None;

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.as_ref()) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_secs);
        if is_timeline_event(&value) {
            stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
            if pending_ts.is_some() {
                prev_ts = pending_ts;
            }
            pending_ts = timestamp;
        }
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                // Running-current (#173) — see `decode_line`; the golden reference mirrors it.
                if let Some(c) = value.pointer("/payload/cwd").and_then(Value::as_str) {
                    if !c.is_empty() {
                        cwd = c.to_string();
                    }
                }
            }
            Some("response_item") => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                match payload.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        push_message(payload, &mut out);
                    }
                    Some("reasoning") => {
                        let text = payload
                            .get("summary")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter(|item| {
                                item.get("type").and_then(Value::as_str) == Some("summary_text")
                            })
                            .filter_map(|item| item.get("text").and_then(Value::as_str))
                            .filter(|text| !text.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !text.is_empty() {
                            let duration_secs = match (timestamp, prev_ts) {
                                (Some(end), Some(start)) if end >= start => {
                                    Some((end - start) as u64)
                                }
                                _ => None,
                            };
                            out.push(Block::Thinking {
                                text,
                                duration_secs,
                                tools: Vec::new(),
                            });
                        }
                    }
                    Some("function_call" | "custom_tool_call") => {
                        let raw_name = payload
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        let input = call_input(payload);
                        let (name, target, diffs) = call_details(raw_name, &input, &cwd);
                        let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                        out.push(Block::ToolUse {
                            name,
                            target,
                            diffs,
                            output: None,
                            patch: None,
                            read_lines: None,
                            cwd: cwd.clone(),
                            execution: None,
                        });
                        let index = out.len() - 1;
                        if !call_id.is_empty() {
                            slots.insert(call_id.to_string(), index);
                        }
                    }
                    Some("function_call_output" | "custom_tool_call_output") => {
                        let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                        let output = output_text(payload.get("output").unwrap_or(&Value::Null));
                        if let Some(index) = slots.get(call_id).copied() {
                            apply_output(&mut out[index], output);
                        } else if !output.trim().is_empty() {
                            out.push(Block::ToolResult(output));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
    out
}

#[cfg(test)]
fn push_message(payload: &Value, out: &mut Vec<Block>) {
    let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
    if !matches!(role, "user" | "assistant") {
        return;
    }
    let wanted = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    for text in payload
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some(wanted))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .filter(|text| role != "user" || !is_host_context(text))
    {
        if role == "user" {
            out.push(Block::UserText(text.to_string()));
        } else if let Some(phase) = assistant_phase(payload) {
            out.push(Block::AssistantMessage {
                text: text.to_string(),
                phase,
            });
        } else {
            out.push(Block::AssistantText(text.to_string()));
        }
    }
}

fn assistant_phase(payload: &Value) -> Option<AssistantPhase> {
    match payload.get("phase").and_then(Value::as_str) {
        Some("commentary") => Some(AssistantPhase::Commentary),
        Some("final" | "final_answer") => Some(AssistantPhase::Final),
        _ => None,
    }
}

pub(crate) fn is_host_context(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("<environment_context>")
        || text.starts_with("<permissions instructions>")
        || text.starts_with("<recommended_plugins>")
        || text.starts_with("# AGENTS.md instructions")
}

fn call_input(payload: &Value) -> Value {
    if payload.get("type").and_then(Value::as_str) == Some("function_call") {
        return payload
            .get("arguments")
            .and_then(Value::as_str)
            .and_then(|arguments| serde_json::from_str(arguments).ok())
            .unwrap_or_else(|| payload.get("arguments").cloned().unwrap_or(Value::Null));
    }
    payload.get("input").cloned().unwrap_or(Value::Null)
}

fn normalize_tool_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "exec" | "exec_command" | "shell" | "shell_command" | "bash" => "Bash".into(),
        "apply_patch" | "edit" | "multi_edit" | "multiedit" => "Edit".into(),
        "write" | "write_file" => "Write".into(),
        "read" | "read_file" | "view_image" => "Read".into(),
        "grep" | "search" | "search_query" => "Grep".into(),
        "glob" | "list_files" => "Glob".into(),
        // Codex's whole-plan snapshot is the same semantic operation as Claude's
        // TodoWrite. The task sidecar already shares that vocabulary via TaskOp.
        "update_plan" => "TodoWrite".into(),
        _ => name.to_string(),
    }
}

fn call_details(
    raw_name: &str,
    input: &Value,
    cwd: &str,
) -> (String, String, Vec<(String, String)>) {
    let name = normalize_tool_name(raw_name);
    let raw_patch = match input {
        Value::String(text) => Some(text.as_str()),
        Value::Object(map) => map.get("patch").and_then(Value::as_str),
        _ => None,
    };
    let mut target = raw_patch
        .and_then(patch_target)
        .map(|path| relativize(&path, cwd))
        .unwrap_or_else(|| input_target(input, cwd));
    if target.is_empty() {
        if let Value::String(text) = input {
            target = text.replace('\n', " ");
        }
    }
    let diffs = if name == "Edit" {
        if let Some(patch) = raw_patch {
            patch_diffs(patch)
        } else {
            vec![(
                string_field(input, &["old_string", "old"]),
                string_field(input, &["new_string", "new"]),
            )]
        }
    } else if name == "Write" {
        vec![(String::new(), string_field(input, &["content", "text"]))]
    } else {
        Vec::new()
    };
    (name, target, diffs)
}

fn input_target(input: &Value, cwd: &str) -> String {
    for key in ["file_path", "path"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            return relativize(value, cwd);
        }
    }
    // Shell commands keep their physical lines. Every presenter already knows how to lay
    // out canonical multi-line Bash targets; flattening here was why a Codex compound lost
    // the same `│` command rows that Claude's Bash vocabulary preserves.
    for key in ["cmd", "command"] {
        if let Some(value) = input.get(key) {
            return value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| display_value(value));
        }
    }
    for key in ["query", "pattern", "description"] {
        if let Some(value) = input.get(key) {
            return display_value(value);
        }
    }
    String::new()
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.replace('\n', " "),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(" "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn string_field(input: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn patch_target(patch: &str) -> Option<String> {
    patch.lines().find_map(|line| {
        ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix).map(str::to_string))
    })
}

fn patch_diffs(patch: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut old = Vec::new();
    let mut new = Vec::new();
    let flush = |out: &mut Vec<(String, String)>, old: &mut Vec<String>, new: &mut Vec<String>| {
        if !old.is_empty() || !new.is_empty() {
            out.push((old.join("\n"), new.join("\n")));
            old.clear();
            new.clear();
        }
    };
    for line in patch.lines() {
        if line.starts_with("@@") || line.starts_with("*** ") {
            flush(&mut out, &mut old, &mut new);
        } else if let Some(line) = line.strip_prefix('-') {
            old.push(line.to_string());
        } else if let Some(line) = line.strip_prefix('+') {
            new.push(line.to_string());
        } else if let Some(line) = line.strip_prefix(' ') {
            old.push(line.to_string());
            new.push(line.to_string());
        }
    }
    flush(&mut out, &mut old, &mut new);
    out
}

fn output_text(value: &Value) -> String {
    match value {
        Value::String(text) => {
            if let Ok(nested) = serde_json::from_str::<Value>(text) {
                for pointer in ["/output", "/text", "/content/0/text"] {
                    if let Some(text) = nested.pointer(pointer).and_then(Value::as_str) {
                        return text.to_string();
                    }
                }
            }
            text.clone()
        }
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn apply_output(block: &mut Block, output: String) {
    match block {
        Block::SubAgent(agent) => {
            if let Some(thread_id) = output.strip_prefix(SUBAGENT_THREAD_RESULT_PREFIX) {
                agent.agent_id = thread_id.to_string();
                return;
            }
            let value = serde_json::from_str::<Value>(&output).ok();
            if let Some(agent_id) = value
                .as_ref()
                .and_then(|value| value.get("agent_id"))
                .and_then(Value::as_str)
            {
                if agent.agent_id.is_empty() {
                    // A legacy inline id never overrides the activity event's thread id —
                    // the thread id is what the relationship index resolves by.
                    agent.agent_id = agent_id.to_string();
                }
                return;
            }
            if let Some(task_name) = value
                .as_ref()
                .and_then(|value| value.get("task_name"))
                .and_then(Value::as_str)
            {
                if agent.agent_id.is_empty() {
                    // Older/copy-trimmed rollouts may lack the activity event. Preserve
                    // the previous path-key fallback, but never overwrite a real thread id.
                    agent.agent_id = encode_agent_path(task_name);
                }
                return;
            }
            if agent.agent_id.is_empty() && !output.trim().is_empty() {
                let target = agent.description.clone();
                *block = Block::ToolUse {
                    name: "spawn_agent".to_string(),
                    target,
                    diffs: Vec::new(),
                    output: Some(output),
                    patch: None,
                    read_lines: None,
                    // A description, not a path — never revealed, so no cwd anchor.
                    cwd: String::new(),
                    execution: None,
                };
            }
        }
        Block::ToolUse {
            name, output: slot, ..
        } if !matches!(name.as_str(), "Edit" | "Write") && !output.trim().is_empty() => {
            *slot = Some(output);
        }
        _ => {}
    }
}

/// The α-lite elision policy (#193). One node: `image_url` — an `input_image`'s
/// `data:<mime>;base64,…` payload; classification reads only the data: header, which the
/// kept prefix preserves. NOT listed, deliberately: `image_generation_call`'s `result` —
/// the bare key `result` is too generic for a suffix rule (other records render a
/// `result`), so that value stays unelided (ceiling-bounded), fail-safe.
pub const CODEX_ELISION: claude_replay_engine::seam::Elision =
    claude_replay_engine::seam::Elision::Keys(&[&["image_url"]]);

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_engine::seam::Block;

    /// #23: codex rollouts carry no structural failure signal (no is_error/success/
    /// exit_code in observed stores), so every result decodes with `is_error: None` —
    /// honest unknown, never a fake "succeeded".
    #[test]
    fn tool_results_report_no_error_signal() {
        let line = r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":[{"type":"input_text","text":"Script failed\nboom"}]}}"#;
        let mut cwd = String::new();
        let mut msgs = Vec::new();
        decode_line(line, &mut cwd, &mut msgs);
        let got = msgs
            .iter()
            .find_map(|m| match m {
                Message::ToolResult { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no ToolResult in {msgs:?}"));
        assert_eq!(got, None, "prose is content, not a signal");
    }

    #[test]
    fn agent_path_key_is_safe_and_reversible() {
        let path = "/root/spec_review/standards_axis";
        let key = encode_agent_path(path);

        assert_eq!(
            key,
            "codex-agent-2f726f6f742f737065635f7265766965772f7374616e64617264735f61786973"
        );
        assert!(!key.contains('/'));
        assert_eq!(decode_agent_path(&key).as_deref(), Some(path));
        assert_eq!(decode_agent_path("other-2f726f6f74"), None);
        assert_eq!(decode_agent_path("codex-agent-f"), None);
        assert_eq!(decode_agent_path("codex-agent-zz"), None);
        assert_eq!(decode_agent_path("codex-agent-ff"), None);
    }

    #[test]
    fn subagent_activity_uses_thread_id_and_final_answer_is_not_terminal() {
        let jsonl = concat!(
            r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"spec_review\",\"message\":\"review it\"}"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child-thread","agent_path":"/root/spec_review","kind":"started"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/spec_review\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"agent_message","author":"/root/spec_review","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nPASS"}]}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::SubAgent(agent)
                if agent.agent_id == "child-thread"
                    && agent.tool_use_id == "spawn-1"
                    && agent.description == "spec_review"
                    && agent.prompt == "review it"
                    && agent.status == AgentStatus::Running
        )));
        assert!(
            !blocks
                .iter()
                .any(|block| matches!(block, Block::AgentDone { .. })),
            "FINAL_ANSWER completes one interaction, not the persistent Codex agent"
        );
        assert!(
            !blocks
                .iter()
                .any(|block| matches!(block, Block::ToolResult(_))),
            "the identity-correlation event is adapter metadata, not visible output"
        );
    }

    #[test]
    fn interrupted_subagent_activity_emits_a_stopped_lifecycle_event() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child-thread","agent_path":"/root/review","kind":"started"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child-thread","agent_path":"/root/review","kind":"interrupted"}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::AgentDone {
                agent_id,
                status: AgentStatus::Stopped,
                ..
            } if agent_id == "child-thread"
        )));
    }

    #[test]
    fn input_image_surfaces_as_a_deferred_attachment() {
        let jsonl = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"inspect"},{"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}]}}"#;

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::Attachment(attachment)
                if attachment.kind == AttachmentKind::Image
                    && attachment.name == "image.png"
                    && attachment.content == (AttachmentContent::Deferred { at: 0, index: 0, span: None })
        )));
        assert_eq!(
            nth_loaded_attachment(jsonl, 0),
            Some(claude_replay_engine::seam::LoadedAttachment::Base64 {
                mime: "image/png".to_string(),
                b64: "aGVsbG8=".to_string(),
            })
        );
    }

    #[test]
    fn remote_input_image_stays_visible_without_claiming_downloadable_content() {
        let jsonl = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_image","image_url":"https://example.test/image.png"}]}}"#;

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::Attachment(attachment)
                if attachment.kind == AttachmentKind::Image
                    && attachment.name == "remote-image"
                    && attachment.content == AttachmentContent::None
        )));
        assert_eq!(nth_loaded_attachment(jsonl, 0), None);
    }

    #[test]
    fn update_plan_emits_a_replace_all_task_snapshot() {
        let line = r#"{"type":"response_item","payload":{"type":"function_call","name":"update_plan","call_id":"plan-1","arguments":"{\"explanation\":\"now\",\"plan\":[{\"step\":\"inspect\",\"status\":\"completed\"},{\"step\":\"fix\",\"status\":\"in_progress\"}]}"}}"#;
        let mut messages = Vec::new();
        decode_line(line, &mut String::new(), &mut messages);

        assert!(messages.iter().any(|message| matches!(
            message,
            Message::TaskOp(TaskOp::Snapshot { todos })
                if todos.len() == 2
                    && todos[0].text == "inspect"
                    && todos[0].status == "completed"
                    && todos[1].text == "fix"
                    && todos[1].status == "in_progress"
        )));
    }

    /// Codex 0.147's `functions.exec` transport is not a shell command. Its paired
    /// `item_completed` events are the semantic operations the real TUI renders; the adapter
    /// maps those to the existing Bash/Edit/TodoWrite vocabulary and drops wrapper receipts.
    #[test]
    fn orchestrated_exec_uses_command_filechange_and_plan_semantics() {
        let lines = [
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:00Z",
                "type": "session_meta",
                "payload": {"cwd": "/repo", "originator": "codex-tui", "cli_version": "0.147.0"}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call", "name": "exec", "call_id": "outer-command",
                    "input": "const results = await Promise.all([tools.exec_command({\"cmd\":\"git diff --check\\ngit status --short\"})]); results.forEach(text);"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:02Z",
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "CommandExecution", "id": "exec-1",
                    "command": ["/bin/zsh", "-lc", "git diff --check\ngit status --short"],
                    "cwd": "file:///repo", "status": "completed", "exit_code": 0,
                    "formatted_output": " M README.md\n"
                }}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:03Z",
                "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "outer-command", "output": [
                    {"type": "input_text", "text": "Script completed\nOutput:\n"},
                    {"type": "input_text", "text": "{\"output\":\" M README.md\\n\"}"}
                ]}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:04Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call", "name": "exec", "call_id": "outer-edit",
                    "input": "const patch = \"*** Begin Patch\"; text(await tools.apply_patch(patch));"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:05Z",
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "FileChange", "id": "exec-2", "status": "completed",
                    "changes": {
                        "/repo/README.md": {"type": "update", "unified_diff": "@@ -1 +1 @@\n-old readme\n+new readme\n"},
                        "/repo/bin/rowt": {"type": "update", "unified_diff": "@@ -2 +2 @@\n-old rowt\n+new rowt\n"}
                    }
                }}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:06Z",
                "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "outer-edit", "output": "{}"}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:07Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call", "name": "wait", "call_id": "outer-wait",
                    "arguments": "{\"cell_id\":\"42\"}"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:08Z",
                "type": "response_item",
                "payload": {"type": "function_call_output", "call_id": "outer-wait", "output": "done"}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:09Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call", "name": "exec", "call_id": "outer-plan",
                    "input": "const r = await tools.update_plan({explanation:\"done\",\"plan\":[{\"step\":\"inspect\",\"status\":\"completed\"}]}); text(r);"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:10Z",
                "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "outer-plan", "output": "{}"}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:11Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call", "name": "exec", "call_id": "outer-web",
                    "input": "const r = await tools.web__run({search_query:[{q:\"rust releases\"}]}); text(r);"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:12Z",
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "Extension", "kind": "web.search", "id": "exec-web",
                    "action": {"type": "search", "queries": ["rust releases"]},
                    "results": [{
                        "type": "text_result", "title": "Rust releases",
                        "url": "https://www.rust-lang.org/releases.html", "snippet": "Release notes"
                    }]
                }}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:13Z",
                "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "outer-web", "output": "Script completed"}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:14Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call", "name": "exec", "call_id": "outer-open",
                    "input": "const r = await tools.web__run({open:[{ref_id:\"turn0search0\"}]}); text(r);"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:15Z",
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "Extension", "kind": "web.search", "id": "exec-open",
                    "action": {"type": "openPage", "url": "https://example.test/opened"},
                    "results": [{"type": "text_result", "title": "Opened page", "snippet": "Body"}]
                }}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T06:00:16Z",
                "type": "response_item",
                "payload": {"type": "custom_tool_call_output", "call_id": "outer-open", "output": "Script completed"}
            }),
        ];
        let jsonl = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let path =
            std::env::temp_dir().join(format!("codex-semantic-exec-{}.jsonl", std::process::id()));
        std::fs::write(&path, jsonl).unwrap();
        let (blocks, _, _) = parse_path_timed_for(&crate::adapters::CodexAdapter, &path).unwrap();
        std::fs::remove_file(path).ok();

        let bash = blocks
            .iter()
            .filter_map(|block| match block {
                Block::ToolUse {
                    name,
                    target,
                    output,
                    ..
                } if name == "Bash" => Some((target.as_str(), output.as_deref())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            bash,
            [(
                "git diff --check\ngit status --short",
                Some(" M README.md\n")
            )],
            "one semantic command, not the outer JavaScript or its receipt"
        );

        let mut edits = blocks
            .iter()
            .filter_map(|block| match block {
                Block::ToolUse {
                    name,
                    target,
                    diffs,
                    ..
                } if name == "Edit" => Some((target.clone(), diffs.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        edits.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(edits.len(), 2, "one canonical edit per changed file");
        assert_eq!(edits[0].0, "README.md");
        assert_eq!(edits[0].1, [("old readme".into(), "new readme".into())]);
        assert_eq!(edits[1].0, "bin/rowt");
        assert_eq!(edits[1].1, [("old rowt".into(), "new rowt".into())]);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, output: Some(output), .. }
                if name == "TodoWrite"
                    && output == "Updated Plan\ndone\nCompleted: inspect"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, target, output: Some(output), .. }
                if name == "WebSearch"
                    && target == "rust releases"
                    && output.contains("https://www.rust-lang.org/releases.html")
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, target, output: Some(output), .. }
                if name == "WebFetch"
                    && target == "https://example.test/opened"
                    && output.contains("Opened page")
        )));
        assert!(!blocks.iter().any(|block| match block {
            Block::ToolUse { name, target, .. } => {
                name == "wait" || name == "exec" || target.contains("tools.")
            }
            Block::ToolResult(text) => text.contains("Script completed"),
            _ => false,
        }));
    }

    /// A real modern `CommandExecution` carries the semantic parse the Codex TUI uses for its
    /// Explored → Read/List/Search rows. Preserve that authoritative parse as one canonical pure
    /// activity span; keep the composite shell and aggregate output on the final action for the
    /// expanded view. Consecutive exploration calls coalesce just like the native cell.
    #[test]
    fn semantic_exec_maps_parsed_commands_to_canonical_activity_span() {
        let lines = [
            serde_json::json!({
                "timestamp": "2026-08-19T07:00:00Z",
                "type": "session_meta",
                "payload": {"cwd": "/repo", "originator": "codex-tui", "cli_version": "0.147.0"}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T07:00:01Z",
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "CommandExecution", "id": "exec-explore-1",
                    "command": ["/bin/zsh", "-lc", "sed -n '1,40p' src/lib.rs && rg needle src && rg --files"],
                    "cwd": "file:///repo", "status": "completed", "exit_code": 0,
                    "duration": {"secs": 1, "nanos": 230000000},
                    "parsed_cmd": [
                        {"type": "read", "cmd": "sed -n '1,40p' src/lib.rs", "name": "lib.rs", "path": "/repo/src/lib.rs"},
                        {"type": "search", "cmd": "rg needle src", "query": "needle", "path": "src"},
                        {"type": "list_files", "cmd": "rg --files", "path": "."}
                    ],
                    "formatted_output": "src/lib.rs\nsrc/main.rs\n"
                }}
            }),
            serde_json::json!({
                "timestamp": "2026-08-19T07:00:02Z",
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "CommandExecution", "id": "exec-explore-2",
                    "command": ["/bin/zsh", "-lc", "cat README.md"],
                    "cwd": "file:///repo", "status": "completed", "exit_code": 0,
                    "parsed_cmd": [
                        {"type": "read", "cmd": "cat README.md", "name": "README.md", "path": "/repo/README.md"}
                    ],
                    "formatted_output": "hello\n"
                }}
            }),
        ];
        let jsonl = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let path =
            std::env::temp_dir().join(format!("codex-parsed-command-{}.jsonl", std::process::id()));
        std::fs::write(&path, jsonl).unwrap();
        let (blocks, _, _) = parse_path_timed_for(&crate::adapters::CodexAdapter, &path).unwrap();
        std::fs::remove_file(path).ok();

        let [Block::Thinking {
            text,
            duration_secs,
            tools,
        }] = blocks.as_slice()
        else {
            panic!("expected one coalesced pure activity span, got {blocks:#?}");
        };
        assert!(text.is_empty());
        assert_eq!(*duration_secs, None);
        assert_eq!(tools.len(), 4);
        assert!(matches!(
            &tools[0],
            Block::ToolUse { name, target, output: None, .. }
                if name == "Read" && target == "src/lib.rs"
        ));
        assert!(matches!(
            &tools[1],
            Block::ToolUse { name, target, output: None, .. }
                if name == "Grep" && target == "needle in src"
        ));
        assert!(matches!(
            &tools[2],
            Block::ToolUse { name, target, output: Some(output), execution: Some(execution), .. }
                if name == "LS"
                    && target == "."
                    && output == "$ sed -n '1,40p' src/lib.rs && rg needle src && rg --files\nsrc/lib.rs\nsrc/main.rs\n"
                    && execution.status == Some(ToolStatus::Completed)
                    && execution.exit_code == Some(0)
                    && execution.duration == Some(ToolDuration { secs: 1, nanos: 230000000 })
        ));
        assert!(matches!(
            &tools[3],
            Block::ToolUse { name, target, output: Some(output), .. }
                if name == "Read"
                    && target == "README.md"
                    && output == "$ cat README.md\nhello\n"
        ));
        assert!(!format!("{blocks:#?}").contains("__codex_explore"));
    }

    /// One unknown parsed action makes the native Codex cell a normal Ran command. Do not split
    /// or partially relabel it: the exact command and output stay in one canonical Bash block.
    #[test]
    fn semantic_exec_with_unknown_parsed_action_stays_bash() {
        let lines = [
            serde_json::json!({
                "type": "session_meta",
                "payload": {"cwd": "/repo", "cli_version": "0.147.0"}
            }),
            serde_json::json!({
                "type": "event_msg",
                "payload": {"type": "item_completed", "item": {
                    "type": "CommandExecution", "id": "exec-run",
                    "command": ["/bin/zsh", "-lc", "cat README.md && cargo test"],
                    "cwd": "file:///repo", "status": "failed", "exit_code": 7,
                    "duration": {"secs": 0, "nanos": 42000000},
                    "parsed_cmd": [
                        {"type": "read", "cmd": "cat README.md", "name": "README.md", "path": "/repo/README.md"},
                        {"type": "unknown", "cmd": "cargo test"}
                    ],
                    "formatted_output": "ok\n"
                }}
            }),
        ];
        let jsonl = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let path = std::env::temp_dir().join(format!(
            "codex-unknown-command-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, jsonl).unwrap();
        let (blocks, _, _) = parse_path_timed_for(&crate::adapters::CodexAdapter, &path).unwrap();
        std::fs::remove_file(path).ok();

        assert!(matches!(
            blocks.as_slice(),
            [Block::ToolUse { name, target, output: Some(output), execution: Some(execution), .. }]
                if name == "Bash"
                    && target == "cat README.md && cargo test"
                    && output == "ok\n"
                    && execution.status == Some(ToolStatus::Failed)
                    && execution.exit_code == Some(7)
                    && execution.duration == Some(ToolDuration { secs: 0, nanos: 42000000 })
        ));
    }

    #[test]
    fn semantic_exec_is_versioned_because_old_wrappers_have_no_mirror() {
        let wrapper = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call", "name": "exec", "call_id": "outer",
                "input": "const r = await tools.exec_command({\"cmd\":\"pwd\"}); text(r);"
            }
        })
        .to_string();

        let mut old = CodexLinePreprocessor::default();
        old.process(
            &serde_json::json!({
                "type": "session_meta",
                "payload": {"cli_version": "0.144.6", "cwd": "/repo"}
            })
            .to_string(),
        );
        assert!(matches!(old.process(&wrapper), PreprocessedLine::Include));

        let mut modern = CodexLinePreprocessor::default();
        modern.process(
            &serde_json::json!({
                "type": "session_meta",
                "payload": {"cli_version": "0.147.0", "cwd": "/repo"}
            })
            .to_string(),
        );
        assert!(matches!(modern.process(&wrapper), PreprocessedLine::Ignore));
    }

    #[test]
    fn compaction_usage_snapshot_backpatches_the_dividers_context_sizes() {
        let jsonl = concat!(
            r#"{"timestamp":"2026-08-19T06:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400,"last_token_usage":{"input_tokens":244385,"output_tokens":200,"total_tokens":244585},"total_token_usage":{"input_tokens":1000000,"cached_input_tokens":900000,"output_tokens":10000}}}}"#,
            "\n",
            r#"{"timestamp":"2026-08-19T06:00:01Z","type":"compacted","payload":{"message":"synthetic continuation","window_number":6}}"#,
            "\n",
            r#"{"timestamp":"2026-08-19T06:00:01Z","type":"world_state","payload":{"full":true,"state":{}}}"#,
            "\n",
            r#"{"timestamp":"2026-08-19T06:00:01Z","type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-19T06:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400,"last_token_usage":{"input_tokens":0,"output_tokens":0,"total_tokens":17186},"total_token_usage":{"input_tokens":1000000,"cached_input_tokens":900000,"output_tokens":10000}}}}"#,
            "\n",
        );
        let path = std::env::temp_dir().join(format!(
            "codex-compaction-usage-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, jsonl).unwrap();
        let (blocks, _, _) = parse_path_timed_for(&crate::adapters::CodexAdapter, &path).unwrap();
        std::fs::remove_file(path).ok();

        assert!(matches!(
            blocks.as_slice(),
            [Block::Compaction {
                trigger: CompactTrigger::Auto,
                pre_tokens: 244_585,
                post_tokens: 17_186,
                summary,
            }] if summary == "synthetic continuation"
        ));

        let mut live = SessionAccumulator::new(&crate::adapters::CodexAdapter);
        let mut offset = 0;
        for (index, line) in jsonl.lines().enumerate() {
            let patched = live.advance_at(offset, line);
            if index == 4 {
                assert_eq!(patched, Some(0), "the late usage must patch block zero");
            }
            offset += line.len() as u64 + 1;
        }
        assert!(matches!(
            live.snapshot().blocks().as_slice(),
            [Block::Compaction {
                pre_tokens: 244_585,
                post_tokens: 17_186,
                ..
            }]
        ));

        // A durable split can land after the boundary and before the following usage record.
        let mut before = CodexLinePreprocessor::default();
        assert!(matches!(
            before.process(jsonl.lines().next().unwrap()),
            PreprocessedLine::Include
        ));
        assert!(matches!(
            before.process(jsonl.lines().nth(1).unwrap()),
            PreprocessedLine::Messages(_)
        ));
        let state = before.state();
        let mut resumed = CodexLinePreprocessor::default();
        resumed.restore(&state);
        let update = resumed.process(jsonl.lines().nth(4).unwrap());
        assert!(matches!(
            update,
            PreprocessedLine::Messages(messages)
                if matches!(messages.as_slice(), [Message::CompactUsage {
                    pre_tokens: 244_585,
                    post_tokens: 17_186,
                }])
        ));
    }

    #[test]
    fn lifecycle_failures_and_compaction_stay_visible() {
        let jsonl = concat!(
            r#"{"timestamp":"2026-08-19T06:00:00Z","type":"compacted","payload":{"message":"synthetic continuation"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-19T06:00:01Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-19T06:00:02Z","type":"event_msg","payload":{"type":"task_complete","error":{"message":"synthetic policy failure","codex_error_info":"policy"}}}"#,
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::Compaction {
                trigger: CompactTrigger::Auto,
                pre_tokens: 0,
                post_tokens: 0,
                summary,
            } if summary == "synthetic continuation"
        )));
        assert!(blocks
            .iter()
            .any(|block| matches!(block, Block::ToolResult(text) if text == "Turn interrupted.")));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolResult(text) if text == "Turn failed: synthetic policy failure"
        )));
        assert_eq!(
            turn_ended(
                r#"{"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}"#
            ),
            Some(true)
        );
    }

    #[test]
    fn goal_calls_create_and_complete_the_single_task() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"create_goal","call_id":"goal-1","arguments":"{\"objective\":\"ship it\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"update_goal","call_id":"goal-2","arguments":"{\"status\":\"complete\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"update_goal","call_id":"goal-3","arguments":"{\"status\":\"blocked\"}"}}"#,
        );
        let mut messages = Vec::new();
        for line in jsonl.lines() {
            decode_line(line, &mut String::new(), &mut messages);
        }

        assert!(messages.iter().any(|message| matches!(
            message,
            Message::TaskOp(TaskOp::Snapshot { todos })
                if todos.len() == 1
                    && todos[0].text == "ship it"
                    && todos[0].status == "in_progress"
        )));
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::TaskOp(TaskOp::Update { task_id, status: Some(status), .. })
                if task_id == "0" && status == "completed"
        )));
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::TaskOp(TaskOp::Update {
                task_id,
                status: Some(status),
                description: Some(description),
                ..
            }) if task_id == "0" && status == "completed" && description == "blocked"
        )));
    }

    #[test]
    fn specialized_search_items_join_like_an_ordinary_tool() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"tool_search_call","id":"search-1","call_id":"search-1","arguments":{"query":"calendar tools"},"status":"in_progress"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"tool_search_output","call_id":"search-1","status":"completed","tools":[{"name":"calendar.list","description":"List events"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"web_search_call","id":"web-1","status":"completed","action":{"type":"search","query":"rust releases"}}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, target, output: Some(output), .. }
                if name == "ToolSearch" && target == "calendar tools" && output.contains("calendar.list")
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, target, .. }
                if name == "WebSearch" && target == "rust releases"
        )));
    }

    #[test]
    fn image_generation_item_keeps_its_prompt_and_defers_the_image() {
        let jsonl = r#"{"type":"response_item","payload":{"type":"image_generation_call","id":"image-1","revised_prompt":"a tiny crab","result":"iVBORw0KGgo="}}"#;

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, target, output: None, .. }
                if name == "ImageGeneration" && target == "a tiny crab"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::Attachment(attachment)
                if attachment.kind == AttachmentKind::Image
                    && attachment.name == "image.png"
                    && matches!(attachment.content, AttachmentContent::Deferred { .. })
        )));
        assert_eq!(
            nth_loaded_attachment(jsonl, 0),
            Some(LoadedAttachment::Base64 {
                mime: "image/png".to_string(),
                b64: "iVBORw0KGgo=".to_string(),
            })
        );
    }

    #[test]
    fn plain_text_spawn_output_preserves_resolved_subagent() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child-thread","agent_path":"/root/review","kind":"started"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"spawned agent child-thread"}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::SubAgent(agent)
                if agent.agent_id == "child-thread"
                    && agent.tool_use_id == "spawn-1"
                    && agent.description == "review"
        )));
        assert!(!blocks
            .iter()
            .any(|block| matches!(block, Block::ToolUse { name, .. } if name == "spawn_agent")));
    }

    #[test]
    fn failed_spawn_is_not_a_navigable_subagent() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"collab spawn failed: agent thread limit reached"}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(
            !blocks
                .iter()
                .any(|block| matches!(block, Block::SubAgent(_))),
            "a failed spawn created no child rollout to navigate"
        );
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse {
                name,
                target,
                output: Some(output),
                ..
            } if name == "spawn_agent"
                && target == "review"
                && output == "collab spawn failed: agent thread limit reached"
        )));
    }

    #[test]
    fn legacy_spawn_output_uses_agent_id() {
        let jsonl = concat!(
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"agent_id\":\"legacy-child\",\"nickname\":\"Nash\"}"}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::SubAgent(agent) if agent.agent_id == "legacy-child"
        )));
    }

    #[test]
    fn parses_canonical_response_items_without_event_duplicates() {
        let jsonl = r#"
{"timestamp":"2026-07-18T01:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/tmp/repo","originator":"codex-tui"}}
{"timestamp":"2026-07-18T01:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix it"},{"type":"input_text","text":"<environment_context>hidden</environment_context>"}]}}
{"timestamp":"2026-07-18T01:00:01Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"developer secret"}]}}
{"timestamp":"2026-07-18T01:00:02Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Inspect parser"}],"encrypted_content":"opaque"}}
{"timestamp":"2026-07-18T01:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"ok"}}
{"timestamp":"2026-07-18T01:00:04Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-1","arguments":"{\"cmd\":\"cargo test\"}"}}
not json
{"timestamp":"2026-07-18T01:00:05Z","type":"event_msg","payload":{"type":"agent_message","message":"Done"}}
{"timestamp":"2026-07-18T01:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","phase":"final","content":[{"type":"output_text","text":"Done"}]}}
"#;
        let blocks = parse_codex(jsonl);
        assert!(matches!(&blocks[0], Block::UserText(text) if text == "Fix it"));
        assert!(!blocks
            .iter()
            .any(|block| matches!(block, Block::UserText(text) if text.contains("developer"))));
        assert!(!blocks.iter().any(
            |block| matches!(block, Block::UserText(text) if text.contains("environment_context"))
        ));
        assert!(blocks.iter().any(
            |block| matches!(block, Block::Thinking { text, .. } if text == "Inspect parser")
        ));
        // `call-1`'s output precedes its `function_call` — a synthetic reversed pair.
        // Forward-references do not occur in real transcripts (0/209 scanned), so the
        // single-pass fold renders the not-yet-joined output as an inline orphan and the
        // Bash tool_use stays result-less.
        assert!(blocks
            .iter()
            .any(|block| matches!(block, Block::ToolResult(text) if text == "ok")));
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, output: None, .. } if name == "Bash"
        )));
        assert_eq!(
            blocks
                .iter()
                .filter(|block| matches!(block, Block::AssistantMessage { text, phase: AssistantPhase::Final } if text == "Done"))
                .count(),
            1
        );
    }

    #[test]
    fn assistant_message_phase_is_canonical_structure() {
        let blocks = parse_codex(
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","phase":"commentary","content":[{"type":"output_text","text":"Working"}]}}
{"type":"response_item","payload":{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Done"}]}}"#,
        );
        assert!(matches!(
            &blocks[0],
            Block::AssistantMessage { text, phase: AssistantPhase::Commentary } if text == "Working"
        ));
        assert!(matches!(
            &blocks[1],
            Block::AssistantMessage { text, phase: AssistantPhase::Final } if text == "Done"
        ));
    }

    #[test]
    fn ignored_event_messages_do_not_zero_reasoning_duration() {
        let jsonl = concat!(
            r#"{"timestamp":"2026-07-18T01:00:00.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix it"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:00.000Z","type":"event_msg","payload":{"type":"user_message","message":"Fix it"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:05.000Z","type":"event_msg","payload":{"type":"agent_reasoning","text":"Inspect parser"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:05.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Inspect parser"}]}}"#,
            "\n",
        );

        let blocks = parse_codex(jsonl);
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::Thinking {
                text,
                duration_secs: Some(5),
                ..
            } if text == "Inspect parser"
        )));
    }

    #[test]
    fn child_rollout_hides_cloned_parent_history_and_counts_only_child_usage() {
        let jsonl = concat!(
            r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"session_meta","payload":{"id":"child-thread","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-thread","depth":1,"agent_path":"/root/review"}}}}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"session_meta","payload":{"id":"parent-thread","cwd":"/repo","source":"cli"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.514Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"parent turn copied into child"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.515Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":20}}}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.645Z","type":"event_msg","payload":{"type":"thread_settings_applied"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.660Z","type":"event_msg","payload":{"type":"task_started","turn_id":"child-turn","started_at":1786315684}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:05.000Z","type":"response_item","payload":{"type":"agent_message","author":"/root","recipient":"/root/review","content":[{"type":"input_text","text":"Message Type: NEW_TASK\nTask name: /root/review\nSender: /root\nPayload:\nreview current change"},{"type":"encrypted_content","encrypted_content":"opaque"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:06.000Z","type":"turn_context","payload":{"model":"gpt-5.6"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:07.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":0,"output_tokens":5}}}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:08.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"child answer"}]}}"#,
            "\n",
        );
        let path = std::env::temp_dir().join(format!(
            "codex-child-bootstrap-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(&path, jsonl).unwrap();

        let (blocks, _, metrics) =
            parse_path_timed_for(&crate::adapters::CodexAdapter, &path).unwrap();
        std::fs::remove_file(path).ok();

        let turns = blocks
            .iter()
            .filter_map(|block| match block {
                Block::UserText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            turns,
            ["Message Type: NEW_TASK\nTask name: /root/review\nSender: /root\nPayload:\nreview current change"]
        );
        assert!(blocks
            .iter()
            .any(|block| matches!(block, Block::AssistantText(text) if text == "child answer")));
        assert_eq!(metrics.input_tokens, 20);
        assert_eq!(metrics.output_tokens, 5);
    }

    #[test]
    fn parse_path_matches_string_and_extracts_apply_patch_diff() {
        let jsonl = r#"{"type":"session_meta","payload":{"id":"s1","cwd":"/tmp/repo","originator":"codex-tui"}}
{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","call_id":"patch-1","input":"*** Begin Patch\n*** Update File: /tmp/repo/src/lib.rs\n@@\n-old\n+new\n*** End Patch"}}
{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"patch-1","output":"Done!"}}
"#;
        let expected = parse_codex(jsonl);
        let path = std::env::temp_dir().join(format!("codex-model-{}.jsonl", std::process::id()));
        std::fs::write(&path, jsonl).unwrap();
        // Through the public dispatcher (the adapter's default `parse_path_timed`).
        let (actual, _, _) =
            claude_replay_engine::seam::parse_path_timed_for(&crate::adapters::CodexAdapter, &path)
                .unwrap();
        std::fs::remove_file(path).ok();
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        assert!(matches!(
            &actual[0],
            Block::ToolUse { name, target, diffs, .. }
                if name == "Edit" && target == "src/lib.rs"
                    && diffs == &[("old".into(), "new".into())]
        ));
    }

    /// The explicit bit-identical gate for the shared L2: `replay(tokenize(x), &CODEX_SHAPING)`
    /// must equal `parse_lines(x)` — same blocks AND same `user_times` — across the Codex
    /// cases (out-of-order result, host-context / developer suppression, reasoning,
    /// apply_patch, orphan output, assistant text, junk lines).
    #[test]
    fn codex_replay_matches_parse_lines() {
        fn equiv(jsonl: &str) {
            let mut ut_lines = Vec::new();
            let via_lines = parse_lines(jsonl.lines(), &mut ut_lines);
            let mut ut_replay = Vec::new();
            let via_replay = claude_replay_engine::seam::replay(
                &tokenize(jsonl.lines()),
                &mut ut_replay,
                &CODEX_SHAPING,
            );
            assert_eq!(
                format!("{via_lines:?}"),
                format!("{via_replay:?}"),
                "blocks differ for:\n{jsonl}"
            );
            assert_eq!(ut_lines, ut_replay, "user_times differ for:\n{jsonl}");
        }
        // Canonical: session_meta cwd, user (host-context + developer filtered), reasoning,
        // an output that arrives BEFORE its call, assistant final text, and junk lines.
        equiv(concat!(
            r#"{"timestamp":"2026-07-18T01:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/tmp/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Fix it"},{"type":"input_text","text":"<environment_context>hidden</environment_context>"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:01Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"secret"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:02Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Inspect"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-1","output":"ok"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-18T01:00:04Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","call_id":"call-1","arguments":"{\"cmd\":\"cargo test\"}"}}"#,
            "\n",
            "not json\n",
            r#"{"timestamp":"2026-07-18T01:00:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}"#,
            "\n",
        ));
        // apply_patch custom_tool_call + its output.
        equiv(concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/tmp/repo"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","call_id":"p1","input":"*** Begin Patch\n*** Update File: /tmp/repo/src/lib.rs\n@@\n-old\n+new\n*** End Patch"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"p1","output":"Done!"}}"#,
            "\n",
        ));
        // Orphan output (no matching call → kept) sandwiched between plain turns.
        equiv(concat!(
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"nope","output":"orphaned"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"bye"}]}}"#,
            "\n",
        ));
    }
}
