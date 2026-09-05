//! Shared Codex-style monitor UI assets.
//!
//! The visual reference is extracted byte-for-byte from the immutable design demo. Both
//! monitor binaries call this module, so there is one frontend and one rollback boundary.

use claude_replay_html::{query_get, HttpResponse};
use std::path::PathBuf;

const PAGE_HEAD: &str = include_str!("codex-ui/page-head.html");
const REFERENCE_SHELL: &str = include_str!("codex-ui/reference-shell.html");
const PAGE_TAIL: &str = include_str!("codex-ui/page-tail.html");

/// Which frontend `/` serves. BOTH are supported while the app shell is being validated —
/// this is not a one-release escape hatch, and the classic shell is not deprecated until the
/// owner says it is. `Classic` is the rail (v1) / the splice shell (v2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    App,
    Classic,
}

impl Shell {
    pub fn as_str(self) -> &'static str {
        match self {
            Shell::App => "app",
            Shell::Classic => "classic",
        }
    }

    /// Parse a `ui=` value. `codex` is accepted for the app shell because that is what the
    /// rollout called it and what already-published links carry.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "classic" => Some(Shell::Classic),
            "app" | "codex" => Some(Shell::App),
            _ => None,
        }
    }
}

/// The remembered choice: `<state_dir>/ui.json`, beside `ignored.json` and for the same reason
/// (#197) — it is user INTENT that cannot be recomputed, so it is state, not cache. Shared by
/// both monitor binaries deliberately: it answers "which shell do I want", not "what is this
/// process". A `?ui=` on the URL still overrides it for one request without disturbing it, which
/// is what makes side-by-side comparison possible while the setting stays put.
fn preference_path() -> PathBuf {
    crate::index::state_dir().join("ui.json")
}

/// The stored preference, or the app shell when nothing is stored or the file is unreadable.
pub fn preference() -> Shell {
    std::fs::read_to_string(preference_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get("ui").and_then(|v| v.as_str()).and_then(Shell::parse))
        .unwrap_or(Shell::App)
}

fn set_preference(shell: Shell) {
    let path = preference_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::json!({ "ui": shell.as_str() }).to_string(),
    );
}

/// Which shell this request wants: an explicit `?ui=` wins, else the remembered choice.
pub fn resolve(query: &str) -> Shell {
    query_get(query, "ui")
        .and_then(Shell::parse)
        .unwrap_or_else(preference)
}

/// `GET /api/ui` reads the preference; `?set=classic|app` writes it. Same shape and same
/// justification as `/api/ignore`: a GET with a query param, because the loopback listener
/// parses only the request line, and persisting a local UI choice at the monitor's own state
/// dir is UI state rather than agent control — inside the read-only contract (R8).
pub fn route(query: &str) -> HttpResponse {
    if let Some(shell) = query_get(query, "set").and_then(Shell::parse) {
        set_preference(shell);
        return HttpResponse::json(
            serde_json::json!({ "ok": true, "ui": shell.as_str() }).to_string(),
        );
    }
    HttpResponse::json(serde_json::json!({ "ok": true, "ui": preference().as_str() }).to_string())
}

/// The app shell for ONE request. `data-ui-default` says whether the app shell is also the
/// remembered preference, which is what lets the page emit clean session links (`?session=…`)
/// when it is, and pin `?ui=app` when it is not — a link copied out of this shell reopens this
/// shell either way. Built per request because the preference is a live setting: the toggle
/// writes it and reloads, so a page minted once at startup would answer with a stale flag.
pub fn app_page(version: &str, paired: bool) -> String {
    page(version, paired, preference() == Shell::App)
}

pub fn page(version: &str, paired: bool, default_ui: bool) -> String {
    let head = PAGE_HEAD
        .replace("{{VERSION}}", version)
        .replace("{{PAIRED}}", if paired { "true" } else { "false" })
        .replace("{{DEFAULT_UI}}", if default_ui { "true" } else { "false" });
    format!("{head}{REFERENCE_SHELL}{PAGE_TAIL}")
}

