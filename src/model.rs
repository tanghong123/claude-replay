//! Parse a Claude Code transcript JSONL into a flat list of render blocks.
//! Nothing is dropped or truncated — every event becomes a block with its full
//! content; what's shown collapsed is a fold-policy decision made in `view`.
//! One JSONL line can yield several blocks.

use crate::{Args, Backend};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One hunk of a Claude Code `structuredPatch` — gives the real file line
/// numbers so an Edit diff can number its rows correctly.
#[derive(Debug, Clone)]
pub struct Hunk {
    /// 1-based line number of this hunk's first line on the OLD side.
    pub old_start: usize,
    /// 1-based line number of this hunk's first line on the NEW side.
    pub new_start: usize,
    /// Patch lines; each begins with ' ' (context), '+' (added), or '-' (removed).
    pub lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Block {
    /// A human turn (a `user` event whose content is a plain string).
    UserText(String),
    /// Assistant prose (markdown).
    AssistantText(String),
    /// A ✻ thinking block, grouped as a "turn" like Claude Code: the thinking text,
    /// the wall-clock seconds it took (floored, from transcript timestamps — `None`
    /// if not derivable), and the tool calls that ran just before it (whose results
    /// it processed). Collapsed → `<activities>, thought for Xs` (natural order —
    /// tools ran first); expanded → the tools followed by the thinking.
    Thinking {
        text: String,
        duration_secs: Option<u64>,
        tools: Vec<Block>,
    },
    /// A tool invocation: name + a short target (file/command/…), with its result
    /// joined in from the matching `tool_result`'s `toolUseResult` metadata.
    ToolUse {
        name: String,
        target: String,
        /// For Edit/Write/MultiEdit/NotebookEdit: (old, new) pairs to diff
        /// (fallback when `patch` is absent).
        diffs: Vec<(String, String)>,
        /// Tool output to show under the call (Bash stdout/stderr, Read content,
        /// generic result text). Edit/Write boilerplate is stripped → `None`.
        output: Option<String>,
        /// Edit/MultiEdit `structuredPatch` (real file line numbers), if present.
        patch: Option<Vec<Hunk>>,
        /// Read line count (from `toolUseResult.file.numLines`), if present.
        read_lines: Option<usize>,
    },
    /// A tool result with no matching tool_use (rare).
    ToolResult(String),
    /// A slash command (e.g. `/compact`) and its local stdout. Rendered like
    /// Claude Code's `❯ /command` header + dim `⎿ output` lines, folded by
    /// default. Parsed from the `<command-name>`/`<command-args>`/
    /// `<local-command-stdout>` wrappers Claude Code injects as user messages.
    Command {
        /// The command, e.g. `/compact`.
        name: String,
        /// Command arguments (may be empty).
        args: String,
        /// `local-command-stdout` chunks shown beneath the header (may be empty).
        output: Vec<String>,
    },
}

/// The fold-policy category for a block. One key per block; `--fold`/`--unfold`
/// and the default fold policy are keyed on these (see `view`).
pub fn fold_key(b: &Block) -> &'static str {
    match b {
        Block::UserText(_) => "user",
        Block::AssistantText(_) => "assistant",
        Block::Thinking { .. } => "thinking",
        Block::ToolResult(_) => "tool_result",
        Block::Command { .. } => "command",
        Block::ToolUse { name, .. } => tool_fold_key(name),
    }
}

/// Inner text of the first `<tag>…</tag>` in `s`, if present.
fn tag_inner<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let rest = &s[start..];
    let end = rest.find(&close)?;
    Some(&rest[..end])
}

