//! The kit (#s432, design/embedding.md, "Delivery"): a page outside this repository mounts both
//! components from the kit alone and reads a session.

mod harness;

use harness::{at, base, eval, serial, until, Answer, Kind, Monitor, Stores};
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
const host = window.__host = { mounted: false, error: null, opened: [], titles: [], errors: [], loops: [], tabs: [] };
// A tab the view opens is recorded, never opened: the case loads each address itself (#s433).
window.open = url => { host.tabs.push(String(url)); return null; };
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? host.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
try {
  const kit = await import("/kit/bundle/index.js");
  host.exports = Object.keys(kit).sort();
  const { backendHandle, mountSessionList, mountSessionView, persist, useBackend } = kit;
  useBackend(backendHandle({ paired: true, mdrevVersion: "{{MDREV}}" }));
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
    {{VIEW_OPTIONS}}
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

/// A host's page for a tab of its own (#s433), as the guide's section 7 writes one: the kit's
/// `mountDetached` on the page's own address, at a route of the host's.
const TAB: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>A kit tab</title>
<meta name="mermaid-runtime" content="{{MERMAID}}">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
<link rel="stylesheet" href="/kit/bundle/detached.css">
<style>#detachedView { position: fixed; inset: 0; }</style>
</head>
<body>
<div id="detachedView"></div>
<script type="module">
const tab = window.__tab = { mounted: false, error: null, errors: [], loops: [] };
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? tab.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : tab.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => tab.errors.push(String(event.reason && event.reason.message || event.reason)));
try {
  const { backendHandle, mountDetached } = await import("/kit/bundle/index.js");
  const shown = window.__detached = mountDetached(document.getElementById("detachedView"), {
    backend: backendHandle({ paired: true, mdrevVersion: "{{MDREV}}" }),
  });
  tab.kind = shown.kind;
  tab.mounted = true;
} catch (error) {
  tab.error = String(error && error.stack || error);
}
</script>
</body>
</html>
"#;

/// The kit as a host serves it: every file of its `bundle/` at `/kit/bundle/…` on the monitor's
/// origin, answered by the browser itself from what `kit::files` lays out, so nothing the case loads
/// comes from the monitor's own `/monitor-ui/`.
fn kit_answers(view_options: &str) -> Vec<Answer> {
    let mut answers = harness::kit_bundle();
    answers.push(Answer {
        pattern: "*/kit-host*".into(),
        path: "/kit-host".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: HOST
            .replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path())
            .replace("{{MDREV}}", claude_monitor::kit::MDREV)
            .replace("{{VIEW_OPTIONS}}", view_options),
    });
    answers.push(Answer {
        pattern: "*/kit-tab*".into(),
        path: "/kit-tab".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: TAB
            .replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path())
            .replace("{{MDREV}}", claude_monitor::kit::MDREV),
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
    harness::answer_in_browser(&tab, kit_answers(""));
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
            "exports": ["backendHandle", "mergedBackend", "mountDetached", "mountSessionList", "mountSessionView", "persist", "splitMachineId", "useBackend"],
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
    // The modules the first entry imports: the second entry's (the app's mount) are not this page's.
    let modules = claude_monitor::kit::component_modules().unwrap().len();
    assert!(
        loaded["kit"].as_u64().unwrap_or(0) as usize > modules,
        "the page loaded the kit's entry and its {modules} modules: {loaded}"
    );
}

