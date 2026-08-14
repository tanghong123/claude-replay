//! `claude-monitor` — every agent session on this machine, one page, over loopback HTTP
//! (#98). The page is the RAIL (this crate's own markup) beside the existing claude-replay
//! session view in an `<iframe src="/session?id=…">` — composition at the document level
//! (§6.3), never a fork of the renderer (R10).
//!
//! Read-only, loopback only (§11). No fold on the index path (R7), no background sweep
//! (§3): a session's durable entry is written by VISITING it, and the rail's counters read
//! that entry's meta stream lock-free.

mod cost;
mod index;
mod state;

use anyhow::{Context, Result};
use claude_replay_core::Agent;
use claude_replay_html::{query_get, service_routes, spawn_listener, HttpResponse, ServiceConfig};
use claude_replay_present::cache::Presentation;
use std::sync::Arc;

/// The stable default port (§11): the monitor is a bookmarkable place.
const DEFAULT_PORT: u16 = 2727;

/// The rail page — self-contained, its own markup and script (§6.3). `{{VERSION}}` is the
/// only server-side substitution: which build is running (mirrors the HTML viewer's brand).
const RAIL_TEMPLATE: &str = include_str!("rail.html");

/// What claiming the cache root came to.
#[derive(Debug)]
enum Claimed {
    /// The root is ours: stand the server up.
    Ours,
    /// A live monitor already serves this root, here. Hand off to it.
    Served(String),
}

/// Take the cache root, or hand off to the monitor that already has it (#160, #166).
///
/// **One monitor per root.** The cache root is single-writer by construction (#96), and every
/// entry a peer holds is one this process would be DENIED. Binding 2727 twice fails, but
/// `--port` walks past it, so the lock is what actually enforces it.
///
/// Being second is not an error, though — it means the thing you asked for is already running.
/// `claude-replay --html` has always handed a second invocation to the first
/// ([`claude_replay_html::existing_server`]); this used to print a message and exit 1 instead,
/// leaving the URL to be copied by hand. Now it opens the running monitor and quits, and prints
/// that URL on stdout exactly where a normal start prints its own — a script capturing stdout to
/// find the monitor gets an answer either way.
///
/// The URL comes from the note, published once the listener binds. A holder that has taken the
/// lock but not bound yet is a real window, not a dead process — it counts as live, and it has no
/// URL, so that one IS an error rather than a hand-off to nowhere. A dead holder is reclaimed by
/// `acquire`, which is what makes a killed monitor's lock harmless. A lock we cannot WRITE is
/// reported and ignored — a temp I/O fault should not stop the tool.
fn claim_root(root: &std::path::Path) -> Result<Claimed> {
    use claude_replay_present::cache::lock;
    let holder = match lock::acquire::<serde_json::Value>(root, |h| {
        lock::pid_alive(h.pid) && port_answers(note_port(h.note.as_ref()))
    }) {
        Ok(lock::Taken::Owned) => return Ok(Claimed::Ours),
        Ok(lock::Taken::Held(h)) => h,
        Err(e) => {
            eprintln!(
                "warning: could not take the root lock at {}: {e}",
                root.display()
            );
            return Ok(Claimed::Ours);
        }
    };
    match note_port(holder.note.as_ref()) {
        Some(port) => Ok(Claimed::Served(format!("http://127.0.0.1:{port}/"))),
        None => anyhow::bail!(
            "claude-monitor is starting up in another process (pid {}) — it holds this cache \
             root but has not published a port yet. Try again in a moment, or stop it first.",
            holder.pid
        ),
    }
}

/// Open `url` in the default browser (best-effort; never fails the run). Both the normal start
/// and the hand-off to a running monitor go through here — the user asked for a monitor, and
/// which process ends up serving it is not their problem.
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(target_os = "windows")]
    let prog = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let prog = "xdg-open";
    let _ = std::process::Command::new(prog)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The port out of a root lock's note, which is plain JSON here — the monitor has no serde
/// derive and needs exactly one field.
fn note_port(note: Option<&serde_json::Value>) -> Option<u16> {
    note?.get("port")?.as_u64().map(|p| p as u16)
}

/// Whether a holder's published port still answers. Same rule the html server's rendezvous
/// uses: a pid alone is not enough (pids are recycled), and a holder that has taken the lock
/// but not yet bound (`None`) counts as live.
fn port_answers(port: Option<u16>) -> bool {
    let Some(port) = port else { return true };
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .is_ok()
}

