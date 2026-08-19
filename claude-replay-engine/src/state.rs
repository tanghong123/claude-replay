//! **Agent-state derivation** (#194): the pure half of busy / wait / idle.
//!
//! The contract (design/agent-states.md §1): **busy** — more progress is coming without
//! user attention; **wait** — blocked by a simple user action (a modal: permission, a
//! question dialog, plan approval); **idle** — the turn (or the process) is over and
//! whatever happens next starts with the human, including "ended with a question" and
//! "died mid-work".
//!
//! Everything here is DERIVED, never carried: [`derive_state`] is a pure function of one
//! tick's [`StateSignals`], so the failure mode of the hook-based predecessor — state
//! asserted once and stuck forever — cannot exist. The signals are gathered by the
//! consumer (claude-monitor: growth clocks, process attribution, the `ps` children
//! probe); the transcript-side facts come from [`tail_pulse`], which runs the ADAPTER'S
//! OWN decoder over a bounded tail — the parser the old `tail | jq` observer never had.

use crate::adapter::{PreprocessedLine, TranscriptAdapter};
use crate::engine::message::{Message, QueueOpKind};
use std::path::Path;

/// The three states of the contract. Serialized lowercase into [`StateEvent`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Busy,
    Wait,
    Idle,
}

/// Why the state is what it is — the context half of the contract, machine-readable.
/// `detail` on the verdict carries the human line (tool name, question text, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateReason {
    /// idle — no live agent process, nothing pending.
    Exited,
    /// idle — no live agent process, but tools were still in flight: needs attention.
    ExitedMidWork,
    /// wait — a pending interactive tool is asking (AskUserQuestion class).
    Question,
    /// wait — a pending plan approval (ExitPlanMode class).
    PlanApproval,
    /// busy — the user already queued a prompt; progress resumes without them.
    QueuedPrompt,
    /// busy — a tool is running (named in `detail`), or its result is being folded.
    Tool,
    /// busy — the model is thinking/streaming (no pending tool, output growing).
    Thinking,
    /// wait (INFERRED) — a pending non-interactive tool, no live tool child, quiet:
    /// the shape of a permission dialog, which writes nothing to the transcript.
    Permission,
    /// idle — the turn ended and the final text reads as a question to the user.
    EndedQuestion,
    /// idle — the turn ended right after a failed tool result.
    Error,
    /// idle — the turn ended cleanly.
    Done,
    /// busy — a user prompt is in, nothing observable yet (API call in flight).
    Starting,
    /// idle — rule 7/fallback aged out: a prompt or mid-turn state with no progress
    /// for `STALL_AFTER_SECS` and no running tool child. Needs attention to unblock.
    Stalled,
}

impl StateReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::ExitedMidWork => "exited-mid-work",
            Self::Question => "question",
            Self::PlanApproval => "plan-approval",
            Self::QueuedPrompt => "queued-prompt",
            Self::Tool => "tool",
            Self::Thinking => "thinking",
            Self::Permission => "permission",
            Self::EndedQuestion => "ended-question",
            Self::Error => "error",
            Self::Done => "done",
            Self::Starting => "starting",
            Self::Stalled => "stalled",
        }
    }
}

/// Observed fact vs inference — carried on the verdict so consumers can render the
/// rule-5 permission guess (design §6: the dialog writes nothing to the transcript,
/// and by owner decision no CPU-delta hardening backs it up) softer than knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Observed,
    Inferred,
}

/// One tick's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub state: AgentState,
    pub reason: StateReason,
    /// The human line: tool name + target, the question's first line, the final prose
    /// snippet. Empty when there is nothing to add.
    pub detail: String,
    pub confidence: Confidence,
}

impl Verdict {
    fn observed(state: AgentState, reason: StateReason, detail: impl Into<String>) -> Self {
        Self {
            state,
            reason,
            detail: detail.into(),
            confidence: Confidence::Observed,
        }
    }
}

/// A tool call with no result yet, as the liveness scan reports it (name best-effort)
/// and the consumer classifies it (`interactive` via `tool_is_interactive`, #21).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTool {
    pub id: String,
    pub name: String,
    pub interactive: bool,
}

