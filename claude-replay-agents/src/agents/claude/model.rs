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

/// Normalize Claude Code's assistant prose into the same presentation phases Codex persists
/// explicitly. Claude does not write a `phase` field, but the completed message carries an
/// equally strong structural signal: `stop_reason=tool_use` means the prose introduced more
/// work in the current turn, while `stop_reason=end_turn` closes the turn. Some older fixtures
/// omit `stop_reason`; a tool call in the same content array is still conclusive commentary.
/// Everything else stays unphased rather than guessing from wording or an incomplete live tail.
fn assistant_phase(v: &Value, content: &[Value]) -> Option<AssistantPhase> {
    match v.pointer("/message/stop_reason").and_then(Value::as_str) {
        Some("tool_use") => Some(AssistantPhase::Commentary),
        Some("end_turn") => Some(AssistantPhase::Final),
        _ if content
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use")) =>
        {
            Some(AssistantPhase::Commentary)
        }
        _ => None,
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
            kind: NoteKind::Plain,
        });
    }
    // A background-execution `<task-notification>`: collapse to its one-line summary/status.
    if tag_inner(s, "task-notification").is_some() {
        if let Some(line) = tag_inner(s, "summary").or_else(|| tag_inner(s, "status")) {
            let line = line.trim();
            if !line.is_empty() {
                return Some(Message::SystemNote {
                    text: line.to_string(),
                    kind: NoteKind::Plain,
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
            return Some(Message::SystemNote {
                text: cleaned,
                kind: NoteKind::Plain,
            });
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
            kind: NoteKind::Plain,
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
    // A question put to the person IS the tool's target (#41): `AskUserQuestion()` told the
    // reader nothing in the default (folded) view, where Codex's `request_user_input(<question>)`
    // does. Several questions show the first and a count, like the Codex adapter's lift.
    if let Some(questions) = input.get("questions").and_then(Value::as_array) {
        if let Some(first) = questions
            .iter()
            .find_map(|q| q.get("question").and_then(Value::as_str))
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            let more = questions.len().saturating_sub(1);
            return if more > 0 {
                format!("{first} +{more}")
            } else {
                first.to_string()
            };
        }
    }
    for k in ["file_path", "path"] {
        if let Some(v) = input.get(k).and_then(|v| v.as_str()) {
            return relativize(v, cwd);
        }
    }
    // A tool that names SEVERAL files (`SendUserFile { files: [...] }`, #207): the first, and a
    // count for the rest, like the questions lift above. Without this the header read
    // `SendUserFile()` and the only place the delivered path appeared was the tool's own output
    // prose — absolute, and not a path anything in the viewer could act on.
    if let Some(files) = input.get("files").and_then(Value::as_array) {
        let paths: Vec<&str> = files.iter().filter_map(Value::as_str).collect();
        if let Some(first) = paths.first() {
            let more = paths.len() - 1;
            let first = relativize(first, cwd);
            return if more > 0 {
                format!("{first} +{more}")
            } else {
                first
            };
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

/// Top-level record `type`s met in the corpus (census, 2026-09-20, twelve largest sessions:
/// 22 of them). The adapter's `match` handles a handful and skips the rest — `custom-title`,
/// `pr-link`, `cost-state` and the other bookkeeping rows are not records a reader wants. A
/// type outside this list is Claude Code writing something it did not write before (#264).
const RECORD_TYPES_KNOWN: &[&str] = &[
    "agent-name",
    "ai-title",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "assistant",
    "atis-latch",
    "attachment",
    "bridge-session",
    "continued-in",
    "cost-state",
    "custom-title",
    "file-history-delta",
    "file-history-snapshot",
    "frame-link",
    "history-suppression",
    "last-prompt",
    "mode",
    "permission-mode",
    "pr-link",
    "queue-operation",
    "system",
    "user",
];

/// `system` subtypes met in the corpus (census, 2026-09-20: 11).
const SYSTEM_SUBTYPES_KNOWN: &[&str] = &[
    "api_error",
    "away_summary",
    "bridge_status",
    "compact_boundary",
    "informational",
    "local_command",
    "model_consent_fallback",
    "model_refusal_fallback",
    "scheduled_task_fire",
    "stop_hook_summary",
    "turn_duration",
];

/// `attachment.type`s met in the corpus (census, 2026-09-20: 39). Most are bookkeeping —
/// `total_tokens_reminder` alone occurs 1,515 times in one session — and the handful that
/// carry something a reader wants are handled above.
const ATTACHMENT_TYPES_KNOWN: &[&str] = &[
    "advisor_tool",
    "agent_listing_delta",
    "auto_mode",
    "bash_output_audience_note",
    "batching_reminder_sent",
    "command_permissions",
    "compact_file_reference",
    // #277: `{type, organizationUuid}` — which organisation the credentials belong to, and
    // nothing else. Account bookkeeping.
    "credential_org",
    "date",
    "date_change",
    "deferred_tools_delta",
    "deferred_tools_record",
    "edited_text_file",
    "environment",
    "file",
    "hook_cancelled",
    "hook_non_blocking_error",
    "hook_system_message",
    "instructions",
    "invoked_skills",
    "mcp_instructions_delta",
    "model",
    "nested_memory",
    "plan_file_reference",
    "plan_mode",
    "plan_mode_exit",
    "prompt_snapshot",
    "queued_command",
    "read_truncation_notice",
    "remote_session_change",
    "session_context",
    "silent_turn_reminder",
    "skill_listing",
    "task_reminder",
    "task_status",
    "thinking_drop",
    "thinking_stripped",
    "total_tokens_reminder",
    "ultra_effort_enter",
    "ultra_effort_exit",
];

/// `message.content[]` block types met in the corpus. Short by design: this is the message
/// vocabulary itself, and a new entry here is a new kind of thing an agent can say.
const CONTENT_TYPES_KNOWN: &[&str] = &[
    "text",
    "thinking",
    "redacted_thinking",
    "tool_use",
    "tool_result",
    "image",
    "document",
    "server_tool_use",
    "web_search_tool_result",
];

/// Report a record shape outside the known vocabulary (#264). One place, so the three
/// categories cannot drift in how they describe themselves.
fn note_unknown_shape(v: &Value, at: UnknownAt, name: Option<&str>, known: &[&str]) {
    let name = name.unwrap_or("(absent)");
    if known.contains(&name) {
        return;
    }
    note_unknown(
        "claude",
        at,
        name,
        v.get("version").and_then(|x| x.as_str()),
        v.get("sessionId").and_then(|x| x.as_str()),
    );
}

/// The `toolUseResult` keys this adapter READS. Everything it does with a tool result comes
/// from one of these eleven. (#280 moved `answers` and `annotations` here from the ignored list:
/// they are an `AskUserQuestion`'s reply, which the card used to take from the result's prose
/// and could not read back when the reader typed their own answer or wrote notes. #281 moved
/// `afkTimeoutMs`: a question the client stopped waiting on, which the card said was still
/// waiting.)
const TOOL_RESULT_READ: &[&str] = &[
    "afkTimeoutMs",
    "agentId",
    "annotations",
    "answers",
    "bashEditDiff",
    "outputFile",
    "state",
    "stderr",
    "stdout",
    "structuredPatch",
    "workflowName",
];

/// The `toolUseResult` keys this adapter has SEEN and deliberately does not read (#264).
///
/// A census of the twelve largest sessions on 2026-09-20 found **125 distinct top-level keys**;
/// eight were read then and the other 117 were not (three of which #280 and #281 have since
/// moved up to the read list). Writing them down is the whole mechanism:
/// "report any key no code reads" would have fired on all 117 on its first run — `isImage`
/// 80,791 times, `noOutputExpected` 80,791, `userModified` 10,051 — and a log nobody can read
/// is a log nobody reads. Against this list, the only thing reported is a key that did not
/// exist when the list was taken, which is exactly the question worth asking: has the format
/// moved since we last looked?
///
/// `bashEditDiff` is the proof. It first appears on 2026-09-13 (client 2.1.270) and we found
/// out a week later from a screenshot (#263); against a snapshot taken before that date it
/// would have raised its hand the first time a session was parsed.
///
/// **Adding a key here is a deliberate act.** It says "seen it, it carries nothing we render" —
/// so it belongs in the same commit as the look that decided so, not in a sweep to make a test
/// pass. `an_unrecognised_tool_result_key_is_reported_and_a_known_one_is_not` and
/// `every_category_reports_what_is_new_and_nothing_that_is_known` are the tests that notice.
const TOOL_RESULT_KNOWN_IGNORED: &[&str] = &[
    "agentType",
    "artifactRead",
    "artifact_id",
    "artifacts",
    "attachments",
    "audience",
    "backgroundCwdHint",
    "backgroundTaskId",
    "backgroundedByUser",
    "bytes",
    "canEdit",
    "canReadOutputFile",
    "cancelledWakeups",
    "capabilities",
    "caption",
    "clampedDelaySeconds",
    "code",
    "codeText",
    "command",
    "commandName",
    "content",
    "contentType",
    "contract",
    "dangerouslyDisableSandbox",
    "description",
    "disabledReason",
    "display",
    // #277, CronCreate: whether the job outlives the session. Its result text says so
    // ("Session-only (not written to disk …)").
    "durable",
    "durationMs",
    "durationSeconds",
    "error",
    "file",
    "filePath",
    "firstPage",
    "gitOperation",
    // #277, CronCreate: the schedule in words, which its result text already states.
    "humanSchedule",
    "interrupted",
    "isAgent",
    "isAsync",
    "isBase64",
    "isImage",
    "listing",
    "liveSubscription",
    "localSent",
    "matches",
    "memdirStamped",
    "message",
    "method",
    // #277, SendMessage: an opaque delivery id; the result text names where the message went.
    "msg_id",
    "name",
    "newString",
    "noOutputExpected",
    "notifications",
    "oldString",
    "originalFile",
    "path",
    "paths",
    "persistedOutputPath",
    "persistedOutputSize",
    "persistent",
    "pin",
    "plan",
    "projectId",
    "projects",
    "prompt",
    "pushSent",
    "query",
    "questions",
    "read",
    // #277, CronCreate: whether the job repeats ("Scheduled recurring job …" in its text).
    "recurring",
    "remaining",
    "replaceAll",
    "resolvedModel",
    "result",
    "results",
    "resumedAgentId",
    "retrieval_status",
    "returnCodeInterpretation",
    "runId",
    "scheduledFor",
    "scope",
    "scriptPath",
    "searchCount",
    "sentAt",
    "seq",
    "staleReadFileStateHint",
    "staleRecovered",
    "status",
    "statusChange",
    "stopped",
    "stored",
    "success",
    "summary",
    "task",
    "taskId",
    "taskType",
    "task_id",
    "task_type",
    "tasks",
    "threads",
    "timedOutAfterMs",
    "timeoutMs",
    "title",
    "toolStats",
    "totalDurationMs",
    "totalTokens",
    "totalToolUseCount",
    "total_deferred_tools",
    "transcriptDir",
    "truncated",
    "type",
    "updated",
    "updatedFields",
    "url",
    "usage",
    "userModified",
    "version",
    "wasClamped",
];

/// Generic key NAMES this adapter knows only inside the result SHAPE they were looked at in
/// (#277).
///
/// The two lists above are keyed by name alone, which is right for a name that means one thing
/// (`bashEditDiff`, `msg_id`) and wrong for one that could mean anything: putting `id` on the
/// ignored list would silence it from every tool Claude Code adds after this. So each name here
/// is known only when EVERY key of the result belongs to the shape it was judged in, and is
/// reported anywhere else exactly like a key nobody has met.
const TOOL_RESULT_KNOWN_IN_SHAPE: &[(&str, &[&str])] = &[
    // CronCreate returns all four and CronDelete `{id}` alone; the result text already states the
    // job id, its schedule in words, whether it recurs and whether it is session-only.
    ("id", &["durable", "humanSchedule", "id", "recurring"]),
    // Read's `file_unchanged` (`{type, file, source}`): `source: "seeded"` says the file was in
    // context from seeding (a CLAUDE.md) rather than from an earlier Read. The page already draws
    // the file_unchanged result, whose text says the file is in context; the provenance word
    // adds nothing a reader needs.
    ("source", &["file", "source", "type"]),
];

/// The `toolUseResult` keys this adapter neither reads nor has already met (#264), in the
/// order the result carries them.
fn unknown_tool_result_keys(tur: &Value) -> Vec<&str> {
    let Some(obj) = tur.as_object() else {
        return Vec::new();
    };
    let in_shape = |k: &str| {
        TOOL_RESULT_KNOWN_IN_SHAPE
            .iter()
            .any(|(name, shape)| *name == k && obj.keys().all(|key| shape.contains(&key.as_str())))
    };
    obj.keys()
        .map(String::as_str)
        .filter(|k| {
            !TOOL_RESULT_READ.contains(k) && !TOOL_RESULT_KNOWN_IGNORED.contains(k) && !in_shape(k)
        })
        .collect()
}

/// Report any `toolUseResult` key this adapter neither reads nor has already met (#264).
fn note_unknown_tool_result_keys(tur: &Value, version: Option<&str>, at: Option<&str>) {
    for k in unknown_tool_result_keys(tur) {
        note_unknown("claude", UnknownAt::ToolResultKey, k, version, at);
    }
}

/// Parse `toolUseResult.bashEditDiff` into the same hunks an Edit produces (#263).
///
/// Claude Code began recording this on 2026-09-13 (client 2.1.270): every Bash command that
/// edits a file carries a real unified diff beside its stdout, and we rendered none of it —
/// 975 records across the owner's sessions, and the only reason it was noticed was a
/// screenshot. The shape, measured over all 975 rather than read off one:
///
/// - `files[]` is CAPPED AT FIVE and `moreFiles` counts the rest (every one of the 102 records
///   with `moreFiles > 0` has exactly five files). The names past the cap are still in
///   `changedFiles`, so nothing is lost by naming them.
/// - a file holds 1..23 hunks, so a file is several groups exactly like a multi-hunk Edit.
/// - `unavailable: true` (12 records) means no diff could be produced.
/// - a file that changed but cannot be diffed — a tarball, say — has no hunks at all; one
///   record here changed sixteen of them.
///
/// So every file the record names becomes at least one hunk: a real one where there is a diff,
/// and an EMPTY one (no lines) where the name is all that is known. The renderers draw the
/// name either way, and a file with no rows says "this changed and there is nothing to show"
/// rather than vanishing.
fn parse_bash_edit_diff(tur: &Value) -> Option<Vec<Hunk>> {
    let diff = tur.get("bashEditDiff")?.as_object()?;
    let mut out: Vec<Hunk> = Vec::new();
    let mut named: Vec<String> = Vec::new();
    for file in diff
        .get("files")
        .and_then(|f| f.as_array())
        .into_iter()
        .flatten()
    {
        let path = file
            .get("filePath")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        named.push(path.clone());
        let hunks: Vec<&Value> = file
            .get("hunks")
            .and_then(|h| h.as_array())
            .into_iter()
            .flatten()
            .collect();
        if hunks.is_empty() {
            out.push(Hunk {
                old_start: 0,
                new_start: 0,
                lines: Vec::new(),
                file: Some(path),
            });
            continue;
        }
        for h in hunks {
            let new_start = h.get("newStart").and_then(|n| n.as_u64()).unwrap_or(1) as usize;
            let old_start = h
                .get("oldStart")
                .and_then(|n| n.as_u64())
                .map(|n| n as usize)
                .unwrap_or(new_start);
            let lines = h
                .get("lines")
                .and_then(|l| l.as_array())
                .into_iter()
                .flatten()
                .filter_map(|l| l.as_str().map(String::from))
                .collect();
            out.push(Hunk {
                old_start,
                new_start,
                lines,
                file: Some(path.clone()),
            });
        }
    }
    // The files past the five-file cap: named from `changedFiles`, which lists them all. Their
    // content is not in the transcript, so a name with no rows is the honest whole of it.
    for path in diff
        .get("changedFiles")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str())
    {
        if !named.iter().any(|n| n == path) {
            out.push(Hunk {
                old_start: 0,
                new_start: 0,
                lines: Vec::new(),
                file: Some(path.to_string()),
            });
        }
    }
    (!out.is_empty()).then_some(out)
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
                // An Edit is one file and the call's target already names it.
                file: None,
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
// The journal is the roster, and it is append-only while the run proceeds. NEWER runs name each
// member: a `started` record carries `label` (the workflow's own, e.g. "find:owned-path-plain")
// and `phase` (the phase() group it ran under). Measured across the 68 runs on this machine,
// 9 carry them and 59 do not — 260 records of 1,947 — so both shapes are live and a member
// without a label is titled from the first line of its result, and until then by its position.

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
    // Which members the journal NAMED. Local rather than a field on the member: it exists only
    // to stop a later `result` from overwriting the run's own name with a line of prose.
    let mut labelled: std::collections::HashSet<String> = std::collections::HashSet::new();
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
                // The workflow's own name for this agent beats anything derivable: a label reads
                // "find:owned-path-plain" where a result's first line reads like prose and a
                // launch position reads like nothing. Empty strings are treated as absent.
                let label = v
                    .get("label")
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|l| !l.is_empty());
                let phase = v
                    .get("phase")
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string);
                if label.is_some() {
                    labelled.insert(id.to_string());
                }
                members.push(SubAgent {
                    agent_id: id.to_string(),
                    tool_use_id: String::new(),
                    agent_type: "workflow".into(),
                    phase,
                    description: label
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("agent {}", members.len() + 1)),
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
                    // A label is the run's OWN name for this agent, so a result never overwrites
                    // one — the result title exists to give a name to a member that has none.
                    if let (Some(title), false) = (title, labelled.contains(id)) {
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
            asked,
            ..
        } => {
            // #280: the reader's answers, from the result's structured half — and #281, why
            // there are none when there are none.
            if let Some(a) = asked.as_deref_mut() {
                apply_answers(a, tur);
                a.unanswered = unanswered(a, txt, tur, is_error);
            }
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
            // An Edit's own patch first; a Bash command that edited files carries its diff
            // under a different key and never has a `structuredPatch` (#263).
            *patch = parse_patch(tur).or_else(|| parse_bash_edit_diff(tur));
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
            // An API failure is not the model speaking (#236). The client writes it as an ordinary
            // assistant message — "API Error", "Please run /login", "Login expired", "Prompt is too
            // long" — and flags the RECORD `isApiErrorMessage`. Nothing read that flag, so the
            // viewer attributed a login failure to the assistant: 175 such records across 39 of
            // the transcripts on this machine, unfilterable and uncountable because nothing could
            // tell them apart from prose. The flag is the transcript's own statement of what the
            // message is, so it is read here, ahead of any phase or content inspection.
            if v.get("isApiErrorMessage")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                for blk in content {
                    if let Some(t) = blk.get("text").and_then(Value::as_str) {
                        let t = t.trim();
                        if !t.is_empty() {
                            msgs.push(Message::SystemNote {
                                text: t.to_string(),
                                // The turn DIED here (#249). Without this the state machine
                                // sees no failed TOOL result and calls the turn `Done`.
                                kind: NoteKind::Failure,
                            });
                        }
                    }
                }
                return;
            }
            let phase = assistant_phase(&v, content);
            for blk in content {
                match blk.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                            if !t.trim().is_empty() {
                                if let Some(phase) = phase {
                                    msgs.push(Message::AssistantMessage {
                                        text: t.to_string(),
                                        phase,
                                        inferred: true,
                                    });
                                } else {
                                    msgs.push(Message::AssistantText(t.to_string()));
                                }
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
                    // A server-side tool the model consulted mid-turn (#237). Every one in this
                    // store is the advisor, and its call carries an EMPTY input — the advisor takes
                    // no parameters; it is handed the conversation. So a consult is a call with a
                    // name and nothing else, which is still far better than the hole it was: the
                    // turn appeared to stop and resume with no cause, 691 times here.
                    Some("server_tool_use") => {
                        let name = blk.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let id = blk.get("id").and_then(Value::as_str).unwrap_or("");
                        msgs.push(Message::ToolUse {
                            id: id.to_string(),
                            name: name.to_string(),
                            input: blk.get("input").cloned().unwrap_or(Value::Null),
                            cwd: String::new(),
                        });
                    }
                    // …and its reply. The advice itself is REDACTED by the transcript format
                    // (`advisor_redacted_result`, 596 of 605 here, a 4.7 KB `encrypted_content`
                    // blob), so there is nothing to render and saying so is the honest result.
                    // The 9 that FAILED are the ones worth surfacing — overloaded, unavailable,
                    // too_many_requests — and they were invisible.
                    Some("advisor_tool_result") => {
                        let tid = blk
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let body = blk.get("content");
                        let kind = body
                            .and_then(|c| c.get("type"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let code = body
                            .and_then(|c| c.get("error_code"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let failed = kind == "advisor_tool_result_error" || !code.is_empty();
                        let text = if failed {
                            if code.is_empty() {
                                "the consult failed".to_string()
                            } else {
                                format!("the consult failed: {code}")
                            }
                        } else {
                            "advice returned — the transcript redacts its text".to_string()
                        };
                        msgs.push(Message::ToolResult {
                            tool_use_id: tid,
                            text,
                            tur: Value::Null,
                            is_error: Some(failed),
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
                        // A `taskq create` in a Bash command: the draft, awaiting the id its
                        // result will announce. Only the command carries the description.
                        // The decode itself is the engine's (`seam::taskq_create_ops`) —
                        // Claude's part is knowing that the command lives at `input.command`.
                        if name == "Bash" {
                            let cmd = input.get("command").and_then(|v| v.as_str());
                            for op in taskq_create_ops(id, cmd.unwrap_or_default()) {
                                msgs.push(Message::TaskOp(op));
                            }
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
                    // The run CHANGED MODEL mid-session because the first was unavailable
                    // (#265). `{"type":"fallback","from":{"model":…},"to":{"model":…}}` — two
                    // names and nothing else. Found by #264's log the first time it was run
                    // against the largest sessions, having arrived unnoticed since at least
                    // client 2.1.220.
                    //
                    // A NOTE rather than a Block variant, deliberately. `Compaction` earns a
                    // variant because it carries structured numbers the pages draw as a
                    // divider with figures, and because it is central; this is two strings and
                    // five occurrences across every session on this machine. The note path
                    // (#236) already makes API errors and hook failures visible on both pages,
                    // which is exactly the weight this deserves — and it matters at all
                    // because the session card names ONE model, and after a fallback that
                    // answer is wrong for every turn that follows.
                    Some("fallback") => {
                        let model = |k: &str| {
                            blk.get(k)
                                .and_then(|m| m.get("model"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("unknown")
                                .to_string()
                        };
                        msgs.push(Message::SystemNote {
                            text: format!("Model fallback: {} → {}", model("from"), model("to")),
                            kind: NoteKind::Plain,
                        });
                    }
                    // #264: an ASSISTANT content block of a kind no arm handles. The user arm
                    // has the same guard — a new message part can arrive on either side, and
                    // the one that is never watched is the one it arrives on.
                    other => {
                        note_unknown_shape(&v, UnknownAt::ContentType, other, CONTENT_TYPES_KNOWN)
                    }
                }
            }
        }
        // A context-compaction boundary (#108).
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("compact_boundary") => {
            if let Some(m) = compact_boundary(&v) {
                msgs.push(m);
            }
        }
        // A slash command the client ran locally (#235). The same `<command-name>` /
        // `<local-command-stdout>` pair `classify_user_string` already understands, but written on
        // a `system` record instead of a `user` one — and the viewer dropped every `system`
        // subtype but the boundary above, calling them "agent bookkeeping with no reader value".
        // That was true of the subtypes that existed when it was written and is not true of this
        // one: measured across the 40 most recent transcripts on this machine, 113 slash-command
        // invocations ride `system/local_command` against 302 on `user`, BOTH shapes emitted by
        // the same client version — so this is not a migration to wait out. `/context`, `/model`,
        // `/loop` and `/remote-control` were invisible.
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("local_command") => {
            if let Some(text) = v.get("content").and_then(Value::as_str) {
                if let Some(m) = classify_user_string(text, injection_of(&v)) {
                    msgs.push(m);
                }
            }
        }
        // An API call that failed and was retried (#236). It carries no `content` — the story is
        // in `error` and `retryAttempt`, so the note is composed rather than copied. This is the
        // only record that explains a turn which appears to stall.
        // #257: the turn's own wall time, which CLOSES a turn rather than opening one. The ms
        // FLOOR to seconds — the reader is shown whole seconds, and rounding 59.6s up to a
        // minute would be a claim the record does not make.
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("turn_duration") => {
            if let Some(ms) = v.get("durationMs").and_then(Value::as_u64) {
                msgs.push(Message::TurnDuration {
                    secs: ms / 1000,
                    at: ev_ts,
                });
            }
        }
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("api_error") => {
            if let Some(text) = api_error_note(&v) {
                // Same as the flagged-record arm above: this turn failed, it did not finish.
                msgs.push(Message::SystemNote {
                    text,
                    kind: NoteKind::Failure,
                });
            }
        }
        // The turn's own recap (#239). Verified against a real pair rather than assumed: the
        // summary reads "all queued tasks are done and released through v1.7.1… Next: your
        // decision on task #17", and the turn it follows is exactly that work. So it describes
        // THAT TURN, addressed to a reader who stepped away — 1,728 of them here, median 236
        // characters, never repeated. On a long session it is a free outline.
        Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("away_summary") => {
            if let Some(text) = v.get("content").and_then(Value::as_str) {
                // "(disable recaps in /config)" is a hint for the terminal that wrote it, not
                // part of the recap — 305 of the 1,728 carry it.
                let text = text
                    .trim()
                    .trim_end_matches("(disable recaps in /config)")
                    .trim();
                if !text.is_empty() {
                    msgs.push(Message::SystemNote {
                        text: text.to_string(),
                        kind: NoteKind::Plain,
                    });
                }
            }
        }
        // A /loop or cron routine started this turn (#238), and the client's own warnings about
        // the run changing underneath the reader — an account that changed, a monitor that
        // disconnected. `notice` stays dropped, being the quieter half of those 165 records.
        Some("system")
            if v.get("subtype").and_then(|s| s.as_str()) == Some("scheduled_task_fire")
                || (v.get("subtype").and_then(|s| s.as_str()) == Some("informational")
                    && v.get("level").and_then(Value::as_str) == Some("warning")) =>
        {
            if let Some(text) = v.get("content").and_then(Value::as_str) {
                let text = text.trim();
                if !text.is_empty() {
                    msgs.push(Message::SystemNote {
                        text: text.to_string(),
                        kind: NoteKind::Plain,
                    });
                }
            }
        }

        Some("user") => {
            let tur = v.get("toolUseResult").cloned().unwrap_or(Value::Null);
            // #264: anything in here we neither read nor have already met is the format
            // moving under us — the only way we learn that without a screenshot.
            note_unknown_tool_result_keys(
                &tur,
                v.get("version").and_then(|x| x.as_str()),
                v.get("sessionId").and_then(|x| x.as_str()),
            );
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
                            for op in taskq_ops(tid, &txt) {
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
                        // #264: a content block of a kind no arm above handles. Reported, not
                        // dropped in silence — this is where a new message part would arrive.
                        other => note_unknown_shape(
                            &v,
                            UnknownAt::ContentType,
                            other,
                            CONTENT_TYPES_KNOWN,
                        ),
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
                Some("popAll") => Some(QueueOpKind::PopAll),
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
                let reason = v.get("reason").and_then(|r| r.as_str()).map(str::to_string);
                msgs.push(Message::QueueOp {
                    op,
                    content,
                    prose,
                    reason,
                });
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
            } else if let Some(note) = a.and_then(attachment_note) {
                // A hook that FAILED or TIMED OUT (#236). Both arrive as attachments, but neither
                // is an attachment in any useful sense — they are the run telling you a hook did
                // not do what it was asked. 38 failures and ~8,000 timed-out tool calls were
                // invisible before this.
                msgs.push(Message::SystemNote {
                    text: note,
                    kind: NoteKind::Plain,
                });
            } else if let Some(att) = a.and_then(attachment_from_event) {
                msgs.push(Message::Attachment(att));
            } else {
                // #264: an attachment of a type nothing above claimed. Most attachment types
                // ARE bookkeeping and say nothing here; this is one the vocabulary has never
                // seen.
                note_unknown_shape(
                    &v,
                    UnknownAt::AttachmentType,
                    a.and_then(|a| a.get("type")).and_then(|t| t.as_str()),
                    ATTACHMENT_TYPES_KNOWN,
                );
            }
        }
        // #264: a top-level record type no arm matched — Claude Code writing a row it did not
        // write before. A `system` record says which subtype, since that is the name that
        // moves.
        other => {
            if other == Some("system") {
                note_unknown_shape(
                    &v,
                    UnknownAt::SystemSubtype,
                    v.get("subtype").and_then(|x| x.as_str()),
                    SYSTEM_SUBTYPES_KNOWN,
                );
            } else {
                note_unknown_shape(&v, UnknownAt::RecordType, other, RECORD_TYPES_KNOWN);
            }
        }
    }
}

/// The note an `api_error` record composes to — one function, so the streaming path and the
/// golden reference cannot word it differently (#236).
fn api_error_note(v: &Value) -> Option<String> {
    if v.get("subtype").and_then(Value::as_str) != Some("api_error") {
        return None;
    }
    let e = v.get("error");
    let msg = e
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .or_else(|| e.and_then(|e| e.get("formatted")).and_then(Value::as_str))
        .unwrap_or("API error")
        .trim();
    let mut text = format!("API error: {}", msg.lines().next().unwrap_or(msg));
    if let Some(status) = e.and_then(|e| e.get("status")).and_then(Value::as_i64) {
        text.push_str(&format!(" (HTTP {status})"));
    }
    match (
        v.get("retryAttempt").and_then(Value::as_i64),
        v.get("maxRetries").and_then(Value::as_i64),
    ) {
        (Some(n), Some(max)) => text.push_str(&format!(" \u{2014} retry {n} of {max}")),
        (Some(n), None) => text.push_str(&format!(" \u{2014} retry {n}")),
        _ => {}
    }
    if e.and_then(|e| e.get("isNetworkDown"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        text.push_str(", network down");
    }
    Some(text)
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
            // An ordinary spawn belongs to no workflow phase (#241).
            phase: None,
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
            asked: asked_from_input(name, input).map(Box::new),
        }
    }
}

/// #255: the questions an `AskUserQuestion` call put to the reader, lifted from its INPUT.
///
/// The transcript records every question with its header, its multi-select flag and every
/// option's label AND description; the block used to keep only `target`, which is the FIRST
/// question's text (`tool_target`). So a two-question call offering three options each showed 2
/// of the 8 things the asker wrote, and the reader could not see what was declined.
///
/// `None` for every other tool, and for a malformed input — a call with no `questions` array is
/// not an ask, and inventing an empty one would draw an empty card.
fn asked_from_input(name: &str, input: &Value) -> Option<Asked> {
    if name != "AskUserQuestion" {
        return None;
    }
    let questions: Vec<AskedQuestion> = input
        .get("questions")?
        .as_array()?
        .iter()
        .filter_map(|q| {
            let text = q.get("question").and_then(Value::as_str)?;
            Some(AskedQuestion {
                header: q
                    .get("header")
                    .and_then(Value::as_str)
                    .map(decode_entities)
                    .unwrap_or_default(),
                question: decode_entities(text),
                multi_select: q
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                options: q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|os| {
                        os.iter()
                            .filter_map(|o| {
                                let label = o.get("label").and_then(Value::as_str)?;
                                Some(AskedOption {
                                    label: decode_entities(label),
                                    description: o
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .map(decode_entities)
                                        .unwrap_or_default(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                // The call is only the asking; `apply_result` brings the answers (#280).
                answer: None,
                notes: String::new(),
            })
        })
        .collect();
    (!questions.is_empty()).then_some(Asked {
        questions,
        unanswered: None,
    })
}

/// #280: what the reader answered, joined onto the questions the call put.
///
/// The client records it twice. The result's PROSE is a sentence for the agent, and cannot be
/// read back in general: a quote the reader typed ends the quoted answer early, a notes-only
/// answer is written `=(no option selected) notes: …` with no quotes at all, and a selected
/// option's preview is appended after it. The STRUCTURED half is exact — `answers` maps each
/// question's whole text to its answer, `annotations` maps it to `{notes, preview}` — so it is
/// the half read here. (`preview` is the chosen option's own preview, the asker's text rather
/// than the reader's, and is not carried.)
///
/// The answer is kept verbatim. The one exception is the client's placeholder for an answer
/// given as notes alone, `(notes only)`: that is the client's word, not the reader's, so the
/// answer stays `None` and the notes carry the reply.
fn apply_answers(asked: &mut Asked, tur: &Value) {
    const NOTES_ONLY: &str = "(notes only)";
    let answers = tur.get("answers").and_then(Value::as_object);
    let annotations = tur.get("annotations").and_then(Value::as_object);
    // Keyed by the question's WHOLE text. The question was entity-decoded when it was lifted
    // from the input, so each key is decoded the same way before it is compared.
    let lookup = |map: Option<&serde_json::Map<String, Value>>, question: &str| -> Option<Value> {
        map?.iter()
            .find(|(k, _)| decode_entities(k) == question)
            .map(|(_, v)| v.clone())
    };
    for q in &mut asked.questions {
        if let Some(a) = lookup(answers, &q.question) {
            q.answer = a
                .as_str()
                .map(str::trim)
                .filter(|a| !a.is_empty() && *a != NOTES_ONLY)
                .map(decode_entities);
        }
        if let Some(n) = lookup(annotations, &q.question) {
            q.notes = n
                .get("notes")
                .and_then(Value::as_str)
                .map(|n| decode_entities(n.trim()))
                .unwrap_or_default();
        }
    }
}

/// #281: why a call that has come back carries no answer — `None` when it carries one, and for
/// a result that says nothing either way (an older client's prose-only answer, which the page
/// still reads).
///
/// Measured over the owner's 223 questions: 198 answered, 20 DECLINED and 5 TIMED OUT. A timeout
/// is structured (`afkTimeoutMs`, with `answers` empty). A decline is the format's failure fact
/// (`is_error`) carrying the client's refusal — the text "The user doesn't want to proceed with
/// this tool use…", the `toolUseResult` "User rejected tool use" or that same sentence behind
/// "Error: " — and all 20 error results are that. An error that is NOT a refusal is kept apart as
/// `Failed`, so a call that broke is never said to have been declined by the reader.
fn unanswered(asked: &Asked, txt: &str, tur: &Value, is_error: Option<bool>) -> Option<Unanswered> {
    if asked
        .questions
        .iter()
        .any(|q| q.answer.is_some() || !q.notes.is_empty())
    {
        return None;
    }
    if let Some(after_ms) = tur.get("afkTimeoutMs").and_then(Value::as_u64) {
        return Some(Unanswered::TimedOut { after_ms });
    }
    if is_error != Some(true) {
        return None;
    }
    const REFUSALS: [&str; 2] = ["doesn't want to proceed", "rejected tool use"];
    let refused = |s: &str| REFUSALS.iter().any(|r| s.contains(r));
    Some(if refused(txt) || tur.as_str().is_some_and(refused) {
        Unanswered::Declined
    } else {
        Unanswered::Failed
    })
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
                // #236 mirror: an API failure is the client's, not the model's.
                if v.get("isApiErrorMessage")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    for blk in content {
                        if let Some(t) = blk.get("text").and_then(Value::as_str) {
                            let t = t.trim();
                            if !t.is_empty() {
                                out.push(Block::ToolResult(t.to_string()));
                            }
                        }
                    }
                    continue;
                }
                let phase = assistant_phase(&v, content);
                for blk in content {
                    match blk.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = blk.get("text").and_then(|t| t.as_str()) {
                                if !t.trim().is_empty() {
                                    if let Some(phase) = phase {
                                        out.push(Block::AssistantMessage {
                                            text: t.to_string(),
                                            phase,
                                            inferred: true,
                                        });
                                    } else {
                                        out.push(Block::AssistantText(t.to_string()));
                                    }
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
                        // #237 mirror: a server-side consult is a call, and its outcome is
                        // worth having even though the format redacts the advice itself.
                        Some("server_tool_use") => {
                            let name = blk.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let id = blk.get("id").and_then(Value::as_str).unwrap_or("");
                            tool_slot.insert(id.to_string(), out.len());
                            out.push(Block::ToolUse {
                                name: name.to_string(),
                                target: String::new(),
                                diffs: vec![],
                                output: None,
                                patch: None,
                                read_lines: None,
                                cwd: String::new(),
                                execution: None,
                                published: None,
                                asked: None,
                            });
                        }
                        Some("advisor_tool_result") => {
                            let tid = blk.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                            let body = blk.get("content");
                            let kind = body
                                .and_then(|c| c.get("type"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let code = body
                                .and_then(|c| c.get("error_code"))
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            let failed = kind == "advisor_tool_result_error" || !code.is_empty();
                            let text = if failed {
                                if code.is_empty() {
                                    "the consult failed".to_string()
                                } else {
                                    format!("the consult failed: {code}")
                                }
                            } else {
                                "advice returned \u{2014} the transcript redacts its text"
                                    .to_string()
                            };
                            if let Some(&idx) = tool_slot.get(tid) {
                                if let Block::ToolUse {
                                    output, execution, ..
                                } = &mut out[idx]
                                {
                                    *output = Some(text);
                                    // A failed consult carries the failure on the call, exactly as
                                    // the streaming path does through `ToolResult { is_error }`.
                                    if failed {
                                        *execution = Some(ToolExecution {
                                            status: Some(ToolStatus::Failed),
                                            exit_code: None,
                                            duration: None,
                                        });
                                    }
                                }
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
                                    phase: None,
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
                                    asked: None,
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
            // #235 mirror: a slash command the client recorded on a `system` record rather than a
            // `user` one. This reference is pinned bit-identical to the streaming path by
            // `replay_tokenize_matches_parse_main`, so an arm added there has to be added here —
            // and the pin can only SEE the difference if a fixture carries such a record, which
            // is why one was added alongside this.
            Some("system")
                if v.get("subtype").and_then(|s| s.as_str()) == Some("local_command") =>
            {
                if let Some(text) = v.get("content").and_then(Value::as_str) {
                    push_user_string(text, &mut out, &mut queue, &mut suppress);
                }
            }
            // #239 mirror: the turn's own recap.
            Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("away_summary") => {
                if let Some(text) = v.get("content").and_then(Value::as_str) {
                    let text = text
                        .trim()
                        .trim_end_matches("(disable recaps in /config)")
                        .trim();
                    if !text.is_empty() {
                        out.push(Block::ToolResult(text.to_string()));
                    }
                }
            }
            // #238 mirror: a scheduled fire, and the client's warnings about the run.
            Some("system")
                if v.get("subtype").and_then(|s| s.as_str()) == Some("scheduled_task_fire")
                    || (v.get("subtype").and_then(|s| s.as_str()) == Some("informational")
                        && v.get("level").and_then(Value::as_str) == Some("warning")) =>
            {
                if let Some(text) = v.get("content").and_then(Value::as_str) {
                    let text = text.trim();
                    if !text.is_empty() {
                        out.push(Block::ToolResult(text.to_string()));
                    }
                }
            }
            // #236 mirror: an API call that failed and retried. Composed, not copied — the record
            // carries no `content`.
            Some("system") if v.get("subtype").and_then(|s| s.as_str()) == Some("api_error") => {
                if let Some(text) = api_error_note(&v) {
                    out.push(Block::ToolResult(text));
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
                // #264: anything in here we neither read nor have already met is the format
                // moving under us — the only way we learn that without a screenshot.
                note_unknown_tool_result_keys(
                    &tur,
                    v.get("version").and_then(|x| x.as_str()),
                    v.get("sessionId").and_then(|x| x.as_str()),
                );
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
                    // #242: `popAll` EMPTIES the queue. Mirrors the production fold in
                    // `replay.rs` — drop every outstanding marker, not just the named one.
                    Some("popAll") => {
                        for item in std::mem::take(&mut queue) {
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
                } else if let Some(note) = a.and_then(attachment_note) {
                    // #236 mirror: a hook that failed, or one killed for running long.
                    out.push(Block::ToolResult(note));
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
/// A hook that failed, or one the client killed for running too long (#236).
///
/// `hook_non_blocking_error` is the plain failure: `exitCode` 1 in every one measured, with the
/// command's `stderr`. `hook_cancelled` is the timeout, and it is written TWICE for one event —
/// once when the pre-hook is cut off and once when the post-hook is cancelled with it — so only
/// the one that says `timedOut` is reported, which is one per affected tool call rather than two.
/// A cancellation that did not time out is the ordinary consequence of something else failing and
/// says nothing on its own.
fn attachment_note(a: &Value) -> Option<String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").trim();
    match a.get("type").and_then(Value::as_str)? {
        // The model changed mid-session (#238, 389 records). "Which model wrote this part" is a
        // real question on a long session and the answer was recorded and thrown away.
        "model" => {
            let id = a.pointer("/identity/marketingName").and_then(Value::as_str);
            let raw = a.pointer("/identity/modelId").and_then(Value::as_str);
            match (id, raw) {
                (Some(name), Some(raw)) if name != raw => Some(format!("model: {name} ({raw})")),
                (Some(name), _) => Some(format!("model: {name}")),
                (None, Some(raw)) => Some(format!("model: {raw}")),
                _ => None,
            }
        }
        // The file read on screen was cut short (#238, 56 records). Without this a reader reasons
        // about a file from a truncated view and never knows.
        "read_truncation_notice" => {
            let banner = s("banner");
            Some(if banner.is_empty() {
                "the read was truncated".to_string()
            } else {
                banner.lines().next().unwrap_or(banner).to_string()
            })
        }
        _ => hook_note(a),
    }
}

fn hook_note(a: &Value) -> Option<String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").trim();
    let name = |n: &str| {
        if n.is_empty() {
            "a hook".to_string()
        } else {
            n.to_string()
        }
    };
    match a.get("type").and_then(Value::as_str)? {
        "hook_non_blocking_error" => {
            let mut out = format!("{} failed", name(s("hookName")));
            if let Some(code) = a.get("exitCode").and_then(Value::as_i64) {
                out.push_str(&format!(" (exit {code})"));
            }
            let detail = if s("stderr").is_empty() {
                s("stdout")
            } else {
                s("stderr")
            };
            if !detail.is_empty() {
                out.push_str(": ");
                out.push_str(detail.lines().next().unwrap_or(detail));
            }
            Some(out)
        }
        "hook_cancelled" if a.get("timedOut").and_then(Value::as_bool).unwrap_or(false) => {
            let mut out = format!("{} timed out", name(s("hookName")));
            match (
                a.get("durationMs").and_then(Value::as_i64),
                a.get("timeoutMs").and_then(Value::as_i64),
            ) {
                (Some(ran), Some(cap)) => out.push_str(&format!(" after {ran}ms (limit {cap}ms)")),
                (Some(ran), None) => out.push_str(&format!(" after {ran}ms")),
                _ => {}
            }
            Some(out)
        }
        _ => None,
    }
}

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
            // The size the transcript recorded (#261). `numLines` is what was put into the
            // context — a Read of a slice records fewer than the file has — so it is the honest
            // one to show, and `totalLines` only stands in when the slice is not stated.
            let lines = f
                .get("numLines")
                .or_else(|| f.get("totalLines"))
                .and_then(|n| n.as_u64())
                .and_then(|n| u32::try_from(n).ok());
            Some(Attachment {
                lines,
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
                lines: None,
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
                lines: None,
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
                lines: None,
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
        lines: None,
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
        lines: None,
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

    /// #241 — a NEWER workflow journal names each member. 9 of the 68 runs measured on this
    /// machine do; the label is the run's own word for the agent and beats anything derivable.
    #[test]
    fn a_journal_that_names_its_members_titles_them_by_label_and_keeps_the_phase() {
        let dir = std::env::temp_dir().join("cr-journal-labelled");
        std::fs::create_dir_all(&dir).unwrap();
        let j = dir.join("journal.jsonl");
        std::fs::write(
            &j,
            concat!(
                r#"{"type":"launched"}"#,
                "\n",
                r#"{"type":"started","agentId":"a1","label":"find:owned-path-plain","phase":"Find"}"#,
                "\n",
                r#"{"type":"started","agentId":"a2","label":"verify:residency","phase":"Verify"}"#,
                "\n",
                r#"{"type":"result","agentId":"a1","result":"A totally different sentence."}"#,
                "\n",
            ),
        )
        .unwrap();
        let m = roster_from_journal(&j);
        assert_eq!(m.len(), 2);
        assert_eq!(
            (m[0].description.as_str(), m[0].phase.as_deref()),
            ("find:owned-path-plain", Some("Find")),
            "the label titles the member and the phase rides with it"
        );
        assert_eq!(
            m[1].phase.as_deref(),
            Some("Verify"),
            "a second phase is carried too"
        );
        assert_eq!(
            m[0].description, "find:owned-path-plain",
            "a RESULT never overwrites the run's own name — the result title exists to name a \
             member that has none"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// …and the 59 runs whose journal names nothing keep exactly the behaviour they had: titled
    /// from the first line of the result, and by launch position until one arrives.
    #[test]
    fn a_journal_without_labels_is_unchanged() {
        let dir = std::env::temp_dir().join("cr-journal-bare");
        std::fs::create_dir_all(&dir).unwrap();
        let j = dir.join("journal.jsonl");
        std::fs::write(
            &j,
            concat!(
                r#"{"type":"started","agentId":"a1"}"#,
                "\n",
                r#"{"type":"started","agentId":"a2"}"#,
                "\n",
                r#"{"type":"result","agentId":"a1","result":"Found the leak in the residency map."}"#,
                "\n",
            ),
        )
        .unwrap();
        let m = roster_from_journal(&j);
        assert_eq!(m.len(), 2);
        assert!(m.iter().all(|x| x.phase.is_none()), "no phase is invented");
        assert_eq!(
            m[0].description, "Found the leak in the residency map.",
            "a member with no label is still titled from its result"
        );
        assert_eq!(
            m[1].description, "agent 2",
            "and by its launch position until one arrives"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// #242: `popAll` EMPTIES the queue — it is what an EDIT of a queued message looks like
    /// (the client drops everything pending, then re-enqueues the corrected text). Its
    /// `content` names only the item the reader was acting on, NOT the set it removed, so
    /// folding it as a `Remove` (which takes one item BY CONTENT) would drop the named marker
    /// and strand every other one on the page as though it were still pending.
    ///
    /// The fixture deliberately has TWO items outstanding when the `popAll` fires. With one
    /// item, the correct rule and the `Remove` guess produce identical output — which is
    /// exactly how this would have shipped wrong.
    #[test]
    fn pop_all_empties_the_queue_rather_than_removing_the_item_it_names() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":"real turn"}}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:01.000Z","content":"first thought"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:02.000Z","content":"second thought"}
{"type":"queue-operation","operation":"popAll","timestamp":"2026-06-30T03:00:03.000Z","content":"second thought"}
{"type":"queue-operation","operation":"enqueue","timestamp":"2026-06-30T03:00:04.000Z","content":"second thought, revised"}
"##;
        let blocks = parse(jsonl);
        let markers: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::QueueEvent { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        // Both pre-`popAll` markers are gone — including "first thought", which the record
        // never named. A `Remove` fold would have left it here.
        assert_eq!(markers, vec!["second thought, revised"], "{blocks:?}");
        // And the queue is genuinely EMPTY afterwards, not merely one item shorter: a later
        // content-less `dequeue` has nothing to pop, so the surviving marker stays put.
        let after = parse(&format!(
            "{jsonl}{}\n",
            r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-06-30T03:00:05.000Z"}"#
        ));
        let still: Vec<&str> = after
            .iter()
            .filter_map(|b| match b {
                Block::QueueEvent { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            still.is_empty(),
            "the re-enqueued item is the only thing left to pop: {after:?}"
        );
    }

    /// #255: an `AskUserQuestion` call carries every question it put and every option it
    /// offered — header, multi-select flag, and each option's label AND description. The block
    /// used to keep only `target`, which is the FIRST question's text, so a two-question call
    /// offering three options each surfaced 2 of the 8 things the asker wrote.
    #[test]
    fn an_ask_carries_every_question_and_every_option() {
        let jsonl = r##"
{"type":"assistant","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"tool_use","id":"a1","name":"AskUserQuestion","input":{"questions":[{"header":"Release","question":"Cut it now, or hold?","multiSelect":false,"options":[{"label":"Cut now","description":"Tag and push both remotes."},{"label":"Hold","description":"Leave it on main."}]},{"header":"Scope","question":"Which crates?","multiSelect":true,"options":[{"label":"engine"},{"label":"html","description":"the page too"}]}]}}]}}
"##;
        let blocks = parse(jsonl);
        let Some(Block::ToolUse { asked, target, .. }) = blocks
            .iter()
            .find(|b| matches!(b, Block::ToolUse { name, .. } if name == "AskUserQuestion"))
        else {
            panic!("the call is there: {blocks:?}")
        };
        assert_eq!(
            target, "Cut it now, or hold? +1",
            "the head is unchanged: the first question, and `+1` for the one it does not show \
             — which is how a reader knew there was more, and all they knew"
        );
        let a = asked
            .as_deref()
            .expect("…and the rest is no longer thrown away");
        assert_eq!(a.questions.len(), 2);
        assert_eq!(a.questions[0].header, "Release");
        assert!(!a.questions[0].multi_select);
        assert_eq!(
            a.questions[0]
                .options
                .iter()
                .map(|o| (o.label.as_str(), o.description.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Cut now", "Tag and push both remotes."),
                ("Hold", "Leave it on main."),
            ],
            "every option, with the description where the trade-off is written"
        );
        assert!(a.questions[1].multi_select, "the flag survives");
        assert_eq!(
            a.questions[1].options[0].description, "",
            "an option with no description carries an empty one rather than vanishing"
        );
    }

    /// #280: the reader's answers are joined onto the questions from the result's STRUCTURED
    /// half, not its prose. Every shape the client records, as measured across the owner's
    /// sessions (262 answers): a pick; the reader's own words, typed instead of picked (54 of
    /// them) — here with a `"` and a `, ` in it, the two characters the prose cannot carry; a
    /// multi-select, comma-joined; a pick with notes; and notes alone, which the client records
    /// as the placeholder `(notes only)`. Keys are the question's whole text, entity-encoded the
    /// way the input's are, and a question nobody answered is left alone.
    #[test]
    fn an_answered_ask_carries_each_answer_and_its_notes() {
        let jsonl = r##"
{"type":"assistant","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"tool_use","id":"a1","name":"AskUserQuestion","input":{"questions":[{"header":"Exit","question":"Exit 2 &amp; warn?","multiSelect":false,"options":[{"label":"Yes"},{"label":"No"}]},{"header":"Switch","question":"Where does it live?","multiSelect":false,"options":[{"label":"Both"},{"label":"Park it"}]},{"header":"Crates","question":"Which crates?","multiSelect":true,"options":[{"label":"engine"},{"label":"html"},{"label":"tui"}]},{"header":"Name","question":"Which name?","multiSelect":false,"options":[{"label":"--manage"},{"label":"--own"}]},{"header":"Later","question":"Anything else?","multiSelect":false,"options":[{"label":"No"}]}]}}]}}
{"type":"user","timestamp":"2026-06-30T03:00:09.000Z","toolUseResult":{"questions":[],"answers":{"Exit 2 &amp; warn?":"Yes","Where does it live?":"Both - say \"manage\" up front, and ask per skill later. ","Which crates?":"engine, tui","Which name?":"(notes only)"},"annotations":{"Exit 2 &amp; warn?":{"notes":"check the installer"},"Which name?":{"notes":"follow sync","preview":"--manage"}}},"message":{"content":[{"type":"tool_result","tool_use_id":"a1","content":"The user answered: …"}]}}
"##;
        let blocks = parse(jsonl);
        let Some(Block::ToolUse { asked, .. }) = blocks
            .iter()
            .find(|b| matches!(b, Block::ToolUse { name, .. } if name == "AskUserQuestion"))
        else {
            panic!("the call is there: {blocks:?}")
        };
        let replies: Vec<(Option<&str>, &str)> = asked
            .as_deref()
            .expect("the call asked")
            .questions
            .iter()
            .map(|q| (q.answer.as_deref(), q.notes.as_str()))
            .collect();
        assert_eq!(
            replies,
            vec![
                (Some("Yes"), "check the installer"),
                (
                    Some("Both - say \"manage\" up front, and ask per skill later."),
                    ""
                ),
                (Some("engine, tui"), ""),
                (None, "follow sync"),
                (None, ""),
            ],
            "a pick with its notes; typed words verbatim, quotes and commas intact (trimmed, \
             not split); a multi-select as recorded; notes alone with the client's placeholder \
             dropped; and a question with no answer left unanswered"
        );
    }

    /// …and no other tool grows the payload, nor does a malformed ask: an empty card is worse
    /// than none.
    #[test]
    fn only_an_ask_carries_questions() {
        let jsonl = r##"
{"type":"assistant","timestamp":"2026-06-30T03:00:00.000Z","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls","questions":[{"question":"not an ask"}]}}]}}
{"type":"assistant","timestamp":"2026-06-30T03:00:01.000Z","message":{"content":[{"type":"tool_use","id":"a2","name":"AskUserQuestion","input":{"questions":[]}}]}}
"##;
        for b in parse(jsonl) {
            if let Block::ToolUse { name, asked, .. } = b {
                assert!(
                    asked.is_none(),
                    "{name} must carry no questions: a non-ask tool that happens to have a \
                     `questions` key is not an ask, and an ask with none is not one either"
                );
            }
        }
    }

    /// #242(a): a `queue-operation` carries its `reason` onto the message. This is DATA only —
    /// nothing renders it, and the field's doc comment says why: measured over 3,093 prose
    /// removes in real transcripts, 99.4% are followed by the delivery of that same text, so
    /// the page already answers "where did my queued message go?" by showing the message.
    #[test]
    fn a_queue_operation_carries_its_reason_and_pop_all_decodes() {
        let mut cwd = String::new();
        let mut msgs: Vec<Message> = Vec::new();
        decode_line(
            r#"{"type":"queue-operation","operation":"remove","content":"x","reason":"absorbed_mid_turn"}"#,
            &mut cwd,
            &mut msgs,
        );
        decode_line(
            r#"{"type":"queue-operation","operation":"popAll","content":"y"}"#,
            &mut cwd,
            &mut msgs,
        );
        let got: Vec<(QueueOpKind, Option<&str>)> = msgs
            .iter()
            .filter_map(|m| match m {
                Message::QueueOp { op, reason, .. } => Some((*op, reason.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (QueueOpKind::Remove, Some("absorbed_mid_turn")),
                // `popAll` is decoded rather than dropped, and says nothing about why.
                (QueueOpKind::PopAll, None),
            ],
            "{msgs:?}"
        );
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
    fn only_the_named_system_subtypes_surface() {
        // A subtype nobody has claimed stays dropped, and a boundary with no metadata still opens
        // nothing on its own. `local_command` is claimed (#235) and is covered separately.
        let jsonl = r##"
{"type":"system","subtype":"hook_result","timestamp":"2026-06-30T03:00:00.000Z","content":"hook ran"}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-06-30T03:00:01.000Z","content":"no metadata here"}
"##;
        assert!(parse(jsonl).is_empty(), "{:?}", parse(jsonl));
    }

    /// #236: an API failure is the CLIENT's, not the model's. The record is an ordinary assistant
    /// message flagged `isApiErrorMessage`, and the flag is read ahead of any phase or content
    /// inspection — the transcript's own statement of what a message IS beats a sniff of its text.
    #[test]
    fn an_api_error_is_not_attributed_to_the_assistant() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"go"}}
{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-09-18T01:00:01.000Z","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Please run /login \u00b7 API Error"}]}}
{"type":"assistant","timestamp":"2026-09-18T01:00:02.000Z","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Back to work."}]}}
"##;
        let blocks = parse(jsonl);
        let says = |needle: &str| -> Vec<&'static str> {
            blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolResult(t) if t.contains(needle) => Some("note"),
                    Block::AssistantText(t) if t.contains(needle) => Some("assistant"),
                    Block::AssistantMessage { text, .. } if text.contains(needle) => {
                        Some("assistant")
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            says("API Error"),
            vec!["note"],
            "the failure is a note about the run, and NOT one of the assistant's messages: {blocks:?}"
        );
        assert_eq!(
            says("Back to work"),
            vec!["assistant"],
            "ordinary prose beside it still reads as the assistant: {blocks:?}"
        );
    }

    /// #239: the recap the client writes for a reader who stepped away — verified to describe the
    /// TURN it follows, not the session, by reading one against its turn before building on it.
    #[test]
    fn a_turns_recap_surfaces_without_its_terminal_hint() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"go"}}
{"type":"system","subtype":"away_summary","timestamp":"2026-09-18T01:00:01.000Z","content":"Drained the queue and cut v1.7.1. Next: your call on #17. (disable recaps in /config)"}
"##;
        let notes: Vec<String> = parse(jsonl)
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            vec!["Drained the queue and cut v1.7.1. Next: your call on #17.".to_string()],
            "the recap surfaces, and the terminal's own \"(disable recaps in /config)\" hint — on \
             305 of the 1,728 in this store — is not part of it"
        );
    }

    /// #238: the run's own context — why a turn began, which model wrote it, and what was cut
    /// short. Small records, each answering a question the transcript could not answer before.
    #[test]
    fn the_runs_context_surfaces() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"go"}}
{"type":"system","subtype":"scheduled_task_fire","timestamp":"2026-09-18T01:00:01.000Z","content":"loop fired: check the deploy"}
{"type":"system","subtype":"informational","level":"warning","timestamp":"2026-09-18T01:00:02.000Z","content":"Remote Control disconnected"}
{"type":"system","subtype":"informational","level":"notice","timestamp":"2026-09-18T01:00:03.000Z","content":"a quieter notice"}
{"type":"attachment","timestamp":"2026-09-18T01:00:04.000Z","attachment":{"type":"model","identity":{"modelId":"claude-opus-5[1m]","marketingName":"Opus 5 (1M context)"},"text":"switched"}}
{"type":"attachment","timestamp":"2026-09-18T01:00:05.000Z","attachment":{"type":"read_truncation_notice","banner":"Showing the first 100 lines of 4000\nuse offset to read more","toolUseID":"t1"}}
"##;
        let notes: Vec<String> = parse(jsonl)
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            vec![
                "loop fired: check the deploy".to_string(),
                "Remote Control disconnected".to_string(),
                "model: Opus 5 (1M context) (claude-opus-5[1m])".to_string(),
                "Showing the first 100 lines of 4000".to_string(),
            ],
            "a `notice` stays dropped, and a truncation banner is summarised by its first line"
        );
    }

    /// #237: an advisor consult is a tool call, and its outcome is worth having even though its
    /// advice is not available.
    ///
    /// The format REDACTS the advice — `advisor_redacted_result` wrapping a 4.7 KB
    /// `encrypted_content` blob, 596 of the 605 results in this store — so there is nothing to
    /// render and saying so plainly is the honest result. The 9 that FAILED are the ones that were
    /// worth surfacing all along, and they were invisible: the turn simply stopped and resumed.
    #[test]
    fn an_advisor_consult_is_a_call_with_an_outcome() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"go"}}
{"type":"assistant","timestamp":"2026-09-18T01:00:01.000Z","message":{"role":"assistant","content":[{"type":"server_tool_use","id":"srv_1","name":"advisor","input":{}},{"type":"advisor_tool_result","tool_use_id":"srv_1","content":{"type":"advisor_redacted_result","encrypted_content":"AAAA"}}]}}
{"type":"assistant","timestamp":"2026-09-18T01:00:02.000Z","message":{"role":"assistant","content":[{"type":"server_tool_use","id":"srv_2","name":"advisor","input":{}},{"type":"advisor_tool_result","tool_use_id":"srv_2","content":{"type":"advisor_tool_result_error","error_code":"overloaded"}}]}}
"##;
        let calls: Vec<(String, String)> = parse(jsonl)
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { name, output, .. } => {
                    Some((name.clone(), output.clone().unwrap_or_default()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2, "both consults are calls: {calls:?}");
        assert_eq!(calls[0].0, "advisor");
        assert!(
            calls[0].1.contains("redacts"),
            "a successful consult says the advice is redacted rather than pretending to show it: {calls:?}"
        );
        assert_eq!(
            calls[1].1, "the consult failed: overloaded",
            "a failed consult names why: {calls:?}"
        );
    }

    /// #236: the failures the transcript records and the viewer used to drop — a hook that failed,
    /// a hook the client killed for running long, and an API call that failed and retried.
    ///
    /// `hook_cancelled` is written TWICE for one timeout (the pre-hook cut off, the post-hook
    /// cancelled with it), so the case carries both and asserts ONE note: at ~8,000 affected tool
    /// calls in this store, one row per record would be twice the noise for the same event.
    #[test]
    fn hook_failures_and_api_errors_surface_once_each() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"go"}}
{"type":"attachment","timestamp":"2026-09-18T01:00:01.000Z","attachment":{"type":"hook_non_blocking_error","hookName":"guard","hookEvent":"PreToolUse","command":"./guard.sh","exitCode":1,"stderr":"refused: dirty tree\nsecond line","durationMs":12}}
{"type":"attachment","timestamp":"2026-09-18T01:00:02.000Z","attachment":{"type":"hook_cancelled","hookName":"slow","hookEvent":"PreToolUse","toolUseID":"call_1","timedOut":true,"durationMs":15023,"timeoutMs":15000}}
{"type":"attachment","timestamp":"2026-09-18T01:00:03.000Z","attachment":{"type":"hook_cancelled","hookName":"slow","hookEvent":"PostToolUse","toolUseID":"call_1","timedOut":false,"durationMs":3}}
{"type":"system","subtype":"api_error","timestamp":"2026-09-18T01:00:04.000Z","retryAttempt":2,"maxRetries":10,"error":{"message":"overloaded","status":529,"isNetworkDown":false}}
"##;
        let notes: Vec<String> = parse(jsonl)
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes.len(),
            3,
            "one note per EVENT — the second `hook_cancelled` is the same timeout, not a new one: {notes:?}"
        );
        assert!(
            notes[0].starts_with("guard failed (exit 1): refused: dirty tree"),
            "{notes:?}"
        );
        assert!(
            !notes[0].contains("second line"),
            "the first line of stderr is the summary, not the whole stream: {notes:?}"
        );
        assert_eq!(
            notes[1], "slow timed out after 15023ms (limit 15000ms)",
            "{notes:?}"
        );
        assert_eq!(
            notes[2], "API error: overloaded (HTTP 529) — retry 2 of 10",
            "{notes:?}"
        );
    }

    /// #235: a slash command the client wrote on a `system/local_command` record surfaces exactly
    /// as it does from a `user` record — and, because `parse_main` is pinned bit-identical to the
    /// streaming path, this fixture is what lets that pin SEE the two disagree. Without a fixture
    /// carrying this shape the equivalence gate was blind to the arm being added to one and not
    /// the other, which is how it was first added to only one.
    #[test]
    fn a_slash_command_on_a_system_record_surfaces() {
        let jsonl = r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"look"}}
{"type":"system","subtype":"local_command","timestamp":"2026-09-18T01:00:01.000Z","content":"<command-name>/context</command-name>\n<command-message>context</command-message>\n<command-args></command-args>"}
{"type":"system","subtype":"local_command","timestamp":"2026-09-18T01:00:02.000Z","content":"<local-command-stdout>99.2k/1m tokens (10%)</local-command-stdout>"}
"##;
        let blocks = parse(jsonl);
        let command = blocks
            .iter()
            .find_map(|b| match b {
                Block::Command { name, output, .. } => Some((name.clone(), output.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a /context command, got {blocks:?}"));
        assert_eq!(command.0, "/context");
        assert_eq!(
            command.1,
            vec!["99.2k/1m tokens (10%)".to_string()],
            "its stdout attaches to it, as a standalone stdout does after a user-record command"
        );
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

    /// #281: a question that came back WITHOUT an answer says why, in each shape the client
    /// records it (measured over the owner's 223 questions: 198 answered, 20 declined, 5 timed
    /// out): the timeout is structured (`afkTimeoutMs`, `answers` empty); a decline is an error
    /// result carrying the client's refusal, whose `toolUseResult` is either "User rejected tool
    /// use" or the refusal sentence itself. An error that is NOT a refusal is a failure — never a
    /// decline the reader did not make — and an answered question has no reason at all.
    #[test]
    fn an_unanswered_ask_says_why() {
        let ask = |id: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"{id}","name":"AskUserQuestion","input":{{"questions":[{{"header":"Q","question":"Cut it now?","multiSelect":false,"options":[{{"label":"Yes"}},{{"label":"No"}}]}}]}}}}]}}}}"#
            )
        };
        let refusal = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.";
        let jsonl = [
            ask("t1"),
            r#"{"type":"user","toolUseResult":{"questions":[],"answers":{},"annotations":{},"afkTimeoutMs":60000},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"No response after 60s — the user may be away from keyboard. Proceed using your best judgment based on the context so far; you can re-ask this question later if it's still relevant."}]}}"#.to_string(),
            ask("t2"),
            format!(r#"{{"type":"user","toolUseResult":"User rejected tool use","message":{{"content":[{{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":"{refusal}"}}]}}}}"#),
            ask("t3"),
            format!(r#"{{"type":"user","toolUseResult":"Error: {refusal}","message":{{"content":[{{"type":"tool_result","tool_use_id":"t3","is_error":true,"content":"{refusal}"}}]}}}}"#),
            ask("t4"),
            r#"{"type":"user","toolUseResult":"Error: InputValidationError: questions[0].options must have at least 2 items","message":{"content":[{"type":"tool_result","tool_use_id":"t4","is_error":true,"content":"<tool_use_error>InputValidationError: questions[0].options must have at least 2 items</tool_use_error>"}]}}"#.to_string(),
            ask("t5"),
            r#"{"type":"user","toolUseResult":{"questions":[],"answers":{"Cut it now?":"Yes"},"annotations":{}},"message":{"content":[{"type":"tool_result","tool_use_id":"t5","content":"The user answered: \"Cut it now?\"=\"Yes\"."}]}}"#.to_string(),
            ask("t6"),
        ]
        .join("\n");
        fn asks(blocks: &[Block], out: &mut Vec<Option<Unanswered>>) {
            for b in blocks {
                match b {
                    Block::ToolUse { name, asked, .. } if name == "AskUserQuestion" => {
                        out.push(asked.as_deref().and_then(|a| a.unanswered))
                    }
                    Block::Thinking { tools, .. } => asks(tools, out),
                    _ => {}
                }
            }
        }
        let mut seen = Vec::new();
        asks(&parse(&jsonl), &mut seen);
        assert_eq!(
            seen,
            vec![
                Some(Unanswered::TimedOut { after_ms: 60_000 }),
                Some(Unanswered::Declined),
                Some(Unanswered::Declined),
                Some(Unanswered::Failed),
                None,
                None,
            ],
            "timed out; declined, both ways the client records it; a failure that is not a \
             refusal; answered; and still waiting (no result yet)"
        );
    }

    /// #264 — a `toolUseResult` key the adapter neither reads nor has met before is REPORTED,
    /// and the ones it has met are not. This is the instrument that would have caught #263 on
    /// the day it appeared instead of a week later from a screenshot.
    ///
    /// The snapshot is process-global and the suite runs in parallel, so this asserts about
    /// the names it introduced rather than about the whole table.
    #[test]
    fn an_unrecognised_tool_result_key_is_reported_and_a_known_one_is_not() {
        let jsonl = r##"
{"type":"assistant","version":"2.1.400","sessionId":"s-264","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"true"}}]}}
{"type":"user","version":"2.1.400","sessionId":"s-264","toolUseResult":{"stdout":"ok","isImage":false,"aFieldFromTheFuture":{"x":1}},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}
{"type":"assistant","version":"2.1.400","sessionId":"s-264","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"true"}}]}}
{"type":"user","version":"2.1.400","sessionId":"s-264","toolUseResult":{"stdout":"ok","aFieldFromTheFuture":{"x":2}},"message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}
"##;
        let _ = parse(jsonl);
        let seen = unknown_shapes();
        let mine: Vec<_> = seen
            .iter()
            .filter(|s| s.name == "aFieldFromTheFuture")
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "one shape, however many times it occurs: {seen:?}"
        );
        assert_eq!(mine[0].count, 2, "…counted every time: {mine:?}");
        assert_eq!(
            (mine[0].version.as_deref(), mine[0].example.as_deref()),
            (Some("2.1.400"), Some("s-264")),
            "…carrying the client that wrote it and a session to open: {mine:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|s| s.name == "isImage" || s.name == "stdout"),
            "a key the adapter READS or has already met says nothing — 125 keys appear in the \
             corpus and 117 are deliberately unread, so reporting those is the noise that \
             makes a log unreadable: {seen:?}"
        );
    }

    /// #277 — seven shapes the daily review judged bookkeeping are known now, and the two with
    /// GENERIC names are known only in the shape they were judged in. `id` on the ignored list
    /// would have silenced it from every tool added after this; scoped to the Cron result, a new
    /// tool that returns an `id` of its own is still reported, as any new key is. The shapes
    /// are the measured ones: CronCreate `{id, humanSchedule, recurring, durable}`, CronDelete
    /// `{id}`, Read's `file_unchanged` `{type, file, source}`, SendMessage `{success, message,
    /// display, msg_id}`.
    #[test]
    fn a_generic_result_key_is_known_only_in_the_shape_it_was_judged_in() {
        let unknown = |v: &Value| -> Vec<String> {
            unknown_tool_result_keys(v)
                .into_iter()
                .map(str::to_string)
                .collect()
        };
        for known in [
            serde_json::json!({"id": "12b4386c", "humanSchedule": "Every hour at :17", "recurring": true, "durable": false}),
            serde_json::json!({"id": "12b4386c"}),
            serde_json::json!({"type": "file_unchanged", "file": {"filePath": "/w/CLAUDE.md"}, "source": "seeded"}),
            serde_json::json!({"success": true, "message": "sent", "display": "sent", "msg_id": "6a90484e"}),
        ] {
            assert_eq!(
                unknown(&known),
                Vec::<String>::new(),
                "judged and known: {known}"
            );
        }
        assert_eq!(
            unknown(&serde_json::json!({"id": "x", "aKeyFromTheFuture": 1})),
            vec!["aKeyFromTheFuture".to_string(), "id".to_string()],
            "the same `id` in any other shape is reported beside the key that is new"
        );
        assert_eq!(
            unknown(&serde_json::json!({"source": "seeded", "stdout": "ok"})),
            vec!["source".to_string()],
            "…and so is `source` outside Read's file_unchanged"
        );

        // The attachment is `{type, organizationUuid}` and nothing else.
        let jsonl = r##"
{"type":"attachment","version":"2.1.281","sessionId":"s-277","attachment":{"type":"credential_org","organizationUuid":"00000000-0000-0000-0000-000000000000"}}
"##;
        let _ = parse(jsonl);
        assert!(
            !unknown_shapes().iter().any(|s| s.name == "credential_org"),
            "an organisation id is account bookkeeping, known"
        );
    }

    /// #265 — the run changed model mid-session, and the reader is told. Found by #264's log
    /// on its first run against the largest sessions: five of these had arrived unnoticed
    /// since client 2.1.220, and the session card names ONE model, so after a fallback that
    /// answer is wrong for every turn after it.
    #[test]
    fn a_model_fallback_names_both_models() {
        let jsonl = r##"
{"type":"assistant","version":"2.1.226","message":{"content":[{"type":"text","text":"Starting."}]}}
{"type":"assistant","version":"2.1.226","message":{"content":[{"type":"fallback","from":{"model":"claude-fable-5"},"to":{"model":"claude-opus-4-8"}}]}}
{"type":"assistant","version":"2.1.226","message":{"content":[{"type":"fallback","to":{"model":"claude-opus-5"}}]}}
"##;
        let notes: Vec<String> = parse(jsonl)
            .into_iter()
            .filter_map(|b| match b {
                Block::ToolResult(t) if t.starts_with("Model fallback") => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(
            notes,
            vec![
                "Model fallback: claude-fable-5 → claude-opus-4-8".to_string(),
                // A half-recorded one still says what it can rather than vanishing.
                "Model fallback: unknown → claude-opus-5".to_string(),
            ],
            "both models, in the order the run moved between them"
        );
    }

    /// #264, the acceptance the owner asked for: a made-up record type, a made-up system
    /// subtype, a made-up attachment type, a made-up content type and a made-up
    /// `toolUseResult` key produce exactly five reports — and the KNOWN vocabulary beside them
    /// produces none. A log that cannot tell "new" from "deliberately ignored" is the noise
    /// that makes it unreadable, and this is the test that keeps that true.
    #[test]
    fn every_category_reports_what_is_new_and_nothing_that_is_known() {
        let jsonl = r##"
{"type":"cost-state","version":"2.1.400","sessionId":"s-cat","cost":1}
{"type":"a-row-from-the-future","version":"2.1.400","sessionId":"s-cat"}
{"type":"system","subtype":"turn_duration","version":"2.1.400","sessionId":"s-cat","content":"7s"}
{"type":"system","subtype":"a_subtype_from_the_future","version":"2.1.400","sessionId":"s-cat","content":"x"}
{"type":"attachment","version":"2.1.400","sessionId":"s-cat","attachment":{"type":"total_tokens_reminder","text":"<total_tokens>1</total_tokens>"}}
{"type":"attachment","version":"2.1.400","sessionId":"s-cat","attachment":{"type":"an_attachment_from_the_future","note":"x"}}
{"type":"assistant","version":"2.1.400","sessionId":"s-cat","message":{"content":[{"type":"text","text":"hi"},{"type":"a_block_from_the_future","x":1}]}}
{"type":"assistant","version":"2.1.400","sessionId":"s-cat","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"true"}}]}}
{"type":"user","version":"2.1.400","sessionId":"s-cat","toolUseResult":{"stdout":"ok","isImage":false,"a_key_from_the_future":1},"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}
"##;
        let _ = parse(jsonl);
        let mut mine: Vec<(String, String)> = unknown_shapes()
            .into_iter()
            .filter(|s| s.name.contains("from_the_future") || s.name.contains("from-the-future"))
            .map(|s| (s.at.as_str().to_string(), s.name))
            .collect();
        mine.sort();
        assert_eq!(
            mine,
            vec![
                (
                    "attachment.type".to_string(),
                    "an_attachment_from_the_future".to_string()
                ),
                (
                    "content.type".to_string(),
                    "a_block_from_the_future".to_string()
                ),
                (
                    "record.type".to_string(),
                    "a-row-from-the-future".to_string()
                ),
                (
                    "system.subtype".to_string(),
                    "a_subtype_from_the_future".to_string()
                ),
                (
                    "toolUseResult.key".to_string(),
                    "a_key_from_the_future".to_string()
                ),
            ],
            "one report per category, and the known `cost-state`, `turn_duration`, \
             `total_tokens_reminder`, `text` and `isImage` beside them say nothing"
        );
    }

    /// #263 — a Bash command that edited files renders its diff, with every file the record
    /// names accounted for. The shape is the measured one: `files[]` capped at five with
    /// `moreFiles` counting the rest, several hunks per file, and files that changed but carry
    /// no hunks at all.
    #[test]
    fn a_bash_edit_diff_becomes_hunks_that_name_their_files() {
        let jsonl = r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"./edit.sh"}}]}}
{"type":"user","toolUseResult":{"stdout":"done","bashEditDiff":{"files":[{"filePath":"/w/a.md","hunks":[{"oldStart":9,"oldLines":2,"newStart":9,"newLines":3,"lines":[" ctx","-gone","+new","+also"]},{"oldStart":40,"newStart":41,"lines":[" tail","-x"]}]},{"filePath":"/w/bin.tar.gz","hunks":[]}],"moreFiles":1,"changedFiles":["/w/a.md","/w/bin.tar.gz","/w/past-the-cap.txt"]}},"message":{"content":[{"type":"tool_result","tool_use_id":"b1","content":"done"}]}}
"##;
        // The call coalesces into an activity span, so the ToolUse is nested in `tools`.
        fn find_patch(blocks: &[Block]) -> Option<Vec<Hunk>> {
            blocks.iter().find_map(|b| match b {
                Block::ToolUse { patch, .. } => patch.clone(),
                Block::Thinking { tools, .. } => find_patch(tools),
                _ => None,
            })
        }
        let patch = find_patch(&parse(jsonl)).expect("the Bash call carries the diff as a patch");
        let seen: Vec<(Option<String>, usize, usize)> = patch
            .iter()
            .map(|h| (h.file.clone(), h.old_start, h.lines.len()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (Some("/w/a.md".into()), 9, 4),
                (Some("/w/a.md".into()), 40, 2),
                // Changed, not diffable: the name is the whole of what is known.
                (Some("/w/bin.tar.gz".into()), 0, 0),
                // Past `files[]`'s five-file cap, recovered from `changedFiles` so that
                // `moreFiles` is a fact the reader can see rather than a number.
                (Some("/w/past-the-cap.txt".into()), 0, 0),
            ],
            "every file the record names becomes at least one hunk, and each hunk says which \
             file it belongs to — a Bash call's header names the COMMAND, so nothing else does"
        );
    }

    /// An Edit is unchanged by #263: one file, named by the call's own target, so its hunks
    /// carry no file and render exactly as they did.
    #[test]
    fn an_edit_patch_still_names_no_file() {
        let jsonl = r##"
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"e1","name":"Edit","input":{"file_path":"/w/x.rs"}}]}}
{"type":"user","toolUseResult":{"filePath":"/w/x.rs","structuredPatch":[{"oldStart":10,"newStart":12,"lines":[" c","-a","+b"]}]},"message":{"content":[{"type":"tool_result","tool_use_id":"e1","content":"The file /w/x.rs has been updated successfully."}]}}
"##;
        fn find_patch(blocks: &[Block]) -> Option<Vec<Hunk>> {
            blocks.iter().find_map(|b| match b {
                Block::ToolUse { patch, .. } => patch.clone(),
                Block::Thinking { tools, .. } => find_patch(tools),
                _ => None,
            })
        }
        let patch = find_patch(&parse(jsonl)).expect("the Edit carries its structuredPatch");
        assert!(
            patch.iter().all(|h| h.file.is_none()),
            "an Edit's hunks name no file: {patch:?}"
        );
    }

    /// #261 — the size the transcript recorded reaches the model. A file a compaction puts back
    /// into context states `numLines`, which is the "(9 lines)" Claude Code's own TUI prints
    /// beside the path; we used to drop it, so no frontend could show a size and five restored
    /// files read as five naked paths. `totalLines` stands in only when the slice is unstated.
    #[test]
    fn a_file_attachment_carries_the_line_count_the_transcript_recorded() {
        let jsonl = r##"
{"type":"attachment","timestamp":"2026-06-30T03:00:00.000Z","attachment":{"type":"file","filename":"/w/a.md","displayPath":"a.md","content":{"type":"text","file":{"filePath":"/w/a.md","content":"one\ntwo","numLines":9,"startLine":1,"totalLines":40}}}}
{"type":"attachment","timestamp":"2026-06-30T03:00:01.000Z","attachment":{"type":"file","filename":"/w/b.md","displayPath":"b.md","content":{"type":"text","file":{"filePath":"/w/b.md","content":"one","totalLines":40}}}}
{"type":"attachment","timestamp":"2026-06-30T03:00:02.000Z","attachment":{"type":"file","filename":"/w/c.md","displayPath":"c.md","content":{"type":"text","file":{"filePath":"/w/c.md","content":"one"}}}}
"##;
        let seen: Vec<(String, Option<u32>)> = parse(jsonl)
            .iter()
            .filter_map(|b| match b {
                Block::Attachment(a) => Some((a.name.clone(), a.lines)),
                _ => None,
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                ("a.md".to_string(), Some(9)),
                ("b.md".to_string(), Some(40)),
                ("c.md".to_string(), None),
            ],
            "numLines wins over totalLines, totalLines stands in, and a file that states \
             neither carries no count rather than a made-up one"
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
        // Four, in this order: the command's draft; the HUMAN output line, which stands the
        // task up even when the record was piped away; the record doing the same from the
        // other side; and the resolve that lands the draft over both.
        assert_eq!(
            ops.len(),
            4,
            "draft + human line + record stub + resolve: {ops:#?}"
        );
        assert!(
            matches!(ops[0], TaskOp::Create { subject, .. } if subject == "Scaffold"),
            "{:#?}",
            ops[0]
        );
        for k in [1, 2] {
            assert!(
                matches!(ops[k], TaskOp::Update { task_id, subject: Some(s), .. }
                    if task_id == "q1" && s == "Scaffold"),
                "{:#?}",
                ops[k]
            );
        }
        assert!(
            matches!(ops[3], TaskOp::Resolve { id: Some(id), .. } if id == "q1"),
            "{:#?}",
            ops[3]
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
        // Span 1 carries across the attachment, un-split — and since #256 the attachment is
        // HELD until the span flushes, so it lands just after the run that produced it instead
        // of ahead of the whole thing. That is the only difference here; the span-transparency
        // rule this case is about is unchanged. The TaskUpdate still splits "after text" from
        // the lone trailing Bash, which still folds into a tools-only span.
        assert_eq!(
            kinds(&blocks),
            vec![
                "user",
                "thinking",
                "attachment",
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
        } = &blocks[1]
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
    fn assistant_prose_uses_stop_reason_and_same_message_tools_for_phase() {
        let blocks = parse(
            r#"
{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"text","text":"I will inspect it first."}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Now I will run it."},{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"true"}}]}}
{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Still streaming"}]}}
"#,
        );
        assert!(matches!(
            &blocks[0],
            Block::AssistantMessage { text, phase: AssistantPhase::Commentary, inferred: true }
                if text == "I will inspect it first."
        ));
        assert!(matches!(
            &blocks[1],
            Block::AssistantMessage { text, phase: AssistantPhase::Commentary, inferred: true }
                if text == "Now I will run it."
        ));
        assert!(matches!(
            &blocks[3],
            Block::AssistantMessage { text, phase: AssistantPhase::Final, inferred: true }
                if text == "Done."
        ));
        assert!(matches!(&blocks[4], Block::AssistantText(text) if text == "Still streaming"));
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
            // #235/#236: the record shapes the two paths must agree on, and could not be held to
            // before — a slash command on a `system` record, a hook failure, a hook timeout, an
            // API error, and an assistant message flagged as one. An arm added to one path and
            // not the other is invisible to this pin unless a fixture carries the shape, which is
            // exactly how it went unnoticed twice.
            r##"
{"type":"user","timestamp":"2026-09-18T01:00:00.000Z","message":{"role":"user","content":"go"}}
{"type":"system","subtype":"local_command","timestamp":"2026-09-18T01:00:01.000Z","content":"<command-name>/context</command-name>\n<command-args></command-args>"}
{"type":"system","subtype":"local_command","timestamp":"2026-09-18T01:00:02.000Z","content":"<local-command-stdout>99.2k/1m tokens (10%)</local-command-stdout>"}
{"type":"attachment","timestamp":"2026-09-18T01:00:03.000Z","attachment":{"type":"hook_non_blocking_error","hookName":"guard","hookEvent":"PreToolUse","exitCode":1,"stderr":"refused"}}
{"type":"attachment","timestamp":"2026-09-18T01:00:04.000Z","attachment":{"type":"hook_cancelled","hookName":"slow","toolUseID":"c1","timedOut":true,"durationMs":15023,"timeoutMs":15000}}
{"type":"system","subtype":"api_error","timestamp":"2026-09-18T01:00:05.000Z","retryAttempt":2,"maxRetries":10,"error":{"message":"overloaded","status":529}}
{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-09-18T01:00:06.000Z","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"API Error"}]}}
{"type":"assistant","timestamp":"2026-09-18T01:00:07.000Z","message":{"role":"assistant","content":[{"type":"server_tool_use","id":"srv_1","name":"advisor","input":{}},{"type":"advisor_tool_result","tool_use_id":"srv_1","content":{"type":"advisor_tool_result_error","error_code":"overloaded"}}]}}
{"type":"system","subtype":"scheduled_task_fire","timestamp":"2026-09-18T01:00:08.000Z","content":"loop fired: check the deploy"}
{"type":"system","subtype":"away_summary","timestamp":"2026-09-18T01:00:08.500Z","content":"Drained the queue and cut v1.7.1. Next: your call on #17. (disable recaps in /config)"}
{"type":"system","subtype":"informational","level":"warning","timestamp":"2026-09-18T01:00:09.000Z","content":"Remote Control disconnected"}
{"type":"system","subtype":"informational","level":"notice","timestamp":"2026-09-18T01:00:10.000Z","content":"a quieter notice that stays dropped"}
{"type":"attachment","timestamp":"2026-09-18T01:00:11.000Z","attachment":{"type":"model","identity":{"modelId":"claude-opus-5[1m]","marketingName":"Opus 5 (1M context)"},"text":"switched"}}
{"type":"attachment","timestamp":"2026-09-18T01:00:12.000Z","attachment":{"type":"read_truncation_notice","banner":"Showing the first 100 lines of 4000","toolUseID":"t1"}}
"##,
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
    fn tool_target_lifts_a_question_put_to_the_person() {
        let one = serde_json::json!({ "questions": [{ "question": " Which UI should be the default? ", "header": "Default", "options": [] }] });
        assert_eq!(tool_target(&one, "/w"), "Which UI should be the default?");
        let two = serde_json::json!({ "questions": [{ "question": "Ship now?" }, { "question": "Tag it?" }] });
        assert_eq!(tool_target(&two, "/w"), "Ship now? +1");
        let empty = serde_json::json!({ "questions": [{ "header": "x" }] });
        assert_eq!(
            tool_target(&empty, "/w"),
            "",
            "no question text, no invented target"
        );
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

    /// #207: `SendUserFile` names the file it delivered — the first, `+n` for the rest — and the
    /// path is relativized like every other path the viewer shows (the owner asked why the one
    /// place it appeared, the tool's own output prose, printed it absolute).
    #[test]
    fn tool_target_names_the_files_a_tool_delivered() {
        let home = std::env::var("HOME").unwrap_or_default();
        let one = serde_json::json!({ "files": ["/w/video/tour.mp4"], "caption": "the tour", "status": "normal" });
        assert_eq!(tool_target(&one, "/w"), "video/tour.mp4");
        let two = serde_json::json!({ "files": ["/w/a.html", "/w/b.html"] });
        assert_eq!(tool_target(&two, "/w"), "a.html +1");
        // Outside the cwd it falls back to the home tilde, which is the ~/ the owner asked for.
        if !home.is_empty() {
            let away = serde_json::json!({ "files": [format!("{home}/code/demo/tour.mp4")] });
            assert_eq!(tool_target(&away, "/w"), "~/code/demo/tour.mp4");
        }
        // An empty list is no target at all, not a crash.
        assert_eq!(tool_target(&serde_json::json!({ "files": [] }), "/w"), "");
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
