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
//! Ports 2811–2814, and 2933 for #374's handed-over files on a phone. The classic page draws no preview pane; its file view already pairs the two
//! (export.js `openArtifact` and its "Reveal in file manager" action), which is the reference here.

use std::path::PathBuf;
use std::time::Duration;

mod harness;
use harness::{
    at, base, chrome_tab, edited_file_at, eval, read_tool_at, serial, tool_result_at, until,
    user_at, Kind, Monitor, Stores,
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
const HEAD_REVEAL: &str = "(function(){ var b = document.querySelector('#previewHead .preview-reveal'); if (!b || b.hidden || !b.offsetWidth) return 'hidden'; var r = b.getBoundingClientRect(); var hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); if (!hit) return 'off-screen at ' + Math.round(r.left) + ',' + Math.round(r.top) + ' in ' + innerWidth + 'x' + innerHeight; return b.contains(hit) ? 'shown' : 'covered by ' + (hit ? hit.tagName.toLowerCase() + (hit.id ? '#' + hit.id : '') + (hit.className && hit.className.baseVal === undefined ? '.' + String(hit.className).trim().split(/\\s+/).join('.') : '') : 'nothing'); })()";

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
    let (_browser, tab) = chrome_tab();
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
    let (_browser, tab) = chrome_tab();
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
    // A wait, as the other files cases use (#295): the pane may still be opening when the refusal
    // lands, and one hit test then reads whatever the pane has not yet slid clear of. A cover that
    // lasts is still a failure, and the probe names what covers it. The first sample is reported
    // when it is not `shown`, so a transient cover is recorded even on a run that passes.
    let first = eval(&tab, HEAD_REVEAL);
    if first.as_str() != Some("shown") {
        let pane = eval(&tab, "(function(){ var p = document.querySelector('.preview'); var r = p ? p.getBoundingClientRect() : null; return JSON.stringify({ w: innerWidth, pane: r ? [Math.round(r.left), Math.round(r.width)] : null, transform: p ? getComputedStyle(p).transform : null, app: document.getElementById('app').className }); })()");
        eprintln!("#295 first sample: {first} — {pane}");
    }
    until(
        &tab,
        &format!("{HEAD_REVEAL} === 'shown'"),
        "the file manager offered in the pane's head beside the refusal",
        Duration::from_secs(5),
        HEAD_REVEAL,
    );
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
    let (_browser, tab) = chrome_tab();
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
    let (_browser, tab) = chrome_tab();
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

/// A real PNG of `w`×`h` grey pixels, uncompressed (stored deflate blocks) — big enough that the
/// pane has to SHRINK it to fit, which a 1×1 image can never show.
fn big_png(w: u32, h: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in bytes {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    let mut raw = Vec::with_capacity(((w + 1) * h) as usize);
    for y in 0..h {
        raw.push(0); // filter: none
        raw.extend((0..w).map(|x| ((x + y) % 200 + 40) as u8));
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit greyscale
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// #305 — the owner: an image opened in the preview pane was drawn thumbnail-sized at the top of an
/// empty pane, where Preview.app fit it to the window. The stage had no height of its own (the
/// image is absolutely positioned), so the fit used its PADDING box, about 68px tall. A large image
/// fills the pane on one axis, and re-fits when the pane changes width.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_preview_fits_a_large_image_to_the_pane() {
    let _serial = serial();
    let (base, stores, repo) = fixture("files-fit");
    std::fs::write(repo.join("shot.png"), big_png(1200, 800)).unwrap();
    let m = Monitor::spawn(Kind::V2, 2816, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    harness::resize(&tab, 1440.0, 900.0);
    open_shell(&m, &tab, true);
    click_path(&tab, &repo, "shot.png");
    until(
        &tab,
        "(function(){ var i = document.querySelector('#previewBody img.artifact-image'); return !!i && i.complete && i.naturalWidth === 1200; })()",
        "the large image in the pane",
        Duration::from_secs(20),
        PANE,
    );
    let measure = "(function(){ var s = document.querySelector('#previewBody .artifact-stage'); var i = s.querySelector('img'); var r = i.getBoundingClientRect(); return JSON.stringify({ sw: s.clientWidth, sh: s.clientHeight, iw: Math.round(r.width), ih: Math.round(r.height) }); })()";
    let fits = |label: &str| -> serde_json::Value {
        until(
            &tab,
            &format!("(function(){{ var m = JSON.parse({measure}); return m.sh > 200 && (m.iw >= Math.min(m.sw, 1200) - 4 || m.ih >= Math.min(m.sh, 800) - 4); }})()"),
            label,
            Duration::from_secs(5),
            measure,
        );
        serde_json::from_str(eval(&tab, measure).as_str().unwrap_or("{}")).unwrap()
    };
    let first = fits("the image to fill the pane on one axis");
    // The pane widens: the image follows it.
    eval(
        &tab,
        "document.getElementById('app').style.setProperty('--preview', '760px'); 'ok'",
    );
    until(
        &tab,
        &format!(
            "JSON.parse({measure}).sw > {}",
            first["sw"].as_i64().unwrap_or(0) + 50
        ),
        "the pane to widen",
        Duration::from_secs(5),
        measure,
    );
    let wider = fits("the image to re-fit the wider pane");
    assert!(
        wider["iw"].as_i64() > first["iw"].as_i64(),
        "a wider pane draws the image wider: {first} → {wider}"
    );
}

/// #313, the owner: images "can not pinch zoom … in either the popup float window or in preview pane
/// (on mac both supports pinch zoom)". The preview pane's image is drawn by the same shared viewer
/// as the floating one (`shared/image-view.js`), which read one pointer and the ctrl+wheel a Mac's
/// pinch sends — never a phone's two fingers. On a phone, two fingers spreading on the pane's image
/// zoom it in.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_preview_pinch_zooms_an_image_on_a_phone() {
    let _serial = serial();
    let (base, stores, repo) = fixture("files-pinch");
    std::fs::write(repo.join("shot.png"), big_png(1200, 800)).unwrap();
    let m = Monitor::spawn(Kind::V2, 2804, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 390, 844);
    open_shell(&m, &tab, true);
    click_path(&tab, &repo, "shot.png");
    let stage = "#previewBody .artifact-stage";
    until(
        &tab,
        &format!("(function(){{ var s = document.querySelector('{stage}'); var i = s && s.querySelector('img'); return !!i && i.complete && i.naturalWidth === 1200 && Number(s.dataset.zoom || 0) > 0 && s.getBoundingClientRect().width > 200; }})()"),
        "the image in the pane, fitted",
        Duration::from_secs(20),
        PANE,
    );
    std::thread::sleep(Duration::from_millis(300));
    let zoom = || {
        eval(
            &tab,
            &format!("Number(document.querySelector('{stage}').dataset.zoom)"),
        )
        .as_f64()
        .unwrap()
    };
    let fit = zoom();
    // As JSON text: `eval` hands an array back by reference, not by value.
    let centre: Vec<f64> = serde_json::from_str(eval(&tab, &format!("JSON.stringify((function(){{ var r = document.querySelector('{stage}').getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; }})())")).as_str().unwrap_or("[]")).unwrap_or_default();
    harness::pinch(&tab, centre[0], centre[1], 60.0, 240.0, 10);
    until(
        &tab,
        &format!(
            "Number(document.querySelector('{stage}').dataset.zoom) >= {}",
            fit * 2.5
        ),
        "two fingers spreading on the pane's image to zoom it in",
        Duration::from_secs(5),
        &format!("document.querySelector('{stage}').dataset.zoom + ' from {fit}'"),
    );
    assert!(zoom() > fit, "zoomed in from {fit}");
}

/// #374: what a session HANDS to the reader is downloadable wherever it lives, on a phone too. The
/// owner sent two films with `SendUserFile` from `~/Movies` and could only reveal them in Finder or
/// copy the path — useless on a paired phone — because a page reads a file only where the render
/// allowlist reaches AND a hosted session explains the path, and `~/Movies` is neither. A file the
/// agent sent, or the user pasted, is handed over; the allowlist and containment are for paths a
/// transcript merely mentions.
///
/// The world: the owner's allowlist (the session's repo alone); a 9 MB film outside every root, so
/// over the viewer's cap as well, which streams it as a download; and a pasted image whose original
/// the client saved under `<home>/uploads/<session>/`, which the allowlist does not reach either.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn a_phone_downloads_what_the_session_handed_over_wherever_it_lives() {
    const HANDED: &str = "5e5510a1-0000-4000-8000-000000000374";
    let _serial = serial();
    let base = base("files-handed");
    let stores = Stores::new(&base);
    let repo = base.join("repo");
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let film = elsewhere.join("tour.mp4");
    let mut bytes = vec![
        0u8, 0, 0, 0x20, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm', 0xff,
    ];
    bytes.resize(9 * 1024 * 1024, 0);
    std::fs::write(&film, &bytes).unwrap();
    let original = stores
        .root
        .join("uploads")
        .join(HANDED)
        .join("0a1b2c3d-image.png");
    std::fs::create_dir_all(original.parent().unwrap()).unwrap();
    std::fs::write(&original, harness::base64_bytes(harness::WIDE_PNG_B64)).unwrap();

    let mut jsonl =
        harness::pasted_image_sized("here is a screenshot", &at("00:01"), harness::TINY_PNG_B64);
    jsonl += &format!(
        "{{\"type\":\"attachment\",\"timestamp\":\"{}\",\"attachment\":{{\"type\":\"inlined_image_paths\",\"paths\":[{:?}]}}}}\n",
        at("00:02"),
        original.display().to_string()
    );
    jsonl += &user_at("send me the film", &at("00:10"));
    jsonl += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"s1\",\"name\":\"SendUserFile\",\"input\":{{\"files\":[{:?}],\"caption\":\"the film\",\"status\":\"normal\",\"display\":\"render\"}}}}]}},\"timestamp\":\"{}\"}}\n",
        film.display().to_string(),
        at("00:11")
    );
    jsonl += &tool_result_at("s1", &at("00:12"));
    let jsonl = jsonl.replace("\"cwd\":\"/r\"", &format!("\"cwd\":\"{}\"", repo.display()));
    stores.claude_session(HANDED, &jsonl);

    // The owner's allowlist, in this monitor's own state dir: the session's repo and nothing else.
    let state = base.join("state-2933");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("render-policy.json"),
        format!(
            "{{\"mode\":\"allowlist\",\"dirs\":[{:?}]}}",
            repo.display().to_string()
        ),
    )
    .unwrap();
    let m = Monitor::spawn(Kind::V2, 2933, &base, Some(&stores), true);
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 440, 956);
    m.pair(&tab);
    m.open(&tab, &format!("?ui=app&session={HANDED}"));
    let selector = format!("[data-reference-path={:?}]", film.display().to_string());
    until(
        &tab,
        &format!("!!document.querySelector({selector:?}) && !!document.querySelector('[data-attachment][data-path$=\"-image.png\"]')"),
        "the delivered film and the pasted image",
        Duration::from_secs(30),
        "document.querySelector('.transcript') ? document.querySelector('.transcript').innerText.slice(0, 300) : 'no transcript'",
    );

    // The film: offered for reading, and the pane hands it over as a download of its real size.
    let fsig = eval(
        &tab,
        &format!("document.querySelector({selector:?}).getAttribute('data-reference-fsig') || ''"),
    );
    assert!(
        !fsig.as_str().unwrap_or("").is_empty(),
        "a delivered file outside the allowlist and every root is still offered for reading"
    );
    eval(
        &tab,
        &format!("document.querySelector({selector:?}).click(); 'ok'"),
    );
    until(
        &tab,
        "!!document.querySelector('#previewBody [data-preview-download]')",
        "the pane offering the film as a download",
        Duration::from_secs(20),
        PANE,
    );
    let pane = eval(&tab, "document.getElementById('previewBody').innerText");
    assert!(
        pane.as_str().unwrap_or("").contains("9.0 MB"),
        "the film's own size, from the streamed download's head: {pane:?}"
    );
    eval(
        &tab,
        "document.querySelector('#previewBody [data-preview-download]').click(); 'ok'",
    );
    until(
        &tab,
        "document.getElementById('toast').textContent === 'Download started'",
        "the whole film fetched and saved",
        Duration::from_secs(20),
        "document.getElementById('toast').textContent",
    );

    // The pasted image: its saved original is served, so the lightbox opens the original rather
    // than the downscaled copy the transcript carries.
    let fetched = eval(
        &tab,
        "(async function(){ var a = document.querySelector('[data-attachment][data-path$=\"-image.png\"]'); var r = await fetch('/file?path=' + encodeURIComponent(a.dataset.path) + '&sig=' + encodeURIComponent(a.dataset.fsig || ''), { cache: 'no-store' }); return r.status + ' ' + (r.headers.get('content-type') || ''); })()",
    );
    assert_eq!(
        fetched.as_str().unwrap_or(""),
        "200 image/png",
        "a pasted image's original under uploads is served whatever the allowlist says"
    );
}
