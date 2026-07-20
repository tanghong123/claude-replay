//! agent-jdi — supervise unattended AI-agent runs behind an agent-agnostic core.
//!
//! Under construction in stages: this stage lands the shared spine (state, lock,
//! backlog) + the `AgentAdapter` trait + the CLI shape. The supervisor loop and the
//! Claude/Codex adapters wire in over the next stages.
#![allow(dead_code)] // spine/trait are built ahead of the adapters that consume them

mod agent;
mod backlog;
mod lock;
mod state;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "agent-jdi",
    version,
    about = "Supervise unattended AI-agent runs (Claude, Codex, …) and follow them with claude-replay."
)]
struct Cli {
    /// Force a specific agent instead of auto-detecting from the directory.
    #[arg(long, value_enum, global = true)]
    agent: Option<crate::Agent>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Resume the most-recent session for this directory, unattended.
    Resume {
        /// Extra instruction appended to the persistence prompt.
        instruction: Vec<String>,
    },
    /// Reattach the viewer to a supervised session's transcript.
    Log {
        /// Session id (default: the one tracked for this directory).
        id: Option<String>,
    },
    /// Show a supervised session's status.
    Status { id: Option<String> },
    /// List tracked sessions.
    List,
    /// Queue follow-up work for a session's next drain.
    Backlog {
        /// Message text (omit to list the queue).
        message: Vec<String>,
    },
    /// Stop a supervised session (state left intact).
    Takeover { id: Option<String> },
    /// Internal: the detached supervisor loop (do not call directly).
    #[command(name = "__run", hide = true)]
    Run { id: String },
}

/// Resolved runtime config. `agent-jdi` uses its own state root (clean cutover from
/// the bash `claude-jdi`); override with `AGENT_JDI_HOME`.
struct Config {
    home: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        let home = std::env::var_os("AGENT_JDI_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let base = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                base.join(".claude").join("agent-jdi")
            });
        Self { home }
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env();
    match cli.command {
        Command::List => cmd_list(&config),
        _ => bail!(
            "agent-jdi: '{:?}' is not implemented yet — under construction (see the staged build)",
            cli.command
        ),
    }
}

/// List tracked sessions (dirs under `home` that have a `meta` file).
fn cmd_list(config: &Config) -> Result<()> {
    let mut any = false;
    if let Ok(entries) = std::fs::read_dir(&config.home) {
        for e in entries.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            let s = state::Session::new(&config.home, &id);
            if !s.exists() {
                continue;
            }
            any = true;
            let st = s.state().map(|x| x.as_str()).unwrap_or("?");
            let live = if s.alive() { "live" } else { "-" };
            println!("{id}\t{st}\t{live}");
        }
    }
    if !any {
        println!("(no tracked sessions under {})", config.home.display());
    }
    Ok(())
}
