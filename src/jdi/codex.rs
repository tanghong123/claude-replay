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
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub struct CodexAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexPermissionSnapshot {
    sandbox: CodexSandboxMode,
    workspace_network: Option<bool>,
}

impl CodexPermissionSnapshot {
    pub(crate) fn from_handoff_parts(
        sandbox: CodexSandboxMode,
        workspace_network: Option<bool>,
    ) -> Result<Self> {
        let coherent = match sandbox {
            CodexSandboxMode::WorkspaceWrite => workspace_network.is_some(),
            CodexSandboxMode::ReadOnly | CodexSandboxMode::DangerFullAccess => {
                workspace_network.is_none()
            }
        };
        if !coherent {
            bail!("cannot preserve the current Codex permission context: incoherent network value");
        }
        Ok(Self {
            sandbox,
            workspace_network,
        })
    }

    pub(crate) fn from_rollout(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("open Codex rollout {}", path.display()))?;
        let mut latest = None;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) == Some("turn_context") {
                latest = Some(value);
            }
        }

        let policy = latest
            .as_ref()
            .and_then(|value| value.pointer("/payload/sandbox_policy"))
            .ok_or_else(|| {
                anyhow!(
                    "cannot preserve the current Codex permission context from {}",
                    path.display()
                )
            })?;
        let kind = policy.get("type").and_then(|value| value.as_str());
        let (sandbox, workspace_network) = match kind {
            Some("read-only") => (CodexSandboxMode::ReadOnly, None),
            Some("workspace-write") => (
                CodexSandboxMode::WorkspaceWrite,
                Some(
                    policy
                        .get("network_access")
                        .and_then(|value| value.as_bool())
                        .ok_or_else(|| {
                            anyhow!(
                                "cannot preserve the current Codex permission context: \
                                 workspace-write network_access is missing or invalid"
                            )
                        })?,
                ),
            ),
            Some("danger-full-access") => (CodexSandboxMode::DangerFullAccess, None),
            _ => {
                bail!(
                    "cannot preserve the current Codex permission context: \
                     unsupported sandbox mode"
                )
            }
        };
        Ok(Self {
            sandbox,
            workspace_network,
        })
    }

    pub(crate) fn sandbox(&self) -> CodexSandboxMode {
        self.sandbox
    }

    pub(crate) fn workspace_network(&self) -> Option<bool> {
        self.workspace_network
    }

    pub(crate) fn config_args(&self) -> Vec<String> {
        let mut args = vec![
            "-c".into(),
            format!("sandbox_mode=\"{}\"", self.sandbox.as_config_value()),
        ];
        if let Some(enabled) = self.workspace_network {
            args.extend([
                "-c".into(),
                format!("sandbox_workspace_write.network_access={enabled}"),
            ]);
        }
        args
    }

    pub(crate) fn summary(&self) -> String {
        match (self.sandbox, self.workspace_network) {
            (CodexSandboxMode::WorkspaceWrite, Some(true)) => {
                "workspace-write, network enabled".into()
            }
            (CodexSandboxMode::WorkspaceWrite, Some(false)) => {
                "workspace-write, network disabled".into()
            }
            (mode, None) => mode.as_config_value().into(),
            _ => unreachable!("constructor enforces the workspace network invariant"),
        }
    }
}

