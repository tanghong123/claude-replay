//! The app shell's file affordances offer BOTH halves (#272): showing the file in the page — or
//! downloading it, for bytes the page does not show — AND revealing it in the file manager. The
//! owner: "I may still need it when running it locally, so I am thinking of offering both for now"
//! (reveal is interim; a web file browser will replace it). The preview pane is where every "show
//! me the file" click lands, so one control in its head covers each view it draws; a prompt card
//! carries the reveal beside its own action, as the process-surface card already did.
//!
//! No case ever reveals anything: the page's `fetch` is wrapped so a `/__reveal` request is recorded
//! and answered, never sent — `open -R` must not run on the machine the suite runs on.
//!
//! Ports 2811–2814. The classic page draws no preview pane; its file view already pairs the two
//! (export.js `openArtifact` and its "Reveal in file manager" action), which is the reference here.

use std::path::PathBuf;
use std::time::Duration;

mod harness;
use harness::{
    at, base, chrome, edited_file_at, eval, read_tool_at, serial, tool_result_at, until, user_at,
    Kind, Monitor, Stores,
};

const SID: &str = "5e5510a1-0000-4000-8000-000000000272";

/// A 1×1 PNG: enough for the pane to draw an image.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// A checkout holding a file of each kind the pane draws — an image, a page, text, and bytes that
/// are none of those — each READ by the session (a stamped path in a tool head), and a prompt
/// carrying a draft the reader had open in their editor (a prompt card).
fn fixture(name: &str) -> (PathBuf, Stores, PathBuf) {
    let base = base(name);
    let stores = Stores::new(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("shot.png"), PNG).unwrap();
    std::fs::write(repo.join("page.html"), "<h1>A page</h1>").unwrap();
    std::fs::write(repo.join("notes.txt"), "plain notes").unwrap();
    std::fs::write(
        repo.join("blob.bin"),
        [0xffu8, 0xfe, 0x00, 0x9f, 0x92, 0x96],
    )
    .unwrap();
    std::fs::write(repo.join("draft.md"), "# Draft\n").unwrap();
    let mut jsonl = user_at("look at these files", &at("00:01"));
    for (i, file) in ["shot.png", "page.html", "notes.txt", "blob.bin"]
        .iter()
        .enumerate()
    {
        let path = repo.join(file).display().to_string();
        let ts = at(&format!("00:1{i}"));
        jsonl += &read_tool_at(&format!("t{i}"), &path, &ts);
        jsonl += &tool_result_at(&format!("t{i}"), &ts);
    }
    jsonl += &user_at("and this draft", &at("00:20"));
    jsonl += &edited_file_at(&repo.join("draft.md").display().to_string(), &at("00:21"));
    let jsonl = jsonl.replace("\"cwd\":\"/r\"", &format!("\"cwd\":\"{}\"", repo.display()));
    stores.claude_session(SID, &jsonl);
    (base, stores, repo)
}

/// Record every `/__reveal` the page asks for, and answer it without sending it.
const STUB_REVEAL: &str = "window.__reveals = []; var real = window.fetch; window.fetch = function (u, o) { var s = String(u); if (/__reveal\\?/.test(s)) { window.__reveals.push(s); return Promise.resolve(new Response('', {status: 200})); } return real(u, o); }; 'ok'";

/// What the pane holds right now, for a failure message.
const PANE: &str = "(function(){ var b = document.getElementById('previewBody'); return b ? b.className + ' | ' + b.innerHTML.slice(0, 400) : 'no pane'; })()";

/// The pane head's reveal control, as `shown` (visible AND the thing a click at its centre hits —
/// a rect alone is not visibility), `hidden`, or `covered`.
const HEAD_REVEAL: &str = "(function(){ var b = document.querySelector('#previewHead .preview-reveal'); if (!b || b.hidden || !b.offsetWidth) return 'hidden'; var r = b.getBoundingClientRect(); var hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return hit && b.contains(hit) ? 'shown' : 'covered'; })()";

fn open_shell(m: &Monitor, tab: &headless_chrome::Tab, paired: bool) {
    if paired {
        m.pair(tab);
    }
    m.open(tab, &format!("?ui=app&session={SID}"));
    until(
        tab,
        "document.querySelectorAll('[data-reference-path]').length >= 4 && !!document.querySelector('.prompt-attachment')",
        "the four stamped paths and the prompt's card",
        Duration::from_secs(30),
        "document.querySelector('.transcript') ? document.querySelector('.transcript').innerText.slice(0, 300) : 'no transcript'",
    );
    eval(tab, STUB_REVEAL);
}

