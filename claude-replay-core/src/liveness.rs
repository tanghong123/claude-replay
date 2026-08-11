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

/// One Codex tool family that can leave a call dangling in the tail: a call record plus the
/// SEPARATE output record that closes it. Both fields are `payload.type` values, matched
/// exactly — never as free substrings, because a tool output can quote a transcript.
///
/// Self-contained record kinds are deliberately absent. The Codex reader also renders
/// `web_search_call` and `image_generation_call` as tool uses (`agents::codex::model`), but
/// those arrive as ONE complete record with no output counterpart — the search results and
/// the image bytes are inside the call record itself. Tracking them here could only ever
/// leave a permanently unmatched id, i.e. a finished session reported busy forever. That is
/// the mirror image of the #82 gap this check exists to close, and the worse half: #82 kills
/// working sessions, this would keep dead ones alive.
struct CodexToolPattern {
    /// `payload.type` of a call record.
    call: &'static str,
    /// `payload.type` of the record that closes it.
    result: &'static str,
    /// Keys that may carry the call identity, in precedence order — the FIRST key present on
    /// the record wins, exactly as the Codex reader's `specialized_call_id` pairs the two
    /// records. Taking every key instead is a bug on real rollouts: a `tool_search_call`
    /// carries both `id` (`tsc_…`) and `call_id` (`call_…`), and only the `call_id` repeats
    /// on the `tool_search_output` (whose own `id` is `tso_…`), so the call's `id` would
    /// dangle for the rest of the session.
    id_keys: &'static [&'static str],
}

const CODEX_TOOL_PATTERNS: &[CodexToolPattern] = &[
    CodexToolPattern {
        call: "function_call",
        result: "function_call_output",
        id_keys: &["call_id"],
    },
    CodexToolPattern {
        call: "custom_tool_call",
        result: "custom_tool_call_output",
        id_keys: &["call_id"],
    },
    CodexToolPattern {
        call: "tool_search_call",
        result: "tool_search_output",
        id_keys: &["call_id", "id"],
    },
];

/// Whether the transcript's TAIL holds an **in-flight tool call** — a `tool_use` (Claude)
/// or Codex call with no matching result yet. Mid-tool is BUSY BY CONSTRUCTION even when
/// every file mtime is stale: transcripts are only written when a result LANDS, so during a
/// long `cargo build`/test run the whole tree sits untouched (#82 — the gap in #32's
/// quiet-tree signal that got working sessions SIGKILLed).
///
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
    // marks calls with `"type":"<call_type>"` and closes them with matching result records,
    // both carrying one of the id keys (`call_id` or `id`).
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
    /// A Codex record's own kind, read from the `payload` envelope it is always serialized
    /// with (`{"type":"response_item","payload":{"type":"…"`). Anchoring here rather than
    /// searching the line keeps a tool output that QUOTES a transcript — this repo's own
    /// sessions do exactly that — from being mistaken for the record it quotes.
    fn codex_payload_type(line: &str) -> Option<&str> {
        const ANCHOR: &str = "\"payload\":{\"type\":\"";
        let rest = &line[line.find(ANCHOR)? + ANCHOR.len()..];
        rest.find('"').map(|end| &rest[..end])
    }
    /// The identity a Codex call and its output are paired by: the first of `keys` present on
    /// the record, mirroring the reader's `specialized_call_id`. `None` when absent or empty —
    /// an id we cannot read is not an id we can wait for.
    fn call_identity(line: &str, keys: &[&str]) -> Option<String> {
        let raw = keys.iter().find_map(|k| field_values(line, k).next())?;
        (!raw.is_empty()).then(|| raw.to_string())
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
        let Some(kind) = codex_payload_type(line) else {
            continue;
        };
        for pat in CODEX_TOOL_PATTERNS {
            if kind == pat.call {
                uses.extend(call_identity(line, pat.id_keys));
            } else if kind == pat.result {
                results.extend(call_identity(line, pat.id_keys));
            }
        }
    }
    uses.iter().any(|u| !results.contains(u))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cr-liveness-{}-{}", name, std::process::id()))
    }

    fn write_tail(path: &std::path::Path, lines: &str) {
        std::fs::write(path, lines).unwrap();
    }

    #[test]
    fn claude_tool_use_without_result_is_inflight() {
        let p = fixture("claude-inflight");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"tool_use","id":"toolu_01","name":"bash","input":{}}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn claude_tool_use_with_result_is_not_inflight() {
        let p = fixture("claude-done");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"tool_use","id":"toolu_02","name":"bash","input":{}}}
{"type":"response_item","payload":{"type":"tool_result","tool_use_id":"toolu_02","content":"ok"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_function_call_without_output_is_inflight() {
        let p = fixture("codex-func");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"fc-1","name":"bash","arguments":{}}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_function_call_with_output_is_not_inflight() {
        let p = fixture("codex-func-done");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"fc-2","name":"bash","arguments":{}}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"fc-2","output":"ok"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_custom_tool_call_is_tracked() {
        let p = fixture("codex-custom");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"ct-1","name":"Read","arguments":{}}}
{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"ct-1","output":"hi"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_tool_search_call_is_closed_by_its_call_id_not_its_own_id() {
        // Shape taken verbatim from a real rollout: the call carries BOTH `id` ("tsc_…") and
        // `call_id`, the output repeats only the `call_id` and has its own `id` ("tso_…").
        // Pairing on every id key would leave "tsc_…" dangling for the rest of the session.
        let p = fixture("codex-search");
        write_tail(
            &p,
            r#"{"timestamp":"2026-07-30T07:59:07.943Z","type":"response_item","payload":{"type":"tool_search_call","id":"tsc_057945","call_id":"call_Pfd96J","status":"completed","execution":"client","arguments":{"query":"board api"}}}
{"timestamp":"2026-07-30T07:59:08.001Z","type":"response_item","payload":{"type":"tool_search_output","id":"tso_019fb208","call_id":"call_Pfd96J","status":"completed","execution":"client","tools":[]}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_tool_search_call_dangling_is_inflight() {
        let p = fixture("codex-search-open");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"tool_search_call","id":"tsc_open","call_id":"call_open","status":"completed","arguments":{}}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_self_contained_records_are_never_inflight() {
        // `web_search_call` and `image_generation_call` carry their own results and have no
        // output record to wait for, so they must not enter the dangling set at all —
        // otherwise every session that ever searched the web would read as busy forever.
        let p = fixture("codex-self-contained");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"rust"}}}
{"type":"response_item","payload":{"type":"image_generation_call","id":"ig_1","revised_prompt":"crab","result":null}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn a_tool_output_quoting_a_call_record_still_closes_it() {
        // Transcripts of work ON this repo embed transcript text in tool output. The record's
        // kind therefore has to come from its own `payload` envelope, not from a substring
        // search that the quoted text can satisfy.
        let p = fixture("codex-quoted");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"fc-3","name":"bash","arguments":{}}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"fc-3","output":"grep hit: {\"type\":\"function_call\",\"call_id\":\"fc-9\"}"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn only_unmatched_calls_count_as_inflight() {
        let p = fixture("codex-mixed");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"done-1","name":"a"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"done-1","output":"ok"}}
{"type":"response_item","payload":{"type":"function_call","call_id":"open-1","name":"b"}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn empty_call_ids_are_ignored() {
        let p = fixture("codex-empty-id");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"","name":"a"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn missing_file_is_not_inflight() {
        let p = fixture("missing");
        assert!(!inflight_tool_in_tail(&p));
    }
}