/// Reclaim the scratch locations this crate used before #162, once.
///
/// `$TMPDIR/claude-monitor` (pre-#161) and `<root>/scratch/<pid>` (pre-#162). Nothing reads
/// either any more. `remove_dir` — not `_all` — on the temp parent, so anything unexpected
/// there is left for a person rather than deleted by a tool.
fn reclaim_legacy_scratch() {
    let legacy = std::env::temp_dir().join("claude-monitor");
    if let Ok(entries) = std::fs::read_dir(&legacy) {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.ends_with(".records") {
                let _ = std::fs::remove_file(e.path());
            } else if name.parse::<u32>().is_ok() {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(&legacy);
}

fn main() -> Result<()> {
    let mut port = DEFAULT_PORT;
    let mut only: Vec<Agent> = Vec::new();
    let mut open_browser = true;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                port = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .context("--port needs a number")?;
            }
            "--agents" => {
                // R1's narrowing axis: e.g. `--agents claude,codex`.
                let v = args.next().context("--agents needs a list")?;
                for name in v.split(',') {
                    only.push(match name.trim().to_ascii_lowercase().as_str() {
                        "claude" => Agent::CLAUDE,
                        "codex" => Agent::CODEX,
                        "qoderwork" | "qoder" => Agent::QODERWORK,
                        other => anyhow::bail!("unknown agent {other:?}"),
                    });
                }
            }
            "--no-open" => open_browser = false,
            "--help" | "-h" => {
                println!(
                    "claude-monitor — every agent session on this machine, over loopback HTTP\n\n\
                     USAGE: claude-monitor [--port N] [--agents claude,codex,qoderwork] [--no-open]\n\n\
                     Serves http://127.0.0.1:{DEFAULT_PORT} (loopback only, read-only).\n\
                     Cache root: $CLAUDE_MONITOR_CACHE, else ~/.cache/claude-monitor —\n\
                     never the viewer's (R5).\n\n\
                     Process recognition: built-in basenames are claude, codex, qoderwork, qoder.\n\
                     Extend with $CLAUDE_MONITOR_AGENT_PATTERNS — comma-separated basename:<name>,\n\
                     argv:<substring>, or a bare <name> (a basename). Wrapper launches need argv:,\n\
                     e.g. \"argv:npx codex,argv:node_modules/.bin/codex,basename:my-agent\"."
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown flag {other:?} (try --help)"),
        }
    }

    let root = index::default_root()?;
    // Before anything is opened: one monitor per root (#160) — and if another one has it, that
    // is where the user wants to go (#166), so open it and stop rather than fail.
    if let Claimed::Served(url) = claim_root(&root)? {
        eprintln!("claude-monitor is already running — opening {url}");
        if open_browser {
            open_url(&url);
        }
        println!("{url}");
        return Ok(());
    }
    // Scratch lives under the monitor's OWN root (#161), not `$TMPDIR` — everything this tool
    // writes is then in one place a person can find, inspect and delete. It was in
    // `$TMPDIR/claude-monitor`, which on macOS resolves to an opaque `/var/folders/…` path
    // nothing sweeps for days; that is where 14 GB accumulated unnoticed. "Temporary" described
    // its LIFETIME, and the directory delivered neither.
    //
    // ONE directory, no pid (#162). The monitor's cache is a single entity under a single root
    // lock: `claim_root` above has already established that no other monitor is running, so
    // there is nobody to segregate from, and a wipe here cannot destroy a live peer's log. A
    // crashed run leaves scratch behind and the next start simply wipes it — which is why the
    // pid-keyed layout and its liveness sweep bought nothing but names to reap.
    let scratch = root.join("scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    reclaim_legacy_scratch();
    // The session service at the MONITOR's root (§3/§10): same presentation namespace,
    // different root — a running `claude-replay --html` and this server cannot contend.
    let service = Arc::new(claude_replay_html::SessionService::new(ServiceConfig {
        cache_root: Some(root.clone()),
        presentation: Presentation::Html,
        fold: Default::default(),
        scratch: scratch.clone(),
    })?);
    let idx = Arc::new(index::Index::new(root.clone(), only));

    let rail = RAIL_TEMPLATE.replace("{{VERSION}}", env!("CARGO_PKG_VERSION"));
    let handler = {
        let service = service.clone();
        let idx = idx.clone();
        let rail = rail.clone();
        let scratch = scratch.clone();
        Arc::new(move |name: &str, query: &str| -> HttpResponse {
            match name {
                "" | "index.html" => HttpResponse::html(rail.clone()),
                "api/sessions" => {
                    let service = &service;
                    HttpResponse::json(idx.sessions_json(|path| {
                        service.register_root(path);
                    }))
                }
                // #113: toggle a hide key (`s:<sid>` / `p:<cwd>` / `a:<label>`). A GET with a
                // query param, because the loopback listener only parses the request line —
                // no method, no body (serve.rs). Persisting a local hide preference at the
                // monitor's own root is UI state, not agent/terminal control, so it stays
                // inside the read-only contract (R8).
                "api/ignore" => {
                    let resp = match (query_get(query, "add"), query_get(query, "remove")) {
                        (Some(k), _) => idx.set_ignore(&index::percent_decode(k), true),
                        (_, Some(k)) => idx.set_ignore(&index::percent_decode(k), false),
                        _ => r#"{"ok":false}"#.to_string(),
                    };
                    HttpResponse::json(resp)
                }
                // The monitor ONLY ever serves the view EMBEDDED. The view navigates
                // sub-agents with a relative `?session=<child>` href that drops the
                // `chrome=embed` param, so default it back here — a drilled-in child keeps
                // embed chrome instead of flashing the full claude-replay brand (#124).
                "session" if query_get(query, "chrome").is_none() => {
                    let q = if query.is_empty() {
                        "chrome=embed".to_string()
                    } else {
                        format!("{query}&chrome=embed")
                    };
                    service_routes(Some(&service), &scratch, name, &q)
                }
                // Everything else is the session service's own wire surface —
                // /session, /pull, /records, /__reveal, static assets (§6.3).
                _ => service_routes(Some(&service), &scratch, name, query),
            }
        })
    };

    let bound = spawn_listener(port, handler)
        .with_context(|| format!("bind 127.0.0.1:{port} (is another monitor running?)"))?;
    service.set_port(bound);
    // Now the root lock can say where we serve, so the next invocation's refusal names a URL
    // instead of just a pid (#160). Only reachable once bound — the same reason the html
    // server publishes its per-session notes late.
    let _ = claude_replay_present::cache::lock::publish(&root, serde_json::json!({"port": bound}));
    let url = format!("http://127.0.0.1:{bound}/");
    eprintln!("claude-monitor serving {url} (loopback only — Ctrl-C to stop)");
    println!("{url}");
    if open_browser {
        open_url(&url);
    }
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_present::cache::lock;

    /// #166: being second is not an error — it means what you asked for is already running.
    ///
    /// Covers all four ways the root lock can read: free, held by a live monitor that published
    /// where it serves (hand off), held by one that has taken the lock but not bound (no target,
    /// so this one really is an error), and held by a dead pid (reclaimed).
    #[test]
    fn a_second_monitor_hands_off_instead_of_failing() {
        let root = std::env::temp_dir().join(format!("cm-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        assert!(
            matches!(claim_root(&root).unwrap(), Claimed::Ours),
            "a free root is ours"
        );
        // Our own lock does not lock us out (`acquire` never denies its own pid).
        assert!(matches!(claim_root(&root).unwrap(), Claimed::Ours));

        // A peer that really exists: a live pid that is not ours, on a port that really answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut peer = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let peer_pid = peer.id();
        let hold = |note: serde_json::Value| {
            std::fs::write(
                lock::lock_path(&root),
                serde_json::json!({"pid": peer_pid, "dir": root, "note": note}).to_string(),
            )
            .unwrap();
        };

        hold(serde_json::json!({ "port": port }));
        match claim_root(&root).unwrap() {
            Claimed::Served(url) => assert_eq!(url, format!("http://127.0.0.1:{port}/")),
            Claimed::Ours => panic!("a live monitor's root must not be taken from it"),
        }

        // It holds the lock but has not bound: a real window, and no URL to send anyone to.
        hold(serde_json::Value::Null);
        let e = claim_root(&root).expect_err("no port ⇒ nowhere to hand off to");
        assert!(
            e.to_string().contains(&peer_pid.to_string()),
            "the refusal names the holder: {e}"
        );

        // A dead holder is reclaimed, whoever holds that port now.
        peer.kill().ok();
        peer.wait().ok();
        hold(serde_json::json!({ "port": port }));
        assert!(
            matches!(claim_root(&root).unwrap(), Claimed::Ours),
            "a dead monitor does not keep the root"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
