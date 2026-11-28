//! Real CLI export over a synthetic multi-model transcript.
use std::process::Command;

#[test]
fn dump_html_contains_turn_and_merged_session_answer_readings() {
    let dir = std::env::temp_dir().join(format!("cr-dump-answer-speed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("synthetic.jsonl");
    let values = [
        serde_json::json!({"type":"user","cwd":"/r","timestamp":"2026-10-30T10:00:00Z","message":{"role":"user","content":"hello"}}),
        serde_json::json!({"type":"assistant","timestamp":"2026-10-30T10:00:02Z","message":{"role":"assistant","id":"a","model":"claude-opus-4-8","usage":{"output_tokens":64},"content":[{"type":"text","text":"hello"}]}}),
        serde_json::json!({"type":"assistant","timestamp":"2026-10-30T10:00:04Z","message":{"role":"assistant","id":"b","model":"claude-opus-4-8[1m]","usage":{"output_tokens":40},"content":[{"type":"text","text":"again"}]}}),
    ];
    let text = values
        .iter()
        .map(|v| v.to_string() + "\n")
        .collect::<String>();
    std::fs::write(&path, text).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
        .arg(&path)
        .args(["--dump-html", "-"])
        .env("CLAUDE_REPLAY_CACHE", dir.join("cache"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = String::from_utf8(out.stdout).unwrap();
    let data = html
        .split("<script id=\"session-data\" type=\"application/jsonl\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let rows: Vec<serde_json::Value> = data
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let usage = &rows[0]["usage"];
    assert_eq!(usage["ans_speed"].as_array().unwrap().len(), 1);
    assert_eq!(
        usage["ans_speed"][0],
        serde_json::json!({"model":"opus4.8","tok":104,"ms":6000,"n":2})
    );
    assert_eq!(usage["ans_last"]["tok"], 40);
    let opener = rows.iter().find(|v| v["kind"] == "user").unwrap();
    assert_eq!(
        opener["ans"],
        serde_json::json!({"model":"opus4.8","tok":104,"ms":6000,"n":2})
    );
    std::fs::remove_dir_all(dir).unwrap();
}
