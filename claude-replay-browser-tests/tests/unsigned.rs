//! #275 — a path the page offers with NO stamp is copied, never sent to `/__reveal` unsigned.
//!
//! A reveal stamp is minted for every offered path whenever a signing key exists, so a stampless
//! path is what a server with NO usable key serves. The classic page answered one with an UNSIGNED
//! reveal, which the server refuses (404) every time — the path flashed "not found" — and the app
//! shell drew it as plain text, which could not even be copied. The shared rule
//! (`referenceAction`, shared/capabilities.js) has always said the third answer is to copy the path.
//!
//! Its own binary on purpose: the signing key is read once per process, so a case that needs NO
//! key cannot share a process with the cases that sign. Each surface gets a key it cannot use —
//! garbage in `file-sig-key`, read-only, so it can be neither parsed nor replaced.
//!
//! Port 2815. Nothing is revealed: the page's `fetch` is wrapped and every `/__reveal` recorded.

mod harness;
use claude_replay_html::start_server;
use claude_replay_present::Args;
use harness::{
    at, base, chrome, copied_text, eval, read_tool_at, serial, stub_clipboard, tool_result_at,
    until, user_at, Kind, Monitor, Stores,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SID: &str = "5e5510a1-0000-4000-8000-000000000275";

/// Record every `/__reveal` the page asks for, and answer it without sending it.
const STUB_REVEAL: &str = "window.__reveals = []; var real = window.fetch; window.fetch = function (u, o) { var s = String(u); if (/__reveal\\?/.test(s)) { window.__reveals.push(s); return Promise.resolve(new Response('', {status: 404})); } return real(u, o); }; 'ok'";

/// A key the server can neither read nor replace: every path it offers goes out unstamped.
fn spoil_key(state: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(state).unwrap();
    let key = state.join("file-sig-key");
    std::fs::write(&key, "not a key").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o400)).unwrap();
}

/// A session that READ one real file: a path in a tool head.
fn fixture(name: &str) -> (PathBuf, Stores, PathBuf, String) {
    let base = base(name);
    let stores = Stores::new(&base);
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let file = repo.join("notes.txt");
    std::fs::write(&file, "plain notes").unwrap();
    let file = file.display().to_string();
    let mut jsonl = user_at("read my notes", &at("00:01"));
    jsonl += &read_tool_at("t1", &file, &at("00:02"));
    jsonl += &tool_result_at("t1", &at("00:02"));
    let jsonl = jsonl.replace("\"cwd\":\"/r\"", &format!("\"cwd\":\"{}\"", repo.display()));
    let transcript = stores.claude_session(SID, &jsonl);
    (base, stores, transcript, file)
}

fn reveals(tab: &headless_chrome::Tab) -> Vec<String> {
    let text = eval(tab, "JSON.stringify(window.__reveals || [])");
    serde_json::from_str(text.as_str().unwrap_or("[]")).unwrap_or_default()
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_copy_a_path_offered_without_a_stamp() {
    let _serial = serial();
    std::env::remove_var("AGENT_MONITOR_STATE");

    // The classic page, from this process's own server: its state dir is the one `base()` set up.
    {
        let (base, stores, transcript, file) = fixture("unsigned-classic");
        spoil_key(&PathBuf::from(
            std::env::var_os("CLAUDE_MONITOR_STATE").expect("base() isolates the state dir"),
        ));
        std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
        for (key, value) in stores.envs() {
            std::env::set_var(key, value);
        }
        let args = Args {
            no_cache: true,
            ..Default::default()
        };
        let server = start_server(&args, std::slice::from_ref(&transcript)).expect("server");
        let browser = chrome();
        let tab = browser.new_tab().unwrap();
        tab.navigate_to(&server.url_for_root(0).expect("hosted"))
            .unwrap();
        tab.wait_until_navigated().unwrap();
        let link = format!(".tool-path[data-path={file:?}]");
        until(
            &tab,
            &format!("!!document.querySelector({link:?})"),
            "the Read's path",
            Duration::from_secs(30),
            "document.body.innerText.slice(0, 300)",
        );
        assert_eq!(
            eval(
                &tab,
                &format!("document.querySelector({link:?}).hasAttribute('data-sig')")
            )
            .as_bool(),
            Some(false),
            "the premise: a server with no key offers the path unstamped"
        );
        eval(&tab, STUB_REVEAL);
        stub_clipboard(&tab);
        eval(
            &tab,
            &format!("document.querySelector({link:?}).click(); 'ok'"),
        );
        until(
            &tab,
            &format!("window.__copied === {file:?}"),
            "the classic page copying the path",
            Duration::from_secs(5),
            "JSON.stringify({ copied: window.__copied, reveals: window.__reveals })",
        );
        assert_eq!(
            reveals(&tab),
            Vec::<String>::new(),
            "Classic: no unsigned reveal is sent — the server would refuse it"
        );
    }

    // The app shell, from a monitor whose own state dir holds the unusable key.
    {
        let (base, stores, _, file) = fixture("unsigned-app");
        spoil_key(&base.join("state-2815"));
        let m = Monitor::spawn(Kind::V2, 2815, &base, Some(&stores), true);
        let browser = chrome();
        let tab = browser.new_tab().unwrap();
        m.pair(&tab);
        m.open(&tab, &format!("?ui=app&session={SID}"));
        until(
            &tab,
            // textContent, not innerText: the Read sits in a collapsed activity until opened.
            "(document.querySelector('.transcript') || {}).textContent && document.querySelector('.transcript').textContent.indexOf('notes.txt') >= 0",
            "the Read on the app shell",
            Duration::from_secs(30),
            "document.querySelector('.transcript') ? document.querySelector('.transcript').innerText.slice(0, 300) : 'no transcript'",
        );
        let link = format!("[data-reference-path={file:?}]");
        let offered = eval(
            &tab,
            &format!(
                "(function(){{ var e = document.querySelector({link:?}); return e ? JSON.stringify({{ sig: e.dataset.referenceSig || '', fsig: e.dataset.referenceFsig || '' }}) : 'absent'; }})()"
            ),
        );
        assert_eq!(
            offered.as_str(),
            Some(r#"{"sig":"","fsig":""}"#),
            "AppShell: the unstamped path is still an offered path — the click copies it"
        );
        eval(&tab, STUB_REVEAL);
        stub_clipboard(&tab);
        eval(
            &tab,
            &format!("document.querySelector({link:?}).click(); 'ok'"),
        );
        until(
            &tab,
            &format!("window.__copied === {file:?}"),
            "the app shell copying the path",
            Duration::from_secs(5),
            "JSON.stringify({ copied: window.__copied, reveals: window.__reveals })",
        );
        assert_eq!(copied_text(&tab), file, "AppShell: the path is copied");
        assert_eq!(
            reveals(&tab),
            Vec::<String>::new(),
            "AppShell: no unsigned reveal is sent"
        );
    }
}
