//! End-to-end smoke: drive the real binary inside a private tmux server with NO
//! controlling TTY (the headless case). Proves the agent-spawns-tmux approach.
//!
//! Opt-in (`#[ignore]`) because it needs `tmux` and is timing-sensitive; the
//! default suite verifies the same behavior deterministically via `TestBackend`.
//! Run with:  cargo test --test tmux_smoke -- --ignored --nocapture

use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .arg("-L")
        .arg(socket)
        .args(args)
        .output()
        .expect("run tmux")
}

#[test]
#[ignore = "needs tmux; run with --ignored"]
fn drives_real_binary_in_headless_tmux() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let bin = env!("CARGO_BIN_EXE_claude-replay");
    let dir = std::env::temp_dir().join(format!("peekv2-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let transcript = dir.join("s.jsonl");
    std::fs::write(
        &transcript,
        b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"SMOKE_MARKER hello\"}}\n",
    )
    .unwrap();

    let socket = format!("peekv2-e2e-{}", std::process::id());
    tmux(&socket, &["kill-server"]); // ignore failure

    let out = tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-x",
            "120",
            "-y",
            "30",
            &format!("{bin} {}", transcript.display()),
        ],
    );
    assert!(out.status.success(), "tmux new-session failed (no TTY?)");

    // Poll capture-pane until the rendered marker shows up (or time out).
    let mut screen = String::new();
    let mut ok = false;
    for _ in 0..20 {
        sleep(Duration::from_millis(150));
        let cap = tmux(&socket, &["capture-pane", "-p", "-t", "0"]);
        screen = String::from_utf8_lossy(&cap.stdout).to_string();
        if screen.contains("SMOKE_MARKER") {
            ok = true;
            break;
        }
    }

    tmux(&socket, &["send-keys", "-t", "0", "q"]);
    sleep(Duration::from_millis(200));
    tmux(&socket, &["kill-server"]);
    std::fs::remove_dir_all(&dir).ok();

    assert!(ok, "TUI did not render the marker; last screen:\n{screen}");
}
