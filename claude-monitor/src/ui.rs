//! Shared Codex-style monitor UI assets.
//!
//! The visual reference is extracted byte-for-byte from the immutable design demo. The
//! monitor serves this one frontend.

use claude_replay_html::HttpResponse;

const PAGE_HEAD: &str = include_str!("codex-ui/page-head.html");
/// The demo's shell with an empty `#sessionView` where the session view's markup was: the view builds
/// itself there from `session-view-markup.js` (#s429 stage 4). Likewise an empty `#sessionList` where
/// the tree was, which the session list fills from `session-list-markup.js` (#s430 stage 3). All are
/// generated from the demo.
const APP_SHELL: &str = include_str!("codex-ui/app-shell.html");
const PAGE_TAIL: &str = include_str!("codex-ui/page-tail.html");

/// The app shell for ONE request. The classic view it was once switched with is retired (#s98),
/// so there is no preference to read: `?ui=` and a `ui.json` left behind are both ignored.
pub fn app_page(version: &str, paired: bool) -> String {
    page(version, paired)
}

pub fn page(version: &str, paired: bool) -> String {
    let head = PAGE_HEAD
        .replace("{{VERSION}}", version)
        .replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path())
        .replace("{{PAIRED}}", if paired { "true" } else { "false" })
        // The backend contract this server answers (#s421), for the page to read; step 4 of
        // design/embedding.md reads it from each backend's handle instead.
        .replace("{{CONTRACT}}", &crate::routes::CONTRACT_VERSION.to_string())
        // The mdrev release the preview pane mounts Markdown with (#270), pinned into this binary
        // (#274) and installed by `routes::handler`; "" only before that, when the page renders
        // Markdown as text, as it always did.
        .replace(
            "{{MDREV}}",
            claude_replay_html::mdrev_version().unwrap_or(""),
        );
    format!("{head}{APP_SHELL}{PAGE_TAIL}")
}

/// The page a Markdown document opens in when the preview pane sends it to a tab of its own (#271):
/// mdrev's viewer as the whole page. STATIC but for the pinned version — `markdown-page.js` reads
/// the document it names from its own address, so nothing a URL carries is ever written into HTML,
/// and every request it makes goes to the same `api/mdrev/` routes, under the same guards, as the
/// pane's.
pub fn markdown_page() -> String {
    include_str!("codex-ui/markdown-page.html").replace(
        "{{MDREV}}",
        claude_replay_html::mdrev_version().unwrap_or(""),
    )
}

/// The page an image opens in when the preview pane sends it to a tab of its own (#s11): the image
/// as the whole window, fit and zoomable. STATIC — `image-page.js` reads the image it names from its
/// own address — and it reads only what the pane was offered: a file through `/file` with the
/// stamp the page minted, or an embedded image handed over through sessionStorage.
pub fn image_page() -> &'static str {
    include_str!("codex-ui/image-page.html")
}

/// The monitor's tab icon (#203): the green list mark, served here so the app shell — extracted
/// byte-for-byte from the hand-written demo, which declares no `<link rel="icon">` — gets it from
/// the production page head's link, and the browser's automatic `/favicon.ico` request resolves
/// instead of 404ing.
pub const FAVICON_SVG: &str = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'><rect width='32' height='32' rx='7' fill='#2e7d55'/><g fill='#fff'><circle cx='9' cy='10' r='2'/><rect x='13' y='9' width='11' height='2' rx='1'/><circle cx='9' cy='16' r='2'/><rect x='13' y='15' width='11' height='2' rx='1'/><circle cx='9' cy='22' r='2'/><rect x='13' y='21' width='11' height='2' rx='1'/></g></svg>";

/// The page as an app on a phone's Home Screen, or installed from a browser (#320, the owner: "add
/// an app-icon for the page so when saving the page to my Home Screen, I can identify it
/// clearly"). The icons are the tab icon's mark, square and opaque — a phone masks the corners
/// itself and draws any transparency black — rendered from `icons/app-icon.svg` by
/// `scripts/render-app-icon.sh`. `display: browser` is deliberate: an iOS Home Screen app that
/// runs standalone keeps cookies of its own, so it would open unpaired; in the browser the
/// shortcut reaches the page the phone already paired.
pub const MANIFEST: &str = r##"{
  "name": "Agent Monitor",
  "short_name": "Agent Monitor",
  "start_url": "/",
  "display": "browser",
  "icons": [
    { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png" }
  ]
}
"##;

