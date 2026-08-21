//! The session engine — the agent-agnostic core of the three-layer parse · replay ·
//! present pipeline.
//!
//! Groups the shared machinery: the canonical `message` log (the Layer 1 ↔ Layer 2 boundary),
//! the `replay` fold that turns it into blocks, `session`/`index` assembly,
//! and the pure `path`/`time` helpers the per-agent parsers share. Only
//! `index` and `session` are part of the public API; the rest are crate-internal.

pub(crate) mod builder; // the single incremental fold orchestrator (batch + live drive it)
pub mod elide; // the bounded eliding line reader — the only byte-toucher (#193)
pub mod index;
pub(crate) mod message; // the L1↔L2 vocabulary — internal; consumers see `Block`, never `Message`
pub mod meta_stream; // the meta record stream + its emission protocol (#96)
pub(crate) mod path; // relativize helpers — internal to the parsers + HTML path rendering
pub(crate) mod reader; // the follower's byte-offset line reader (tail + resume) — machinery (#87)
pub(crate) mod replay; // Layer-2 fold engine (Replayer/Shaping)
pub mod seam; // the audited adapter contract — everything `crate::agents` may use (#87)
pub mod session;
pub mod tasks; // the agent-neutral task/todo model + op-log fold (#15)
pub mod tier_b;
pub(crate) mod time; // epoch-seconds parsing — internal to the parsers/metrics

pub use builder::{SessionAccumulator, StreamRead};
pub use elide::{
    read_line_elided, Elision, LineOutcome, ELIDE_CEILING, ELIDE_STRING_BYTES, POSTFIX_KEEP,
    PREFIX_KEEP, SCAN_THRESHOLD,
};
pub use reader::{bounded_lines, ElisionCounts, LineSource, TornTail};
// The frontier's one tuning bound — public so a consumer (and the test that guards it) can name
// it rather than restate the number.
pub use index::SessionIndex;
pub use replay::MAX_PINNED_TURNS;
pub use session::{
    build_sub_agents, populate_sub_agent_transcripts, ArcStore, BlockAccess, BlockRead, BlockStore,
    ChildMeta, InMemoryStore, Session, SessionMeta,
};
pub use tasks::{TaskItem, TaskList, TaskStatus};