/// Click the stamped path for `file`, and return its two stamps (file, reveal) as the page was
/// offered them. A DOM click, as the mdrev cases click a path: the span sits in the virtualised
/// transcript, where a CDP mouse click waits on a scroll event that may never come.
fn click_path(tab: &headless_chrome::Tab, repo: &std::path::Path, file: &str) -> (String, String) {
    let path = repo.join(file).display().to_string();
    let selector = format!("[data-reference-path={path:?}]");
    let stamp = |attr: &str| {
        eval(
            tab,
            &format!("document.querySelector({selector:?}).getAttribute('{attr}') || ''"),
        )
        .as_str()
        .unwrap_or("")
        .to_string()
    };
    let stamps = (stamp("data-reference-fsig"), stamp("data-reference-sig"));
    eval(
        tab,
        &format!("document.querySelector({selector:?}).click(); 'ok'"),
    );
    stamps
}

/// The reveals the page asked for — as JSON text, since an array comes back from `eval` by
/// reference, not by value.
fn reveals(tab: &headless_chrome::Tab) -> Vec<String> {
    let text = eval(tab, "JSON.stringify(window.__reveals || [])");
    serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default()
}

/// Every view the pane draws for a file — an image, a page, text, a download — offers the file
/// manager beside it, from one control in the pane's head, and a click asks with the REVEAL stamp
/// the server offered for that path, never the file stamp. An empty pane offers nothing.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_preview_offers_the_file_manager_for_whatever_it_shows() {
    let _serial = serial();
    let (base, stores, repo) = fixture("files-pane");
    let m = Monitor::spawn(Kind::V2, 2811, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab, true);

    // Nothing open, nothing to reveal.
    tab.find_element("#previewBtn").unwrap().click().unwrap();
    until(
        &tab,
        "!!document.querySelector('#previewBody .preview-empty')",
        "the empty pane",
        Duration::from_secs(10),
        PANE,
    );
    assert_eq!(
        eval(&tab, HEAD_REVEAL).as_str(),
        Some("hidden"),
        "an empty pane offers no file"
    );

    for (file, view) in [
        ("shot.png", "#previewBody img.artifact-image"),
        ("page.html", "#previewBody iframe.artifact-html-frame"),
        ("notes.txt", "#previewBody pre.artifact-text"),
        ("blob.bin", "#previewBody [data-preview-download]"),
    ] {
        click_path(&tab, &repo, file);
        until(
            &tab,
            &format!("!!document.querySelector({view:?})"),
            &format!("the pane's view of {file}"),
            Duration::from_secs(20),
            PANE,
        );
        until(
            &tab,
            &format!("{HEAD_REVEAL} === 'shown'"),
            &format!("the file manager offered beside {file}"),
            Duration::from_secs(5),
            HEAD_REVEAL,
        );
    }
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('#previewBody pre.artifact-text').length"
        )
        .as_i64(),
        Some(0),
        "bytes the page does not show are offered as a download, never read as text: {}",
        eval(&tab, PANE)
    );
    // And the download is wired: the card fetches the bytes with the FILE stamp and hands them
    // to a download link — whose click is recorded here, never performed, so nothing lands in
    // this machine's Downloads folder.
    eval(
        &tab,
        "window.__downloads = []; var click = HTMLAnchorElement.prototype.click; HTMLAnchorElement.prototype.click = function () { if (this.hasAttribute('download')) { window.__downloads.push(this.download); return; } return click.call(this); }; 'ok'",
    );
    tab.find_element("#previewBody [data-preview-download]")
        .unwrap()
        .click()
        .unwrap();
    until(
        &tab,
        "(window.__downloads || []).indexOf('blob.bin') >= 0",
        "the download the card asked for",
        Duration::from_secs(10),
        "JSON.stringify(window.__downloads)",
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('#previewBody [data-preview-reveal]').length"
        )
        .as_i64(),
        Some(0),
        "one reveal for the pane, in its head — not a second one in the view"
    );

    // Back to the image, and ask: the query carries the reveal stamp, not the file stamp.
    let (fsig, sig) = click_path(&tab, &repo, "shot.png");
    assert!(
        !fsig.is_empty() && !sig.is_empty() && fsig != sig,
        "two stamps: {fsig:?} {sig:?}"
    );
    until(
        &tab,
        "!!document.querySelector('#previewBody img.artifact-image')",
        "the image again",
        Duration::from_secs(20),
        PANE,
    );
    tab.find_element("#previewHead .preview-reveal")
        .unwrap()
        .click()
        .unwrap();
    until(
        &tab,
        "(window.__reveals || []).length > 0",
        "the reveal the head asked for",
        Duration::from_secs(5),
        "JSON.stringify(window.__reveals)",
    );
    let asked = reveals(&tab).pop().unwrap();
    let path = repo.join("shot.png").display().to_string();
    let want: String = url_encode(&path);
    assert!(asked.contains(&format!("path={want}")), "{asked}");
    assert!(
        asked.ends_with(&format!("sig={sig}")),
        "the reveal stamp: {asked}"
    );
    assert!(!asked.contains(&fsig), "never the file stamp: {asked}");
}

