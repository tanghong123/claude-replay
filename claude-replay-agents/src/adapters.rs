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

/// Every built-in adapter, in the stable order detection iterates. Qoder sits ahead of
/// QoderWork because both stores share the `runtime-config` head shape: each owns only its
/// own variant (the key probe below), but the order keeps detection deterministic even if
/// a future head matched both.
pub static REGISTRY: &[&'static dyn TranscriptAdapter] = &[
    &ClaudeAdapter,
    &CodexAdapter,
    &QoderAdapter,
    &QoderWorkAdapter,
];

impl MetricsAccumulator for agents::claude::metrics::MetricsAcc {
    fn push(&mut self, v: &Value) {
        agents::claude::metrics::MetricsAcc::push(self, v)
    }
    fn finish(&self) -> Metrics {
        self.clone().finish()
    }
    /// #96 §7: the resumable form. Both agents hold the same shape — per-model counters, an
    /// `extra` bag and a span — so the seam is agent-agnostic even though the two REPORT
    /// differently (Claude increments, Codex running totals), which each `push` normalises.
    fn totals(&self) -> claude_replay_engine::seam::MetricsTotals {
        agents::claude::metrics::MetricsAcc::totals(self)
    }
    fn reseed(
        &mut self,
        tokens: std::collections::BTreeMap<String, claude_replay_engine::seam::TokenCounts>,
        extra: std::collections::BTreeMap<String, u64>,
        span: Option<(f64, f64)>,
    ) {
        agents::claude::metrics::MetricsAcc::reseed(self, tokens, extra, span)
    }
}
impl MetricsAccumulator for agents::codex::metrics::CodexMetricsAcc {
    fn push(&mut self, v: &Value) {
        agents::codex::metrics::CodexMetricsAcc::push(self, v)
    }
    fn malformed_line(&mut self) {
        agents::codex::metrics::CodexMetricsAcc::malformed_line(self)
    }
    fn state(&self) -> Value {
        agents::codex::metrics::CodexMetricsAcc::state(self)
    }
    fn restore(&mut self, state: &Value) {
        agents::codex::metrics::CodexMetricsAcc::restore(self, state)
    }
    fn finish(&self) -> Metrics {
        self.clone().finish()
    }
    /// #96 §7: the resumable form. Both agents hold the same shape — per-model counters, an
    /// `extra` bag and a span — so the seam is agent-agnostic even though the two REPORT
    /// differently (Claude increments, Codex running totals), which each `push` normalises.
    fn totals(&self) -> claude_replay_engine::seam::MetricsTotals {
        agents::codex::metrics::CodexMetricsAcc::totals(self)
    }
    fn reseed(
        &mut self,
        tokens: std::collections::BTreeMap<String, claude_replay_engine::seam::TokenCounts>,
        extra: std::collections::BTreeMap<String, u64>,
        span: Option<(f64, f64)>,
    ) {
        agents::codex::metrics::CodexMetricsAcc::reseed(self, tokens, extra, span)
    }
}

/// Claude adapter — delegates to the `claude_model` / `claude_discover` implementations.
pub struct ClaudeAdapter;
impl TranscriptAdapter for ClaudeAdapter {
    fn store_transcripts(&self) -> Vec<std::path::PathBuf> {
        crate::agents::claude::discover::store_transcripts_in(
            &crate::agents::claude::discover::projects_dir(),
        )
    }

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
    fn session_card(
        &self,
        path: &Path,
        memo: Option<&claude_replay_engine::seam::CardMemo>,
    ) -> claude_replay_engine::seam::CardOutcome {
        agents::claude::discover::session_card(path, memo)
    }
    fn store_contains(&self, path: &Path) -> bool {
        path.starts_with(agents::claude::discover::projects_dir())
    }
}

