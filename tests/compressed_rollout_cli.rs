//! `agent-replay` reads a Codex rollout compressed to `rollout-….jsonl.zst` as the JSONL it
//! stands for (#s383), through the real binary: the block stream and the `--paths` facts of the
//! compressed file are the plain file's. Codex compresses a rollout once it has gone seven days
//! untouched; the old binary read the zstd bytes as a transcript holding nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-zst-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
        .args(args)
        .env("CLAUDE_REPLAY_CACHE", dir.join("cache"))
        .env("CODEX_HOME", dir.join("codex-home"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "agent-replay {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

const ROLLOUT: &str = concat!(
    r#"{"timestamp":"2026-10-01T09:00:00.000Z","type":"session_meta","payload":{"id":"0193a5c0-0000-7000-8000-0000000c1383","cwd":"/repo","originator":"codex_cli_rs","source":"cli","cli_version":"0.167.0"}}"#,
    "\n",
    r#"{"timestamp":"2026-10-01T09:00:01.000Z","type":"turn_context","payload":{"cwd":"/repo","model":"gpt-5.6"}}"#,
    "\n",
    r#"{"timestamp":"2026-10-01T09:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"List the crates"}]}}"#,
    "\n",
    r#"{"timestamp":"2026-10-01T09:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"call_1","arguments":"{\"command\":[\"ls\"],\"workdir\":\"/repo\"}"}}"#,
    "\n",
    r#"{"timestamp":"2026-10-01T09:00:04.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"{\"output\":\"engine\\nagents\\n\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":0.1}}"}}"#,
    "\n",
    r#"{"timestamp":"2026-10-01T09:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1200,"cached_input_tokens":200,"output_tokens":80}}}}"#,
    "\n",
    r#"{"timestamp":"2026-10-01T09:00:06.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Two crates: engine and agents."}]}}"#,
    "\n",
);

const NAME: &str = "rollout-2026-10-01T09-00-00-0193a5c0-0000-7000-8000-0000000c1383.jsonl";

#[test]
fn a_compressed_rollout_dumps_and_describes_as_its_jsonl() {
    let dir = scratch("dump");
    let plain = dir.join("plain").join(NAME);
    std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
    std::fs::write(&plain, ROLLOUT).unwrap();
    let copy = dir.join("zst").join(NAME);
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&plain, &copy).unwrap();
    let z = claude_replay_core::source::compress_in_place_for_tests(&copy).unwrap();
    let (p, zs) = (plain.to_str().unwrap(), z.to_str().unwrap());

    let want = run(&dir, &["--agent", "codex", p, "--dump", "-", "--json"]);
    assert!(want.contains("Two crates: engine and agents."), "{want}");
    let got = run(&dir, &["--agent", "codex", zs, "--dump", "-", "--json"]);
    assert_eq!(got, want, "the same block stream");

    // The agent is told by the file's head, as for a plain rollout.
    assert_eq!(run(&dir, &[zs, "--dump", "-", "--json"]), want);

    let facts = |path: &str| -> serde_json::Value {
        serde_json::from_str(&run(&dir, &["--paths", path])).unwrap()
    };
    let (fp, fz) = (facts(p), facts(zs));
    assert_eq!(fz["path"], zs);
    assert_eq!(fz["agent"], "codex");
    assert_eq!(fz["session_id"], "0193a5c0-0000-7000-8000-0000000c1383");
    for key in [
        "agent",
        "session_id",
        "first_cwd",
        "latest_cwd",
        "session_key",
    ] {
        assert_eq!(fz[key], fp[key], "{key}");
    }

    // The resumable dump (`--cache`) keeps an entry for it, under the session's one id, and
    // gives the same bytes (that it is SERVED from the entry is `dump_cache`'s own case).
    let entry = dir
        .join("cache/sessions/json")
        .join(NAME.trim_end_matches(".jsonl"));
    for run_no in 1..=2 {
        let cached = run(
            &dir,
            &["--agent", "codex", zs, "--dump", "-", "--json", "--cache"],
        );
        assert_eq!(cached, want, "cached dump {run_no}");
        assert!(
            entry.join("blocks.jsonl").is_file(),
            "the entry, run {run_no}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
