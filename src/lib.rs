//! claude-replay library — the thin assembly crate for the `claude-replay` viewer and the
//! `agent-jdi` supervisor binaries.
//!
//! The layers live in sibling crates (#71): `claude-replay-core` (parser/replay engine),
//! `claude-replay-present` (cache + shared presentation helpers + `Args`),
//! `claude-replay-tui` and `claude-replay-html` (the two frontends). This crate re-exports
//! them under their long-standing module paths (so `claude_replay::model`,
//! `claude_replay::tui::app`, … keep working), owns the CLI entry point, and hosts `jdi`.

pub mod jdi;

pub use claude_replay_html::html_export;
pub use claude_replay_tui as tui;

pub use claude_replay_core::{
    claude_discover, codex_discover, diff, discover, engine, fold, follow, metrics, model, summary,
    Agent, Transcript,
};
pub use claude_replay_present::{cache, highlight, present, sys, Args, SessionCache};

use anyhow::Result;
use clap::Parser;

/// Entry point for the `claude-replay` viewer binary.
pub fn run_viewer() -> Result<()> {
    let args = Args::parse();
    // `--html`: open a browser instead of the TUI, but with the SAME session
    // selection as the terminal viewer — an explicit id/path or `--latest` resolves
    // directly (cwd-scoped for `--latest`); otherwise show the picker (like a bare
    // `-f`), so `-f --html` prompts when this dir has several sessions.
    if args.html {
        let path = if args.target.is_some() || args.latest {
            discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?
        } else {
            match tui::app::pick_session(&args)? {
                Some(p) => p,
                None => return Ok(()), // user aborted the picker
            }
        };
        return html_export::serve(&args, &path);
    }
    // No id/path/--latest and not dumping → interactive picker ↔ viewer flow. The
    // picker merges sessions from every agent (filtered by --agent) for this dir.
    if args.target.is_none()
        && !args.latest
        && args.dump.is_none()
        && args.dump_html.is_none()
        && args.dump_all_html.is_none()
    {
        return tui::app::run_interactive(&args);
    }
    // Explicit path / session id / --latest: resolve across agents (honoring the
    // --agent filter). The agent for each opened file is auto-detected downstream.
    let path = discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?;
    if args.dump_all_html.is_some() {
        html_export::dump_all_html(&args, &path)
    } else if args.dump_html.is_some() {
        html_export::dump_html(&args, &path)
    } else if args.dump.is_some() {
        tui::app::dump(&args, &path)
    } else {
        tui::app::run(&args, &path)
    }
}
