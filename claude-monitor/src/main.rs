//! `claude-monitor` — every agent session on this machine, one page, over loopback HTTP
//! (#98). The page is the RAIL (this crate's own markup) beside the existing claude-replay
//! session view in an `<iframe src="/session?id=…">` — composition at the document level
//! (§6.3), never a fork of the renderer (R10).
//!
//! Read-only, loopback only (§11). No fold on the index path (R7), no background sweep
//! (§3): a session's durable entry is written by VISITING it, and the rail's counters read
//! that entry's meta stream lock-free.

mod index;

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

/// Drop the scratch of monitor runs that are no longer alive (#157).
///
/// Per-run scratch fixes the sharing, but nothing would ever collect a dead run's copy: a
/// crashed monitor's gigabytes would sit in `$TMPDIR` until the OS cleaned it, which on macOS
/// means days. The name of each run dir IS its pid, so liveness is the whole decision, and
/// [`claude_replay_present::cache::lock::pid_alive`] is the audited answer the cache already
/// trusts for exactly this question. Where liveness cannot be decided (non-Unix) nothing is
/// swept — deleting a live run's log would be far worse than keeping a dead one's.
fn sweep_dead_runs(runs: &std::path::Path) {
    if !claude_replay_present::cache::lock::liveness_decidable() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(runs) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        // Pre-#157 layout: shared `<id>.records` logs directly under the runs dir, the ones
        // that reached gigabytes. No version reads them any more, and unlinking is safe even
        // if an old binary is still running — its open fd keeps the inode alive until it exits.
        if name.ends_with(".records") {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid != std::process::id() && !claude_replay_present::cache::lock::pid_alive(pid) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
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
                     never the viewer's (R5)."
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown flag {other:?} (try --help)"),
        }
    }

    let root = index::default_root()?;
    // Scratch is RUN-scoped and wiped on the way in, the same contract `--html` gives its own
    // bundle dir (html_export::serve::start_server) and for the same two reasons (#157).
    //
    // It was a fixed `$TMPDIR/claude-monitor`, so every monitor this machine has ever run
    // shared one record log per session and nothing ever reset it. The store appends and
    // deliberately never truncates in its constructor (#96 — a durable log must survive to be
    // resumed), and a session that falls off the 30s tail TTL is re-materialized by re-folding,
    // which appends a COMPLETE re-render. Measured: +29 MB per reap cycle, a log born at 03:56
    // that reached 14 GB by 18:00 for one 29 MB session, and two live monitors appending to it
    // at once. Per-pid, wiped at startup: a run cannot inherit another's log or write into a
    // peer's, and a run's growth ends with the run.
    let runs = std::env::temp_dir().join("claude-monitor");
    let scratch = runs.join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&scratch);
    sweep_dead_runs(&runs);
    // The session service at the MONITOR's root (§3/§10): same presentation namespace,
    // different root — a running `claude-replay --html` and this server cannot contend.
    let service = Arc::new(claude_replay_html::SessionService::new(ServiceConfig {
        cache_root: Some(root.clone()),
        presentation: Presentation::Html,
        fold: Default::default(),
        scratch: scratch.clone(),
    })?);
    let idx = Arc::new(index::Index::new(root, only));

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
    let url = format!("http://127.0.0.1:{bound}/");
    eprintln!("claude-monitor serving {url} (loopback only — Ctrl-C to stop)");
    println!("{url}");
    if open_browser {
        #[cfg(target_os = "macos")]
        let prog = "open";
        #[cfg(target_os = "windows")]
        let prog = "explorer";
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let prog = "xdg-open";
        let _ = std::process::Command::new(prog)
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    loop {
        std::thread::park();
    }
}
