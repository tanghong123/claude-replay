//! #327: `scripts/tap-clean.sh`, which `corp-publish.sh` asks at preflight and at verify whether
//! the installed corp tap clone is clean. The clone is shared with alibrew, which rewrites its own
//! files there while it updates: a dirty path that is not one of our formulae is waited out, while
//! one of ours stops the publish at once. Against a hermetic git repository shaped like the tap.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tap-clean.sh")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A clean tap: our formula and alibrew's own, committed.
fn tap(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tap-clean-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Formula")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(
        dir.join("Formula/agent-replay.rb"),
        "class AgentReplay\nend\n",
    )
    .unwrap();
    std::fs::write(dir.join("Formula/alibrew.rb"), "class Alibrew\nend\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// Run the check: (exit code, stdout, stderr, how long it took).
fn check(dir: &Path, step: u32, max: u32) -> (i32, String, String, Duration) {
    let t0 = Instant::now();
    let out = Command::new("sh")
        .arg(script())
        .args([dir.to_str().unwrap(), "verify", "why it matters"])
        .env("TAP_WAIT_STEP", step.to_string())
        .env("TAP_WAIT_MAX", max.to_string())
        .env(
            "TOOLS",
            "agent-replay agent-monitor agent-monitor-fleet agent-jdi",
        )
        .output()
        .expect("sh");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        t0.elapsed(),
    )
}

#[test]
fn a_clean_tap_passes_at_once() {
    let dir = tap("clean");
    let (code, _, err, took) = check(&dir, 1, 10);
    assert_eq!(code, 0, "{err}");
    assert!(took < Duration::from_secs(2), "no waiting: {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Our formula, edited and not committed: the state that hid five unpublished releases. Never
/// waited on.
#[test]
fn one_of_our_formulae_dirty_stops_at_once() {
    let dir = tap("ours");
    std::fs::write(
        dir.join("Formula/agent-replay.rb"),
        "class AgentReplay\n  # edited\nend\n",
    )
    .unwrap();
    let (code, _, err, took) = check(&dir, 5, 60);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("one of our formulae is dirty"), "{err}");
    assert!(
        err.contains("why it matters"),
        "the caller's reason is given: {err}"
    );
    assert!(
        took < Duration::from_secs(4),
        "stopped without waiting: {took:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// alibrew updating itself: an untracked file appears and goes away again. The check waits it
/// out, saying so, and then passes.
#[test]
fn a_tap_alibrew_is_updating_is_waited_out() {
    let dir = tap("alibrew");
    std::fs::write(dir.join("install.sh"), "#!/bin/sh\n").unwrap();
    let gone = dir.join("install.sh");
    let cleaner = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        std::fs::remove_file(gone).unwrap();
    });
    let (code, out, err, took) = check(&dir, 1, 10);
    cleaner.join().unwrap();
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("changing under us") && out.contains("install.sh"),
        "it says what it waits on: {out}"
    );
    assert!(took >= Duration::from_secs(1), "it waited: {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Someone else's file that stays dirty is not waited on forever.
#[test]
fn a_tap_that_stays_dirty_stops_after_the_wait() {
    let dir = tap("stuck");
    std::fs::write(
        dir.join("Formula/alibrew.rb"),
        "class Alibrew\n  # mid-update\nend\n",
    )
    .unwrap();
    let (code, _, err, took) = check(&dir, 1, 2);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("still dirty after 2s") && err.contains("none of it ours"),
        "{err}"
    );
    assert!(took >= Duration::from_secs(2), "it waited first: {took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
