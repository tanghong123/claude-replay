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
    /// Start a FRESH unattended run of a task. The worker runs detached in the
    /// background; a summary is printed (use -f to open the live viewer instead).
    /// Default agent: the latest run's in this directory (override with --agent).
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
        /// After launching, open the live replay viewer instead of printing a
        /// summary and returning (like `log -f`).
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Resume the most-recent session for this directory, unattended. The worker
    /// runs detached in the background; a summary is printed (use -f to open the
    /// live viewer instead).
    Resume {
        /// Extra instruction folded into the persistence prompt.
        instruction: Vec<String>,
        /// Seconds between relaunch attempts (default 600).
        #[arg(long)]
        interval: Option<u64>,
        /// Give up after this many attempts (0 = never; default 0).
        #[arg(long)]
        max_attempts: Option<u32>,
        /// After launching, open the live replay viewer instead of printing a
        /// summary and returning (like `log -f`).
        #[arg(long, short = 'f')]
        follow: bool,
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
            follow,
        } => cmd_start(
            &config,
            cli.agent,
            &task.join(" "),
            task_file,
            interval,
            max_attempts,
            dry,
            follow,
        ),
        Command::Resume {
            instruction,
            interval,
            max_attempts,
            follow,
        } => cmd_resume(
            &config,
            cli.agent,
            &instruction.join(" "),
            interval,
            max_attempts,
            dry,
            follow,
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

#[allow(clippy::too_many_arguments)]
fn cmd_resume(
    config: &Config,
    forced: Option<Agent>,
    instruction: &str,
    interval: Option<u64>,
    max_attempts: Option<u32>,
    dry_run: bool,
    follow: bool,
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

    guard_no_conflict(config, &cwd)?;
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

    let pid = supervisor::spawn_detached(&config.home, &slot)?;
    session.meta_set("pid", &pid.to_string())?;
    session.meta_set("state", "running")?;
    session.meta_stamp("started");
    session.meta_set("finished", "")?; // clear any prior run's finish stamp
    drop(_lock); // the worker runs lock-free; liveness is via its pid

    // `-f/--follow`: take over the terminal with the live viewer. Otherwise (the
    // default, like `claude-jdi resume`) the worker keeps running detached and we
    // just print a summary and return.
    if follow {
        eprintln!(
            "agent-jdi: {} worker {pid} running for session {} — press q to leave; it keeps going.",
            agent.label(),
            resumable.id
        );
        return follow_viewer(&resumable.transcript);
    }
    let plan = if adapter.initial_mode(agent::Trigger::Resume) == agent::Mode::ResumeDump {
        "1) dump the agreed plan, then 2) execute it."
    } else {
        "resume the session and drive the work to completion."
    };
    print!(
        "{}",
        supervisor_summary(
            "resume",
            &slot,
            &cwd,
            agent,
            &resumable.id,
            Some(resumable.idle_secs),
            interval.unwrap_or(600),
            max_attempts.unwrap_or(0),
            adapter.unattended_note(),
            plan,
        )
    );
    Ok(())
}

/// The `claude-jdi`-style run summary shown after a supervisor is launched: what it
/// is, where, which session, its retry/autonomy policy, and the follow-up commands.
/// Purely informational (the worker already runs detached).
#[allow(clippy::too_many_arguments)]
fn supervisor_summary(
    verb: &str,
    slot: &str,
    cwd: &Path,
    agent: Agent,
    session_id: &str,
    idle_secs: Option<u64>,
    interval: u64,
    max_attempts: u32,
    unattended: &str,
    plan: &str,
) -> String {
    // The session line adapts: a fresh Codex `start` has no id yet (assigned after
    // turn 1); a resume shows the id + how long since it was last active.
    let session_line = if session_id.is_empty() {
        "(assigned by the agent after turn 1)".to_string()
    } else if let Some(idle) = idle_secs {
        format!("{session_id}  (last active {} ago)", human_ago(idle))
    } else {
        session_id.to_string()
    };
    format!(
        "▶ agent-jdi {verb}: {slot}\n  \
         cwd:        {cwd}\n  \
         agent:      {agent}\n  \
         session:    {session_line}\n  \
         retry:      every {interval}s, max-attempts={max_attempts} (0=unlimited)\n  \
         runs with:  {unattended}\n\n  \
         it will:    {plan}\n  \
         check:      agent-jdi status {slot}\n  \
         watch:      agent-jdi log {slot} -f\n  \
         take over:  agent-jdi takeover {slot}\n",
        cwd = cwd.display(),
        agent = agent.label(),
    )
}

/// "7s", "3m", "2h4m" — a compact "how long ago" for the summary.
fn human_ago(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// One supervisor per directory: refuse if another `agent-jdi` — or the bash
/// `claude-jdi` — is already live for this cwd (two would fight over the session).
/// The own-tool check also catches a symlink-aliased cwd the slot lock would miss.
fn guard_no_conflict(config: &Config, cwd: &Path) -> Result<()> {
    if let Some(pid) = detect::live_supervisor_pid(&config.home, cwd) {
        bail!(
            "agent-jdi is already supervising {} (pid {pid}) — stop it first: `agent-jdi takeover`.",
            cwd.display()
        );
    }
    if let Some(pid) = detect::claude_jdi_live_for_cwd(cwd) {
        bail!(
            "claude-jdi is already supervising {} (pid {pid}) — use one supervisor per directory. \
             Stop it first: `claude-jdi takeover`.",
            cwd.display()
        );
    }
    Ok(())
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
    follow: bool,
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

    guard_no_conflict(config, &cwd)?;
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

    let pid = supervisor::spawn_detached(&config.home, &slot)?;
    session.meta_set("pid", &pid.to_string())?;
    session.meta_set("state", "running")?;
    session.meta_stamp("started");
    session.meta_set("finished", "")?; // clear any prior run's finish stamp
    let expected = adapter.expected_transcript(&session_id, &cwd);
    drop(_lock);

    // `-f/--follow`: take over the terminal with the live viewer. Claude's
    // transcript path is known up front → wait briefly for the file, then follow;
    // Codex's id/path isn't known until capture, so point at `log`.
    if follow {
        eprintln!(
            "agent-jdi: started {} run (worker {pid}) in {} — press q to leave; it keeps going.",
            agent.label(),
            cwd.display()
        );
        match expected {
            Some(p) => {
                for _ in 0..40 {
                    if p.exists() {
                        return follow_viewer(&p);
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                eprintln!("(transcript not visible yet — run `agent-jdi log` to watch)");
                return Ok(());
            }
            None => {
                eprintln!("run `agent-jdi log` once it's underway to watch it live.");
                return Ok(());
            }
        }
    }

    // Default (like `resume`): the worker runs detached — print a summary and return.
    print!(
        "{}",
        supervisor_summary(
            "start",
            &slot,
            &cwd,
            agent,
            &session_id,
            None,
            interval.unwrap_or(600),
            max_attempts.unwrap_or(0),
            adapter.unattended_note(),
            "run the task to completion, committing per step.",
        )
    );
    Ok(())
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
    let sid = get("session_id");
    let cwd = PathBuf::from(get("cwd"));
    let agent = session
        .meta_get("agent")
        .and_then(|a| Agent::from_label(&a));

    // --- header (meta) ---
    println!("id:        {}", get("id"));
    println!("agent:     {}", get("agent"));
    println!("cwd:       {}", cwd.display());
    println!("session:   {sid}");
    let live = if session.alive() { " (live)" } else { "" };
    println!("state:     {}{live}", get("state"));
    println!("mode:      {}", get("mode"));
    if let Some(t) = session.meta_get("started").filter(|s| !s.is_empty()) {
        println!("started:   {t}");
    }
    if let Some(t) = session.meta_get("finished").filter(|s| !s.is_empty()) {
        println!("finished:  {t}");
    }
    println!(
        "attempts:  {}    retry every {}s, max {}",
        get("attempts"),
        get("interval"),
        get("max_attempts")
    );
    if let Some(r) = session.meta_get("last_reason") {
        println!("last:      {r}");
    }
    println!("logs:      {}", session.supervisor_log().display());

    // --- supervisor log tail ---
    let tail = tail_lines(&session.supervisor_log(), 12);
    if !tail.is_empty() {
        println!("\n── supervisor log (last {}) ──", tail.len());
        for l in &tail {
            println!("{l}");
        }
    }

    // --- live progress (from the session transcript) ---
    let transcript = session
        .meta_get("transcript")
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .or_else(|| {
            agent
                .and_then(|a| agent::adapter(a).transcript_path(&sid, &cwd))
                .filter(|p| p.exists())
        });
    if let (Some(a), Some(tp)) = (agent, &transcript) {
        print_live_progress(a, tp);
    }

    // --- task queue (live) ---
    if let Some(a) = agent {
        if let Some(q) = agent::adapter(a).task_queue() {
            println!("\n── task queue (live) ──");
            println!("{}", q.render(&sid));
        }
    }

    // --- recent commits in cwd ---
    let commits = recent_commits(&cwd, 8);
    if !commits.is_empty() {
        println!("\n── recent commits in cwd ──");
        for c in &commits {
            println!("  {c}");
        }
    }

    // --- backlog ---
    let bl = backlog::Backlog::new(session.backlog_root());
    let (p, d) = (bl.pending_count(), bl.draining_count());
    if p + d > 0 {
        println!("\nbacklog:   {p} pending, {d} draining");
    }
    Ok(())
}

/// Last `n` lines of a file (empty if unreadable). Reads the whole file — the
/// supervisor log is small.
fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// `git log --oneline` for the last `n` commits in `cwd` (empty if not a git repo).
fn recent_commits(cwd: &Path, n: usize) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["log", "--oneline", "--decorate", "-n"])
        .arg(n.to_string())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

/// Parse the transcript and print a claude-jdi-style live-progress block: size +
/// last-activity, a tool-call histogram, the last few actions, and what the agent
/// is currently doing. Best-effort — silently skips on a parse/read error.
fn print_live_progress(agent: Agent, path: &Path) {
    let args = crate::Args {
        target: None,
        agent: None,
        latest: false,
        follow: false,
        no_thinking: false,
        reads: true, // count Reads/greps too
        results: true,
        no_user: false,
        full: false,
        fold: None,
        unfold: None,
        read_match: None,
        dump: None,
        width: None,
    };
    let Ok(blocks) = crate::model::parse_path_for(agent, path, &args) else {
        return;
    };
    let mut tools: Vec<(String, String)> = Vec::new();
    let mut currently: Option<String> = None;
    collect_tool_activity(&blocks, &mut tools, &mut currently);

    println!("\n── live progress (from session transcript) ──");
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let ago = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| human_ms(d.as_secs()))
        .unwrap_or_else(|| "?".into());
    println!(
        "transcript:    {} ({}, last active {ago} ago)",
        path.display(),
        human_bytes(size)
    );

    // Histogram: counts per tool name, most-used first.
    let mut hist: Vec<(String, usize)> = Vec::new();
    for (name, _) in &tools {
        match hist.iter_mut().find(|(n, _)| n == name) {
            Some((_, c)) => *c += 1,
            None => hist.push((name.clone(), 1)),
        }
    }
    hist.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    if !hist.is_empty() {
        let cells: Vec<String> = hist.iter().map(|(n, c)| format!("{n}×{c}")).collect();
        println!("tool calls:    {}", cells.join("  "));
    }
    if let Some(c) = &currently {
        println!("currently:     {}", truncate_flat(c, 72));
    }
    let recent = &tools[tools.len().saturating_sub(6)..];
    if !recent.is_empty() {
        println!("recent actions:");
        for (name, target) in recent {
            println!("  {name:<7} {}", truncate_flat(target, 60));
        }
    }
}

/// Walk blocks (descending into coalesced activity runs) collecting `(tool, target)`
/// in order, and tracking the latest assistant line as "currently".
fn collect_tool_activity(
    blocks: &[crate::model::Block],
    tools: &mut Vec<(String, String)>,
    currently: &mut Option<String>,
) {
    use crate::model::Block;
    for b in blocks {
        match b {
            Block::ToolUse { name, target, .. } => {
                tools.push((name.clone(), target.clone()));
            }
            Block::Thinking { tools: inner, .. } => {
                collect_tool_activity(inner, tools, currently);
            }
            Block::AssistantText(t) => {
                if let Some(line) = t.lines().find(|l| !l.trim().is_empty()) {
                    *currently = Some(line.to_string());
                }
            }
            _ => {}
        }
    }
}

/// Flatten newlines and truncate to `max` display chars with an ellipsis.
fn truncate_flat(s: &str, max: usize) -> String {
    let flat = s.replace('\n', "\\n");
    if flat.chars().count() > max {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    } else {
        flat
    }
}

/// "0m 13s", "3m 5s", "2h 4m" — a compact elapsed time for the progress block.
fn human_ms(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Human byte size: "5.0M", "812K", "45B".
fn human_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1}M", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.0}K", n as f64 / (1u64 << 10) as f64)
    } else {
        format!("{n}B")
    }
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
    fn human_ago_is_compact() {
        assert_eq!(human_ago(7), "7s");
        assert_eq!(human_ago(59), "59s");
        assert_eq!(human_ago(600), "10m");
        assert_eq!(human_ago(3600), "1h0m");
        assert_eq!(human_ago(7 * 3600 + 4 * 60), "7h4m");
    }

    #[test]
    fn resume_summary_matches_claude_jdi_shape_and_uses_real_commands() {
        let s = supervisor_summary(
            "resume",
            "knack-98db47",
            Path::new("/Users/hong/code/knack"),
            Agent::Claude,
            "a3cdd86e-398b-498f-807c-185332447c5c",
            Some(7),
            600,
            0,
            "--dangerously-skip-permissions (unattended)",
            "1) dump the agreed plan, then 2) execute it.",
        );
        // Header + all the labelled fields, claude-jdi style.
        assert!(s.starts_with("▶ agent-jdi resume: knack-98db47\n"), "{s}");
        assert!(s.contains("  cwd:        /Users/hong/code/knack\n"), "{s}");
        assert!(s.contains("(last active 7s ago)"), "{s}");
        assert!(
            s.contains("  retry:      every 600s, max-attempts=0 (0=unlimited)\n"),
            "{s}"
        );
        assert!(
            s.contains("  runs with:  --dangerously-skip-permissions (unattended)\n"),
            "{s}"
        );
        // Follow-up hints must be the *real* agent-jdi commands (copy-pasteable),
        // not claude-jdi, and must NOT auto-launch the viewer.
        assert!(
            s.contains("  check:      agent-jdi status knack-98db47\n"),
            "{s}"
        );
        assert!(
            s.contains("  watch:      agent-jdi log knack-98db47 -f\n"),
            "{s}"
        );
        assert!(
            s.contains("  take over:  agent-jdi takeover knack-98db47\n"),
            "{s}"
        );
    }

    #[test]
    fn start_summary_handles_an_unassigned_session_id() {
        // A fresh Codex `start` has no id yet (assigned after turn 1) and no
        // last-active time → the session line says so, no `(last active …)`.
        let s = supervisor_summary(
            "start",
            "knack-98db47",
            Path::new("/Users/hong/code/knack"),
            Agent::Codex,
            "", // not pinned
            None,
            600,
            0,
            "sandbox=workspace-write, approvals=never (unattended)",
            "run the task to completion, committing per step.",
        );
        assert!(s.starts_with("▶ agent-jdi start: knack-98db47\n"), "{s}");
        assert!(
            s.contains("  session:    (assigned by the agent after turn 1)\n"),
            "{s}"
        );
        assert!(
            !s.contains("last active"),
            "no last-active for a fresh start: {s}"
        );
    }

    #[test]
    fn start_and_resume_expose_the_follow_flag() {
        use clap::Parser;
        // `-f` parses on both subcommands and defaults to false.
        let start = Cli::parse_from(["agent-jdi", "start", "do it"]);
        assert!(matches!(
            start.command,
            Command::Start { follow: false, .. }
        ));
        let start_f = Cli::parse_from(["agent-jdi", "start", "do it", "-f"]);
        assert!(matches!(
            start_f.command,
            Command::Start { follow: true, .. }
        ));
        let resume_f = Cli::parse_from(["agent-jdi", "resume", "--follow"]);
        assert!(matches!(
            resume_f.command,
            Command::Resume { follow: true, .. }
        ));
    }

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
