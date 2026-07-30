//! **The adapter seam** (#87) — the complete, curated surface a per-agent adapter
//! ([`crate::agents`]) may use, in one auditable place. What A.1 of
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
//!   (the L2 hook table), [`SessionAccumulator`]; test-only, the frozen-reference
//!   vocabulary the equivalence gates drive (`QueueItem`, `stamp_user_turns`,
//!   `replay`, `parse_path_timed_for`, `parse_session_as`, `build_sub_agents`).
//! - **Folding usage** — [`estimate_cost`], [`parse_reader_for`], [`parse_ts`],
//!   [`TimeSpan`].
//! - **Discovering transcripts** — [`ancestors_below`], [`home_dir`], and the task
//!   sidecar vocabulary [`TaskList`]/[`TaskOp`]/[`task_from_json`].
//! - **Small shared utilities** — [`relativize`], [`epoch_secs`].
//! - **The Claude-format store, for derived agents** — [`candidates_scoped_in`] /
//!   [`transcript_by_id_in`]: discovery over a Claude-shaped store rooted anywhere.
//!   A derived agent (QoderWork) builds on these instead of reaching into the Claude
//!   family's internals.

pub(crate) use crate::agents::claude::discover::{candidates_scoped_in, transcript_by_id_in};
pub(crate) use crate::discover::{ancestors_below, home_dir, Candidate};
pub(crate) use crate::engine::builder::SessionAccumulator;
pub(crate) use crate::engine::message::{Message, QueueOpKind};
pub(crate) use crate::engine::path::relativize;
pub(crate) use crate::engine::replay::Shaping;
// The frozen whole-file reference vocabulary the equivalence gates drive (test-only).
#[cfg(test)]
pub(crate) use crate::engine::replay::{
    parse_path_timed_for, replay, stamp_user_turns, QueueItem, Replayer,
};
pub(crate) use crate::engine::tasks::{task_from_json, TaskList, TaskOp};
pub(crate) use crate::engine::time::epoch_secs;
#[cfg(test)]
pub(crate) use crate::engine::{build_sub_agents, parse_session_as};
#[cfg(test)]
pub(crate) use crate::metrics::human_tokens;
pub(crate) use crate::metrics::{estimate_cost, parse_reader_for, parse_ts, Metrics, TimeSpan};
pub(crate) use crate::model::*;
pub(crate) use crate::Agent;