/// A page of the native app's kind (#s551, design/app-mount.md, stage 2), written from the design alone,
/// since the guide offers none of it: the app's three stylesheets in order, the viewport meta with
/// `viewport-fit=cover`, the Mermaid runtime the view reads, an empty wrapper, and the kit's second
/// entry. Before it mounts, it makes two calls the mount must refuse (an option no stage takes yet, and
/// no server); then it mounts with a server and a version of its own, which its body does not carry:
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
const host = window.__host = { mounted: false, error: null, refused: [], again: null, asks: [], errors: [], loops: [] };
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? host.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
const refusal = run => { try { run(); return null; } catch (error) { return String(error && error.message || error); } };
try {
  const native = await import("/kit/bundle/native-app.js");
  host.exports = Object.keys(native).sort();
  const { backendHandle } = await import("/kit/bundle/index.js");
  const server = backendHandle({ paired: true });
  const backend = { ...server, fetch: (path, init) => { host.asks.push(String(path).split("?")[0]); return server.fetch(path, init); } };
  const root = document.getElementById("appRoot");
  host.refused.push(refusal(() => native.mountApp(root, { backend, keyboard: "page" })));
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
    let mut answers = kit_answers("");
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
                "mountApp takes no option named keyboard; its options are backend, machines, assets, version, theme, show, onMachines, reviewSheet, frame",
                "mountApp needs its backend: the server the web app reads, as a backend handle (backendHandle()), or the machines it reads",
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

/// A page of the native app's kind over two machines (#s595, design/app-mount.md, stage 3): the kit's
/// second entry, mounted with `machines` and the `assets` the app carries, and no Mermaid runtime in
/// its head (the mount names it). The first machine is the page's own monitor, the second is relayed
/// under `/machine-b`, and the assets under `/app-vendor` (relayed to the first monitor, which serves
/// mdrev's bundle and the runtime, so a load from there is told apart from a load from a machine).
/// Each machine says it pins an mdrev release that does not exist, so a bundle read from a machine
/// would fail. The second is unpaired, the first paired. Every fetch the page makes is recorded.
const APP_MACHINES_HOST: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<title>The web app over two machines</title>
<link rel="stylesheet" href="/kit/bundle/app-shell.css">
<link rel="stylesheet" href="/kit/bundle/session-list.css">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
</head>
<body>
<div id="appRoot"></div>
<script type="module">
const host = window.__host = { mounted: false, error: null, errors: [], loops: [], asked: [] };
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? host.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
const pageFetch = window.fetch.bind(window);
window.fetch = (input, init) => { host.asked.push(new URL(String(input && input.url || input), location.href).href.slice(location.origin.length)); return pageFetch(input, init); };
try {
  const native = await import("/kit/bundle/native-app.js");
  const { backendHandle } = await import("/kit/bundle/index.js");
  native.mountApp(document.getElementById("appRoot"), {
    machines: [
      { machine: "a", label: "tern-mini.tail5e1f.ts.net", handle: backendHandle({ paired: true, mdrevVersion: "0.0.1-not-a-release" }) },
      { machine: "b", label: "skua-book.tail5e1f.ts.net", handle: backendHandle({ base: "/machine-b", paired: false, mdrevVersion: "0.0.2-not-a-release" }) },
    ],
    assets: { mdrev: "/app-vendor/", mdrevVersion: "{{MDREV}}", mermaid: "/app-vendor{{MERMAID}}" },
    version: "9.9.9-kit",
  });
  host.mounted = true;
} catch (error) {
  host.error = String(error && error.stack || error);
}
</script>
</body>
</html>
"#;

/// One session id held by both machines, with different text: a mix-up of a machine's own id and
/// the list's (`<machine>/<id>`) shows as the wrong text rather than passing by chance.
const SID_BOTH: &str = "cccccccc-0000-4000-8000-000000005950";
/// A second session on the first machine: a row of the machine the view is on, no remount.
const SID_A_TOO: &str = "cccccccc-0000-4000-8000-000000005951";

/// #s595 (design/app-mount.md, "One list over every machine", stage 3) — the web app from the kit
/// over two hermetic monitors. One list holds both machines' sessions. A link to the second
/// machine's session opens it read from that machine alone, by its own id; its diagram draws with
/// the runtime the app carries, and its document opens in mdrev with the app's bundle while mdrev's
/// routes go to that machine and no machine is asked which release it serves; write mode follows
/// that machine, unpaired. A row of the first machine with the same id mounts the view again on the
/// first machine: its text, its routes, its pairing, the pane's state on `#app` the new view's, and
/// a view key reaching the new view; a second row there opens with no remount; ⌘K names each row's
/// machine; and the browser's Back goes across machines.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_web_app_over_two_machines_reads_each_session_from_its_own_machine() {
    let _serial = serial();
    let base = base("kit-app-machines");
    let stores = Stores::new(&base);
    stores.claude_session(
        SID_BOTH,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::rfc3339_secs_ago(400),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::rfc3339_secs_ago(390),
        )),
    );
    stores.claude_session(
        SID_A_TOO,
        &(harness::user_at(
            "how many tern chicks fledged?",
            &harness::rfc3339_secs_ago(300),
        ) + &harness::assistant_at("thirty, by the last count", &harness::rfc3339_secs_ago(290))),
    );
    let monitor = Monitor::spawn(Kind::V1, 3331, &base, Some(&stores), true);
    let there_base = base.join("machine-b");
    let there_stores = Stores::new(&there_base);
    let diagram = serde_json::to_string(
        "forty-one, by the cairn\n\n```mermaid\ngraph TD\n  Ridge --> Cairn\n```",
    )
    .unwrap();
    let notes = "/r/SKUA.md";
    let attachment = format!(
        "{{\"type\":\"attachment\",\"timestamp\":\"{ts}\",\"attachment\":{{\"type\":\"file\",\"filename\":\"{notes}\",\"displayPath\":\"SKUA.md\",\"content\":{{\"type\":\"text\",\"file\":{{\"filePath\":\"{notes}\",\"content\":\"# Skua notes\\n\\nThe ridge holds forty-one burrows.\\n\",\"numLines\":3,\"startLine\":1,\"totalLines\":3}}}}}}}}\n",
        ts = harness::rfc3339_secs_ago(170)
    );
    there_stores.claude_session(
        SID_BOTH,
        &(harness::user_at(
            "count the skua burrows on the ridge",
            &harness::rfc3339_secs_ago(200),
        ) + &harness::assistant_at(
            &diagram[1..diagram.len() - 1],
            &harness::rfc3339_secs_ago(190),
        ) + &attachment),
    );
    let there = Monitor::spawn(Kind::V1, 3332, &there_base, Some(&there_stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    let runtime = claude_replay_html::mermaid_runtime_path();
    let mut answers = kit_answers("");
    answers.push(Answer {
        pattern: "*/kit-machines*".into(),
        path: "/kit-machines".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: APP_MACHINES_HOST
            .replace("{{MDREV}}", claude_monitor::kit::MDREV)
            .replace("{{MERMAID}}", runtime),
    });
    harness::answer_and_relay_in_browser(
        &tab,
        answers,
        vec![
            harness::Relay {
                prefix: "/machine-b".into(),
                port: there.port,
                token: there.token(),
            },
            harness::Relay {
                prefix: "/app-vendor".into(),
                port: monitor.port,
                token: monitor.token(),
            },
        ],
    );
    let address = |id: &str| {
        format!(
            "{}?session={}",
            monitor.url("kit-machines"),
            id.replace('/', "%2F")
        )
    };
    tab.navigate_to(&address(&format!("b/{SID_BOTH}"))).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error))",
        "the page to mount the web app over two machines",
        Duration::from_secs(20),
        "JSON.stringify(window.__host || null)",
    );
    let now = "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText, transcript: (document.querySelector('#sessionView .transcript') || {}).innerText, address: location.search })";
    let transcript_says = |text: &str, what: &str| {
        harness::until(
            &tab,
            &format!("((document.querySelector('#sessionView .transcript') || {{}}).innerText || '').includes({text:?})"),
            what,
            Duration::from_secs(30),
            now,
        );
    };
    transcript_says(
        "forty-one, by the cairn",
        "the second machine's session, read from it",
    );
    let rows = |id: &str| format!("#sessionList .tree-row.session[data-session=\"{id}\"]");
    harness::until(
        &tab,
        &format!(
            "!!document.querySelector({:?}) && !!document.querySelector({:?}) && !!document.querySelector({:?})",
            rows(&format!("a/{SID_BOTH}")),
            rows(&format!("b/{SID_BOTH}")),
            rows(&format!("a/{SID_A_TOO}"))
        ),
        "one list over both machines",
        Duration::from_secs(30),
        now,
    );
    harness::until(
        &tab,
        "document.querySelectorAll('#sessionView [data-mermaid-state=ready] svg').length === 1",
        "the second machine's diagram drawn with the runtime the app carries",
        Duration::from_secs(30),
        "JSON.stringify({ cards: [...document.querySelectorAll('[data-mermaid]')].map(e => e.dataset.mermaidState), meta: (document.querySelector('meta[name=mermaid-runtime]') || {}).content, errors: window.__host.errors })",
    );
    // What the page loaded, and asked, by path: a script or a stylesheet is a resource, not a fetch.
    let loaded = "(function () { var names = performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }); return { names: names.filter(function (p) { return /mermaid|mdrev/.test(p); }), asked: window.__host.asked.slice() }; })()";
    let first = harness::probe(&tab, loaded);
    let names: Vec<String> = serde_json::from_value(first["names"].clone()).unwrap();
    let asked: Vec<String> = serde_json::from_value(first["asked"].clone()).unwrap();
    let on_b = |path: &str| {
        asked
            .iter()
            .any(|a| a.starts_with(&format!("/machine-b{path}")))
    };
    assert!(
        on_b(&format!("/pull?session={SID_BOTH}")) && !asked.iter().any(|a| a.starts_with("/pull")),
        "the second machine's session is pulled from that machine alone, by its own id: {asked:?}"
    );
    assert_eq!(
        names
            .iter()
            .filter(|p| p.ends_with(".js")
                && p.contains("mermaid")
                && *p != "/kit/bundle/shared/mermaid.js")
            .collect::<Vec<_>>(),
        vec![&format!("/app-vendor{runtime}")],
        "the runtime the app carries (the kit's own shared module aside), and none from a machine: {names:?}"
    );
    let state = harness::probe(
        &tab,
        "(function () { return { meta: document.querySelector('meta[name=mermaid-runtime]').content, writeOn: document.getElementById('writeSwitch').getAttribute('aria-checked'), pairHint: !document.getElementById('writeInfo').hidden, address: new URLSearchParams(location.search).get('session') }; })()",
    );
    assert_eq!(
        state,
        serde_json::json!({ "meta": format!("/app-vendor{runtime}"), "writeOn": "false", "pairHint": true, "address": format!("b/{SID_BOTH}") }),
        "the mount named the runtime in the head, write mode follows the unpaired machine, and the address holds the list's id"
    );
    harness::eval(&tab, "document.getElementById('writeSwitch').click(); 'ok'");
    assert_eq!(
        harness::probe(
            &tab,
            "document.getElementById('writeSwitch').getAttribute('aria-checked')"
        ),
        serde_json::json!("false"),
        "write mode does not turn on for an unpaired machine"
    );

    // The document the second machine's session carries, in mdrev with the app's bundle.
    harness::eval(
        &tab,
        "document.querySelector('#sessionView [data-attachment-action=\"preview\"]').click(); 'ok'",
    );
    harness::until(
        &tab,
        "((document.querySelector('#previewBody .mdrev-host h1') || {}).textContent || '').trim() === 'Skua notes'",
        "the document in mdrev, from the app's bundle",
        Duration::from_secs(30),
        "JSON.stringify({ pane: (document.getElementById('previewBody') || {}).innerText, asked: window.__host.asked, errors: window.__host.errors })",
    );
    let after = harness::probe(&tab, loaded);
    let names: Vec<String> = serde_json::from_value(after["names"].clone()).unwrap();
    let asked: Vec<String> = serde_json::from_value(after["asked"].clone()).unwrap();
    let mdrev = claude_monitor::kit::MDREV;
    // The bundle's files (mdrev's routes, `/api/mdrev…`, are fetches, read below).
    let bundle: Vec<&String> = names
        .iter()
        .filter(|p| p.contains("/mdrev/") && !p.contains("/api/"))
        .collect();
    let from_app = format!("/app-vendor/mdrev/{mdrev}/");
    assert!(
        bundle.iter().all(|p| p.starts_with(&from_app))
            && bundle.contains(&&format!("{from_app}mdrev.js"))
            && bundle.contains(&&format!("{from_app}mdrev.css")),
        "mdrev's bundle is the release the app carries, from the app's copy, and nothing of it from a machine: {names:?}"
    );
    // That no machine is asked which release it serves when the app's bundle fails to load is held
    // in node (`codex_machines.test.mjs`): here the app's bundle loads, so nothing would ask.
    assert!(
        asked
            .iter()
            .any(|a| a.starts_with("/machine-b/api/mdrev/hold"))
            && !asked.iter().any(|a| a.starts_with("/api/mdrev")),
        "mdrev's routes go to the session's machine: {asked:?}"
    );
    let pane_open = harness::probe(&tab, "(function () { var app = document.getElementById('app'), view = document.getElementById('sessionView'); return { app: app.classList.contains('preview-off'), view: view.classList.contains('preview-off') }; })()");
    assert_eq!(
        pane_open,
        serde_json::json!({ "app": false, "view": false }),
        "the pane is open"
    );

    // The first machine's session of the same id: the view is mounted again there.
    harness::eval(
        &tab,
        "document.querySelector('#sessionView .transcript').__s595 = 'b'; 'ok'",
    );
    let asked_before = harness::probe(&tab, "window.__host.asked.length")
        .as_u64()
        .unwrap() as usize;
    harness::eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            rows(&format!("a/{SID_BOTH}"))
        ),
    );
    transcript_says(
        "past the second dune",
        "the first machine's session of the same id",
    );
    let switched = harness::probe(
        &tab,
        &format!("(function () {{ var app = document.getElementById('app'), view = document.getElementById('sessionView'); var since = window.__host.asked.slice({asked_before}); return {{ remounted: document.querySelector('#sessionView .transcript').__s595 !== 'b', viewAsked: since.filter(function (a) {{ return !/\\/api\\/(sessions|layout|ignore)\\b/.test(a); }}), stateOnApp: ['preview-off', 'preview-full'].every(function (c) {{ return app.classList.contains(c) === view.classList.contains(c); }}), pairHint: !document.getElementById('writeInfo').hidden, address: new URLSearchParams(location.search).get('session'), wrap: view.classList.contains('wrap-code') }}; }})()"),
    );
    // Everything but the list's own reads, which go to every machine.
    let view_asked: Vec<String> = serde_json::from_value(switched["viewAsked"].clone()).unwrap();
    // The old view may still have polled its machine before it went; from the new one's first pull
    // on, every request the view makes is the first machine's.
    let first_a = view_asked
        .iter()
        .position(|a| a.starts_with(&format!("/pull?session={SID_BOTH}")));
    assert!(
        first_a.is_some_and(|at| view_asked[at..].iter().all(|a| !a.starts_with("/machine-b/"))),
        "the first machine's session is read from the first machine alone, by its own id: {switched}"
    );
    assert_eq!(
        (&switched["remounted"], &switched["stateOnApp"], &switched["pairHint"], &switched["address"]),
        (&serde_json::json!(true), &serde_json::json!(true), &serde_json::json!(false), &serde_json::json!(format!("a/{SID_BOTH}"))),
        "a new view on the first machine, the pane's state on #app the new view's, no pairing hint on the paired machine, and the list's id in the address: {switched}"
    );
    // Write mode turns on for the paired machine.
    let write = "document.getElementById('writeSwitch').getAttribute('aria-checked')";
    harness::eval(&tab, "document.getElementById('writeSwitch').click(); 'ok'");
    assert_eq!(
        harness::probe(&tab, write),
        serde_json::json!("true"),
        "write mode turns on for the paired machine"
    );
    // A view key reaches the view mounted now.
    let wrap_before = switched["wrap"].clone();
    harness::key(&tab, "w", false);
    let wrap_after = harness::probe(
        &tab,
        "document.getElementById('sessionView').classList.contains('wrap-code')",
    );
    assert_ne!(wrap_after, wrap_before, "w toggles the new view's wrapping");
    harness::key(&tab, "w", false);

    // Another row of the same machine: no remount, and write mode stays on.
    harness::eval(
        &tab,
        "document.querySelector('#sessionView .transcript').__s595 = 'a'; 'ok'",
    );
    harness::eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            rows(&format!("a/{SID_A_TOO}"))
        ),
    );
    transcript_says(
        "thirty, by the last count",
        "a second session on the same machine",
    );
    assert_eq!(
        (
            harness::probe(
                &tab,
                "document.querySelector('#sessionView .transcript').__s595 || null"
            ),
            harness::probe(&tab, write)
        ),
        (serde_json::json!("a"), serde_json::json!("true")),
        "a row of the machine the view is on opens in the same view, write mode still on"
    );

    // ⌘K names each row's machine, and its row of the other machine opens there.
    harness::eval(&tab, "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', metaKey: true, bubbles: true, cancelable: true })); 'ok'");
    harness::until(&tab, "document.getElementById('searchLayer').classList.contains('production-open') && document.querySelectorAll('#searchResults .search-result').length > 0", "⌘K's rows", Duration::from_secs(10), now);
    let jump = harness::probe(&tab, "[...document.querySelectorAll('#searchResults .search-result')].filter(function (b) { return b.querySelector('.result-kind').textContent === 'session'; }).map(function (b) { return b.querySelector('b').textContent + ' | ' + b.querySelector('small').textContent; }).sort()");
    let jump: Vec<String> = serde_json::from_value(jump).unwrap();
    assert_eq!(jump.len(), 3, "⌘K lists the three sessions: {jump:?}");
    assert_eq!(
        (
            jump.iter().filter(|r| r.ends_with(" · tern-mini")).count(),
            jump.iter().filter(|r| r.ends_with(" · skua-book")).count()
        ),
        (2, 1),
        "each names its machine by the list's short name: {jump:?}"
    );
    let chosen = harness::probe(&tab, "(function () { var row = [...document.querySelectorAll('#searchResults .search-result')].find(function (b) { return b.querySelector('.result-kind').textContent === 'session' && / · skua-book$/.test(b.querySelector('small').textContent); }); if (!row) return 'no row'; row.click(); return 'chosen'; })()");
    assert_eq!(chosen, serde_json::json!("chosen"));
    transcript_says(
        "forty-one, by the cairn",
        "⌘K's row of the second machine, opened there",
    );
    let chose = harness::probe(&tab, &format!("(function () {{ return {{ address: new URLSearchParams(location.search).get('session'), write: {write}, pairHint: !document.getElementById('writeInfo').hidden }}; }})()"));
    assert_eq!(
        chose,
        serde_json::json!({ "address": format!("b/{SID_BOTH}"), "write": "false", "pairHint": true }),
        "⌘K's row opens on its machine, where write mode is off: the machine is not paired"
    );

    // The browser's Back, across machines and on one, the list's selection following.
    let at = "(function () { var selected = [...document.querySelectorAll('#sessionList .tree-row.session.selected')].map(function (r) { return r.dataset.session; }); return { address: new URLSearchParams(location.search).get('session'), selected: selected, write: document.getElementById('writeSwitch').getAttribute('aria-checked'), errors: window.__host.errors }; })()";
    let mut backs = Vec::new();
    for (text, what) in [
        ("thirty, by the last count", "Back to the first machine"),
        ("past the second dune", "Back on the first machine"),
        ("forty-one, by the cairn", "Back to the second machine"),
    ] {
        harness::eval(&tab, "history.back(); 'ok'");
        transcript_says(text, what);
        backs.push(harness::probe(&tab, at));
    }
    // An id no machine of the list holds: said once, and kept as the selection through the index's
    // next answers (it used to be selected again at each, shutting a phone's drawer), the switch
    // saying to open a session; a row chosen after it opens.
    harness::eval(&tab, "history.pushState({}, '', '?session=zz%2Fnowhere'); dispatchEvent(new PopStateEvent('popstate')); 'ok'");
    transcript_says("Cannot read this session", "an id no machine holds, said");
    let mark = "(function () { var said = [...document.querySelectorAll('#sessionView strong')].find(function (e) { return e.textContent === 'Cannot read this session'; }); if (!said) return 'not said'; said.__s595 = 1; return window.__host.asked.length; })()";
    let polls_from = harness::probe(&tab, mark)
        .as_u64()
        .expect("the refusal is drawn") as usize;
    // Two of the index's answers (it asks every 5 s) from each machine.
    harness::until(
        &tab,
        &format!("window.__host.asked.slice({polls_from}).filter(function (a) {{ return /\\/api\\/sessions/.test(a); }}).length >= 4"),
        "two more index answers from each machine",
        Duration::from_secs(30),
        now,
    );
    harness::eval(&tab, "document.getElementById('writeInfo').click(); 'ok'");
    let nowhere = harness::probe(&tab, "(function () { var said = [...document.querySelectorAll('#sessionView strong')].find(function (e) { return e.textContent === 'Cannot read this session'; }); return { kept: !!(said && said.__s595), address: new URLSearchParams(location.search).get('session'), selected: [...document.querySelectorAll('#sessionList .tree-row.session.selected')].length, write: document.getElementById('writeSwitch').getAttribute('aria-checked'), help: (document.getElementById('writeHelp').querySelector('b') || {}).textContent }; })()");
    harness::eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            rows(&format!("a/{SID_BOTH}"))
        ),
    );
    transcript_says("past the second dune", "a row chosen after it");
    let after = harness::probe(&tab, at);
    drop(there);
    drop(monitor);
    let one = |id: String, write: &str| serde_json::json!({ "address": id, "selected": [id], "write": write, "errors": [] });
    assert_eq!(
        backs,
        vec![
            one(format!("a/{SID_A_TOO}"), "true"),
            one(format!("a/{SID_BOTH}"), "true"),
            one(format!("b/{SID_BOTH}"), "false"),
        ],
        "Back goes across machines, the list selecting the row it opens and write mode following its machine, and the page said no error"
    );
    assert_eq!(
        nowhere,
        serde_json::json!({ "kept": true, "address": "zz/nowhere", "selected": 0, "write": "false", "help": "Open a session to write to it" }),
        "an id no machine holds is said once and stays the selection, no row selected, the switch saying to open a session"
    );
    assert_eq!(
        after,
        one(format!("a/{SID_BOTH}"), "true"),
        "and a row chosen after it opens"
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
const host = window.__host = { mounted: false, error: null, errors: [], loops: [] };
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? host.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : host.errors.push(String(event.message)));
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
    let index = sessions_of(&other, SID_LISTED);
    assert!(
        index.contains(SID_LISTED) && !index.contains(SID),
        "the list's server lists its own session alone: {index}"
    );
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    let mut answers: Vec<Answer> = kit_answers("")
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

