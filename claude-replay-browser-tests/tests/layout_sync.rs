//! The Arrange choice (#s387, design/layout-sync.md): the session list arranged like the Claude
//! app, the Codex app or Herdr, on hermetic stores — the Claude app's session records and settings
//! file and the Codex app's databases and state file written by each case, and the harness's
//! herdr stand-in (`Stores::new`) answering from fixture files, so no case reads this machine's
//! apps or runs its herdr.
//!
//! Ports 3211–3215.

use std::time::{Duration, Instant};

mod harness;
use harness::{base, chrome_tab, eval, serial, until, user_at, Kind, Monitor, Stores};

const A: &str = "5a000000-0000-4000-8000-000000000387";
const B: &str = "5b000000-0000-4000-8000-000000000387";
const C: &str = "5c000000-0000-4000-8000-000000000387";
const D: &str = "5d000000-0000-4000-8000-000000000387";

/// A Claude Code transcript in the hermetic store, last written `secs` ago.
fn transcript(stores: &Stores, sid: &str, title: &str, secs: u64) {
    let proj = stores.root.join("claude").join("-r");
    std::fs::create_dir_all(&proj).unwrap();
    let path = proj.join(format!("{sid}.jsonl"));
    let body = format!(
        "{{\"type\":\"ai-title\",\"aiTitle\":\"{title}\",\"sessionId\":\"{sid}\"}}\n{}{}",
        user_at(title, &harness::rfc3339_secs_ago(secs + 5)),
        harness::history::assistant_phased_at("Done.", &harness::rfc3339_secs_ago(secs), true)
    );
    std::fs::write(&path, body).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// Four sessions: A (active, in the app's Sessions), B (idle eight hours, filed under the app's
/// Planning group), C (A's side session) and D (not in the app at all).
fn world(stores: &Stores) {
    transcript(stores, A, "alpha session", 60);
    transcript(stores, B, "beta session", 8 * 3600);
    transcript(stores, C, "gamma session", 120);
    transcript(stores, D, "delta session", 30);
    stores.app_session("local_a", A, "App alpha", 60, None);
    stores.app_session("local_b", B, "App beta", 8 * 3600, None);
    stores.app_session("local_c", C, "App gamma", 120, Some("local_a"));
    stores.app_settings(&[("cg-plan", "Planning", &["local_b"])]);
}

/// The arranged list as the reader sees it: each head, each sub-head, and each row's id, title,
/// whether it is nested, and whether it is drawn as absent.
const LIST_JS: &str = "JSON.stringify({ strip: (document.getElementById('arrangeStrip') || {}).innerText || '', warn: !!document.querySelector('#arrangeStrip.warn'), flat: document.getElementById('app').classList.contains('sessions-flat'), items: Array.from(document.querySelectorAll('#tree .arranged-head, #tree .arranged-group, #tree .tree-row.session')).map(function (e) { if (e.classList.contains('arranged-head')) return ['head', e.querySelector('.arranged-label').innerText]; if (e.classList.contains('arranged-group')) return ['group', e.innerText]; return ['row', e.dataset.session, e.querySelector('.tree-title').innerText, e.classList.contains('is-side'), e.classList.contains('is-absent')]; }) })";

fn list(tab: &headless_chrome::Tab) -> serde_json::Value {
    eval(tab, LIST_JS)
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn wait_list(
    tab: &headless_chrome::Tab,
    want: &dyn Fn(&serde_json::Value) -> bool,
    what: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = list(tab);
    while !want(&seen) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
        seen = list(tab);
    }
    assert!(want(&seen), "{what}: {seen}");
    seen
}

/// Click a control by selector, through the page (a desktop's click).
fn click(tab: &headless_chrome::Tab, sel: &str) {
    let r = eval(
        tab,
        &format!("(function () {{ var el = document.querySelector({sel:?}); if (!el) return 'missing'; el.click(); return 'ok'; }})()"),
    );
    assert_eq!(r.as_str(), Some("ok"), "clicked {sel}");
}

/// Choose an arrangement in the filter sheet.
fn arrange(tab: &headless_chrome::Tab, key: &str) {
    click(tab, "#filterBtn");
    click(tab, &format!("#sessionFilter [data-arrange=\"{key}\"]"));
    eval(
        tab,
        "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))",
    );
}

fn heads(v: &serde_json::Value) -> Vec<String> {
    v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i[0] == "head")
                .map(|i| i[1].as_str().unwrap_or("").to_uppercase())
                .collect()
        })
        .unwrap_or_default()
}

