//! **The per-agent seam** — one trait, one registry.
//!
//! Everything that varies by agent lives behind [`TranscriptAdapter`]: the Layer-1
//! tokenizer + its `Shaping` hooks, the metrics accumulator, transcript detection, and
//! discovery. The rest of the engine (the [`Replayer`](crate::engine::replay::Replayer) fold,
//! `Session`, the parse dispatchers, the live follower) is agent-agnostic and reaches the
//! per-agent behavior only through [`adapter`]. Adding an agent is therefore one `impl
//! TranscriptAdapter` + one row in [`adapter`]/[`adapters`] — no `match agent` scattered
//! across the codebase. (This mirrors `jdi::agent::adapter` on the supervisor side.)

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
pub(crate) trait MetricsAccumulator: Send {
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
/// it via [`adapter`]. The three per-agent hooks (`sniff`/`decode_line`/`metrics_acc` + the
/// `shaping` const) drive the shared [`SessionBuilder`](crate::engine::builder::SessionBuilder),
/// which both the whole-file batch parse and the live follower feed, so batch and live share
/// one seam. Discovery (`candidates_scoped`/`resolve_id`) and the optional
/// `enrich`/`subagent_source` round it out.
pub(crate) trait TranscriptAdapter: Sync {
    /// Which agent this adapter handles.
    fn agent(&self) -> Agent;

    /// Does a transcript whose head parses to `head` look like this agent's format?
    /// (Used by [`crate::discover::detect_agent`]; the sniffs are mutually exclusive.)
    fn sniff(&self, head: &Value) -> bool;

    // ── whole-file parse ──
    /// Load sub-agent child transcripts into their `SubAgent.blocks` (Claude's flat
    /// `subagents/` dir). Default no-op — an agent with no sub-agent tree (Codex) doesn't
    /// enrich. Backs [`crate::parse_session_enriched`].
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
    /// at), for [`Transcript::load_attachment`](crate::Transcript::load_attachment). Default
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
    /// if it exists. Default `None` — an agent with no sub-agent tree (Codex) has none;
    /// Claude resolves its flat `<root-stem>/subagents/agent-<id>.jsonl` layout. Backs the
    /// presentation layer's descend-into-child and per-child HTML streams.
    fn subagent_source(&self, _root: &Path, _child_id: &str) -> Option<PathBuf> {
        None
    }
}

/// The adapter for `agent`.
pub(crate) fn adapter(agent: Agent) -> &'static dyn TranscriptAdapter {
    match agent {
        Agent::Claude => &ClaudeAdapter,
        Agent::Codex => &CodexAdapter,
    }
}

/// Every registered adapter, in a stable order (drives `detect_agent` iteration and the
/// cross-agent picker order). A new agent adds one entry here and one arm in [`adapter`].
pub(crate) fn adapters() -> &'static [&'static dyn TranscriptAdapter] {
    &[&ClaudeAdapter, &CodexAdapter]
}

// ── MetricsAccumulator impls (the two accumulators are structurally identical) ──
impl MetricsAccumulator for crate::claude_metrics::MetricsAcc {
    fn push(&mut self, v: &Value) {
        crate::claude_metrics::MetricsAcc::push(self, v)
    }
    fn finish(&self) -> Metrics {
        self.clone().finish()
    }
}
impl MetricsAccumulator for crate::codex_metrics::CodexMetricsAcc {
    fn push(&mut self, v: &Value) {
        crate::codex_metrics::CodexMetricsAcc::push(self, v)
    }
    fn finish(&self) -> Metrics {
        self.clone().finish()
    }
}

/// Claude adapter — delegates to the `claude_model` / `claude_discover` implementations.
pub(crate) struct ClaudeAdapter;
impl TranscriptAdapter for ClaudeAdapter {
    fn agent(&self) -> Agent {
        Agent::Claude
    }
    fn sniff(&self, head: &Value) -> bool {
        head.get("sessionId").is_some() || head.get("message").is_some()
    }
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        crate::claude_model::enrich_tree(path, blocks)
    }
    fn shaping(&self) -> &'static Shaping {
        &crate::claude_model::CLAUDE_SHAPING
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        crate::claude_model::decode_line(line, cwd, out)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        Box::new(crate::claude_metrics::MetricsAcc::default())
    }
    fn load_attachment(&self, line: &str, index: usize) -> Option<crate::model::LoadedAttachment> {
        crate::claude_model::nth_loaded_attachment(line, index)
    }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        crate::claude_discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        crate::claude_discover::transcript_by_id(id)
    }
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        crate::claude_model::subagent_file(root, child_id)
    }
}

/// Codex adapter — delegates to the `codex_model` / `codex_discover` implementations.
pub(crate) struct CodexAdapter;
impl TranscriptAdapter for CodexAdapter {
    fn agent(&self) -> Agent {
        Agent::Codex
    }
    fn sniff(&self, head: &Value) -> bool {
        let ty = head.get("type").and_then(Value::as_str);
        ty == Some("session_meta")
            || (head.get("payload").is_some()
                && matches!(ty, Some("response_item" | "turn_context" | "event_msg")))
    }
    fn shaping(&self) -> &'static Shaping {
        &crate::codex_model::CODEX_SHAPING
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        crate::codex_model::decode_line(line, cwd, out)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        Box::new(crate::codex_metrics::CodexMetricsAcc::default())
    }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        crate::codex_discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        crate::codex_discover::resolve(Some(id), false).ok()
    }
}
