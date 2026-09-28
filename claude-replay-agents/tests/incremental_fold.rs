//! **Folding in N steps equals one full fold** — for the per-line consumer view a periodic
//! collector builds its tool-call and active-time figures from.
//!
//! `MetricsFold` already proves this for tokens (`engine_integration.rs`). A collector that
//! also counts tool calls and active time reads the SAME lines through the adapter's
//! `line_preprocessor` + `decode_line`, parks `state()` between runs and `restore()`s it —
//! a second surface where resumed state could drift: the Codex preprocessor's
//! `semantic_exec`/`transport_calls` (learned from `session_meta`, which a resume never
//! re-reads), the accumulator's span clock, a torn final line. The reference collector
//! below is deliberately small and agent-agnostic (the same shape as a downstream metrics
//! fold): complete lines only, a torn tail left unconsumed, ToolUse counted where
//! `tool_is_call` says so, and a gap-based active-time rule. The property: every way of
//! growing the file — two runs split at every byte, one run per line, runs of a few bytes
//! that tear lines mid-record — gives exactly what one fold of the whole file gives.
//!
//! Fixtures are synthetic.

use claude_replay_agents::{ClaudeAdapter, CodexAdapter};
use claude_replay_engine::adapter::{PreprocessedLine, TranscriptAdapter};
use claude_replay_engine::seam::Message;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

const IDLE_GAP: f64 = 300.0;
const GAP_CAP: f64 = 1800.0;

/// Everything a run parks for the next one.
#[derive(Clone, Default)]
struct Park {
    offset: usize,
    pre: Value,
    acc: Value,
    cwd: String,
    prev_ts: Option<f64>,
    pending: HashMap<String, String>,
}

#[derive(Debug, Default, PartialEq)]
struct Tally {
    calls: BTreeMap<String, u64>,
    /// Every ToolUse, including the ones `tool_is_call` excludes — to show what it excludes.
    raw_tool_uses: u64,
    active_secs: f64,
}

impl Tally {
    fn add(&mut self, other: Tally) {
        for (k, v) in other.calls {
            *self.calls.entry(k).or_default() += v;
        }
        self.raw_tool_uses += other.raw_tool_uses;
        self.active_secs += other.active_secs;
    }
    fn total_calls(&self) -> u64 {
        self.calls.values().sum()
    }
}

