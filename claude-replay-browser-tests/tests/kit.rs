//! The kit (#s432, design/embedding.md, "Delivery"): a page outside this repository mounts both
//! components from the kit alone and reads a session.

mod harness;

use harness::{base, serial, Answer, Kind, Monitor, Stores};
use std::time::Duration;

const SID: &str = "cccccccc-0000-4000-8000-000000000432";

/// A host's page, written from the kit's guide alone: it links the kit's two stylesheets, names the
/// Mermaid runtime the guide names, gives each part an empty element with the part's own id, imports
/// nothing but the kit's entry, and wires the list and the view to each other as the guide's
/// section 6 says. It knows nothing of the monitor's page.
const HOST: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>A kit host</title>
<meta name="mermaid-runtime" content="{{MERMAID}}">
<link rel="stylesheet" href="/kit/bundle/session-list.css">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
<style>
.host-sessions { position: fixed; top: 0; left: 0; bottom: 0; width: 300px; display: flex; }
.host-sessions > #sessionList { flex: 1 1 auto; }
.host-pane { position: fixed; top: 0; left: 300px; right: 0; bottom: 0; }
.host-pane > #sessionView { position: absolute; inset: 0; }
</style>
</head>
<body>
<div class="host-sessions"><div id="sessionList"></div></div>
<div class="host-pane" id="pane"><div id="sessionView"></div></div>
<script type="module">
const host = window.__host = { mounted: false, error: null, opened: [], titles: [], errors: [] };
addEventListener("error", event => host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
try {
  const kit = await import("/kit/bundle/index.js");
  host.exports = Object.keys(kit).sort();
  const { backendHandle, mountSessionList, mountSessionView, persist, useBackend } = kit;
  useBackend(backendHandle({ paired: true }));
  let view = null;
  const show = id => { list.select(id); view.select(id); view.open(id); };
  const list = window.__list = mountSessionList(document.getElementById("sessionList"), {
    persist,
    onOpen: ({ id }) => { host.opened.push(id); show(id); },
    onIndex: () => view && view.indexChanged(),
  });
  view = window.__view = mountSessionView(document.getElementById("sessionView"), {
    grid: document.getElementById("pane"),
    selectedId: () => list.selected(),
    sessionRow: id => list.row(id),
    sessionWorkers: id => list.workers(id),
    selectSession: id => show(id),
    renderList: () => list.redraw(),
    refreshIndex: () => list.refresh(),
    pauseIndex: () => list.stop(),
    resumeIndex: () => list.start(),
    onTitle: ({ title }) => host.titles.push(title),
  });
  list.start();
  host.mounted = true;
} catch (error) {
  host.error = String(error && error.stack || error);
}
</script>
</body>
</html>
"#;

/// The kit as a host serves it: every file of its `bundle/` at `/kit/bundle/…` on the monitor's
/// origin, answered by the browser itself from what `kit::files` lays out, so nothing the case loads
/// comes from the monitor's own `/monitor-ui/`.
fn kit_answers() -> Vec<Answer> {
    let files = claude_monitor::kit::files().expect("the kit is laid out");
    let mut answers: Vec<Answer> = files
        .into_iter()
        .filter_map(|(path, bytes)| {
            let name = path.strip_prefix("bundle/")?;
            let content_type = match name.rsplit_once('.')?.1 {
                "js" => "text/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                _ => return None,
            };
            Some(Answer {
                pattern: format!("*/kit/bundle/{name}*"),
                path: format!("/kit/{path}"),
                content_type: content_type.into(),
                body: String::from_utf8(bytes).expect("the kit's bundle is text"),
            })
        })
        .collect();
    answers.push(Answer {
        pattern: "*/kit-host*".into(),
        path: "/kit-host".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: HOST.replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path()),
    });
    answers
}

