//! #s124 — `--dump-html --no-storage`: a page for a frame sandboxed with scripts but without its
//! origin, where every browser store throws `SecurityError` and the host's lint flags the very
//! names. The page keeps its theme, folds and view in memory and never reaches for a store.
//!
//! The sandbox is modelled before the page's own scripts run: each store, and `document.cookie`,
//! throws as a sandboxed frame's does, and counts the touch. The `--no-storage` page must switch
//! theme, fold and search with no touch and no error; the default page, under the same trap, still
//! reaches for its stores (guarded, so it keeps working), and with real stores it remembers its
//! theme across a reload, as before. Opened from disk: no server, no port.

mod harness;

use claude_replay_present::Args;
use harness::{
    assistant_at, base, bash_call_at, chrome_tab, eval, serial, tool_result_lines, until, user_at,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SID: &str = "eeeeeeee-0000-4000-8000-0000000001a9";

/// What a frame sandboxed without `allow-same-origin` does to a page's stores, with a counter.
const SANDBOX_TRAP: &str = r#"(function(){
  window.__storeTouches = 0; window.__pageErrors = [];
  addEventListener('error', function (e) { window.__pageErrors.push(String(e.message)); });
  addEventListener('unhandledrejection', function (e) { window.__pageErrors.push(String(e.reason)); });
  function refuse() { window.__storeTouches++; throw new DOMException('The document is sandboxed and lacks the allow-same-origin flag.', 'SecurityError'); }
  ['localStorage', 'sessionStorage', 'indexedDB'].forEach(function (name) {
    Object.defineProperty(window, name, { configurable: true, get: refuse });
  });
  Object.defineProperty(Document.prototype, 'cookie', { configurable: true, get: refuse, set: refuse });
})();"#;

fn transcript() -> String {
    [
        user_at("Count the sample items.", "2026-09-01T04:00:00.000Z"),
        bash_call_at(
            "demo-count --sample",
            "toolu_s124a",
            "2026-09-01T04:00:01.000Z",
        ),
        tool_result_lines("toolu_s124a", 30, "2026-09-01T04:00:02.000Z"),
        assistant_at(
            "The sample store holds forty items.",
            "2026-09-01T04:00:03.000Z",
        ),
        user_at("And the demo queue?", "2026-09-01T04:01:00.000Z"),
        assistant_at(
            "The demo queue holds no sample items.",
            "2026-09-01T04:01:01.000Z",
        ),
    ]
    .concat()
}

fn dump(dir: &Path, transcript_path: &Path, stem: &str, no_storage: bool) -> PathBuf {
    claude_replay_html::dump_html(
        &Args {
            dump_html: Some(Some(dir.join(stem).to_string_lossy().into_owned())),
            no_cache: true,
            no_storage,
            ..Default::default()
        },
        transcript_path,
    )
    .unwrap();
    dir.join(format!("{stem}.html"))
}

fn open(tab: &headless_chrome::Tab, page: &Path) {
    tab.navigate_to(&format!("file://{}?mountall=1", page.display()))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        tab,
        "document.querySelectorAll('#stream .fold').length > 0",
        "the session drawn, with its fold",
        Duration::from_secs(10),
        "document.querySelectorAll('#stream [data-k]').length",
    );
}

fn trap(tab: &headless_chrome::Tab) {
    use headless_chrome::protocol::cdp::Page;
    tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
        source: SANDBOX_TRAP.to_string(),
        world_name: None,
        include_command_line_api: None,
        run_immediately: None,
    })
    .expect("the sandbox trap is installed");
}

const THEME: &str = "document.documentElement.getAttribute('data-theme')";

/// Theme, a fold and the search, each through the page's own controls.
fn exercise(tab: &headless_chrome::Tab) {
    assert_eq!(eval(tab, THEME), "light");
    eval(tab, "document.getElementById('btn-theme').click(); true");
    assert_eq!(eval(tab, THEME), "dark", "the theme switches");
    let open_before = eval(
        tab,
        "document.querySelector('#stream .fold').dataset.open || ''",
    );
    eval(
        tab,
        "document.querySelector('#stream .fold > .fold-h').click(); true",
    );
    until(
        tab,
        &format!("(document.querySelector('#stream .fold').dataset.open || '') !== {open_before}"),
        "the fold to toggle",
        Duration::from_secs(5),
        "document.querySelector('#stream .fold').outerHTML.slice(0, 160)",
    );
    eval(
        tab,
        "(function(){ var q = document.getElementById('q'); q.value = 'sample'; q.dispatchEvent(new Event('input', {bubbles: true})); q.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true})); return true; })()",
    );
    until(
        tab,
        "/[0-9]/.test(document.getElementById('qcount').textContent)",
        "the search to count its matches",
        Duration::from_secs(5),
        "document.getElementById('qcount').textContent",
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn a_no_storage_export_works_where_every_store_is_refused() {
    let _serial = serial();
    let dir = base("no-storage");
    let stores = harness::Stores::new(&dir);
    let path = stores.claude_session(SID, &transcript());
    let memory = dump(&dir, &path, "memory", true);
    let browser = dump(&dir, &path, "browser", false);

    // The --no-storage page, sandboxed: everything works, and no store is ever touched.
    let (_b1, tab) = chrome_tab();
    trap(&tab);
    open(&tab, &memory);
    exercise(&tab);
    open(&tab, &memory);
    assert_eq!(
        eval(&tab, THEME),
        "light",
        "memory lasts for the page's life: a reload starts afresh"
    );
    assert_eq!(eval(&tab, "window.__storeTouches"), 0, "no store touched");
    assert_eq!(
        eval(&tab, "window.__pageErrors.length"),
        0,
        "no error: {}",
        eval(&tab, "JSON.stringify(window.__pageErrors)")
    );

    // The default page under the same sandbox still reaches for its stores, every touch guarded,
    // so it keeps working too.
    let (_b2, tab) = chrome_tab();
    trap(&tab);
    open(&tab, &browser);
    exercise(&tab);
    assert!(
        eval(&tab, "window.__storeTouches").as_i64().unwrap_or(0) > 0,
        "the default page still uses the browser's stores"
    );
    assert_eq!(eval(&tab, "window.__pageErrors.length"), 0);

    // With real stores the default page remembers its theme across a reload, as before.
    let (_b3, tab) = chrome_tab();
    open(&tab, &browser);
    eval(&tab, "document.getElementById('btn-theme').click(); true");
    open(&tab, &browser);
    assert_eq!(eval(&tab, THEME), "dark", "the default page remembers");
}
