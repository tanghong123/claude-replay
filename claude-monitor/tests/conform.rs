//! The kit's conformance check (#s434, `kit/conform.mjs`) against a real `agent-monitor`: the
//! binary, paired, over stores of its own, read with its token as a host's reader would pass its
//! sign-in. The monitor is the contract's reference server ("where this document and that server
//! disagree, the server is right"), so every rule the check runs passes or skips here, none fails,
//! and the write routes, which a host's mounted table leaves out, are checked too.
//!
//! The mounted test host (`mount_host.rs`) runs the same check over a table that serves reading
//! only, and a host that breaks rules on purpose, in its own binary.

#[path = "conform_kit/mod.rs"]
mod conform_kit;
#[path = "contract_index/mod.rs"]
mod contract_index;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cm-conform-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the serving monitor however the test ends, so a failed assertion strands nothing.
struct Reap(std::process::Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The monitor, paired, with every store, state and cache root inside `base`.
fn monitor(base: &Path, port: u16) -> Command {
    let stores = base.join("stores");
    let log = std::fs::File::create(base.join("monitor.log")).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-monitor"));
    cmd.args(["--pair", "--no-open", "--port", &port.to_string()])
        // The token alone admits, on Linux as on macOS (the browser harness's reason, #366).
        .env("AGENT_MONITOR_TOKEN_ONLY", "1")
        .env_remove("AGENT_MONITOR_TRUSTED_HOSTS")
        .env_remove("CODEX_NATIVE_SERVER_URL")
        .env("HOME", base.join("home"))
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_STATE_HOME", base.join("xdg-state"))
        .env("XDG_CACHE_HOME", base.join("xdg-cache"))
        .env("AGENT_MONITOR_STATE", base.join("state"))
        .env("CLAUDE_MONITOR_STATE", base.join("state"))
        .env("AGENT_MONITOR_CACHE", base.join("cache"))
        .env("CLAUDE_MONITOR_CACHE", base.join("cache"))
        .env("MDREV_STATE_DIR", base.join("mdrev-state"))
        .env("ARIES_STATE_DIR", base.join("aries-state"))
        .env("CLAUDE_PROJECTS_DIR", stores.join("claude"))
        .env("CLAUDE_SCRATCH_ROOT", stores.join("claude-scratch"))
        .env("CLAUDE_DESKTOP_SESSIONS_DIR", stores.join("claude-desktop"))
        .env("CODEX_HOME", stores.join("codex"))
        .env("CODEX_SESSIONS_DIR", stores.join("codex/sessions"))
        .env("QODER_PROJECTS_DIR", stores.join("qoder"))
        .env("QODERWORK_PROJECTS_DIR", stores.join("qoderwork"))
        .env("QODERWORK_DB", stores.join("qoderwork.db"))
        .env("QWENWORK_PROJECTS_DIR", stores.join("qwenwork"))
        .env("QWENWORK_DB", stores.join("qwenwork.db"))
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    if let Some(node) = conform_kit::real_node() {
        cmd.env("MDREV_NODE", node);
    }
    cmd
}

/// Every file under `dir`, with its bytes: what the check may not have changed.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push((path, bytes));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_conformance_check_passes_against_a_paired_agent_monitor() {
    let base = scratch("monitor");
    let sid = "5a5a5a5a-0000-4000-8000-00000000c0f1";
    conform_kit::write_session(&base.join("stores/claude"), &base.join("tern-harbour"), sid);
    let port = free_port();
    let _serving = Reap(monitor(&base, port).spawn().unwrap());
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the monitor never listened on {port}: {}",
            std::fs::read_to_string(base.join("monitor.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let token = std::fs::read_to_string(base.join("state/auth-token")).unwrap();
    let bearer = format!("Authorization: Bearer {}", token.trim());
    let state_before = snapshot(&base.join("state"));

    let url = format!("http://127.0.0.1:{port}/");
    let Some(out) = conform_kit::run(&url, &["--header", &bearer, "--session", sid]) else {
        return;
    };
    assert_eq!(
        out.failed(),
        Vec::<String>::new(),
        "the monitor breaks no rule:\n{}",
        out.listing()
    );
    assert_eq!(out.code, Some(0), "{}", out.listing());
    assert_eq!(out.report["conforms"], true);

    // The rules that need a whole session and a paired reader ran, and were not skipped: the
    // records' every route, the tail, the files and the writes.
    let statuses = out.statuses();
    for id in [
        "request.signed-in",
        "sessions.reply",
        "sessions.row",
        "sessions.before-list",
        "pull.reply",
        "pull.fresh",
        "pull.own-cursor",
        "pull.tail",
        "records.reply",
        "records.end-carried",
        "records.not-a-start",
        "records.stale",
        "turn-index.reply",
        "turn-index.facts",
        "turn-index.turns",
        "turn-index.upto",
        "search.reply",
        "search.no-text",
        "search.tool-filter",
        "record.fields",
        "record.turns",
        "record.kinds",
        "record.parts",
        "record.shapes",
        "record.chips",
        "pull.sub-agent",
        "meta.fields",
        "file.offered",
        "file.not-offered",
        "reveal.forged",
        "mdrev.bundle",
        "mdrev.hold",
        "mdrev.origin",
        "mdrev.resolve",
        "mdrev.open",
        "tabs.review-prefix",
        "mermaid.runtime",
        "ignore.add-remove",
        "layout.sources",
        "writes.method",
        "writes.origin",
        "writes.unpaired",
        "writes.refusal",
        "request.body-limit",
        "request.hold-limit",
        "tabs.static",
        "index.core",
    ] {
        assert_eq!(
            statuses.get(id).map(String::as_str),
            Some("pass"),
            "{id} passes against the monitor:\n{}",
            out.listing()
        );
    }
    // The kinds' rules were held against every kind the fixture writes, not only a prompt and a
    // reply: what a host's own records are checked against has passed on the reference server's.
    let kinds = out.result("record.kinds")["detail"]
        .as_str()
        .unwrap()
        .to_string();
    for kind in [
        "act",
        "agent",
        "assistant",
        "attachment",
        "bash",
        "command",
        "compaction",
        "edit",
        "read",
        "think",
        "tool",
        "user",
        "write",
    ] {
        assert!(
            kinds.split([' ', ',']).any(|k| k == kind),
            "the session holds a {kind} record: {kinds}"
        );
    }
    // A bearer token is not a cookie a browser withholds: the check cannot see the monitor's
    // SameSite=Strict cookie refuse another site's request, and says so rather than failing it.
    assert_eq!(out.result("request.cross-site")["status"], "skip");

    // It says where each rule comes from, and the route index it walked is the contract's.
    let doc = contract_index::read();
    conform_kit::assert_sections_are_headings(&out, &doc);
    let walked: Vec<(String, String, String)> = out.report["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap().to_string(),
                r["path"].as_str().unwrap().to_string(),
                r["tier"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let index: Vec<(String, String, String)> = contract_index::rows(&doc)
        .into_iter()
        .map(|r| (r.method, r.path, r.tier))
        .collect();
    assert_eq!(
        walked, index,
        "the check walks the contract's route index, row for row"
    );
    // The monitor serves every row (but native mode, which a build without it does not offer).
    for r in out.report["routes"].as_array().unwrap() {
        if r["tier"] != "native" && r["path"] != "/__reveal" {
            assert_eq!(r["served"], true, "{r}");
        }
    }

    // Nothing it did stayed done: the hide list is as it was (a key added and removed), and no
    // pane was granted. The token, which it was given, is in no line it printed.
    let state_after = snapshot(&base.join("state"));
    let changed: Vec<&PathBuf> = state_after
        .iter()
        .filter(|(p, bytes)| {
            let before = state_before.iter().find(|(q, _)| q == p).map(|(_, b)| b);
            before != Some(bytes) && !p.ends_with("ignored.json")
        })
        .map(|(p, _)| p)
        .collect();
    assert!(
        !changed
            .iter()
            .any(|p| p.to_string_lossy().contains("consent")),
        "no pane was granted: {changed:?}"
    );
    let ignored = std::fs::read_to_string(base.join("state/ignored.json")).unwrap_or_default();
    assert!(
        !ignored.contains("conform-check"),
        "the key it hid is gone: {ignored}"
    );
    assert!(
        !out.report.to_string().contains(token.trim()),
        "the token is never printed"
    );

    let _ = std::fs::remove_dir_all(&base);
}
