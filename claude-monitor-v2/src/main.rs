//! `agent-monitor-v2` — every agent session on this machine in ONE document.
//!
//! The default frontend is the Codex-style app shell: it consumes the existing structured
//! `/pull` + `/records` projection and renders it through its own component registry.  The
//! splice-based v2 frontend is the second supported shell, reached at `?ui=classic` or by the
//! switch in each shell's header, which remembers the choice (`claude_monitor::ui`).
//!
//! **This is a front-end, not a second implementation.** The session page comes from the same
//! public backend (`SessionService::page`) with this crate's shell spliced in, and every other
//! route is delegated to `service_routes` verbatim. The session INDEX and the whole control
//! plane come from `claude_monitor`'s library half — the same `Index`, the same proven
//! session→pane attribution, the same consent store, the same pairing token. A security
//! surface with two implementations has two behaviours, and "may this prompt be injected into
//! that pane" is the last question in this repo that should be answered twice.
//!
//! The two monitors run side by side on different ports and different cache roots, so v2 can
//! be built against real sessions without touching a working tool. Loopback-only, exactly like
//! v1; the write routes need pairing, exactly like v1.

use anyhow::{Context, Result};
use claude_monitor::control::{
    ensure_token, read_token, set_passcode_interactive, tokened_url, Attempts,
};
use claude_monitor::index;
use claude_replay_core::{discover, Agent};
use claude_replay_html::{
    query_get, AuthGate, HttpResponse, PageChrome, RootLock, ServiceConfig, SessionService,
};
use claude_replay_present::cache::Presentation;
use std::sync::Arc;

/// v2's own port. v1 keeps 2727 — both must be runnable at once, or v2 could not be
/// developed against the sessions a person is actually working in.
const DEFAULT_PORT: u16 = 2828;

/// The splice frontend — the second supported shell, served at `?ui=classic` or when that is
/// the remembered preference.
const CLASSIC_SHELL: &str = include_str!("shell.html");

/// The rail's width, in CSS pixels — ONE number, used twice: it sizes `#v2rail` in the
/// shell's own stylesheet, and it is the inset v2 asks the session page to make for it
/// (`PageChrome::host_inset`). Keeping it here rather than in both stylesheets is the point:
/// the page's offsets and the rail they clear can no longer drift apart.
const RAIL_W: u32 = 326;

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

/// The id everything here is keyed by: the transcript's FILE STEM.
///
/// Not `discover::session_id`, which reads the id the agent recorded INSIDE the file. The two
/// agree for Claude, whose transcript is named for its session, and diverge for Codex, whose
/// rollout is `rollout-<timestamp>-<uuid>.jsonl`. The serving layer keys its roots and its
/// durable entries by the stem, so a rail that offered the in-band id handed out links the
/// service had never heard of: every Codex session 404'd on open and showed no counters.
/// One id, chosen to be the one the backend already uses.
fn stem_of(path: &std::path::Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
}

