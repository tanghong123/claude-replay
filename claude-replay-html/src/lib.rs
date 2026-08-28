//! **claude-replay-html** — the HTML frontend (#71): one render core (`html_export`) behind
//! three entry points — `--dump-html` (single self-contained file), `--dump-all-html`
//! (offline directory bundle with the full agent tree + materialized attachments), and
//! `--html` (live loopback server speaking the cursor-pull protocol). No terminal
//! dependencies; the shared span vocabulary and session cache come from
//! `claude-replay-present`.

pub mod html_export;

pub use html_export::{
    display_title, dump_all_html, dump_html, existing_server, get_request, handoff_url, mint_token,
    query_get, serve, service_routes, spawn_listener, spawn_listener_gated, start_server, AuthGate,
    HttpResponse, LiveServer, PageChrome, Request, RootLock, RouteHandler, ServiceConfig,
    SessionService, StaleEpoch,
};

// Aliases so the moved module keeps referring to `crate::model`, `crate::cache`, …
// unchanged.
pub(crate) use claude_replay_core::{
    adapter, diff, discover, engine, fold, metrics, model, parse_session_as, Agent, Transcript,
};
pub(crate) use claude_replay_present::{cache, highlight, present, sys, Args, SessionCache};