fn row_ids(v: &serde_json::Value) -> Vec<String> {
    v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i[0] == "row")
                .map(|i| i[1].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// #s387: "Like the Claude app" draws the app's sidebar — its groups and Sessions, by its titles,
/// a side session under its starter — and shows the SAME sessions the app shows: an idle session
/// and a hidden one are still drawn in the app's group, whatever the session filter and the hides
/// say; everything else waits under a collapsed "Not in the Claude app". The choice survives a
/// reload, and so does the arrangement it replaced (the review of #s387: "One list by activity"
/// was lost), which the line's ✕ returns to.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn arranged_like_the_claude_app_shows_its_sessions_in_its_groups() {
    let _serial = serial();
    let base = base("layout-claude");
    let stores = Stores::new(&base);
    world(&stores);
    let _monitor = Monitor::spawn(Kind::V1, 3211, &base, Some(&stores), false);
    let (_browser, tab) = chrome_tab();
    let url = "http://127.0.0.1:3211/?ui=app";
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        "!!document.querySelector('#tree .tree-row.session')",
        "the list is drawn",
        Duration::from_secs(60),
        "document.getElementById('tree').innerText",
    );
    // Hide B, which the app files under Planning.
    let hidden = eval(
        &tab,
        &format!("fetch('/api/sessions').then(r => r.json()).then(d => {{ const row = d.groups.flatMap(g => g.rows).find(r => r.id === '{B}'); return fetch('/api/ignore?add=' + encodeURIComponent(row.ignoreKey)).then(r => r.status); }})"),
    );
    assert_eq!(hidden, serde_json::json!(200), "B hidden");
    // One list by activity first, then the Claude app over it.
    click(&tab, "#filterBtn");
    click(&tab, "#sessionFilter [data-flat]");
    click(&tab, "#sessionFilter [data-arrange=\"claude-app\"]");
    eval(
        &tab,
        "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))",
    );
    let seen = wait_list(
        &tab,
        &|v| heads(v).contains(&"PLANNING".to_string()),
        "the app's groups are drawn",
    );
    assert_eq!(
        heads(&seen),
        vec!["PLANNING", "SESSIONS", "NOT IN THE CLAUDE APP"],
        "an empty Pinned is not drawn: {seen}"
    );
    assert!(
        seen["strip"]
            .as_str()
            .unwrap()
            .contains("Arranged like the Claude app"),
        "{seen}"
    );
    // B — idle eight hours and hidden — is in Planning, by the app's title; A, then C under it.
    let items = seen["items"].as_array().unwrap().clone();
    let row = |id: &str| items.iter().find(|i| i[0] == "row" && i[1] == id).cloned();
    assert_eq!(row(B).unwrap()[2], "App beta", "{seen}");
    assert_eq!(row(A).unwrap()[2], "App alpha", "{seen}");
    assert_eq!(row(C).unwrap()[3], true, "C nests under A: {seen}");
    assert_eq!(
        row_ids(&seen),
        vec![B, A, C],
        "D waits under Not in: {seen}"
    );
    // The rest opens on a click and holds D.
    click(&tab, "#tree .arranged-rest");
    let open = wait_list(
        &tab,
        &|v| row_ids(v).contains(&D.to_string()),
        "D under Not in",
    );
    assert_eq!(row_ids(&open).last().map(String::as_str), Some(D), "{open}");
    // A reload keeps the choice, and the arrangement it replaced, which ✕ goes back to.
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    wait_list(
        &tab,
        &|v| heads(v).contains(&"PLANNING".to_string()),
        "still arranged after a reload",
    );
    click(&tab, "#arrangeStrip [data-arrange-off]");
    wait_list(
        &tab,
        &|v| v["flat"] == true && heads(v).is_empty(),
        "back to one list by activity",
    );
}

