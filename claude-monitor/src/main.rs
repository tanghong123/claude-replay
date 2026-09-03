//! `agent-monitor` — every agent session on this machine, one page, over loopback HTTP
//! (#98). The Codex-style app shell is the default frontend over the existing `/pull` +
//! `/records` protocol. The rail-and-iframe page is the SECOND SUPPORTED shell, not a rollback
//! seam: `ui::preference` remembers which one you want, each carries a button to the other, and
//! `?ui=` overrides for one request. It goes when the app shell has been validated.
//!
//! Read-only, loopback only (§11). No fold on the index path (R7), no background sweep
//! (§3): a session's durable entry is written by VISITING it, and the rail's counters read
//! that entry's meta stream lock-free.

use claude_monitor::control::{
    ensure_token, read_token, set_passcode_interactive, tokened_url, Attempts,
};
use claude_monitor::{control, index};

use anyhow::{Context, Result};
use claude_replay_core::Agent;
use claude_replay_html::{query_get, service_routes, HttpResponse, RootLock, ServiceConfig};
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
            "agent-monitor is starting up in another process (pid {}) — it holds this cache \
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

fn classic_ui_requested(query: &str) -> bool {
    claude_monitor::ui::resolve(query) == claude_monitor::ui::Shell::Classic
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

/// `--help`. Two columns for the flags, one paragraph per topic, and every line inside 80
/// so it reads in a default terminal instead of wrapping into a wall — the shape
/// `agent-monitor-fleet` already uses. The literal is flush-left on purpose: indenting it to
/// match the code would put that indentation in the output.
fn help_text() -> String {
    format!(
        "\
agent-monitor — every agent session on this machine, one page over loopback HTTP

USAGE:
  agent-monitor [--pair] [--port N] [--agents LIST] [--no-open]
  agent-monitor --set-passcode
  agent-monitor --version

  --pair            Require a token to reach the monitor — a 0600 secret, minted
                    once. Run it on a SHARED machine; it prints the URL to open,
                    and a plain `agent-monitor` then keeps requiring that token.
  --port N          Serve on N instead of {DEFAULT_PORT}.
  --agents LIST     Only these agents: claude, codex, qoder, qoderwork.
  --no-open         Print the URL instead of opening a browser.
  --set-passcode    Set (or clear) the passcode that granting injection into a
                    live session requires — a speed bump for an unlocked,
                    unattended machine. Terminal-only on purpose, so an open
                    browser cannot lift the gate meant to stop it.

Serves http://127.0.0.1:{DEFAULT_PORT} — loopback only, read-only.

TWO INTERFACES, both supported:
  the app shell (default) and the classic rail-and-iframe page. Each carries a
  button that switches to the other and REMEMBERS the choice (stored in
  <state_dir>/ui.json, shared with agent-monitor-v2). `?ui=classic` / `?ui=app`
  on the URL override for one request without changing what is stored, which is
  how you compare them. The classic page stays until the app shell is validated.

CACHE ROOT:
  $AGENT_MONITOR_CACHE, else $CLAUDE_MONITOR_CACHE (legacy, still honored). With
  neither set, an existing ~/.cache/claude-monitor keeps being used and a fresh
  install creates ~/.cache/agent-monitor. Never the viewer's own root.

PROCESS RECOGNITION:
  Built-in basenames: claude, codex, qoderwork, qoder. Extend that set with
  $AGENT_MONITOR_AGENT_PATTERNS, comma-separated:

    basename:<name>     match the executable's own name
    argv:<substring>    match anywhere in the command line (wrapper launches)
    <name>              bare, the same as basename:<name>

  e.g. AGENT_MONITOR_AGENT_PATTERNS=\"argv:npx codex,basename:my-agent\"
"
    )
}

fn parse_agent_name(name: &str) -> Result<Agent> {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude" => Ok(Agent::CLAUDE),
        "codex" => Ok(Agent::CODEX),
        "qoder" => Ok(Agent::QODER),
        "qoderwork" => Ok(Agent::QODERWORK),
        other => anyhow::bail!("unknown agent {other:?}"),
    }
}

