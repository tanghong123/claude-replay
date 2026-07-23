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
        /// summary and returning (`agent-jdi log` follows by default).
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Resume the most-recent session for this directory, unattended. The worker
    /// runs detached in the background; a summary is printed (use -f to open the
    /// live viewer instead).
    Resume {
        /// Resume an exact tracked slot from `agent-jdi list`.
        #[arg(long, value_name = "ID", conflicts_with = "session")]
        id: Option<String>,
        /// Extra instruction folded into the persistence prompt.
        instruction: Vec<String>,
        /// Seconds between relaunch attempts (default 600).
        #[arg(long)]
        interval: Option<u64>,
        /// Give up after this many attempts (0 = never; default 0).
        #[arg(long)]
        max_attempts: Option<u32>,
        /// After launching, open the live replay viewer instead of printing a
        /// summary and returning (`agent-jdi log` follows by default).
        #[arg(long, short = 'f')]
        follow: bool,
        /// Resume this exact session id (skips discovery + the stale-session
        /// prompt). Default: the newest session for this directory.
        #[arg(long, conflicts_with = "id")]
        session: Option<String>,
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
    /// A live run drains it automatically once its current work finishes; use
    /// --drain to start a drain now for a session that has already stopped.
    Backlog {
        message: Vec<String>,
        /// Session id (default: this directory's).
        #[arg(long)]
        id: Option<String>,
        /// Drain the queue now (relaunches a stopped session to work through it).
        #[arg(long)]
        drain: bool,
    },
    /// Stop a supervised session and hand it back to you — launches the agent
    /// interactively resumed on the session (state left intact). Use --no-launch
    /// to just stop and report, without opening the agent.
    ///
    /// With no agent-jdi run tracked for this directory, it instead takes over the
    /// newest **unmanaged** claude/codex session here. If another agent is already
    /// live on that session it refuses (and prints the resume command) unless
    /// --force, which kills that agent first.
    Takeover {
        id: Option<String>,
        /// Only stop the supervisor; don't launch the interactive agent.
        #[arg(long)]
        no_launch: bool,
        /// Kill the agent currently holding the session, then take it over.
        #[arg(long)]
        force: bool,
        /// Resume with approvals ON (prompt per action). Default keeps the run's
        /// unattended posture (Claude: --dangerously-skip-permissions).
        #[arg(long)]
        supervised: bool,
    },
    /// Hand THIS interactive session over to an unattended agent-jdi run — the
    /// mirror of `takeover`. Run from inside a claude/codex session: it quits the
    /// session and resumes it in the background. Use --armed to quit it yourself.
    Handoff {
        /// Instruction folded into the unattended resume prompt.
        instruction: Vec<String>,
        /// Seconds between relaunch attempts (default 600).
        #[arg(long)]
        interval: Option<u64>,
        /// Give up after this many attempts (0 = never; default 0).
        #[arg(long)]
        max_attempts: Option<u32>,
        /// Arm only — don't quit the session for you (you press /exit).
        #[arg(long)]
        armed: bool,
        /// Pin this exact session id instead of auto-detecting it from the process.
        #[arg(long)]
        session: Option<String>,
    },
    /// Internal: the detached supervisor loop (do not call directly).
    #[command(name = "__run", hide = true)]
    Run { id: String },
    /// Internal: wait for the interactive session to exit, then resume (handoff).
    #[command(name = "__handoff", hide = true)]
    HandoffWait {
        #[arg(long)]
        watch_pid: u32,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        interval: Option<u64>,
        #[arg(long)]
        max_attempts: Option<u32>,
        #[arg(long)]
        agent: Option<Agent>,
        /// The session was sent SIGTERM — escalate to SIGKILL if it ignores it.
        #[arg(long)]
        escalate: bool,
        /// The pinned session id to resume (from `handoff`).
        #[arg(long)]
        session: Option<String>,
        /// Normalized Codex sandbox captured by the parent handoff command.
        #[arg(long, value_enum)]
        codex_sandbox: Option<codex::CodexSandboxMode>,
        /// Exact workspace-write network flag captured by the parent handoff.
        #[arg(long, requires = "codex_sandbox")]
        codex_workspace_network: Option<bool>,
        instruction: Vec<String>,
    },
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
            id,
            instruction,
            interval,
            max_attempts,
            follow,
            session,
        } => cmd_resume(
            &config,
            cli.agent,
            id.as_deref(),
            &instruction.join(" "),
            interval,
            max_attempts,
            dry,
            follow,
            session.as_deref(),
            None,
        ),
        Command::Log { id } => cmd_log(&config, id.as_deref()),
        Command::Status { id } => cmd_status(&config, id.as_deref()),
        Command::List => cmd_list(&config),
        Command::Backlog { message, id, drain } => {
            cmd_backlog(&config, id.as_deref(), &message.join(" "), dry, drain)
        }
        Command::Takeover {
            id,
            no_launch,
            force,
            supervised,
        } => cmd_takeover(
            &config,
            cli.agent,
            id.as_deref(),
            dry,
            no_launch,
            force,
            !supervised,
        ),
        Command::Handoff {
            instruction,
            interval,
            max_attempts,
            armed,
            session,
        } => cmd_handoff(
            &config,
            cli.agent,
            &instruction.join(" "),
            interval,
            max_attempts,
            armed,
            dry,
            session.as_deref(),
        ),
        Command::Run { id } => supervisor::run_loop(&config.home, &id),
        Command::HandoffWait {
            watch_pid,
            cwd,
            interval,
            max_attempts,
            agent,
            escalate,
            session,
            codex_sandbox,
            codex_workspace_network,
            instruction,
        } => cmd_handoff_wait(
            &config,
            watch_pid,
            &cwd,
            agent,
            &instruction.join(" "),
            interval,
            max_attempts,
            escalate,
            session.as_deref(),
            codex_sandbox,
            codex_workspace_network,
        ),
    }
}

/// Resolve the session for a command: an explicit id, else this directory's slot.
fn resolve_session(config: &Config, id: Option<&str>) -> Result<Session> {
    let sid = match id {
        Some(id) => {
            let mut components = Path::new(id).components();
            if id.contains('/')
                || id.contains('\\')
                || !matches!(
                    (components.next(), components.next()),
                    (Some(std::path::Component::Normal(_)), None)
                )
            {
                bail!(
                    "invalid tracked session id '{id}' — expected one name from `agent-jdi list`"
                );
            }
            id.to_string()
        }
        None => state::slot_id(&std::env::current_dir()?),
    };
    let s = Session::new(&config.home, &sid);
    if !s.exists() {
        bail!("no tracked session '{sid}' — run `agent-jdi resume` here first");
    }
    Ok(s)
}