/// Remove every `<local-command-caveat>…</local-command-caveat>` block (pure
/// noise Claude Code injects around local commands), returning the remainder.
fn strip_caveat(s: &str) -> String {
    let (open, close) = ("<local-command-caveat>", "</local-command-caveat>");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        match rest[i + open.len()..].find(close) {
            Some(j) => rest = &rest[i + open.len() + j + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The harness injects a loaded skill's instruction body as a user message that
/// opens with this marker. It's reference material, not user prose, so we model it
/// as a foldable (default-collapsed) result block instead of a `❯` user turn.
fn is_skill_body(s: &str) -> bool {
    s.trim_start().starts_with("Base directory for this skill:")
}

/// Turn one plain-string `user` message into block(s). A slash-command
/// invocation (`<command-name>`) and its `<local-command-stdout>` become a
/// `Block::Command`; the `<local-command-caveat>` noise is dropped; everything
/// else is ordinary `UserText`.
fn push_user_string(s: &str, out: &mut Vec<Block>) {
    // A background-execution notification (`<task-notification>…`): collapse the raw
    // XML to its one-line `<summary>` (else `<status>`), as a foldable result block.
    if tag_inner(s, "task-notification").is_some() {
        if let Some(line) = tag_inner(s, "summary").or_else(|| tag_inner(s, "status")) {
            let line = line.trim();
            if !line.is_empty() {
                out.push(Block::ToolResult(line.to_string()));
                return;
            }
        }
    }
    // A slash command: `<command-name>/foo</command-name>` (+ optional args /
    // inline stdout). The caveat, if bundled in the same message, is ignored.
    if let Some(name) = tag_inner(s, "command-name") {
        let args = tag_inner(s, "command-args")
            .unwrap_or("")
            .trim()
            .to_string();
        let mut output = Vec::new();
        if let Some(o) = tag_inner(s, "local-command-stdout") {
            if !o.trim().is_empty() {
                output.push(o.trim().to_string());
            }
        }
        out.push(Block::Command {
            name: name.trim().to_string(),
            args,
            output,
        });
        return;
    }
    // A standalone stdout message — attach to the command it follows, else show
    // it on its own (command-less).
    if let Some(o) = tag_inner(s, "local-command-stdout") {
        let o = o.trim().to_string();
        if o.is_empty() {
            return;
        }
        if let Some(Block::Command { output, .. }) = out.last_mut() {
            output.push(o);
        } else {
            out.push(Block::Command {
                name: String::new(),
                args: String::new(),
                output: vec![o],
            });
        }
        return;
    }
    // Drop pure caveat noise; otherwise it's ordinary user prose (unless it's an
    // injected skill body, which folds as a result block).
    let cleaned = strip_caveat(s);
    if !cleaned.trim().is_empty() {
        if is_skill_body(&cleaned) {
            out.push(Block::ToolResult(cleaned));
        } else {
            out.push(Block::UserText(cleaned));
        }
    }
}

/// Categorize a `tool_use` by name. Edit/Write/Bash get their own keys;
/// read-ish tools collapse under `read`; anything else under `tool`.
fn tool_fold_key(name: &str) -> &'static str {
    match name {
        "Edit" | "MultiEdit" => "edit",
        "Write" | "NotebookEdit" => "write",
        "Bash" => "bash",
        "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => "read",
        _ => "tool",
    }
}

/// Make an absolute path relative to the session's cwd when it sits under it
/// (matching how Claude Code shows tool targets — relative to the cwd recorded in
/// the transcript, NOT peek's runtime cwd); else leave it as-is.
fn relativize(p: &str, base: &str) -> String {
    relativize_with(p, base, std::env::var("HOME").ok().as_deref())
}

/// Make `p` relative to the session cwd `base` when it sits under it; else
/// abbreviate a `$HOME` prefix to `~` (matching Claude Code, which shows
/// out-of-project paths as `~/…`); else leave it absolute.
fn relativize_with(p: &str, base: &str, home: Option<&str>) -> String {
    let path = std::path::Path::new(p);
    if !base.is_empty() {
        if let Ok(r) = path.strip_prefix(base) {
            return r.display().to_string();
        }
    }
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        if let Ok(r) = path.strip_prefix(home) {
            return format!("~/{}", r.display());
        }
    }
    p.to_string()
}

fn tool_target(input: &Value, cwd: &str) -> String {
    for k in ["file_path", "path"] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return relativize(v, cwd);
        }
    }
    // A shell command keeps its line breaks — the header lays a multi-line command
    // out across rows (see `render::tool_header_lines`), matching Claude Code.
    if let Some(v) = input.get("command").and_then(|v| v.as_str()) {
        return v.to_string();
    }
    // Descriptions/patterns/skill-names are kept in full (no truncation), but their
    // newlines are flattened so these one-line headers stay one line.
    for k in ["description", "pattern", "skill"] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return v.replace('\n', " ");
        }
    }
    String::new()
}

