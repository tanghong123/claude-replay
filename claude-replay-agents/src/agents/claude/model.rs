//! **Claude's transcript parser — the Layer 1 adapter** (mirrors `codex_model`). Holds
//! Claude Code's per-line tokenizer (`decode_line` / `tokenize`), the Claude `Shaping`
//! (`CLAUDE_SHAPING`, `claude_build_tool`, `apply_result`, turn grouping/coalescing), the
//! streaming parse entry points, sub-agent transcript loading, and the tool/attachment
//! decode helpers. The agent-neutral engine it feeds — the `Block` data model, the
//! `Replayer` / `replay` fold, the `SessionAccumulator` driver, and the shared message-handling
//! helpers — lives in the engine's `model`. `parse_main` is the frozen `#[cfg(test)]` reference parser.

use claude_replay_engine::seam::*;
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
        // QoderWork's word for a failed spawn (#28, measured on a real store:
        // `{kind:"agent-result", state:"error", terminateReason:"ERROR"}`, beside 307
        // `completed`) — a failure, not a mystery, so it maps to `Failed` not `Unknown`.
        "error" => AgentStatus::Failed,
        "killed" => AgentStatus::Killed,
        "stopped" => AgentStatus::Stopped,
        _ => return None,
    })
}

/// How a `user` event was injected, if at all — the event-level flags that say its content
/// is system content rather than a human turn.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Injected {
    /// A genuine human turn.
    No,
    /// `isMeta`: an instruction / skill / caveat body. Folds to a system-note block.
    Meta,
    /// `isCompactSummary`: the continuation summary written back after a compaction. Folds
    /// into the [`Block::Compaction`] divider the boundary record just opened (#108) — it
    /// used to be lumped in with `Meta` as a loose system note, which discarded the pairing.
    CompactSummary,
}

impl Injected {
    /// Is this content injected at all (either flavour)? The old `is_injected_event`
    /// predicate, kept where only the yes/no matters (caveat stripping, turn suppression).
    fn is_injected(self) -> bool {
        self != Self::No
    }
}

/// Classify a `user` event's injection flags. `isCompactSummary` wins over `isMeta`: it is
/// the more specific claim, and the two co-occur on nothing observed.
fn injection_of(v: &Value) -> Injected {
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("isCompactSummary") {
        Injected::CompactSummary
    } else if flag("isMeta") {
        Injected::Meta
    } else {
        Injected::No
    }
}

/// L1 classification of a plain-string `user` message into the **structured** message the
/// shared fold places — this is where Claude's raw wrappers (`<task-notification>`,
/// `<command-name>`, `<local-command-*>`, skill bodies, caveats) are parsed, so the fold
/// never sees them. Mirrors the retired `push_user_string`, but returns a `Message` instead
/// of pushing a block. `None` drops the message (caveat-only / phantom keystroke).
fn classify_user_string(s: &str, injected: Injected) -> Option<Message> {
    // The compaction summary is claimed by its event flag, ahead of every content sniff below:
    // the flag is the transcript's own statement of what this message is, and the fold needs it
    // whole to join to the boundary divider (#108).
    if injected == Injected::CompactSummary {
        return compact_summary(s);
    }
    let injected = injected.is_injected();
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
fn classify_user_array_text(text: &str, injected: Injected) -> Option<Message> {
    if injected == Injected::CompactSummary {
        return compact_summary(text);
    }
    Some(if is_skill_body(text) {
        Message::SkillBody {
            text: text.to_string(),
            fallback: text.to_string(),
        }
    } else if injected.is_injected() {
        Message::SystemNote {
            text: text.to_string(),
        }
    } else {
        Message::UserText {
            text: text.to_string(),
        }
    })
}

/// The prose half of a compaction, cleaned exactly as an injected system note is (caveats
/// stripped, trimmed) so the divider's expansion reads the same as the loose result block it
/// replaces. Empty prose yields nothing — the divider then stands on its metadata alone.
fn compact_summary(s: &str) -> Option<Message> {
    let cleaned = strip_caveat(s);
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| Message::CompactSummary {
        text: cleaned.to_string(),
    })
}

/// The metadata half: Claude's `system` / `compact_boundary` record. `preTokens` and
/// `postTokens` are present on all 65 compactions across this machine's transcripts;
/// `cumulativeDroppedTokens` is NOT (54/65), which is why the session total is summed from
/// `pre - post` rather than read from the record.
fn compact_boundary(v: &Value) -> Option<Message> {
    let m = v.get("compactMetadata")?;
    let n = |k: &str| m.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(Message::CompactBoundary {
        trigger: CompactTrigger::parse(m.get("trigger").and_then(Value::as_str).unwrap_or("")),
        pre_tokens: n("preTokens"),
        post_tokens: n("postTokens"),
    })
}

/// Injected/system content Claude flags at the event level (`isMeta`/`isCompactSummary`) —
/// folds as a system result block; caveat-only noise is dropped. Used by the frozen
/// reference parser [`parse_main`]; the streaming path uses [`classify_user_string`].
/// The compaction summary's placement in the frozen reference parser — the mirror of the
/// fold's `CompactSummary` arm: fill the divider it directly follows, else stand alone as a
/// system-note block.
#[cfg(test)]
fn push_compact_summary(s: &str, out: &mut Vec<Block>) {
    let Some(Message::CompactSummary { text }) = compact_summary(s) else {
        return;
    };
    match out.last_mut() {
        Some(Block::Compaction { summary, .. }) if summary.is_empty() => *summary = text,
        _ => out.push(Block::ToolResult(text)),
    }
}

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
fn task_op(name: &str, id: &str, input: &Value) -> Option<claude_replay_engine::seam::TaskOp> {
    use claude_replay_engine::seam::TaskOp;
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
        // #126: `TodoWrite` sends the WHOLE list every time — `{todos:[{description,status}]}`.
        // Mapped here, in the shared decoder, deliberately: QoderWork delegates `decode_line`
        // to this tokenizer, so this is what lights up its panel (measured: 268 calls in one
        // session where the panel was otherwise blank), and a tool name should mean the same
        // thing across Claude-format agents. It is inert for Claude today — 0 of 133
        // transcripts use it.
        "TodoWrite" => Some(TaskOp::Snapshot {
            todos: input
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| {
                    let f = |t: &Value, k: &str| {
                        t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
                    };
                    a.iter()
                        .map(|t| {
                            // `description` or `content` depending on the caller's version —
                            // measured across 6323 real items: 5285 vs 1048, never both.
                            let text = match f(t, "description") {
                                s if s.is_empty() => f(t, "content"),
                                s => s,
                            };
                            claude_replay_engine::seam::Todo {
                                text,
                                status: f(t, "status"),
                                active_form: f(t, "activeForm"),
                            }
                        })
                        .filter(|t| !t.text.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        }),
        _ => None,
    }
}

/// The `taskq` audit-record sentinel (agentdev `docs/taskq-DESIGN.md` §9). A mutating
/// `taskq` command prints one such line per mutation as the LAST line of its stdout, and the
/// harness captures that into the transcript — so the queue's history is already in every
/// transcript, in a form designed to be extracted. The sentinel carries no quotes,
/// backslashes or non-ASCII precisely so it survives JSON string escaping verbatim.
const TASKQ_SENTINEL: &str = "##taskq/v1";

/// Task ops carried by a **Bash** tool result, from `taskq` records.
///
/// The queue this reads is not the harness's. `taskq` is a repo-rooted, cross-agent work queue
/// (`tasks/*.json` at the git root) that exists because the native `TaskCreate`/`TaskUpdate`
/// tools are harness-private and absent from some builds — the sessions using it therefore
/// render an EMPTY task panel while doing all their work through a queue the transcript
/// records in full. Measured on the session that motivated this: 47 creates, 36 claims, 45
/// dones, 14 logs, and a panel showing nothing.
///
/// Every record becomes ONE op, and the mapping is deliberately onto the vocabulary the fold
/// already has rather than a new one — the ops are the same ops:
///
/// * `create` → [`TaskOp::Create`] plus an immediate [`TaskOp::Resolve`]. taskq assigns the id
///   inside the record, so unlike the native flow there is nothing to wait for. The synthetic
///   `tool_use_id` is namespaced by the record's `rid` (unique by construction) so it can
///   never collide with a real one.
/// * `claim`/`done`/`cancel`/`release`/`update` → [`TaskOp::Update`], with the status taken
///   from `changes.status.to` — the record states the transition rather than implying it.
/// * `log` → a no-op here: a progress note changes no field the panel shows, and mapping it to
///   an `Update` would be recording a change that did not happen.
/// * `archive`/`delete`/`renumber` → left alone for now; they mean "leave the list", which the
///   append-only fold cannot express (the same reason `Snapshot` exists for `TodoWrite`).
///
/// Re-seeing a record is HARMLESS and needs no dedupe state, which is what keeps this a
/// per-line decode like every other: `taskq list --with-history` echoes other agents' records
/// into this transcript and the journal's tail can be printed, so the same mutation
/// legitimately appears more than once — but a repeated `create` lands under the same id and
/// `join` already replaces rather than appends ("a re-created id replaces the older item"),
/// and a repeated `Update` sets the same status twice. Both are idempotent by construction.
/// The `rid` still namespaces the synthetic `tool_use_id`, so an echoed create pairs with its
/// own resolve and never with a different one.
fn taskq_ops(text: &str) -> Vec<TaskOp> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix(TASKQ_SENTINEL) else {
            continue;
        };
        let Ok(rec) = serde_json::from_str::<Value>(rest.trim()) else {
            continue; // a mangled or truncated line is skipped, never guessed at
        };
        let str_of = |k: &str| rec.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        // The record id namespaces the synthetic `tool_use_id` below, so a create always
        // pairs with its OWN resolve. A record without one is not trusted.
        let rid = str_of("rid");
        if rid.is_empty() {
            continue;
        }
        let task = str_of("task");
        if task.is_empty() {
            continue; // nothing to address
        }
        // `changes.<field>.to` — where a record states the value a mutation moved a field TO.
        let to = |field: &str| {
            rec.pointer(&format!("/changes/{field}/to"))
                .and_then(|v| v.as_str())
                .map(String::from)
        };
        match str_of("op") {
            "create" => {
                let tuid = format!("taskq:{rid}");
                out.push(TaskOp::Create {
                    tool_use_id: tuid.clone(),
                    subject: str_of("subject").to_string(),
                    // The record truncates long text (~120 chars) by design — it is an audit
                    // line, not a store. The subject is short and complete; a description
                    // would be a lie at this width, so none is claimed.
                    description: String::new(),
                    active_form: String::new(),
                    blocked_by: Vec::new(),
                });
                out.push(TaskOp::Resolve {
                    tool_use_id: tuid,
                    id: Some(task.to_string()),
                });
            }
            // Every other state-moving op says where it moved to, so one arm reads them all.
            "claim" | "done" | "cancel" | "release" | "update" => {
                let status = to("status");
                if status.is_none() {
                    continue; // e.g. a description-only edit — nothing the panel renders
                }
                // The subject comes from the record's TOP-LEVEL field, which every record
                // carries, not from `changes.subject.to`, which appears only when the subject
                // itself was edited. That distinction is load-bearing: an update for a task
                // this transcript never saw created materializes a stub (#125), and for the
                // native tools that stub is necessarily titleless — "Updated task #5 status"
                // says nothing more. A taskq record DOES name its task, so passing the subject
                // on every state op is what keeps a resumed or mid-session view from rendering
                // a row with no content.
                let subject = Some(str_of("subject").to_string()).filter(|s| !s.is_empty());
                out.push(TaskOp::Update {
                    task_id: task.to_string(),
                    status,
                    subject,
                    description: None,
                    active_form: None,
                    add_blocks: Vec::new(),
                    add_blocked_by: Vec::new(),
                });
            }
            _ => {}
        }
    }
    out
}

