//! **claude-replay-tui** — the ratatui terminal frontend (#71): the `app` terminal + input
//! loop, the `view` state machine and draw (TestBackend-testable), the rendering pipeline
//! (`render` blocks → styled lines, plus `markdown`, `wrap`, `theme`), the fuzzy session
//! `picker`, and OS `clipboard` access. Everything terminal-specific lives here; the shared
//! text formatters come from `claude-replay-present` and the parser from
//! `claude-replay-core`.

pub mod app;
pub mod view;

mod clipboard;
mod markdown;
mod picker;
mod render;
mod theme;
mod wrap;

// Self-alias: the modules moved here from the root crate's `src/tui/` and still refer to
// each other as `crate::tui::…`.
pub(crate) mod tui {
    pub(crate) use crate::{clipboard, markdown, picker, render, theme, view, wrap};
}

// Aliases so moved modules keep referring to `crate::model`, `crate::present`, … unchanged.
pub(crate) use claude_replay_core::{
    diff, discover, engine, fold, metrics, model, Agent, Transcript,
};
pub(crate) use claude_replay_present::{highlight, present, sys, Args, SessionCache};