/// #s387: a source whose files are absent is offered disabled, with the reason as its second line;
/// a source that stops reading after it was chosen falls back to the arrangement it replaced under
/// an amber line, and returns when it reads again. A read that fails while the files are there is
/// never a reason to disable the choice (the second review of #s387: it stayed disabled until the
/// monitor restarted): left, the source is still offered, saying what its last read met.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn an_unreadable_app_is_disabled_and_a_failing_one_falls_back() {
    let _serial = serial();
    let base = base("layout-fallback");
    let stores = Stores::new(&base);
    transcript(&stores, A, "alpha session", 60);
    let _monitor = Monitor::spawn(Kind::V1, 3212, &base, Some(&stores), false);
    let (_browser, tab) = chrome_tab();
    let url = "http://127.0.0.1:3212/?ui=app";
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    click(&tab, "#filterBtn");
    let option = "document.querySelector('#sessionFilter [data-arrange=\"claude-app\"]')";
    until(
        &tab,
        &format!("{option}.getAttribute('aria-disabled') === 'true' && /not installed|not found/.test({option}.innerText)"),
        "the Claude app is offered disabled, with its reason",
        Duration::from_secs(20),
        &format!("{option}.outerHTML"),
    );
    eval(
        &tab,
        "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))",
    );
    // The app appears: choose it.
    stores.app_session("local_a", A, "App alpha", 60, None);
    stores.app_settings(&[]);
    click(&tab, "#filterBtn");
    until(
        &tab,
        &format!("{option}.getAttribute('aria-disabled') === 'false'"),
        "offered once its files are there",
        Duration::from_secs(20),
        &format!("{option}.outerHTML"),
    );
    click(&tab, "#sessionFilter [data-arrange=\"claude-app\"]");
    eval(
        &tab,
        "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))",
    );
    wait_list(
        &tab,
        &|v| heads(v).contains(&"SESSIONS".to_string()),
        "arranged",
    );
    // Its settings go: the tree comes back under an amber line, the choice kept.
    let settings = stores.root.join("claude-desktop-config.json");
    let kept = std::fs::read(&settings).unwrap();
    std::fs::remove_file(&settings).unwrap();
    let fell = wait_list(
        &tab,
        &|v| v["warn"] == true && heads(v).is_empty(),
        "the tree, under an amber line",
    );
    assert!(
        fell["strip"]
            .as_str()
            .unwrap()
            .contains("could not be read"),
        "{fell}"
    );
    // They come back: so does the arrangement.
    std::fs::write(&settings, &kept).unwrap();
    wait_list(
        &tab,
        &|v| v["warn"] == false && heads(v).contains(&"SESSIONS".to_string()),
        "arranged again",
    );
    // The settings are there but hold no sidebar layout: the read fails, the tree comes back.
    std::fs::write(&settings, br#"{"preferences":{}}"#).unwrap();
    wait_list(
        &tab,
        &|v| v["warn"] == true && heads(v).is_empty(),
        "a failed read falls back",
    );
    // Left, the choice is still offered, saying what the read met, and chosen it reads again.
    click(&tab, "#arrangeStrip [data-arrange-off]");
    until(
        &tab,
        "document.getElementById('arrangeStrip').hidden",
        "left",
        Duration::from_secs(10),
        "document.getElementById('arrangeStrip').outerHTML",
    );
    click(&tab, "#filterBtn");
    until(
        &tab,
        &format!("{option}.getAttribute('aria-disabled') === 'false' && /Last read: .*no sidebar layout/.test({option}.innerText)"),
        "still offered, saying what its last read met",
        Duration::from_secs(20),
        &format!("{option}.outerHTML"),
    );
    std::fs::write(&settings, &kept).unwrap();
    click(&tab, "#sessionFilter [data-arrange=\"claude-app\"]");
    eval(
        &tab,
        "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }))",
    );
    wait_list(
        &tab,
        &|v| v["warn"] == false && heads(v).contains(&"SESSIONS".to_string()),
        "chosen again, it reads again",
    );
}

