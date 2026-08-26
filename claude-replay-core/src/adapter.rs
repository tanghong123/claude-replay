//! **The wired registry** (#87 step 3): the facade's join of the agent-free machinery
//! (`claude-replay-engine`) with the agent implementations (`claude-replay-agents`).
//! [`adapter`]/[`adapters`] answer "which adapter handles this agent id" for every
//! dispatching entry point in this crate. A third party building on the engine crate
//! passes its own slice instead — this is OUR curry, not a global.

use crate::Agent;
pub use claude_replay_engine::adapter::{
    LinePreprocessor, MetricsAccumulator, PreprocessedLine, SniffClaim, SpawnRoster,
    TranscriptAdapter,
};
pub use claude_replay_engine::metrics_fold::{
    CursorReject, FoldStart, MetricsCursor, MetricsEvent, MetricsFold,
};

/// The adapter for `agent` — a scan of [`adapters`], so the registry row is the ONE
/// place an agent is wired. An id with no registered adapter is a programming error:
/// ids in circulation come from detection (registry-derived), the built-in constants,
/// or a sidecar label that already round-tripped through [`Agent::from_label`].
pub fn adapter(agent: Agent) -> &'static dyn TranscriptAdapter {
    adapters()
        .iter()
        .copied()
        .find(|a| a.agent() == agent)
        .unwrap_or_else(|| panic!("no adapter registered for agent {:?}", agent.label()))
}

/// Every registered adapter, in a stable order (drives `detect_agent` iteration and the
/// cross-agent picker order).
pub fn adapters() -> &'static [&'static dyn TranscriptAdapter] {
    claude_replay_agents::REGISTRY
}

/// Open a resumable metrics fold for `agent`'s transcript at `path` (#14) — the facade curry of
/// [`MetricsFold::open`] over [`adapter`], for consumers that hold an [`Agent`] rather than an
/// adapter. `cursor` is a value a previous run's [`MetricsFold::cursor`] produced (stored
/// anywhere, interpreted nowhere); an absent or no-longer-valid one folds cold from byte 0, and
/// [`MetricsFold::start`] says which happened.
pub fn metrics_fold(
    agent: Agent,
    path: &std::path::Path,
    cursor: Option<&MetricsCursor>,
) -> std::io::Result<MetricsFold> {
    MetricsFold::open(adapter(agent), path, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #15: the types [`TranscriptAdapter`]'s own contract hands back must be nameable from
    /// core — a consumer gating lines on the preprocessor must not need a direct engine
    /// dependency just to write the `matches!`.
    #[test]
    fn preprocessor_vocabulary_is_reachable_from_core() {
        fn _takes(p: &mut dyn LinePreprocessor) -> bool {
            matches!(p.process("{}"), PreprocessedLine::Include)
        }
        let _named: fn(&mut dyn LinePreprocessor) -> bool = _takes;
    }
}
