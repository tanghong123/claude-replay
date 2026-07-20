//! agent-jdi — supervise unattended AI-agent runs behind an agent-agnostic core.
//!
//! The spine (state, lock, backlog, retry loop) is agent-neutral; each agent is a
//! `AgentAdapter` (claude.rs / codex.rs). New agents = one module + one registry arm.
// The spine + `AgentAdapter` expose a complete contract; a few pieces (backlog-drain
// runs, `GaveUp`/`supports_fresh_run`, `ctx.cwd`) are wired by later flows / future
// agents rather than the current command set, so keep them without dead-code noise.
#![allow(dead_code)]

mod agent;
mod backlog;
mod claude;
mod codex;
mod detect;
mod lock;
mod state;
mod supervisor;

use crate::Agent;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use state::Session;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "agent-jdi",
    version,
    about = "Supervise unattended AI-agent runs (Claude, Codex, …) and follow them with claude-replay."
)]
struct Cli {
    /// Force a specific agent instead of auto-detecting from the directory.
    #[arg(long, value_enum, global = true)]
    agent: Option<Agent>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Resume the most-recent session for this directory, unattended, and follow it.
    Resume {
        /// Extra instruction folded into the persistence prompt.
        instruction: Vec<String>,
        /// Seconds between relaunch attempts (default 600).
        #[arg(long)]
        interval: Option<u64>,
        /// Give up after this many attempts (0 = never; default 0).
        #[arg(long)]
        max_attempts: Option<u32>,
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
    /// Queue follow-up work for a session's next drain (omit text to list the queue).
    Backlog {
        message: Vec<String>,
        /// Session id (default: this directory's).
        #[arg(long)]
        id: Option<String>,
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
        Command::Resume {
            instruction,
            interval,
            max_attempts,
        } => cmd_resume(
            &config,
            cli.agent,
            &instruction.join(" "),
            interval,
            max_attempts,
        ),
        Command::Log { id } => cmd_log(&config, id.as_deref()),
        Command::Status { id } => cmd_status(&config, id.as_deref()),
        Command::List => cmd_list(&config),
        Command::Backlog { message, id } => cmd_backlog(&config, id.as_deref(), &message.join(" ")),
        Command::Takeover { id } => cmd_takeover(&config, id.as_deref()),
        Command::Run { id } => supervisor::run_loop(&config.home, &id),
    }
}

/// Resolve the session for a command: an explicit id, else this directory's slot.
fn resolve_session(config: &Config, id: Option<&str>) -> Result<Session> {
    let sid = match id {
        Some(id) => id.to_string(),
        None => state::slot_id(&std::env::current_dir()?),
    };
    let s = Session::new(&config.home, &sid);
    if !s.exists() {
        bail!("no tracked session '{sid}' — run `agent-jdi resume` here first");
    }
    Ok(s)
}

fn cmd_resume(
    config: &Config,
    forced: Option<Agent>,
    instruction: &str,
    interval: Option<u64>,
    max_attempts: Option<u32>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let agent = detect::agent_for(&cwd, forced).ok_or_else(|| anyhow_no_session(&cwd))?;
    let adapter = agent::adapter(agent);
    adapter.preflight()?;
    let resumable = adapter.discover_resumable(&cwd)?;

    let slot = state::slot_id(&cwd);
    let session = Session::new(&config.home, &slot);

    // Take the slot (single-instance guard) before writing anything.
    let _lock = match lock::acquire(&session.dir, || session.alive())? {
        lock::Acquire::Acquired(l) => l,
        lock::Acquire::AlreadyRunning => {
            bail!("a supervisor is already running for {}", cwd.display())
        }
        lock::Acquire::SetupInFlight => bail!("another agent-jdi is setting up this directory"),
    };

    let cwd_str = cwd.to_string_lossy();
    session.ensure_dir()?;
    session.meta_set("id", &slot)?;
    session.meta_set("agent", agent.label())?;
    session.meta_set("cwd", &cwd_str)?;
    session.meta_set("session_id", &resumable.id)?;
    session.meta_set("transcript", &resumable.transcript.to_string_lossy())?;
    session.meta_set("resumed", "true")?;
    session.meta_set("interval", &interval.unwrap_or(600).to_string())?;
    session.meta_set("max_attempts", &max_attempts.unwrap_or(0).to_string())?;
    session.meta_set(
        "mode",
        adapter.initial_mode(agent::Trigger::Resume).as_str(),
    )?;
    session.meta_set("state", "starting")?;
    std::fs::write(session.dir.join("task.md"), instruction).ok();

    // Nudge the deprecated bash claude-jdi if it managed this dir.
    if detect::mark_legacy_superseded(&cwd) {
        eprintln!("note: this directory was managed by claude-jdi; it's now on agent-jdi.");
    }

    let pid = supervisor::spawn_detached(&config.home, &slot)?;
    session.meta_set("pid", &pid.to_string())?;
    session.meta_set("state", "running")?;
    drop(_lock); // the worker runs lock-free; liveness is via its pid

    eprintln!(
        "agent-jdi: {} worker {pid} running for session {} — press q to leave; it keeps going.",
        agent.label(),
        resumable.id
    );
    follow_viewer(&resumable.transcript)
}

fn cmd_log(config: &Config, id: Option<&str>) -> Result<()> {
    let session = resolve_session(config, id)?;
    let path = session
        .meta_get("transcript")
        .map(PathBuf::from)
        .or_else(|| {
            let agent = session
                .meta_get("agent")
                .and_then(|a| Agent::from_label(&a))?;
            let sid = session.meta_get("session_id")?;
            let cwd = session.meta_get("cwd").map(PathBuf::from)?;
            agent::adapter(agent).transcript_path(&sid, &cwd)
        })
        .context("no transcript recorded for this session")?;
    follow_viewer(&path)
}

fn cmd_status(config: &Config, id: Option<&str>) -> Result<()> {
    let session = resolve_session(config, id)?;
    let get = |k: &str| session.meta_get(k).unwrap_or_else(|| "-".into());
    println!("id:        {}", get("id"));
    println!("agent:     {}", get("agent"));
    println!("cwd:       {}", get("cwd"));
    println!("session:   {}", get("session_id"));
    let live = if session.alive() { " (live)" } else { "" };
    println!("state:     {}{live}", get("state"));
    println!("mode:      {}", get("mode"));
    println!("attempts:  {}", get("attempts"));
    if let Some(agent) = session
        .meta_get("agent")
        .and_then(|a| Agent::from_label(&a))
    {
        if let Some(q) = agent::adapter(agent).task_queue() {
            println!("{}", q.render(&get("session_id")));
        }
    }
    let bl = backlog::Backlog::new(session.backlog_root());
    let (p, d) = (bl.pending_count(), bl.draining_count());
    if p + d > 0 {
        println!("backlog:   {p} pending, {d} draining");
    }
    Ok(())
}

fn cmd_list(config: &Config) -> Result<()> {
    let mut any = false;
    if let Ok(entries) = std::fs::read_dir(&config.home) {
        for e in entries.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            let s = Session::new(&config.home, &id);
            if !s.exists() {
                continue;
            }
            any = true;
            let st = s.state().map(|x| x.as_str()).unwrap_or("?");
            let agent = s.meta_get("agent").unwrap_or_else(|| "-".into());
            let live = if s.alive() { "live" } else { "-" };
            println!("{id}\t{agent}\t{st}\t{live}");
        }
    }
    if !any {
        println!("(no tracked sessions under {})", config.home.display());
    }
    Ok(())
}