/// Seconds since the Unix epoch for an ISO-8601 UTC timestamp like
/// `2026-06-30T03:36:44.500Z` (we only ever use *differences*, so the absolute
/// epoch just needs to be consistent). Returns `None` if it doesn't parse.
fn epoch_secs(ts: &str) -> Option<f64> {
    let (date, time) = ts.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let mut t = time.split(':');
    let h: f64 = t.next()?.parse().ok()?;
    let mi: f64 = t.next()?.parse().ok()?;
    let s: f64 = t.next()?.parse().ok()?;
    // days_from_civil (Howard Hinnant): civil date → days since 1970-01-01.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days as f64 * 86400.0 + h * 3600.0 + mi * 60.0 + s)
}

/// Tools Claude Code summarizes into a `Thought for …` turn line (transient reads/
/// searches whose results feed the thinking) rather than showing expanded. Edit/
/// Write/other tools produce durable output (diffs, etc.) and stay expanded.
/// `pub(crate)` so the live-tail path (`view::ingest`) can re-group a thinking
/// block with activity tools that arrived in an earlier poll.
pub(crate) fn is_activity_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash" | "Read" | "NotebookRead" | "Grep" | "Glob" | "LS"
    )
}

/// Fold each `Thinking` block together with the contiguous run of *activity* tool
/// calls that immediately precede it (whose results it processed), matching Claude
/// Code's `Thought for Xs, <activities>` turn summary. Edit/Write and other tools
/// (and any tool not directly before a thinking) are left expanded.
fn group_turns(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len());
    for b in blocks {
        if let Block::Thinking {
            text,
            duration_secs,
            ..
        } = b
        {
            let mut tools = Vec::new();
            while matches!(out.last(), Some(Block::ToolUse { name, .. }) if is_activity_tool(name))
            {
                tools.push(out.pop().unwrap());
            }
            tools.reverse();
            out.push(Block::Thinking {
                text,
                duration_secs,
                tools,
            });
        } else {
            out.push(b);
        }
    }
    out
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .first()
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string(),
        _ => content.to_string(),
    }
}

/// Is this tool_result text the no-information boilerplate Edit/Write emits?
fn is_boilerplate(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("The file ") && s.contains("has been updated successfully"))
        || s.starts_with("File created successfully at")
}

/// Parse `toolUseResult.structuredPatch` into hunks (real line numbers).
fn parse_patch(tur: &Value) -> Option<Vec<Hunk>> {
    let arr = tur.get("structuredPatch")?.as_array()?;
    let hunks: Vec<Hunk> = arr
        .iter()
        .filter_map(|h| {
            let new_start = h.get("newStart").and_then(|n| n.as_u64())? as usize;
            let old_start = h
                .get("oldStart")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize)
                .unwrap_or(new_start);
            let lines = h
                .get("lines")?
                .as_array()?
                .iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect();
            Some(Hunk {
                old_start,
                new_start,
                lines,
            })
        })
        .collect();
    (!hunks.is_empty()).then_some(hunks)
}

/// The output text to show under a tool call. Edit/Write show their diff/code,
/// not the boilerplate result, so they get `None`. Bash uses stdout/stderr; Read
/// uses the file content; other tools use the raw result (unless boilerplate).
fn tool_output(name: &str, tur: Option<&Value>, res_txt: &str) -> Option<String> {
    match name {
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => None,
        "Bash" => {
            if let Some(tur) = tur {
                let out = tur.get("stdout").and_then(|s| s.as_str()).unwrap_or("");
                let err = tur.get("stderr").and_then(|s| s.as_str()).unwrap_or("");
                let combined = match (out.trim().is_empty(), err.trim().is_empty()) {
                    (true, true) => String::new(),
                    (false, true) => out.to_string(),
                    (true, false) => err.to_string(),
                    (false, false) => format!("{out}\n{err}"),
                };
                if !combined.trim().is_empty() {
                    return Some(combined);
                }
            }
            (!res_txt.trim().is_empty()).then(|| res_txt.to_string())
        }
        "Read" => tur
            .and_then(|t| t.pointer("/file/content"))
            .and_then(|c| c.as_str())
            .map(String::from)
            .or_else(|| (!res_txt.trim().is_empty()).then(|| res_txt.to_string())),
        _ => (!res_txt.trim().is_empty() && !is_boilerplate(res_txt)).then(|| res_txt.to_string()),
    }
}

