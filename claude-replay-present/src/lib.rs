//! **claude-replay-present** — the presentation-support layer between the parser core and
//! the actual frontends (#71). What lives here is everything that makes *building* a
//! presentation easier without being one: the session residency [`cache`] (registry + TTL
//! reaping + the pull-servable [`cache::SharedSession`]), the shared plain-text formatters
//! ([`present`]), the syntect [`highlight`]er (returns toolkit-neutral `HlSpan`s — text +
//! xterm-256 fg index — the shared span vocabulary both frontends map onto their own output
//! backend), small OS/path helpers ([`sys`]), and the viewer options type ([`Args`],
//! clap-free unless the `cli` feature is on).

pub mod args;
pub mod cache;
pub mod highlight;
pub mod present;
pub mod pull;
pub mod sys;

pub use args::Args;
pub use cache::SessionCache;

// Alias the core's modules at this crate's root so the moved modules keep referring to
// `crate::engine`, `crate::model`, … unchanged (the same transparency trick the original
// core split used).
pub(crate) use claude_replay_core::{engine, follow, metrics, model, summary, Agent, Transcript};
