//! `agent-replay --field-coverage` (#363): a KNOWN field the newest client version stopped writing
//! is flagged, through the real binary, over a hermetic store — `--unknown` cannot see this, since
//! nothing NEW appears when a field merely goes empty.
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-coverage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(root: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
        .args(args)
        .current_dir(root)
        .env("CLAUDE_PROJECTS_DIR", root.join("claude"))
        .env("CODEX_HOME", root.join("codex"))
        .env("QODER_PROJECTS_DIR", root.join("qoder"))
        .env("QODER_TASKS_ROOT", root.join("qoder-tasks"))
        .env("QODERWORK_PROJECTS_DIR", root.join("qoderwork"))
        .env("QODERWORK_DB", root.join("qoderwork.db"))
        .env("QWENWORK_PROJECTS_DIR", root.join("qwenwork"))
        .env("QWENWORK_DB", root.join("qwenwork.db"))
        .env("CLAUDE_REPLAY_CACHE", root.join("cache"))
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// A session of `n` replies written by `version`, each with or without its usage block.
fn session(root: &Path, sid: &str, version: &str, n: usize, usage: bool) {
    let project = root.join("claude").join("-work-coverage");
    std::fs::create_dir_all(&project).unwrap();
    let mut out = String::new();
    for i in 0..n {
        let usage = if usage {
            r#","usage":{"input_tokens":3,"output_tokens":5}"#
        } else {
            ""
        };
        out += &format!(
            r#"{{"type":"assistant","cwd":"/work","sessionId":"{sid}","version":"{version}","requestId":"r{i}","timestamp":"2026-09-25T00:00:{:02}Z","message":{{"id":"m{i}","model":"claude-sonnet-5","role":"assistant","content":[{{"type":"text","text":"ok"}}]{usage}}}}}"#,
            i % 60
        );
        out.push('\n');
    }
    std::fs::write(project.join(format!("{sid}.jsonl")), out).unwrap();
}

#[test]
fn a_field_the_newest_version_stopped_writing_is_flagged() {
    let root = scratch("drop");
    session(
        &root,
        "0f0e0d0c-0000-4000-8000-000000000363",
        "2.1.1",
        25,
        true,
    );
    session(
        &root,
        "0f0e0d0c-0000-4000-8000-000000000364",
        "2.1.2",
        25,
        false,
    );
    let (out, err, ok) = run(&root, &["--field-coverage", "--json"]);
    assert!(ok, "{err}");
    let rows: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    let usage = |version: &str| {
        rows.iter()
            .find(|r| r["field"] == "assistant.message.usage" && r["version"] == version)
            .unwrap_or_else(|| panic!("no usage row for {version}: {out}"))
            .clone()
    };
    let newest = usage("2.1.2");
    assert_eq!(newest["dropped"], true, "{newest}");
    assert_eq!(newest["rate"], 0.0);
    assert_eq!(
        newest["usual"], 1.0,
        "the version before it wrote usage on every reply"
    );
    assert_eq!(usage("2.1.1")["dropped"], false, "the past is not flagged");
    assert!(
        rows.iter().filter(|r| r["dropped"] == true).count() == 1,
        "the model, the ids and the timestamps did not move: {out}"
    );
    // A person's table says the same.
    let (table, _, _) = run(&root, &["--field-coverage"]);
    assert!(
        table
            .lines()
            .any(|l| l.contains("assistant.message.usage") && l.contains("DROPPED")),
        "{table}"
    );
}

#[test]
fn the_mode_takes_since_and_json_and_refuses_unknown_beside_it() {
    let root = scratch("flags");
    let (_, err, ok) = run(&root, &["--field-coverage", "--since", "1d", "--json"]);
    assert!(ok, "--since and --json go with it: {err}");
    let (_, _, ok) = run(&root, &["--field-coverage", "--unknown"]);
    assert!(!ok, "one sweep at a time");
}

fn rows(out: &str) -> Vec<serde_json::Value> {
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// #s370: a sub-agent's records are counted with its session's. A Claude sub-agent lives in its
/// session's `subagents/` dir, and the sweep read main sessions alone, so a field only sub-agents
/// stopped writing went unseen.
#[test]
fn a_sub_agent_s_records_are_counted() {
    let root = scratch("sub-agent");
    let sid = "0f0e0d0c-0000-4000-8000-000000000370";
    session(&root, sid, "2.1.3", 3, true);
    let subs = root
        .join("claude")
        .join("-work-coverage")
        .join(sid)
        .join("subagents");
    std::fs::create_dir_all(&subs).unwrap();
    let mut sub = String::new();
    for i in 0..4 {
        sub += &format!(
            r#"{{"type":"assistant","cwd":"/work","sessionId":"{sid}","isSidechain":true,"agentId":"a1","version":"2.1.3","requestId":"s{i}","timestamp":"2026-09-25T00:01:{i:02}Z","message":{{"id":"sm{i}","model":"claude-sonnet-5","role":"assistant","content":[{{"type":"text","text":"ok"}}],"usage":{{"input_tokens":3,"output_tokens":5}}}}}}"#
        );
        sub.push('\n');
    }
    std::fs::write(subs.join("agent-a1.jsonl"), sub).unwrap();
    let (out, err, ok) = run(&root, &["--field-coverage", "--json"]);
    assert!(ok, "{err}");
    assert!(err.contains("read 2 transcript"), "{err}");
    let usage = rows(&out)
        .into_iter()
        .find(|r| r["field"] == "assistant.message.usage" && r["version"] == "2.1.3")
        .unwrap_or_else(|| panic!("no usage row: {out}"));
    assert_eq!(
        usage["records"], 7,
        "3 replies in the session, 4 in its sub-agent: {usage}"
    );
}

/// #s370: Codex keeps a sub-agent in a rollout of its own, which its head marks; it is counted too.
#[test]
fn a_codex_sub_agent_rollout_is_counted() {
    let root = scratch("codex-sub-agent");
    let day = root.join("codex").join("sessions").join("2026/09/25");
    std::fs::create_dir_all(&day).unwrap();
    let head = |id: &str, lineage: &str| {
        format!(
            r#"{{"timestamp":"2026-09-25T00:00:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/work/skua",{lineage}"originator":"codex_cli_rs","cli_version":"0.170.0"}}}}"#
        ) + "\n"
    };
    std::fs::write(day.join("rollout-m370.jsonl"), head("m370", "")).unwrap();
    std::fs::write(
        day.join("rollout-s370.jsonl"),
        head(
            "s370",
            r#""session_id":"m370","thread_source":"subagent","parent_thread_id":"m370","#,
        ),
    )
    .unwrap();
    let (out, err, ok) = run(&root, &["--field-coverage", "--json", "--agent", "codex"]);
    assert!(ok, "{err}");
    assert!(
        err.contains("read 2 transcript"),
        "the main rollout and its sub-agent's: {err}"
    );
    let cwd = rows(&out)
        .into_iter()
        .find(|r| r["agent"] == "codex" && r["field"] == "session_meta.cwd")
        .unwrap_or_else(|| panic!("no cwd row: {out}"));
    assert_eq!(cwd["records"], 2, "{cwd}");
}

/// #s383: a Codex rollout compressed to `.jsonl.zst` (Codex does so once one has gone seven days
/// untouched, so a 14-day window holds them) is counted as its JSONL. Read as raw bytes, it was
/// counted as a transcript read and gave no record at all.
#[test]
fn a_compressed_codex_rollout_is_counted() {
    let root = scratch("codex-zst");
    let day = root.join("codex").join("sessions").join("2026/09/25");
    std::fs::create_dir_all(&day).unwrap();
    let plain = day.join("rollout-z383.jsonl");
    std::fs::write(
        &plain,
        concat!(
            r#"{"timestamp":"2026-09-25T00:00:00Z","type":"session_meta","payload":{"id":"z383","cwd":"/work/skua","originator":"codex_cli_rs","cli_version":"0.170.0"}}"#,
            "\n",
            r#"{"timestamp":"2026-09-25T00:00:01Z","type":"turn_context","payload":{"cwd":"/work/skua","model":"gpt-5.6"}}"#,
            "\n",
        ),
    )
    .unwrap();
    claude_replay_core::source::compress_in_place_for_tests(&plain).unwrap();
    let (out, err, ok) = run(&root, &["--field-coverage", "--json", "--agent", "codex"]);
    assert!(ok, "{err}");
    assert!(err.contains("read 1 transcript"), "{err}");
    let rows = rows(&out);
    for field in ["session_meta.cwd", "turn_context.model"] {
        let row = rows
            .iter()
            .find(|r| r["agent"] == "codex" && r["field"] == field)
            .unwrap_or_else(|| panic!("no {field} row: {out}"));
        assert_eq!(row["records"], 1, "{row}");
        assert_eq!(row["rate"], 1.0, "{row}");
    }
}
