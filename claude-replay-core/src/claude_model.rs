//! **Claude's transcript parser — the Layer 1 adapter** (mirrors `codex_model`). Holds
//! Claude Code's per-line tokenizer (`decode_line` / `tokenize`), the Claude `Shaping`
//! (`CLAUDE_SHAPING`, `claude_build_tool`, `apply_result`, turn grouping/coalescing), the
//! streaming parse entry points, sub-agent transcript loading, and the tool/attachment
//! decode helpers. The agent-neutral engine it feeds — the `Block` data model, the
//! `Replayer` / `replay` fold, the `SessionAccumulator` driver, and the shared message-handling
//! helpers — lives in [`crate::model`]. `parse_main` is the frozen `#[cfg(test)]` reference parser.

use crate::engine::message::{Message, QueueOpKind};
use crate::engine::path::relativize;
use crate::engine::replay::*;
use crate::engine::time::epoch_secs;
use crate::model::*;
use crate::Agent;
use serde_json::Value;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;

/// Parse Claude's `toolUseResult.status` / `<task-notification>` `<status>` string into the
/// shared [`AgentStatus`]. Claude-format-specific, so it lives in the Claude adapter (the
/// `AgentStatus` enum itself is agent-neutral).
fn status_from_str(s: &str) -> Option<AgentStatus> {
    Some(match s {
        "async_launched" => AgentStatus::AsyncLaunched,
        "completed" => AgentStatus::Completed,
        "failed" => AgentStatus::Failed,
        "killed" => AgentStatus::Killed,
        "stopped" => AgentStatus::Stopped,
        _ => return None,
    })
}

/// Is this `user` event injected/system content rather than a human turn?
/// `isMeta` marks instruction/skill/caveat bodies; `isCompactSummary` marks the
/// summary `/compact` writes back into the transcript.
fn is_injected_event(v: &Value) -> bool {
    v.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
        || v.get("isCompactSummary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// L1 classification of a plain-string `user` message into the **structured** message the
/// shared fold places — this is where Claude's raw wrappers (`<task-notification>`,
/// `<command-name>`, `<local-command-*>`, skill bodies, caveats) are parsed, so the fold
/// never sees them. Mirrors the retired `push_user_string`, but returns a `Message` instead
/// of pushing a block. `None` drops the message (caveat-only / phantom keystroke).
fn classify_user_string(s: &str, injected: bool) -> Option<Message> {
    // A skill instruction body: the fold nests it into the last `Skill` block; the fallback
    // (no skill block to nest into) is a system-note result, cleaned exactly as the old fold
    // did — an injected body is trimmed, a bare one is not.
    if is_skill_body(s) {
        let fallback = if injected {
            strip_caveat(s).trim().to_string()
        } else {
            strip_caveat(s)
        };
        return Some(Message::SkillBody {
            text: s.to_string(),
            fallback,
        });
    }
    if injected {
        let cleaned = strip_caveat(s);
        let cleaned = cleaned.trim();
        return (!cleaned.is_empty()).then(|| Message::SystemNote {
            text: cleaned.to_string(),
        });
    }
    // A background-execution `<task-notification>`: collapse to its one-line summary/status.
    if tag_inner(s, "task-notification").is_some() {
        if let Some(line) = tag_inner(s, "summary").or_else(|| tag_inner(s, "status")) {
            let line = line.trim();
            if !line.is_empty() {
                return Some(Message::SystemNote {
                    text: line.to_string(),
                });
            }
        }
    }
    // A slash command `<command-name>/foo</command-name>` (+ optional args / inline stdout).
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
        return Some(Message::Command {
            name: name.trim().to_string(),
            args,
            output,
        });
    }
    // A standalone stdout message — the fold attaches it to the command it follows.
    if let Some(o) = tag_inner(s, "local-command-stdout") {
        let o = o.trim().to_string();
        if o.is_empty() {
            return None;
        }
        return Some(Message::CommandStdout { text: o });
    }
    // Otherwise ordinary user prose; drop pure caveat noise / phantom keystrokes.
    let cleaned = strip_caveat(s);
    let has_visible = cleaned
        .chars()
        .any(|c| !c.is_whitespace() && !c.is_control());
    if has_visible {
        if is_skill_body(&cleaned) {
            return Some(Message::SystemNote { text: cleaned });
        }
        return Some(Message::UserText { text: cleaned });
    }
    None
}

/// L1 classification of a non-empty `text` item inside a `user` array — simpler than the
/// plain-string case (no command/notification parsing): a skill body nests, other injected
/// content is a system note, else it's a human turn.
fn classify_user_array_text(text: &str, injected: bool) -> Message {
    if is_skill_body(text) {
        Message::SkillBody {
            text: text.to_string(),
            fallback: text.to_string(),
        }
    } else if injected {
        Message::SystemNote {
            text: text.to_string(),
        }
    } else {
        Message::UserText {
            text: text.to_string(),
        }
    }
}

/// Injected/system content Claude flags at the event level (`isMeta`/`isCompactSummary`) —
/// folds as a system result block; caveat-only noise is dropped. Used by the frozen
/// reference parser [`parse_main`]; the streaming path uses [`classify_user_string`].
#[cfg(test)]
fn push_injected(s: &str, out: &mut Vec<Block>) {
    let cleaned = strip_caveat(s);
    let cleaned = cleaned.trim();
    if !cleaned.is_empty() {
        out.push(Block::ToolResult(cleaned.to_string()));
    }
}

/// Map a `TaskCreate`/`TaskUpdate` call input onto a structured task op (#15) — the
/// L1-only extraction (the built `ToolUse` block doesn't retain inputs). Any other
/// tool → `None`. Field names follow the harness's task-tool schema; `blockedBy` on
/// an update is treated as additive alongside `addBlockedBy`.
fn task_op(name: &str, id: &str, input: &Value) -> Option<crate::engine::tasks::TaskOp> {
    use crate::engine::tasks::TaskOp;
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(String::from);
    let list = |k: &str| -> Vec<String> {
        input
            .get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    match name {
        "TaskCreate" => Some(TaskOp::Create {
            tool_use_id: id.to_string(),
            subject: s("subject").unwrap_or_default(),
            description: s("description").unwrap_or_default(),
            active_form: s("activeForm").unwrap_or_default(),
            blocked_by: list("blockedBy"),
        }),
        "TaskUpdate" => Some(TaskOp::Update {
            task_id: s("taskId").unwrap_or_default(),
            status: s("status"),
            subject: s("subject"),
            description: s("description"),
            active_form: s("activeForm"),
            add_blocks: [list("addBlocks"), list("blocks")].concat(),
            add_blocked_by: [list("addBlockedBy"), list("blockedBy")].concat(),
        }),
        _ => None,
    }
}

/// Turn one plain-string `user` message into block(s) — a slash command becomes a
/// `Command`, a task-notification collapses to its summary, caveat noise is dropped, and
/// the rest is `UserText`. Used by the frozen reference parser [`parse_main`]; the streaming
/// path uses [`classify_user_string`]. `queue`/`suppress` mirror the engine's #52 op-less
/// delivery: prose matching a PENDING queued prompt pops it and collapses its marker.
#[cfg(test)]
fn push_user_string(
    s: &str,
    out: &mut Vec<Block>,
    queue: &mut Vec<QueueItem>,
    suppress: &mut Vec<BlockIndex>,
) {
    if tag_inner(s, "task-notification").is_some() {
        if let Some(line) = tag_inner(s, "summary").or_else(|| tag_inner(s, "status")) {
            let line = line.trim();
            if !line.is_empty() {
                out.push(Block::ToolResult(line.to_string()));
                return;
            }
        }
    }
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
    let cleaned = strip_caveat(s);
    let has_visible = cleaned
        .chars()
        .any(|c| !c.is_whitespace() && !c.is_control());
    if has_visible {
        if is_skill_body(&cleaned) {
            out.push(Block::ToolResult(cleaned));
        } else {
            // #52 op-less delivery, plain-string form (see the array-text arm).
            if let Some(pos) = queue.iter().position(|q| q.content == cleaned.trim()) {
                if let Some(mi) = queue.remove(pos).marker_idx {
                    suppress.push(mi);
                }
            }
            out.push(Block::UserText(cleaned));
        }
    }
}

pub(crate) fn tool_target(input: &Value, cwd: &str) -> String {
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
    // Task tools (#62): TaskUpdate/TaskGet name the task id (`TaskUpdate(#52)`, not
    // the bare `TaskUpdate()`); TaskCreate shows its SUBJECT — checked before the
    // generic `description` key, whose task-tool value is long prose that would
    // swamp the one-line header.
    if let Some(tid) = input.get("taskId").and_then(|v| v.as_str()) {
        return format!("#{tid}");
    }
    if let Some(s) = input.get("subject").and_then(|v| v.as_str()) {
        return s.replace('\n', " ");
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

// (Turn grouping is the shared, agent-neutral span coalescer now — see
// `crate::model::coalesce_spans` and `design/cc-activity-coalescing.md` (#57). The
// former per-assistant-message `group_turns`/`coalesce_activity_runs` pair rendered
// far more summary lines than Claude Code and was subsumed by it.)

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
/// live-tail path (small in-memory batches).
///
/// This in-memory batch entry runs the new two-layer engine — Layer 1 [`tokenize`]
/// (message log) then Layer 2 [`replay`] (the forward fold) — which is asserted
/// bit-identical to the (now frozen, test-only) `parse_main` — see
/// `replay_tokenize_matches_parse_main`. The large-file streaming path (the shared
/// `SessionAccumulator`, fed line-by-line by the batch parse and the live follower) runs the same
/// engine per line (M9), so production no longer touches `parse_main`.
#[cfg(test)]
pub(crate) fn parse(jsonl: &str) -> Vec<Block> {
    replay(&tokenize(jsonl.lines()), &mut Vec::new(), &CLAUDE_SHAPING)
}

/// Load each spawned sub-agent's child transcript (recursively) into its `SubAgent.blocks`,
/// so a spawn can be descended into and its subtree cost rolled up. All of a session's agents
/// — any depth — share one flat `<session>/subagents/` dir, so one dir resolves the whole
/// tree. No-op when the dir is absent. This is the enrichment behind `parse_session_enriched`
/// (the Claude adapter's `TranscriptAdapter::enrich`).
pub(crate) fn enrich_tree(path: &std::path::Path, blocks: &mut [Block]) {
    if let Some(dir) = subagents_dir(path) {
        enrich_subagents(blocks, &dir);
    }
}

/// Parse a transcript file into blocks WITHOUT loading sub-agent children — the raw pass
/// the adapter's `parse_path_timed` builds on. `enrich_tree` (the adapter's `enrich`, backing
/// `parse_session_enriched`) adds the children; that recursion reuses this so grandchildren
/// resolve against the same session `subagents/` dir.
fn parse_file(path: &std::path::Path) -> std::io::Result<Vec<Block>> {
    // Stream through the shared incremental fold in a single pass, one line resident, and keep
    // only the blocks (this sub-agent path doesn't need times or metrics).
    let mut b = crate::engine::builder::SessionAccumulator::new(Agent::Claude);
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    b.advance_reader(&mut reader)?;
    Ok(b.fold().0)
}

/// The `<project>/<sessionId>/subagents/` dir for a transcript at
/// `<project>/<sessionId>.jsonl`, if it exists on disk.
fn subagents_dir(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?.join(stem).join("subagents");
    dir.is_dir().then_some(dir)
}

/// The on-disk transcript for `agent_id` under the root session at `session_path`
/// (`<session>/subagents/agent-<id>.jsonl`), if it exists — the file a descended child is
/// live-tailed from. All of a session's agents (any depth) share this one flat dir.
pub fn subagent_file(session_path: &std::path::Path, agent_id: &str) -> Option<std::path::PathBuf> {
    let stem = session_path.file_stem()?.to_str()?;
    let f = session_path
        .parent()?
        .join(stem)
        .join("subagents")
        .join(format!("agent-{agent_id}.jsonl"));
    f.is_file().then_some(f)
}

/// Fill each `SubAgent` block's `blocks` (child transcript) + `subtree_cost` by parsing
/// `<sadir>/agent-<id>.jsonl`, recursing into grandchildren against the same `sadir`.
/// A missing child file (older session, a copied `.jsonl`) leaves `blocks` empty —
/// never a dead affordance.
fn enrich_subagents(blocks: &mut [Block], sadir: &std::path::Path) {
    for b in blocks.iter_mut() {
        if let Block::SubAgent(sa) = b {
            if sa.agent_id.is_empty() {
                continue;
            }
            let child = sadir.join(format!("agent-{}.jsonl", sa.agent_id));
            let Ok(mut cb) = parse_file(&child) else {
                continue;
            };
            enrich_subagents(&mut cb, sadir); // grandchildren (same flat dir)
                                              // The completion `<task-notification>` is the sole authority for terminal
                                              // status — a child file existing does NOT mean the agent finished (it keeps
                                              // growing while it runs). Upgrading to Completed here would hide a live agent
                                              // from `active`, so leave the status alone and only attach the transcript.
            sa.subtree_cost = subtree_cost(&child, &cb);
            sa.blocks = cb;
        }
    }
}

/// A sub-agent's own cost (from its transcript's metrics) plus all descendants'
/// rolled-up costs. `None` when neither is known.
fn subtree_cost(child_path: &std::path::Path, child_blocks: &[Block]) -> Option<UsdCost> {
    let own = std::fs::File::open(child_path).ok().and_then(|f| {
        crate::metrics::parse_reader_for(Agent::Claude, std::io::BufReader::new(f)).cost_usd
    });
    let desc: UsdCost = child_blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) => sa.subtree_cost,
            _ => None,
        })
        .sum();
    match own {
        Some(o) => Some(o + desc),
        None if desc > 0.0 => Some(desc),
        None => None,
    }
}

/// Fill a `tool_use` block's result fields (output / diff line numbers / read
/// count) from its matching `tool_result`'s `toolUseResult` metadata + text.
fn apply_result(block: &mut Block, txt: &str, tur: &Value) {
    match block {
        Block::ToolUse {
            name,
            output,
            patch,
            read_lines,
            ..
        } => {
            *output = tool_output(name, Some(tur), txt);
            *patch = parse_patch(tur);
            *read_lines = tur
                .pointer("/file/numLines")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize);
        }
        // An `Agent`/`Task` spawn's result: `toolUseResult` carries the agent id, the
        // launch status, and (sync) the inline result or (async) the output-file path.
        Block::SubAgent(sa) => {
            if let Some(aid) = tur.get("agentId").and_then(|v| v.as_str()) {
                sa.agent_id = aid.to_string();
            }
            if let Some(st) = tur
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(status_from_str)
            {
                sa.status = st;
            }
            if let Some(of) = tur.get("outputFile").and_then(|v| v.as_str()) {
                if !of.is_empty() {
                    sa.output_file = Some(of.to_string());
                }
            }
            // A synchronous spawn returns its answer inline; an async one returns only
            // the "async_launched" marker here (the real result arrives in the completion
            // notification / output-file), so it must NOT be captured as the result.
            let inline = tur
                .get("content")
                .and_then(|v| v.as_str())
                .filter(|c| !c.trim().is_empty())
                .or_else(|| {
                    Some(txt).filter(|t| {
                        let t = t.trim();
                        !t.is_empty() && t != "async_launched" && !t.starts_with("Launching")
                    })
                });
            if let Some(c) = inline {
                sa.result = Some(c.to_string());
            }
        }
        _ => {}
    }
}

