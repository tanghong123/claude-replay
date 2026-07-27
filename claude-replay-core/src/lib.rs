//! **claude-replay-core** — the agent-agnostic transcript parser/replay engine.
//!
//! This is the L1/L2 half of the three-layer engine (`design/parser-engine.md`): the
//! per-agent tokenizers (Claude + Codex → a canonical [`engine::message`] log), the shared
//! stateful [`model::Replayer`] fold, session assembly ([`Session`] / [`SessionIndex`]),
//! transcript discovery, incremental follow ([`follow::FollowParser`]), and the live
//! [`engine::store::SessionStore`]. It carries **no** TUI / HTML / CLI dependencies — only
//! `serde_json` and `anyhow` — so it can be reused by any frontend. The `claude-replay`
//! binary crate depends on it and adds the ratatui viewer, the HTML export, and the clap CLI.
//!
//! The split is enforced by the crate boundary: nothing here can reach ratatui/syntect/clap,
//! which guarantees the "core is presentation-agnostic" invariant that was previously only a
//! convention.

mod adapter;
mod agent;
pub mod claude_discover;
pub mod claude_metrics;
pub mod claude_model;
pub mod codex_discover;
pub mod codex_metrics;
pub mod codex_model;
pub mod discover;
pub mod engine;
pub mod follow;
pub mod metrics;
pub mod model;
pub mod tail;

// ── Public API ────────────────────────────────────────────────────────────────────────
// The intended surface for a library consumer. [`parse_session`] is THE entry point: it
// returns a [`Session`] (blocks + index + metrics + cwd) for a transcript path. For a live
// tail, [`FollowParser`] folds only appended bytes each poll. Everything a `Session` exposes
// is re-exported here so a consumer never has to reach through module paths.
pub use agent::Agent;
pub use engine::index::{AgentEntry, AttachmentEntry, ToolCount, ToolEntry, TurnEntry};
pub use engine::{parse_session, parse_session_as, Session, SessionIndex};
pub use follow::FollowParser;
pub use metrics::Metrics;
pub use model::{AgentStatus, Attachment, AttachmentContent, Block, Hunk, SubAgent};
