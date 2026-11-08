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

/// A finished Claude session in `store`, in the project at `cwd`: three turns, a reply that reads a
/// file in `cwd` (so a record offers a path with its stamps), a command, and the answers. Its
/// records fill both of a pull's zones, the turn index has turns to map and search has text and a
/// tool to find. Every line is dated ten minutes ago, so it is idle and stays quiet between pulls.
pub fn write_session(store: &Path, cwd: &Path, sid: &str) {
    let cwd_s = cwd.to_str().unwrap();
    std::fs::create_dir_all(cwd).unwrap();
    let tally = cwd.join("tally.md");
    std::fs::write(&tally, "# Harbour wall\n\nterns: 14\nskuas: 3\n").unwrap();
    let tally_s = tally.to_str().unwrap();
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 600;
    let mut n = 0u64;
    let mut line = |kind: &str, message: Value, extra: Value| {
        n += 1;
        let mut rec = json!({
            "type": kind,
            "sessionId": sid,
            "cwd": cwd_s,
            "uuid": format!("00000000-0000-4000-8000-{n:012}"),
            "timestamp": rfc3339(start + n * 5),
            "message": message,
        });
        if let (Value::Object(rec), Value::Object(extra)) = (&mut rec, extra) {
            rec.extend(extra);
        }
        format!("{rec}\n")
    };
    let user = |text: &str| json!({"role": "user", "content": text});
    let assistant = |content: Value| {
        json!({
            "role": "assistant",
            "model": "claude-opus-5-5",
            "content": content,
            "usage": {"input_tokens": 120, "output_tokens": 30},
        })
    };
    let result = |id: &str, text: &str| json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": text}]});
    let mut jsonl = String::new();
    jsonl += &line(
        "user",
        user("Count the terns on the harbour wall."),
        json!({}),
    );
    jsonl += &line(
        "assistant",
        assistant(json!([
            {"type": "text", "text": "I will read the tally first."},
            {"type": "tool_use", "id": "toolu_read1", "name": "Read", "input": {"file_path": tally_s}},
        ])),
        json!({}),
    );
    jsonl += &line(
        "user",
        result(
            "toolu_read1",
            "     1\t# Harbour wall\n     2\t\n     3\tterns: 14\n     4\tskuas: 3\n",
        ),
        json!({"toolUseResult": {"type": "text", "file": {
            "filePath": tally_s,
            "content": "# Harbour wall\n\nterns: 14\nskuas: 3\n",
            "numLines": 4, "startLine": 1, "totalLines": 4,
        }}}),
    );
    jsonl += &line(
        "assistant",
        assistant(json!([{"type": "text", "text": "There are **14 terns** on the wall."}])),
        json!({}),
    );
    jsonl += &line("user", user("And the skuas?"), json!({}));
    jsonl += &line(
        "assistant",
        assistant(
            json!([{"type": "tool_use", "id": "toolu_bash1", "name": "Bash", "input": {
                "command": "grep skuas tally.md", "description": "Find the skua count",
            }}]),
        ),
        json!({}),
    );
    jsonl += &line("user", result("toolu_bash1", "skuas: 3"), json!({}));
    jsonl += &line(
        "assistant",
        assistant(json!([{"type": "text", "text": "Three skuas, on the far end of the wall."}])),
        json!({}),
    );
    jsonl += &line("user", user("Thanks, that is all."), json!({}));
    jsonl += &line(
        "assistant",
        assistant(json!([{"type": "text", "text": "You are welcome."}])),
        json!({}),
    );
    let project = store.join(cwd_s.replace('/', "-"));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(format!("{sid}.jsonl")), jsonl).unwrap();
}