/// A Codex rollout whose thread id is `id`, last written `secs` ago: a prompt and an answer.
#[cfg(target_os = "macos")]
fn codex_rollout(stores: &Stores, id: &str, prompt: &str, secs: u64) -> (String, String) {
    let at = |s: u64| harness::rfc3339_secs_ago(s);
    let jsonl = format!(
        "{{\"timestamp\":\"{}\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/r\",\"originator\":\"codex-tui\",\"cli_version\":\"0.147.0\"}}}}\n\
         {{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{prompt}\"}}]}}}}\n\
         {{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"phase\":\"final_answer\",\"content\":[{{\"type\":\"output_text\",\"text\":\"Done.\"}}]}}}}\n",
        at(secs + 10),
        at(secs + 5),
        at(secs)
    );
    let stem = format!("2026-08-21T10-00-00-{id}");
    let path = stores.codex_session(&stem, &jsonl);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
    (format!("rollout-{stem}"), path.display().to_string())
}

/// Run SQL on a database through macOS's own `sqlite3`, so the case needs no SQLite crate; with
/// `-init /dev/null`, so no `~/.sqliterc` reaches it. That `sqlite3` keeps a WAL database's `-wal`
/// and `-shm` after it exits, so its log is checkpointed into the database and both are removed:
/// the database is left as an app that closed leaves it.
#[cfg(target_os = "macos")]
fn sqlite(db: &std::path::Path, sql: &str) {
    let out = std::process::Command::new("/usr/bin/sqlite3")
        .args(["-init", "/dev/null"])
        .arg(db)
        .arg(format!("{sql}\nPRAGMA wal_checkpoint(TRUNCATE);"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sqlite3 {}: {}",
        db.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    for side in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{side}", db.display()));
    }
}

/// The `-shm` and `-wal` files in a directory: a reader of a WAL database makes them unless it is
/// careful, and they stay after it closes.
#[cfg(target_os = "macos")]
fn sqlite_side_files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with("-shm") || n.ends_with("-wal"))
        .collect();
    names.sort();
    names
}

