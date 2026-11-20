//! #s204: a paired monitor started where no person reads its output — as its LaunchAgent runs it,
//! both streams appended to one log file — writes no token there, on a fresh start or on the
//! hand-off to a monitor already running. The pure rule is `control::printed_url`'s test; this is
//! the binary, so a print site that bypasses the rule fails here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cm-startup-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The monitor with every store, state and cache root inside `base`, its output in `log` (one
/// file for both streams, as launchd's StandardOutPath and StandardErrorPath name one file),
/// started with `--pair` when `pair`.
fn monitor(base: &Path, log: &Path, port: u16, pair: bool) -> Command {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    let err = out.try_clone().unwrap();
    let stores = base.join("stores");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-monitor"));
    if pair {
        cmd.arg("--pair");
    }
    cmd.args(["--no-open", "--port", &port.to_string()])
        .env("HOME", base.join("home"))
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_STATE_HOME", base.join("xdg-state"))
        .env("XDG_CACHE_HOME", base.join("xdg-cache"))
        .env("AGENT_MONITOR_STATE", base.join("state"))
        .env("CLAUDE_MONITOR_STATE", base.join("state"))
        .env("AGENT_MONITOR_CACHE", base.join("cache"))
        .env("CLAUDE_MONITOR_CACHE", base.join("cache"))
        .env("CLAUDE_PROJECTS_DIR", stores.join("claude"))
        .env("CLAUDE_SCRATCH_ROOT", stores.join("claude-scratch"))
        .env("CODEX_HOME", stores.join("codex"))
        .env("CODEX_SESSIONS_DIR", stores.join("codex/sessions"))
        .env("QODER_PROJECTS_DIR", stores.join("qoder"))
        .env("QODERWORK_PROJECTS_DIR", stores.join("qoderwork"))
        .env("QODERWORK_DB", stores.join("qoderwork.db"))
        .env("QWENWORK_PROJECTS_DIR", stores.join("qwenwork"))
        .env("QWENWORK_DB", stores.join("qwenwork.db"))
        .env_remove("CODEX_NATIVE_SERVER_URL")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    cmd
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

#[test]
fn a_paired_monitor_writes_no_token_to_a_log() {
    let base = scratch("paired");
    let port = free_port();
    let log = base.join("agent-monitor.log");
    let serving = Reap(monitor(&base, &log, port, true).spawn().unwrap());

    let deadline = Instant::now() + Duration::from_secs(60);
    let started = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("agent-monitor serving") && text.lines().count() >= 3 {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "the monitor never said it was serving; its log: {text}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };

    // The hand-off: a second start finds the first and prints where it is, then exits.
    let handoff_log = base.join("handoff.log");
    let status = monitor(&base, &handoff_log, port, true).status().unwrap();
    assert!(status.success(), "the hand-off exited with {status}");
    let handoff = std::fs::read_to_string(&handoff_log).unwrap();
    drop(serving);
    // #s604: the running monitor was paired with this token, so the hand-off says nothing more.
    assert!(
        !handoff.contains("was started without this token"),
        "the hand-off to a monitor paired with the token said it was not: {handoff}"
    );

    let token = std::fs::read_to_string(base.join("state/auth-token")).unwrap();
    let token = token.trim();
    assert!(!token.is_empty(), "--pair minted no token");
    let url = format!("http://127.0.0.1:{port}/");
    for (what, text) in [("start", &started), ("hand-off", &handoff)] {
        // Neither the token nor any part of a tokened URL, so the check holds without echoing it.
        assert!(
            !text.contains(token),
            "the {what} wrote the token to its log"
        );
        assert!(
            !text.contains("?token="),
            "the {what} wrote a tokened URL to its log"
        );
        assert!(
            text.lines().any(|l| l.trim() == url),
            "the {what} printed no bare URL on a line of its own: {text}"
        );
        assert!(
            text.contains("agent-monitor --pair` in a terminal"),
            "the {what} did not say where the token is: {text}"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// One request to the monitor at `port`, its whole reply as text.
fn http(port: u16, head: &str, body: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let length = format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(format!("{head}{length}{body}").as_bytes())
        .unwrap();
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    raw
}

/// Wait for the log at `log` to hold `said`; its text.
fn wait_for(log: &Path, said: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if text.contains(said) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "the monitor never said {said:?}; its log: {text}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// An unpaired monitor the cases below pair while it runs (#s619): the token alone admits once it
/// is paired, on Linux as on macOS (`AGENT_MONITOR_TOKEN_ONLY`), no dialog is raised, and nothing
/// relays to its port.
fn unpaired_monitor(base: &Path, log: &Path, port: u16) -> Reap {
    let mut cmd = monitor(base, log, port, false);
    cmd.env("AGENT_MONITOR_TOKEN_ONLY", "1")
        .env("AGENT_MONITOR_PAIR_DIALOG", "0")
        .env_remove("AGENT_MONITOR_TRUSTED_HOSTS");
    let serving = Reap(cmd.spawn().unwrap());
    wait_for(log, "agent-monitor serving");
    // Admitted while unpaired, once its first read of the tailnet has said no relay reaches it.
    let deadline = Instant::now() + Duration::from_secs(30);
    let bare = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    loop {
        let page = http(port, &bare, "");
        if page.starts_with("HTTP/1.1 200") {
            assert!(page.contains(r#"data-paired="false""#), "{page}");
            break;
        }
        assert!(Instant::now() < deadline, "never admitted unpaired: {page}");
        std::thread::sleep(Duration::from_millis(200));
    }
    serving
}

/// What the paired monitor answers: no token refused, the token admitted to a page that says it is
/// paired, and `/pair` answered as a paired monitor answers it.
fn assert_paired_now(port: u16, token: &str) {
    let bare = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    let refused = http(port, &bare, "");
    assert!(
        refused.starts_with("HTTP/1.1 401"),
        "the paired monitor admitted a request with no token: {}",
        refused.lines().next().unwrap_or("")
    );
    // The token in a request is swapped for the cookie (a 302 to the same page), which then admits.
    let swap = http(
        port,
        &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n"),
        "",
    );
    let cookie = swap
        .lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: "))
        .and_then(|c| c.split(';').next())
        .filter(|c| c.starts_with("cmauth-") && c.ends_with(&format!("={token}")))
        .unwrap_or_else(|| {
            panic!(
                "the token was not swapped for the cookie: {}",
                swap.lines().next().unwrap_or("")
            )
        })
        .to_string();
    let page = http(
        port,
        &format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\n"),
        "",
    );
    assert!(
        page.starts_with("HTTP/1.1 200") && page.contains(r#"data-paired="true""#),
        "the page does not say the monitor is paired: {}",
        page.lines().next().unwrap_or("")
    );
    let pair = http(
        port,
        &format!("GET /pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"),
        "",
    );
    assert!(
        pair.contains("Pairing lets this browser read the monitor")
            && !pair.contains("pairs the monitor too"),
        "/pair does not answer as a paired monitor's: {pair}"
    );
}

/// #s619 (it was #s604's restart note): `--pair` handed off to a monitor that started unpaired
/// pairs the running monitor at once. The hand-off opens it with the token, as for any paired
/// monitor, and says nothing about a restart; the serving monitor says it is now paired; neither
/// writes the token to its log.
#[test]
fn a_pair_handed_to_an_unpaired_monitor_pairs_it_at_once() {
    let base = scratch("unpaired");
    let port = free_port();
    let log = base.join("agent-monitor.log");
    let serving = unpaired_monitor(&base, &log, port);
    assert!(
        !base.join("state/auth-token").exists(),
        "the unpaired monitor minted a token"
    );

    let handoff_log = base.join("handoff.log");
    let status = monitor(&base, &handoff_log, port, true).status().unwrap();
    assert!(status.success(), "the hand-off exited with {status}");
    let handoff = std::fs::read_to_string(&handoff_log).unwrap();
    let token = std::fs::read_to_string(base.join("state/auth-token")).unwrap();
    let token = token.trim().to_string();
    assert!(!token.is_empty(), "--pair minted no token");
    assert_paired_now(port, &token);
    let served = wait_for(&log, "agent-monitor is now paired");
    drop(serving);
    assert!(
        served.contains("`agent-monitor --pair` wrote its token"),
        "{served}"
    );
    assert!(
        !handoff.contains("was started without this token") && !handoff.contains("Restart"),
        "the hand-off said the running monitor needs a restart: {handoff}"
    );
    // The token is handed on, and kept out of this log: the note says where it is.
    assert!(
        handoff.contains("agent-monitor --pair` in a terminal"),
        "the hand-off did not hand on the token: {handoff}"
    );
    for (what, text) in [("hand-off", &handoff), ("serving monitor", &served)] {
        assert!(
            !text.contains(&token),
            "the {what} wrote the token to its log"
        );
        assert!(
            !text.contains("?token="),
            "the {what} wrote a tokened URL to its log"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// #s619: on a monitor that started unpaired, a browser on this machine pairs from the page, and
/// that pairs the running monitor: `/pair` says so before it asks, the code (here from the state
/// dir, as `--pair-code` would print it; no dialog is raised) creates the token file at 0600 and
/// sets the cookie that carries it, and from then on the monitor requires the token, its page says
/// it is paired, `/pair` answers as a paired monitor's, and its log says it is now paired.
#[test]
fn an_unpaired_monitor_is_paired_from_its_page_at_once() {
    let base = scratch("from-the-page");
    let port = free_port();
    let log = base.join("agent-monitor.log");
    let serving = unpaired_monitor(&base, &log, port);
    let here = format!("Host: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n");
    let page = http(
        port,
        &format!("GET /pair HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"),
        "",
    );
    assert!(
        page.starts_with("HTTP/1.1 200")
            && page.contains("Pairing this browser pairs the monitor too")
            && page.contains("requires pairing for every browser and tool that reaches it"),
        "{page}"
    );
    let ask = http(port, &format!("POST /api/pair/ask HTTP/1.1\r\n{here}"), "");
    assert!(ask.contains(r#""shown":"terminal""#), "{ask}");
    let codes = std::fs::read_to_string(base.join("state/page-codes")).unwrap();
    let code = codes
        .lines()
        .find_map(|l| l.strip_prefix("code "))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("a page code waits")
        .to_string();
    assert!(!ask.contains(&code), "never in the reply");
    let paired = http(port, &format!("POST /api/pair HTTP/1.1\r\n{here}"), &code);
    assert!(paired.starts_with("HTTP/1.1 200"), "{paired}");
    let token_path = base.join("state/auth-token");
    let token = std::fs::read_to_string(&token_path).unwrap();
    let token = token.trim().to_string();
    assert_eq!(token.len(), 64, "a token was minted");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "owner-only");
    }
    assert!(
        paired
            .lines()
            .any(|l| l.starts_with("Set-Cookie: cmauth-") && l.contains(&format!("={token};"))),
        "the cookie carries the token the file holds"
    );
    assert_paired_now(port, &token);
    let served = wait_for(&log, "agent-monitor is now paired");
    drop(serving);
    assert!(
        served.contains("a browser on this machine paired from the page"),
        "{served}"
    );
    assert!(
        !served.contains(&token),
        "the monitor wrote the token to its log"
    );
    let _ = std::fs::remove_dir_all(&base);
}