/// Turn one plain-string `user` message into block(s) — a slash command becomes a/// Turn one plain-string `user` message into block(s) — a slash command becomes a
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
// `claude_replay_engine::seam::coalesce_spans` and `design/cc-activity-coalescing.md` (#57). The
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

/// [`enrich_tree`] against an explicit `subagents/` dir — for a derived store (Qoder) whose
/// companion dir can sit under a DIFFERENT project slug than the transcript (a mid-session
/// `cwd` change files it under the new cwd's slug), so "beside the transcript" is not the
/// only place to look. Composable: children a pass can't resolve are left untouched, so
/// several candidate dirs may be tried in turn.
/// Tools whose "execution" is a human thinking (#21): a gap they bound is user latency,
/// not agent work. Shared by every Claude-Code-format adapter (Claude, Qoder, QoderWork —
/// one tool vocabulary), so the list lives once, next to the decoder that interprets it.
pub(crate) fn tool_is_interactive(name: &str) -> bool {
    matches!(name, "AskUserQuestion" | "ExitPlanMode")
}

/// Whether this raw line says the assistant's TURN is over (#194), Claude-format:
/// an assistant record's `stop_reason` of `end_turn`/`stop_sequence` ends it; any other
/// assistant record is mid-stream; a user record (a prompt or a tool result feeding
/// back) proves the conversation moved past whatever ended before. Field-level on
/// purpose — this runs per tail line on the monitor's scan path, like the liveness scan.
pub(crate) fn turn_ended(raw_line: &str) -> Option<bool> {
    if raw_line.contains("\"type\":\"assistant\"") {
        return Some(
            raw_line.contains("\"stop_reason\":\"end_turn\"")
                || raw_line.contains("\"stop_reason\":\"stop_sequence\""),
        );
    }
    if raw_line.contains("\"type\":\"user\"") {
        return Some(false);
    }
    None
}

pub(crate) fn enrich_tree_in(sadir: &std::path::Path, blocks: &mut [Block]) {
    enrich_subagents(blocks, sadir);
}

/// Parse a transcript file into blocks WITHOUT loading sub-agent children — the raw pass
/// the adapter's `parse_path_timed` builds on. `enrich_tree` (the adapter's `enrich`, backing
/// `parse_session_enriched`) adds the children; that recursion reuses this so grandchildren
/// resolve against the same session `subagents/` dir.
fn parse_file(path: &std::path::Path) -> std::io::Result<Vec<Block>> {
    // Stream through the shared incremental fold in a single pass, one line resident, and keep
    // only the blocks (this sub-agent path doesn't need times or metrics).
    let mut b =
        claude_replay_engine::seam::SessionAccumulator::new(&crate::adapters::ClaudeAdapter);
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

// ── Workflow runs: one call, a fleet of agents (#38) ──────────────────────────────────
//
// A dynamic workflow launches N agents from a single `Workflow` tool call, and the transcript
// names none of them. What it does record — in the result text the block already keeps — is
// where the run lives:
//
//   Workflow launched in background. Task ID: …
//   Transcript dir: <session>/subagents/workflows/<runId>
//
// and under that directory:
//
//   agent-<id>.jsonl        each member's transcript — ordinary Claude transcripts
//   agent-<id>.meta.json    {"agentType":"workflow-subagent","spawnDepth":1}
//   journal.jsonl           {"type":"started"|"result","agentId":…,"result":…}
//
// The journal is the roster, and it is append-only while the run proceeds. It carries no label
// for a member — only ids and, once an agent returns, its result — so a member is titled from
// the first line of that result and, until then, by its position in the run.

/// The run id a `Workflow` call launched, read from the result text the block already carries.
/// The trailing component of the recorded `Transcript dir:` — matching on the id rather than the
/// whole path so a session directory that has been moved or copied still resolves.
pub(crate) fn workflow_run(b: &Block) -> Option<String> {
    let Block::ToolUse { name, output, .. } = b else {
        return None;
    };
    if name != "Workflow" {
        return None;
    }
    let dir = output.as_deref()?.lines().find_map(|l| {
        l.strip_prefix("Transcript dir:")
            .map(|rest| rest.trim().to_string())
    })?;
    let run = std::path::Path::new(&dir).file_name()?.to_str()?;
    (!run.is_empty()).then(|| run.to_string())
}

/// Every workflow run under this session, each with the members its journal records.
///
/// Read by directory, so it needs no prior knowledge of which runs the transcript mentions; the
/// `is_dir` probe short-circuits every session that ran no workflow, which is nearly all of them.
pub(crate) fn workflow_rosters(session_path: &std::path::Path) -> Vec<SpawnRoster> {
    let Some(runs) = subagents_dir(session_path).map(|d| d.join("workflows")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&runs) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let dir = e.path();
        let Some(run) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let members = roster_from_journal(&dir.join("journal.jsonl"));
        if !members.is_empty() {
            out.push(SpawnRoster {
                run: run.to_string(),
                members,
            });
        }
    }
    out
}

/// One run's members, in the order the journal started them. A `result` record completes the
/// member it names and titles it; a member with no result yet is still running, and carries its
/// launch position as a title until its own words arrive.
fn roster_from_journal(journal: &std::path::Path) -> Vec<SubAgent> {
    let Ok(text) = std::fs::read_to_string(journal) else {
        return Vec::new();
    };
    let mut members: Vec<SubAgent> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = v.get("agentId").and_then(|x| x.as_str()) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("started") => {
                if members.iter().any(|m| m.agent_id == id) {
                    continue;
                }
                members.push(SubAgent {
                    agent_id: id.to_string(),
                    tool_use_id: String::new(),
                    agent_type: "workflow".into(),
                    description: format!("agent {}", members.len() + 1),
                    prompt: String::new(),
                    status: AgentStatus::Running,
                    result: None,
                    output_file: None,
                    blocks: Vec::new(),
                    subtree_cost: None,
                });
            }
            Some("result") => {
                // A member given a schema returns a structured VALUE, not prose — that is a
                // first-class way to run one, so a result that is not a string is a result all
                // the same. Keep it verbatim as compact JSON; only prose yields a title, so a
                // structured member keeps its launch position rather than being titled with a
                // brace.
                let (result, title) = match v.get("result") {
                    None | Some(Value::Null) => (None, None),
                    Some(Value::String(t)) => (Some(t.clone()), result_title(t)),
                    Some(other) => (Some(other.to_string()), None),
                };
                if let Some(m) = members.iter_mut().find(|m| m.agent_id == id) {
                    m.status = AgentStatus::Completed;
                    m.result = result;
                    if let Some(title) = title {
                        m.description = title;
                    }
                }
            }
            _ => {}
        }
    }
    members
}

/// A member's title: the first non-blank line of what it returned, undecorated of markdown
/// heading marks and clipped to a chip's worth. `None` when the result opens with nothing
/// usable, leaving the launch-position title in place rather than an empty one.
fn result_title(result: &str) -> Option<String> {
    let line = result.lines().find(|l| !l.trim().is_empty())?;
    let t = line.trim().trim_start_matches('#').trim();
    if t.is_empty() {
        return None;
    }
    Some(match t.char_indices().nth(60) {
        Some((i, _)) => format!("{}…", &t[..i]),
        None => t.to_string(),
    })
}