pub fn asset(name: &str) -> Option<HttpResponse> {
    // Shared modules (seam 0): ONE source in the html crate, served here unchanged as ES
    // modules and inlined by that crate into its own pages. Anything under `shared/` the html
    // crate does not know is a 404, like any other unregistered asset.
    if let Some(rest) = name.strip_prefix("monitor-ui/shared/") {
        let module = rest.strip_suffix(".js")?;
        let source = claude_replay_html::shared_source(module)?;
        let mut response =
            HttpResponse::ok("text/javascript; charset=utf-8", source.as_bytes().to_vec());
        response.headers.push("Cache-Control: no-store".into());
        return Some(response);
    }
    let (content_type, bytes) = match name {
        "monitor-ui/reference.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-ui/reference.css").as_slice(),
        ),
        "monitor-ui/production.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-ui/production.css").as_slice(),
        ),
        "monitor-ui/app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/app.js").as_slice(),
        ),
        "monitor-ui/icons.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/icons.js").as_slice(),
        ),
        "monitor-ui/state.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/state.js").as_slice(),
        ),
        "monitor-ui/record-store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/record-store.js").as_slice(),
        ),
        "monitor-ui/session-index-store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-index-store.js").as_slice(),
        ),
        "monitor-ui/view-model.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/view-model.js").as_slice(),
        ),
        "monitor-ui/components.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/components.js").as_slice(),
        ),
        "monitor-ui/viewport.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/viewport.js").as_slice(),
        ),
        "monitor-ui/control-store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/control-store.js").as_slice(),
        ),
        "monitor-ui/preview.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/preview.js").as_slice(),
        ),
        "monitor-ui/view-memory.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/view-memory.js").as_slice(),
        ),
        "monitor-ui/sandbox.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/sandbox.js").as_slice(),
        ),
        "monitor-ui/attachment-viewer.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/attachment-viewer.js").as_slice(),
        ),
        _ => return None,
    };
    let mut response = HttpResponse::ok(content_type, bytes.to_vec());
    // The monitor is commonly rebuilt and restarted on the same loopback port. Keeping an
    // older ES module in the browser cache can mix two UI revisions in one page, so assets
    // deliberately follow the live record APIs' no-store behavior.
    response.headers.push("Cache-Control: no-store".into());
    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_is_the_reference_shell_without_mock_runtime() {
        let page = page("test", false, true);
        assert!(page.contains(REFERENCE_SHELL));
        assert!(page.contains("/monitor-ui/app.js"));
        assert!(!page.contains("sample-transcript-data.js"));
        assert!(!page.contains("REAL_TRANSCRIPT"));
    }

    /// `?ui=` overrides for ONE request; the stored preference answers everything else. That
    /// pair is what lets both shells stay reachable: the setting is how you choose, the param is
    /// how you look at the other one without changing your mind.
    #[test]
    fn an_explicit_ui_param_overrides_the_remembered_preference() {
        let _g = crate::index::STATE_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ui-pref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("AGENT_MONITOR_STATE", &dir);

        assert_eq!(preference(), Shell::App, "app shell with nothing stored");
        assert_eq!(resolve(""), Shell::App);
        assert_eq!(resolve("ui=classic"), Shell::Classic);

        let reply = route("set=classic");
        assert!(String::from_utf8_lossy(&reply.body).contains("\"ui\":\"classic\""));
        assert_eq!(preference(), Shell::Classic, "the choice persisted");
        assert_eq!(resolve(""), Shell::Classic, "and answers a bare request");
        assert_eq!(
            resolve("ui=codex"),
            Shell::App,
            "…while an explicit param still overrides it"
        );
        assert_eq!(
            preference(),
            Shell::Classic,
            "and overriding does NOT rewrite the preference"
        );

        route("set=app");
        assert_eq!(preference(), Shell::App);
        std::env::remove_var("AGENT_MONITOR_STATE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every `./x.js` a served module imports must itself be served. The list above is
    /// explicit, so a new module that is imported but not registered 404s in the browser and
    /// the WHOLE module graph fails to load — the shell paints nothing, and no node-side test
    /// can see it, because node resolves the same import from disk. That is exactly what
    /// happened with `session-visibility.js` (2026-09-03): the contract test passed and the
    /// page was blank. This walks the closure from the entry point.
    #[test]
    fn every_module_the_shell_imports_is_served() {
        let sources: &[(&str, &str)] = &[
            ("app.js", include_str!("codex-ui/app.js")),
            (
                "attachment-viewer.js",
                include_str!("codex-ui/attachment-viewer.js"),
            ),
            ("components.js", include_str!("codex-ui/components.js")),
            (
                "control-store.js",
                include_str!("codex-ui/control-store.js"),
            ),
            ("icons.js", include_str!("codex-ui/icons.js")),
            ("preview.js", include_str!("codex-ui/preview.js")),
            ("record-store.js", include_str!("codex-ui/record-store.js")),
            ("sandbox.js", include_str!("codex-ui/sandbox.js")),
            (
                "session-index-store.js",
                include_str!("codex-ui/session-index-store.js"),
            ),
            (
                "shared/session-visibility.js",
                claude_replay_html::shared_source("session-visibility").unwrap(),
            ),
            (
                "shared/state-labels.js",
                claude_replay_html::shared_source("state-labels").unwrap(),
            ),
            (
                "shared/keymap.js",
                claude_replay_html::shared_source("keymap").unwrap(),
            ),
            (
                "shared/reading.js",
                claude_replay_html::shared_source("reading").unwrap(),
            ),
            (
                "shared/capabilities.js",
                claude_replay_html::shared_source("capabilities").unwrap(),
            ),
            (
                "shared/control-protocol.js",
                claude_replay_html::shared_source("control-protocol").unwrap(),
            ),
            (
                "shared/record-stream.js",
                claude_replay_html::shared_source("record-stream").unwrap(),
            ),
            (
                "shared/runtime.js",
                claude_replay_html::shared_source("runtime").unwrap(),
            ),
            (
                "shared/ids.js",
                claude_replay_html::shared_source("ids").unwrap(),
            ),
            (
                "shared/parts.js",
                claude_replay_html::shared_source("parts").unwrap(),
            ),
            (
                "shared/search.js",
                claude_replay_html::shared_source("search").unwrap(),
            ),
            (
                "shared/time.js",
                claude_replay_html::shared_source("time").unwrap(),
            ),
            (
                "shared/tool-head.js",
                claude_replay_html::shared_source("tool-head").unwrap(),
            ),
            (
                "shared/interaction.js",
                claude_replay_html::shared_source("interaction").unwrap(),
            ),
            (
                "shared/filter.js",
                claude_replay_html::shared_source("filter").unwrap(),
            ),
            (
                "shared/virtual-window.js",
                claude_replay_html::shared_source("virtual-window").unwrap(),
            ),
            (
                "shared/task-card.js",
                claude_replay_html::shared_source("task-card").unwrap(),
            ),
            ("state.js", include_str!("codex-ui/state.js")),
            ("view-memory.js", include_str!("codex-ui/view-memory.js")),
            ("view-model.js", include_str!("codex-ui/view-model.js")),
            ("viewport.js", include_str!("codex-ui/viewport.js")),
        ];
        let mut seen = std::collections::BTreeSet::new();
        let mut todo = vec!["app.js".to_string()];
        while let Some(name) = todo.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            assert!(
                asset(&format!("monitor-ui/{name}")).is_some(),
                "{name} is imported by the shell but `asset()` does not serve it — register it"
            );
            let src = sources
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, s)| *s)
                .unwrap_or_else(|| {
                    panic!("{name} is served but not listed in this test's sources")
                });
            for line in src.lines() {
                let line = line.trim();
                if !(line.starts_with("import ") || line.starts_with("export ")) {
                    continue;
                }
                if let Some(rest) = line.split(" from \"./").nth(1) {
                    if let Some(dep) = rest.split('"').next() {
                        // The names this import asks for must be exported by the module it
                        // names — a module moved into shared/ (or a symbol lifted out of a
                        // module) otherwise links to nothing and the shell renders blank,
                        // which no other test sees before real Chrome does (#46).
                        if let Some(list) = line
                            .strip_prefix("import {")
                            .and_then(|l| l.split('}').next())
                        {
                            let target = sources
                                .iter()
                                .find(|(n, _)| *n == dep)
                                .map(|(_, src)| *src)
                                .unwrap_or("");
                            for binding in list.split(',').map(str::trim).filter(|n| !n.is_empty())
                            {
                                let local = binding.split(" as ").next().unwrap_or(binding).trim();
                                let exported = target.lines().any(|l| {
                                    let l = l.trim_start();
                                    l.starts_with(&format!("export function {local}"))
                                        || l.starts_with(&format!("export const {local}"))
                                        || l.starts_with(&format!("export class {local}"))
                                        || l.starts_with(&format!("export let {local}"))
                                        || (l.starts_with("export {")
                                            && l.split('{').nth(1).is_some_and(|inner| {
                                                inner
                                                    .split('}')
                                                    .next()
                                                    .unwrap_or("")
                                                    .split(',')
                                                    .any(|n| n.trim() == local)
                                            }))
                                        || (l.starts_with("export const ")
                                            && l.contains(&format!(", {local} =")))
                                });
                                assert!(
                                    exported,
                                    "{name} imports `{binding}` from {dep}, which does not export it"
                                );
                            }
                        }
                        todo.push(dep.to_string());
                    }
                }
            }
        }
        assert!(seen.len() >= 15, "walked the graph: {seen:?}");
    }

    #[test]
    fn serves_the_production_attachment_viewer() {
        let response = asset("monitor-ui/attachment-viewer.js").unwrap();
        assert!(response
            .headers
            .iter()
            .any(|header| header == "Cache-Control: no-store"));
    }
}