/// The second review of #s387: "Like the Codex app" draws the threads the app lists, from its
/// database and its sidebar state (built here with macOS's own `sqlite3`), under the app's own
/// titles: its project folders, then Chats. A `codex exec` run the database holds is not drawn —
/// by its source while the app's catalogue of threads is missing, which the line says, and by the
/// catalogue once there is one, which then decides alone. Reading makes no file beside either
/// database.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn arranged_like_the_codex_app_shows_the_threads_it_lists() {
    const X: &str = "5e000000-0000-4000-8000-000000000387";
    const Y: &str = "5f000000-0000-4000-8000-000000000387";
    const Z: &str = "60000000-0000-4000-8000-000000000387";
    let _serial = serial();
    let base = base("layout-codex");
    let stores = Stores::new(&base);
    let (x, x_path) = codex_rollout(&stores, X, "x prompt", 60);
    let (y, y_path) = codex_rollout(&stores, Y, "y prompt", 90);
    let (z, z_path) = codex_rollout(&stores, Z, "z prompt", 120);
    let app = stores.root.join("codex-app");
    std::fs::create_dir_all(&app).unwrap();
    sqlite(
        &app.join("state_5.sqlite"),
        &format!(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL);
             CREATE TABLE thread_sections (id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, title TEXT NOT NULL, updated_at INTEGER NOT NULL, archived INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER, recency_at_ms INTEGER NOT NULL DEFAULT 0, name TEXT, is_pinned INTEGER NOT NULL DEFAULT 0, thread_section_id TEXT, section_position INTEGER, project_id TEXT, created_at INTEGER NOT NULL DEFAULT 0, created_at_ms INTEGER, source TEXT NOT NULL);
             INSERT INTO projects VALUES ('srv-api', 'api', 0);
             INSERT INTO threads (id, rollout_path, title, updated_at, recency_at_ms, name, project_id, source) VALUES
               ('{X}', '{x_path}', 'x prompt', 1, 3000, 'App thread X', 'srv-api', 'vscode'),
               ('{Y}', '{y_path}', 'y prompt', 1, 2000, NULL, 'srv-api', 'exec'),
               ('{Z}', '{z_path}', 'z prompt', 1, 1000, 'App chat Z', NULL, 'vscode');"
        ),
    );
    let state = serde_json::json!({
        "electron-persisted-atom-state": {
            "sidebar-custom-sections-v3": { "acct": {
                "sections": [], "collapsedSectionIds": [],
                "sectionOrder": ["pinned", "threads", "chats"],
            }},
            "sidebar-collapsed-sections-v1": { "pinned": false, "threads": false, "chats": false },
            "sidebar-project-expanded-v1-codex:lp-api": true,
        },
        "pinned-thread-ids": [],
        "project-order": ["lp-api"],
        "local-projects": { "lp-api": { "id": "lp-api", "name": "api" } },
        "app-server-project-id-by-legacy-project-id-by-host": { "local:/h/.codex": { "lp-api": "srv-api" } },
        "projectless-thread-ids": [Z],
    });
    std::fs::write(
        app.join("codex-global-state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let _monitor = Monitor::spawn(Kind::V1, 3215, &base, Some(&stores), false);
    let (_browser, tab) = chrome_tab();
    let url = "http://127.0.0.1:3215/?ui=app";
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        "document.querySelectorAll('#tree .tree-row.session').length >= 3",
        "the list is drawn",
        Duration::from_secs(60),
        "document.getElementById('tree').innerText",
    );
    arrange(&tab, "codex-app");
    // No catalogue yet: the exec run is left out by its source, and the line says so.
    let seen = wait_list(
        &tab,
        &|v| heads(v).contains(&"PROJECTS".to_string()),
        "the app's sidebar is drawn",
    );
    assert_eq!(
        heads(&seen),
        vec!["PROJECTS", "CHATS", "NOT IN THE CODEX APP"],
        "{seen}"
    );
    assert_eq!(row_ids(&seen), vec![x.clone(), z.clone()], "{seen}");
    let items = seen["items"].as_array().unwrap().clone();
    let title = |id: &str| {
        items
            .iter()
            .find(|i| i[0] == "row" && i[1] == id)
            .map(|i| i[2].clone())
    };
    assert_eq!(title(&x), Some(serde_json::json!("App thread X")), "{seen}");
    assert!(
        seen["strip"].as_str().unwrap().contains("was not found"),
        "{seen}"
    );
    click(&tab, "#tree .arranged-rest");
    let open = wait_list(
        &tab,
        &|v| row_ids(v).contains(&y),
        "the exec run waits under Not in",
    );
    assert_eq!(row_ids(&open).last(), Some(&y), "{open}");
    // The catalogue appears and lists X alone: it decides, so Z goes too, and the line no longer
    // calls the list a guess.
    sqlite(
        &app.join("codex.db"),
        &format!(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE local_thread_catalog (host_id TEXT NOT NULL, thread_id TEXT NOT NULL);
             INSERT INTO local_thread_catalog VALUES ('local', '{X}');"
        ),
    );
    let listed = wait_list(
        &tab,
        &|v| {
            !heads(v).contains(&"CHATS".to_string())
                && !v["strip"].as_str().unwrap_or("").contains("was not found")
        },
        "the catalogue decides",
    );
    assert_eq!(
        heads(&listed),
        vec!["PROJECTS", "NOT IN THE CODEX APP"],
        "{listed}"
    );
    assert_eq!(row_ids(&listed).first(), Some(&x), "{listed}");
    assert!(
        sqlite_side_files(&app).is_empty(),
        "the reads made {:?}",
        sqlite_side_files(&app)
    );
}