/// A file the monitor cannot read — here because it was never paired, so `/file` answers 401 —
/// still offers the file manager: a button beside the explanation (and the head's control), on the
/// reader's click. Never automatically: a reveal is a side effect on the reader's desktop.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_preview_offers_the_file_manager_when_it_cannot_read_the_file() {
    let _serial = serial();
    let (base, stores, repo) = fixture("files-unpaired");
    let m = Monitor::spawn(Kind::V2, 2812, &base, Some(&stores), false);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab, false);
    let (_, sig) = click_path(&tab, &repo, "notes.txt");
    until(
        &tab,
        "!!document.querySelector('#previewBody .preview-error [data-preview-reveal]')",
        "the refusal, with the file manager offered beside it",
        Duration::from_secs(20),
        PANE,
    );
    let said = eval(
        &tab,
        "document.querySelector('#previewBody .preview-error').textContent",
    )
    .as_str()
    .unwrap_or("")
    .to_string();
    assert!(said.contains("requires pairing"), "says why: {said}");
    assert_eq!(eval(&tab, HEAD_REVEAL).as_str(), Some("shown"));
    assert!(
        reveals(&tab).is_empty(),
        "nothing revealed until the reader asks"
    );
    tab.find_element("#previewBody .preview-error [data-preview-reveal]")
        .unwrap()
        .click()
        .unwrap();
    until(
        &tab,
        "(window.__reveals || []).length > 0",
        "the reveal the reader asked for",
        Duration::from_secs(5),
        "JSON.stringify(window.__reveals)",
    );
    assert!(reveals(&tab)
        .pop()
        .unwrap()
        .ends_with(&format!("sig={sig}")));
}

/// A prompt card keeps its own action and gains the file manager beside it, as the process-surface
/// card already had: two controls, never a second action folded into one.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_prompt_card_offers_the_file_manager_beside_its_action() {
    let _serial = serial();
    let (base, stores, _repo) = fixture("files-card");
    let m = Monitor::spawn(Kind::V2, 2813, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    open_shell(&m, &tab, true);
    let card = ".prompt-attachment-pair .prompt-attachment";
    let reveal = ".prompt-attachment-pair .prompt-attachment-reveal";
    until(
        &tab,
        &format!("!!document.querySelector({reveal:?})"),
        "the draft's card, with the file manager beside it",
        Duration::from_secs(10),
        "document.querySelector('.prompt-attachments') ? document.querySelector('.prompt-attachments').outerHTML.slice(0, 600) : 'no cards'",
    );
    assert_eq!(
        eval(
            &tab,
            &format!("document.querySelector({card:?}).dataset.attachmentAction")
        )
        .as_str(),
        Some("preview"),
        "the card keeps its own action"
    );
    let hit = format!("(function(){{ var b = document.querySelector({reveal:?}); b.scrollIntoView({{block: 'center'}}); var r = b.getBoundingClientRect(); var h = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!h && b.contains(h) && r.width > 0; }})()");
    assert_eq!(
        eval(&tab, &hit).as_bool(),
        Some(true),
        "the reveal is on screen and takes the click"
    );
    let sig = eval(
        &tab,
        &format!("document.querySelector({reveal:?}).dataset.sig"),
    )
    .as_str()
    .unwrap_or("")
    .to_string();
    assert!(!sig.is_empty());
    tab.find_element(reveal).unwrap().click().unwrap();
    until(
        &tab,
        "(window.__reveals || []).length > 0",
        "the reveal the card asked for",
        Duration::from_secs(5),
        "JSON.stringify(window.__reveals)",
    );
    assert!(reveals(&tab)
        .pop()
        .unwrap()
        .ends_with(&format!("sig={sig}")));
    assert_eq!(
        eval(&tab, "document.querySelectorAll('#previewBody .artifact-text, #previewBody .mdrev-host').length").as_i64(),
        Some(0),
        "revealing opened nothing in the page"
    );
}

/// `encodeURIComponent`, as the page encodes a path in the query.
/// A 1×1 24-bit BMP: a raster `/file` serves as `image/bmp`.
const BMP: &[u8] = &[
    0x42, 0x4d, 0x3a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00,
    0x00, // file header
    0x28, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x13, 0x0b, 0x00, 0x00, 0x13, 0x0b, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // info header
    0x00, 0x00, 0xff, 0x00, // one pixel, and the row's padding
];

