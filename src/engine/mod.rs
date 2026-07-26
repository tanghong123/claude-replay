//! The session engine — the agent-agnostic core of the three-layer parse · replay ·
//! present pipeline (see `design/parser-engine.md`).
//!
//! Phase 0 seeds this module with the pure helpers that were duplicated across the
//! per-agent parsers: [`time::epoch_secs`] and [`path::relativize`]. Later phases add
//! the canonical message log (Layer 1 ↔ Layer 2 boundary) and the replay fold.

pub mod message;
pub mod path;
pub mod session;
pub mod time;

pub use session::{parse_session, parse_session_as, Session};
