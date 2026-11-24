//! Decode the plaintext trajectory log produced by Antigravity CLI.

use claude_replay_engine::seam::{
    epoch_secs, Block, Elision, LinePreprocessor, LineSource, Message, NoteKind, PreprocessedLine,
    Shaping, ToolExecution, ToolStatus, TornTail,
};
use serde_json::Value;
use std::collections::VecDeque;
use std::io::{self, BufRead};

pub(super) const ELISION: Elision = Elision::Keys(&[&["media", "data"]]);

pub(crate) fn is_step(value: &Value) -> bool {
    value["step_index"].is_u64()
        && value["created_at"].is_string()
        && value["source"].is_string()
        && value["type"].is_string()
        && value["status"].is_string()
}

pub(crate) fn read_snapshot(reader: &mut dyn BufRead) -> io::Result<Vec<String>> {
    let mut lines = LineSource::new(reader, 0, TornTail::Yield, ELISION);
    let mut records = Vec::new();
    while let Some((_, line)) = lines.next()? {
        records.push(line.to_owned());
    }
    Ok(records)
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

/// The CLI emits the outcomes of a planner batch as subsequent GENERIC steps in call order.
/// There are no call IDs in the log. A new planner response or user prompt ends the batch.
#[derive(Default)]
pub(crate) struct ResultLinker {
    pending: VecDeque<String>,
}

impl LinePreprocessor for ResultLinker {
    fn process(&mut self, line: &str) -> PreprocessedLine {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return PreprocessedLine::Include;
        };
        let Some(step) = value["step_index"].as_u64() else {
            return PreprocessedLine::Include;
        };
        match value["type"].as_str() {
            Some("PLANNER_RESPONSE") => {
                self.pending = calls(&value)
                    .map(|(ordinal, _)| tool_id(step, ordinal))
                    .collect();
            }
            Some("USER_INPUT" | "ERROR_MESSAGE") => self.pending.clear(),
            Some("GENERIC") if value["source"] == "MODEL" => {
                let ts = timestamp(&value);
                let text = field(&value, "content").or_else(|| field(&value, "error"));
                if let Some(id) = self.pending.pop_front() {
                    return PreprocessedLine::Messages(vec![
                        Message::LineStart(ts),
                        Message::ToolResult {
                            tool_use_id: id,
                            text: text.unwrap_or_default().into(),
                            tur: value["status"].clone(),
                            is_error: None,
                        },
                    ]);
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
    let target = field(input, key).or_else(|| field(input, "toolSummary"));
    Block::ToolUse {
        name: name.into(),
        target: target.map(str::to_owned).unwrap_or_else(|| {
            if input.is_null() {
                String::new()
            } else {
                input.to_string()
            }
        }),
        diffs: Vec::new(),
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
    finish_turns: |blocks| blocks,
};