/// One collector run: resume at `park` (or cold), fold every COMPLETE line past it, return
/// the delta tally and the new park.
fn run(adapter: &dyn TranscriptAdapter, path: &Path, park: Option<&Park>) -> (Tally, Park) {
    let bytes = std::fs::read(path).unwrap();
    let mut pre = adapter.line_preprocessor();
    let mut acc = adapter.metrics_acc();
    let mut p = Park::default();
    if let Some(prev) = park {
        pre.restore(&prev.pre);
        acc.restore(&prev.acc);
        p = prev.clone();
    }
    let mut out = Tally::default();
    let mut msgs: Vec<Message> = Vec::new();
    while let Some(nl) = bytes[p.offset..].iter().position(|b| *b == b'\n') {
        let line = std::str::from_utf8(&bytes[p.offset..p.offset + nl]).unwrap();
        p.offset += nl + 1;
        let body = line.trim_end();
        if body.is_empty() {
            continue;
        }
        msgs.clear();
        match pre.process(body) {
            PreprocessedLine::Ignore => continue,
            PreprocessedLine::Messages(m) => msgs = m,
            _ => {}
        }
        match serde_json::from_str::<Value>(body) {
            Ok(v) => acc.push(&v),
            Err(_) => {
                acc.malformed_line();
                continue;
            }
        }
        if msgs.is_empty() {
            adapter.decode_line(body, &mut p.cwd, &mut msgs);
        }

        // What the line means for the gap it ends: a prompt is idle; an assistant action or
        // a non-interactive tool result is the agent working.
        let mut prompt = false;
        let mut working = false;
        for m in &msgs {
            match m {
                Message::UserText { .. } => prompt = true,
                Message::ToolResult { tool_use_id, .. } => {
                    let interactive = p
                        .pending
                        .remove(tool_use_id)
                        .is_some_and(|name| adapter.tool_is_interactive(&name));
                    working |= !interactive;
                }
                Message::ToolUse { id, name, .. } => {
                    p.pending.insert(id.clone(), name.clone());
                    working = true;
                    out.raw_tool_uses += 1;
                    if adapter.tool_is_call(name) {
                        *out.calls.entry(name.clone()).or_default() += 1;
                    }
                }
                Message::AssistantText(_)
                | Message::AssistantMessage { .. }
                | Message::Thinking { .. } => working = true,
                _ => {}
            }
        }
        let Some(ts) = acc.totals().2.map(|(_, end)| end) else {
            continue;
        };
        if let Some(prev) = p.prev_ts {
            let gap = ts - prev;
            if gap > 0.0 {
                out.active_secs += if gap <= IDLE_GAP {
                    gap
                } else if working && !prompt {
                    gap.min(GAP_CAP)
                } else {
                    0.0
                };
            }
        }
        p.prev_ts = Some(ts);
    }
    p.pre = pre.state();
    p.acc = acc.state();
    (out, p)
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn scratch() -> PathBuf {
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("cr-incremental-fold-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("t{n}.jsonl"))
}

/// Grow a file through `cuts` (byte offsets, ascending), folding after each growth, and sum
/// the runs' deltas — what a collector's store holds after merge-adding them.
fn fold_in_steps(adapter: &dyn TranscriptAdapter, full: &str, cuts: &[usize]) -> Tally {
    let path = scratch();
    let mut total = Tally::default();
    let mut park: Option<Park> = None;
    for &cut in cuts.iter().chain(std::iter::once(&full.len())) {
        std::fs::write(&path, &full.as_bytes()[..cut]).unwrap();
        let (delta, next) = run(adapter, &path, park.as_ref());
        total.add(delta);
        park = Some(next);
    }
    std::fs::remove_file(&path).ok();
    total
}

fn assert_every_growth_matches(adapter: &dyn TranscriptAdapter, full: &str, whole: &Tally) {
    // Two runs, split at EVERY byte: every line boundary and every torn mid-line tail.
    for cut in 0..full.len() {
        let t = fold_in_steps(adapter, full, &[cut]);
        assert_eq!(&t, whole, "two runs split at byte {cut}");
    }
    // One run per appended line.
    let line_ends: Vec<usize> = full.match_indices('\n').map(|(i, _)| i + 1).collect();
    assert_eq!(
        &fold_in_steps(adapter, full, &line_ends),
        whole,
        "one run per line"
    );
    // Many small writes, each tearing the line in progress.
    for step in [7, 31, 113] {
        let cuts: Vec<usize> = (step..full.len()).step_by(step).collect();
        assert_eq!(
            &fold_in_steps(adapter, full, &cuts),
            whole,
            "a run every {step} bytes"
        );
    }
}

fn jsonl(lines: &[Value]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// A Codex 0.147+ rollout: `exec`/`wait` transport around semantic `item_completed` records,
/// an exploration with three parsed actions, a two-file patch, an orchestrated `update_plan`,
/// a long working gap, a long idle gap, and a compaction.
fn codex_rollout() -> String {
    let t = |s: &str| format!("2026-08-19T07:{s}Z");
    let exec = |ts: &str, id: &str, code: &str| {
        serde_json::json!({"timestamp": t(ts), "type": "response_item", "payload":
            {"type": "custom_tool_call", "name": "exec", "call_id": id, "input": code}})
    };
    let exec_out = |ts: &str, id: &str| {
        serde_json::json!({"timestamp": t(ts), "type": "response_item", "payload":
            {"type": "custom_tool_call_output", "call_id": id, "output": "ok"}})
    };
    let tokens = |ts: &str, input: u64, output: u64| {
        serde_json::json!({"timestamp": t(ts), "type": "event_msg", "payload": {"type": "token_count",
            "info": {"total_token_usage": {"input_tokens": input, "cached_input_tokens": 0, "output_tokens": output},
                     "last_token_usage": {"input_tokens": input, "total_tokens": input + output}}}})
    };
    jsonl(&[
        serde_json::json!({"timestamp": t("00:00"), "type": "session_meta", "payload":
            {"id": "s1", "cwd": "/repo", "originator": "codex-tui", "cli_version": "0.147.0"}}),
        serde_json::json!({"timestamp": t("00:00"), "type": "turn_context", "payload": {"model": "gpt-5.5", "cwd": "/repo"}}),
        serde_json::json!({"timestamp": t("00:01"), "type": "response_item", "payload":
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "look around"}]}}),
        exec(
            "00:05",
            "c1",
            "const r = await tools.exec_command({cmd: \"cargo test\"});",
        ),
        serde_json::json!({"timestamp": t("00:06"), "type": "response_item", "payload":
            {"type": "function_call", "name": "wait", "call_id": "w1", "arguments": "{\"cell_id\":\"1\"}"}}),
        serde_json::json!({"timestamp": t("00:40"), "type": "event_msg", "payload": {"type": "item_completed", "item": {
            "type": "CommandExecution", "id": "cmd-1", "command": ["/bin/zsh", "-lc", "cargo test"],
            "cwd": "file:///repo", "status": "completed", "exit_code": 0, "formatted_output": "ok\n"}}}),
        serde_json::json!({"timestamp": t("00:41"), "type": "response_item", "payload":
            {"type": "function_call_output", "call_id": "w1", "output": "done"}}),
        exec_out("00:41", "c1"),
        tokens("00:42", 1000, 50),
        exec(
            "00:50",
            "c2",
            "await tools.exec_command({cmd: \"sed -n 1,9p a.rs && rg x src && ls\"});",
        ),
        serde_json::json!({"timestamp": t("00:51"), "type": "event_msg", "payload": {"type": "item_completed", "item": {
            "type": "CommandExecution", "id": "cmd-2",
            "command": ["/bin/zsh", "-lc", "sed -n 1,9p a.rs && rg x src && ls"],
            "cwd": "file:///repo", "status": "completed", "exit_code": 0,
            "parsed_cmd": [
                {"type": "read", "cmd": "sed -n 1,9p a.rs", "name": "a.rs", "path": "/repo/a.rs"},
                {"type": "search", "cmd": "rg x src", "query": "x", "path": "src"},
                {"type": "list_files", "cmd": "ls", "path": "."}
            ],
            "formatted_output": "a.rs\n"}}}),
        exec_out("00:52", "c2"),
        exec(
            "01:00",
            "c3",
            "await tools.update_plan({\"plan\":[{\"step\":\"fix\",\"status\":\"in_progress\"}]});",
        ),
        exec_out("01:01", "c3"),
        exec(
            "01:10",
            "c4",
            "await tools.apply_patch(\"*** Begin Patch\");",
        ),
        serde_json::json!({"timestamp": t("01:11"), "type": "event_msg", "payload": {"type": "item_completed", "item": {
            "type": "FileChange", "id": "fc-1", "changes": {
                "/repo/a.rs": {"type": "update", "unified_diff": "@@ -1 +1 @@\n-a\n+b\n"},
                "/repo/b.rs": {"type": "add", "unified_diff": "@@ -0,0 +1 @@\n+c\n"}}}}}),
        exec_out("01:12", "c4"),
        tokens("01:13", 2000, 90),
        // A 15-minute working gap: the assistant's answer ends it.
        serde_json::json!({"timestamp": t("16:13"), "type": "response_item", "payload":
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Done."}]}}),
        serde_json::json!({"timestamp": t("16:14"), "type": "event_msg", "payload": {"type": "task_complete"}}),
        // A 40-minute idle gap: the user's next prompt ends it.
        serde_json::json!({"timestamp": t("56:14"), "type": "response_item", "payload":
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "and now?"}]}}),
        serde_json::json!({"timestamp": t("56:20"), "type": "compacted", "payload": {"message": "summary"}}),
        tokens("56:21", 300, 10),
        serde_json::json!({"timestamp": t("56:30"), "type": "response_item", "payload":
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Next."}]}}),
    ])
}

/// A Claude Code transcript: tool calls, an interactive question answered after a long
/// pause (a human's latency, not work), a long working gap, and a long idle gap.
fn claude_transcript() -> String {
    let t = |s: &str| format!("2026-09-01T10:{s}Z");
    let user = |ts: &str, text: &str| {
        serde_json::json!({"type": "user", "cwd": "/r", "timestamp": t(ts),
            "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
    };
    let tool_use = |ts: &str, id: &str, name: &str, out: u64| {
        serde_json::json!({"type": "assistant", "cwd": "/r", "timestamp": t(ts), "message": {
            "role": "assistant", "model": "claude-opus-5", "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "id": id, "name": name, "input": {"command": "true"}}],
            "usage": {"input_tokens": 10, "output_tokens": out}}})
    };
    let result = |ts: &str, id: &str| {
        serde_json::json!({"type": "user", "cwd": "/r", "timestamp": t(ts), "message": {
            "role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
    };
    let say = |ts: &str, text: &str| {
        serde_json::json!({"type": "assistant", "cwd": "/r", "timestamp": t(ts), "message": {
            "role": "assistant", "model": "claude-opus-5", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}], "usage": {"input_tokens": 5, "output_tokens": 3}}})
    };
    jsonl(&[
        user("00:00", "build it"),
        tool_use("00:04", "t1", "Bash", 7),
        result("00:30", "t1"),
        tool_use("00:35", "t2", "Read", 4),
        result("00:36", "t2"),
        tool_use("00:40", "q1", "AskUserQuestion", 2),
        // Answered ten minutes later: the human's pause, not work.
        result("10:40", "q1"),
        tool_use("10:45", "t3", "Bash", 9),
        // A 20-minute build: a non-interactive result ends a working gap.
        result("30:45", "t3"),
        say("30:50", "Built."),
        // Idle for 25 minutes, then a new prompt.
        user("55:50", "thanks"),
        say("55:55", "You're welcome."),
    ])
}

#[test]
fn a_codex_rollout_folded_in_n_steps_equals_one_full_fold() {
    let full = codex_rollout();
    let whole = fold_in_steps(&CodexAdapter, &full, &[]);

    // What a Codex 0.147+ tool call IS: the semantic actions. The `exec` wrappers and `wait`
    // polls are transport; the exploration-detail carrier rides beside its three actions.
    let expected: BTreeMap<String, u64> = [
        ("exec_command", 1),
        ("__codex_explore_read", 1),
        ("__codex_explore_search", 1),
        ("__codex_explore_list", 1),
        ("update_plan", 1),
        ("apply_patch", 2),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(whole.calls, expected);
    assert_eq!(
        whole.raw_tool_uses,
        whole.total_calls() + 1,
        "exactly one ToolUse — the exploration detail — is not a call"
    );
    // 72s of short gaps before the answer, the 15-minute working gap in full, nothing for the
    // 40-minute idle gap ended by a prompt, then 16s after it.
    assert_eq!(whole.active_secs, 73.0 + 900.0 + 1.0 + 16.0);

    assert_every_growth_matches(&CodexAdapter, &full, &whole);
}

#[test]
fn a_claude_transcript_folded_in_n_steps_equals_one_full_fold() {
    let full = claude_transcript();
    let whole = fold_in_steps(&ClaudeAdapter, &full, &[]);
    assert_eq!(whole.total_calls(), 4);
    assert_eq!(whole.raw_tool_uses, 4, "every Claude ToolUse is a call");
    // 40s, then the question's 10-minute answer counts nothing, 5s, the 20-minute build in
    // full, 5s, the 25-minute idle gap nothing, 5s.
    assert_eq!(whole.active_secs, 40.0 + 5.0 + 1200.0 + 5.0 + 5.0);

    assert_every_growth_matches(&ClaudeAdapter, &full, &whole);
}
