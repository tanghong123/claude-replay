//! **claude-replay-core** — the FACADE: the agent-free machinery
//! (`claude-replay-engine`) wired to the built-in adapters (`claude-replay-agents`),
//! presenting the same API this crate always had (#87 step 3). Consumers keep
//! importing from here; a third party composing its own agent set builds on the
//! engine crate directly and brings its own registry.
//!
//! What lives HERE (everything that consults the wired registry): the
//! [`adapter()`](adapter())/[`adapters`] lookup, registry-driven [`discover`]y
//! (`detect_agent`/`resolve_any`/`candidates_all`), the [`Transcript`] source handle,
//! and the `parse_session*` dispatchers. Everything else re-exports from the engine.
//!
//! No TUI / HTML / CLI dependencies — the crate boundary still enforces
//! "core is presentation-agnostic".

mod adapter;
pub mod discover;
mod session_entry;
pub mod transcript; // the canonical `Transcript` source handle (parse/follow/attachment)

pub use adapter::{adapter, adapters, MetricsAccumulator, SniffClaim, TranscriptAdapter};
pub use claude_replay_agents::{claude_discover, codex_discover, qoderwork_discover};
pub use claude_replay_engine::{diff, engine, fold, follow, metrics, model, seam, summary};
pub use session_entry::{
    parse_session, parse_session_as, parse_session_enriched, parse_session_enriched_as,
};

// ── Public API ────────────────────────────────────────────────────────────────────────
// The intended surface for a library consumer. [`parse_session`] is THE entry point: it
// returns a [`Session`] (blocks + index + metrics + cwd) for a transcript path. For a live
// tail, [`FollowParser`] folds only appended bytes each poll. Everything a `Session` exposes
// is re-exported here so a consumer never has to reach through module paths.
pub use claude_replay_engine::{
    Agent, AgentId, AgentStatus, Attachment, AttachmentContent, AttachmentEntry, Block,
    BlockAccess, BlockStore, ChildMeta, FollowParser, Hunk, InMemoryStore, LoadedAttachment,
    Metrics, Session, SessionAccumulator, SessionIndex, SessionMeta, SubAgent, SubAgentMeta,
    ToolCount, ToolEntry, TurnEntry,
};
pub use transcript::Transcript;
