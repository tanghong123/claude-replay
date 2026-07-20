//! claude-replay library — shared modules for the `claude-replay` viewer and the
//! `agent-jdi` supervisor binaries.
//!
//! The viewer is **read-only** (scroll, fold, search, live-tail); `agent-jdi` reuses
//! this crate's transcript discovery/parsing to supervise unattended agent runs.

pub mod app;
mod clipboard;
pub mod codex_discover;
pub mod codex_metrics;
pub mod codex_model;
pub mod discover;
mod highlight;
pub mod jdi;
mod markdown;
pub mod metrics;
pub mod model;
mod picker;
mod render;
mod tail;
mod theme;
pub mod view;
mod wrap;

use anyhow::Result;
use clap::{Parser, ValueEnum};

/// Which agent produced a session. Detected per file from its contents; the
/// `--agent` flag only *filters* the picker/`--latest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// Short label for the picker row / CLI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// View flags. Defaults mirror the bash `claude-peek`: thinking + user turns +
/// code-modifying actions shown; non-modifying ops, tool output hidden.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "claude-replay",
    version,
    about = "Read an AI agent session transcript like a screen (read-only)."
)]
pub struct Args {
    /// Session id, or a path to a .jsonl transcript.
    pub target: Option<String>,

    /// Only show sessions from this agent (claude or codex). Default: all agents.
    #[arg(long, value_enum)]
    pub agent: Option<Agent>,

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
    /// thinking, read, bash, edit, write, tool, tool_result, command.
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

/// Entry point for the `claude-replay` viewer binary.
pub fn run_viewer() -> Result<()> {
    let args = Args::parse();
    // No id/path/--latest and not dumping → interactive picker ↔ viewer flow. The
    // picker merges sessions from every agent (filtered by --agent) for this dir.
    if args.target.is_none() && !args.latest && args.dump.is_none() {
        return app::run_interactive(&args);
    }
    // Explicit path / session id / --latest: resolve across agents (honoring the
    // --agent filter). The agent for each opened file is auto-detected downstream.
    let path = discover::resolve_any(args.agent, args.target.as_deref(), args.latest)?;
    if args.dump.is_some() {
        app::dump(&args, &path)
    } else {
        app::run(&args, &path)
    }
}