/// What the bounded tail said about the conversation's last word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TailLast {
    /// The last conversational record is a REAL user turn (prompt or slash command).
    User,
    /// The last conversational record is assistant output and the turn ENDED
    /// (the adapter's `turn_ended` said so).
    AssistantEnded,
    /// Assistant output, turn not known to have ended (streaming / mid-turn).
    AssistantMid,
    /// Nothing conversational in the window.
    #[default]
    Unknown,
}

/// Everything [`derive_state`] looks at — one tick's observation, all plain data.
#[derive(Debug, Clone, Default)]
pub struct StateSignals {
    /// A live agent process is attributed to this session.
    pub process_alive: bool,
    /// Does that process have a RECENT child (a tool executing)? `None` = not probed.
    pub tool_children: Option<bool>,
    /// The transcript tree grew within the consumer's hold window (S1).
    pub grew_recently: bool,
    /// Seconds since the last observed content activity.
    pub quiet_secs: u64,
    /// Unresolved tool calls in the tail (S2), interactivity already classified.
    pub pending: Vec<PendingTool>,
    /// A queued user prompt is waiting in the tail (S4).
    pub queued_prompt: bool,
    /// The tail's last conversational word (S4).
    pub last: TailLast,
    /// The adapter's `ends_with_question` on the final assistant text.
    pub ends_with_question: bool,
    /// The final assistant text's first line (detail for idle verdicts).
    pub final_line: Option<String>,
    /// The last tool result in the tail reported failure (#23).
    pub last_tool_error: bool,
}

/// Rule 5's quiet threshold: a pending non-interactive tool with no live child reads
/// as a permission dialog only after this much silence — the pre-permission write
/// burst must settle first.
pub const PERMISSION_QUIET_SECS: u64 = 20;

/// Rules 7/fallback: how long a "starting"/mid-turn state may sit with no progress and
/// no running child before it reads as stalled (needs attention to unblock).
pub const STALL_AFTER_SECS: u64 = 600;

/// The §4 decision procedure — first match wins. Pure: same signals, same verdict.
pub fn derive_state(s: &StateSignals) -> Verdict {
    use AgentState::*;
    // 1. No live process: over, whatever the transcript hoped would happen next.
    if !s.process_alive {
        return if s.pending.is_empty() {
            Verdict::observed(Idle, StateReason::Exited, "")
        } else {
            Verdict::observed(
                Idle,
                StateReason::ExitedMidWork,
                format!("exited with {} pending", tool_list(&s.pending)),
            )
        };
    }
    // 2. A pending INTERACTIVE tool is a modal by definition (#21 vocabulary).
    if let Some(t) = s.pending.iter().find(|t| t.interactive) {
        let (reason, what) = if t.name == "ExitPlanMode" {
            (StateReason::PlanApproval, "plan ready for approval".into())
        } else {
            (
                StateReason::Question,
                s.final_line.clone().unwrap_or_else(|| t.name.clone()),
            )
        };
        return Verdict::observed(Wait, reason, what);
    }
    // 3. A queued prompt means the user already acted; progress resumes unattended.
    if s.queued_prompt {
        return Verdict::observed(Busy, StateReason::QueuedPrompt, "");
    }
    // 4. Growth is the plainest busy — context from what is open.
    if s.grew_recently {
        return if let Some(t) = s.pending.first() {
            Verdict::observed(Busy, StateReason::Tool, t.name.clone())
        } else {
            Verdict::observed(Busy, StateReason::Thinking, "")
        };
    }
    // 5. Quiet with pending non-interactive tools: a live child says a tool is genuinely
    //    running (#82 — a silent build is busy); no child + enough quiet is the shape of
    //    a permission dialog. Unknown child state stays busy — the safe direction.
    if let Some(t) = s.pending.first() {
        return match s.tool_children {
            Some(false) if s.quiet_secs >= PERMISSION_QUIET_SECS => Verdict {
                state: Wait,
                reason: StateReason::Permission,
                detail: t.name.clone(),
                confidence: Confidence::Inferred,
            },
            _ => Verdict::observed(Busy, StateReason::Tool, t.name.clone()),
        };
    }
    // 6. Nothing open, turn ended: idle — the context is what the ending SAID.
    if s.last == TailLast::AssistantEnded {
        let detail = s.final_line.clone().unwrap_or_default();
        return if s.ends_with_question {
            Verdict::observed(Idle, StateReason::EndedQuestion, detail)
        } else if s.last_tool_error {
            Verdict::observed(Idle, StateReason::Error, detail)
        } else {
            Verdict::observed(Idle, StateReason::Done, detail)
        };
    }
    // 7 + fallback. A prompt in with nothing observable yet, or assistant output that
    //    never concluded: busy — until it has sat unmoving long enough with no child,
    //    which is a stall the human has to unblock.
    let stalled = s.quiet_secs >= STALL_AFTER_SECS && s.tool_children != Some(true);
    if stalled {
        return Verdict::observed(Idle, StateReason::Stalled, "");
    }
    match s.last {
        TailLast::User => Verdict::observed(Busy, StateReason::Starting, ""),
        _ => Verdict::observed(Busy, StateReason::Thinking, ""),
    }
}