/// The sessions of [`one_list_over_two_machines_labels_routes_and_greys_each_machine`]: the page's
/// monitor's, and the other machine's two, all in one directory (the same path on both machines).
const SID_HERE: &str = "cccccccc-0000-4000-8000-000000005411";
const SID_THERE: &str = "cccccccc-0000-4000-8000-000000005412";
const SID_THERE_HIDDEN: &str = "cccccccc-0000-4000-8000-000000005413";
const SID_HERE_LATER: &str = "cccccccc-0000-4000-8000-000000005414";

/// A host listing two machines' sessions in one list (#s541, design/monitor-hub.md D6, Hong's
/// choice on #s539), written from the guide: the merged source over a handle for each machine (the
/// page's own monitor, and the other's relayed at `/machine-b` on the page's origin), passed to the
/// list as its `backend`; a session the reader opens is shown by a view mounted on its own machine's
/// handle, unmounted and mounted again when the reader opens one of the other machine. It records
/// every request the page makes and which machine each view was mounted on.
const HOST_MERGED: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>A kit host over two machines</title>
<meta name="mermaid-runtime" content="{{MERMAID}}">
<link rel="stylesheet" href="/kit/bundle/session-list.css">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
<style>
.host-sessions { position: fixed; top: 0; left: 0; bottom: 0; width: 320px; display: flex; }
.host-sessions > #sessionList { flex: 1 1 auto; }
.host-pane { position: fixed; top: 0; left: 320px; right: 0; bottom: 0; }
.host-pane > #sessionView { position: absolute; inset: 0; }
</style>
</head>
<body>
<div class="host-sessions"><div id="sessionList"></div></div>
<div class="host-pane" id="pane"><div id="sessionView"></div></div>
<script type="module">
const host = window.__host = { mounted: false, error: null, errors: [], loops: [], opened: [], views: [], asked: [] };
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? host.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
const pageFetch = window.fetch.bind(window);
window.fetch = (input, init) => { host.asked.push(new URL(String(input && input.url || input), location.href).href.slice(location.origin.length)); return pageFetch(input, init); };
try {
  const { backendHandle, mergedBackend, mountSessionList, mountSessionView } = await import("/kit/bundle/index.js");
  const merged = window.__merged = mergedBackend([
    { machine: "a", label: "tern-mini", handle: backendHandle({ paired: true }) },
    { machine: "b", label: "skua-book", handle: backendHandle({ base: "/machine-b", paired: true }) },
  ], { timeout: 1500 });
  let view = null, on = "";
  const show = id => {
    const where = merged.locate(id);
    if (!where) { host.errors.push(`no machine for ${id}`); return; }
    list.select(id);
    if (on !== where.machine) {
      if (view) view.unmount();
      const rows = merged.forView(list, where.machine);
      view = window.__view = mountSessionView(document.getElementById("sessionView"), {
        grid: document.getElementById("pane"),
        backend: where.handle,
        selectedId: rows.selectedId,
        sessionRow: rows.sessionRow,
        sessionWorkers: rows.sessionWorkers,
        selectSession: own => show(rows.listId(own)),
        renderList: () => list.redraw(),
        refreshIndex: () => list.refresh(),
        pauseIndex: () => list.stop(),
        resumeIndex: () => list.start(),
      });
      on = where.machine;
      host.views.push(on);
    }
    view.select(where.id);
    view.open(where.id);
  };
  const list = window.__list = mountSessionList(document.getElementById("sessionList"), {
    backend: merged,
    onOpen: ({ id }) => { host.opened.push(id); show(id); },
    onIndex: () => view && view.indexChanged(),
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

/// #s541 — one list over two machines. Two hermetic monitors, the page on the first, the second
/// relayed under `/machine-b` (as a host relays another machine's server, the guide's section 2),
/// each holding sessions in the same directory. The list shows both machines' sessions, the shared
/// path as two projects each labelled with its machine; a session opened from the list is read from
/// its own machine, the view mounted again with that machine's handle when the reader moves to the
/// other machine, and no request for one machine's session goes to the other; a hide from the list
/// reaches only the row's machine; and once the second monitor stops, its rows stay listed, greyed,
/// under a head saying since when it has not answered, while the first's go on as before and a
/// session it starts then is listed.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn one_list_over_two_machines_labels_routes_and_greys_each_machine() {
    let _serial = serial();
    let base = base("kit-merged");
    let stores = Stores::new(&base);
    stores.claude_session(
        SID_HERE,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::at("00:01"),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::at("00:02"),
        )),
    );
    let monitor = Monitor::spawn(Kind::V1, 3314, &base, Some(&stores), true);
    let there_base = base.join("machine-b");
    let there_stores = Stores::new(&there_base);
    there_stores.claude_session(
        SID_THERE,
        &(harness::user_at("count the skua burrows on the ridge", &harness::at("00:03"))
            + &harness::assistant_at("forty-one, most of them occupied", &harness::at("00:04"))),
    );
    there_stores.claude_session(
        SID_THERE_HIDDEN,
        &(harness::user_at("log the petrel calls after dusk", &harness::at("00:05"))
            + &harness::assistant_at("logged, nine calls by the cairn", &harness::at("00:06"))),
    );
    let there = Monitor::spawn(Kind::V1, 3315, &there_base, Some(&there_stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    let mut answers: Vec<Answer> = kit_answers("")
        .into_iter()
        .filter(|answer| answer.path != "/kit-host")
        .collect();
    answers.push(Answer {
        pattern: "*/kit-merged*".into(),
        path: "/kit-merged".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: HOST_MERGED.replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path()),
    });
    harness::answer_and_relay_in_browser(
        &tab,
        answers,
        vec![harness::Relay {
            prefix: "/machine-b".into(),
            port: there.port,
            token: there.token(),
        }],
    );
    tab.navigate_to(&monitor.url("kit-merged")).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error))",
        "the host to mount the list over two machines",
        Duration::from_secs(20),
        "JSON.stringify(window.__host || null)",
    );
    assert_eq!(
        harness::probe(&tab, "window.__host.error"),
        serde_json::Value::Null,
        "the host mounts the list with the merged source"
    );
    let row = |machine: &str, sid: &str| {
        format!("#sessionList .tree-row.session[data-session=\"{machine}/{sid}\"]")
    };
    let both = format!(
        "!!document.querySelector('{}') && !!document.querySelector('{}') && !!document.querySelector('{}')",
        row("a", SID_HERE),
        row("b", SID_THERE),
        row("b", SID_THERE_HIDDEN)
    );
    harness::until(
        &tab,
        &both,
        "both machines' sessions in the one list",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText })",
    );
    let listed = harness::probe(
        &tab,
        "(function () { var heads = Array.from(document.querySelectorAll('#sessionList .tree-row.project')); return { titles: heads.map(function (h) { return h.querySelector('.tree-title').textContent; }), tags: heads.map(function (h) { var t = h.querySelector('.tree-machine'); return t ? t.textContent : null; }).sort(), machines: window.__merged.machines().map(function (m) { return m.machine + ':' + m.state; }), banner: !!document.querySelector('#sessionList .tree-machines'), stale: document.querySelectorAll('#sessionList .machine-stale').length, errors: window.__host.errors }; })()",
    );
    let titles = listed["titles"].as_array().cloned().unwrap_or_default();
    assert!(
        titles.len() == 2 && titles[0] == titles[1],
        "the same path on two machines is two projects: {listed}"
    );
    assert_eq!(
        (
            &listed["tags"],
            &listed["machines"],
            &listed["banner"],
            &listed["stale"],
            &listed["errors"]
        ),
        (
            &serde_json::json!(["skua-book", "tern-mini"]),
            &serde_json::json!(["a:live", "b:live"]),
            &serde_json::json!(false),
            &serde_json::json!(0),
            &serde_json::json!([])
        ),
        "each project's head names its machine, both machines answer, and nothing is greyed: {listed}"
    );

    // Open the other machine's session, then the page's own: each read from its own machine.
    tab.find_element(&row("b", SID_THERE))
        .unwrap()
        .click()
        .unwrap();
    harness::until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('most of them occupied')",
        "the view to read the other machine's session from it",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, transcript: (document.getElementById('transcript') || {}).innerText.slice(0, 300) })",
    );
    tab.find_element(&row("a", SID_HERE))
        .unwrap()
        .click()
        .unwrap();
    harness::until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('past the second dune')",
        "the view to read the page's own machine's session from it",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, transcript: (document.getElementById('transcript') || {}).innerText.slice(0, 300) })",
    );
    let read = harness::probe(
        &tab,
        &format!(
            "(function () {{ var asked = window.__host.asked; var servers = function (sid) {{ return asked.filter(function (p) {{ return /\\/(pull|records)\\?/.test(p) && p.indexOf(sid) >= 0; }}).map(function (p) {{ return p.split('?')[0].replace(/\\/(pull|records)$/, '') || '(the page\\'s own)'; }}); }}; var uniq = function (xs) {{ return xs.filter(function (x, i) {{ return xs.indexOf(x) === i; }}); }}; return {{ opened: window.__host.opened, views: window.__host.views, session: window.__view.session(), selected: window.__list.selected(), there: uniq(servers('{SID_THERE}')), here: uniq(servers('{SID_HERE}')), sessionsAsked: uniq(asked.filter(function (p) {{ return /api\\/sessions/.test(p); }}).map(function (p) {{ return p.split('?')[0]; }})).sort(), errors: window.__host.errors }}; }})()"
        ),
    );
    assert_eq!(
        read,
        serde_json::json!({
            "opened": [format!("b/{SID_THERE}"), format!("a/{SID_HERE}")],
            "views": ["b", "a"],
            "session": SID_HERE,
            "selected": format!("a/{SID_HERE}"),
            "there": ["/machine-b"],
            "here": ["(the page's own)"],
            "sessionsAsked": ["/api/sessions", "/machine-b/api/sessions"],
            "errors": [],
        }),
        "each session was opened in a view on its own machine, in that machine's id, and read from it alone"
    );

    // A hide from the list goes to the row's machine alone, which hides it.
    let hide = format!("{} [data-ignore-op=\"add\"]", row("b", SID_THERE_HIDDEN));
    harness::probe(
        &tab,
        &format!("document.querySelector('{hide}').click(), true"),
    );
    harness::until(
        &tab,
        &format!("!document.querySelector('{}')", row("b", SID_THERE_HIDDEN)),
        "the hidden session to leave the list",
        Duration::from_secs(20),
        "JSON.stringify({ asked: window.__host.asked.filter(function (p) { return /ignore/.test(p); }), errors: window.__host.errors })",
    );
    let ignores = harness::probe(
        &tab,
        "window.__host.asked.filter(function (p) { return /api\\/ignore/.test(p); })",
    );
    assert_eq!(
        ignores,
        serde_json::json!([format!("/machine-b/api/ignore?add=s%3A{SID_THERE_HIDDEN}")]),
        "the hide went to the other machine, in its own key, and to no other"
    );
    let hidden_on = |index: &str| -> Vec<String> {
        let index: serde_json::Value = serde_json::from_str(index).expect("an index is JSON");
        index["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group["rows"].as_array().cloned().unwrap_or_default())
            .filter(|row| row["hidden"] == serde_json::json!(true))
            .filter_map(|row| row["id"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(
        (
            hidden_on(&sessions_of(&there, SID_THERE)),
            hidden_on(&sessions_of(&monitor, SID_HERE))
        ),
        (vec![SID_THERE_HIDDEN.to_string()], Vec::<String>::new()),
        "the other machine hid its session, and the page's own hid nothing"
    );

    // The other machine stops answering: its last rows stay, greyed, under a head saying since when.
    drop(there);
    harness::until(
        &tab,
        "!!document.querySelector('#sessionList .tree-machines .tree-machine-head.tree-machine-head-stale[data-machine=\"b\"]')",
        "a head saying the other machine has stopped answering",
        Duration::from_secs(30),
        "JSON.stringify({ machines: window.__merged.machines(), list: (document.getElementById('sessionList') || {}).innerText })",
    );
    let stale = harness::probe(
        &tab,
        &format!(
            "(function () {{ var there = document.querySelector('{}'), here = document.querySelector('{}'); return {{ head: document.querySelector('#sessionList .tree-machine-head[data-machine=\"b\"]').textContent, heads: document.querySelectorAll('#sessionList .tree-machine-head').length, thereListed: !!there, thereGreyed: !!there && there.classList.contains('machine-stale'), thereFaded: !!there && getComputedStyle(there).opacity, hereGreyed: !!here && here.classList.contains('machine-stale'), staleTags: Array.from(document.querySelectorAll('#sessionList .tree-machine.tree-machine-stale')).map(function (t) {{ return t.textContent; }}), machines: window.__merged.machines().map(function (m) {{ return m.machine + ':' + m.state; }}), session: window.__view.session(), errors: window.__host.errors }}; }})()",
            row("b", SID_THERE),
            row("a", SID_HERE)
        ),
    );
    // The page's own machine still updates: a session it starts now is listed, not greyed.
    stores.claude_session(
        SID_HERE_LATER,
        &(harness::user_at(
            "ring the plover chicks by the lagoon",
            &harness::at("00:07"),
        ) + &harness::assistant_at("six ringed, two more tomorrow", &harness::at("00:08"))),
    );
    let later = row("a", SID_HERE_LATER);
    harness::until(
        &tab,
        &format!("!!document.querySelector('{later}')"),
        "a session the page's own machine starts after the other stopped, in the list",
        Duration::from_secs(30),
        "JSON.stringify({ machines: window.__merged.machines(), list: (document.getElementById('sessionList') || {}).innerText })",
    );
    let fresh = harness::probe(
        &tab,
        &format!("(function () {{ var r = document.querySelector('{later}'); return {{ greyed: r.classList.contains('machine-stale'), stillStale: !!document.querySelector('{}.machine-stale') }}; }})()", row("b", SID_THERE)),
    );
    drop(monitor);
    assert_eq!(
        fresh,
        serde_json::json!({ "greyed": false, "stillStale": true }),
        "the live machine's new session is listed as live, beside the stopped machine's greyed rows"
    );
    let head = stale["head"].as_str().unwrap_or("").to_string();
    assert!(
        head.starts_with("skua-book: not answering since "),
        "the head names the machine and since when: {stale}"
    );
    assert_eq!(
        serde_json::json!({
            "heads": stale["heads"],
            "thereListed": stale["thereListed"],
            "thereGreyed": stale["thereGreyed"],
            "thereFaded": stale["thereFaded"],
            "hereGreyed": stale["hereGreyed"],
            "staleTags": stale["staleTags"],
            "machines": stale["machines"],
            "session": stale["session"],
            "errors": stale["errors"],
        }),
        serde_json::json!({
            "heads": 1,
            "thereListed": true,
            "thereGreyed": true,
            "thereFaded": "0.55",
            "hereGreyed": false,
            "staleTags": ["skua-book"],
            "machines": ["a:live", "b:stale"],
            "session": SID_HERE,
            "errors": [],
        }),
        "the stopped machine's rows stay listed and greyed by the kit's stylesheet, its project's label marked, the page's machine untouched: {stale}"
    );
}

/// A monitor's own answer to `GET /api/sessions` once it lists `wanted`, asked as a paired reader
/// (its token in the legacy cookie, which the gate still reads), over a plain socket.
fn sessions_of(monitor: &Monitor, wanted: &str) -> String {
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
        if body.contains(wanted) {
            return body.to_string();
        }
        // The first scan may not have listed the session yet.
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("the list's server never listed {wanted}");
}

const SID_A: &str = "cccccccc-0000-4000-8000-000000005611";
const SID_B: &str = "cccccccc-0000-4000-8000-000000005612";

/// #s561 — Group by machine in the filter (Hong, on the iOS app, 2026-11-17: "extend the filter menu
/// to have another check box to group agents by machine (right above include idle)"; "When group
/// agents by machine is unchecked, need to show the host the agent is running on (limit to the first
/// dot component maybe)"). The #s541 host over two hermetic monitors, the machines named by their full
/// host names. With the box off the list is arranged as before, each project's head and each row of
/// the list by activity naming its machine by the host name's first label; the box sits between Idle
/// and Include hidden; ticked, a head per machine (its short name) holds that machine's own agents,
/// projects and rows, which no longer name it, in the tree and the list by activity alike; the choice
/// survives a reload; a machine that stops answering says so on its own head, its rows kept under it,
/// with no heads over the list; unticked, the project arrangement comes back, with them. A list of
/// one machine offers no box, whatever was chosen. Run on a desktop and on a phone.
fn groups_by_machine_from_the_filter(phone: bool, here_port: u16, there_port: u16) {
    let _serial = serial();
    let base = base(if phone {
        "kit-by-machine-phone"
    } else {
        "kit-by-machine"
    });
    let stores = Stores::new(&base);
    stores.claude_session(
        SID_A,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::at("00:01"),
        ) + &harness::assistant_at("on the north spit", &harness::at("00:02"))),
    );
    let monitor = Monitor::spawn(Kind::V1, here_port, &base, Some(&stores), true);
    let there_base = base.join("machine-b");
    let there_stores = Stores::new(&there_base);
    there_stores.claude_session(
        SID_B,
        &(harness::user_at("count the skua burrows on the ridge", &harness::at("00:03"))
            + &harness::assistant_at("forty-one", &harness::at("00:04"))),
    );
    let there = Monitor::spawn(Kind::V1, there_port, &there_base, Some(&there_stores), true);
    let (_browser, tab) = harness::chrome_tab();
    if phone {
        harness::phone(&tab, 390, 844);
    }
    monitor.pair(&tab);
    let page = HOST_MERGED
        .replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path())
        .replace(
            r#"label: "tern-mini""#,
            r#"label: "tern-mini.tail5e1f.ts.net""#,
        )
        .replace(
            r#"label: "skua-book""#,
            r#"label: "skua-book.tail5e1f.ts.net""#,
        )
        // The list remembers the reader's choices through the `persist` its host passes (the guide's
        // section 6); #s541's host passes none.
        .replace(
            "const { backendHandle, mergedBackend, mountSessionList, mountSessionView } = await import",
            "const { backendHandle, mergedBackend, mountSessionList, mountSessionView, persist } = await import",
        )
        .replace("    backend: merged,\n    onOpen:", "    backend: merged,\n    persist,\n    onOpen:");
    assert!(
        page.contains("mountSessionView, persist }") && page.contains("    persist,\n    onOpen:"),
        "the host page passes the list its persist"
    );
    let mut answers = kit_answers("");
    answers.push(Answer {
        pattern: "*/kit-merged*".into(),
        path: "/kit-merged".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: page,
    });
    harness::answer_and_relay_in_browser(
        &tab,
        answers,
        vec![harness::Relay {
            prefix: "/machine-b".into(),
            port: there.port,
            token: there.token(),
        }],
    );
    let open_host = |what: &str| {
        harness::until(
            &tab,
            "!!(window.__host && (window.__host.mounted || window.__host.error))",
            what,
            Duration::from_secs(20),
            "JSON.stringify(window.__host || null)",
        );
        assert_eq!(
            harness::probe(&tab, "window.__host.error"),
            serde_json::Value::Null,
            "the host mounts its list: {what}"
        );
    };
    tab.navigate_to(&monitor.url("kit-merged")).unwrap();
    tab.wait_until_navigated().unwrap();
    open_host("the host to mount the list over two machines");
    let both = format!(
        "!!document.querySelector('#sessionList .tree-row.session[data-session=\"a/{SID_A}\"]') && !!document.querySelector('#sessionList .tree-row.session[data-session=\"b/{SID_B}\"]')"
    );
    harness::until(
        &tab,
        &both,
        "both machines' sessions in the one list",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText })",
    );
    // The tree's children in order: a machine's head (its key and the name it shows), an agent's
    // head, a project's head and a session row, each under a machine's head checked to be that
    // machine's; the machine labels drawn; and the box's state.
    let layout = "(function () { var tree = document.getElementById('tree'), machine = null, seq = [], wrong = []; Array.from(tree.children).forEach(function (el) { var c = el.classList; if (c.contains('tree-machine-group')) { machine = el.dataset.machine; seq.push('M:' + machine + ':' + el.querySelector('.tree-machine-name').textContent); } else if (c.contains('agent-label')) { seq.push('A'); if (machine && el.dataset.toggle.indexOf('a:' + machine + '/') !== 0) wrong.push(el.dataset.toggle); } else if (c.contains('project')) { seq.push('P'); if (machine && el.dataset.toggle.indexOf('p:' + machine + '/') !== 0) wrong.push(el.dataset.toggle); } else if (c.contains('session')) { seq.push('S'); if (machine && el.dataset.session.indexOf(machine + '/') !== 0) wrong.push(el.dataset.session); } else if (c.contains('tree-machine-empty')) seq.push('E'); else if (!c.contains('tree-machines')) seq.push('?' + el.className); }); var box = document.querySelector('#sessionFilter [data-by-machine]'); var stale = tree.querySelector('.tree-machine-group.tree-machine-group-stale'); return { seq: seq, wrong: wrong, staleHead: stale ? stale.dataset.machine + ':' + ((stale.querySelector('.tree-machine-note') || {}).textContent || '') : null, tags: Array.from(tree.querySelectorAll('.tree-machine')).map(function (t) { return t.textContent; }).sort(), banner: !!tree.querySelector('.tree-machines'), box: box ? box.getAttribute('aria-checked') : null, errors: window.__host.errors }; })()";
    let click = |selector: &str| {
        harness::probe(
            &tab,
            &format!("document.querySelector('{selector}').click(), true"),
        );
    };
    let until_seq = |seq: serde_json::Value, what: &str| {
        harness::until(
            &tab,
            &format!("JSON.stringify(({layout}).seq) === {:?}", seq.to_string()),
            what,
            Duration::from_secs(20),
            &format!("JSON.stringify({layout})"),
        );
        harness::probe(&tab, layout)
    };

    // The box off: the arrangement of #s541, each project's head naming its machine by the first
    // label of its host name.
    let off = until_seq(
        serde_json::json!(["A", "P", "S", "P", "S"]),
        "the list by agent and project",
    );
    assert_eq!(
        (&off["tags"], &off["banner"], &off["box"], &off["errors"]),
        (
            &serde_json::json!(["skua-book", "tern-mini"]),
            &serde_json::json!(false),
            &serde_json::json!("false"),
            &serde_json::json!([])
        ),
        "each project's head names its machine by its host name's first label: {off}"
    );
    // The filter's sheet: the box between Idle and Include hidden.
    click("#filterBtn");
    let options = harness::probe(
        &tab,
        "Array.from(document.querySelectorAll('#sessionFilter .scope-option')).filter(function (o) { return o.getClientRects().length > 0; }).map(function (o) { return o.dataset.bucket || (o.hasAttribute('data-by-machine') ? 'machine' : o.hasAttribute('data-include-hidden') ? 'hidden' : 'arrange:' + o.dataset.arrange); })",
    );
    assert_eq!(
        options.as_array().map(|all| all[..5].to_vec()),
        Some(vec![
            serde_json::json!("recent"),
            serde_json::json!("blocked"),
            serde_json::json!("idle"),
            serde_json::json!("machine"),
            serde_json::json!("hidden")
        ]),
        "Group by machine comes right after the states, before Include hidden: {options}"
    );
    // One list by activity names each row's machine after its project.
    click("#sessionFilter [data-arrange=\"flat\"]");
    let flat = until_seq(serde_json::json!(["S", "S"]), "the list by activity");
    assert_eq!(
        harness::probe(
            &tab,
            "Array.from(document.querySelectorAll('#tree .tree-row.session.flat .tree-context')).map(function (c) { var kids = Array.from(c.children).map(function (k) { return k.className; }); return kids.indexOf('tree-project') >= 0 && kids.indexOf('tree-machine') > kids.indexOf('tree-project') ? c.querySelector('.tree-machine').textContent : 'NOT AFTER THE PROJECT'; }).sort()"
        ),
        serde_json::json!(["skua-book", "tern-mini"]),
        "each row of the list by activity names its machine after its project: {flat}"
    );

    // Ticked: a head per machine, each holding its own rows, in the list by activity and the tree.
    click("#sessionFilter [data-by-machine]");
    let flat_grouped = until_seq(
        serde_json::json!(["M:a:tern-mini", "S", "M:b:skua-book", "S"]),
        "the list by activity under a head per machine",
    );
    assert_eq!(
        (
            &flat_grouped["tags"],
            &flat_grouped["wrong"],
            &flat_grouped["box"]
        ),
        (
            &serde_json::json!([]),
            &serde_json::json!([]),
            &serde_json::json!("true")
        ),
        "each machine's rows under its own head, which alone names it: {flat_grouped}"
    );
    // There the machines' heads fold, so Collapse all is offered, and folds them alone: the agents
    // and projects the list by activity does not show stay as they were.
    assert_eq!(
        harness::probe(
            &tab,
            "document.getElementById('collapseBtn').hasAttribute('data-flat')"
        ),
        serde_json::json!(false),
        "Collapse all is offered in the list by activity grouped by machine"
    );
    click("#collapseBtn");
    until_seq(
        serde_json::json!(["M:a:tern-mini", "M:b:skua-book"]),
        "Collapse all to fold the machines' heads",
    );
    assert_eq!(
        harness::probe(
            &tab,
            "JSON.parse(localStorage.getItem('am-demo-collapsed') || '[]').sort()"
        ),
        serde_json::json!(["m:a", "m:b"]),
        "Collapse all folded the machines' heads and nothing else"
    );
    click("#expandBtn");
    until_seq(
        serde_json::json!(["M:a:tern-mini", "S", "M:b:skua-book", "S"]),
        "Expand all to open them again",
    );
    click("#sessionFilter [data-arrange=\"flat\"]");
    let grouped = until_seq(
        serde_json::json!([
            "M:a:tern-mini",
            "A",
            "P",
            "S",
            "M:b:skua-book",
            "A",
            "P",
            "S"
        ]),
        "the tree under a head per machine",
    );
    assert_eq!(
        (
            &grouped["tags"],
            &grouped["wrong"],
            &grouped["banner"],
            &grouped["errors"]
        ),
        (
            &serde_json::json!([]),
            &serde_json::json!([]),
            &serde_json::json!(false),
            &serde_json::json!([])
        ),
        "each machine's own agents, projects and rows under its head, none naming the machine again: {grouped}"
    );
    // A machine's head folds its tree away.
    click("#tree .tree-machine-group[data-machine=\"b\"]");
    until_seq(
        serde_json::json!(["M:a:tern-mini", "A", "P", "S", "M:b:skua-book"]),
        "the other machine folded under its head",
    );
    click("#tree .tree-machine-group[data-machine=\"b\"]");

    // Remembered across a reload.
    tab.reload(false, None).unwrap();
    tab.wait_until_navigated().unwrap();
    open_host("the host to mount the list again after a reload");
    let again = until_seq(
        serde_json::json!([
            "M:a:tern-mini",
            "A",
            "P",
            "S",
            "M:b:skua-book",
            "A",
            "P",
            "S"
        ]),
        "the list still grouped by machine after a reload",
    );
    assert_eq!(
        again["box"],
        serde_json::json!("true"),
        "the box still ticked: {again}"
    );

    // The other machine stops answering: its own head says so, over its last rows, and the heads
    // over the list give way to it.
    drop(there);
    harness::until(
        &tab,
        "!!document.querySelector('#tree .tree-machine-group.tree-machine-group-stale[data-machine=\"b\"]')",
        "the other machine's head to say it has stopped answering",
        Duration::from_secs(30),
        &format!("JSON.stringify({layout})"),
    );
    let stopped = harness::probe(&tab, layout);
    assert_eq!(
        (&stopped["seq"], &stopped["banner"], &stopped["tags"]),
        (
            &serde_json::json!(["M:a:tern-mini", "A", "P", "S", "M:b:skua-book", "A", "P", "S"]),
            &serde_json::json!(false),
            &serde_json::json!([])
        ),
        "the stopped machine's rows stay under its head, and nothing is drawn over the list: {stopped}"
    );
    assert!(
        stopped["staleHead"]
            .as_str()
            .is_some_and(|head| head.starts_with("b:not answering since ")),
        "its head says since when it has not answered: {stopped}"
    );
    // A list too narrow for both: the note gives way, so the machine keeps its name to the fraction of
    // a pixel it had at the host's width, its head draws nothing past its edge, and enough of the note
    // shows for its colour to say the machine stopped (#s561's walk: an iPad's 320px list cut both).
    // Narrowed and put back in one read, so no frame draws it: this reads the CSS alone, not a class
    // the list's script sets on a resize. The note shrinking as the name does (the rule before) cuts
    // the name there, so the width is narrow enough to tell.
    let narrow = harness::probe(
        &tab,
        "(function () { var host = document.querySelector('.host-sessions'); var g = document.querySelector('#tree .tree-machine-group[data-machine=\"b\"]'), name = g.querySelector('.tree-machine-name'), note = g.querySelector('.tree-machine-note'); var whole = name.getBoundingClientRect().width; host.style.width = '240px'; var seen = { nameWhole: name.getBoundingClientRect().width === whole && name.scrollWidth <= name.clientWidth + 1, noteCut: note.scrollWidth > note.clientWidth + 1, noteWide: note.clientWidth >= 30, inside: g.scrollWidth <= g.clientWidth + 1, name: whole + ' then ' + name.getBoundingClientRect().width + ' (' + name.clientWidth + '/' + name.scrollWidth + ')', note: note.clientWidth + '/' + note.scrollWidth }; note.style.flex = '0 1 auto'; note.style.minWidth = '0'; seen.evenShareCutsName = name.scrollWidth > name.clientWidth + 1; note.style.flex = ''; note.style.minWidth = ''; host.style.width = ''; return seen; })()",
    );
    assert_eq!(
        (
            &narrow["nameWhole"],
            &narrow["noteCut"],
            &narrow["noteWide"],
            &narrow["inside"],
            &narrow["evenShareCutsName"]
        ),
        (
            &serde_json::json!(true),
            &serde_json::json!(true),
            &serde_json::json!(true),
            &serde_json::json!(true),
            &serde_json::json!(true)
        ),
        "in a narrow list the stopped machine's note gives way and its name stays whole: {narrow}"
    );

    // Unticked: the project arrangement and the labels come back.
    click("#filterBtn");
    click("#sessionFilter [data-by-machine]");
    let back = until_seq(
        serde_json::json!(["A", "P", "S", "P", "S"]),
        "the list by agent and project again",
    );
    assert_eq!(
        (&back["tags"], &back["box"], &back["banner"]),
        (
            &serde_json::json!(["skua-book", "tern-mini"]),
            &serde_json::json!("false"),
            &serde_json::json!(true)
        ),
        "the labels back on the projects' heads, and the stopped machine's head over the list: {back}"
    );

    // A list of one machine offers no box, even with Group by machine chosen.
    click("#sessionFilter [data-by-machine]");
    tab.navigate_to(&monitor.url("kit-host")).unwrap();
    tab.wait_until_navigated().unwrap();
    open_host("the one-machine host to mount its list");
    harness::until(
        &tab,
        &format!("!!document.querySelector('#sessionList .tree-row.session[data-session=\"{SID_A}\"]')"),
        "the one machine's session in its list",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText })",
    );
    click("#filterBtn");
    let lone = harness::probe(
        &tab,
        "(function () { var part = document.querySelector('#sessionFilter [data-machine-part]'), idle = document.querySelector('#sessionFilter [data-bucket=\"idle\"]'); return { chosen: localStorage.getItem('am-prod-by-machine'), sheetOpen: !!idle && idle.getClientRects().length > 0, part: !!part, hidden: !!part && part.hidden, shown: !!part && part.getClientRects().length > 0, heads: document.querySelectorAll('#tree .tree-machine-group').length }; })()",
    );
    drop(monitor);
    assert_eq!(
        lone,
        serde_json::json!({ "chosen": "1", "sheetOpen": true, "part": true, "hidden": true, "shown": false, "heads": 0 }),
        "one machine: the box is not offered and the list has no machine heads, though the choice is kept"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_list_over_two_machines_groups_by_machine_from_the_filter() {
    groups_by_machine_from_the_filter(false, 3321, 3322);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_phone_list_over_two_machines_groups_by_machine_from_the_filter() {
    groups_by_machine_from_the_filter(true, 3323, 3324);
}

/// What a host page's tab of its own holds right now, for a failure message.
const TAB_NOW: &str = "JSON.stringify({ href: location.href, tab: window.__tab || null, text: (document.getElementById('detachedView') || {}).innerText ? document.getElementById('detachedView').innerText.slice(0, 300) : '' })";

/// #s433 (design/embedding.md, "Detached views"; Hong's choices on #s519 and #s528): a host serves
/// a tab of its own on its own route, drawn by the kit. Its view names that page as `detachTo`, so
/// the transcript's ↗ and the artifact pane's ↗ (a document the transcript carries, an image it
/// embeds) open the host's page with the query that says what to show; that page mounts the kit's
/// `mountDetached`, which reads the query and draws the transcript, the document in mdrev's viewer
/// and the image, keeping the host's route in the address. The text and the image reach the tab in
/// its sessionStorage, as `window.open` would copy them: the case loads each address in the tab the
/// host page ran in. Nothing comes from the monitor's `/monitor-ui/`, and `unmount` leaves the
/// host's element empty.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_host_serves_a_tab_of_its_own_from_the_kit() {
    let _serial = serial();
    let base = base("kit-tab");
    let stores = Stores::new(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let notes_path = repo.join("NOTES.md").display().to_string();
    let notes = "# Field notes\n\nThe terns came back on Tuesday.\n";
    let attachment = format!(
        "{{\"type\":\"attachment\",\"timestamp\":\"{ts}\",\"attachment\":{{\"type\":\"file\",\"filename\":\"{notes_path}\",\"displayPath\":\"NOTES.md\",\"content\":{{\"type\":\"text\",\"file\":{{\"filePath\":\"{notes_path}\",\"content\":\"{text}\",\"numLines\":3,\"startLine\":1,\"totalLines\":3}}}}}}}}\n",
        ts = at("00:03"),
        text = notes.replace('\n', "\\n"),
    );
    let jsonl = [
        harness::pasted_image_sized(
            "where do the terns nest this spring?",
            &at("00:01"),
            harness::WIDE_PNG_B64,
        ),
        harness::assistant_at("on the north spit, past the second dune", &at("00:02")),
        attachment,
    ]
    .concat()
    .replace("\"cwd\":\"/r\"", &format!("\"cwd\":\"{}\"", repo.display()));
    stores.claude_session(SID, &jsonl);
    let monitor = Monitor::spawn(Kind::V1, 3315, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    // The host's page carries a query of its own, which every tab keeps, and a fragment, which
    // stays last (the review of #s433).
    harness::answer_in_browser(&tab, kit_answers(r#"detachTo: "/kit-tab?team=terns#top","#));
    tab.navigate_to(&monitor.url("kit-host")).unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error))",
        "the kit host to mount both parts",
        Duration::from_secs(20),
        "JSON.stringify(window.__host || null)",
    );
    let row = format!("#sessionList .tree-row.session[data-session=\"{SID}\"]");
    until(
        &tab,
        &format!("!!document.querySelector('{}')", row.replace('\'', "\\'")),
        "the session listed in the kit's list",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, list: (document.getElementById('sessionList') || {}).innerText })",
    );
    tab.find_element(&row).unwrap().click().unwrap();
    until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('past the second dune')",
        "the kit's view to read the session",
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host, transcript: (document.getElementById('transcript') || {}).innerText.slice(0, 300) })",
    );

    // The transcript's ↗, which only a host that names its page (`detachTo`) gets.
    until(
        &tab,
        "(function(){ var b = document.getElementById('sessionNewTab'); return !!b && !b.hidden && b.offsetWidth > 0; })()",
        "the transcript's ↗ in the view's top bar",
        Duration::from_secs(10),
        "JSON.stringify({ button: !!document.getElementById('sessionNewTab'), hidden: (document.getElementById('sessionNewTab') || {}).hidden })",
    );
    harness::click_when_settled(
        &tab,
        "#sessionNewTab",
        "the transcript's ↗ to hold still where a reader can press it",
    );
    // The document the transcript carries, in the pane, then the pane's ↗.
    eval(
        &tab,
        "document.querySelector('[data-attachment-action=\"preview\"]').click(); 'ok'",
    );
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1') && !!document.querySelector('#previewHead .preview-newtab:not([hidden])')",
        "the notes in mdrev's viewer in the pane, with the pane's ↗",
        Duration::from_secs(30),
        "(document.getElementById('previewBody') || {}).innerText",
    );
    harness::click_when_settled(
        &tab,
        "#previewHead .preview-newtab",
        "the pane's ↗ to hold still where a reader can press it",
    );
    // The image the prompt embeds, in the pane, then the pane's ↗.
    eval(
        &tab,
        "document.querySelector('.prompt-image').click(); 'ok'",
    );
    until(
        &tab,
        "(function(){ var i = document.querySelector('#previewBody .artifact-stage img'); var b = document.querySelector('#previewHead .preview-newtab'); return !!i && i.naturalWidth > 1 && !!b && !b.hidden; })()",
        "the pasted image in the pane, with the pane's ↗",
        Duration::from_secs(20),
        "(document.getElementById('previewBody') || {}).innerText",
    );
    harness::click_when_settled(
        &tab,
        "#previewHead .preview-newtab",
        "the pane's ↗ to hold still where a reader can press it",
    );
    let tabs: Vec<String> =
        serde_json::from_value(harness::probe(&tab, "window.__host.tabs")).unwrap();
    assert_eq!(tabs.len(), 3, "three ↗s, three tabs: {tabs:?}");
    assert_eq!(
        tabs[0],
        format!("/kit-tab?team=terns&session={SID}#top"),
        "the transcript's tab is the host's page with the session"
    );
    assert!(
        tabs[1].starts_with("/kit-tab?team=terns&root=")
            && tabs[1].contains("NOTES.md")
            && tabs[1].ends_with("#top"),
        "the document's tab is the host's page with the held text's place: {tabs:?}"
    );
    assert!(
        tabs[2].starts_with("/kit-tab?team=terns&")
            && tabs[2].contains("held=am-image")
            && tabs[2].ends_with("#top"),
        "the image's tab is the host's page with the copy it was handed: {tabs:?}"
    );

    // Each address, loaded in this tab: its sessionStorage holds what the host page left there.
    // Each loaded without the fragment, which the session view would read as a record to land on.
    let load = |address: &str| {
        let address = address.split('#').next().unwrap_or_default();
        tab.navigate_to(&monitor.url(address.trim_start_matches('/')))
            .unwrap();
        tab.wait_until_navigated().unwrap();
        until(
            &tab,
            "!!(window.__tab && (window.__tab.mounted || window.__tab.error))",
            "the host's tab to mount",
            Duration::from_secs(20),
            TAB_NOW,
        );
    };
    load(&tabs[0]);
    until(
        &tab,
        "(document.querySelector('#detachedView > #sessionView #transcript') || {}).innerText.includes('past the second dune')",
        "the transcript alone in the host's tab",
        Duration::from_secs(30),
        TAB_NOW,
    );
    let transcript = harness::probe(
        &tab,
        "({ kind: window.__tab.kind, error: window.__tab.error, errors: window.__tab.errors, path: location.pathname, team: new URLSearchParams(location.search).get('team'), session: new URLSearchParams(location.search).get('session'), list: !!document.getElementById('sessionList'), ownNewTab: !!document.getElementById('sessionNewTab'), title: document.title !== 'A kit tab' && document.title.length > 0, monitorUi: performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }).filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }) })",
    );
    load(&tabs[1]);
    until(
        &tab,
        "!!document.querySelector('#detachedView > .detached-doc.mdrev-host h1')",
        "the notes in mdrev's viewer in the host's tab",
        Duration::from_secs(30),
        TAB_NOW,
    );
    let document = harness::probe(
        &tab,
        "({ kind: window.__tab.kind, error: window.__tab.error, errors: window.__tab.errors, path: location.pathname, team: new URLSearchParams(location.search).get('team'), h1: document.querySelector('#detachedView > .detached-doc h1').textContent.trim(), title: document.title, monitorUi: performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }).filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }) })",
    );
    load(&tabs[2]);
    until(
        &tab,
        "(function(){ var i = document.querySelector('#detachedView > .detached-stage img'); return !!i && i.complete && i.naturalWidth > 1 && i.getBoundingClientRect().width > 10; })()",
        "the pasted image whole in the host's tab",
        Duration::from_secs(20),
        TAB_NOW,
    );
    let image = harness::probe(
        &tab,
        "({ kind: window.__tab.kind, error: window.__tab.error, errors: window.__tab.errors, path: location.pathname, team: new URLSearchParams(location.search).get('team'), title: document.title !== 'A kit tab' && document.title.length > 0, monitorUi: performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }).filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }) })",
    );
    // The handle takes the tab away and leaves the host's element as it gave it.
    let unmounted = harness::probe(
        &tab,
        "(function () { window.__detached.unmount(); var el = document.getElementById('detachedView'); return { empty: !!el && el.childNodes.length === 0, theme: el ? el.dataset.theme || null : 'no element', kind: el ? el.dataset.kind || null : 'no element' }; })()",
    );
    drop(monitor);
    assert_eq!(
        transcript,
        serde_json::json!({ "kind": "session", "error": null, "errors": [], "path": "/kit-tab", "team": "terns", "session": SID, "list": false, "ownNewTab": false, "title": true, "monitorUi": [] }),
        "the transcript alone, no list and no ↗ of its own, on the host's route"
    );
    assert_eq!(
        document,
        serde_json::json!({ "kind": "markdown", "error": null, "errors": [], "path": "/kit-tab", "team": "terns", "h1": "Field notes", "title": "NOTES.md", "monitorUi": [] }),
        "the document in mdrev's viewer, on the host's route"
    );
    assert_eq!(
        image,
        serde_json::json!({ "kind": "image", "error": null, "errors": [], "path": "/kit-tab", "team": "terns", "title": true, "monitorUi": [] }),
        "the image, on the host's route"
    );
    assert_eq!(
        unmounted,
        serde_json::json!({ "empty": true, "theme": null, "kind": null }),
        "unmount leaves the host's element empty"
    );
}

