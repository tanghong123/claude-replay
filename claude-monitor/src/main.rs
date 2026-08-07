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
use claude_replay_html::{service_routes, spawn_listener, HttpResponse, ServiceConfig};
use claude_replay_present::cache::Presentation;
use std::sync::Arc;

/// The stable default port (§11): the monitor is a bookmarkable place.
const DEFAULT_PORT: u16 = 2727;

/// The rail page — self-contained, its own markup and script (§6.3).
const RAIL: &str = include_str!("rail.html");

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
    // The session service at the MONITOR's root (§3/§10): same presentation namespace,
    // different root — a running `claude-replay --html` and this server cannot contend.
    let service = Arc::new(claude_replay_html::SessionService::new(ServiceConfig {
        cache_root: Some(root.clone()),
        presentation: Presentation::Html,
        fold: Default::default(),
        scratch: std::env::temp_dir().join("claude-monitor"),
    })?);
    let idx = Arc::new(index::Index::new(root, only));

    let handler = {
        let service = service.clone();
        let idx = idx.clone();
        let scratch = std::env::temp_dir().join("claude-monitor");
        Arc::new(move |name: &str, query: &str| -> HttpResponse {
            match name {
                "" | "index.html" => HttpResponse::html(RAIL.to_string()),
                "api/sessions" => {
                    let service = &service;
                    HttpResponse::json(idx.sessions_json(|path| {
                        service.register_root(path);
                    }))
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
