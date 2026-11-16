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
const host = window.__host = { mounted: false, error: null, opened: [], titles: [], errors: [], tabs: [] };
// A tab the view opens is recorded, never opened: the case loads each address itself (#s433).
window.open = url => { host.tabs.push(String(url)); return null; };
addEventListener("error", event => host.errors.push(String(event.message)));
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
const tab = window.__tab = { mounted: false, error: null, errors: [] };
addEventListener("error", event => tab.errors.push(String(event.message)));
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
            "exports": ["backendHandle", "mountDetached", "mountSessionList", "mountSessionView", "persist", "useBackend"],
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
    let monitor = Monitor::spawn(Kind::V1, 3312, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    harness::answer_in_browser(&tab, kit_answers(r#"detachTo: "/kit-tab","#));
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
        format!("/kit-tab?session={SID}"),
        "the transcript's tab is the host's page with the session"
    );
    assert!(
        tabs[1].starts_with("/kit-tab?root=") && tabs[1].contains("NOTES.md"),
        "the document's tab is the host's page with the held text's place: {tabs:?}"
    );
    assert!(
        tabs[2].starts_with("/kit-tab?") && tabs[2].contains("held=am-image"),
        "the image's tab is the host's page with the copy it was handed: {tabs:?}"
    );

    // Each address, loaded in this tab: its sessionStorage holds what the host page left there.
    let load = |address: &str| {
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
        "({ kind: window.__tab.kind, error: window.__tab.error, errors: window.__tab.errors, path: location.pathname, session: new URLSearchParams(location.search).get('session'), list: !!document.getElementById('sessionList'), ownNewTab: !!document.getElementById('sessionNewTab'), title: document.title !== 'A kit tab' && document.title.length > 0, monitorUi: performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }).filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }) })",
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
        "({ kind: window.__tab.kind, error: window.__tab.error, errors: window.__tab.errors, path: location.pathname, h1: document.querySelector('#detachedView > .detached-doc h1').textContent.trim(), title: document.title, monitorUi: performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }).filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }) })",
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
        "({ kind: window.__tab.kind, error: window.__tab.error, errors: window.__tab.errors, path: location.pathname, title: document.title !== 'A kit tab' && document.title.length > 0, monitorUi: performance.getEntriesByType('resource').map(function (e) { return new URL(e.name).pathname; }).filter(function (p) { return p.indexOf('/monitor-ui/') >= 0; }) })",
    );
    // The handle takes the tab away and leaves the host's element as it gave it.
    let unmounted = harness::probe(
        &tab,
        "(function () { window.__detached.unmount(); var el = document.getElementById('detachedView'); return { empty: !!el && el.childNodes.length === 0, theme: el ? el.dataset.theme || null : 'no element', kind: el ? el.dataset.kind || null : 'no element' }; })()",
    );
    drop(monitor);
    assert_eq!(
        transcript,
        serde_json::json!({ "kind": "session", "error": null, "errors": [], "path": "/kit-tab", "session": SID, "list": false, "ownNewTab": false, "title": true, "monitorUi": [] }),
        "the transcript alone, no list and no ↗ of its own, on the host's route"
    );
    assert_eq!(
        document,
        serde_json::json!({ "kind": "markdown", "error": null, "errors": [], "path": "/kit-tab", "h1": "Field notes", "title": "NOTES.md", "monitorUi": [] }),
        "the document in mdrev's viewer, on the host's route"
    );
    assert_eq!(
        image,
        serde_json::json!({ "kind": "image", "error": null, "errors": [], "path": "/kit-tab", "title": true, "monitorUi": [] }),
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
        &(harness::user_at(
            "where do the terns nest this spring?",
            &harness::at("00:01"),
        ) + &harness::assistant_at(
            "on the north spit, past the second dune",
            &harness::at("00:02"),
        )),
    );
    let monitor = Monitor::spawn(Kind::V1, 3313, &base, Some(&stores), true);
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
    let shown = harness::probe(
        &own,
        "({ path: location.pathname, session: new URLSearchParams(location.search).get('session'), list: !!document.getElementById('sessionList'), ownNewTab: !!document.getElementById('sessionNewTab'), paneNewTab: !!document.querySelector('#previewHead .preview-newtab:not([hidden])'), theme: document.getElementById('sessionView').dataset.theme || 'light' })",
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
}