fn main() -> Result<()> {
    let mut port = DEFAULT_PORT;
    let mut only: Option<Agent> = None;
    let mut do_pair = false;
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
                // `qoder` is Qoder CLI and `qoderwork` is QoderWork — two agents with two
                // stores, as `Agent::from_str` has always had it. Aliasing `qoder` onto
                // QODERWORK here silently selected the wrong store.
                only = Some(match v.trim().to_ascii_lowercase().as_str() {
                    "claude" => Agent::CLAUDE,
                    "codex" => Agent::CODEX,
                    "qoder" => Agent::QODER,
                    "qoderwork" => Agent::QODERWORK,
                    other => anyhow::bail!("unknown agent {other:?}"),
                });
            }
            // Pairing, shared with v1 (#196 §4.2): the SAME 0600 token file, because the
            // `cmauth` cookie is scoped to `127.0.0.1` and not to a port — two tokens would
            // mean whichever page loaded last clobbers the other's cookie, and the other's
            // writes start 401ing. One token, one consent store, one passcode: one machine.
            "--pair" | "pair" => do_pair = true,
            // Terminal-only, like v1's: setting the grant passcode needs shell access, so an
            // open browser cannot reset the gate that exists to stop it.
            "--set-passcode" | "set-passcode" => return set_passcode_interactive(),
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
    // THE index — v1's, from its library half. Its own cache root (so the two monitors keep
    // separate durable entries and separate hide lists), but the same scan, the same proven
    // session→process attribution and the same send decisions. This is what makes v2's compose
    // affordance mean exactly what v1's means.
    let idx = Arc::new(index::Index::new(
        root.clone(),
        only.into_iter().collect::<Vec<_>>(),
    ));

    // `--pair` mints the shared token; a plain run READS it, so pairing v1 pairs v2 and the
    // write routes light up in both.
    if do_pair {
        ensure_token(&root)?;
    }
    let token = read_token(&root);
    let classic_shell = CLASSIC_SHELL
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
        // Seam 0 (#43): the shared frontend modules inlined ahead of the shell's script, as
        // v1's rail and the html crate's pages carry them.
        .replace("{{SHARED}}", &claude_replay_html::shared_inline_all())
        .replace("{{RAIL_W}}", &RAIL_W.to_string())
        // The compose affordance exists only when paired: unpaired, every write route 401s,
        // so offering the button would be offering a dead end (v1's `{{PAIRED}}` rule).
        .replace("{{PAIRED}}", if token.is_some() { "true" } else { "false" });
    let paired = token.is_some();
    // The route table is the library's (claude_monitor::routes, #47) — the same five API
    // arms, assets and service fallthrough as v1; this binary contributes only its classic
    // page: the splice shell with the session spliced in.
    let backend = Arc::new(claude_monitor::routes::Backend {
        service: service.clone(),
        idx: idx.clone(),
        scratch: scratch.clone(),
        // The passcode lockout counter, owned by the handler — one per process, one user.
        attempts: std::sync::Mutex::new(Attempts::default()),
    });
    let handler = claude_monitor::routes::handler(
        backend,
        claude_monitor::routes::Frontend {
            version: env!("CARGO_PKG_VERSION"),
            paired,
            classic: {
                let classic_shell = classic_shell.clone();
                let service = service.clone();
                Arc::new(move |query: &str| {
                    // Rollback frontend: with a session it IS the legacy session page with
                    // the old rail spliced in.  Its routes and data are still the same ones
                    // the new shell consumes.
                    let id = query_get(query, "session").unwrap_or("");
                    if id.is_empty() {
                        return HttpResponse::html(empty_shell(&classic_shell));
                    }
                    // `artifacts`: v2 serves clicked file paths through the browser
                    // (goal 3) rather than opening Finder on the server.
                    let chrome = PageChrome {
                        embed: true,
                        theme: None,
                        artifacts: true,
                        // `host_search`: the rail's box is the only one (goal 2). It drives
                        // the page's search by writing into `#q`, which stays where it is.
                        host_search: true,
                        // The rail is `position: fixed` over the left edge; the page makes
                        // room for it itself rather than this shell reaching into its CSS.
                        host_inset: Some(RAIL_W),
                    };
                    // A deep link can arrive before any list fetch, and the service only
                    // knows ids it has been shown — so on a miss, look the id up on disk and
                    // register it before giving up.
                    let page = service.page(id, Some(&chrome)).or_else(|| {
                        let e = discover::store_all(only)
                            .into_iter()
                            .find(|e| stem_of(&e.path).as_deref() == Some(id))?;
                        service.register_root(&e.path);
                        service.page(id, Some(&chrome))
                    });
                    match page {
                        Some(page) => HttpResponse::html(splice(&page, &classic_shell)),
                        None => HttpResponse::html(empty_shell(&classic_shell)),
                    }
                })
            },
            session: None,
        },
    );

    // Paired, the listener enforces the token; unpaired it is D3b (same-user on Linux,
    // same-machine on macOS) — v1's rule exactly, from the same gate.
    let gate = match token.as_deref() {
        Some(t) => AuthGate::with_token(t),
        None => AuthGate::same_user(),
    };
    let bound = claude_replay_html::spawn_listener_gated(port, handler, gate)
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    let url = tokened_url(&format!("http://127.0.0.1:{bound}/"), token.as_deref());
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
  agent-monitor-v2 [--pair] [--port N] [--agents claude|codex|qoder|qoderwork]
  agent-monitor-v2 --set-passcode
  agent-monitor-v2 --version

  --pair            Require a token to reach the monitor, and turn on the write
                    routes (send a prompt, grant a pane). The SAME 0600 secret
                    agent-monitor uses — pairing either pairs both.
  --port N          Serve on N instead of {DEFAULT_PORT}.
  --agents AGENT    Only this agent's sessions.
  --set-passcode    Set (or clear) the passcode that granting injection into a
                    live session requires. Terminal-only, so an open browser
                    cannot lift the gate meant to stop it.

