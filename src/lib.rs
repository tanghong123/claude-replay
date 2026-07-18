//! Shared replay core for Claude Code and Codex transcript viewers.

mod app;
mod clipboard;
mod codex_discover;
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
use clap::{CommandFactory, FromArgMatches, Parser};

/// Transcript format and product identity selected by the launcher binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Claude,
    Codex,
}

impl Backend {
    pub fn command_name(self) -> &'static str {
        match self {
            Self::Claude => "claude-replay",
            Self::Codex => "codex-replay",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Self::Claude => "Read a Claude Code session transcript like a screen (read-only).",
            Self::Codex => "Read a Codex session transcript like a screen (read-only).",
        }
    }

    pub fn transcript_label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// View flags shared by both transcript viewers.
#[derive(Parser, Debug, Clone)]
#[command(version)]
pub struct Args {
    /// Session id, or a path to a .jsonl transcript.
    pub target: Option<String>,

    /// Open the most-recently-active transcript anywhere.
    #[arg(long)]
    pub latest: bool,

    /// Follow the file and show new events live (tail -f).
    #[arg(short = 'f', long)]
    pub follow: bool,

    /// Hide thinking summaries (shown by default).
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

    /// Start these block types expanded (comma-separated). Wins over --fold.
    #[arg(long, value_name = "TYPES")]
    pub unfold: Option<String>,

    /// Also show Read calls whose file path contains this substring.
    #[arg(long)]
    pub read_match: Option<String>,

    /// Render the whole transcript (no TUI) and exit. With no value, write
    /// `<stem>.txt` + `<stem>.ansi`; `--dump -` prints plain text.
    #[arg(long, num_args(0..=1), value_name = "STEM")]
    pub dump: Option<Option<String>>,

    /// Width for `--dump` (columns). Defaults to terminal width, else 100.
    #[arg(long, value_name = "N")]
    pub width: Option<usize>,
}

pub fn command(backend: Backend) -> clap::Command {
    Args::command()
        .name(backend.command_name())
        .about(backend.about())
}

/// Parse process arguments and run the selected viewer backend.
pub fn run(backend: Backend) -> Result<()> {
    let args = Args::from_arg_matches(&command(backend).get_matches())?;
    run_with_args(backend, args)
}

fn run_with_args(backend: Backend, args: Args) -> Result<()> {
    if args.target.is_none() && !args.latest && args.dump.is_none() {
        return app::run_interactive(backend, &args);
    }
    let path = discover::resolve_for(backend, args.target.as_deref(), args.latest)?;
    if args.dump.is_some() {
        app::dump(backend, &args, &path)
    } else {
        app::run(backend, &args, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_identity_is_binary_specific() {
        assert_eq!(Backend::Claude.command_name(), "claude-replay");
        assert_eq!(Backend::Codex.command_name(), "codex-replay");
        assert!(Backend::Codex.about().contains("Codex"));
    }

    #[test]
    fn codex_help_uses_codex_binary_name() {
        assert_eq!(command(Backend::Codex).get_name(), "codex-replay");
    }
}
