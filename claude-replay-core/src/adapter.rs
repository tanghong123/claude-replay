//! **The per-agent seam** — one trait, one registry.
//!
//! Everything that varies by agent lives behind [`TranscriptAdapter`]: the Layer-1
//! tokenizer + its `Shaping` hooks, the metrics accumulator, transcript detection,
//! discovery, and relationship-graph construction. The rest of the engine (the
//! [`Replayer`](crate::engine::replay::Replayer) fold, `Session`, the parse dispatchers,
//! the live follower) is agent-agnostic and reaches the per-agent behavior only through
//! [`adapter`]. Adding an agent is therefore one `impl
//! TranscriptAdapter` + one row in [`adapter`]/[`adapters`] — no `match agent` scattered
//! across the codebase. (This mirrors `jdi::agent::adapter` on the supervisor side.)

use crate::discover::Candidate;
use crate::engine::message::Message;
use crate::engine::replay::Shaping;
use crate::metrics::Metrics;
use crate::model::Block;
use crate::{Agent, SessionGraph};
use serde_json::Value;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// A per-agent token/cost accumulator, folded one transcript line at a time. Object-safe
/// (`Box<dyn>`) so the live follower can hold one without knowing the agent; `Send` so the
/// follower can move between threads (the HTML live server tails on a background thread).
pub(crate) trait MetricsAccumulator: Send {
    /// Fold one raw transcript line's usage into the running total.
    fn push(&mut self, v: &Value);
    /// The metrics so far, without consuming the accumulator (for a live snapshot).
    fn finish(&self) -> Metrics;
}

/// The single agent-specific interface. A new agent implements this once; the engine calls
/// it via [`adapter`]. The four per-agent hooks (`sniff`/`scan_join_ids`/`decode_line`/
/// `metrics_acc` + the `shaping` const) drive both the whole-file `parse_path_timed` and the
/// live follower through one provided path, so batch and live share a seam. Discovery
/// (`candidates_scoped`/`resolve_id`) and operation-scoped [`SessionGraph`] construction
/// round it out.
pub(crate) trait TranscriptAdapter: Sync {
    /// Which agent this adapter handles.
    fn agent(&self) -> Agent;

    /// Does a transcript whose head parses to `head` look like this agent's format?
    /// (Used by [`crate::discover::detect_agent`]; the sniffs are mutually exclusive.)
    fn sniff(&self, head: &Value) -> bool;

    // ── whole-file parse ──
    /// Blocks + one timestamp per user turn + folded metrics, in one streaming pass — the
    /// single whole-file parse seam (the flat source; relationship resolution is applied
    /// afterward by an operation-scoped [`SessionGraph`]). A provided method: pass-1
    /// [`scan_join_ids`](Self::scan_join_ids), then pass-2 folds each line through
    /// [`decode_line`](Self::decode_line) + [`shaping`](Self::shaping) with metrics
    /// accumulated by [`metrics_acc`](Self::metrics_acc). Identical orchestration for every
    /// agent — no adapter overrides it; a new agent supplies only the four hooks.
    fn parse_path_timed(
        &self,
        path: &Path,
        times: &mut Vec<Option<f64>>,
    ) -> io::Result<(Vec<Block>, Metrics)> {
        let ids = self.scan_join_ids(path)?; // pass 1
        let mut cwd = String::new();
        let mut macc = self.metrics_acc();
        let reader = io::BufReader::new(std::fs::File::open(path)?); // pass 2 (fresh read)
        let blocks = crate::engine::replay::parse_stream(
            reader,
            ids,
            self.shaping(),
            |line, out| self.decode_line(line, &mut cwd, out),
            |v| macc.push(v),
            times,
        )?;
        Ok((blocks, macc.finish()))
    }
    /// Pass-1 id pre-scan: the tool-call ids a later result will be joined onto (so a
    /// forward-referencing result isn't mis-emitted as an orphan). Per-agent because the id
    /// lives at a different JSON path in each format; the shared line-streaming skeleton is
    /// [`scan_ids`](crate::engine::replay::scan_ids). Streams the file — no whole-file buffer.
    fn scan_join_ids(&self, path: &Path) -> io::Result<HashSet<String>>;
    /// Metrics only, from a reader. A provided method: fold every line through a fresh
    /// [`MetricsAccumulator`] — identical for every agent, so no adapter overrides it.
    #[cfg(test)]
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

    // ── discovery ──
    /// Sessions for `cwd` (or its nearest ancestor with sessions), as picker candidates.
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate>;
    /// Resolve a bare session id to its transcript path in this agent's store.
    fn resolve_id(&self, id: &str) -> Option<PathBuf>;
    /// Create an operation-scoped relationship resolver anchored at `anchor`.
    fn session_graph(&self, anchor: &Path) -> SessionGraph;
    /// Resolve a child through a short-lived operation graph. Presentation paths
    /// that traverse more than one node share a graph directly instead.
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        self.session_graph(root).subagent_source(root, child_id)
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
    fn scan_join_ids(&self, path: &Path) -> io::Result<HashSet<String>> {
        use std::io::BufRead;
        let f = io::BufReader::new(std::fs::File::open(path)?);
        Ok(crate::claude_model::scan_join_ids(
            f.lines().map_while(Result::ok),
        ))
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
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        crate::claude_discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        crate::claude_discover::transcript_by_id(id)
    }
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        crate::claude_model::subagent_file(root, child_id)
    }
    fn session_graph(&self, _anchor: &Path) -> SessionGraph {
        SessionGraph::from_backend(Box::new(crate::claude_discover::ClaudeSessionGraph))
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
    fn scan_join_ids(&self, path: &Path) -> io::Result<HashSet<String>> {
        use std::io::BufRead;
        let f = io::BufReader::new(std::fs::File::open(path)?);
        Ok(crate::codex_model::scan_join_ids(
            f.lines().map_while(Result::ok),
        ))
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
    fn session_graph(&self, anchor: &Path) -> SessionGraph {
        SessionGraph::from_backend(Box::new(crate::codex_discover::CodexSessionGraph::open(
            anchor,
        )))
    }
}
