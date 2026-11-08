//! A server that mounts agent-monitor's route table under a path of its own (#s426, stage 5 of
//! `design/backend-mount.md`), built from the crates' public API alone, as another Rust web
//! service would build it. It names its own sessions, leaves this machine's processes alone, takes
//! a cache root of its own, signs its readers in its own way, and serves reading only.
//!
//! What it holds: the host's sessions are listed and the environment's are not; every route the
//! contract documents is answered, except the ones this host leaves out, which answer exactly the
//! ceiling's 404; and the host's sign-in, body limits and finished replies behave as the contract
//! asks.
//!
//! One test, in a binary of its own: the table installs mdrev's state and the relayed-message key
//! once per process, and the store variables are process-wide.

#[path = "contract_index/mod.rs"]
mod contract_index;

use claude_monitor::index::Index;
use claude_monitor::root_claim::{self, Claimed, Holder};
use claude_monitor::routes::{self, Backend, Frontend, OtherTools, Serves};
use claude_replay_core::Agent;
use claude_replay_html::{
    finish_reply, HttpResponse, ReplyClient, Request, RootLock, RouteHandler, ServiceConfig,
    SessionService,
};
use claude_replay_present::cache::Presentation;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Where the host mounts the table.
const PREFIX: &str = "/agents/monitor/";
/// The host's own sign-in: a cookie it set when its reader signed in.
const SIGNED_IN: &str = "Cookie: host-session=fixture";
/// What this host serves beyond reading: nothing.
const READS_ONLY: Serves = Serves {
    files: false,
    reveal: false,
    writes: false,
    review: false,
};

/// One reply as the host's reader receives it.
struct Reply {
    code: String,
    head: String,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.head.lines().find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }
}

/// The host's server: one thread per connection, each request read whole and answered once.
fn serve(listener: TcpListener, table: RouteHandler) {
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let table = table.clone();
            std::thread::spawn(move || answer(conn, &table, port));
        }
    });
}

fn answer(mut conn: TcpStream, table: &RouteHandler, port: u16) {
    let mut raw = Vec::new();
    let mut byte = [0u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        if raw.len() > 16 * 1024 || conn.read(&mut byte).unwrap_or(0) == 0 {
            return;
        }
        raw.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    let reply = |code: &'static str, text: &str| {
        let mut r = HttpResponse::ok("text/plain; charset=utf-8", text.as_bytes().to_vec());
        r.code = code;
        r
    };

    // The host's sign-in, in front of the table. It refuses a request another site started,
    // whatever its method (the contract's rule for a host): the table counts a missing Origin as
    // the page's own, and two of its GET routes have side effects.
    let response = if header("cookie") != Some("host-session=fixture") {
        reply("401 Unauthorized", "sign in to the host first")
    } else if header("sec-fetch-site").is_some_and(|s| s != "same-origin" && s != "none")
        || header("origin").is_some_and(|o| o != format!("http://127.0.0.1:{port}"))
    {
        reply("403 Forbidden", "another site's request")
    } else if let Some(rest) = target.strip_prefix(PREFIX) {
        let (name, query) = rest.split_once('?').unwrap_or((rest, ""));
        let length: usize = header("content-length")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        if length > routes::body_limit(name) {
            reply("413 Payload Too Large", "too large for this route")
        } else {
            let mut body = vec![0u8; length];
            if conn.read_exact(&mut body).is_err() {
                return;
            }
            let request = Request {
                method,
                name,
                query,
                body: &body,
                // No reader of this host is the machine's owner, and it serves no writes.
                authenticated: false,
                // The sign-in above refused every other site's request.
                origin_ok: true,
                masked: false,
            };
            finish_reply(
                table(&request),
                ReplyClient {
                    method,
                    accepts_gzip: header("accept-encoding").is_some_and(|e| e.contains("gzip")),
                    remote: false,
                    masked: false,
                },
            )
        }
    } else {
        reply("404 Not Found", "not this host's")
    };

    let length = match &response.stream {
        Some((_, length)) => *length,
        None => response.body.len() as u64,
    };
    let mut out = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {length}\r\n",
        response.code, response.content_type
    );
    for line in &response.headers {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    let _ = conn.write_all(out.as_bytes());
    match &response.stream {
        Some((path, _)) => {
            if let Ok(mut file) = std::fs::File::open(path) {
                let _ = std::io::copy(&mut file, &mut conn);
            }
        }
        None => {
            let _ = conn.write_all(&response.body);
        }
    }
}

