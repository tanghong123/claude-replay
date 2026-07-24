use crate::model::Block;
use crate::Args;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead};
use std::path::Path;

pub(crate) fn parse_codex(jsonl: &str, args: &Args) -> Vec<Block> {
    let call_ids = scan_call_ids(jsonl.lines());
    parse_lines(jsonl.lines(), &call_ids, args, &mut Vec::new())
}

pub(crate) fn parse_codex_path(path: &Path, args: &Args) -> io::Result<Vec<Block>> {
    let open = || -> io::Result<_> { Ok(std::io::BufReader::new(std::fs::File::open(path)?)) };
    let call_ids = scan_call_ids(open()?.lines().map_while(|line| line.ok()));
    Ok(parse_lines(
        open()?.lines().map_while(|line| line.ok()),
        &call_ids,
        args,
        &mut Vec::new(),
    ))
}

/// `parse_codex_path` + one timestamp per user turn (see `model::parse_main`).
pub(crate) fn parse_codex_path_timed(
    path: &Path,
    args: &Args,
    user_times: &mut Vec<Option<f64>>,
) -> io::Result<Vec<Block>> {
    let open = || -> io::Result<_> { Ok(std::io::BufReader::new(std::fs::File::open(path)?)) };
    let call_ids = scan_call_ids(open()?.lines().map_while(|line| line.ok()));
    Ok(parse_lines(
        open()?.lines().map_while(|line| line.ok()),
        &call_ids,
        args,
        user_times,
    ))
}

fn scan_call_ids<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.as_ref()) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let kind = value.pointer("/payload/type").and_then(Value::as_str);
        if matches!(kind, Some("function_call" | "custom_tool_call")) {
            if let Some(id) = value.pointer("/payload/call_id").and_then(Value::as_str) {
                out.insert(id.to_string());
            }
        }
    }
    out
}

fn parse_lines<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    call_ids: &HashSet<String>,
    _args: &Args,
    user_times: &mut Vec<Option<f64>>,
) -> Vec<Block> {
    let mut out = Vec::new();
    // See `model::parse_main`: stamp the previous event's user turns on the next
    // iteration so an early `continue` can't drop them.
    let mut pending_ts: Option<f64> = None;
    let mut stamped = 0usize;
    let mut slots: HashMap<String, usize> = HashMap::new();
    let mut pending: HashMap<String, String> = HashMap::new();
    let mut cwd = String::new();
    let mut trigger_ts = None;

    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(line.as_ref()) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(epoch_secs);
        crate::model::stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
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
                        if payload.get("role").and_then(Value::as_str) == Some("user") {
                            if let Some(ts) = timestamp {
                                trigger_ts = Some(ts);
                            }
                        }
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
                            let duration_secs = match (timestamp, trigger_ts) {
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
                            if let Some(output) = pending.remove(call_id) {
                                apply_output(&mut out[index], output);
                            }
                        }
                    }
                    Some("function_call_output" | "custom_tool_call_output") => {
                        if let Some(ts) = timestamp {
                            trigger_ts = Some(ts);
                        }
                        let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                        let output = output_text(payload.get("output").unwrap_or(&Value::Null));
                        if let Some(index) = slots.get(call_id).copied() {
                            apply_output(&mut out[index], output);
                        } else if call_ids.contains(call_id) {
                            pending.insert(call_id.to_string(), output);
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
    crate::model::stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
    out
}

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

fn relativize(path: &str, cwd: &str) -> String {
    let path = Path::new(path);
    if !cwd.is_empty() {
        if let Ok(relative) = path.strip_prefix(cwd) {
            return relative.display().to_string();
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        if let Ok(relative) = path.strip_prefix(home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

fn epoch_secs(timestamp: &str) -> Option<f64> {
    let (date, time) = timestamp.split_once('T')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut time = time.trim_end_matches('Z').split(':');
    let hour: f64 = time.next()?.parse().ok()?;
    let minute: f64 = time.next()?.parse().ok()?;
    let second: f64 = time.next()?.parse().ok()?;
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = (if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    }) / 400;
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    Some(days as f64 * 86400.0 + hour * 3600.0 + minute * 60.0 + second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Block;
    use crate::Args;

    fn args() -> Args {
        Args {
            target: None,
            agent: None,
            latest: false,
            follow: false,
            no_thinking: false,
            reads: false,
            results: false,
            no_user: false,
            full: false,
            fold: None,
            unfold: None,
            read_match: None,
            dump: None,
            dump_html: None,
            width: None,
        }
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
        let blocks = parse_codex(jsonl, &args());
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
        assert!(blocks.iter().any(|block| matches!(
            block,
            Block::ToolUse { name, output: Some(output), .. }
                if name == "Bash" && output == "ok"
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
        let expected = parse_codex(jsonl, &args());
        let path = std::env::temp_dir().join(format!("codex-model-{}.jsonl", std::process::id()));
        std::fs::write(&path, jsonl).unwrap();
        let actual = parse_codex_path(&path, &args()).unwrap();
        std::fs::remove_file(path).ok();
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        assert!(matches!(
            &actual[0],
            Block::ToolUse { name, target, diffs, .. }
                if name == "Edit" && target == "src/lib.rs"
                    && diffs == &[("old".into(), "new".into())]
        ));
    }
}
