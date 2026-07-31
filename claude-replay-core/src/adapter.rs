//! **The wired registry** (#87 step 3): the facade's join of the agent-free machinery
//! (`claude-replay-engine`) with the agent implementations (`claude-replay-agents`).
//! [`adapter`]/[`adapters`] answer "which adapter handles this agent id" for every
//! dispatching entry point in this crate. A third party building on the engine crate
//! passes its own slice instead — this is OUR curry, not a global.

use crate::Agent;
pub use claude_replay_engine::adapter::{MetricsAccumulator, SniffClaim, TranscriptAdapter};

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
