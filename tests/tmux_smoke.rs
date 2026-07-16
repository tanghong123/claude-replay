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

/// With more than one session, launching with no args shows the picker; opening a
/// session and pressing `Esc` returns to the list (not quit). `q` from the viewer
/// quits the program.
#[test]
#[ignore = "needs tmux; run with --ignored"]
fn esc_returns_from_viewer_to_session_list() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let bin = env!("CARGO_BIN_EXE_claude-replay");
    let dir = std::env::temp_dir().join(format!("peekv2-switch-{}", std::process::id()));
    // A single project dir holding two sessions → a 2-entry picker.
    let proj = dir.join("-tmp-proj");
    std::fs::create_dir_all(&proj).unwrap();
    let write = |name: &str, marker: &str| {
        std::fs::write(
            proj.join(name),
            format!(
                "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{marker} hello\"}}}}\n"
            ),
        )
        .unwrap();
    };
    write("a.jsonl", "AAAMARKER");
    write("b.jsonl", "BBBMARKER");

    let socket = format!("peekv2-switch-{}", std::process::id());
    tmux(&socket, &["kill-server"]);

    // No args → picker. CLAUDE_PROJECTS_DIR scopes discovery to our temp tree.
    let out = tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-x",
            "120",
            "-y",
            "30",
            &format!("CLAUDE_PROJECTS_DIR={} {bin}", dir.display()),
        ],
    );
    assert!(out.status.success(), "tmux new-session failed (no TTY?)");

    let capture = |socket: &str| -> String {
        let cap = tmux(socket, &["capture-pane", "-p", "-t", "0"]);
        String::from_utf8_lossy(&cap.stdout).to_string()
    };
    let wait_for = |socket: &str, needle: &str| -> (bool, String) {
        let mut screen = String::new();
        for _ in 0..20 {
            sleep(Duration::from_millis(150));
            screen = capture(socket);
            if screen.contains(needle) {
                return (true, screen);
            }
        }
        (false, screen)
    };

    // 1. Picker is shown.
    let (picker_ok, s1) = wait_for(&socket, "pick a session");
    // 2. Enter opens a session → viewer (a marker shows, picker header gone).
    tmux(&socket, &["send-keys", "-t", "0", "Enter"]);
    let (viewer_ok, s2) = wait_for(&socket, "MARKER");
    let viewer_not_picker = !s2.contains("pick a session");
    // 3. Esc returns to the list (the key behavior).
    tmux(&socket, &["send-keys", "-t", "0", "Escape"]);
    let (back_ok, s3) = wait_for(&socket, "pick a session");

    tmux(&socket, &["send-keys", "-t", "0", "Enter"]);
    sleep(Duration::from_millis(150));
    // 4. `q` from the viewer quits the program (pane no longer shows a marker).
    tmux(&socket, &["send-keys", "-t", "0", "q"]);
    sleep(Duration::from_millis(300));
    let s4 = capture(&socket);
    let quit_ok = !s4.contains("MARKER");

    tmux(&socket, &["kill-server"]);
    std::fs::remove_dir_all(&dir).ok();

    assert!(picker_ok, "picker not shown; screen:\n{s1}");
    assert!(viewer_ok, "session did not open on Enter; screen:\n{s2}");
    assert!(
        viewer_not_picker,
        "still in picker after Enter; screen:\n{s2}"
    );
    assert!(
        back_ok,
        "Esc did not return to the session list; screen:\n{s3}"
    );
    assert!(quit_ok, "q did not quit the viewer; screen:\n{s4}");
}