/// What a device may read before, or without, pairing (#320): the icons and the manifest, which
/// say nothing of the machine. A phone fetches the icon on its own when the page is saved to its
/// Home Screen, and iOS asks the root for `apple-touch-icon.png` (and its `-precomposed` twin)
/// on its own. The monitor hands this to its gate (`AuthGate::with_public`); every name here is
/// served by [`asset`].
pub const PUBLIC_ASSETS: &[&str] = &[
    "favicon.svg",
    "favicon.ico",
    "apple-touch-icon.png",
    "apple-touch-icon-precomposed.png",
    "icon-192.png",
    "icon-512.png",
    "manifest.webmanifest",
];

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
        // The tab icon under both names the browser may ask for (#203).
        "favicon.svg" | "favicon.ico" => ("image/svg+xml", FAVICON_SVG.as_bytes()),
        // The Home Screen and install icons, and the manifest that names them (#320).
        "apple-touch-icon.png" | "apple-touch-icon-precomposed.png" => (
            "image/png",
            include_bytes!("icons/apple-touch-icon.png").as_slice(),
        ),
        "icon-192.png" => ("image/png", include_bytes!("icons/icon-192.png").as_slice()),
        "icon-512.png" => ("image/png", include_bytes!("icons/icon-512.png").as_slice()),
        "manifest.webmanifest" => ("application/manifest+json", MANIFEST.as_bytes()),
        // The page's three stylesheets (#s429 stage 5b, #s430 stage 3), all derived from
        // reference.css and production.css by the extraction script: the app's chrome and the page's,
        // then the session list's, every rule of which hangs on `#sessionList`, then the session
        // view's, every rule of which hangs on `#sessionView`. The page links them in that order.
        "monitor-ui/app-shell.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-ui/app-shell.css").as_slice(),
        ),
        "monitor-ui/session-list.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-ui/session-list.css").as_slice(),
        ),
        "monitor-ui/session-view.css" => (
            "text/css; charset=utf-8",
            include_bytes!("codex-ui/session-view.css").as_slice(),
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
        // ⌘K's own query (#294, decoupled from the session grammar 2026-09-28).
        "monitor-ui/jump-query.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/jump-query.js").as_slice(),
        ),
        "monitor-ui/record-store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/record-store.js").as_slice(),
        ),
        "monitor-ui/session-index-store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-index-store.js").as_slice(),
        ),
        "monitor-ui/session-list.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-list.js").as_slice(),
        ),
        // The session list as a component (#s430, design/session-list-component.md, stage 2).
        "monitor-ui/session-list-view.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-list-view.js").as_slice(),
        ),
        // The list's markup, generated from the demo (stage 3).
        "monitor-ui/session-list-markup.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-list-markup.js").as_slice(),
        ),
        "monitor-ui/view-model.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/view-model.js").as_slice(),
        ),
        "monitor-ui/backend.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/backend.js").as_slice(),
        ),
        "monitor-ui/components.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/components.js").as_slice(),
        ),
        "monitor-ui/viewport.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/viewport.js").as_slice(),
        ),
        "monitor-ui/view-state.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/view-state.js").as_slice(),
        ),
        "monitor-ui/pane-layout.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/pane-layout.js").as_slice(),
        ),
        "monitor-ui/view-root.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/view-root.js").as_slice(),
        ),
        "monitor-ui/session-view.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-view.js").as_slice(),
        ),
        "monitor-ui/session-view-markup.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/session-view-markup.js").as_slice(),
        ),
        "monitor-ui/native-policy.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/native-policy.js").as_slice(),
        ),
        "monitor-ui/native-menu.js" => (
            "application/javascript; charset=utf-8",
            include_bytes!("codex-ui/native-menu.js").as_slice(),
        ),
        "monitor-ui/native-mode.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/native-mode.js").as_slice(),
        ),
        "monitor-ui/phone-keyboard.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/phone-keyboard.js").as_slice(),
        ),
        "monitor-ui/control-store.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/control-store.js").as_slice(),
        ),
        "monitor-ui/dialog-card.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/dialog-card.js").as_slice(),
        ),
        "monitor-ui/mdrev-pane.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/mdrev-pane.js").as_slice(),
        ),
        "monitor-ui/markdown-page.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/markdown-page.js").as_slice(),
        ),
        "monitor-ui/image-page.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("codex-ui/image-page.js").as_slice(),
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

    /// A PNG's width, height and colour type, read from its IHDR chunk.
    fn png_header(bytes: &[u8]) -> (u32, u32, u8) {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
        assert_eq!(&bytes[12..16], b"IHDR", "IHDR first");
        let be = |at: usize| {
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        (be(16), be(20), bytes[25])
    }

    /// #320: every path a device may read unpaired is one this table serves — a name in
    /// `PUBLIC_ASSETS` with no asset behind it would be a hole the gate opens onto nothing, and the
    /// icons are what they claim: the size the name and the manifest give, and OPAQUE (colour type
    /// 2, RGB — no alpha channel at all), because iOS draws a Home Screen icon's transparency black.
    #[test]
    fn the_home_screen_icons_are_served_opaque_at_their_sizes() {
        for name in PUBLIC_ASSETS {
            let served = asset(name).unwrap_or_else(|| panic!("{name} is public but not served"));
            assert!(served.code.starts_with("200"), "{name}");
        }
        for (name, size) in [
            ("apple-touch-icon.png", 180),
            ("apple-touch-icon-precomposed.png", 180),
            ("icon-192.png", 192),
            ("icon-512.png", 512),
        ] {
            let icon = asset(name).unwrap();
            assert_eq!(icon.content_type, "image/png", "{name}");
            assert_eq!(
                png_header(&icon.body),
                (size, size, 2),
                "{name}: square, {size}px, opaque RGB"
            );
        }
        let manifest: serde_json::Value =
            serde_json::from_str(MANIFEST).expect("the manifest is JSON");
        assert_eq!(manifest["name"], "Agent Monitor");
        assert_eq!(
            manifest["display"], "browser",
            "an iOS standalone app keeps cookies of its own and would open unpaired"
        );
        let icons = manifest["icons"].as_array().expect("icons");
        assert!(!icons.is_empty());
        for icon in icons {
            let src = icon["src"].as_str().unwrap();
            let name = src.strip_prefix('/').unwrap();
            assert!(PUBLIC_ASSETS.contains(&name), "{src} is readable unpaired");
            let size: u32 = icon["sizes"]
                .as_str()
                .unwrap()
                .split('x')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                png_header(&asset(name).unwrap().body).0,
                size,
                "{src} is the size the manifest says"
            );
        }
        assert_eq!(
            asset("manifest.webmanifest").unwrap().content_type,
            "application/manifest+json"
        );
    }

    /// #320: the app shell's page names its icons, its manifest and a short Home Screen label — and
    /// asks for no standalone mode, which on iOS would open with cookies of its own, unpaired.
    #[test]
    fn the_page_names_its_home_screen_icon() {
        let page = page("test", false);
        for tag in [
            r#"<link rel="icon" type="image/svg+xml" href="favicon.svg">"#,
            r#"<link rel="apple-touch-icon" href="apple-touch-icon.png">"#,
            r#"<link rel="manifest" href="manifest.webmanifest">"#,
            r#"<meta name="apple-mobile-web-app-title" content="Agent Monitor">"#,
        ] {
            assert!(page.contains(tag), "{tag}");
        }
        assert!(!page.contains("apple-mobile-web-app-capable"));
        assert!(!page.contains("mobile-web-app-capable"));
    }

    /// #s426: every page the table serves writes its links relative to itself, so a host that
    /// serves it under a prefix of its own (at `/agents/monitor/`, say) has it load its modules,
    /// styles and icons from under that prefix, with nothing to rewrite.
    #[test]
    fn no_page_writes_a_root_path() {
        for (name, html) in [
            ("the app shell", page("test", false)),
            ("the paired app shell", page("test", true)),
            ("the Markdown tab", markdown_page()),
            ("the image tab", image_page().to_string()),
        ] {
            for root in [r#"href="/"#, r#"src="/"#] {
                assert!(!html.contains(root), "{name} writes {root}…");
            }
        }
    }

    /// #s429 (design/session-view-component.md, stage 1): the shell's modules find what they draw
    /// in through `view-root.js`, the session view's root or the app's page, never through
    /// `document` itself. In a shadow root a `document` lookup finds nothing the view drew, a
    /// popover on `document.body` takes none of its styles, and `document.activeElement` names the
    /// host's element. Read from the directory, so a new module is held to it too. The tabs' own
    /// pages (`/markdown`, `/image`) are pages, not the view.
    #[test]
    fn the_shell_reaches_the_page_only_through_its_root() {
        const PAGE_READS: &[&str] = &[
            "document.getElementById(",
            "document.querySelector(",
            "document.querySelectorAll(",
            "document.activeElement",
            "document.body",
            "document.documentElement",
            "document.head",
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/codex-ui");
        let mut read = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".js")
                || matches!(
                    name.as_str(),
                    "view-root.js" | "image-page.js" | "markdown-page.js"
                )
            {
                continue;
            }
            read += 1;
            let source = std::fs::read_to_string(&path).unwrap();
            for (n, line) in source.lines().enumerate() {
                for pattern in PAGE_READS {
                    assert!(
                        !line.contains(pattern),
                        "{name}:{} reads {pattern}… directly; go through `viewRoot` or `page` \
                         (view-root.js)",
                        n + 1
                    );
                }
            }
        }
        assert!(read > 20, "read {read} modules from {}", dir.display());
    }

    /// Every style rule's selectors in `css`, one per comma, each with its line, whitespace
    /// collapsed: comments out, the at-rules that hold rules (`@media`, `@supports`, `@container`,
    /// a `@layer` block) read through, other at-rules skipped.
    fn style_selectors(css: &str) -> Vec<(usize, String)> {
        let mut text = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(at) = rest.find("/*") {
            text.push_str(&rest[..at]);
            let end = rest[at + 2..].find("*/").map_or(rest.len(), |e| at + 4 + e);
            text.extend(rest[at..end].chars().filter(|&c| c == '\n'));
            rest = &rest[end..];
        }
        text.push_str(rest);
        fn walk(text: &str, base_line: usize, out: &mut Vec<(usize, String)>) {
            let mut i = 0;
            while let Some(open) = text[i..].find('{').map(|at| i + at) {
                let (mut depth, mut close) = (1, open + 1);
                for (k, c) in text[open + 1..].char_indices() {
                    depth += match c {
                        '{' => 1,
                        '}' => -1,
                        _ => 0,
                    };
                    if depth == 0 {
                        close = open + 1 + k;
                        break;
                    }
                }
                let head = &text[i..open];
                let lead = head.len() - head.trim_start().len();
                let line = base_line + text[..i + lead].matches('\n').count();
                let prelude = head.trim();
                if ["@media", "@supports", "@container", "@layer"]
                    .iter()
                    .any(|at| prelude.starts_with(at))
                {
                    let inner = base_line + text[..open + 1].matches('\n').count();
                    walk(&text[open + 1..close], inner, out);
                } else if !prelude.starts_with('@') {
                    let (mut depth, mut start) = (0, 0);
                    let mut push = |piece: &str| {
                        out.push((line, piece.split_whitespace().collect::<Vec<_>>().join(" ")));
                    };
                    for (k, c) in prelude.char_indices() {
                        match c {
                            '(' | '[' => depth += 1,
                            ')' | ']' => depth -= 1,
                            ',' if depth == 0 => {
                                push(&prelude[start..k]);
                                start = k + 1;
                            }
                            _ => {}
                        }
                    }
                    push(&prelude[start..]);
                }
                i = close + 1;
            }
        }
        let mut out = Vec::new();
        walk(&text, 1, &mut out);
        out
    }

    /// The class and id names a selector carries (`.x`, `#x`), wherever they sit in it.
    fn selector_names(selector: &str) -> Vec<&str> {
        let mut names = Vec::new();
        let bytes = selector.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'.' || bytes[i] == b'#' {
                let end = selector[i + 1..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                    .map_or(selector.len(), |e| i + 1 + e);
                if end > i + 1 {
                    names.push(&selector[i..end]);
                }
                i = end.max(i + 1);
            } else {
                i += 1;
            }
        }
        names
    }

    /// A selector's first compound, and what follows it after the combinator.
    fn leading_compound(selector: &str) -> (&str, &str) {
        let mut depth = 0;
        for (k, c) in selector.char_indices() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ' ' | '>' | '+' | '~' if depth == 0 => {
                    return (
                        &selector[..k],
                        selector[k..].trim_start_matches([' ', '>', '+', '~']),
                    )
                }
                _ => {}
            }
        }
        (selector, "")
    }

    /// #s429 stage 5a (design/session-view-component.md): the session view's rules hang on its
    /// own element, `#sessionView`, so they hold in a host's page, where no `#app` surrounds it.
    /// Every selector in production.css that names something OUTSIDE the view — `#app`, `.app`,
    /// `body`, `html`, `:root`, `[data-theme]` — is one of the kinds below, each for its reason. A
    /// new one fails, named with its line, until it hangs on `#sessionView` or is listed here.
    #[test]
    fn the_view_s_styles_hang_on_its_own_element() {
        // The app's own chrome, which the app draws around the view (app-shell.html, app.js): a
        // rule naming one of these is the app's, whatever it hangs on.
        const APP_CHROME: &[&str] = &[
            ".sidebar",
            ".side-head",
            ".head-actions",
            ".brand",
            ".tree",
            ".tree-row",
            ".tree-project-more",
            ".agent-label",
            ".sidebar-mini",
            ".sidebar-resizer",
            "#sidebarCollapse",
            "#collapseBtn",
            "#expandBtn",
            "#filterBtn",
            ".nav",
            ".navbtn",
            ".arrange-strip",
            ".arrange-strip-close",
            ".arranged-view",
            ".section-view-scrim",
            ".section-view-menu",
            ".section-menu-name",
            ".session-filter-options",
            ".drawer-handle",
            ".drawer-scrim",
            ".column-switch",
            ".column-pane",
        ];
        // The app's states, kept on `#app`: the app placing or styling the view from outside —
        // its rail, the drawer, a wide phone's column and its pane, and the artifact pane's state
        // copied onto `#app` for its grid. Stage 6 lays the view out by its own pane's size
        // instead. The list's arrangement is on the list's own element since #s430 stage 1.
        const APP_STATES: &[&str] = &[
            ".sidebar-off",
            ".sidebar-tight",
            ".mobile-detail",
            ".phone-column-off",
            ".column-pane-on",
            ".preview-off",
            ".preview-full",
        ];
        // What `:is(#sessionView,#app)` may reach: elements the app's chrome shares with the view
        // (the glyph button, the session filter's and section menu's popover rows) and the lists
        // a wide phone's column borrows from the outline into the app's sidebar
        // (`PHONE_PANES`, `#columnPane.phone-pane-body`). In the app both hooks match; in a host,
        // `#sessionView` alone.
        const SHARED: &[&str] = &[
            ".iconbtn",
            ".navigator-options",
            ".scope-option",
            ".scope-menu-action",
            ".navbtn",
            ".phone-pane-body",
            ".activity-empty",
            ".activity-empty-show",
            ".outline-agent",
            ".outline-turn-row",
            ".outline-number",
            ".outline-label",
            ".outline-epoch",
            ".outline-peer",
            ".outline-peer-from",
            ".outline-peer-glyph",
            ".outline-quote-marker",
            ".outline-quoted",
            ".outline-reply-count",
            ".outline-turn-pending",
        ];
        const SHARED_HOOK: &str = ":is(#sessionView,#app)";
        // `body`, `html` or `:root` as an element or pseudo-class (never a class like `.body`).
        let bare = |selector: &str, word: &str| {
            let ident = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '#');
            selector.match_indices(word).any(|(at, _)| {
                !selector[..at].ends_with(ident) && !selector[at + word.len()..].starts_with(ident)
            })
        };
        let outside = |names: &[&str], selector: &str| {
            names.iter().any(|n| *n == "#app" || *n == ".app")
                || ["body", "html", ":root"].iter().any(|w| bare(selector, w))
        };
        let css = include_str!("codex-ui/production.css");
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        let selectors = style_selectors(css);
        for (line, selector) in &selectors {
            let mut rest = selector.as_str();
            // The page's theme and its tokens (`:root`, `html`, `[data-theme]`), which the view
            // reads from its own element since stage 5b (no shadow root, #s438); and native
            // mode's mark on `body`, which `backend.js` reads. The rest of such a selector is held
            // to the same rules.
            let (lead, after) = leading_compound(rest);
            if lead.starts_with(":root")
                || lead.starts_with(":where(:root")
                || lead.starts_with("[data-theme")
                || lead == "html"
            {
                *counts.entry("theme and tokens").or_default() += 1;
                rest = after;
            } else if lead == "body[data-shared-codex]" || lead == "body:not([data-shared-codex])" {
                *counts.entry("native mode's mark").or_default() += 1;
                rest = after;
            }
            if rest.is_empty() {
                continue;
            }
            let names = selector_names(rest);
            let (lead, after) = leading_compound(rest);
            let whose = if lead == SHARED_HOOK {
                // …or every `button`, the app's among them (a long press selects no glyph).
                (names.iter().any(|n| SHARED.contains(n)) || bare(after, "button"))
                    .then_some("shared")
            } else if lead.starts_with("#app") || lead.starts_with(".app") {
                if (lead == "#app" || lead == ".app") && after.is_empty() {
                    // The view's tokens set on the app's grid (`--phone-dock-h`, `--phone-glass`,
                    // which embedding's step 6 moves with the pane-sized layout) and the app's own
                    // `--sidebar`. The reading ones (`--code-size`, `--measure`) are the view
                    // element's since stage 6.
                    Some("tokens on the app's grid")
                } else if selector_names(lead).iter().any(|n| APP_STATES.contains(n)) {
                    Some("an app state")
                } else if names.iter().any(|n| APP_CHROME.contains(n)) {
                    Some("the app's chrome")
                } else {
                    None
                }
            } else if !outside(&names, rest) {
                Some(if lead.starts_with("#sessionView") {
                    "the view's"
                } else if lead.starts_with("#sessionList") {
                    // The session list's own element (#s430): its state since stage 1, and its
                    // phone rules since stage 3 (they were led by `#app`).
                    "the list's"
                } else {
                    "hangs on nothing outside"
                })
            } else {
                None
            };
            let Some(whose) = whose else {
                panic!(
                    "production.css:{line}: `{selector}` hangs on the page around the session \
                     view. A rule of the view's hangs on `#sessionView`; one for what a wide \
                     phone's column borrows or the app's chrome shares hangs on `{SHARED_HOOK}` \
                     and names it in SHARED; the app's own names its chrome or its state \
                     (design/session-view-component.md, stage 5a)"
                );
            };
            *counts.entry(whose).or_default() += 1;
        }
        let count = |k: &str| counts.get(k).copied().unwrap_or(0);
        assert!(
            selectors.len() > 1000
                && count("the view's") >= 250
                && count("shared") >= 20
                && count("the app's chrome") >= 15
                && count("the list's") >= 10
                // #s431 stage 2 moved the pane's sideways rules from the app's copy of its state
                // to the view's own: 37 remain.
                && count("an app state") >= 30
                && count("tokens on the app's grid") >= 3,
            "read {} selectors: {counts:?}",
            selectors.len()
        );
    }

    /// #s442: stage 2 moved the view's own states from `#app` to `#sessionView`, and Codex native
    /// mode kept reading two of them, `composing` and `keyboard-up`, on `#app`, where nothing sets
    /// them any more: on a phone the status chip stayed up beside the keyboard, the sheets rose from
    /// the dock's place under the open composer, and the transcript kept room for a dock that the
    /// composer hides. Every class session-view.js sets on its own element is read there by native
    /// mode too: no selector in its stylesheets names one on the app (`#app` or `.app`, outside a
    /// `:has(…)`, which looks below), no `classList` call in its scripts touches one on anything but
    /// the view's element, and no selector string in them names one on the app.
    #[test]
    fn native_mode_reads_the_view_s_states_on_the_view() {
        let view = include_str!("codex-ui/session-view.js");
        let mut states = Vec::new();
        for call in ["toggle", "add", "remove"] {
            for rest in view
                .split(&format!("sessionView.classList.{call}(\""))
                .skip(1)
            {
                let state = rest.split('"').next().unwrap();
                if !states.contains(&state) {
                    states.push(state);
                }
            }
        }
        assert!(
            states.contains(&"composing") && states.contains(&"keyboard-up"),
            "session-view.js sets its states on #sessionView: {states:?}"
        );
        // A compound without its `:has(…)` parts: `#app:has(#sessionView.composing)` reads the
        // state on the view.
        let outside_has = |compound: &str| {
            let (mut out, mut rest) = (String::new(), compound);
            while let Some(at) = rest.find(":has(") {
                out.push_str(&rest[..at]);
                let (mut depth, mut end) = (0, rest.len());
                for (k, c) in rest[at + 4..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = at + 4 + k + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                rest = &rest[end..];
            }
            out.push_str(rest);
            out
        };
        for (name, css) in [
            (
                "codex-workspace/monitor.css",
                include_str!("codex-workspace/monitor.css"),
            ),
            (
                "codex-ui/native-entry.css",
                include_str!("codex-ui/native-entry.css"),
            ),
        ] {
            for (line, selector) in &style_selectors(css) {
                let mut rest = selector.as_str();
                while !rest.is_empty() {
                    let (compound, after) = leading_compound(rest);
                    let compound = outside_has(compound);
                    let names = selector_names(&compound);
                    if let Some(state) = states
                        .iter()
                        .find(|s| names.contains(&format!(".{s}").as_str()))
                    {
                        assert!(
                            !names.contains(&"#app") && !names.contains(&".app"),
                            "{name}:{line}: `{selector}` reads the view's state `{state}` on the \
                             app; the view sets it on #sessionView"
                        );
                    }
                    rest = after;
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut scripts = Vec::new();
        for (dir, prefix) in [("codex-workspace", ""), ("codex-ui", "native-")] {
            for entry in std::fs::read_dir(src.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                let file = path.file_name().unwrap().to_string_lossy().into_owned();
                if file.starts_with(prefix) && file.ends_with(".js") {
                    scripts.push((
                        format!("{dir}/{file}"),
                        std::fs::read_to_string(&path).unwrap(),
                    ));
                }
            }
        }
        assert!(
            scripts.len() >= 8
                && scripts
                    .iter()
                    .any(|(n, _)| n.ends_with("monitor-controls.js")),
            "read {:?}",
            scripts.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        let boundary = |c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_');
        for (name, text) in &scripts {
            for (at, _) in text.match_indices("classList.") {
                let receiver = text[..at].trim_end_matches('.').trim_end_matches('?');
                let state = text[at..]
                    .split_once('(')
                    .and_then(|(_, arg)| arg.get(1..))
                    .and_then(|arg| arg.split(['"', '\'']).next())
                    .unwrap_or("");
                let on_view = [
                    "(\"sessionView\")",
                    "('sessionView')",
                    "sessionView",
                    "viewRoot.element()",
                ]
                .iter()
                .any(|r| receiver.ends_with(r));
                assert!(
                    on_view || !states.contains(&state),
                    "{name}: `…{}.classList.{}` touches the view's state `{state}` on another \
                     element; the view sets it on #sessionView",
                    &receiver[receiver.len().saturating_sub(30)..],
                    text[at + 10..].split(')').next().unwrap_or("")
                );
            }
            for state in &states {
                for app in ["#app", ".app"] {
                    let wrong = format!("{app}.{state}");
                    for (at, _) in text.match_indices(&wrong) {
                        assert!(
                            text[at + wrong.len()..].starts_with(|c: char| !boundary(c)),
                            "{name}: a selector string names the view's state `{state}` on the \
                             app (`{wrong}`); the view sets it on #sessionView"
                        );
                    }
                }
            }
        }
    }

    /// #s429 stage 5b (design/session-view-component.md): the session view's stylesheet names
    /// nothing outside its own element, so a host's page can load it beside its own and lose
    /// nothing to it. Every selector in session-view.css starts at the view's element —
    /// `:where(#sessionView)`, which weighs nothing, or `#sessionView` (stage 5a's rules, which keep
    /// their id) — none names the page or the app around it, and none goes on from the element to
    /// its siblings (`+`, `~`); only a `@keyframes` block is outside a selector. The page loads it
    /// after the app's own, app-shell.css.
    #[test]
    fn the_view_s_stylesheet_names_nothing_outside_its_element() {
        let css = include_str!("codex-ui/session-view.css");
        // `body`, `html` or `:root` as an element or pseudo-class (never a class like `.body`).
        let bare = |selector: &str, word: &str| {
            let ident = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '#');
            selector.match_indices(word).any(|(at, _)| {
                !selector[..at].ends_with(ident) && !selector[at + word.len()..].starts_with(ident)
            })
        };
        let selectors = style_selectors(css);
        for (line, selector) in &selectors {
            // A rule its sources wrote under a size query is gated by the classes the layout module
            // sets on the view's element (#s431 stage 2): `:where(#sessionView.pl-max-w-1180)`,
            // `:where(#sessionView:is(…))`, still the view's element.
            let rooted = [":where(#sessionView)", "#sessionView"].iter().any(|root| {
                selector.strip_prefix(root).is_some_and(|rest| {
                    !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                })
            }) || selector
                .strip_prefix(":where(#sessionView")
                .is_some_and(|rest| rest.starts_with(['.', ':']));
            // The view's element's siblings are no part of the view: nothing goes on from it with
            // `+` or `~` (`:where(#sessionView).pane-open+.x`).
            let (lead, _) = leading_compound(selector);
            let to_siblings = selector[lead.len()..].trim_start().starts_with(['+', '~']);
            let outside = to_siblings
                || selector_names(selector)
                    .iter()
                    .any(|n| matches!(*n, "#app" | ".app"))
                || ["body", "html", ":root"].iter().any(|w| bare(selector, w));
            assert!(
                rooted && !outside,
                "session-view.css:{line}: `{selector}` reaches outside the session view's element. \
                 Every rule of the view's hangs on #sessionView; change the demo \
                 (design/agent-monitor-codex-demo.html) or production.css and run \
                 scripts/extract-agent-monitor-demo.mjs \
                 (design/session-view-component.md, stage 5b)"
            );
        }
        // Every rule of the file is a selector's, read through the at-rules that hold rules, or a
        // keyframe's: nothing else at the top.
        let at_rules: Vec<&str> = css
            .lines()
            .filter(|line| line.starts_with('@'))
            .filter(|line| {
                !["@media", "@supports", "@container", "@layer", "@keyframes"]
                    .iter()
                    .any(|at| line.starts_with(at))
            })
            .collect();
        assert!(at_rules.is_empty(), "session-view.css holds {at_rules:?}");
        assert!(
            selectors.len() > 2000,
            "read {} selectors of session-view.css",
            selectors.len()
        );
        let app = PAGE_HEAD.find(r#"href="monitor-ui/app-shell.css""#);
        let view = PAGE_HEAD.find(r#"href="monitor-ui/session-view.css""#);
        assert!(
            matches!((app, view), (Some(a), Some(v)) if a < v),
            "the page links app-shell.css, then session-view.css"
        );
    }

    /// #s430 stage 3 (design/session-list-component.md): the session list's stylesheet names nothing
    /// outside its own element, as the view's does not, so a host's page can load it beside its own.
    /// Every selector in session-list.css starts at the list's element — `:where(#sessionList)`,
    /// which weighs nothing, or `#sessionList` (the rules that already hung on it) — none names the
    /// page, the app or the view around it, and none goes on from the element to its siblings; its
    /// keyframes are its own (`sl-<name>`). The rules for the parts the app places outside the
    /// element (the controls, the sheet, the rail's parts, the flyout) are prefixed the same way, and
    /// in the app match nothing: their copies in app-shell.css style them there. The page links the
    /// app's file, then the list's, then the view's.
    #[test]
    fn the_list_s_stylesheet_names_nothing_outside_its_element() {
        let css = include_str!("codex-ui/session-list.css");
        let bare = |selector: &str, word: &str| {
            let ident = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '#');
            selector.match_indices(word).any(|(at, _)| {
                !selector[..at].ends_with(ident) && !selector[at + word.len()..].starts_with(ident)
            })
        };
        let selectors = style_selectors(css);
        for (line, selector) in &selectors {
            let rooted = [":where(#sessionList)", "#sessionList"].iter().any(|root| {
                selector.strip_prefix(root).is_some_and(|rest| {
                    !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                })
            });
            let (lead, _) = leading_compound(selector);
            let to_siblings = selector[lead.len()..].trim_start().starts_with(['+', '~']);
            let outside = to_siblings
                || selector_names(selector)
                    .iter()
                    .any(|n| matches!(*n, "#app" | ".app" | "#sessionView" | ".session-view"))
                || ["body", "html", ":root"].iter().any(|w| bare(selector, w));
            assert!(
                rooted && !outside,
                "session-list.css:{line}: `{selector}` reaches outside the session list's element. \
                 Every rule of the list's hangs on #sessionList; change the demo \
                 (design/agent-monitor-codex-demo.html) or production.css and run \
                 scripts/extract-agent-monitor-demo.mjs \
                 (design/session-list-component.md, stage 3)"
            );
        }
        let at_rules: Vec<&str> = css
            .lines()
            .filter(|line| line.starts_with('@'))
            .filter(|line| {
                !["@media", "@supports", "@container", "@layer"]
                    .iter()
                    .any(|at| line.starts_with(at))
            })
            .collect();
        assert!(
            !at_rules.is_empty()
                && at_rules
                    .iter()
                    .all(|line| line.starts_with("@keyframes sl-")),
            "session-list.css holds {at_rules:?}; its keyframes are named sl-<name>"
        );
        assert!(
            selectors.len() > 200,
            "read {} selectors of session-list.css",
            selectors.len()
        );
        // The list's body is styled here: the tree, a row, the strip.
        for name in [".tree", ".tree-row", ".agent-label", ".arrange-strip-box"] {
            assert!(
                selectors
                    .iter()
                    .any(|(_, s)| selector_names(s).contains(&name)),
                "session-list.css styles {name}"
            );
        }
        let links: Vec<usize> = [
            "monitor-ui/app-shell.css",
            "monitor-ui/session-list.css",
            "monitor-ui/session-view.css",
        ]
        .iter()
        .map(|href| {
            PAGE_HEAD
                .find(&format!(r#"href="{href}""#))
                .unwrap_or_else(|| panic!("the page links {href}"))
        })
        .collect();
        assert!(
            links.windows(2).all(|pair| pair[0] < pair[1]),
            "the page links app-shell.css, then session-list.css, then session-view.css"
        );
    }

    /// #s429, #s438: the collaboration app mounts the session view in a pane of its own page and
    /// styles these classes there. Our rules hang on `#sessionView` and cannot reach its elements,
    /// but its rules reach ours by class, so the view names none of them: its own are `sv-<name>`
    /// (`.sv-session-main`, `.sv-task-card` and its family, `.sv-primary`…). A selector of the
    /// view's stylesheet naming one, inside `:not()` too, fails here with its line.
    #[test]
    fn the_view_s_stylesheet_names_no_class_the_collaboration_app_styles() {
        const COLLIDING: &[&str] = &[
            ".session-main",
            ".task-card",
            ".hint",
            ".crumbs",
            ".pinned",
            ".primary",
        ];
        let css = include_str!("codex-ui/session-view.css");
        let selectors = style_selectors(css);
        assert!(selectors.len() > 2000, "read {} selectors", selectors.len());
        for (line, selector) in &selectors {
            let names = selector_names(selector);
            let found: Vec<&&str> = names
                .iter()
                .filter(|n| COLLIDING.contains(n) || n.starts_with(".task-card-"))
                .collect();
            assert!(
                found.is_empty(),
                "session-view.css:{line}: `{selector}` names {found:?}, which the collaboration \
                 app styles in the page the view is mounted in (#s438). Name the view's element \
                 sv-<name> in the demo (design/agent-monitor-codex-demo.html) or production.css \
                 and in the code that draws it, then run scripts/extract-agent-monitor-demo.mjs"
            );
        }
        // The renamed classes are the ones the view's rules name now.
        for renamed in [
            ".sv-session-main",
            ".sv-task-card",
            ".sv-pinned",
            ".sv-primary",
        ] {
            assert!(
                selectors
                    .iter()
                    .any(|(_, s)| selector_names(s).contains(&renamed)),
                "session-view.css styles {renamed}"
            );
        }
    }

    #[test]
    fn page_is_the_reference_shell_without_mock_runtime() {
        let page = page("test", false);
        assert!(page.contains(APP_SHELL));
        // The view's markup is not in the page: the view builds itself from its own (stage 4).
        assert!(page.contains(r#"<div class="session-view preview-off" id="sessionView"></div>"#));
        assert!(!page.contains(r#"<main class="workspace">"#));
        // Nor the list's: it builds itself in an empty `#sessionList` (#s430 stage 3).
        assert!(page.contains(r#"<div class="session-list" id="sessionList"></div>"#));
        assert!(!page.contains(r#"id="tree""#));
        assert!(!page.contains(r#"id="collapseBtn""#));
        assert!(page.contains(r#"src="monitor-ui/app.js""#));
        assert!(!page.contains("sample-transcript-data.js"));
        assert!(!page.contains("REAL_TRANSCRIPT"));
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
            ("native-mode.js", include_str!("codex-ui/native-mode.js")),
            ("native-menu.js", include_str!("codex-ui/native-menu.js")),
            (
                "native-policy.js",
                include_str!("codex-ui/native-policy.js"),
            ),
            (
                "attachment-viewer.js",
                include_str!("codex-ui/attachment-viewer.js"),
            ),
            ("backend.js", include_str!("codex-ui/backend.js")),
            ("components.js", include_str!("codex-ui/components.js")),
            (
                "control-store.js",
                include_str!("codex-ui/control-store.js"),
            ),
            ("dialog-card.js", include_str!("codex-ui/dialog-card.js")),
            ("icons.js", include_str!("codex-ui/icons.js")),
            ("preview.js", include_str!("codex-ui/preview.js")),
            ("jump-query.js", include_str!("codex-ui/jump-query.js")),
            ("mdrev-pane.js", include_str!("codex-ui/mdrev-pane.js")),
            (
                "markdown-page.js",
                include_str!("codex-ui/markdown-page.js"),
            ),
            ("image-page.js", include_str!("codex-ui/image-page.js")),
            ("record-store.js", include_str!("codex-ui/record-store.js")),
            ("sandbox.js", include_str!("codex-ui/sandbox.js")),
            (
                "session-index-store.js",
                include_str!("codex-ui/session-index-store.js"),
            ),
            ("session-list.js", include_str!("codex-ui/session-list.js")),
            (
                "session-list-view.js",
                include_str!("codex-ui/session-list-view.js"),
            ),
            (
                "session-list-markup.js",
                include_str!("codex-ui/session-list-markup.js"),
            ),
            (
                "shared/session-visibility.js",
                claude_replay_html::shared_source("session-visibility").unwrap(),
            ),
            (
                "shared/cost-display.js",
                claude_replay_html::shared_source("cost-display").unwrap(),
            ),
            (
                "shared/fleet.js",
                claude_replay_html::shared_source("fleet").unwrap(),
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
            (
                "shared/image-view.js",
                claude_replay_html::shared_source("image-view").unwrap(),
            ),
            (
                "shared/mermaid.js",
                claude_replay_html::shared_source("mermaid").unwrap(),
            ),
            (
                "shared/inbox.js",
                claude_replay_html::shared_source("inbox").unwrap(),
            ),
            ("state.js", include_str!("codex-ui/state.js")),
            ("view-memory.js", include_str!("codex-ui/view-memory.js")),
            ("view-model.js", include_str!("codex-ui/view-model.js")),
            ("view-root.js", include_str!("codex-ui/view-root.js")),
            ("view-state.js", include_str!("codex-ui/view-state.js")),
            ("pane-layout.js", include_str!("codex-ui/pane-layout.js")),
            ("session-view.js", include_str!("codex-ui/session-view.js")),
            (
                "session-view-markup.js",
                include_str!("codex-ui/session-view-markup.js"),
            ),
            ("viewport.js", include_str!("codex-ui/viewport.js")),
            (
                "phone-keyboard.js",
                include_str!("codex-ui/phone-keyboard.js"),
            ),
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
                                        || l.starts_with(&format!("export async function {local}"))
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

    /// #s32, #s33: ONE phone, in the scripts and in both stylesheets. A phone is `PHONE_QUERY` — 760px,
    /// or a finger on a short side (a phone held sideways, at any width) — and every media query that
    /// names the breakpoint says exactly that. A `min-width` rule either means "not a phone", so each of
    /// its queries also needs `(min-height:501px)` or `(pointer:fine)` — written as `min-width:761px`
    /// alone, one put a landscape phone's drawer back into the page's flow, which pushed the owner's
    /// session view off the screen — or it is a rule FOR a phone held sideways (#s37: the wide phone's
    /// two columns), which says so with `(pointer:coarse) and (max-height:500px)`, and then it is
    /// `WIDE_PHONE_QUERY` exactly.
    #[test]
    fn every_media_query_agrees_on_what_a_phone_is() {
        let body = |name: &str| String::from_utf8(asset(name).unwrap().body).unwrap();
        let capabilities = body("monitor-ui/shared/capabilities.js");
        let phone = capabilities
            .split("const PHONE_QUERY = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("capabilities.js defines PHONE_QUERY")
            .replace(", ", ",");
        let wide_phone = capabilities
            .split("const WIDE_PHONE_QUERY = \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("capabilities.js defines WIDE_PHONE_QUERY")
            .to_string();
        let mut phones = 0;
        let mut wides = 0;
        let mut wide_phones = 0;
        for (name, css) in [
            ("monitor-ui/app-shell.css", body("monitor-ui/app-shell.css")),
            (
                "monitor-ui/session-list.css",
                body("monitor-ui/session-list.css"),
            ),
            (
                "monitor-ui/session-view.css",
                body("monitor-ui/session-view.css"),
            ),
            (
                "monitor-ui/native-entry.css",
                include_str!("codex-ui/native-entry.css").to_owned(),
            ),
            (
                "codex-workspace/monitor.css",
                include_str!("codex-workspace/monitor.css").to_owned(),
            ),
        ] {
            for prelude in css
                .split("@media")
                .skip(1)
                .map(|rest| rest.split('{').next().unwrap())
            {
                let prelude = prelude.trim();
                if prelude.contains("max-width:760px") && !prelude.contains("(hover:hover)") {
                    assert_eq!(prelude, phone, "{name}: a phone rule says what a phone is");
                    phones += 1;
                }
                if prelude.contains("min-width:")
                    && prelude.contains("(pointer:coarse) and (max-height:500px)")
                {
                    assert_eq!(
                        prelude, wide_phone,
                        "{name}: a rule for a wide phone held sideways says what one is"
                    );
                    wide_phones += 1;
                } else if prelude.contains("min-width:") {
                    for query in prelude.split(',') {
                        assert!(
                            query.contains("(min-height:501px)") || query.contains("(pointer:fine)"),
                            "{name}: `{prelude}` is a wide window's rule, and `{query}` also matches a phone held sideways"
                        );
                    }
                    wides += 1;
                }
            }
        }
        // The session view lays itself out by its own size and the screen's pointer (#s431 stage 2,
        // design/pane-layout.md): its sheet asks no size query, and reads a phone as the classes
        // pane-layout.js sets, which the node contract holds to the old query exactly.
        let view = body("monitor-ui/session-view.css");
        for prelude in view
            .split("@media")
            .skip(1)
            .map(|rest| rest.split('{').next().unwrap())
        {
            assert!(
                !prelude.contains("width") && !prelude.contains("height"),
                "session-view.css asks the window `{}`, where it reads its own size",
                prelude.trim()
            );
        }
        assert!(
            view.matches(":where(.pl-max-w-760,.pl-coarse.pl-max-h-500)")
                .count()
                >= 100,
            "session-view.css reads a phone as the layout module's classes"
        );
        assert!(
            phones >= 10 && wides >= 2 && wide_phones >= 1,
            "read the stylesheets: {phones} phone rules, {wides} wide ones, {wide_phones} for a wide phone"
        );
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
            Failed,
            Done,
            Starting,
            Stalled,
            Fleet,
        ];
        let table = claude_replay_html::shared_source("state-labels").expect("registered");
        for reason in all {
            // Exhaustive by construction: a new variant fails to compile here until listed.
            match reason {
                Exited | ExitedMidWork | Question | PlanApproval | QueuedPrompt | Tool
                | Thinking | Permission | EndedQuestion | Error | Failed | Done | Starting
                | Stalled | Fleet => (),
            }
            let key = StateReason::as_str(reason);
            let quoted = format!("\"{key}\":");
            let bare = format!("\n  {key}:");
            assert!(
                table.contains(&quoted) || table.contains(&bare),
                "state-labels.js has no label for the tracker reason `{key}`"
            );
            // #202: and a bucket — active / blocked / idle — in REASON_BUCKETS, the one
            // definition of "needs attention" both of the app shell's tooltips describe.
            let buckets = table
                .split("const REASON_BUCKETS = {")
                .nth(1)
                .and_then(|t| t.split("};").next())
                .expect("state-labels.js declares REASON_BUCKETS");
            assert!(
                buckets.contains(&format!("{key}: \""))
                    || buckets.contains(&format!("\"{key}\": \"")),
                "state-labels.js REASON_BUCKETS has no bucket for the tracker reason `{key}`"
            );
        }
    }
}
