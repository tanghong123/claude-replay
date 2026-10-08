//! mdrev's embedded viewer in the app shell's preview pane (#270), in a real Chrome against the
//! REAL released guest — `design/mdrev-in-the-preview-pane.md`. A stand-in bundle would prove
//! nothing about the thing the owner asked for. The guest is the release the monitors PIN (#274,
//! `vendor/mdrev`), built into the binary under test, so these cases need nothing installed and
//! run wherever the suite does, CI included. History, notes and `conform` run the pinned CLI under
//! node (20 or later), which `harness::mdrev_cli` demands by name.
//!
//! Ports 2821–2829. The classic page has no preview pane, so there is no second surface here: the
//! owner named the right-most pane, which only the app shell has. 2826–2829 are #271: the pane's
//! document in a tab of its own (`/markdown`). 3070–3071 are #s13:
//! every text file through mdrev, as source code when it is not Markdown.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod harness;
use harness::{
    at, base, chrome_tab, eval, mdrev_cli, mdrev_pin, read_tool_at, serial, tool_result_at, until,
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
    let (_browser, tab) = chrome_tab();
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
    let (_browser, tab) = chrome_tab();
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
    // #272: the file manager beside mdrev's view too — the pane head's one control for any file.
    assert_eq!(
        eval(
            &tab,
            "(function(){ var b = document.querySelector('#previewHead .preview-reveal'); return !!b && !b.hidden && b.offsetWidth > 0; })()"
        )
        .as_bool(),
        Some(true),
        "the file manager offered beside the Markdown file"
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
    let (_browser, tab) = chrome_tab();
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
    let (_browser, tab) = chrome_tab();
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
    let (_browser, tab) = chrome_tab();
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
    let (browser, tab) = chrome_tab();
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
    let (browser, tab) = chrome_tab();
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
    let (browser, tab) = chrome_tab();
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
    let (browser, tab) = chrome_tab();
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

/// #s10's world: the fixture's checkout with a review store of its own (a local bare repository,
/// named by a committed `.mdrev.json`), a mdrev state directory of the case's own with its viewer key
/// (never this machine's `~/.mdrev`), and the machine paired with the store the way the owner pairs
/// — an agent asks, the viewer key confirms. Returns the state directory beside the fixture.
fn shared_review_world(case: &str) -> (PathBuf, Stores, PathBuf, PathBuf) {
    let (base, stores, repo) = fixture(case);
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let store = base.join("store.git");
    git(&["init", "-q", "--bare", &store.display().to_string()]);
    let r = repo.display().to_string();
    git(&["-C", &r, "config", "user.email", "t@example.invalid"]);
    git(&["-C", &r, "config", "user.name", "T"]);
    git(&["-C", &r, "config", "commit.gpgsign", "false"]);
    std::fs::write(
        repo.join(".mdrev.json"),
        format!(
            "{{\"review\": {{\"remote\": \"file://{}\", \"branch\": \"refs/notes/mdrev-review\"}}}}\n",
            store.display()
        ),
    )
    .unwrap();
    git(&["-C", &r, "add", ".mdrev.json"]);
    git(&["-C", &r, "commit", "-qm", "the review store"]);
    let state = base.join("mdrev-state");
    std::fs::create_dir_all(&state).unwrap();
    let key = "5e5510a1c0ffee00000000000000000000000000000000000000000000000010";
    std::fs::write(state.join("token"), key).unwrap();
    let cli = |args: &[&str], viewer: bool| {
        let mut c = mdrev_cli();
        c.args(args)
            .args(["--root", &r])
            .env("MDREV_STATE_DIR", &state);
        if viewer {
            c.env("MDREV_VIEWER_KEY", key);
        } else {
            c.env_remove("MDREV_VIEWER_KEY").env("CLAUDECODE", "1");
        }
        let out = c.output().expect("mdrev-cli runs");
        assert!(
            out.status.success(),
            "mdrev-cli {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    cli(
        &[
            "review",
            "pair",
            "--email",
            "t@example.invalid",
            "--name",
            "T",
        ],
        false,
    );
    cli(&["review", "pair", "--confirm", "--viewer"], true);

    (base, stores, repo, state)
}

/// #s10, the owner: shared review "maybe not in the main interface, but in the full detached
/// view". A checkout whose `.mdrev.json` names a review store (here a local bare repository), and a
/// machine paired with it the way the owner pairs — an agent asks, the viewer key confirms — in a
/// mdrev state directory of the case's own (never this machine's `~/.mdrev`). The pane's guest asks
/// `review` and is told 404, so it offers no Share and no Push; the detached tab's guest, on the
/// review prefix, is answered with the store's state, paired. And mdrev's own definition of a host
/// holds on that prefix too: `mdrev-cli conform`, every route and the note round trip.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn the_detached_tab_offers_shared_review_and_the_pane_does_not() {
    let _serial = serial();
    let (base, stores, repo, state) = shared_review_world("mdrev-shared");

    let port = 2945;
    let m = Monitor::spawn_with(
        Kind::V2,
        port,
        &base,
        Some(&stores),
        true,
        &[("MDREV_STATE_DIR", &state.display().to_string())],
    );
    let (browser, tab) = chrome_tab();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "the guide in the pane",
        Duration::from_secs(30),
        PANE,
    );
    // The pane's prefix, asked with the pane's own document stamp: no shared review there, so its
    // guest (which does not even ask) could never be offered Share or Push.
    let pane = eval(
        &tab,
        "(async function(){ var d = document.querySelector('.mdrev-pane').dataset; var q = 'root=' + encodeURIComponent(d.root) + '&path=' + encodeURIComponent(d.path) + '&cap=' + encodeURIComponent(d.cap); var r = await fetch(d.contract + '/review?' + q); return d.contract + ' ' + r.status; })()",
    );
    assert_eq!(
        pane.as_str().unwrap_or(""),
        "/api/mdrev 404",
        "the pane offers no shared review"
    );
    assert_eq!(
        eval(&tab, "(function(){ var b = document.querySelector('.preview-review'); return !b || b.hidden; })()"),
        true,
        "the desktop has its detached tab, and no review sheet (#s12)"
    );

    open_in_a_tab(&tab);
    let own = opened_tab(&browser);
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the guide in a tab of its own",
        Duration::from_secs(30),
        OWN,
    );
    // The detached tab's guest is mounted on the REVIEW prefix (#s10), and the next check asks that
    // prefix for the store with the tab's own document facts. Not "the guest asked": when mdrev
    // asks is its own business, and reading it off the browser's resource timing was red on CI's
    // Linux runner, where the guest had not asked by the deadline.
    until(
        &own,
        "(document.getElementById('doc') || {dataset: {}}).dataset.contract === '/api/mdrev-review'",
        "the detached tab mounted on the review prefix",
        Duration::from_secs(20),
        "JSON.stringify((document.getElementById('doc') || {dataset: {}}).dataset)",
    );
    let paired_as = eval(
        &own,
        "(async function(){ var d = document.getElementById('doc').dataset; var q = 'root=' + encodeURIComponent(d.root) + '&path=' + encodeURIComponent(d.path) + '&cap=' + encodeURIComponent(d.cap); var r = await fetch(d.contract + '/review?' + q); var j = await r.json(); return r.status + ' ' + (j.paired && j.paired.email) + (j.error ? ' — ' + j.error : ''); })()",
    );
    assert_eq!(
        paired_as.as_str().unwrap_or(""),
        "200 t@example.invalid",
        "…and is answered with the store's state, paired as the case paired"
    );

    // mdrev's definition of a host, on the review prefix.
    let fact = |k: &str| {
        eval(&own, &format!("document.getElementById('doc').dataset.{k}"))
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let (root, path, cap) = (fact("root"), fact("path"), fact("cap"));
    let token = m.token().expect("a paired monitor has a token");
    let out = mdrev_cli()
        .args([
            "conform",
            "--url",
            &format!("http://127.0.0.1:{port}/api/mdrev-review"),
        ])
        .args(["--path", &path, "--root", &root, "--cap", &cap])
        .args(["--header", &format!("Cookie: cmauth={token}")])
        .args(["--review", "--annotate"])
        .env("MDREV_STATE_DIR", &state)
        .output()
        .expect("mdrev-cli runs");
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && report.contains("conforms"),
        "conform on the review prefix:\n{report}"
    );
}

