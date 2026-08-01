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

/// One entry in a cwd's session list, for `resume`'s stale-confirmation picker.
#[derive(Debug, Clone)]
pub struct SessionBrief {
    pub id: String,
    pub idle_secs: u64,
    pub snippet: String,
}

/// Brief/prompt inputs for a turn.
#[derive(Debug, Default, Clone)]
pub struct Brief {
    /// The task brief (from `--task-file` / a resume dump).
    pub text: String,
    /// Claimed backlog items to fold in.
    pub backlog: Vec<String>,
    /// Where the agent should keep a durable `- [ ]` / `- [x]` checklist when it has
    /// no native task-management tools. Doubles as the fallback done-signal: the
    /// supervisor counts unchecked items, so "planned ≠ done" survives without them.
    pub checklist: Option<PathBuf>,
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
    /// **Actionable** task count — open tasks that are neither explicitly parked
    /// (`deferred`/`paused`) nor blocked by a non-completed task (#79: a queue holding
    /// only parked/blocked work must classify as DONE, not spawn retry rounds forever).
    /// `None` = unknown (missing dir / parse fail), which callers treat as "trust the
    /// exit code" rather than "zero left".
    fn actionable_count(&self, session_id: &str, cwd: &Path) -> Option<usize>;
    /// A stable fingerprint of the queue's `(id, status)` pairs — the task half of the
    /// supervisor's no-progress detection (#79). `None` = unknown.
    fn fingerprint(&self, session_id: &str, cwd: &Path) -> Option<u64> {
        let _ = (session_id, cwd);
        None
    }
    /// Human-readable rendering for `status`.
    fn render(&self, session_id: &str, cwd: &Path) -> String;
}

/// An agent's per-session permission/sandbox posture, captured on takeover so the resumed
/// unattended run executes under the exact context the interactive run had (#17). Only
/// Codex has one today; an agent without a posture never constructs one (its
/// `capture_permissions` returns `None`). The spine holds `Box<dyn PermissionPosture>`
/// and never names a concrete posture type — serialization, application, and rendering
/// all go through these methods.
pub trait PermissionPosture: std::fmt::Debug {
    /// Extra CLI args that impose this posture on the agent invocation (`-c …` pairs).
    fn config_args(&self) -> Vec<String>;
    /// One-line posture summary (e.g. "workspace-write, network disabled").
    fn summary(&self) -> String;
    /// Serialize into neutral `--permission-arg` VALUES for the detached `__handoff`
    /// re-invocation; round-trips via [`AgentAdapter::parse_handoff_permissions`].
    fn handoff_flags(&self) -> Vec<String>;
    /// The `permissions` meta note persisted for a run under this posture.
    fn persisted_note(&self) -> String;
    /// The two run-banner lines (`permissions:` / `approvals:`) shown when a handoff
    /// preserves this posture.
    fn banner_lines(&self) -> (String, String);
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

    /// All resumable sessions for a cwd, newest-first — for `resume`'s stale-confirm
    /// picker. Default: just the newest (so the picker is a no-op); adapters that can
    /// list the directory's sessions override it.
    fn sessions_for_cwd(&self, cwd: &Path) -> Vec<SessionBrief> {
        self.discover_resumable(cwd)
            .map(|r| {
                vec![SessionBrief {
                    id: r.id,
                    idle_secs: r.idle_secs,
                    snippet: String::new(),
                }]
            })
            .unwrap_or_default()
    }

    /// Locate a session's transcript (for `log` / progress).
    fn transcript_path(&self, session_id: &str, cwd: &Path) -> Option<PathBuf>;

    /// The transcript a fresh run *will* write, if deterministic from a pinned id
    /// (Claude). Lets `start` follow before the file exists. Default `None` (Codex
    /// assigns the id, so the path isn't known until capture).
    fn expected_transcript(&self, _session_id: &str, _cwd: &Path) -> Option<PathBuf> {
        None
    }

    /// Prompt text for a mode (adapter-specific — task tools vs. a plain prompt).
    /// The prompt for a turn. `session_id` lets an adapter tailor it to what this
    /// session actually has — e.g. Claude omits the checklist-fallback paragraph
    /// when the session demonstrably uses the native task queue.
    fn prompt_for(&self, mode: Mode, brief: &Brief, session_id: &str) -> String;

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

    /// Capture the live permission posture for a takeover, so the resumed run keeps the
    /// exact context the interactive run had (Codex reconstructs its sandbox/approval
    /// config from the rollout; Claude carries none). Default `Ok(None)` — the supervisor
    /// then just clears any stale permission state. An agent WITH a posture is fail-closed
    /// here: a missing session id / unreadable rollout is an `Err`, never a silent
    /// capability change. Gating on this capability (not on agent identity) keeps
    /// permission handling out of the spine (#17).
    fn capture_permissions(
        &self,
        _session_id: Option<&str>,
        _transcript: Option<&Path>,
    ) -> Result<Option<Box<dyn PermissionPosture>>> {
        Ok(None)
    }

    /// Reconstruct a posture from the neutral `--permission-arg` values the parent handoff
    /// serialized (the inverse of [`PermissionPosture::handoff_flags`]). Empty input means
    /// "no posture captured" (`Ok(None)`); malformed input is an `Err` (fail-closed).
    /// Default `Ok(None)` — an agent with no posture ignores stray args.
    fn parse_handoff_permissions(
        &self,
        _args: &[String],
    ) -> Result<Option<Box<dyn PermissionPosture>>> {
        Ok(None)
    }