fn tool_list(pending: &[PendingTool]) -> String {
    let names: Vec<&str> = pending.iter().map(|t| t.name.as_str()).take(3).collect();
    names.join(", ")
}

/// The generic final-text question heuristic — the DEFAULT body of the adapter's
/// `ends_with_question` hook (#194, owner-resolved: an adapter hook from day one, like
/// #21). It only refines idle's CONTEXT, never flips busy/wait, so a miss costs a
/// softer notification, not a wrong state.
pub fn generic_ends_with_question(final_text: &str) -> bool {
    let t = final_text.trim_end();
    if t.ends_with('?') || t.ends_with('？') {
        return true;
    }
    let last_para = t.rsplit("\n\n").next().unwrap_or(t).to_lowercase();
    [
        "let me know",
        "shall i",
        "want me to",
        "which of",
        "say the word",
        "should i",
    ]
    .iter()
    .any(|p| last_para.contains(p))
}

/// One state TRANSITION, as written to the consumer-facing `events.jsonl` (§5/§7 of the
/// design). The writer is the monitor; this type lives here so every consumer
/// deserializes against exactly what was serialized.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StateEvent {
    /// Schema version.
    pub v: u16,
    /// RFC3339 UTC of the transition.
    pub ts: String,
    pub sid: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub title: String,
    pub state: AgentState,
    /// The state this transition left.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<AgentState>,
    pub reason: StateReason,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub detail: String,
    pub confidence: Confidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The controllable terminal target (tmux pane / screen name), when one exists —
    /// named, never used (§4 of the liveness probe).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
}

/// What the bounded tail decode said (S4) — computed by [`tail_pulse`].
#[derive(Debug, Clone, Default)]
pub struct TailPulse {
    pub last: TailLast,
    /// The last assistant prose in the window (full text; consumers first-line it).
    pub final_text: Option<String>,
    pub queued_prompt: bool,
    pub last_tool_error: bool,
}

/// How many tail bytes [`tail_pulse`] decodes. Smaller than the liveness in-flight
/// window: this read is per changed session per tick, and the SEMANTIC facts it wants
/// live at the very end of the file.
pub const PULSE_TAIL_BYTES: u64 = 64 * 1024;