/// #s433: the monitor is a host of the same tab. Its view names `/detached` for the transcript, so
/// the top bar carries a ↗ that opens the session's transcript alone in a tab of its own (no list,
/// no ↗ of its own), and that tab follows the app's theme when the reader changes it in another tab.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_monitor_opens_a_transcript_in_a_tab_of_its_own() {
    let _serial = serial();
    let base = base("kit-monitor-tab");
    let stores = Stores::new(&base);
    stores.claude_session(
        SID,
        &(harness::pasted_image_sized(
            "where do the terns nest this spring?",
            &harness::at("00:01"),
            harness::WIDE_PNG_B64,
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::at("00:02"),
        )),
    );
    let monitor = Monitor::spawn(Kind::V1, 3316, &base, Some(&stores), true);
    let (browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?session={SID}"));
    until(
        &tab,
        "(document.getElementById('transcript') || {}).innerText.includes('past the second dune')",
        "the app to read the session",
        Duration::from_secs(30),
        "(document.getElementById('transcript') || {}).innerText",
    );
    until(
        &tab,
        "(function(){ var b = document.getElementById('sessionNewTab'); return !!b && !b.hidden && b.offsetWidth > 0; })()",
        "the transcript's ↗ in the top bar",
        Duration::from_secs(10),
        "JSON.stringify({ button: !!document.getElementById('sessionNewTab') })",
    );
    harness::click_when_settled(
        &tab,
        "#sessionNewTab",
        "the transcript's ↗ to hold still where a reader can press it",
    );
    let own = harness::opened_tab(&browser, "/detached?session=", Duration::from_secs(20));
    until(
        &own,
        "(document.querySelector('#detachedView > #sessionView #transcript') || {}).innerText.includes('past the second dune')",
        "the transcript alone in a tab of its own",
        Duration::from_secs(30),
        TAB_NOW,
    );
    // The image the prompt embeds, in the tab's own pane: no ↗ there either.
    eval(
        &own,
        "document.querySelector('.prompt-image').click(); 'ok'",
    );
    until(
        &own,
        "(function(){ var i = document.querySelector('#previewBody .artifact-stage img'); return !!i && i.naturalWidth > 1; })()",
        "the pasted image in the tab's pane",
        Duration::from_secs(20),
        "(document.getElementById('previewBody') || {}).innerText",
    );
    let shown = harness::probe(
        &own,
        "({ path: location.pathname, session: new URLSearchParams(location.search).get('session'), list: !!document.getElementById('sessionList'), ownNewTab: !!document.getElementById('sessionNewTab'), paneNewTab: (function () { var b = document.querySelector('#previewHead .preview-newtab'); return !!b && !b.hidden && b.offsetWidth > 0; })(), theme: document.getElementById('sessionView').dataset.theme || 'light' })",
    );
    // The address keeps what else it carries (a trace asked for, a host's own key) as the tab
    // rewrites it with each title.
    own.navigate_to(&monitor.url(&format!("detached?trace=viewport&session={SID}")))
        .unwrap();
    own.wait_until_navigated().unwrap();
    until(
        &own,
        "(document.querySelector('#detachedView > #sessionView #transcript') || {}).innerText.includes('past the second dune') && document.title !== 'Agent Monitor'",
        "the transcript and its title, again, from an address with a trace",
        Duration::from_secs(30),
        TAB_NOW,
    );
    let address = harness::probe(
        &own,
        "({ trace: new URLSearchParams(location.search).get('trace'), session: new URLSearchParams(location.search).get('session') })",
    );
    // The app's theme, changed in the app's tab, reaches the tab of its own.
    eval(&tab, "localStorage.setItem('am-demo-theme', 'dark'); 'ok'");
    until(
        &own,
        "document.getElementById('sessionView').dataset.theme === 'dark'",
        "the tab of its own to follow the app's theme",
        Duration::from_secs(10),
        "document.getElementById('sessionView').dataset.theme",
    );
    eval(&tab, "localStorage.removeItem('am-demo-theme'); 'ok'");
    drop(monitor);
    assert_eq!(
        shown,
        serde_json::json!({ "path": "/detached", "session": SID, "list": false, "ownNewTab": false, "paneNewTab": false, "theme": "light" }),
        "the session's transcript alone, at /detached"
    );
    assert_eq!(
        address,
        serde_json::json!({ "trace": "viewport", "session": SID }),
        "the tab keeps the rest of its address"
    );
}

