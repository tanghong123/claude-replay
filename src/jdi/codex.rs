//! Codex adapter. Reuses the viewer's `codex_discover`; supervises `codex exec
//! resume` with sandboxed, no-approval defaults. Codex has **no** native task queue,
//! so the done-signal is the exit code and `task_queue()` stays `None`.
//!
//! The CLI contract here is verified against Codex CLI 0.145.0: `codex exec`,
//! `codex exec resume`, `codex resume`, `codex login status`, and the JSON
//! `thread.started` event carrying `thread_id`.

use super::agent::{
    self, AgentAdapter, Brief, Invocation, Mode, ResumableSession, Trigger, TurnContext,
    TurnOutcome,
};
use crate::{codex_discover, Agent};
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

pub struct CodexAdapter;

impl CodexAdapter {
    fn preflight_program(program: &Path) -> Result<()> {
        let output = std::process::Command::new(program)
            .args(["login", "status"])
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("run `{} login status`", program.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!(
            "`{} login status` failed ({}): {}",
            program.display(),
            output.status,
            if detail.is_empty() {
                "no diagnostic output"
            } else {
                detail
            }
        )
    }
}

/// TODO(deferred): bring this in line with the Claude adapter's queue discipline —
/// a durable FIFO queue, skip-on-blocked, prerequisites done now vs. follow-ups
/// appended, and "don't end the turn while actionable work remains" (see
/// `claude.rs`'s START/DUMP/EXECUTE prompts and `jdi/DESIGN.md`).
///
/// Deliberately NOT done yet. Codex has no native task queue, so the discipline
/// would have to hang entirely off `Brief::checklist`. Keep that behavior change
/// separate from the now-verified CLI integration. Until then Codex keeps this
/// short persistence nudge, and the done-signal stays the exit code (`classify`),
/// which does not depend on any queue.
const PERSISTENCE: &str = "You are running UNATTENDED and headless — the human has stepped away and cannot answer anything. Do NOT ask for input, and do NOT stop to confirm. Do as much as you can. If an action is blocked, try safe alternatives and clearly record any remaining blocker in your final response.";

impl AgentAdapter for CodexAdapter {
    fn id(&self) -> Agent {
        Agent::Codex
    }

    /// Codex has no plan/dump step: `start` is a fresh turn, everything else runs
    /// (resumes) directly.
    fn initial_mode(&self, trigger: Trigger) -> Mode {
        match trigger {
            Trigger::Start => Mode::Start,
            _ => Mode::Execute,
        }
    }

    fn resolve_binary(&self) -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("AGENT_JDI_CODEX_BIN") {
            return Ok(PathBuf::from(p));
        }
        agent::which("codex").ok_or_else(|| {
            anyhow!("codex CLI not found on PATH — install it: curl -fsSL https://chatgpt.com/codex/install.sh | sh")
        })
    }

    fn preflight(&self) -> Result<()> {
        Self::preflight_program(&self.resolve_binary()?)
    }

    fn build_invocation(&self, ctx: &TurnContext) -> Invocation {
        let program = self
            .resolve_binary()
            .unwrap_or_else(|_| PathBuf::from("codex"));
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
        args.push(self.prompt_for(ctx.mode, ctx.brief, ctx.session_id));
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

    fn sessions_for_cwd(&self, cwd: &Path) -> Vec<super::agent::SessionBrief> {
        codex_discover::sessions_for_cwd(cwd)
            .into_iter()
            .map(|(id, mtime, snippet)| super::agent::SessionBrief {
                id,
                idle_secs: mtime.elapsed().map(|d| d.as_secs()).unwrap_or(0),
                snippet,
            })
            .collect()
    }

    fn prompt_for(&self, _mode: Mode, brief: &Brief, _session_id: &str) -> String {
        let mut out = if brief.text.trim().is_empty() {
            PERSISTENCE.to_string()
        } else {
            format!("{PERSISTENCE}\n\nAdditional instruction: {}", brief.text)
        };
        // Claimed backlog items MUST reach the agent: the supervisor moves them to
        // `drained/` when the turn comes back clean, so a prompt that omitted them
        // would silently discard the human's queued follow-ups.
        if !brief.backlog.is_empty() {
            let items = brief
                .backlog
                .iter()
                .enumerate()
                .map(|(i, b)| format!("### Backlog item {}\n{}", i + 1, b.trim()))
                .collect::<Vec<_>>()
                .join("\n\n");
            out.push_str(&format!(
                "\n\nThe human queued the following {} follow-up message(s) for THIS \
                 session while you were working. Go through them ONE BY ONE: understand \
                 each request and carry out the concrete, fully-scoped work it implies. \
                 Anything ambiguous: do not act on it — record it as a blocker instead.\
                 \n\n{items}",
                brief.backlog.len()
            ));
        }
        out
    }

    /// Codex assigns session ids itself — `start` captures the id afterward.
    fn pins_session_id(&self) -> bool {
        false
    }

    fn continue_mode(&self) -> Mode {
        Mode::Execute
    }

    fn unattended_note(&self) -> &'static str {
        "sandbox=workspace-write, approvals=never (unattended)"
    }