/// A page outside this repository (the case's own, written from the guide) mounts the session list
/// and the session view from the kit alone, served under a path of its own: the list fills from the
/// monitor's index, a real click on the session's row reaches the host through `onOpen`, the host
/// opens it in the view, and the view reads the session's records and draws its turns. Every script
/// and stylesheet the page loaded is the kit's, and none came from the monitor's `/monitor-ui/`.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_page_mounts_both_components_from_the_kit_alone_and_reads_a_session() {
    let _serial = serial();
    let base = base("kit-host");
    let stores = Stores::new(&base);
    stores.claude_session(
        SID,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::at("00:01"),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::at("00:02"),
        )),
    );
    let monitor = Monitor::spawn(Kind::V1, 3311, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    harness::answer_in_browser(&tab, kit_answers());
    tab.navigate_to(&monitor.url("kit-host")).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error))",
        "the kit host to mount both parts",
        Duration::from_secs(20),
        "JSON.stringify(window.__host || null)",
    );
    let mounted = harness::probe(
        &tab,
        "({ error: window.__host.error, exports: window.__host.exports, list: !!document.querySelector('.host-sessions > #sessionList') && document.getElementById('sessionList').classList.contains('session-list'), view: !!document.querySelector('.host-pane > #sessionView #transcript') })",
    );
    let row = format!("#sessionList .tree-row.session[data-session=\"{SID}\"]");
    harness::until(
        &tab,
        &format!("!!document.querySelector('{}')", row.replace('\'', "\\'")),
        "the session listed in the kit's list",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText })",
    );
    tab.find_element(&row).unwrap().click().unwrap();
    harness::until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('past the second dune')",
        "the kit's view to read the session the list chose",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, transcript: (document.getElementById('transcript') || {}).innerText.slice(0, 300) })",
    );
    let read = harness::probe(
        &tab,
        "({ opened: window.__host.opened, session: window.__view.session(), selected: window.__list.selected(), prompt: document.getElementById('transcript').innerText.includes('where do the terns nest'), errors: window.__host.errors })",
    );
    let loaded = harness::probe(
        &tab,
        "(function () { var names = performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }); return { monitorUi: names.filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }), notKit: names.filter(function (p) { return /\\.(js|css)$/.test(p) && p.indexOf('/kit/bundle/') !== 0; }), kit: names.filter(function (p) { return p.indexOf('/kit/bundle/') === 0; }).length }; })()",
    );
    drop(monitor);
    assert_eq!(
        mounted,
        serde_json::json!({
            "error": null,
            "exports": ["backendHandle", "mountSessionList", "mountSessionView", "persist", "useBackend"],
            "list": true,
            "view": true,
        }),
        "the page imports the kit's entry, which gives both parts, and mounts them"
    );
    assert_eq!(
        read,
        serde_json::json!({ "opened": [SID], "session": SID, "selected": SID, "prompt": true, "errors": [] }),
        "the list's choice reached the host, which opened it in the view; the view read both turns"
    );
    assert_eq!(
        loaded["monitorUi"],
        serde_json::json!([]),
        "nothing came from the monitor's /monitor-ui/: {loaded}"
    );
    assert_eq!(
        loaded["notKit"],
        serde_json::json!([]),
        "every script and stylesheet is the kit's: {loaded}"
    );
    let modules = claude_monitor::kit::modules().unwrap().len();
    assert!(
        loaded["kit"].as_u64().unwrap_or(0) as usize > modules,
        "the page loaded the kit's entry and its {modules} modules: {loaded}"
    );
}