fn main() -> Result<()> {
    let mut port = DEFAULT_PORT;
    let mut only: Vec<Agent> = Vec::new();
    let mut open_browser = true;
    let mut do_pair = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            // `--pair` (§4.2): mint the 0600 token if absent, then run enforcing it — the
            // shared-Mac gate. A FLAG, to match this CLI's `--port`/`--agents`/`--no-open`
            // shape: pairing modifies the run (it keeps serving), it is not a separate
            // command that acts and exits. `pair` is accepted as a friendly alias.
            "--pair" | "pair" => do_pair = true,
            // Set (or clear) the injection passcode and EXIT — the grant gate for #133. A
            // terminal-only action ON PURPOSE: setting it needs shell access, so an open
            // browser cannot reset the gate it is meant to defend against.
            "--set-passcode" | "set-passcode" => return set_passcode_interactive(),
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
                    only.push(parse_agent_name(name)?);
                }
            }
            "--no-open" => open_browser = false,
            "--help" | "-h" => {
                print!("{}", help_text());
                return Ok(());
            }
            // Same shape as `agent-monitor-fleet`'s. Missing until now, which made the two
            // siblings disagree about something every CLI is expected to answer.
            "--version" | "-V" => {
                println!("agent-monitor {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!("unknown flag {other:?} (try --help)"),
        }
    }

    let root = index::default_root()?;
    // `pair` mints the token before anything else, so both a fresh start and a hand-off to
    // an already-running monitor below can print the tokened URL.
    if do_pair {
        ensure_token(&root)?;
    }
    let token = read_token(&root);
    let with_token = |base: &str| tokened_url(base, token.as_deref());
    // Before anything is opened: one monitor per root (#160) — and if another one has it, that
    // is where the user wants to go (#166), so open it and stop rather than fail. The hand-off
    // URL carries the token too: the second invocation is the same user, who can read the file.
    if let Claimed::Served(url) = claim_root(&root)? {
        // The bare URL, deliberately: which shell `/` serves is the REMEMBERED preference
        // (`ui::resolve`), and the running monitor is the one that answers it. Pinning a `ui=`
        // here would override the user's own choice on every hand-off.
        let url = with_token(&url);
        eprintln!("agent-monitor is already running — opening {url}");
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
    // `claim_root` above established this process as the root's ONLY writer, so the
    // service takes no per-entry locks at all (#167 §4.3 c) — `SingleWriter` under the
    // monitor's claim, not `PerSession` re-proving exclusivity entry by entry.
    let service = Arc::new(claude_replay_html::SessionService::new(ServiceConfig {
        cache_root: Some(root.clone()),
        presentation: Presentation::Html,
        fold: Default::default(),
        scratch: scratch.clone(),
        root_lock: RootLock::SingleWriter,
    })?);
    let idx = Arc::new(index::Index::new(root.clone(), only));

    let rail = RAIL_TEMPLATE
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
        // #133 3b: the compose affordance only exists when paired — an unpaired monitor
        // has no write capability (the route 401s), so the UI must not offer it.
        .replace("{{PAIRED}}", if token.is_some() { "true" } else { "false" });
    // The passcode lockout counter, owned by the handler (single-user → one global counter).
    let attempts = std::sync::Mutex::new(Attempts::default());
    let paired = token.is_some();
    let handler = {
        let service = service.clone();
        let idx = idx.clone();
        let rail = rail.clone();
        let scratch = scratch.clone();
        Arc::new(move |req: &claude_replay_html::Request| -> HttpResponse {
            let (name, query) = (req.name, req.query);
            if let Some(asset) = claude_monitor::ui::asset(name) {
                return asset;
            }
            match name {
                "" | "index.html" => {
                    if classic_ui_requested(query) {
                        HttpResponse::html(rail.clone())
                    } else {
                        HttpResponse::html(claude_monitor::ui::app_page(
                            env!("CARGO_PKG_VERSION"),
                            paired,
                        ))
                    }
                }
                // Read or set which shell `/` serves. Both are supported while the app shell
                // is validated; the toggle in each shell's header calls this and reloads.
                "api/ui" => claude_monitor::ui::route(query),
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
                // #133 idle slice: send a prompt to a FINISHED session by resuming it.
                // A WRITE — gated by `deny_write` (POST + same-origin + a token, so a stock
                // unpaired binary cannot reach it) — then by the idle/project-inactive rules
                // in `resolve_send`. The resume is an autonomous agent turn (skip-permissions,
                // owner-authorized). Body is the raw prompt; `?target=<sid>` is the session.
                "api/send" => control::send_route(&idx, req),
                // #133 tmux slice: grant/revoke consent to inject into a live session's pane.
                // A WRITE (deny_write-gated). `?op=revoke` clears a session's grants (always
                // allowed — removes stale consent); otherwise a grant requires the session to
                // resolve as a PROVEN live tmux link (`resolve_tmux_send`), so consent can only
                // be granted for a target that could actually be injected.
                "api/consent" => control::consent_route(&idx, req, &attempts),
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
                    service_routes(
                        Some(&service),
                        &scratch,
                        &claude_replay_html::Request { query: &q, ..*req },
                    )
                }
                // Everything else is the session service's own wire surface —
                // /session, /pull, /records, /__reveal, static assets (§6.3).
                _ => service_routes(Some(&service), &scratch, req),
            }
        })
    };

    // #196 §4.2: paired ⇒ the token gate (same-user OR the token); unpaired ⇒ D3b same-user.
    let gate = match &token {
        Some(t) => claude_replay_html::AuthGate::with_token(t.as_str()),
        None => claude_replay_html::AuthGate::same_user(),
    };
    let bound = claude_replay_html::spawn_listener_gated(port, handler, gate)
        .with_context(|| format!("bind 127.0.0.1:{port} (is another monitor running?)"))?;
    service.set_port(bound);
    // Now the root lock can say where we serve, so the next invocation's refusal names a URL
    // instead of just a pid (#160). Only reachable once bound — the same reason the html
    // server publishes its per-session notes late.
    let _ = claude_replay_present::cache::lock::publish(&root, serde_json::json!({"port": bound}));
    let base = format!("http://127.0.0.1:{bound}/");
    let url = with_token(&base);
    if token.is_some() {
        eprintln!("agent-monitor serving {url} (paired — token required · Ctrl-C to stop)");
    } else {
        eprintln!("agent-monitor serving {url} (loopback only — Ctrl-C to stop)");
        // The silent-hole warning (§4.2): unpaired + a platform that cannot verify a TCP
        // peer's uid = every local user can reach this monitor (and `/__reveal` pops Finder
        // on the server). Harmless on a personal Mac; a hole on a shared one.
        if cfg!(not(target_os = "linux")) {
            eprintln!(
                "  note: loopback peers can't be verified on this platform — if this machine \
                 is SHARED, run `agent-monitor --pair` to require a token."
            );
        }
    }
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

    #[test]
    fn app_shell_is_the_default_and_classic_stays_reachable() {
        let app = claude_monitor::ui::page("test", false, true);
        assert!(app.contains("/monitor-ui/app.js"));
        assert!(app.contains("data-ui-default=\"true\""));
        assert!(claude_monitor::ui::asset("monitor-ui/record-store.js").is_some());
        assert!(claude_monitor::ui::asset("monitor-ui/components.js").is_some());
        assert!(super::RAIL_TEMPLATE.contains("requestedSession"));
        assert!(super::RAIL_TEMPLATE.contains("URLSearchParams(location.search)"));
        assert!(super::help_text().contains("?ui=classic"));
        assert!(!super::classic_ui_requested("ui=codex"));
        assert!(!super::classic_ui_requested("ui=app"));
        assert!(super::classic_ui_requested("ui=classic"));
    }

    #[test]
    fn qoder_and_qoderwork_are_distinct_agent_filters() {
        assert_eq!(super::parse_agent_name("qoder").unwrap(), Agent::QODER);
        assert_eq!(
            super::parse_agent_name("qoderwork").unwrap(),
            Agent::QODERWORK
        );
        assert_ne!(Agent::QODER, Agent::QODERWORK);
    }

    /// `--help` has to READ in a default terminal. It used to be one 103-column USAGE line and
    /// four prose paragraphs of flag descriptions, which wrapped into a wall; the shape is now
    /// two columns and one paragraph per topic. Keeping every line inside 80 is the part a
    /// change can silently undo, so it is the part asserted.
    #[test]
    fn the_help_screen_fits_a_terminal() {
        let help = super::help_text();
        for (n, line) in help.lines().enumerate() {
            assert!(
                line.chars().count() <= 80,
                "help line {} is {} columns:\n{line}",
                n + 1,
                line.chars().count()
            );
            assert_eq!(
                line.trim_end(),
                line,
                "help line {} has trailing space",
                n + 1
            );
        }
        // Every flag the argument loop accepts is documented — the drift that makes a help
        // screen wrong rather than merely ugly.
        for flag in [
            "--pair",
            "--port",
            "--agents",
            "--no-open",
            "--set-passcode",
            "--version",
        ] {
            assert!(help.contains(flag), "{flag} is undocumented");
        }
        assert!(
            help.contains(&super::DEFAULT_PORT.to_string()),
            "the default port is stated, and comes from the constant"
        );
    }
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