/// One session id on both machines, long enough to scroll, for #s612.
const SID_PLACE: &str = "cccccccc-0000-4000-8000-000000006120";

/// #s612 (#s597's browser case): a session's reading place is kept per machine. Over two machines
/// one id names a session on each, and the view remembers where the reader was by the handle's base
/// and the id (`viewMemoryKey`: `am-view:<id>` for the page's own server, `am-view:<base>|<id>` for
/// another). The reader scrolls the first machine's copy to a turn well inside it, opens the second
/// machine's copy of the same id, which has nothing remembered and so follows its own end, not the
/// first machine's place; back on the first machine, its place comes back. The first machine is the
/// page's own server, which keeps the key it always had.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_web_app_over_two_machines_keeps_each_machines_reading_place_for_a_shared_id() {
    let _serial = serial();
    let base = base("kit-app-places");
    // Sixty turns at a fixed past hour, then a last one now, so the list shows the session.
    let long = |last: &str| {
        harness::long_session(60, harness::Shape::default())
            + &harness::user_at(&format!("{last}?"), &harness::rfc3339_secs_ago(120))
            + &harness::assistant_at(&format!("{last}, counted"), &harness::rfc3339_secs_ago(110))
    };
    let stores = Stores::new(&base);
    stores.claude_session(SID_PLACE, &long("the terns on the north spit"));
    let monitor = Monitor::spawn(Kind::V1, 3333, &base, Some(&stores), true);
    let there_base = base.join("machine-b");
    let there_stores = Stores::new(&there_base);
    there_stores.claude_session(SID_PLACE, &long("the skuas on the ridge"));
    let there = Monitor::spawn(Kind::V1, 3334, &there_base, Some(&there_stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    let mut answers = kit_answers("");
    answers.push(Answer {
        pattern: "*/kit-machines*".into(),
        path: "/kit-machines".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: APP_MACHINES_HOST
            .replace("{{MDREV}}", claude_monitor::kit::MDREV)
            .replace("{{MERMAID}}", claude_replay_html::mermaid_runtime_path()),
    });
    harness::answer_and_relay_in_browser(
        &tab,
        answers,
        vec![
            harness::Relay {
                prefix: "/machine-b".into(),
                port: there.port,
                token: there.token(),
            },
            harness::Relay {
                prefix: "/app-vendor".into(),
                port: monitor.port,
                token: monitor.token(),
            },
        ],
    );
    let own_key = format!("am-view:{SID_PLACE}");
    let there_key = format!("am-view:/machine-b|{SID_PLACE}");
    // Where the view is: the first unit at the transcript's top (the viewport's own anchor, as it
    // remembers it), whether it is at its end, which machine's text its end says, and what each
    // machine's memory holds.
    let place = format!(
        "(function () {{ var s = document.querySelector('#sessionView .transcript'), w = document.querySelector('#sessionView .virtual-window'); var anchor = null; if (s && w) {{ var top = s.getBoundingClientRect().top; for (var c of w.children) {{ var r = c.getBoundingClientRect(); if (c.dataset.unitKey && r.bottom > top + 1) {{ anchor = {{ key: c.dataset.unitKey, top: Math.round(r.top - top) }}; break; }} }} }} var memory = function (k) {{ try {{ return JSON.parse(sessionStorage.getItem(k) || 'null'); }} catch (e) {{ return 'unreadable'; }} }}; var keys = []; for (var i = 0; i < sessionStorage.length; i++) {{ if (sessionStorage.key(i).indexOf('am-view:') === 0) keys.push(sessionStorage.key(i)); }} var text = (s && s.innerText) || ''; return {{ session: new URLSearchParams(location.search).get('session'), anchor: anchor, tail: !!s && s.scrollHeight - s.clientHeight - s.scrollTop <= 2, tall: !!s && s.scrollHeight > s.clientHeight * 3, terns: text.includes('the terns on the north spit, counted'), skuas: text.includes('the skuas on the ridge, counted'), own: memory({own_key:?}), there: memory({there_key:?}), keys: keys.sort() }}; }})()"
    );
    let now = format!("JSON.stringify({place})");
    tab.navigate_to(&format!(
        "{}?session=a%2F{SID_PLACE}",
        monitor.url("kit-machines")
    ))
    .unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        &format!("(function () {{ var p = {place}; return p.tall && p.tail && p.terns; }})()"),
        "the first machine's session, tall enough to scroll, at its end",
        Duration::from_secs(30),
        &now,
    );

    // The reader scrolls the first machine's copy to a turn well inside it.
    eval(&tab, "(function () { var s = document.querySelector('#sessionView .transcript'); s.dispatchEvent(new WheelEvent('wheel', { deltaY: 1 })); s.scrollTop = Math.floor(s.scrollHeight * 0.45); return 'ok'; })()");
    until(
        &tab,
        &format!("(function () {{ var p = {place}; return !p.tail && !!p.own && p.own.following === false && !!p.anchor && p.anchor.key === p.own.key; }})()"),
        "the first machine's place, remembered at the turn the reader scrolled to",
        Duration::from_secs(20),
        &now,
    );
    let rows = |id: &str| format!("#sessionList .tree-row.session[data-session=\"{id}\"]");
    until(
        &tab,
        &format!(
            "!!document.querySelector({:?}) && !!document.querySelector({:?})",
            rows(&format!("a/{SID_PLACE}")),
            rows(&format!("b/{SID_PLACE}"))
        ),
        "the session's row on each machine",
        Duration::from_secs(30),
        &now,
    );

    // The second machine's copy of the same id: nothing remembered there, so its own end.
    eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            rows(&format!("b/{SID_PLACE}"))
        ),
    );
    until(
        &tab,
        &format!(
            "(function () {{ var p = {place}; return p.session === {:?} && !!p.anchor; }})()",
            format!("b/{SID_PLACE}")
        ),
        "the view mounted on the second machine",
        Duration::from_secs(30),
        &now,
    );
    // The view either follows its end or restores a place as the records stream in; give a
    // restore every chance to happen before reading where it is.
    let landed_there = (0..80)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(250));
            let p = harness::probe(&tab, &place);
            (p["tail"] == true && p["skuas"] == true).then_some(p)
        })
        .map(|_| {
            std::thread::sleep(Duration::from_millis(1500));
            harness::probe(&tab, &place)
        })
        .unwrap_or_else(|| harness::probe(&tab, &place));
    let own_memory = landed_there["own"].clone();
    assert_ne!(
        landed_there["anchor"]["key"], own_memory["key"],
        "the second machine's copy is not at the first machine's turn: {landed_there}"
    );
    assert_eq!(
        (&landed_there["tail"], &landed_there["skuas"]),
        (&serde_json::json!(true), &serde_json::json!(true)),
        "the second machine's copy opens at its own end, not at the first machine's turn: {landed_there}"
    );

    // Back on the first machine: its place comes back.
    eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            rows(&format!("a/{SID_PLACE}"))
        ),
    );
    let back = (0..80)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(250));
            let p = harness::probe(&tab, &place);
            (p["session"] == format!("a/{SID_PLACE}") && p["anchor"]["key"] == own_memory["key"])
                .then_some(p)
        })
        // Once there, it stays there: a restore that lands and then drifts is not one.
        .map(|_| {
            std::thread::sleep(Duration::from_millis(1500));
            harness::probe(&tab, &place)
        })
        .unwrap_or_else(|| harness::probe(&tab, &place));
    drop(there);
    drop(monitor);
    assert_eq!(
        back["anchor"]["key"], own_memory["key"],
        "back on the first machine, the turn the reader left is at the top again: left {own_memory}, back {back}"
    );
    let drift = (back["anchor"]["top"].as_i64().unwrap_or(9999)
        - own_memory["top"].as_i64().unwrap_or(0))
    .abs();
    assert!(
        drift <= 6,
        "…within a few pixels: left {own_memory}, back {back}"
    );
    // The page's own server keeps the key it always had; the second machine's is its own.
    assert_eq!(
        back["own"]["key"], own_memory["key"],
        "the first machine's memory is under am-view:<id>, untouched by the second machine: {back}"
    );
    let keys: Vec<String> = serde_json::from_value(back["keys"].clone()).unwrap();
    assert!(
        keys.contains(&own_key) && keys.iter().all(|k| *k == own_key || *k == there_key),
        "the views keep their places under am-view:<id> and am-view:/machine-b|<id> alone: {keys:?}"
    );
    assert!(
        back["there"].is_null() || back["there"]["following"] == true,
        "the second machine's memory, when it wrote one, says it followed its end: {back}"
    );
}

