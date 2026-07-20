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
use std::time::Duration;

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

    /// Print what a command would do and exit — no spawn, kill, or state change.
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start a FRESH unattended run of a task, and follow it. Default agent: the
    /// latest run's in this directory (override with --agent).
    Start {
        /// The task to run (or use --task-file).
        task: Vec<String>,
        /// Read the task from a file instead of the positional args.
        #[arg(long, value_name = "PATH")]
        task_file: Option<PathBuf>,
        /// Seconds between relaunch attempts (default 600).
        #[arg(long)]
        interval: Option<u64>,
        /// Give up after this many attempts (0 = never; default 0).
        #[arg(long)]
        max_attempts: Option<u32>,
    },
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

/// Resolved runtime config. `agent-jdi` is agent-neutral, so its state lives in a
/// neutral state dir — `$XDG_STATE_HOME/agent-jdi` (default `~/.local/state/agent-jdi`),
/// **not** under `~/.claude`. Override the whole path with `AGENT_JDI_HOME`.
struct Config {
    home: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        let home = std::env::var_os("AGENT_JDI_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_home().join("agent-jdi"));
        Self { home }
    }
}

/// The base state directory: `$XDG_STATE_HOME` if set, else `~/.local/state`.
fn state_home() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|x| !x.is_empty()) {
        return PathBuf::from(x);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("state")
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env();
    let dry = cli.dry_run;
    match cli.command {
        Command::Start {
            task,
            task_file,
            interval,
            max_attempts,
        } => cmd_start(
            &config,
            cli.agent,
            &task.join(" "),
            task_file,
            interval,
            max_attempts,
            dry,
        ),
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
            dry,
        ),
        Command::Log { id } => cmd_log(&config, id.as_deref()),
        Command::Status { id } => cmd_status(&config, id.as_deref()),
        Command::List => cmd_list(&config),
        Command::Backlog { message, id } => {
            cmd_backlog(&config, id.as_deref(), &message.join(" "), dry)
        }
        Command::Takeover { id } => cmd_takeover(&config, id.as_deref(), dry),
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
    dry_run: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let agent = detect::agent_for(&cwd, forced).ok_or_else(|| anyhow_no_session(&cwd))?;
    let adapter = agent::adapter(agent);
    adapter.preflight()?;
    let resumable = adapter.discover_resumable(&cwd)?;

    // `--dry-run`: show what would run, with no side effects (no slot, no spawn, no
    // viewer). Safe way to verify agent detection + the exact invocation.
    if dry_run {
        let brief = agent::Brief {
            text: instruction.to_string(),
            backlog: Vec::new(),
        };
        let mode = adapter.initial_mode(agent::Trigger::Resume);
        let ctx = agent::TurnContext {
            mode,
            session_id: &resumable.id,
            session_created: true,
            cwd: &cwd,
            brief: &brief,
            extra_args: &[],
        };
        let inv = adapter.build_invocation(&ctx);
        println!("agent:      {}", agent.label());
        println!(
            "binary:     {}",
            adapter
                .resolve_binary()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|e| format!("(unresolved: {e})"))
        );
        println!("session:    {}", resumable.id);
        println!("transcript: {}", resumable.transcript.display());
        println!("mode:       {}", mode.as_str());
        println!(
            "would run:  {} {}",
            inv.program.display(),
            shell_join(&inv.args)
        );
        return Ok(());
    }

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

/// The default agent for a fresh `start` in `cwd`: the last agent-jdi run here, else
/// the agent of the most recent session of any kind, else Claude.
fn default_agent(config: &Config, cwd: &Path) -> Agent {
    let slot = Session::new(&config.home, &state::slot_id(cwd));
    if let Some(a) = slot.meta_get("agent").and_then(|s| Agent::from_label(&s)) {
        return a;
    }
    detect::agent_for(cwd, None).unwrap_or(Agent::Claude)
}

