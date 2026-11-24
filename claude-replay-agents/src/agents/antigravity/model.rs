//! Decode the plaintext trajectory log produced by Antigravity CLI.

use claude_replay_engine::seam::{
    epoch_secs, note_unknown, unknown_watching_keys, Block, Elision, LinePreprocessor, Message,
    NoteKind, PreprocessedLine, Shaping, ToolExecution, ToolStatus, UnknownAt,
};
use serde_json::Value;

pub(crate) const ELISION: Elision = Elision::Keys(&[&["media", "data"]]);

pub(crate) fn is_step(value: &Value) -> bool {
    value["step_index"].is_u64()
        && value["created_at"].is_string()
        && value["source"].is_string()
        && value["type"].is_string()
        && value["status"].is_string()
}

// The JSONL projection has carried these keys. Optional fields can be absent.
const STEP_KEYS: &[&str] = &[
    "step_index",
    "created_at",
    "source",
    "type",
    "status",
    "content",
    "error",
    "thinking",
    "tool_calls",
    "model",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "media",
];

fn note_shape(value: &Value) {
    // The line preprocessor has no transcript path. AGY steps carry neither a
    // trajectory ID nor a client version, so the report has no clickable example.
    let note = |at, name: &str| note_unknown("antigravity", at, name, None, None);
    if !is_step(value) {
        note(UnknownAt::RecordType, "invalid step envelope");
        return;
    }
    let kind = value["type"].as_str().unwrap_or_default();
    let kind = &kind[..kind.char_indices().nth(120).map_or(kind.len(), |(i, _)| i)];
    match kind {
        "GENERIC" if value["source"] != "MODEL" => {
            note(UnknownAt::RecordType, "GENERIC with unexpected source");
        }
        "USER_INPUT" | "PLANNER_RESPONSE" | "GENERIC" | "ERROR_MESSAGE" | "SYSTEM_MESSAGE" => {}
        other => note(UnknownAt::RecordType, other),
    }
    if !unknown_watching_keys() {
        return;
    }
    if let Some(fields) = value.as_object() {
        for key in fields
            .keys()
            .filter(|key| !STEP_KEYS.contains(&key.as_str()))
        {
            note(UnknownAt::RecordKey, &format!("{kind}.{key}"));
        }
    }
    for (key, is_expected) in [
        ("model", Value::is_string as fn(&Value) -> bool),
        ("tool_calls", Value::is_array),
        ("input_tokens", |v: &Value| v.is_u64()),
        ("output_tokens", |v: &Value| v.is_u64()),
        ("cache_read_tokens", |v: &Value| v.is_u64()),
        ("cache_write_tokens", |v: &Value| v.is_u64()),
    ] {
        if value.get(key).is_some_and(|field| !is_expected(field)) {
            note(
                UnknownAt::RecordKey,
                &format!("{kind}.{key} (unexpected type)"),
            );
        }
    }
}

pub(super) fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|s| !s.trim().is_empty())
}

pub(super) fn user_text(value: &Value) -> Option<&str> {
    if value["type"] != "USER_INPUT" {
        return None;
    }
    let text = field(value, "content")?;
    let prompt = text
        .trim_start()
        .strip_prefix("<USER_REQUEST>")
        .and_then(|body| body.split_once("</USER_REQUEST>"))
        .filter(|(_, rest)| {
            rest.trim().is_empty() || rest.trim_start().starts_with("<ADDITIONAL_METADATA>")
        })
        .map_or(text, |(prompt, _)| prompt);
    let prompt = prompt.trim();
    (!prompt.is_empty()).then_some(prompt)
}

fn tool_id(step: u64, ordinal: usize) -> String {
    format!("agy-{step}-{ordinal}")
}

fn calls(value: &Value) -> impl Iterator<Item = (usize, &Value)> {
    value["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, call)| field(call, "name").is_some())
}

fn timestamp(value: &Value) -> Option<f64> {
    field(value, "created_at").and_then(epoch_secs)
}

pub(crate) fn decode_line(line: &str, out: &mut Vec<Message>) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if !is_step(&value) {
        return;
    }
    let ts = timestamp(&value);
    out.push(Message::LineStart(ts));
    match value["type"].as_str() {
        Some("USER_INPUT") => {
            if let Some(text) = user_text(&value) {
                out.push(Message::UserText { text: text.into() });
            }
        }
        Some("PLANNER_RESPONSE") => {
            if let Some(text) = field(&value, "thinking") {
                out.push(Message::Thinking {
                    text: text.into(),
                    ts,
                });
            }
            if let Some(text) = field(&value, "content") {
                out.push(Message::AssistantText(text.into()));
            }
            let step = value["step_index"].as_u64().unwrap_or_default();
            for (ordinal, call) in calls(&value) {
                out.push(Message::ToolUse {
                    id: tool_id(step, ordinal),
                    name: call["name"].as_str().unwrap_or_default().into(),
                    input: call["args"].clone(),
                    cwd: String::new(),
                });
            }
        }
        Some("ERROR_MESSAGE") => {
            if let Some(text) = field(&value, "error").or_else(|| field(&value, "content")) {
                out.push(Message::SystemNote {
                    text: text.into(),
                    kind: NoteKind::Failure,
                });
            }
        }
        _ => {}
    }
}

