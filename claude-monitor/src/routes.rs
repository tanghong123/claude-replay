//! The monitor's route table (#47, design/monitor-shell-duplication.md §2): ONE dispatch for
//! both binaries. The five API arms (`api/ui`, `api/sessions`, `api/ignore`, `api/send`,
//! `api/consent`), the `monitor-ui/*` assets and the fallthrough to the session service's own
//! wire surface (`/session`, `/pull`, `/records`, `/__reveal`, static assets) were identical
//! in `agent-monitor` and `agent-monitor-v2`; they live here once. What genuinely differs is
//! NAMED in [`Frontend`] and each main.rs passes its own: which CLASSIC page `/` serves (v1's
//! rail, v2's splice with its session), and v1's `chrome=embed`-defaulting `session` arm.
//! No behaviour changed in the move — the browser cases against both binaries are the proof.

use crate::control::{self, Attempts};
use crate::index::{self, Index};
use crate::ui;
use claude_replay_html::{
    query_get, service_routes, HttpResponse, Request, RouteHandler, SessionService,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// What both binaries hold behind their routes: the session service, THE index, the scratch
/// dir the service's routes use, and the passcode lockout counter (one per process, one user).
pub struct Backend {
    pub service: Arc<SessionService>,
    pub idx: Arc<Index>,
    pub scratch: PathBuf,
    pub attempts: Mutex<Attempts>,
}

/// The page `/` serves when the classic shell is wanted, given the request's query.
pub type ClassicPage = Arc<dyn Fn(&str) -> HttpResponse + Send + Sync>;

/// A binary's own arm for `session` requests without a `chrome` parameter.
pub type SessionArm = Arc<dyn Fn(&Request, &Backend) -> HttpResponse + Send + Sync>;

/// The two genuine differences between the binaries' tables, as parameters.
pub struct Frontend {
    pub version: &'static str,
    pub paired: bool,
    /// The classic page: v1's rail, v2's splice shell with the session spliced in.
    pub classic: ClassicPage,
    /// v1 only ever serves the view EMBEDDED. The view navigates sub-agents with a relative
    /// `?session=<child>` href that drops the `chrome=embed` param, so v1 defaults it back
    /// (#124) — a drilled-in child keeps embed chrome instead of flashing the full brand. v2
    /// passes `None`: its `session` requests go to the service like everything else.
    pub session: Option<SessionArm>,
}

/// The listener's handler over one backend and one frontend. It also installs the pinned mdrev,
/// because this is the one constructor both binaries go through.
pub fn handler(backend: Arc<Backend>, front: Frontend) -> RouteHandler {
    install_mdrev(&backend.scratch);
    Arc::new(move |req: &Request| dispatch(&backend, &front, req))
}

/// mdrev's embedded viewer for the preview pane (#270), PINNED (#274): the release vendored at
/// `vendor/mdrev`, built into this binary — no mdrev installed on the machine is looked for. Its
/// bundle is served from memory; its CLI is written under `scratch` when a route first needs it,
/// and run with node.
pub fn install_mdrev(scratch: &Path) {
    claude_replay_html::install_mdrev(
        claude_replay_html::MdrevKit {
            version: mdrev::VERSION,
            bundle: mdrev::BUNDLE,
            cli: mdrev::CLI,
            package_json: mdrev::PACKAGE_JSON,
        },
        scratch.join("mdrev"),
    );
}

/// One request through the table.
pub fn dispatch(backend: &Backend, front: &Frontend, req: &Request) -> HttpResponse {
    let (name, query) = (req.name, req.query);
    if let Some(asset) = ui::asset(name) {
        return asset;
    }
    match name {
        // Which shell `/` serves: an explicit `?ui=` wins, else the remembered preference
        // (shared by both binaries — one preference, so `?ui=` is what compares them side by
        // side). Both shells are supported while the app shell is validated.
        "" | "index.html" => {
            if ui::resolve(query) == ui::Shell::Classic {
                (front.classic)(query)
            } else {
                HttpResponse::html(ui::app_page(front.version, front.paired))
            }
        }
        // A Markdown document from the preview pane, in a tab of its own (#271).
        "markdown" => HttpResponse::html(ui::markdown_page()),
        // Read or set which shell `/` serves; the toggle in each shell's header calls this
        // and reloads.
        "api/ui" => ui::route(query),
        // The session list — the shared index's, so a row's liveness, its counters, its
        // family and its `injectable`/`consented` facts are ONE derivation. The register
        // callback is what makes `?session=<id>` resolvable: the shells render by id, and the
        // service only knows the ids it has been shown.
        "api/sessions" => {
            let service = &backend.service;
            HttpResponse::json(backend.idx.sessions_json(|path| {
                service.register_root(path);
            }))
        }
        // #113: toggle a hide key (`s:<sid>` / `p:<cwd>` / `a:<label>`). A GET with a query
        // param, because the loopback listener only parses the request line — no method, no
        // body (serve.rs). Persisting a local hide preference at the monitor's own root is UI
        // state, not agent/terminal control, so it stays inside the read-only contract (R8).
        "api/ignore" => {
            let resp = match (query_get(query, "add"), query_get(query, "remove")) {
                (Some(k), _) => backend.idx.set_ignore(&index::percent_decode(k), true),
                (_, Some(k)) => backend.idx.set_ignore(&index::percent_decode(k), false),
                _ => r#"{"ok":false}"#.to_string(),
            };
            HttpResponse::json(resp)
        }
        // The two WRITE routes (#133), from the shared control plane: send a prompt into a
        // session (resume it if finished — an autonomous agent turn, owner-authorized — or
        // inject into its tmux pane if live, proven and consented), and grant/revoke that
        // consent. Both are `deny_write`-gated inside — POST, same-origin, and a token — so a
        // stock unpaired binary cannot reach them.
        "api/send" => control::send_route(&backend.idx, req),
        "api/consent" => control::consent_route(&backend.idx, req, &backend.attempts),
        "session" if front.session.is_some() && query_get(query, "chrome").is_none() => {
            (front.session.as_ref().expect("checked"))(req, backend)
        }
        // Everything else is the session service's own wire surface — /session, /pull,
        // /records, /__reveal, static assets (§6.3).
        _ => service_routes(Some(&backend.service), &backend.scratch, req),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_html::{RootLock, ServiceConfig};
    use claude_replay_present::cache::Presentation;

    fn make_backend(tag: &str) -> Arc<Backend> {
        let root = std::env::temp_dir().join(format!("cm-routes-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let scratch = root.join("scratch");
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
        Arc::new(Backend {
            service,
            idx: Arc::new(Index::new(root.clone(), root.join("state"), Vec::new())),
            scratch,
            attempts: Mutex::new(Attempts::default()),
        })
    }

    fn get<'a>(name: &'a str, query: &'a str) -> Request<'a> {
        Request {
            method: "GET",
            name,
            query,
            body: b"",
            authenticated: false,
            origin_ok: true,
        }
    }

    fn body(resp: &HttpResponse) -> String {
        String::from_utf8_lossy(&resp.body).into_owned()
    }

    /// Both binaries build their handler here, so both carry the pinned mdrev (#274): the page
    /// names the pin's version, and the bundle under it is the vendored file, byte for byte.
    #[test]
    fn the_handler_serves_the_pinned_mdrev() {
        // The app page reads the remembered shell from the state dir (#153: never the real one).
        let state = std::env::temp_dir().join(format!("cm-routes-mdrev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        let _env = crate::index::StateEnv::set(&state);
        let front = Frontend {
            version: "0.0.0-test",
            paired: false,
            classic: Arc::new(|_: &str| HttpResponse::html(String::new())),
            session: None,
        };
        let serve = handler(make_backend("mdrev"), front);
        assert_eq!(claude_replay_html::mdrev_version(), Some(mdrev::VERSION));
        let page = body(&serve(&get("", "ui=app")));
        assert!(
            page.contains(&format!("data-mdrev=\"{}\"", mdrev::VERSION)),
            "the app shell names the pinned version"
        );
        let entry = format!("mdrev/{}/mdrev.js", mdrev::VERSION);
        let r = serve(&get(&entry, ""));
        assert_eq!(r.code, "200 OK");
        let vendored =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/mdrev/release/bundle/mdrev.js");
        assert!(
            r.body == std::fs::read(vendored).unwrap(),
            "the vendored entry, byte for byte"
        );
        assert_eq!(
            serve(&get("mdrev/0.16.45/mdrev.js", "")).code,
            "404 Not Found",
            "only the pin's version"
        );
    }

    /// A Markdown document's tab of its own (#271) is a STATIC page: it names the pin, loads its
    /// module, and writes nothing its address carries into its HTML — the module reads the address
    /// itself and every fetch it makes goes through the pane's own guarded routes.
    #[test]
    fn the_markdown_tab_is_a_static_page() {
        let front = Frontend {
            version: "0.0.0-test",
            paired: false,
            classic: Arc::new(|_: &str| HttpResponse::html(String::new())),
            session: None,
        };
        let serve = handler(make_backend("markdown"), front);
        let hostile =
            "root=%3Cscript%3Ealert(1)%3C%2Fscript%3E&path=%22%3E%3Cimg%20src%3Dx%3E&cap=x";
        let page = serve(&get("markdown", hostile));
        assert_eq!(page.code, "200 OK");
        let html = body(&page);
        assert!(html.contains(&format!("data-mdrev=\"{}\"", mdrev::VERSION)));
        assert!(html.contains(r#"src="/monitor-ui/markdown-page.js""#));
        for reflected in ["alert(1)", "<img", "%3Cscript", "src=x"] {
            assert!(
                !html.contains(reflected),
                "the address reached the HTML: {reflected}"
            );
        }
        assert_eq!(
            body(&serve(&get("markdown", ""))),
            html,
            "one page, whatever it is asked"
        );
        let module = serve(&get("monitor-ui/markdown-page.js", ""));
        assert_eq!(
            (module.code, module.content_type),
            ("200 OK", "text/javascript; charset=utf-8")
        );
    }

    /// The table serves the same surface for both frontends; only the named differences vary.
    #[test]
    fn the_table_dispatches_the_shared_arms_and_the_named_differences() {
        let state = std::env::temp_dir().join(format!("cm-routes-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        let _env = crate::index::StateEnv::set(&state);
        // Hermetic: every store points into the scratch root, so `api/sessions` scans the
        // fixture world (empty here), never this machine's sessions.
        for var in [
            "CLAUDE_PROJECTS_DIR",
            "QODERWORK_PROJECTS_DIR",
            "QODER_PROJECTS_DIR",
            "CODEX_HOME",
        ] {
            std::env::set_var(var, state.join(var.to_ascii_lowercase()));
        }
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let front = Frontend {
            version: "0.0.0-test",
            paired: false,
            classic: Arc::new(|query: &str| HttpResponse::html(format!("CLASSIC[{query}]"))),
            session: Some({
                let seen = seen.clone();
                Arc::new(move |req: &Request, _backend: &Backend| {
                    seen.lock().unwrap().push(req.query.to_string());
                    HttpResponse::html("SESSION".to_string())
                })
            }),
        };
        let backend = make_backend("v1");
        // The app shell by default, the classic page on request — through the shared resolver.
        assert!(
            body(&dispatch(&backend, &front, &get("", "ui=app"))).contains("/monitor-ui/app.js")
        );
        assert_eq!(
            body(&dispatch(
                &backend,
                &front,
                &get("", "ui=classic&session=s1")
            )),
            "CLASSIC[ui=classic&session=s1]"
        );
        assert_eq!(
            body(&dispatch(
                &backend,
                &front,
                &get("index.html", "ui=classic")
            )),
            "CLASSIC[ui=classic]"
        );
        // An asset is served before anything else is consulted.
        assert!(
            body(&dispatch(&backend, &front, &get("monitor-ui/app.js", ""))).contains("import")
        );
        // #203: the tab icon, one source for both shells — under the name the app shell's link
        // asks for and the one the browser asks for on its own.
        for name in ["favicon.svg", "favicon.ico"] {
            let icon = dispatch(&backend, &front, &get(name, ""));
            assert_eq!(icon.code, "200 OK", "{name}");
            assert_eq!(icon.content_type, "image/svg+xml", "{name}");
            assert!(body(&icon).starts_with("<svg"), "{name}");
        }
        // The shared API arms.
        assert!(body(&dispatch(&backend, &front, &get("api/ui", ""))).contains("\"ui\""));
        assert!(body(&dispatch(&backend, &front, &get("api/sessions", ""))).contains("\"groups\""));
        assert!(body(&dispatch(&backend, &front, &get("api/ignore", ""))).contains("\"ok\":false"));
        // The write routes refuse an unauthenticated GET rather than acting.
        assert!(!dispatch(&backend, &front, &get("api/send", "target=s1"))
            .code
            .starts_with("200"));
        assert!(
            !dispatch(&backend, &front, &get("api/consent", "target=s1"))
                .code
                .starts_with("200")
        );
        // v1's session arm sees `session` requests without `chrome`, and only those.
        assert_eq!(
            body(&dispatch(&backend, &front, &get("session", "session=s1"))),
            "SESSION"
        );
        assert_ne!(
            body(&dispatch(
                &backend,
                &front,
                &get("session", "session=s1&chrome=embed")
            )),
            "SESSION"
        );
        assert_eq!(seen.lock().unwrap().as_slice(), ["session=s1".to_string()]);
        // v2 passes no session arm: the service answers (a 404 for an unknown id here).
        let v2 = Frontend {
            version: "0.0.0-test",
            paired: true,
            classic: Arc::new(|_q: &str| HttpResponse::html("SPLICE".to_string())),
            session: None,
        };
        let backend2 = make_backend("v2");
        assert_ne!(
            body(&dispatch(&backend2, &v2, &get("session", "session=s1"))),
            "SESSION"
        );
        assert_eq!(
            body(&dispatch(&backend2, &v2, &get("", "ui=classic"))),
            "SPLICE"
        );
        assert!(body(&dispatch(&backend2, &v2, &get("", "ui=app"))).contains("/monitor-ui/app.js"));
    }
}