/// #s387: "Like Herdr" draws each workspace and its tabs, a row per pane whose agent reported its
/// session, through the harness's herdr stand-in and a socket this case owns.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn arranged_like_herdr_shows_its_workspaces_and_tabs() {
    let _serial = serial();
    let base = base("layout-herdr");
    let stores = Stores::new(&base);
    transcript(&stores, A, "alpha session", 60);
    transcript(&stores, D, "delta session", 30);
    // A socket's path must be short (about 104 bytes), which the case's scratch under the
    // workspace's target is not: the server's socket lives in a directory of its own under /tmp,
    // removed when the case ends.
    struct Gone(std::path::PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let server = Gone(std::path::PathBuf::from(format!(
        "/tmp/cr-herdr-{}",
        std::process::id()
    )));
    std::fs::create_dir_all(&server.0).unwrap();
    let socket = server.0.join("herdr.sock");
    let _ = std::fs::remove_file(&socket);
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::fs::write(
        stores.root.join("herdr-sessions.json"),
        serde_json::json!({ "sessions": [{ "name": "work", "running": true, "socket_path": socket }] }).to_string(),
    )
    .unwrap();
    std::fs::write(
        stores.root.join("herdr-snapshot.json"),
        serde_json::json!({ "id": "1", "result": { "type": "snapshot", "snapshot": {
            "workspaces": [{ "workspace_id": "w1", "number": 1, "label": "api" }],
            "tabs": [{ "tab_id": "t1", "workspace_id": "w1", "number": 1, "label": "auth" }],
            "panes": [
                { "pane_id": "p1", "tab_id": "t1", "agent_session": { "source": "hook", "agent": "claude", "kind": "id", "value": A } },
                { "pane_id": "p2", "tab_id": "t1", "agent_session": null },
            ],
        }}})
        .to_string(),
    )
    .unwrap();
    let _monitor = Monitor::spawn(Kind::V1, 3213, &base, Some(&stores), false);
    let (_browser, tab) = chrome_tab();
    tab.navigate_to("http://127.0.0.1:3213/?ui=app").unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        "!!document.querySelector('#tree .tree-row.session')",
        "the list is drawn",
        Duration::from_secs(60),
        "document.getElementById('tree').innerText",
    );
    arrange(&tab, "herdr");
    let seen = wait_list(
        &tab,
        &|v| heads(v).contains(&"API".to_string()),
        "Herdr's workspace",
    );
    assert_eq!(heads(&seen), vec!["API", "NOT IN HERDR"], "{seen}");
    let groups: Vec<_> = seen["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i[0] == "group")
        .map(|i| i[1].as_str().unwrap().to_string())
        .collect();
    assert_eq!(groups, vec!["auth"], "{seen}");
    assert_eq!(row_ids(&seen), vec![A], "the shell pane is no row: {seen}");
}