/// The native app's page as #s596's cases serve it: under the app's own content security policy
/// (agent-monitor-app `KitPage.policy`, its `app://monitor/kit/` this origin's `/kit/`), so every
/// script is the kit's or the host's file under `/kit/`, none inline; what the page reads of its
/// machine goes to this origin; inline styles stay allowed, as there.
const NATIVE_HOST: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src {{KIT}}; style-src {{KIT}} 'unsafe-inline'; font-src {{KIT}} data:; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self' data: blob:; object-src 'none'; form-action 'none'; base-uri 'none'">
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<title>The native app's own options</title>
<link rel="stylesheet" href="/kit/bundle/app-shell.css">
<link rel="stylesheet" href="/kit/bundle/session-list.css">
<link rel="stylesheet" href="/kit/bundle/session-view.css">
<script type="module" src="/kit/native-host.js"></script>
</head>
<body>
<div id="appRoot"></div>
</body>
</html>
"#;

/// The host's script, a file under `/kit/` as the app's is: the mount with the native app's own
/// options (#s596), its Machines row counting its calls.
const NATIVE_SCRIPT: &str = r#"const host = window.__host = { mounted: false, error: null, errors: [], loops: [], violations: [], machines: 0 };
addEventListener("securitypolicyviolation", event => host.violations.push(`${event.effectiveDirective} ${event.blockedURI}`));
// A ResizeObserver loop notice is the browser putting a size report off to the next frame, not a throw: it is kept apart, with the step the case names, and fails nothing (#s596).
addEventListener("error", event => /^ResizeObserver loop/.test(String(event.message)) ? host.loops.push({ step: window.__step || null, t: Math.round(performance.now()) }) : host.errors.push(String(event.message)));
addEventListener("unhandledrejection", event => host.errors.push(String(event.reason && event.reason.message || event.reason)));
try {
  const native = await import("/kit/bundle/native-app.js");
  const { backendHandle } = await import("/kit/bundle/index.js");
  native.mountApp(document.getElementById("appRoot"), {
    backend: backendHandle({ paired: true }),
    version: "9.9.9-kit",
    theme: "dark",
    show: { detach: false },
    onMachines: () => { host.machines += 1; },
    reviewSheet: true,
  });
  host.mounted = true;
} catch (error) {
  host.error = String(error && error.stack || error);
}
"#;