/// In the CLI's JSONL projection, each result occupies the step immediately after its
/// planner step plus the call ordinal. Native call IDs are absent. Use the step position
/// so an omitted log row cannot shift every subsequent result to the previous call.
#[derive(Default)]
pub(crate) struct ResultLinker {
    batch: Option<(u64, Vec<usize>)>,
}

impl LinePreprocessor for ResultLinker {
    fn process(&mut self, line: &str) -> PreprocessedLine {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return PreprocessedLine::Include;
        };
        if unknown_watching_keys() {
            note_shape(&value);
        }
        if !is_step(&value) {
            return PreprocessedLine::Include;
        }
        let step = value["step_index"].as_u64().unwrap_or_default();
        match value["type"].as_str() {
            Some("PLANNER_RESPONSE") => {
                let ordinals: Vec<_> = calls(&value).map(|(ordinal, _)| ordinal).collect();
                self.batch = (!ordinals.is_empty()).then_some((step, ordinals));
            }
            Some("USER_INPUT" | "ERROR_MESSAGE") => self.batch = None,
            Some("GENERIC") if value["source"] == "MODEL" => {
                let ts = timestamp(&value);
                let text = field(&value, "content").or_else(|| field(&value, "error"));
                if let Some((planner_step, ordinals)) = &mut self.batch {
                    let ordinal = step
                        .checked_sub(*planner_step)
                        .and_then(|distance| distance.checked_sub(1))
                        .and_then(|index| usize::try_from(index).ok());
                    if let Some((ordinal, position)) =
                        ordinal.and_then(|i| ordinals.binary_search(&i).ok().map(|p| (i, p)))
                    {
                        ordinals.drain(..=position);
                        return PreprocessedLine::Messages(vec![
                            Message::LineStart(ts),
                            Message::ToolResult {
                                tool_use_id: tool_id(*planner_step, ordinal),
                                text: text.unwrap_or_default().into(),
                                tur: value["status"].clone(),
                                is_error: None,
                            },
                        ]);
                    }
                    if !ordinals.is_empty() && unknown_watching_keys() {
                        note_unknown(
                            "antigravity",
                            UnknownAt::RecordType,
                            "GENERIC outside expected tool result positions",
                            None,
                            None,
                        );
                    }
                }
                if let Some(text) = text {
                    return PreprocessedLine::Messages(vec![
                        Message::LineStart(ts),
                        Message::SystemNote {
                            text: text.into(),
                            kind: NoteKind::Plain,
                        },
                    ]);
                }
            }
            _ => {}
        }
        PreprocessedLine::Include
    }
}

fn build_tool(_id: &str, raw_name: &str, input: &Value, cwd: &str) -> Block {
    let (name, key) = match raw_name {
        "run_command" => ("Bash", "CommandLine"),
        "read_file" => ("Read", "Path"),
        "write_file" => ("Write", "Path"),
        "edit_file" => ("Edit", "Path"),
        "list_directory" => ("LS", "Path"),
        "grep_search" => ("Grep", "Query"),
        "find_by_name" => ("Glob", "Pattern"),
        _ => (raw_name, "toolSummary"),
    };
    let target = if key == "toolSummary" {
        None
    } else {
        field(input, key).map(str::to_owned)
    };
    let target = target.unwrap_or_else(|| {
        field(input, "toolSummary")
            .unwrap_or_default()
            .chars()
            .take(240)
            .collect()
    });
    // Like the Claude adapter, show the change carried by the invocation.
    // Preserve empty strings (deletion and empty file creation) and whitespace.
    let text = |key| input[key].as_str();
    let diff = match raw_name {
        "write_file" => text("Content").map(|content| ("", content)),
        "edit_file" => text("Target").zip(text("Replacement")),
        _ => None,
    };
    Block::ToolUse {
        name: name.into(),
        target,
        diffs: diff
            .into_iter()
            .map(|(old, new)| (old.into(), new.into()))
            .collect(),
        output: None,
        patch: None,
        read_lines: None,
        cwd: cwd.into(),
        execution: None,
        published: None,
        asked: None,
        delivered: Vec::new(),
        sent: None,
        sent_to: None,
        saved_output: None,
    }
}

fn join_result(block: &mut Block, text: &str, status: &Value, _is_error: Option<bool>) {
    let Block::ToolUse {
        output, execution, ..
    } = block
    else {
        return;
    };
    *output = (!text.is_empty()).then(|| text.into());
    let status = match status.as_str() {
        Some("DONE") => ToolStatus::Completed,
        Some("ERROR") => ToolStatus::Failed,
        Some("CANCELED" | "CANCELLED") => ToolStatus::Cancelled,
        _ => return,
    };
    *execution = Some(ToolExecution {
        status: Some(status),
        exit_code: None,
        duration: None,
        outside_sandbox: false,
    });
}

pub(crate) const SHAPING: Shaping = Shaping {
    build_tool,
    join_result,
    keep_orphan: |_| true,
    finish_turns: claude_replay_engine::seam::coalesce_spans,
};
