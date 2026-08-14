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

/// What an agent-specific transcript preprocessor decided about one raw line.
///
/// Most agents pass every line through unchanged. Formats that physically embed another
/// transcript (for example a Codex child rollout's fork snapshot) can suppress those records,
/// or replace a transport-only record with canonical messages before the ordinary decoder sees
/// it. The same decision gates metrics, keeping content and usage on one session boundary.
#[derive(Debug)]
#[non_exhaustive]
pub enum PreprocessedLine {
    /// Run the adapter's ordinary [`TranscriptAdapter::decode_line`] and fold metrics.
    Include,
    /// The record belongs to embedded/bootstrap data, not this session.
    Ignore,
    /// Use these already-normalized messages instead of the ordinary decoder, while still
    /// folding this line's timestamp/usage into metrics.
    Messages(Vec<Message>),
}

/// Per-session raw-line state owned by the shared accumulator.
pub trait LinePreprocessor: Send {
    /// Classify or normalize one complete raw transcript line before decoding and metrics folding.
    fn process(&mut self, line: &str) -> PreprocessedLine;

    /// This preprocessor's fold state as an **opaque, serializable** value (#14) — whatever
    /// `process` reads that was learned from earlier lines. A stateless preprocessor returns
    /// `Null`, the default. Consumers store it inside a
    /// [`MetricsCursor`](crate::metrics_fold::MetricsCursor) and hand it back through
    /// [`restore`](Self::restore); they never interpret it.
    fn state(&self) -> Value {
        Value::Null
    }

    /// Restore what [`state`](Self::state) captured. A value this implementation does not
    /// recognize (foreign, stale format, `Null`) must be ignored — a cursor is a cache, and an
    /// unreadable cache is a cold start, never an error.
    fn restore(&mut self, _state: &Value) {}
}

struct PassThrough;

impl LinePreprocessor for PassThrough {
    fn process(&mut self, _line: &str) -> PreprocessedLine {
        PreprocessedLine::Include
    }
}

/// A per-agent token/cost accumulator, folded one transcript line at a time. Object-safe
/// (`Box<dyn>`) so the live follower can hold one without knowing the agent; `Send` so the
/// follower can move between threads (the HTML live server tails on a background thread).
pub trait MetricsAccumulator: Send {
    /// Fold one raw transcript line's usage into the running total.
    ///
    /// This is also the seam for **agent-specific** metrics the shared [`Metrics`] shouldn't
    /// grow a typed field for: an accumulator folds such a counter into the accumulating
    /// [`Metrics::extra`] bag here (each impl has a `bump(key, n)` helper), and `finish` emits
    /// it. Codex uses the bag for skipped-record diagnostics.
    fn push(&mut self, v: &Value);
    /// One raw JSONL record could not be parsed. Default no-op for adapters that deliberately
    /// expose no schema diagnostics; adapters with an observability counter record it here.
    fn malformed_line(&mut self) {}
    /// The metrics so far, without consuming the accumulator (for a live snapshot).
    fn finish(&self) -> Metrics;

    /// This accumulator's fold state as an **opaque, serializable** value (#14). The default
    /// captures exactly what [`reseed`](Self::reseed) can restore — the shared totals — which is
    /// the fidelity the durable cache already trusts for a resume. An adapter whose `push` reads
    /// **private** fold state must override BOTH this and [`restore`](Self::restore) to carry it:
    /// Codex banks cumulative usage against `last_total`/`model`, and a resume that lost those
    /// double-counted the first usage record after the checkpoint.
    fn state(&self) -> Value {
        serde_json::to_value(self.totals()).unwrap_or(Value::Null)
    }

    /// Restore what [`state`](Self::state) captured. Unrecognized input must be ignored — a
    /// cursor is a cache, and an unreadable one is a cold start, never an error.
    fn restore(&mut self, state: &Value) {
        if let Ok((tokens, extra, span)) =
            serde_json::from_value::<crate::metrics::MetricsTotals>(state.clone())
        {
            self.reseed(tokens, extra, span);
        }
    }

    /// Running totals as of the last [`push`](Self::push) — per model, plus the agent-specific
    /// counter bag and the observed span (#96 §7).
    ///
    /// The builder captures these at a turn-opening line and persists **differences**; the
    /// adapter converts whatever its agent reports (Claude per-message increments, Codex running
    /// totals) into these totals, so agent specificity ends at the decoder as it does everywhere
    /// else. Defaulted, because an accumulator that reports nothing resumes correctly from
    /// nothing.
    fn totals(&self) -> crate::metrics::MetricsTotals {
        Default::default()
    }

