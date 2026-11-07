//! #s416 (design/embedding.md, "The order of the work", step 1): every request the page makes to
//! the server that feeds it, and every URL to that server it builds, goes through the backend
//! handle (`codex-ui/backend.js`). A server path written anywhere else would reach the page's own
//! origin whatever handle a host or the native app gave the components, so this reads every
//! module the shell and Codex native mode serve and refuses a server path on a line that does not
//! hand it to the handle. The exceptions are named, each with the reason it is not a request.

use std::path::Path;

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
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut refused = Vec::new();
    let mut read = 0;
    for dir in ["codex-ui", "codex-workspace"] {
        let mut entries: Vec<_> = std::fs::read_dir(src.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .collect();
        entries.sort();
        for path in entries {
            let name = format!("{dir}/{}", path.file_name().unwrap().to_string_lossy());
            if NOT_REQUESTS.iter().any(|(module, _)| *module == name) {
                continue;
            }
            read += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                if comment(line) || server_paths(line).is_empty() {
                    continue;
                }
                if THROUGH_THE_HANDLE.iter().any(|call| line.contains(call)) {
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
    }
    assert!(read > 20, "read only {read} modules");
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