#[allow(clippy::too_many_arguments)]
fn cmd_start(
    config: &Config,
    forced: Option<Agent>,
    task_arg: &str,
    task_file: Option<PathBuf>,
    interval: Option<u64>,
    max_attempts: Option<u32>,
    dry_run: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let task = match task_file {
        Some(p) => std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?,
        None => task_arg.to_string(),
    };
    if task.trim().is_empty() {
        bail!("start needs a task (positional text or --task-file)");
    }
    // A fresh start has no session of its own to detect from, so default to the
    // agent of the *latest run* in this directory (its last agent-jdi run, else the
    // most recent session of any kind); Claude only when there's no history.
    let agent = match forced {
        Some(a) => a,
        None => default_agent(config, &cwd),
    };
    let adapter = agent::adapter(agent);
    adapter.preflight()?;
    adapter.resolve_binary()?;

    let run_id = state::new_run_id();
    // Claude pins the id (`--session-id`); Codex assigns one, so leave it empty and
    // recover it after the first turn via the nonce.
    let (session_id, nonce) = if adapter.pins_session_id() {
        (run_id.clone(), run_id)
    } else {
        (String::new(), run_id)
    };
    let brief = agent::Brief {
        text: task.clone(),
        backlog: Vec::new(),
    };

    if dry_run {
        let ctx = agent::TurnContext {
            mode: agent::Mode::Start,
            session_id: &session_id,
            session_created: false,
            cwd: &cwd,
            brief: &brief,
            extra_args: &[],
        };
        let inv = adapter.fresh_invocation(&ctx, &nonce);
        println!("agent:      {}", agent.label());
        println!(
            "binary:     {}",
            adapter
                .resolve_binary()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|e| format!("(unresolved: {e})"))
        );
        println!(
            "session:    {}",
            if session_id.is_empty() {
                "(assigned by the agent, captured after turn 1)"
            } else {
                &session_id
            }
        );
        println!("mode:       start");
        println!(
            "would run:  {} {}",
            inv.program.display(),
            shell_join(&inv.args)
        );
        return Ok(());
    }

    let slot = state::slot_id(&cwd);
    let session = Session::new(&config.home, &slot);
    let _lock = match lock::acquire(&session.dir, || session.alive())? {
        lock::Acquire::Acquired(l) => l,
        lock::Acquire::AlreadyRunning => {
            bail!("a supervisor is already running for {}", cwd.display())
        }
        lock::Acquire::SetupInFlight => bail!("another agent-jdi is setting up this directory"),
    };

    session.ensure_dir()?;
    session.meta_set("id", &slot)?;
    session.meta_set("agent", agent.label())?;
    session.meta_set("cwd", &cwd.to_string_lossy())?;
    session.meta_set("session_id", &session_id)?;
    session.meta_set("nonce", &nonce)?;
    session.meta_set("resumed", "false")?;
    session.meta_set("interval", &interval.unwrap_or(600).to_string())?;
    session.meta_set("max_attempts", &max_attempts.unwrap_or(0).to_string())?;
    session.meta_set("mode", agent::Mode::Start.as_str())?;
    session.meta_set("state", "starting")?;
    std::fs::write(session.dir.join("task.md"), &task).ok();
    detect::mark_legacy_superseded(&cwd);

    let pid = supervisor::spawn_detached(&config.home, &slot)?;
    session.meta_set("pid", &pid.to_string())?;
    session.meta_set("state", "running")?;
    let follow = adapter.expected_transcript(&session_id, &cwd);
    drop(_lock);

    eprintln!(
        "agent-jdi: started {} run (worker {pid}) in {} — press q to leave; it keeps going.",
        agent.label(),
        cwd.display()
    );
    // Claude's transcript path is known up front → wait briefly for the file, then
    // follow. Codex's id/path isn't known until capture, so point at `log` instead.
    match follow {
        Some(p) => {
            for _ in 0..40 {
                if p.exists() {
                    return follow_viewer(&p);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            eprintln!("(transcript not visible yet — run `agent-jdi log` to watch)");
            Ok(())
        }
        None => {
            eprintln!("run `agent-jdi log` once it's underway to watch it live.");
            Ok(())
        }
    }
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

fn cmd_backlog(config: &Config, id: Option<&str>, message: &str, dry_run: bool) -> Result<()> {
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
    if dry_run {
        println!(
            "[dry-run] would queue for {}: {message}",
            session.dir.display()
        );
        return Ok(());
    }
    let path = bl.add(message)?;
    println!("queued: {}", path.display());
    Ok(())
}

fn cmd_takeover(config: &Config, id: Option<&str>, dry_run: bool) -> Result<()> {
    let session = resolve_session(config, id)?;
    if dry_run {
        match session.pid() {
            Some(pid) => println!(
                "[dry-run] would stop session {} (kill worker pid {pid} + its children)",
                session.dir.display()
            ),
            None => println!(
                "[dry-run] session {} has no recorded worker pid",
                session.dir.display()
            ),
        }
        return Ok(());
    }
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

/// Quote args for readable display (single-line preview; not for execution).
fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty()
                || a.chars()
                    .any(|c| c.is_whitespace() || "\"'\\$`\n".contains(c))
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn anyhow_no_session(cwd: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "no resumable Claude or Codex session found for {} (use --agent to force one)",
        cwd.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_agent_reuses_the_last_run_in_this_dir() {
        let base = std::env::temp_dir().join(format!("ajdi-defagent-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let home = base.join("home");
        let cwd = base.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = Config { home };
        // Record a prior Codex run for this dir's slot → a fresh start reuses it.
        Session::new(&config.home, &state::slot_id(&cwd))
            .meta_set("agent", "codex")
            .unwrap();
        assert_eq!(default_agent(&config, &cwd), Agent::Codex);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn shell_join_quotes_only_when_needed() {
        assert_eq!(
            shell_join(&["--resume".into(), "abc".into()]),
            "--resume abc"
        );
        // Args with spaces / quotes get single-quoted for a readable preview.
        assert_eq!(shell_join(&["a b".into()]), "'a b'");
        assert_eq!(shell_join(&["it's".into()]), "'it'\\''s'");
    }
}
