//! `agent-monitor-v2` — every agent session on this machine in ONE document.
//!
//! v1 (`agent-monitor`) composes at the document level: its rail owns the page and the
//! session view lives in an `<iframe src="/session?id=…">`. That frame is doing two jobs —
//! layout, and lifecycle (a session switch is `view.src = …`, which disposes of the view's
//! whole JS context for free). v2 drops it for one shell, and pays for the second job with a
//! full navigation per switch (the reload-first decision); a native `init`/`destroy` for the
//! view's module state is deferred until the reload proves too coarse.
//!
//! **Nothing here modifies v1 or the renderer.** The session page is fetched from the same
//! public backend (`SessionService::page`) and this crate's shell is spliced into it; every
//! other route is delegated to `service_routes` verbatim. The two monitors run side by side
//! on different ports and different cache roots, so v2 can be built against real sessions
//! without touching a working tool.
//!
//! Read-only and loopback-only, exactly like v1.

use anyhow::{Context, Result};
use claude_replay_core::{discover, Agent};
use claude_replay_html::{
    query_get, service_routes, HttpResponse, PageChrome, RootLock, ServiceConfig, SessionService,
};
use claude_replay_present::cache::Presentation;
use serde_json::json;
use std::sync::Arc;

/// v2's own port. v1 keeps 2727 — both must be runnable at once, or v2 could not be
/// developed against the sessions a person is actually working in.
const DEFAULT_PORT: u16 = 2828;

/// The shell fragment: this crate's markup, styles and script, injected INTO the session
/// document. `{{VERSION}}` is the only substitution.
const SHELL: &str = include_str!("shell.html");

/// Splice the shell into a rendered session page.
///
/// A string insertion before `</body>`, and deliberately nothing cleverer: the seam is one
/// tag the renderer has always emitted, v2 owns the risk, and the alternative — teaching the
/// html crate about rails — would coupling a shared library to one frontend's chrome.
/// Position in the DOM does not matter because the rail is `position: fixed`.
fn splice(page: &str, shell: &str) -> String {
    match page.rfind("</body>") {
        Some(i) => format!("{}{}{}", &page[..i], shell, &page[i..]),
        None => format!("{page}{shell}"),
    }
}

/// How recent a transcript's last write has to be for the rail to call it live.
///
/// v1 PROVES liveness by scanning processes and matching an agent to a session; that machinery
/// is private to it, and this app must not reach into it. Recency is the honest approximation
/// available from the public surface, so v2 says "growing/idle/finished" from mtime and does not
/// claim a proven process. Replacing this with a real liveness probe is its own piece of work.
const GROWING_SECS: f64 = 90.0;
const IDLE_SECS: f64 = 30.0 * 60.0;

/// "3m" / "2h" / "4d" — the compact age the rail shows beside a row.
fn human_age(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 90.0 {
        format!("{}s", s as u64)
    } else if s < 90.0 * 60.0 {
        format!("{}m", (s / 60.0).round() as u64)
    } else if s < 36.0 * 3600.0 {
        format!("{}h", (s / 3600.0).round() as u64)
    } else {
        format!("{}d", (s / 86400.0).round() as u64)
    }
}

/// One project's worth of rows in the rail, while the list is being assembled.
struct Group {
    /// The project path — the grouping key AND the row's secondary line.
    key: String,
    label: String,
    secondary: String,
    rows: Vec<serde_json::Value>,
    /// Newest activity in the group; the sort key, so the project you touched last is on top.
    latest: f64,
    growing: usize,
    idle: usize,
}

/// The rail's session list, grouped by project — v1's shape, built entirely on the discovery
/// facade (`store_all` + `project_path` + `session_card` + `session_id`), which is the same API
/// the developer guide documents for third parties. No access to v1's private index is needed
/// or taken.
fn sessions_json(service: &SessionService, only: Option<Agent>, limit: usize) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let mut groups: Vec<Group> = Vec::new();
    for e in discover::store_all(only).into_iter().take(limit) {
        let Some(id) = discover::session_id(&e.path) else {
            continue;
        };
        // Registering here is what makes `?session=<id>` resolvable: the shell renders by id,
        // and the service only knows the ids it has been shown. Idempotent for a known one.
        service.register_root(&e.path);
        let card = discover::session_card(e.agent, &e.path);
        let name = card
            .as_ref()
            .and_then(|c| c.title.clone())
            .or_else(|| card.as_ref().and_then(|c| c.last_prompt.clone()))
            .unwrap_or_else(|| id.clone());
        let proj = discover::project_path(&e.path);
        let key = proj
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| e.agent.label().to_string());
        let label = proj
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| e.agent.label().to_string());
        let age = now - e.mtime;
        let state = if age < GROWING_SECS {
            "growing"
        } else if age < IDLE_SECS {
            "idle"
        } else {
            "finished"
        };
        let row = json!({
            "id": id,
            "name": name,
            "agent": e.agent.label(),
            "state": state,
            "activity": human_age(age),
        });
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => {
                g.rows.push(row);
                g.latest = g.latest.max(e.mtime);
                g.growing += usize::from(state == "growing");
                g.idle += usize::from(state == "idle");
            }
            None => groups.push(Group {
                key: key.clone(),
                label,
                secondary: key,
                rows: vec![row],
                latest: e.mtime,
                growing: usize::from(state == "growing"),
                idle: usize::from(state == "idle"),
            }),
        }
    }
    groups.sort_by(|a, b| b.latest.total_cmp(&a.latest));
    let out: Vec<serde_json::Value> = groups
        .into_iter()
        .map(|g| {
            json!({
                "label": g.label,
                "secondary": g.secondary,
                "total": g.rows.len(),
                "growing": g.growing,
                "idle": g.idle,
                "rows": g.rows,
            })
        })
        .collect();
    json!({ "groups": out }).to_string()
}

