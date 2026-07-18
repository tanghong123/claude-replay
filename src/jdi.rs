use crate::codex_discover::{self, CodexSession};
use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_PROMPT: &str = "Continue the current task. Do as much as you can without asking the user. If an action is blocked, try safe alternatives and clearly record any remaining blocker in your final response.";

#[derive(Parser, Debug)]
#[command(
    name = "codex-jdi",
    version,
    about = "Continue a Codex session headlessly and follow it with codex-replay."
)]
struct JdiArgs {
    #[command(subcommand)]
    command: JdiCommand,
}

#[derive(Subcommand, Debug)]
enum JdiCommand {
    /// Resume the newest interactive session for the current repository.
    Resume {
        /// Additional instruction appended to the built-in persistence prompt.
        instruction: Vec<String>,
    },
    /// Reattach codex-replay to the last JDI session for this repository.
    Log,
}

#[derive(Debug, Clone)]
struct SupervisorConfig {
    codex_bin: PathBuf,
    replay_bin: PathBuf,
    state_dir: PathBuf,
    sessions_dir: PathBuf,
    cwd: PathBuf,
}

impl SupervisorConfig {
    fn from_env() -> Result<Self> {
        let cwd = std::env::current_dir().context("determine current working directory")?;
        Ok(Self {
            codex_bin: std::env::var_os("CODEX_JDI_CODEX_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("codex")),
            replay_bin: std::env::var_os("CODEX_JDI_REPLAY_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("codex-replay")),
            state_dir: std::env::var_os("CODEX_JDI_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| codex_discover::codex_home().join("jdi")),
            sessions_dir: codex_discover::sessions_dir(),
            cwd,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResumeState {
    session_id: String,
    rollout_path: PathBuf,
    cwd: PathBuf,
    pid: u32,
    diagnostic_log: PathBuf,
    started_at_unix: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    program: PathBuf,
    args: Vec<String>,
}

pub fn run() -> Result<()> {
    let args = JdiArgs::parse();
    let config = SupervisorConfig::from_env()?;
    match args.command {
        JdiCommand::Resume { instruction } => resume(&config, &instruction),
        JdiCommand::Log => open_log(&config),
    }
}

fn continuation_prompt(extra: &[String]) -> String {
    if extra.is_empty() {
        DEFAULT_PROMPT.to_string()
    } else {
        format!(
            "{DEFAULT_PROMPT}\n\nAdditional instruction: {}",
            extra.join(" ")
        )
    }
}

fn resume_invocation(program: PathBuf, session: &CodexSession, prompt: &str) -> Invocation {
    Invocation {
        program,
        args: vec![
            "exec".into(),
            "resume".into(),
            "-c".into(),
            "approval_policy=\"never\"".into(),
            "-c".into(),
            "sandbox_mode=\"workspace-write\"".into(),
            "--json".into(),
            session.id.clone(),
            prompt.to_string(),
        ],
    }
}

fn check_command(
    program: &Path,
    args: &[&str],
    missing_help: &str,
    failure_help: &str,
) -> Result<()> {
    let output = Command::new(program).args(args).output().map_err(|error| {
        anyhow!(
            "could not run {}: {error}\n{missing_help}",
            program.display()
        )
    })?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    bail!(
        "{} failed: {}\n{failure_help}",
        program.display(),
        detail.trim()
    );
}

fn check_prerequisites(config: &SupervisorConfig, needs_codex: bool) -> Result<()> {
    if needs_codex {
        check_command(
            &config.codex_bin,
            &["--version"],
            "Install Codex CLI: curl -fsSL https://chatgpt.com/codex/install.sh | sh",
            "Reinstall or update Codex CLI, then run codex --version.",
        )?;
        check_command(
            &config.codex_bin,
            &["login", "status"],
            "Install Codex CLI first.",
            "Authenticate with: codex login",
        )?;
    }
    check_command(
        &config.replay_bin,
        &["--version"],
        "Install the Codex replay tools: brew install tanghong123/tap/codex-replay",
        "Reinstall codex-replay and verify codex-replay --version.",
    )
}

fn state_key(cwd: &Path) -> String {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn state_path(config: &SupervisorConfig) -> PathBuf {
    config
        .state_dir
        .join(format!("{}.json", state_key(&config.cwd)))
}

fn save_state(config: &SupervisorConfig, state: &ResumeState) -> Result<()> {
    fs::create_dir_all(&config.state_dir)
        .with_context(|| format!("create JDI state directory {}", config.state_dir.display()))?;
    let destination = state_path(config);
    let temporary = destination.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&temporary, &destination)?;
    Ok(())
}

fn load_state(config: &SupervisorConfig) -> Result<ResumeState> {
    let path = state_path(config);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "no Codex JDI session recorded for {}; run codex-jdi resume first",
            config.cwd.display()
        )
    })?;
    let state: ResumeState = serde_json::from_slice(&bytes)
        .with_context(|| format!("read JDI state from {}", path.display()))?;
    Ok(state)
}

fn safe_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn diagnostic_tail(path: &Path) -> String {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<_> = content.lines().rev().take(20).collect();
    lines.reverse();
    lines.join("\n")
}

fn run_replay(replay_bin: &Path, rollout: &Path) -> Result<()> {
    let status = Command::new(replay_bin)
        .arg(rollout)
        .arg("--follow")
        .status()
        .with_context(|| format!("start {}", replay_bin.display()))?;
    if !status.success() {
        bail!("{} exited with {status}", replay_bin.display());
    }
    Ok(())
}

fn resume(config: &SupervisorConfig, extra: &[String]) -> Result<()> {
    check_prerequisites(config, true)?;
    let session = codex_discover::latest_interactive_for_cwd_in(&config.sessions_dir, &config.cwd)?;
    fs::create_dir_all(&config.state_dir)?;
    let diagnostic_log = config
        .state_dir
        .join(format!("{}.log", safe_id(&session.id)));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&diagnostic_log)
        .with_context(|| format!("open diagnostic log {}", diagnostic_log.display()))?;
    let invocation = resume_invocation(
        config.codex_bin.clone(),
        &session,
        &continuation_prompt(extra),
    );
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .current_dir(&session.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().with_context(|| {
        format!(
            "start headless Codex for session {} (see {})",
            session.id,
            diagnostic_log.display()
        )
    })?;
    std::thread::sleep(Duration::from_millis(150));
    if let Some(status) = child.try_wait()? {
        if !status.success() {
            bail!(
                "headless Codex exited immediately with {status}\n{}\nDiagnostic log: {}",
                diagnostic_tail(&diagnostic_log),
                diagnostic_log.display()
            );
        }
    }

    let state = ResumeState {
        session_id: session.id,
        rollout_path: session.path,
        cwd: session.cwd,
        pid: child.id(),
        diagnostic_log: diagnostic_log.clone(),
        started_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    save_state(config, &state)?;
    eprintln!(
        "Codex JDI worker {} is running for session {}. Press q to leave replay; the worker will continue.",
        state.pid, state.session_id
    );
    run_replay(&config.replay_bin, &state.rollout_path)?;
    eprintln!(
        "Replay closed. Codex worker PID {} was left running. Diagnostic log: {}",
        state.pid,
        state.diagnostic_log.display()
    );
    Ok(())
}

fn open_log(config: &SupervisorConfig) -> Result<()> {
    check_prerequisites(config, false)?;
    let state = load_state(config)?;
    run_replay(&config.replay_bin, &state.rollout_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn invocation_is_exact_sandboxed_and_noninteractive() {
        let session = crate::codex_discover::CodexSession {
            id: "session-1".into(),
            path: PathBuf::from("/tmp/rollout.jsonl"),
            cwd: PathBuf::from("/tmp/repo"),
            mtime: std::time::SystemTime::UNIX_EPOCH,
            interactive: true,
        };
        let invocation = resume_invocation(PathBuf::from("codex"), &session, "Continue");
        assert_eq!(invocation.program, PathBuf::from("codex"));
        assert!(invocation
            .args
            .windows(2)
            .any(|window| window == ["-c", "approval_policy=\"never\""]));
        assert!(invocation
            .args
            .windows(2)
            .any(|window| window == ["-c", "sandbox_mode=\"workspace-write\""]));
        assert!(invocation.args.iter().any(|arg| arg == "session-1"));
        assert!(!invocation.args.iter().any(|arg| arg == "--last"));
        assert!(!invocation
            .args
            .iter()
            .any(|arg| arg.contains("dangerously-bypass")));
    }

    #[test]
    fn default_prompt_keeps_persistence_instruction_when_extra_is_added() {
        let prompt = continuation_prompt(&["Prioritize tests".into()]);
        assert!(prompt.contains("without asking the user"));
        assert!(prompt.contains("Prioritize tests"));
    }

    #[cfg(unix)]
    struct Fixture {
        root: PathBuf,
        config: SupervisorConfig,
        replay_args: PathBuf,
    }

    #[cfg(unix)]
    impl Fixture {
        fn new(worker_body: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "codex-jdi-test-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::remove_dir_all(&root).ok();
            let repo = root.join("repo");
            let sessions = root.join("sessions");
            let rollout_dir = sessions.join("2026/07/18");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::create_dir_all(&rollout_dir).unwrap();
            let rollout = rollout_dir.join("rollout-session-1.jsonl");
            let meta = serde_json::json!({
                "type": "session_meta",
                "payload": {
                    "id": "session-1",
                    "cwd": repo,
                    "originator": "codex-tui",
                    "source": "cli"
                }
            });
            std::fs::write(&rollout, format!("{meta}\n")).unwrap();

            let codex = root.join("codex");
            write_executable(
                &codex,
                &format!(
                    "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo codex-cli-test; exit 0; fi\nif [ \"$1\" = \"login\" ]; then echo 'Logged in'; exit 0; fi\nif [ \"$1\" = \"exec\" ]; then echo \"ARGS:$*\"; {worker_body}; fi\nexit 9\n"
                ),
            );
            let replay_args = root.join("replay-args");
            let replay = root.join("codex-replay");
            write_executable(
                &replay,
                &format!(
                    "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo replay-test; exit 0; fi\nprintf '%s\\n' \"$@\" > '{}'; exit 0\n",
                    replay_args.display()
                ),
            );
            let config = SupervisorConfig {
                codex_bin: codex,
                replay_bin: replay,
                state_dir: root.join("state"),
                sessions_dir: sessions,
                cwd: repo,
            };
            Self {
                root,
                config,
                replay_args,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replay_exit_leaves_worker_running_and_log_can_reattach() {
        let fixture = Fixture::new("sleep 5; exit 0");
        resume(&fixture.config, &[]).unwrap();
        let state = load_state(&fixture.config).unwrap();
        let alive = Command::new("kill")
            .arg("-0")
            .arg(state.pid.to_string())
            .status()
            .unwrap()
            .success();
        assert!(alive, "worker did not survive replay exit");
        let diagnostic = std::fs::read_to_string(&state.diagnostic_log).unwrap();
        assert!(diagnostic.contains("approval_policy=\"never\""));
        assert!(diagnostic.contains("sandbox_mode=\"workspace-write\""));
        assert!(diagnostic.contains("session-1"));
        assert!(!diagnostic.contains("--last"));
        open_log(&fixture.config).unwrap();
        let replay_args = std::fs::read_to_string(&fixture.replay_args).unwrap();
        assert!(replay_args.contains("rollout-session-1.jsonl"));
        assert!(replay_args.contains("--follow"));
        Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{}", state.pid))
            .status()
            .ok();
    }

    #[cfg(unix)]
    #[test]
    fn immediate_worker_failure_surfaces_diagnostic_tail() {
        let fixture = Fixture::new("echo boom >&2; exit 7");
        let error = resume(&fixture.config, &[]).unwrap_err().to_string();
        assert!(error.contains("boom"), "error: {error}");
        assert!(error.contains("Diagnostic log:"), "error: {error}");
    }
}