/// #s49, the owner on 1.360.0: "the layout of mdrev is messed up … I mean the tool bar area", upright
/// and sideways, and sometimes it "could magically fix itself". mdrev's toolbar is a `header.topbar`, and
/// the app shell's phone rules for its OWN top bar were written `#app .topbar` — an id that outranked
/// mdrev's own rules inside the pane (no wrapping, the row spread, the shell's padding). They are
/// scoped to `.workspace>.topbar` now, so the pane's toolbar keeps mdrev's own layout.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn a_phone_pane_leaves_mdrevs_toolbar_its_own_layout() {
    let _serial = serial();
    let (base, stores, repo) = fixture("mdrev-toolbar");
    let m = Monitor::spawn(Kind::V2, 2943, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 440, 956);
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1') && !!document.querySelector('#preview .mdrev-host .topbar')",
        "the guide in the pane with mdrev's toolbar",
        Duration::from_secs(30),
        PANE,
    );
    let bar = "(function(){ var t = document.querySelector('#preview .mdrev-host .topbar'), c = getComputedStyle(t); return JSON.stringify({ wrap: c.flexWrap, justify: c.justifyContent }); })()";
    for (w, h) in [(440u32, 956u32), (956, 440)] {
        harness::phone(&tab, w, h);
        until(
            &tab,
            &format!("innerWidth === {w}"),
            "the new size",
            Duration::from_secs(5),
            "innerWidth",
        );
        let seen: serde_json::Value =
            serde_json::from_str(eval(&tab, bar).as_str().unwrap_or("{}")).unwrap();
        assert!(
            seen["wrap"] != "nowrap" && seen["justify"] != "space-between",
            "{w}x{h}: mdrev's toolbar keeps its own layout, none of the shell's top bar rules: {seen}"
        );
    }
}

/// #s12, the owner chose it: on a phone, where a tab of its own has no way back (#335), shared review
/// is a FULL-SCREEN SHEET over the app. The pane's Markdown shows a Review control there; the sheet
/// mounts the document on the review prefix (#s10), its guest asking for and fetching the store, and
/// its close control returns the reader to the pane and the transcript exactly as they were — no
/// navigation, no reload, the transcript's offset unchanged.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn a_phone_reviews_in_a_full_screen_sheet_and_closes_back_to_where_it_was() {
    let _serial = serial();
    let (base, stores, repo, state) = shared_review_world("mdrev-sheet");
    let m = Monitor::spawn_with(
        Kind::V2,
        2972,
        &base,
        Some(&stores),
        true,
        &[("MDREV_STATE_DIR", &state.display().to_string())],
    );
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 440, 956);
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1') && (function(){ var b = document.querySelector('.preview-review'); return !!b && !b.hidden && b.offsetWidth > 0; })()",
        "the guide in the pane, with its Review control",
        Duration::from_secs(30),
        PANE,
    );
    let offset = || {
        eval(
            &tab,
            "Math.round(document.querySelector('.transcript').scrollTop)",
        )
        .as_i64()
        .unwrap_or(-1)
    };
    let before = offset();
    eval(
        &tab,
        "document.querySelector('.preview-review').click(); 'ok'",
    );
    // The sheet's guest is mounted on the REVIEW prefix (#s10), and that prefix, asked for the store
    // with the sheet's own document facts, answers paired. Not "the guest asked": when mdrev asks is
    // its own business — it differs with the layout it picks for the device — and reading it off the
    // browser's resource timing was intermittently red on CI's Linux runner.
    until(
        &tab,
        "!!document.querySelector('.review-sheet .mdrev-host h1') && (document.querySelector('.review-sheet .mdrev-pane') || {dataset: {}}).dataset.contract === '/api/mdrev-review'",
        "the review sheet, mounted on the review prefix",
        Duration::from_secs(30),
        "(function(){ var s = document.querySelector('.review-sheet'); return s ? s.innerText.slice(0, 200) + ' | ' + JSON.stringify((s.querySelector('.mdrev-pane') || {dataset: {}}).dataset) : 'no sheet'; })()",
    );
    let paired_as = eval(
        &tab,
        "(async function(){ var d = document.querySelector('.review-sheet .mdrev-pane').dataset; var q = 'root=' + encodeURIComponent(d.root) + '&path=' + encodeURIComponent(d.path) + '&cap=' + encodeURIComponent(d.cap); var r = await fetch(d.contract + '/review?' + q); var j = await r.json(); return r.status + ' ' + (j.paired && j.paired.email) + (j.error ? ' — ' + j.error : ''); })()",
    );
    assert_eq!(
        paired_as.as_str().unwrap_or(""),
        "200 t@example.invalid",
        "the sheet's prefix answers with the store's state, paired as the case paired (#s34: a red run names mdrev-cli's error)"
    );
    // The document is READ there: its heading across the sheet's width and the thing a tap on it
    // hits — not mdrev squeezed into a shrink-to-fit column (a rect alone would not say so).
    let readable = eval(
        &tab,
        "(function(){ var h = document.querySelector('.review-sheet .mdrev-host h1'); var r = h.getBoundingClientRect(); var hit = document.elementFromPoint(r.left + Math.min(20, r.width / 2), r.top + r.height / 2); return JSON.stringify({ width: Math.round(r.width), hit: !!hit && h.contains(hit), host: Math.round(document.querySelector('.review-sheet .mdrev-host').getBoundingClientRect().width), inner: innerWidth }); })()",
    );
    let readable: serde_json::Value =
        serde_json::from_str(readable.as_str().unwrap_or("{}")).unwrap();
    assert!(
        readable["hit"] == true
            && readable["host"].as_i64().unwrap_or(0) * 10
                >= readable["inner"].as_i64().unwrap_or(1) * 9
            && readable["width"].as_i64().unwrap_or(0) > 120,
        "the document fills the sheet and is the thing a tap hits: {readable}"
    );
    let covers = eval(
        &tab,
        "(function(){ var s = document.querySelector('.review-sheet'); var r = s.getBoundingClientRect(); var c = s.querySelector('[data-review-close]'); var cr = c.getBoundingClientRect(); var hit = document.elementFromPoint(cr.left + cr.width / 2, cr.top + cr.height / 2); return r.top === 0 && r.left === 0 && Math.round(r.width) === innerWidth && Math.round(r.height) === innerHeight && cr.width >= 44 && cr.height >= 44 && c.contains(hit); })()",
    );
    assert_eq!(
        covers, true,
        "the sheet covers the app, and its close control is a 44px target on top"
    );
    eval(
        &tab,
        "document.querySelector('.review-sheet [data-review-close]').click(); 'ok'",
    );
    until(
        &tab,
        "!document.querySelector('.review-sheet') && !!document.querySelector('#previewBody .mdrev-host h1')",
        "the sheet gone and the pane as it was",
        Duration::from_secs(10),
        PANE,
    );
    assert_eq!(
        offset(),
        before,
        "the transcript is where the reader left it"
    );
}

