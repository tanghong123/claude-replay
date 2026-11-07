//! #s416 (design/embedding.md, "The order of the work", step 1): every request the page makes to
//! the server that feeds it, and every URL to that server it builds, goes through the backend
//! handle (`codex-ui/backend.js`). A server path written anywhere else would reach the page's own
//! origin whatever handle a host or the native app gave the components, so this reads every
//! module the shell and Codex native mode serve and refuses a server path on a line that does not
//! hand it to the handle. The exceptions are named, each with the reason it is not a request.

use std::path::Path;

mod contract_index;

/// The routes the page's server answers, as a path's first segment.
const ROUTES: &[&str] = &[
    "api",
    "records",
    "pull",
    "file",
    "__reveal",
    "search",
    "turn-index",
    "markdown",
    "image",
    "mdrev",
    "codex-workspace",
    "pair",
    "monitor-ui",
    "mermaid",
];

/// A line that hands its server path to the handle.
const THROUGH_THE_HANDLE: &[&str] = &[
    "backend().fetch(",
    "backend().url(",
    "backend().mdrevUrl(",
    "backend().mdrevFetch(",
];

/// (module, text on the line): server paths that are names, not requests.
const NAMED: &[(&str, &str, &str)] = &[
    (
        "codex-ui/attachment-viewer.js",
        "const filePath = `/file?",
        "the path the next two lines hand to backend().url and backend().fetch",
    ),
    (
        "codex-ui/mdrev-pane.js",
        "const CONTRACT = \"/api/mdrev\"",
        "mdrev's route prefix, joined through backend().mdrevUrl and mdrevFetch",
    ),
    (
        "codex-ui/mdrev-pane.js",
        "const REVIEW_CONTRACT = \"/api/mdrev-review\"",
        "the detached tab's route prefix, joined through backend().mdrevUrl",
    ),
    (
        "codex-ui/native-menu.js",
        "fetcher(\"/api/codex/status\"",
        "a path for the fetcher its callers hand in, backend().fetch",
    ),
    (
        "codex-workspace/api.js",
        "`/api/codex/${route}`",
        "a path for GatewayApi's fetcher, which the shell hands in as backend().fetch",
    ),
    (
        "codex-workspace/api.js",
        "\"/api/sessions\"",
        "a path for GatewayApi's fetcher, as above",
    ),
    (
        "codex-workspace/monitor-controls.js",
        "new EventSource(this.actions?.backend ?",
        "the handle's events URL when the shell gives one; the workspace's own page has none",
    ),
];

/// Modules that are not the page's requests at all.
const NOT_REQUESTS: &[(&str, &str)] = &[
    (
        "codex-ui/backend.js",
        "the request layer itself: it names the routes it explains",
    ),
    (
        "codex-workspace/sw.js",
        "the Codex workspace page's own service worker and the files it caches",
    ),
];

/// The shared modules (`codex-ui/shared`, the html crate's, #s421) also run on the offline page,
/// which has no backend handle, so they may not import it. They build path strings for their
/// callers (`revealQuery`, `sendQuery`), and those callers hand them to the handle. A server path
/// they USE themselves is a request: a line that loads or fetches one.
const SHARED_USES: &[&str] = &[
    "fetch(",
    ".src",
    ".href",
    "new EventSource(",
    "import(",
    "location",
];

/// Every module the page's server serves the shell, as `(dir/name, path)`, sorted, with the
/// modules that are not requests left out.
fn modules() -> Vec<(String, std::path::PathBuf)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for dir in ["codex-ui", "codex-workspace", "codex-ui/shared"] {
        let mut entries: Vec<_> = std::fs::read_dir(src.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .collect();
        entries.sort();
        for path in entries {
            let name = format!("{dir}/{}", path.file_name().unwrap().to_string_lossy());
            if !NOT_REQUESTS.iter().any(|(module, _)| *module == name) {
                found.push((name, path));
            }
        }
    }
    found
}

/// Every string or template literal on `line` that opens with a server route.
fn server_paths(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for quote in ['"', '`'] {
        for (at, _) in line.match_indices(quote) {
            let rest = &line[at + 1..];
            let Some(path) = rest.strip_prefix('/') else {
                continue;
            };
            let segment: String = path
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if ROUTES.contains(&segment.as_str()) {
                found.push(format!("/{segment}"));
            }
        }
    }
    found
}

/// A line that is a comment, or the inside of one.
fn comment(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("//") || line.starts_with('*') || line.starts_with("/*")
}

