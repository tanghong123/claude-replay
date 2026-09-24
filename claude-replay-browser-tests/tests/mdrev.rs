//! mdrev's embedded viewer in the app shell's preview pane (#270), in a real Chrome against the
//! REAL released guest — `design/mdrev-in-the-preview-pane.md`. A stand-in bundle would prove
//! nothing about the thing the owner asked for. The guest is the release the monitors PIN (#274,
//! `vendor/mdrev`), built into the binary under test, so these cases need nothing installed and
//! run wherever the suite does, CI included. History, notes and `conform` run the pinned CLI under
//! node (20 or later), which `harness::mdrev_cli` demands by name.
//!
//! Ports 2821–2829. The classic page has no preview pane, so there is no second surface here: the
//! owner named the right-most pane, which only the app shell has. 2826–2829 are #271: the pane's
//! document in a tab of its own (`/markdown`).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod harness;
use harness::{
    at, base, chrome, eval, mdrev_cli, mdrev_pin, read_tool_at, serial, tool_result_at, until,
    user_at, Kind, Monitor, Stores,
};

const SID: &str = "5e5510a1-0000-4000-8000-000000000270";
const NOTES: &str = "# Field notes\n\nA paragraph with **bold** text.\n\n- one\n- two\n";

/// A checkout with `docs/guide.md` committed twice — the second time linking `docs/other.md` — and
/// a session — its cwd that checkout, so containment explains the file — which READ the guide (a
/// stamped path in a tool head) and carries `NOTES.md` as an attachment WITH its text: the
/// transcript's own Markdown.
fn fixture(name: &str) -> (PathBuf, Stores, PathBuf) {
    let base = base(name);
    let stores = Stores::new(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("docs/guide.md"), "# Old guide\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "first"]);
    std::fs::write(
        repo.join("docs/guide.md"),
        "# The guide\n\nSome **bold** prose, and [the other guide](other.md).\n",
    )
    .unwrap();
    std::fs::write(repo.join("docs/other.md"), "# The other guide\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "second"]);

    let guide = repo.join("docs/guide.md").display().to_string();
    let notes_path = repo.join("NOTES.md").display().to_string();
    let notes_json = NOTES.replace('\n', "\\n");
    let attachment = format!(
        "{{\"type\":\"attachment\",\"timestamp\":\"{ts}\",\"attachment\":{{\"type\":\"file\",\"filename\":\"{notes_path}\",\"displayPath\":\"NOTES.md\",\"content\":{{\"type\":\"text\",\"file\":{{\"filePath\":\"{notes_path}\",\"content\":\"{notes_json}\",\"numLines\":6,\"startLine\":1,\"totalLines\":6}}}}}}}}\n",
        ts = at("00:04")
    );
    let jsonl = [
        user_at("read the guide and the notes", &at("00:01")),
        read_tool_at("t1", &guide, &at("00:02")),
        tool_result_at("t1", &at("00:03")),
        attachment,
    ]
    .concat()
    .replace("\"cwd\":\"/r\"", &format!("\"cwd\":\"{}\"", repo.display()));
    stores.claude_session(SID, &jsonl);
    (base, stores, repo)
}

fn open_shell(m: &Monitor, tab: &headless_chrome::Tab) {
    m.pair(tab);
    m.open(tab, &format!("?ui=app&session={SID}"));
    until(
        tab,
        "!!document.querySelector('[data-attachment-action=\"preview\"]') && !!document.querySelector('[data-reference-path]')",
        "the session's attachment card and its stamped path",
        Duration::from_secs(30),
        "document.querySelector('.transcript') ? document.querySelector('.transcript').innerText.slice(0, 300) : 'no transcript'",
    );
    // An English reader, on every machine: mdrev takes its language from `navigator.languages`
    // when its module first loads — which is the first Markdown mount, still ahead — and this
    // harness's Chrome otherwise inherits the machine's (Chinese here, English on CI).
    eval(
        tab,
        "Object.defineProperty(Navigator.prototype, 'languages', { configurable: true, get: () => ['en-US'] }); 'ok'",
    );
}

/// What the preview pane holds right now, for a failure message.
const PANE: &str = "(function(){ var b = document.getElementById('previewBody'); return b ? b.className + ' | ' + b.innerHTML.slice(0, 400) : 'no pane'; })()";

fn open_notes(tab: &headless_chrome::Tab) {
    eval(
        tab,
        "document.querySelector('[data-attachment-action=\"preview\"]').click(); 'ok'",
    );
}

fn open_guide(tab: &headless_chrome::Tab, repo: &Path) {
    let guide = repo.join("docs/guide.md").display().to_string();
    eval(
        tab,
        &format!("document.querySelector('[data-reference-path={guide:?}]').click(); 'ok'"),
    );
}

fn h1(tab: &headless_chrome::Tab) -> String {
    eval(
        tab,
        "(document.querySelector('#previewBody .mdrev-host h1') || {}).textContent || ''",
    )
    .as_str()
    .unwrap_or("")
    .trim()
    .to_string()
}

/// mdrev's notes control on its toolbar, as `hidden|shown`, or `absent`. The guide: `annotate: false`
/// "hides the notes control and the selection invite" — in the pinned bundle, `hidden: !notesAllowed`
/// on a button whose `aria-label` is `Q("notes")`, the English source string itself for the English
/// reader `open_shell` pins. Its label is all it carries, so a relabelled control reads `absent`,
/// which fails rather than passes.
const NOTES_CONTROL: &str = "(function(){ var b = [...document.querySelectorAll('#previewBody .mdrev-host .topbar button')].find(b => b.getAttribute('aria-label') === 'notes'); return b ? (b.hidden || b.offsetWidth === 0 ? 'hidden' : 'shown') : 'absent'; })()";

/// mdrev's invitation to select text and file a note — only drawn where notes are allowed.
const NOTE_INVITE: &str =
    "document.querySelectorAll('#previewBody .mdrev-host .note-hint-x').length";

/// The owner: "For embedded contents, only show a cleanly rendered viewer (as a reader)". The
/// attachment's text is rendered by mdrev — a real heading, not `# Field notes` in a `<pre>` — held
/// by the monitor for the contract, and with no toolbar at all.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn mdrev_renders_transcript_markdown_as_a_clean_reader() {
    let _serial = serial();
    let (base, stores, _repo) = fixture("mdrev-reader");
    let m = Monitor::spawn(Kind::V2, 2821, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    assert_eq!(
        eval(&tab, "document.body.dataset.mdrev").as_str(),
        Some(mdrev_pin().as_str()),
        "the page names the pinned release, and nothing on this machine chose it"
    );
    open_notes(&tab);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "mdrev to render the held notes",
        Duration::from_secs(30),
        PANE,
    );
    assert_eq!(h1(&tab), "Field notes");
    assert_eq!(
        eval(
            &tab,
            "document.querySelector('#previewBody .mdrev-host strong').textContent"
        )
        .as_str(),
        Some("bold"),
        "Markdown rendered, not shown as source"
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.mdrev-pane').dataset.root").as_str(),
        Some("held")
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('#previewBody .mdrev-host .topbar').length"
        )
        .as_i64(),
        Some(0),
        "a reader: no toolbar at all (mdrev's toolbar: 'none')"
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('#previewBody pre.artifact-text').length"
        )
        .as_i64(),
        Some(0),
        "and not the old text view"
    );
}

