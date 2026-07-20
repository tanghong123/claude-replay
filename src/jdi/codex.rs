//! Codex adapter. Reuses the viewer's `codex_discover`; supervises `codex exec
//! resume` with sandboxed, no-approval defaults. Codex has **no** native task queue,
//! so the done-signal is the exit code and `task_queue()` stays `None`.
//!
//! NOTE: the exact `codex` CLI surface (subcommand path, `-c` override keys/quoting,
//! `--json`, whether resume writes a *new* rollout file) is **unverified** — it's all
//! isolated here as `TODO(verify)` so one edit corrects it once a real `codex` is
//! available.

use super::agent::{
    self, AgentAdapter, Brief, Invocation, Mode, ResumableSession, Trigger, TurnContext,
    TurnOutcome,
};
use crate::{codex_discover, Agent};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

pub struct CodexAdapter;

const PERSISTENCE: &str = "You are running UNATTENDED and headless — the human has stepped away and cannot answer anything. Do NOT ask for input, and do NOT stop to confirm. Do as much as you can. If an action is blocked, try safe alternatives and clearly record any remaining blocker in your final response.";

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> Agent {
        Agent::Codex
    }

    /// Codex has no plan/dump step — resume executes directly.
    fn initial_mode(&self, _trigger: Trigger) -> Mode {
        Mode::Execute
    }

    fn resolve_binary(&self) -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("AGENT_JDI_CODEX_BIN") {
            return Ok(PathBuf::from(p));
        }
        agent::which("codex").ok_or_else(|| {
            anyhow!("codex CLI not found on PATH — install it: curl -fsSL https://chatgpt.com/codex/install.sh | sh")
        })
    }

    // TODO(verify): `codex login status` is the auth probe — confirm subcommand.
    fn preflight(&self) -> Result<()> {
        Ok(())
    }

    fn build_invocation(&self, ctx: &TurnContext) -> Invocation {
        let program = self
            .resolve_binary()
            .unwrap_or_else(|_| PathBuf::from("codex"));
        // TODO(verify): subcommand path + `-c key="val"` override syntax + `--json`.
        let mut args = vec![
            "exec".into(),
            "resume".into(),
            "-c".into(),
            "approval_policy=\"never\"".into(),
            "-c".into(),
            "sandbox_mode=\"workspace-write\"".into(),
            "--json".into(),
        ];
        args.extend(ctx.extra_args.iter().cloned());
        args.push(ctx.session_id.to_string());
        args.push(self.prompt_for(ctx.mode, ctx.brief));
        Invocation { program, args }
    }

    fn classify(&self, rc: i32, _capture: &str, _ctx: &TurnContext) -> TurnOutcome {
        match rc {
            0 => TurnOutcome::Done,
            130 | 143 => TurnOutcome::Stopped(rc),
            // Codex has no error taxonomy wired yet; treat non-signal failures as
            // recoverable so a transient error is retried. TODO(verify): map codex's
            // terminal errors (auth/sandbox-unavailable) to Failed.
            _ => TurnOutcome::Retry,
        }
    }

    fn discover_resumable(&self, cwd: &Path) -> Result<ResumableSession> {
        let s = codex_discover::latest_for_cwd(cwd)
            .ok_or_else(|| anyhow!("no Codex session found for {}", cwd.display()))?;
        let idle_secs = s.mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        Ok(ResumableSession {
            id: s.id,
            transcript: s.path,
            idle_secs,
        })
    }

    fn transcript_path(&self, session_id: &str, _cwd: &Path) -> Option<PathBuf> {
        codex_discover::resolve(Some(session_id), false).ok()
    }

    fn prompt_for(&self, _mode: Mode, brief: &Brief) -> String {
        if brief.text.trim().is_empty() {
            PERSISTENCE.to_string()
        } else {
            format!("{PERSISTENCE}\n\nAdditional instruction: {}", brief.text)
        }
    }

    /// Codex assigns session ids itself — no fresh-run-with-pinned-id.
    fn supports_fresh_run(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(id: &'a str, brief: &'a Brief, extra: &'a [String]) -> TurnContext<'a> {
        TurnContext {
            mode: Mode::Execute,
            session_id: id,
            session_created: true,
            cwd: Path::new("/tmp/repo"),
            brief,
            extra_args: extra,
        }
    }

    #[test]
    fn invocation_is_sandboxed_noninteractive_and_resumes_by_id() {
        let a = CodexAdapter;
        let brief = Brief::default();
        let inv = a.build_invocation(&ctx("sess-1", &brief, &[]));
        assert!(inv
            .args
            .windows(2)
            .any(|w| w == ["-c", "approval_policy=\"never\""]));
        assert!(inv
            .args
            .windows(2)
            .any(|w| w == ["-c", "sandbox_mode=\"workspace-write\""]));
        assert!(inv.args.iter().any(|x| x == "resume"));
        assert!(inv.args.iter().any(|x| x == "sess-1"));
        assert!(!inv.args.iter().any(|x| x.contains("dangerously")));
    }

    #[test]
    fn classify_maps_exit_codes() {
        let a = CodexAdapter;
        let brief = Brief::default();
        let c = ctx("s", &brief, &[]);
        assert_eq!(a.classify(0, "", &c), TurnOutcome::Done);
        assert_eq!(a.classify(130, "", &c), TurnOutcome::Stopped(130));
        assert_eq!(a.classify(143, "", &c), TurnOutcome::Stopped(143));
        assert_eq!(a.classify(1, "", &c), TurnOutcome::Retry);
    }

    #[test]
    fn prompt_folds_extra_instruction() {
        let a = CodexAdapter;
        let brief = Brief {
            text: "prioritize tests".into(),
            backlog: vec![],
        };
        let p = a.prompt_for(Mode::Execute, &brief);
        assert!(p.contains("UNATTENDED"));
        assert!(p.contains("prioritize tests"));
    }
}
