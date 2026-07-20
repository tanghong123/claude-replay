//! The agent-agnostic supervisor: detach a long-lived worker, then run the retry
//! loop, driving each turn through the selected `AgentAdapter`. A port of
//! claude-jdi's `cmd_start` detach + `cmd_run` loop — the agent-specific decisions
//! (invocation, done-signal, mode transitions) live in the adapter's `classify`.

use super::agent::{self, Brief, Mode, TurnContext, TurnOutcome};
use super::state::Session;
use crate::Agent;
use anyhow::{bail, Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Re-exec `agent-jdi __run <id>` as a detached worker (its own process group, so
/// it survives the terminal/parent), logging to the session's `supervisor.log`.
/// Returns the worker pid.
pub fn spawn_detached(home: &Path, id: &str) -> Result<u32> {
    let exe = std::env::current_exe().context("locate agent-jdi executable")?;
    let session = Session::new(home, id);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(session.supervisor_log())
        .with_context(|| "open supervisor.log")?;
    let mut cmd = Command::new(exe);
    cmd.arg("__run")
        .arg(id)
        .env("AGENT_JDI_HOME", home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("spawn detached supervisor")?;
    Ok(child.id())
}

/// The detached worker body (`__run <id>`): load state, pick the adapter, and loop.
pub fn run_loop(home: &Path, id: &str) -> Result<()> {
    let session = Session::new(home, id);
    let get = |k: &str| session.meta_get(k);

    let cwd = get("cwd").context("meta missing cwd")?;
    let agent = get("agent")
        .and_then(|a| Agent::from_label(&a))
        .context("meta missing/invalid agent")?;
    let mut session_id = get("session_id").unwrap_or_default();
    let nonce = get("nonce").unwrap_or_default();
    let interval: u64 = get("interval").and_then(|s| s.parse().ok()).unwrap_or(600);
    let max_attempts: u32 = get("max_attempts")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut mode = get("mode")
        .and_then(|s| Mode::parse(&s))
        .unwrap_or(Mode::Execute);
    let extra_args: Vec<String> = fs::read_to_string(session.cargs_path())
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let brief = Brief {
        text: fs::read_to_string(session.dir.join("task.md")).unwrap_or_default(),
        backlog: Vec::new(),
    };

    let adapter = agent::adapter(agent);
    // The child runs *in* cwd (Command::current_dir); the supervisor itself never
    // chdirs, so this is safe to run concurrently.
    adapter.resolve_binary().inspect_err(|_| {
        session.meta_set("state", "failed").ok();
    })?;

    let cwd_path = Path::new(&cwd);
    let mut session_created = get("resumed").as_deref() == Some("true");
    // The first turn of a `start` run is a *fresh* invocation (feed the task); after
    // it we capture the assigned id (Codex) and drop into the continue mode.
    let mut fresh = mode == Mode::Start;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        session.meta_set("attempts", &attempt.to_string()).ok();
        session.meta_set("state", "running").ok();
        session.meta_set("mode", mode.as_str()).ok();

        let turn_ctx = TurnContext {
            mode,
            session_id: &session_id,
            session_created,
            cwd: cwd_path,
            brief: &brief,
            extra_args: &extra_args,
        };
        let inv = if fresh {
            adapter.fresh_invocation(&turn_ctx, &nonce)
        } else {
            adapter.build_invocation(&turn_ctx)
        };
        let (rc, capture) = run_turn(&inv, &session, cwd_path)?;

        if fresh {
            // Learn the id the agent assigned (Claude pinned it; Codex must capture).
            if session_id.is_empty() {
                match adapter.capture_session_id(&capture, cwd_path, &nonce) {
                    Some(id) => {
                        session_id = id;
                        session.meta_set("session_id", &session_id).ok();
                    }
                    None => {
                        session.meta_set("state", "failed").ok();
                        session
                            .meta_set("last_reason", "could not capture session id")
                            .ok();
                        return Ok(());
                    }
                }
            }
            // Record the transcript the run writes, so `log` can follow it.
            if let Some(t) = adapter
                .expected_transcript(&session_id, cwd_path)
                .or_else(|| adapter.transcript_path(&session_id, cwd_path))
            {
                session.meta_set("transcript", &t.to_string_lossy()).ok();
            }
            mode = adapter.continue_mode();
            session.meta_set("mode", mode.as_str()).ok();
            fresh = false;
        }
        session_created = true;

        // Classify against the current (post-fresh-transition) mode.
        let ctx = TurnContext {
            mode,
            session_id: &session_id,
            session_created,
            cwd: cwd_path,
            brief: &brief,
            extra_args: &extra_args,
        };
        match adapter.classify(rc, &capture, &ctx) {
            TurnOutcome::Done => {
                session.meta_set("state", "done").ok();
                session.meta_set("exit_code", "0").ok();
                return Ok(());
            }
            TurnOutcome::AdvanceMode(next) => {
                mode = next;
                attempt = attempt.saturating_sub(1); // the transition turn doesn't count
                continue;
            }
            TurnOutcome::RecreateSession => {
                session_created = false;
                continue;
            }
            TurnOutcome::Stopped(code) => {
                session.meta_set("state", "stopped").ok();
                session.meta_set("exit_code", &code.to_string()).ok();
                return Ok(());
            }
            TurnOutcome::Failed(code) => {
                session.meta_set("state", "failed").ok();
                session.meta_set("exit_code", &code.to_string()).ok();
                return Ok(());
            }
            TurnOutcome::GaveUp => {
                session.meta_set("state", "gaveup").ok();
                return Ok(());
            }
            TurnOutcome::Retry => {
                if max_attempts > 0 && attempt >= max_attempts {
                    session.meta_set("state", "gaveup").ok();
                    session.meta_set("exit_code", &rc.to_string()).ok();
                    return Ok(());
                }
                session.meta_set("state", "retrying").ok();
                std::thread::sleep(Duration::from_secs(interval));
            }
        }
    }
}

/// Run one agent turn: combined stdout+stderr → a capture file (read back for
/// `classify`) and appended to `output.log`. Returns `(exit_code, captured)`.
fn run_turn(inv: &agent::Invocation, session: &Session, cwd: &Path) -> Result<(i32, String)> {
    let cap = session.dir.join(format!("cap-{}", std::process::id()));
    let file = fs::File::create(&cap).with_context(|| "create capture file")?;
    let status = Command::new(&inv.program)
        .args(&inv.args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .with_context(|| format!("run {}", inv.program.display()))?;
    let captured = fs::read_to_string(&cap).unwrap_or_default();
    fs::remove_file(&cap).ok();
    if let Ok(mut out) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(session.output_log())
    {
        let _ = out.write_all(captured.as_bytes());
    }
    Ok((exit_code(&status), captured))
}

/// Exit code, mapping a signal death to `128 + signum` (so SIGINT→130, SIGTERM→143),
/// matching claude-jdi's 130/143 handling.
fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// Stop a supervised session: kill the worker's children and the worker itself,
/// then mark it stopped. Unix (`kill`/`pkill`); a no-op-with-error elsewhere.
pub fn takeover(session: &Session) -> Result<()> {
    let Some(pid) = session.pid() else {
        bail!("no supervisor pid recorded for this session");
    };
    #[cfg(unix)]
    {
        // Kill the agent children first, then the supervisor.
        Command::new("pkill")
            .arg("-P")
            .arg(pid.to_string())
            .status()
            .ok();
        Command::new("kill").arg(pid.to_string()).status().ok();
        if let Some(sid) = session.meta_get("session_id") {
            if !sid.is_empty() {
                Command::new("pkill").arg("-f").arg(&sid).status().ok();
            }
        }
    }
    #[cfg(not(unix))]
    {
        bail!("takeover is only supported on unix");
    }
    session.meta_set("state", "stopped")?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::jdi::state;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "agent-jdi-sup-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&d).ok();
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_codex(root: &Path, body: &str) -> PathBuf {
        let p = root.join("codex");
        fs::write(
            &p,
            format!("#!/bin/sh\nif [ \"$1\" = exec ]; then {body}; fi\nexit 0\n"),
        )
        .unwrap();
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&p, perm).unwrap();
        p
    }

    // The adapter reads `AGENT_JDI_CODEX_BIN` from the process env, so serialize the
    // tests that set it (Rust runs tests in parallel threads sharing the env).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set up a Codex session's meta, point the adapter at a fake `codex`, run the
    /// loop, and return the terminal state.
    fn drive(body: &str, max_attempts: u32) -> (state::RunState, PathBuf) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tmp();
        let home = root.join("home");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let codex = fake_codex(&root, body);

        let slot = "sess";
        let s = Session::new(&home, slot);
        s.ensure_dir().unwrap();
        s.meta_set("agent", "codex").unwrap();
        s.meta_set("cwd", &repo.to_string_lossy()).unwrap();
        s.meta_set("session_id", "sid-1").unwrap();
        s.meta_set("interval", "0").unwrap(); // no real sleep on retry
        s.meta_set("max_attempts", &max_attempts.to_string())
            .unwrap();
        s.meta_set("mode", "execute").unwrap();
        s.meta_set("resumed", "true").unwrap();

        std::env::set_var("AGENT_JDI_CODEX_BIN", &codex);
        run_loop(&home, slot).unwrap();
        std::env::remove_var("AGENT_JDI_CODEX_BIN");
        (s.state().unwrap(), root)
    }

    #[test]
    fn clean_turn_marks_done_and_captures_output() {
        let (state, root) = drive("echo hello-from-codex; exit 0", 0);
        assert_eq!(state, state::RunState::Done);
        // The turn's output was appended to output.log.
        let log = Session::new(&root.join("home"), "sess").output_log();
        assert!(fs::read_to_string(log)
            .unwrap()
            .contains("hello-from-codex"));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn failing_turn_retries_then_gives_up_at_max_attempts() {
        let (state, root) = drive("echo boom >&2; exit 1", 1);
        assert_eq!(state, state::RunState::GaveUp);
        fs::remove_dir_all(&root).ok();
    }

    /// A Codex `start` runs a fresh `codex exec` (no id), then recovers the id Codex
    /// assigned by finding the rollout carrying our nonce.
    #[test]
    fn codex_start_captures_the_assigned_session_id() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = tmp();
        let home = root.join("home");
        let repo = root.join("repo");
        let sessions = root.join("codex-sessions");
        fs::create_dir_all(&repo).unwrap();

        // Fake codex: on `exec`, write a rollout whose session_meta id is "cap-sid"
        // and whose body echoes all args (so it contains our nonce), then exit 0.
        let codex = root.join("codex");
        let script = r#"#!/bin/sh
if [ "$1" = exec ]; then
  D="$CODEX_SESSIONS_DIR/2026/07/21"; mkdir -p "$D"
  printf '{"type":"session_meta","payload":{"id":"cap-sid","cwd":"%s"}}\n' "$PWD" > "$D/rollout-cap.jsonl"
  printf '%s\n' "$*" >> "$D/rollout-cap.jsonl"
  exit 0
fi
exit 0
"#;
        fs::write(&codex, script).unwrap();
        let mut perm = fs::metadata(&codex).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&codex, perm).unwrap();

        let s = Session::new(&home, "sess");
        s.ensure_dir().unwrap();
        s.meta_set("agent", "codex").unwrap();
        s.meta_set("cwd", &repo.to_string_lossy()).unwrap();
        s.meta_set("session_id", "").unwrap();
        s.meta_set("nonce", "NONCE-run-1").unwrap();
        s.meta_set("interval", "0").unwrap();
        s.meta_set("max_attempts", "1").unwrap();
        s.meta_set("mode", "start").unwrap();
        s.meta_set("resumed", "false").unwrap();

        std::env::set_var("AGENT_JDI_CODEX_BIN", &codex);
        std::env::set_var("CODEX_SESSIONS_DIR", &sessions);
        run_loop(&home, "sess").unwrap();
        std::env::remove_var("AGENT_JDI_CODEX_BIN");
        std::env::remove_var("CODEX_SESSIONS_DIR");

        assert_eq!(
            s.meta_get("session_id").as_deref(),
            Some("cap-sid"),
            "captured the assigned id via the nonce"
        );
        assert_eq!(s.state(), Some(state::RunState::Done));
        fs::remove_dir_all(&root).ok();
    }
}
