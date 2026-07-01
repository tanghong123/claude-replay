//! claude-replay — interactive, read-only Claude Code transcript viewer.
//!
//! Like `claude --resume`, but you can only read: scroll, fold, search, live-tail.
//! See justdoit/peek-v2/DESIGN.md for the full spec and phased plan.

mod app;
mod discover;
mod highlight;
mod markdown;
mod metrics;
mod model;
mod picker;
mod render;
mod tail;
mod theme;
mod view;
mod wrap;

use anyhow::Result;
use clap::Parser;

/// View flags. Defaults mirror the bash `claude-peek`: thinking + user turns +
/// code-modifying actions shown; non-modifying ops, tool output hidden.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "claude-replay",
    about = "Read a Claude Code session transcript like a screen (read-only)."
)]
pub struct Args {
    /// Session id, or a path to a .jsonl transcript.
    pub target: Option<String>,

    /// Open the most-recently-active transcript anywhere.
    #[arg(long)]
    pub latest: bool,

    /// Follow the file and show new events live (tail -f).
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Hide ✻ thinking summaries (shown by default).
    #[arg(long)]
    pub no_thinking: bool,

    /// Include non-modifying ops (Read/grep/ls/test) — hidden by default.
    #[arg(long)]
    pub reads: bool,

    /// Include tool output / results — hidden by default.
    #[arg(long)]
    pub results: bool,

    /// Hide user turns.
    #[arg(long)]
    pub no_user: bool,

    /// Show everything expanded (unfold every block type).
    #[arg(short = 'v', long)]
    pub full: bool,

    /// Start these block types collapsed (comma-separated): user, assistant,
    /// thinking, read, bash, edit, write, tool, tool_result. Overrides defaults.
    #[arg(long, value_name = "TYPES")]
    pub fold: Option<String>,

    /// Start these block types expanded (comma-separated). Wins over --fold and
    /// the defaults. Same type keys as --fold.
    #[arg(long, value_name = "TYPES")]
    pub unfold: Option<String>,

    /// Also show Read calls whose file path contains this substring.
    #[arg(long)]
    pub read_match: Option<String>,

    /// Render the whole transcript (no TUI) and exit. With no value, write
    /// `<stem>.txt` + `<stem>.ansi` using a deduced stem; `--dump <stem>` writes to
    /// that stem; `--dump -` prints plain text to stdout (for pipes / tests).
    #[arg(long, num_args(0..=1), value_name = "STEM")]
    pub dump: Option<Option<String>>,
    /// Width for `--dump` (columns). Defaults to the terminal width, else 100.
    #[arg(long, value_name = "N")]
    pub width: Option<usize>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    // No id/path/--latest and we have a TUI → show the session picker.
    let path = if args.target.is_none() && !args.latest && args.dump.is_none() {
        match app::pick()? {
            Some(p) => p,
            None => return Ok(()), // user cancelled the picker
        }
    } else {
        discover::resolve(args.target.as_deref(), args.latest)?
    };
    if args.dump.is_some() {
        app::dump(&args, &path)
    } else {
        app::run(&args, &path)
    }
}
