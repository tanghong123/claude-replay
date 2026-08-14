//! **claude-replay-engine** — the agent-FREE transcript replay machinery (#87 step 3).
//!
//! Everything here is generic over agents: the [`Block`] data model, the L2 `Replayer`
//! fold (`engine::replay`), session assembly ([`Session`] / [`SessionIndex`]), the
//! block stores, the incremental follower ([`FollowParser`]), the discovery
//! *vocabulary* ([`discover`]), and the two contracts an agent plugs into — the
//! [`adapter::TranscriptAdapter`] trait and the [`seam`] (the audited helper surface
//! adapter code builds on). No adapter implementations live here: the built-ins are in
//! `claude-replay-agents`, and `claude-replay-core` is the facade that wires them to
//! these entry points. A third party adds an agent by implementing the trait against
//! this crate alone — no fork.
//!
//! Only `serde`/`serde_json` + `anyhow`; no TUI/HTML/CLI dependencies.

pub mod adapter;
mod agent;
pub mod diff;
pub mod discover;
pub mod engine;
pub mod fold;
pub mod follow;
pub mod metrics;
pub mod metrics_fold;
pub mod model;
pub mod state;
pub mod summary;

pub use agent::Agent;
pub use engine::index::{AttachmentEntry, ToolCount, ToolEntry, TurnEntry};
pub use engine::seam;
pub use engine::{
    BlockAccess, BlockStore, ChildMeta, InMemoryStore, Session, SessionAccumulator, SessionIndex,
    SessionMeta,
};
pub use follow::FollowParser;
pub use metrics::Metrics;
pub use model::{
    AgentId, AgentStatus, Attachment, AttachmentContent, Block, Hunk, LoadedAttachment, SubAgent,
    SubAgentMeta,
};
