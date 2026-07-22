//! The `AgentAdapter` trait: one agent's integration with the supervisor spine.
//! New agents implement this; optional capabilities default to "unsupported" so an
//! adapter can leave features unimplemented (Codex has no native task queue, etc.).

use crate::Agent;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// A supervised turn's mode — the dump→execute two-step and its backlog variants,
/// so "planned ≠ done": a dump turn produces a plan/queue then STOPs; an execute
/// turn drains it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The first turn of a fresh `start` run — feed the task brief.
    Start,
    /// Plain run of the brief.
    Execute,
    /// Resume: enqueue the agreed plan, then STOP.
    ResumeDump,
    /// Resume: drain the plan to completion.
    ResumeExecute,
    /// Backlog: triage claimed items into one brief, then STOP.
    BacklogDump,
    /// Backlog: execute the triaged brief.
    BacklogExecute,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Execute => "execute",
            Self::ResumeDump => "resume-dump",
            Self::ResumeExecute => "resume-execute",
            Self::BacklogDump => "backlog-dump",
            Self::BacklogExecute => "backlog-execute",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "start" => Self::Start,
            "execute" => Self::Execute,
            "resume-dump" => Self::ResumeDump,
            "resume-execute" => Self::ResumeExecute,
            "backlog-dump" => Self::BacklogDump,
            "backlog-execute" => Self::BacklogExecute,
            _ => return None,
        })
    }
}

/// A resumable session an adapter found for a cwd.
#[derive(Debug, Clone)]
pub struct ResumableSession {
    pub id: String,
    pub transcript: PathBuf,
    pub idle_secs: u64,
}

/// Brief/prompt inputs for a turn.
#[derive(Debug, Default, Clone)]
pub struct Brief {
    /// The task brief (from `--task-file` / a resume dump).
    pub text: String,
    /// Claimed backlog items to fold in.
    pub backlog: Vec<String>,
}

/// What kicked off a supervised run — selects the adapter's initial mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// `start`: a fresh unattended run of a task.
    Start,
    /// `resume`: continue the most-recent session.
    Resume,
    /// A backlog drain (queued follow-up work).
    BacklogDrain,
}

/// Everything an adapter needs to build one turn's invocation.
pub struct TurnContext<'a> {
    pub mode: Mode,
    pub session_id: &'a str,
    /// Resume an existing session (true) vs. start with a fresh pinned id (false).
    pub session_created: bool,
    pub cwd: &'a Path,
    pub brief: &'a Brief,
    /// Passthrough args the user appended after `--`.
    pub extra_args: &'a [String],
}

/// A CLI invocation: program + args, passed straight to `Command` (no shell → no
/// injection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// What the spine should do after a finished turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Genuinely complete.
    Done,
    /// Recoverable failure → sleep the interval and loop.
    Retry,
    /// Advance the mode (dump→execute); no sleep, the attempt doesn't count.
    AdvanceMode(Mode),
    /// The session vanished ("no conversation found") → recreate with a fresh id.
    RecreateSession,
    /// Terminal, unrecoverable (auth/billing/…).
    Failed(i32),
    /// Interrupted by a signal (SIGINT/SIGTERM).
    Stopped(i32),
    /// Hit the max-attempts ceiling.
    GaveUp,
}

/// Optional native task-queue introspection. Claude has one (`~/.claude/tasks/`);
/// Codex doesn't, so it leaves this unimplemented and the done-signal falls back to
/// the exit code.
pub trait TaskQueue {
    /// Open (non-completed) task count; `None` = unknown (missing dir / parse fail),
    /// which callers treat as "trust the exit code" rather than "zero left".
    fn open_count(&self, session_id: &str) -> Option<usize>;
    /// Human-readable rendering for `status`.
    fn render(&self, session_id: &str) -> String;
}

/// One agent's integration with the supervisor.
pub trait AgentAdapter {
    fn id(&self) -> Agent;

    /// The mode a run starts in for a given trigger. Claude uses a plan→execute
    /// two-step (`ResumeDump`→`ResumeExecute`); Codex has no plan step (`Execute`).
    fn initial_mode(&self, trigger: Trigger) -> Mode;

    /// Resolve the agent's CLI binary (PATH + known locations); must never resolve
    /// our own executable (the supervisor).
    fn resolve_binary(&self) -> Result<PathBuf>;

