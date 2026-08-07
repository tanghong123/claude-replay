//! **Transcript-tree liveness signals** — agent-neutral file reads that answer "is this
//! session doing anything?", moved down from the `jdi` supervisor (#98 §10) so a consumer
//! that is not the viewer binary (the monitor, a separate repo later) can link them without
//! dragging in clap, ratatui and the whole TUI.
//!
//! Two signals, and they are deliberately BOTH needed (#82): mtime growth misses a session
//! blocked in a long tool call (transcripts are only written when a result *lands*, so a
//! `cargo build` leaves the whole tree untouched while the session is maximally busy), and
//! the in-flight check alone misses ordinary streaming growth between tool calls.

use std::path::Path;

/// The most recent write anywhere in a session's transcript TREE: the root transcript
/// plus every child transcript under its `<stem>/subagents/` dir — the signal that the
/// session is actively working even when the root is quiet (a sub-agent holds the turn).
/// `None` when nothing is readable.
pub fn latest_tree_activity(transcript: &Path) -> Option<std::time::SystemTime> {
    let mut latest = std::fs::metadata(transcript)
        .and_then(|m| m.modified())
        .ok();
    let subagents = transcript
        .parent()
        .zip(transcript.file_stem())
        .map(|(dir, stem)| dir.join(stem).join("subagents"));
    if let Some(entries) = subagents.and_then(|d| std::fs::read_dir(d).ok()) {
        for e in entries.flatten() {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                latest = Some(latest.map_or(m, |cur| cur.max(m)));
            }
        }
    }
    latest
}

/// How far back the in-flight scan reads. Large enough that a tool call's `tool_use` line is
/// still inside the window when its result lands pages later; bounded so the check is O(1)
/// in transcript size.
const INFLIGHT_TAIL_BYTES: u64 = 262_144;

/// Whether the transcript's TAIL holds an **in-flight tool call** — a `tool_use` (Claude)
/// or `function_call` (Codex) with no matching result yet. Mid-tool is BUSY BY
/// CONSTRUCTION even when every file mtime is stale: transcripts are only written when a
/// result LANDS, so during a long `cargo build`/test run the whole tree sits untouched
/// (#82 — the gap in #32's quiet-tree signal that got working sessions SIGKILLed).
/// Scans the last `INFLIGHT_TAIL_BYTES` (256 KiB) only; a use whose result predates the window
/// can't appear dangling (uses are collected from within the window, results may close
/// them from anywhere in it). Covers sub-agent work too: while a child runs, the parent's
/// spawning `tool_use` is itself unresolved in the ROOT tail.
pub fn inflight_tool_in_tail(transcript: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(transcript) else {
        return false;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(INFLIGHT_TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return false; // a seek into a multi-byte char etc. — treat as unknown/idle
    }
    let mut uses: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut results: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Field-level extraction: `key":"value"` occurrences on each line. Claude marks calls
    // as `"type":"tool_use"` with `"id":"toolu_…"` and results via `"tool_use_id"`; Codex
    // marks calls as `"type":"function_call"` and results as `"function_call_output"`,
    // both carrying `"call_id"`.
    fn field_values<'a>(line: &'a str, key: &str) -> impl Iterator<Item = &'a str> {
        let pat = format!("\"{key}\":\"");
        let mut rest = line;
        let mut out = Vec::new();
        while let Some(i) = rest.find(&pat) {
            let v = &rest[i + pat.len()..];
            if let Some(end) = v.find('"') {
                out.push(&v[..end]);
                rest = &v[end..];
            } else {
                break;
            }
        }
        out.into_iter()
    }
    for line in buf.lines() {
        if line.contains("\"type\":\"tool_use\"") {
            uses.extend(
                field_values(line, "id")
                    .filter(|v| v.starts_with("toolu"))
                    .map(str::to_string),
            );
        }
        if line.contains("\"tool_use_id\"") {
            results.extend(field_values(line, "tool_use_id").map(str::to_string));
        }
        if line.contains("\"type\":\"function_call\"") {
            uses.extend(field_values(line, "call_id").map(str::to_string));
        }
        if line.contains("\"type\":\"function_call_output\"") {
            results.extend(field_values(line, "call_id").map(str::to_string));
        }
    }
    uses.iter().any(|u| !results.contains(u))
}