    /// Re-seed a resumed accumulator from folded totals and the observed span (#96 §7).
    fn reseed(
        &mut self,
        _tokens: std::collections::BTreeMap<String, crate::metrics::TokenCounts>,
        _extra: std::collections::BTreeMap<String, u64>,
        _span: Option<(crate::model::EpochSeconds, crate::model::EpochSeconds)>,
    ) {
    }
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
        let mut preprocessor = self.line_preprocessor();
        let mut line = String::new();
        while {
            line.clear();
            matches!(reader.read_line(&mut line), Ok(n) if n > 0)
        } {
            let complete = line.ends_with('\n');
            let body = line.trim_end();
            if body.is_empty() {
                continue; // a blank line carries nothing — neither content nor a diagnostic
            }
            if matches!(preprocessor.process(body), PreprocessedLine::Ignore) {
                continue;
            }
            match serde_json::from_str::<Value>(body) {
                Ok(v) => acc.push(&v),
                // A final line without its newline is a write IN PROGRESS — the agent is
                // appending at this moment — not schema drift. Counting it would flash a
                // "skipped" diagnostic on every one-shot parse of a live transcript.
                Err(_) if complete => acc.malformed_line(),
                Err(_) => {}
            }
        }
        acc.finish()
    }

    // ── incremental-follower primitives ──
    /// This agent's L2 shaping hooks (`&'static`, a per-agent const).
    fn shaping(&self) -> &'static Shaping;
    /// Decode one raw line into 0+ canonical messages (`cwd` threads across lines).
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>);
    /// A fresh per-session raw-line preprocessor. The default is a stateless pass-through;
    /// adapters only override this when their physical transcript contains records outside the
    /// logical session or transport records that need session context to decode.
    fn line_preprocessor(&self) -> Box<dyn LinePreprocessor> {
        Box::new(PassThrough)
    }
    /// Whether a tool of this `name` blocks on a **human** answer rather than on the agent
    /// doing work (#21) — Claude Code's `AskUserQuestion`/`ExitPlanMode` class. The
    /// distinction belongs to the adapter because the tool vocabulary does: a consumer
    /// computing "how long was the agent actually working" treats a gap ended by such a
    /// tool's result as user latency, not agent work, and hardcoding the names downstream
    /// makes an agent-agnostic consumer quietly agent-specific. Default `false`; adapters
    /// opt in as they gain equivalents.
    fn tool_is_interactive(&self, _name: &str) -> bool {
        false
    }
    /// Whether this raw transcript line says the assistant's TURN is over (#194) —
    /// `Some(true)` for an end-of-turn marker (Claude's `stop_reason: end_turn`, Codex's
    /// `task_complete`), `Some(false)` for a line that proves the turn is still open (a
    /// user/tool-result record feeding back, a mid-stream assistant chunk), `None` when
    /// the line says nothing either way. [`tail_pulse`](crate::state::tail_pulse) walks
    /// the tail backwards and takes the first opinion. Default `None`: an adapter
    /// without the vocabulary degrades the idle/busy split to the growth and in-flight
    /// signals, never to a wrong answer.
    fn turn_ended(&self, _raw_line: &str) -> Option<bool> {
        None
    }
    /// Whether a turn-ending assistant text reads as a QUESTION to the user (#194,
    /// owner-resolved: an adapter hook from day one, in the #21 mold). Only refines
    /// idle's context — never flips busy/wait. The default is the engine's generic
    /// heuristic; agents with distinctive turn-closing formats override.
    fn ends_with_question(&self, final_text: &str) -> bool {
        crate::state::generic_ends_with_question(final_text)
    }
    /// A fresh metrics accumulator for this agent.
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator>;

    /// Extract the `index`-th content-bearing attachment's bytes from one raw transcript
    /// `line` (the line a [`Deferred`](crate::model::AttachmentContent::Deferred) locator points
    /// at), for the facade's `Transcript::load_attachment`. Default
    /// `None` — an agent whose transcripts embed no attachments never produces a `Deferred`
    /// locator, so this is never called for it.
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
    /// The session this one was FORKED from, if the agent records forks (#142).
    ///
    /// Forking copies the conversation up to the fork point, so a fork's transcript is
    /// largely a replay of its origin's — measured on QoderWork, 82–99% of every fork.
    /// Returning the origin's id lets a consumer group a fork FAMILY instead of showing a
    /// dozen near-identical sessions. Default `None`: an agent without forks says nothing
    /// and every session is its own family.
    ///
    /// Ids only — resolving them to paths is [`resolve_id`](Self::resolve_id)'s job, and
    /// keeping this id-shaped means a consumer can build the whole family map from one pass.
    fn fork_origin(&self, _path: &Path) -> Option<String> {
        None
    }

    /// The LIVE on-disk task list for the session at `path` (#15). Default `None` —
    /// an agent with no task store (Codex) has none; Claude reads
    /// `~/.claude/tasks/<session-id>/*.json`. Backs `discover::session_tasks`.
    fn load_tasks(&self, _path: &Path) -> Option<crate::engine::tasks::TaskList> {
        None
    }

    /// Every MAIN session transcript in this agent's store, MACHINE-WIDE (#98 R1) — the
    /// monitor's scan surface, deliberately unscoped where
    /// [`candidates_scoped`](Self::candidates_scoped) is cwd-scoped. Sub-agent transcripts
    /// are excluded by the family that knows how they are stored. Defaulted empty: an
    /// adapter without a machine-wide answer simply contributes no rows.
    fn store_transcripts(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    /// Every SUB-AGENT transcript in this agent's store, MACHINE-WIDE, with its lineage:
    /// `(path, own session id, parent thread id)`. The scan surface
    /// ([`store_transcripts`](Self::store_transcripts)) deliberately excludes sub-agents
    /// from rows — but their usage is real spend, and a cost consumer that only folds main
    /// transcripts under-reports by whatever the sub-agents burned (measured on one Codex
    /// project: 95% of the total). The parent id is what lets the consumer bank each
    /// sub-agent's cost onto the ROOT session's account, chasing parent ids for nested
    /// spawns. Defaulted empty: an agent whose sub-agent activity lives INSIDE the main
    /// transcript (Claude's sidechains) has nothing separate to report.
    fn store_subagent_transcripts(&self) -> Vec<(std::path::PathBuf, String, String)> {
        Vec::new()
    }

    /// Whether this agent's sessions are ANCHORED to a workspace — a repo or working
    /// directory that identifies them (#98 §4.2). A monitor groups anchored agents'
    /// sessions by project; a desktop-collaboration agent (QoderWork) whose cwd is noise
    /// groups under the agent itself. Defaulted `true`: coding agents are the common case,
    /// and a new adapter that forgets this gets the harmless grouping, not a junk one.
    fn workspace_anchored(&self) -> bool {
        true
    }

    /// What this agent calls the session at `path` — see [`SessionCard`](crate::discover::SessionCard).
    ///
    /// Same class as [`load_tasks`](Self::load_tasks): it takes a path, it may do I/O, and the
    /// **fold never calls it**. That is not a convention to be careful about but the reason the
    /// hook can exist at all — an agent may keep its titles in its own database rather than the
    /// transcript, and the accumulator is sans-io by design.
    ///
    /// `memo` is whatever this adapter returned for this same path last time, or `None` on a
    /// first call, after a cache eviction, or when the stored memo could not be read. It is the
    /// adapter's own state — see [`CardMemo`](crate::discover::CardMemo) for the rules, of which
    /// the hardest is that a memo must never be required and never be trusted unverified.
    ///
    /// Default [`Absent`](crate::discover::CardOutcome::Absent): an agent that names nothing
    /// costs its adapter no code, and consumers already have a fallback that always exists.
    fn session_card(
        &self,
        _path: &Path,
        _memo: Option<&crate::discover::CardMemo>,
    ) -> crate::discover::CardOutcome {
        crate::discover::CardOutcome::Absent
    }

    /// [`session_card`](Self::session_card) for MANY sessions in one operation-scoped call, for
    /// adapters whose per-call setup dominates — opening a database, scanning a store. Provided:
    /// the obvious loop, which is right for an adapter that reads one file per session.
    ///
    /// Same shape and same reason as [`subagent_sources`](Self::subagent_sources): a memo makes
    /// the repeat cheap, but only a batch makes the *setup* cheap.
    fn session_cards(
        &self,
        items: &[(&Path, Option<&crate::discover::CardMemo>)],
    ) -> Vec<crate::discover::CardOutcome> {
        items
            .iter()
            .map(|(p, m)| self.session_card(p, *m))
            .collect()
    }
}
