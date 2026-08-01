//! **The per-agent contract** — the one trait an agent implements (#87 step 3).
//!
//! Everything that varies by agent lives behind [`TranscriptAdapter`]: the Layer-1
//! tokenizer + its `Shaping` hooks, the metrics accumulator, transcript detection, and
//! discovery. This crate is agent-FREE: the implementations live in
//! `claude-replay-agents` (built on [`crate::seam`]), and the registry that wires them
//! to the machinery lives in the facade crate (`claude-replay-core`). Adding an agent is
//! one `impl TranscriptAdapter` + one registry row there — no `match agent` anywhere.
//! (This mirrors `jdi::agent::adapter` on the supervisor side.)

use crate::discover::Candidate;
use crate::engine::message::Message;
use crate::engine::replay::Shaping;
use crate::metrics::Metrics;
use crate::model::Block;
use crate::Agent;
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// A per-agent token/cost accumulator, folded one transcript line at a time. Object-safe
/// (`Box<dyn>`) so the live follower can hold one without knowing the agent; `Send` so the
/// follower can move between threads (the HTML live server tails on a background thread).
pub trait MetricsAccumulator: Send {
    /// Fold one raw transcript line's usage into the running total.
    ///
    /// This is also the seam for **agent-specific** metrics the shared [`Metrics`] shouldn't
    /// grow a typed field for: an accumulator folds such a counter into the accumulating
    /// [`Metrics::extra`] bag here (each impl has a `bump(key, n)` helper), and `finish` emits
    /// it. No agent populates `extra` yet — the interface is ready for the first one.
    fn push(&mut self, v: &Value);
    /// The metrics so far, without consuming the accumulator (for a live snapshot).
    fn finish(&self) -> Metrics;
}

/// The single agent-specific interface. A new agent implements this once; the engine calls
/// it via the facade's `adapter()`. The three per-agent hooks (`sniff`/`decode_line`/`metrics_acc` + the
/// `shaping` const) drive the shared [`SessionAccumulator`](crate::engine::builder::SessionAccumulator),
/// which both the whole-file batch parse and the live follower feed, so batch and live share
/// one seam. Discovery (`candidates_scoped`/`resolve_id`) and the optional
/// `enrich`/`subagent_source`/`subagent_sources` round it out.
/// An adapter's claim strength on a sniffed transcript head (#59). Ordering matters
/// to the facade's `detect_agent`: `Owns` beats `CanParse` beats `No`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SniffClaim {
    /// A distinctive marker proves the transcript belongs to this agent.
    Owns,
    /// The format is compatible (parseable), but nothing proves ownership.
    CanParse,
    /// Not this agent's format.
    No,
}

pub trait TranscriptAdapter: Sync {
    /// Which agent this adapter handles.
    fn agent(&self) -> Agent;

    /// How strongly this adapter claims a transcript whose head parses to `head`
    /// (#59). [`SniffClaim::Owns`] means a DISTINCTIVE marker proves the transcript
    /// is this agent's (Codex's `session_meta`, QoderWork's `runtime-config`);
    /// [`SniffClaim::CanParse`] means only "the format is compatible" (Claude's
    /// adapter can parse any Claude-format lines, including derived agents').
    /// `detect_agent` picks an OWNER over a mere parser, so a new Claude-format
    /// agent never needs a carve-out in Claude's sniff to be detected correctly.
    fn sniff(&self, head: &Value) -> SniffClaim;

    // ── whole-file parse ──
    /// Load sub-agent child transcripts into their `SubAgent.blocks`. Default no-op for
    /// adapters whose source has no resolvable sub-agent tree. Backs the facade's
    /// `parse_session_enriched`.
    fn enrich(&self, _path: &Path, _blocks: &mut [Block]) {}
    /// Metrics only, from a reader. A provided method: fold every line through a fresh
    /// [`MetricsAccumulator`] — identical for every agent, so no adapter overrides it.
    fn parse_reader(&self, reader: &mut dyn io::BufRead) -> Metrics {
        let mut acc = self.metrics_acc();
        let mut line = String::new();
        while {
            line.clear();
            matches!(reader.read_line(&mut line), Ok(n) if n > 0)
        } {
            if let Ok(v) = serde_json::from_str::<Value>(line.trim_end()) {
                acc.push(&v);
            }
        }
        acc.finish()
    }

    // ── incremental-follower primitives ──
    /// This agent's L2 shaping hooks (`&'static`, a per-agent const).
    fn shaping(&self) -> &'static Shaping;
    /// Decode one raw line into 0+ canonical messages (`cwd` threads across lines).
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>);
    /// A fresh metrics accumulator for this agent.
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator>;

    /// Extract the `index`-th content-bearing attachment's bytes from one raw transcript
    /// `line` (the line a [`Deferred`](crate::model::AttachmentContent::Deferred) locator points
    /// at), for the facade's `Transcript::load_attachment`. Default
    /// `None` — an agent whose transcripts embed no attachments (Codex) never produces a
    /// `Deferred` locator, so this is never called for it.
    fn load_attachment(
        &self,
        _line: &str,
        _index: usize,
    ) -> Option<crate::model::LoadedAttachment> {
        None
    }

    // ── discovery ──
    /// Sessions for `cwd` (or its nearest ancestor with sessions), as picker candidates.
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate>;
    /// Resolve a bare session id to its transcript path in this agent's store.
    fn resolve_id(&self, id: &str) -> Option<PathBuf>;
    /// The source transcript of sub-agent `child_id` spawned under the session at `root`,
    /// if it exists. Default `None` for adapters whose source has no resolvable child
    /// transcript. Backs the presentation layer's descend-into-child and per-child HTML
    /// streams.
    fn subagent_source(&self, _root: &Path, _child_id: &str) -> Option<PathBuf> {
        None
    }
    /// Resolve MANY children's transcript paths in one operation-scoped call — returned in
    /// `ids` order. The default delegates to [`TranscriptAdapter::subagent_source`] once per
    /// child; adapters backed by a relationship store override this to scan that store once.
    /// Path-only, so every batch consumer shares it: the enriched parse's sub-agent meta AND
    /// a presentation layer registering a parent's children (the live server's child
    /// registry) resolve through the same single scan.
    fn subagent_sources(&self, root: &Path, ids: &[&str]) -> Vec<Option<PathBuf>> {
        ids.iter()
            .map(|id| self.subagent_source(root, id))
            .collect()
    }
    /// Is `path` inside this agent's OWN transcript store (#66)? Store provenance
    /// proves ownership even without an in-band marker — a file in
    /// `~/.claude/projects` is Claude's, no sniff needed. Default `false`.
    fn store_contains(&self, _path: &Path) -> bool {
        false
    }
    /// The LIVE on-disk task list for the session at `path` (#15). Default `None` —
    /// an agent with no task store (Codex) has none; Claude reads
    /// `~/.claude/tasks/<session-id>/*.json`. Backs `discover::session_tasks`.
    fn load_tasks(&self, _path: &Path) -> Option<crate::engine::tasks::TaskList> {
        None
    }
}