/// The kit, the native host page and its script, for #s596's cases on `monitor`.
fn native_answers(monitor: &Monitor) -> Vec<Answer> {
    let mut answers = harness::kit_bundle();
    answers.push(Answer {
        pattern: "*/kit-native*".into(),
        path: "/kit-native".into(),
        content_type: "text/html; charset=utf-8".into(),
        body: NATIVE_HOST.replace("{{KIT}}", &monitor.url("kit/")),
    });
    answers.push(Answer {
        pattern: "*/kit/native-host.js*".into(),
        path: "/kit/native-host.js".into(),
        content_type: "text/javascript; charset=utf-8".into(),
        body: NATIVE_SCRIPT.into(),
    });
    answers
}

/// A recent session, so the list's default filter shows it.
const SID_NATIVE: &str = "cccccccc-0000-4000-8000-000000005960";

fn native_world(case: &str, port: u16) -> (Monitor, std::path::PathBuf) {
    let base = base(case);
    let stores = Stores::new(&base);
    stores.claude_session(
        SID_NATIVE,
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::rfc3339_secs_ago(400),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::rfc3339_secs_ago(390),
        )),
    );
    (
        Monitor::spawn(Kind::V1, port, &base, Some(&stores), true),
        base,
    )
}

/// Where a control is and whether a reader reaches it: drawn, its box, whether its centre hit-tests
/// to it, and the ids beside it.
fn reach(tab: &headless_chrome::Tab, id: &str) -> serde_json::Value {
    let read = eval(
        tab,
        &format!(
            "(function () {{ var b = document.getElementById({id:?}); if (!b) return JSON.stringify(null); var r = b.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2, hit = document.elementFromPoint(x, y); return JSON.stringify({{ reached: r.width > 0 && r.height > 0 && x >= 0 && y >= 0 && x < innerWidth && y < innerHeight && !!hit && b.contains(hit), box: [r.left, r.top, r.width, r.height].map(Math.round), centre: [x, y], hit: hit ? hit.tagName.toLowerCase() + (hit.id ? '#' + hit.id : '') : null, before: b.previousElementSibling ? b.previousElementSibling.id : null, label: b.innerText.trim() || b.getAttribute('aria-label') }}); }})()"
        ),
    );
    serde_json::from_str(read.as_str().unwrap_or("null")).unwrap()
}