/// A page of the native app's kind (#s551, design/app-mount.md, stage 2), written from the design alone,
/// since the guide offers none of it: the app's three stylesheets in order, the viewport meta with
/// `viewport-fit=cover`, the Mermaid runtime the view reads, an empty wrapper, and the kit's second
/// entry. Before it mounts, it makes two calls the mount must refuse (an option no stage takes yet, and
/// no backend); then it mounts with a server and a version of its own, which its body does not carry:
/// a paired server where the page's own handle would say unpaired, whose every `fetch` it counts, and
/// a version the page names nowhere. Then a second mount, which must throw.
const APP_HOST: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<title>The web app from the kit</title>
<meta name="mermaid-runtime" content="{{MERMAID}}">
<link rel="stylesheet" href="/kit/bundle/app-shell.css">
<link rel="stylesheet" href="/kit/bundle/session-list.css">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
</head>
<body>
<div id="appRoot"></div>
<script type="module">
const host = window.__host = { mounted: false, error: null, refused: [], again: null, asks: [], errors: [] };
addEventListener("error", event => host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
const refusal = run => { try { run(); return null; } catch (error) { return String(error && error.message || error); } };
try {
  const native = await import("/kit/bundle/native-app.js");
  host.exports = Object.keys(native).sort();
  const { backendHandle } = await import("/kit/bundle/index.js");
  const server = backendHandle({ paired: true });
  const backend = { ...server, fetch: (path, init) => { host.asks.push(String(path).split("?")[0]); return server.fetch(path, init); } };
  const root = document.getElementById("appRoot");
  host.refused.push(refusal(() => native.mountApp(root, { backend, machines: [] })));
  host.refused.push(refusal(() => native.mountApp(root, { version: "9.9.9-kit" })));
  native.mountApp(root, { backend, version: "9.9.9-kit" });
  host.again = refusal(() => native.mountApp(document.createElement("div"), { backend }));
  host.mounted = true;
} catch (error) {
  host.error = String(error && error.stack || error);
}
</script>
</body>
</html>
"#;

/// #s551 stage 2 — the web app mounted from the kit alone (its second entry, `native-app.js`), on a page
/// of its own: a refused call draws nothing and the next is taken; the app's shell is drawn into the
/// wrapper, which draws no box, so `#app` fills the window as on the monitor's page; the list fills
/// from the mount's own server and the mount opens its first session in the view, which reads it;
/// the brand names the mount's version and write mode reads the mount's server as paired, where the
/// page's body says neither; a second mount throws; and every script and stylesheet the page loaded
/// is the kit's.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_web_app_mounts_from_the_kits_second_entry_with_its_own_server_and_version() {
    let _serial = serial();
    let base = base("kit-app-host");
    let stores = Stores::new(&base);
    stores.claude_session(
        SID,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::rfc3339_secs_ago(120),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::rfc3339_secs_ago(60),
        )),
    );
    let monitor = Monitor::spawn(Kind::V1, 3314, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    let mut answers = kit_answers();
    answers.push(Answer {
        pattern: "*/native-app-host*".into(),
        path: "/native-app-host".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: APP_HOST.replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path()),
    });
    harness::answer_in_browser(&tab, answers);
    tab.navigate_to(&monitor.url("native-app-host")).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error))",
        "the page to mount the web app from the kit",
        Duration::from_secs(20),
        "JSON.stringify(window.__host || null)",
    );
    harness::until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('past the second dune')",
        "the mount to open its first session and the view to read it",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText, transcript: (document.getElementById('transcript') || {}).innerText.slice(0, 300) })",
    );
    let mounted = harness::probe(
        &tab,
        "(function () { var root = document.getElementById('appRoot'), app = document.getElementById('app'), r = app.getBoundingClientRect(); return { error: window.__host.error, exports: window.__host.exports, refused: window.__host.refused, again: window.__host.again, wrapper: root.className, contents: getComputedStyle(root).display, children: [].map.call(root.children, function (e) { return e.id; }), fills: Math.round(r.left) === 0 && Math.round(r.top) === 0 && Math.round(r.width) === innerWidth && Math.round(r.height) === innerHeight, brand: document.querySelector('.brand small').textContent, address: new URLSearchParams(location.search).get('session') }; })()",
    );
    let row = harness::probe(
        &tab,
        &format!(
            "!!document.querySelector('#sessionList .tree-row.session[data-session=\"{SID}\"]')"
        ),
    );
    let asks_before_write = harness::probe(&tab, "window.__host.asks.slice()");
    let write = harness::probe(
        &tab,
        "(function () { var info = document.getElementById('writeInfo').hidden; document.getElementById('writeSwitch').click(); return { infoHidden: info, on: document.getElementById('writeSwitch').getAttribute('aria-checked'), help: !document.getElementById('writeHelp').classList.contains('show') }; })()",
    );
    let loaded = harness::probe(
        &tab,
        "(function () { var names = performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }); return { monitorUi: names.filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }), notKit: names.filter(function (p) { return /\\.(js|css)$/.test(p) && p.indexOf('/kit/bundle/') !== 0; }), mount: names.indexOf('/kit/bundle/app-mount.js') >= 0 && names.indexOf('/kit/bundle/app-shell-markup.js') >= 0 && names.indexOf('/kit/bundle/app-shell.css') >= 0, errors: window.__host.errors }; })()",
    );
    drop(monitor);
    assert_eq!(
        mounted,
        serde_json::json!({
            "error": null,
            "exports": ["mountApp"],
            "refused": [
                "mountApp takes no option named machines; its options are backend, version",
                "mountApp needs its backend: the server the web app reads, as a backend handle (backendHandle())",
            ],
            "again": "mountApp draws the web app once per page, and this page holds it already",
            "wrapper": "app-mount",
            "contents": "contents",
            "children": ["app", "searchLayer", "confirmLayer", "toast"],
            "fills": true,
            "brand": "v9.9.9-kit",
            "address": SID,
        }),
        "the kit's second entry gives the mount alone, which refuses before it draws, draws the app's \
         shell into the wrapper, opens the first session and names the mount's version"
    );
    assert_eq!(
        row,
        serde_json::json!(true),
        "the session is listed in the mount's list"
    );
    let asks: Vec<String> = serde_json::from_value(asks_before_write).unwrap();
    assert!(
        asks.iter().any(|path| path == "/api/sessions"),
        "the list reads its index through the mount's own server: {asks:?}"
    );
    assert!(
        asks.iter().any(|path| path == "/pull"),
        "and the view its session's records: {asks:?}"
    );
    assert_eq!(
        write,
        serde_json::json!({ "infoHidden": true, "on": "true", "help": true }),
        "write mode reads the mount's server as paired, where the page's body says nothing"
    );
    assert_eq!(
        loaded,
        serde_json::json!({ "monitorUi": [], "notKit": [], "mount": true, "errors": [] }),
        "every script and stylesheet is the kit's, the mount and the app's stylesheet among them"
    );
}