/// The owner: "otherwise, show toolbar to gain the revision and annotation capabilities of mdrev".
/// A file on disk — a stamped path from a tool head — mounts the whole viewer over the file's own
/// checkout, with mdrev's toolbar.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn mdrev_renders_a_local_markdown_file_with_its_toolbar() {
    let _serial = serial();
    drop(mdrev_cli()); // the monitor's history and notes run the pinned CLI under node
    let (base, stores, repo) = fixture("mdrev-local");
    let m = Monitor::spawn(Kind::V2, 2822, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "mdrev to render the guide",
        Duration::from_secs(30),
        PANE,
    );
    assert_eq!(h1(&tab), "The guide", "the file as it stands");
    until(
        &tab,
        "document.querySelectorAll('#previewBody .mdrev-host .topbar').length > 0",
        "mdrev's toolbar — review and notes are effective, so it is always present",
        Duration::from_secs(20),
        PANE,
    );
    assert_eq!(
        eval(&tab, "document.querySelector('.mdrev-pane').dataset.root").as_str(),
        Some(repo.display().to_string().as_str()),
        "the collection is the file's checkout"
    );
    assert_eq!(
        eval(&tab, NOTES_CONTROL).as_str(),
        Some("shown"),
        "notes are allowed: the monitor runs the pinned CLI under node"
    );
    assert_eq!(
        eval(&tab, NOTE_INVITE).as_i64(),
        Some(1),
        "and mdrev invites one"
    );
}