/// #s387 on a phone (the review: CLAUDE.md's phone rules): upright, the section menu opens as a
/// sheet at the foot over a dim, from a 44px glyph, with 44px rows; a tap on the dim closes it and
/// reaches nothing beneath; the line's ✕ is a 44px target; Arrange's two-line rows in the filter
/// sheet hold their reason. Held sideways the menu is a popover at its glyph's side.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_phone_sorts_an_arranged_section_from_a_sheet() {
    let _serial = serial();
    let base = base("layout-phone");
    let stores = Stores::new(&base);
    world(&stores);
    let _monitor = Monitor::spawn(Kind::V1, 3214, &base, Some(&stores), false);
    let (_browser, tab) = chrome_tab();
    harness::phone(&tab, 390, 844);
    let url = "http://127.0.0.1:3214/?ui=app";
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    eval(
        &tab,
        "localStorage.setItem('am-prod-session-arrange', 'claude-app')",
    );
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    let open_drawer = |tab: &headless_chrome::Tab| {
        until(
            tab,
            "!!document.querySelector('#tree .arranged-head [data-section-view]')",
            "the arranged list is drawn",
            Duration::from_secs(60),
            "document.getElementById('tree').innerText",
        );
        eval(
            tab,
            "document.getElementById('app').classList.remove('mobile-detail')",
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    open_drawer(&tab);
    let rect = |tab: &headless_chrome::Tab, sel: &str| -> serde_json::Value {
        eval(
            tab,
            &format!("(function () {{ var r = document.querySelector({sel:?}).getBoundingClientRect(); return JSON.stringify([r.left, r.top, r.width, r.height, innerWidth, innerHeight]); }})()"),
        )
        .as_str()
        .map(|s| serde_json::from_str(s).unwrap())
        .unwrap_or_default()
    };
    let glyph = rect(&tab, "#tree .arranged-head [data-section-view]");
    assert!(
        glyph[2].as_f64().unwrap() >= 44.0 && glyph[3].as_f64().unwrap() >= 44.0,
        "the glyph is a 44px target: {glyph}"
    );
    let close = rect(&tab, "#arrangeStrip [data-arrange-off]");
    assert!(
        close[2].as_f64().unwrap() >= 44.0 && close[3].as_f64().unwrap() >= 44.0,
        "the line's ✕ is a 44px target: {close}"
    );
    let before = eval(&tab, "location.search");
    let (x, y) = (
        glyph[0].as_f64().unwrap() + glyph[2].as_f64().unwrap() / 2.0,
        glyph[1].as_f64().unwrap() + glyph[3].as_f64().unwrap() / 2.0,
    );
    harness::finger_tap(&tab, x, y);
    until(
        &tab,
        "!document.getElementById('sectionViewMenu').hidden && document.getElementById('sectionViewMenu').classList.contains('sheet')",
        "the menu opens as a sheet",
        Duration::from_secs(10),
        "document.getElementById('sectionViewMenu') && document.getElementById('sectionViewMenu').className",
    );
    let sheet = rect(&tab, "#sectionViewMenu");
    assert_eq!(
        sheet[0].as_f64().unwrap().round(),
        8.0,
        "8px from the side: {sheet}"
    );
    let bottom =
        sheet[5].as_f64().unwrap() - (sheet[1].as_f64().unwrap() + sheet[3].as_f64().unwrap());
    assert!((bottom - 8.0).abs() < 1.5, "at the foot: {sheet}");
    let row = rect(&tab, "#sectionViewMenu .scope-option");
    assert!(row[3].as_f64().unwrap() >= 44.0, "44px rows: {row}");
    // A tap on the dim closes it; nothing beneath is chosen, and the drawer stays.
    harness::finger_tap(&tab, 200.0, 60.0);
    until(
        &tab,
        "document.getElementById('sectionViewMenu').hidden",
        "the dim closes it",
        Duration::from_secs(10),
        "document.getElementById('sectionViewMenu').hidden",
    );
    assert_eq!(
        eval(&tab, "location.search"),
        before,
        "no session was chosen"
    );
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('app').classList.contains('mobile-detail')"
        ),
        serde_json::json!(false),
        "the drawer stays"
    );
    // The filter sheet's Arrange rows hold their reason (the Codex app is absent here).
    click(&tab, "#filterBtn");
    let fits = eval(
        &tab,
        "(function () { var o = document.querySelector('#sessionFilter [data-arrange=\"codex-app\"]'); var n = o.querySelector('.arrange-note'); var r = o.getBoundingClientRect(), q = n.getBoundingClientRect(); return JSON.stringify([n.innerText.length > 0, r.height >= 44, q.bottom <= r.bottom + 0.5]); })()",
    );
    assert_eq!(
        fits,
        serde_json::json!("[true,true,true]"),
        "the reason stays inside its row"
    );
    // Held sideways: a popover at the glyph's side, not a band across the screen.
    harness::phone(&tab, 844, 390);
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    open_drawer(&tab);
    let glyph = rect(&tab, "#tree .arranged-head [data-section-view]");
    harness::finger_tap(
        &tab,
        glyph[0].as_f64().unwrap() + glyph[2].as_f64().unwrap() / 2.0,
        glyph[1].as_f64().unwrap() + glyph[3].as_f64().unwrap() / 2.0,
    );
    until(
        &tab,
        "!document.getElementById('sectionViewMenu').hidden",
        "the menu opens",
        Duration::from_secs(10),
        "document.getElementById('sectionViewMenu').className",
    );
    let pop = rect(&tab, "#sectionViewMenu");
    assert_eq!(
        eval(
            &tab,
            "document.getElementById('sectionViewMenu').classList.contains('sheet')"
        ),
        serde_json::json!(false),
        "no sheet held sideways"
    );
    assert!(
        pop[2].as_f64().unwrap() < 400.0,
        "a popover, not a band: {pop}"
    );
}
