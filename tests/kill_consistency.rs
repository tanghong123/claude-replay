//! **SIGKILL a real process** (#96 §12.1) — the half the truncation harness cannot reach.
//!
//! `claude-replay-agents/tests/crash_consistency.rs` enumerates every truncation of the two
//! streams and proves each one resumes correctly. That covers what a crash *produces*. It does
//! not prove the writer only ever produces those shapes: that both streams really are
//! append-only under a live fold, that nothing lands out of order between them, and that no
//! buffering surprise leaves a hole rather than a clean prefix.
//!
//! So this kills a real binary, mid-fold, against a growing transcript — `SIGKILL`, not
//! `SIGTERM`, because the point is the *ungraceful* path: no destructors, no `Drop`, no flush.
//! Whatever it leaves behind must still resume to exactly what a cold parse yields.
//!
//! Opt-in (`#[ignore]`) — it needs `tmux` (the established way to run the TUI headless) and it
//! is timing-sensitive. Run with:
//! `cargo test --test kill_consistency -- --ignored --nocapture`

use claude_replay::engine::meta_stream::Versions;
use claude_replay::{parse_session_as, Agent, Transcript};
use claude_replay_present::cache::{admit, Admission, Presentation, SessionCache};
use claude_replay_tui::store::{ArcLog, TuiNote};
use std::path::{Path, PathBuf};
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