// ── #s13: every text file through mdrev, as source code when it is not Markdown ──────────────

/// A 1×1 transparent PNG.
const PNG_1X1: [u8; 67] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Held code: a Python file the session attached WITH its text, so the pane holds it for mdrev.
const HELPER: &str = "# a helper\ndef add(x, y):\n    \"\"\"Sum.\"\"\"\n    return x + y\n";

/// The `fixture` checkout plus `src/app.ts` committed twice (a history), and beside it a binary, an
/// image and an HTML page; the session READ each of those four (stamped paths in tool heads) and
/// carries `helper.py` as an attachment with its text.
fn code_fixture(name: &str) -> (PathBuf, Stores, PathBuf) {
    let (base, stores, repo) = fixture(name);
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/app.ts"), "export const a = 1;\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "app one"]);
    std::fs::write(
        repo.join("src/app.ts"),
        "// the app\nexport const a = 2;\nexport function add(x: number, y: number): number {\n  return x + y;\n}\n",
    )
    .unwrap();
    git(&["commit", "-qam", "app two"]);
    std::fs::write(repo.join("docs/blob.bin"), [0xff, 0xfe, 0x00, 0x01, 0x02]).unwrap();
    std::fs::write(repo.join("docs/pic.png"), PNG_1X1).unwrap();
    std::fs::write(
        repo.join("docs/page.html"),
        "<!doctype html><title>Page</title><h1>A page</h1>\n",
    )
    .unwrap();
    let path = |rel: &str| repo.join(rel).display().to_string();
    let helper = path("scratch/helper.py");
    let attachment = format!(
        "{{\"type\":\"attachment\",\"timestamp\":\"{ts}\",\"attachment\":{{\"type\":\"file\",\"filename\":\"{helper}\",\"displayPath\":\"helper.py\",\"content\":{{\"type\":\"text\",\"file\":{{\"filePath\":\"{helper}\",\"content\":{text},\"numLines\":4,\"startLine\":1,\"totalLines\":4}}}}}}}}\n",
        ts = at("00:14"),
        text = serde_json::to_string(HELPER).unwrap(),
    );
    let mut jsonl = user_at("read the code and the files beside it", &at("00:10"));
    for (i, rel) in [
        "src/app.ts",
        "docs/blob.bin",
        "docs/pic.png",
        "docs/page.html",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("c{i}");
        jsonl += &read_tool_at(&id, &path(rel), &at(&format!("00:1{i}")));
        jsonl += &tool_result_at(&id, &at(&format!("00:1{i}")));
    }
    jsonl += &attachment;
    let jsonl = jsonl.replace("\"cwd\":\"/r\"", &format!("\"cwd\":\"{}\"", repo.display()));
    stores.claude_session(SID, &jsonl);
    (base, stores, repo)
}

fn open_path(tab: &headless_chrome::Tab, abs: &str) {
    let found = eval(
        tab,
        &format!("(function(){{ var e = document.querySelector('[data-reference-path={abs:?}]'); if (!e) return false; e.click(); return true; }})()"),
    );
    assert_eq!(found.as_bool(), Some(true), "a stamped reference to {abs}");
}

/// What mdrev drew in the pane: its source view (rows, numbers, highlighted spans) or a document.
const SOURCE: &str = "(function(){ var h = document.querySelector('#previewBody .mdrev-host'); if (!h) return null; var v = h.querySelector('.source-view'); return JSON.stringify({ source: !!v, doc: !!h.querySelector('article.doc'), rows: v ? v.querySelectorAll('.src-line').length : 0, first: v && v.querySelector('.src-num') ? v.querySelector('.src-num').getAttribute('data-n') : null, copied: v && v.querySelector('.src-num') ? v.querySelector('.src-num').textContent : null, coloured: v ? v.querySelectorAll('span[style*=\"--shiki\"], span[style*=\"color\"]').length : 0, text: v ? v.innerText : '', root: (document.querySelector('.mdrev-pane') || {dataset: {}}).dataset.root || null }); })()";

fn source(tab: &headless_chrome::Tab) -> serde_json::Value {
    eval(tab, SOURCE)
        .as_str()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// #s13: a source file on disk opens in mdrev 1.1.21's source view — highlighted, its lines
/// numbered, never rendered as a document — over the file's own checkout, and with nothing of notes
/// or shared review, which mdrev does not do on code.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn mdrev_shows_a_local_code_file_as_highlighted_numbered_source() {
    let _serial = serial();
    drop(mdrev_cli()); // the file's history runs the pinned CLI under node
    let (base, stores, repo) = code_fixture("mdrev-code-local");
    let m = Monitor::spawn(Kind::V2, 3070, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    open_shell(&m, &tab);
    open_path(&tab, &repo.join("src/app.ts").display().to_string());
    until(
        &tab,
        "document.querySelectorAll('#previewBody .mdrev-host .source-view .src-line').length > 0",
        "mdrev to show src/app.ts as source",
        Duration::from_secs(30),
        PANE,
    );
    // The colours come after the rows: mdrev fetches the language's grammar on demand, and on a
    // slower machine (CI's Linux runner) the first rows are drawn plain until it lands.
    until(
        &tab,
        "document.querySelectorAll('#previewBody .mdrev-host .source-view span[style*=\"--shiki\"], #previewBody .mdrev-host .source-view span[style*=\"color\"]').length > 0",
        "mdrev to highlight src/app.ts",
        Duration::from_secs(30),
        PANE,
    );
    let seen = source(&tab);
    assert_eq!(seen["source"], true, "the source view: {seen}");
    assert_eq!(seen["doc"], false, "never rendered as a document: {seen}");
    assert_eq!(seen["first"], "1", "its lines are numbered: {seen}");
    assert_eq!(
        seen["copied"], "",
        "and the number is drawn, never part of the copied text: {seen}"
    );
    assert!(
        seen["coloured"].as_i64().unwrap_or(0) > 0,
        "highlighted by its language: {seen}"
    );
    assert!(
        seen["text"]
            .as_str()
            .unwrap_or("")
            .contains("export function add"),
        "the file as it stands: {seen}"
    );
    assert_eq!(
        seen["root"].as_str(),
        Some(repo.display().to_string().as_str()),
        "the whole viewer, over the file's checkout: {seen}"
    );
    assert_ne!(
        eval(&tab, NOTES_CONTROL).as_str(),
        Some("shown"),
        "no notes on code"
    );
    assert_eq!(
        eval(&tab, NOTE_INVITE).as_i64(),
        Some(0),
        "and no invitation to file one"
    );
    assert_eq!(
        eval(&tab, "(function(){ var b = document.querySelector('.preview-review'); return !b || b.hidden; })()")
            .as_bool(),
        Some(true),
        "no shared review on code: it is threads on a document's notes"
    );
    // #s26 (mdrev 1.1.23): the display menu reaches the source view — its text-size step makes the
    // code larger in OUR pane, where no host rule stands in its way. Through mdrev's own control,
    // as a reader would: its Aa, then "larger".
    let size = "(function(){ var v = document.querySelector('#previewBody .mdrev-host .source-view'); return v ? getComputedStyle(v).fontSize : 'none'; })()";
    let before = eval(&tab, size).as_str().unwrap_or("").to_string();
    let pressed = eval(
        &tab,
        "(function(){ var aa = [...document.querySelectorAll('#previewBody .mdrev-host button')].find(function (b) { return b.textContent === 'Aa'; }); if (!aa) return 'no Aa'; aa.click(); return 'opened'; })()",
    );
    assert_eq!(
        pressed.as_str(),
        Some("opened"),
        "mdrev's display control is on the pane's toolbar"
    );
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host .display-menu .size-large')",
        "mdrev's display menu",
        Duration::from_secs(10),
        PANE,
    );
    eval(
        &tab,
        "(function(){ document.querySelector('#previewBody .mdrev-host .display-menu .size-large').closest('button').click(); return 'ok'; })()",
    );
    until(
        &tab,
        &format!("{size} !== {before:?}"),
        "the source view's text to grow",
        Duration::from_secs(10),
        PANE,
    );
    let after = eval(&tab, size).as_str().unwrap_or("").to_string();
    let px = |s: &str| s.trim_end_matches("px").parse::<f64>().unwrap_or(0.0);
    assert!(
        px(&after) > px(&before),
        "the larger step makes the code larger in the pane: {before} -> {after}"
    );
}

