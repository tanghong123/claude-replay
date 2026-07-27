//! The session engine — the agent-agnostic core of the three-layer parse · replay ·
//! present pipeline.
//!
//! Groups the shared machinery: the canonical `message` log (the Layer 1 ↔ Layer 2 boundary),
//! the `replay` fold that turns it into blocks, `session`/`index` assembly, the `cache`
//! residency tiers, and the pure `path`/`time` helpers the per-agent parsers share. Only
//! `index` and `session` are part of the public API; the rest are crate-internal.

pub(crate) mod builder; // the single incremental fold orchestrator (batch + live drive it)
pub(crate) mod cache; // residency tiers over incremental followers (the live HTML server)
pub mod index;
pub(crate) mod message; // the L1↔L2 vocabulary — internal; consumers see `Block`, never `Message`
pub(crate) mod path; // relativize helpers — internal to the parsers + HTML path rendering
pub(crate) mod replay; // Layer-2 fold engine (Replayer/Shaping)
pub mod session;
pub(crate) mod time; // epoch-seconds parsing — internal to the parsers/metrics

pub use builder::SessionAccumulator;
pub use index::SessionIndex;
pub use session::{
    build_sub_agents, parse_session, parse_session_as, parse_session_enriched,
    parse_session_enriched_as, BlockAccess, BlockStore, InMemoryStore, Session,
};