/// Parse JSONL text into the **complete** block list. Kept for tests and the
/// live-tail path (small in-memory batches); makes two cheap passes over the str.
pub fn parse(jsonl: &str, args: &Args) -> Vec<Block> {
    let tool_ids = scan_tool_ids(jsonl.lines());
    parse_main(jsonl.lines(), &tool_ids, args)
}

pub fn parse_for(backend: Backend, jsonl: &str, args: &Args) -> Vec<Block> {
    match backend {
        Backend::Claude => parse(jsonl, args),
        Backend::Codex => crate::codex_model::parse_codex(jsonl, args),
    }
}

/// Parse a transcript file by **streaming** it — one line resident at a time, in
/// two passes (each a fresh read) — so a large transcript never balloons into a
/// whole-file `Vec<Value>` (~5–8× the file in RAM) or a whole-file `String`. See
/// `STREAMING-PARSE-DESIGN.md`.
pub fn parse_path(path: &std::path::Path, args: &Args) -> std::io::Result<Vec<Block>> {
    use std::io::BufRead;
    let open = || -> std::io::Result<_> { Ok(std::io::BufReader::new(std::fs::File::open(path)?)) };
    // Pass 1: collect the set of all tool_use ids (small — ids only), so pass 2 can
    // tell a genuine orphan tool_result from one whose tool_use appears later.
    let tool_ids = scan_tool_ids(open()?.lines().map_while(|r| r.ok()));
    Ok(parse_main(
        open()?.lines().map_while(|r| r.ok()),
        &tool_ids,
        args,
    ))
}

pub fn parse_path_for(
    backend: Backend,
    path: &std::path::Path,
    args: &Args,
) -> std::io::Result<Vec<Block>> {
    match backend {
        Backend::Claude => parse_path(path, args),
        Backend::Codex => crate::codex_model::parse_codex_path(path, args),
    }
}

