//! `agent-replay --unknown` sweeps EVERY agent's store, whatever directory it runs from (#276).
//!
//! It used the viewer's discovery, which is scoped to the working directory: run from `/tmp` it
//! swept nothing at all, while its own doc promised every store. A daily job started by launchd —
//! whose working directory is `/` — would have reported silence forever. These cases run the real
//! binary from a directory unrelated to the store, with every store and the cache home pointed
//! into the case's scratch, so nothing on this machine is read or reclaimed.

use std::path::{Path, PathBuf};
use std::process::Command;

const SID: &str = "0f0e0d0c-0000-4000-8000-000000000276";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-unknown-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `agent-replay <args>` from `cwd`, hermetic: every agent's store and the cache home under `root`.
fn run(root: &Path, cwd: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
        .args(args)
        .current_dir(cwd)
        .env("CLAUDE_PROJECTS_DIR", root.join("claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("QODER_PROJECTS_DIR", root.join("qoder"))
        .env("QODER_TASKS_ROOT", root.join("qoder-tasks"))
        .env("QODERWORK_PROJECTS_DIR", root.join("qoderwork"))
        .env("QODERWORK_DB", root.join("qoderwork.db"))
        .env("CLAUDE_REPLAY_CACHE", root.join("cache"))
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// A Claude store with one session, in a project nowhere near the working directory, carrying a
/// top-level record type no adapter knows.
fn store_with_a_new_shape(root: &Path) -> PathBuf {
    let project = root.join("claude").join("-work-elsewhere");
    std::fs::create_dir_all(&project).unwrap();
    let path = project.join(format!("{SID}.jsonl"));
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            r#"{"type":"user","cwd":"/work/elsewhere","sessionId":"0f0e0d0c-0000-4000-8000-000000000276","version":"9.9.9","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-09-25T00:00:00Z"}"#,
            r#"{"type":"a_record_nobody_has_seen","sessionId":"0f0e0d0c-0000-4000-8000-000000000276","version":"9.9.9","timestamp":"2026-09-25T00:00:01Z"}"#,
        ),
    )
    .unwrap();
    path
}

fn rows(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

#[test]
fn the_sweep_is_machine_wide_whatever_the_working_directory() {
    let root = scratch("wide");
    store_with_a_new_shape(&root);
    let elsewhere = scratch("elsewhere");
    let (out, err, ok) = run(&root, &elsewhere, &["--unknown", "--json"]);
    assert!(ok, "{err}");
    assert!(err.contains("scanning 1 transcript"), "{err}");
    let rows = rows(&out);
    assert_eq!(rows.len(), 1, "{out}");
    assert_eq!(rows[0]["agent"], "claude", "which adapter dropped it");
    assert_eq!(rows[0]["where"], "record.type");
    assert_eq!(rows[0]["name"], "a_record_nobody_has_seen");
    assert_eq!(rows[0]["count"], 1);
    assert_eq!(rows[0]["version"], "9.9.9", "the client that wrote it");
    assert_eq!(rows[0]["example"], SID, "the session to go and look in");
    // The table says the same, for a person.
    let (table, _, _) = run(&root, &elsewhere, &["--unknown"]);
    assert!(table.contains("a_record_nobody_has_seen"), "{table}");
}

#[test]
fn since_trims_by_mtime_and_json_says_nothing_when_nothing_is_new() {
    let root = scratch("since");
    let path = store_with_a_new_shape(&root);
    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86400);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();
    let (out, err, ok) = run(&root, &root, &["--unknown", "--since", "1d", "--json"]);
    assert!(ok, "{err}");
    assert!(
        err.contains("scanning 0 transcript"),
        "outside the window: {err}"
    );
    assert_eq!(out, "", "nothing new: nothing on stdout");
    let (out, _, ok) = run(&root, &root, &["--unknown", "--since", "3d", "--json"]);
    assert!(ok && out.contains("a_record_nobody_has_seen"), "{out}");
}

#[test]
fn json_and_since_are_refused_where_they_mean_nothing() {
    let root = scratch("refused");
    for (args, says) in [
        (&["--json"][..], "--json goes with"),
        (&["--since", "1d"][..], "--since goes with"),
    ] {
        let (_, err, ok) = run(&root, &root, args);
        assert!(!ok, "{args:?} ran");
        assert!(err.contains(says), "{args:?}: {err}");
    }
}

/// A model that produced tokens with no price is reported — its cost is dropped, and the session
/// reads as a lower bound — while a priced model in the same session is not.
#[test]
fn a_model_with_no_price_is_reported_and_a_priced_one_is_not() {
    let root = scratch("unpriced");
    let project = root.join("claude").join("-work-models");
    std::fs::create_dir_all(&project).unwrap();
    let assistant = |model: &str, ts: &str| {
        format!(
            r#"{{"type":"assistant","sessionId":"{SID}","version":"9.9.9","message":{{"role":"assistant","model":"{model}","content":[{{"type":"text","text":"ok"}}],"usage":{{"input_tokens":10,"output_tokens":5}}}},"timestamp":"{ts}"}}"#
        )
    };
    std::fs::write(
        project.join(format!("{SID}.jsonl")),
        format!(
            "{}\n{}\n{}\n",
            r#"{"type":"user","cwd":"/work/models","sessionId":"0f0e0d0c-0000-4000-8000-000000000276","version":"9.9.9","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-09-25T00:00:00Z"}"#,
            assistant("claude-imaginary-9", "2026-09-25T00:00:01Z"),
            assistant("claude-opus-4-8", "2026-09-25T00:00:02Z"),
        ),
    )
    .unwrap();
    let (out, err, ok) = run(&root, &root, &["--unknown", "--json"]);
    assert!(ok, "{err}");
    let rows = rows(&out);
    let unpriced: Vec<_> = rows
        .iter()
        .filter(|r| r["where"] == "model.unpriced")
        .collect();
    assert_eq!(unpriced.len(), 1, "{out}");
    assert_eq!(unpriced[0]["name"], "claude-imaginary-9");
    assert_eq!(unpriced[0]["agent"], "claude");
    assert_eq!(unpriced[0]["count"], 1, "sessions");
    assert_eq!(unpriced[0]["example"], SID);
}
