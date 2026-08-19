//! The canonical **message log** — the Layer 1 ↔ Layer 2 contract of the session
//! engine (see `design/parser-engine.md` §0, §6.1-resolved).
//!
//! A `Message` is the fine-grained "one message per interesting line/content-item"
//! vocabulary an agent's **raw parser** (Layer 1, `tokenize`) emits. It is the **shared,
//! agent-neutral interface**: each agent's L1 decoder maps *its own* raw transcript format
//! onto these variants — including the richer ones (`Completion`, `Command`, `SkillBody`,
//! `SystemNote`, `QueueOp`, `Attachment`). Because agents name/shape these things
//! differently, all of that format parsing is L1's job; the variants carry **structured,
//! normalized fields**, never raw agent strings. The agent-agnostic **replay / state
//! builder** (Layer 2, `replay`) folds the stream into the block list — back-patch, thinking
//! clock, user-turn stamping, queue lifecycle, turn grouping — and parses **no** raw agent
//! format. An agent that has no analogue for a variant (Codex emits no `QueueOp` or
//! `SkillBody`) simply never produces it.
//!
//! Phase note: no variant carries a built `Block` (the M14 block-model lift) — `ToolUse`
//! holds raw `name`/`input`/`cwd` and L2 shapes the block via `Shaping::build_tool`; the
//! richer content variants carry structured fields L1 fills in. `Attachment` is a leaf value.
//! One later, separately-gated step remains: the incremental phase's `seq`/`offset`/`Reset`
//! envelope (§5.2 Phase 6). `tokenize`/`replay` are pure and I/O-free (§3.6's sans-I/O core).

use crate::engine::tasks::TaskOp;
use crate::model::{AgentStatus, AssistantPhase, Attachment, EpochSeconds};
use serde_json::Value;

/// Which queue-operation a `queue-operation` line performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueOpKind {
    Enqueue,
    Remove,
    Dequeue,
}