/// Pass 1: the set of every `tool_use` id in the transcript.
fn scan_tool_ids<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> HashSet<String> {
    let mut ids = HashSet::new();
    for line in lines {
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("assistant") {
            if let Some(arr) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for blk in arr {
                    if blk.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        if let Some(id) = blk.get("id").and_then(|s| s.as_str()) {
                            ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }
    ids
}

/// Fill a `tool_use` block's result fields (output / diff line numbers / read
/// count) from its matching `tool_result`'s `toolUseResult` metadata + text.
fn apply_result(block: &mut Block, txt: &str, tur: &Value) {
    if let Block::ToolUse {
        name,
        output,
        patch,
        read_lines,
        ..
    } = block
    {
        *output = tool_output(name, Some(tur), txt);
        *patch = parse_patch(tur);
        *read_lines = tur
            .pointer("/file/numLines")
            .and_then(|n| n.as_u64())
            .map(|n| n as usize);
    }
}

/// Pass 2: build blocks in order, streaming one line at a time. Nothing is dropped
/// or truncated. A `tool_use` is emitted immediately with an empty result; its
/// `tool_result` **back-patches** the already-emitted block in place (via
/// `tool_slot`: id → block index). Transcripts are **not** strictly ordered — a
/// result can precede its tool_use (compaction / sidechain reordering) — so a
/// result whose tool_use we haven't emitted yet is held in `pending` and applied
/// when that tool_use arrives (its id is in `tool_ids`); only a result whose id is
/// in **no** tool_use is a genuine orphan, emitted inline. This reproduces the old
/// two-pass semantics exactly while keeping at most one line's `Value` live.
/// `_args` is unused (fold flags are resolved in `view`).
fn parse_main<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    tool_ids: &HashSet<String>,
    _args: &Args,
) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    // tool_use id -> index of its ToolUse block in `out`, for result back-patching.
    let mut tool_slot: HashMap<String, usize> = HashMap::new();
    // Results seen before their tool_use (id is in `tool_ids`), awaiting it.
    let mut pending: HashMap<String, (String, Value)> = HashMap::new();
    // The session's cwd (from the transcript) — tool targets are shown relative to
    // it. CC records it on every event, so it's set from the first line, before any
    // tool_use; fall back to "" (absolute paths) if a tool_use somehow precedes it.
    let mut cwd = String::new();
    // Timestamp of the last user/tool-result event — the moment the model's next
    // generation was requested — so a thinking block's duration is `its ts − this`.
    let mut trigger_ts: Option<f64> = None;

    for line in lines {
        let line = line.as_ref().trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                cwd = c.to_string();
            }
        }
        let ev_ts = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(epoch_secs);
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                    continue;
                };
                for blk in content {
                    match blk.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    out.push(Block::AssistantText(t.to_string()));
                                }
                            }
                        }
                        Some("thinking") => {
                            let t = blk
                                .get("thinking")
                                .or_else(|| blk.get("text"))
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            if !t.trim().is_empty() {
                                let duration_secs = match (ev_ts, trigger_ts) {
                                    (Some(end), Some(start)) if end >= start => {
                                        Some((end - start) as u64)
                                    }
                                    _ => None,
                                };
                                out.push(Block::Thinking {
                                    text: t.to_string(),
                                    duration_secs,
                                    tools: Vec::new(),
                                });
                            }
                        }
                        Some("tool_use") => {
                            let name = blk.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                            let input = blk.get("input").cloned().unwrap_or(Value::Null);
                            let id = blk.get("id").and_then(|s| s.as_str()).unwrap_or("");
                            out.push(Block::ToolUse {
                                name: name.to_string(),
                                target: tool_target(&input, &cwd),
                                diffs: extract_diffs(name, &input),
                                output: None,
                                patch: None,
                                read_lines: None,
                            });
                            let idx = out.len() - 1;
                            if !id.is_empty() {
                                tool_slot.insert(id.to_string(), idx);
                                // A result that arrived before this tool_use? Apply it now.
                                if let Some((txt, tur)) = pending.remove(id) {
                                    apply_result(&mut out[idx], &txt, &tur);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                // A user turn or tool_result — the trigger for the next generation.
                if let Some(t) = ev_ts {
                    trigger_ts = Some(t);
                }
                // The message-level toolUseResult metadata (shared by its result blocks).
                let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                if let Some(s) = content.as_str() {
                    push_user_string(s, &mut out);
                } else if let Some(arr) = content.as_array() {
                    for blk in arr {
                        match blk.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                    if !t.trim().is_empty() {
                                        if is_skill_body(t) {
                                            out.push(Block::ToolResult(t.to_string()));
                                        } else {
                                            out.push(Block::UserText(t.to_string()));
                                        }
                                    }
                                }
                            }
                            Some("tool_result") => {
                                let tid = blk
                                    .get("tool_use_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");
                                let txt = result_text(blk.get("content").unwrap_or(&Value::Null));
                                if let Some(&idx) = tool_slot.get(tid) {
                                    // Its tool_use is already emitted — back-patch in place.
                                    apply_result(&mut out[idx], &txt, &tur);
                                } else if tool_ids.contains(tid) {
                                    // Its tool_use appears later — hold until then (last wins).
                                    pending.insert(tid.to_string(), (txt, tur.clone()));
                                } else if !txt.trim().is_empty() && !is_boilerplate(&txt) {
                                    // No tool_use anywhere — a genuine orphan, shown inline.
                                    out.push(Block::ToolResult(txt));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    group_turns(out)
}

fn extract_diffs(name: &str, input: &Value) -> Vec<(String, String)> {
    match name {
        "Edit" => {
            let o = input
                .get("old_string")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let n = input
                .get("new_string")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            vec![(o.to_string(), n.to_string())]
        }
        "Write" => {
            let n = input.get("content").and_then(|s| s.as_str()).unwrap_or("");
            vec![(String::new(), n.to_string())]
        }
        "NotebookEdit" => {
            let n = input
                .get("new_source")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            vec![(String::new(), n.to_string())]
        }
        "MultiEdit" => input
            .get("edits")
            .and_then(|e| e.as_array())
            .map(|edits| {
                edits
                    .iter()
                    .map(|e| {
                        (
                            e.get("old_string")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string(),
                            e.get("new_string")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            target: None,
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
            dump: Some(Some("-".into())),
            width: None,
        }
    }

    fn kinds(blocks: &[Block]) -> Vec<&'static str> {
        blocks.iter().map(fold_key).collect()
    }

    /// A thinking block absorbs the activity tools that ran just before it and
    /// carries a duration = (its timestamp − the triggering event's timestamp).
    #[test]
    fn thinking_groups_preceding_tools_with_duration() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:12.000Z","message":{"content":[{"type":"thinking","thinking":"hmm let me consider"}]}}
"#;
        let blocks = parse(jsonl, &args());
        // The Bash is absorbed into the thinking (not a top-level block).
        assert_eq!(kinds(&blocks), vec!["user", "thinking"], "{blocks:?}");
        let Block::Thinking {
            duration_secs,
            tools,
            ..
        } = &blocks[1]
        else {
            panic!("not a thinking turn: {blocks:?}");
        };
        // 03:00:12 − 03:00:03 (last tool_result) = 9s, floored.
        assert_eq!(*duration_secs, Some(9));
        assert_eq!(tools.len(), 1, "did not absorb the preceding Bash");
    }

    /// Edit/Write tools are NOT absorbed into a following thinking (CC shows their
    /// diffs expanded); only transient activity tools (Bash/Read/…) group in.
    #[test]
    fn edit_stays_expanded_next_to_thinking() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"thinking","thinking":"ok"}]}}
"#;
        let blocks = parse(jsonl, &args());
        assert_eq!(
            kinds(&blocks),
            vec!["user", "edit", "thinking"],
            "{blocks:?}"
        );
    }

    #[test]
    fn skill_call_names_its_target_and_body_folds_as_result() {
        let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /Users/dev/.claude/skills/dump-tasks\n\n# dump-tasks\n\nTurn the work into a brief."}]}}
"#;
        let blocks = parse(jsonl, &args());
        // The Skill tool_use carries its skill name as the target (CC: Skill(dump-tasks)).
        match &blocks[0] {
            Block::ToolUse { name, target, .. } => {
                assert_eq!(name, "Skill");
                assert_eq!(target, "dump-tasks", "skill name not used as target");
            }
            other => panic!("expected Skill ToolUse, got {other:?}"),
        }
        // The Skill call is its own block; the injected body folds as a result
        // block (not a `❯` user turn).
        assert_eq!(
            kinds(&blocks),
            vec!["tool", "tool_result"],
            "skill body should fold as a tool_result, not user text"
        );
    }

    #[test]
    fn task_notification_folds_to_summary_line() {
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Build release\" completed (exit code 0)</summary>\n</task-notification>"}}
"#;
        let blocks = parse(jsonl, &args());
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            Block::ToolResult(t) => {
                assert_eq!(
                    t,
                    "Background command \"Build release\" completed (exit code 0)"
                );
                assert!(!t.contains("task-notification"), "raw XML leaked: {t}");
                assert!(!t.contains("task-id"), "raw XML leaked: {t}");
            }
            other => panic!("expected ToolResult summary, got {other:?}"),
        }
    }

    #[test]
    fn relativize_uses_cwd_then_home_tilde() {
        let home = Some("/Users/h");
        // Under the session cwd → relative.
        assert_eq!(
            relativize_with("/Users/h/proj/src/a.rs", "/Users/h/proj", home),
            "src/a.rs"
        );
        // Not under cwd but under $HOME → ~/…  (matches Claude Code).
        assert_eq!(
            relativize_with("/Users/h/.claude/x.md", "/Users/h/proj", home),
            "~/.claude/x.md"
        );
        // Outside both → left absolute.
        assert_eq!(
            relativize_with("/etc/hosts", "/Users/h/proj", home),
            "/etc/hosts"
        );
    }

    #[test]
    fn nothing_is_dropped_by_default() {
        // A Read, a non-modifying Bash (`ls`), an Edit, and a tool_result must
        // ALL produce blocks now — no parse-time filtering.
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"do it"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"FILE CONTENTS"}]}}
"#;
        let blocks = parse(jsonl, &args());
        assert_eq!(
            kinds(&blocks),
            vec!["user", "assistant", "read", "bash", "edit", "tool_result"]
        );
    }

    #[test]
    fn tool_result_text_is_not_truncated() {
        // Build a >20-line, long result; the full text must survive parsing.
        let big: String = (0..40)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\\n");
        let jsonl = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":"{big}"}}]}}}}"#
        );
        let blocks = parse(&jsonl, &args());
        assert_eq!(blocks.len(), 1);
        let Block::ToolResult(t) = &blocks[0] else {
            panic!("expected a tool_result block");
        };
        assert_eq!(t.lines().count(), 40, "result was truncated: {t:?}");
        assert!(t.contains("line 39"), "tail line missing");
    }

    #[test]
    fn joins_tooluseresult_metadata() {
        let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"oldLines":1,"newStart":12,"newLines":1,"lines":[" ctx","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"The file /x.rs has been updated successfully. (file state is current in your context — no need to Read it back)"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/y.rs"}}]}}
{"type":"user","toolUseResult":{"type":"text","file":{"filePath":"/y.rs","content":"l1\nl2\nl3","numLines":3,"startLine":1,"totalLines":3}},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"l1\nl2\nl3"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","toolUseResult":{"stdout":"file1\nfile2","stderr":"","interrupted":false},"message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"file1\nfile2"}]}}
"#;
        let blocks = parse(jsonl, &args());
        // 3 tool blocks; the boilerplate Edit result is NOT a separate block.
        assert_eq!(kinds(&blocks), vec!["edit", "read", "bash"]);

        let Block::ToolUse { patch, output, .. } = &blocks[0] else {
            panic!("expected Edit ToolUse");
        };
        assert_eq!(patch.as_ref().unwrap()[0].new_start, 12, "real newStart");
        assert!(output.is_none(), "edit boilerplate dropped");

        let Block::ToolUse { read_lines, .. } = &blocks[1] else {
            panic!("expected Read ToolUse");
        };
        assert_eq!(*read_lines, Some(3));

        let Block::ToolUse { output, .. } = &blocks[2] else {
            panic!("expected Bash ToolUse");
        };
        assert_eq!(output.as_deref(), Some("file1\nfile2"));
    }

    /// Transcripts are NOT strictly ordered: a `tool_result` can appear *before*
    /// its `tool_use` (compaction / sidechain reordering — seen in real 78/298 MB
    /// sessions). The streaming parse must still join them (via the tool_use id
    /// pre-scan + a pending buffer), or the Edit loses its structuredPatch line
    /// numbers and a Read loses its content.
    #[test]
    fn result_before_tool_use_still_joins() {
        let jsonl = r#"
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"newStart":88,"lines":[" c","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"The file /x.rs has been updated successfully."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
"#;
        let blocks = parse(jsonl, &args());
        // The out-of-order result joined its Edit — no stray orphan block.
        assert_eq!(kinds(&blocks), vec!["edit"], "{blocks:?}");
        let Block::ToolUse { patch, .. } = &blocks[0] else {
            panic!("expected Edit ToolUse");
        };
        assert_eq!(
            patch.as_ref().expect("patch joined from earlier result")[0].new_start,
            88,
            "structuredPatch line number lost — result-before-use not joined"
        );
    }

    /// A `tool_result` whose id belongs to no `tool_use` anywhere is a genuine
    /// orphan and is shown inline (not swallowed).
    #[test]
    fn orphan_result_with_no_tool_use_shown_inline() {
        let jsonl = r#"
{"type":"user","message":{"content":"go"}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"ghost","content":"orphan output"}]}}
"#;
        let blocks = parse(jsonl, &args());
        assert_eq!(kinds(&blocks), vec!["user", "tool_result"], "{blocks:?}");
        let Block::ToolResult(t) = &blocks[1] else {
            panic!("expected orphan ToolResult");
        };
        assert_eq!(t, "orphan output");
    }

    /// `parse_path` (streaming file read, two passes) must produce exactly what
    /// `parse(&str)` produces for the same content.
    #[test]
    fn parse_path_matches_parse_str() {
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/p","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","toolUseResult":{"stdout":"out","stderr":""},"message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#,
            "\n",
        );
        let via_str = parse(jsonl, &args());
        let file = std::env::temp_dir().join("claude-replay-parse-path-test.jsonl");
        std::fs::write(&file, jsonl).unwrap();
        let via_path = parse_path(&file, &args()).unwrap();
        std::fs::remove_file(&file).ok();
        assert_eq!(format!("{via_str:?}"), format!("{via_path:?}"));
    }

    #[test]
    fn fold_keys_categorize_tools() {
        let mk = |name: &str| Block::ToolUse {
            name: name.into(),
            target: String::new(),
            diffs: vec![],
            output: None,
            patch: None,
            read_lines: None,
        };
        assert_eq!(fold_key(&mk("Read")), "read");
        assert_eq!(fold_key(&mk("Grep")), "read");
        assert_eq!(fold_key(&mk("Bash")), "bash");
        assert_eq!(fold_key(&mk("Edit")), "edit");
        assert_eq!(fold_key(&mk("MultiEdit")), "edit");
        assert_eq!(fold_key(&mk("Write")), "write");
        assert_eq!(fold_key(&mk("SomeMcpTool")), "tool");
        assert_eq!(
            fold_key(&Block::Thinking {
                text: "x".into(),
                duration_secs: None,
                tools: vec![]
            }),
            "thinking"
        );
        assert_eq!(fold_key(&Block::ToolResult("x".into())), "tool_result");
    }

    #[test]
    fn slash_command_becomes_command_block_caveat_stripped() {
        // A /compact invocation with inline stdout and a caveat: one Command
        // block, caveat dropped, no raw tags surviving.
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: noise</local-command-caveat><command-name>/compact</command-name><command-message>compact</command-message><command-args></command-args>"}}
{"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted (ctrl+o to see full summary)</local-command-stdout>"}}
"#;
        let blocks = parse(jsonl, &args());
        assert_eq!(
            blocks.len(),
            1,
            "should be a single Command block: {blocks:?}"
        );
        let Block::Command { name, args, output } = &blocks[0] else {
            panic!("expected Block::Command, got {:?}", blocks[0]);
        };
        assert_eq!(name, "/compact");
        assert!(args.is_empty(), "no args expected: {args:?}");
        assert_eq!(
            output,
            &vec!["Compacted (ctrl+o to see full summary)".to_string()]
        );
        // No raw wrapper tags leaked through.
        let joined = format!("{blocks:?}");
        assert!(!joined.contains("command-name"), "raw tag leaked: {joined}");
        assert!(!joined.contains("caveat"), "caveat leaked: {joined}");
    }

    #[test]
    fn caveat_only_message_is_dropped() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>just noise</local-command-caveat>"}}"#;
        assert!(
            parse(jsonl, &args()).is_empty(),
            "caveat-only should yield nothing"
        );
    }

    #[test]
    fn tool_target_relativizes_paths_under_session_cwd() {
        // Relative to the transcript's cwd (the repo root), not peek's runtime cwd.
        let base = "/Users/dev/project";
        let input = serde_json::json!({ "file_path": "/Users/dev/project/src/picker.rs" });
        assert_eq!(tool_target(&input, base), "src/picker.rs");

        // A path outside the session cwd is left absolute.
        let outside = serde_json::json!({ "file_path": "/etc/hosts" });
        assert_eq!(tool_target(&outside, base), "/etc/hosts");
    }

    #[test]
    fn tool_target_keeps_command_newlines_but_flattens_others() {
        // A multi-line shell command keeps its line breaks (the header lays it out
        // across rows); descriptions/patterns stay one line.
        let cmd = serde_json::json!({ "command": "cd /x\ncargo test" });
        assert_eq!(tool_target(&cmd, "/x"), "cd /x\ncargo test");

        let desc = serde_json::json!({ "description": "line one\nline two" });
        assert_eq!(tool_target(&desc, "/x"), "line one line two");
    }
}
