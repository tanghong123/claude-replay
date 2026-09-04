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

/// Every built-in adapter, in the stable order detection iterates. Qoder and QoderWork
/// write IN-BAND IDENTICAL transcripts (both open with the same keyed `runtime-config`
/// head — verified against real stores, #20), so no sniff can tell them apart: Qoder is
/// attributed by store provenance (`store_contains`, consulted before any sniff), and the
/// shared head stays QoderWork's sniff signature for out-of-store files.
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
    fn bump_extra(&mut self, key: &str, n: u64) {
        self.bump(key, n)
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
    /// Overridden (not the totals-only default) so the repeat-collapsing guard survives a
    /// mid-file resume — see `MetricsAcc::state`.
    fn state(&self) -> Value {
        agents::claude::metrics::MetricsAcc::state(self)
    }
    fn restore(&mut self, state: &Value) {
        agents::claude::metrics::MetricsAcc::restore(self, state)
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
    fn bump_extra(&mut self, key: &str, n: u64) {
        self.bump(key, n)
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
    fn store_subagent_transcripts(&self) -> Vec<(std::path::PathBuf, String, String)> {
        crate::agents::claude::discover::subagent_transcripts_in(
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
    fn elision(&self) -> claude_replay_engine::seam::Elision {
        agents::claude::model::CLAUDE_ELISION
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::claude::model::decode_line(line, cwd, out)
    }
    fn tool_is_interactive(&self, name: &str) -> bool {
        agents::claude::model::tool_is_interactive(name)
    }
    fn turn_ended(&self, raw_line: &str) -> Option<bool> {
        agents::claude::model::turn_ended(raw_line)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        // What Claude Code's transcript records of the runtime snapshot (#62): the reasoning
        // effort, the permission mode and the client version — no sandbox, no context window.
        Box::new(agents::claude::metrics::MetricsAcc::recording(&[
            "effort",
            "permission",
            "client",
        ]))
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
    // One `Workflow` call launches a fleet the transcript never names (#38).
    fn spawn_run(&self, b: &claude_replay_engine::model::Block) -> Option<String> {
        agents::claude::model::workflow_run(b)
    }
    fn spawn_rosters(&self, path: &Path) -> Vec<claude_replay_engine::seam::SpawnRoster> {
        agents::claude::model::workflow_rosters(path)
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
    fn turn_ended(&self, raw_line: &str) -> Option<bool> {
        agents::codex::model::turn_ended(raw_line)
    }
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        agents::codex::model::enrich_tree(path, blocks)
    }
    fn shaping(&self) -> &'static Shaping {
        &agents::codex::model::CODEX_SHAPING
    }
    fn elision(&self) -> claude_replay_engine::seam::Elision {
        agents::codex::model::CODEX_ELISION
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::codex::model::decode_line(line, cwd, out)
    }
    fn line_preprocessor(&self) -> Box<dyn claude_replay_engine::adapter::LinePreprocessor> {
        Box::new(agents::codex::model::CodexLinePreprocessor::default())
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        // Codex's `turn_context` / `thread_settings_applied` / token-count events record the
        // whole snapshot but the client version (#62).
        Box::new(agents::codex::metrics::CodexMetricsAcc::recording(&[
            "context",
            "effort",
            "mode",
            "sandbox",
            "approvals",
            "permission",
            "tier",
            "plan",
        ]))
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
    fn session_card(
        &self,
        path: &Path,
        memo: Option<&claude_replay_engine::seam::CardMemo>,
    ) -> claude_replay_engine::seam::CardOutcome {
        agents::codex::discover::session_card(path, memo)
    }
}

/// Qoder CLI adapter — a Claude-Code-format terminal agent with its own store
/// (`~/.qoder/projects`). Everything format-level DELEGATES to the Claude
/// implementations (tokenizer, shaping, metrics — whose shared usage fold also reads
/// Qoder's `usage.credits` — enrichment, attachments, the `subagents/agent-<id>.jsonl`
/// sub-agent layout); only the store roots differ.
pub struct QoderAdapter;
impl TranscriptAdapter for QoderAdapter {
    fn store_transcripts(&self) -> Vec<std::path::PathBuf> {
        crate::agents::qoder::discover::store_transcripts()
    }
    fn store_subagent_transcripts(&self) -> Vec<(std::path::PathBuf, String, String)> {
        crate::agents::claude::discover::subagent_transcripts_in(
            &crate::agents::qoder::discover::projects_dir(),
        )
    }

    fn agent(&self) -> Agent {
        Agent::QODER
    }
    /// Never a claim: Qoder's transcripts are IN-BAND IDENTICAL to QoderWork's — real
    /// QoderWork stores write the same keyed `runtime-config` head (`reasoningEffort`/
    /// `contextWindow` present-and-null), so no head shape is distinctively Qoder's.
    /// A `~/.qoder/projects` session is attributed by store PROVENANCE instead
    /// (`store_contains`, which detection consults before any sniff); an out-of-store
    /// file with the shared head honestly labels as QoderWork.
    fn sniff(&self, _head: &Value) -> SniffClaim {
        SniffClaim::No
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
    fn elision(&self) -> claude_replay_engine::seam::Elision {
        agents::claude::model::CLAUDE_ELISION
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::claude::model::decode_line(line, cwd, out)
    }
    // Claude Code's tool vocabulary, so Claude's interactive set (#21).
    fn tool_is_interactive(&self, name: &str) -> bool {
        agents::claude::model::tool_is_interactive(name)
    }
    // …and Claude's turn-lifecycle vocabulary (#194), for the same reason.
    fn turn_ended(&self, raw_line: &str) -> Option<bool> {
        agents::claude::model::turn_ended(raw_line)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        // The family's `runtime-config` head carries `reasoningEffort`/`contextWindow` —
        // present-and-null in real stores, which is exactly "recorded, unknown" (#62).
        Box::new(agents::claude::metrics::MetricsAcc::recording(&[
            "context", "effort",
        ]))
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
    fn load_tasks(&self, path: &Path) -> Option<claude_replay_engine::engine::tasks::TaskList> {
        agents::qoder::discover::load_tasks(path)
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
/// decoder as no-ops); only detection and the store root differ. Historical QoderWork sessions
/// may carry no usage at all; current sessions can report zeroed token counts plus
/// `usage.credits`, which the shared metrics fold converts to cost just like Qoder CLI.
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
        // The `runtime-config` head is the qwork-family signature. Qoder CLI writes the
        // SAME head (both keyed with `reasoningEffort`/`contextWindow` — verified against
        // real stores, #20), so ownership of the shared shape stays here and Qoder is
        // told apart by store provenance, which detection consults before any sniff.
        if head.get("type").and_then(Value::as_str) == Some("runtime-config") {
            SniffClaim::Owns
        } else {
            SniffClaim::No
        }
    }
    fn store_contains(&self, path: &Path) -> bool {
        path.starts_with(agents::qoderwork::discover::projects_dir())
    }
    /// The one place QoderWork cannot simply delegate: it keeps the spawn→child relation in
    /// SIDECARS rather than inline in the transcript, so the ids are adopted from those
    /// first — after which the shared Claude pass (which resolves children by exactly those
    /// ids) does the rest against an identical `subagents/` layout.
    fn enrich(&self, path: &Path, blocks: &mut [Block]) {
        agents::claude::model::enrich_tree(path, blocks)
    }
    // A running spawn is nameless in the transcript; its id sits in a sidecar (#37).
    fn spawn_links(&self, path: &Path) -> Vec<claude_replay_engine::seam::SpawnLink> {
        agents::qoderwork::discover::spawn_links(path)
    }
    fn shaping(&self) -> &'static Shaping {
        &agents::claude::model::CLAUDE_SHAPING
    }
    fn elision(&self) -> claude_replay_engine::seam::Elision {
        agents::claude::model::CLAUDE_ELISION
    }
    fn decode_line(&self, line: &str, cwd: &mut String, out: &mut Vec<Message>) {
        agents::claude::model::decode_line(line, cwd, out)
    }
    // Claude Code's tool vocabulary, so Claude's interactive set (#21).
    fn tool_is_interactive(&self, name: &str) -> bool {
        agents::claude::model::tool_is_interactive(name)
    }
    // …and Claude's turn-lifecycle vocabulary (#194), for the same reason.
    fn turn_ended(&self, raw_line: &str) -> Option<bool> {
        agents::claude::model::turn_ended(raw_line)
    }
    fn metrics_acc(&self) -> Box<dyn MetricsAccumulator> {
        // The family's `runtime-config` head carries `reasoningEffort`/`contextWindow` —
        // present-and-null in real stores, which is exactly "recorded, unknown" (#62).
        Box::new(agents::claude::metrics::MetricsAcc::recording(&[
            "context", "effort",
        ]))
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

    /// EVERY `runtime-config` head — keyed or key-less — is owned by QoderWork ALONE at the
    /// sniff level. The keyed variant is NOT Qoder's signature: a real, current QoderWork
    /// store writes `reasoningEffort`/`contextWindow` too (the first head here is verbatim
    /// from one, id redacted — 72/72 heads on that store were keyed), so key presence
    /// cannot discriminate and Qoder is attributed by store provenance instead
    /// (`detect_agent_claimed` consults `store_contains` before sniffing).
    #[test]
    fn every_runtime_config_head_is_qoderworks_at_the_sniff_level() {
        let real_qoderwork = r#"{"type":"runtime-config","sessionId":"REDACTED","model":"","reasoningEffort":null,"contextWindow":null,"timestamp":1784282861519}"#;
        let keyless = r#"{"type":"runtime-config","sessionId":"s","model":"qwork-ultimate","timestamp":1785068132048}"#;
        for head in [real_qoderwork, keyless] {
            let owners: Vec<Agent> = claims(head)
                .into_iter()
                .filter(|(_, c)| matches!(c, SniffClaim::Owns))
                .map(|(a, _)| a)
                .collect();
            assert_eq!(
                owners,
                vec![Agent::QODERWORK],
                "the shared head must never flip a QoderWork session's identity: {head}"
            );
        }
    }

    /// #21: every Claude-Code-format adapter answers `true` for the human-blocking tools
    /// (one shared vocabulary, one shared list), Codex stays on the default until its
    /// equivalents are identified, and ordinary work tools are `false` everywhere — the
    /// consumer's hardcoded name list moves behind the seam.
    #[test]
    fn interactive_tools_are_declared_by_the_adapter() {
        for a in REGISTRY {
            let expect_claude_family = a.agent() != Agent::CODEX;
            for tool in ["AskUserQuestion", "ExitPlanMode"] {
                assert_eq!(
                    a.tool_is_interactive(tool),
                    expect_claude_family,
                    "{:?} / {tool}",
                    a.agent()
                );
            }
            for tool in ["Bash", "Edit", "Read", "Agent", ""] {
                assert!(!a.tool_is_interactive(tool), "{:?} / {tool:?}", a.agent());
            }
        }
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