/// Ask the host, as a browser on it would.
fn ask(port: u16, method: &str, path: &str, headers: &[&str], body: &[u8]) -> Reply {
    let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    for line in headers {
        head.push_str(line);
        head.push_str("\r\n");
    }
    if !headers
        .iter()
        .any(|h| h.to_ascii_lowercase().starts_with("content-length:"))
    {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    conn.write_all(head.as_bytes()).unwrap();
    conn.write_all(body).unwrap();
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a reply head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let code = head
        .lines()
        .next()
        .and_then(|status| status.split_once(' '))
        .map(|(_, code)| code.to_string())
        .unwrap_or_default();
    Reply {
        code,
        head,
        body: raw[split + 4..].to_vec(),
    }
}

/// A process named `claude` waiting in `cwd`: what the monitor's probes would link to the lone
/// session there. Stopped when dropped.
struct StandIn(std::process::Child);

impl StandIn {
    fn spawn(bin: &Path, cwd: &Path) -> Self {
        std::fs::create_dir_all(bin).unwrap();
        // A link, not a copy: macOS kills a copied system binary for its signature.
        let exe = bin.join("claude");
        std::os::unix::fs::symlink("/bin/sleep", &exe).unwrap();
        let child = std::process::Command::new(&exe)
            .arg("600")
            .current_dir(cwd)
            .spawn()
            .unwrap();
        Self(child)
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// A one-prompt Claude session in `store`, in the project at `cwd`.
fn write_session(store: &Path, cwd: &str, sid: &str) {
    let project = store.join(cwd.replace('/', "-"));
    std::fs::create_dir_all(&project).unwrap();
    let ts = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600;
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
    };
    std::fs::write(
        project.join(format!("{sid}.jsonl")),
        format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"cwd\":\"{cwd}\",\"timestamp\":\"{ts}\",\"message\":{{\"role\":\"user\",\"content\":\"Count the terns on the harbour wall.\"}}}}\n"
        ),
    )
    .unwrap();
}

#[test]
fn a_host_mounts_the_table_under_its_own_path_with_its_own_sessions_and_sign_in() {
    let base = std::env::temp_dir().join(format!("cm-mount-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    // The environment names a store of its own, holding a session the host does not serve, and a
    // monitor's state directory the host must never write into.
    let decoy = base.join("environment-projects");
    std::env::set_var("CLAUDE_PROJECTS_DIR", &decoy);
    for (var, dir) in [
        ("QODERWORK_PROJECTS_DIR", "qoderwork"),
        ("QWENWORK_PROJECTS_DIR", "qwenwork"),
        ("QODER_PROJECTS_DIR", "qoder"),
        ("CODEX_HOME", "codex"),
    ] {
        std::env::set_var(var, base.join(dir));
    }
    std::env::set_var("AGENT_MONITOR_STATE", base.join("environment-state"));
    let on_the_machine = "5a5a5a5a-0000-4000-8000-00000000decf";
    write_session(&decoy, "/tmp/skua-cliffs", on_the_machine);

    // The host's own world: its sessions, its cache root, its state.
    let store = base.join("host-projects");
    let hosted = "5a5a5a5a-0000-4000-8000-000000004260";
    let harbour = base.join("tern-harbour");
    std::fs::create_dir_all(&harbour).unwrap();
    write_session(&store, harbour.to_str().unwrap(), hosted);
    // An agent process in the session's directory, which the monitor's own index would link to
    // it (a lone agent in a session's directory is that session's): this host never looks.
    let _agent = StandIn::spawn(&base.join("bin"), &harbour);
    let root = base.join("host-cache");
    let state = base.join("host-state");
    std::fs::create_dir_all(&root).unwrap();
    assert!(matches!(
        root_claim::claim(&root, Holder::Host).unwrap(),
        Claimed::Ours
    ));

    let scratch = base.join("host-scratch");
    let service = Arc::new(
        SessionService::new(ServiceConfig {
            cache_root: Some(root.clone()),
            presentation: Presentation::Html,
            fold: Default::default(),
            scratch: scratch.clone(),
            root_lock: RootLock::SingleWriter,
        })
        .unwrap(),
    );
    let idx = Index::new(root.clone(), state.clone(), Vec::new())
        .with_stores(vec![(Agent::CLAUDE, store.clone())])
        .without_process_probes();
    let backend = Arc::new(Backend {
        service,
        idx: Arc::new(idx),
        scratch,
        attempts: Mutex::new(Default::default()),
        rescanned: Mutex::new(None),
        layouts: Default::default(),
        serves: READS_ONLY,
    });
    let table = routes::mounted(
        backend,
        Frontend {
            version: "0.0.0-host",
            paired: false,
        },
        OtherTools::NONE,
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    serve(listener, table);
    let get = |path: &str| ask(port, "GET", &format!("{PREFIX}{path}"), &[SIGNED_IN], b"");

    // The host's own sign-in and its own path are the host's, in front of the table: what the
    // contract asks of a host, shown working with the table behind it.
    let out = ask(port, "GET", &format!("{PREFIX}api/sessions"), &[], b"");
    assert_eq!(out.code, "401 Unauthorized", "{}", out.text());
    let beside = ask(port, "GET", "/api/sessions", &[SIGNED_IN], b"");
    assert_eq!(
        (beside.code.as_str(), beside.text().as_str()),
        ("404 Not Found", "not this host's")
    );

    // The page, at the host's path, speaks the contract's version.
    let page = get("");
    assert_eq!(page.code, "200 OK");
    assert!(
        page.text().contains("data-contract=\"1\""),
        "{}",
        page.text()
    );

    // The host's sessions and not the environment's, and nothing read off this machine's
    // processes: the agent beside the session is not linked to it.
    let list = get("api/sessions");
    assert_eq!(list.code, "200 OK", "{}", list.text());
    let list: serde_json::Value = serde_json::from_slice(&list.body).unwrap();
    let rows: Vec<&serde_json::Value> = list["groups"]
        .as_array()
        .expect("groups")
        .iter()
        .flat_map(|g| g["rows"].as_array().expect("rows"))
        .collect();
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, [hosted], "the host's session, never the environment's");
    for field in ["pid", "term", "target"] {
        assert!(rows[0].get(field).is_none(), "no {field}: {}", rows[0]);
    }

    // Every route the contract documents: answered, unless this host leaves it out, and then the
    // ceiling's own 404. A core route is never left out. Codex native mode is the monitor
    // binary's alone, so its routes are no route here.
    let left_out = [
        "/api/layout",
        "/file",
        "/__reveal",
        "/api/send",
        "/api/consent",
        "/api/dialog",
        "/api/answer",
        "/api/mdrev-review/",
    ];
    let doc = contract_index::read();
    let index = contract_index::rows(&doc);
    let mut answered = 0;
    for row in &index {
        let probe = contract_index::probe(
            row,
            mdrev::VERSION,
            claude_replay_html::mermaid_runtime_path(),
        );
        let method = contract_index::method(row);
        let reply = ask(port, method, &format!("{PREFIX}{probe}"), &[SIGNED_IN], b"");
        let what = format!(
            "{} {} ({}: {})",
            row.method,
            row.path,
            reply.code,
            reply.text()
        );
        assert!(
            reply.header("cache-control").is_some(),
            "every finished reply says how to cache it: {what}"
        );
        // The table's answer to a path no route has: its fallback's 404 or 403.
        let no_route = (reply.code == "404 Not Found" && reply.text() == "not found")
            || (reply.code == "403 Forbidden" && reply.text() == "forbidden");
        if row.tier == "native" {
            assert!(no_route, "{what}");
        } else if left_out.contains(&row.path.as_str()) {
            assert_ne!(row.tier, "core", "a host cannot leave out {what}");
            assert_eq!(
                (reply.code.as_str(), reply.text().as_str()),
                ("404 Not Found", "not served by this host"),
                "{what}"
            );
        } else {
            assert!(
                !no_route && reply.text() != "not served by this host",
                "the host serves {what}"
            );
            answered += 1;
        }
    }
    assert!(answered >= 10, "answered only {answered}");
    for path in left_out {
        assert!(
            index.iter().any(|row| row.path == path),
            "{path} is a row of the route index"
        );
    }

    // Another site's request is refused by the host's sign-in, a GET with a side effect
    // included (the table cannot tell one from the page's own), and the hide list is never written.
    let hide = format!("{PREFIX}api/ignore?add=s%3A{hosted}");
    for from in [
        "Sec-Fetch-Site: cross-site",
        "Origin: https://elsewhere.example",
    ] {
        let refused = ask(port, "GET", &hide, &[SIGNED_IN, from], b"");
        assert_eq!(refused.code, "403 Forbidden", "{from}");
    }
    assert!(!state.join("ignored.json").exists(), "nothing was hidden");
    // The host's own page may: its hide list is in its own state directory.
    let hidden = ask(
        port,
        "GET",
        &hide,
        &[SIGNED_IN, "Sec-Fetch-Site: same-origin"],
        b"",
    );
    assert_eq!(hidden.code, "200 OK", "{}", hidden.text());
    assert!(state.join("ignored.json").is_file());
    assert!(
        !base.join("environment-state").join("ignored.json").exists(),
        "and never the environment's"
    );

    // Each route's body limit is the table's, read by route: `hold` takes a document far larger
    // than any other route's body, and one byte over its own is refused before it is read.
    let general = routes::body_limit("api/sessions");
    assert!(routes::body_limit("api/mdrev/hold") > general);
    let large = vec![b' '; general + 1];
    let held = ask(
        port,
        "POST",
        &format!("{PREFIX}api/mdrev/hold"),
        &[SIGNED_IN],
        &large,
    );
    assert_ne!(held.code, "413 Payload Too Large", "{}", held.text());
    let over = routes::body_limit("api/mdrev/hold") + 1;
    let length = format!("Content-Length: {over}");
    let big = ask(
        port,
        "POST",
        &format!("{PREFIX}api/mdrev/hold"),
        &[SIGNED_IN, &length],
        b"",
    );
    assert_eq!(big.code, "413 Payload Too Large");

    // The Mermaid runtime the route keeps gzipped reaches a reader that cannot take gzip decoded,
    // and one that can as it is.
    let mermaid = claude_replay_html::mermaid_runtime_path().trim_start_matches('/');
    let plain = get(mermaid);
    assert_eq!(plain.code, "200 OK");
    assert!(plain.header("content-encoding").is_none(), "{}", plain.head);
    assert!(
        !plain.body.starts_with(&[0x1f, 0x8b]),
        "decoded for this reader"
    );
    let packed = ask(
        port,
        "GET",
        &format!("{PREFIX}{mermaid}"),
        &[SIGNED_IN, "Accept-Encoding: gzip"],
        b"",
    );
    assert_eq!(packed.header("content-encoding"), Some("gzip"));
    assert!(packed.body.starts_with(&[0x1f, 0x8b]));

    // The host's root says a host holds it, which is what refuses a monitor started over it
    // (`root_claim`'s own test holds the refusal).
    let lock: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("LOCK")).unwrap()).unwrap();
    assert_eq!(lock["note"]["holder"], "host", "{lock}");

    let _ = std::fs::remove_dir_all(&base);
}
