//! **The adapter seam** (#87) — the complete, curated surface a per-agent adapter
//! (`claude-replay-agents`) may use, in one auditable place. What A.1 of
//! `design/core-layout.md` found scattered as ad-hoc imports is now the written
//! contract: an adapter that needs something new gets it added HERE, deliberately —
//! `agents_import_only_the_seam` (in `agents/mod.rs`) fails any other `crate::` path.
//! When the engine/agents crate split lands (step 3), this module's contents are
//! exactly what `claude-replay-engine` must export to adapter crates.
//!
//! Grouped by what the adapter is doing:
//!
//! - **Speaking the vocabulary** — the public data model, re-exported flat so agent
//!   files need one import root: everything in [`crate::model`], plus [`Agent`],
//!   [`Metrics`], [`Candidate`].
//! - **Feeding the fold** — [`Message`]/[`QueueOpKind`] (the L1 output), [`Shaping`]
//!   (the L2 hook table), [`SessionAccumulator`]; doc-hidden, the frozen-reference
//!   vocabulary the equivalence gates drive (`QueueItem`, `stamp_user_turns`,
//!   `replay`, `parse_path_timed_for`, `build_sub_agents`).
//! - **Folding usage** — [`estimate_cost`], `parse_reader_for`, [`parse_ts`],
//!   [`TimeSpan`].
//! - **Discovering transcripts** — [`ancestors_below`], [`home_dir`], and the task
//!   sidecar vocabulary [`TaskList`]/[`TaskOp`]/[`task_from_json`], plus
//!   [`taskq_create_ops`]/[`taskq_ops`] — the agent-neutral `taskq` decode, here rather
//!   than in a family because it reads that queue's contract and no agent's.
//! - **Small shared utilities** — [`relativize`], [`epoch_secs`].
//! - **Derived-agent reuse** (a Claude-format store rooted anywhere) now lives WITH
//!   the families in `claude-replay-agents` — an intra-crate import there, no longer
//!   a seam concern (#87 step 3).

pub use crate::adapter::{LinePreprocessor, PreprocessedLine, SpawnLink, SpawnRoster};
pub use crate::discover::{
    ancestors_below, home_dir, Candidate, CardMemo, CardOutcome, SessionCard, SNIPPET_CHARS,
};
pub use crate::engine::builder::SessionAccumulator;
// #193: the bounded eliding line reader — the one scanner every consumer shares (adapters
// hand out an `Elision` policy the way they hand out `Shaping`; third parties drive
// `LineSource` instead of growing their own scanner that drifts).
pub use crate::engine::elide::{
    parse_marker, read_line_elided, Elision, LineOutcome, MarkerSpan, ELIDE_CEILING,
    ELIDE_STRING_BYTES, POSTFIX_KEEP, PREFIX_KEEP, SCAN_THRESHOLD,
};
pub use crate::engine::message::{Message, QueueOpKind};
pub use crate::engine::path::relativize;
pub use crate::engine::reader::{bounded_lines, ElisionCounts, LineSource, TornTail};
pub use crate::engine::replay::Shaping;
// The frozen whole-file reference vocabulary the adapter crates' equivalence gates
// drive. `#[doc(hidden)]` rather than cfg(test): a cfg(test) item is invisible to a
// DOWNSTREAM crate's tests, and these must cross the engine/agents boundary (#87).
#[doc(hidden)]
pub use crate::engine::build_sub_agents;
#[doc(hidden)]
pub use crate::engine::replay::{
    parse_path_timed_for, replay, stamp_user_turns, QueueItem, Replayer,
};
pub use crate::engine::taskq::{queue_tasks, taskq_create_ops, taskq_ops, TASKQ_SENTINEL};
pub use crate::engine::tasks::{task_from_json, TaskList, TaskOp, Todo};
pub use crate::engine::time::epoch_secs;
#[doc(hidden)]
pub use crate::metrics::human_tokens;
pub use crate::metrics::{
    credits_cost, estimate_cost, parse_reader_with, parse_ts, total_cost, Metrics, MetricsTotals,
    RateLimitWindow, RateLimits, RuntimeInfo, TimeSpan, TokenCounts,
};
#[doc(hidden)]
pub use crate::model::attach_skill_body;
pub use crate::model::*;
pub use crate::Agent;