/// mdrev acts on its own keys while the reader is engaged with it; the shell's document-wide keymap
/// must not answer the same press. `\` is the hardest case — a `when: "any"` binding (the sidebar),
/// which no context value can switch off. Red without `inGuest` in shared/keymap.js.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn mdrev_owns_a_key_pressed_while_the_reader_is_engaged_with_it() {
    let _serial = serial();
    let (base, stores, repo) = fixture("mdrev-keys");
    let m = Monitor::spawn(Kind::V2, 2823, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "mdrev to render the guide",
        Duration::from_secs(30),
        PANE,
    );
    let sidebar_off = || {
        eval(
            &tab,
            "document.getElementById('app').classList.contains('sidebar-off')",
        )
        .as_bool()
        .unwrap_or(false)
    };
    let before = sidebar_off();
    harness::quiet_keys(&tab);
    // Every keydown the page sees, with where it landed — so a press that never arrived cannot
    // pass the first half by doing nothing (the control below would say so, and this says why).
    eval(&tab, "window.__keys = []; document.addEventListener('keydown', e => window.__keys.push(e.key + '@' + (e.target.className || e.target.tagName)), true); 'ok'");
    let seen = || eval(&tab, "JSON.stringify(window.__keys)");

    // Engage mdrev the way a reader does — a real click on its prose, which leaves the focus on
    // <body>: ENGAGEMENT, not the keydown's target, is what says whose key this is.
    tab.find_element("#previewBody .mdrev-host h1")
        .unwrap()
        .click()
        .unwrap();
    tab.press_key("\\").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        seen().as_str().unwrap_or("").contains("\\\\@BODY"),
        "the press reached the page, on <body>: {:?}",
        seen()
    );
    assert_eq!(
        sidebar_off(),
        before,
        "a key pressed inside mdrev moved the shell (keys: {:?})",
        seen()
    );

    // The control: outside the guest the same key is the shell's — the case is not vacuous.
    tab.find_element(".transcript").unwrap().click().unwrap();
    tab.press_key("\\").unwrap();
    until(
        &tab,
        &format!("document.getElementById('app').classList.contains('sidebar-off') !== {before}"),
        "the same key outside mdrev to toggle the sidebar",
        Duration::from_secs(5),
        "JSON.stringify(window.__keys) + ' active=' + (document.activeElement && document.activeElement.tagName)",
    );
}

/// No node (#274): the pinned guest needs none, and neither does history — the contract makes the
/// host's store the source of revisions, and the monitor reads them from git — but notes are
/// mdrev's own format, written only through its CLI. So the local file keeps its toolbar and its
/// two revisions, and the monitor refuses notes, which mdrev renders by hiding their control.
/// `MDREV_NODE` is final when set (mdrev's own rule): that is how the case takes node away on a
/// machine that has one.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn without_node_a_local_markdown_file_keeps_its_history_and_takes_no_notes() {
    let _serial = serial();
    let (base, stores, repo) = fixture("mdrev-nonode");
    let m = Monitor::spawn_with(
        Kind::V2,
        2824,
        &base,
        Some(&stores),
        true,
        &[("MDREV_NODE", "/nonexistent-node")],
    );
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "document.querySelectorAll('#previewBody .mdrev-host .topbar').length > 0",
        "the guide with mdrev's toolbar — history is in play without node",
        Duration::from_secs(30),
        PANE,
    );
    assert_eq!(h1(&tab), "The guide");
    eval(
        &tab,
        "(function(){ var d = document.querySelector('.mdrev-pane').dataset; \
         fetch('/api/mdrev/revisions?root=' + encodeURIComponent(d.root) + '&path=' + \
         encodeURIComponent(d.path) + '&cap=' + d.cap).then(r => r.json()) \
         .then(l => { window.__revisions = l.map(r => r.subject).join(','); }); return 'ok'; })()",
    );
    until(
        &tab,
        "window.__revisions === 'second,first'",
        "the guide's two commits, newest first, from git",
        Duration::from_secs(10),
        "String(window.__revisions)",
    );
    assert_eq!(
        eval(&tab, NOTES_CONTROL).as_str(),
        Some("hidden"),
        "no node, no notes: the monitor refuses them and mdrev hides its control"
    );
    assert_eq!(
        eval(&tab, NOTE_INVITE).as_i64(),
        Some(0),
        "nor does it invite one"
    );
}