    /// Hand the session to a human: `codex resume <id>` — the interactive TUI, not
    /// the sandboxed `exec` turn. `autonomous` keeps the no-approval posture the
    /// unattended run had.
    fn interactive_invocation(
        &self,
        session_id: &str,
        _cwd: &Path,
        autonomous: bool,
    ) -> Option<Invocation> {
        if session_id.is_empty() {
            return None;
        }
        let program = self.resolve_binary().ok()?;
        let mut args = vec!["resume".into(), session_id.to_string()];
        if autonomous {
            args.extend([
                "-c".into(),
                "approval_policy=\"never\"".into(),
                "-c".into(),
                "sandbox_mode=\"workspace-write\"".into(),
            ]);
        }
        Some(Invocation { program, args })
    }

    fn resume_commands(&self, session_id: &str) -> Vec<(String, String)> {
        if session_id.is_empty() {
            return Vec::new();
        }
        vec![(
            "# resume the interactive session:".into(),
            format!("codex resume {session_id}"),
        )]
    }

    /// Fresh run: `codex exec <task+nonce> --json …` (no `resume`, no id — Codex
    /// assigns one, which `capture_session_id` then recovers).
    fn fresh_invocation(&self, ctx: &TurnContext, nonce: &str) -> Invocation {
        let program = self
            .resolve_binary()
            .unwrap_or_else(|_| PathBuf::from("codex"));
        let mut args = vec![
            "exec".into(),
            "-c".into(),
            "approval_policy=\"never\"".into(),
            "-c".into(),
            "sandbox_mode=\"workspace-write\"".into(),
            "--json".into(),
        ];
        args.extend(ctx.extra_args.iter().cloned());
        let prompt = format!(
            "{}\n\n<!-- agent-jdi run: {nonce} -->",
            self.prompt_for(ctx.mode, ctx.brief, ctx.session_id)
        );
        args.push(prompt);
        Invocation { program, args }
    }

    /// Recover the id Codex assigned: first from the `thread.started` JSON event,
    /// then by finding the rollout whose first user message carries our nonce.
    fn capture_session_id(&self, output: &str, _cwd: &Path, nonce: &str) -> Option<String> {
        // 1) Parse the --json stream. Codex 0.145.0 emits
        // `{"type":"thread.started","thread_id":"…"}`.
        for line in output.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            for ptr in [
                "/thread_id",
                "/session_id",
                "/id",
                "/payload/id",
                "/payload/session_id",
                "/session/id",
            ] {
                if let Some(id) = v.pointer(ptr).and_then(|x| x.as_str()) {
                    if !id.is_empty() {
                        return Some(id.to_string());
                    }
                }
            }
        }
        // 2) Fallback: the rollout whose first user message contains the nonce.
        codex_discover::session_id_with_marker(nonce)
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
        assert_eq!(
            &inv.args[..8],
            [
                "exec",
                "resume",
                "-c",
                "approval_policy=\"never\"",
                "-c",
                "sandbox_mode=\"workspace-write\"",
                "--json",
                "sess-1",
            ]
        );
        assert!(!inv.args.iter().any(|x| x.contains("dangerously")));
    }

    #[test]
    fn takeover_resume_command_matches_the_interactive_codex_cli() {
        let a = CodexAdapter;
        assert_eq!(
            a.resume_commands("sess-1"),
            vec![(
                "# resume the interactive session:".to_string(),
                "codex resume sess-1".to_string(),
            )]
        );
    }

    #[test]
    fn captures_thread_id_from_real_codex_json_event() {
        let a = CodexAdapter;
        let output = r#"{"type":"thread.started","thread_id":"thread-123"}"#;
        assert_eq!(
            a.capture_session_id(output, Path::new("/tmp"), "missing-nonce"),
            Some("thread-123".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_invokes_codex_login_status_and_reports_failure() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("agent-jdi-codex-preflight-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let ok = root.join("codex-ok");
        let failed = root.join("codex-failed");
        for (path, exit) in [(&ok, 0), (&failed, 7)] {
            std::fs::write(
                path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\necho login-state >&2\nexit {exit}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        CodexAdapter::preflight_program(&ok).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("codex-ok.args")).unwrap(),
            "login\nstatus\n"
        );
        let error = CodexAdapter::preflight_program(&failed).unwrap_err();
        assert!(error.to_string().contains("login status"), "{error:#}");
        assert!(error.to_string().contains("login-state"), "{error:#}");
        std::fs::remove_dir_all(&root).ok();
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
    fn fresh_invocation_is_exec_not_resume_and_carries_the_nonce() {
        let a = CodexAdapter;
        let brief = Brief {
            text: "do the thing".into(),
            ..Default::default()
        };
        let inv = a.fresh_invocation(&ctx("", &brief, &[]), "NONCE-abc123");
        assert!(inv.args.iter().any(|x| x == "exec"));
        assert!(!inv.args.iter().any(|x| x == "resume"), "fresh ≠ resume");
        assert!(inv.args.iter().any(|x| x == "--json"));
        assert!(
            inv.args.iter().any(|x| x.contains("NONCE-abc123")),
            "nonce embedded for id capture"
        );
    }

    #[test]
    fn prompt_folds_extra_instruction() {
        let a = CodexAdapter;
        let brief = Brief {
            text: "prioritize tests".into(),
            ..Default::default()
        };
        let p = a.prompt_for(Mode::Execute, &brief, "");
        assert!(p.contains("UNATTENDED"));
        assert!(p.contains("prioritize tests"));
    }
}
