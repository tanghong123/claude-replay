//! mdrev's embedded viewer in the app shell's preview pane (#270), in a real Chrome against the
//! REAL released guest — `design/mdrev-in-the-preview-pane.md`. A stand-in bundle would prove
//! nothing about the thing the owner asked for, so these cases find the installed release the way
//! the monitor does, and panic naming the fix when there is none (`harness::mdrev_release`).
//!
//! Ports 2821–2825. The classic page has no preview pane, so there is no second surface here: the
//! owner named the right-most pane, which only the app shell has.
//!
//! `mdrev_guest_` marks the cases that need the real guest (mdrev >= 1.1.6). CI skips that prefix
//! by name, in the workflow, because no such release is public yet — the public tap stops at
//! 0.16.45 — and says so there; `without_mdrev_markdown_stays_text` needs none and runs everywhere.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod harness;
use harness::{
    at, base, chrome, eval, mdrev_cli, mdrev_release, read_tool_at, serial, tool_result_at, until,
    user_at, Kind, Monitor, Stores,
};

const SID: &str = "5e5510a1-0000-4000-8000-000000000270";
const NOTES: &str = "# Field notes\n\nA paragraph with **bold** text.\n\n- one\n- two\n";

/// A checkout with `docs/guide.md` committed twice, and a session — its cwd that checkout, so
/// containment explains the file — which READ the guide (a stamped path in a tool head) and carries
/// `NOTES.md` as an attachment WITH its text: the transcript's own Markdown.
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
        "# The guide\n\nSome **bold** prose.\n",
    )
    .unwrap();
    git(&["commit", "-qam", "second"]);

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

/// The owner: "For embedded contents, only show a cleanly rendered viewer (as a reader)". The
/// attachment's text is rendered by mdrev — a real heading, not `# Field notes` in a `<pre>` — held
/// by the monitor for the contract, and with no toolbar at all.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and an installed mdrev release"]
fn mdrev_guest_renders_transcript_markdown_as_a_clean_reader() {
    let _serial = serial();
    let _ = mdrev_release();
    let (base, stores, _repo) = fixture("mdrev-reader");
    let m = Monitor::spawn(Kind::V2, 2821, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
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
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and an installed mdrev release"]
fn mdrev_guest_renders_a_local_markdown_file_with_its_toolbar() {
    let _serial = serial();
    let _ = mdrev_release();
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
}

/// mdrev acts on its own keys while the reader is engaged with it; the shell's document-wide keymap
/// must not answer the same press. `\` is the hardest case — a `when: "any"` binding (the sidebar),
/// which no context value can switch off. Red without `inGuest` in shared/keymap.js.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and an installed mdrev release"]
fn mdrev_guest_owns_a_key_pressed_while_the_reader_is_engaged_with_it() {
    let _serial = serial();
    let _ = mdrev_release();
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

/// No mdrev, no regression: with the release made absent the attachment's Markdown is in the
/// pane's `<pre>`, exactly as before #270.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn without_mdrev_markdown_stays_text() {
    let _serial = serial();
    let (base, stores, _repo) = fixture("mdrev-absent");
    let m = Monitor::spawn_with(
        Kind::V2,
        2824,
        &base,
        Some(&stores),
        true,
        &[("AGENT_MONITOR_MDREV", "/nonexistent-mdrev-release")],
    );
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab);
    assert_eq!(
        eval(&tab, "document.body.dataset.mdrev").as_str(),
        Some(""),
        "the page knows there is none"
    );
    open_notes(&tab);
    until(
        &tab,
        "!!document.querySelector('#previewBody pre.artifact-text')",
        "the text view",
        Duration::from_secs(20),
        PANE,
    );
    let text = eval(
        &tab,
        "document.querySelector('#previewBody pre.artifact-text').textContent",
    )
    .as_str()
    .unwrap_or("")
    .to_string();
    assert!(
        text.starts_with("# Field notes"),
        "the Markdown source, as before: {text:?}"
    );
    assert_eq!(
        eval(&tab, "document.querySelectorAll('.mdrev-host').length").as_i64(),
        Some(0)
    );
}

/// mdrev's own definition of a host: `mdrev-cli conform` against the live monitor — every route
/// and shape, the refusals, and the round trip: a note filed through our routes, found in the
/// sidecar with the code `mdrev --notes` uses, closed and deleted. The guide: when your host passes
/// it, you are done. The capability is read off the page, where a reader gets one too.
#[test]
#[ignore = "needs a local Chrome, a built agent-monitor-v2 and an installed mdrev release"]
fn mdrev_guest_contract_passes_mdrev_cli_conform() {
    let _serial = serial();
    let tree = mdrev_release();
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
    let out = Command::new(mdrev_cli(&tree))
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