/// mdrev's own definition of a host: `mdrev-cli conform` against the live monitor — every route
/// and shape, the refusals, and the round trip: a note filed through our routes, found in the
/// sidecar with the code `mdrev --notes` uses, closed and deleted. The guide: when your host passes
/// it, you are done. The capability is read off the page, where a reader gets one too.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn mdrev_contract_passes_mdrev_cli_conform() {
    let _serial = serial();
    let (base, stores, repo) = fixture("mdrev-conform");
    let port = 2825;
    let m = Monitor::spawn(Kind::V2, port, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!(document.querySelector('.mdrev-pane') || {dataset: {}}).dataset.cap",
        "the mount's capability",
        Duration::from_secs(30),
        PANE,
    );
    let fact = |k: &str| {
        eval(
            &tab,
            &format!("document.querySelector('.mdrev-pane').dataset.{k}"),
        )
        .as_str()
        .unwrap_or("")
        .to_string()
    };
    let (root, path, cap) = (fact("root"), fact("path"), fact("cap"));
    assert_eq!(path, "docs/guide.md");
    let token = m.token().expect("a paired monitor has a token");
    let out = mdrev_cli()
        .args([
            "conform",
            "--url",
            &format!("http://127.0.0.1:{port}/api/mdrev"),
        ])
        .args(["--path", &path, "--root", &root, "--cap", &cap])
        .args(["--header", &format!("Cookie: cmauth={token}")])
        .args(["--review", "--annotate"])
        .output()
        .expect("mdrev-cli runs");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "conform:\n{report}");
    assert!(report.contains("conforms"), "{report}");
}

// ------------------------------------------------------------------ a tab of its own (#271)

/// What a tab of its own holds right now, for a failure message.
const OWN: &str = "(function(){ var d = document.getElementById('doc'); return location.href + ' | ' + (d ? d.className + ' | ' + d.innerHTML.slice(0, 300) : 'no #doc'); })()";

/// The app shell for an UNPAIRED monitor: held text needs no pairing, and a case that proves so
/// must not pair first.
fn open_shell_unpaired(m: &Monitor, tab: &headless_chrome::Tab) {
    m.open(tab, &format!("?ui=app&session={SID}"));
    until(
        tab,
        "!!document.querySelector('[data-attachment-action=\"preview\"]')",
        "the session's attachment card",
        Duration::from_secs(30),
        "document.querySelector('.transcript') ? document.querySelector('.transcript').innerText.slice(0, 300) : 'no transcript'",
    );
}

/// The pane's control for a tab of its own, pressed the way a reader presses it — a trusted click,
/// which is what lets `window.open` past the popup blocker.
fn open_in_a_tab(tab: &headless_chrome::Tab) {
    until(
        tab,
        "!!document.querySelector('#previewHead .preview-newtab:not([hidden])')",
        "the pane's open-in-a-tab control",
        Duration::from_secs(10),
        PANE,
    );
    tab.find_element("#previewHead .preview-newtab")
        .unwrap()
        .click()
        .unwrap();
}

/// The tab the pane opened, once it has its address.
fn opened_tab(browser: &headless_chrome::Browser) -> std::sync::Arc<headless_chrome::Tab> {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let found = browser
            .get_tabs()
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.get_url().contains("/markdown?"))
            .cloned();
        if let Some(tab) = found {
            return tab;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the pane opened no tab at /markdown?"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn own_h1(tab: &headless_chrome::Tab) -> String {
    eval(
        tab,
        "(document.querySelector('#doc.mdrev-host h1') || {}).textContent || ''",
    )
    .as_str()
    .unwrap_or("")
    .trim()
    .to_string()
}

