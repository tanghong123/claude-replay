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
            "exports": ["backendHandle", "mergedBackend", "mountSessionList", "mountSessionView", "persist", "splitMachineId", "useBackend"],
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
    let index = sessions_of(&other, SID_LISTED);
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
const host = window.__host = { mounted: false, error: null, errors: [], opened: [], views: [], asked: [] };
addEventListener("error", event => host.errors.push(String(event.message)));
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
    let mut answers: Vec<Answer> = kit_answers()
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
