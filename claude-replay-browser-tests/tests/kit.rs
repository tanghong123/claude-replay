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
        loaded["kit"].as_u64().unwrap_or(0) as usize >= modules + 1,
        "the page loaded the kit's entry and its {modules} modules: {loaded}"
    );
}