fn main() -> Result<()> {
    let mut port = DEFAULT_PORT;
    let mut only: Option<Agent> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                port = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .context("--port needs a number")?
            }
            "--agents" => {
                let v = args.next().context("--agents needs an agent")?;
                only = Some(match v.trim().to_ascii_lowercase().as_str() {
                    "claude" => Agent::CLAUDE,
                    "codex" => Agent::CODEX,
                    "qoderwork" | "qoder" => Agent::QODERWORK,
                    other => anyhow::bail!("unknown agent {other:?}"),
                });
            }
            "--help" | "-h" => {
                print!("{}", help_text());
                return Ok(());
            }
            "--version" | "-V" => {
                println!("agent-monitor-v2 {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => anyhow::bail!("unknown flag {other:?} (try --help)"),
        }
    }

    // v2's OWN root. Sharing v1's would contend for its single-writer lock, and the point of
    // a separate app is that both can run.
    let root = dirs_cache().join("agent-monitor-v2");
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let scratch = root.join("runs");
    std::fs::create_dir_all(&scratch)?;

    let service = Arc::new(SessionService::new(ServiceConfig {
        cache_root: Some(root.clone()),
        presentation: Presentation::Html,
        fold: Default::default(),
        scratch: scratch.clone(),
        root_lock: RootLock::SingleWriter,
    })?);

    let shell = SHELL.replace("{{VERSION}}", env!("CARGO_PKG_VERSION"));
    let handler = {
        let service = service.clone();
        let shell = shell.clone();
        let scratch = scratch.clone();
        Arc::new(move |req: &claude_replay_html::Request| -> HttpResponse {
            let (name, query) = (req.name, req.query);
            match name {
                // The shell. With a session, it IS that session's page with the rail spliced
                // in — one document, one scroller, no frame.
                "" | "index.html" => {
                    let id = query_get(query, "session").unwrap_or("");
                    if id.is_empty() {
                        return HttpResponse::html(empty_shell(&shell));
                    }
                    let chrome = PageChrome {
                        embed: true,
                        theme: None,
                    };
                    // A deep link can arrive before any list fetch, and the service only
                    // knows ids it has been shown — so on a miss, look the id up on disk and
                    // register it before giving up.
                    let page = service.page(id, Some(&chrome)).or_else(|| {
                        let e = discover::store_all(only)
                            .into_iter()
                            .find(|e| discover::session_id(&e.path).as_deref() == Some(id))?;
                        service.register_root(&e.path);
                        service.page(id, Some(&chrome))
                    });
                    match page {
                        Some(page) => HttpResponse::html(splice(&page, &shell)),
                        None => HttpResponse::html(empty_shell(&shell)),
                    }
                }
                "api/sessions" => HttpResponse::json(sessions_json(&service, only, 400)),
                // Everything else — /pull, /records, /session, /__reveal, assets — is the
                // shared backend, unchanged.
                _ => service_routes(Some(&service), &scratch, name, query),
            }
        })
    };

    let bound = claude_replay_html::spawn_listener(port, handler)
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let url = format!("http://127.0.0.1:{bound}/");
    eprintln!("agent-monitor-v2 serving {url} (loopback only · Ctrl-C to stop)");
    println!("{url}");
    // The listener runs on its own thread; park this one so the process stays up.
    loop {
        std::thread::park();
    }
}

/// The no-session page: the rail alone, in a document of its own shape. Deliberately the same
/// fragment, so there is one shell and not two.
fn empty_shell(shell: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>agent-monitor v2</title>\n\
         <style>:root{{--bg:#141414;--fg:#e6e6e6;--faint:#7a7a7a;--border:#2a2a2a;--panel:#1c1c1c;\
         --tool:#6d4fa1;--sans:system-ui,sans-serif;--mono:ui-monospace,monospace}}\
         body{{margin:0;background:var(--bg);color:var(--fg)}}</style>\n</head>\n<body>\n\
         {shell}\n<div style=\"margin-left:300px;padding:64px 24px;color:#7a7a7a;\
         font:13px system-ui,sans-serif\">Pick a session from the rail.</div>\n</body>\n</html>\n"
    )
}

fn dirs_cache() -> std::path::PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
}

fn help_text() -> String {
    format!(
        "\
agent-monitor-v2 — every agent session on this machine, in one app-shell

USAGE:
  agent-monitor-v2 [--port N] [--agents claude|codex|qoderwork]
  agent-monitor-v2 --version

  --port N          Serve on N instead of {DEFAULT_PORT}.
  --agents AGENT    Only this agent's sessions.

Serves http://127.0.0.1:{DEFAULT_PORT} — loopback only, read-only.

v1 (`agent-monitor`, port 2727) is untouched and can run at the same time: this app has its
own port and its own cache root (~/.cache/agent-monitor-v2), and shares only the backend.
"
    )
}
