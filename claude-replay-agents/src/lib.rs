//! **claude-replay-agents** — the built-in agent implementations (#87 step 3): the
//! Claude, Codex, and QoderWork `model`/`metrics`/`discover` families, their
//! `TranscriptAdapter` impls, and the [`REGISTRY`] slice a facade hands to the
//! engine's dispatching entry points.
//!
//! The crate boundary IS the contract: family code reaches the engine only through
//! `claude_replay_engine::seam` (audited by `agents_import_only_the_seam` in
//! `agents/mod.rs`) — adding a capability to an adapter means adding it to the seam
//! deliberately, never reaching into machinery internals.

pub mod adapters;
pub(crate) mod agents;

pub use adapters::{ClaudeAdapter, CodexAdapter, QoderWorkAdapter, REGISTRY};
// The per-agent discovery modules keep their long-standing public paths (the facade
// re-exports them as `claude_discover`/`codex_discover`/`qoderwork_discover`).
pub use agents::claude::discover as claude_discover;
pub use agents::codex::discover as codex_discover;
pub use agents::qoderwork::discover as qoderwork_discover;