/// Codex adapter — delegates to the `codex_model` / `codex_discover` implementations.
pub struct CodexAdapter;
impl TranscriptAdapter for CodexAdapter {
    fn store_transcripts(&self) -> Vec<std::path::PathBuf> {
        crate::agents::codex::discover::store_transcripts_machine()
    }
    fn store_subagent_transcripts(&self) -> Vec<(std::path::PathBuf, String, String)> {
        crate::agents::codex::discover::subagent_transcripts_machine()
    }

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
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        agents::codex::model::enrich_tree(path, blocks)
    }
    fn shaping(&self) -> &'static Shaping {
        &agents::codex::model::CODEX_SHAPING
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::codex::model::decode_line(line, cwd, out)
    }
    fn line_preprocessor(&self) -> Box<dyn claude_replay_engine::adapter::LinePreprocessor> {
        Box::new(agents::codex::model::CodexLinePreprocessor::default())
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        Box::new(agents::codex::metrics::CodexMetricsAcc::default())
    }
    fn load_attachment(
        &self,
        line: &str,
        index: usize,
    ) -> Option<claude_replay_engine::model::LoadedAttachment> {
        agents::codex::model::nth_loaded_attachment(line, index)
    }
    fn candidates_scoped(&self, cwd: &Path) -> Vec<Candidate> {
        agents::codex::discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        agents::codex::discover::resolve(Some(id), false).ok()
    }
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        agents::codex::discover::subagent_source(root, child_id)
    }
    fn subagent_sources(&self, root: &Path, ids: &[&str]) -> Vec<Option<PathBuf>> {
        agents::codex::discover::subagent_sources(root, ids)
    }
}

/// Whether a `runtime-config` head is Qoder CLI's variant: it carries the
/// `reasoningEffort`/`contextWindow` keys (present even when null), which QoderWork's
/// head never writes. Key PRESENCE is the probe — the values are usually null.
fn is_qoder_runtime_config(head: &Value) -> bool {
    head.get("reasoningEffort").is_some() || head.get("contextWindow").is_some()
}

/// Qoder CLI adapter — a Claude-Code-format terminal agent with its own store
/// (`~/.qoder/projects`) and a `runtime-config` head carrying `reasoningEffort`/
/// `contextWindow`. Everything format-level DELEGATES to the Claude implementations
/// (tokenizer, shaping, metrics — whose shared usage fold also reads Qoder's
/// `usage.credits` — enrichment, attachments, the `subagents/agent-<id>.jsonl`
/// sub-agent layout); only detection and the store root differ.
pub struct QoderAdapter;
impl TranscriptAdapter for QoderAdapter {
    fn store_transcripts(&self) -> Vec<std::path::PathBuf> {
        crate::agents::qoder::discover::store_transcripts()
    }