/// **Layer 1 (Claude) — tokenize.** Map each JSONL line to zero or more canonical
/// [`Message`]s: pure line-shape classification, **no** back-patch, grouping, joins,
/// queue lifecycle, or turn stamping (those are the fold's job — see [`replay`]). The
/// session cwd is captured here (first non-empty `cwd` wins) purely to shape tool
/// targets, exactly as `parse_main` does. Streaming: one `Value` resident at a time.
///
/// This is the L1 half of `parse_main`; `replay(tokenize(x))` is asserted bit-identical
/// to `parse_main(x)` (see the tests). `parse_main` stays live and unchanged.
#[cfg(test)]
pub(crate) fn tokenize<S: AsRef<str>>(lines: impl Iterator<Item = S>) -> Vec<Message> {
    let mut msgs: Vec<Message> = Vec::new();
    let mut cwd = String::new();
    for line in lines {
        decode_line(line.as_ref(), &mut cwd, &mut msgs);
    }
    msgs
}

/// **Layer 1 — Claude decode, per line** (the streaming unit). Decode ONE raw transcript
/// line into 0+ canonical messages appended to `msgs`. `cwd` is threaded across lines (set
/// once from the first line that carries it) so tool targets relativize. `tokenize` is this
/// over every line; the streaming driver (M9) calls it one line at a time so no whole-file
/// `Vec<Message>` is ever built.
pub(crate) fn decode_line(line: &str, cwd: &mut String, msgs: &mut Vec<Message>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if cwd.is_empty() {
        if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
            *cwd = c.to_string();
        }
    }
    let ev_ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(epoch_secs);
    msgs.push(Message::LineStart(ev_ts));
    match v.get("type").and_then(|t| t.as_str()) {
        Some("assistant") => {
            let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                return;
            };
            for blk in content {
                match blk.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                            if !t.trim().is_empty() {
                                msgs.push(Message::AssistantText(t.to_string()));
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
                            msgs.push(Message::Thinking {
                                text: t.to_string(),
                                ts: ev_ts,
                            });
                        }
                    }
                    Some("tool_use") => {
                        let name = blk.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        let input = blk.get("input").cloned().unwrap_or(Value::Null);
                        let id = blk.get("id").and_then(|s| s.as_str()).unwrap_or("");
                        // Task-queue ops (#15): only L1 sees the call input, so the
                        // structured op is emitted here, alongside the ToolUse.
                        if let Some(op) = task_op(name, id, &input) {
                            msgs.push(Message::TaskOp(op));
                        }
                        // Raw fields only — the block is shaped in L2 via `claude_build_tool`.
                        msgs.push(Message::ToolUse {
                            id: id.to_string(),
                            name: name.to_string(),
                            input,
                            cwd: cwd.to_string(),
                        });
                        // #16: ExitPlanMode's input.plan is the FULL plan markdown — the
                        // only record of it for source-A plans. Surface it as a plan
                        // attachment (the same shape as `plan_file_reference`), content
                        // deferred and re-loaded from this line on demand.
                        if let Some(a) = exit_plan_attachment(blk) {
                            msgs.push(Message::Attachment(a));
                        }
                    }
                    _ => {}
                }
            }
        }
        Some("user") => {
            let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
            let injected = is_injected_event(&v);
            let Some(content) = v.pointer("/message/content") else {
                return;
            };
            if let Some(s) = content.as_str() {
                if let Some(m) = classify_user_string(s, injected) {
                    msgs.push(m);
                }
            } else if let Some(arr) = content.as_array() {
                for blk in arr {
                    match blk.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    msgs.push(classify_user_array_text(t, injected));
                                }
                            }
                        }
                        Some("image") => {
                            if let Some(att) = image_attachment(blk) {
                                msgs.push(Message::Attachment(att));
                            }
                        }
                        Some("tool_result") => {
                            let tid = blk
                                .get("tool_use_id")
                                .and_then(|s| s.as_str())
                                .unwrap_or("");
                            let txt = result_text(blk.get("content").unwrap_or(&Value::Null));
                            msgs.push(Message::ToolResult {
                                tool_use_id: tid.to_string(),
                                text: txt,
                                tur: tur.clone(),
                            });
                            if let Some(items) = blk.get("content").and_then(|c| c.as_array()) {
                                for item in items {
                                    if let Some(att) = image_attachment(item) {
                                        msgs.push(Message::Attachment(att));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("queue-operation") => {
            let content = v
                .get("content")
                .and_then(|c| c.as_str())
                .map(|c| c.to_string());
            let op = match v.get("operation").and_then(|o| o.as_str()) {
                Some("enqueue") => Some(QueueOpKind::Enqueue),
                Some("remove") => Some(QueueOpKind::Remove),
                Some("dequeue") => Some(QueueOpKind::Dequeue),
                _ => None,
            };
            if let Some(op) = op {
                // L1 parses Claude's formats here so the shared fold never sees them: an
                // agent completion `<task-notification>` becomes a structured `Completion`,
                // and `prose` pre-classifies whether the enqueue renders a visible marker.
                if op == QueueOpKind::Enqueue {
                    if let Some(c) = &content {
                        if is_agent_notification(c) {
                            msgs.push(Message::Completion {
                                tool_use_id: tag_inner(c, "tool-use-id")
                                    .unwrap_or_default()
                                    .to_string(),
                                task_id: tag_inner(c, "task-id").unwrap_or_default().to_string(),
                                status: tag_inner(c, "status").and_then(status_from_str),
                                description: tag_inner(c, "summary")
                                    .map(summary_description)
                                    .unwrap_or_default(),
                                result: tag_inner(c, "result")
                                    .map(str::trim)
                                    .filter(|r| !r.is_empty())
                                    .map(str::to_string),
                            });
                        }
                    }
                }
                let prose = content.as_deref().map(is_queue_prose).unwrap_or(false);
                msgs.push(Message::QueueOp { op, content, prose });
            }
        }
        Some("attachment") => {
            let a = v.get("attachment");
            let is_prompt = a.and_then(|a| a.get("type")).and_then(|t| t.as_str())
                == Some("queued_command")
                && a.and_then(|a| a.get("commandMode"))
                    .and_then(|m| m.as_str())
                    == Some("prompt");
            if is_prompt {
                if let Some(p) = a.and_then(|a| a.get("prompt")).and_then(|p| p.as_str()) {
                    if !p.trim().is_empty() {
                        msgs.push(Message::AttachmentPrompt {
                            text: p.to_string(),
                        });
                    }
                }
            } else if let Some(att) = a.and_then(attachment_from_event) {
                msgs.push(Message::Attachment(att));
            }
        }
        _ => {}
    }
}

fn claude_keep_orphan(t: &str) -> bool {
    !is_boilerplate(t)
}
fn claude_finish(blocks: Vec<Block>) -> Vec<Block> {
    crate::model::coalesce_spans(blocks)
}

/// Claude's `build_tool`: an `Agent`/`Task` spawn becomes a launched `SubAgent` block;
/// any other tool becomes a `ToolUse` with its path target relativized against `cwd` and
/// its diffs extracted. (Formerly inline in `decode_line`; lifted to L2 in M14.)
pub(crate) fn claude_build_tool(id: &str, name: &str, input: &Value, cwd: &str) -> Block {
    if name == "Agent" || name == "Task" {
        let s = |k: &str| {
            input
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let agent_type = {
            let t = s("subagent_type");
            if t.is_empty() {
                "agent".to_string()
            } else {
                t
            }
        };
        Block::SubAgent(SubAgent {
            agent_id: String::new(),
            tool_use_id: id.to_string(),
            agent_type,
            description: s("description"),
            prompt: s("prompt"),
            status: AgentStatus::Running,
            result: None,
            output_file: None,
            blocks: Vec::new(),
            subtree_cost: None,
        })
    } else {
        Block::ToolUse {
            name: name.to_string(),
            target: tool_target(input, cwd),
            diffs: extract_diffs(name, input),
            output: None,
            patch: None,
            read_lines: None,
        }
    }
}

/// Claude's shaping — the historical `parse_main` behavior.
pub(crate) const CLAUDE_SHAPING: Shaping = Shaping {
    build_tool: claude_build_tool,
    join_result: apply_result,
    keep_orphan: claude_keep_orphan,
    finish_turns: claude_finish,
};

/// Build blocks in order, streaming one line at a time. Nothing is dropped
/// or truncated. A `tool_use` is emitted immediately with an empty result; its
/// `tool_result` **back-patches** the already-emitted block in place (via
/// `tool_slot`: id → block index). A result whose tool_use hasn't been seen yet is a
/// genuine orphan, emitted inline (forward-references — a result physically before its
/// own tool_use — do not occur in real transcripts: 0/209 scanned). Keeps at most one
/// line's `Value` live. `_args` is unused (fold flags are resolved in `view`).
/// `user_times` is filled with one entry per emitted **user turn** (`UserText` /
/// `Command`), in order: the epoch-seconds of the event that produced it (`None`
/// when unparsable). Turn grouping never absorbs or reorders user blocks, so the
/// Nth user turn of the returned list is `user_times[N]`. Only the HTML export
/// consumes it; the TUI passes a throwaway vec.
///
/// **Frozen golden reference** (M9): production parses through the streaming engine (the shared
/// `SessionAccumulator` → `decode_line` + `Replayer`); this pre-engine parser is retained only
/// to pin `replay(tokenize(x))` bit-identical in `replay_tokenize_matches_parse_main`.
#[cfg(test)]
pub(crate) fn parse_main<S: AsRef<str>>(
    lines: impl Iterator<Item = S>,
    user_times: &mut Vec<Option<EpochSeconds>>,
) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    // Timestamp for blocks emitted by the event being processed, plus how far we've
    // stamped. Flushed at the next iteration so an early `continue` can't lose it.
    let mut pending_ts: Option<EpochSeconds> = None;
    let mut stamped = 0usize;
    // tool_use id -> index of its ToolUse block in `out`, for result back-patching.
    let mut tool_slot: HashMap<String, BlockIndex> = HashMap::new();
    // The session's cwd (from the transcript) — tool targets are shown relative to
    // it. CC records it on every event, so it's set from the first line, before any
    // tool_use; fall back to "" (absolute paths) if a tool_use somehow precedes it.
    let mut cwd = String::new();
    // Timestamp of the previous event line of ANY kind — CC's thinking clock (#57,
    // verified empirically): a thinking's duration is `its ts − this`, so a burst
    // right after the turn's own text measures from that text, not from the last
    // tool result.
    let mut prev_ts: Option<EpochSeconds> = None;
    // Messages the human submits mid-turn are recorded as `queue-operation` events
    // (not `user` events). Their lifecycle: `enqueue` → `remove`/`dequeue` (a FIFO
    // front pop) when the agent picks the prompt up → a `queued_command` **attachment**
    // at the consumption point that carries the prompt text. For the vast majority of
    // typed prompts that attachment is the ONLY record — CC never writes a standalone
    // `user` event — so we render `queued_command`/"prompt" attachments inline as user
    // turns (see the `attachment` arm below); that recovers messages that would
    // otherwise vanish. The `queue` here tracks only what is enqueued-but-not-yet-
    // consumed: a content-less pop drops the front, so on a settled transcript it nets
    // to empty. Whatever prose is still queued at the end (a live `-f` session mid-
    // flight) renders as pending user turns.
    let mut queue: Vec<QueueItem> = Vec::new();
    // Marker indices to drop collect in `suppress` and are filtered out after the
    // loop (safe — `tool_slot` is only used during the loop).
    let mut suppress: Vec<BlockIndex> = Vec::new();
    // Index of the most recent `Skill` tool_use block. The harness delivers a loaded
    // skill's instruction body as a following injected user message ("Base directory
    // for this skill: …"); we nest that body into this block so a skill load reads as
    // ONE collapsible unit named by the skill, instead of a loose result block beside it.
    let mut last_skill: Option<BlockIndex> = None;

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
        // Stamp the user turns the previous event emitted, then claim this event's ts
        // (the outgoing `pending_ts` is the previous line's — the thinking clock's zero).
        stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
        if pending_ts.is_some() {
            prev_ts = pending_ts;
        }
        pending_ts = ev_ts;
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
                                let duration_secs = match (ev_ts, prev_ts) {
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
                            // An `Agent`/`Task` spawn becomes a `SubAgent` block (agent hue,
                            // descendable). Its result back-patches the id/status/result.
                            if name == "Agent" || name == "Task" {
                                let s = |k: &str| {
                                    input
                                        .get(k)
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string()
                                };
                                let agent_type = {
                                    let t = s("subagent_type");
                                    if t.is_empty() {
                                        "agent".to_string()
                                    } else {
                                        t
                                    }
                                };
                                out.push(Block::SubAgent(SubAgent {
                                    agent_id: String::new(),
                                    tool_use_id: id.to_string(),
                                    agent_type,
                                    description: s("description"),
                                    prompt: s("prompt"),
                                    status: AgentStatus::Running,
                                    result: None,
                                    output_file: None,
                                    blocks: Vec::new(),
                                    subtree_cost: None,
                                }));
                            } else {
                                out.push(Block::ToolUse {
                                    name: name.to_string(),
                                    target: tool_target(&input, &cwd),
                                    diffs: extract_diffs(name, &input),
                                    output: None,
                                    patch: None,
                                    read_lines: None,
                                });
                            }
                            let idx = out.len() - 1;
                            if name == "Skill" {
                                last_skill = Some(idx);
                            }
                            if !id.is_empty() {
                                tool_slot.insert(id.to_string(), idx);
                            }
                            // #16 mirror: ExitPlanMode's inline plan surfaces as a
                            // plan attachment right after the call block.
                            if let Some(a) = exit_plan_attachment(blk) {
                                out.push(Block::Attachment(a));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                // The message-level toolUseResult metadata (shared by its result blocks).
                let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
                // `isMeta`/`isCompactSummary` events are injected system content, not
                // human turns — route their prose to a folded system block so it never
                // gets a turn/sidebar/sticky entry (see `push_injected`).
                let injected = is_injected_event(&v);
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                if let Some(s) = content.as_str() {
                    if is_skill_body(s) && attach_skill_body(&mut out, last_skill, s) {
                        // Nested into its `Skill` block above — no loose result block.
                    } else if injected {
                        push_injected(s, &mut out);
                    } else {
                        push_user_string(s, &mut out, &mut queue, &mut suppress);
                    }
                } else if let Some(arr) = content.as_array() {
                    for blk in arr {
                        match blk.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                    if !t.trim().is_empty() {
                                        if is_skill_body(t)
                                            && attach_skill_body(&mut out, last_skill, t)
                                        {
                                            // Nested into its `Skill` block above.
                                        } else if injected || is_skill_body(t) {
                                            out.push(Block::ToolResult(t.to_string()));
                                        } else {
                                            // #52 op-less delivery: a user message matching a
                                            // PENDING queued prompt is that prompt arriving.
                                            if let Some(pos) =
                                                queue.iter().position(|q| q.content == t.trim())
                                            {
                                                if let Some(mi) = queue.remove(pos).marker_idx {
                                                    suppress.push(mi);
                                                }
                                            }
                                            out.push(Block::UserText(t.to_string()));
                                        }
                                    }
                                }
                            }
                            // A pasted image (a top-level image block in the prompt).
                            Some("image") => {
                                if let Some(att) = image_attachment(blk) {
                                    out.push(Block::Attachment(att));
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
                                } else if !txt.trim().is_empty() && !is_boilerplate(&txt) {
                                    // No tool_use seen yet — a genuine orphan, shown inline.
                                    out.push(Block::ToolResult(txt));
                                }
                                // A tool result may also carry image(s) (e.g. reading a
                                // screenshot) — surface each as a downloadable attachment.
                                if let Some(items) = blk.get("content").and_then(|c| c.as_array()) {
                                    for item in items {
                                        if let Some(att) = image_attachment(item) {
                                            out.push(Block::Attachment(att));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Human input submitted mid-turn: queued, not yet a `user` event. We model
            // the WHOLE queue — prose prompts AND background `<task-notification>`s — so
            // a content-less `dequeue`/`remove` (a FIFO front pop) lands on the right
            // entry. A prose `enqueue` also emits a `⧗ queued:` marker in place; a pop
            // that finds the prompt was picked up with no agent work in between marks
            // that marker for suppression (immediate → the `❯` turn alone suffices).
            Some("queue-operation") => {
                let content = v.get("content").and_then(|c| c.as_str());
                match v.get("operation").and_then(|o| o.as_str()) {
                    Some("enqueue") => {
                        if let Some(c) = content {
                            if is_agent_notification(c) {
                                // Render the completion as its OWN event at this
                                // position (the spawn stays "launched" up where it was
                                // created). Type is copied from the matching spawn in a
                                // post-enrich pass (`stamp_agent_done_types`).
                                let status = tag_inner(c, "status")
                                    .and_then(status_from_str)
                                    .unwrap_or(AgentStatus::Completed);
                                let description = tag_inner(c, "summary")
                                    .map(summary_description)
                                    .unwrap_or_default();
                                let result = tag_inner(c, "result")
                                    .map(str::trim)
                                    .filter(|r| !r.is_empty())
                                    .map(str::to_string);
                                let agent_id = tag_inner(c, "task-id")
                                    .or_else(|| tag_inner(c, "tool-use-id"))
                                    .unwrap_or_default()
                                    .to_string();
                                out.push(Block::AgentDone {
                                    agent_id,
                                    agent_type: String::new(),
                                    description,
                                    status,
                                    result,
                                });
                            }
                            let is_prose = is_queue_prose(c);
                            let marker_idx = if is_prose {
                                out.push(Block::QueueEvent {
                                    text: c.trim().to_string(),
                                });
                                Some(out.len() - 1)
                            } else {
                                None
                            };
                            queue.push(QueueItem {
                                content: c.trim().to_string(),
                                marker_idx,
                            });
                        }
                    }
                    Some("remove") | Some("dequeue") => {
                        let popped = match content.map(str::trim) {
                            Some(c) => queue
                                .iter()
                                .position(|q| q.content == c)
                                .map(|i| queue.remove(i)),
                            None if !queue.is_empty() => Some(queue.remove(0)), // FIFO front pop
                            None => None,
                        };
                        // #52: a popped prompt's marker ALWAYS collapses — delivered (dequeue)
                        // or withdrawn (remove), Claude Code shows only the one message.
                        if let Some(item) = popped {
                            if let Some(mi) = item.marker_idx {
                                suppress.push(mi);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // The authoritative record of a consumed mid-turn prompt. CC emits a
            // `queued_command` attachment at the moment the agent picks the prompt up,
            // grouped with the tool-result-carrying `user` event of the running turn —
            // and for typed prompts this is usually the ONLY place the text survives
            // (no standalone `user` event is ever written). Render the human ones
            // (`commandMode == "prompt"`; `"task-notification"` is background noise) as
            // a user turn right here, so they land in chronological order at the point
            // they took effect.
            Some("attachment") => {
                let a = v.get("attachment");
                let is_prompt = a.and_then(|a| a.get("type")).and_then(|t| t.as_str())
                    == Some("queued_command")
                    && a.and_then(|a| a.get("commandMode"))
                        .and_then(|m| m.as_str())
                        == Some("prompt");
                if is_prompt {
                    if let Some(p) = a.and_then(|a| a.get("prompt")).and_then(|p| p.as_str()) {
                        if !p.trim().is_empty() {
                            // #52 op-less delivery, attachment form (see above).
                            if let Some(pos) = queue.iter().position(|q| q.content == p.trim()) {
                                if let Some(mi) = queue.remove(pos).marker_idx {
                                    suppress.push(mi);
                                }
                            }
                            out.push(Block::UserText(p.to_string()));
                        }
                    }
                } else if let Some(att) = a.and_then(attachment_from_event) {
                    // A file/plan/edited/compact attachment — surface it so the reader
                    // can download the embedded content or reveal the path (see
                    // `attachment_from_event`). Other attachment types (listings,
                    // reminders, deltas) are harness bookkeeping and stay dropped.
                    out.push(Block::Attachment(att));
                }
            }
            _ => {}
        }
    }
    stamp_user_turns(&out, &mut stamped, pending_ts, user_times);
    // Give each `AgentDone` event its spawn's `agent_type`/`agent_id` (the notification
    // carries only status/summary/result), resolving its id from the spawn's `tool_use_id`
    // when the notification keyed by `tool-use-id` rather than `task-id`. NO status
    // back-patch: the spawn keeps its launch status; the `sub_agents` index derives the
    // terminal status from the AgentDone event (two durable events). MUST run before the
    // `suppress` filter below removes blocks. A no-op when there are no `AgentDone` blocks.
    {
        let mut by_id: HashMap<String, (String, String)> = HashMap::new(); // id/toolid → (agent_id, type)
        for b in out.iter() {
            if let Block::SubAgent(sa) = b {
                let v = (sa.agent_id.clone(), sa.agent_type.clone());
                if !sa.agent_id.is_empty() {
                    by_id.insert(sa.agent_id.clone(), v.clone());
                }
                if !sa.tool_use_id.is_empty() {
                    by_id.insert(sa.tool_use_id.clone(), v);
                }
            }
        }
        for b in out.iter_mut() {
            if let Block::AgentDone {
                agent_id,
                agent_type,
                ..
            } = b
            {
                if let Some((real_id, ty)) = by_id.get(agent_id.as_str()) {
                    *agent_type = ty.clone();
                    *agent_id = real_id.clone();
                }
            }
        }
    }
    // Drop the `⧗ queued:` markers of prompts picked up immediately (no agent work
    // between submit and pickup) — their `❯` turn alone conveys them. Prompts still
    // queued at the end keep their marker (a live `-f` session's in-flight input).
    // Safe here: `tool_slot` is finished, and this runs before turn grouping
    // so surviving markers keep their positions.
    let _ = queue; // consumed via `suppress` during the loop; nothing to flush
    if !suppress.is_empty() {
        let drop: HashSet<usize> = suppress.into_iter().collect();
        let mut i = 0usize;
        out.retain(|_| {
            let keep = !drop.contains(&i);
            i += 1;
            keep
        });
    }
    crate::model::coalesce_spans(out)
}

/// Build an [`Attachment`] from a `type:"attachment"` event's inner `attachment`
/// object, for the types that carry a file/plan worth surfacing. Returns `None` for
/// harness bookkeeping (listings, reminders, deltas, plan-mode toggles) and for
/// `queued_command` (rendered as a turn elsewhere). Content-bearing types (`file`/`plan`) get
/// a [`Deferred`](AttachmentContent::Deferred) locator — the **bytes are never built here**;
/// [`load_attachment_from_event`] re-extracts them on demand. Path-only types
/// (`edited_text_file`/`compact_file_reference`) get [`AttachmentContent::None`] (reveal). The
/// `at`/`index` in `Deferred` are placeholders (0); `SessionAccumulator::advance_at` stamps the
/// real byte offset one level up (where it's known).
fn attachment_from_event(a: &Value) -> Option<Attachment> {
    fn basename(p: &str) -> String {
        p.rsplit('/').next().unwrap_or(p).to_string()
    }
    let s = |k: &str| a.get(k).and_then(|x| x.as_str());
    match a.get("type").and_then(|t| t.as_str())? {
        // Full attached-file bytes, embedded → downloadable (loaded on demand).
        "file" => {
            let f = a.get("content")?.get("file")?;
            f.get("content").and_then(|c| c.as_str())?; // require content, but never build it
            let path = f
                .get("filePath")
                .and_then(|p| p.as_str())
                .or_else(|| s("filename"));
            let name = s("displayPath")
                .map(str::to_string)
                .or_else(|| path.map(basename))?;
            Some(Attachment {
                kind: AttachmentKind::File,
                name,
                path: path.map(str::to_string),
                content: AttachmentContent::Deferred { at: 0, index: 0 },
            })
        }
        // Full plan markdown, embedded and not shown inline anywhere → downloadable.
        "plan_file_reference" => {
            s("planContent")?; // require plan content, but never build it
            let path = s("planFilePath");
            Some(Attachment {
                kind: AttachmentKind::Plan,
                name: path.map(basename).unwrap_or_else(|| "plan.md".to_string()),
                path: path.map(str::to_string),
                content: AttachmentContent::Deferred { at: 0, index: 0 },
            })
        }
        // An in-editor file — its inline `snippet` is truncated, so reveal the real file.
        "edited_text_file" => {
            let path = s("filename")?;
            Some(Attachment {
                kind: AttachmentKind::Edited,
                name: basename(path),
                path: Some(path.to_string()),
                content: AttachmentContent::None,
            })
        }
        // A bare pointer to a file that was in context → reveal.
        "compact_file_reference" => {
            let path = s("filename")?;
            let name = s("displayPath")
                .map(str::to_string)
                .unwrap_or_else(|| basename(path));
            Some(Attachment {
                kind: AttachmentKind::Ref,
                name,
                path: Some(path.to_string()),
                content: AttachmentContent::None,
            })
        }
        _ => None,
    }
}

/// The load-time twin of [`attachment_from_event`]: re-extract the embedded **bytes** for a
/// content-bearing `attachment` event (`file` / `plan`), as a [`LoadedAttachment`]. Returns
/// `None` for path-only / bookkeeping types (they carry no bytes). Kept structurally parallel
/// to [`attachment_from_event`] so the two never diverge on which types are loadable.
fn load_attachment_from_event(a: &Value) -> Option<LoadedAttachment> {
    let s = |k: &str| a.get(k).and_then(|x| x.as_str());
    match a.get("type").and_then(|t| t.as_str())? {
        "file" => {
            let content = a.get("content")?.get("file")?.get("content")?.as_str()?;
            Some(LoadedAttachment::Text(content.to_string()))
        }
        "plan_file_reference" => Some(LoadedAttachment::Text(s("planContent")?.to_string())),
        _ => None,
    }
}

/// Build an image [`Attachment`] from an `{type:"image", source:{type:"base64",…}}`
/// content block (a pasted image, or a tool result that returned one). Images are not
/// `attachment` events — they ride inside message/tool-result content — so this is a
/// separate path from [`attachment_from_event`]. `None` for non-image / non-base64.
fn image_attachment(blk: &Value) -> Option<Attachment> {
    let src = image_source(blk)?;
    let mime = image_mime(src);
    let ext = mime
        .rsplit('/')
        .next()
        .filter(|e| !e.is_empty())
        .unwrap_or("png");
    Some(Attachment {
        kind: AttachmentKind::Image,
        name: format!("image.{ext}"),
        path: None,
        // The base64 bytes are NEVER built here — `load_image_attachment` re-extracts them on
        // demand. `at`/`index` are placeholders; `advance_at` stamps the real byte offset.
        content: AttachmentContent::Deferred { at: 0, index: 0 },
    })
}

/// The `{type:"base64",…}` source of an `image` content block, or `None` if `blk` isn't a
/// base64 image. The shared shape check for both the metadata ([`image_attachment`]) and the
/// bytes ([`load_image_attachment`]) paths.
fn image_source(blk: &Value) -> Option<&Value> {
    if blk.get("type").and_then(|t| t.as_str()) != Some("image") {
        return None;
    }
    let src = blk.get("source")?;
    (src.get("type").and_then(|t| t.as_str()) == Some("base64")).then_some(src)
}

fn image_mime(src: &Value) -> String {
    src.get("media_type")
        .and_then(|m| m.as_str())
        .unwrap_or("image/png")
        .to_string()
}

/// The load-time twin of [`image_attachment`]: re-extract the embedded base64 **bytes** for an
/// `image` content block as a [`LoadedAttachment`]. `None` for non-image / non-base64 blocks.
fn load_image_attachment(blk: &Value) -> Option<LoadedAttachment> {
    let src = image_source(blk)?;
    let b64 = src.get("data").and_then(|d| d.as_str())?.to_string();
    Some(LoadedAttachment::Base64 {
        mime: image_mime(src),
        b64,
    })
}

/// Load the `index`-th content-bearing attachment embedded on ONE raw transcript `line` — the
/// on-demand byte-fetch backing [`crate::Transcript::load_attachment`]. Walks the line's JSON
/// in the SAME order [`decode_line`] emits its attachments (user-message images, then each
/// tool-result's images; or the sole `attachment`-event file/plan), so `index` lines up with
/// the ordinal `advance_at` stamped into the `Deferred` locator. Only one [`LoadedAttachment`]
/// is alive at any instant (each non-matching candidate is built then dropped as we count),
/// keeping the load O(1) in memory.
pub(crate) fn nth_loaded_attachment(line: &str, index: usize) -> Option<LoadedAttachment> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let v = serde_json::from_str::<Value>(line).ok()?;
    // Count content-bearing attachments in document order; return the `index`-th one. Building
    // each candidate transiently (then dropping non-matches) keeps a single one resident.
    let mut seen = 0usize;
    let mut take = |la: Option<LoadedAttachment>| -> Option<Option<LoadedAttachment>> {
        // Outer Some ⇒ "stop, this is our answer"; inner Option carries the (matched) content.
        match la {
            Some(la) => {
                if seen == index {
                    Some(Some(la))
                } else {
                    seen += 1;
                    None // drop this candidate; keep counting
                }
            }
            None => None,
        }
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("user") => {
            let content = v.pointer("/message/content").and_then(|c| c.as_array())?;
            for blk in content {
                match blk.get("type").and_then(|t| t.as_str()) {
                    Some("image") => {
                        if let Some(hit) = take(load_image_attachment(blk)) {
                            return hit;
                        }
                    }
                    Some("tool_result") => {
                        if let Some(items) = blk.get("content").and_then(|c| c.as_array()) {
                            for item in items {
                                if let Some(hit) = take(load_image_attachment(item)) {
                                    return hit;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        Some("attachment") => v
            .get("attachment")
            .and_then(load_attachment_from_event)
            .filter(|_| index == 0),
        // #16: an assistant line's ExitPlanMode call carries the plan body inline.
        Some("assistant") => {
            let content = v.pointer("/message/content").and_then(|c| c.as_array())?;
            for blk in content {
                if blk.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && exit_plan_attachment(blk).is_some()
                {
                    let plan = blk.pointer("/input/plan").and_then(|p| p.as_str())?;
                    if let Some(hit) = take(Some(LoadedAttachment::Text(plan.to_string()))) {
                        return hit;
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// The plan [`Attachment`] for an `ExitPlanMode` tool_use content item carrying a
/// non-empty `input.plan` (#16) — `None` otherwise. Shared by the L1 emission, the
/// frozen `parse_main` mirror, and the deferred loader's counting walk (so their
/// ordinals agree).
fn exit_plan_attachment(blk: &Value) -> Option<Attachment> {
    if blk.get("name").and_then(|n| n.as_str()) != Some("ExitPlanMode") {
        return None;
    }
    let plan = blk.pointer("/input/plan").and_then(|p| p.as_str())?;
    if plan.trim().is_empty() {
        return None;
    }
    Some(Attachment {
        kind: AttachmentKind::Plan,
        name: "plan.md".to_string(),
        path: None,
        content: AttachmentContent::Deferred { at: 0, index: 0 },
    })
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

/// Extract the quoted description from a completion `<summary>` like
/// `Agent "Design the parser" finished` → `Design the parser`. Falls back to the whole
/// trimmed summary when there's no quoted span.
fn summary_description(summary: &str) -> String {
    if let (Some(a), Some(b)) = (summary.find('"'), summary.rfind('"')) {
        if b > a {
            return summary[a + 1..b].to_string();
        }
    }
    summary.trim().to_string()
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

/// Is this string an agent-completion `<task-notification>` — `summary` "Agent \"…\"
/// finished" with a `status` — as opposed to a background-`Bash` or `Monitor` one?
/// (Their task-id namespaces differ too: agents `a…`, background `b…`.)
fn is_agent_notification(s: &str) -> bool {
    tag_inner(s, "status").is_some()
        && tag_inner(s, "summary")
            .map(|sm| sm.trim_start().starts_with("Agent \""))
            .unwrap_or(false)
}

/// A queued message worth showing as a pending human turn — genuine prose, not a
/// background `<task-notification>`, an interrupt marker, or blank input.
fn is_queue_prose(s: &str) -> bool {
    let t = s.trim_start();
    !t.is_empty()
        && !t.starts_with("<task-notification>")
        && !t.starts_with("[Request interrupted")
        && t.chars().any(|c| !c.is_whitespace() && !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(blocks: &[Block]) -> Vec<&'static str> {
        blocks.iter().map(fold_key).collect()
    }

    /// Injected system content — a skill/command instruction body (`isMeta`) or a
    /// `/compact` continuation summary (`isCompactSummary`) — is NOT a human turn.
    /// It must fold as a system block, never a `❯` UserText (which would give it a
    /// phantom sidebar/sticky turn entry). A genuine user message between them still
    /// reads as a turn, so the turn count stays correct.
    #[test]
    fn injected_meta_and_compact_summary_are_not_user_turns() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}
{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"# /loop — schedule a recurring or self-paced prompt\nParse the input below…"}}
{"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"This session is being continued from a previous conversation…"}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}
"##;
        let blocks = parse(jsonl);
        // Two genuine turns; the injected pair folds as tool_result, not user.
        assert_eq!(
            kinds(&blocks),
            vec!["user", "tool_result", "tool_result", "user"],
            "{blocks:?}"
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, Block::UserText(_)))
                .count(),
            2,
            "only the two human messages are turns"
        );
    }

    /// #16: an `ExitPlanMode` call's `input.plan` — the only record of a source-A
    /// plan — surfaces as a plan attachment right after the call block, and its body
    /// re-loads from the line on demand (the Deferred locator's ordinal 0).
    #[test]
    fn exit_plan_mode_plan_becomes_a_loadable_attachment() {
        let line = r##"{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"tool_use","id":"ep1","name":"ExitPlanMode","input":{"plan":"# The plan\n1. do the thing"}}]}}"##;
        let blocks = parse(line);
        assert_eq!(kinds(&blocks), vec!["tool", "attachment"], "{blocks:?}");
        let Block::Attachment(a) = &blocks[1] else {
            panic!("expected the plan attachment: {blocks:?}");
        };
        assert_eq!(a.kind, crate::model::AttachmentKind::Plan);
        assert_eq!(a.name, "plan.md");
        // The body loads back from the raw line (what the builder's stamped locator does).
        match nth_loaded_attachment(line, 0) {
            Some(crate::model::LoadedAttachment::Text(t)) => {
                assert_eq!(t, "# The plan\n1. do the thing");
            }
            other => panic!("plan body did not load: {other:?}"),
        }
        // An empty plan emits no attachment.
        let none = parse(
            r##"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"ep2","name":"ExitPlanMode","input":{"plan":"  "}}]}}"##,
        );
        assert_eq!(kinds(&none), vec!["tool"], "{none:?}");
    }

    /// The two-tier queue model (#52): a prose `enqueue` emits a `⧗ queued:` marker
    /// (`QueueEvent`) that lives only while its prompt is PENDING. ANY pop — a
    /// content-less FIFO front pop (`dequeue`), a content-named `remove`, or an
    /// op-less delivery (a user message whose text matches the pending content) —
    /// collapses it: Claude Code shows only the one delivered message, even for
    /// type-ahead with agent work in between. A prompt still queued at the end keeps
    /// its marker (live in-flight input). The interleaved background
    /// `<task-notification>` is tracked (no marker) so a front pop lands on it, not
    /// on a real prompt.
    #[test]
    fn queue_markers_collapse_on_any_pop_and_survive_only_while_pending() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up immediately"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:02.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:03.000Z","content":"picked up after a gap"}
{"type":"assistant","timestamp":"2026-06-30T03:00:04.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:05.000Z","content":"<task-notification>\nbg\n</task-notification>"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:06.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:07.000Z","content":"delivered sans op"}
{"type":"user","timestamp":"2026-06-30T03:00:08.000Z","message":{"content":"delivered sans op"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:09.000Z","content":"still waiting"}
"##;
        let blocks = parse(jsonl);
        // "picked up immediately": enqueue→dequeue → marker dropped.
        // "picked up after a gap": popped by the second dequeue despite the Bash in
        //   between (type-ahead) → marker dropped too — the #52 fix.
        // "delivered sans op": no dequeue was ever written; the matching user message
        //   IS the delivery → marker dropped, one user turn.
        // "still waiting": never popped → marker kept. The task-notification: no marker.
        let markers: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::QueueEvent { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["still waiting"], "{blocks:?}");
        // Real user turns are unaffected; the op-less delivery renders exactly once.
        let users: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::UserText(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users, vec!["real turn", "delivered sans op"], "{blocks:?}");
    }

    /// A mid-turn prompt is usually recorded ONLY as a `queued_command` attachment at
    /// the point the agent consumes it (no standalone `user` event is ever written), so
    /// we render the human ones (`commandMode == "prompt"`) as a user turn in place —
    /// keeping the true chronological order — and skip `task-notification`s. Losing
    /// these would drop real messages the human typed.
    #[test]
    fn queued_command_attachment_renders_as_a_turn_in_order() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"first turn"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>bg</task-notification>"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"mid-turn interjection"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"text","text":"ok"}]}}
{"type":"user","timestamp":"2026-06-30T03:00:06.000Z","message":{"content":"last turn"}}
"##;
        let blocks = parse(jsonl);
        let users: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::UserText(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        // The human interjection appears as a turn between "first turn" and "last turn";
        // the task-notification attachment is not a turn.
        assert_eq!(
            users,
            vec!["first turn", "mid-turn interjection", "last turn"],
            "{blocks:?}"
        );
    }

    /// The four content-bearing attachment types surface as `Block::Attachment`:
    /// `file`/`plan` carry embedded text (downloadable → a `Deferred` locator), while
    /// `edited_text_file`/`compact_file_reference` are path-only (reveal → `content:
    /// None`). Bookkeeping attachments (e.g. `skill_listing`) stay dropped. The bytes are
    /// never resident — only a locator — so we re-load them via `nth_loaded_attachment`.
    #[test]
    fn attachment_events_surface_with_download_vs_reveal() {
        let jsonl = r##"
{"type":"attachment","timestamp":"2026-06-30T03:00:00.000Z","attachment":{"type":"file","filename":"/w/backlog.md","displayPath":"backlog.md","content":{"type":"text","file":{"filePath":"/w/backlog.md","content":"# Backlog\nitem"}}}}
{"type":"attachment","timestamp":"2026-06-30T03:00:01.000Z","attachment":{"type":"plan_file_reference","planFilePath":"/p/plan-x.md","planContent":"# Plan\nstep 1"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:02.000Z","attachment":{"type":"edited_text_file","filename":"/w/src/main.rs","snippet":"1\tfn main(){}"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"compact_file_reference","filename":"/w/src/lib.rs","displayPath":"src/lib.rs"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"skill_listing","content":"noise"}}
"##;
        let blocks = parse(jsonl);
        let atts: Vec<(&str, &str, bool, Option<&str>)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Attachment(a) => Some((
                    a.kind.as_str(),
                    a.name.as_str(),
                    // Downloadable ⇒ a `Deferred` locator; path-only ⇒ `None`.
                    matches!(a.content, AttachmentContent::Deferred { .. }),
                    a.path.as_deref(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            atts,
            vec![
                ("file", "backlog.md", true, Some("/w/backlog.md")),
                ("plan", "plan-x.md", true, Some("/p/plan-x.md")),
                ("edited", "main.rs", false, Some("/w/src/main.rs")),
                ("ref", "src/lib.rs", false, Some("/w/src/lib.rs")),
            ],
            "{blocks:?}"
        );
        // No bytes are held resident — the block carries only a locator. Re-load the `file`
        // body on demand from its own transcript line (index 0).
        let file_line = jsonl
            .lines()
            .find(|l| l.contains("\"type\":\"file\""))
            .unwrap();
        assert_eq!(
            nth_loaded_attachment(file_line, 0),
            Some(LoadedAttachment::Text("# Backlog\nitem".into()))
        );
    }

    /// Base64 images surface as downloadable `Block::Attachment`s from both paths: a
    /// top-level image block in a prompt, and an image inside a tool result (e.g.
    /// reading a screenshot). Images ride in message/tool-result content, NOT in
    /// `attachment` events.
    #[test]
    fn base64_images_surface_from_prompt_and_tool_result() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"text","text":"look at this"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"Zm9v"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"/w/shot.png"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"r1","content":[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"YmFy"}}]}]}}
"##;
        let blocks = parse(jsonl);
        // The blocks carry only locators (name + a `Deferred` marker) — no base64 resident.
        let imgs: Vec<(&str, bool)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Attachment(a) if a.kind == AttachmentKind::Image => Some((
                    a.name.as_str(),
                    matches!(a.content, AttachmentContent::Deferred { .. }),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            imgs,
            vec![("image.png", true), ("image.jpeg", true)],
            "{blocks:?}"
        );
        // The mime/bytes are re-loaded on demand from each image's own line (index 0).
        let prompt_line = jsonl.lines().find(|l| l.contains("look at this")).unwrap();
        assert_eq!(
            nth_loaded_attachment(prompt_line, 0),
            Some(LoadedAttachment::Base64 {
                mime: "image/png".into(),
                b64: "Zm9v".into()
            })
        );
        let result_line = jsonl.lines().find(|l| l.contains("tool_result")).unwrap();
        assert_eq!(
            nth_loaded_attachment(result_line, 0),
            Some(LoadedAttachment::Base64 {
                mime: "image/jpeg".into(),
                b64: "YmFy".into()
            })
        );
    }

    /// A user message with no visible character — only whitespace or a control
    /// byte like `\x11` (a stray Ctrl-Q keystroke) — is a phantom, not a turn.
    #[test]
    fn control_only_user_message_is_dropped() {
        let jsonl = "\
{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:00.000Z\",\"message\":{\"content\":\"\u{11}\"}}
{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:01.000Z\",\"message\":{\"content\":\"real\"}}
";
        let blocks = parse(jsonl);
        assert_eq!(kinds(&blocks), vec!["user"], "{blocks:?}");
        assert!(matches!(&blocks[0], Block::UserText(t) if t == "real"));
    }

    /// A span absorbs the activity tools around its thinking bursts (#57) and
    /// carries a duration = (each burst's timestamp − the previous event's timestamp).
    #[test]
    fn thinking_groups_preceding_tools_with_duration() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:12.000Z","message":{"content":[{"type":"thinking","thinking":"hmm let me consider"}]}}
"#;
        let blocks = parse(jsonl);
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

    /// Edit/Write tools are NOT absorbed into a span (CC shows their diffs expanded,
    /// and they BREAK the span); only transient activity tools (Bash/Read/…) fold in.
    #[test]
    fn edit_stays_expanded_next_to_thinking() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"thinking","thinking":"ok"}]}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(
            kinds(&blocks),
            vec!["user", "edit", "thinking"],
            "{blocks:?}"
        );
    }

    /// The #57 span rule end-to-end (`design/cc-activity-coalescing.md`): ALL
    /// consecutive thinking bursts + activity tools between two visible outputs merge
    /// into ONE `Thinking` block — across assistant messages, across tool results, and
    /// straight over a transparent attachment — with the bursts' durations SUMMED
    /// (each = its ts − the previous event's ts, so the burst 4s after the turn's own
    /// text contributes 4, not its distance from the last tool result). Task-
    /// bookkeeping tools (TaskUpdate & co) break the span like CC (which renders them
    /// invisibly; we keep their block). A LONE activity tool folds too.
    #[test]
    fn spans_merge_between_visible_outputs_and_break_on_cc_breakers() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:10.000Z","message":{"content":[{"type":"thinking","thinking":"burst one"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:12.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"cargo build"}}]}}
{"type":"user","timestamp":"2026-06-30T03:01:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"ok"}]}}
{"type":"attachment","timestamp":"2026-06-30T03:01:02.000Z","attachment":{"type":"edited_text_file","filename":"/w/x.rs","snippet":"1\tfn x(){}"}}
{"type":"assistant","timestamp":"2026-06-30T03:01:07.000Z","message":{"content":[{"type":"thinking","thinking":"burst two"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:01:08.000Z","message":{"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"/w/a.rs"}}]}}
{"type":"user","timestamp":"2026-06-30T03:02:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"r1","content":"1\tsrc"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:02:05.000Z","message":{"content":[{"type":"text","text":"VISIBLE."}]}}
{"type":"assistant","timestamp":"2026-06-30T03:02:09.000Z","message":{"content":[{"type":"thinking","thinking":"after text"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:02:10.000Z","message":{"content":[{"type":"tool_use","id":"t1","name":"TaskUpdate","input":{"taskId":"9","status":"completed"}}]}}
{"type":"user","timestamp":"2026-06-30T03:03:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Updated task #9 status"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:03:04.000Z","message":{"content":[{"type":"tool_use","id":"b2","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:04:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b2","content":"src"}]}}
"#;
        let blocks = parse(jsonl);
        // Span 1 carries across the attachment (which renders in place, un-split);
        // the TaskUpdate splits "after text" from the lone trailing Bash, which still
        // folds into a tools-only span.
        assert_eq!(
            kinds(&blocks),
            vec![
                "user",
                "attachment",
                "thinking",
                "assistant",
                "thinking",
                "tool",
                "thinking"
            ],
            "{blocks:?}"
        );
        let Block::Thinking {
            text,
            duration_secs,
            tools,
        } = &blocks[2]
        else {
            panic!("span 1 missing: {blocks:?}");
        };
        // 10s (03:00:10−03:00:00) + 5s (03:01:07−03:01:02, measured from the
        // attachment line — the previous event) = 15s.
        assert_eq!(*duration_secs, Some(15), "summed burst durations");
        assert_eq!(
            text, "burst one\n\nburst two",
            "burst texts join blank-line separated"
        );
        assert_eq!(tools.len(), 2, "Bash + Read folded into the one span");
        // The post-text burst measures from the TEXT event (4s), not the last
        // tool result (65s) — CC's thinking clock.
        let Block::Thinking { duration_secs, .. } = &blocks[4] else {
            panic!("post-text span missing: {blocks:?}");
        };
        assert_eq!(*duration_secs, Some(4), "previous-event clock, not trigger");
        // The lone trailing Bash folded into a tools-only span.
        let Block::Thinking {
            text,
            duration_secs,
            tools,
        } = &blocks[6]
        else {
            panic!("lone-activity span missing: {blocks:?}");
        };
        assert!(text.is_empty() && duration_secs.is_none());
        assert_eq!(tools.len(), 1, "a lone activity tool still folds");
    }

    /// A skill load is ONE collapsible unit: the `Skill` tool_use names the skill, and
    /// the injected "Base directory for this skill: …" body is NESTED into that block's
    /// output — not a loose result block beside it, and never a `❯` user turn.
    #[test]
    fn skill_body_nests_into_the_skill_call() {
        let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"Launching skill: dump-tasks"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /Users/dev/.claude/skills/dump-tasks\n\n# dump-tasks\n\nTurn the work into a brief."}]}}
"#;
        let blocks = parse(jsonl);
        // Exactly one block — the Skill call — with the skill name as its target and
        // the fold key "skill" (default-folded, like reads/thinking).
        assert_eq!(kinds(&blocks), vec!["skill"], "{blocks:?}");
        match &blocks[0] {
            Block::ToolUse {
                name,
                target,
                output,
                ..
            } => {
                assert_eq!(name, "Skill");
                assert_eq!(target, "dump-tasks", "skill name not used as target");
                let out = output.as_deref().unwrap_or("");
                assert!(
                    out.contains("Launching skill: dump-tasks"),
                    "keeps the result"
                );
                assert!(
                    out.contains("Base directory for this skill:"),
                    "skill body nested into the Skill block: {out:?}"
                );
            }
            other => panic!("expected Skill ToolUse, got {other:?}"),
        }
    }

    /// With no preceding `Skill` block, a "Base directory…" body still folds on its own
    /// as a result block (the nesting is a best-effort attach, not a hard requirement).
    #[test]
    fn orphan_skill_body_still_folds_as_result() {
        let jsonl = r#"
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /x\n\n# s"}]}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(kinds(&blocks), vec!["tool_result"], "{blocks:?}");
    }

    /// An `Agent` spawn becomes a `SubAgent` block (the "launched" event); its later
    /// completion `<task-notification>` becomes a SEPARATE `AgentDone` event at the point
    /// it arrived — the two-message model. The spawn's status is still back-patched to
    /// terminal (so active-tracking drops it from `a active N`), but the returned result
    /// renders on the `AgentDone`, not folded back onto the spawn.
    #[test]
    fn agent_spawn_and_completion_are_two_events() {
        let jsonl = r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"Review the rewrite","prompt":"Review render.rs"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Review the rewrite\" finished</summary>\n<result>Two gaps found.</result>\n</task-notification>"}
"##;
        let blocks = parse(jsonl);
        // Two agent blocks: the spawn (launched) then the completion (done).
        assert_eq!(kinds(&blocks), vec!["agent", "agent"], "{blocks:?}");
        let Block::SubAgent(sa) = &blocks[0] else {
            panic!("not a SubAgent: {blocks:?}")
        };
        assert_eq!(sa.tool_use_id, "toolu_A");
        assert_eq!(sa.agent_id, "aXYZ1234");
        assert_eq!(sa.agent_type, "code-reviewer");
        assert_eq!(sa.description, "Review the rewrite");
        assert_eq!(sa.prompt, "Review render.rs");
        assert_eq!(
            sa.status,
            AgentStatus::AsyncLaunched,
            "spawn keeps its LAUNCH status — no back-patch; the spawn/finish blocks are immutable"
        );
        assert_eq!(
            sa.result, None,
            "result renders on AgentDone, not the spawn"
        );
        // The terminal status is DERIVED by the sub_agents index from the AgentDone (finish)
        // event superseding the spawn — not by mutating the spawn block (two durable events).
        let map = crate::engine::build_sub_agents(&blocks);
        assert_eq!(
            map["aXYZ1234"].status,
            AgentStatus::Completed,
            "index derives terminal status from the finish event"
        );
        // The completion is a distinct AgentDone event carrying status + result, with the
        // agent_type resolved back from the spawn.
        let Block::AgentDone {
            agent_id,
            agent_type,
            description,
            status,
            result,
        } = &blocks[1]
        else {
            panic!("second block is not AgentDone: {blocks:?}")
        };
        assert_eq!(agent_id, "aXYZ1234");
        assert_eq!(agent_type, "code-reviewer", "type resolved from the spawn");
        assert_eq!(description, "Review the rewrite");
        assert_eq!(*status, AgentStatus::Completed);
        assert_eq!(result.as_deref(), Some("Two gaps found."));
        // Both fold under the "agent" key; the default-collapse *policy* is a view concern
        // (asserted in `view`'s `default_fold_policy_collapses_agent_blocks`).
        assert_eq!(fold_key(&blocks[0]), "agent");
        assert_eq!(fold_key(&blocks[1]), "agent");
    }

    /// `enrich_tree` (via `parse_session_enriched`) loads each `SubAgent`'s child transcript
    /// from the flat `<session>/subagents/agent-<id>.jsonl`, so the spawn's tool count is
    /// **node-scoped** (the child's tools, not the parent's), and `subtree_cost` rolls up.
    #[test]
    fn enrich_loads_child_scoped_and_rolls_up_cost() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-subagent-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("proj").join("sid.jsonl");
        let sadir = base.join("proj").join("sid").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        // Parent: one Agent spawn; its own transcript has a Bash tool the child must NOT
        // be credited with.
        let parent = r##"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_P","name":"Bash","input":{"command":"ls"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]}}
{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]}}
"##;
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(parent.as_bytes())
            .unwrap();
        // Child transcript: two Read tools + model tokens (for a nonzero cost).
        let child = r##"{"type":"user","message":{"content":"go"}}
{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":500},"content":[{"type":"tool_use","id":"c1","name":"Read","input":{"file_path":"/a"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c2","name":"Read","input":{"file_path":"/b"}}]}}
"##;
        std::fs::File::create(sadir.join("agent-achild01.jsonl"))
            .unwrap()
            .write_all(child.as_bytes())
            .unwrap();

        let mut blocks = parse_file(&sess).unwrap();
        enrich_tree(&sess, &mut blocks); // load the sub-agent tree
        let Some(Block::SubAgent(sa)) = blocks.iter().find(|b| matches!(b, Block::SubAgent(_)))
        else {
            panic!("no SubAgent: {blocks:?}")
        };
        assert!(
            sa.blocks.len() >= 2,
            "child transcript loaded: {}",
            sa.blocks.len()
        );
        // The live-tail child-file resolver finds the same file (Stage 6), and misses.
        assert!(
            subagent_file(&sess, "achild01").is_some(),
            "child file resolved"
        );
        assert!(subagent_file(&sess, "nope").is_none());
        // Node-scoped: the child's blocks are its own 2 Reads, not the parent's Bash. The
        // *count* (which folds coalesced activity into a thinking block's tool list) is a
        // render concern, asserted in `render`'s `child_scoped_tool_count`. Here we assert
        // the pure-model contract: the child transcript loaded and the cost rolled up.
        assert!(
            sa.subtree_cost.unwrap_or(0.0) > 0.0,
            "subtree cost rolled up"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn task_notification_folds_to_summary_line() {
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Build release\" completed (exit code 0)</summary>\n</task-notification>"}}
"#;
        let blocks = parse(jsonl);
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
    fn nothing_is_dropped_by_default() {
        // A Read, a non-modifying Bash (`ls`), an Edit, and a tool_result must
        // ALL produce blocks now — no parse-time filtering.
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"do it"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"FILE CONTENTS"}]}}
"#;
        let blocks = parse(jsonl);
        // Nothing is dropped — but the consecutive Read + Bash coalesce into one
        // activity run (their blocks live inside it), and Edit stays expanded.
        assert_eq!(
            kinds(&blocks),
            vec!["user", "assistant", "thinking", "edit", "tool_result"]
        );
        let Block::Thinking { tools, .. } = &blocks[2] else {
            panic!("expected the coalesced Read+Bash run");
        };
        assert_eq!(
            kinds(tools),
            vec!["read", "bash"],
            "both preserved in the run"
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
        let blocks = parse(&jsonl);
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
        let blocks = parse(jsonl);
        // Edit stays expanded; the boilerplate Edit result is NOT a separate block.
        // Read + Bash are consecutive activity tools → coalesced into one activity run.
        assert_eq!(kinds(&blocks), vec!["edit", "thinking"]);

        let Block::ToolUse { patch, output, .. } = &blocks[0] else {
            panic!("expected Edit ToolUse");
        };
        assert_eq!(patch.as_ref().unwrap()[0].new_start, 12, "real newStart");
        assert!(output.is_none(), "edit boilerplate dropped");

        // Metadata is joined into the tools *before* coalescing — dig into the run.
        let Block::Thinking { tools, .. } = &blocks[1] else {
            panic!("expected a coalesced activity run");
        };
        let Block::ToolUse { read_lines, .. } = &tools[0] else {
            panic!("expected Read ToolUse");
        };
        assert_eq!(*read_lines, Some(3));

        let Block::ToolUse { output, .. } = &tools[1] else {
            panic!("expected Bash ToolUse");
        };
        assert_eq!(output.as_deref(), Some("file1\nfile2"));
    }

    #[test]
    fn consecutive_activity_tools_coalesce_into_one_summary() {
        // A run of activity tools with no thinking → one activity block (like CC's
        // "Searched for 1 pattern, ran N shell commands"); a lone one folds too (#57).
        let mut jsonl = String::from(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"go\"}]}}\n",
        );
        jsonl.push_str("{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"g\",\"name\":\"Grep\",\"input\":{\"pattern\":\"foo\"}}]}}\n");
        for i in 0..9 {
            jsonl.push_str(&format!("{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"b{i}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo {i}\"}}}}]}}}}\n"));
        }
        let blocks = parse(&jsonl);
        assert_eq!(kinds(&blocks), vec!["assistant", "thinking"]);
        let Block::Thinking {
            tools,
            text,
            duration_secs,
        } = &blocks[1]
        else {
            panic!("expected a coalesced activity run");
        };
        assert_eq!(tools.len(), 10, "1 grep + 9 bash coalesced");
        assert!(
            text.is_empty() && duration_secs.is_none(),
            "pure activity run"
        );
    }

    /// A synthetic reversed pair — a `tool_result` physically *before* its own `tool_use` —
    /// does NOT join under the single-pass fold: forward-references do not occur in real
    /// transcripts (0/209 scanned), so the not-yet-seen result renders as an inline orphan and
    /// the later `tool_use` is emitted result-less.
    #[test]
    fn result_before_tool_use_renders_as_orphan() {
        let jsonl = r#"
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"newStart":88,"lines":[" c","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"reversed result text"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
"#;
        let blocks = parse(jsonl);
        // The reversed result renders inline as an orphan; the Edit follows, result-less.
        assert_eq!(kinds(&blocks), vec!["tool_result", "edit"], "{blocks:?}");
        let Block::ToolResult(t) = &blocks[0] else {
            panic!("expected orphan ToolResult");
        };
        assert_eq!(t, "reversed result text");
        let Block::ToolUse { patch, .. } = &blocks[1] else {
            panic!("expected Edit ToolUse");
        };
        assert!(
            patch.is_none(),
            "reversed pair must not join — the Edit has no patch"
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
        let blocks = parse(jsonl);
        assert_eq!(kinds(&blocks), vec!["user", "tool_result"], "{blocks:?}");
        let Block::ToolResult(t) = &blocks[1] else {
            panic!("expected orphan ToolResult");
        };
        assert_eq!(t, "orphan output");
    }

    /// `parse_file` (streaming single-pass file read) must produce exactly what
    /// `parse(&str)` produces for the same content.
    #[test]
    fn parse_file_matches_parse_str() {
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
        let via_str = parse(jsonl);
        let file = std::env::temp_dir().join("claude-replay-parse-path-test.jsonl");
        std::fs::write(&file, jsonl).unwrap();
        let via_path = parse_file(&file).unwrap(); // flat streaming parse (no sub-agents here)
        std::fs::remove_file(&file).ok();
        assert_eq!(format!("{via_str:?}"), format!("{via_path:?}"));
    }

    /// The Layer-1 (`tokenize`) + Layer-2 (`replay`) split must be **bit-identical** to
    /// the fused `parse_main` — same blocks AND same `user_times` — across the whole
    /// golden corpus. This is the Phase-1 equivalence gate: only once this is rock-solid
    /// may `parse_main` be repointed at `tokenize`+`replay`.
    #[test]
    fn replay_tokenize_matches_parse_main() {
        fn assert_equiv(jsonl: &str) {
            let mut ut_main = Vec::new();
            let via_main = parse_main(jsonl.lines(), &mut ut_main);
            let mut ut_engine = Vec::new();
            let via_engine = replay(&tokenize(jsonl.lines()), &mut ut_engine, &CLAUDE_SHAPING);
            assert_eq!(
                format!("{via_main:?}"),
                format!("{via_engine:?}"),
                "blocks differ for:\n{jsonl}"
            );
            assert_eq!(ut_main, ut_engine, "user_times differ for:\n{jsonl}");
        }

        let corpus: &[&str] = &[
            // Injected meta / compact-summary are not turns.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}
{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"# /loop — schedule\nParse the input…"}}
{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"This session is being continued…"}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}
"##,
            // Queue markers: immediate pickup, type-ahead pop, op-less delivery (both the
            // plain-string and array-text user shapes); interleaved task-notification.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up immediately"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:02.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:03.000Z","content":"picked up after a gap"}
{"type":"assistant","timestamp":"2026-06-30T03:00:04.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:05.000Z","content":"<task-notification>\nbg\n</task-notification>"}
{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:06.000Z"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:07.000Z","content":"delivered sans op"}
{"type":"user","timestamp":"2026-06-30T03:00:08.000Z","message":{"content":"delivered sans op"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:09.000Z","content":"delivered sans op as array"}
{"type":"user","timestamp":"2026-06-30T03:00:10.000Z","message":{"content":[{"type":"text","text":"delivered sans op as array"}]}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:11.000Z","content":"still waiting"}
"##,
            // Queued-command attachment renders as a turn in order; task-notification skipped.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"first turn"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"<task-notification>bg</task-notification>"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"mid-turn interjection"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"text","text":"ok"}]}}
{"type":"user","timestamp":"2026-06-30T03:00:06.000Z","message":{"content":"last turn"}}
"##,
            // The four content-bearing attachment types + a dropped bookkeeping one.
            r##"
{"type":"attachment","timestamp":"2026-06-30T03:00:00.000Z","attachment":{"type":"file","filename":"/w/backlog.md","displayPath":"backlog.md","content":{"type":"text","file":{"filePath":"/w/backlog.md","content":"# Backlog\nitem"}}}}
{"type":"attachment","timestamp":"2026-06-30T03:00:01.000Z","attachment":{"type":"plan_file_reference","planFilePath":"/p/plan-x.md","planContent":"# Plan\nstep 1"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:02.000Z","attachment":{"type":"edited_text_file","filename":"/w/src/main.rs","snippet":"1\tfn main(){}"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:03.000Z","attachment":{"type":"compact_file_reference","filename":"/w/src/lib.rs","displayPath":"src/lib.rs"}}
{"type":"attachment","timestamp":"2026-06-30T03:00:04.000Z","attachment":{"type":"skill_listing","content":"noise"}}
"##,
            // ExitPlanMode carries the full plan inline (#16) — call + plan attachment.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"plan something"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"tool_use","id":"ep1","name":"ExitPlanMode","input":{"plan":"# The plan\n1. do the thing"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:06.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"ep1","content":"User has approved your plan."}]}}
"##,
            // Base64 images from a prompt and a tool result.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"text","text":"look at this"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"Zm9v"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"r1","name":"Read","input":{"file_path":"/w/shot.png"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"r1","content":[{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"YmFy"}}]}]}}
"##,
            // Control-only phantom message dropped.
            "{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:00.000Z\",\"message\":{\"content\":\"\u{11}\"}}\n{\"type\":\"user\",\"timestamp\":\"2026-06-30T03:00:01.000Z\",\"message\":{\"content\":\"real\"}}\n",
            // Thinking groups preceding activity tools + duration.
            r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:12.000Z","message":{"content":[{"type":"thinking","thinking":"hmm let me consider"}]}}
"#,
            // Edit stays expanded next to thinking.
            r#"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":[{"type":"thinking","thinking":"ok"}]}}
"#,
            // Skill body nests into the Skill call.
            r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"Launching skill: dump-tasks"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /Users/dev/.claude/skills/dump-tasks\n\n# dump-tasks\n\nTurn the work into a brief."}]}}
"#,
            // Orphan skill body still folds as a result.
            r#"
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /x\n\n# s"}]}}
"#,
            // Agent spawn + completion are two events.
            r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"Review the rewrite","prompt":"Review render.rs"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Review the rewrite\" finished</summary>\n<result>Two gaps found.</result>\n</task-notification>"}
"##,
            // Task-notification folds to its summary line.
            r#"
{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Build release\" completed (exit code 0)</summary>\n</task-notification>"}}
"#,
            // Nothing dropped by default: coalesced Read+Bash run, Edit expanded.
            r#"
{"type":"user","message":{"role":"user","content":"do it"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"/x.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"FILE CONTENTS"}]}}
"#,
            // toolUseResult metadata joins (Edit patch, Read numLines, Bash stdout).
            r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"oldLines":1,"newStart":12,"newLines":1,"lines":[" ctx","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"The file /x.rs has been updated successfully."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/y.rs"}}]}}
{"type":"user","toolUseResult":{"type":"text","file":{"filePath":"/y.rs","content":"l1\nl2\nl3","numLines":3}},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"l1\nl2\nl3"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","toolUseResult":{"stdout":"file1\nfile2","stderr":""},"message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"file1\nfile2"}]}}
"#,
            // Result-before-tool_use still joins (out-of-order).
            r#"
{"type":"user","toolUseResult":{"filePath":"/x.rs","structuredPatch":[{"oldStart":10,"newStart":88,"lines":[" c","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"The file /x.rs has been updated successfully."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/x.rs","old_string":"a","new_string":"b"}}]}}
"#,
            // Orphan result with no tool_use anywhere shown inline.
            r#"
{"type":"user","message":{"content":"go"}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"ghost","content":"orphan output"}]}}
"#,
            // Slash command + inline stdout, caveat stripped.
            r#"
{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: noise</local-command-caveat><command-name>/compact</command-name><command-message>compact</command-message><command-args></command-args>"}}
{"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted (ctrl+o to see full summary)</local-command-stdout>"}}
"#,
            // Caveat-only message dropped.
            r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>just noise</local-command-caveat>"}}"#,
            // A standalone assistant thinking + text with a cwd on the first line.
            r#"{"type":"user","cwd":"/p","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","toolUseResult":{"stdout":"out","stderr":""},"message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}
"#,
        ];
        for j in corpus {
            assert_equiv(j);
        }
    }

    /// M8 keystone: folding messages in two pieces (`apply(a); apply(b)`) equals one
    /// `apply(all)` for every split point — same blocks, same `user_times`. This is the
    /// property that makes the streaming (M9) and incremental (M11) paths safe. Covers the
    /// state that must survive a split: the tool back-patch (`tool_slot`), the
    /// queue lifecycle, the thinking clock (`prev_ts`), and stamping (`pending_ts`).
    #[test]
    fn replayer_split_apply_is_identical() {
        fn assert_split(jsonl: &str) {
            let msgs = tokenize(jsonl.lines());
            let mut whole = Replayer::new(&CLAUDE_SHAPING);
            whole.apply(&msgs);
            let whole = whole.into_blocks();
            for k in 0..=msgs.len() {
                let mut r = Replayer::new(&CLAUDE_SHAPING);
                r.apply(&msgs[..k]);
                r.apply(&msgs[k..]);
                let split = r.into_blocks();
                assert_eq!(
                    format!("{:?}", whole.0),
                    format!("{:?}", split.0),
                    "blocks differ, split at {k} of {}:\n{jsonl}",
                    msgs.len()
                );
                assert_eq!(
                    whole.1, split.1,
                    "user_times differ, split at {k}:\n{jsonl}"
                );
            }
        }
        // tool_use then its result (back-patch across the split) + a later thinking block.
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#,
            "\n",
        ));
        // queue enqueue/dequeue lifecycle across the split.
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}"#,
            "\n",
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up after a gap"}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:03.000Z"}"#,
            "\n",
        ));
        // injected meta + real turns (user-turn stamping across the split).
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"meta note"}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}"#,
            "\n",
        ));
    }

    /// `apply`'s back-patch signal (§9a): it reports the min raw-logical index of an **already-
    /// emitted** block the batch mutated in place, or `None` for an append-only batch. This is the
    /// signal the streaming layer turns into a provisional-generation bump; the fold's blocks are
    /// unaffected (covered byte-identical elsewhere).
    #[test]
    fn apply_reports_backpatch_of_already_emitted_blocks() {
        let user =
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#;
        let tool = r#"{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#;
        let result = r#"{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#;
        let text = r#"{"type":"assistant","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":[{"type":"text","text":"done"}]}}"#;

        // Back-patch ACROSS batches: the tool_use is emitted in batch 1, its result lands in batch 2
        // and fills the already-emitted `ToolUse.output` ⇒ `Some(index of that ToolUse)`.
        let mut r = Replayer::new(&CLAUDE_SHAPING);
        assert_eq!(
            r.apply(&tokenize([user, tool].into_iter())),
            None,
            "appends only"
        );
        // The open turn is [UserText(0), ToolUse(1)]; the result back-patches index 1.
        assert_eq!(
            r.apply(&tokenize([result].into_iter())),
            Some(1),
            "result back-patches the already-emitted ToolUse at logical index 1"
        );
        // A pure-append batch afterward ⇒ None again.
        assert_eq!(
            r.apply(&tokenize([text].into_iter())),
            None,
            "append-only after"
        );

        // Same-batch tool_use + result: the block is appended AND patched within one batch, so the
        // patched index is >= the entry frontier ⇒ invisible to clients ⇒ `None`.
        let mut r2 = Replayer::new(&CLAUDE_SHAPING);
        assert_eq!(
            r2.apply(&tokenize([user, tool, result].into_iter())),
            None,
            "a tool whose result arrives in the same batch is a fresh append, not a back-patch"
        );
    }

    /// M11 keystone: driving the `Replayer` **one line at a time** (a live tail: `decode` the
    /// line, `apply`, `snapshot`) yields byte-identical blocks + user_times to a full batch
    /// `replay(tokenize(whole))` — at EVERY prefix, not just the end. This is the
    /// incremental-fold guarantee the live follower (M11 routing) stands on; a rewritten tail
    /// is handled by the follower rebuilding from scratch (which is trivially the full replay
    /// of the new content).
    #[test]
    fn incremental_line_by_line_matches_full_replay() {
        fn assert_follow(lines: &[&str]) {
            let mut cwd = String::new();
            let mut r = Replayer::new(&CLAUDE_SHAPING);
            for (i, line) in lines.iter().enumerate() {
                let mut delta = Vec::new();
                decode_line(line, &mut cwd, &mut delta);
                r.apply(&delta);
                // Snapshot after each line must match a full replay of the lines so far.
                let (inc_blocks, inc_ut) = r.snapshot();
                let mut ref_ut = Vec::new();
                let ref_blocks = replay(
                    &tokenize(lines[..=i].iter().copied()),
                    &mut ref_ut,
                    &CLAUDE_SHAPING,
                );
                assert_eq!(
                    format!("{ref_blocks:?}"),
                    format!("{inc_blocks:?}"),
                    "blocks differ after line {i}"
                );
                assert_eq!(ref_ut, inc_ut, "user_times differ after line {i}");
            }
        }
        // tool_use then its result (back-patch across poll boundaries) + a trailing thinking.
        assert_follow(&[
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"go"}}"#,
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:09.000Z","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#,
        ]);
        // queue enqueue then (later poll) dequeue — the lifecycle spans polls.
        assert_follow(&[
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"turn"}}"#,
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"picked up after a gap"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-30T03:00:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:03.000Z"}"#,
        ]);
        // injected meta between real turns (user-turn stamping across polls).
        assert_follow(&[
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real question"}}"#,
            r#"{"type":"user","isMeta":true,"timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"meta note"}}"#,
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"another real question"}}"#,
        ]);
    }

    #[test]
    fn slash_command_becomes_command_block_caveat_stripped() {
        // A /compact invocation with inline stdout and a caveat: one Command
        // block, caveat dropped, no raw tags surviving.
        let jsonl = r#"
{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat: noise</local-command-caveat><command-name>/compact</command-name><command-message>compact</command-message><command-args></command-args>"}}
{"type":"user","message":{"role":"user","content":"<local-command-stdout>Compacted (ctrl+o to see full summary)</local-command-stdout>"}}
"#;
        let blocks = parse(jsonl);
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
        assert!(parse(jsonl).is_empty(), "caveat-only should yield nothing");
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
