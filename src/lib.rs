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
    claude_discover, codex_discover, diff, discover, engine, fold, follow, metrics, model,
    parse_session_as, parse_session_enriched_as, summary, Agent, Transcript,
};
pub use claude_replay_present::{cache, highlight, present, sys, Args, SessionCache};

use anyhow::Result;
use clap::Parser;

/// Entry point for the `claude-replay` viewer binary.
pub fn run_viewer() -> Result<()> {
    let args = Args::parse();
    // Take back what dead runs left behind (#165), on EVERY invocation — the frontends each do
    // this when they build a cache, which leaves the `--dump*` modes never reclaiming anything.
    // A machine used only for dumps would keep the leftovers forever, and those are exactly the
    // ones nobody is watching.
    sys::reclaim();
    // `--html`: open a browser instead of the TUI, but with the SAME session
    // selection as the terminal viewer — an explicit id/path or `--latest` resolves
    // directly (cwd-scoped for `--latest`); otherwise show the picker (like a bare
    // `-f`), so `-f --html` prompts when this dir has several sessions.
    if args.html {
        if args.target.is_some() || args.latest {
            let path = discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?;
            return html_export::serve(&args, &path);
        }
        // No explicit target: the picker chooses. With SEVERAL matches, serve them ALL at
        // once and stay on the picker — each pick opens that session's browser tab while
        // the list remains, so there is a way back (the TUI has always had one). A single
        // match keeps the original direct-serve path untouched.
        let cands = discover::candidates_all(args.agent);
        if cands.is_empty() {
            anyhow::bail!("no transcripts found for any agent in this directory");
        }
        if cands.len() == 1 {
            return html_export::serve(&args, &cands[0].path);
        }
        let paths: Vec<std::path::PathBuf> = cands.iter().map(|c| c.path.clone()).collect();
        let server = html_export::start_server(&args, &paths)?;
        let by_path: std::collections::HashMap<_, _> = paths
            .iter()
            .cloned()
            .zip(server.root_ids.iter().cloned())
            .collect();
        let status = format!(
            "serving {} sessions at 127.0.0.1:{} — Enter/click opens a tab, Esc quits",
            server.root_ids.len(),
            server.port
        );
        tui::app::pick_session_loop(cands, &status, &mut |path| {
            if let Some(sid) = by_path.get(path) {
                server.open(sid);
            }
        })?;
        return Ok(());
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