#[test]
fn every_server_request_the_page_makes_goes_through_the_backend_handle() {
    let mut refused = Vec::new();
    let mut read = 0;
    let mut shared = 0;
    for (name, path) in modules() {
        read += 1;
        let is_shared = name.starts_with("codex-ui/shared/");
        shared += usize::from(is_shared);
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            if comment(line) || server_paths(line).is_empty() {
                continue;
            }
            if THROUGH_THE_HANDLE.iter().any(|call| line.contains(call)) {
                continue;
            }
            if is_shared && !SHARED_USES.iter().any(|call| line.contains(call)) {
                continue;
            }
            if NAMED
                .iter()
                .any(|(module, text, _)| *module == name && line.contains(text))
            {
                continue;
            }
            refused.push(format!(
                "{name}:{}: {} — {}",
                n + 1,
                server_paths(line).join(", "),
                line.trim().chars().take(140).collect::<String>()
            ));
        }
    }
    assert!(read > 40, "read only {read} modules");
    assert!(shared > 15, "read only {shared} shared modules");
    assert!(
        refused.is_empty(),
        "a server path built outside the backend handle (route it through backend() in codex-ui/backend.js, or name it in this test with the reason it is not a request):\n{}",
        refused.join("\n")
    );
}

/// The check finds what it is for: a server path in a plain fetch, a template, an import or a
/// link, and not one in a comment.
#[test]
fn the_check_finds_a_server_path_anywhere_on_a_line() {
    assert_eq!(server_paths(r#"fetch("/api/sessions")"#), ["/api"]);
    assert_eq!(server_paths("fetch(`/file?path=${p}`)"), ["/file"]);
    assert_eq!(
        server_paths(r#"import("/codex-workspace/api.js")"#),
        ["/codex-workspace"]
    );
    assert_eq!(
        server_paths(r#"link.href = "/mdrev/1/mdrev.css""#),
        ["/mdrev"]
    );
    assert!(server_paths(r#"fetch("./relative.js")"#).is_empty());
    assert!(server_paths(r#"const PROMPT = "/compact""#).is_empty());
    assert!(comment("  // the `/pull` route"));
    assert!(comment("   * `/records` ranges"));
}

/// Every server path on `line`, whole: from its opening quote to the first character that ends a
/// path (a query, a quote, a template's `${`, a space or a closing bracket). A template's path is
/// cut at its first `${`, so `/api/codex/${route}` names the prefix `/api/codex/`.
fn whole_paths(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for quote in ['"', '`', '\''] {
        for (at, _) in line.match_indices(quote) {
            let rest = &line[at + 1..];
            let segment: String = rest
                .strip_prefix('/')
                .unwrap_or("")
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !ROUTES.contains(&segment.as_str()) {
                continue;
            }
            let end = rest
                .find(['?', '"', '`', '\'', '$', ' ', ')', ']', '#'])
                .unwrap_or(rest.len());
            found.push(rest[..end].to_string());
        }
    }
    found
}

/// #s421: every server path the page's modules name is one `docs/backend-contract.md` documents:
/// a row of its route index, or under a prefix row. Lines the handle rule lets through (they hand
/// the path to the handle, or name it for a caller that does) are read too: what matters here is
/// that the route is in the contract, whoever asks for it. `routes`' test holds the other half,
/// that every row is a route the table answers.
#[test]
fn every_server_path_the_page_names_is_one_the_contract_documents() {
    let rows = contract_index::rows(&contract_index::read());
    let mut undocumented = Vec::new();
    let mut named = 0;
    for (name, path) in modules() {
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            if comment(line) {
                continue;
            }
            for found in whole_paths(line) {
                named += 1;
                if !rows.iter().any(|row| row.covers(&found)) {
                    undocumented.push(format!("{name}:{}: {found}", n + 1));
                }
            }
        }
    }
    assert!(named > 30, "found only {named} server paths");
    assert!(
        undocumented.is_empty(),
        "a server path the page names that docs/backend-contract.md does not document (add it to the route index, with its section):\n{}",
        undocumented.join("\n")
    );
}

#[test]
fn whole_paths_end_where_a_path_ends() {
    assert_eq!(whole_paths(r#"fetch("/api/sessions")"#), ["/api/sessions"]);
    assert_eq!(whole_paths("`/api/send?target=${t}`"), ["/api/send"]);
    assert_eq!(whole_paths("`/api/codex/${route}`"), ["/api/codex/"]);
    assert_eq!(
        whole_paths(r#"import(backend().url("/codex-workspace/api.js"))"#),
        ["/codex-workspace/api.js"]
    );
    assert!(whole_paths(r#"fetch("./relative.js")"#).is_empty());
    assert!(whole_paths(r#"const PROMPT = "/compact""#).is_empty());
}

/// The index's rule for a path under a prefix row, and for a prefix named without its slash.
#[test]
fn the_route_index_covers_a_path_under_a_prefix() {
    let rows = contract_index::rows(&contract_index::read());
    let row = |path: &str| rows.iter().find(|r| r.path == path).unwrap();
    assert!(row("/api/mdrev/").covers("/api/mdrev"));
    assert!(row("/api/mdrev/").covers("/api/mdrev/hold"));
    assert!(!row("/api/mdrev/").covers("/api/mdrev-review"));
    assert!(row("/mdrev/<mdrev>/").covers("/mdrev/"));
    assert!(row("/api/sessions").covers("/api/sessions"));
    assert!(!row("/api/sessions").covers("/api/sessions/x"));
}