/// The owner: "open the markdown file shown in the right pane in a standalone tab". Text the
/// transcript carries opens there as it reads in the pane — the clean reader, no toolbar — from a
/// monitor that was never paired: held text touches no disk, so it asks for no pairing.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn mdrev_opens_held_text_in_a_tab_of_its_own() {
    let _serial = serial();
    let (base, stores, _repo) = fixture("mdrev-tab-held");
    let m = Monitor::spawn(Kind::V2, 2826, &base, Some(&stores), false);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell_unpaired(&m, &tab);
    open_notes(&tab);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "mdrev to render the held notes in the pane",
        Duration::from_secs(30),
        PANE,
    );
    open_in_a_tab(&tab);
    let own = opened_tab(&browser);
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the held notes in a tab of their own",
        Duration::from_secs(30),
        OWN,
    );
    assert_eq!(own_h1(&own), "Field notes");
    assert_eq!(
        eval(&own, "document.getElementById('doc').dataset.root").as_str(),
        Some("held")
    );
    assert_eq!(
        eval(
            &own,
            "document.querySelectorAll('#doc.mdrev-host .topbar').length"
        )
        .as_i64(),
        Some(0),
        "a reader there too: no toolbar"
    );
    assert_eq!(eval(&own, "document.title").as_str(), Some("NOTES.md"));
}

/// A file on disk opens in its tab as the whole viewer, at the range the reader chose in the pane —
/// and the tab is a frame around the same guarded routes, not a new way to read: a forged
/// capability in its address shows a refusal, never the text.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn mdrev_opens_a_file_in_a_tab_where_the_reader_was() {
    let _serial = serial();
    drop(mdrev_cli()); // the file's history and notes run the pinned CLI under node
    let (base, stores, repo) = fixture("mdrev-tab-local");
    let m = Monitor::spawn(Kind::V2, 2827, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "document.querySelectorAll('#previewBody .mdrev-host .topbar button.pick').length > 0",
        "the guide on mdrev's toolbar, with its range picks",
        Duration::from_secs(30),
        PANE,
    );
    // The reader picks the file's last change on mdrev's own toolbar (the pick reads "1" in every
    // language), and the pane hears it.
    eval(
        &tab,
        "[...document.querySelectorAll('#previewBody .mdrev-host .topbar button.pick')].find(b => b.textContent.trim() === '1').click(); 'ok'",
    );
    until(
        &tab,
        "(function(){ var d = document.querySelector('.mdrev-pane').dataset; return !!(d.from || d.to); })()",
        "the pane to hear the range the reader chose",
        Duration::from_secs(10),
        PANE,
    );
    let range = |t: &headless_chrome::Tab, el: &str| {
        eval(
            t,
            &format!("(function(){{ var d = document.querySelector('{el}').dataset; return (d.from || '') + '..' + (d.to || ''); }})()"),
        )
        .as_str()
        .unwrap_or("")
        .to_string()
    };
    let chosen = range(&tab, ".mdrev-pane");
    open_in_a_tab(&tab);
    let own = opened_tab(&browser);
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the guide in a tab of its own",
        Duration::from_secs(30),
        OWN,
    );
    // The tab shows what the pane shows, and the range is what decides it: a tab that dropped the
    // range would read the file as it stands ("The guide"), while the pick took the reader to a
    // revision from before the last change.
    let shown = own_h1(&own);
    assert_eq!(
        shown, "Old guide",
        "the tab reads the guide at the reader's range"
    );
    until(
        &tab,
        &format!("(document.querySelector('#previewBody .mdrev-host h1') || {{}}).textContent === {shown:?}"),
        "the pane and its tab to show the same revision",
        Duration::from_secs(10),
        PANE,
    );
    assert_eq!(
        range(&own, "#doc"),
        chosen,
        "the tab opened at the reader's range: {}",
        own.get_url()
    );
    let (from, to) = chosen.split_once("..").unwrap();
    for (key, value) in [("from", from), ("to", to)] {
        assert!(
            value.is_empty() || own.get_url().contains(&format!("{key}={value}")),
            "the address carries it: {}",
            own.get_url()
        );
    }
    until(
        &own,
        "document.querySelectorAll('#doc.mdrev-host .topbar').length > 0",
        "the whole viewer — history and notes are in play for a file",
        Duration::from_secs(20),
        OWN,
    );
    assert_eq!(
        eval(&own, "document.title").as_str(),
        Some("guide.md"),
        "the tab names its document"
    );

    let cap = eval(&own, "document.getElementById('doc').dataset.cap")
        .as_str()
        .unwrap_or("")
        .to_string();
    assert_eq!(cap.len(), 64, "a stamp: {cap:?}");
    let forged = browser.new_tab().unwrap();
    forged
        .navigate_to(&own.get_url().replace(&cap, &"0".repeat(64)))
        .unwrap();
    forged.wait_until_navigated().unwrap();
    until(
        &forged,
        "document.getElementById('doc').classList.contains('unavailable')",
        "the forged address refused",
        Duration::from_secs(20),
        OWN,
    );
    assert_eq!(
        eval(&forged, "document.querySelectorAll('.mdrev-host').length").as_i64(),
        Some(0),
        "no viewer, no text: {}",
        eval(&forged, OWN)
    );
}