/// #275 — a PATH is offered as an image exactly when `/file` will serve it as one.
///
/// The page decided "image" from a list of extensions the server did not share: a path `.svg` was
/// offered as "Enlarge", the lightbox asked `/file`, got the SVG's source as text (an SVG served
/// from this origin could run script, so it is never served as an image) and showed its error;
/// a path `.bmp`, which the server does serve as an image, was offered as a download. Codex
/// Desktop's "Files mentioned by the user" is where such a path arrives, one pointer per prompt
/// (two in a row would fold into a run and never reach the rule).
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_offers_a_path_as_an_image_exactly_when_the_server_serves_one() {
    let _serial = serial();
    let base = base("files-raster");
    let stores = Stores::new(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("diagram.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="red"/></svg>"#,
    )
    .unwrap();
    std::fs::write(repo.join("scan.bmp"), BMP).unwrap();
    const CODEX: &str = "5e5510a1-0000-4000-8000-000000000275";
    let line = |v: serde_json::Value| format!("{v}\n");
    let mentioned = |file: &str, ts: &str| {
        line(serde_json::json!({
            "timestamp": ts, "type": "response_item",
            "payload": {"type": "message", "role": "user", "content": [{"type": "input_text",
                "text": format!("# Files mentioned by the user:\n\n## {file}: {}\n\n## My request:\nLook at {file}\n", repo.join(file).display())}]}}))
    };
    let said = |text: &str, ts: &str| {
        line(serde_json::json!({
            "timestamp": ts, "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}}))
    };
    let mut jsonl = line(serde_json::json!({
        "timestamp": at("00:00"), "type": "session_meta",
        "payload": {"id": CODEX, "cwd": repo.display().to_string(), "originator": "codex-tui", "cli_version": "0.147.0"}}));
    jsonl += &mentioned("diagram.svg", &at("00:01"));
    jsonl += &said("A red square.", &at("00:02"));
    jsonl += &mentioned("scan.bmp", &at("00:03"));
    jsonl += &said("One pixel.", &at("00:04"));
    stores.codex_session(CODEX, &jsonl);

    let m = Monitor::spawn(Kind::V2, 2814, &base, Some(&stores), true);
    let browser = chrome();
    let tab = browser.new_tab().unwrap();
    m.pair(&tab);
    // The monitor names a Codex session by its rollout's file stem.
    m.open(&tab, &format!("?ui=app&session=rollout-{CODEX}"));
    let card = |file: &str| format!("[data-attachment-action][data-path$=\"/{file}\"]");
    until(
        &tab,
        &format!(
            "!!document.querySelector({:?}) && !!document.querySelector({:?})",
            card("diagram.svg"),
            card("scan.bmp")
        ),
        "a card for each mentioned file",
        Duration::from_secs(30),
        "document.querySelector('.transcript') ? document.querySelector('.transcript').innerText.slice(0, 300) : 'no transcript'",
    );
    eval(&tab, STUB_REVEAL);
    let action = |file: &str| {
        eval(
            &tab,
            &format!(
                "document.querySelector({:?}).dataset.attachmentAction",
                card(file)
            ),
        )
        .as_str()
        .unwrap_or("")
        .to_string()
    };
    assert_eq!(
        (action("diagram.svg"), action("scan.bmp")),
        ("preview".to_string(), "image".to_string()),
        "a path .svg is text to read (the server sends its source) and a path .bmp is an image \
         (the server serves it as one)"
    );

    // The image the page now offers really draws.
    eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            card("scan.bmp")
        ),
    );
    until(
        &tab,
        "(function(){ var i = document.querySelector('[data-lightbox-image]'); var e = document.querySelector('.image-lightbox-error'); return !!i && i.naturalWidth === 1 && !!e && e.hidden; })()",
        "the lightbox drawing the one-pixel BMP",
        Duration::from_secs(10),
        "(function(){ var i = document.querySelector('[data-lightbox-image]'); var e = document.querySelector('.image-lightbox-error'); return JSON.stringify({ src: i && i.getAttribute('src'), w: i && i.naturalWidth, error: e && !e.hidden }); })()",
    );
    eval(
        &tab,
        "document.querySelector('[data-lightbox-close]').click(); 'ok'",
    );

    // …and the SVG is read as its source, in the preview pane.
    eval(
        &tab,
        &format!(
            "document.querySelector({:?}).click(); 'ok'",
            card("diagram.svg")
        ),
    );
    until(
        &tab,
        "(function(){ var t = document.querySelector('#previewBody pre.artifact-text'); return !!t && t.textContent.indexOf('<svg') >= 0; })()",
        "the preview pane showing the SVG's source",
        Duration::from_secs(10),
        PANE,
    );
    assert!(
        reveals(&tab).is_empty(),
        "nothing asked the file manager for anything"
    );
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
