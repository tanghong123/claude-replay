use claude_replay_engine::seam::{
    epoch_secs, parse_path_timed_for, relativize, AgentStatus, Attachment, AttachmentContent,
    AttachmentKind, Block, LinePreprocessor, LoadedAttachment, Message, Metrics, PreprocessedLine,
    Shaping, SubAgent, TaskOp, Todo, UsdCost,
};
#[cfg(test)]
use claude_replay_engine::seam::{replay, stamp_user_turns, BlockIndex, EpochSeconds};
use serde_json::Value;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const AGENT_PATH_KEY_PREFIX: &str = "codex-agent-";
const SUBAGENT_THREAD_RESULT_PREFIX: &str = "\0codex-subagent-thread:";

#[derive(Default)]
pub(crate) struct CodexLinePreprocessor {
    child: Option<ChildRollout>,
}

struct ChildRollout {
    session_started_at: f64,
    agent_path: String,
    skipping_parent_snapshot: bool,
}

impl LinePreprocessor for CodexLinePreprocessor {
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
                return PreprocessedLine::Include;
            }
            // A second session_meta in a child rollout starts a physical copy of the parent's
            // transcript. It is bootstrap context, not authored child history.
            if let Some(child) = self.child.as_mut() {
                child.skipping_parent_snapshot = true;
                return PreprocessedLine::Ignore;
            }
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
    blocks // identity — Codex does no turn grouping
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
    Block::ToolUse {
        name,
        target,
        diffs,
        output: None,
        patch: None,
        read_lines: None,
    }
}

/// Codex's L2 shaping: bare output back-patch, keep all orphans, no grouping.
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
            if cwd.is_empty() {
                *cwd = value
                    .pointer("/payload/cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
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
                                        msgs.push(Message::AssistantText(text.to_string()));
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
            if payload.get("type").and_then(Value::as_str) == Some("sub_agent_activity") {
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
        }
        _ => {}
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

fn input_image_attachment(item: &Value) -> Option<Attachment> {
    let url = item.get("image_url").and_then(Value::as_str)?;
    if let Some((mime, _)) = data_image(url) {
        return Some(deferred_image(mime));
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
        content: AttachmentContent::Deferred { at: 0, index: 0 },
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
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("session_meta" | "response_item")
    )
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
                if cwd.is_empty() {
                    cwd = value
                        .pointer("/payload/cwd")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
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
        } else {
            out.push(Block::AssistantText(text.to_string()));
        }
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
    for key in ["cmd", "command", "query", "pattern", "description"] {
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

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_engine::seam::Block;

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
                    && attachment.content == AttachmentContent::Deferred { at: 0, index: 0 }
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
                .filter(|block| matches!(block, Block::AssistantText(text) if text == "Done"))
                .count(),
            1
        );
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