/// The on-disk transcript for `agent_id` under the root session at `session_path`
/// (`<session>/subagents/agent-<id>.jsonl`), if it exists — the file a descended child is
/// live-tailed from. All of a session's agents (any depth) share this one flat dir.
pub fn subagent_file(session_path: &std::path::Path, agent_id: &str) -> Option<std::path::PathBuf> {
    let stem = session_path.file_stem()?.to_str()?;
    let sadir = session_path.parent()?.join(stem).join("subagents");
    child_file(&sadir, agent_id)
}

/// `agent_id`'s transcript under a `subagents/` dir. The flat dir first — where every ordinary
/// agent of a session lives, whatever its depth — then each `workflows/<runId>/` beneath it,
/// which is where a workflow run keeps its own members (#38). Checked in that order because the
/// flat dir is the common case and a run dir is a scan.
fn child_file(sadir: &std::path::Path, agent_id: &str) -> Option<std::path::PathBuf> {
    let leaf = format!("agent-{agent_id}.jsonl");
    let flat = sadir.join(&leaf);
    if flat.is_file() {
        return Some(flat);
    }
    std::fs::read_dir(sadir.join("workflows"))
        .ok()?
        .flatten()
        .map(|e| e.path().join(&leaf))
        .find(|f| f.is_file())
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
            let Some(child) = child_file(sadir, &sa.agent_id) else {
                continue;
            };
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
        claude_replay_engine::seam::parse_reader_with(
            &crate::adapters::ClaudeAdapter,
            std::io::BufReader::new(f),
        )
        .cost_usd
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
fn apply_result(block: &mut Block, txt: &str, tur: &Value, is_error: Option<bool>) {
    match block {
        Block::ToolUse {
            name,
            target,
            output,
            patch,
            read_lines,
            execution,
            published,
            ..
        } => {
            // A `Workflow` call's input is the script, so it builds with nothing to show for
            // itself and renders as a bare `Workflow()`. Its result names the run (#38) — take
            // that as the label, so the launched fleet below it has a heading.
            if name == "Workflow" && target.is_empty() {
                if let Some(n) = tur.get("workflowName").and_then(|v| v.as_str()) {
                    *target = n.to_string();
                }
            }
            // A `StructuredOutput` block already carries the answer, taken from the call's own
            // input; its result is the fixed stub, so overwriting here would replace the whole
            // payload with "Structured output provided successfully".
            if name != "StructuredOutput" {
                *output = tool_output(name, Some(tur), txt);
            }
            *patch = parse_patch(tur);
            *read_lines = tur
                .pointer("/file/numLines")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize);
            // #36: the format's failure fact — `is_error: true` on the result content item —
            // becomes a structural `ToolExecution` status. FAILURES ONLY: this format's
            // success is the key's absence, not a recorded word (see the decoder's #26
            // note), and a success badge on every tool would be noise the presenters
            // deliberately drop. Exit code and duration genuinely are not in this format
            // and stay `None` rather than be invented. Guarded: a status a richer record
            // already set is never stomped.
            if is_error == Some(true) {
                let e = execution.get_or_insert(ToolExecution {
                    status: None,
                    exit_code: None,
                    duration: None,
                });
                if e.status.is_none() {
                    e.status = Some(ToolStatus::Failed);
                }
            }
            // An `Artifact` publish: the URL arrives only now, in the result's prose. With it
            // the block becomes a link to a real thing and the output is dropped — the rest of
            // that result is instructions to the AGENT (how to republish, that artifacts are
            // private), not information about the artifact, and the `{}` raw toggle still has
            // the original. Without a URL the call published nothing, so the fact goes away and
            // an ordinary tool block is what is left.
            if let Some(p) = published.as_deref_mut() {
                match artifact_url(txt) {
                    Some(url) => {
                        p.url = url;
                        *output = None;
                    }
                    None => *published = None,
                }
            }
        }
        // An `Agent`/`Task` spawn's result: `toolUseResult` carries the agent id, the
        // launch status, and (sync) the inline result or (async) the output-file path.
        Block::SubAgent(sa) => {
            if let Some(aid) = tur.get("agentId").and_then(|v| v.as_str()) {
                sa.agent_id = aid.to_string();
            }
            // Claude records the launch status under `status`; QoderWork's synchronous
            // `agent-result` records the TERMINAL state under `state` (#95:
            // `{kind:"agent-result", state:"completed", terminateReason:"GOAL", …}`) —
            // accept either, so a QoderWork spawn resolves instead of staying running.
            // A PRESENT word outside the vocabulary resolves to `Unknown` (#28), never
            // silently stays `Running`: a result line is the spawn's outcome, so whatever
            // it says, the spawn is over (measured: no in-progress word on any result line
            // across ~60 sessions in two stores — `async_launched`, the detached marker,
            // is in the vocabulary). Absent status/state changes nothing, as before.
            if let Some(word) = tur
                .get("status")
                .or_else(|| tur.get("state"))
                .and_then(|v| v.as_str())
            {
                sa.status = status_from_str(word).unwrap_or(AgentStatus::Unknown);
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
/// cwd is threaded here (running-current — each line's non-empty `cwd` moves it forward,
/// #173) purely to shape tool targets, exactly as `parse_main` does. Streaming: one
/// `Value` resident at a time.
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
/// line into 0+ canonical messages appended to `msgs`. `cwd` is threaded across lines
/// (running-current — each line's non-empty `cwd` moves it forward, #173) so tool targets
/// relativize against the cwd in effect at that line, and each `ToolUse` carries it for the
/// reveal action. `tokenize` is this over every line; the streaming driver (M9) calls it one
/// line at a time so no whole-file `Vec<Message>` is ever built.
pub(crate) fn decode_line(line: &str, cwd: &mut String, msgs: &mut Vec<Message>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    // Running-current (#173): each line that records a non-empty cwd moves the anchor
    // forward (a mid-session `cd`); a line without one keeps the previous value. Never
    // clear on absence — most lines carry no cwd.
    if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
        if !c.is_empty() {
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
                    // Encrypted reasoning (Qoder's `QE:`-prefixed `data`, Anthropic's
                    // redacted blocks): the ciphertext is never shown, but the block still
                    // marks reasoning time, so it joins the ✻ work-span as a placeholder.
                    Some("redacted_thinking") => {
                        msgs.push(Message::Thinking {
                            text: "[redacted thinking]".to_string(),
                            ts: ev_ts,
                        });
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
        // The only `system` record the viewer surfaces: a context-compaction boundary (#108).
        // Every other subtype stays dropped — they are agent bookkeeping with no reader value.
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("compact_boundary") => {
            if let Some(m) = compact_boundary(&v) {
                msgs.push(m);
            }
        }
        Some("user") => {
            let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
            let injected = injection_of(&v);
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
                                    if let Some(m) = classify_user_array_text(t, injected) {
                                        msgs.push(m);
                                    }
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
                            // `taskq` records ride a Bash result's stdout (see `taskq_ops`).
                            // Emitted BEFORE the ToolResult so a create's ops are in the same
                            // order the fold would have seen them from native task tools.
                            for op in taskq_ops(&txt) {
                                msgs.push(Message::TaskOp(op));
                            }
                            msgs.push(Message::ToolResult {
                                tool_use_id: tid.to_string(),
                                text: txt,
                                tur: tur.clone(),
                                // #26: Claude Code writes `is_error` on FAILURE, and for every
                                // tool except Bash it OMITS the key on success (measured: Edit/
                                // Read/Write/mcp all show 0 explicit false, thousands absent). So
                                // absence is success in this format — decode it as `Some(false)`,
                                // never `None`. `None` stays reserved for formats that genuinely
                                // say nothing either way (Codex), so a failure-rate consumer can
                                // still exclude the undecidable instead of misreading it as
                                // success — which is exactly what the old tri-state `None` here
                                // caused downstream (agent-metrics saw Edit at 22.7%, true 1.3%).
                                is_error: Some(
                                    blk.get("is_error")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false),
                                ),
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
    claude_replay_engine::seam::coalesce_spans(blocks)
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
        // `StructuredOutput` is how an agent given a schema returns its answer (#38): the whole
        // payload is the tool's INPUT, and the result is a fixed stub ("Structured output
        // provided successfully"). A block that keeps only the result therefore showed an empty
        // call where the agent's entire work was — so carry the payload itself.
        let structured = name == "StructuredOutput";
        let publish = artifact_publish(name, input);
        Block::ToolUse {
            name: name.to_string(),
            // An artifact publish is labelled by the ARTIFACT — `🧭 rowt-deck`, the name a
            // reader uses for it — not by the local file that happened to hold its markup.
            // Setting it here rather than in each presenter means every header path (collapsed,
            // expanded, TUI, HTML, the block-stream vocabulary) agrees for free.
            target: match (&publish, structured) {
                (Some(p), _) => p.label(),
                (None, true) => structured_fields(input),
                // A non-publish `Artifact` call (`list`, `read`, `comments`, …) names no file
                // and no command, so the generic target is EMPTY and it renders as a bare
                // `Artifact()`. Its action is the only thing it is about — say that instead.
                (None, false) if name == "Artifact" => input
                    .get("action")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| tool_target(input, cwd)),
                (None, false) => tool_target(input, cwd),
            },
            diffs: extract_diffs(name, input),
            output: if structured {
                structured_body(input)
            } else {
                None
            },
            patch: None,
            read_lines: None,
            cwd: cwd.to_string(),
            execution: None,
            published: publish.map(Box::new),
        }
    }
}

/// The half of an `Artifact` publish that the CALL knows: what to call it, what it is, and the
/// emoji it chose. The other half — the URL, which is the artifact's only stable handle —
/// exists nowhere in the input; [`artifact_url`] lifts it out of the result's prose once that
/// arrives, and a call whose result yields no URL drops this again (it was not a publish).
///
/// Only a publish qualifies: the tool also lists, reads, watches and comments, and none of
/// those produce a thing to open. `action` absent means publish (the tool's own default).
fn artifact_publish(name: &str, input: &Value) -> Option<Published> {
    if name != "Artifact" {
        return None;
    }
    let action = input
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("publish");
    if action != "publish" {
        return None;
    }
    let file = input.get("file_path").and_then(|v| v.as_str())?;
    // The name the reader will see. A `title` was given for it; otherwise the file's stem,
    // which is what the terminal shows and what an owner calls it ("rowt-deck").
    let name = input
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|t| !t.trim().is_empty())
        .map(decode_entities)
        .unwrap_or_else(|| {
            std::path::Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(file)
                .to_string()
        });
    Some(Published {
        name,
        url: String::new(), // filled from the result
        description: decode_entities(
            input
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
        ),
        icon: input
            .get("favicon")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// Undo ONE level of HTML escaping in an artifact's title or description.
///
/// These fields are prose the CALLER supplied, and a caller that lifted the text out of the
/// page's own `<title>` hands it over still escaped — observed: a real title arrived as
/// `crux-web · Service &amp; Module Contracts`. Nothing downstream will undo it: the value
/// travels as JSON and is written with `textContent`, so the entity would be shown literally
/// wherever the artifact is named.
///
/// One level, and only the entities markup actually needs. The cost is that a title genuinely
/// containing the seven characters `&amp;` now reads as `&` — accepted deliberately (owner,
/// 2026-08-28): a title about HTML escaping is a great deal rarer than a title with an
/// ampersand in it.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string(); // the overwhelmingly common case, untouched
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        // `&…;` within a short window — beyond that it is an ampersand in prose, not an entity.
        let end = tail[1..].find(';').map(|j| j + 1).filter(|&j| j <= 10);
        let decoded: Option<std::borrow::Cow<'static, str>> = end.and_then(|j| {
            let body = &tail[1..j];
            // Numeric first — `&#183;`/`&#xB7;` is general, and covers every character a
            // named table would have to enumerate one by one.
            if let Some(digits) = body.strip_prefix('#') {
                let cp = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => digits.parse::<u32>().ok(),
                }?;
                return char::from_u32(cp).map(|c| c.to_string().into());
            }
            // …then the named ones prose actually carries. An entity outside this set is
            // LEFT ALONE rather than guessed at: showing `&thinsp;` is a smaller wrong than
            // inventing a character.
            Some(std::borrow::Cow::Borrowed(match body {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" => "'",
                "nbsp" => "\u{a0}",
                "middot" => "·",
                "ndash" => "–",
                "mdash" => "—",
                "hellip" => "…",
                "lsquo" => "\u{2018}",
                "rsquo" => "\u{2019}",
                "ldquo" => "\u{201c}",
                "rdquo" => "\u{201d}",
                _ => return None,
            }))
        });
        match (decoded, end) {
            (Some(text), Some(j)) => {
                out.push_str(&text);
                rest = &tail[j + 1..];
            }
            _ => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The URL an `Artifact` result announces — `Published <path> at <url>`, followed by several
/// paragraphs of instructions to the agent. Matched on the URL SHAPE rather than the sentence,
/// so a reworded result keeps working; anything else yields `None` and the block stays an
/// ordinary tool call.
fn artifact_url(txt: &str) -> Option<String> {
    let at = txt.find("https://")?;
    let url: String = txt[at..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '<' && *c != ')')
        .collect();
    (url.len() > "https://".len()).then_some(url)
}

/// A `StructuredOutput` call's label: the payload's top-level field names, which say what the
/// agent was asked to return (`findings`, `scores`, …) without unfolding any of it.
fn structured_fields(input: &Value) -> String {
    let Some(o) = input.as_object() else {
        return String::new();
    };
    let all: Vec<&str> = o.keys().map(String::as_str).collect();
    match all.len() {
        0 => String::new(),
        1..=3 => all.join(", "),
        n => format!("{}, +{} more", all[..2].join(", "), n - 2),
    }
}

/// The payload itself, pretty-printed so it reads as the answer it is rather than one long line.
fn structured_body(input: &Value) -> Option<String> {
    serde_json::to_string_pretty(input)
        .ok()
        .filter(|s| !s.trim().is_empty() && s != "null" && s != "{}")
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
    // #88 mirror of the streaming fold's pickup dedup (see `Replayer`): the text of the
    // immediately-preceding event iff it emitted a `UserText`, plus notes for popped
    // `rendered` queue items whose pickup stamp is still to come.
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
        // Running-current (#173) — see `decode_line`; the golden reference mirrors it.
        if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
            if !c.is_empty() {
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
                                    cwd: cwd.clone(),
                                    execution: None,
                                    published: None,
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
            // #108 mirror: the compaction boundary opens a summary-less divider, which the
            // `isCompactSummary` message that follows then fills.
            Some("system")
                if v.get("subtype").and_then(|s| s.as_str()) == Some("compact_boundary") =>
            {
                if let Some(Message::CompactBoundary {
                    trigger,
                    pre_tokens,
                    post_tokens,
                }) = compact_boundary(&v)
                {
                    out.push(Block::Compaction {
                        trigger,
                        pre_tokens,
                        post_tokens,
                        summary: String::new(),
                    });
                }
            }
            Some("user") => {
                // The message-level toolUseResult metadata (shared by its result blocks).
                let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
                // `isMeta` events are injected system content, not human turns — route their
                // prose to a folded system block so it never gets a turn/sidebar/sticky entry
                // (see `push_injected`); `isCompactSummary` fills the divider above instead.
                let injection = injection_of(&v);
                let injected = injection.is_injected();
                let Some(content) = v.pointer("/message/content") else {
                    continue;
                };
                if let Some(s) = content.as_str() {
                    if injection == Injected::CompactSummary {
                        push_compact_summary(s, &mut out);
                    } else if is_skill_body(s) && attach_skill_body(&mut out, last_skill, s) {
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
                                        if injection == Injected::CompactSummary {
                                            push_compact_summary(t, &mut out);
                                        } else if is_skill_body(t)
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
                                    // (#26 decode rule, mirrored: an absent key is an explicit
                                    // success in this format.)
                                    let is_error = Some(
                                        blk.get("is_error")
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false),
                                    );
                                    apply_result(&mut out[idx], &txt, &tur, is_error);
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
                                // #26 class: an unrecognized `<status>` word reads as the honest
                                // terminal `Unknown`, never a false `Completed` (mirrors the
                                // production fold in `replay.rs`).
                                let status = tag_inner(c, "status")
                                    .and_then(status_from_str)
                                    .unwrap_or(AgentStatus::Unknown);
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
                            // #52 op-less delivery, attachment form (see above). The pickup
                            // ALWAYS renders its turn (kept in lockstep with the streaming
                            // fold's `AttachmentPrompt` arm).
                            let t = p.trim();
                            if let Some(pos) = queue.iter().position(|q| q.content == t) {
                                let item = queue.remove(pos);
                                if let Some(mi) = item.marker_idx {
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
    claude_replay_engine::seam::coalesce_spans(out)
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
                content: AttachmentContent::Deferred {
                    at: 0,
                    index: 0,
                    span: None,
                },
            })
        }
        // Full plan markdown, embedded and not shown inline anywhere → downloadable.
        "plan_file_reference" => {
            let plan = s("planContent")?; // require plan content, but never build it
            let path = s("planFilePath");
            let span = parse_marker(plan).map(|m| SpanHint {
                off: m.off,
                len: m.len,
                prefix: m.prefix,
                postfix: m.postfix,
                mime: None,
            });
            Some(Attachment {
                kind: AttachmentKind::Plan,
                name: path.map(basename).unwrap_or_else(|| "plan.md".to_string()),
                path: path.map(str::to_string),
                content: AttachmentContent::Deferred {
                    at: 0,
                    index: 0,
                    span,
                },
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
    // #193: an elided body carries its marker; decode harvests it as the locator hint,
    // with the MIME the walk would re-derive from the SIBLING `media_type` field.
    let span = src
        .get("data")
        .and_then(Value::as_str)
        .and_then(parse_marker)
        .map(|m| SpanHint {
            off: m.off,
            len: m.len,
            prefix: m.prefix,
            postfix: m.postfix,
            mime: Some(mime.clone()),
        });
    Some(Attachment {
        kind: AttachmentKind::Image,
        name: format!("image.{ext}"),
        path: None,
        // The base64 bytes are NEVER built here — `load_image_attachment` re-extracts them on
        // demand. `at`/`index` are placeholders; `advance_at` stamps the real byte offset.
        content: AttachmentContent::Deferred {
            at: 0,
            index: 0,
            span,
        },
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
/// on-demand byte-fetch backing `Transcript::load_attachment`. Walks the line's JSON
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
        content: AttachmentContent::Deferred {
            at: 0,
            index: 0,
            span: None,
        },
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

/// The α-lite elision policy (#193): the attachment-body nodes this fold DEFERS and never
/// renders, named by key suffix. Deliberately narrow — audited against the derivation rule
/// (nothing renders or derives from these values beyond the kept prefix):
///
/// - `file.base64` — a pasted file's base64 body (`toolUseResult.file.base64`).
/// - `source.data` — an image content block's base64 (`{type:"image",source:{data}}`, both
///   the pasted form and the tool_result twin; MIME rides the SIBLING `media_type`).
/// - `planContent` — the plan attachment's full markdown, loaded on demand.
///
/// NOT listed, deliberately: `file.content` — the same suffix names the Read tool's
/// RENDERED output (`toolUseResult.file.content`), so eliding it would break the sans-io
/// invariant; file-text attachment bodies therefore stay unelided (ceiling-bounded).
pub const CLAUDE_ELISION: claude_replay_engine::seam::Elision =
    claude_replay_engine::seam::Elision::Keys(&[
        &["file", "base64"],
        &["source", "data"],
        &["planContent"],
    ]);

#[cfg(test)]
mod tests {

    /// An agent given a schema returns through `StructuredOutput`, whose whole payload is the
    /// call's INPUT — its result is a fixed stub. Keeping only the result showed an empty call
    /// where the agent's entire answer was, so the payload is carried and the stub must not
    /// overwrite it.
    #[test]
    fn structured_output_carries_the_payload_not_the_stub() {
        let input = serde_json::json!({
            "findings": [{"title": "a bug", "severity": "high"}]
        });
        let mut b = claude_build_tool("toolu_S", "StructuredOutput", &input, "/r");
        match &b {
            Block::ToolUse { target, output, .. } => {
                assert_eq!(target, "findings", "labelled by what it returned");
                let out = output.as_deref().expect("the payload is the content");
                assert!(out.contains("\"title\""), "pretty-printed payload: {out}");
                assert!(out.contains("a bug"));
            }
            other => panic!("expected a tool block, got {other:?}"),
        }
        // The result record then arrives; its text is the stub, which must change nothing.
        apply_result(
            &mut b,
            "Structured output provided successfully",
            &Value::Null,
            None,
        );
        match &b {
            Block::ToolUse { output, .. } => assert!(
                output.as_deref().unwrap_or("").contains("a bug"),
                "the stub did not replace the answer"
            ),
            other => panic!("expected a tool block, got {other:?}"),
        }
    }

    /// The label names the fields without unfolding them, and stays short when a payload has
    /// many.
    #[test]
    fn a_structured_payloads_label_names_its_fields() {
        let f = |v: Value| structured_fields(&v);
        assert_eq!(f(serde_json::json!({"findings": []})), "findings");
        assert_eq!(f(serde_json::json!({"a": 1, "b": 2, "c": 3})), "a, b, c");
        assert_eq!(
            f(serde_json::json!({"a": 1, "b": 2, "c": 3, "d": 4})),
            "a, b, +2 more"
        );
        assert_eq!(
            f(serde_json::json!([1, 2])),
            "",
            "a non-object has no fields"
        );
    }
    use super::*;

    fn kinds(blocks: &[Block]) -> Vec<&'static str> {
        blocks.iter().map(fold_key).collect()
    }

    /// #36: the format's failure fact reaches the BLOCK — `is_error: true` on a result
    /// becomes `ToolExecution { status: Failed }` on its `ToolUse` (exit/duration stay
    /// `None`: this format does not record them, and inventing them would be worse than
    /// omitting). A successful result leaves `execution` absent entirely — success is
    /// the key's absence here, not a recorded word, and a badge on every tool is noise.
    /// QoderWork/Qoder parse through this same shaping, so the mapping covers them too.
    #[test]
    fn is_error_lands_on_the_tool_block_as_failed() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"ok","name":"Bash","input":{"command":"true"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"ok","content":"fine"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"bad","name":"Bash","input":{"command":"false"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"bad","content":"boom","is_error":true}]}}"#,
            "\n",
        );
        let blocks = replay(&tokenize(jsonl.lines()), &mut Vec::new(), &CLAUDE_SHAPING);
        // Consecutive activity calls coalesce into one span — harvest tools from both
        // the top level and inside spans.
        let execs: Vec<Option<ToolExecution>> = blocks
            .iter()
            .flat_map(|b| match b {
                Block::ToolUse { execution, .. } => vec![Some(*execution)],
                Block::Thinking { tools, .. } => tools
                    .iter()
                    .filter_map(|t| match t {
                        Block::ToolUse { execution, .. } => Some(Some(*execution)),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .flatten()
            .collect();
        assert_eq!(execs.len(), 2, "both calls present: {blocks:?}");
        assert_eq!(
            execs[0], None,
            "success records NO execution fact — absence is this format's success"
        );
        assert_eq!(
            execs[1],
            Some(ToolExecution {
                status: Some(ToolStatus::Failed),
                exit_code: None,
                duration: None,
            }),
            "the recorded failure is a structural fact"
        );
    }

    /// #23/#26: the decoder carries the content item's `is_error`. For CLAUDE it is never
    /// `None` — the format writes the key on failure and omits it on success (for every tool
    /// but Bash), so an absent key decodes as `Some(false)`, an explicit success. `None` is
    /// reserved for formats that genuinely give no signal (Codex), so a failure-rate consumer
    /// can exclude the undecidable rather than misread absence as success.
    #[test]
    fn claude_tool_result_error_absent_means_success() {
        let mk = |flag: &str| {
            format!(
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"t1","content":"boom"{flag}}}]}}}}"#
            )
        };
        for (flag, want) in [
            (r#","is_error":true"#, Some(true)),
            (r#","is_error":false"#, Some(false)),
            ("", Some(false)), // #26: absent key = success in the Claude format, not `None`
        ] {
            let mut cwd = String::new();
            let mut msgs = Vec::new();
            decode_line(&mk(flag), &mut cwd, &mut msgs);
            let got = msgs
                .iter()
                .find_map(|m| match m {
                    Message::ToolResult { is_error, .. } => Some(*is_error),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no ToolResult in {msgs:?}"));
            assert_eq!(got, want, "flag {flag:?}");
        }
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
        assert_eq!(a.kind, claude_replay_engine::seam::AttachmentKind::Plan);
        assert_eq!(a.name, "plan.md");
        // The body loads back from the raw line (what the builder's stamped locator does).
        match nth_loaded_attachment(line, 0) {
            Some(claude_replay_engine::seam::LoadedAttachment::Text(t)) => {
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

    /// #88: Claude Code sometimes writes a mid-turn typed prompt as a standalone `user`
    /// A prompt submitted, then submitted AGAIN while the agent is busy, renders as
    /// **two** turns — once where it was typed, once where the queued copy was delivered.
    ///
    /// This shape used to be de-duplicated into one turn, on the belief that Claude Code
    /// logs a single submission twice. Measured against every local transcript, that is
    /// not what happens: of 1859 enqueue records only 3 have a matching `user` event just
    /// before, and those are 21s, 6m30s and 3s apart with texts like "continue" and
    /// "try again" — a human retyping, not one submission logged twice. The dedup was
    /// therefore hiding a real second submission, so it was removed (#97).
    ///
    /// The `⧗ queued:` marker still collapses at pickup (#52) — that is a separate rule.
    /// Both the streaming fold and the `parse_main` reference must agree (their
    /// equivalence is the gate).
    #[test]
    fn resubmitted_prompt_renders_both_submissions() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-07-01T03:48:00.000Z","message":{"content":"do the thing"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-01T03:48:01.000Z","content":"do the thing"}
{"type":"assistant","timestamp":"2026-07-01T03:48:02.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"queue-operation","operation":"remove","timestamp":"2026-07-01T03:48:03.000Z","content":"do the thing"}
{"type":"user","timestamp":"2026-07-01T03:48:04.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"attachment","timestamp":"2026-07-01T03:48:05.000Z","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"do the thing"}}
{"type":"assistant","timestamp":"2026-07-01T03:48:06.000Z","message":{"content":[{"type":"text","text":"done"}]}}
"##;
        for blocks in [
            parse(jsonl),
            parse_main(
                jsonl.lines().filter(|l| !l.trim().is_empty()),
                &mut Vec::new(),
            ),
        ] {
            let users: Vec<&str> = blocks
                .iter()
                .filter_map(|b| match b {
                    Block::UserText(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                users,
                vec!["do the thing", "do the thing"],
                "both submissions render: {blocks:?}"
            );
            assert!(
                !blocks.iter().any(|b| matches!(b, Block::QueueEvent { .. })),
                "the queued marker still collapses at pickup (#52): {blocks:?}"
            );
        }
        // The attachment-only flow (no standalone user event) still renders the pickup
        // as the turn — the dedup must not eat a genuinely new prompt, even one whose
        // text repeats an earlier turn's.
        let jsonl2 = r##"
{"type":"user","timestamp":"2026-07-01T03:48:00.000Z","message":{"content":"continue"}}
{"type":"assistant","timestamp":"2026-07-01T03:48:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-07-01T03:48:02.000Z","content":"continue"}
{"type":"queue-operation","operation":"remove","timestamp":"2026-07-01T03:48:03.000Z","content":"continue"}
{"type":"user","timestamp":"2026-07-01T03:48:04.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]}}
{"type":"attachment","timestamp":"2026-07-01T03:48:05.000Z","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"continue"}}
"##;
        for blocks in [
            parse(jsonl2),
            parse_main(
                jsonl2.lines().filter(|l| !l.trim().is_empty()),
                &mut Vec::new(),
            ),
        ] {
            let users: Vec<&str> = blocks
                .iter()
                .filter_map(|b| match b {
                    Block::UserText(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                users,
                vec!["continue", "continue"],
                "a tool_use between the turn and the enqueue breaks adjacency — the \
                 pickup is a new prompt: {blocks:?}"
            );
        }
    }

    /// #108: Claude records a compaction as TWO adjacent events — a `system` /
    /// `compact_boundary` carrying the metadata, then a `user` event flagged
    /// `isCompactSummary` carrying the prose. Before this, the boundary was dropped
    /// entirely (no `system` arm existed) and the summary folded into a generic system
    /// note, so the trigger and the token figures never survived. They must now pair into
    /// ONE divider, and the compaction must NOT count as a turn.
    #[test]
    fn compaction_boundary_and_summary_pair_into_one_divider() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"before"}}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:01.000Z","content":"Conversation compacted","compactMetadata":{"trigger":"auto","preTokens":594718,"postTokens":8617,"cumulativeDroppedTokens":586101}}
{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"This session is being continued…"}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"after"}}
"##;
        let mut ut = Vec::new();
        let blocks = replay(&tokenize(jsonl.lines()), &mut ut, &CLAUDE_SHAPING);
        assert_eq!(
            blocks
                .iter()
                .filter(|b| matches!(b, Block::Compaction { .. }))
                .count(),
            1,
            "one divider, not a divider plus a loose note: {blocks:?}"
        );
        let Some(Block::Compaction {
            trigger,
            pre_tokens,
            post_tokens,
            summary,
        }) = blocks
            .iter()
            .find(|b| matches!(b, Block::Compaction { .. }))
        else {
            panic!("no Compaction: {blocks:?}")
        };
        assert_eq!(*trigger, CompactTrigger::Auto);
        assert_eq!((*pre_tokens, *post_tokens), (594718, 8617));
        assert_eq!(summary, "This session is being continued…");
        // Two human turns — the compaction is a seam between them, not a third.
        assert_eq!(ut.len(), 2, "compaction must not open a turn: {ut:?}");
    }

    /// The pairing is a ONE-BLOCK reach, so neither half can capture something that
    /// isn't its partner: a boundary followed by a real turn keeps an empty summary and
    /// leaves the turn alone, and a summary with no boundary before it stays the loose
    /// system note it has always been.
    #[test]
    fn unpaired_compaction_halves_degrade_cleanly() {
        let jsonl = r##"
{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:00.000Z","compactMetadata":{"trigger":"manual","preTokens":100,"postTokens":10}}
{"type":"user","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":"a real turn"}}
{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"orphan summary"}}
"##;
        let blocks = parse(jsonl);
        assert!(
            matches!(&blocks[0], Block::Compaction { trigger, summary, .. }
                if *trigger == CompactTrigger::Manual && summary.is_empty()),
            "lone boundary keeps an empty summary: {blocks:?}"
        );
        assert!(
            matches!(&blocks[1], Block::UserText(t) if t == "a real turn"),
            "the turn after a boundary is still a turn: {blocks:?}"
        );
        assert!(
            matches!(&blocks[2], Block::ToolResult(t) if t == "orphan summary"),
            "an unpaired summary stays a system note: {blocks:?}"
        );
    }

    /// A `system` record of any OTHER subtype stays dropped — the arm is deliberately
    /// narrow, and a missing/empty `compactMetadata` yields no divider at all rather
    /// than one claiming `0 → 0`.
    #[test]
    fn only_compact_boundary_system_records_surface() {
        let jsonl = r##"
{"type":"system","subtype":"hook_result","timestamp":"2026-06-30T03:00:00.000Z","content":"hook ran"}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:01.000Z","content":"no metadata here"}
"##;
        assert!(parse(jsonl).is_empty(), "{:?}", parse(jsonl));
    }

    /// #95: QoderWork's synchronous spawn result (`{kind:"agent-result",
    /// state:"completed", …}`) resolves the spawn's terminal status — its transcripts
    /// are Claude-format, but completion rides `state`, not Claude's `status`.
    #[test]
    fn qoderwork_agent_result_state_resolves_status() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-07-27T13:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-07-27T13:00:01.000Z","message":{"content":[{"type":"tool_use","id":"call_1","name":"Agent","input":{"subagent_type":"Explore","description":"find dir","prompt":"search"}}]}}
{"type":"user","timestamp":"2026-07-27T13:00:02.000Z","toolUseResult":{"kind":"agent-result","agentId":"aExplore-8df2c962","agentType":"Explore","content":"findings","state":"completed","terminateReason":"GOAL"},"message":{"content":[{"type":"tool_result","tool_use_id":"call_1","content":"findings"}]}}
"##;
        let blocks = parse(jsonl);
        let Some(Block::SubAgent(sa)) = blocks.iter().find(|b| matches!(b, Block::SubAgent(_)))
        else {
            panic!("no SubAgent: {blocks:?}")
        };
        assert_eq!(sa.agent_id, "aExplore-8df2c962");
        assert_eq!(sa.status, AgentStatus::Completed, "{blocks:?}");
    }

    /// #28, confirmed on a real store: QoderWork reports a failed spawn as
    /// `state:"error"` + `terminateReason:"ERROR"` — a word outside the original
    /// vocabulary, which used to leave the agent `Running` forever (an inflated
    /// live-agent count that never resolved). It is a failure, so it reads `Failed`.
    #[test]
    fn qoderwork_agent_result_error_state_is_failed_not_running() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-08-13T00:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-08-13T00:00:01.000Z","message":{"content":[{"type":"tool_use","id":"call_1","name":"Agent","input":{"subagent_type":"general-purpose","description":"recover","prompt":"try"}}]}}
{"type":"user","timestamp":"2026-08-13T00:00:02.000Z","toolUseResult":{"kind":"agent-result","agentId":"ageneral-purpose-fe5c9aa2","agentType":"general-purpose","content":"model queue recovery attempts exceeded","state":"error","terminateReason":"ERROR"},"message":{"content":[{"type":"tool_result","tool_use_id":"call_1","content":"terminated"}]}}
"##;
        let blocks = parse(jsonl);
        let Some(Block::SubAgent(sa)) = blocks.iter().find(|b| matches!(b, Block::SubAgent(_)))
        else {
            panic!("no SubAgent: {blocks:?}")
        };
        assert_eq!(sa.status, AgentStatus::Failed, "{blocks:?}");
    }

    /// #28, the general rule: a PRESENT-but-unrecognized result word resolves to
    /// `Unknown` — terminal and honest — never silently stays `Running`. A result line
    /// is the spawn's outcome; whatever it says, the spawn is over.
    #[test]
    fn an_unrecognized_result_state_reads_unknown_not_running() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-08-13T00:00:00.000Z","message":{"content":"go"}}
{"type":"assistant","timestamp":"2026-08-13T00:00:01.000Z","message":{"content":[{"type":"tool_use","id":"call_1","name":"Agent","input":{"subagent_type":"Explore","description":"find","prompt":"x"}}]}}
{"type":"user","timestamp":"2026-08-13T00:00:02.000Z","toolUseResult":{"kind":"agent-result","agentId":"aExplore-11","agentType":"Explore","content":"done-ish","state":"exploded"},"message":{"content":[{"type":"tool_result","tool_use_id":"call_1","content":"done-ish"}]}}
"##;
        let blocks = parse(jsonl);
        let Some(Block::SubAgent(sa)) = blocks.iter().find(|b| matches!(b, Block::SubAgent(_)))
        else {
            panic!("no SubAgent: {blocks:?}")
        };
        assert_eq!(sa.status, AgentStatus::Unknown, "{blocks:?}");
        assert!(sa.status.is_terminal(), "unknown is terminal, not running");
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

    /// `taskq` records ride a Bash result's stdout and become the same task ops the native
    /// tools produce — the queue exists precisely because those tools are absent from some
    /// builds, so a session using it rendered an empty panel while doing all its work there.
    ///
    /// Pinned here: a create lands under the id the RECORD carries (no result to wait for),
    /// state ops read their status from `changes.status.to`, a `log` note changes nothing,
    /// and a mangled or rid-less line is skipped rather than guessed at.
    #[test]
    fn taskq_records_in_a_bash_result_become_task_ops() {
        let rec = |rid: &str, op: &str, task: &str, subject: &str, changes: &str| {
            format!(
                "##taskq/v1 {{\"rid\":\"{rid}\",\"ts\":\"2026-08-29T23:20:35Z\",\"repo\":\"mdviewer\",\
                 \"op\":\"{op}\",\"task\":\"{task}\",\"subject\":\"{subject}\",\
                 \"by\":\"claude-code/hong@mac\",\"changes\":{changes}}}"
            )
        };
        let text = [
            "some ordinary command output first",
            &rec(
                "r1",
                "create",
                "1",
                "Scaffold the monorepo",
                r#"{"status":{"from":null,"to":"pending"}}"#,
            ),
            &rec(
                "r2",
                "create",
                "2",
                "Engine: inline diffs",
                r#"{"status":{"from":null,"to":"pending"}}"#,
            ),
            &rec(
                "r3",
                "claim",
                "1",
                "Scaffold the monorepo",
                r#"{"status":{"from":"pending","to":"in_progress"}}"#,
            ),
            &rec(
                "r4",
                "done",
                "1",
                "Scaffold the monorepo",
                r#"{"status":{"from":"in_progress","to":"completed"}}"#,
            ),
            // A progress note moves nothing the panel shows.
            &rec(
                "r5",
                "log",
                "2",
                "Engine: inline diffs",
                r#"{"log":{"to":"a note"}}"#,
            ),
            // Not trusted: no rid to namespace the create's join, and unparsable JSON.
            "##taskq/v1 {\"op\":\"create\",\"task\":\"9\",\"subject\":\"no rid\"}",
            "##taskq/v1 {this is not json",
        ]
        .join("\n");

        let ops = taskq_ops(&text);
        // 2 creates (each with its resolve) + claim + done. The fold itself is the engine's,
        // and `TaskFold` is deliberately NOT on the adapter seam — an adapter emits ops and
        // never reduces them — so this asserts the ops, which is this layer's whole output.
        assert_eq!(ops.len(), 6, "{ops:#?}");
        let expect = |op: &TaskOp| -> String {
            match op {
                TaskOp::Create {
                    tool_use_id,
                    subject,
                    ..
                } => format!("create({tool_use_id}, {subject})"),
                TaskOp::Resolve { tool_use_id, id } => {
                    format!(
                        "resolve({tool_use_id} -> {})",
                        id.clone().unwrap_or_default()
                    )
                }
                TaskOp::Update {
                    task_id,
                    status,
                    subject,
                    ..
                } => format!(
                    "update({task_id}, {}, {})",
                    status.clone().unwrap_or_default(),
                    subject.clone().unwrap_or_default()
                ),
                _ => "other".into(),
            }
        };
        let seen: Vec<String> = ops.iter().map(expect).collect();
        assert_eq!(
            seen,
            vec![
                // A create carries its id in the record, so its resolve rides along
                // immediately — nothing to wait for, unlike the native flow.
                "create(taskq:r1, Scaffold the monorepo)".to_string(),
                "resolve(taskq:r1 -> 1)".to_string(),
                "create(taskq:r2, Engine: inline diffs)".to_string(),
                "resolve(taskq:r2 -> 2)".to_string(),
                // State ops read the destination out of `changes.status.to`.
                // Every state op carries the subject from the record's top-level field, so a
                // stub materialized for a task this transcript never saw created still has a
                // title — the "one task, no content" the motivating session showed.
                "update(1, in_progress, Scaffold the monorepo)".to_string(),
                "update(1, completed, Scaffold the monorepo)".to_string(),
            ],
            "the log note, the rid-less record and the mangled line all contribute nothing"
        );

        // Re-seeing the same records — `list --with-history` echoes other agents' records into
        // this transcript — yields the SAME ops, which the fold applies idempotently (a
        // re-created id replaces rather than appends). That is what lets this decode stay
        // per-line and stateless like every other.
        let again: Vec<String> = taskq_ops(&text).iter().map(expect).collect();
        assert_eq!(again, seen, "records re-read identically");
    }

    /// The reported symptom, reproduced at the op level: a view that starts PART-WAY through a
    /// session (a durable resume, or a client that joined late) sees a `done` whose `create`
    /// lies in bytes it will never read. The fold materializes a stub for it (#125) — that is
    /// correct and deliberate — and the stub must still carry a TITLE, because unlike the
    /// native tools' "Updated task #5 status", a taskq record names its task on every op.
    ///
    /// Without the top-level subject this rendered exactly what the session showed: one task,
    /// no content.
    #[test]
    fn a_state_op_alone_still_names_its_task() {
        let done = "##taskq/v1 {\"rid\":\"r9\",\"ts\":\"2026-08-29T23:59:00Z\",\"op\":\"done\",\
                    \"task\":\"47\",\"subject\":\"Daemon: one server, many documents\",\
                    \"changes\":{\"status\":{\"from\":\"in_progress\",\"to\":\"completed\"},\
                    \"outcome\":{\"from\":null,\"to\":\"shipped\"}}}";
        let ops = taskq_ops(done);
        assert_eq!(ops.len(), 1, "{ops:#?}");
        let TaskOp::Update {
            task_id,
            status,
            subject,
            ..
        } = &ops[0]
        else {
            panic!("{:#?}", ops[0])
        };
        assert_eq!(task_id, "47");
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(
            subject.as_deref(),
            Some("Daemon: one server, many documents"),
            "a stub built from this op is not blank"
        );
    }

    /// End to end through the decoder: a `Bash` tool result carrying records feeds the same
    /// `Message::TaskOp` stream the native task tools do.
    #[test]
    fn taskq_ops_reach_the_message_stream_from_a_bash_result() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-08-29T23:20:00.000Z","message":{"content":"queue the work"}}
{"type":"assistant","timestamp":"2026-08-29T23:20:01.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"taskq create --subject 'Scaffold'"}}]}}
{"type":"user","timestamp":"2026-08-29T23:20:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"Task #1 created successfully: Scaffold\n##taskq/v1 {\"rid\":\"r1\",\"ts\":\"2026-08-29T23:20:03Z\",\"op\":\"create\",\"task\":\"1\",\"subject\":\"Scaffold\",\"changes\":{\"status\":{\"from\":null,\"to\":\"pending\"}}}"}]}}
"#;
        let msgs = tokenize(jsonl.lines());
        let ops: Vec<_> = msgs
            .iter()
            .filter_map(|m| match m {
                Message::TaskOp(op) => Some(op),
                _ => None,
            })
            .collect();
        assert_eq!(ops.len(), 2, "create + its resolve: {ops:#?}");
        assert!(
            matches!(ops[0], TaskOp::Create { subject, .. } if subject == "Scaffold"),
            "{:#?}",
            ops[0]
        );
        assert!(
            matches!(ops[1], TaskOp::Resolve { id: Some(id), .. } if id == "1"),
            "{:#?}",
            ops[1]
        );
    }

    /// An `Artifact` publish is lifted out of prose into a fact: the block is labelled by
    /// the ARTIFACT (`🧭 rowt-deck`, not the local `.html` that held its markup), carries the
    /// URL that only the RESULT knew, and drops that result — the rest of it is instructions to
    /// the agent, and the raw toggle still has them.
    ///
    /// The two negatives are the ones that keep this honest: a non-publish action (the tool also
    /// lists, reads and comments) is an ordinary tool call, and so is a publish whose result
    /// announced no URL — it published nothing, so there is nothing to link.
    #[test]
    fn an_artifact_publish_becomes_a_linkable_fact() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-08-28T10:00:00.000Z","message":{"content":"publish it"}}
{"type":"assistant","timestamp":"2026-08-28T10:00:01.000Z","message":{"content":[{"type":"tool_use","id":"a1","name":"Artifact","input":{"file_path":"/w/deck/rowt-deck.html","description":"A 24-slide tour.","favicon":"🧭"}}]}}
{"type":"user","timestamp":"2026-08-28T10:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"a1","content":"Published /w/deck/rowt-deck.html at https://claude.ai/code/artifact/f37a45eb-a40c\n\nTo update: republish the same file path."}]}}
{"type":"assistant","timestamp":"2026-08-28T10:00:04.000Z","message":{"content":[{"type":"tool_use","id":"a2","name":"Artifact","input":{"action":"list","limit":5}}]}}
{"type":"user","timestamp":"2026-08-28T10:00:05.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"a2","content":"1. rowt-deck — https://claude.ai/code/artifact/f37a45eb-a40c"}]}}
{"type":"assistant","timestamp":"2026-08-28T10:00:06.000Z","message":{"content":[{"type":"tool_use","id":"a3","name":"Artifact","input":{"file_path":"/w/deck/other.html","description":"nope","favicon":"📄"}}]}}
{"type":"user","timestamp":"2026-08-28T10:00:07.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"a3","content":"Refused: publishing is not enabled for this account."}]}}
"#;
        let blocks = parse(jsonl);
        let tools: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ToolUse { name, .. } if name == "Artifact"))
            .collect();
        assert_eq!(tools.len(), 3, "{blocks:?}");

        let Block::ToolUse {
            target,
            output,
            published,
            ..
        } = tools[0]
        else {
            unreachable!()
        };
        let p = published
            .as_deref()
            .expect("a publish carries its artifact");
        assert_eq!(
            target, "🧭 rowt-deck",
            "labelled by the artifact, not the file"
        );
        assert_eq!(p.name, "rowt-deck");
        assert_eq!(p.url, "https://claude.ai/code/artifact/f37a45eb-a40c");
        assert_eq!(p.description, "A 24-slide tour.");
        assert_eq!(p.icon, "🧭");
        assert_eq!(p.label(), "🧭 rowt-deck");
        assert!(
            output.is_none(),
            "the result was instructions to the agent, not information: {output:?}"
        );

        // `action: list` names no file and publishes nothing — an ordinary tool call, even
        // though its OUTPUT happens to contain an artifact URL.
        let Block::ToolUse {
            published, target, ..
        } = tools[1]
        else {
            unreachable!()
        };
        assert!(published.is_none(), "a listing published nothing");
        assert_eq!(
            target, "list",
            "…and is labelled by its action, having no file to name (it read as `Artifact()`)"
        );

        // A publish whose result announced no URL: the fact is dropped rather than left
        // half-built, so nothing renders a link to nowhere.
        let Block::ToolUse {
            published, output, ..
        } = tools[2]
        else {
            unreachable!()
        };
        assert!(published.is_none(), "no URL ⇒ nothing was published");
        assert!(
            output.as_deref().is_some_and(|o| o.contains("Refused")),
            "and its result is kept, being a real one: {output:?}"
        );
    }

    /// A title lifted out of a page's own `<title>` arrives still HTML-escaped (observed:
    /// `crux-web · Service &amp; Module Contracts`), and nothing downstream would undo it —
    /// the value travels as JSON and is written with `textContent`. One level, decoded here.
    #[test]
    fn an_artifact_title_arrives_html_escaped_and_is_decoded() {
        let jsonl = r#"
{"type":"user","timestamp":"2026-08-28T10:00:00.000Z","message":{"content":"publish"}}
{"type":"assistant","timestamp":"2026-08-28T10:00:01.000Z","message":{"content":[{"type":"tool_use","id":"a1","name":"Artifact","input":{"file_path":"/w/x.html","title":"crux-web &middot; Service &amp; Module Contracts","description":"Seams &lt;between&gt; modules &amp; their owners","favicon":"📐"}}]}}
{"type":"user","timestamp":"2026-08-28T10:00:03.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"a1","content":"Published /w/x.html at https://claude.ai/code/artifact/abc"}]}}
"#;
        let blocks = parse(jsonl);
        let Some(Block::ToolUse {
            target, published, ..
        }) = blocks
            .iter()
            .find(|b| matches!(b, Block::ToolUse { name, .. } if name == "Artifact"))
        else {
            panic!("{blocks:?}")
        };
        let p = published.as_deref().expect("published");
        assert_eq!(p.name, "crux-web · Service & Module Contracts");
        assert_eq!(p.description, "Seams <between> modules & their owners");
        assert_eq!(target, "📐 crux-web · Service & Module Contracts");
        // Numeric forms too, decimal and hex — general, so a named table need not enumerate
        // every character. An entity outside the set is left ALONE rather than guessed at.
        assert_eq!(decode_entities("a&#183;b &#x2014; c"), "a·b — c");
        assert_eq!(
            decode_entities("keep &thinsp; and &#xZZ;"),
            "keep &thinsp; and &#xZZ;",
            "an unknown entity is shown, not invented"
        );

        // Prose that merely CONTAINS an ampersand is untouched — no `;` nearby, nothing to
        // decode, and the fast path returns it whole.
        let plain = decode_entities("Tom & Jerry, R&D, a&b");
        assert_eq!(plain, "Tom & Jerry, R&D, a&b");
        // …and a lone trailing ampersand does not run off the end.
        assert_eq!(decode_entities("ends with &"), "ends with &");
        assert_eq!(decode_entities("&amp;amp;"), "&amp;", "exactly one level");
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

    /// A skill body may only nest into a `Skill` in the SAME turn.
    ///
    /// Real transcripts settle this: 27 of 32 bodies arrive two lines after their call, and every
    /// long-reach case is the same shape — jdi injects a `jdi-handoff` body with no `Skill` call
    /// at all. Unbounded, that body glued itself onto whatever skill came last, thousands of
    /// lines back, so an unrelated block grew content that was never its own. Bounding it also
    /// unpins the durability frontier (see `frontier_advances_past_a_completed_skill_turn`).
    #[test]
    fn a_skill_body_never_nests_across_a_user_turn() {
        let jsonl = r#"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"Launching skill: dump-tasks"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"next thing please"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"sure"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"Base directory for this skill: /Users/dev/.claude/skills/jdi-handoff\n\n# jdi-handoff"}]}}
"#;
        let blocks = parse(jsonl);
        assert_eq!(
            kinds(&blocks),
            vec!["skill", "user", "assistant", "tool_result"],
            "the orphan stands on its own, in order: {blocks:?}"
        );
        let Block::ToolUse { output, .. } = &blocks[0] else {
            panic!("expected the Skill block at 0: {blocks:?}")
        };
        assert!(
            !output.as_deref().unwrap_or("").contains("jdi-handoff"),
            "an earlier turn's Skill must not absorb it: {output:?}"
        );
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
        let map = claude_replay_engine::seam::build_sub_agents(&blocks);
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

    /// A completion `<status>` word we don't recognize must NOT read as `Completed` (the #26
    /// class: an unknown signal coerced to the most positive outcome). It becomes the honest
    /// terminal `Unknown` — done (so it does not stay "running"), but rendered "finished", never
    /// a false "completed".
    #[test]
    fn unknown_completion_status_is_unknown_not_completed() {
        let jsonl = r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"Review","prompt":"go"}}]}}
{"type":"user","toolUseResult":{"agentId":"aX","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aX</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>cancelled</status>\n<summary>Agent \"Review\" ended</summary>\n<result>n/a</result>\n</task-notification>"}
"##;
        let blocks = parse(jsonl);
        let Block::AgentDone { status, .. } = &blocks[1] else {
            panic!("second block is not AgentDone: {blocks:?}")
        };
        assert_eq!(
            *status,
            AgentStatus::Unknown,
            "an unrecognized status word is Unknown, not a false Completed"
        );
        assert!(status.is_terminal(), "a completion event is terminal");
        assert_eq!(
            status.done_verb(),
            "finished",
            "honest neutral verb, not 'completed'"
        );
        // The index derives the same honest terminal status from the finish event.
        let map = claude_replay_engine::seam::build_sub_agents(&blocks);
        assert_eq!(map["aX"].status, AgentStatus::Unknown);
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
            // #108 compaction: the boundary + its summary pair into ONE divider; a LONE
            // boundary keeps an empty summary; a boundary whose next line is an ordinary
            // turn must not swallow it.
            r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"before the cut"}}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:01.000Z","content":"Conversation compacted","compactMetadata":{"trigger":"auto","preTokens":594718,"postTokens":8617,"cumulativeDroppedTokens":586101}}
{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"This session is being continued from a previous conversation…"}}
{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"after the cut"}}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:04.000Z","compactMetadata":{"trigger":"manual","preTokens":725463,"postTokens":7015}}
{"type":"user","timestamp":"2026-06-30T03:00:05.000Z","message":{"content":"a real turn, not a summary"}}
{"type":"system","subtype":"other_subtype","timestamp":"2026-06-30T03:00:06.000Z","content":"ignored"}
{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:07.000Z","message":{"content":"an unpaired summary stays a system note"}}
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
        // #108: the compaction pair split BETWEEN its two halves — the case where the fold
        // has to hold an open divider across an `apply` boundary, exactly as the live tail
        // delivers it.
        assert_split(concat!(
            r#"{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"before"}}"#,
            "\n",
            r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:01.000Z","compactMetadata":{"trigger":"auto","preTokens":900,"postTokens":9}}"#,
            "\n",
            r#"{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:02.000Z","message":{"content":"continued…"}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-06-30T03:00:03.000Z","message":{"content":"after"}}"#,
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

        // #108: filling a compaction divider's summary is the SAME kind of back-patch. A live
        // reader is handed the boundary as soon as it lands (the summary is a separate line, and
        // on a long compaction arrives seconds later); without the signal the divider would sit
        // there expanding to nothing until an unrelated edit forced a re-render.
        let boundary = r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:04.000Z","compactMetadata":{"trigger":"auto","preTokens":900,"postTokens":9}}"#;
        let summary = r#"{"type":"user","isCompactSummary":true,"timestamp":"2026-06-30T03:00:05.000Z","message":{"content":"continued…"}}"#;
        let mut r3 = Replayer::new(&CLAUDE_SHAPING);
        assert_eq!(
            r3.apply(&tokenize([user, boundary].into_iter())),
            None,
            "appends only"
        );
        assert_eq!(
            r3.apply(&tokenize([summary].into_iter())),
            Some(1),
            "the summary back-patches the already-emitted divider at logical index 1"
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
    fn tool_target_relativizes_paths_under_the_cwd() {
        // Relative to the transcript's cwd (the repo root), not peek's runtime cwd.
        let base = "/Users/dev/project";
        let input = serde_json::json!({ "file_path": "/Users/dev/project/src/picker.rs" });
        assert_eq!(tool_target(&input, base), "src/picker.rs");

        // A path outside the session cwd is left absolute.
        let outside = serde_json::json!({ "file_path": "/etc/hosts" });
        assert_eq!(tool_target(&outside, base), "/etc/hosts");
    }

    #[test]
    fn running_current_cwd_relativizes_and_is_carried_per_block() {
        // A mid-session `cd` into a subdir (#173): the first tool ran under the repo root,
        // the second under the subdir. Each target relativizes against the cwd in effect at
        // ITS line (running-current, not the frozen first cwd), and each block carries that
        // cwd so the reveal action can rebuild the absolute path after the `cd`.
        let jsonl = concat!(
            r#"{"type":"assistant","cwd":"/repo","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/repo/a.rs"}}]}}"#,
            "\n",
            r#"{"type":"assistant","cwd":"/repo/sub","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/repo/sub/b.rs"}}]}}"#,
        );
        // Consecutive activity tools coalesce into a ✻ work-span (#57), so the ToolUse blocks
        // nest under `Thinking.tools` — collect from both levels, in order.
        fn collect(blocks: &[Block], out: &mut Vec<(String, String)>) {
            for b in blocks {
                match b {
                    Block::ToolUse { target, cwd, .. } => out.push((target.clone(), cwd.clone())),
                    Block::Thinking { tools, .. } => collect(tools, out),
                    _ => {}
                }
            }
        }
        let facts = |blocks: Vec<Block>| -> Vec<(String, String)> {
            let mut out = Vec::new();
            collect(&blocks, &mut out);
            out
        };
        let want = vec![
            ("a.rs".to_string(), "/repo".to_string()),
            ("b.rs".to_string(), "/repo/sub".to_string()),
        ];
        assert_eq!(
            facts(parse(jsonl)),
            want,
            "each tool relativizes against — and carries — the cwd in effect at its line"
        );
        // The frozen golden reference must fold multi-cwd identically to the streaming engine.
        assert_eq!(
            facts(parse_main(jsonl.lines(), &mut Vec::new())),
            want,
            "parse_main golden matches the streaming engine on a mid-session cd"
        );
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