/// #s13: what the pane does NOT hand mdrev stays exactly as it was — and held code goes to mdrev as
/// source. Text the transcript carries (`helper.py`) opens in mdrev's source view as a reader; a
/// binary still offers its download, an image is still drawn, and an HTML page still renders as a
/// page (the owner, 2026-10-06: "html pages render as pages, no need to send to mdrev").
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_pane_hands_mdrev_held_code_and_keeps_its_own_view_of_the_rest() {
    let _serial = serial();
    let (base, stores, repo) = code_fixture("mdrev-code-rest");
    let m = Monitor::spawn(Kind::V2, 3071, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    open_shell(&m, &tab);
    open_notes(&tab);
    until(
        &tab,
        "document.querySelectorAll('#previewBody .mdrev-host .source-view .src-line').length > 0",
        "mdrev to show the held helper.py as source",
        Duration::from_secs(30),
        PANE,
    );
    let seen = source(&tab);
    assert_eq!(
        (&seen["doc"], &seen["root"]),
        (&serde_json::json!(false), &serde_json::json!("held")),
        "held text, by its real name, is code: {seen}"
    );
    assert!(
        seen["text"].as_str().unwrap_or("").contains("def add"),
        "{seen}"
    );

    let open = |rel: &str, ready: &str, what: &str| {
        open_path(&tab, &repo.join(rel).display().to_string());
        until(&tab, ready, what, Duration::from_secs(20), PANE);
        assert_eq!(
            eval(
                &tab,
                "document.querySelectorAll('#previewBody .mdrev-host').length"
            )
            .as_i64(),
            Some(0),
            "{rel} is not handed to mdrev"
        );
    };
    open(
        "docs/blob.bin",
        "!!document.querySelector('#previewBody .preview-download')",
        "the binary's download, as before",
    );
    open(
        "docs/pic.png",
        "!!document.querySelector('#previewBody img.artifact-image')",
        "the image, drawn by the pane",
    );
    open(
        "docs/page.html",
        "!!document.querySelector('#previewBody iframe.artifact-html-frame')",
        "the HTML page, rendered as a page",
    );
}

/// Whether the pane head's print control is offered, and where a tap at its centre lands — the
/// control itself, or what covers it. `hidden` when it is not drawn.
const PRINT_CONTROL: &str = "(function(){ var b = document.querySelector('#previewHead .preview-print, .preview-head .preview-print'); if (!b || b.hidden || !b.offsetWidth) return 'hidden'; var r = b.getBoundingClientRect(); if (r.right > innerWidth || r.left < 0) return 'off-screen'; var hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return hit && b.contains(hit) ? 'shown' : 'covered by ' + (hit ? hit.className : 'nothing'); })()";

/// #s30, the owner: "for markdown, in mdrev, I actually think print still makes sense, though we
/// have to make sure we can fit the icon on the toolbar". mdrev's own toolbar prints — on a phone
/// from its Aa menu since 1.1.24 (#s31) — and a held reader has no toolbar at all. There the pane
/// head offers mdrev's own print (`mounted.print()`) for a Markdown document — on screen and
/// hit-tested at a phone's width, beside the head's other controls — and nothing for a code file.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_pane_prints_a_markdown_document_where_mdrev_offers_no_print() {
    let _serial = serial();
    drop(mdrev_cli());
    // The guide (a file) and the carried notes (held), plus a code file read after them.
    let (base, stores, repo) = fixture("mdrev-print");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/app.ts"), "export const a = 1;\n").unwrap();
    let session = stores.root.join("claude/-r").join(format!("{SID}.jsonl"));
    let mut jsonl = std::fs::read_to_string(&session).unwrap();
    let code = repo.join("src/app.ts").display().to_string();
    jsonl += &read_tool_at("c1", &code, &at("00:05"));
    jsonl += &tool_result_at("c1", &at("00:05"));
    stores.claude_session(SID, &jsonl);
    let m = Monitor::spawn(Kind::V2, 3077, &base, Some(&stores), true);
    let stub_print =
        "window.__printed = 0; window.print = function () { window.__printed++; }; 'ok'";

    // A phone: the guide (a file, mdrev's whole viewer in its phone layout).
    let (_phone_browser, phone) = chrome_tab();
    harness::phone(&phone, 390, 844);
    open_shell(&m, &phone);
    eval(&phone, stub_print);
    open_path(&phone, &repo.join("docs/guide.md").display().to_string());
    until(
        &phone,
        "!!document.querySelector('#previewBody .mdrev-host article.doc')",
        "the guide in mdrev on a phone",
        Duration::from_secs(30),
        PANE,
    );
    // #s31 (mdrev 1.1.24): a file prints from mdrev's own Aa menu on a phone, so the pane adds no
    // second print control there.
    let opened = eval(
        &phone,
        "(function(){ var aa = [...document.querySelectorAll('#previewBody .mdrev-host button')].find(function (b) { return b.textContent === 'Aa'; }); if (!aa) return 'no Aa'; aa.click(); return 'opened'; })()",
    );
    assert_eq!(
        opened.as_str(),
        Some("opened"),
        "mdrev's display menu on a phone"
    );
    until(
        &phone,
        "(function(){ var b = document.querySelector('#previewBody .mdrev-host .display-print-btn'); return !!b && !!b.offsetWidth; })()",
        "mdrev's own print in its Aa menu on a phone",
        Duration::from_secs(10),
        PANE,
    );
    eval(
        &phone,
        "document.querySelector('#previewBody .mdrev-host .display-print-btn').click(); 'ok'",
    );
    until(
        &phone,
        "window.__printed === 1",
        "mdrev's print to run from its menu",
        Duration::from_secs(10),
        "String(window.__printed)",
    );
    assert_eq!(
        eval(&phone, PRINT_CONTROL).as_str(),
        Some("hidden"),
        "no second print control in the pane head"
    );
    // A held reader on a phone has no mdrev toolbar: the pane's own print, on screen and on top at
    // 390px, beside the head's other controls.
    eval(
        &phone,
        "window.__printed = 0; document.querySelector('[data-attachment-action=\"preview\"]').click(); 'ok'",
    );
    until(
        &phone,
        "(function(){ var h = document.querySelector('#previewBody .mdrev-host'); return !!h && h.dataset.root === 'held'; })()",
        "the carried notes, held, on a phone",
        Duration::from_secs(30),
        PANE,
    );
    until(
        &phone,
        &format!("{PRINT_CONTROL} === 'shown'"),
        "the pane's print control, on screen and on top, at 390px",
        Duration::from_secs(10),
        &format!("{PRINT_CONTROL} + ' | ' + {PANE}"),
    );
    eval(
        &phone,
        "document.querySelector('.preview-print').click(); 'ok'",
    );
    until(
        &phone,
        "window.__printed === 1",
        "mdrev's print to run from the pane",
        Duration::from_secs(10),
        "String(window.__printed)",
    );
    // A code file offers no print.
    open_path(&phone, &repo.join("src/app.ts").display().to_string());
    until(
        &phone,
        "!!document.querySelector('#previewBody .mdrev-host .source-view')",
        "the code file as source",
        Duration::from_secs(30),
        PANE,
    );
    assert_eq!(
        eval(&phone, PRINT_CONTROL).as_str(),
        Some("hidden"),
        "no print for code"
    );

    // A desktop: the guide has mdrev's own print on its toolbar, so the pane adds none; the notes
    // the transcript carried (gone from disk, so held) are a reader with no toolbar, and get it.
    let (_desk_browser, desk) = chrome_tab();
    open_shell(&m, &desk);
    eval(&desk, stub_print);
    open_path(&desk, &repo.join("docs/guide.md").display().to_string());
    until(
        &desk,
        "!!document.querySelector('#previewBody .mdrev-host article.doc')",
        "the guide in mdrev on a desktop",
        Duration::from_secs(30),
        PANE,
    );
    assert_eq!(
        eval(&desk, PRINT_CONTROL).as_str(),
        Some("hidden"),
        "mdrev's own toolbar prints a file on a desktop"
    );
    eval(
        &desk,
        "document.querySelector('[data-attachment-action=\"preview\"]').click(); 'ok'",
    );
    until(
        &desk,
        "(function(){ var h = document.querySelector('#previewBody .mdrev-host'); return !!h && h.dataset.root === 'held' && !!h.querySelector('article.doc'); })()",
        "the carried notes, held, in mdrev",
        Duration::from_secs(30),
        PANE,
    );
    until(
        &desk,
        &format!("{PRINT_CONTROL} === 'shown'"),
        "the pane's print control for a held reader",
        Duration::from_secs(10),
        PRINT_CONTROL,
    );
    eval(
        &desk,
        "document.querySelector('.preview-print').click(); 'ok'",
    );
    until(
        &desk,
        "window.__printed === 1",
        "mdrev's print to run for the held reader",
        Duration::from_secs(10),
        "String(window.__printed)",
    );
}