    fn agent(&self) -> Agent {
        Agent::QODER
    }
    fn sniff(&self, head: &Value) -> SniffClaim {
        if head.get("type").and_then(Value::as_str) == Some("runtime-config")
            && is_qoder_runtime_config(head)
        {
            SniffClaim::Owns // the keyed runtime-config head is Qoder CLI's signature
        } else {
            SniffClaim::No
        }
    }
    fn store_contains(&self, path: &Path) -> bool {
        path.starts_with(agents::qoder::discover::projects_dir())
    }
    /// Claude's enrichment over EVERY candidate `subagents/` dir: a mid-session `cwd`
    /// change files the companion dir under the new cwd's slug, away from the transcript
    /// (see `qoder::discover::subagents_dirs`). Passes compose — a child one dir can't
    /// resolve is left for the next.
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        for dir in agents::qoder::discover::subagents_dirs(path) {
            agents::claude::model::enrich_tree_in(&dir, blocks);
        }
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
        agents::qoder::discover::candidates_scoped(cwd)
    }
    fn resolve_id(&self, id: &str) -> Option<PathBuf> {
        agents::qoder::discover::transcript_by_id(id)
    }
    fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        agents::qoder::discover::subagent_file(root, child_id)
    }
    fn session_card(
        &self,
        path: &Path,
        memo: Option<&claude_replay_engine::seam::CardMemo>,
    ) -> claude_replay_engine::seam::CardOutcome {
        agents::qoder::discover::session_card(path, memo)
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
    fn fork_origin(&self, path: &Path) -> Option<String> {
        agents::qoderwork::discover::fork_origin(path)
    }
    fn store_transcripts(&self) -> Vec<std::path::PathBuf> {
        crate::agents::qoderwork::discover::store_transcripts()
    }

    fn agent(&self) -> Agent {
        Agent::QODERWORK
    }

    /// QoderWork is a DESKTOP-collaboration agent: its sessions' cwd is usually `$HOME` or
    /// nothing meaningful, so a monitor groups them under the agent, not a project (#98 §4.2).
    fn workspace_anchored(&self) -> bool {
        false
    }
    fn sniff(&self, head: &Value) -> SniffClaim {
        // QoderWork's signature MINUS Qoder's: both stores open with a `runtime-config`
        // head, but only Qoder CLI's carries the `reasoningEffort`/`contextWindow` keys —
        // so each variant is owned by exactly one adapter, order-independently.
        if head.get("type").and_then(Value::as_str) == Some("runtime-config")
            && !is_qoder_runtime_config(head)
        {
            SniffClaim::Owns // the key-less runtime-config head is QoderWork's signature
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
    fn session_card(
        &self,
        path: &Path,
        memo: Option<&claude_replay_engine::seam::CardMemo>,
    ) -> claude_replay_engine::seam::CardOutcome {
        agents::qoderwork::discover::session_card(path, memo)
    }
}

#[cfg(test)]
mod sniff_tests {
    use super::*;

    fn claims(head: &str) -> Vec<(Agent, SniffClaim)> {
        let v: Value = serde_json::from_str(head).unwrap();
        REGISTRY.iter().map(|a| (a.agent(), a.sniff(&v))).collect()
    }

    /// The three-way disambiguation on the shared `runtime-config` head shape: Qoder CLI's
    /// variant (keys present, even when null) is owned by Qoder ALONE, QoderWork's key-less
    /// variant by QoderWork ALONE — order-independent, so registry reshuffles can't flip a
    /// store's identity.
    #[test]
    fn runtime_config_heads_are_owned_by_exactly_one_adapter() {
        let qoder = r#"{"type":"runtime-config","sessionId":"s","model":"cmodel","reasoningEffort":null,"contextWindow":null,"timestamp":1786606218598}"#;
        let owners: Vec<Agent> = claims(qoder)
            .into_iter()
            .filter(|(_, c)| matches!(c, SniffClaim::Owns))
            .map(|(a, _)| a)
            .collect();
        assert_eq!(
            owners,
            vec![Agent::QODER],
            "the keyed head is Qoder's alone"
        );

        let qoderwork = r#"{"type":"runtime-config","sessionId":"s","model":"qwork-ultimate","timestamp":1785068132048}"#;
        let owners: Vec<Agent> = claims(qoderwork)
            .into_iter()
            .filter(|(_, c)| matches!(c, SniffClaim::Owns))
            .map(|(a, _)| a)
            .collect();
        assert_eq!(
            owners,
            vec![Agent::QODERWORK],
            "the key-less head stays QoderWork's alone"
        );
    }

    /// A plain Claude conversation line stays a CAN-PARSE claim for Claude and NOTHING for
    /// the derived agents — untouched by the Qoder addition.
    #[test]
    fn claude_lines_are_untouched_by_the_qoder_adapter() {
        let line = r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"hi"}}"#;
        for (agent, claim) in claims(line) {
            match agent {
                Agent::CLAUDE => assert!(matches!(claim, SniffClaim::CanParse)),
                _ => assert!(matches!(claim, SniffClaim::No), "{agent:?} must not claim"),
            }
        }
    }
}