/// Decode the transcript's bounded tail through the adapter's OWN decoder and report
/// the conversation's last word (design §4 S4). Lossy on the window edge for the same
/// reason the liveness scan is (#82): a split character must not cost the whole signal.
pub fn tail_pulse(adapter: &dyn TranscriptAdapter, path: &Path) -> TailPulse {
    use std::io::{Read, Seek, SeekFrom};
    let mut pulse = TailPulse::default();
    let Ok(mut f) = std::fs::File::open(path) else {
        return pulse;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(PULSE_TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return pulse;
    }
    let mut raw = Vec::new();
    if f.read_to_end(&mut raw).is_err() {
        return pulse;
    }
    let buf = String::from_utf8_lossy(&raw);
    let mut lines: Vec<&str> = buf.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the window's first line is severed — never decode it
    }

    // The turn-ended fact comes from the RAW lines through the adapter's vocabulary
    // hook — the last line with an opinion wins.
    let mut ended: Option<bool> = None;
    for line in lines.iter().rev() {
        if let Some(e) = adapter.turn_ended(line) {
            ended = Some(e);
            break;
        }
    }

    // Everything else comes from the DECODED messages.
    let mut pre = adapter.line_preprocessor();
    let mut cwd = String::new();
    let mut msgs: Vec<Message> = Vec::new();
    for line in &lines {
        let body = line.trim_end();
        if body.is_empty() {
            continue;
        }
        match pre.process(body) {
            PreprocessedLine::Ignore => continue,
            PreprocessedLine::Messages(m) => msgs.extend(m),
            PreprocessedLine::Include => adapter.decode_line(body, &mut cwd, &mut msgs),
        }
    }
    let mut queue_len: i64 = 0;
    let mut last_user = false;
    let mut saw_conversation = false;
    for m in &msgs {
        match m {
            Message::UserText { .. } | Message::Command { .. } => {
                last_user = true;
                saw_conversation = true;
            }
            Message::AssistantText(t) => {
                last_user = false;
                saw_conversation = true;
                pulse.final_text = Some(t.clone());
            }
            Message::AssistantMessage { text, .. } => {
                last_user = false;
                saw_conversation = true;
                pulse.final_text = Some(text.clone());
            }
            Message::ToolResult { is_error, .. } => {
                pulse.last_tool_error = *is_error == Some(true);
            }
            Message::QueueOp { op, prose, .. } => match op {
                QueueOpKind::Enqueue if *prose => queue_len += 1,
                QueueOpKind::Dequeue | QueueOpKind::Remove => queue_len -= 1,
                _ => {}
            },
            _ => {}
        }
    }
    pulse.queued_prompt = queue_len > 0;
    pulse.last = if !saw_conversation {
        TailLast::Unknown
    } else if last_user {
        TailLast::User
    } else if ended == Some(true) {
        TailLast::AssistantEnded
    } else {
        TailLast::AssistantMid
    };
    pulse
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> StateSignals {
        StateSignals {
            process_alive: true,
            ..Default::default()
        }
    }
    fn pending(name: &str, interactive: bool) -> PendingTool {
        PendingTool {
            id: "t1".into(),
            name: name.into(),
            interactive,
        }
    }

    /// Rule 1: a dead process is idle whatever the transcript says — and pending work
    /// makes it attention-needing, not busy (the old system's "stuck in busy").
    #[test]
    fn a_dead_process_is_idle_never_busy() {
        let mut s = base();
        s.process_alive = false;
        s.grew_recently = true; // even with a fresh-looking file
        assert_eq!(derive_state(&s).reason, StateReason::Exited);
        s.pending = vec![pending("Bash", false)];
        let v = derive_state(&s);
        assert_eq!(
            (v.state, v.reason),
            (AgentState::Idle, StateReason::ExitedMidWork)
        );
    }

    /// Rule 2: an interactive tool is a modal wait, and it outranks growth (the
    /// question was WRITTEN, so the file just grew — still a wait).
    #[test]
    fn an_interactive_tool_is_a_wait_even_while_fresh() {
        let mut s = base();
        s.grew_recently = true;
        s.pending = vec![pending("AskUserQuestion", true)];
        s.final_line = Some("Which option?".into());
        let v = derive_state(&s);
        assert_eq!(
            (v.state, v.reason),
            (AgentState::Wait, StateReason::Question)
        );
        assert_eq!(v.detail, "Which option?");
        s.pending = vec![pending("ExitPlanMode", true)];
        assert_eq!(derive_state(&s).reason, StateReason::PlanApproval);
    }

    /// Rule 3: a queued prompt beats wait/idle below it — the user already acted.
    #[test]
    fn a_queued_prompt_is_busy() {
        let mut s = base();
        s.queued_prompt = true;
        s.last = TailLast::AssistantEnded;
        let v = derive_state(&s);
        assert_eq!(
            (v.state, v.reason),
            (AgentState::Busy, StateReason::QueuedPrompt)
        );
    }

    /// Rules 4/5: the #82 case — a silent long-running tool stays busy through its live
    /// child, at ANY quiet age; only no-child + quiet reads as a permission dialog, and
    /// that verdict is marked inferred.
    #[test]
    fn quiet_pending_tools_split_on_the_child_probe() {
        let mut s = base();
        s.pending = vec![pending("Bash", false)];
        s.quiet_secs = 3600;
        s.tool_children = Some(true);
        let v = derive_state(&s);
        assert_eq!((v.state, v.reason), (AgentState::Busy, StateReason::Tool));
        s.tool_children = Some(false);
        let v = derive_state(&s);
        assert_eq!(
            (v.state, v.reason),
            (AgentState::Wait, StateReason::Permission)
        );
        assert_eq!(v.confidence, Confidence::Inferred);
        // Below the quiet threshold the pre-permission write burst is still settling.
        s.quiet_secs = PERMISSION_QUIET_SECS - 1;
        assert_eq!(derive_state(&s).state, AgentState::Busy);
        // Unprobed children stay busy — the safe direction.
        s.quiet_secs = 3600;
        s.tool_children = None;
        assert_eq!(derive_state(&s).state, AgentState::Busy);
    }

    /// Rule 6: an ended turn is idle, with the context split three ways.
    #[test]
    fn an_ended_turn_is_idle_with_context() {
        let mut s = base();
        s.last = TailLast::AssistantEnded;
        s.final_line = Some("All done.".into());
        assert_eq!(derive_state(&s).reason, StateReason::Done);
        s.ends_with_question = true;
        assert_eq!(derive_state(&s).reason, StateReason::EndedQuestion);
        s.ends_with_question = false;
        s.last_tool_error = true;
        assert_eq!(derive_state(&s).reason, StateReason::Error);
    }

    /// Rule 7 + fallback: a fresh prompt is busy; the same state aged past the stall
    /// window with no child is idle·stalled — never busy forever.
    #[test]
    fn starting_ages_into_stalled() {
        let mut s = base();
        s.last = TailLast::User;
        assert_eq!(derive_state(&s).reason, StateReason::Starting);
        s.quiet_secs = STALL_AFTER_SECS;
        let v = derive_state(&s);
        assert_eq!(
            (v.state, v.reason),
            (AgentState::Idle, StateReason::Stalled)
        );
        // A live child holds it busy regardless of age.
        s.tool_children = Some(true);
        assert_eq!(derive_state(&s).state, AgentState::Busy);
    }

    /// The generic question heuristic: terminal `?` (ASCII and CJK) and closing offers.
    #[test]
    fn the_generic_question_heuristic() {
        assert!(generic_ends_with_question("Should I proceed?"));
        assert!(generic_ends_with_question("修复完成，需要发布吗？"));
        assert!(generic_ends_with_question(
            "Done.\n\nLet me know if you want the fix."
        ));
        assert!(!generic_ends_with_question("All shipped. CI is green."));
    }

    /// The event schema round-trips, and lowercase/kebab wire names hold.
    #[test]
    fn the_event_schema_round_trips() {
        let e = StateEvent {
            v: 1,
            ts: "2026-08-14T23:59:59Z".into(),
            sid: "abc".into(),
            agent: "claude".into(),
            cwd: Some("/w".into()),
            title: "t".into(),
            state: AgentState::Wait,
            prev: Some(AgentState::Busy),
            reason: StateReason::PlanApproval,
            detail: "plan ready".into(),
            confidence: Confidence::Observed,
            pid: Some(1),
            term: Some("%3".into()),
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains(r#""state":"wait""#) && j.contains(r#""reason":"plan-approval""#));
        let back: StateEvent = serde_json::from_str(&j).unwrap();
        assert_eq!(back.reason, StateReason::PlanApproval);
    }
}
