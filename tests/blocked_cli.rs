//! #s600 end to end: a call a hook or auto mode stopped before it ran, in the records Claude Code
//! writes (the result's text, and for an edit the `toolUseResult` string with the client's
//! "Error: " before it), reads `blocked` with the client's reason as it wrote it, in the TUI's
//! dump, the `--json` stream and the page's data alike. The unit tests build their blocks by hand;
//! this one goes through the adapter, as the review of cb80ee3a asked.
use std::process::Command;

const SID: &str = "0f0e0d0c-0000-4000-8000-000000000600";
const EDIT: &str = "PreToolUse:Edit hook error: [guard] blocked: generated file";
const BASH: &str = "PreToolUse:Bash hook error: [guard] deploys need a person";
const AUTO: &str = "Auto mode blocked this command: it deletes outside the project.";

fn record(v: serde_json::Value) -> String {
    format!("{v}\n")
}

fn transcript() -> String {
    let at = |s: u32| format!("2026-11-19T10:00:{s:02}Z");
    let head = serde_json::json!({"cwd": "/w", "sessionId": SID, "version": "2.1.328"});
    let with = |mut v: serde_json::Value, s: u32| {
        for (k, x) in head.as_object().unwrap() {
            v[k] = x.clone();
        }
        v["timestamp"] = serde_json::json!(at(s));
        record(v)
    };
    let call = |id: &str, name: &str, input: serde_json::Value, s: u32| {
        with(
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": name, "input": input}]}}),
            s,
        )
    };
    // As the client writes it: the result's text, the `toolUseResult` string with its "Error: ",
    // and the record's own refusal fields.
    let stopped = |id: &str, text: &str, kind: &str, hook: bool, s: u32| {
        let mut v = serde_json::json!({"type": "user", "toolUseResult": format!("Error: {text}"),
            "toolDenialKind": kind,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "is_error": true, "content": text}]}});
        if hook {
            v["permissionDecision"] =
                serde_json::json!({"decision": "reject", "source": "hook", "reasonType": "hook"});
        }
        with(v, s)
    };
    [
        with(
            serde_json::json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "text", "text": "Regenerate the parser, deploy, then clean up."}]}}),
            0,
        ),
        call(
            "e1",
            "Edit",
            serde_json::json!({"file_path": "/w/gen.rs", "old_string": "alpha_old", "new_string": "beta_new"}),
            1,
        ),
        stopped("e1", EDIT, "permission-rule", true, 2),
        call(
            "b1",
            "Bash",
            serde_json::json!({"command": "make deploy"}),
            3,
        ),
        stopped("b1", BASH, "permission-rule", true, 4),
        call(
            "m1",
            "Bash",
            serde_json::json!({"command": "rm -rf ../shared"}),
            5,
        ),
        stopped("m1", AUTO, "automode-blocked", false, 6),
        with(
            serde_json::json!({"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "All three were stopped before they ran."}]}}),
            7,
        ),
    ]
    .concat()
}

#[test]
fn a_call_a_hook_or_auto_mode_stopped_reads_blocked_with_its_reason_everywhere() {
    let dir = std::env::temp_dir().join(format!("cr-blocked-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{SID}.jsonl"));
    std::fs::write(&path, transcript()).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
            .arg(&path)
            .args(args)
            .env("CLAUDE_REPLAY_CACHE", dir.join("cache"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // The --json stream: each call's status is the word, its output the reason as written.
    let json = run(&["--dump", "-", "--json"]);
    let calls: Vec<serde_json::Value> = json
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        // A run of calls is one `act` block, its calls under `tools`.
        .flat_map(|v| match v.get("tools").and_then(|t| t.as_array()) {
            Some(tools) => tools.clone(),
            None => vec![v],
        })
        .filter(|v| v.get("status").is_some())
        .collect();
    let by_target = |needle: &str| {
        calls
            .iter()
            .find(|v| v["target"].as_str().is_some_and(|t| t.contains(needle)))
            .unwrap_or_else(|| panic!("no call on {needle} in {json}"))
    };
    for (needle, reason) in [("gen.rs", EDIT), ("make deploy", BASH), ("../shared", AUTO)] {
        let v = by_target(needle);
        assert_eq!(v["status"], "blocked", "{needle}: {v}");
        assert_eq!(
            v["output"], reason,
            "{needle}: the reason as the client wrote it, no \"Error: \": {v}"
        );
    }

    // The TUI's dump: the word on the head, the reason under it, no diff of an edit never made.
    let text = run(&["--dump", "-", "--full", "--width", "120"]);
    assert!(
        text.matches("blocked").count() >= 3,
        "every head says blocked:\n{text}"
    );
    for reason in [EDIT, BASH, AUTO] {
        assert!(
            text.contains(reason),
            "the reason is drawn: {reason}\n{text}"
        );
    }
    assert!(
        !text.contains("Error: PreToolUse"),
        "no error of the call's:\n{text}"
    );
    assert!(
        !text.contains("beta_new"),
        "no diff of an edit that never happened:\n{text}"
    );

    // The page's data: the chip and the reason, the same words.
    let html = run(&["--dump-html", "-"]);
    assert!(
        html.matches(r#"{"c":"fail","x":"blocked"}"#).count() >= 3,
        "each head's chip reads blocked, as a failure"
    );
    assert!(
        !html.contains("Error: PreToolUse"),
        "the page's data carries no error of the call's"
    );
    assert!(
        html.contains(EDIT) && html.contains(BASH) && html.contains(AUTO),
        "the page's data carries each reason"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