/// The session of the list's own server in [`a_list_given_a_server_of_its_own_keeps_to_it_beside_a_view_on_another`]:
/// a session the page's monitor does not hold.
const SID_LISTED: &str = "cccccccc-0000-4000-8000-000000000540";

/// A host that gives each part a server of its own (#s540, Hong's choice on #s539: each part keeps
/// its own server): the list reads one under `/list-server`, the view the page's own monitor. The view
/// is mounted after the list, with its `backend`, which on its own points the page at the monitor.
const HOST_APART: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>A kit host with two servers</title>
<meta name="mermaid-runtime" content="{{MERMAID}}">
<link rel="stylesheet" href="/kit/bundle/session-list.css">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
<style>
.host-sessions { position: fixed; top: 0; left: 0; bottom: 0; width: 300px; display: flex; }
.host-sessions > #sessionList { flex: 1 1 auto; }
.host-pane { position: fixed; top: 0; left: 300px; right: 0; bottom: 0; }
.host-pane > #sessionView { position: absolute; inset: 0; }
</style>
</head>
<body>
<div class="host-sessions"><div id="sessionList"></div></div>
<div class="host-pane" id="pane"><div id="sessionView"></div></div>
<script type="module">
const host = window.__host = { mounted: false, error: null, errors: [] };
addEventListener("error", event => host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
try {
  const { backendHandle, mountSessionList, mountSessionView } = await import("/kit/bundle/index.js");
  const list = window.__list = mountSessionList(document.getElementById("sessionList"), {
    backend: backendHandle({ base: "/list-server", paired: true }),
  });
  let chosen = "";
  const view = window.__view = mountSessionView(document.getElementById("sessionView"), {
    grid: document.getElementById("pane"),
    backend: backendHandle({ paired: true }),
    selectedId: () => chosen,
    sessionRow: () => null,
    sessionWorkers: () => [],
    selectSession: () => {},
    renderList: () => {},
    refreshIndex: () => {},
    pauseIndex: () => {},
    resumeIndex: () => {},
  });
  list.start();
  chosen = "{{VIEWED}}";
  view.select("{{VIEWED}}");
  view.open("{{VIEWED}}");
  host.mounted = true;
} catch (error) {
  host.error = String(error && error.stack || error);
}
</script>
</body>
</html>
"#;

/// #s540 — a list given a server of its own keeps to it beside a view on another. The page's
/// monitor holds one session, which the view reads; the list's server, answered at `/list-server`
/// on the page's origin, holds another. The list shows its own server's session and polls it again
/// at its pace, and never asks the page's monitor for its sessions, although the view's `backend`
/// points the page there. Red before #s540: the list refused the option.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_list_given_a_server_of_its_own_keeps_to_it_beside_a_view_on_another() {
    let _serial = serial();
    let base = base("kit-apart");
    let stores = Stores::new(&base);
    stores.claude_session(
        SID,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::at("00:01"),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::at("00:02"),
        )),
    );
    let monitor = Monitor::spawn(Kind::V1, 3312, &base, Some(&stores), true);
    // The list's server: a second hermetic monitor holding a session the page's does not. Its own
    // answer to /api/sessions, read here, is what the page is answered at `/list-server`, since a
    // part reads its server on the page's origin (the guide's section 2: a host relays it there).
    let other_base = base.join("list-server");
    let other_stores = Stores::new(&other_base);
    other_stores.claude_session(
        SID_LISTED,
        &(harness::user_at("count the skua burrows on the ridge", &harness::at("00:01"))
            + &harness::assistant_at("forty-one, most of them occupied", &harness::at("00:02"))),
    );
    let other = Monitor::spawn(Kind::V1, 3313, &other_base, Some(&other_stores), true);
    let index = sessions_of(&other);
    assert!(
        index.contains(SID_LISTED) && !index.contains(SID),
        "the list's server lists its own session alone: {index}"
    );
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    let mut answers: Vec<Answer> = kit_answers()
        .into_iter()
        .filter(|answer| answer.path != "/kit-host")
        .collect();
    answers.push(Answer {
        pattern: "*/kit-apart*".into(),
        path: "/kit-apart".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: HOST_APART
            .replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path())
            .replace("{{VIEWED}}", SID),
    });
    answers.push(Answer {
        pattern: "*/list-server/api/sessions*".into(),
        path: "/list-server/api/sessions".into(),
        content_type: "application/json".into(),
        body: index,
    });
    harness::answer_in_browser(&tab, answers);
    tab.navigate_to(&monitor.url("kit-apart")).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error))",
        "the host to mount both parts",
        Duration::from_secs(20),
        "JSON.stringify(window.__host || null)",
    );
    let error = harness::probe(&tab, "window.__host.error");
    assert_eq!(
        error,
        serde_json::Value::Null,
        "the host mounts the list with a server of its own"
    );
    let listed = format!("#sessionList .tree-row.session[data-session=\"{SID_LISTED}\"]");
    harness::until(
        &tab,
        &format!("!!document.querySelector('{listed}')"),
        "the list's own server's session in the list",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText })",
    );
    harness::until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('past the second dune')",
        "the view to read the page's monitor",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, transcript: (document.getElementById('transcript') || {}).innerText.slice(0, 300) })",
    );
    // The list polls every 5 seconds: wait for a second ask of its own server, after the view's
    // backend had pointed the page at the monitor.
    let asks = "performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; })";
    harness::until(
        &tab,
        &format!("{asks}.filter(function (p) {{ return p === '/list-server/api/sessions'; }}).length >= 2"),
        "the list to ask its own server again",
        Duration::from_secs(20),
        &format!("JSON.stringify({asks})"),
    );
    let read = harness::probe(
        &tab,
        &format!(
            "(function () {{ var names = {asks}; return {{ own: names.filter(function (p) {{ return p === '/list-server/api/sessions'; }}).length >= 2, page: names.filter(function (p) {{ return p === '/api/sessions'; }}), listedHere: !!document.querySelector('#sessionList .tree-row.session[data-session=\"{SID}\"]'), session: window.__view.session(), errors: window.__host.errors }}; }})()"
        ),
    );
    drop(monitor);
    drop(other);
    assert_eq!(
        read,
        serde_json::json!({ "own": true, "page": [], "listedHere": false, "session": SID, "errors": [] }),
        "the list asked only its own server, twice, and listed only its session; the view read the page's monitor"
    );
}

/// A monitor's own answer to `GET /api/sessions`, asked as a paired reader (its token in the
/// legacy cookie, which the gate still reads), over a plain socket.
fn sessions_of(monitor: &Monitor) -> String {
    use std::io::{Read, Write};
    let token = monitor.token().expect("the list's server is paired");
    for _ in 0..60 {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", monitor.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET /api/sessions HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: cmauth={token}\r\nConnection: close\r\n\r\n",
            monitor.port
        )
        .unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        let reply = String::from_utf8_lossy(&reply).into_owned();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap_or((&reply, ""));
        assert!(
            head.starts_with("HTTP/1.1 200"),
            "the list's server answers its sessions: {head}"
        );
        assert!(
            !head
                .to_ascii_lowercase()
                .contains("transfer-encoding: chunked"),
            "a whole body: {head}"
        );
        if body.contains(SID_LISTED) {
            return body.to_string();
        }
        // The first scan may not have listed the session yet.
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("the list's server never listed {SID_LISTED}");
}