struct ResumeTarget {
    slot: String,
    cwd: PathBuf,
    agent: Agent,
    resumable: agent::ResumableSession,
}

fn resolve_resume_target(
    config: &Config,
    forced: Option<Agent>,
    tracked_id: Option<&str>,
    raw_session_id: Option<&str>,
    current_cwd: &Path,
) -> Result<ResumeTarget> {
    if let Some(slot) = tracked_id {
        let tracked = resolve_session(config, Some(slot))?;
        let meta = |key: &str| {
            tracked.meta_get(key).ok_or_else(|| {
                anyhow::anyhow!("tracked session '{slot}' is missing required {key}= metadata")
            })
        };
        let cwd = PathBuf::from(meta("cwd")?);
        let recorded_agent = meta("agent")?;
        let agent = Agent::from_label(&recorded_agent).ok_or_else(|| {
            anyhow::anyhow!("tracked session '{slot}' has unsupported agent '{recorded_agent}'")
        })?;
        if let Some(forced) = forced {
            if forced != agent {
                bail!(
                    "tracked session '{slot}' uses agent {}, but --agent {} was requested",
                    agent.label(),
                    forced.label()
                );
            }
        }
        let session_id = meta("session_id")?;
        let transcript = PathBuf::from(meta("transcript")?);
        let idle_secs = std::fs::metadata(&transcript)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .map(|idle| idle.as_secs())
            .unwrap_or(0);
        return Ok(ResumeTarget {
            slot: slot.to_string(),
            cwd,
            agent,
            resumable: agent::ResumableSession {
                id: session_id,
                transcript,
                idle_secs,
            },
        });
    }

    let cwd = current_cwd.to_path_buf();
    let agent = detect::agent_for(&cwd, forced).ok_or_else(|| anyhow_no_session(&cwd))?;
    let adapter = agent::adapter(agent);
    let resumable = match raw_session_id {
        Some(id) => explicit_resumable(adapter.as_ref(), &cwd, id)?,
        None => adapter.discover_resumable(&cwd)?,
    };
    Ok(ResumeTarget {
        slot: state::slot_id(&cwd),
        cwd,
        agent,
        resumable,
    })
}

/// Build a `ResumableSession` for an explicit id (no discovery). Errors if no
/// transcript is recorded for it under this cwd.
fn explicit_resumable(
    adapter: &dyn agent::AgentAdapter,
    cwd: &Path,
    id: &str,
) -> Result<agent::ResumableSession> {
    let transcript = adapter
        .transcript_path(id, cwd)
        .with_context(|| format!("no transcript found for session {id}"))?;
    let idle_secs = std::fs::metadata(&transcript)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(agent::ResumableSession {
        id: id.to_string(),
        transcript,
        idle_secs,
    })
}

/// Seconds after which the newest session is "stale" enough to double-check (env
/// `AGENT_JDI_STALE`, default 1h — mirrors the bash `CLAUDE_JDI_STALE`).
fn stale_secs() -> u64 {
    std::env::var("AGENT_JDI_STALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600)
}

/// When the newest session is stale AND there's more than one to choose from AND a
/// human is at the terminal, show a numbered list and let them pick which to resume
/// (Enter = newest, `q` = abort). Otherwise return `newest` unchanged — so an
/// unattended run (no TTY), a single-session dir, or a fresh session never prompts.
fn confirm_if_stale(
    adapter: &dyn agent::AgentAdapter,
    cwd: &Path,
    newest: agent::ResumableSession,
) -> Result<agent::ResumableSession> {
    use std::io::{IsTerminal, Write};
    if newest.idle_secs <= stale_secs() {
        return Ok(newest);
    }
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "agent-jdi: newest session here is {} old and there's no TTY to confirm — \
             using it (pass --session <id> to choose).",
            human_ago(newest.idle_secs)
        );
        return Ok(newest);
    }
    let sessions = adapter.sessions_for_cwd(cwd);
    if sessions.len() <= 1 {
        return Ok(newest); // nothing to choose between
    }
    eprintln!(
        "The newest session here was last active {} ago — pick which to resume:",
        human_ago(newest.idle_secs)
    );
    for (i, s) in sessions.iter().enumerate() {
        let id8: String = s.id.chars().take(8).collect();
        eprintln!(
            "  {:>2}) last active {:>6} ago  {id8}…  {}",
            i + 1,
            human_ago(s.idle_secs),
            s.snippet
        );
    }
    eprint!(
        "Pick [1-{}], Enter for 1 (newest), q to abort: ",
        sessions.len()
    );
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let choice = line.trim();
    if choice.eq_ignore_ascii_case("q") {
        bail!("aborted — no session chosen.");
    }
    let idx = if choice.is_empty() {
        0
    } else {
        choice
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=sessions.len()).contains(n))
            .map(|n| n - 1)
            .ok_or_else(|| anyhow::anyhow!("invalid choice — aborting."))?
    };
    explicit_resumable(adapter, cwd, &sessions[idx].id)
}