    /// The `permissions` meta note for a run of this agent with NO captured posture —
    /// `Some` iff the agent has a posture concept at all (its documented default posture).
    /// `None` (the default) means the supervisor clears any stale permission state instead.
    fn default_permission_note(&self) -> Option<&'static str> {
        None
    }

    /// The command-line flags that pin a session id in this agent's invocation — the
    /// REVERSE direction of `build_invocation`, used to recover the id from a running
    /// process's argv. The default is the Claude-Code family shape (QoderWork and other
    /// forks carry the same flags); Codex overrides with its `resume` subcommand.
    fn resume_id_flags(&self) -> &'static [&'static str] {
        &["--resume", "--session-id"]
    }

    /// The agent's "ambient" session id, if it exposes one to child processes outside the
    /// transcript (Codex's `CODEX_THREAD_ID` env var). Consulted by `handoff` between an
    /// explicit `--session` and the argv-derived id. Default `None`.
    fn ambient_session_id(&self) -> Option<String> {
        None
    }

    /// One-line description of the autonomy the agent runs under (for the `resume`/
    /// `start` summary's `runs with:` line). Default is generic.
    fn unattended_note(&self) -> &'static str {
        "unattended (no human in the loop)"
    }

    /// The **interactive** invocation that hands a stopped session back to a human
    /// (`takeover` launches this) — a real interactive session, never the unattended
    /// `-p` batch turn. `autonomous` keeps the run's permission posture (Claude's
    /// `--dangerously-skip-permissions`): a session supervised unattended was already
    /// running that way, so dropping it would prompt on every tool call. `false`
    /// resumes with approvals on. `None` = no interactive resume (or no id yet).
    fn interactive_invocation(
        &self,
        _session_id: &str,
        _cwd: &Path,
        _autonomous: bool,
    ) -> Option<Invocation> {
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
/// a new module + one arm here (and one entry in [`agents`]). `Agent` is an open id
/// (#87): the wildcard arm routes any id jdi has no driver for to the same clean
/// unsupported-at-preflight failure as QoderWork.
pub fn adapter(agent: Agent) -> Box<dyn AgentAdapter> {
    match agent {
        Agent::CLAUDE => Box::new(super::claude::ClaudeAdapter),
        Agent::CODEX => Box::new(super::codex::CodexAdapter),
        // The VIEWER supports QoderWork (and any third-party-registered) transcripts;
        // the supervisor drives only the two CLIs above. Detection never yields others
        // (`agents()` lists only the driveable two); an explicit `--agent <other>`
        // reaches the stub, which fails cleanly at preflight instead of resuming the
        // wrong binary.
        other => Box::new(UnsupportedAdapter { agent: other }),
    }
}

/// A stub for agents the viewer parses but the supervisor can't drive: every entry point that
/// would launch or resume fails at `resolve_binary` with a clear message.
struct UnsupportedAdapter {
    agent: Agent,
}
impl AgentAdapter for UnsupportedAdapter {
    fn id(&self) -> Agent {
        self.agent
    }
    fn initial_mode(&self, _trigger: Trigger) -> Mode {
        Mode::Execute
    }
    fn resolve_binary(&self) -> anyhow::Result<std::path::PathBuf> {
        anyhow::bail!(
            "agent-jdi does not support driving '{}' sessions yet (the viewer can replay them;              supervision needs its CLI resume shape verified)",
            self.agent.label()
        )
    }
    fn build_invocation(&self, _ctx: &TurnContext) -> Invocation {
        Invocation {
            program: std::path::PathBuf::from(self.agent.label()),
            args: Vec::new(),
        }
    }
    fn classify(&self, rc: i32, _capture: &str, _ctx: &TurnContext) -> TurnOutcome {
        TurnOutcome::Failed(rc)
    }
    fn discover_resumable(&self, _cwd: &std::path::Path) -> anyhow::Result<ResumableSession> {
        anyhow::bail!(
            "agent-jdi does not support '{}' sessions yet",
            self.agent.label()
        )
    }
    fn transcript_path(&self, _id: &str, _cwd: &std::path::Path) -> Option<std::path::PathBuf> {
        None
    }
    fn prompt_for(&self, _mode: Mode, _brief: &Brief, _session_id: &str) -> String {
        String::new()
    }
}

/// Every agent the supervisor knows, in a stable order — the single list detection iterates,
/// mirroring the engine's `adapter::adapters()`. Keeps the agent set in one place instead of
/// hardcoded at each iteration site.
pub fn agents() -> &'static [Agent] {
    &[Agent::CLAUDE, Agent::CODEX]
}

/// Seconds since `mtime` (a session's idle time), clamped to 0 if the clock went backwards.
/// The shared idiom behind every adapter's `idle_secs` in `ResumableSession`/`SessionBrief`.
pub(crate) fn idle_secs(mtime: std::time::SystemTime) -> u64 {
    mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0)
}

/// Render queued backlog items into the numbered `### Backlog item N` block that a dump
/// prompt folds in. Shared by both adapters' `prompt_for` (the surrounding prose that
/// introduces the block is per-agent; this inner list is identical).
pub(crate) fn format_backlog_items(backlog: &[String]) -> String {
    backlog
        .iter()
        .enumerate()
        .map(|(i, b)| format!("### Backlog item {}\n{}", i + 1, b.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
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
