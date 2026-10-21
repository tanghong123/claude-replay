//! #s204: a paired monitor started where no person reads its output — as its LaunchAgent runs it,
//! both streams appended to one log file — writes no token there, on a fresh start or on the
//! hand-off to a monitor already running. The pure rule is `control::printed_url`'s test; this is
//! the binary, so a print site that bypasses the rule fails here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cm-startup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The monitor with every store, state and cache root inside `base`, its output in `log` (one
/// file for both streams, as launchd's StandardOutPath and StandardErrorPath name one file).
fn monitor(base: &Path, log: &Path, port: u16) -> Command {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    let err = out.try_clone().unwrap();
    let stores = base.join("stores");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-monitor"));
    cmd.args(["--pair", "--no-open", "--port", &port.to_string()])
        .env("HOME", base.join("home"))
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_STATE_HOME", base.join("xdg-state"))
        .env("XDG_CACHE_HOME", base.join("xdg-cache"))
        .env("AGENT_MONITOR_STATE", base.join("state"))
        .env("CLAUDE_MONITOR_STATE", base.join("state"))
        .env("AGENT_MONITOR_CACHE", base.join("cache"))
        .env("CLAUDE_MONITOR_CACHE", base.join("cache"))
        .env("CLAUDE_PROJECTS_DIR", stores.join("claude"))
        .env("CLAUDE_SCRATCH_ROOT", stores.join("claude-scratch"))
        .env("CODEX_HOME", stores.join("codex"))
        .env("CODEX_SESSIONS_DIR", stores.join("codex/sessions"))
        .env("QODER_PROJECTS_DIR", stores.join("qoder"))
        .env("QODERWORK_PROJECTS_DIR", stores.join("qoderwork"))
        .env("QODERWORK_DB", stores.join("qoderwork.db"))
        .env("QWENWORK_PROJECTS_DIR", stores.join("qwenwork"))
        .env("QWENWORK_DB", stores.join("qwenwork.db"))
        .env_remove("CODEX_NATIVE_SERVER_URL")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    cmd
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the serving monitor however the test ends, so a failed assertion strands nothing.
struct Reap(std::process::Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_paired_monitor_writes_no_token_to_a_log() {
    let base = scratch("paired");
    let port = free_port();
    let log = base.join("agent-monitor.log");
    let serving = Reap(monitor(&base, &log, port).spawn().unwrap());

    let deadline = Instant::now() + Duration::from_secs(60);
    let started = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("agent-monitor serving") && text.lines().count() >= 3 {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the monitor never said it was serving; its log: {text}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    // The hand-off: a second start finds the first and prints where it is, then exits.
    let handoff_log = base.join("handoff.log");
    let status = monitor(&base, &handoff_log, port).status().unwrap();
    assert!(status.success(), "the hand-off exited with {status}");
    let handoff = std::fs::read_to_string(&handoff_log).unwrap();
    drop(serving);

    let token = std::fs::read_to_string(base.join("state/auth-token")).unwrap();
    let token = token.trim();
    assert!(!token.is_empty(), "--pair minted no token");
    let url = format!("http://127.0.0.1:{port}/");
    for (what, text) in [("start", &started), ("hand-off", &handoff)] {
        // Neither the token nor any part of a tokened URL, so the check holds without echoing it.
        assert!(
            !text.contains(token),
            "the {what} wrote the token to its log"
        );
        assert!(
            !text.contains("?token="),
            "the {what} wrote a tokened URL to its log"
        );
        assert!(
            text.lines().any(|l| l.trim() == url),
            "the {what} printed no bare URL on a line of its own: {text}"
        );
        assert!(
            text.contains("agent-monitor --pair` in a terminal"),
            "the {what} did not say where the token is: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
