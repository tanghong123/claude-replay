use crate::engine::seam::{epoch_secs, relativize, Block, Message};
use serde_json::Value;
#[cfg(test)]
use std::collections::HashMap;

#[cfg(test)]
fn parse_codex(jsonl: &str) -> Vec<Block> {
    // In-memory batch entry on the shared engine (L1 `tokenize` → L2 `replay`). The
    // streaming path (the shared `SessionAccumulator`) also runs on the engine now, per line
    // via `decode_line` + `Replayer` (M9).
    crate::engine::seam::replay(&tokenize(jsonl.lines()), &mut Vec::new(), &CODEX_SHAPING)
}

/// Codex's back-patch is simpler than Claude's — no `toolUseResult` metadata, and the
/// output is skipped for Edit/Write. Shim it into `Shaping::apply`'s `(&mut Block, &str,
/// &Value)` signature (the `Value` is always Null for Codex).
fn apply_output_shaping(block: &mut Block, text: &str, _tur: &Value) {
    apply_output(block, text.to_string());
}
fn codex_keep_orphan(_t: &str) -> bool {
    true // Codex keeps every non-empty orphan output (no boilerplate filter)
}
fn codex_finish(blocks: Vec<Block>) -> Vec<Block> {
    blocks // identity — Codex does no turn grouping
}

/// Codex's `build_tool`: normalize the tool name and shape the target/diffs via
/// `call_details` (Codex has no `SubAgent` spawns, so `id` is unused). The raw `input` was
/// already extracted by `call_input` in the tokenizer. (Lifted to L2 in M14.)
fn codex_build_tool(_id: &str, raw_name: &str, input: &Value, cwd: &str) -> Block {
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
pub(crate) const CODEX_SHAPING: crate::engine::seam::Shaping = crate::engine::seam::Shaping {
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
    msgs.push(Message::LineStart(ts));
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
                                // Codex user input is always a genuine human turn — it maps
                                // straight to the shared `UserText` (no injected/skill/command
                                // notions to classify, unlike Claude's L1).
                                msgs.push(Message::UserText {
                                    text: text.to_string(),
                                });
                            } else {
                                msgs.push(Message::AssistantText(text.to_string()));
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
                    msgs.push(Message::ToolUse {
                        id: call_id.to_string(),
                        name: raw_name.to_string(),
                        input: call_input(payload),
                        cwd: cwd.to_string(),
                    });
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
                _ => {}
            }
        }
        _ => {}
    }
}

/// **Frozen golden reference** (M9): production parses Codex through the streaming engine;
/// this pre-engine parser is retained only to pin the shared `replay` bit-identical in
/// `codex_replay_matches_parse_lines`.
#[cfg(test)]
fn parse_lines<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    user_times: &mut Vec<Option<crate::engine::seam::EpochSeconds>>,
) -> Vec<Block> {
    let mut out = Vec::new();
    // See `model::parse_main`: stamp the previous event's user turns on the next
    // iteration so an early `continue` can't drop them.
    let mut pending_ts: Option<crate::engine::seam::EpochSeconds> = None;
    let mut stamped = 0usize;
    let mut slots: HashMap<String, crate::engine::seam::BlockIndex> = HashMap::new();
    let mut cwd = String::new();
    // The previous line's ts — CC's thinking clock (#57): a thinking's duration is
    // `its ts − this` (mirrors the engine's `prev_ts`).
    let mut prev_ts = None;

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.as_ref()) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_secs);
        crate::engine::seam::stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
        if pending_ts.is_some() {
            prev_ts = pending_ts;
        }
        pending_ts = timestamp;
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
    crate::engine::seam::stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
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
    if let Block::ToolUse {
        name, output: slot, ..
    } = block
    {
        if !matches!(name.as_str(), "Edit" | "Write") && !output.trim().is_empty() {
            *slot = Some(output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::seam::Block;

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
            crate::engine::seam::parse_path_timed_for(crate::engine::seam::Agent::CODEX, &path)
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
            let via_replay = crate::engine::seam::replay(
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