impl CodexAdapter {
    fn unattended_config_args(extra_args: &[String]) -> Vec<String> {
        if !extra_args.is_empty() {
            return extra_args.to_vec();
        }
        CodexPermissionSnapshot::from_handoff_parts(CodexSandboxMode::WorkspaceWrite, Some(false))
            .expect("the static unattended default must be coherent")
            .config_args()
    }

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
        ];
        args.extend(Self::unattended_config_args(ctx.extra_args));
        args.push("--json".into());
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
        ];
        args.extend(Self::unattended_config_args(ctx.extra_args));
        args.push("--json".into());
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

    struct PermissionFixture {
        root: PathBuf,
        rollout: PathBuf,
    }

    impl PermissionFixture {
        fn with_contexts(contexts: &[serde_json::Value]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "agent-jdi-codex-permissions-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).unwrap();
            let rollout = root.join("rollout.jsonl");
            let mut body = String::from("not-json\n");
            for context in contexts {
                body.push_str(&format!("{context}\n"));
            }
            std::fs::write(&rollout, body).unwrap();
            Self { root, rollout }
        }
    }

    impl Drop for PermissionFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

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
    fn parses_latest_codex_turn_permissions() {
        let fixture = PermissionFixture::with_contexts(&[
            serde_json::json!({
                "type": "turn_context",
                "payload": {
                    "sandbox_policy": {
                        "type": "workspace-write",
                        "network_access": false
                    }
                }
            }),
            serde_json::json!({
                "type": "turn_context",
                "payload": {
                    "sandbox_policy": {
                        "type": "danger-full-access"
                    }
                }
            }),
        ]);
        let snapshot = CodexPermissionSnapshot::from_rollout(&fixture.rollout).unwrap();
        assert_eq!(snapshot.sandbox(), CodexSandboxMode::DangerFullAccess);
        assert_eq!(snapshot.workspace_network(), None);
    }

    #[test]
    fn parses_workspace_network_enabled() {
        let fixture = PermissionFixture::with_contexts(&[serde_json::json!({
            "type": "turn_context",
            "payload": {
                "sandbox_policy": {
                    "type": "workspace-write",
                    "network_access": true
                }
            }
        })]);
        let snapshot = CodexPermissionSnapshot::from_rollout(&fixture.rollout).unwrap();
        assert_eq!(snapshot.sandbox(), CodexSandboxMode::WorkspaceWrite);
        assert_eq!(snapshot.workspace_network(), Some(true));
    }

    #[test]
    fn parses_workspace_network_disabled() {
        let fixture = PermissionFixture::with_contexts(&[serde_json::json!({
            "type": "turn_context",
            "payload": {
                "sandbox_policy": {
                    "type": "workspace-write",
                    "network_access": false
                }
            }
        })]);
        let snapshot = CodexPermissionSnapshot::from_rollout(&fixture.rollout).unwrap();
        assert_eq!(snapshot.workspace_network(), Some(false));
    }

    #[test]
    fn parses_read_only_permissions() {
        let fixture = PermissionFixture::with_contexts(&[serde_json::json!({
            "type": "turn_context",
            "payload": {"sandbox_policy": {"type": "read-only"}}
        })]);
        assert_eq!(
            CodexPermissionSnapshot::from_rollout(&fixture.rollout)
                .unwrap()
                .sandbox(),
            CodexSandboxMode::ReadOnly
        );
    }

    #[test]
    fn invalid_latest_turn_does_not_reuse_older_full_access() {
        let fixture = PermissionFixture::with_contexts(&[
            serde_json::json!({
                "type": "turn_context",
                "payload": {"sandbox_policy": {"type": "danger-full-access"}}
            }),
            serde_json::json!({
                "type": "turn_context",
                "payload": {"sandbox_policy": {"type": "future-mode"}}
            }),
        ]);
        assert!(CodexPermissionSnapshot::from_rollout(&fixture.rollout).is_err());
    }

    #[test]
    fn missing_or_incomplete_permissions_fail_closed() {
        let no_context = PermissionFixture::with_contexts(&[]);
        assert!(CodexPermissionSnapshot::from_rollout(&no_context.rollout).is_err());

        let missing_network = PermissionFixture::with_contexts(&[serde_json::json!({
            "type": "turn_context",
            "payload": {"sandbox_policy": {"type": "workspace-write"}}
        })]);
        assert!(CodexPermissionSnapshot::from_rollout(&missing_network.rollout).is_err());
    }

    #[test]
    fn permission_snapshots_render_normalized_config_args_and_summaries() {
        let cases = [
            (
                CodexSandboxMode::ReadOnly,
                None,
                vec!["-c", "sandbox_mode=\"read-only\""],
                "read-only",
            ),
            (
                CodexSandboxMode::WorkspaceWrite,
                Some(false),
                vec![
                    "-c",
                    "sandbox_mode=\"workspace-write\"",
                    "-c",
                    "sandbox_workspace_write.network_access=false",
                ],
                "workspace-write, network disabled",
            ),
            (
                CodexSandboxMode::WorkspaceWrite,
                Some(true),
                vec![
                    "-c",
                    "sandbox_mode=\"workspace-write\"",
                    "-c",
                    "sandbox_workspace_write.network_access=true",
                ],
                "workspace-write, network enabled",
            ),
            (
                CodexSandboxMode::DangerFullAccess,
                None,
                vec!["-c", "sandbox_mode=\"danger-full-access\""],
                "danger-full-access",
            ),
        ];

        for (sandbox, network, args, summary) in cases {
            let snapshot = CodexPermissionSnapshot::from_handoff_parts(sandbox, network).unwrap();
            assert_eq!(snapshot.config_args(), args);
            assert_eq!(snapshot.summary(), summary);
        }
    }

    #[test]
    fn handoff_parts_reject_incoherent_network_values() {
        assert!(CodexPermissionSnapshot::from_handoff_parts(
            CodexSandboxMode::WorkspaceWrite,
            None,
        )
        .is_err());
        assert!(CodexPermissionSnapshot::from_handoff_parts(
            CodexSandboxMode::DangerFullAccess,
            Some(true),
        )
        .is_err());
    }

    #[test]
    fn resumed_invocation_uses_safe_default_policy_once() {
        let a = CodexAdapter;
        let brief = Brief::default();
        let inv = a.build_invocation(&ctx("sess-1", &brief, &[]));
        assert!(inv.args.iter().any(|arg| arg == "sess-1"));
        assert_eq!(
            inv.args
                .iter()
                .filter(|arg| arg.starts_with("sandbox_mode="))
                .collect::<Vec<_>>(),
            ["sandbox_mode=\"workspace-write\""]
        );
        assert_eq!(
            inv.args
                .iter()
                .filter(|arg| arg.starts_with("sandbox_workspace_write.network_access="))
                .collect::<Vec<_>>(),
            ["sandbox_workspace_write.network_access=false"]
        );
    }

    #[test]
    fn resumed_invocation_uses_persisted_full_access_without_duplicate_sandbox() {
        let a = CodexAdapter;
        let brief = Brief::default();
        let persisted =
            CodexPermissionSnapshot::from_handoff_parts(CodexSandboxMode::DangerFullAccess, None)
                .unwrap()
                .config_args();
        let inv = a.build_invocation(&ctx("sess-1", &brief, &persisted));
        assert_eq!(
            inv.args
                .iter()
                .filter(|arg| arg.starts_with("sandbox_mode="))
                .collect::<Vec<_>>(),
            ["sandbox_mode=\"danger-full-access\""]
        );
        assert!(inv
            .args
            .windows(2)
            .any(|pair| pair == ["-c", "approval_policy=\"never\""]));
    }

    #[test]
    fn fresh_invocation_uses_safe_default_policy_once() {
        let a = CodexAdapter;
        let brief = Brief::default();
        let inv = a.fresh_invocation(&ctx("", &brief, &[]), "nonce");
        assert_eq!(
            inv.args
                .iter()
                .filter(|arg| arg.starts_with("sandbox_mode="))
                .collect::<Vec<_>>(),
            ["sandbox_mode=\"workspace-write\""]
        );
        assert_eq!(
            inv.args
                .iter()
                .filter(|arg| arg.starts_with("sandbox_workspace_write.network_access="))
                .collect::<Vec<_>>(),
            ["sandbox_workspace_write.network_access=false"]
        );
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
