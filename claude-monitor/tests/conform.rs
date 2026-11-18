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

/// The programs a regression could make the monitor run on this machine: the file manager (a
/// reveal), and an agent or a terminal it could type into (a send or a resume). Each is a stub
/// first on the monitor's PATH that writes its name and arguments to `calls` and does nothing.
const STUBBED: [&str; 6] = ["open", "xdg-open", "claude", "codex", "tmux", "herdr"];

fn stubs(dir: &Path, calls: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    for name in STUBBED {
        let stub = dir.join(name);
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit 1\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The monitor, paired, with every store, state and cache root inside `base`, and its log, its
/// PATH's stubs and their calls in `run`, beside it.
fn monitor(base: &Path, run: &Path, port: u16) -> Command {
    let stores = base.join("stores");
    let log = std::fs::File::create(run.join("monitor.log")).unwrap();
    let bin = run.join("bin");
    stubs(&bin, &run.join("calls.log"));
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-monitor"));
    cmd.args(["--pair", "--no-open", "--port", &port.to_string()])
        .env("PATH", path)
        // The token alone admits, on Linux as on macOS (the browser harness's reason, #366).
        .env("AGENT_MONITOR_TOKEN_ONLY", "1")
        .env_remove("AGENT_MONITOR_TRUSTED_HOSTS")
        .env_remove("CODEX_NATIVE_SERVER_URL")
        // The Arrange choice's sources, none of this machine's (the browser harness's stand-ins):
        // the Claude app's settings, the Codex app's databases, and herdr, which is the stub.
        .env("AGENT_MONITOR_HERDR", bin.join("herdr"))
        .env(
            "CLAUDE_DESKTOP_CONFIG",
            base.join("claude-desktop-config.json"),
        )
        .env(
            "AGENT_MONITOR_CODEX_APP_DB",
            base.join("codex-app/state_5.sqlite"),
        )
        .env(
            "AGENT_MONITOR_CODEX_APP_STATE",
            base.join("codex-app/codex-global-state.json"),
        )
        .env(
            "AGENT_MONITOR_CODEX_APP_CATALOG",
            base.join("codex-app/codex.db"),
        )
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

/// Every file under `dir` but those under `cache/` (the durable session cache, which a pull is
/// meant to write), relative to `dir`, with its bytes: what the check may not have changed.
fn snapshot(dir: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            if rel.starts_with("cache") {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.insert(rel, bytes);
            }
        }
    }
    out
}

/// The files that differ between two snapshots: added, changed or removed.
fn changed(
    before: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
    after: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|p| before.get(*p) != after.get(*p))
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

#[test]
fn the_conformance_check_passes_against_a_paired_agent_monitor() {
    let base = scratch("monitor");
    let run = scratch("monitor-run");
    let sid = "5a5a5a5a-0000-4000-8000-00000000c0f1";
    conform_kit::write_session(&base.join("stores/claude"), &base.join("tern-harbour"), sid);
    let port = free_port();
    let _serving = Reap(monitor(&base, &run, port).spawn().unwrap());
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the monitor never listened on {port}: {}",
            std::fs::read_to_string(run.join("monitor.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let token = std::fs::read_to_string(base.join("state/auth-token")).unwrap();
    let token = token.trim().to_string();
    let bearer = format!("Authorization: Bearer {token}");
    let before = snapshot(&base);

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

    // The token, which it was given, is in nothing it printed: the report, its stderr, the lines a
    // person reads, and the refusal of a --header it could not use, which names no part of it.
    for said in [&out.stdout, &out.stderr] {
        assert!(!said.contains(&token), "the token is never printed");
    }
    let (code, stdout, stderr) =
        conform_kit::run_text(&url, &["--header", &bearer, "--session", sid]).unwrap();
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("conforms to contract 1"), "{stdout}");
    assert!(!stdout.contains(&token) && !stderr.contains(&token));
    for malformed in [
        format!("Bearer {token}"),
        format!("Authorization: Bearer {token}\r\nX-Other: 1"),
    ] {
        let (code, stdout, stderr) =
            conform_kit::run_text(&url, &["--header", &malformed]).unwrap();
        assert_eq!(code, Some(2), "a usage error: {stdout}{stderr}");
        assert!(
            !stdout.contains(&token) && !stderr.contains(&token),
            "the malformed header is not quoted: {stderr}"
        );
    }
    let (code, _, stderr) =
        conform_kit::run_text(&format!("http://reader:{token}@127.0.0.1:{port}/"), &[]).unwrap();
    assert_eq!(code, Some(2));
    assert!(!stderr.contains(&token), "{stderr}");

    // Nothing it did stayed done. Outside the session cache, which a pull is meant to write, the
    // one file that differs is the hide list, which holds no key of the check's, and no pane was
    // granted.
    let after = snapshot(&base);
    assert_eq!(
        changed(&before, &after),
        [PathBuf::from("state/ignored.json")],
        "the check changed only the hide list"
    );
    let ignored = std::fs::read_to_string(base.join("state/ignored.json")).unwrap_or_default();
    assert!(
        !ignored.contains("conform-check"),
        "the key it hid is gone: {ignored}"
    );
    // And nothing ran on this machine in the monitor's name: no file manager, agent or terminal.
    // The one call a stub may see is the Arrange choice reading herdr's session list, which the
    // check's own `GET /api/layout?source=herdr` asks for; the stub answers it with nothing.
    let calls = std::fs::read_to_string(run.join("calls.log")).unwrap_or_default();
    let acted: Vec<&str> = calls
        .lines()
        .filter(|call| *call != "herdr session list --json")
        .collect();
    assert_eq!(
        acted,
        Vec::<&str>::new(),
        "the monitor ran none of {STUBBED:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&run);
}
