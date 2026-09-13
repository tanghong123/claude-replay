//! #205: `scripts/sweep-scratch.sh` prunes the browser harness's scratch roots by the LIVENESS
//! of the pid in their names — a root whose test process is gone goes however young, a live
//! run's roots stay — and everything else by age, against a scratch directory of this test's
//! own making with a marker process standing in for a live run.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/sweep-scratch.sh")
}

fn run(dir: &Path, dry: bool) -> String {
    let mut cmd = Command::new("bash");
    cmd.arg(script()).arg(dir);
    if dry {
        cmd.arg("--dry-run");
    }
    let out = cmd.output().expect("bash scripts/sweep-scratch.sh");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn mk(dir: &Path, name: &str) -> PathBuf {
    let d = dir.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("payload"), "x").unwrap();
    d
}

/// A pid that is certainly dead: a process that has already been reaped.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[test]
fn harness_roots_go_by_liveness_and_the_rest_by_age() {
    let dir = std::env::temp_dir().join(format!("sweep-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A live run: a marker process whose pid the roots carry.
    let mut live = Command::new("sleep").arg("60").spawn().unwrap();
    let live_pid = live.id();
    let dead = dead_pid();
    let live_follow = mk(&dir, &format!("cr-browser-follow-{live_pid}-scenario-x"));
    let live_state = mk(&dir, &format!("cr-browser-state-{live_pid}"));
    let live_chrome = mk(&dir, &format!("cr-browser-chrome-{live_pid}-1"));
    let dead_follow = mk(&dir, &format!("cr-browser-follow-{dead}-scenario-y"));
    let dead_state = mk(&dir, &format!("cr-browser-state-{dead}"));
    let dead_chrome = mk(&dir, &format!("cr-browser-chrome-{dead}-2"));
    // Names without a pid: young stays, old goes.
    let young = mk(&dir, "sc-young");
    let old = mk(&dir, "sc-old");
    let status = Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(&old)
        .status()
        .unwrap();
    assert!(status.success(), "touch -t");
    // A name the sweep does not own stays whatever its age.
    let foreign = mk(&dir, "debug");

    let dry = run(&dir, true);
    assert!(
        dry.contains("would drop 3 ("),
        "the dry run names the three dead roots and deletes nothing: {dry}"
    );
    assert!(
        dead_follow.exists() && dead_state.exists() && dead_chrome.exists(),
        "dry run"
    );

    let out = run(&dir, false);
    assert!(out.contains("dead test processes: dropped 3 ("), "{out}");
    assert!(out.contains("older than a day: dropped 1"), "{out}");
    for d in [&live_follow, &live_state, &live_chrome] {
        assert!(d.exists(), "a live run's root stays: {}", d.display());
    }
    for d in [&dead_follow, &dead_state, &dead_chrome] {
        assert!(!d.exists(), "a dead pid's root goes: {}", d.display());
    }
    assert!(young.exists(), "young scratch without a pid stays");
    assert!(!old.exists(), "old scratch without a pid goes");
    assert!(
        foreign.exists(),
        "a name the sweep does not own is untouched"
    );

    let _ = live.kill();
    let _ = live.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