    /// Optional pre-flight (auth/login checks). Default: no-op.
    fn preflight(&self) -> Result<()> {
        Ok(())
    }

    /// Build the CLI invocation for one supervised turn.
    fn build_invocation(&self, ctx: &TurnContext) -> Invocation;

    /// Classify a finished turn into the spine's next action.
    fn classify(&self, rc: i32, capture: &str, ctx: &TurnContext) -> TurnOutcome;

    /// The newest resumable session for a cwd.
    fn discover_resumable(&self, cwd: &Path) -> Result<ResumableSession>;

    /// Locate a session's transcript (for `log` / progress).
    fn transcript_path(&self, session_id: &str, cwd: &Path) -> Option<PathBuf>;

    /// The transcript a fresh run *will* write, if deterministic from a pinned id
    /// (Claude). Lets `start` follow before the file exists. Default `None` (Codex
    /// assigns the id, so the path isn't known until capture).
    fn expected_transcript(&self, _session_id: &str, _cwd: &Path) -> Option<PathBuf> {
        None
    }

    /// Prompt text for a mode (adapter-specific — task tools vs. a plain prompt).
    fn prompt_for(&self, mode: Mode, brief: &Brief) -> String;

    // --- fresh-run (`start`) hooks ---

    /// Build the FIRST turn of a fresh `start` run (feeds the task brief, not a
    /// continue prompt). `nonce` is embedded so the assigned session can be
    /// identified afterward (Codex). Default: reuse `build_invocation`.
    fn fresh_invocation(&self, ctx: &TurnContext, nonce: &str) -> Invocation {
        let _ = nonce;
        self.build_invocation(ctx)
    }

    /// After a fresh turn, learn the session id the agent assigned — from the turn's
    /// captured output (+ cwd + nonce for a transcript fallback). Default `None`;
    /// only agents that *don't* pin an id (Codex) implement this.
    fn capture_session_id(&self, _output: &str, _cwd: &Path, _nonce: &str) -> Option<String> {
        None
    }

    /// The mode a run drops into after its first (dump/start) turn, for relaunches.
    fn continue_mode(&self) -> Mode {
        Mode::Execute
    }

    // --- optional capabilities (defaults = unsupported) ---

    /// Native task queue, if the agent has one (drives the done-signal).
    fn task_queue(&self) -> Option<&dyn TaskQueue> {
        None
    }

    /// Whether the agent pins its own session id up front (Claude `--session-id`).
    /// If false (Codex assigns ids), `start` captures the id after the first turn.
    fn pins_session_id(&self) -> bool {
        true
    }

    /// One-line description of the autonomy the agent runs under (for the `resume`/
    /// `start` summary's `runs with:` line). Default is generic.
    fn unattended_note(&self) -> &'static str {
        "unattended (no human in the loop)"
    }

    /// The **interactive** invocation that hands a stopped session back to a human
    /// (`takeover` launches this) — a normal, human-in-the-loop resume, NOT the
    /// unattended `-p`/`--dangerously-skip-permissions` turn. `None` = the agent
    /// can't be resumed interactively (or no id yet), so `takeover` just reports.
    fn interactive_invocation(&self, _session_id: &str, _cwd: &Path) -> Option<Invocation> {
        None
    }

    /// Human-facing resume commands for `takeover`'s "resume it yourself" block:
    /// `(comment, command)` pairs (e.g. an autonomous and a supervised variant),
    /// shown verbatim with the readable binary name. Empty = no printable hint.
    fn resume_commands(&self, _session_id: &str) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// The adapter registry: the one place that knows every agent. Adding an agent is
/// a new module + one arm here.
pub fn adapter(agent: Agent) -> Box<dyn AgentAdapter> {
    match agent {
        Agent::Claude => Box::new(super::claude::ClaudeAdapter),
        Agent::Codex => Box::new(super::codex::CodexAdapter),
    }
}

/// Locate an agent CLI on PATH (then the usual install dirs), never returning our
/// own executable. Used by adapters' `resolve_binary`.
pub fn which(name: &str) -> Option<PathBuf> {
    let self_exe = std::env::current_exe().ok();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    for d in dirs {
        let p = d.join(name);
        if p.is_file() && self_exe.as_ref() != Some(&p) {
            return Some(p);
        }
    }
    None
}