/// One entry in the canonical message log. Ordered exactly as the transcript's lines
/// (and, within a line, its content items) appear.
#[derive(Debug, Clone)]
pub enum Message {
    /// The start of a successfully-parsed transcript line, carrying its epoch-seconds
    /// timestamp. Drives the deferred user-turn stamping: the fold stamps the *previous*
    /// line's turns with the running `pending_ts`, then adopts this line's ts — exactly
    /// `parse_main`'s `pending_ts` / `stamped` dance, so `user_times` come out identical.
    LineStart(Option<EpochSeconds>),
    /// Assistant prose content item (already non-empty).
    AssistantText(String),
    /// Assistant prose with an explicitly persisted phase.
    AssistantMessage { text: String, phase: AssistantPhase },
    /// Assistant thinking content item + this line's ts (the fold computes the duration
    /// as `ts − the previous line's ts` — CC's thinking clock, #57).
    Thinking {
        text: String,
        ts: Option<EpochSeconds>,
    },
    /// A `tool_use` (an ordinary tool or an `Agent`/`Task` spawn) — its **raw** fields
    /// (the join `id`, the tool `name`, the call `input`, and the session `cwd` used to
    /// relativize path targets), NOT a built block. The fold builds the block via the
    /// agent's `Shaping::build_tool` hook, then appends it, records `id → index` for the
    /// back-patch, and tracks the most recent `Skill`. Keeping the raw fields here (rather
    /// than a `Block`) is the block-model lift: Layer 1 no longer shapes blocks.
    ToolUse {
        id: String,
        name: String,
        input: Value,
        cwd: String,
    },
    /// A `tool_result` to join by id: its text plus the message-level `toolUseResult`
    /// metadata (`Value::Null` when absent).
    ToolResult {
        tool_use_id: String,
        text: String,
        tur: Value,
        /// Whether this result reports a FAILURE, when the agent's format says (#23/#26):
        /// Claude's `is_error` content key — written on failure and OMITTED on success (for
        /// every tool but Bash), so for Claude an absent key decodes as `Some(false)` and
        /// `is_error` is never `None`. `None` is reserved for formats that give no signal
        /// either way (Codex rollouts carry none) — "unknown" stays distinct from "succeeded",
        /// so a consumer computing a failure RATE can exclude the undecidable instead of
        /// silently counting it as success.
        is_error: Option<bool>,
    },
    /// A genuine human turn — already cleaned by L1 (caveats stripped, classified as neither
    /// a command nor injected/system content). Becomes a `UserText` block.
    UserText { text: String },
    /// Injected/system content that isn't a human turn — a caveat-stripped `isMeta` body, a
    /// `/compact` summary, a task-notification's one-line summary, or an orphan skill body.
    /// Becomes a foldable `ToolResult` block. L1 has already reduced it to its final text.
    SystemNote { text: String },
    /// A loaded skill's instruction body (L1-detected). The fold nests it into the most
    /// recent `Skill` tool block; if there is none, it falls back to `fallback` as a
    /// `SystemNote`-style result block. Only L1 knows the raw format; the fold only places it.
    SkillBody { text: String, fallback: String },
    /// A slash-command invocation (`/name args`) with any inline stdout — L1-parsed from
    /// Claude's `<command-name>`/`<command-args>`/`<local-command-stdout>` wrappers. Becomes
    /// a `Command` block.
    Command {
        name: String,
        args: String,
        output: Vec<String>,
    },
    /// Standalone local-command stdout (no command line on the same message). The fold
    /// appends it to the preceding `Command` block, else starts a command-less one.
    CommandStdout { text: String },
    /// A consumed mid-turn prompt (`queued_command` with `commandMode == "prompt"`) —
    /// rendered as a user turn at the point it took effect.
    AttachmentPrompt { text: String },
    /// A file / plan / image attachment to surface as-is.
    Attachment(Attachment),
    /// A task-queue operation (#15) — emitted by an agent's L1 ALONGSIDE the
    /// `ToolUse` when it sees a `TaskCreate`/`TaskUpdate` call (only the tokenizer
    /// sees tool inputs; the built `ToolUse` block doesn't retain them). Folded by
    /// the ACCUMULATOR into the session's [`TaskList`](crate::engine::tasks::TaskList)
    /// — not by the block replayer, so the block oracle and its equivalence gates
    /// are untouched. An agent with no task tools (Codex) never emits it.
    TaskOp(TaskOp),
    /// A `queue-operation` line + its content (if any). The fold owns the queue
    /// *lifecycle* (marker emit, FIFO pop, immediate-pickup suppression) but no longer
    /// parses the content: `prose` is L1's classification of whether this enqueue should
    /// render as a visible `⧗ queued` marker (vs. a silent bookkeeping entry).
    QueueOp {
        op: QueueOpKind,
        content: Option<String>,
        prose: bool,
    },
    /// An agent/task **completion** — the structured form of Claude's `<task-notification>`,
    /// parsed by L1 so the fold never sees the raw format. The fold emits an `AgentDone`
    /// block and back-patches the matching `SubAgent`'s terminal status. `status` is `None`
    /// when the source carried no explicit status (the fold then defaults `AgentDone` to
    /// `Completed` but leaves the spawn's status untouched). Join by `tool_use_id` first,
    /// else `task_id`.
    Completion {
        /// The spawning `Agent`/`Task` tool_use id (`<tool-use-id>`) — the primary join to the
        /// `SubAgent` spawn. Empty when the notification carried none.
        tool_use_id: String,
        /// The `<task-id>`; for an agent completion this **is the agent's id** (fallback join,
        /// matched against `SubAgent.agent_id`). Empty when absent.
        task_id: String,
        /// Terminal state (`<status>`); `None` when the notification carried none.
        status: Option<AgentStatus>,
        /// Human description from the notification (`Agent "…"`).
        description: String,
        /// The agent's returned text (`<result>`), if any.
        result: Option<String>,
    },
    /// A **context-compaction boundary** — the metadata half of a compaction, from the
    /// record the agent writes at the cut. The fold pushes a summary-less
    /// [`Block::Compaction`](crate::model::Block::Compaction); the prose arrives next as
    /// [`CompactSummary`](Self::CompactSummary).
    CompactBoundary {
        trigger: crate::model::CompactTrigger,
        pre_tokens: u64,
        post_tokens: u64,
    },
    /// A late before/after context-usage pair for the compaction boundary immediately before
    /// it. Some agents persist the boundary first and the compacted context size in a following
    /// usage snapshot, so Layer 1 cannot populate both fields in `CompactBoundary`. The fold
    /// back-patches that divider when this arrives; without a preceding divider it is ignored.
    CompactUsage { pre_tokens: u64, post_tokens: u64 },
    /// The **continuation summary** written back after a compaction — the prose half.
    /// The fold fills it into the `Compaction` block it directly follows; with no such
    /// block it degrades to a `SystemNote`-style result block, which is what this content
    /// rendered as before compactions were paired.
    CompactSummary { text: String },
}

impl Message {
    /// Can this message open a **turn** — i.e. author a `UserText`/`Command` block (#96 §6.1)?
    ///
    /// The set is every `Replayer::apply` arm that pushes one, and it lives here so a new arm
    /// sits beside the predicate it must update. Read off the fold, not guessed: `CommandStdout`
    /// belongs because it pushes a `Block::Command` when there is no preceding `Command` to
    /// attach to — the case that is easy to miss. `SkillBody` and `QueueOp` push a `ToolResult`
    /// and a `QueueEvent` and are excluded.
    ///
    /// **Over-approximating is safe, under-approximating is not**: a missed line loses a resume
    /// point, while a spurious one only wastes a capture that the "did it author a block?" check
    /// then discards.
    pub fn can_open_turn(&self) -> bool {
        matches!(
            self,
            Message::UserText { .. }
                | Message::Command { .. }
                | Message::CommandStdout { .. }
                | Message::AttachmentPrompt { .. }
        )
    }
}
