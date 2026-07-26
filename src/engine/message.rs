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
//! there. The **block-model lift is done** (M14): no variant carries a built `Block` — the
//! `ToolUse` variant now holds the raw `name`/`input`/`cwd`, and Layer 2 builds the block
//! via `Shaping::build_tool`, so Layer 1 (`tokenize`) is pure line-shape → raw fields. The
//! variants still carry the `Attachment` value type (a leaf, not a shaped block). One later,
//! separately-gated step remains: the incremental phase's `seq` / `offset` / `Reset`
//! envelope (§5.2 Phase 6). `tokenize` / `replay` are already pure and I/O-free, so they
//! satisfy §3.6's sans-I/O pull core now — only the vocabulary converges.

use crate::model::Attachment;
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
