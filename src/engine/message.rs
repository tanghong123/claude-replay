//! The canonical **message log** — the Layer 1 ↔ Layer 2 contract of the session
//! engine (see `design/parser-engine.md` §0, §6.1-resolved).
//!
//! A `Message` is the fine-grained "one message per interesting line/content-item"
//! vocabulary an agent's **raw parser** (Layer 1, `tokenize`) emits: pure line-shape
//! mapping, with **no** back-patch, grouping, or joins. The agent-agnostic **replay /
//! state builder** (Layer 2, `replay`) folds this stream into the block list — it owns
//! the `id → block index` back-patch, the thinking clock, user-turn stamping, the queue
//! lifecycle, and turn grouping.
//!
//! Phase 1 note: this is a deliberate **waypoint** toward the clean, agent-neutral `Event`
//! vocabulary specified in `design/parser-engine.md` §3.1 — see the "Message waypoint" note
//! there. The variants carry `crate::model` types (`Block`, `Attachment`) so the split
//! reuses the exact block-shaping already proven correct, guaranteeing the
//! `replay(tokenize(x)) == parse_main(x)` equivalence. Two later, separately-gated steps
//! converge it: the block-model lift drops this `Block` back-reference and folds the
//! Claude-shaped variants into the `Event` set; the incremental phase (§5.2 Phase 6) adds
//! the `seq` / `offset` / `Reset` envelope. `tokenize` / `replay` are already pure and
//! I/O-free, so they satisfy §3.6's sans-I/O pull core now — only the vocabulary converges.

use crate::model::{Attachment, Block};
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
    LineStart(Option<f64>),
    /// A `user`-type line occurred — the trigger that resets the thinking-duration clock.
    /// Emitted right after this line's `LineStart`, before its content messages.
    Trigger(Option<f64>),
    /// Assistant prose content item (already non-empty).
    AssistantText(String),
    /// Assistant thinking content item + this line's ts (the fold computes the duration
    /// as `ts − trigger_ts`).
    Thinking { text: String, ts: Option<f64> },
    /// A `tool_use` (an ordinary tool or an `Agent`/`Task` spawn) — its initial block
    /// shape plus the join `id`. The fold appends it, records `id → index` for the
    /// back-patch, and tracks the most recent `Skill`.
    ToolUse { id: String, block: Block },
    /// A `tool_result` to join by id: its text plus the message-level `toolUseResult`
    /// metadata (`Value::Null` when absent).
    ToolResult {
        tool_use_id: String,
        text: String,
        tur: Value,
    },
    /// A `user` line whose `message.content` is a plain string (raw), plus whether the
    /// line is injected system content (`isMeta`/`isCompactSummary`).
    UserString { text: String, injected: bool },
    /// A non-empty `text` item inside a `user` array `message.content`, plus the injected
    /// flag.
    UserArrayText { text: String, injected: bool },
    /// A consumed mid-turn prompt (`queued_command` with `commandMode == "prompt"`) —
    /// rendered as a user turn at the point it took effect.
    AttachmentPrompt { text: String },
    /// A file / plan / image attachment to surface as-is.
    Attachment(Attachment),
    /// A `queue-operation` line + its content (if any). The fold owns the queue
    /// lifecycle (marker emit, FIFO pop, immediate-pickup suppression, completions).
    QueueOp {
        op: QueueOpKind,
        content: Option<String>,
    },
}