/// Held text lives in the monitor's memory, which a restart empties — so the pane leaves the tab its
/// own copy, and a tab kept open across a restart shows its document again on reload. The control:
/// without that copy, the same address after another restart says the text is gone rather than
/// showing nothing.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn a_tab_of_held_text_survives_a_monitor_restart() {
    let _serial = serial();
    let (base, stores, _repo) = fixture("mdrev-tab-restart");
    let m = Monitor::spawn(Kind::V2, 2828, &base, Some(&stores), false);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell_unpaired(&m, &tab);
    open_notes(&tab);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "mdrev to render the held notes in the pane",
        Duration::from_secs(30),
        PANE,
    );
    open_in_a_tab(&tab);
    let own = opened_tab(&browser);
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the held notes in a tab of their own",
        Duration::from_secs(30),
        OWN,
    );

    drop(m); // the monitor goes, and the text it held with it
    let m = Monitor::spawn(Kind::V2, 2828, &base, Some(&stores), false);
    own.reload(false, None).unwrap();
    own.wait_until_navigated().unwrap();
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the notes again, held from the tab's own copy",
        Duration::from_secs(30),
        OWN,
    );
    assert_eq!(own_h1(&own), "Field notes");

    eval(&own, "sessionStorage.clear(); 'ok'");
    drop(m);
    let _m = Monitor::spawn(Kind::V2, 2828, &base, Some(&stores), false);
    own.reload(false, None).unwrap();
    own.wait_until_navigated().unwrap();
    until(
        &own,
        "document.getElementById('doc').classList.contains('unavailable')",
        "without its copy, the tab to say the text is gone",
        Duration::from_secs(20),
        OWN,
    );
    let said = eval(&own, "document.getElementById('doc').textContent")
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(said.contains("no longer held"), "{said}");
}

/// The reader follows a link to another document of the collection, inside mdrev: the pane learns
/// the capability for it (`resolve`, asked in the name of the document it was given) and a tab of
/// its own opens THAT document — not the one the pane started with, and not with the capability
/// that opened it, which names another path.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn mdrev_follows_the_reader_to_another_document_into_its_tab() {
    let _serial = serial();
    drop(mdrev_cli()); // the file's history and notes run the pinned CLI under node
    let (base, stores, repo) = fixture("mdrev-tab-follow");
    let m = Monitor::spawn(Kind::V2, 2829, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host a[href$=\"other.md\"]')",
        "the guide, with its link to the other guide",
        Duration::from_secs(30),
        PANE,
    );
    let pane_cap = |t: &headless_chrome::Tab| {
        eval(t, "document.querySelector('.mdrev-pane').dataset.cap")
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let guide_cap = pane_cap(&tab);
    // mdrev draws a link before it has finished preparing it, and a click that lands first follows
    // nothing (measured: the same click a moment later moves the pane) — so the reader clicks
    // again until the pane moves, as a reader would.
    let link = "#previewBody .mdrev-host a[href$=\"other.md\"]";
    let moved = "(function(){ var d = document.querySelector('.mdrev-pane').dataset; return d.path === 'docs/other.md' && d.cap.length === 64; })()";
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while eval(&tab, moved).as_bool() != Some(true) {
        assert!(
            std::time::Instant::now() < deadline,
            "the pane never followed the reader to the other guide, with its own capability: {}",
            eval(&tab, PANE)
        );
        if let Ok(element) = tab.find_element(link) {
            let _ = element.click();
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
    assert_ne!(
        pane_cap(&tab),
        guide_cap,
        "a capability names one path: the guide's cannot open the other guide"
    );
    open_in_a_tab(&tab);
    let own = opened_tab(&browser);
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the other guide in a tab of its own",
        Duration::from_secs(30),
        OWN,
    );
    assert_eq!(own_h1(&own), "The other guide", "{}", own.get_url());
    assert!(
        own.get_url().contains("path=docs%2Fother.md"),
        "{}",
        own.get_url()
    );
}
