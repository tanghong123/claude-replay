//! **claude-replay-core** — the agent-agnostic transcript parser/replay engine.
//!
//! This is the L1/L2 half of the three-layer engine: the per-agent tokenizers (Claude +
//! Codex → a canonical `engine::message` log), the shared stateful `Replayer` fold (in
//! `engine::replay`), session assembly ([`Session`] / [`SessionIndex`]), transcript discovery,
//! incremental follow ([`FollowParser`]), and the live `engine::store::SessionStore`. It
//! carries **no** TUI / HTML / CLI dependencies — only
//! `serde_json` and `anyhow` — so it can be reused by any frontend. The `claude-replay`
//! binary crate depends on it and adds the ratatui viewer, the HTML export, and the clap CLI.
//!
//! The split is enforced by the crate boundary: nothing here can reach ratatui/syntect/clap,
//! which guarantees the "core is presentation-agnostic" invariant that was previously only a
//! convention.

mod adapter;
mod agent;
pub mod claude_discover; // pub: the viewer's `--dump` stem + jdi's Claude supervisor use it
pub(crate) mod claude_metrics; // internal: reached via the adapter / metrics dispatch
pub(crate) mod claude_model; // internal: L1 tokenizer, reached via the adapter registry
pub mod codex_discover; // pub: jdi's Codex supervisor uses it
pub(crate) mod codex_metrics; // internal
pub(crate) mod codex_model; // internal: reached via the adapter registry
pub mod diff;
pub mod discover;
pub mod engine;
pub mod follow;
pub mod metrics;
pub mod model;
pub(crate) mod reader; // internal: the follower's byte-offset line reader (tail + resume)

// ── Public API ────────────────────────────────────────────────────────────────────────
// The intended surface for a library consumer. [`parse_session`] is THE entry point: it
// returns a [`Session`] (blocks + index + metrics + cwd) for a transcript path. For a live
// tail, [`FollowParser`] folds only appended bytes each poll. Everything a `Session` exposes
// is re-exported here so a consumer never has to reach through module paths.
pub use agent::Agent;
pub use engine::index::{AttachmentEntry, ToolCount, ToolEntry, TurnEntry};
pub use engine::{
    parse_session, parse_session_as, parse_session_enriched, parse_session_enriched_as, Session,
    SessionBuilder, SessionIndex,
};
pub use follow::FollowParser;
pub use metrics::Metrics;
pub use model::{
    AgentId, AgentStatus, Attachment, AttachmentContent, Block, Hunk, SubAgent, SubAgentMeta,
};
