//! What the tests of the kit's conformance check share (#s434, `claude-monitor/kit/conform.mjs`):
//! a session with something for each of its rules to read, and the check itself, run with node
//! and read back as its `--json` report. Included by path by each test binary that runs it.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The check, where the kit will pick it up.
pub fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("kit/conform.mjs")
}

/// What one run of the check said.
pub struct Outcome {
    pub code: Option<i32>,
    pub report: Value,
    pub stderr: String,
}

impl Outcome {
    /// Each rule's status (`pass`, `fail` or `skip`), by its id.
    pub fn statuses(&self) -> BTreeMap<String, String> {
        self.results()
            .iter()
            .map(|r| {
                (
                    r["id"].as_str().unwrap_or("").to_string(),
                    r["status"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect()
    }

    pub fn results(&self) -> Vec<Value> {
        self.report["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The rule with `id`.
    pub fn result(&self, id: &str) -> Value {
        self.results()
            .into_iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("the check ran no rule {id}:\n{}", self.listing()))
    }

    /// Every rule as a line, for a failed assertion's message.
    pub fn listing(&self) -> String {
        let mut out = String::new();
        for r in self.results() {
            out.push_str(&format!(
                "{:4} {} [{}] {}\n",
                r["status"].as_str().unwrap_or(""),
                r["id"].as_str().unwrap_or(""),
                r["section"].as_str().unwrap_or(""),
                r["detail"].as_str().unwrap_or("")
            ));
        }
        if !self.stderr.is_empty() {
            out.push_str(&format!("stderr: {}\n", self.stderr));
        }
        out
    }

    /// The ids of the rules that failed.
    pub fn failed(&self) -> Vec<String> {
        self.statuses()
            .into_iter()
            .filter(|(_, s)| s == "fail")
            .map(|(id, _)| id)
            .collect()
    }
}

/// Run the check against `base` with `args`, or `None` where there is no node to run it with:
/// node is not a build dependency of this workspace, so a machine without it skips, as
/// `ui_contract.rs` does, and CI, where node is always present, runs it.
pub fn run(base: &str, args: &[&str]) -> Option<Outcome> {
    let out = match std::process::Command::new("node")
        .arg(script())
        .arg(base)
        .args(args)
        .arg("--json")
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skip: no `node` on PATH to run the conformance check");
            return None;
        }
        Err(e) => panic!("could not run node: {e}"),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let report: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("the check printed no JSON report ({e}): {stdout}\nstderr: {stderr}")
    });
    Some(Outcome {
        code: out.status.code(),
        report,
        stderr,
    })
}

/// The node binary this test process runs, resolved with the REAL environment
/// (`node -p process.execPath`, so a version manager's shim answers with the binary it runs), for a
/// monitor whose HOME is a scratch directory: there a shim starts from an empty cache, and the
/// monitor's first `node --version` for mdrev hung installing node (the browser harness's
/// `real_node`, #s203). `None` leaves the monitor to its own search.
pub fn real_node() -> Option<PathBuf> {
    let out = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    path.is_file().then_some(path)
}

/// Every section a rule names is a heading of the contract: a `##` heading, then optionally ` › `
/// and the start of a `###` heading under it (backticks dropped). So a rule's section is one a
/// host can find in `docs/backend-contract.md`.
pub fn assert_sections_are_headings(outcome: &Outcome, doc: &str) {
    let mut heads: Vec<(String, Vec<String>)> = Vec::new();
    for line in doc.lines() {
        let plain = line.replace('`', "");
        if let Some(h) = plain.strip_prefix("## ") {
            heads.push((h.trim().to_string(), Vec::new()));
        } else if let Some(h) = plain.strip_prefix("### ") {
            if let Some((_, subs)) = heads.last_mut() {
                subs.push(h.trim().to_string());
            }
        }
    }
    for r in outcome.results() {
        let section = r["section"].as_str().unwrap_or("");
        let (top, sub) = match section.split_once(" › ") {
            Some((top, sub)) => (top, Some(sub)),
            None => (section, None),
        };
        let found = heads.iter().any(|(h, subs)| {
            h == top && sub.is_none_or(|sub| subs.iter().any(|s| s.starts_with(sub)))
        });
        assert!(
            found,
            "rule {} names {section:?}, which is no heading of the contract",
            r["id"]
        );
    }
}

/// `secs` since the epoch as RFC 3339.
pub fn rfc3339(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The sub-agent [`write_session`] starts.
pub const SUB_AGENT: &str = "a7e2c41b9d05f3e8";

/// A finished Claude session in `store`, in the project at `cwd`, with something for each rule of
/// the check to read, in the shapes Claude Code writes them (the browser harness's builders): a
/// file read in `cwd` (so a record offers a path with its stamps), a command, a slash command with
/// its output, a file attached to a prompt, thinking alone and before a call, an edit and a write,
/// a question asked through the client and answered, a sub-agent with its own transcript, a file
/// handed to the reader, a compaction and the answers. Its records fill both of a pull's zones, the
/// turn index has turns, a compaction and an agent to map, and search has text and tools to find.
/// Every line is dated ten minutes ago, so it is idle and stays quiet between pulls.
pub fn write_session(store: &Path, cwd: &Path, sid: &str) {
    let cwd_s = cwd.to_str().unwrap();
    std::fs::create_dir_all(cwd).unwrap();
    let tally = cwd.join("tally.md");
    std::fs::write(&tally, "# Harbour wall\n\nterns: 14\nskuas: 3\n").unwrap();
    let tally_s = tally.to_str().unwrap();
    let notes = cwd.join("notes.md");
    let notes_s = notes.to_str().unwrap();
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 600;
    let user = |text: &str| json!({"type": "user", "message": {"role": "user", "content": text}});
    let assistant = |content: Value| {
        json!({"type": "assistant", "message": {
            "role": "assistant",
            "model": "claude-opus-5-5",
            "content": content,
            "usage": {"input_tokens": 120, "output_tokens": 30},
        }})
    };
    let call = |id: &str, name: &str, input: Value| {
        assistant(json!([{"type": "tool_use", "id": id, "name": name, "input": input}]))
    };
    let result = |id: &str, text: &str| {
        json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": text},
        ]}})
    };
    // Each record is stamped with the session, its directory, an id and its time, in order.
    let mut records: Vec<Value> = Vec::new();

    // Turn 1: a file read, and the answer.
    records.push(user("Count the terns on the harbour wall."));
    records.push(assistant(json!([
        {"type": "text", "text": "I will read the tally first."},
        {"type": "tool_use", "id": "toolu_read1", "name": "Read", "input": {"file_path": tally_s}},
    ])));
    let mut read = result(
        "toolu_read1",
        "     1\t# Harbour wall\n     2\t\n     3\tterns: 14\n     4\tskuas: 3\n",
    );
    read["toolUseResult"] = json!({"type": "text", "file": {
        "filePath": tally_s,
        "content": "# Harbour wall\n\nterns: 14\nskuas: 3\n",
        "numLines": 4, "startLine": 1, "totalLines": 4,
    }});
    records.push(read);
    records.push(assistant(
        json!([{"type": "text", "text": "There are **14 terns** on the wall."}]),
    ));

    // Turn 2: a command.
    records.push(user("And the skuas?"));
    records.push(call(
        "toolu_bash1",
        "Bash",
        json!({"command": "grep skuas tally.md", "description": "Find the skua count"}),
    ));
    records.push(result("toolu_bash1", "skuas: 3"));
    records.push(assistant(json!([
        {"type": "text", "text": "Three skuas, on the far end of the wall."},
    ])));

    // Turn 3: a slash command, and its output.
    records.push(user(
        "<command-message>tally</command-message>\n<command-name>/tally</command-name>\n<command-args>terns</command-args>",
    ));
    records.push(user(
        "<local-command-stdout>terns: 14</local-command-stdout>",
    ));

    // Turn 4: a prompt with a file attached, thinking, an edit and a write, a question asked
    // through the client, a sub-agent, and a file handed to the reader.
    records.push(user(
        "Correct the tally to 15 terns, note it, and ask me before you share it.",
    ));
    records.push(json!({"type": "attachment", "attachment": {
        "type": "edited_text_file", "filename": tally_s, "snippet": "3\tterns: 14",
    }}));
    records.push(assistant(json!([
        {"type": "thinking", "thinking": "The tally says 14; the reader counted 15.", "signature": "c2ln"},
    ])));
    records.push(assistant(json!([
        {"type": "thinking", "thinking": "Edit the one line.", "signature": "c2ln"},
        {"type": "tool_use", "id": "toolu_edit1", "name": "Edit", "input": {
            "file_path": tally_s, "old_string": "terns: 14", "new_string": "terns: 15",
        }},
    ])));
    records.push(result(
        "toolu_edit1",
        &format!("The file {tally_s} has been updated successfully."),
    ));
    records.push(call(
        "toolu_write1",
        "Write",
        json!({"file_path": notes_s, "content": "Recounted the terns: 15.\n"}),
    ));
    records.push(result(
        "toolu_write1",
        &format!("File created successfully at: {notes_s}"),
    ));
    records.push(call(
        "toolu_ask1",
        "request_user_input",
        json!({"question": "Share the tally?"}),
    ));
    records.push(result(
        "toolu_ask1",
        "{\"answers\":{\"share\":{\"answers\":[\"Yes\"]}}}",
    ));
    records.push(call(
        "toolu_agent1",
        "Agent",
        json!({
            "subagent_type": "Explore",
            "description": "check the far end",
            "prompt": "Count the skuas at the far end of the wall.",
        }),
    ));
    let mut spawned = result("toolu_agent1", "Three skuas at the far end.");
    spawned["toolUseResult"] = json!({
        "status": "completed", "prompt": "Count the skuas at the far end of the wall.",
        "agentId": SUB_AGENT, "agentType": "Explore",
        "content": [{"type": "text", "text": "Three skuas at the far end."}],
        "totalDurationMs": 1200, "totalTokens": 900, "totalToolUseCount": 1,
    });
    records.push(spawned);
    records.push(call(
        "toolu_send1",
        "SendUserFile",
        json!({"files": [tally_s], "caption": "the tally", "status": "normal"}),
    ));
    records.push(result(
        "toolu_send1",
        &format!(
            "1 files delivered to user.\n  {tally_s} → file_uuid: 00000000-0000-4000-8000-000000000000"
        ),
    ));
    records.push(assistant(
        json!([{"type": "text", "text": "Corrected to 15, noted, and shared."}]),
    ));

    // The context compacted, then turn 5, the last.
    records.push(json!({
        "type": "system", "subtype": "compact_boundary", "content": "Conversation compacted",
        "compactMetadata": {"trigger": "auto", "preTokens": 59_471, "postTokens": 8_617},
    }));
    records.push(user("Thanks, that is all."));
    records.push(assistant(json!([
        {"type": "thinking", "thinking": "Nothing is left to count.", "signature": "c2ln"},
    ])));
    records.push(assistant(
        json!([{"type": "text", "text": "You are welcome."}]),
    ));

    let mut jsonl = String::new();
    for (n, mut rec) in records.into_iter().enumerate() {
        let at = rec.as_object_mut().unwrap();
        at.insert("sessionId".into(), json!(sid));
        at.insert("cwd".into(), json!(cwd_s));
        at.insert(
            "uuid".into(),
            json!(format!("00000000-0000-4000-8000-{n:012}")),
        );
        at.insert("timestamp".into(), json!(rfc3339(start + n as u64 * 3)));
        jsonl.push_str(&format!("{rec}\n"));
    }
    let project = store.join(cwd_s.replace('/', "-"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(format!("{sid}.jsonl")), jsonl).unwrap();

    // The sub-agent's own transcript, beside its parent's (its lineage is the path alone).
    let child = project.join(sid).join("subagents");
    std::fs::create_dir_all(&child).unwrap();
    let mut sub = String::new();
    for (m, mut rec) in [
        user("Count the skuas at the far end of the wall."),
        assistant(json!([{"type": "text", "text": "Three skuas at the far end."}])),
    ]
    .into_iter()
    .enumerate()
    {
        let at = rec.as_object_mut().unwrap();
        at.insert("sessionId".into(), json!(sid));
        at.insert("agentId".into(), json!(SUB_AGENT));
        at.insert("isSidechain".into(), json!(true));
        at.insert("cwd".into(), json!(cwd_s));
        at.insert("timestamp".into(), json!(rfc3339(start + 60 + m as u64)));
        sub.push_str(&format!("{rec}\n"));
    }
    std::fs::write(child.join(format!("agent-{SUB_AGENT}.jsonl")), sub).unwrap();
}