fn cmd_backlog(config: &Config, id: Option<&str>, message: &str) -> Result<()> {
    let session = resolve_session(config, id)?;
    let bl = backlog::Backlog::new(session.backlog_root());
    if message.trim().is_empty() {
        println!(
            "backlog: {} pending, {} draining",
            bl.pending_count(),
            bl.draining_count()
        );
        return Ok(());
    }
    let path = bl.add(message)?;
    println!("queued: {}", path.display());
    Ok(())
}

fn cmd_takeover(config: &Config, id: Option<&str>) -> Result<()> {
    let session = resolve_session(config, id)?;
    supervisor::takeover(&session)?;
    println!("stopped session {}", session.dir.display());
    Ok(())
}

/// Launch the read-only viewer on `path`, following live. Runs in-process (the
/// viewer is the same crate); it takes over the terminal and exits the process.
fn follow_viewer(path: &Path) -> Result<()> {
    let args = crate::Args {
        target: Some(path.to_string_lossy().to_string()),
        agent: None,
        latest: false,
        follow: true,
        no_thinking: false,
        reads: false,
        results: false,
        no_user: false,
        full: false,
        fold: None,
        unfold: None,
        read_match: None,
        dump: None,
        width: None,
    };
    crate::app::run(&args, path)
}

fn anyhow_no_session(cwd: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "no resumable Claude or Codex session found for {} (use --agent to force one)",
        cwd.display()
    )
}