fn mounted(tab: &headless_chrome::Tab, what: &str) {
    until(
        tab,
        "!!(window.__host && (window.__host.mounted || window.__host.error)) && !!document.querySelector('#sessionView .transcript') && document.querySelector('#sessionView .transcript').innerText.indexOf('north spit') >= 0",
        what,
        Duration::from_secs(30),
        "JSON.stringify({ host: window.__host || null, transcript: (document.querySelector('#sessionView .transcript') || {}).innerText })",
    );
}

const THEMES: &str = "JSON.stringify({ page: document.documentElement.dataset.theme || 'light', list: document.getElementById('sessionList').dataset.theme || 'light', view: document.getElementById('sessionView').dataset.theme || 'light', stored: localStorage.getItem('am-demo-theme') })";

/// #s596 (design/app-mount.md, "Kept for the native app", stage 4) — the web app from the kit with
/// the native app's own options, under the app's content security policy. With nothing stored it
/// opens in the start theme the app passed, the page, the list and the view alike; a theme the
/// reader picks is stored and wins over the start at the next load. The Machines row sits after
/// Search in the nav and a reader reaches it, at the default width and at the list's least (232px),
/// and pressing it calls `onMachines`; on the rail it is a glyph after the filter's, pressed the
/// same. `show: { detach: false }` draws no ↗ on the transcript's top bar. Nothing the page loads
/// breaks the policy.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_native_app_s_own_options_from_the_kit_under_its_policy() {
    let _serial = serial();
    let (monitor, _base) = native_world("kit-native", 3341);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    harness::answer_in_browser(&tab, native_answers(&monitor));
    tab.navigate_to(&monitor.url("kit-native")).unwrap();
    tab.wait_until_navigated().unwrap();
    mounted(&tab, "the app from the kit, its first session read");
    let opened: serde_json::Value =
        serde_json::from_str(eval(&tab, THEMES).as_str().unwrap()).unwrap();
    let row = reach(&tab, "machinesBtn");
    let new_tab = eval(
        &tab,
        "(function () { var b = document.getElementById('sessionNewTab'); return !!b && !b.hidden && b.offsetWidth > 0; })()",
    );
    harness::click_when_settled(&tab, "#machinesBtn", "the Machines row");
    let after_row = eval(&tab, "window.__host.machines");

    // The rail: the glyph after the filter's, pressed the same.
    harness::click_when_settled(&tab, "#sidebarCollapse", "the sidebar's collapse");
    until(
        &tab,
        "document.getElementById('app').classList.contains('sidebar-off') && (function () { var b = document.getElementById('sidebarMiniMachines'); return !!b && b.offsetWidth > 0; })()",
        "the rail, with its Machines glyph",
        Duration::from_secs(10),
        "document.getElementById('app').className",
    );
    let glyph = reach(&tab, "sidebarMiniMachines");
    harness::click_when_settled(&tab, "#sidebarMiniMachines", "the rail's Machines glyph");
    let after_glyph = eval(&tab, "window.__host.machines");
    harness::click_when_settled(&tab, "#sidebarMiniExpand", "the rail's expand");
    until(
        &tab,
        "!document.getElementById('app').classList.contains('sidebar-off')",
        "the sidebar back",
        Duration::from_secs(10),
        "document.getElementById('app').className",
    );

    // The list at its least, and the theme the reader picks, after a load.
    harness::click_when_settled(&tab, "#themeBtn", "the theme toggle");
    eval(
        &tab,
        "localStorage.setItem('am-sidebar-width', '232'); 'ok'",
    );
    tab.navigate_to(&monitor.url("kit-native")).unwrap();
    tab.wait_until_navigated().unwrap();
    mounted(&tab, "the app again, after a load");
    let reloaded: serde_json::Value =
        serde_json::from_str(eval(&tab, THEMES).as_str().unwrap()).unwrap();
    let narrow = reach(&tab, "machinesBtn");
    let sidebar = eval(
        &tab,
        "(function () { var s = document.querySelector('#app>.sidebar').getBoundingClientRect(), b = document.getElementById('machinesBtn').getBoundingClientRect(); return JSON.stringify({ width: Math.round(s.width), inside: b.left >= s.left && b.right <= s.right + 0.5 }); })()",
    );
    let sidebar: serde_json::Value = serde_json::from_str(sidebar.as_str().unwrap()).unwrap();
    let host: serde_json::Value = serde_json::from_str(
        eval(&tab, "JSON.stringify(window.__host)")
            .as_str()
            .unwrap(),
    )
    .unwrap();
    drop(monitor);

    assert_eq!(
        opened,
        serde_json::json!({ "page": "dark", "list": "dark", "view": "dark", "stored": null }),
        "nothing stored: the app's start theme, everywhere"
    );
    assert!(
        row["reached"] == true && row["before"] == "searchBtn" && row["label"] == "Machines",
        "the Machines row after Search, reached: {row}"
    );
    assert_eq!(after_row, 1, "the row calls onMachines");
    assert!(
        glyph["reached"] == true && glyph["before"] == "sidebarMiniFilter",
        "the rail's glyph after the filter's, reached: {glyph}"
    );
    assert_eq!(after_glyph, 2, "and so does the glyph");
    assert_eq!(
        new_tab, false,
        "detach off: no ↗ on the transcript's top bar"
    );
    assert_eq!(
        reloaded,
        serde_json::json!({ "page": "light", "list": "light", "view": "light", "stored": "light" }),
        "the reader's choice is stored and wins over the start"
    );
    assert!(
        narrow["reached"] == true && sidebar["width"] == 232 && sidebar["inside"] == true,
        "the row reached at the list's least, inside the sidebar: {narrow} {sidebar}"
    );
    assert_eq!(
        (&host["error"], &host["errors"], &host["violations"]),
        (
            &serde_json::Value::Null,
            &serde_json::json!([]),
            &serde_json::json!([])
        ),
        "no error, and nothing breaks the app's policy"
    );
}

/// #s596 on a phone: the Machines row is in the drawer's nav, upright, and in a wide phone's column
/// held sideways, and a finger's tap on it calls `onMachines` in each.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_native_app_s_machines_row_is_reached_on_a_phone() {
    let _serial = serial();
    let (monitor, _base) = native_world("kit-native-phone", 3342);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    harness::phone(&tab, 390, 844);
    harness::answer_in_browser(&tab, native_answers(&monitor));
    tab.navigate_to(&monitor.url("kit-native")).unwrap();
    tab.wait_until_navigated().unwrap();
    mounted(&tab, "the app on a phone");
    harness::click_when_settled(&tab, "#drawerHandle", "the drawer's handle");
    until(
        &tab,
        "!document.getElementById('app').classList.contains('mobile-detail') && (function () { var b = document.getElementById('machinesBtn'); if (!b) return false; var r = b.getBoundingClientRect(); var h = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!h && b.contains(h) && document.getAnimations().every(function (a) { return a.playState !== 'running'; }); })()",
        "the drawer open, its Machines row on top",
        Duration::from_secs(10),
        "document.getElementById('app').className",
    );
    let drawer = reach(&tab, "machinesBtn");
    let centre = |v: &serde_json::Value| {
        (
            v["centre"][0].as_f64().unwrap(),
            v["centre"][1].as_f64().unwrap(),
        )
    };
    let (x, y) = centre(&drawer);
    harness::finger_tap(&tab, x, y);
    harness::until(
        &tab,
        "window.__host.machines === 1",
        "the drawer's row to call onMachines",
        Duration::from_secs(5),
        "window.__host.machines",
    );

    harness::phone(&tab, 956, 440);
    tab.navigate_to(&monitor.url("kit-native")).unwrap();
    tab.wait_until_navigated().unwrap();
    mounted(&tab, "the app on a wide phone held sideways");
    until(
        &tab,
        "(function () { var b = document.getElementById('machinesBtn'); if (!b) return false; var r = b.getBoundingClientRect(); var h = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!h && b.contains(h); })()",
        "the column's Machines row",
        Duration::from_secs(10),
        "JSON.stringify({ app: document.getElementById('app').className, row: (function () { var b = document.getElementById('machinesBtn'); return b ? [b.offsetWidth, b.offsetHeight] : null; })() })",
    );
    let column = reach(&tab, "machinesBtn");
    let (x, y) = centre(&column);
    harness::finger_tap(&tab, x, y);
    harness::until(
        &tab,
        "window.__host.machines === 1",
        "the column's row to call onMachines",
        Duration::from_secs(5),
        "window.__host.machines",
    );
    let host: serde_json::Value = serde_json::from_str(
        eval(&tab, "JSON.stringify(window.__host)")
            .as_str()
            .unwrap(),
    )
    .unwrap();
    drop(monitor);
    assert!(
        drawer["reached"] == true && column["reached"] == true,
        "reached in the drawer and in the column: {drawer} {column}"
    );
    assert_eq!(
        (&host["error"], &host["errors"], &host["violations"]),
        (
            &serde_json::Value::Null,
            &serde_json::json!([]),
            &serde_json::json!([])
        ),
        "no error, and nothing breaks the app's policy"
    );
}