#[cfg(test)]
mod state_label_tests {
    /// Every reason the tracker can emit has wording in the shared state-label table (#44):
    /// the JS side is held to the Rust enum, so a new `StateReason` cannot ship unlabelled.
    /// The list is exhaustive by construction — the `match` fails to compile the day a
    /// variant is added without being listed here.
    #[test]
    fn every_tracker_reason_has_a_label() {
        use claude_replay_core::state::StateReason;
        use StateReason::*;
        let all = [
            Exited,
            ExitedMidWork,
            Question,
            PlanApproval,
            QueuedPrompt,
            Tool,
            Thinking,
            Permission,
            EndedQuestion,
            Error,
            Done,
            Starting,
            Stalled,
        ];
        let table = claude_replay_html::shared_source("state-labels").expect("registered");
        for reason in all {
            // Exhaustive by construction: a new variant fails to compile here until listed.
            match reason {
                Exited | ExitedMidWork | Question | PlanApproval | QueuedPrompt | Tool
                | Thinking | Permission | EndedQuestion | Error | Done | Starting | Stalled => (),
            }
            let key = StateReason::as_str(reason);
            let quoted = format!("\"{key}\":");
            let bare = format!("\n  {key}:");
            assert!(
                table.contains(&quoted) || table.contains(&bare),
                "state-labels.js has no label for the tracker reason `{key}`"
            );
        }
    }
}