/// Kills a test's private tmux server when the test ends — **including when it ends by
/// panicking**. The explicit `kill-server` calls below are all on the happy path, so a failed
/// assertion stranded the server and the viewer running inside it: one such pair was found still
/// alive 3.5 days later, holding a cache entry's lock (#164).
struct Server(String);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .args(["-L", &self.0, "kill-server"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn user(t: &str, s: u32) -> String {
    format!("{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}]}},\"timestamp\":\"2026-07-26T10:{:02}:{:02}Z\"}}\n", s / 60, s % 60)
}
fn asst(t: &str, s: u32) -> String {
    format!("{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":5,\"output_tokens\":8}}}},\"timestamp\":\"2026-07-26T10:{:02}:{:02}Z\"}}\n", s / 60, s % 60)
}

/// Wait for `p` to appear, up to `limit`. Returns whether it did.
///
/// The kill timer must start from "the viewer is up and folding", not from "tmux returned" — a
/// debug binary takes long enough to start that a fixed sleep would otherwise kill it before it
/// had written anything, and the test would pass by exercising nothing.
fn wait_for(p: &Path, limit: Duration) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < limit {
        if p.exists() {
            return true;
        }
        sleep(Duration::from_millis(20));
    }
    false
}

fn append(p: &Path, s: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

/// Where a run with `XDG_CACHE_HOME = cache_home` keeps `src`'s entry.
fn entry_of(cache_home: &Path, src: &Path) -> PathBuf {
    admit::entry_dir(
        &cache_home.join("claude-replay").join("sessions"),
        Presentation::Tui,
        &src.file_stem().unwrap().to_string_lossy(),
    )
}

/// Load the durable entry the killed process left and fold to EOF, exactly as a next run would.
fn resume_and_fold(root: &Path, src: &Path) -> (Vec<claude_replay::model::Block>, admit::Origin) {
    // #167 step 3: liveness lives on the provider — the killed process is gone, so any
    // holder is dead and its lock reclaims.
    let c: SessionCache<ArcLog, (), claude_replay_present::cache::PerSession<TuiNote>> =
        SessionCache::with_entries(
            claude_replay_present::cache::PerSession::<TuiNote>::new(
                root.to_path_buf(),
                Presentation::Tui,
                Versions::current(None),
            )
            .liveness(|_| false),
        );
    let id = src.file_stem().unwrap().to_string_lossy().to_string();
    c.register(&id, Transcript::open(Agent::CLAUDE, src.to_path_buf()));
    let (session, origin) = match c.admit(&id, |dir| ArcLog::open_append(&dir.join("blocks.jsonl")))
    {
        Admission::Owned { session, origin } => (session, origin),
        Admission::Denied(d) => panic!("a dead holder must not deny: {d:?}"),
    };
    let d = c
        .poll_view(&id, ArcLog::memory)
        .expect("registered")
        .expect("readable");
    let mut blocks: Vec<_> = session
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    blocks.extend(d.provisional.iter().map(|a| a.as_ref().clone()));
    (blocks, origin)
}

/// Kill the viewer at `kill_after` while it is following a growing transcript, then prove the
/// durable entry it left behind resumes to exactly what a cold parse yields.
fn kill_at(kill_after: Duration, label: &str) {
    let bin = env!("CARGO_BIN_EXE_claude-replay");
    let dir = std::env::temp_dir().join(format!(
        "cr-kill-{}-{label}-{}",
        std::process::id(),
        kill_after.as_millis()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache_home = dir.join("cache"); // NEVER the developer's real cache
    std::fs::create_dir_all(&cache_home).unwrap();
    let src = dir.join("s.jsonl");

    // Enough turns that the fold is still committing when the kill lands.
    for i in 0..40 {
        append(&src, &user(&format!("ask {i}"), i * 4));
        append(&src, &asst(&format!("reply {i}"), i * 4 + 1));
    }

    let socket = format!("cr-kill-{}-{label}", std::process::id());

    let _server = Server(socket.clone());
    tmux(&socket, &["kill-server"]);
    let out = tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-x",
            "120",
            "-y",
            "30",
            "-e",
            &format!("XDG_CACHE_HOME={}", cache_home.display()),
            &format!("{bin} -f {}", src.display()),
        ],
    );
    assert!(out.status.success(), "tmux new-session: {out:?}");

    // Grow the transcript under it, so the kill lands mid-fold rather than on a quiet session.
    let grower = {
        let src = src.clone();
        std::thread::spawn(move || {
            for i in 40..120 {
                append(&src, &user(&format!("ask {i}"), i * 4));
                append(&src, &asst(&format!("reply {i}"), i * 4 + 1));
                sleep(Duration::from_millis(4));
            }
        })
    };

    // Start the clock only once the viewer has actually created its entry.
    assert!(
        wait_for(
            &entry_of(&cache_home, &src).join("meta.jsonl"),
            Duration::from_secs(10)
        ),
        "[{label}] the viewer never created a durable entry — check tmux/-e support"
    );
    sleep(kill_after);
    // SIGKILL the viewer itself, not the pane's shell: -9 so no destructor, no flush, no
    // `release_all` — the ungraceful path this test exists for.
    let pids = String::from_utf8_lossy(&tmux(&socket, &["list-panes", "-F", "#{pane_pid}"]).stdout)
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    for pid in &pids {
        let _ = Command::new("pkill").args(["-9", "-P", pid]).status();
        let _ = Command::new("kill").args(["-9", pid]).status();
    }
    grower.join().unwrap();
    tmux(&socket, &["kill-server"]);
    sleep(Duration::from_millis(120));

    let root = cache_home.join("claude-replay").join("sessions");
    let (got, origin) = resume_and_fold(&root, &src);
    let want = parse_session_as(Agent::CLAUDE, &src).unwrap().blocks();
    assert_eq!(
        got, want,
        "[{label}] after SIGKILL at {kill_after:?}, the resumed session must equal a cold parse"
    );
    eprintln!(
        "[{label}] killed at {kill_after:?} → {origin:?}, {} blocks",
        got.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Killed at several points across the fold. Each is a different interleaving of "content
/// written / meta written / neither", which is the space this test samples and the truncation
/// harness enumerates.
#[test]
#[ignore = "needs tmux; run with --ignored"]
fn sigkill_mid_fold_still_resumes_to_a_cold_parse() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    for ms in [120u64, 200, 320, 450] {
        kill_at(Duration::from_millis(ms), "grow");
    }
}

/// The same, but the source stops growing before the kill — so the process is killed while
/// idle, holding a lock, with a complete stream. The next run must reclaim the lock (its holder
/// is dead) and resume, not refuse.
#[test]
#[ignore = "needs tmux; run with --ignored"]
fn a_killed_holders_lock_is_reclaimed_by_the_next_run() {
    if !have_tmux() {
        eprintln!("skipping: tmux not installed");
        return;
    }
    let bin = env!("CARGO_BIN_EXE_claude-replay");
    let dir = std::env::temp_dir().join(format!("cr-kill-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache_home = dir.join("cache");
    std::fs::create_dir_all(&cache_home).unwrap();
    let src = dir.join("s.jsonl");
    for i in 0..30 {
        append(&src, &user(&format!("ask {i}"), i * 4));
        append(&src, &asst(&format!("reply {i}"), i * 4 + 1));
    }

    let socket = format!("cr-kill-lock-{}", std::process::id());

    let _server = Server(socket.clone());
    tmux(&socket, &["kill-server"]);
    tmux(
        &socket,
        &[
            "new-session",
            "-d",
            "-x",
            "120",
            "-y",
            "30",
            "-e",
            &format!("XDG_CACHE_HOME={}", cache_home.display()),
            &format!("{bin} -f {}", src.display()),
        ],
    );
    let root = cache_home.join("claude-replay").join("sessions");
    let entry = entry_of(&cache_home, &src);
    assert!(
        wait_for(&entry.join("LOCK"), Duration::from_secs(10)),
        "the live viewer takes and holds its lock"
    );
    sleep(Duration::from_millis(400)); // let it settle with the whole file folded

    for pid in
        String::from_utf8_lossy(&tmux(&socket, &["list-panes", "-F", "#{pane_pid}"]).stdout).lines()
    {
        let _ = Command::new("pkill").args(["-9", "-P", pid]).status();
        let _ = Command::new("kill").args(["-9", pid]).status();
    }
    tmux(&socket, &["kill-server"]);
    sleep(Duration::from_millis(120));

    assert!(
        entry.join("LOCK").exists(),
        "a SIGKILLed process cannot release — the stale lock is exactly the case to handle"
    );
    let (got, origin) = resume_and_fold(&root, &src);
    assert!(
        matches!(origin, admit::Origin::Resumed { .. }),
        "the next run reclaims the dead holder's lock and resumes, got {origin:?}"
    );
    assert_eq!(got, parse_session_as(Agent::CLAUDE, &src).unwrap().blocks());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Guard rail for the two tests above: an isolated `XDG_CACHE_HOME` must actually isolate them.
/// This one is NOT `#[ignore]`d — it needs no tmux, and it is the check that keeps a timing test
/// from quietly writing into the developer's real cache.
#[test]
fn an_isolated_cache_home_keeps_the_kill_tests_off_the_real_root() {
    let scratch = std::env::temp_dir().join(format!("cr-kill-guard-{}", std::process::id()));
    let entry = entry_of(&scratch, Path::new("/anywhere/s.jsonl"));
    assert!(
        entry.starts_with(&scratch),
        "the entry must live under the scratch cache home: {}",
        entry.display()
    );
    if let Some(real) = admit::default_root() {
        assert!(
            !entry.starts_with(&real),
            "an isolated run must never land under the real root {}",
            real.display()
        );
    }
}
