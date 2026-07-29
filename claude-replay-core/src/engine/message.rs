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
//! format. An agent that has no analogue for a variant (Codex emits no `QueueOp`/
//! `Completion`/`SkillBody`) simply never produces it.
//!
//! Phase note: no variant carries a built `Block` (the M14 block-model lift) — `ToolUse`
//! holds raw `name`/`input`/`cwd` and L2 shapes the block via `Shaping::build_tool`; the
//! richer content variants carry structured fields L1 fills in. `Attachment` is a leaf value.
//! One later, separately-gated step remains: the incremental phase's `seq`/`offset`/`Reset`
//! envelope (§5.2 Phase 6). `tokenize`/`replay` are pure and I/O-free (§3.6's sans-I/O core).

use crate::model::{AgentStatus, Attachment, EpochSeconds};
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
}