/// #s54: mdrev's draft 8 in the detached tab, end to end with the pinned mdrev — the shape of
/// mdrev's own `scripts/check-shared-review.mjs`. The project tells people through one channel, a
/// fake `mdrev-notify-fake` on the MONITOR's PATH that keeps what it is asked to send (agent-monitor
/// runs its vendored mdrev-cli with its own environment, so the plugin's commands are found on PATH).
/// A second reviewer, Bob, works from his own checkout: he opens a thread, pushes it and subscribes
/// to the project. The case's reader ("Alice", paired as `t@example.invalid`) then, in the tab:
/// - her first fetch writes her starting read mark to the store;
/// - Bob's reply arrives new, is read once its card is seen, and the page leaving sends that mark;
/// - she subscribes to the document, typing back the code the channel was asked to send her;
/// - she opens a thread mentioning Carol and pushes it, and the push tells Bob (subscribed) and
///   Carol (mentioned), as her.
///
/// The pane's prefix still answers 404 to every draft 8 route.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and node"]
fn the_detached_tab_reads_subscribes_and_notifies_through_draft_8() {
    let _serial = serial();
    let (base, stores, repo, state) = shared_review_world("mdrev-draft8");
    let r = repo.display().to_string();
    let git = |dir: &Path, args: &[&str]| -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // The project's channel: a fake that keeps what it is asked, and knows three people.
    let fake = base.join("fake-bin");
    std::fs::create_dir_all(&fake).unwrap();
    let calls = fake.join("calls.jsonl");
    std::fs::write(
        fake.join("people.json"),
        r#"{"t@example.invalid":"Alice A","bob@example.com":"Bob B","carol@example.com":"Carol C"}"#,
    )
    .unwrap();
    let script = format!(
        "#!/usr/bin/env node\nconst fs = require('fs');\nlet input = '';\nprocess.stdin.on('data', d => (input += d)).on('end', () => {{\n  const req = JSON.parse(input);\n  fs.appendFileSync({calls:?}, JSON.stringify(req) + '\\n');\n  const known = JSON.parse(fs.readFileSync({people:?}, 'utf8'));\n  if (req.lookup) process.stdout.write(JSON.stringify({{people: req.lookup.filter(e => known[e]).map(e => ({{email: e, name: known[e]}})), unknown: req.lookup.filter(e => !known[e])}}));\n}});\n",
        calls = calls.display().to_string(),
        people = fake.join("people.json").display().to_string(),
    );
    let notifier = fake.join("mdrev-notify-fake");
    std::fs::write(&notifier, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&notifier, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        fake.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // The project names its channel (committed, so Bob's clone carries it).
    let config = std::fs::read_to_string(repo.join(".mdrev.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    let mut config = config.as_object().unwrap().clone();
    config.insert("notify".into(), serde_json::json!({"channels": ["fake"]}));
    std::fs::write(
        repo.join(".mdrev.json"),
        serde_json::to_string(&config).unwrap(),
    )
    .unwrap();
    git(&repo, &["add", ".mdrev.json"]);
    git(&repo, &["commit", "-qm", "the project's channel"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    // What the channel was asked to send, as (to, why, from, text).
    let told = || -> Vec<(String, String, String, String)> {
        std::fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .flat_map(|c| {
                let from = c["from"]["email"].as_str().unwrap_or("").to_string();
                c["messages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |m| {
                        let s = |k: &str| m[k].as_str().unwrap_or("").to_string();
                        (s("to"), s("why"), from.clone(), s("text"))
                    })
            })
            .collect()
    };
    let code_for = |email: &str| -> String {
        let text = told()
            .into_iter()
            .rev()
            .find(|(to, why, _, _)| to == email && why == "confirm")
            .map(|t| t.3)
            .unwrap_or_default();
        text.split(|c: char| !c.is_ascii_digit())
            .find(|w| w.len() == 6)
            .unwrap_or_else(|| panic!("no code sent to {email}: {text}"))
            .to_string()
    };

    // Bob, on his own machine: a clone, his own viewer key, paired; a thread pushed; the project followed.
    let bob = base.join("bob");
    let bob_state = base.join("bob-state");
    std::fs::create_dir_all(&bob_state).unwrap();
    let bob_key = "b0b0000000000000000000000000000000000000000000000000000000000001";
    std::fs::write(bob_state.join("token"), bob_key).unwrap();
    git(&base, &["clone", "-q", &r, &bob.display().to_string()]);
    git(&bob, &["config", "user.email", "bob@example.com"]);
    git(&bob, &["config", "user.name", "Bob"]);
    let bob_cli = |args: &[&str], input: &str, viewer: bool| -> String {
        let mut c = mdrev_cli();
        c.args(args)
            .args(["--root", &bob.display().to_string()])
            .env("MDREV_STATE_DIR", &bob_state)
            .env("PATH", &path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if viewer {
            c.env("MDREV_VIEWER_KEY", bob_key);
        } else {
            c.env_remove("MDREV_VIEWER_KEY").env("CLAUDECODE", "1");
        }
        let mut child = c.spawn().expect("mdrev-cli runs");
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "mdrev-cli {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    bob_cli(
        &[
            "review",
            "pair",
            "--email",
            "bob@example.com",
            "--name",
            "Bob-on-desktop",
        ],
        "",
        false,
    );
    bob_cli(&["review", "pair", "--confirm", "--viewer"], "", true);
    let doc = std::fs::read_to_string(repo.join("docs/guide.md")).unwrap();
    let quote = "prose";
    let start = doc.find(quote).unwrap();
    let thread: serde_json::Value = serde_json::from_str(&bob_cli(
        &["notes", "add", "--path", "docs/guide.md", "--viewer"],
        &serde_json::json!({"body": "Why bold here?", "anchor": {"exact": quote, "start": start, "end": start + quote.len(), "space": "source", "side": "to", "trail": ["The guide"]}, "shared": true, "rev": sha}).to_string(),
        true,
    ))
    .unwrap();
    let thread = thread["id"].as_str().expect("the thread's id").to_string();
    bob_cli(&["review", "push", "--viewer"], "", true);
    let asked: serde_json::Value =
        serde_json::from_str(&bob_cli(&["review", "subscribe", "--viewer"], "", true)).unwrap();
    if asked.get("confirm").is_some() {
        bob_cli(
            &[
                "review",
                "confirm",
                "--code",
                &code_for("bob@example.com"),
                "--viewer",
            ],
            "",
            true,
        );
    }

    // Alice's monitor, its mdrev-cli finding the channel on the monitor's PATH.
    let store = base.join("store.git");
    let alice_read = |id: Option<&str>| -> bool {
        let log = Command::new("git")
            .args([
                "--git-dir",
                &store.display().to_string(),
                "log",
                "--author=t@example.invalid",
                "--format=%H",
                "--all",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&log.stdout).lines().any(|h| {
            let shown = Command::new("git")
                .args(["--git-dir", &store.display().to_string(), "show", h])
                .output()
                .unwrap();
            let shown = String::from_utf8_lossy(&shown.stdout);
            shown.contains("\"kind\": \"read\"")
                && id.is_none_or(|id| shown.contains(&format!("\"{id}\"")))
        })
    };
    let port = 2756;
    let m = Monitor::spawn_with(
        Kind::V2,
        port,
        &base,
        Some(&stores),
        true,
        &[
            ("MDREV_STATE_DIR", &state.display().to_string()),
            ("PATH", &path),
        ],
    );
    let (browser, tab) = chrome_tab();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#previewBody .mdrev-host h1')",
        "the guide in the pane",
        Duration::from_secs(30),
        PANE,
    );
    // The pane offers none of it.
    let pane = eval(
        &tab,
        "(async function(){ var d = document.querySelector('.mdrev-pane').dataset; var q = 'root=' + encodeURIComponent(d.root) + '&path=' + encodeURIComponent(d.path) + '&cap=' + encodeURIComponent(d.cap); var out = []; for (const r of ['review/subscribers', 'review/lookup']) out.push((await fetch(d.contract + '/' + r + '?' + q)).status); for (const r of ['review/subscribe', 'review/read', 'review/confirm', 'review/unsubscribe']) out.push((await fetch(d.contract + '/' + r + '?' + q, {method: 'POST', headers: {'content-type': 'application/json'}, body: '{\"doc\":true,\"ids\":[\"shr-1\"],\"code\":\"123456\"}'})).status); return out.join(','); })()",
    );
    assert_eq!(
        pane.as_str().unwrap_or(""),
        "404,404,404,404,404,404",
        "the pane offers no draft 8"
    );

    open_in_a_tab(&tab);
    let own = opened_tab(&browser);
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1')",
        "the guide in a tab of its own",
        Duration::from_secs(30),
        OWN,
    );
    // mdrev speaks the reader's language (navigator.languages): English, so the words below hold on
    // a machine whose Chrome is in another.
    let agent = eval(&own, "navigator.userAgent")
        .as_str()
        .unwrap_or("")
        .to_string();
    own.set_user_agent(&agent, Some("en-US,en"), None).unwrap();
    own.reload(false, None).unwrap();
    until(
        &own,
        "!!document.querySelector('#doc.mdrev-host h1') && navigator.language === 'en-US'",
        "the tab again, in English",
        Duration::from_secs(30),
        OWN,
    );
    let chip = format!(".ann-marker[data-id=\"{thread}\"], .ann-marker[data-ids~=\"{thread}\"]");
    until(
        &own,
        &format!("!!document.querySelector('{chip}')"),
        "Bob's thread on the page",
        Duration::from_secs(30),
        OWN,
    );
    // The first fetch writes her starting read mark: what was there is not new to her.
    let mut seen = false;
    for _ in 0..100 {
        if alice_read(None) {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        seen,
        "her first fetch put a starting read mark in the store"
    );

    // Bob answers; opened again, the reply is new; seen to its card's end, it is read.
    let replied: serde_json::Value = serde_json::from_str(&bob_cli(
        &[
            "notes",
            "reply",
            &thread,
            "--body",
            "Still bold?",
            "--viewer",
        ],
        "",
        true,
    ))
    .unwrap();
    let reply = replied["replies"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|r| r["id"].as_str())
        .expect("the reply's id")
        .to_string();
    bob_cli(&["review", "push", "--viewer"], "", true);
    own.reload(false, None).unwrap();
    until(&own, &format!("(document.querySelector('{chip}') || {{}}).querySelector && document.querySelector('{chip}').querySelector('.ann-new-count') && document.querySelector('{chip}').querySelector('.ann-new-count').textContent === '1'"), "Bob's reply, new to her", Duration::from_secs(30), OWN);
    eval(&own, &format!("(function(){{ var c = document.querySelector('{chip}'); c.scrollIntoView({{block: 'center'}}); c.click(); return 1; }})()"));
    let card = format!(".ann-card[data-id=\"{thread}\"]");
    until(&own, &format!("!!document.querySelector('{card}') && !document.querySelector('{card} .ann-badge-new') && !document.querySelector('{chip}').querySelector('.ann-new-count')"), "the reply read once its card is seen", Duration::from_secs(30), OWN);

    // She subscribes to this document from the bar; her email takes a code, which she types back.
    eval(
        &own,
        "(function(){ document.querySelector('.ann-review-follow').click(); return 1; })()",
    );
    until(
        &own,
        "!!document.querySelector('.ann-follow')",
        "the subscribe panel",
        Duration::from_secs(20),
        OWN,
    );
    let panel = eval(&own, "document.querySelector('.ann-follow').innerText")
        .as_str()
        .unwrap_or("")
        .to_string();
    assert!(
        panel.contains("bob@example.com"),
        "the panel says who is subscribed: {panel}"
    );
    eval(&own, "(function(){ document.querySelector('.ann-follow .ann-follow-row button.primary').click(); return 1; })()");
    until(
        &own,
        "!!document.querySelector('.ann-follow-code input')",
        "the code step",
        Duration::from_secs(20),
        "document.querySelector('.ann-follow').innerText",
    );
    let code = code_for("t@example.invalid");
    eval(
        &own,
        "(function(){ document.querySelector('.ann-follow-code input').focus(); return 1; })()",
    );
    own.call_method(headless_chrome::protocol::cdp::Input::InsertText { text: code })
        .unwrap();
    eval(&own, "(function(){ document.querySelector('.ann-follow-code button.primary').click(); return 1; })()");
    until(&own, "(document.querySelector('.ann-follow') || {}).innerText && document.querySelector('.ann-follow').innerText.includes('subscribed as t@example.invalid')", "subscribed to the document", Duration::from_secs(30), "document.querySelector('.ann-follow').innerText");
    eval(&own, "(function(){ [...document.querySelectorAll('.ann-follow button')].find(b => b.textContent === 'close').click(); return 1; })()");
    // …and the subscription is in the store: Bob, fetching, sees her subscribed.
    bob_cli(&["review", "fetch", "--viewer"], "", true);
    let subs = bob_cli(
        &["review", "subscribers", "--path", "docs/guide.md"],
        "",
        true,
    );
    assert!(
        subs.contains("t@example.invalid"),
        "her subscription reached the store: {subs}"
    );

    // A thread of her own mentioning Carol, then the push: it tells Bob and Carol, as her.
    eval(&own, "(function(){ var doc = document.querySelector('.doc'); var w = document.createTreeWalker(doc, NodeFilter.SHOW_TEXT); var n; while ((n = w.nextNode()) && !n.data.includes('the other guide')); var at = n.data.indexOf('the other guide'); var r = document.createRange(); r.setStart(n, at); r.setEnd(n, at + 'the other guide'.length); var s = getSelection(); s.removeAllRanges(); s.addRange(r); document.dispatchEvent(new Event('selectionchange')); return 1; })()");
    until(
        &own,
        "!!document.querySelector('.ann-invite')",
        "the invitation to note the selection",
        Duration::from_secs(20),
        OWN,
    );
    eval(
        &own,
        "(function(){ document.querySelector('.ann-invite').click(); return 1; })()",
    );
    until(
        &own,
        "!!document.querySelector('.ann-draft .ann-share input')",
        "the composer",
        Duration::from_secs(20),
        OWN,
    );
    eval(&own, "(function(){ document.querySelector('.ann-draft .ann-share input').click(); return 1; })()");
    eval(
        &own,
        "(function(){ document.querySelector('.ann-draft textarea').focus(); return 1; })()",
    );
    own.call_method(headless_chrome::protocol::cdp::Input::InsertText {
        text: "Which guide is this? @carol@example.com, can you check?".into(),
    })
    .unwrap();
    eval(
        &own,
        "(function(){ document.querySelector('.ann-draft button.primary').click(); return 1; })()",
    );
    until(
        &own,
        "!!document.querySelector('.ann-marker.ann-shared.ann-unpushed')",
        "her own thread, unpushed",
        Duration::from_secs(20),
        OWN,
    );
    let before = told().len();
    until(&own, "[...document.querySelectorAll('.ann-review-pill button')].some(function (b) { return /^push \\d/.test(b.textContent); })", "the push control", Duration::from_secs(20), "document.querySelector('.ann-review-pill') && document.querySelector('.ann-review-pill').innerText");
    eval(&own, "(function(){ document.querySelector('.ann-review-pill button.primary').click(); return 1; })()");
    until(
        &own,
        "!!document.querySelector('.ann-review-panel')",
        "the push list",
        Duration::from_secs(20),
        OWN,
    );
    eval(&own, "(function(){ document.querySelector('.ann-review-panel button.primary').click(); return 1; })()");
    until(&own, "(document.querySelector('.ann-review-panel') || {}).innerText && /pushed \\d/.test(document.querySelector('.ann-review-panel').innerText)", "the push to land", Duration::from_secs(30), "document.querySelector('.ann-review-panel').innerText");
    let sent: Vec<_> = told().into_iter().skip(before).collect();
    assert!(
        sent.iter().any(|(to, why, from, _)| to == "bob@example.com"
            && why == "subscribed"
            && from == "t@example.invalid"),
        "the push told Bob, who follows the project, as her: {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|(to, why, _, text)| to == "carol@example.com"
                && why == "mentioned"
                && text.contains("can you check?")),
        "and Carol, whom her thread mentions: {sent:?}"
    );
    assert!(
        !sent.iter().any(|(to, ..)| to == "t@example.invalid"),
        "never herself: {sent:?}"
    );
    let panel = eval(
        &own,
        "document.querySelector('.ann-review-panel').innerText",
    )
    .as_str()
    .unwrap_or("")
    .to_string();
    assert!(
        panel.contains("told"),
        "the page says who the push told: {panel}"
    );
    assert!(
        alice_read(Some(&reply)),
        "the push took her read mark of Bob's reply with it"
    );
}

/// #s57, the owner on 1.362.0 from an iPhone: "The aA menu was clipped sometimes, the print menu
/// print the page not the document" — in the pane and the review sheet alike.
///
/// The menu: mdrev anchors it to the Aa button's right edge except at its narrowest toolbar tiers,
/// so a wider tier with the controls row at the left (a long document name) ran it off the left
/// edge. The case puts the toolbar in such a tier and opens the menu: it must lie wholly on screen.
///
/// The print: mdrev isolates the document with a print stylesheet around `window.print()` and took
/// it away in a `finally` — right after, on iOS, whose print() returns before the page is captured.
/// The case makes print() return at once, as iOS does, and prints from the menu: the isolation must
/// still hold (the pane's own head hidden on the paper, the document shown) until `afterprint`.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn a_phone_prints_the_document_alone_and_keeps_mdrevs_menu_on_screen() {
    let _serial = serial();
    let (base, stores, repo) = fixture("mdrev-phone-print");
    // A long document name with no history, as the owner's (a memory file outside any checkout):
    // mdrev gives it its reader toolbar, whose controls row sits at the LEFT at its middle tiers.
    std::fs::write(
        repo.join("docs/guide.md"),
        "# The guide\n\nSee [decisions](decisions-become-person-tasks.md).\n",
    )
    .unwrap();
    for args in [
        &["add", "."][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "long",
        ][..],
    ] {
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(
        repo.join("docs/decisions-become-person-tasks.md"),
        "# Decisions\n\nWhen a deliverable ends with awaiting your decision, file it.\n",
    )
    .unwrap();
    let m = Monitor::spawn(Kind::V2, 2731, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 440, 956);
    // A phone, as mdrev tests for one (its toolbar's phone layout keys on these).
    tab.call_method(
        headless_chrome::protocol::cdp::Emulation::SetEmulatedMedia {
            media: None,
            features: Some(vec![
                headless_chrome::protocol::cdp::Emulation::MediaFeature {
                    name: "hover".into(),
                    value: "none".into(),
                },
                headless_chrome::protocol::cdp::Emulation::MediaFeature {
                    name: "pointer".into(),
                    value: "coarse".into(),
                },
            ]),
        },
    )
    .unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#preview .mdrev-host .topbar .display-wrap')",
        "mdrev's toolbar in the pane",
        Duration::from_secs(30),
        PANE,
    );
    eval(&tab, "(function(){ var a = [...document.querySelectorAll('#preview .mdrev-host a')].find(function (a) { return /decisions/.test(a.getAttribute('href') || ''); }); a.click(); return 1; })()");
    until(&tab, "/Decisions/.test((document.querySelector('#preview .mdrev-host h1') || {}).textContent || '')", "the long-named document", Duration::from_secs(20), PANE);

    // The owner's document was a memory file outside any checkout: mdrev gave it its READER toolbar,
    // four controls in a row at the LEFT, and hung the menu from the Aa button, so it ran off the
    // left edge. A window here gets the full toolbar with Aa at the right; the case moves Aa to the
    // start of its row, where the owner's was, and asks where the menu hangs from.
    eval(&tab, "(function(){ var bar = document.querySelector('#preview .mdrev-host .topbar'); bar.className = bar.className.replace(/\\bset-\\w+\\b/, 'set-read').replace(/\\btier-\\d\\b/, 'tier-2'); var w = bar.querySelector('.display-wrap'); w.parentNode.prepend(w); return 1; })()");
    let aa = eval(&tab, "Math.round(document.querySelector('#preview .mdrev-host .display-wrap button').getBoundingClientRect().right)");
    assert!(
        aa.as_i64().unwrap_or(999) < 260,
        "the Aa control sits at the left, as on the owner's iPhone: {aa}"
    );
    eval(&tab, "(function(){ document.querySelector('#preview .mdrev-host .display-wrap button').click(); return 1; })()");
    until(
        &tab,
        "!!document.querySelector('#preview .mdrev-host .display-menu')",
        "the Aa menu",
        Duration::from_secs(10),
        PANE,
    );
    let menu = eval(&tab, "(function(){ var r = document.querySelector('#preview .mdrev-host .display-menu').getBoundingClientRect(); return JSON.stringify([Math.round(r.left), Math.round(r.right), innerWidth]); })()");
    let menu: Vec<i64> = serde_json::from_str(menu.as_str().unwrap()).unwrap();
    assert!(
        menu[0] >= 0 && menu[1] <= menu[2],
        "the Aa menu lies wholly on screen: {menu:?}"
    );

    // print() returns at once, as on iOS.
    eval(&tab, "(function(){ window.__printed = 0; window.print = function () { window.__printed++; }; return 1; })()");
    until(
        &tab,
        "!!document.querySelector('#preview .mdrev-host .display-print-btn')",
        "the menu's print",
        Duration::from_secs(10),
        PANE,
    );
    eval(&tab, "(function(){ document.querySelector('#preview .mdrev-host .display-print-btn').click(); return 1; })()");
    until(
        &tab,
        "window.__printed === 1",
        "the print asked for",
        Duration::from_secs(10),
        "String(window.__printed)",
    );
    let held = eval(&tab, "(function(){ var host = document.querySelector('.mdrev-print-target'); return JSON.stringify({ sheet: !!document.getElementById('mdrev-print-isolation'), target: !!host && host.classList.contains('mdrev-pane'), out: !!host && host.parentNode === document.body, title: document.title }); })()");
    let held: serde_json::Value = serde_json::from_str(held.as_str().unwrap()).unwrap();
    assert_eq!(
        held["sheet"], true,
        "the isolation outlives print() returning: {held}"
    );
    assert_eq!(
        held["target"], true,
        "the document is what it isolates: {held}"
    );
    assert_eq!(
        held["out"], true,
        "stepped out of the app into the body for the print: {held}"
    );
    assert_eq!(
        held["title"], "decisions-become-person-tasks",
        "under the document's own name: {held}"
    );
    // On the paper: the document, and none of the monitor around it.
    tab.call_method(
        headless_chrome::protocol::cdp::Emulation::SetEmulatedMedia {
            media: Some("print".into()),
            features: None,
        },
    )
    .unwrap();
    let paper = eval(&tab, "(function(){ var v = function (s) { var e = document.querySelector(s); return e ? getComputedStyle(e).visibility : 'none'; }; return JSON.stringify({ head: v('#preview .preview-head'), title: v('#sessionTitle'), doc: v('.mdrev-print-target h1'), app: getComputedStyle(document.getElementById('app')).display, body: getComputedStyle(document.body).backgroundColor }); })()");
    tab.call_method(
        headless_chrome::protocol::cdp::Emulation::SetEmulatedMedia {
            media: Some(String::new()),
            features: None,
        },
    )
    .unwrap();
    let paper: serde_json::Value = serde_json::from_str(paper.as_str().unwrap()).unwrap();
    assert_eq!(
        paper["doc"], "visible",
        "the document is on the paper: {paper}"
    );
    assert_eq!(
        paper["head"], "hidden",
        "the pane's own head is not: {paper}"
    );
    assert_eq!(
        paper["title"], "hidden",
        "nor the monitor's top bar: {paper}"
    );
    assert_eq!(
        paper["app"], "none",
        "the app takes no room on the paper, so no second page: {paper}"
    );
    assert_eq!(
        paper["body"], "rgb(255, 255, 255)",
        "and the paper is white, not the monitor's grey (iOS prints backgrounds): {paper}"
    );
    // …and the print over, the page is the monitor again.
    eval(
        &tab,
        "(function(){ dispatchEvent(new Event('afterprint')); return 1; })()",
    );
    let after = eval(&tab, "(function(){ var host = document.querySelector('#preview .mdrev-host'); return JSON.stringify([!!document.getElementById('mdrev-print-isolation'), !host || host.classList.contains('mdrev-print-target') || document.documentElement.classList.contains('printing-document'), document.title]); })()");
    let after: serde_json::Value = serde_json::from_str(after.as_str().unwrap()).unwrap();
    assert_eq!(
        after[0], false,
        "the isolation goes after the print: {after}"
    );
    assert_eq!(
        after[1], false,
        "the document back in the pane, unmarked: {after}"
    );
    assert_ne!(
        after[2], "decisions-become-person-tasks",
        "the page's own title back: {after}"
    );
}

/// #s57 probe: the PDF the pane's print makes, written to the scratch for a look.
#[test]
#[ignore = "probe"]
fn probe_pane_print_pdf() {
    let _serial = serial();
    let (base, stores, repo) = fixture("mdrev-print-pdf");
    let m = Monitor::spawn(Kind::V2, 2739, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 440, 956);
    tab.call_method(
        headless_chrome::protocol::cdp::Emulation::SetEmulatedMedia {
            media: None,
            features: Some(vec![
                headless_chrome::protocol::cdp::Emulation::MediaFeature {
                    name: "hover".into(),
                    value: "none".into(),
                },
                headless_chrome::protocol::cdp::Emulation::MediaFeature {
                    name: "pointer".into(),
                    value: "coarse".into(),
                },
            ]),
        },
    )
    .unwrap();
    open_shell(&m, &tab);
    open_guide(&tab, &repo);
    until(
        &tab,
        "!!document.querySelector('#preview .mdrev-host h1')",
        "the guide",
        Duration::from_secs(30),
        PANE,
    );
    eval(
        &tab,
        "(function(){ window.print = function () {}; return 1; })()",
    );
    eval(&tab, "(function(){ document.querySelector('#preview .mdrev-host .display-wrap button').click(); return 1; })()");
    until(
        &tab,
        "!!document.querySelector('#preview .mdrev-host .display-print-btn')",
        "print",
        Duration::from_secs(10),
        PANE,
    );
    eval(&tab, "(function(){ document.querySelector('#preview .mdrev-host .display-print-btn').click(); return 1; })()");
    std::thread::sleep(Duration::from_millis(500));
    tab.call_method(
        headless_chrome::protocol::cdp::Emulation::SetEmulatedMedia {
            media: Some("print".into()),
            features: None,
        },
    )
    .unwrap();
    println!("BG: {}", eval(&tab, "(function(){ var out = []; [document.documentElement, document.body, document.getElementById('app'), document.querySelector('#app>.workspace'), document.getElementById('preview'), document.querySelector('#preview .preview-body'), document.querySelector('#preview .mdrev-host')].forEach(function (e) { if (!e) return; var c = getComputedStyle(e); out.push((e.id || e.className || e.tagName).toString().slice(0, 30) + ' bg=' + c.backgroundColor + ' vis=' + c.visibility + ' pos=' + c.position + ' h=' + Math.round(e.getBoundingClientRect().height)); }); return out.join(' | '); })()"));
    tab.call_method(
        headless_chrome::protocol::cdp::Emulation::SetEmulatedMedia {
            media: Some(String::new()),
            features: None,
        },
    )
    .unwrap();
    let pdf = tab
        .print_to_pdf(Some(headless_chrome::types::PrintToPdfOptions {
            print_background: Some(true),
            ..Default::default()
        }))
        .unwrap();
    let out = std::path::PathBuf::from(
        std::env::var("PROBE_OUT").unwrap_or_else(|_| "/tmp/pane-print.pdf".into()),
    );
    std::fs::write(&out, pdf).unwrap();
    println!("PDF: {}", out.display());
}