Serves http://127.0.0.1:{DEFAULT_PORT} — loopback only. Reads are open to the
same user; writing (and reading a local FILE through the page) needs pairing.

`agent-monitor` (port 2727) and this binary both serve the app shell by default and keep their
previous pages as a supported second interface — a button in each switches and remembers the
choice, and `?ui=classic` / `?ui=app` override for one request. This binary uses its own cache
root (~/.cache/agent-monitor-v2) for parallel comparison while sharing the backend and control
plane — the token, passcode, consent grants, and the shell preference.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let from = source.find(start).expect("reference start marker") + start.len();
        let to = source[from..].find(end).expect("reference end marker") + from;
        &source[from..to]
    }

    #[test]
    fn production_reference_assets_are_exact_demo_extractions() {
        let demo = include_str!("../../design/agent-monitor-codex-demo.html");
        let css = format!("{}\n", between(demo, "<style>\n", "\n</style>"));
        let shell = between(
            demo,
            "<body>\n",
            "<script src=\"sample-transcript-data.js\"></script>",
        );
        assert_eq!(
            include_str!("../../claude-monitor/src/codex-ui/reference.css"),
            css
        );
        assert_eq!(
            include_str!("../../claude-monitor/src/codex-ui/reference-shell.html"),
            shell
        );
    }

    #[test]
    fn the_app_shell_is_a_production_pull_client_beside_the_classic_one() {
        let app = claude_monitor::ui::page("test", false, true);
        assert!(app.contains("/monitor-ui/app.js"));
        assert!(claude_monitor::ui::asset("monitor-ui/record-store.js").is_some());
        assert!(CLASSIC_SHELL.contains("v2rail"));
        // #43: the splice's hidden filter is the shared predicate, inlined at serve time.
        assert!(CLASSIC_SHELL.contains("{{SHARED}}"));
        assert!(CLASSIC_SHELL.contains("__shared.rowVisible("));
        assert!(CLASSIC_SHELL
            .replace("{{SHARED}}", &claude_replay_html::shared_inline_all())
            .contains("Object.assign(window.__shared, { "));
        assert!(!app.contains("sample-transcript-data"));
        assert!(!app.contains("REAL_TRANSCRIPT"));
    }

    #[test]
    fn every_wire_family_has_a_renderer_or_the_readable_fallback() {
        for kind in ["user", "assistant", "queue", "compaction", "attachment"] {
            assert!(
                include_str!("../../claude-monitor/src/codex-ui/view-model.js").contains(kind),
                "missing {kind}"
            );
        }
        for kind in [
            "think", "act", "bash", "read", "write", "edit", "skill", "tool", "agent", "command",
        ] {
            assert!(
                include_str!("../../claude-monitor/src/codex-ui/view-model.js").contains(kind),
                "missing {kind}"
            );
        }
        assert!(
            include_str!("../../claude-monitor/src/codex-ui/components.js").contains("fallback")
        );
    }

    #[test]
    fn interactive_html_preview_is_opaque_and_network_denied() {
        let preview = include_str!("../../claude-monitor/src/codex-ui/preview.js");
        let sandbox = include_str!("../../claude-monitor/src/codex-ui/sandbox.js");
        assert!(preview.contains("sandbox=\"allow-scripts\""));
        assert!(!preview.contains("allow-same-origin"));
        assert!(sandbox.contains("connect-src 'none'"));
        // Placement is by rule, never by searching the artifact — the regex that looked for
        // `<head>` is what a `<head>` inside a comment defeated. `ui_contract.mjs` holds the
        // behavioural cases; this pins that the search is gone.
        assert!(!sandbox.contains("<head[^>]*>"));
        assert!(!preview.contains("<head"));
    }
}