#[allow(clippy::too_many_arguments)]
fn cmd_resume(
    config: &Config,
    forced: Option<Agent>,
    tracked_id: Option<&str>,
    instruction: &str,
    interval: Option<u64>,
    max_attempts: Option<u32>,
    dry_run: bool,
    follow: bool,
    session: Option<&str>,
    handoff_permissions: Option<&codex::CodexPermissionSnapshot>,
) -> Result<()> {
    let current_cwd = std::env::current_dir()?;
    let ResumeTarget {
        slot,
        cwd,
        agent,
        mut resumable,
    } = resolve_resume_target(config, forced, tracked_id, session, &current_cwd)?;
    let adapter = agent::adapter(agent);
    adapter.preflight()?;
    if tracked_id.is_none() && session.is_none() {
        resumable = confirm_if_stale(adapter.as_ref(), &cwd, resumable)?;
    }

    // `--dry-run`: show what would run, with no side effects (no slot, no spawn, no
    // viewer). Safe way to verify agent detection + the exact invocation.
    if dry_run {
        let brief = agent::Brief {
            text: instruction.to_string(),
            backlog: Vec::new(),
            // Same path the real run uses, so the previewed prompt matches it.
            checklist: Some(Session::new(&config.home, &slot).dir.join("checklist.md")),
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
    persist_codex_permissions(&session, agent, handoff_permissions)?;

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

fn persist_codex_permissions(
    session: &Session,
    agent: Agent,
    permissions: Option<&codex::CodexPermissionSnapshot>,
) -> Result<()> {
    if agent != Agent::Codex {
        return Ok(());
    }
    session.ensure_dir()?;
    match permissions {
        Some(snapshot) => {
            let args = snapshot.config_args();
            std::fs::write(session.cargs_path(), format!("{}\n", args.join("\n")))
                .with_context(|| format!("write {}", session.cargs_path().display()))?;
            session.meta_set(
                "permissions",
                &format!("{} (preserved from current Codex turn)", snapshot.summary()),
            )?;
        }
        None => {
            match std::fs::remove_file(session.cargs_path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            session.meta_set("permissions", "workspace-write, network disabled (default)")?;
        }
    }
    Ok(())
}

fn handoff_permission_args(permissions: Option<&codex::CodexPermissionSnapshot>) -> Vec<String> {
    let Some(snapshot) = permissions else {
        return Vec::new();
    };
    let mut args = vec![
        "--codex-sandbox".to_owned(),
        snapshot.sandbox().as_config_value().to_owned(),
    ];
    if let Some(enabled) = snapshot.workspace_network() {
        args.push("--codex-workspace-network".to_owned());
        args.push(enabled.to_string());
    }
    args
}

fn handoff_permission_snapshot(
    agent: Agent,
    session_id: Option<&str>,
    transcript: Option<&Path>,
) -> Result<Option<codex::CodexPermissionSnapshot>> {
    if agent != Agent::Codex {
        return Ok(None);
    }
    let session_id = session_id.ok_or_else(|| {
        anyhow::anyhow!(
            "cannot preserve the current Codex permission context without a pinned session id"
        )
    })?;
    let transcript = transcript.ok_or_else(|| {
        anyhow::anyhow!(
            "cannot locate Codex rollout for {session_id}; current session remains active"
        )
    })?;
    codex::CodexPermissionSnapshot::from_rollout(transcript).map(Some)
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
         watch:      agent-jdi log {slot}\n  \
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
        checklist: Some(
            Session::new(&config.home, &state::slot_id(&cwd))
                .dir
                .join("checklist.md"),
        ),
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

fn cmd_backlog(
    config: &Config,
    id: Option<&str>,
    message: &str,
    dry_run: bool,
    drain: bool,
) -> Result<()> {
    let session = resolve_session(config, id)?;
    let bl = backlog::Backlog::new(session.backlog_root());

    if !message.trim().is_empty() {
        if dry_run {
            println!(
                "[dry-run] would queue for {}: {message}",
                session.dir.display()
            );
        } else {
            println!("queued: {}", bl.add(message)?.display());
        }
    }

    let (pending, draining) = (bl.pending_count(), bl.draining_count());
    if message.trim().is_empty() && !drain {
        println!("backlog: {pending} pending, {draining} draining");
        return Ok(());
    }

    if !drain {
        // A live worker drains on its own once the current work finishes.
        if session.alive() {
            println!("the running supervisor will drain it when its current work finishes.");
        } else {
            println!(
                "no supervisor is running — drain it with `agent-jdi backlog --drain`{}.",
                id.map(|i| format!(" --id {i}")).unwrap_or_default()
            );
        }
        return Ok(());
    }

    // --drain: a live worker already picks the queue up, so never start a second one.
    if session.alive() {
        println!(
            "a supervisor is already running (pid {}) — it drains the queue when its \
             current work finishes.",
            session.pid().unwrap_or(0)
        );
        return Ok(());
    }
    if pending + draining == 0 {
        println!("backlog is empty — nothing to drain.");
        return Ok(());
    }
    if dry_run {
        println!(
            "[dry-run] would relaunch {} to drain {pending} pending + {draining} in-flight item(s)",
            session.dir.display()
        );
        return Ok(());
    }

    let cwd = session
        .meta_get("cwd")
        .map(PathBuf::from)
        .unwrap_or_default();
    guard_no_conflict(config, &cwd)?;
    let agent = session
        .meta_get("agent")
        .and_then(|a| Agent::from_label(&a))
        .context("session meta has no agent")?;
    // Start the worker straight in the drain mode; run_loop claims the items.
    session.meta_set(
        "mode",
        agent::adapter(agent)
            .initial_mode(agent::Trigger::BacklogDrain)
            .as_str(),
    )?;
    session.meta_set("state", "draining")?;
    let slot = session.meta_get("id").unwrap_or_else(|| {
        session
            .dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("session")
            .to_string()
    });
    let pid = supervisor::spawn_detached(&config.home, &slot)?;
    session.meta_set("pid", &pid.to_string())?;
    session.meta_stamp("started");
    session.meta_set("finished", "")?;
    println!("draining {pending} pending + {draining} in-flight item(s) — worker {pid}.");
    println!("  watch:  agent-jdi log {} -f", slot);
    println!("  check:  agent-jdi status {}", slot);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_takeover(
    config: &Config,
    forced: Option<Agent>,
    id: Option<&str>,
    dry_run: bool,
    no_launch: bool,
    force: bool,
    autonomous: bool,
) -> Result<()> {
    // Nothing agent-jdi-managed for this directory → take over the newest
    // *unmanaged* claude/codex session here instead of erroring out.
    if id.is_none() {
        let slot = state::slot_id(&std::env::current_dir()?);
        if !Session::new(&config.home, &slot).exists() {
            return takeover_unmanaged(forced, dry_run, no_launch, force, autonomous);
        }
    }
    let session = resolve_session(config, id)?;
    // The interactive resume to hand the human, unless --no-launch (or the agent
    // can't be resumed interactively / has no id yet).
    let sid = session.meta_get("session_id").unwrap_or_default();
    let cwd = session.meta_get("cwd").map(PathBuf::from);
    let interactive = if no_launch {
        None
    } else {
        match (
            session
                .meta_get("agent")
                .and_then(|a| Agent::from_label(&a)),
            &cwd,
        ) {
            (Some(a), Some(c)) => agent::adapter(a).interactive_invocation(&sid, c, autonomous),
            _ => None,
        }
    };

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
        if let Some(inv) = &interactive {
            println!(
                "[dry-run] then hand you: {} {}",
                inv.program.display(),
                shell_join(&inv.args)
            );
        }
        return Ok(());
    }

    supervisor::takeover(&session)?;

    // Rich "resume it yourself" block (claude-jdi parity): the session slot, how to
    // resume by hand (autonomous vs supervised), and where to see remaining work.
    let slot = session
        .meta_get("id")
        .or_else(|| {
            session
                .dir
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "session".into());
    let cmds = session
        .meta_get("agent")
        .and_then(|a| Agent::from_label(&a))
        .map(|a| agent::adapter(a).resume_commands(&sid))
        .unwrap_or_default();

    println!("■ stopped session: {slot}\n");
    if let (Some(c), false) = (&cwd, cmds.is_empty()) {
        println!("Resume it yourself (full context preserved):\n");
        println!("  cd {}\n", c.display());
        for (comment, cmd) in &cmds {
            println!("  {comment}");
            println!("  {cmd}\n");
        }
    }
    println!("See remaining work via: agent-jdi status {slot}");

    // Default: also launch the supervised resume so you continue right away. The
    // block above stays in the scrollback for when you exit the agent.
    if let Some(inv) = interactive {
        let cwd = cwd.expect("interactive implies a cwd");
        println!("\nLaunching the supervised resume now… (--no-launch to skip)\n");
        // Inherit the terminal so the agent's interactive UI takes over; exit with
        // its status when the human is done.
        let status = std::process::Command::new(&inv.program)
            .args(&inv.args)
            .current_dir(&cwd)
            .status()
            .with_context(|| format!("launch {}", inv.program.display()))?;
        std::process::exit(status.code().unwrap_or(0));
    }
    Ok(())
}

/// Take over the newest **unmanaged** claude/codex session for this directory —
/// one agent-jdi never supervised. Discovers it (same agent as the latest session),
/// refuses if another agent is already live on it (unless `--force`, which kills
/// that agent first), then launches the interactive resume — or, with
/// `--no-launch`, just prints the commands to resume it by hand.
fn takeover_unmanaged(
    forced: Option<Agent>,
    dry_run: bool,
    no_launch: bool,
    force: bool,
    autonomous: bool,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let agent = detect::agent_for(&cwd, forced).ok_or_else(|| anyhow_no_session(&cwd))?;
    let adapter = agent::adapter(agent);
    let resumable = adapter.discover_resumable(&cwd)?;
    let bin = match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    };
    let live = live_agent_for_session(bin, &resumable.id, &cwd)?;
    let cmds = adapter.resume_commands(&resumable.id);

    if dry_run {
        println!(
            "[dry-run] takeover (unmanaged): agent={} cwd={}",
            agent.label(),
            cwd.display()
        );
        println!(
            "[dry-run] newest session: {}  (last active {} ago)",
            resumable.id,
            human_ago(resumable.idle_secs)
        );
        match live {
            Some(pid) if force => println!("[dry-run] would kill the live {bin} (pid {pid})"),
            Some(pid) => println!("[dry-run] would REFUSE: {bin} pid {pid} is live (use --force)"),
            None => println!("[dry-run] no live {bin} holds this session"),
        }
        println!(
            "[dry-run] then {}",
            if no_launch {
                "print the resume commands"
            } else {
                "launch the interactive resume"
            }
        );
        return Ok(());
    }

    // Another agent already drives this transcript — two would corrupt it.
    if let Some(pid) = live {
        if !force {
            println!(
                "■ session {} is already live ({bin} pid {pid})\n",
                resumable.id
            );
            if !cmds.is_empty() {
                println!("Attach to it yourself instead:\n");
                println!("  cd {}\n", cwd.display());
                for (comment, cmd) in &cmds {
                    println!("  {comment}");
                    println!("  {cmd}\n");
                }
            }
            bail!(
                "refusing to take over a live session — re-run with --force to kill {bin} pid {pid} first."
            );
        }
        eprintln!("killing the live {bin} (pid {pid}) holding this session…");
        kill_and_wait(pid);
    }

    println!("■ taking over session: {}", resumable.id);
    println!("  cwd:      {}", cwd.display());
    println!("  agent:    {}", agent.label());
    println!("  transcript: {}\n", resumable.transcript.display());

    if no_launch {
        if !cmds.is_empty() {
            println!("Resume it yourself (full context preserved):\n");
            println!("  cd {}\n", cwd.display());
            for (comment, cmd) in &cmds {
                println!("  {comment}");
                println!("  {cmd}\n");
            }
        }
        return Ok(());
    }

    let Some(inv) = adapter.interactive_invocation(&resumable.id, &cwd, autonomous) else {
        bail!("no interactive resume available for {}", agent.label());
    };
    println!("Launching the supervised resume now… (--no-launch to skip)\n");
    let status = std::process::Command::new(&inv.program)
        .args(&inv.args)
        .current_dir(&cwd)
        .status()
        .with_context(|| format!("launch {}", inv.program.display()))?;
    std::process::exit(status.code().unwrap_or(0));
}

/// SIGTERM a pid and wait for it to go, escalating to SIGKILL after a grace period,
/// so the transcript is free before we resume it.
fn kill_and_wait(pid: u32) {
    let signal = |sig: &str| {
        std::process::Command::new("kill")
            .arg(sig)
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok();
    };
    signal("-TERM");
    for i in 0..15 {
        if !state::pid_alive(pid) {
            return;
        }
        if i == 9 {
            signal("-KILL");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The live agent process (if any) driving `session_id` in `cwd`: an agent process
/// whose argv names the session, else one whose working directory is `cwd`. Used to
/// refuse taking over a transcript another agent already holds.
/// Two subprocess calls at most, never one-per-pid: a single `ps` for the agent
/// processes and their argv, then — only if no argv named the session — a single
/// batched `lsof` for every agent process's working directory.
#[cfg(unix)]
fn live_agent_for_session(bin: &str, session_id: &str, cwd: &Path) -> Result<Option<u32>> {
    let want = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let me = std::process::id();

    // Pass 1: agent processes + their command lines, in one `ps`.
    let out = std::process::Command::new("ps")
        .args(["-Ao", "pid=,comm=,args="])
        .output()
        .context("list live agent processes")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut agent_pids: Vec<u32> = Vec::new();
    let mut exact_pids: Vec<u32> = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let Some((pid_s, rest)) = t.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_s.parse::<u32>() else {
            continue;
        };
        if pid == me {
            continue;
        }
        let rest = rest.trim_start();
        let Some((comm, args)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if comm_name(comm) != bin {
            continue;
        }
        // Strongest signal: the session id appears in its argv (`--resume <id>`).
        if !session_id.is_empty() && args.split_whitespace().any(|token| token == session_id) {
            exact_pids.push(pid);
        }
        agent_pids.push(pid);
    }
    if !exact_pids.is_empty() {
        return select_live_agent(&exact_pids, &[]);
    }

    // Pass 2: match on working directory, batched over all agent processes.
    let cwd_pids = agent_cwds(bin)
        .into_iter()
        .filter(|(pid, dir)| agent_pids.contains(pid) && *dir == want)
        .map(|(pid, _)| pid)
        .collect::<Vec<_>>();
    select_live_agent(&[], &cwd_pids)
}

#[cfg(not(unix))]
fn live_agent_for_session(_bin: &str, _session_id: &str, _cwd: &Path) -> Result<Option<u32>> {
    Ok(None)
}

fn select_live_agent(exact_pids: &[u32], cwd_pids: &[u32]) -> Result<Option<u32>> {
    let candidates = if exact_pids.is_empty() {
        cwd_pids
    } else {
        exact_pids
    };
    match candidates {
        [] => Ok(None),
        [pid] => Ok(Some(*pid)),
        many => {
            let pids = many
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "ambiguous live agent processes ({pids}) for this session — \
                 refusing to choose or kill one; close the extra session or pass an exact session id"
            )
        }
    }
}

/// `(pid, cwd)` for every process named `bin` — one call, not one per pid.
#[cfg(unix)]
fn agent_cwds(bin: &str) -> Vec<(u32, PathBuf)> {
    #[cfg(target_os = "linux")]
    {
        // /proc is cheap enough to read directly; no subprocess at all.
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return out;
        };
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            if comm_name(comm.trim()) != bin {
                continue;
            }
            if let Ok(dir) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
                out.push((pid, dir));
            }
        }
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        // `lsof -c <bin> -d cwd -Fpn` emits `p<pid>` then `n<dir>` for ALL matching
        // processes in one shot.
        let Ok(out) = std::process::Command::new("lsof")
            .args(["-a", "-d", "cwd", "-Fpn", "-c"])
            .arg(bin)
            .output()
        else {
            return Vec::new();
        };
        let mut res = Vec::new();
        let mut cur: Option<u32> = None;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(p) = line.strip_prefix('p') {
                cur = p.trim().parse().ok();
            } else if let Some(n) = line.strip_prefix('n') {
                if let Some(pid) = cur {
                    res.push((pid, PathBuf::from(n.trim())));
                }
            }
        }
        res
    }
}

/// Hand the current interactive session to an unattended run: arm a detached
/// watcher that resumes it once the session exits, then (by default) quit the
/// session for you. The mirror of `takeover`.
#[allow(clippy::too_many_arguments)]
fn cmd_handoff(
    config: &Config,
    forced: Option<Agent>,
    instruction: &str,
    interval: Option<u64>,
    max_attempts: Option<u32>,
    armed: bool,
    dry_run: bool,
    session: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    // This runs inside a live agent turn, so do the bare minimum here: ONE `ps` to
    // find the session we're inside — which also tells us the agent, so nothing has
    // to scan sessions on disk. Discovery, the conflict guard and the resume itself
    // are all deferred to the headless watcher.
    let found = ancestor_agent(forced);

    // Pin the exact session id NOW, so the deferred resume targets the session this
    // handoff came from — never "whatever's newest later", which could be a sibling
    // session in the same dir. Air-tight order: an explicit id → the id in our own
    // `CODEX_THREAD_ID` exposed by Codex itself → the id in our own agent process's
    // argv (`--resume <id>` / `codex resume <id>`) → the newest for this cwd captured
    // at arm time (we are that session, so it's newest right now).
    let session_id = found.and_then(|(pid, agent)| {
        let thread_env = std::env::var("CODEX_THREAD_ID").ok();
        let argv_id = session_id_from_argv(pid, agent);
        pinned_handoff_session_id(session, agent, thread_env.as_deref(), argv_id.as_deref())
            .or_else(|| {
                agent::adapter(agent)
                    .discover_resumable(&cwd)
                    .ok()
                    .map(|r| r.id)
            })
    });

    // Codex handoff is fail-closed: capture the current turn's exact permission
    // policy before spawning the watcher or terminating the interactive process.
    // This keeps a malformed/missing rollout from silently reducing capability or
    // escalating permissions in the resumed run.
    let codex_permissions = match found {
        Some((_, agent)) => {
            let transcript = session_id
                .as_deref()
                .and_then(|id| agent::adapter(agent).transcript_path(id, &cwd));
            handoff_permission_snapshot(agent, session_id.as_deref(), transcript.as_deref())?
        }
        None => None,
    };

    if dry_run {
        match found {
            Some((pid, agent)) => {
                println!(
                    "[dry-run] handoff: agent={} cwd={}",
                    agent.label(),
                    cwd.display()
                );
                let target = session_id
                    .as_deref()
                    .map(|s| format!("--session {s}"))
                    .unwrap_or_else(|| "(newest at drain time)".into());
                println!(
                    "[dry-run] would watch pid {pid}; on its exit run: agent-jdi resume {target} {instruction}"
                );
                if let Some(permissions) = codex_permissions.as_ref() {
                    println!(
                        "[dry-run] permissions: {} (preserved from current Codex turn)",
                        permissions.summary()
                    );
                }
            }
            None => println!("[dry-run] (not inside a claude/codex session — nothing to hand off)"),
        }
        println!(
            "[dry-run] then {}",
            if armed {
                "wait for you to quit"
            } else {
                "quit this session (SIGTERM)"
            }
        );
        return Ok(());
    }

    let Some((watch_pid, agent)) = found else {
        bail!(
            "couldn't find the interactive claude/codex process to hand off — run \
             `agent-jdi handoff` from inside a session, or quit and use `agent-jdi resume`."
        );
    };

    // Spawn the detached watcher (its own process group, so it survives the session).
    let exe = std::env::current_exe().context("locate agent-jdi executable")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("__handoff")
        .arg("--watch-pid")
        .arg(watch_pid.to_string())
        .arg("--cwd")
        .arg(&cwd)
        .arg("--agent")
        .arg(agent.label());
    cmd.args(handoff_permission_args(codex_permissions.as_ref()));
    if let Some(i) = interval {
        cmd.arg("--interval").arg(i.to_string());
    }
    if let Some(m) = max_attempts {
        cmd.arg("--max-attempts").arg(m.to_string());
    }
    if let Some(sid) = &session_id {
        cmd.arg("--session").arg(sid);
    }
    if !armed {
        // We're about to SIGTERM the session; let the watcher escalate if needed.
        cmd.arg("--escalate");
    }
    if !instruction.is_empty() {
        cmd.arg("--").arg(instruction);
    }
    cmd.env("AGENT_JDI_HOME", &config.home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().context("spawn handoff watcher")?;

    println!(
        "▶ handoff armed for this {} session (pid {watch_pid}).",
        agent.label()
    );
    println!(
        "  when it exits, agent-jdi resumes it unattended in {}.",
        cwd.display()
    );
    if let Some(permissions) = codex_permissions.as_ref() {
        println!(
            "  permissions: {} (preserved from current Codex turn).",
            permissions.summary()
        );
    }
    if armed {
        println!("  quit now (/exit or Ctrl-D) to hand off.");
    } else {
        println!("  quitting this session now…");
        // SIGTERM the interactive agent; the watcher then resumes it. (Spawn the
        // watcher first — above — so it's independent of this session's death.)
        std::process::Command::new("kill")
            .arg("-TERM")
            .arg(watch_pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok();
    }
    Ok(())
}

fn pinned_handoff_session_id(
    explicit: Option<&str>,
    agent: Agent,
    codex_thread_id: Option<&str>,
    argv_id: Option<&str>,
) -> Option<String> {
    explicit
        .filter(|id| !id.is_empty())
        .or_else(|| {
            (agent == Agent::Codex)
                .then_some(codex_thread_id)
                .flatten()
                .filter(|id| !id.is_empty())
        })
        .or_else(|| argv_id.filter(|id| !id.is_empty()))
        .map(str::to_string)
}

/// The detached handoff watcher: wait for the interactive session (`watch_pid`) to
/// exit, then resume it unattended in `cwd`. Runs with no TTY (stdio is /dev/null).
#[allow(clippy::too_many_arguments)]
fn cmd_handoff_wait(
    config: &Config,
    watch_pid: u32,
    cwd: &Path,
    agent: Option<Agent>,
    instruction: &str,
    interval: Option<u64>,
    max_attempts: Option<u32>,
    escalate: bool,
    session: Option<&str>,
    codex_sandbox: Option<codex::CodexSandboxMode>,
    codex_workspace_network: Option<bool>,
) -> Result<()> {
    // Wait for the session to exit, capped at ~2h so a stuck watcher can't linger.
    // If we asked it to quit (SIGTERM) and it's still alive after a grace period,
    // escalate to SIGKILL — otherwise an agent that ignores SIGTERM would leave the
    // handoff armed forever.
    const GRACE_SECS: usize = 10;
    for elapsed in 0..7200usize {
        if !state::pid_alive(watch_pid) {
            break;
        }
        if escalate && elapsed == GRACE_SECS {
            std::process::Command::new("kill")
                .arg("-KILL")
                .arg(watch_pid.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .ok();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    // Let the transcript flush before we discover + resume it.
    std::thread::sleep(Duration::from_secs(1));
    std::env::set_current_dir(cwd).with_context(|| format!("chdir into {}", cwd.display()))?;
    let codex_permissions = codex_sandbox
        .map(|sandbox| {
            codex::CodexPermissionSnapshot::from_handoff_parts(sandbox, codex_workspace_network)
        })
        .transpose()?;
    // Resume unattended (no follow — this process has no terminal), pinned to the
    // exact session `handoff` captured, so no discovery/picker can pick a sibling.
    cmd_resume(
        config,
        agent,
        None,
        instruction,
        interval,
        max_attempts,
        false,
        false,
        session,
        codex_permissions.as_ref(),
    )
}

/// `(pid, ppid, executable name)` for every process, from **one** `ps` call.
///
/// `handoff` runs inside a live agent turn, where every subprocess spawn is latency
/// the human watches (~100ms each on macOS). Walking the ancestry with a `ps` per
/// level cost ~1.2s; one table lookup costs ~0.1s.
#[cfg(unix)]
fn process_table() -> Vec<(u32, u32, String)> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-Ao", "pid=,ppid=,comm="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let t = line.trim_start();
            let (pid, rest) = t.split_once(char::is_whitespace)?;
            let (ppid, comm) = rest.trim_start().split_once(char::is_whitespace)?;
            Some((
                pid.parse().ok()?,
                ppid.parse().ok()?,
                comm.trim().to_string(),
            ))
        })
        .collect()
}

/// The executable's bare name: `…/bin/claude` → `claude`, and a login shell's
/// `-zsh` → `zsh`.
fn comm_name(comm: &str) -> &str {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
}

/// The nearest ancestor process that **is** an agent binary, plus which agent it is
/// — i.e. the interactive session we're running inside. Identifying the agent from
/// the process itself means `handoff` never has to scan sessions on disk to detect
/// it. `forced` restricts the search to one agent. Unix-only.
#[cfg(unix)]
fn ancestor_agent(forced: Option<Agent>) -> Option<(u32, Agent)> {
    // Targeted per-level lookups (ppid + comm in ONE `ps` each) — cheaper than
    // dumping the whole process table, and only a handful of levels deep.
    let mut pid = std::process::id();
    for _ in 0..64 {
        let (ppid, comm) = ps_parent(pid)?;
        if ppid <= 1 {
            return None;
        }
        let found = match comm_name(&comm) {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            _ => None,
        };
        if let Some(a) = found {
            if forced.is_none_or(|f| f == a) {
                return Some((ppid, a));
            }
        }
        pid = ppid;
    }
    None
}

/// `(ppid, comm-of-that-parent)` for `pid` in a single `ps` call.
#[cfg(unix)]
fn ps_parent(pid: u32) -> Option<(u32, String)> {
    let ppid: u32 = ps_field(pid, "ppid=")?.trim().parse().ok()?;
    if ppid <= 1 {
        return Some((ppid, String::new()));
    }
    Some((ppid, ps_field(ppid, "comm=").unwrap_or_default()))
}

#[cfg(not(unix))]
fn ancestor_agent(_forced: Option<Agent>) -> Option<(u32, Agent)> {
    None
}

/// The session id in the agent process's own command line — the most precise source
/// there is, since it's literally the id that session is running under. Claude passes
/// `--resume <id>` / `--session-id <id>`; Codex `resume <id>`. `None` for a fresh
/// interactive session that carries no id in its argv (caller then falls back to
/// discovery).
#[cfg(unix)]
fn session_id_from_argv(pid: u32, agent: Agent) -> Option<String> {
    session_id_in_cmdline(&ps_field(pid, "command=")?, agent)
}

#[cfg(not(unix))]
fn session_id_from_argv(_pid: u32, _agent: Agent) -> Option<String> {
    None
}

/// The session id in an agent command line: the token after `--resume`/`--session-id`
/// (Claude) or `resume` (Codex). Pure, so it's unit-testable independent of process
/// state. `None` when no id flag is present (a fresh interactive session).
fn session_id_in_cmdline(cmdline: &str, agent: Agent) -> Option<String> {
    let toks: Vec<&str> = cmdline.split_whitespace().collect();
    let flags: &[&str] = match agent {
        Agent::Claude => &["--resume", "--session-id"],
        Agent::Codex => &["resume"],
    };
    let looks_like_id =
        |s: &str| s.len() >= 8 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    toks.windows(2)
        .find_map(|w| (flags.contains(&w[0]) && looks_like_id(w[1])).then(|| w[1].to_string()))
}

/// Is `pid` the agent process itself? Compares the **executable name** (`ps -o comm=`)
/// and never the full command line.
///
/// This distinction is load-bearing: a Claude Code tool shell runs
/// `zsh -c source ~/.claude/shell-snapshots/snapshot-….sh`, whose *argv* contains the
/// substring "claude". Matching argv therefore selected that shell — the nearest
/// ancestor — instead of the session, so `handoff` signalled the shell (leaving the
/// TUI alive but broken) and its watcher, seeing that shell die at once, fired the
/// unattended resume *while the interactive session was still running* — two agents
/// on one transcript, draining the task queue underneath it.
#[cfg(unix)]
fn is_agent_process(pid: u32, agent_bin: &str) -> bool {
    let Some(comm) = ps_field(pid, "comm=") else {
        return false;
    };
    // `comm` may be a path (…/bin/claude) and a login shell shows as `-zsh`.
    let name = comm
        .rsplit('/')
        .next()
        .unwrap_or(&comm)
        .trim_start_matches('-');
    name == agent_bin
}

#[cfg(not(unix))]
fn ancestor_pid_running(_needle: &str) -> Option<u32> {
    None
}

/// One `ps -o <field> -p <pid>` value (e.g. `ppid=`, `command=`), trimmed.
#[cfg(unix)]
fn ps_field(pid: u32, field: &str) -> Option<String> {
    let out = std::process::Command::new("ps")
        .arg("-o")
        .arg(field)
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
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
    fn handoff_permissions_are_persisted_and_external_resume_clears_them() {
        let root = std::env::temp_dir().join(format!(
            "agent-jdi-permission-state-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        let session = Session::new(&root, "slot");
        let full = codex::CodexPermissionSnapshot::from_handoff_parts(
            codex::CodexSandboxMode::DangerFullAccess,
            None,
        )
        .unwrap();

        persist_codex_permissions(&session, Agent::Codex, Some(&full)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session.cargs_path()).unwrap(),
            "-c\nsandbox_mode=\"danger-full-access\"\n"
        );
        assert_eq!(
            session.meta_get("permissions").as_deref(),
            Some("danger-full-access (preserved from current Codex turn)")
        );

        persist_codex_permissions(&session, Agent::Codex, None).unwrap();
        assert!(!session.cargs_path().exists());
        assert_eq!(
            session.meta_get("permissions").as_deref(),
            Some("workspace-write, network disabled (default)")
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn codex_permission_persistence_does_not_touch_claude_cargs() {
        let root = std::env::temp_dir().join(format!(
            "agent-jdi-claude-permission-state-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&root).ok();
        let session = Session::new(&root, "slot");
        session.ensure_dir().unwrap();
        std::fs::write(session.cargs_path(), "--existing-claude-arg\n").unwrap();

        persist_codex_permissions(&session, Agent::Claude, None).unwrap();

        assert_eq!(
            std::fs::read_to_string(session.cargs_path()).unwrap(),
            "--existing-claude-arg\n"
        );
        assert_eq!(session.meta_get("permissions"), None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn watcher_args_carry_normalized_workspace_network_policy() {
        let snapshot = codex::CodexPermissionSnapshot::from_handoff_parts(
            codex::CodexSandboxMode::WorkspaceWrite,
            Some(true),
        )
        .unwrap();
        assert_eq!(
            handoff_permission_args(Some(&snapshot)),
            [
                "--codex-sandbox",
                "workspace-write",
                "--codex-workspace-network",
                "true",
            ]
        );
    }

    #[test]
    fn watcher_args_omit_network_for_full_access_and_claude() {
        let full = codex::CodexPermissionSnapshot::from_handoff_parts(
            codex::CodexSandboxMode::DangerFullAccess,
            None,
        )
        .unwrap();
        assert_eq!(
            handoff_permission_args(Some(&full)),
            ["--codex-sandbox", "danger-full-access"]
        );
        assert!(handoff_permission_args(None).is_empty());
    }

    #[test]
    fn codex_handoff_fails_closed_without_a_pinned_transcript() {
        let error = handoff_permission_snapshot(Agent::Codex, None, None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot preserve the current Codex permission context"),
            "{error:#}"
        );
        let error =
            handoff_permission_snapshot(Agent::Codex, Some("thread-123"), None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot locate Codex rollout for thread-123"),
            "{error:#}"
        );
    }

    #[test]
    fn claude_handoff_does_not_require_a_codex_permission_snapshot() {
        assert_eq!(
            handoff_permission_snapshot(Agent::Claude, None, None).unwrap(),
            None
        );
    }

    /// Regression: a Claude Code tool shell runs
    /// `zsh -c source ~/.claude/shell-snapshots/…`, so its *argv* contains "claude".
    /// Matching argv made `handoff` target that shell instead of the session — it
    /// signalled the shell (TUI left alive but wedged) and the watcher, seeing the
    /// shell die instantly, fired the unattended resume while the session was still
    /// running, draining its task queue. Identity must come from the executable
    /// name (`comm`), never the command line.
    #[cfg(unix)]
    #[test]
    fn a_shell_whose_argv_mentions_claude_is_not_the_agent() {
        // A shell whose command line contains "claude" — like the tool shell.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 5 # /Users/x/.claude/shell-snapshots/snapshot-zsh-1.sh")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn decoy shell");
        let pid = child.id();
        // Its argv really does contain the substring the old matcher keyed on...
        let argv = ps_field(pid, "command=").unwrap_or_default();
        assert!(
            argv.contains("claude"),
            "decoy argv should mention claude: {argv}"
        );
        // ...but it is NOT the agent: comm is `sh`, not `claude`.
        assert!(
            !is_agent_process(pid, "claude"),
            "a shell that merely mentions claude in argv must not match: {argv}"
        );
        // And our own test process isn't the agent either.
        assert!(!is_agent_process(std::process::id(), "claude"));
        child.kill().ok();
        child.wait().ok();
    }

    /// Handoff is air-tight because it reads the session id from the agent's own
    /// command line — not from "newest at drain time", which could be a sibling.
    #[test]
    fn session_id_parsed_from_the_agents_own_command_line() {
        // Claude: --resume / --session-id.
        assert_eq!(
            session_id_in_cmdline(
                "claude --resume 094539f2-40d7-4703-a510-8c3ee69657a4 --dangerously-skip-permissions",
                Agent::Claude
            )
            .as_deref(),
            Some("094539f2-40d7-4703-a510-8c3ee69657a4")
        );
        assert_eq!(
            session_id_in_cmdline("claude --session-id abc12345 -p hi", Agent::Claude).as_deref(),
            Some("abc12345")
        );
        // Codex: the `resume <id>` subcommand.
        assert_eq!(
            session_id_in_cmdline("codex resume 019f7ff6-2664-7263", Agent::Codex).as_deref(),
            Some("019f7ff6-2664-7263")
        );
        // A fresh interactive session carries no id → None (caller falls back).
        assert_eq!(session_id_in_cmdline("claude", Agent::Claude), None);
        assert_eq!(session_id_in_cmdline("codex", Agent::Codex), None);
        // Don't mistake a non-id token (a path, a prompt word) for the id.
        assert_eq!(
            session_id_in_cmdline("claude --resume ./notes -p go", Agent::Claude),
            None,
            "`./notes` is not id-shaped"
        );
    }

    #[test]
    fn codex_handoff_prefers_thread_environment_over_argv_and_discovery() {
        assert_eq!(
            pinned_handoff_session_id(None, Agent::Codex, Some("env-thread"), Some("argv-thread"),)
                .as_deref(),
            Some("env-thread")
        );
        assert_eq!(
            pinned_handoff_session_id(
                Some("explicit-thread"),
                Agent::Codex,
                Some("env-thread"),
                Some("argv-thread"),
            )
            .as_deref(),
            Some("explicit-thread")
        );
        assert_eq!(
            pinned_handoff_session_id(
                None,
                Agent::Claude,
                Some("stale-codex-env"),
                Some("claude-id")
            )
            .as_deref(),
            Some("claude-id"),
            "Codex environment must never leak into Claude handoff"
        );
        assert_eq!(
            pinned_handoff_session_id(None, Agent::Codex, None, None),
            None,
            "None tells the caller to use cwd-scoped discovery"
        );
    }

    #[test]
    fn live_agent_selection_refuses_ambiguous_same_cwd_processes() {
        assert_eq!(select_live_agent(&[], &[]).unwrap(), None);
        assert_eq!(select_live_agent(&[], &[41]).unwrap(), Some(41));
        assert_eq!(select_live_agent(&[17], &[41, 42]).unwrap(), Some(17));

        let error = select_live_agent(&[], &[41, 42]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("ambiguous"), "{message}");
        assert!(
            message.contains("41") && message.contains("42"),
            "{message}"
        );

        let error = select_live_agent(&[17, 18], &[]).unwrap_err();
        assert!(error.to_string().contains("17"));
        assert!(error.to_string().contains("18"));
    }

    /// The stale-confirm picker must never block an unattended run: with no TTY (as
    /// in `cargo test`, and in the detached handoff watcher) it returns the newest
    /// session unchanged even when it's stale, rather than waiting for input.
    #[test]
    fn stale_confirm_never_blocks_without_a_tty() {
        let adapter = agent::adapter(Agent::Claude);
        let cwd = std::env::temp_dir().join("agent-jdi-nonexistent-cwd");
        let stale = agent::ResumableSession {
            id: "the-only-one".into(),
            transcript: cwd.join("t.jsonl"),
            idle_secs: 99_999, // well past the 1h threshold
        };
        // No TTY under `cargo test` → returns the input without prompting.
        let out = confirm_if_stale(adapter.as_ref(), &cwd, stale.clone()).unwrap();
        assert_eq!(out.id, stale.id);

        // A fresh session skips the check entirely (early return).
        let fresh = agent::ResumableSession {
            idle_secs: 5,
            ..stale.clone()
        };
        assert_eq!(
            confirm_if_stale(adapter.as_ref(), &cwd, fresh.clone())
                .unwrap()
                .id,
            fresh.id
        );
    }

    #[test]
    fn stale_threshold_defaults_to_one_hour_and_honors_env() {
        std::env::remove_var("AGENT_JDI_STALE");
        assert_eq!(stale_secs(), 3600);
        std::env::set_var("AGENT_JDI_STALE", "60");
        assert_eq!(stale_secs(), 60);
        std::env::remove_var("AGENT_JDI_STALE");
    }

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
            s.contains("  watch:      agent-jdi log knack-98db47\n"),
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
        use clap::{CommandFactory, Parser};
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

        let mut command = Cli::command();
        for subcommand in ["start", "resume"] {
            let help = command
                .find_subcommand_mut(subcommand)
                .expect("subcommand exists")
                .render_long_help()
                .to_string();
            assert!(!help.contains("log -f"), "{subcommand} help: {help}");
            assert!(
                help.contains("`agent-jdi log` follows by default"),
                "{subcommand} help: {help}"
            );
        }
    }

    #[test]
    fn resume_accepts_a_tracked_slot_id_alongside_raw_session_id() {
        assert!(Cli::try_parse_from(["agent-jdi", "resume", "--id", "avatar-kit-5ce3fb"]).is_ok());
        assert!(Cli::try_parse_from(["agent-jdi", "resume", "--session", "thread-123"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "agent-jdi",
                "resume",
                "--id",
                "avatar-kit-5ce3fb",
                "--session",
                "thread-123",
            ])
            .is_err(),
            "tracked slot and raw session id must be mutually exclusive"
        );
    }

    #[test]
    fn handoff_watcher_accepts_a_normalized_codex_permission_snapshot() {
        let cli = Cli::try_parse_from([
            "agent-jdi",
            "__handoff",
            "--watch-pid",
            "42",
            "--cwd",
            "/tmp/repo",
            "--agent",
            "codex",
            "--session",
            "thread-123",
            "--codex-sandbox",
            "workspace-write",
            "--codex-workspace-network",
            "true",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::HandoffWait {
                codex_sandbox: Some(codex::CodexSandboxMode::WorkspaceWrite),
                codex_workspace_network: Some(true),
                ..
            }
        ));
    }

    #[test]
    fn explicit_session_id_rejects_paths() {
        let config = Config {
            home: PathBuf::from("/state/agent-jdi"),
        };
        for id in [
            "../outside",
            "nested/session",
            "/absolute/session",
            "tracked-session/",
            "tracked-session/.",
            "tracked-session//",
            "tracked-session\\child",
            ".",
            "..",
        ] {
            let error = match resolve_session(&config, Some(id)) {
                Ok(_) => panic!("path-like id {id:?} was accepted"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("invalid tracked session id"),
                "unexpected error for {id:?}: {error:#}"
            );
        }
    }

    #[test]
    fn explicit_resume_target_comes_from_tracked_slot_metadata() {
        let base = std::env::temp_dir().join(format!("ajdi-resume-id-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let config = Config {
            home: base.join("home"),
        };
        let tracked = Session::new(&config.home, "avatar-kit-5ce3fb");
        tracked
            .meta_set("cwd", "/work/repos/project-h/avatar-kit")
            .unwrap();
        tracked.meta_set("agent", "codex").unwrap();
        tracked.meta_set("session_id", "codex-session-123").unwrap();
        tracked
            .meta_set("transcript", "/sessions/codex-session-123.jsonl")
            .unwrap();

        let target = resolve_resume_target(
            &config,
            Some(Agent::Codex),
            Some("avatar-kit-5ce3fb"),
            None,
            Path::new("/an/unrelated/current/directory"),
        )
        .unwrap();

        assert_eq!(target.slot, "avatar-kit-5ce3fb");
        assert_eq!(target.cwd, Path::new("/work/repos/project-h/avatar-kit"));
        assert_eq!(target.agent, Agent::Codex);
        assert_eq!(target.resumable.id, "codex-session-123");
        assert_eq!(
            target.resumable.transcript,
            Path::new("/sessions/codex-session-123.jsonl")
        );
        std::fs::remove_dir_all(&base).ok();
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
