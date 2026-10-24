//! The monitor's route table (#47): the API arms (`api/sessions`, `api/ignore`, `api/send`,
//! `api/consent`), the `monitor-ui/*` assets, the app shell at `/`, and the fallthrough to the
//! session service's own wire surface (`/session`, `/pull`, `/records`, `/__reveal`, static
//! assets). It was shared by `agent-monitor` and `agent-monitor-v2` until the classic view and v2
//! were retired (#s98, design/classic-view-deprecation.md stage 3): `/` serves the app shell
//! whatever `?ui=` says, and `/api/ui`, which remembered the choice between the two, answers 404.

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
    /// When the last on-demand rescan ran (#309): a request for an id the service has not been
    /// shown rescans at most once per [`RESCAN_COOLDOWN`], so a truly unknown id polled every
    /// second does not walk every store every second.
    pub rescanned: Mutex<Option<std::time::Instant>>,
}

/// How often an unknown id may trigger a rescan (#309).
pub const RESCAN_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);

/// Before the session service answers a request for `id`, make sure it has been SHOWN that id.
/// The service knows only the sessions a scan registered — `/api/sessions` registers each row it
/// lists — so right after a restart a tab left open on a session polled before any page had
/// fetched the list, and a valid transcript read "no such transcript" (#309, the owner). On a miss
/// this runs the same scan `/api/sessions` does, which widens nothing: it registers exactly the
/// transcripts the index lists.
fn ensure_known(backend: &Backend, id: &str) {
    if id.is_empty() || backend.service.knows(id) {
        return;
    }
    {
        let mut last = backend.rescanned.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|at| at.elapsed() < RESCAN_COOLDOWN) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    let service = &backend.service;
    let _ = backend.idx.sessions_json(|path| {
        service.register_root(path);
    });
}

/// What the app shell's page names: the build, and whether the monitor is paired.
pub struct Frontend {
    pub version: &'static str,
    pub paired: bool,
}

/// The listener's handler over one backend and one frontend. It also installs the pinned mdrev,
/// because this is the one constructor the monitor goes through.
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
        // The app shell, whatever `?ui=` says: `?ui=classic` and a `ui.json` that remembered the
        // classic view both get it since that view was retired (#s98).
        "" | "index.html" => HttpResponse::html(ui::app_page(front.version, front.paired)),
        // A Markdown document from the preview pane, in a tab of its own (#271).
        "markdown" => HttpResponse::html(ui::markdown_page()),
        // An image from the preview pane, in a tab of its own (#s11).
        "image" => HttpResponse::html(ui::image_page().to_string()),
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
        // The route that remembered the classic view (#s98): a page from before the release
        // that still asks gets a plain 404, not the static fallback's 403.
        "api/ui" => HttpResponse::not_found("the classic view is retired"),
        // Everything else is the session service's own wire surface — /session, /pull,
        // /records, /__reveal, static assets (§6.3). A session's feed is asked for by id, and the
        // service must have been shown that id first (#309).
        _ => {
            if matches!(name, "pull" | "records") {
                if let Some(id) = query_get(query, "session") {
                    ensure_known(backend, &index::percent_decode(id));
                }
            }
            service_routes(Some(&backend.service), &backend.scratch, req)
        }
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
            rescanned: Mutex::new(None),
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

    /// The table: the app shell at `/` whatever `?ui=` asks (the classic view is retired, #s98),
    /// `/api/ui` gone, the API arms, and `/session` left to the session service.
    #[test]
    fn the_table_serves_the_app_shell_and_the_api_arms() {
        let state = std::env::temp_dir().join(format!("cm-routes-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        let _env = crate::index::StateEnv::set(&state);
        // Hermetic: every store points into the scratch root, so `api/sessions` scans the
        // fixture world (empty here), never this machine's sessions.
        for var in [
            "CLAUDE_PROJECTS_DIR",
            "QODERWORK_PROJECTS_DIR",
            "QWENWORK_PROJECTS_DIR",
            "QODER_PROJECTS_DIR",
            "CODEX_HOME",
        ] {
            std::env::set_var(var, state.join(var.to_ascii_lowercase()));
        }
        // A preference the classic view left behind is read as the app shell.
        std::fs::write(state.join("ui.json"), r#"{"ui":"classic"}"#).unwrap();
        let front = Frontend {
            version: "0.0.0-test",
            paired: false,
        };
        let backend = make_backend("table");
        for query in ["", "ui=app", "ui=classic", "ui=classic&session=s1"] {
            assert!(
                body(&dispatch(&backend, &front, &get("", query))).contains("/monitor-ui/app.js"),
                "{query}"
            );
        }
        assert!(body(&dispatch(
            &backend,
            &front,
            &get("index.html", "ui=classic")
        ))
        .contains("/monitor-ui/app.js"));
        assert_eq!(
            dispatch(&backend, &front, &get("api/ui", "set=classic")).code,
            "404 Not Found",
            "the route that remembered the choice is gone"
        );
        // An asset is served before anything else is consulted.
        assert!(
            body(&dispatch(&backend, &front, &get("monitor-ui/app.js", ""))).contains("import")
        );
        // #203: the tab icon, under the name the app shell's link asks for and the one the
        // browser asks for on its own.
        for name in ["favicon.svg", "favicon.ico"] {
            let icon = dispatch(&backend, &front, &get(name, ""));
            assert_eq!(icon.code, "200 OK", "{name}");
            assert_eq!(icon.content_type, "image/svg+xml", "{name}");
            assert!(body(&icon).starts_with("<svg"), "{name}");
        }
        // The API arms.
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
        // `/session` is the session service's own (a 404 for an unknown id here).
        assert_ne!(
            dispatch(&backend, &front, &get("session", "session=s1")).code,
            "200 OK"
        );
    }
}
