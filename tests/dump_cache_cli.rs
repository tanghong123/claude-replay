//! `agent-replay --dump - --json --cache` through the real binary (#10): the flag's contract as a
//! shell-out consumer meets it. The stream is the plain dump's bytes; the plain dump still writes
//! nothing and takes no lock; and the flag is refused where it would mean nothing. The cache home
//! is the case's scratch, so no entry on this machine is read, written or reclaimed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-dump-cache-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `agent-replay <args>`, its cache home under `root`.
fn run(root: &Path, args: &[&str]) -> (Vec<u8>, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
        .args(args)
        .env("CLAUDE_REPLAY_CACHE", root.join("cache"))
        .output()
        .unwrap();
    (
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// A short working session: two turns, each a prompt, a tool call, its result and an answer.
fn transcript(dir: &Path) -> PathBuf {
    let path = dir.join("0f0e0d0c-0000-4000-8000-000000000010.jsonl");
    let mut body = String::new();
    for t in 0..2 {
        body.push_str(&format!(
            concat!(
                r#"{{"type":"user","cwd":"/w","timestamp":"2026-09-26T10:0{t}:00Z","message":{{"role":"user","content":[{{"type":"text","text":"turn {t}"}}]}}}}"#,
                "\n",
                r#"{{"type":"assistant","timestamp":"2026-09-26T10:0{t}:01Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t{t}","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#,
                "\n",
                r#"{{"type":"user","timestamp":"2026-09-26T10:0{t}:02Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{t}","content":"a.rs"}}]}}}}"#,
                "\n",
                r#"{{"type":"assistant","timestamp":"2026-09-26T10:0{t}:03Z","message":{{"role":"assistant","content":[{{"type":"text","text":"answer {t}"}}]}}}}"#,
                "\n",
            ),
            t = t
        ));
    }
    std::fs::write(&path, body).unwrap();
    path
}

/// The plain dump writes nothing — not an entry, not the cache home itself — which is what lets a
/// sweep run beside live viewers. `--cache` writes the entry, and both write the same bytes, the
/// second time from the entry too.
#[test]
fn the_cached_stream_is_the_plain_one_and_only_the_flag_writes() {
    let dir = scratch("stream");
    let path = transcript(&dir);
    let p = path.to_str().unwrap();

    let (plain, err, ok) = run(&dir, &["--dump", "-", "--json", p]);
    assert!(ok, "{err}");
    assert!(!plain.is_empty());
    assert!(
        !dir.join("cache").exists(),
        "a plain dump wrote under the cache home"
    );

    let entry = dir.join("cache/sessions/json/0f0e0d0c-0000-4000-8000-000000000010");
    for run_no in 1..=2 {
        let (cached, err, ok) = run(&dir, &["--dump", "-", "--json", "--cache", p]);
        assert!(ok, "{err}");
        assert_eq!(
            String::from_utf8_lossy(&cached),
            String::from_utf8_lossy(&plain),
            "cached dump {run_no}"
        );
        assert!(entry.join("blocks.jsonl").is_file(), "the entry's blocks");
        assert!(
            entry.join("meta.jsonl").is_file(),
            "the entry's meta stream"
        );
        assert!(!entry.join("LOCK").exists(), "the lock is given back");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The flag is refused where it would do nothing, rather than ignored — and never combined with
/// the one that says the opposite.
#[test]
fn the_flag_goes_with_a_json_dump_only() {
    let dir = scratch("refused");
    let path = transcript(&dir);
    let p = path.to_str().unwrap();
    for args in [
        vec!["--dump", "-", "--cache", p],
        vec!["--paths", "--cache", p],
        vec!["--dump", "-", "--json", "--cache", "--no-cache", p],
    ] {
        let (out, err, ok) = run(&dir, &args);
        assert!(
            !ok,
            "{args:?} was accepted: {}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            err.contains("--cache") || err.contains("--no-cache"),
            "{args:?}: {err}"
        );
    }
    assert!(!dir.join("cache").exists(), "a refused run wrote nothing");
    let _ = std::fs::remove_dir_all(&dir);
}
