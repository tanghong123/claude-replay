//! The three built-in adapters — each an `impl TranscriptAdapter` over its family's
//! `model`/`metrics`/`discover` modules — plus [`REGISTRY`], the slice a facade (or a
//! third party composing its own) hands to the engine's dispatching entry points.

use crate::agents;
use claude_replay_engine::adapter::{MetricsAccumulator, SniffClaim, TranscriptAdapter};
use claude_replay_engine::discover::Candidate;
use claude_replay_engine::metrics::Metrics;
use claude_replay_engine::model::Block;
use claude_replay_engine::seam::{Message, Shaping};
use claude_replay_engine::Agent;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Every built-in adapter, in the stable order detection iterates.
pub static REGISTRY: &[&'static dyn TranscriptAdapter] =
    &[&ClaudeAdapter, &CodexAdapter, &QoderWorkAdapter];

impl MetricsAccumulator for agents::claude::metrics::MetricsAcc {
    fn push(&mut self, v: &Value) {
        agents::claude::metrics::MetricsAcc::push(self, v)
    }
    fn finish(&self) -> Metrics {
        self.clone().finish()
    }
}
impl MetricsAccumulator for agents::codex::metrics::CodexMetricsAcc {
    fn push(&mut self, v: &Value) {
        agents::codex::metrics::CodexMetricsAcc::push(self, v)
    }
    fn finish(&self) -> Metrics {
        self.clone().finish()
    }
}

/// Claude adapter — delegates to the `claude_model` / `claude_discover` implementations.
pub struct ClaudeAdapter;
impl TranscriptAdapter for ClaudeAdapter {
    fn agent(&self) -> Agent {
        Agent::CLAUDE
    }
    fn sniff(&self, head: &Value) -> SniffClaim {
        // Claude-format lines (sessionId/message) are only a CAN-PARSE claim: derived
        // agents (QoderWork) share the body format and OWN their distinctive heads,
        // which outranks this — no per-agent carve-outs needed here (#59).
        if head.get("sessionId").is_some() || head.get("message").is_some() {
            SniffClaim::CanParse
        } else {
            SniffClaim::No
        }
    }
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        agents::claude::model::enrich_tree(path, blocks)
    }
    fn shaping(&self) -> &'static Shaping {
        &agents::claude::model::CLAUDE_SHAPING
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::claude::model::decode_line(line, cwd, out)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        Box::new(agents::claude::metrics::MetricsAcc::default())
    }
    fn load_attachment(
        &self,
        line: &str,
        index: usize,
    ) -> Option<claude_replay_engine::model::LoadedAttachment> {
        agents::claude::model::nth_loaded_attachment(line, index)
    }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        agents::claude::discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        agents::claude::discover::transcript_by_id(id)
    }
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        agents::claude::model::subagent_file(root, child_id)
    }
    fn load_tasks(&self, path: &Path) -> Option<claude_replay_engine::engine::tasks::TaskList> {
        agents::claude::discover::load_tasks(path)
    }
    fn store_contains(&self, path: &Path) -> bool {
        path.starts_with(agents::claude::discover::projects_dir())
    }
}

/// Codex adapter — delegates to the `codex_model` / `codex_discover` implementations.
pub struct CodexAdapter;
impl TranscriptAdapter for CodexAdapter {
    fn agent(&self) -> Agent {
        Agent::CODEX
    }
    fn sniff(&self, head: &Value) -> SniffClaim {
        let ty = head.get("type").and_then(Value::as_str);
        let hit = ty == Some("session_meta")
            || (head.get("payload").is_some()
                && matches!(ty, Some("response_item" | "turn_context" | "event_msg")));
        if hit {
            SniffClaim::Owns // Codex's rollout shapes are distinctive to Codex
        } else {
            SniffClaim::No
        }
    }
    fn shaping(&self) -> &'static Shaping {
        &agents::codex::model::CODEX_SHAPING
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::codex::model::decode_line(line, cwd, out)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        Box::new(agents::codex::metrics::CodexMetricsAcc::default())
    }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        agents::codex::discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        agents::codex::discover::resolve(Some(id), false).ok()
    }
}

/// QoderWork adapter — a Claude-Code-format client with its own store and a `runtime-config`
/// head line. Everything format-level DELEGATES to the Claude implementations (tokenizer,
/// shaping, metrics, enrichment, attachments, sub-agent layout — the transcripts are
/// Claude-shaped, and the foreign `runtime-config`/unknown lines fall through the Claude
/// decoder as no-ops); only detection and the store root differ. Note: QoderWork records no
/// per-line token usage, so metrics honestly fold to zero tokens/cost.
pub struct QoderWorkAdapter;
impl TranscriptAdapter for QoderWorkAdapter {
    fn agent(&self) -> Agent {
        Agent::QODERWORK
    }
    fn sniff(&self, head: &Value) -> SniffClaim {
        if head.get("type").and_then(Value::as_str) == Some("runtime-config") {
            SniffClaim::Owns // the runtime-config head is QoderWork's signature
        } else {
            SniffClaim::No
        }
    }
    fn store_contains(&self, path: &Path) -> bool {
        path.starts_with(agents::qoderwork::discover::projects_dir())
    }
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        agents::claude::model::enrich_tree(path, blocks)
    }
    fn shaping(&self) -> &'static Shaping {
        &agents::claude::model::CLAUDE_SHAPING
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::claude::model::decode_line(line, cwd, out)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        Box::new(agents::claude::metrics::MetricsAcc::default())
    }
    fn load_attachment(
        &self,
        line: &str,
        index: usize,
    ) -> Option<claude_replay_engine::model::LoadedAttachment> {
        agents::claude::model::nth_loaded_attachment(line, index)
    }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        agents::qoderwork::discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        agents::qoderwork::discover::transcript_by_id(id)
    }
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        agents::claude::model::subagent_file(root, child_id)
    }
}
