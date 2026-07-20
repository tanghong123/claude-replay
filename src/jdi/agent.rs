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

    /// Prompt text for a mode (adapter-specific — task tools vs. a plain prompt).
    fn prompt_for(&self, mode: Mode, brief: &Brief) -> String;

    // --- optional capabilities (defaults = unsupported) ---

    /// Native task queue, if the agent has one (drives the done-signal).
    fn task_queue(&self) -> Option<&dyn TaskQueue> {
        None
    }

    /// Whether the agent supports a fresh run with a pinned session id (Claude:
    /// yes; Codex assigns ids after the fact, so resume-only).
    fn supports_fresh_run(&self) -> bool {
        true
    }
}
