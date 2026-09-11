//! Scenarios that run against BOTH pages — the classic page (`export.js` on the html server,
//! the reference) and the monitor's app shell — through one vocabulary (`harness::Surface`
//! and its probes), on hermetic fixtures, with live growth where the bug bit (#53).
//!
//! Every case is `#[ignore]`d: `cargo test -p claude-replay-browser-tests -- --ignored`, which
//! needs a local Chrome and `cargo build --release -p claude-monitor-v2`. A case whose result on
//! a surface is a QUEUED bug carries `known_red` in its name and the number of the task that
//! owns it; the gate runs with `--skip known_red` and the fix removes the marker — the case is
//! the bug's repro and is never weakened. The classic page is the reference for the app shell,
//! not an oracle: a scenario can find a classic-page bug too (#71 did).
//!
//! Adding a scenario: write it once as `fn scenario_x(tab, surface, fixture)`, then one
//! `#[test]` per surface that opens the fixture on that surface and calls it. The classic
//! test is the reference; the app-shell test is held to the same assertions.

mod harness;

use claude_replay_html::start_server;
use claude_replay_present::Args;
use harness::{
    agent_spawn, assistant_at, at_tail, base, click_session_id, codex_tool_session, command_at,
    copied_text, drag_select, edit_tool_at, eval, image_result_at, jump_to_end, key,
    last_mounted_turn, long_session, named_tool_at, now_minus, open_last_fold, open_turn_session,
    probe, queued_at, queued_text, read_tool_at, scroll_by, selection_text, serial,
    session_id_chip, stub_clipboard, tap_console, thinking_at, tool_open_at, tool_result_at,
    tool_result_lines, tool_result_text, turn_at_top, until, user_at, view_anchor_index,
    write_tool_at, Kind, LiveGrowth, Monitor, Shape, Stores, Surface,
};
use std::path::PathBuf;
use std::time::Duration;

const SID: &str = "eeeeeeee-0000-4000-8000-000000000001";

/// A long fixture session on disk, plus where it lives for each surface.
struct Fixture {
    base: PathBuf,
    path: PathBuf,
    turns: u32,
}

fn fixture(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, &long_session(turns, Shape::default()));
    Fixture { base, path, turns }
}

/// The id the monitor addresses the fixture by: its file stem, for a Claude session and a Codex
/// rollout alike (the rail lists `rollout-<id>`, not the session_meta id).
fn sid_of(fx: &Fixture) -> String {
    fx.path.file_stem().unwrap().to_string_lossy().to_string()
}

/// A fixture whose session launched a workflow run: the `Workflow` call that names the run, the
/// run's journal (one member finished and titled by its result, one still running), and a real
/// session for each member so the roster's links resolve on both surfaces.
fn fixture_workflow(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    // Long enough for the classic page's readiness gate (three viewports), so the launching
    // call sits at the tail — where a scenario lands first.
    let mut transcript = long_session(20, Shape::default());
    transcript += &user_at("question 20: fan the work out", &now_minus(90));
    transcript += &harness::workflow_call_at("wf1", RUN, &now_minus(88));
    transcript += &assistant_at("answer 20: two agents are on it", &now_minus(80));
    let path = stores.claude_session(SID, &transcript);
    stores.claude_workflow_run(
        SID,
        RUN,
        &[(MEMBER_DONE, "Reviewed the parser"), (MEMBER_RUNNING, "")],
    );
    for member in [MEMBER_DONE, MEMBER_RUNNING] {
        stores.claude_session(member, &long_session(2, Shape::default()));
    }
    Fixture {
        base,
        path,
        turns: 21,
    }
}

const RUN: &str = "wf_run_119";
const MEMBER_DONE: &str = "dddddddd-0000-4000-8000-000000000001";
const MEMBER_RUNNING: &str = "dddddddd-0000-4000-8000-000000000002";

const CODEX_SID: &str = "s117";

/// A Codex fixture (harness `codex_tool_session`): the heads whose chips carry an exit code, a
/// duration and a status word, which Claude's own format never records.
fn fixture_codex(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.codex_session(CODEX_SID, &codex_tool_session(CODEX_SID, turns));
    Fixture { base, path, turns }
}

/// A Claude fixture whose tail carries sub-agent SPAWNS with no result: the launch event, whose
/// chip reads `launched` whatever the spawn's status (present.rs `spawn_chip` — the terminal verb
/// arrives on a separate completion record). A closed session is full of them.
fn fixture_spawns(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    for i in 0..3u32 {
        jsonl += &agent_spawn(&format!("spawn-{i}"), "explore", 900 + i);
    }
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

/// A fixture whose tail carries a BARE tool result — a `tool_result` with no `tool_use` before
/// it, which the engine keeps as its own `ToolResult` block (#122).
fn fixture_bare_result(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    jsonl += &harness::tool_result_text(
        "orphan-1",
        "checked 42 files and found the one that matters, a very long first line that runs past seventy characters\\nsecond line\\nthird line",
        "2026-08-21T10:15:01Z",
    );
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

/// A fixture whose tail carries two questions an agent asked through its own client (#121):
/// one still waiting, one answered.
fn fixture_input_requests(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    jsonl +=
        &harness::input_request_at("ask-1", "Which shell should stay?", "2026-08-21T10:15:01Z");
    jsonl += &harness::input_request_at("ask-2", "Ship the release now?", "2026-08-21T10:15:02Z");
    jsonl +=
        &harness::input_request_answer("ask-2", "ship", "Yes, ship it", "2026-08-21T10:15:03Z");
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

/// The surface, opened on the fixture: the html server for the classic page (in-process, one
/// root), a paired v2 monitor for the app shell. Returns the tab and what keeps the page alive.
struct Opened {
    tab: std::sync::Arc<headless_chrome::Tab>,
    _browser: headless_chrome::Browser,
    _server: Option<claude_replay_html::LiveServer>,
    monitor: Option<Monitor>,
}

fn open(surface: Surface, fx: &Fixture, port: u16) -> Opened {
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    match surface {
        Surface::Classic => {
            std::env::set_var("CLAUDE_REPLAY_CACHE", &fx.base);
            // The html server runs IN this process, so the stores it reads are this process's
            // env — a monitor gets them as spawn env instead (#125 needed the task store).
            for (key, value) in (Stores {
                root: fx.base.join("stores"),
            })
            .envs()
            {
                std::env::set_var(key, value);
            }
            let args = Args {
                no_cache: true,
                ..Default::default()
            };
            let server =
                start_server(&args, std::slice::from_ref(&fx.path)).expect("server starts");
            let url = server.url_for_root(0).expect("hosted");
            tab.navigate_to(&url).unwrap();
            tab.wait_until_navigated().unwrap();
            // The classic page windows its DOM too (#50): ready means tall enough to scroll
            // and a few turns mounted, not every turn in the DOM.
            harness::until(
                &tab,
                "document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3",
                "the classic page to render the fixture",
                Duration::from_secs(30),
                "document.querySelectorAll('#stream [data-turn]').length",
            );
            Opened {
                tab,
                _browser: browser,
                _server: Some(server),
                monitor: None,
            }
        }
        Surface::AppShell => {
            let stores = Stores {
                root: fx.base.join("stores"),
            };
            let monitor = Monitor::spawn(Kind::V2, port, &fx.base, Some(&stores), true);
            monitor.pair(&tab);
            monitor.open(&tab, &format!("?ui=app&session={}", sid_of(fx)));
            harness::until(
                &tab,
                "document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3",
                "the app shell to mount the fixture",
                Duration::from_secs(30),
                "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : 'no window'",
            );
            Opened {
                tab,
                _browser: browser,
                _server: None,
                monitor: Some(monitor),
            }
        }
    }
}

/// Both surfaces on ONE v2 monitor, so a server restart can be driven: the classic page is the
/// splice (`?ui=classic&session=`, the same export.js DOM in the document), the app shell is
/// `?ui=app&session=`. The monitor is owned by the returned page and can be respawned.
fn open_on_v2(surface: Surface, fx: &Fixture, port: u16) -> Opened {
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    let stores = Stores {
        root: fx.base.join("stores"),
    };
    let monitor = Monitor::spawn(Kind::V2, port, &fx.base, Some(&stores), true);
    monitor.pair(&tab);
    let (query, ready, diag) = match surface {
        Surface::Classic => (format!("?ui=classic&session={}", sid_of(fx)), "document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3", "document.querySelectorAll('#stream [data-turn]').length"),
        Surface::AppShell => (format!("?ui=app&session={}", sid_of(fx)), "document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3", "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : 'no window'"),
    };
    monitor.open(&tab, &query);
    harness::until(
        &tab,
        ready,
        "the page to render the fixture",
        Duration::from_secs(30),
        diag,
    );
    Opened {
        tab,
        _browser: browser,
        _server: None,
        monitor: Some(monitor),
    }
}

/// Kill the page's monitor and start a new one on the same port over the same state and
/// stores — a server restart under a watching page.
fn restart_monitor(page: &mut Opened, fx: &Fixture, port: u16) {
    let stores = Stores {
        root: fx.base.join("stores"),
    };
    page.monitor = None; // reaped
    std::thread::sleep(Duration::from_millis(800));
    page.monitor = Some(Monitor::spawn(
        Kind::V2,
        port,
        &fx.base,
        Some(&stores),
        true,
    ));
}

fn settle() {
    std::thread::sleep(Duration::from_millis(700));
}

/// Wait for the scroller to sit at its tail (a jump may scroll smoothly), or fail saying so.
fn await_tail(tab: &headless_chrome::Tab, surface: Surface, what: &str) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        if at_tail(tab, surface) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "timed out waiting for {what} (top turn {})",
        turn_at_top(tab, surface)
    );
}

// ── scenario: a fold toggled near the end, then a scroll back (#51) ─────────────────────────

/// Jump to the end, open the last fold there, scroll back up a few screens: the turn at the
/// top must walk back a few turns at a time — never leap to the beginning. On a 120-turn
/// fixture the reader is within the last 30 turns throughout.
fn scenario_fold_toggle_near_the_end(tab: &headless_chrome::Tab, surface: Surface, fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the jump to land at the tail");
    let opened = open_last_fold(tab, surface);
    // -1 = no fold header mounted; -2 = found, but it sits outside any turn (a tool fold on the
    // classic page carries no turn of its own). Either way the click happened when found.
    assert!(
        opened != -1,
        "a fold header near the end was found ({opened})"
    );
    settle();
    let mut previous = turn_at_top(tab, surface);
    assert!(previous >= 0, "a turn is at the top after the toggle");
    for step in 0..4 {
        scroll_by(tab, surface, -600);
        settle();
        let now = turn_at_top(tab, surface);
        assert!(
            now >= 0 && now <= previous && now as u32 + 30 >= fx.turns,
            "step {step}: the reader stays near the end, walking back a few turns at a time: {previous} -> {now} (of {})",
            fx.turns
        );
        previous = now;
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_the_viewport_when_a_fold_toggles_near_the_end() {
    let _serial = serial();
    let fx = fixture("scenario-fold-classic", 400);
    let page = open(Surface::Classic, &fx, 0);
    scenario_fold_toggle_near_the_end(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_the_viewport_when_a_fold_toggles_near_the_end() {
    let _serial = serial();
    let fx = fixture("scenario-fold-app", 400);
    let page = open(Surface::AppShell, &fx, 2851);
    scenario_fold_toggle_near_the_end(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the tail pin holds through growth; an unpinned reader is not moved ────────────

/// The growth script: four live turns, timestamps a minute back from now.
fn growth_script() -> Vec<String> {
    (0..4)
        .flat_map(|k| {
            vec![
                user_at(
                    &format!("live question {k}: {}", "keep going. ".repeat(8)),
                    &now_minus(60 - k * 10),
                ),
                assistant_at(
                    &format!(
                        "live answer {k}: {}",
                        "streamed prose for the tail. ".repeat(12)
                    ),
                    &now_minus(55 - k * 10),
                ),
            ]
        })
        .collect()
}

/// A fresh open lands pinned at the tail and STAYS there while the transcript grows.
fn scenario_follows_the_tail_when_pinned(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    let last_before = last_mounted_turn(tab, surface);
    let growth = LiveGrowth::start(
        fx.path.clone(),
        growth_script(),
        Duration::from_millis(2600),
    );
    assert_eq!(
        growth.finish(Duration::from_secs(40)),
        8,
        "the driver appended the whole script"
    );
    // > every consumer's poll: the last apply lands.
    std::thread::sleep(Duration::from_millis(4000));
    let last_after = last_mounted_turn(tab, surface);
    assert!(
        last_after > last_before,
        "the growth reached the page: last mounted turn {last_before} -> {last_after}"
    );
    assert!(
        at_tail(tab, surface),
        "pinned: the tail followed the growth (last turn {last_before} -> {last_after})"
    );
}

/// A reader who scrolled up is NOT moved by growth, and is not re-pinned.
fn scenario_holds_when_unpinned(tab: &headless_chrome::Tab, surface: Surface, fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    let last = turn_at_top(tab, surface);
    scroll_by(tab, surface, -900);
    scroll_by(tab, surface, -900);
    settle();
    let held = turn_at_top(tab, surface);
    assert!(
        held >= 0 && held < last.max(1),
        "the reader scrolled up: top turn {last} -> {held}"
    );
    let growth = LiveGrowth::start(
        fx.path.clone(),
        growth_script(),
        Duration::from_millis(2600),
    );
    assert_eq!(
        growth.finish(Duration::from_secs(40)),
        8,
        "the driver appended the whole script"
    );
    std::thread::sleep(Duration::from_millis(4000));
    let after = turn_at_top(tab, surface);
    assert_eq!(
        after, held,
        "unpinned: growth did not move the reader ({held} -> {after})"
    );
    assert!(!at_tail(tab, surface), "…and did not re-pin");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_follows_the_tail_when_pinned() {
    let _serial = serial();
    let fx = fixture("scenario-pinned-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_follows_the_tail_when_pinned(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_follows_the_tail_when_pinned() {
    let _serial = serial();
    let fx = fixture("scenario-pinned-app", 40);
    let page = open(Surface::AppShell, &fx, 2852);
    scenario_follows_the_tail_when_pinned(&page.tab, Surface::AppShell, &fx);
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_when_unpinned_through_growth() {
    let _serial = serial();
    let fx = fixture("scenario-unpinned-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_holds_when_unpinned(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_when_unpinned_through_growth() {
    let _serial = serial();
    let fx = fixture("scenario-unpinned-app", 40);
    let page = open(Surface::AppShell, &fx, 2858);
    scenario_holds_when_unpinned(&page.tab, Surface::AppShell, &fx);
}

/// A fixture whose tail holds a slash command whose output carries terminal styling — the dim
/// pair Claude Code really writes around `/compact`'s line (#130).
fn fixture_styled_command(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(14, Shape::default());
    // `\u001b` in the JSON, so the fixture carries a real escape byte the way a transcript does.
    jsonl += &command_at(
        "compact",
        "",
        "\\u001b[2mCompacted (ctrl+o to see full summary) \\u001b[22m",
        "2026-08-21T10:15:01Z",
    );
    let path = stores.claude_session(SID, &jsonl);
    Fixture {
        base,
        path,
        turns: 15,
    }
}

/// A command's output reads as output (#130, the owner's report): the terminal's own styling is
/// gone — no `[2m` in the page — and the body wears the same ⎿ result shape as every other
/// output rather than a bordered code block of its own.
fn scenario_a_command_output_is_plain_output(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let open = match surface {
        Surface::Classic => "(function(){ var c = [...document.querySelectorAll('#stream .fold')].find(function (f) { var n = f.querySelector(':scope > .fold-h > .tool-name'); return n && /compact/.test(f.textContent); }); if (c && c.dataset.open !== '1') c.querySelector(':scope > .fold-h').click(); return 'ok'; })()",
        Surface::AppShell => "(function(){ var b = [...document.querySelectorAll('[data-prompt-toggle]')].find(function (e) { return /compact/.test(e.textContent); }); if (b && b.getAttribute('aria-expanded') === 'false') b.click(); return 'ok'; })()",
    };
    eval(tab, open);
    settle();
    settle();
    let seen = probe(
        tab,
        match surface {
            Surface::Classic => "(function(){ var c = [...document.querySelectorAll('#stream .fold, #stream .blk')].find(function (f) { return /Compacted \\(ctrl/.test(f.textContent); }); if (!c) return null; return { text: c.textContent, marks: c.querySelectorAll('.result > .lead').length }; })()",
            Surface::AppShell => "(function(){ var c = [...document.querySelectorAll('.turn.command, .renderer')].find(function (f) { return /Compacted \\(ctrl/.test(f.textContent); }); if (!c) return null; return { text: c.textContent, marks: c.querySelectorAll('.renderer-result > .renderer-result-lead').length }; })()",
        },
    );
    assert!(!seen.is_null(), "the command's output is on the page");
    let text = seen["text"].as_str().unwrap_or("");
    assert!(
        text.contains("Compacted (ctrl+o to see full summary)"),
        "the sentence survives: {text:?}"
    );
    assert!(
        !text.contains("[2m") && !text.contains("[22m"),
        "…and the terminal's styling does not reach the reader: {text:?}"
    );
    assert!(
        seen["marks"].as_i64().unwrap_or(0) >= 1,
        "the output wears the same result body as any other output: {seen:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_command_output_is_plain_output() {
    let _serial = serial();
    let fx = fixture_styled_command("scenario-ansi-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_command_output_is_plain_output(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_command_output_is_plain_output() {
    let _serial = serial();
    let fx = fixture_styled_command("scenario-ansi-app");
    let page = open(Surface::AppShell, &fx, 2904);
    scenario_a_command_output_is_plain_output(&page.tab, Surface::AppShell, &fx);
}

/// A transcript keeps growing while a tool filter is on (#126). The filter's hit set was
/// computed once, so anything that arrived afterwards was in no set — and the paint hid every
/// one of them: on a live session, the transcript silently stopped.
fn scenario_the_filter_takes_in_what_arrives(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (select, visible_bash) = match surface {
        Surface::Classic => (
            "(function(){ var b = document.getElementById('btn-tools'); if (b) b.click(); var it = document.querySelector('.tool-item[data-label=\"Bash\"]'); if (!it) return 'no item'; it.click(); return 'selected'; })()",
            "[...document.querySelectorAll('#stream .fold[data-tool=\"Bash\"]')].filter(function (f) { return f.getBoundingClientRect().height > 0; }).length",
        ),
        Surface::AppShell => (
            "(function(){ var it = document.querySelector('.tool-type-option[data-tool-filter=\"Bash\"]'); if (!it) { document.getElementById('filterTranscriptBtn').click(); it = document.querySelector('.tool-type-option[data-tool-filter=\"Bash\"]'); } if (!it) return 'no item'; it.click(); return 'selected'; })()",
            "[...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"]')].filter(function (t) { return t.getBoundingClientRect().height > 0; }).length",
        ),
    };
    eval(tab, select);
    settle();
    settle();
    let before = eval(tab, visible_bash).as_i64().unwrap_or(0);
    assert!(before > 0, "the filter shows the Bash calls it already had");
    // Two more Bash calls arrive while the filter is on.
    let script = vec![
        assistant_at("running one more check", &now_minus(30)),
        tool_open_at("late-1", &now_minus(29)),
        tool_result_at("late-1", &now_minus(28)),
        tool_open_at("late-2", &now_minus(20)),
        tool_result_at("late-2", &now_minus(19)),
    ];
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(1200));
    assert_eq!(
        growth.finish(Duration::from_secs(40)),
        5,
        "the driver appended"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut after = before;
    while std::time::Instant::now() < deadline {
        after = eval(tab, visible_bash).as_i64().unwrap_or(0);
        if after > before {
            break;
        }
        settle();
    }
    // The transcript keeps growing under the filter — the defect was that it stopped — and
    // nothing that answers the filter is hidden BY it. (How many of the new calls are mounted
    // at once is the window's business, not the filter's.)
    assert!(
        after > before,
        "the calls that arrived under the filter reach the page ({before} -> {after})"
    );
    let hidden = eval(
        tab,
        match surface {
            Surface::Classic => "document.querySelectorAll('#stream .fold[data-tool=\"Bash\"].filter-hidden').length",
            Surface::AppShell => "document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"].filter-hidden, .renderer-turn[data-tool-name=\"Bash\"] .filter-hidden').length",
        },
    )
    .as_i64()
    .unwrap_or(-1);
    assert_eq!(hidden, 0, "…and the filter hides none of them");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_filter_takes_in_what_arrives() {
    let _serial = serial();
    let fx = fixture("scenario-livefilter-classic", 14);
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_filter_takes_in_what_arrives(&page.tab, Surface::Classic, &fx);
}

// The app shell has no equivalent of this case, and needs none: it hides nothing (#133), so a
// call that arrives can never be hidden by the filter. What must hold there — an arriving call
// joins the filter's count and its fold opens — is asserted in
// `app_shell_the_tool_filter_is_a_search_by_kind`. Counting VISIBLE rows would measure the
// window, not the filter: with nothing cut, how much of the tail is mounted is the window's
// business.

/// Rule 7's hysteresis (#127): acquiring the pin needs the true end, KEEPING it only the old
/// slack. A reader who nudges the view a few pixels is still reading the tail and must keep it;
/// a real scroll away lets go.
fn scenario_a_nudge_keeps_the_tail(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    assert!(at_tail(tab, surface), "following the tail to begin with");
    let following = |tab: &headless_chrome::Tab| {
        match surface {
            Surface::Classic => eval(tab, "document.body.classList.contains('following')")
                .as_bool()
                .unwrap_or(false),
            // The jump control is the page's own statement of it: shown exactly when NOT following.
            Surface::AppShell => eval(
                tab,
                "document.getElementById('jumpToBottom').getAttribute('aria-hidden') === 'true'",
            )
            .as_bool()
            .unwrap_or(false),
        }
    };
    assert!(following(tab), "…and the page says so");
    // A nudge — less than the hold slack — is still reading the tail.
    scroll_by(tab, surface, -40);
    settle();
    settle();
    assert!(
        following(tab),
        "a 40px nudge keeps the tail: the pin holds through the old slack"
    );
    // A real scroll away lets go.
    scroll_by(tab, surface, -1200);
    settle();
    settle();
    assert!(
        !following(tab),
        "…and a scroll away from the tail unpins, as it always did"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_nudge_keeps_the_tail() {
    let _serial = serial();
    let fx = fixture("scenario-nudge-classic", 20);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_nudge_keeps_the_tail(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_nudge_keeps_the_tail() {
    let _serial = serial();
    let fx = fixture("scenario-nudge-app", 20);
    let page = open(Surface::AppShell, &fx, 2906);
    scenario_a_nudge_keeps_the_tail(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: stepping and paging from the top ──────────────────────────────────────────────

/// From the top, `]` three times lands on turn 3 or later and each press moves forward; a
/// page down then moves forward again.
fn scenario_step_and_page(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    scroll_by(tab, surface, -1_000_000);
    settle();
    let start = turn_at_top(tab, surface);
    assert!(start >= 0, "a turn is at the top");
    // From the very top the first `]` may only LAND the first turn on the page's landing line
    // (the classic page's rule); after that every press moves forward. Never backward.
    let mut seen = vec![start];
    for _ in 1..=3 {
        key(tab, "]", false);
        settle();
        seen.push(turn_at_top(tab, surface));
    }
    let forward = seen.windows(2).filter(|w| w[1] > w[0]).count();
    assert!(
        seen.windows(2).all(|w| w[1] >= w[0]) && forward >= 2 && seen[3] >= start + 2,
        "three `]` presses walk forward from the top: {seen:?}"
    );
    let previous = seen[3];
    key(tab, " ", false);
    settle();
    // The classic page scrolls natively on Space; the app shell pages through its action —
    // either way the reader moved forward, or (a short page) stayed.
    let paged = turn_at_top(tab, surface);
    assert!(
        paged >= previous,
        "a page down never moves backward: {previous} -> {paged}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_steps_turns_from_the_top() {
    let _serial = serial();
    let fx = fixture("scenario-step-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_step_and_page(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_steps_turns_from_the_top() {
    let _serial = serial();
    let fx = fixture("scenario-step-app", 40);
    let page = open(Surface::AppShell, &fx, 2853);
    scenario_step_and_page(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a growth above the reader is corrected BEFORE the frame paints (#132) ─────────

/// A mounted row above the viewport grows on its own — an image decoding, a late reflow, a
/// height learned — and the reader must see NOTHING. The pixel-hold scenarios sample every
/// 250ms and cannot tell a one-frame jolt from a clean hold, so this one reads the anchor's
/// position at the one moment that decides it: inside the ResizeObserver delivery for the
/// grown element, AFTER the page's own observer has run (observers are invoked in creation
/// order, and the page's is older) and BEFORE the frame paints. A page that measures and
/// restores inside its observer reads the anchor back in place there; a page that defers the
/// repair past the frame reads it displaced by the growth — and that displaced frame is what
/// the reader saw. The classic page restores inside its body observer (`restoreAnchor`); the
/// app shell deferred with `setTimeout(0)` until #132.
fn scenario_growth_above_is_corrected_before_paint(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    for _ in 0..3 {
        scroll_by(tab, surface, -700);
    }
    settle();
    settle();
    let (held_key, _) = harness::view_anchor(tab, surface);
    assert!(
        !held_key.is_empty() && !at_tail(tab, surface),
        "the reader scrolled up (looking at {held_key})"
    );
    // Arm: the first visible element is the anchor; the last mounted element ENTIRELY above the
    // viewport is what will grow. A second observer on it reads the anchor's offset in the same
    // delivery as the page's own.
    let (items, top) = match surface {
        Surface::Classic => ("document.querySelectorAll('#stream [data-idx]')", "0"),
        Surface::AppShell => (
            "document.querySelector('.virtual-window').children",
            "document.querySelector('.transcript').getBoundingClientRect().top",
        ),
    };
    let armed = probe(
        tab,
        &format!("(function(){{ var top = {top}; var els = [...{items}]; var anchor = null, above = null; for (var e of els) {{ var r = e.getBoundingClientRect(); if (r.bottom < top - 10) above = e; else if (!anchor && r.bottom > top) anchor = e; }} if (!anchor || !above) return {{ ok: false, els: els.length }}; var base = anchor.getBoundingClientRect().top - top; window.__jolt = {{ base: base, atObserver: null, deliveries: [] }}; new ResizeObserver(function () {{ var offset = anchor.getBoundingClientRect().top - ({top}); var grown = !!above.querySelector('[data-jolt-spacer]'); window.__jolt.deliveries.push({{ offset: offset, grown: grown }}); if (grown && window.__jolt.atObserver == null) window.__jolt.atObserver = offset; }}).observe(above); window.__joltAbove = above; window.__joltAnchor = anchor; return {{ ok: true, base: base, aboveBottom: above.getBoundingClientRect().top + above.getBoundingClientRect().height - top }}; }})()"),
    );
    assert_eq!(
        armed["ok"], true,
        "a mounted element above the viewport and an anchor to hold: {armed}"
    );
    let base = armed["base"].as_f64().unwrap();
    // Grow it, synchronously, by more than any tolerance: a 300px block appended inside it.
    eval(tab, "(function(){ var s = document.createElement('div'); s.style.height = '300px'; s.setAttribute('data-jolt-spacer', ''); window.__joltAbove.appendChild(s); return 'ok'; })()");
    until(
        tab,
        "window.__jolt && window.__jolt.atObserver != null",
        "the observer delivery for the grown element",
        Duration::from_secs(5),
        "JSON.stringify(window.__jolt)",
    );
    let at_observer = eval(tab, "window.__jolt.atObserver").as_f64().unwrap();
    // …and afterwards the hold is complete, as the coarse scenarios already require.
    settle();
    let after = eval(
        tab,
        &format!("window.__joltAnchor.getBoundingClientRect().top - ({top})"),
    )
    .as_f64()
    .unwrap();
    assert!(
        (after - base).abs() <= 1.0,
        "after the growth settled the anchor is back where it was: {base:.1} -> {after:.1}"
    );
    let deliveries = eval(tab, "JSON.stringify(window.__jolt.deliveries)");
    assert!(
        (at_observer - base).abs() <= 1.0,
        "inside the observer delivery — before the frame painted — the anchor was already back in place: {base:.1} -> {at_observer:.1} (a displaced value here is the frame the reader saw); deliveries {deliveries}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_corrects_a_growth_above_before_paint() {
    let _serial = serial();
    let fx = fixture("scenario-jolt-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_growth_above_is_corrected_before_paint(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_corrects_a_growth_above_before_paint() {
    let _serial = serial();
    let fx = fixture("scenario-jolt-app", 40);
    let page = open(Surface::AppShell, &fx, 2909);
    scenario_growth_above_is_corrected_before_paint(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the reader's own motion is never fought (#132 step 3) ─────────────────────────

/// While the reader is moving the view — a wheel still travelling as a fling, a thumb held —
/// the page must not write `scrollTop` underneath them: the fling stutters or dies, the thumb
/// jumps under the pointer. A correction owed during that window is paid at its end instead, so
/// nothing is lost. The probe counts the writes the page makes to its own scroller.
fn scenario_the_readers_motion_is_never_fought(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    for _ in 0..3 {
        scroll_by(tab, surface, -700);
    }
    settle();
    // Count every scroll the PAGE performs, on either surface's scroller.
    let install = match surface {
        Surface::Classic => "(function(){ window.__writes = 0; var el = document.scrollingElement; ['scrollTo','scrollBy'].forEach(function (m) { var f = window[m].bind(window); window[m] = function () { window.__writes++; return f.apply(null, arguments); }; }); var d = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop'); Object.defineProperty(el, 'scrollTop', { get: function () { return d.get.call(el); }, set: function (v) { window.__writes++; d.set.call(el, v); } }); return 'ok'; })()",
        Surface::AppShell => "(function(){ window.__writes = 0; var el = document.querySelector('.transcript'); var d = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop'); Object.defineProperty(el, 'scrollTop', { get: function () { return d.get.call(el); }, set: function (v) { window.__writes++; d.set.call(el, v); } }); return 'ok'; })()",
    };
    eval(tab, install);
    // The reader is mid-fling: a wheel, and then growth arriving inside the intent window.
    let wheel = match surface {
        Surface::Classic => "window",
        Surface::AppShell => "document.querySelector('.transcript')",
    };
    let growth = LiveGrowth::start(
        fx.path.clone(),
        harness::open_turn_growth(40, 2),
        Duration::from_millis(300),
    );
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_millis(2600) {
        eval(
            tab,
            &format!("(function(){{ {wheel}.dispatchEvent(new WheelEvent('wheel', {{ deltaY: -40, bubbles: true }})); return 'ok'; }})()"),
        );
        std::thread::sleep(Duration::from_millis(80));
    }
    let during = eval(tab, "window.__writes").as_i64().unwrap_or(-1);
    growth.finish(Duration::from_secs(10));
    assert_eq!(
        during, 0,
        "while the reader's own motion is in flight the page wrote the scroll offset {during} time(s) — a fling fights every one of them"
    );
    // …and the correction was not dropped: once the motion stops the reader is holding the same
    // record, which is what the owed correction pays for.
    settle();
    settle();
    let (key, _) = harness::view_anchor(tab, surface);
    assert!(
        !key.is_empty(),
        "after the motion stopped the reader is on a record"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_never_fights_the_readers_motion() {
    let _serial = serial();
    let fx = fixture_open_turn("scenario-fling-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_readers_motion_is_never_fought(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_never_fights_the_readers_motion() {
    let _serial = serial();
    let fx = fixture_open_turn("scenario-fling-app");
    let page = open(Surface::AppShell, &fx, 2910);
    scenario_the_readers_motion_is_never_fought(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: wide mode drops the reading-width cap (#136) ──────────────────────────────────

/// The reading measure serves prose; a diff-heavy session wants the window. Both pages offer a
/// wide switch, and on both it must actually widen the transcript — and be remembered. (The app
/// shell had the switch, the class and the remembered preference, and no rule that read the
/// class: the reader flipped it and the page did not move.)
fn scenario_wide_mode_drops_the_reading_cap(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // Wide enough that the cap is what limits the column — under it, `min()` already returns the
    // window and the switch is rightly a no-op.
    harness::resize(tab, 1800.0, 900.0);
    // set_bounds is asynchronous: measuring before the window has actually grown reads the
    // WINDOW widening, not the cap dropping — which passes on a page that has no wide mode.
    until(
        tab,
        "innerWidth >= 1700",
        "the window to reach its wide size",
        Duration::from_secs(10),
        "innerWidth",
    );
    settle();
    let column = match surface {
        Surface::Classic => "document.getElementById('main').getBoundingClientRect().width",
        Surface::AppShell => {
            "document.querySelector('.transcript-inner').getBoundingClientRect().width"
        }
    };
    let toggle = match surface {
        Surface::Classic => "(function(){ document.getElementById('btn-wide').click(); return 'ok'; })()",
        Surface::AppShell => "(function(){ var t = document.querySelector('[data-reading-toggle=\"wide\"]'); if (!t) { document.getElementById('filterTranscriptBtn').click(); t = document.querySelector('[data-reading-toggle=\"wide\"]'); } if (!t) return 'no switch'; t.click(); return 'ok'; })()",
    };
    let narrow = eval(tab, column).as_f64().unwrap_or(0.0);
    assert!(
        narrow > 100.0 && narrow < 1100.0,
        "the transcript starts at its reading measure: {narrow}px in an 1800px window"
    );
    assert_eq!(eval(tab, toggle), "ok", "the wide switch is reachable");
    settle();
    let wide = eval(tab, column).as_f64().unwrap_or(0.0);
    assert!(
        wide > narrow + 100.0,
        "wide mode drops the cap: {narrow}px -> {wide}px"
    );
    // Remembered: the preference is the reader's, not the page's.
    tab.reload(false, None).expect("reload");
    tab.wait_until_navigated().expect("reloaded");
    settle();
    settle();
    settle();
    let after = eval(tab, column).as_f64().unwrap_or(0.0);
    assert!(
        (after - wide).abs() <= 8.0,
        "the choice survives a reload: {wide}px -> {after}px"
    );
    // …and off again puts the measure back.
    assert_eq!(eval(tab, toggle), "ok", "the switch is reachable again");
    settle();
    let back = eval(tab, column).as_f64().unwrap_or(0.0);
    assert!(
        (back - narrow).abs() <= 8.0,
        "turning it off restores the reading measure: {back}px, was {narrow}px"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_wide_mode_drops_the_reading_cap() {
    let _serial = serial();
    let fx = fixture("scenario-wide-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_wide_mode_drops_the_reading_cap(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_wide_mode_drops_the_reading_cap() {
    let _serial = serial();
    let fx = fixture("scenario-wide-app", 12);
    let page = open(Surface::AppShell, &fx, 2911);
    scenario_wide_mode_drops_the_reading_cap(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: folding near the tail does not take the scroll away (#138) ────────────────────

/// The owner's repro: cycle a command block's fold state a few times near the bottom, scroll to
/// the tail, then scroll up — and the page would not stay where it was put. A fold is a CLICK,
/// and a click is user intent, so the correction the fold owed was postponed (#132 step 3) and
/// then paid a third of a second after the reader had scrolled away, dragging them back to
/// where the fold had been. It read as a transcript that could not be scrolled, and it freed
/// itself the moment a new record arrived and reconciled the debt to a no-op.
fn scenario_folding_near_the_tail_keeps_the_scroll(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Cycle the last fold's state, the way a reader reads a command and closes it again — with a
    // real pointer gesture, since `element.click()` fires no `pointerdown` and the engine reads
    // intent from the pointer, not from the click. Folding without that is not what a reader
    // does, and the defect only exists for someone who used a pointer.
    let head = match surface {
        Surface::Classic => ".fold-h",
        Surface::AppShell => "button.renderer-head",
    };
    let fold = format!("(function(){{ var hs = document.querySelectorAll('{head}'); if (!hs.length) return 'none'; var h = hs[hs.length - 1]; var r = h.getBoundingClientRect(); var at = {{ bubbles: true, cancelable: true, clientX: r.left + 8, clientY: r.top + 8, pointerId: 1, isPrimary: true }}; h.dispatchEvent(new PointerEvent('pointerdown', at)); h.dispatchEvent(new PointerEvent('pointerup', at)); h.click(); return 'ok'; }})()");
    for _ in 0..4 {
        eval(tab, &fold);
        std::thread::sleep(Duration::from_millis(250));
    }
    settle();
    jump_to_end(tab, surface);
    settle();
    // Now scroll up, and stay there.
    for _ in 0..3 {
        scroll_by(tab, surface, -600);
    }
    settle();
    let (key_after, top_after) = harness::view_anchor(tab, surface);
    let where_after = harness::scroll_top(tab, surface);
    assert!(
        !at_tail(tab, surface),
        "the reader scrolled up off the tail (at {where_after})"
    );
    // Long enough for any owed correction to come due (the intent window plus its settle).
    std::thread::sleep(Duration::from_millis(1500));
    let (key_later, top_later) = harness::view_anchor(tab, surface);
    let where_later = harness::scroll_top(tab, surface);
    assert!(
        key_later == key_after && (top_later - top_after).abs() <= 4.0,
        "the reader stays where they scrolled to: was {key_after} @ {top_after:.0} ({where_after}), later {key_later} @ {top_later:.0} ({where_later})"
    );
    assert!(
        !at_tail(tab, surface),
        "…and is not dragged back to the tail: {where_after} -> {where_later}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_folding_near_the_tail_keeps_the_scroll() {
    let _serial = serial();
    let fx = fixture("scenario-foldscroll-classic", 20);
    let page = open(Surface::Classic, &fx, 0);
    scenario_folding_near_the_tail_keeps_the_scroll(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_folding_near_the_tail_keeps_the_scroll() {
    let _serial = serial();
    let fx = fixture("scenario-foldscroll-app", 20);
    let page = open(Surface::AppShell, &fx, 2912);
    scenario_folding_near_the_tail_keeps_the_scroll(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a workflow call carries its fleet in flow (#119) ──────────────────────────────

/// The call that launched a run shows its members under it — a dot per member, pulsing for the
/// one still running — each naming a session the reader can open. The agents pane keeps its own
/// list; this is the in-flow half the owner asked for.
fn scenario_the_call_carries_its_fleet(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // The launching call is at the tail: land there, so both pages have it mounted.
    jump_to_end(tab, surface);
    settle();
    let roster = "(function(){ var host = document.querySelector('[data-run]'); if (!host) return { host: false }; var rows = [...host.querySelectorAll('.fleet-row')]; return { host: true, run: host.dataset.run, rows: rows.length, running: host.querySelectorAll('.fleet-dot.on').length, names: rows.map(function (r) { return (r.querySelector('.fleet-name') || {}).textContent; }), hrefs: rows.map(function (r) { var a = r.querySelector('.fleet-name'); return a ? a.getAttribute('href') : ''; }), ids: rows.map(function (r) { return (r.querySelector('.fleet-id') || {}).textContent; }) }; })()";
    until(
        tab,
        "!!document.querySelector('[data-run] .fleet-row')",
        "the launching call to carry its fleet",
        Duration::from_secs(20),
        "JSON.stringify({ hosts: document.querySelectorAll('[data-run]').length, fleets: document.querySelectorAll('.fleet').length, workflowText: document.body.innerText.indexOf('Workflow') >= 0 })",
    );
    let fleet = probe(tab, roster);
    assert_eq!(
        fleet["run"], RUN,
        "the roster hangs under its own run: {fleet}"
    );
    assert_eq!(fleet["rows"], 2, "one row per member: {fleet}");
    assert_eq!(
        fleet["running"], 1,
        "the member still working carries the running dot: {fleet}"
    );
    assert_eq!(
        fleet["names"][0], "Reviewed the parser",
        "a finished member is titled by its result: {fleet}"
    );
    assert_eq!(
        fleet["ids"][0], MEMBER_DONE,
        "…beside the id that addresses it: {fleet}"
    );
    let href = fleet["hrefs"][0].as_str().unwrap_or_default().to_string();
    assert!(
        href.contains(&format!("session={MEMBER_DONE}")),
        "a member's name opens that member's session: {href}"
    );
    // And the click gets there: the page ends up addressing the member's session.
    eval(
        tab,
        "document.querySelector('[data-run] .fleet-row .fleet-name').click(); 'ok'",
    );
    until(
        tab,
        &format!("location.search.indexOf('session={MEMBER_DONE}') >= 0"),
        "the click to open the member's session",
        Duration::from_secs(15),
        "location.search",
    );
    let _ = surface;
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_call_carries_its_fleet() {
    let _serial = serial();
    let fx = fixture_workflow("scenario-fleet-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_call_carries_its_fleet(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_call_carries_its_fleet() {
    let _serial = serial();
    let fx = fixture_workflow("scenario-fleet-app");
    let page = open(Surface::AppShell, &fx, 2908);
    scenario_the_call_carries_its_fleet(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the turn bar names the turn, and returns to it (#123) ─────────────────────────

/// A strip under the top bar reads "Turn N — <label>" for the turn the reader is inside, and a
/// click on it returns to that turn's card. At the very top it is off: the first turn is on
/// screen naming itself.
fn scenario_the_turn_bar_names_and_returns(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    scroll_by(tab, surface, -1_000_000);
    settle();
    assert!(
        harness::sticky_turn(tab, surface).is_none(),
        "at the very top the bar is off — the first turn names itself: {:?}",
        harness::sticky_turn(tab, surface)
    );
    // Scroll in until the reader is inside turn 7.
    let mut named = None;
    for _ in 0..40 {
        scroll_by(tab, surface, 320);
        settle();
        if let Some((turn, text)) = harness::sticky_turn(tab, surface) {
            if turn >= 7 {
                named = Some((turn, text));
                break;
            }
        }
    }
    let (turn, text) = named.expect("the bar names a turn once the reader has scrolled in");
    assert_eq!(
        turn, 7,
        "the bar names the turn the reader is inside: {text}"
    );
    assert!(
        text.contains("Turn 7 — question 6"),
        "…as 'Turn N — <the turn's own label>': {text}"
    );
    let top = turn_at_top(tab, surface);
    assert!(
        (top - turn).abs() <= 1,
        "…the same turn the viewport is showing: bar {turn}, viewport {top}"
    );
    // Read on, then click the bar: it returns to the turn it names.
    for _ in 0..3 {
        scroll_by(tab, surface, 700);
    }
    settle();
    let (later, later_text) =
        harness::sticky_turn(tab, surface).expect("the bar still names the turn being read");
    assert!(
        later > turn,
        "reading on moves the bar forward: {turn} -> {later} ({later_text})"
    );
    harness::click_sticky_turn(tab, surface);
    settle();
    settle();
    let landed = turn_at_top(tab, surface);
    assert!(
        (landed - later).abs() <= 1,
        "the click returns to the turn the bar named: {later}, landed on {landed}"
    );
    let base = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{}.getBoundingClientRect().top", surface.scroller()),
    };
    let card = eval(
        tab,
        &format!("(function(){{ var c = document.querySelector('[data-turn=\"{later}\"]'); if (!c) return 9999; return Math.round(c.getBoundingClientRect().top - ({base})); }})()"),
    )
    .as_i64()
    .unwrap_or(9999);
    assert!(
        (-8..=160).contains(&card),
        "…with that turn's card at the top of the viewport, clear of the bar: {card}px"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_turn_bar_names_and_returns() {
    let _serial = serial();
    let fx = fixture("scenario-turnbar-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_turn_bar_names_and_returns(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_turn_bar_names_and_returns() {
    let _serial = serial();
    let fx = fixture("scenario-turnbar-app", 40);
    let page = open(Surface::AppShell, &fx, 2907);
    scenario_the_turn_bar_names_and_returns(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the pane's focused turn follows the transcript (#52) ──────────────────────────

/// Scrolled a few screens into the session, the pane names the turn at the top of the
/// viewport (±1: the sticky line sits a little below the edge).
fn scenario_pane_follows_the_transcript(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    scroll_by(tab, surface, -1_000_000);
    settle();
    for _ in 0..6 {
        scroll_by(tab, surface, 900);
    }
    settle();
    let top = turn_at_top(tab, surface);
    assert!(top >= 2, "the reader is a few turns in ({top})");
    let focus = harness::pane_focus_turn(tab, surface);
    assert!(
        (focus - top).abs() <= 1,
        "the pane names the turn at the top: pane {focus}, viewport {top}"
    );
    // The other direction: choosing a turn in the pane moves the transcript there, and the
    // pane then names exactly that turn — the spy does not overwrite the choice.
    assert!(
        harness::jump_to_turn(tab, surface, 3),
        "the pane lists turn 3"
    );
    settle();
    settle();
    let landed = turn_at_top(tab, surface);
    let named = harness::pane_focus_turn(tab, surface);
    assert!(
        (landed - 3).abs() <= 1,
        "the pane's choice moved the transcript: top {landed}"
    );
    assert_eq!(
        named, 3,
        "…and the pane names the chosen turn (viewport {landed})"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_pane_follows_the_transcript() {
    let _serial = serial();
    let fx = fixture("scenario-pane-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_pane_follows_the_transcript(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_pane_follows_the_transcript() {
    let _serial = serial();
    let fx = fixture("scenario-pane-app", 40);
    let page = open(Surface::AppShell, &fx, 2854);
    scenario_pane_follows_the_transcript(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: search hits and highlights survive growth ─────────────────────────────────────

/// A query that every user turn matches: the count is the turn count, highlights are mounted;
/// after the transcript grows by two matching turns the count follows and the highlights stay.
fn scenario_search_through_growth(tab: &headless_chrome::Tab, surface: Surface, fx: &Fixture) {
    let hits = harness::search(tab, surface, "question");
    assert!(
        hits >= fx.turns as i64,
        "every user turn matches: {hits} hits for {} turns",
        fx.turns
    );
    // The app shell marks only its CURRENT hit, and only once the reader steps to it; the
    // classic page marks every materialized hit. Stepping once is what both pages agree on.
    harness::search_next(tab, surface);
    assert!(
        harness::search_marks(tab, surface) > 0,
        "the current hit is highlighted"
    );
    let script = vec![
        user_at("question 1000: a late question", &now_minus(30)),
        assistant_at("answer 1000: a late answer", &now_minus(25)),
        user_at("question 1001: another late question", &now_minus(20)),
        assistant_at("answer 1001: another late answer", &now_minus(15)),
    ];
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(2600));
    assert_eq!(growth.finish(Duration::from_secs(30)), 4);
    std::thread::sleep(Duration::from_millis(4000));
    let after = harness::search_hits(tab, surface);
    assert!(
        after >= hits + 2,
        "the count followed the growth: {hits} -> {after}"
    );
    assert!(
        harness::search_marks(tab, surface) > 0,
        "highlights survived the growth"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_search_survives_growth() {
    let _serial = serial();
    let fx = fixture("scenario-search-classic", 30);
    let page = open(Surface::Classic, &fx, 0);
    scenario_search_through_growth(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_search_survives_growth() {
    let _serial = serial();
    let fx = fixture("scenario-search-app", 30);
    let page = open(Surface::AppShell, &fx, 2855);
    scenario_search_through_growth(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a deep jump, then paging and stepping around it ───────────────────────────────

/// Jump to a turn deep in the session through the pane, then page down twice and step with
/// `]` and `[`: every move is relative to where the jump landed, never a leap elsewhere.
fn scenario_deep_jump_then_page_and_step(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let target = fx.turns / 2;
    assert!(
        harness::jump_to_turn(tab, surface, target),
        "the pane lists turn {target}"
    );
    settle();
    settle();
    let landed = turn_at_top(tab, surface);
    assert!(
        (landed - target as i64).abs() <= 1,
        "the jump landed on turn {target}: top {landed}"
    );
    key(tab, " ", false);
    settle();
    key(tab, " ", false);
    settle();
    let paged = turn_at_top(tab, surface);
    assert!(
        paged >= landed && paged <= landed + 12,
        "two pages down stay near the landing: {landed} -> {paged}"
    );
    key(tab, "]", false);
    settle();
    let next = turn_at_top(tab, surface);
    assert!(
        next > paged && next <= paged + 3,
        "`]` steps to the next turn: {paged} -> {next}"
    );
    key(tab, "[", false);
    settle();
    let back = turn_at_top(tab, surface);
    assert!(
        back < next && back + 3 >= next,
        "`[` steps back: {next} -> {back}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_pages_and_steps_around_a_deep_jump() {
    let _serial = serial();
    let fx = fixture("scenario-deep-classic", 120);
    let page = open(Surface::Classic, &fx, 0);
    scenario_deep_jump_then_page_and_step(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_pages_and_steps_around_a_deep_jump() {
    let _serial = serial();
    let fx = fixture("scenario-deep-app", 120);
    let page = open(Surface::AppShell, &fx, 2856);
    scenario_deep_jump_then_page_and_step(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a resize while pinned keeps the tail ──────────────────────────────────────────

/// Pinned at the tail, a narrower then a wider window: every measured height is a guess
/// again, and the reader must still be at the tail after each.
fn scenario_resize_while_pinned(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    harness::resize(tab, 1000.0, 700.0);
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        at_tail(tab, surface),
        "narrower: still at the tail (top turn {})",
        turn_at_top(tab, surface)
    );
    harness::resize(tab, 1400.0, 900.0);
    std::thread::sleep(Duration::from_millis(1200));
    assert!(
        at_tail(tab, surface),
        "wider again: still at the tail (top turn {})",
        turn_at_top(tab, surface)
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_keeps_the_tail_through_a_resize() {
    let _serial = serial();
    let fx = fixture("scenario-resize-classic", 60);
    let page = open(Surface::Classic, &fx, 0);
    scenario_resize_while_pinned(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_keeps_the_tail_through_a_resize() {
    let _serial = serial();
    let fx = fixture("scenario-resize-app", 60);
    let page = open(Surface::AppShell, &fx, 2857);
    scenario_resize_while_pinned(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a server restart under a watching page — resume, position kept ────────────────

/// The server dies and comes back on the same port. A page pinned at the tail is at the tail
/// again with every record back; a page scrolled up keeps the turn it was reading.
fn scenario_restart_resumes(page: &mut Opened, surface: Surface, fx: &Fixture, port: u16) {
    let tab = page.tab.clone();
    jump_to_end(&tab, surface);
    await_tail(&tab, surface, "a fresh open to land at the tail");
    let last = last_mounted_turn(&tab, surface);
    assert!(
        last >= fx.turns as i64 - 1,
        "the whole fixture is there before the restart ({last} of {})",
        fx.turns
    );
    restart_monitor(page, fx, port);
    let t0 = std::time::Instant::now();
    let mut back = false;
    while t0.elapsed() < Duration::from_secs(25) {
        if last_mounted_turn(&tab, surface) >= last && at_tail(&tab, surface) {
            back = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(back, "pinned: after the restart the records are back and the page is at the tail (last {}, at tail {})", last_mounted_turn(&tab, surface), at_tail(&tab, surface));
    // Unpinned: scroll up, restart again, the reader keeps their turn.
    scroll_by(&tab, surface, -900);
    scroll_by(&tab, surface, -900);
    settle();
    let held = turn_at_top(&tab, surface);
    assert!(held >= 0 && held < last, "the reader scrolled up ({held})");
    restart_monitor(page, fx, port);
    std::thread::sleep(Duration::from_millis(6000));
    let after = turn_at_top(&tab, surface);
    assert!(
        (after - held).abs() <= 1,
        "unpinned: the restart kept the reader's turn ({held} -> {after})"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_resumes_after_a_server_restart() {
    let _serial = serial();
    let fx = fixture("scenario-restart-classic", 40);
    let mut page = open_on_v2(Surface::Classic, &fx, 2859);
    scenario_restart_resumes(&mut page, Surface::Classic, &fx, 2859);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_resumes_after_a_server_restart() {
    let _serial = serial();
    let fx = fixture("scenario-restart-app", 40);
    let mut page = open_on_v2(Surface::AppShell, &fx, 2860);
    scenario_restart_resumes(&mut page, Surface::AppShell, &fx, 2860);
}

// ── scenario: an unpinned reader holds to the PIXEL through growth (#51's reproduced part) ──

/// Scrolled up and left alone, WHAT THE READER SEES does not move while records arrive —
/// the first visible element keeps its offset (±4px), checked after every apply. (The scroll
/// offset itself moves legitimately whenever content above the viewport changes height.)
fn scenario_unpinned_holds_to_the_pixel(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    scroll_by(tab, surface, -700);
    scroll_by(tab, surface, -700);
    settle();
    let (held_key, held_top) = harness::view_anchor(tab, surface);
    assert!(
        !held_key.is_empty() && !at_tail(tab, surface),
        "the reader scrolled up (looking at {held_key})"
    );
    let growth = LiveGrowth::start(
        fx.path.clone(),
        growth_script(),
        Duration::from_millis(2600),
    );
    let t0 = std::time::Instant::now();
    let mut worst = 0.0f64;
    let mut worst_at = String::new();
    while t0.elapsed() < Duration::from_secs(26) {
        std::thread::sleep(Duration::from_millis(250));
        let (key, top) = harness::view_anchor(tab, surface);
        let drift = if key == held_key {
            (top - held_top).abs()
        } else {
            1e6
        };
        if drift > worst {
            worst = drift;
            worst_at = format!(
                "{:.1}s, appended {}, looking at {key} @ {top:.0}",
                t0.elapsed().as_secs_f64(),
                growth.count()
            );
        }
    }
    let appended = growth.finish(Duration::from_secs(10));
    assert_eq!(appended, 8, "the driver appended the whole script");
    assert!(
        worst <= 4.0,
        "unpinned: what the reader sees moved during growth (worst {worst:.0}px at {worst_at}); held {held_key} @ {held_top:.0}px"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_to_the_pixel_when_unpinned_through_growth() {
    let _serial = serial();
    let fx = fixture("scenario-pixel-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_unpinned_holds_to_the_pixel(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_to_the_pixel_when_unpinned_through_growth() {
    let _serial = serial();
    let fx = fixture("scenario-pixel-app", 40);
    let page = open(Surface::AppShell, &fx, 2861);
    scenario_unpinned_holds_to_the_pixel(&page.tab, Surface::AppShell, &fx);
}

/// The real-transcript shape (#51): the reader is scrolled a few screens back INSIDE a long open
/// turn while that turn keeps growing. Same pixel rule as above.
fn fixture_open_turn(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, &harness::long_open_turn_session(12, 40));
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// The same rule with the reader INSIDE a long open turn while it grows (the real-transcript
/// shape, #51): the first visible element keeps its offset through every rewrite of the tail.
fn scenario_unpinned_inside_an_open_turn_holds_to_the_pixel(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    for _ in 0..3 {
        scroll_by(tab, surface, -700);
    }
    settle();
    let (held_key, held_top) = harness::view_anchor(tab, surface);
    assert!(
        !held_key.is_empty() && !at_tail(tab, surface),
        "the reader scrolled up inside the open turn (looking at {held_key})"
    );
    let growth = LiveGrowth::start(
        fx.path.clone(),
        harness::open_turn_growth(40, 3),
        Duration::from_millis(2600),
    );
    let t0 = std::time::Instant::now();
    let mut worst = 0.0f64;
    let mut worst_at = String::new();
    while t0.elapsed() < Duration::from_secs(36) {
        std::thread::sleep(Duration::from_millis(250));
        let (key, top) = harness::view_anchor(tab, surface);
        let drift = if key == held_key {
            (top - held_top).abs()
        } else {
            1e6
        };
        if drift > worst {
            worst = drift;
            worst_at = format!(
                "{:.1}s, appended {}, looking at {key} @ {top:.0}",
                t0.elapsed().as_secs_f64(),
                growth.count()
            );
        }
    }
    let appended = growth.finish(Duration::from_secs(10));
    assert_eq!(appended, 12, "the driver appended the whole script");
    assert!(
        worst <= 4.0,
        "unpinned inside the open turn: what the reader sees moved during growth (worst {worst:.0}px at {worst_at}); held {held_key} @ {held_top:.0}px"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_to_the_pixel_inside_an_open_turn() {
    let _serial = serial();
    let fx = fixture_open_turn("scenario-openturn-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_unpinned_inside_an_open_turn_holds_to_the_pixel(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_to_the_pixel_inside_an_open_turn() {
    let _serial = serial();
    let fx = fixture_open_turn("scenario-openturn-app");
    let page = open(Surface::AppShell, &fx, 2862);
    scenario_unpinned_inside_an_open_turn_holds_to_the_pixel(&page.tab, Surface::AppShell, &fx);
}

/// The probe's shape (#51): the reader keeps scrolling back INSIDE the open turn while it grows.
/// Between two of the reader's own scrolls, what they see must not move (±4px); each scroll
/// resets the expectation.
fn scenario_scrolling_back_during_growth_holds_between_scrolls(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    for _ in 0..3 {
        scroll_by(tab, surface, -700);
    }
    settle();
    assert!(
        !at_tail(tab, surface),
        "the reader scrolled up inside the open turn"
    );
    let growth = LiveGrowth::start(
        fx.path.clone(),
        harness::open_turn_growth(40, 4),
        Duration::from_millis(2600),
    );
    let t0 = std::time::Instant::now();
    let mut expect = harness::view_anchor(tab, surface);
    let mut worst = 0.0f64;
    let mut worst_at = String::new();
    let mut ticks = 0u32;
    while t0.elapsed() < Duration::from_secs(44) {
        std::thread::sleep(Duration::from_millis(250));
        ticks += 1;
        if ticks.is_multiple_of(9) {
            scroll_by(tab, surface, -400);
            // 150ms, not the intent window: a reader who stops scrolling must not be moved at
            // all. This was widened to 500ms while #132 step 3 was owing corrections across the
            // reader's own scrolls — an 11px drift that was the DEFECT (#138), not the rule.
            std::thread::sleep(Duration::from_millis(150));
            expect = harness::view_anchor(tab, surface);
            continue;
        }
        let (key, top) = harness::view_anchor(tab, surface);
        let drift = if key == expect.0 {
            (top - expect.1).abs()
        } else {
            1e6
        };
        if drift > worst {
            worst = drift;
            worst_at = format!(
                "{:.1}s, appended {}, looking at {key} @ {top:.0} (expected {} @ {:.0})",
                t0.elapsed().as_secs_f64(),
                growth.count(),
                expect.0,
                expect.1
            );
        }
    }
    let appended = growth.finish(Duration::from_secs(10));
    assert_eq!(appended, 16, "the driver appended the whole script");
    assert!(
        worst <= 4.0,
        "between the reader's own scrolls what the reader sees moved during growth (worst {worst:.0}px at {worst_at})"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_between_scrolls_while_the_open_turn_grows() {
    let _serial = serial();
    let fx = fixture_open_turn("scenario-scrollgrow-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_scrolling_back_during_growth_holds_between_scrolls(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_between_scrolls_while_the_open_turn_grows() {
    let _serial = serial();
    let fx = fixture_open_turn("scenario-scrollgrow-app");
    let page = open(Surface::AppShell, &fx, 2863);
    scenario_scrolling_back_during_growth_holds_between_scrolls(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the "N new messages" pill (#64) ───────────────────────────────────────────────

/// Scrolled up, the reader sees no count; eight records arrive and the pill says "8 new
/// messages" (records, as the classic page counts); clicking it lands at the tail and the count
/// is gone.
fn scenario_new_messages_pill(tab: &headless_chrome::Tab, surface: Surface, fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    scroll_by(tab, surface, -900);
    scroll_by(tab, surface, -900);
    settle();
    assert!(
        harness::new_messages_pill(tab, surface) <= 0,
        "nothing new yet: no count ({})",
        harness::new_messages_pill(tab, surface)
    );
    let growth = LiveGrowth::start(
        fx.path.clone(),
        growth_script(),
        Duration::from_millis(2600),
    );
    assert_eq!(
        growth.finish(Duration::from_secs(40)),
        8,
        "the driver appended the whole script"
    );
    harness::await_pill(tab, surface, 8, "the pill says how many records arrived");
    harness::click_pill(tab, surface);
    await_tail(tab, surface, "the pill's click to land at the tail");
    settle();
    assert!(
        harness::new_messages_pill(tab, surface) <= 0,
        "at the tail the count is gone ({})",
        harness::new_messages_pill(tab, surface)
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_shows_the_new_messages_pill() {
    let _serial = serial();
    let fx = fixture("scenario-pill-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_new_messages_pill(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_shows_the_new_messages_pill() {
    let _serial = serial();
    let fx = fixture("scenario-pill-app", 40);
    let page = open(Surface::AppShell, &fx, 2864);
    scenario_new_messages_pill(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a queued prompt shows its text (#65) ─────────────────────────────────────────

/// Pinned at the tail, a turn arrives and then a prompt the user queued while the agent was
/// busy; both pages show the queued marker WITH the prompt's own words.
fn scenario_queued_prompt_shows_its_text(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    assert_eq!(queued_text(tab, surface), "", "nothing queued yet");
    let script = vec![
        user_at("question 700: one more before the queue", &now_minus(30)),
        assistant_at("answer 700: working on it", &now_minus(25)),
        queued_at(
            "please also run the tests when this finishes",
            &now_minus(20),
        ),
    ];
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(2000));
    assert_eq!(
        growth.finish(Duration::from_secs(30)),
        3,
        "the driver appended the whole script"
    );
    until(
        tab,
        &format!(
            "{}.length > 0",
            match surface {
                Surface::Classic => "document.querySelectorAll('.qmarker .qmd')",
                Surface::AppShell => "document.querySelectorAll('.renderer-queue-text')",
            }
        ),
        "the queued marker to render",
        Duration::from_secs(20),
        "document.body.innerText.slice(-300)",
    );
    let text = queued_text(tab, surface);
    assert!(
        text.contains("please also run the tests when this finishes"),
        "the marker shows the queued prompt's words, got {text:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_shows_a_queued_prompts_text() {
    let _serial = serial();
    let fx = fixture("scenario-queued-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_queued_prompt_shows_its_text(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_shows_a_queued_prompts_text() {
    let _serial = serial();
    let fx = fixture("scenario-queued-app", 12);
    let page = open(Surface::AppShell, &fx, 2865);
    scenario_queued_prompt_shows_its_text(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the session id in the header copies the transcript path (#50) ─────────────

/// The page shows the session id — the classic page's short form, the app shell's full id in
/// its title menu (#83 dropped the header chip) — and the page's own control copies the
/// transcript's path on disk.
fn scenario_session_id_copies_the_transcript_path(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let shown = match surface {
        Surface::Classic => "document.getElementById('sid')",
        Surface::AppShell => "document.querySelector('[data-session-copy-value=\"id\"]')",
    };
    until(
        tab,
        &format!(
            "(({shown}) || {{textContent: ''}}).textContent.trim().indexOf('{}') === 0",
            &SID[..8]
        ),
        "the page to show the session id",
        Duration::from_secs(20),
        &format!("(({shown}) || {{textContent: 'no element'}}).textContent"),
    );
    assert!(
        session_id_chip(tab, surface).starts_with(&SID[..8]),
        "the id shown begins with the UUID's first eight hex digits"
    );
    stub_clipboard(tab);
    click_session_id(tab, surface);
    until(
        tab,
        "window.__copied != null",
        "the click to copy",
        Duration::from_secs(5),
        "String(window.__copied)",
    );
    assert_eq!(
        copied_text(tab),
        fx.path.to_string_lossy(),
        "what was copied is the transcript's path"
    );
    if surface == Surface::Classic {
        until(
            tab,
            "(document.getElementById('sid') || {}).textContent === 'copied transcript path'",
            "the classic chip to say it copied",
            Duration::from_secs(5),
            "(document.getElementById('sid') || {}).textContent",
        );
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_session_id_copies_the_transcript_path() {
    let _serial = serial();
    let fx = fixture("scenario-sid-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_session_id_copies_the_transcript_path(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_session_id_copies_the_transcript_path() {
    let _serial = serial();
    let fx = fixture("scenario-sid-app", 12);
    let page = open(Surface::AppShell, &fx, 2866);
    scenario_session_id_copies_the_transcript_path(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: an embedded image renders (#80) ──────────────────────────────────────────────

/// A Read of a PNG records the image in the tool result. The classic page shows it inline; the
/// app shell shows a line, expands it to a bounded thumbnail on the first click, and opens the
/// full-size lightbox on the second.
fn scenario_embedded_image_renders(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    match surface {
        Surface::Classic => {
            until(tab, "!!document.querySelector('.amark img') && document.querySelector('.amark img').naturalWidth >= 1 && document.querySelector('.amark img').getBoundingClientRect().height > 0", "the classic page to show the image inline and visible", Duration::from_secs(20), "document.querySelectorAll('.amark').length + ' attachment blocks, imgs: ' + document.querySelectorAll('.amark img').length");
        }
        Surface::AppShell => {
            // The reader's flow (#106): the image is a row of its own inside the process; its
            // fold is opened by ITS head — not whichever fold happens to be last — and the
            // toggle is clicked only once it has a rect, as a reader would see it.
            until(tab, "!!document.querySelector('[data-image-toggle]')", "the image block to render inside the process", Duration::from_secs(20), "document.querySelectorAll('.renderer-note, .renderer-image').length + ' attachment views'");
            let opened = eval(tab, "(function(){ var t = document.querySelector('[data-image-toggle]'); var r = t.closest('.renderer'); if (!r) return 'no renderer'; if (!r.classList.contains('closed')) return 'already open'; var h = r.querySelector('button.renderer-head'); if (!h) return 'no head button'; h.click(); return 'opened'; })()");
            assert!(
                opened == "opened" || opened == "already open",
                "the image row's fold opens from its own head: {opened}"
            );
            until(tab, "(function(){ var t = document.querySelector('[data-image-toggle]'); return !!t && t.getBoundingClientRect().height > 0; })()", "the Show image control to be visible in its open row", Duration::from_secs(10), "document.querySelector('[data-image-toggle]') ? document.querySelector('[data-image-toggle]').closest('.renderer').className : 'no toggle'");
            assert_eq!(
                eval(
                    tab,
                    "document.querySelectorAll('.renderer-image img').length"
                ),
                0,
                "collapsed: no image yet"
            );
            eval(
                tab,
                "document.querySelector('[data-image-toggle]').click(); 'ok'",
            );
            until(tab, "!!document.querySelector('.renderer-image-thumb img') && document.querySelector('.renderer-image-thumb img').naturalWidth >= 1", "the first click to show a thumbnail with real dimensions", Duration::from_secs(10), "(function(){ var i = document.querySelector('.renderer-image-thumb img'); return i ? JSON.stringify({ src: i.getAttribute('src').slice(0, 40), complete: i.complete, natural: i.naturalWidth, shown: i.offsetParent !== null }) : 'no img'; })()");
            // Visible, not merely decoded (#106): the click re-renders the window, and the tool
            // fold holding the image must come back open — a thumbnail inside a closed fold has
            // its natural size and no rect, which is what "Show image is broken" looked like.
            let thumb = probe(tab, "(function(){ var i = document.querySelector('.renderer-image-thumb img'); var r = i.getBoundingClientRect(); var t = document.querySelector('[data-image-toggle]'); return { height: Math.round(r.height), width: Math.round(r.width), natural: i.naturalWidth, toggle: t ? t.textContent : null, foldOpen: !!i.closest('.renderer') && !i.closest('.renderer').classList.contains('closed') }; })()");
            assert!(
                thumb["height"].as_f64().unwrap_or(0.0) > 0.0
                    && thumb["width"].as_f64().unwrap_or(0.0) > 0.0,
                "the thumbnail is visible after the click: {thumb}"
            );
            assert!(
                thumb["height"].as_f64().unwrap_or(999.0) <= 320.0,
                "the thumbnail is bounded: {thumb}"
            );
            eval(
                tab,
                "document.querySelector('.renderer-image-thumb').click(); 'ok'",
            );
            until(tab, "(function(){ var l = document.querySelector('.image-lightbox'); if (!l || l.hidden) return false; var r = l.getBoundingClientRect(); var img = l.querySelector('img'); return r.width > 0 && r.height > 0 && getComputedStyle(l).visibility !== 'hidden' && !!img && (img.getAttribute('src') || '').indexOf('data:image/png') === 0; })()", "the second click to open the lightbox", Duration::from_secs(10), "(function(){ var l = document.querySelector('.image-lightbox'); if (!l) return 'no lightbox'; var r = l.getBoundingClientRect(); return JSON.stringify({ hidden: l.hidden, w: r.width, h: r.height, vis: getComputedStyle(l).visibility, img: !!l.querySelector('img') }); })()");
        }
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_renders_an_embedded_image() {
    let _serial = serial();
    let fx = image_fixture("scenario-image-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_embedded_image_renders(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_renders_an_embedded_image() {
    let _serial = serial();
    let fx = image_fixture("scenario-image-app");
    let page = open(Surface::AppShell, &fx, 2873);
    scenario_embedded_image_renders(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then a Read of a PNG whose result embeds the image.
fn image_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question 12: read the screenshot", &now_minus(40));
    // Assistant text and a tool call BEFORE the read: the engine places an image attachment
    // where its result landed, so this one follows the assistant's words and lands inside the
    // turn's process (the owner's case — a screenshot read mid-turn) rather than as a prompt
    // attachment of the user turn, which the app shell already shows as a thumbnail card.
    transcript += &assistant_at("answer 12a: let me look at it", &now_minus(39));
    transcript += &harness::tool_open_at("t-pre", &now_minus(38));
    transcript += &harness::tool_result_at("t-pre", &now_minus(37));
    transcript += &read_tool_at("t-img", "/tmp/shot.png", &now_minus(36));
    transcript += &image_result_at("t-img", &now_minus(32));
    transcript += &assistant_at("answer 12: the screenshot shows the deck", &now_minus(28));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: reading deep inside a long OPEN turn while it is rewritten (#98) ─────────────

/// The owner's case: the agent is mid-turn, the turn is long, and the reader has scrolled up
/// inside it (the prompt is off-screen above). Every poll re-emits the open turn's records — same
/// positions, new block ids — as more tool calls arrive. The view must hold: the same record at
/// the same offset, and the turns pane's focus with it.
fn scenario_reading_inside_a_long_open_turn_holds_through_rewrites(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Up into the open turn: far enough that the prompt is above the viewport, on both pages
    // (the classic page's records are compact; the open turn is long enough on either).
    scroll_by(tab, surface, -900);
    scroll_by(tab, surface, -900);
    settle();
    assert!(!at_tail(tab, surface), "the reader is scrolled up");
    let before = view_anchor_index(tab, surface);
    assert!(
        before.0 > 0,
        "a real record is at the top of the view: {before:?}"
    );
    // The turn grows by six more tool calls, each a rewrite of the provisional zone.
    let script: Vec<String> = (0..6)
        .flat_map(|k| {
            vec![
                tool_open_at(&format!("late-{k}"), &now_minus(40 - k * 6)),
                tool_result_at(&format!("late-{k}"), &now_minus(37 - k * 6)),
            ]
        })
        .collect();
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(2000));
    let mut worst = 0.0f64;
    let mut drift = None;
    for _ in 0..14 {
        std::thread::sleep(Duration::from_millis(1000));
        let now = view_anchor_index(tab, surface);
        let moved = (now.1 - before.1).abs();
        if now.0 != before.0 || moved > 4.0 {
            drift = Some(now);
        }
        if moved > worst {
            worst = moved;
        }
    }
    assert_eq!(
        growth.finish(Duration::from_secs(30)),
        12,
        "the driver appended the whole script"
    );
    let after = view_anchor_index(tab, surface);
    assert!(drift.is_none() && after.0 == before.0 && (after.1 - before.1).abs() <= 4.0, "the view held through the rewrites: before {before:?}, worst drift {worst:.1}px, first drift {drift:?}, after {after:?}");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_inside_a_long_open_turn_through_rewrites() {
    let _serial = serial();
    let fx = open_turn_fixture("scenario-open-turn-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_reading_inside_a_long_open_turn_holds_through_rewrites(
        &page.tab,
        Surface::Classic,
        &fx,
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_inside_a_long_open_turn_through_rewrites() {
    let _serial = serial();
    let fx = open_turn_fixture("scenario-open-turn-app");
    let page = open(Surface::AppShell, &fx, 2877);
    scenario_reading_inside_a_long_open_turn_holds_through_rewrites(
        &page.tab,
        Surface::AppShell,
        &fx,
    );
}

/// Show the open turn's whole run: the classic page folds a run of tool calls into one block,
/// the app shell shows the first rows of a process and a "more" control for the rest.
fn expand_open_turn(tab: &headless_chrome::Tab, surface: Surface) -> String {
    let js = match surface {
        // The classic page hides a long run behind a "⋯ N more" expander inside the turn.
        Surface::Classic => "(function(){ var bs = document.querySelectorAll('#stream button[data-more]'); if (!bs.length) return 'no expander'; bs[bs.length - 1].click(); return 'expanded ' + bs.length + ' expanders, records ' + document.querySelectorAll('#stream [data-idx]').length + ', height ' + document.body.scrollHeight; })()",
        Surface::AppShell => "(function(){ var s = [...document.querySelectorAll('[data-process-surface]')].pop(); if (!s) return 'no process'; var m = s.querySelector('[data-process-more]'); if (m) m.click(); s = [...document.querySelectorAll('[data-process-surface]')].pop(); return (m ? 'expanded' : 'no more control') + ', rows ' + s.querySelectorAll('.process-event:not(.progressive-hidden)').length + '/' + s.querySelectorAll('.process-event').length + ', height ' + document.querySelector('.transcript').scrollHeight; })()",
    };
    eval(tab, js).as_str().unwrap_or("").to_string()
}

/// Twelve finished turns, then an open turn of 160 tool calls (many screens on either page).
fn open_turn_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, &open_turn_session(12, 160));
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: growth above the reader, inside the same turn, holds the view (#98) ──────────

/// The owner's jump: reading up through a long turn, something above the visible region grows —
/// a thumbnail decoding, a fold opening, a late reflow — and the content under the reader moves
/// by that height. The rule (the classic page's): the anchor is the first visible RECORD, and
/// every height change puts it back at the same offset, even a change inside the same turn.
fn scenario_growth_above_the_reader_in_the_same_turn_holds(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let expanded = expand_open_turn(tab, surface);
    settle();
    jump_to_end(tab, surface);
    settle();
    scroll_by(tab, surface, -1800);
    settle();
    let before = view_anchor_index(tab, surface);
    // Twelve finished turns take the first 36 records; the reader is inside the open turn.
    assert!(
        before.0 >= 40,
        "a record deep in the open turn is at the top of the view: {before:?} ({expanded})"
    );
    let target = before.0 - 4;
    let grow = match surface {
        Surface::Classic => format!("(function(){{ var e = document.querySelector('#stream [data-idx=\"{target}\"]'); if (!e) return 'missing'; e.style.paddingBottom = '300px'; return 'grown'; }})()"),
        Surface::AppShell => format!("(function(){{ var e = document.querySelector('.virtual-window [data-block-index=\"{target}\"]'); if (!e) return 'missing'; e.style.paddingBottom = '300px'; return 'grown'; }})()"),
    };
    assert_eq!(
        eval(tab, &grow),
        "grown",
        "the record four above the reader is mounted"
    );
    std::thread::sleep(Duration::from_millis(1500));
    let after = view_anchor_index(tab, surface);
    assert!(after.0 == before.0 && (after.1 - before.1).abs() <= 4.0, "the view held through a 300px growth above it in the same turn: before {before:?}, after {after:?}");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_through_growth_above_the_reader() {
    let _serial = serial();
    let fx = open_turn_fixture("scenario-growth-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_growth_above_the_reader_in_the_same_turn_holds(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_through_growth_above_the_reader() {
    let _serial = serial();
    let fx = open_turn_fixture("scenario-growth-app");
    let page = open(Surface::AppShell, &fx, 2878);
    scenario_growth_above_the_reader_in_the_same_turn_holds(&page.tab, Surface::AppShell, &fx);
}

// ── the scrollbar thumb owns the position while it is held (#98, app shell) ────────────────

/// Dragging the thumb into unvisited territory: units mount there with real heights that differ
/// from the estimates, and a page that re-anchors on its old first-visible element snaps the
/// thumb away from where the pointer holds it — the "zone the slider cannot rest in". While the
/// pointer holds the thumb, the scroll offset is the truth: the window follows it and nothing
/// corrects it; the anchor rule resumes on release.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_lets_the_thumb_own_the_position_while_dragged() {
    let _serial = serial();
    let fx = open_turn_fixture("scenario-thumb-app");
    let page = open(Surface::AppShell, &fx, 2879);
    let tab = &page.tab;
    let heights = r#"(function(){var vw=document.querySelector('.virtual-window');var out={};for(const c of vw.children){out[(c.dataset.unitIndex||'?')+':'+(c.dataset.unitKey||'?')]=Math.round(c.getBoundingClientRect().height);}var t=document.querySelectorAll('.virtual-pad');out['__pads']=[t[0].style.height,t[1].style.height];return out;})()"#;
    jump_to_end(tab, Surface::AppShell);
    await_tail(tab, Surface::AppShell, "a fresh open to land at the tail");
    settle();
    // The pointer lands on the scrollbar (x inside the scroller's box but past its client width).
    eval(tab, "(function(){ var s = document.querySelector('.transcript'); var r = s.getBoundingClientRect(); s.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: r.right - 3, clientY: r.top + 40, pointerId: 1, buttons: 1 })); return 'down'; })()");
    // …and drags to a tenth of the range in one go, then holds there.
    let set = "(function(){ var s = document.querySelector('.transcript'); s.scrollTop = Math.round(s.scrollHeight * 0.1); return s.scrollTop; })()";
    let wanted = eval(tab, set).as_f64().unwrap_or(0.0);
    let mut worst = 0.0f64;
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(150));
        let st = eval(tab, "document.querySelector('.transcript').scrollTop")
            .as_f64()
            .unwrap_or(0.0);
        let drift = (st - wanted).abs();
        if drift > worst {
            worst = drift;
        }
    }
    let a = view_anchor_index(tab, Surface::AppShell);
    assert!(
        a.0 >= 0 && a.1 <= 2.0,
        "the window followed the thumb: a mounted record holds the viewport top ({a:?})"
    );
    assert!(
        worst <= 2.0,
        "nothing corrected the held position (worst drift {worst:.1}px)"
    );
    eval(tab, "(function(){ dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerId: 1 })); return 'up'; })()");
    settle();
    // Released: the anchor rule is back — a growth above the reader is corrected again.
    let before = view_anchor_index(tab, Surface::AppShell);
    let grow = format!("(function(){{ var e = document.querySelector('.virtual-window [data-block-index=\"{}\"]'); if (!e) return 'missing'; e.style.paddingBottom = '200px'; return 'grown'; }})()", before.0 - 2);
    let probe_js = "(function(){ var s = document.querySelector('.transcript'); var pads = [...document.querySelectorAll('.virtual-pad')].map(function (p) { return p.style.height; }); var j = document.getElementById('jumpToBottom'); return JSON.stringify({ st: s.scrollTop, sh: s.scrollHeight, pads: pads, mounted: document.querySelectorAll('.virtual-window > [data-unit-key]').length, jumpHidden: j ? j.hidden : null }); })()";
    let probe_before = eval(tab, probe_js);
    if eval(tab, &grow) == "grown" {
        std::thread::sleep(Duration::from_millis(1200));
        let after = view_anchor_index(tab, Surface::AppShell);
        let probe_after = eval(tab, probe_js);
        assert!(after.0 == before.0 && (after.1 - before.1).abs() <= 4.0, "after release the anchor rule holds again: before {before:?} {probe_before}, after {after:?} {probe_after}");
    }
}

// ── scenario: output caps — the first rows show, the rest wait, an expansion is remembered (#108)

/// Row 3.1 of design/rendering-parity-audit.md. The server caps every pre/num/diff part; the
/// reader sees the first rows and a "⋯ N more lines · to line M" control, expands in place, and
/// a small expansion survives leaving and returning (the block re-materializes open).
fn scenario_output_caps_expand_and_remember(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Open the Bash and Read rows of the last turn the way a reader does: by their heads,
    // outermost first — a tool call sits inside an activity fold on both pages.
    let mut opened = String::new();
    for tool in ["bash", "read"] {
        for _ in 0..6 {
            let step = match surface {
                Surface::Classic => eval(tab, &format!("(function(){{ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"{tool}\"]')].pop(); if (!f) return 'none'; var chain = []; for (var e = f; e; e = e.parentElement.closest('.fold')) chain.push(e); var closed = chain.reverse().find(function (x) {{ return x.dataset.open === '0'; }}); if (!closed) return 'open'; closed.querySelector('.fold-h').click(); return 'clicked'; }})()")),
                Surface::AppShell => {
                    let name = if tool == "bash" { "Bash" } else { "Read" };
                    eval(tab, &format!("(function(){{ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"{name}\"] > .renderer')].pop(); if (!t) return 'none'; var chain = []; for (var e = t; e; e = e.parentElement && e.parentElement.closest('.renderer')) chain.push(e); var closed = chain.reverse().find(function (x) {{ return x.classList.contains('closed'); }}); if (!closed) return 'open'; closed.querySelector('button.renderer-head').click(); return 'clicked'; }})()"))
                }
            };
            let step = step.as_str().unwrap_or("").to_string();
            opened.push_str(&format!("{tool}:{step} "));
            settle();
            if step == "open" || step == "none" {
                break;
            }
        }
    }
    let (bash_lines, bash_btn, read_rows, read_btn, hidden_sel) = match surface {
        Surface::Classic => (
            "(function(){ var r = [...document.querySelectorAll('#stream .fold[data-kind=\"bash\"] .result')].pop(); if (!r) return -1; return [...r.querySelectorAll('pre')].filter(function (p) { return p.getBoundingClientRect().height > 0; }).map(function (p) { return p.textContent.split('\\n').length; }).reduce(function (a, b) { return a + b; }, 0); })()",
            "(function(){ var r = [...document.querySelectorAll('#stream .fold[data-kind=\"bash\"] .result')].pop(); var b = r ? r.querySelector('button.morebtn') : null; return b ? b.textContent : ''; })()",
            "(function(){ var n = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"] .numbered')].pop(); if (!n) return -1; return [...n.querySelectorAll('.nrow')].filter(function (x) { return x.getBoundingClientRect().height > 0; }).length; })()",
            "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"]')].pop(); var b = f ? f.querySelector('button.morebtn') : null; return b ? b.textContent : ''; })()",
            "#stream .fold[data-kind=\"read\"] button.morebtn",
        ),
        Surface::AppShell => (
            "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"] .renderer-terminal')].pop(); if (!t) return -1; return [...t.querySelectorAll('pre')].filter(function (p) { return p.getBoundingClientRect().height > 0; }).map(function (p) { return p.textContent.split('\\n').length; }).reduce(function (a, b) { return a + b; }, 0); })()",
            "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"] .renderer-terminal')].pop(); var b = t ? t.querySelector('.cap-more-btn') : null; return b ? b.textContent : ''; })()",
            "(function(){ var c = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] .codebox')].pop(); if (!c) return -1; return [...c.querySelectorAll('.line')].filter(function (x) { return x.getBoundingClientRect().height > 0; }).length; })()",
            "(function(){ var c = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] .codebox')].pop(); var b = c ? c.querySelector('.cap-more-btn') : null; return b ? b.textContent : ''; })()",
            ".renderer-turn[data-tool-name=\"Read\"] .cap-more-btn",
        ),
    };
    let diag_js = match surface {
        Surface::Classic => "(function(){ var r = [...document.querySelectorAll('#stream .fold[data-kind=\"bash\"] .result')].pop(); if (!r) return 'no result'; var chain = []; for (var e = r; e && e !== document.body; e = e.parentElement) chain.push(e.tagName.toLowerCase() + '.' + String(e.className).split(' ').slice(0, 3).join('.') + ':' + Math.round(e.getBoundingClientRect().height) + (e.dataset && e.dataset.open != null ? '[open=' + e.dataset.open + ']' : '')); return chain.join(' > ') + ' | pres=' + r.querySelectorAll('pre').length + ' h=' + [...r.querySelectorAll('pre')].map(function (x) { return Math.round(x.getBoundingClientRect().height); }).join(','); })()",
        Surface::AppShell => "(function(){ var r = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"] .renderer-terminal')].pop(); if (!r) return 'no terminal'; var chain = []; for (var e = r; e && e !== document.body; e = e.parentElement) chain.push(e.tagName.toLowerCase() + '.' + String(e.className).split(' ').slice(0, 3).join('.') + ':' + Math.round(e.getBoundingClientRect().height)); return chain.join(' > ') + ' | pres=' + r.querySelectorAll('pre').length + ' h=' + [...r.querySelectorAll('pre')].map(function (x) { return Math.round(x.getBoundingClientRect().height); }).join(','); })()",
    };
    let before = (
        eval(tab, bash_lines),
        eval(tab, bash_btn),
        eval(tab, read_rows),
        eval(tab, read_btn),
    );
    let diag = eval(tab, diag_js);
    assert_eq!(
        before.0, 12,
        "the Bash result shows its first 12 lines ({opened:?}): {before:?} {diag}"
    );
    assert_eq!(
        before.1, "⋯ 188 more lines",
        "…with the expander naming the rest: {before:?}"
    );
    assert_eq!(
        before.2, 10,
        "the Read result shows its first 10 rows: {before:?}"
    );
    assert_eq!(
        before.3, "⋯ 50 more lines · to line 60",
        "…with the range in the expander: {before:?}"
    );
    // Expand the Read cap in place.
    eval(tab, &format!("(function(){{ var bs = document.querySelectorAll('{hidden_sel}'); var b = bs[bs.length - 1]; if (b) b.click(); return !!b; }})()"));
    settle();
    let after = (eval(tab, read_rows), eval(tab, read_btn));
    assert_eq!(after.0, 60, "expanding shows every row: {after:?}");
    assert_eq!(after.1, "", "…and the control is gone: {after:?}");
    // Leave (the block dematerializes far away) and return: the expansion is remembered.
    scroll_by(tab, surface, -40000);
    settle();
    settle();
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the jump back to the tail");
    settle();
    let back = (
        eval(tab, read_rows),
        eval(tab, read_btn),
        eval(tab, bash_lines),
    );
    let diag_back = eval(tab, diag_js);
    let folds_back = eval(tab, match surface {
        Surface::Classic => "(function(){ return [...document.querySelectorAll('#stream .fold[data-kind=\"act\"], #stream .fold[data-kind=\"bash\"], #stream .fold[data-kind=\"read\"]')].slice(-4).map(function (f) { return f.dataset.kind + ':' + f.dataset.open; }).join(' '); })()",
        Surface::AppShell => "(function(){ return [...document.querySelectorAll('.renderer-turn')].slice(-6).map(function (t) { var r = t.querySelector(':scope > .renderer'); return (t.dataset.toolName || t.dataset.kind) + ':' + (r && r.classList.contains('closed') ? 'closed' : 'open'); }).join(' '); })()",
    });
    assert_eq!(
        back.0, 60,
        "back at the tail the Read expansion holds: {back:?} {diag_back} [{folds_back}]"
    );
    assert_eq!(back.1, "", "…without a control: {back:?}");
    // The Bash sits in its own unit above (the narration splits the turn); bring it into the
    // window before asking whether its untouched cap is still a cap.
    let mut bash_back = back.2.clone();
    for _ in 0..6 {
        if bash_back != -1 {
            break;
        }
        scroll_by(tab, surface, -700);
        settle();
        bash_back = eval(tab, bash_lines);
    }
    assert_eq!(
        bash_back, 12,
        "…while the untouched Bash cap is still a cap: {back:?} {diag_back} [{folds_back}]"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_caps_output_and_remembers_an_expansion() {
    let _serial = serial();
    let fx = caps_fixture("scenario-caps-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_output_caps_expand_and_remember(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_caps_output_and_remembers_an_expansion() {
    let _serial = serial();
    let fx = caps_fixture("scenario-caps-app");
    let page = open(Surface::AppShell, &fx, 2882);
    scenario_output_caps_expand_and_remember(&page.tab, Surface::AppShell, &fx);
}

/// Forty turns (tall enough that the tail dematerializes when the reader leaves), then a turn
/// with a 200-line Bash result and a 60-line Read.
fn caps_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(40, Shape::default());
    // A word before each call, as a working agent writes: a bare call is absorbed into an
    // activity fold and would sit inside a closed parent on both pages.
    transcript += &user_at("question caps: run the long checks", &now_minus(90));
    transcript += &assistant_at("Running the long check first.", &now_minus(85));
    transcript += &tool_open_at("t-long-bash", &now_minus(70));
    transcript += &tool_result_lines("t-long-bash", 200, &now_minus(60));
    transcript += &assistant_at("Then reading the long file.", &now_minus(55));
    transcript += &read_tool_at("t-long-read", "/tmp/long-file.txt", &now_minus(50));
    transcript += &tool_result_lines("t-long-read", 60, &now_minus(40));
    transcript += &assistant_at("answer caps: both outputs are long", &now_minus(30));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 41,
    }
}

// ── scenario: the raw text of a user turn, per turn and globally, persisted (#109) ─────────

const RAW_SOURCE: &str = "  two leading spaces\nword  gap   wider\nlast line";

/// Row 1.4 of design/rendering-parity-audit.md. Markdown loses the indentation and the double
/// spaces; the `{}` toggle shows the turn exactly as typed, the global switch does it for every
/// user turn and survives a reload, and either can be turned back.
fn scenario_raw_text_of_a_user_turn(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (toggle, raw_text, raw_count, user_count, global) = match surface {
        Surface::Classic => (
            "(function(){ var t = [...document.querySelectorAll('#stream .uturn .rawbtn')].pop(); if (!t) return 'none'; t.click(); return 'clicked'; })()",
            "(function(){ var p = [...document.querySelectorAll('#stream .uturn pre.raw')].pop(); return p ? p.textContent : null; })()",
            "document.querySelectorAll('#stream .uturn pre.raw').length",
            "document.querySelectorAll('#stream .uturn').length",
            "(function(){ var b = document.getElementById('btn-raw'); if (!b) return 'none'; b.click(); return 'clicked'; })()",
        ),
        Surface::AppShell => (
            "(function(){ var t = [...document.querySelectorAll('.turn.user .raw-toggle')].pop(); if (!t) return 'none'; t.click(); return 'clicked'; })()",
            "(function(){ var p = [...document.querySelectorAll('.turn.user pre.turn-raw-text')].pop(); return p ? p.textContent : null; })()",
            "document.querySelectorAll('.turn.user pre.turn-raw-text').length",
            "document.querySelectorAll('.turn.user').length",
            "(function(){ var b = document.querySelector('[data-reading-toggle=\"rawUser\"]'); if (!b) return 'none'; b.click(); return 'clicked'; })()",
        ),
    };
    assert_eq!(eval(tab, raw_count), 0, "rendered by default: no raw view");
    assert_eq!(
        eval(tab, toggle),
        "clicked",
        "the last user turn has a raw toggle"
    );
    settle();
    assert_eq!(
        eval(tab, raw_text),
        RAW_SOURCE,
        "the raw view is the text as typed, whitespace intact"
    );
    assert_eq!(eval(tab, toggle), "clicked");
    settle();
    assert_eq!(eval(tab, raw_count), 0, "toggled back: rendered again");
    // Global: every mounted user turn, and it survives a reload.
    assert_eq!(eval(tab, global), "clicked", "the global switch exists");
    settle();
    let (raws, users) = (eval(tab, raw_count), eval(tab, user_count));
    assert!(
        raws.as_i64().unwrap_or(0) >= 1 && raws == users,
        "every mounted user turn shows raw: {raws} of {users}"
    );
    assert_eq!(
        eval(tab, raw_text),
        RAW_SOURCE,
        "…the last one exactly as typed"
    );
    eval(tab, "location.reload(); 'ok'");
    std::thread::sleep(Duration::from_millis(1500));
    until(
        tab,
        &format!("{user_count} >= 1"),
        "the page to come back after the reload",
        Duration::from_secs(30),
        user_count,
    );
    jump_to_end(tab, surface);
    settle();
    settle();
    let (raws, users) = (eval(tab, raw_count), eval(tab, user_count));
    assert!(
        raws.as_i64().unwrap_or(0) >= 1 && raws == users,
        "after a reload the preference holds: {raws} of {users}"
    );
    assert_eq!(eval(tab, global), "clicked");
    settle();
    assert_eq!(eval(tab, raw_count), 0, "global off: rendered again");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_shows_a_user_turn_as_raw_text() {
    let _serial = serial();
    let fx = raw_fixture("scenario-raw-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_raw_text_of_a_user_turn(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_shows_a_user_turn_as_raw_text() {
    let _serial = serial();
    let fx = raw_fixture("scenario-raw-app");
    let page = open(Surface::AppShell, &fx, 2883);
    scenario_raw_text_of_a_user_turn(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then a prompt whose indentation and spacing markdown would lose.
fn raw_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at(
        "  two leading spaces\\nword  gap   wider\\nlast line",
        &now_minus(40),
    );
    transcript += &assistant_at("answer raw: noted the spacing", &now_minus(30));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: the tool filter hides, keeps landmarks, opens the hits, lands, restores (#110)

/// Row 5.8 of design/rendering-parity-audit.md. Selecting a tool leaves only its rows (open)
/// and the dimmed user turns; answers and other tools are gone; the view lands on the hit;
/// clearing brings everything back with the folds as they were.
fn scenario_tool_filter_hides_and_lands(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // A tool sits inside an activity row; visibility is the ROW's (the tool's own element has no
    // height while the activity is folded), and "open" is the tool's fold itself.
    let (select, clear, read_state, bash_visible, answers_visible, dimmed_turns) = match surface {
        Surface::Classic => (
            "(function(){ var b = document.getElementById('btn-tools'); if (b) b.click(); var it = document.querySelector('.tool-item[data-label=\"Read\"]'); if (!it) return 'no item'; it.click(); return 'selected'; })()",
            "(function(){ var x = document.querySelector('.tf-x'); if (x) { x.click(); return 'cleared'; } var it = document.querySelector('.tool-item[data-label=\"Read\"]'); if (it) { it.click(); return 'cleared'; } return 'no clear'; })()",
            "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-tool=\"Read\"]')].pop(); if (!f) return 'absent'; var row = f; while (row.parentElement && row.parentElement.closest('.fold')) row = row.parentElement.closest('.fold'); var r = row.getBoundingClientRect(); return (r.height > 0 ? 'visible' : 'hidden') + ':' + (f.dataset.open === '1' ? 'open' : 'closed') + ':' + (r.top >= 0 && r.top < innerHeight ? 'inview' : 'offscreen'); })()",
            "[...document.querySelectorAll('#stream .fold[data-tool=\"Bash\"]')].map(function (f) { var row = f; while (row.parentElement && row.parentElement.closest('.fold')) row = row.parentElement.closest('.fold'); return row; }).filter(function (row) { return row.getBoundingClientRect().height > 0; }).length",
            "[...document.querySelectorAll('#stream .ablock')].filter(function (f) { return f.getBoundingClientRect().height > 0; }).length",
            "document.querySelectorAll('#stream .uturn.filter-dim').length",
        ),
        Surface::AppShell => (
            "(function(){ var it = document.querySelector('.tool-type-option[data-tool-filter=\"Read\"]'); if (!it) { document.getElementById('filterTranscriptBtn').click(); it = document.querySelector('.tool-type-option[data-tool-filter=\"Read\"]'); } if (!it) return 'no item'; it.click(); return 'selected'; })()",
            "(function(){ var x = document.getElementById('clearTranscriptFilters'); if (!x) return 'no clear'; x.click(); return 'cleared'; })()",
            "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"]')].pop(); if (!t) return 'absent'; var row = t.closest('.process-event') || t; var r = row.getBoundingClientRect(); var s = document.querySelector('.transcript').getBoundingClientRect(); var ren = t.querySelector(':scope > .renderer'); return (r.height > 0 ? 'visible' : 'hidden') + ':' + (ren && ren.classList.contains('closed') ? 'closed' : 'open') + ':' + (r.top >= s.top && r.top < s.bottom ? 'inview' : 'offscreen'); })()",
            "[...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"]')].map(function (t) { return t.closest('.process-event') || t; }).filter(function (row) { return row.getBoundingClientRect().height > 0; }).length",
            "[...document.querySelectorAll('.virtual-window > .turn.assistant')].filter(function (f) { return f.getBoundingClientRect().height > 0; }).length",
            "document.querySelectorAll('.turn.user.filter-dim').length",
        ),
    };
    let before = eval(tab, read_state);
    assert!(
        before.as_str().unwrap_or("").starts_with("hidden")
            || before.as_str().unwrap_or("").contains(":closed:"),
        "before filtering the Read row is a closed fold (or inside one): {before}"
    );
    assert!(
        eval(tab, bash_visible).as_i64().unwrap_or(0) >= 1,
        "Bash rows are visible before filtering"
    );
    assert!(
        eval(tab, answers_visible).as_i64().unwrap_or(0) >= 1,
        "answers are visible before filtering"
    );
    assert_eq!(
        eval(tab, select),
        "selected",
        "the Read tool can be selected in the filter"
    );
    settle();
    settle();
    let state = eval(tab, read_state);
    assert_eq!(
        state, "visible:open:inview",
        "the Read row is visible, open and landed on: {state}"
    );
    assert_eq!(eval(tab, bash_visible), 0, "Bash rows are hidden");
    assert_eq!(eval(tab, answers_visible), 0, "answers are hidden");
    assert!(
        eval(tab, dimmed_turns).as_i64().unwrap_or(0) >= 1,
        "user turns stay as dimmed landmarks"
    );
    assert_eq!(eval(tab, clear), "cleared", "the filter clears");
    settle();
    settle();
    assert!(
        eval(tab, bash_visible).as_i64().unwrap_or(0) >= 1,
        "Bash rows are back"
    );
    assert!(
        eval(tab, answers_visible).as_i64().unwrap_or(0) >= 1,
        "answers are back"
    );
    assert_eq!(eval(tab, dimmed_turns), 0, "nothing is dimmed");
    let after = eval(tab, read_state);
    assert!(
        after.as_str().unwrap_or("").contains(":closed:"),
        "the Read fold is back to closed, as it was: {after}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_tool_filter_hides_and_lands() {
    let _serial = serial();
    let fx = filter_fixture("scenario-filter-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_tool_filter_hides_and_lands(&page.tab, Surface::Classic, &fx);
}

// ── scenario: a scroll the main thread delivered LATE is still the reader's (#156) ──────────

/// #156, reproduced deterministically rather than 4-times-in-8. A scroll event carries the time
/// it was CREATED, but the handler that classifies it runs whenever the main thread is free — and
/// a long task holds the queue. Measured on the classic page before the fix: 908ms of handler lag
/// turned the reader's own scroll into "560ms since input", outside the 300ms intent window, so a
/// following page called it displacement and healed them back to the tail, throwing away the
/// 1400px they had just scrolled. On the event's clock that scroll is -348ms from the input.
///
/// The case blocks the thread ON PURPOSE, so the lag is not left to chance: scroll away from the
/// tail, then hold the thread through the intent window, then let the handler run. The reader
/// must still be where they put themselves.
fn scenario_a_late_scroll_is_still_the_readers(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let gap_of = match surface {
        Surface::Classic => "Math.round(document.scrollingElement.scrollHeight - document.scrollingElement.clientHeight - document.scrollingElement.scrollTop)",
        Surface::AppShell => "(function(){ var t = document.querySelector('.transcript'); return Math.round(t.scrollHeight - t.clientHeight - t.scrollTop); })()",
    };
    // One gesture and one move, then BLOCK — all inside a single task, so the scroll event cannot
    // be delivered until the block ends and the intent window has already gone by.
    let shove = match surface {
        Surface::Classic => "(function(){ var want = Math.max(0, document.scrollingElement.scrollTop - 1400); dispatchEvent(new WheelEvent('wheel', { deltaY: -1400, bubbles: true })); scrollTo({ top: want, behavior: 'instant' }); var until = performance.now() + 900; while (performance.now() < until) {} return 'shoved'; })()",
        Surface::AppShell => "(function(){ var t = document.querySelector('.transcript'); var want = Math.max(0, t.scrollTop - 1400); t.dispatchEvent(new WheelEvent('wheel', { deltaY: -1400, bubbles: true })); t.scrollTo({ top: want, behavior: 'instant' }); var until = performance.now() + 900; while (performance.now() < until) {} return 'shoved'; })()",
    };
    assert_eq!(
        eval(tab, shove),
        "shoved",
        "the reader scrolled and the thread stalled"
    );
    settle();
    settle();
    let gap = eval(tab, gap_of).as_f64().unwrap_or(0.0);
    assert!(
        gap > 400.0,
        "a scroll delivered late is still the reader's — the page must not heal them back to the \
         tail because its own handler ran after the intent window: gap {gap}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_late_scroll_is_still_the_readers() {
    let _serial = serial();
    let fx = fixture("scenario-late-scroll-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_late_scroll_is_still_the_readers(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_late_scroll_is_still_the_readers() {
    let _serial = serial();
    let fx = fixture("scenario-late-scroll-app", 40);
    let page = open(Surface::AppShell, &fx, 2894);
    scenario_a_late_scroll_is_still_the_readers(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: wrap reaches every rendering that can hold a long line (#161) ─────────────────

/// A transcript whose every long-line rendering carries one unbroken 420-character line: a Bash
/// result, a Read with numbered source, a user turn, and an assistant answer. The marker is the
/// same in each so a failure names which one did not wrap.
fn long_line_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let long: String = "LONGLINE_".to_string() + &"x".repeat(420);
    // Long enough that the page is several viewports tall, which is what the opener waits for.
    let mut transcript = long_session(14, Shape::default());
    transcript += &user_at(&format!("look at this: {long}"), &now_minus(120));
    transcript += &tool_open_at("t-long", &now_minus(110));
    transcript += &tool_result_text(
        "t-long",
        &format!("first line\\n{long}\\nlast line"),
        &now_minus(100),
    );
    transcript += &assistant_at(&format!("the output was {long}"), &now_minus(90));
    transcript += &write_tool_at("t-long-write", "/tmp/long.py", 6, &now_minus(80));
    transcript += &assistant_at("done", &now_minus(70));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 15,
    }
}

/// Row for #161. With "wrap long lines" ON, NOTHING in the transcript scrolls sideways — asked
/// of every element rather than of a named list, because a list-shaped test would have passed
/// all along while a plain tool-output `pre` quietly ignored the preference.
fn scenario_wrap_reaches_every_rendering(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Open every fold first, so the long lines are mounted and the code panes exist, then make
    // sure wrap is ON. The two pages DEFAULT differently — the classic page wraps out of the box,
    // the shell does not — so this reads the state and toggles only if it has to.
    match surface {
        Surface::Classic => {
            eval(tab, "(function(){ document.querySelectorAll('#stream .fold').forEach(function (f) { if (f.dataset.open === '0') { var h = f.querySelector('.fold-h'); if (h) h.click(); } }); return 'opened'; })()");
        }
        Surface::AppShell => {
            for _ in 0..2 {
                eval(tab, "(function(){ document.querySelectorAll('.process-surface.closed [data-process-toggle]').forEach(function (b) { b.click(); }); document.querySelectorAll('[data-process-more][aria-expanded=\"false\"]').forEach(function (b) { b.click(); }); document.querySelectorAll('.renderer.closed > button.renderer-head').forEach(function (h) { h.click(); }); document.querySelectorAll('.cap-more-btn').forEach(function (b) { b.click(); }); return 'opened'; })()");
                settle();
            }
        }
    }
    settle();
    let wrapped = match surface {
        // `.ms-wrap` carries `on` when wrapping is OFF (the button offers the other mode).
        Surface::Classic => eval(tab, "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-wrap')].pop(); if (!b) return 'no bar'; if (b.classList.contains('on')) b.click(); return 'wrapping'; })()"),
        // #173: the reading popover's "Wrap long lines" row is gone — a page-wide control that
        // sat in a menu while an identical-looking one sat on every block. The BASELINE now moves
        // by keyboard only (`w`, shared/keymap.js:21), which is the route this drives; the bars on
        // the blocks set per-block overrides and would answer only for their own block.
        Surface::AppShell => {
            if eval(tab, "document.getElementById('app').classList.contains('wrap-code')")
                .as_bool()
                != Some(true)
            {
                eval(tab, "(function(){ var t = document.querySelector('.transcript'); if (t) t.focus(); return 'ok'; })()");
                key(tab, "w", false);
                settle();
            }
            eval(tab, "document.getElementById('app').classList.contains('wrap-code') ? 'wrapping' : 'still off'")
        }
    };
    assert_eq!(
        wrapped.as_str().unwrap_or(""),
        "wrapping",
        "the page is set to wrap long lines: {wrapped}"
    );
    settle();
    settle();
    let scope = match surface {
        Surface::Classic => "#stream",
        Surface::AppShell => ".transcript",
    };
    let edge_of = match surface {
        Surface::Classic => "document.scrollingElement",
        Surface::AppShell => "document.querySelector('.transcript')",
    };
    // Two ways a long line defeats the preference, and the case has to see both: the element
    // SCROLLS sideways (it clips or offers a scrollbar), or it simply SPILLS past the reading
    // column's right edge. Checking `scrollWidth` alone misses the spill; checking the edge
    // alone misses a `pre` that quietly scrolls inside its own box.
    let probe_js = format!(
        "(function(){{ var root = document.querySelector('{scope}'); if (!root) return {{ err: 'no root' }}; \
         var sc = {edge_of}; var edge = sc.getBoundingClientRect().left + sc.clientWidth; \
         var bad = []; \
         [...root.querySelectorAll('*')].forEach(function (e) {{ \
           var t = (e.textContent || ''); if (t.indexOf('LONGLINE_') < 0 && t.indexOf('SENTINEL_') < 0) return; \
           if ([...e.children].some(function (c) {{ var s = (c.textContent || ''); return s.indexOf('LONGLINE_') >= 0 || s.indexOf('SENTINEL_') >= 0; }})) return; \
           var cs = getComputedStyle(e); \
           var scrolls = e.scrollWidth - e.clientWidth > 1; \
           var spills = e.getBoundingClientRect().right > edge + 1; \
           var stuck = getComputedStyle(e).whiteSpace === 'pre'; \
           if (!scrolls && !spills && !stuck) return; \
           bad.push((e.tagName + '.' + (typeof e.className === 'string' ? e.className : '')).slice(0, 40) + (scrolls ? ' scrolls ' + Math.round(e.scrollWidth - e.clientWidth) : '') + (spills ? ' spills ' + Math.round(e.getBoundingClientRect().right - edge) : '') + (stuck ? ' white-space:pre' : '')); }}); \
         return {{ overflowing: bad.length, which: bad.slice(0, 8), marked: root.textContent.indexOf('LONGLINE_') >= 0 }}; }})()"
    );
    let seen = probe(tab, &probe_js);
    assert_eq!(
        seen["marked"], true,
        "the long lines are mounted, or this proves nothing: {seen}"
    );
    assert_eq!(
        seen["overflowing"].as_i64().unwrap_or(-1),
        0,
        "with wrap ON nothing holding a long line scrolls sideways: {seen}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_wrap_reaches_every_rendering() {
    let _serial = serial();
    let fx = long_line_fixture("scenario-wrap-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_wrap_reaches_every_rendering(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_wrap_reaches_every_rendering() {
    let _serial = serial();
    let fx = long_line_fixture("scenario-wrap-app");
    let page = open(Surface::AppShell, &fx, 2886);
    scenario_wrap_reaches_every_rendering(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the spot controls hold their own slot (#162) ──────────────────────────────────

/// Rule from three owner reports: the anchor and the raw toggle must sit IN their row, never on
/// top of a trailing chip and never past the scroller's content edge. The classic page has always
/// done it — `.alink` is a flex item with `margin-left:auto` in a fold head, so it cannot land on
/// anything — and the app shell hung both controls off negative offsets instead (measured before
/// the fix: the raw toggle rendered at 1388..1412 against a content edge of 1390, and a fold
/// head's anchor at 1313..1337 over a tail chip at ~1309).
///
/// Hit-tested, not measured: a control that is covered still reports a full-size rect, and that
/// is exactly the failure mode here.
fn scenario_spot_controls_hold_their_slot(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (scroller, spots) = match surface {
        Surface::Classic => (
            "document.scrollingElement",
            "#stream .alink, #stream .rawbtn",
        ),
        Surface::AppShell => (
            "document.querySelector('.transcript')",
            ".transcript .spot-link",
        ),
    };
    let probe_js = format!(
        "(function(){{ var sc = {scroller}; var edge = sc.getBoundingClientRect().left + sc.clientWidth;          var all = [...document.querySelectorAll('{spots}')].filter(function (e) {{ var r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; }});          var past = all.filter(function (e) {{ return e.getBoundingClientRect().right > edge + 0.5; }}).length;          var overlaps = 0, sample = null;          all.forEach(function (e) {{ var r = e.getBoundingClientRect();            var row = e.closest('.fold-h, .renderer-head, .turn, .uturn') || e.parentElement;            if (!row) return;            [...row.querySelectorAll('*')].forEach(function (o) {{              if (o === e || e.contains(o) || o.contains(e)) return;              if (o.children.length || !(o.textContent || '').trim()) return;              var b = o.getBoundingClientRect(); if (!b.width || !b.height) return;              if (b.left < r.right - 0.5 && b.right > r.left + 0.5 && b.top < r.bottom - 0.5 && b.bottom > r.top + 0.5) {{                overlaps++; if (!sample) sample = (o.className || o.tagName) + ':' + (o.textContent || '').trim().slice(0, 18); }} }}); }});          return {{ n: all.length, past: past, overlaps: overlaps, sample: sample, edge: Math.round(edge) }}; }})()"
    );
    let seen = probe(tab, &probe_js);
    assert!(
        seen["n"].as_i64().unwrap_or(0) >= 1,
        "the page has spot controls to check: {seen}"
    );
    assert_eq!(
        seen["past"].as_i64().unwrap_or(-1),
        0,
        "no spot control reaches past the scroller's content edge, where the scrollbar is: {seen}"
    );
    assert_eq!(
        seen["overlaps"].as_i64().unwrap_or(-1),
        0,
        "…and none of them sits on top of text in its own row: {seen}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_spot_controls_hold_their_slot() {
    let _serial = serial();
    let fx = filter_fixture("scenario-spot-slot-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_spot_controls_hold_their_slot(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_spot_controls_hold_their_slot() {
    let _serial = serial();
    let fx = filter_fixture("scenario-spot-slot-app");
    let page = open(Surface::AppShell, &fx, 2885);
    scenario_spot_controls_hold_their_slot(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a filter big enough to take the SPARSE window (#140 step 3) ───────────────────

/// What the window mounts, what index range it covers, and whether anything the filter hid got
/// mounted anyway. `#stream`'s children are the two pads with the materialized run between
/// them, and `matBlock` stamps each one: `data-idx` is its record index, `data-kind` its kind,
/// and a filter marks a turn record `filter-dim` and a matching record's header `filter-hit`.
/// So a mounted element that is neither is a record the filter hid — a `stray`.
const WINDOW_PROBE: &str = "(function(){ var els = [...document.querySelectorAll('#vwin > [data-idx]')]; if (!els.length) return { mounted: 0, span: 0, stray: 0, pads: 0 }; var idxs = els.map(function (e) { return +e.dataset.idx; }); var lo = Math.min.apply(null, idxs), hi = Math.max.apply(null, idxs); var stray = els.filter(function (e) { return e.dataset.kind !== 'user' && e.dataset.kind !== 'command' && !e.querySelector('.fold-h.filter-hit'); }).length; var pads = [...document.querySelectorAll('#stream > .vpad')].reduce(function (a, p) { return a + p.getBoundingClientRect().height; }, 0); return { mounted: els.length, lo: lo, hi: hi, span: hi - lo + 1, stray: stray, pads: Math.round(pads) }; })()";

/// Row 5.8 of design/rendering-parity-audit.md, and the case #140 step 3 asks for. Under a
/// filter the classic page is SPARSE: it mounts only the records that match (plus the turn
/// records, which a filter dims rather than hides) and lets the pads absorb everything between
/// them, so the window's index SPAN runs far past the handful of elements in it. The range walk
/// is what makes that possible — `effH` is 0 for a hidden record, so the walk crosses a run of
/// them without spending any of its pixel budget.
///
/// No filter case before this one exercised that path at all: the page renders a filter's
/// visible set in FULL while it is small (`filterFull` in export.js — at most 50 hits and at
/// most 400 visible records), and every existing fixture is well under the ceiling, so their
/// filters mount `[0, N)` and the sparse window never runs. This fixture is over it.
///
/// Classic-only on purpose. The app shell's filter is a SEARCH BY KIND (#133): it hides
/// nothing, so it has no sparse window to have — `app_shell_the_tool_filter_is_a_search_by_kind`
/// is what it must do instead.
#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_sparse_filter_mounts_only_what_matches() {
    let _serial = serial();
    let fx = sparse_filter_fixture("scenario-sparse-filter");
    let page = open(Surface::Classic, &fx, 0);
    let tab = &page.tab;
    jump_to_end(tab, Surface::Classic);
    await_tail(tab, Surface::Classic, "a fresh open to land at the tail");
    settle();

    // The control: with no filter the window is CONTIGUOUS — every index in its span is
    // mounted. That is also what proves the probe reads what it claims to.
    let plain = probe(tab, WINDOW_PROBE);
    let plain_mounted = plain["mounted"].as_i64().unwrap_or(0);
    assert!(plain_mounted >= 3, "the fixture is mounted: {plain}");
    assert_eq!(
        plain["span"], plain["mounted"],
        "unfiltered, the window mounts every index in its span: {plain}"
    );
    assert!(
        plain["pads"].as_i64().unwrap_or(0) > 0,
        "…and it is a WINDOW: the pads hold the rest of the transcript: {plain}"
    );

    let select = "(function(){ var b = document.getElementById('btn-tools'); if (b) b.click(); var it = document.querySelector('.tool-item[data-label=\"Read\"]'); if (!it) return 'no item'; it.click(); return 'selected'; })()";
    assert_eq!(eval(tab, select), "selected", "the Read filter is selected");
    settle();
    settle();

    // Away from the tail, so the window is one the range walk built rather than the converge.
    scroll_by(tab, Surface::Classic, -4000);
    settle();
    settle();
    let sparse = probe(tab, WINDOW_PROBE);
    let mounted = sparse["mounted"].as_i64().unwrap_or(0);
    let span = sparse["span"].as_i64().unwrap_or(0);
    assert!(
        mounted >= 3,
        "the filtered window still has records in it: {sparse}"
    );
    assert_eq!(
        sparse["stray"].as_i64().unwrap_or(-1),
        0,
        "every mounted record is one the filter kept — a turn or a match: {sparse}"
    );
    // Measured on this fixture: 118 mounted across 263 indices, 0 strays. With `isHiddenRec`
    // forced to `false` — the dense model — the same case reads 107 across 107 with 60 strays,
    // so the two models are 1.0x and 2.2x and the floor below separates them with room to spare.
    assert!(
        span * 2 >= mounted * 3,
        "the window is SPARSE: its index span runs far past what it mounts, because the walk \
         crosses a hidden record for free ({mounted} mounted across {span} indices): {sparse}"
    );
    assert!(
        sparse["pads"].as_i64().unwrap_or(0) > 0,
        "…and the pads still absorb what is not mounted: {sparse}"
    );
}

/// The app shell's filter is a SEARCH BY KIND (#133), so this rule is the classic page's alone
/// — a deliberate divergence, not a gap. What the app shell must do instead is
/// `app_shell_the_tool_filter_is_a_search_by_kind` below.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_tool_filter_is_a_search_by_kind() {
    let _serial = serial();
    let fx = filter_fixture("scenario-filter-app");
    let page = open(Surface::AppShell, &fx, 2884);
    let tab = &page.tab;
    let surface = Surface::AppShell;
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let height = "Math.round(document.querySelector('.transcript').scrollHeight)";
    let hidden = "document.querySelectorAll('.filter-hidden').length";
    let count = "document.getElementById('transcriptSearchCount').textContent.trim()";
    // A tool sits inside an activity row, so visibility is the ROW's — the tool's own element
    // has no height while its process is folded (the probe the classic-page case uses).
    let bash_rows = "[...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"]')].map(function (t) { return t.closest('.process-event') || t; }).filter(function (row) { return row.getBoundingClientRect().height > 0; }).length";
    let answers = "[...document.querySelectorAll('.virtual-window > .turn.assistant')].filter(function (f) { return f.getBoundingClientRect().height > 0; }).length";
    let before_height = eval(tab, height).as_i64().unwrap_or(0);
    let before_bash = eval(tab, bash_rows).as_i64().unwrap_or(0);
    assert!(
        before_bash >= 1 && before_height > 0,
        "the fixture is mounted"
    );
    assert_eq!(
        eval(tab, "(function(){ var it = document.querySelector('.tool-type-option[data-tool-filter=\"Read\"]'); if (!it) { document.getElementById('filterTranscriptBtn').click(); it = document.querySelector('.tool-type-option[data-tool-filter=\"Read\"]'); } if (!it) return 'no item'; it.click(); return 'selected'; })()"),
        "selected",
        "the Read tool can be selected in the filter"
    );
    settle();
    settle();
    // Nothing is hidden, and nothing changed height: the window stays dense (#133).
    assert_eq!(
        eval(tab, hidden).as_i64().unwrap_or(-1),
        0,
        "a filter hides nothing on this shell"
    );
    assert!(
        eval(tab, bash_rows).as_i64().unwrap_or(0) >= 1,
        "the calls the filter did NOT match are still there to read around the ones it did"
    );
    assert!(
        eval(tab, answers).as_i64().unwrap_or(0) >= 1,
        "…and so are the answers"
    );
    // Nothing is taken AWAY. The page may well grow — a match opens its fold so the reader can
    // see it — but under the classic page's rule it would shrink to almost nothing here.
    let after_height = eval(tab, height).as_i64().unwrap_or(0);
    assert!(
        after_height >= before_height - 8,
        "the transcript did not shrink: {before_height} -> {after_height}"
    );
    // It counts, the way a search counts.
    let said = eval(tab, count);
    assert_eq!(
        said.as_str().unwrap_or(""),
        "1 match",
        "the box says what the filter found: {said}"
    );
    // The match is marked and the view landed on it, with its surroundings on screen.
    let landed = probe(tab, "(function(){ var v = document.querySelector('.transcript').getBoundingClientRect(); var head = document.querySelector('.renderer-turn[data-tool-name=\"Read\"] .renderer-head.filter-hit'); if (!head) return { marked: false }; var r = head.getBoundingClientRect(); var seen = [...document.querySelectorAll('.virtual-window [data-block-index]')].filter(function (e) { var b = e.getBoundingClientRect(); return b.height > 0 && b.bottom > v.top && b.top < v.bottom; }); var others = seen.filter(function (e) { return !e.querySelector(':scope > .renderer-head.filter-hit'); }).length; return { marked: true, inview: r.top >= v.top - 1 && r.top <= v.bottom, others: others }; })()");
    assert_eq!(
        landed["marked"], true,
        "the matching head is marked: {landed}"
    );
    assert_eq!(
        landed["inview"], true,
        "…and the view landed on it: {landed}"
    );
    assert!(
        landed["others"].as_i64().unwrap_or(0) >= 1,
        "…with records the filter did NOT match on screen beside it, which is the point of not hiding: {landed}"
    );
    // A matching call that arrives live joins the count.
    let growth = LiveGrowth::start(
        fx.path.clone(),
        vec![
            assistant_at("reading one more file", &now_minus(20)),
            read_tool_at("t-late-read", "/tmp/other.toml", &now_minus(19)),
            tool_result_lines("t-late-read", 4, &now_minus(18)),
        ],
        Duration::from_millis(900),
    );
    growth.finish(Duration::from_secs(40));
    until(
        tab,
        "document.getElementById('transcriptSearchCount').textContent.trim() === '2 matches'",
        "the arriving Read call to join the count",
        Duration::from_secs(25),
        count,
    );
    assert_eq!(
        eval(tab, hidden).as_i64().unwrap_or(-1),
        0,
        "…and still nothing is hidden"
    );
    // Stepping walks the matches, as it walks search hits.
    let at = "(function(){ var v = document.querySelector('.transcript').getBoundingClientRect(); var hit = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"]')].find(function (t) { var r = t.getBoundingClientRect(); return r.bottom > v.top && r.top < v.bottom; }); return hit ? hit.dataset.blockIndex : ''; })()";
    // Let the arrival's own apply finish before stepping: a jump issued mid-apply is undone by
    // the anchor restore that follows it.
    settle();
    settle();
    // Stepping walks the matches, as it walks search hits — wherever the growth left the view,
    // pressing next visits BOTH of them.
    let mut visited = std::collections::BTreeSet::new();
    for _ in 0..3 {
        let seen = eval(tab, at).as_str().unwrap_or("").to_string();
        if !seen.is_empty() {
            visited.insert(seen);
        }
        eval(tab, "document.getElementById('findNext').click(); 'ok'");
        settle();
        settle();
    }
    assert!(
        visited.len() >= 2,
        "stepping visits every match, not just the one it starts on: {visited:?}"
    );
}

/// Twelve turns of Bash, then a turn with one Read among the Bash calls.
/// A transcript over the page's `filterFull` ceiling (export.js: at most 50 hits and at most
/// 400 visible records), so a filter on it takes the WINDOWED path. 160 turns, each a question,
/// a deliberation, one tool call and an answer; every third turn reaches for `Read` and the rest
/// for `Bash`, which puts 54 `Read` hits on the page — four over the ceiling — with long runs of
/// hidden records between them for the range walk to cross.
fn sparse_filter_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let turns = 160u32;
    let mut transcript = String::new();
    for i in 0..turns {
        let s = 4000 - u64::from(i) * 20;
        transcript += &user_at(&format!("question {i}: what is in the log"), &now_minus(s));
        transcript += &thinking_at(
            &format!("deliberation {i}: weighing the options carefully. "),
            &now_minus(s - 4),
        );
        if i % 3 == 0 {
            transcript += &named_tool_at(
                &format!("r{i}"),
                "Read",
                &format!("/tmp/log-{i}.txt"),
                &now_minus(s - 8),
            );
        } else {
            transcript += &tool_open_at(&format!("t{i}"), &now_minus(s - 8));
            transcript += &tool_result_at(&format!("t{i}"), &now_minus(s - 10));
        }
        transcript += &assistant_at(
            &format!("answer {i}: sed do eiusmod tempor incididunt ut labore."),
            &now_minus(s - 12),
        );
    }
    let path = stores.claude_session(SID, &transcript);
    Fixture { base, path, turns }
}

fn filter_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at(
        "question filter: read the config then check it",
        &now_minus(90),
    );
    transcript += &assistant_at("Reading the config first.", &now_minus(85));
    transcript += &read_tool_at("t-filter-read", "/tmp/config.toml", &now_minus(70));
    transcript += &tool_result_lines("t-filter-read", 5, &now_minus(60));
    transcript += &assistant_at("Now checking it.", &now_minus(55));
    transcript += &tool_open_at("t-filter-bash", &now_minus(50));
    transcript += &tool_result_lines("t-filter-bash", 3, &now_minus(40));
    transcript += &assistant_at("answer filter: the config is fine", &now_minus(30));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: every hit in the window is marked; a JSON field name finds nothing (#111) ────

/// Rows 5.3 and 5.4 of design/rendering-parity-audit.md. Searching a word present in a prompt
/// and twice in the answer marks all three on screen, one of them current after a step; a query
/// that is only a JSON field name of the records matches nothing on either page.
fn scenario_every_hit_marked_and_text_haystack(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (type_query, next, marks, current) = match surface {
        Surface::Classic => (
            "(function(q){ var i = document.getElementById('q'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ var b = document.getElementById('qnext'); if (b) { b.click(); return 'next'; } var i = document.getElementById('q'); i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return 'enter'; })()",
            "document.querySelectorAll('#stream mark.hl').length",
            "document.querySelectorAll('#stream mark.hl.cur').length",
        ),
        Surface::AppShell => (
            "(function(q){ var i = document.getElementById('transcriptSearchInput'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ document.getElementById('findNext').click(); return 'next'; })()",
            "document.querySelectorAll('.virtual-window mark.search-mark').length",
            "document.querySelectorAll('.virtual-window mark.search-mark.current').length",
        ),
    };
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    let n = eval(tab, marks).as_i64().unwrap_or(0);
    assert!(
        n >= 3,
        "every occurrence on screen is marked (prompt + twice in the answer): {n}"
    );
    eval(tab, next);
    settle();
    assert_eq!(
        eval(tab, current),
        1,
        "after a step exactly one mark is the current one"
    );
    assert!(
        eval(tab, marks).as_i64().unwrap_or(0) >= 3,
        "…and the others stay marked"
    );
    // A JSON field name every record carries ("label", "kind") is not text a reader can see: no
    // marks, and the count says none.
    let count = match surface {
        Surface::Classic => "(function(){ var c = document.getElementById('qcount'); return c ? c.textContent.trim() : ''; })()",
        Surface::AppShell => "document.getElementById('transcriptSearchCount').textContent.trim()",
    };
    for field in ["label", "kind"] {
        eval(tab, &format!("{type_query}('{field}')"));
        settle();
        settle();
        assert_eq!(eval(tab, marks), 0, "the field name {field} marks nothing");
        let c = eval(tab, count).as_str().unwrap_or("").to_string();
        assert!(
            c.is_empty() || c.starts_with('0'),
            "…and the count says none for {field}: {c:?}"
        );
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_marks_every_hit_and_searches_text() {
    let _serial = serial();
    let fx = search_fixture("scenario-search-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_every_hit_marked_and_text_haystack(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_marks_every_hit_and_searches_text() {
    let _serial = serial();
    let fx = search_fixture("scenario-search-app");
    let page = open(Surface::AppShell, &fx, 2885);
    scenario_every_hit_marked_and_text_haystack(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then a prompt with the word once and an answer with it twice.
fn search_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question search: find the needle here", &now_minus(40));
    transcript += &assistant_at(
        "answer search: the needle and the needle again",
        &now_minus(30),
    );
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: timestamps on user turns — a clock today, the date on older turns (#112) ────

/// Row 3.19 of design/rendering-parity-audit.md. Both pages show when a user turn was sent:
/// a bare clock time for today, the date (and the year when it differs) for older turns.
fn scenario_user_turn_timestamps(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let times = match surface {
        Surface::Classic => "JSON.stringify([...document.querySelectorAll('#stream .uturn .ts')].slice(-2).map(function (e) { return e.textContent; }))",
        Surface::AppShell => "JSON.stringify([...document.querySelectorAll('.turn.user .turn-time')].slice(-2).map(function (e) { return e.textContent; }))",
    };
    let v: Vec<String> =
        serde_json::from_str(eval(tab, times).as_str().unwrap_or("[]")).unwrap_or_default();
    assert_eq!(v.len(), 2, "the last two user turns carry a time: {v:?}");
    let (today, old) = (&v[0], &v[1]);
    assert!(
        regex_lite_time(today) && !today.contains("2025") && !today.contains("2026"),
        "today's turn shows a bare clock time: {today:?}"
    );
    assert!(
        old.contains("2025") && regex_lite_time(old),
        "an older turn carries its date and year: {old:?}"
    );
}

/// Something that looks like `h:mm` is in the text (locale-agnostic on the hour form).
fn regex_lite_time(s: &str) -> bool {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(2))
        .any(|i| b[i].is_ascii_digit() && b[i + 1] == b':' && b[i + 2].is_ascii_digit())
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_shows_user_turn_timestamps() {
    let _serial = serial();
    let fx = time_fixture("scenario-time-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_user_turn_timestamps(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_shows_user_turn_timestamps() {
    let _serial = serial();
    let fx = time_fixture("scenario-time-app");
    let page = open(Surface::AppShell, &fx, 2886);
    scenario_user_turn_timestamps(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns from today, then a turn dated in another year.
fn time_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question time: an old one", "2025-03-09T10:20:00Z");
    transcript += &assistant_at("answer time: noted", "2025-03-09T10:21:00Z");
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: a slash command is a turn — a card with badge and preview, a pane row (#113) ──

/// Row 1.5 of design/rendering-parity-audit.md. A `/command` is the user speaking: it shows as
/// a turn card carrying the command's badge and argument preview, folded until opened (the
/// output inside), and the turns pane lists it like any turn.
fn scenario_command_turn_is_a_turn(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (rows, card, open, out_visible) = match surface {
        Surface::Classic => (
            "document.querySelectorAll('#turnlist .side-item').length",
            "(function(){ var c = [...document.querySelectorAll('#stream .uturn[data-kind=\"command\"]')].pop(); if (!c) return 'none'; var b = c.querySelector('.cmd-badge'), p = c.querySelector('.cmd-preview'); return (b ? b.textContent : '') + '|' + (p ? p.textContent : ''); })()",
            "(function(){ var c = [...document.querySelectorAll('#stream .uturn[data-kind=\"command\"]')].pop(); var h = c && c.querySelector('.fold-h'); if (!h) return 'none'; h.click(); return 'opened'; })()",
            "[...document.querySelectorAll('#stream pre')].some(function (p) { return p.textContent.includes('Compacted 12 turns') && p.getBoundingClientRect().height > 0; })",
        ),
        Surface::AppShell => (
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
            "(function(){ var c = [...document.querySelectorAll('.turn.user.command')].pop(); if (!c) return 'none'; var b = c.querySelector('.command-badge'), p = c.querySelector('.command-preview'); return (b ? b.textContent : '') + '|' + (p ? p.textContent : ''); })()",
            "(function(){ var c = [...document.querySelectorAll('.turn.user.command')].pop(); var h = c && c.querySelector('.command-head'); if (!h) return 'none'; h.click(); return 'opened'; })()",
            "[...document.querySelectorAll('.virtual-window pre')].some(function (p) { return p.textContent.includes('Compacted 12 turns') && p.getBoundingClientRect().height > 0; })",
        ),
    };
    assert_eq!(
        eval(tab, rows),
        13,
        "the turns pane lists the command as the 13th turn"
    );
    let c = eval(tab, card).as_str().unwrap_or("").to_string();
    assert!(
        c.contains("compact") && c.contains("focus on the plan"),
        "the card carries the badge and the argument preview: {c:?}"
    );
    assert_eq!(
        eval(tab, out_visible),
        false,
        "folded by default: the output is not on screen"
    );
    assert_eq!(eval(tab, open), "opened", "the card opens from its head");
    settle();
    assert_eq!(
        eval(tab, out_visible),
        true,
        "…and shows the command's output"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_shows_a_command_turn() {
    let _serial = serial();
    let fx = command_fixture("scenario-command-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_command_turn_is_a_turn(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_shows_a_command_turn() {
    let _serial = serial();
    let fx = command_fixture("scenario-command-app");
    let page = open(Surface::AppShell, &fx, 2887);
    scenario_command_turn_is_a_turn(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then `/compact focus on the plan` with its stdout, and an answer.
fn command_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &command_at(
        "compact",
        "focus on the plan",
        "Compacted 12 turns",
        &now_minus(40),
    );
    transcript += &assistant_at(
        "answer command: continuing from the summary",
        &now_minus(30),
    );
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: the reader's choices survive a reload (and, on the app shell, a switch) (#114)

/// Row 4.4 of design/rendering-parity-audit.md. A fold the reader opened, a cap they expanded
/// and a turn they read raw come back after a reload — and, on the app shell, after switching
/// to another session and back.
fn scenario_view_state_survives(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (read_rows, read_btn, raw_toggle, raw_count, last_turn) = match surface {
        Surface::Classic => (
            "(function(){ var n = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"] .numbered')].pop(); if (!n) return -1; return [...n.querySelectorAll('.nrow')].filter(function (x) { return x.getBoundingClientRect().height > 0; }).length; })()",
            "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"]')].pop(); var b = f ? f.querySelector('button.morebtn') : null; if (b) b.click(); return !!b; })()",
            "(function(){ var t = [...document.querySelectorAll('#stream .uturn .rawbtn')].pop(); if (!t) return 'none'; t.click(); return 'clicked'; })()",
            "document.querySelectorAll('#stream .uturn pre.raw').length",
            "(function(){ var r = [...document.querySelectorAll('#turnlist .side-item')].pop(); if (!r) return 'none'; r.click(); return 'jumped'; })()",
        ),
        Surface::AppShell => (
            "(function(){ var c = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] .codebox')].pop(); if (!c) return -1; return [...c.querySelectorAll('.line')].filter(function (x) { return x.getBoundingClientRect().height > 0; }).length; })()",
            "(function(){ var bs = document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] .cap-more-btn'); var b = bs[bs.length - 1]; if (b) b.click(); return !!b; })()",
            "(function(){ var t = [...document.querySelectorAll('.turn.user .raw-toggle')].pop(); if (!t) return 'none'; t.click(); return 'clicked'; })()",
            "document.querySelectorAll('.turn.user pre.turn-raw-text').length",
            "(function(){ var r = [...document.querySelectorAll('#navigatorTurns .outline-turn-row')].pop(); if (!r) return 'none'; r.click(); return 'jumped'; })()",
        ),
    };
    // Raw the last prompt first, while it is mounted at the tail.
    assert_eq!(
        eval(tab, raw_toggle),
        "clicked",
        "the last prompt has a raw toggle"
    );
    settle();
    assert_eq!(eval(tab, raw_count), 1, "the last prompt reads raw");
    // Open the Read row's chain (activity, then the tool), then expand its cap.
    for _ in 0..6 {
        let step = match surface {
            Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"]')].pop(); if (!f) return 'none'; var chain = []; for (var e = f; e; e = e.parentElement.closest('.fold')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.dataset.open === '0'; }); if (!closed) return 'open'; closed.querySelector('.fold-h').click(); return 'clicked'; })()"),
            Surface::AppShell => eval(tab, "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] > .renderer')].pop(); if (!t) return 'none'; var chain = []; for (var e = t; e; e = e.parentElement && e.parentElement.closest('.renderer')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.classList.contains('closed'); }); if (!closed) return 'open'; closed.querySelector('button.renderer-head').click(); return 'clicked'; })()"),
        };
        settle();
        if step != "clicked" {
            break;
        }
    }
    assert_eq!(eval(tab, read_rows), 10, "the Read row is open and capped");
    assert_eq!(eval(tab, read_btn), true, "its cap expands");
    settle();
    assert_eq!(eval(tab, read_rows), 60, "…to every row");
    settle();
    // Reload, land on the last turn (the prompt at the top, the Read below it): the raw view, the
    // fold and the expansion are as they were.
    let _console = tap_console(tab);
    eval(tab, "location.reload(); 'ok'");
    std::thread::sleep(Duration::from_millis(1500));
    // The remembered expansion makes the last process tall, so no prompt need be in the window
    // at the tail: "back" is the turns pane listing the session again.
    let pane_rows = match surface {
        Surface::Classic => "document.querySelectorAll('#turnlist .side-item').length",
        Surface::AppShell => {
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length"
        }
    };
    until(
        tab,
        &format!("{pane_rows} >= 13"),
        "the page to come back after the reload",
        Duration::from_secs(30),
        pane_rows,
    );
    settle();
    assert_eq!(
        eval(tab, last_turn),
        "jumped",
        "the last turn is in the pane"
    );
    settle();
    settle();
    assert_eq!(
        eval(tab, raw_count),
        1,
        "after the reload the prompt still reads raw"
    );
    assert_eq!(
        eval(tab, read_rows),
        60,
        "…and the Read row is open with its cap expanded"
    );
    if surface == Surface::AppShell {
        // Switch to the other session and back.
        let other = format!("document.querySelector('.tree-row.session[data-session=\"{SID2}\"]')");
        until(
            tab,
            &format!("!!{other}"),
            "the second session in the tree",
            Duration::from_secs(20),
            "document.querySelectorAll('.tree-row.session').length",
        );
        eval(tab, &format!("{other}.click(); 'ok'"));
        until(
            tab,
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length === 3",
            "the second session to open",
            Duration::from_secs(20),
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
        );
        eval(
            tab,
            &format!(
                "document.querySelector('.tree-row.session[data-session=\"{SID}\"]').click(); 'ok'"
            ),
        );
        until(
            tab,
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length === 13",
            "the first session to open again",
            Duration::from_secs(20),
            "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
        );
        settle();
        assert_eq!(eval(tab, last_turn), "jumped");
        settle();
        settle();
        assert_eq!(
            eval(tab, raw_count),
            1,
            "after switching away and back the prompt still reads raw"
        );
        assert_eq!(
            eval(tab, read_rows),
            60,
            "…and the Read row is open with its cap expanded"
        );
    }
}

const SID2: &str = "eeeeeeee-0000-4000-8000-000000000002";

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_keeps_view_state_across_a_reload() {
    let _serial = serial();
    let fx = view_state_fixture("scenario-viewstate-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_view_state_survives(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_keeps_view_state_across_a_reload_and_a_switch() {
    let _serial = serial();
    let fx = view_state_fixture("scenario-viewstate-app");
    let page = open(Surface::AppShell, &fx, 2888);
    scenario_view_state_survives(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns then a narrated Read of 60 lines; and a second, three-turn session to switch to.
fn view_state_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question state: read the long file", &now_minus(80));
    transcript += &assistant_at("Reading the long file.", &now_minus(70));
    transcript += &read_tool_at("t-state-read", "/tmp/long-file.txt", &now_minus(60));
    transcript += &tool_result_lines("t-state-read", 60, &now_minus(50));
    transcript += &assistant_at("answer state: read", &now_minus(40));
    let path = stores.claude_session(SID, &transcript);
    stores.claude_session(SID2, &long_session(3, Shape::default()));
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: the code pane — unselectable gutters, a bar per pane, copy without gutters (#115)

/// Rows 3.2 and 3.3 of design/rendering-parity-audit.md. A numbered pane's gutter never enters
/// a selection; its bar steps the code size, toggles wrap, and copies the code cells alone.
fn scenario_code_pane_bar_and_gutters(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Open the Read row's chain so its pane is on screen.
    for _ in 0..6 {
        let step = match surface {
            Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"]')].pop(); if (!f) return 'none'; var chain = []; for (var e = f; e; e = e.parentElement.closest('.fold')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.dataset.open === '0'; }); if (!closed) return 'open'; closed.querySelector('.fold-h').click(); return 'clicked'; })()"),
            Surface::AppShell => eval(tab, "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] > .renderer')].pop(); if (!t) return 'none'; var chain = []; for (var e = t; e; e = e.parentElement && e.parentElement.closest('.renderer')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.classList.contains('closed'); }); if (!closed) return 'open'; closed.querySelector('button.renderer-head').click(); return 'clicked'; })()"),
        };
        settle();
        if step != "clicked" {
            break;
        }
    }
    let (gutter_select, size_val, size_up, wrap_btn, wrap_state, copy_btn) = match surface {
        Surface::Classic => (
            "(function(){ var g = [...document.querySelectorAll('#stream .numbered .gut')].pop(); return g ? getComputedStyle(g).userSelect : 'none-found'; })()",
            "(function(){ var v = [...document.querySelectorAll('#stream .codebar .ms-val')].pop(); return v ? v.textContent.trim() : 'none'; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-up')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-wrap')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-wrap')].pop(); return b ? b.textContent : ''; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .cpy-code')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
        ),
        Surface::AppShell => (
            "(function(){ var g = [...document.querySelectorAll('.codebox .ln')].pop(); return g ? getComputedStyle(g).userSelect : 'none-found'; })()",
            "(function(){ var v = [...document.querySelectorAll('.codebox [data-code-size-val]')].pop(); return v ? v.textContent.trim() : 'none'; })()",
            "(function(){ var b = [...document.querySelectorAll('.codebox [data-code-size=\"1\"]')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
            "(function(){ var b = [...document.querySelectorAll('.codebox [data-code-wrap]')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
            "(function(){ var b = [...document.querySelectorAll('.codebox [data-code-wrap]')].pop(); return b ? b.textContent : ''; })()",
            "(function(){ var b = [...document.querySelectorAll('.codebox [data-code-copy]')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
        ),
    };
    assert_eq!(
        eval(tab, gutter_select),
        "none",
        "the line-number gutter never enters a selection"
    );
    // The bar reads as a RELATIVE step, not a pixel count (#160) — an absolute number pins the
    // reader to one base size, and the adjustment has always been relative. Each page counts
    // from ITS OWN default, so "0" means "where this page starts" on both.
    let size = eval(tab, size_val);
    assert_eq!(
        size.as_str().unwrap_or(""),
        "0",
        "the pane's bar starts at this page's own size: {size}"
    );
    assert_eq!(
        eval(tab, size_up),
        "clicked",
        "the pane's bar steps the size"
    );
    settle();
    let after = eval(tab, size_val);
    assert_eq!(
        after.as_str().unwrap_or(""),
        "+1",
        "…by one step, and says so relatively: {size} → {after}"
    );
    let glyph = eval(tab, wrap_state).as_str().unwrap_or("").to_string();
    assert_eq!(
        eval(tab, wrap_btn),
        "clicked",
        "the pane's bar toggles wrap"
    );
    settle();
    let glyph_after = eval(tab, wrap_state).as_str().unwrap_or("").to_string();
    assert!(
        glyph != glyph_after && !glyph_after.is_empty(),
        "…and the glyph follows: {glyph:?} → {glyph_after:?}"
    );
    stub_clipboard(tab);
    assert_eq!(eval(tab, copy_btn), "clicked", "the pane's bar copies");
    settle();
    let copied = copied_text(tab);
    let lines: Vec<&str> = copied.lines().collect();
    assert!(
        lines.len() >= 10,
        "the copy holds the pane's lines: {} lines",
        lines.len()
    );
    assert!(
        lines.iter().all(|l| l.starts_with("line ")),
        "…the code cells alone, no gutters: {:?}",
        &lines[..3.min(lines.len())]
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_code_pane_bar_and_gutters() {
    let _serial = serial();
    let fx = view_state_fixture("scenario-codepane-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_code_pane_bar_and_gutters(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_code_pane_bar_and_gutters() {
    let _serial = serial();
    let fx = view_state_fixture("scenario-codepane-app");
    let page = open(Surface::AppShell, &fx, 2889);
    scenario_code_pane_bar_and_gutters(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a deep link to a tool call — copied from its row, landed on with its chain open (#116)

/// Row 3.11 of design/rendering-parity-audit.md. A tool row offers a link to itself; opening
/// the page at that link lands on the row with its fold chain open, on both pages.
fn scenario_deep_link_to_a_tool_row(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // The Read sits inside an activity: open the chain so its row (and its link) is visible.
    for _ in 0..6 {
        let step = match surface {
            Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"]')].pop(); if (!f) return 'none'; var chain = []; for (var e = f; e; e = e.parentElement.closest('.fold')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.dataset.open === '0'; }); if (!closed) return 'open'; closed.querySelector('.fold-h').click(); return 'clicked'; })()"),
            Surface::AppShell => eval(tab, "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] > .renderer')].pop(); if (!t) return 'none'; var chain = []; for (var e = t; e; e = e.parentElement && e.parentElement.closest('.renderer')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.classList.contains('closed'); }); if (!closed) return 'open'; closed.querySelector('button.renderer-head').click(); return 'clicked'; })()"),
        };
        settle();
        if step != "clicked" {
            break;
        }
    }
    stub_clipboard(tab);
    let copied = match surface {
        Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-tool=\"Read\"]')].pop(); var a = f && f.querySelector(':scope > .fold-h a.alink'); if (!a) return 'none'; a.click(); return 'clicked'; })()"),
        Surface::AppShell => eval(tab, "(function(){ var r = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"] > .renderer')].pop(); var b = r && r.querySelector(':scope > .renderer-spot'); if (!b) return 'none'; b.click(); return 'clicked'; })()"),
    };
    assert_eq!(copied, "clicked", "the Read row offers a link to itself");
    settle();
    let link = copied_text(tab);
    assert!(
        link.contains('#'),
        "the link carries the record id: {link:?}"
    );
    // Open the page at the link — a real load, not a same-document hash change (which fires no
    // navigation event): leave the page first. The row is on screen with its chain open.
    tab.navigate_to("about:blank").unwrap();
    tab.wait_until_navigated().unwrap();
    tab.navigate_to(&link).unwrap();
    tab.wait_until_navigated().unwrap();
    let landed = match surface {
        Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-tool=\"Read\"]')].pop(); if (!f) return 'absent'; var row = f; while (row.parentElement && row.parentElement.closest('.fold')) row = row.parentElement.closest('.fold'); var r = f.getBoundingClientRect(); return (f.dataset.open === '1' ? 'open' : 'closed') + ':' + (row.dataset.open === '1' ? 'chain-open' : 'chain-closed') + ':' + (r.height > 0 && r.top >= -2 && r.top < innerHeight ? 'inview' : 'offscreen'); })()",
        Surface::AppShell => "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Read\"]')].pop(); if (!t) return 'absent'; var ren = t.querySelector(':scope > .renderer'); var outer = t.parentElement && t.parentElement.closest('.renderer'); var r = t.getBoundingClientRect(); var s = document.querySelector('.transcript').getBoundingClientRect(); return (ren.classList.contains('closed') ? 'closed' : 'open') + ':' + (outer ? (outer.classList.contains('closed') ? 'chain-closed' : 'chain-open') : 'chain-open') + ':' + (r.height > 0 && r.top >= s.top - 2 && r.top < s.bottom ? 'inview' : 'offscreen'); })()",
    };
    until(
        tab,
        &format!("{landed} === 'open:chain-open:inview'"),
        "the deep link to land on the open Read row",
        Duration::from_secs(30),
        landed,
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_deep_links_to_a_tool_row() {
    let _serial = serial();
    let fx = view_state_fixture("scenario-deeplink-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_deep_link_to_a_tool_row(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_deep_links_to_a_tool_row() {
    let _serial = serial();
    let fx = view_state_fixture("scenario-deeplink-app");
    let page = open(Surface::AppShell, &fx, 2890);
    scenario_deep_link_to_a_tool_row(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: hit stepping shows the term and re-enters from the viewport (#100) ──────────

/// Row 5.5 of design/rendering-parity-audit.md and the owner's report. Every "next" lands with
/// the matched term on screen — a hit deep in a long tool output included — and after the
/// reader scrolls away, "next" is the hit nearest the view, not the one after the old current.
fn scenario_hit_stepping_shows_the_term_and_reenters(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (type_query, next, current_state) = match surface {
        Surface::Classic => (
            "(function(q){ var i = document.getElementById('q'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ var b = document.getElementById('qnext'); if (b) { b.click(); return 'next'; } return 'none'; })()",
            "(function(){ var m = document.querySelector('#stream mark.hl.cur'); if (!m) return 'no current'; var r = m.getBoundingClientRect(); var blk = m.closest('.blk'); return (r.top >= 0 && r.bottom <= innerHeight ? 'inview' : 'offscreen') + ':' + (blk ? (blk.classList.contains('uturn') ? 'prompt' : blk.dataset.kind || blk.className.split(' ')[0]) : '?'); })()",
        ),
        Surface::AppShell => (
            "(function(q){ var i = document.getElementById('transcriptSearchInput'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ document.getElementById('findNext').click(); return 'next'; })()",
            "(function(){ var m = document.querySelector('.virtual-window mark.search-mark.current'); if (!m) return 'no current'; var r = m.getBoundingClientRect(); var s = document.querySelector('.transcript').getBoundingClientRect(); var turn = m.closest('.turn'); return (r.top >= s.top && r.bottom <= s.bottom ? 'inview' : 'offscreen') + ':' + (turn ? (turn.classList.contains('user') ? 'prompt' : turn.dataset.kind || turn.className.split(' ')[0]) : '?'); })()",
        ),
    };
    scroll_by(tab, surface, -40000);
    settle();
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    // Three hits: the prompt, line 55 of a 60-line output, the answer. Every step shows the term.
    for expected in ["prompt", "tool", "assistant"] {
        eval(tab, next);
        settle();
        settle();
        let state = eval(tab, current_state).as_str().unwrap_or("").to_string();
        assert!(
            state.starts_with("inview:"),
            "after a step the term is on screen ({expected}): {state:?}"
        );
        if expected == "prompt" {
            assert!(
                state.ends_with(":prompt"),
                "the first step from the top lands on the prompt: {state:?}"
            );
        }
    }
    // Scrolled away to the top: the next step is the hit nearest the view — the prompt again,
    // not the one after the old current (which would wrap to the prompt too — so go from the
    // middle: land on the output hit, scroll to the top, and the next is the prompt).
    eval(tab, next);
    settle();
    let mid = eval(tab, current_state).as_str().unwrap_or("").to_string();
    assert!(mid.starts_with("inview:"), "{mid:?}");
    scroll_by(tab, surface, -40000);
    settle();
    settle();
    eval(tab, next);
    settle();
    settle();
    let reentered = eval(tab, current_state).as_str().unwrap_or("").to_string();
    assert!(
        reentered.starts_with("inview:") && reentered.ends_with(":prompt"),
        "after scrolling to the top, next re-enters at the prompt, the nearest hit: {reentered:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_hit_stepping_shows_the_term_and_reenters() {
    let _serial = serial();
    let fx = hits_fixture("scenario-hits-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_hit_stepping_shows_the_term_and_reenters(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_hit_stepping_shows_the_term_and_reenters() {
    let _serial = serial();
    let fx = hits_fixture("scenario-hits-app");
    let page = open(Surface::AppShell, &fx, 2891);
    scenario_hit_stepping_shows_the_term_and_reenters(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then a prompt with the word, a 60-line Read with the word on line 55, and an
/// answer with the word.
fn hits_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question hits: where is the needle", &now_minus(90));
    transcript += &assistant_at("Reading the long file.", &now_minus(85));
    transcript += &read_tool_at("t-hits-read", "/tmp/haystack.txt", &now_minus(70));
    let body: String = (1..=60)
        .map(|k| {
            if k == 55 {
                "line 55 has the needle here\\n".to_string()
            } else {
                format!("line {k}\\n")
            }
        })
        .collect();
    transcript += &tool_result_text("t-hits-read", &body, &now_minus(60));
    transcript += &assistant_at("answer hits: the needle is on line 55", &now_minus(30));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// Twelve turns, then the three shapes the shared search rules turn on (#118): a QUEUED prompt
/// (a record no scope class claims), an `Edit` (which both pages DISPLAY as "Update"), and hits
/// far apart so stepping has a nearest one to find.
fn scope_edge_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question one: the needle is here", &now_minus(300));
    transcript += &assistant_at("Editing the file.", &now_minus(290));
    transcript += &edit_tool_at("t-edit", "/tmp/needle-notes.md", &now_minus(280));
    transcript += &tool_result_text("t-edit", "updated /tmp/needle-notes.md", &now_minus(275));
    transcript += &queued_at("queued while busy: the needle again", &now_minus(260));
    transcript += &long_session(12, Shape::default());
    transcript += &user_at("question two: one more needle", &now_minus(60));
    transcript += &assistant_at("Done.", &now_minus(30));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 26,
    }
}

/// The three rules the shared search settles, each of which the app shell read its own way
/// (#118): `w:` alone scopes NOTHING (so a record no class claims is still counted), an edit is
/// marked under `e:` (the gate is the record's kind, not the head's display name), and a step
/// after scrolling re-enters at the nearest hit, not the session's first.
fn scenario_scope_edges_count_mark_and_reenter(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (type_query, total_of, next, marks_in_edit, current_index) = match surface {
        Surface::Classic => (
            "(function(q){ var i = document.getElementById('q'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ var e = document.getElementById('qcount'); return e ? e.textContent.trim() : 'none'; })()",
            "(function(){ var b = document.getElementById('qnext'); if (b) b.click(); return 'next'; })()",
            "document.querySelectorAll('#stream .blk[data-kind=\"edit\"] mark.hl').length",
            "(function(){ var m = document.querySelector('#stream mark.hl.cur'); if (!m) return -1; var blk = m.closest('.blk'); return blk ? Number(blk.dataset.idx) : -1; })()",
        ),
        Surface::AppShell => (
            "(function(q){ var i = document.getElementById('transcriptSearchInput'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ var e = document.getElementById('transcriptSearchCount'); return e ? e.textContent.trim() : 'none'; })()",
            "(function(){ document.getElementById('findNext').click(); return 'next'; })()",
            "document.querySelectorAll('.virtual-window [data-record-kind=\"edit\"] mark.search-mark').length",
            "(function(){ var m = document.querySelector('.virtual-window mark.search-mark.current'); if (!m) return -1; var row = m.closest('[data-block-index]'); return row ? Math.floor(Number(row.dataset.blockIndex)) : -1; })()",
        ),
    };
    // 1. `w:` is whole words EVERYWHERE — the same total as the plain needle, and no scope in
    //    the label. A mask of all seven classes would drop the queued prompt, which no class owns.
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    let plain = eval(tab, total_of).as_str().unwrap_or("").to_string();
    eval(tab, &format!("{type_query}('w:needle')"));
    settle();
    settle();
    let whole = eval(tab, total_of).as_str().unwrap_or("").to_string();
    let hits_of = |label: &str| -> i64 {
        label
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(-1)
    };
    assert!(
        hits_of(&plain) >= 4,
        "the fixture holds the hits: {plain:?}"
    );
    assert_eq!(
        hits_of(&whole),
        hits_of(&plain),
        "whole words alone scopes nothing — every record still counts ({plain:?} vs {whole:?})"
    );
    assert!(
        whole.ends_with("· whole words") && !whole.contains(" in "),
        "…and the label says so without naming a scope: {whole:?}"
    );
    // 2. An edit is marked under `e:` — the gate is the record's KIND, not the head's name,
    //    which reads "Update" on both pages.
    eval(tab, &format!("{type_query}('e:needle')"));
    settle();
    settle();
    for _ in 0..2 {
        eval(tab, next);
        settle();
        settle();
    }
    assert!(
        eval(tab, marks_in_edit).as_i64().unwrap_or(0) >= 1,
        "an edit shows its hits under e: (marks: {:?}, count: {:?})",
        eval(tab, marks_in_edit),
        eval(tab, total_of)
    );
    // 3. Stepping re-enters where the reader is: after scrolling to the tail, the next hit is
    //    the last one, not the session's first.
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    jump_to_end(tab, surface);
    settle();
    settle();
    eval(tab, next);
    settle();
    settle();
    let landed = eval(tab, current_index).as_i64().unwrap_or(-1);
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    scroll_by(tab, surface, -400000);
    settle();
    settle();
    eval(tab, next);
    settle();
    settle();
    let from_top = eval(tab, current_index).as_i64().unwrap_or(-1);
    assert!(
        from_top >= 0 && landed >= 0,
        "both steps landed somewhere ({from_top}, {landed})"
    );
    assert!(
        landed > from_top,
        "a step from the tail re-enters at a later hit than one from the top ({landed} vs {from_top})"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_scope_edges_count_mark_and_reenter() {
    let _serial = serial();
    let fx = scope_edge_fixture("scenario-scope-edge-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_scope_edges_count_mark_and_reenter(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_scope_edges_count_mark_and_reenter() {
    let _serial = serial();
    let fx = scope_edge_fixture("scenario-scope-edge-app");
    let page = open(Surface::AppShell, &fx, 2900);
    scenario_scope_edges_count_mark_and_reenter(&page.tab, Surface::AppShell, &fx);
}

/// Rule 5 of design/virtual-window.md (#107 step 3): an unmeasured record is ESTIMATED, and the
/// estimate must sit UNDER the real height. Then learning heights only grows the page below the
/// reader; over-estimate and the page SHRINKS as it is read, which above the viewport is a jump.
fn scenario_learning_heights_only_grows_the_page(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    let total = match surface {
        Surface::Classic => "document.body.scrollHeight",
        Surface::AppShell => "document.querySelector('.transcript').scrollHeight",
    };
    // Start at the top, where almost everything below is still an estimate.
    scroll_by(tab, surface, -400000);
    settle();
    settle();
    let before = probe(tab, total).as_f64().unwrap_or(0.0);
    assert!(
        before > 1000.0,
        "the fixture is long enough to estimate: {before}"
    );
    // Read down through it, measuring as it goes, then come back.
    for _ in 0..8 {
        scroll_by(tab, surface, 2000);
        settle();
    }
    settle();
    scroll_by(tab, surface, -400000);
    settle();
    settle();
    let after = probe(tab, total).as_f64().unwrap_or(0.0);
    // A couple of pixels of sub-pixel rounding across dozens of measured records is not a
    // shrink; an over-estimate is thousands (132px guessed against a 40px note, forty times).
    assert!(
        after >= before - 2.0,
        "measuring may only grow the page, never shrink it under the reader ({before} → {after})"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_learning_heights_only_grows_the_page() {
    let _serial = serial();
    let fx = fixture("scenario-estimate-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_learning_heights_only_grows_the_page(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_learning_heights_only_grows_the_page() {
    let _serial = serial();
    let fx = fixture("scenario-estimate-app", 40);
    let page = open(Surface::AppShell, &fx, 2901);
    scenario_learning_heights_only_grows_the_page(&page.tab, Surface::AppShell, &fx);
}

/// A fixture whose queue holds a task with a life: an owner, the three stamps, an acceptance
/// list, an outcome and two worklog entries — and a pending one that is blocked (#125).
fn fixture_task_life(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, &long_session(14, Shape::default()));
    stores.claude_task_file(SID, 1, r#"{"id":"125","subject":"Render tasks the way the board does","description":"the prose the queue kept","activeForm":"Rendering the task card","status":"completed","blockedBy":[],"blocks":[],"accept":["the glyph and the chips","the worklog"],"owner":"claude-code/hong@aries-black","created_at":"2026-09-04T18:23:00Z","claimed_at":"2026-09-04T22:14:00Z","completed_at":"2026-09-04T22:53:00Z","updated_at":"2026-09-04T22:53:00Z","outcome":"shipped as v1.200.0","checks":["node tests/ui_contract.mjs"],"log":[{"ts":"2026-09-04T22:49:00Z","by":"claude-code","msg":"found the seam"},{"ts":"2026-09-04T22:51:00Z","by":"claude-code","msg":"both pages render it"}]}"#);
    stores.claude_task_file(SID, 2, r#"{"id":"126","subject":"The blocked one","description":"waits on 125","status":"pending","blockedBy":["125"],"blocks":[],"owner":"claude-code/hong@aries-black","created_at":"2026-09-04T19:00:00Z","updated_at":"2026-09-04T19:00:00Z"}"#);
    Fixture {
        base,
        path,
        turns: 14,
    }
}

/// A task reads as the queue's own board shows it (#125, the owner's report): a glyph, the
/// chips, the created·claimed·completed line and labelled sections — description, acceptance,
/// outcome, worklog — with a blocked row saying so on its own second line.
fn scenario_a_task_reads_like_the_board(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // #186: this case opens a FINISHED task, which is exactly what the live-only filter holds
    // back. A no-op on the classic page, which has no such pane.
    harness::show_every_pane_row(tab);
    // Open the panel that holds the tasks, then the task itself.
    let open = match surface {
        Surface::Classic => "(function(){ var b = document.getElementById('btn-tasks'); if (b) b.click(); var it = document.querySelector('#taskbox .task-item'); if (it) it.classList.add('open'); return 'ok'; })()",
        Surface::AppShell => "(function(){ var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) { var h = c.querySelector('[data-nav-card-toggle]'); if (h) h.click(); } var t = document.querySelector('[data-task-open]'); if (t) t.click(); return 'ok'; })()",
    };
    eval(tab, open);
    settle();
    settle();
    let card = match surface {
        Surface::Classic => "(function(){ var c = document.querySelector('#taskbox .tcard'); if (!c) return null; return { glyph: (c.querySelector('.tcard-glyph')||{}).textContent, id: (c.querySelector('.tcard-id')||{}).textContent, chips: [...c.querySelectorAll('.tchip')].map(e => e.textContent.trim()), dates: (c.querySelector('.tcard-dates')||{}).textContent, labels: [...c.querySelectorAll('.tcard-label')].map(e => e.textContent), log: [...c.querySelectorAll('.tcard-lt')].map(e => e.textContent), meta: [...document.querySelectorAll('#taskbox .task-meta')].map(e => e.textContent) }; })()",
        Surface::AppShell => "(function(){ var c = document.querySelector('.task-card'); if (!c) return null; return { glyph: (c.querySelector('.task-card-glyph')||{}).textContent, id: (c.querySelector('.task-card-id')||{}).textContent, chips: [...c.querySelectorAll('.task-chip')].map(e => e.textContent.trim()), dates: (c.querySelector('.task-card-dates')||{}).textContent, labels: [...c.querySelectorAll('.task-card-label')].map(e => e.textContent), log: [...c.querySelectorAll('.task-card-log-time')].map(e => e.textContent), meta: [...document.querySelectorAll('#navigatorWork .work-task-meta')].map(e => e.textContent) }; })()",
    };
    let seen = probe(tab, card);
    assert!(
        !seen.is_null(),
        "the task card rendered (panel: {:?}, nav: {:?})",
        probe(tab, "(document.getElementById('taskbox')||{}).innerHTML"),
        probe(tab, "(document.getElementById('tasknav')||{}).outerHTML")
    );
    assert_eq!(
        seen["glyph"], "✓",
        "a finished task wears its glyph: {seen:?}"
    );
    assert_eq!(seen["id"], "#125");
    assert_eq!(
        seen["dates"], "created 09-04 18:23 · claimed 09-04 22:14 · completed 09-04 22:53",
        "its life on one line: {seen:?}"
    );
    let chips: Vec<String> = seen["chips"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        chips.iter().any(|c| c.contains("completed")),
        "a status chip: {chips:?}"
    );
    assert!(
        chips
            .iter()
            .any(|c| c.contains("claude-code/hong@aries-black")),
        "…and who held it: {chips:?}"
    );
    let labels: Vec<String> = seen["labels"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        labels,
        vec!["description", "acceptance", "outcome", "worklog"],
        "the sections a reader wants, in order"
    );
    assert_eq!(
        seen["log"].as_array().map(Vec::len),
        Some(2),
        "each worklog entry keeps its time: {seen:?}"
    );
    {
        let meta: Vec<String> = seen["meta"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| c.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            meta.iter().any(|m| m.contains("blocked by #125")),
            "the blocked row says so on its own line: {meta:?}"
        );
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_task_reads_like_the_board() {
    let _serial = serial();
    let fx = fixture_task_life("scenario-task-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_task_reads_like_the_board(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_task_reads_like_the_board() {
    let _serial = serial();
    let fx = fixture_task_life("scenario-task-app");
    let page = open(Surface::AppShell, &fx, 2902);
    scenario_a_task_reads_like_the_board(&page.tab, Surface::AppShell, &fx);
}

/// A fixture holding the #125 STUB and nothing else: a session with NO task store on disk, whose
/// transcript carries one `taskq` audit line moving a task it never created.
///
/// Both halves are load-bearing. Only an UPDATE for an unseen id reaches the stub branch — a
/// create carries its subject, and every task file on disk carries one too — and a `taskq` audit
/// line is the shape of it this repo's own sessions produce. The store is ABSENT rather than
/// merely lacking that id: `tasks::merged` takes disk per id and APPENDS what only the op-log
/// saw, so a store here would work (measured) but would put two unrelated questions in one
/// fixture.
fn fixture_unrecorded_task(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(14, Shape::default());
    jsonl += &harness::taskq_state("call-taskq", "119", "done", "in_progress", "completed", 51);
    let path = stores.claude_session(SID, &jsonl);
    Fixture {
        base,
        path,
        turns: 14,
    }
}

/// A task whose title this transcript never saw says WHY it has none, rather than rendering an
/// id, a status chip and silence (#188, the second half of the owner's #187 report: "it did not
/// include all the task details").
///
/// The absence is the ENGINE's decision and it is deliberate. `engine/tasks.rs`, `TaskOp::Update`
/// on an unknown id: "The subject stays EMPTY on purpose (#125). It is not recoverable … the
/// on-disk store is keyed by session with per-queue integer ids that COLLIDE … Scanning sibling
/// task directories for a matching id would attach a confidently WRONG title, which is worse than
/// none. The frontends render the absence honestly instead." #155 draws the same line for a
/// pruned title: "a visible gap, not an invented one."
///
/// Rendering the gap is not the same as SAYING it, and that is the whole of this case. Both
/// pages showed the gap in silence, which reads as broken. So: the card explains itself, in one
/// shared wording; the row names the absence instead of inventing a title; and — the half that
/// would rot quietly — NO section is invented to carry the explanation, because the card's
/// labels name the queue's own fields and a synthetic one would read as a field the task has.
fn scenario_a_task_with_no_title_says_why(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // The stub's status is COMPLETED — a transcript that only ever CLOSED a task is the ordinary
    // way to get one — so #186's live-only filter holds it back by default. That is correct (it
    // is finished work), and it is the first place the two features meet: a reader whose only
    // trace of a task is its completion sees an empty pane until they ask for everything.
    harness::show_every_pane_row(tab);
    let open = match surface {
        Surface::Classic => "(function(){ var b = document.getElementById('btn-tasks'); if (b) b.click(); var it = document.querySelector('#taskbox .task-item'); if (it) it.classList.add('open'); return 'ok'; })()",
        Surface::AppShell => "(function(){ var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) { var h = c.querySelector('[data-nav-card-toggle]'); if (h) h.click(); } var t = document.querySelector('[data-task-open]'); if (t) t.click(); return 'ok'; })()",
    };
    eval(tab, open);
    settle();
    settle();
    let card = match surface {
        Surface::Classic => "(function(){ var c = document.querySelector('#taskbox .tcard'); if (!c) return null; return { rows: document.querySelectorAll('#taskbox .task-item').length, id: (c.querySelector('.tcard-id')||{}).textContent, title: (c.querySelector('.tcard-title')||{}).textContent, gap: (c.querySelector('.tcard-gap')||{}).textContent || '', labels: [...c.querySelectorAll('.tcard-label')].map(e => e.textContent), row: (document.querySelector('#taskbox .task-subj')||{}).textContent }; })()",
        Surface::AppShell => "(function(){ var c = document.querySelector('.task-card'); if (!c) return null; return { rows: document.querySelectorAll('#navigatorWork .work-task').length, id: (c.querySelector('.task-card-id')||{}).textContent, title: (c.querySelector('.task-card-title')||{}).textContent, gap: (c.querySelector('.task-card-gap')||{}).textContent || '', labels: [...c.querySelectorAll('.task-card-label')].map(e => e.textContent), row: (document.querySelector('#navigatorWork .work-copy strong')||{}).textContent }; })()",
    };
    let seen = probe(tab, card);
    assert!(
        !seen.is_null(),
        "the stub task reached the pane at all (panel: {:?}, nav: {:?})",
        probe(tab, "(document.getElementById('taskbox')||{}).innerHTML"),
        probe(
            tab,
            "(document.getElementById('navigatorWork')||{}).innerHTML"
        )
    );
    assert_eq!(
        seen["rows"], 1,
        "one task, from one record and no store: {seen:?}"
    );
    // The `q` prefix is taskq's own: two queues number from 1 and the panel holds both, so a
    // repo-tier task is `q119` everywhere it is keyed. Asserting the bare number would pass
    // against a panel that had lost the prefix and started colliding with a native task.
    assert_eq!(seen["id"], "#q119", "the id it does have: {seen:?}");
    assert_eq!(
        seen["title"], "",
        "…and the title it does NOT have stays empty — the engine's whole point: {seen:?}"
    );
    let gap = seen["gap"].as_str().unwrap_or("");
    assert!(
        gap.contains("not in this stream"),
        "the card says why it is empty, in the register of the jump beside it: {seen:?}"
    );
    assert!(
        gap.contains("only its status was recorded here"),
        "…and says which part of the task DID reach this transcript: {seen:?}"
    );
    assert_eq!(
        seen["labels"],
        serde_json::json!([]),
        "…and invents no section to carry it: the labels name the queue's own fields: {seen:?}"
    );
    assert_eq!(
        seen["row"], "(no title recorded in this session)",
        "the pane row names the absence rather than inventing a title: {seen:?}"
    );
    // …and it has to be READABLE, which is not the same as present. #187 was a heading that
    // still opened, still measured a rect and still held exactly the right text while setting
    // itself one character per line inside a 12px grid track; a rect is not visibility (#98),
    // and a sentence explaining an absence is worthless if it renders as a column of letters.
    let gapbox = match surface {
        Surface::Classic => probe(tab, "(function(){ var e = document.querySelector('#taskbox .tcard-gap'); if (!e) return null; var r = e.getBoundingClientRect(); var cs = getComputedStyle(e); var line = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.5; var hit = document.elementFromPoint(Math.round(r.left + r.width / 2), Math.round(r.top + Math.min(r.height / 2, line / 2))); return { w: Math.round(r.width), lines: Math.round(r.height / line), chars: e.textContent.length, own: !!hit && e.contains(hit) }; })()"),
        Surface::AppShell => probe(tab, "(function(){ var e = document.querySelector('#taskPopover .task-card-gap'); if (!e) return null; var r = e.getBoundingClientRect(); var cs = getComputedStyle(e); var line = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) * 1.5; var hit = document.elementFromPoint(Math.round(r.left + r.width / 2), Math.round(r.top + Math.min(r.height / 2, line / 2))); return { w: Math.round(r.width), lines: Math.round(r.height / line), chars: e.textContent.length, own: !!hit && e.contains(hit) }; })()"),
    };
    let chars = gapbox["chars"].as_f64().unwrap_or(0.0);
    let lines = gapbox["lines"].as_f64().unwrap_or(999.0);
    assert!(
        gapbox["w"].as_f64().unwrap_or(0.0) > 120.0 && lines <= 6.0 && chars > 100.0,
        "the note sets itself ACROSS the card, not down it — {chars} characters in {lines} lines: {gapbox}"
    );
    assert_eq!(
        gapbox["own"], true,
        "…and nothing is drawn over it: {gapbox}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_task_with_no_title_says_why() {
    let _serial = serial();
    let fx = fixture_unrecorded_task("scenario-stub-task-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_task_with_no_title_says_why(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_task_with_no_title_says_why() {
    let _serial = serial();
    let fx = fixture_unrecorded_task("scenario-stub-task-app");
    let page = open(Surface::AppShell, &fx, 2944);
    scenario_a_task_with_no_title_says_why(&page.tab, Surface::AppShell, &fx);
}

/// A fixture whose tail holds a Bash call whose command runs far past the head's one line.
fn fixture_long_command(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(14, Shape::default());
    let command = "cargo test -p claude-replay-browser-tests --test scenarios -- --ignored --skip known_red app_shell --nocapture 2>&1 | grep -E 'the needle in a very long pipeline that keeps going and going past any reasonable head width' | sed -e 's/one thing/another thing entirely/' -e 's/and yet another substitution/to make quite sure this line cannot fit/' | sort -u | head -20";
    jsonl += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"long-1\",\"name\":\"Bash\",\"input\":{{\"command\":\"{command}\"}}}}]}},\"timestamp\":\"2026-08-21T10:15:01Z\"}}\n"
    );
    jsonl += &tool_result_text("long-1", "one line of output", "2026-08-21T10:15:02Z");
    let path = stores.claude_session(SID, &jsonl);
    Fixture {
        base,
        path,
        turns: 15,
    }
}

/// The same long command, but with a long tail AFTER it — so the block can be scrolled out of the
/// window and back, which is what #189 is about.
fn fixture_long_command_midway(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(14, Shape::default());
    let command = "cargo test -p claude-replay-browser-tests --test scenarios -- --ignored --skip known_red app_shell --nocapture 2>&1 | grep -E 'the needle in a very long pipeline that keeps going and going past any reasonable head width' | sed -e 's/one thing/another thing entirely/' | sort -u | head -20";
    jsonl += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"mid-1\",\"name\":\"Bash\",\"input\":{{\"command\":\"{command}\"}}}}]}},\"timestamp\":\"2026-08-21T10:15:01Z\"}}\n"
    );
    jsonl += &tool_result_text("mid-1", "one line of output", "2026-08-21T10:15:02Z");
    jsonl += &long_session(22, Shape::default());
    let path = stores.claude_session(SID, &jsonl);
    Fixture {
        base,
        path,
        turns: 37,
    }
}

/// #189. A fold the READER opened keeps the head state the reader left it in, across a
/// re-materialization. The classic page lost it: `renderBlock` emits the header target in the
/// EXPANDED pre-wrap form for any block whose record says `b.open`, `toggleFold` has already
/// called `setRecordOpen` so `b.open` is true for a fold the reader opened, and nothing
/// re-applies the reader's own head step — there is no `userFulls` beside `userFolds`. So the
/// tidy one-line target became the whole wrapped command on the next scroll past and back.
///
/// The app shell keeps `state.fullTargets` keyed by record id and survives its own re-render, so
/// this is the reference page being the one out of step — which #71 established can happen.
///
/// Non-vacuous by construction: the case TAGS the element before scrolling and requires the tag
/// to be gone afterwards. Without that, a run where the block never left the window would assert
/// that nothing changed and pass for the wrong reason.
fn scenario_a_fold_keeps_its_head_state_across_a_rematerialization(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // The fold sits MID-DOCUMENT on purpose — that is what lets it leave the window later — so
    // the case has to go and find it first.
    let find = match surface {
        Surface::Classic => "!![...document.querySelectorAll('#stream .fold > .fold-h > .tool-target')].find(function (t) { return t.textContent.indexOf('the needle in a very long pipeline') >= 0; })",
        Surface::AppShell => "!![...document.querySelectorAll('.virtual-window .renderer > .renderer-head > .renderer-target')].find(function (t) { return t.textContent.indexOf('the needle in a very long pipeline') >= 0; })",
    };
    let mut found = false;
    for _ in 0..24 {
        if eval(tab, find).as_bool() == Some(true) {
            found = true;
            break;
        }
        scroll_by(tab, surface, -1600);
        settle();
    }
    assert!(
        found,
        "{surface:?}: scrolling up reached the long-command fold within the fixture"
    );
    settle();
    // Open it ONE step (the output, target still one clipped line). The click and the READ are
    // two separate probes on purpose: the app shell's head handler ends in `actions.rerender()`,
    // which replaces every node in the window, so a reference taken before the click is detached
    // afterwards and reports the OLD element's state — measured, as `open: "false"` on a
    // renderer that had in fact opened.
    let id = eval(
        tab,
        match surface {
            Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(function (x) { var t = x.querySelector(':scope > .fold-h > .tool-target'); return t && t.textContent.indexOf('the needle in a very long pipeline') >= 0; }); if (!f) return ''; if (f.dataset.open !== '1') f.querySelector(':scope > .fold-h').click(); return f.id; })()",
            Surface::AppShell => "(function(){ var r = [...document.querySelectorAll('.virtual-window .renderer[data-record-id]')].find(function (x) { var t = x.querySelector(':scope > .renderer-head > .renderer-target'); return t && t.textContent.indexOf('the needle in a very long pipeline') >= 0; }); if (!r) return ''; var id = r.dataset.recordId; if (r.classList.contains('closed')) r.querySelector(':scope > button.renderer-head').click(); return id; })()",
        },
    )
    .as_str()
    .unwrap_or_default()
    .to_string();
    assert!(
        !id.is_empty(),
        "{surface:?}: the fixture's long-command fold is addressable by a record id"
    );
    settle();
    // …then tag the FRESH element and read the state it settled into.
    let opened = probe(
        tab,
        &match surface {
            Surface::Classic => format!("(function(){{ var f = document.getElementById('{id}'); if (!f) return null; f.dataset.auditTag = '1'; var t = f.querySelector(':scope > .fold-h > .tool-target'); return {{ id: f.id, ws: t ? getComputedStyle(t).whiteSpace : 'no-target', open: f.dataset.open }}; }})()"),
            Surface::AppShell => format!("(function(){{ var r = document.querySelector('.virtual-window .renderer[data-record-id=\"{id}\"]'); if (!r) return null; r.dataset.auditTag = '1'; var t = r.querySelector(':scope > .renderer-head > .renderer-target'); return {{ id: r.dataset.recordId, ws: t ? getComputedStyle(t).whiteSpace : 'no-target', open: String(!r.classList.contains('closed')) }}; }})()"),
        },
    );
    settle();

    // Go away — down to the tail — then come back, so the window unmounts the block and builds
    // it afresh when the reader returns.
    jump_to_end(tab, surface);
    settle();
    settle();
    for _ in 0..24 {
        if eval(tab, find).as_bool() == Some(true) {
            break;
        }
        scroll_by(tab, surface, -1600);
        settle();
    }
    settle();

    let after = probe(
        tab,
        &match surface {
            Surface::Classic => format!("(function(){{ var f = document.getElementById('{id}'); if (!f) return {{ missing: true }}; var t = f.querySelector(':scope > .fold-h > .tool-target'); return {{ rebuilt: f.dataset.auditTag !== '1', ws: t ? getComputedStyle(t).whiteSpace : 'no-target', open: f.dataset.open, full: f.dataset.full === undefined ? 'unset' : f.dataset.full }}; }})()"),
            Surface::AppShell => format!("(function(){{ var r = document.querySelector('.virtual-window .renderer[data-record-id=\"{id}\"]'); if (!r) return {{ missing: true }}; var t = r.querySelector(':scope > .renderer-head > .renderer-target'); return {{ rebuilt: r.dataset.auditTag !== '1', ws: t ? getComputedStyle(t).whiteSpace : 'no-target', open: String(!r.classList.contains('closed')), full: r.dataset.target || 'unset' }}; }})()"),
        },
    );
    assert!(
        after["missing"].as_bool() != Some(true),
        "{surface:?}: the fold came back into the window: {after}"
    );
    assert_eq!(
        after["rebuilt"], true,
        "{surface:?}: …and it was REBUILT rather than kept — otherwise this case asserts that \
         nothing changed and passes for the wrong reason: {after}"
    );
    assert_eq!(
        after["ws"], "nowrap",
        "{surface:?}: a fold the reader opened to step 2 must come back at step 2. The classic \
         page came back at step 3 — `renderBlock` emits the expanded pre-wrap target for any \
         block whose record says `b.open`, and `toggleFold` had already made that true, so the \
         reader's one-line target became the whole wrapped command on the next scroll past \
         (#189): {after}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_fold_keeps_its_head_state_across_a_rematerialization() {
    let _serial = serial();
    let fx = fixture_long_command_midway("scenario-headstate-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_fold_keeps_its_head_state_across_a_rematerialization(
        &page.tab,
        Surface::Classic,
        &fx,
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_fold_keeps_its_head_state_across_a_rematerialization() {
    let _serial = serial();
    let fx = fixture_long_command_midway("scenario-headstate-app");
    let page = open(Surface::AppShell, &fx, 2946);
    scenario_a_fold_keeps_its_head_state_across_a_rematerialization(
        &page.tab,
        Surface::AppShell,
        &fx,
    );
}

/// The head's click cycle (#129, the owner's report and their spec): a long command is one
/// clipped line, and clicking used to reveal only the output. Now, from folded: the output,
/// then the whole command, then the command folds back, then the output.
fn scenario_a_long_command_unfolds_on_the_second_click(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    if surface == Surface::AppShell {
        eval(
            tab,
            "document.querySelectorAll('[data-process-more][aria-expanded=\"false\"]').forEach(b => b.click())",
        );
        settle();
    }
    // The head of the long call, and what it is showing: whether the body is open, and whether
    // the target is one clipped line or the whole command.
    let (click, probe_js) = match surface {
        Surface::Classic => (
            "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(f => /cargo test -p claude-replay-browser-tests/.test((f.querySelector(':scope > .fold-h > .tool-target')||{}).textContent||'')); if (!f) return 'none'; f.querySelector('.fold-h').click(); return 'ok'; })()",
            "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(f => /cargo test -p claude-replay-browser-tests/.test((f.querySelector(':scope > .fold-h > .tool-target')||{}).textContent||'')); if (!f) return null; var t = f.querySelector(':scope > .fold-h > .tool-target'); return { open: f.dataset.open === '1', full: getComputedStyle(t).whiteSpace === 'pre-wrap', text: t.textContent.length }; })()",
        ),
        Surface::AppShell => (
            "(function(){ var r = [...document.querySelectorAll('.renderer[data-renderer-kind]')].find(r => /cargo test -p claude-replay-browser-tests/.test((r.querySelector(':scope > .renderer-head > .renderer-target')||{}).textContent||'')); if (!r) return 'none'; r.querySelector('.renderer-head').click(); return 'ok'; })()",
            "(function(){ var r = [...document.querySelectorAll('.renderer[data-renderer-kind]')].find(r => /cargo test -p claude-replay-browser-tests/.test((r.querySelector(':scope > .renderer-head > .renderer-target')||{}).textContent||'')); if (!r) return null; var t = r.querySelector('.renderer-target'); return { open: !r.classList.contains('closed'), full: getComputedStyle(t).whiteSpace === 'pre-wrap', text: t.textContent.length }; })()",
        ),
    };
    let at = |tab: &headless_chrome::Tab| {
        let seen = probe(tab, probe_js);
        assert!(!seen.is_null(), "the long command's head is on the page");
        assert!(
            seen["text"].as_i64().unwrap_or(0) > 250,
            "the head carries the whole command as text: {seen:?}"
        );
        (
            seen["open"].as_bool().unwrap_or(false),
            seen["full"].as_bool().unwrap_or(false),
        )
    };
    // A single tool call is grouped — an activity fold on the classic page, a process surface
    // on the app shell — and its head has no layout box while that parent is closed. Open the
    // ancestors first; the cycle under test is the head's own.
    let open_parents = match surface {
        Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(f => /cargo test -p claude-replay/.test((f.querySelector(':scope > .fold-h > .tool-target')||{}).textContent||'')); if (!f) return 'none'; var p = f.parentElement && f.parentElement.closest('.fold'); while (p) { if (p.dataset.open !== '1') p.querySelector(':scope > .fold-h').click(); p = p.parentElement && p.parentElement.closest('.fold'); } return 'ok'; })()",
        Surface::AppShell => "(function(){ document.querySelectorAll('.process-surface.closed [data-process-toggle]').forEach(function (h) { h.click(); }); document.querySelectorAll('[data-process-more]').forEach(function (b) { if (b.getAttribute('aria-expanded') === 'false') b.click(); }); for (var pass = 0; pass < 4; pass++) { var host = [...document.querySelectorAll('.renderer.closed')].find(function (r) { var own = r.querySelector(':scope > .renderer-head > .renderer-target'); return (!own || !/cargo test -p claude-replay/.test(own.textContent)) && /cargo test -p claude-replay/.test(r.textContent); }); if (!host) break; host.querySelector(':scope > .renderer-head').click(); } return 'ok'; })()",
    };
    eval(tab, open_parents);
    settle();
    settle();
    println!("MATCHES {surface:?}: {:?}", probe(tab, "[...document.querySelectorAll('#stream .fold, .virtual-window .renderer')].filter(function (f) { var t = f.querySelector('.tool-target, .renderer-target'); return t && /cargo test -p claude-replay/.test(t.textContent); }).map(function (f) { var t = f.querySelector('.tool-target, .renderer-target'); return { w: Math.round(f.getBoundingClientRect().width), tw: Math.round(t.getBoundingClientRect().width), off: f.offsetParent === null, cls: f.className.slice(0, 24) }; })"));
    println!("STREAM {surface:?}: {:?} body={:?}", probe(tab, "(function(){ var s = document.getElementById('stream') || document.querySelector('.virtual-window'); return s ? Math.round(s.getBoundingClientRect().width) : -1; })()"), probe(tab, "Math.round(document.body.getBoundingClientRect().width)"));
    assert_eq!(
        at(tab),
        (false, false),
        "folded: one clipped line, no output"
    );
    let mut walk = vec![at(tab)];
    for _ in 0..4 {
        eval(tab, click);
        settle();
        walk.push(at(tab));
    }
    assert_eq!(
        walk,
        vec![
            (false, false),
            (true, false),
            (true, true),
            (true, false),
            (false, false),
        ],
        "the output, then the whole command, then the command folds, then the output"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_long_command_unfolds_on_the_second_click() {
    let _serial = serial();
    let fx = fixture_long_command("scenario-longcmd-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_long_command_unfolds_on_the_second_click(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_long_command_unfolds_on_the_second_click() {
    let _serial = serial();
    let fx = fixture_long_command("scenario-longcmd-app");
    let page = open(Surface::AppShell, &fx, 2903);
    scenario_a_long_command_unfolds_on_the_second_click(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: scope counts, a typed scope prefix, scoped stepping, the escape (#101) ─────

/// Row 5.6 of design/rendering-parity-audit.md and the owner's report. A query shows how many
/// hits each class holds; a typed `u:` prefix checks the User box and limits stepping to
/// prompts; a leading `:` searches the literal; clicking a scope button writes the prefix.
fn scenario_scope_counts_prefix_and_gating(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (type_query, count_of, next, current_kind, box_value, scope_on, click_scope, open_menu) = match surface {
        Surface::Classic => (
            "(function(q){ var i = document.getElementById('q'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(k){ var e = document.getElementById('qsn-' + k); return e ? e.textContent.trim() : 'none'; })",
            "(function(){ var b = document.getElementById('qnext'); if (b) { b.click(); return 'next'; } return 'none'; })()",
            "(function(){ var m = document.querySelector('#stream mark.hl.cur'); if (!m) return 'no current'; var blk = m.closest('.blk'); return blk && blk.classList.contains('uturn') ? 'prompt' : 'other'; })()",
            "document.getElementById('q').value",
            "(function(k){ var cb = document.getElementById('qs-' + k); return cb ? cb.checked : null; })",
            "(function(k){ var cb = document.getElementById('qs-' + k); if (!cb) return 'none'; cb.checked = !cb.checked; cb.dispatchEvent(new Event('change', { bubbles: true })); return 'clicked'; })",
            "(function(){ var q = document.getElementById('qscope'); if (q) q.click(); return 'ok'; })()",
        ),
        Surface::AppShell => (
            "(function(q){ var i = document.getElementById('transcriptSearchInput'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(k){ var e = document.querySelector('[data-scope-count=\"' + k + '\"]'); return e ? e.textContent.trim() : 'none'; })",
            "(function(){ document.getElementById('findNext').click(); return 'next'; })()",
            "(function(){ var m = document.querySelector('.virtual-window mark.search-mark.current'); if (!m) return 'no current'; var t = m.closest('.turn'); return t && t.classList.contains('user') ? 'prompt' : 'other'; })()",
            "document.getElementById('transcriptSearchInput').value",
            "(function(k){ var b = document.querySelector('.scope-option[data-scope=\"' + k + '\"]'); return b ? b.classList.contains('on') : null; })",
            "(function(k){ var b = document.querySelector('.scope-option[data-scope=\"' + k + '\"]'); if (!b) return 'none'; b.click(); return 'clicked'; })",
            "(function(){ var b = document.getElementById('filterTranscriptBtn'); if (b && !document.getElementById('navigatorOptions').classList.contains('open')) b.click(); return 'ok'; })()",
        ),
    };
    eval(tab, open_menu);
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    assert_eq!(
        eval(tab, &format!("{count_of}('u')")),
        "1",
        "one hit in prompts"
    );
    assert_eq!(
        eval(tab, &format!("{count_of}('b')")),
        "2",
        "two hits in Bash output"
    );
    // A typed prefix: the User box checks, the others do not, and stepping stays in prompts.
    eval(tab, &format!("{type_query}('u:needle')"));
    settle();
    settle();
    assert_eq!(
        eval(tab, &format!("{scope_on}('u')")),
        true,
        "u: checks the User box"
    );
    assert_eq!(
        eval(tab, &format!("{scope_on}('b')")),
        false,
        "…and not Bash"
    );
    for _ in 0..3 {
        eval(tab, next);
        settle();
        assert_eq!(
            eval(tab, current_kind),
            "prompt",
            "stepping under u: stays in prompts"
        );
    }
    // The escape: the literal "u:needle" is nowhere.
    eval(tab, &format!("{type_query}(':u:needle')"));
    settle();
    settle();
    assert_eq!(
        eval(tab, &format!("{count_of}('u')")),
        "0",
        "an escaped prefix is searched literally"
    );
    // A scope button writes the prefix into the box.
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    eval(tab, open_menu);
    assert_eq!(
        eval(tab, &format!("{click_scope}('b')")),
        "clicked",
        "the Bash scope button exists"
    );
    settle();
    let value = eval(tab, box_value).as_str().unwrap_or("").to_string();
    let prefix = value.split(':').next().unwrap_or("").to_string();
    assert!(
        value.contains(':') && prefix.contains('b') && value.ends_with("needle"),
        "the button wrote a prefix with b: {value:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_scope_counts_prefix_and_gating() {
    let _serial = serial();
    let fx = scope_fixture("scenario-scope-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_scope_counts_prefix_and_gating(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_scope_counts_prefix_and_gating() {
    let _serial = serial();
    let fx = scope_fixture("scenario-scope-app");
    let page = open(Surface::AppShell, &fx, 2892);
    scenario_scope_counts_prefix_and_gating(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then a prompt with the word once and a Bash output with it twice.
fn scope_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question scope: find the needle", &now_minus(90));
    transcript += &assistant_at("Running the search.", &now_minus(85));
    transcript += &tool_open_at("t-scope-bash", &now_minus(70));
    transcript += &tool_result_text(
        "t-scope-bash",
        "a needle here\\nanother needle there\\n",
        &now_minus(60),
    );
    transcript += &assistant_at("answer scope: two in the output", &now_minus(30));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: a large session searches on Enter, not on every keystroke (#104) ────────────

/// Row 5.7 of design/rendering-parity-audit.md and the owner's report. Above the shared
/// haystack limit, typing shows "⏎ to search" and marks nothing; Enter runs the search.
fn scenario_large_session_searches_on_enter(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (type_query, press_enter, count, marks) = match surface {
        Surface::Classic => (
            "(function(q){ var i = document.getElementById('q'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ var i = document.getElementById('q'); i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return 'enter'; })()",
            "(function(){ var c = document.getElementById('qcount'); return c ? c.textContent.trim() : ''; })()",
            "document.querySelectorAll('#stream mark.hl').length",
        ),
        Surface::AppShell => (
            "(function(q){ var i = document.getElementById('transcriptSearchInput'); i.value = q; i.dispatchEvent(new Event('input', { bubbles: true })); return 'typed'; })",
            "(function(){ var i = document.getElementById('transcriptSearchInput'); i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return 'enter'; })()",
            "document.getElementById('transcriptSearchCount').textContent.trim()",
            "document.querySelectorAll('.virtual-window mark.search-mark').length",
        ),
    };
    // The whole session must have streamed in for the size to be known.
    until(
        tab,
        match surface {
            Surface::Classic => "document.querySelectorAll('#turnlist .side-item').length >= 13",
            Surface::AppShell => {
                "document.querySelectorAll('#navigatorTurns .outline-turn-row').length >= 13"
            }
        },
        "the session to stream in",
        Duration::from_secs(60),
        "document.readyState",
    );
    std::thread::sleep(Duration::from_millis(2500));
    eval(tab, &format!("{type_query}('needle')"));
    settle();
    settle();
    let c = eval(tab, count).as_str().unwrap_or("").to_string();
    assert!(
        c.contains("⏎"),
        "typing in a large session does not search: {c:?}"
    );
    assert_eq!(eval(tab, marks), 0, "…and marks nothing");
    eval(tab, press_enter);
    settle();
    settle();
    let c2 = eval(tab, count).as_str().unwrap_or("").to_string();
    assert!(
        c2.starts_with('1') || c2.starts_with("1/"),
        "Enter runs the search: {c2:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_large_session_searches_on_enter() {
    let _serial = serial();
    let fx = large_fixture("scenario-large-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_large_session_searches_on_enter(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_large_session_searches_on_enter() {
    let _serial = serial();
    let fx = large_fixture("scenario-large-app");
    let page = open(Surface::AppShell, &fx, 2893);
    scenario_large_session_searches_on_enter(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns, then a turn of 200 narrated Bash calls with ~62 KB of output each (~12.5 MB
/// of haystack, each under the reader's per-string eliding bound), the last one carrying the word.
fn large_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question large: run everything", &now_minus(9000));
    let chunk: String = (0..1200)
        .map(|k| format!("output line {k:04} of a long run that goes on and on\\n"))
        .collect();
    for k in 0..200u64 {
        transcript += &assistant_at(&format!("step {k}"), &now_minus(8000 - k * 30));
        transcript += &tool_open_at(&format!("t-large-{k}"), &now_minus(8000 - k * 30 - 10));
        let body = if k == 199 {
            format!("{chunk}the needle is here\\n")
        } else {
            chunk.clone()
        };
        transcript += &tool_result_text(
            &format!("t-large-{k}"),
            &body,
            &now_minus(8000 - k * 30 - 20),
        );
    }
    transcript += &assistant_at("answer large: done", &now_minus(100));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

// ── scenario: the turn ordinal a process header and the turn list show is the turn's own (#103)

/// The owner's report: "Turn 05" on a process while the session is in its 900s. The ordinal
/// must be the record's own turn, the same one the turns pane shows — in a 120-turn session,
/// at the tail, after a live turn arrives, and after a reload.
fn scenario_turn_ordinal_is_the_turns_own(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (last_row, surface_label) = match surface {
        Surface::Classic => (
            "(function(){ var r = [...document.querySelectorAll('#turnlist .side-item')].pop(); return r ? r.textContent.trim().split(' ')[0] : ''; })()",
            "(function(){ var t = document.getElementById('stickytext'); return t ? t.textContent.trim() : ''; })()",
        ),
        Surface::AppShell => (
            "(function(){ var r = [...document.querySelectorAll('#navigatorTurns .outline-turn-row .outline-number')].pop(); return r ? r.textContent.trim().replace(/\\s*·$/, '') : ''; })()",
            "(function(){ var s = [...document.querySelectorAll('.virtual-window .process-surface')].pop(); if (!s) return 'no surface'; var l = s.querySelector('.process-surface-label'); if (!l) return 'no label'; var c = getComputedStyle(l, '::after').content; c = c.charAt(0) === String.fromCharCode(34) ? c.slice(1, -1) : c; return c + '|' + s.dataset.turn; })()",
        ),
    };
    assert_eq!(eval(tab, last_row), "120", "the turn list ends at 120");
    if surface == Surface::AppShell {
        let label = eval(tab, surface_label).as_str().unwrap_or("").to_string();
        assert!(surface_label_ok(&label), "the last process header says its own turn (three digits deep in the session), not the count of mounted prompts: {label:?}");
    }
    // A live turn arrives: 121 on both.
    let growth = LiveGrowth::start(
        fx.path.clone(),
        vec![
            user_at("question 121: one more", &now_minus(10)),
            assistant_at("answer 121", &now_minus(8)),
        ],
        Duration::from_millis(400),
    );
    assert_eq!(growth.finish(Duration::from_secs(20)), 2);
    until(
        tab,
        &format!("{last_row} === '121'"),
        "the turn list to reach 121",
        Duration::from_secs(30),
        last_row,
    );
    jump_to_end(tab, surface);
    settle();
    settle();
    if surface == Surface::AppShell {
        // The new turn has no process (no tool call); the last surface is still turn 120's.
        let label = eval(tab, surface_label).as_str().unwrap_or("").to_string();
        assert!(
            surface_label_ok(&label),
            "the last surface keeps its own ordinal after growth: {label:?}"
        );
    }
    // After a reload the ordinals hold.
    eval(tab, "location.reload(); 'ok'");
    std::thread::sleep(Duration::from_millis(1500));
    until(
        tab,
        &format!("{last_row} === '121'"),
        "the page to come back with 121 turns",
        Duration::from_secs(30),
        last_row,
    );
    jump_to_end(tab, surface);
    settle();
    settle();
    if surface == Surface::AppShell {
        let label = eval(tab, surface_label).as_str().unwrap_or("").to_string();
        assert!(surface_label_ok(&label), "…and after a reload: {label:?}");
    }
}

/// "Turn NNN|NNN": the label names the surface's own turn, three digits into the session.
fn surface_label_ok(label: &str) -> bool {
    let (text, own) = match label.split_once('|') {
        Some(p) => p,
        None => return false,
    };
    let n = text.trim_start_matches("Turn ").trim();
    n == own.trim_start_matches('0') && n.len() >= 3 && n.chars().all(|c| c.is_ascii_digit())
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_turn_ordinal_is_the_turns_own() {
    let _serial = serial();
    let fx = ordinal_fixture("scenario-ordinal-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_turn_ordinal_is_the_turns_own(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_turn_ordinal_is_the_turns_own() {
    let _serial = serial();
    let fx = ordinal_fixture("scenario-ordinal-app");
    let page = open(Surface::AppShell, &fx, 2894);
    scenario_turn_ordinal_is_the_turns_own(&page.tab, Surface::AppShell, &fx);
}

/// A 120-turn session, every turn with a tool call (a process surface).
fn ordinal_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, &long_session(120, Shape::default()));
    Fixture {
        base,
        path,
        turns: 120,
    }
}

// ── scenario: dragging across a one-line prompt selects the prompt alone (#99) ────────────

/// The owner's report: a one-line user message pasted as three lines. A drag from one corner
/// of the card to the other must select the message text only — no control labels, no time.
fn scenario_dragging_a_card_copies_one_line(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let card_rect = match surface {
        Surface::Classic => "(function(){ var c = [...document.querySelectorAll('#stream .uturn')].pop(); if (!c) return null; var r = c.getBoundingClientRect(); var b = c.querySelector('.uturn-md') || c; return { left: r.left, top: r.top, right: r.right, bottom: r.bottom, text: b.textContent.trim() }; })()",
        Surface::AppShell => "(function(){ var c = [...document.querySelectorAll('.turn.user')].pop(); if (!c) return null; var r = c.getBoundingClientRect(); var b = c.querySelector('.body.markdown') || c; return { left: r.left, top: r.top, right: r.right, bottom: r.bottom, text: b.textContent.trim() }; })()",
    };
    let card = probe(tab, card_rect);
    let (l, t, r, b) = (
        card["left"].as_f64().unwrap(),
        card["top"].as_f64().unwrap(),
        card["right"].as_f64().unwrap(),
        card["bottom"].as_f64().unwrap(),
    );
    let text = card["text"].as_str().unwrap_or("").to_string();
    assert!(
        !text.contains('\n') && text.len() > 10,
        "the last prompt is one line: {text:?}"
    );
    drag_select(tab, l + 2.0, t + 2.0, r - 2.0, b - 2.0);
    settle();
    let selected = selection_text(tab);
    let selected = selected.trim().to_string();
    assert_eq!(
        selected.lines().count(),
        1,
        "a drag across the whole card selects one line, not the controls too: {selected:?}"
    );
    assert_eq!(selected, text, "…the message itself");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_dragging_a_card_copies_one_line() {
    let _serial = serial();
    let fx = fixture("scenario-copyline-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_dragging_a_card_copies_one_line(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_dragging_a_card_copies_one_line() {
    let _serial = serial();
    let fx = fixture("scenario-copyline-app", 12);
    let page = open(Surface::AppShell, &fx, 2895);
    scenario_dragging_a_card_copies_one_line(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a tool head carries its state, exit and duration (#117) ─────────────────────

/// The shared tool head (shared/tool-head.js, audit row 3.4): a failed call shows failure
/// presentation WITH its exit code, a long call shows its duration, a declined call says so,
/// and an Edit reads Update — the classic page as chips, the app shell as one state pill.
fn scenario_tool_heads_carry_state_exit_and_duration(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    if surface == Surface::AppShell {
        // A process surface shows its first events and folds the rest behind "Show N more".
        eval(
            tab,
            "document.querySelectorAll('[data-process-more][aria-expanded=\"false\"]').forEach(b => b.click())",
        );
        settle();
    }
    let heads = probe(
        tab,
        match surface {
            Surface::Classic => "[...document.querySelectorAll('#stream .fold')].map(f => { var h = f.querySelector('.fold-h'); return { name: (h.querySelector('.tool-name') || {}).textContent || '', target: (h.querySelector('.tool-target,.tool-path') || {}).textContent || '', chips: [...h.querySelectorAll('.chip')].map(c => ({ c: c.className.replace('chip', '').trim(), x: c.textContent })) }; })",
            Surface::AppShell => "[...document.querySelectorAll('.renderer[data-renderer-kind]')].map(r => ({ name: (r.querySelector('.renderer-title') || {}).textContent || '', target: (r.querySelector('.renderer-target') || {}).textContent || '', state: r.dataset.state || '', pill: (r.querySelector('.renderer-state') || {}).textContent || '' }))",
        },
    );
    let heads = heads.as_array().cloned().unwrap_or_default();
    let find = |name: &str, target: &str| {
        heads
            .iter()
            .find(|h| h["name"] == name && h["target"] == target)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} {target} head among {heads:?}"))
    };
    let failed = find("Bash", "cargo test --lib");
    let long = find("Bash", "cargo build --release");
    let declined = find("Bash", "cargo fmt");
    let update = find("Update", "README.md");
    match surface {
        Surface::Classic => {
            // A call with output carries its line count first; the execution chip is last.
            let last = |h: &serde_json::Value| {
                h["chips"]
                    .as_array()
                    .and_then(|c| c.last().cloned())
                    .unwrap_or_default()
            };
            assert_eq!(
                last(&failed),
                serde_json::json!({ "c": "fail", "x": "exit 1 · 2.50s" }),
                "a failed call: failure presentation with its exit and duration"
            );
            assert_eq!(
                last(&long),
                serde_json::json!({ "c": "", "x": "exit 0 · 1m 5s" }),
                "a long call shows its duration"
            );
            assert_eq!(
                last(&declined),
                serde_json::json!({ "c": "fail", "x": "declined · 42ms" })
            );
            assert_eq!(
                update["chips"][0]["c"], "add",
                "an Edit reads Update: {update:?}"
            );
        }
        Surface::AppShell => {
            assert_eq!(
                (failed["state"].as_str(), failed["pill"].as_str()),
                (Some("failed"), Some("failed · exit 1")),
                "a failed call's pill names the failure and its exit"
            );
            assert_eq!(long["state"].as_str(), Some("completed"));
            assert!(
                long["pill"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with("exit 0 · 1m 5s"),
                "a long call's pill shows its duration: {long:?}"
            );
            assert_eq!(
                (declined["state"].as_str(), declined["pill"].as_str()),
                (Some("failed"), Some("declined"))
            );
            assert_eq!(
                (update["state"].as_str(), update["pill"].as_str()),
                (Some("completed"), Some("+1 · −1")),
                "an Edit reads Update with its change chips"
            );
        }
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_tool_heads_carry_state_exit_and_duration() {
    let _serial = serial();
    let fx = fixture_codex("scenario-toolhead-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_tool_heads_carry_state_exit_and_duration(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_tool_heads_carry_state_exit_and_duration() {
    let _serial = serial();
    let fx = fixture_codex("scenario-toolhead-app", 12);
    let page = open(Surface::AppShell, &fx, 2896);
    scenario_tool_heads_carry_state_exit_and_duration(&page.tab, Surface::AppShell, &fx);
}

/// A spawn's `launched` chip is a launch EVENT, not liveness (#117): a closed session's
/// sub-agents must read as finished work on both pages, not as calls still in flight.
fn scenario_a_launched_spawn_is_not_a_running_head(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    if surface == Surface::AppShell {
        eval(
            tab,
            "document.querySelectorAll('[data-process-more][aria-expanded=\"false\"]').forEach(b => b.click())",
        );
        settle();
    }
    match surface {
        Surface::Classic => {
            let chips = probe(
                tab,
                "[...document.querySelectorAll('#stream .fold .chip')].map(c => c.textContent)",
            );
            let chips = chips.as_array().cloned().unwrap_or_default();
            assert!(
                chips.iter().any(|c| c.as_str().unwrap_or("") == "launched"),
                "the reference page shows the launch event as its own chip: {chips:?}"
            );
        }
        Surface::AppShell => {
            let spawns = probe(
                tab,
                "[...document.querySelectorAll('.renderer[data-renderer-kind=\"agent\"]')].map(r => ({ state: r.dataset.state || '', pill: (r.querySelector('.renderer-state') || {}).textContent || '', closed: r.classList.contains('closed') }))",
            );
            let spawns = spawns.as_array().cloned().unwrap_or_default();
            assert!(!spawns.is_empty(), "the fixture's spawns mounted");
            for spawn in &spawns {
                assert_eq!(
                    (spawn["state"].as_str(), spawn["pill"].as_str()),
                    (Some("completed"), Some("launched")),
                    "a launch event is not an in-flight call: {spawn:?}"
                );
                assert_eq!(
                    spawn["closed"], true,
                    "…and it does not force its fold open: {spawn:?}"
                );
            }
            let running = probe(
                tab,
                "document.querySelectorAll('.renderer[data-state=\"running\"], .process-surface.process-running').length",
            );
            assert_eq!(
                running.as_i64(),
                Some(0),
                "nothing in a closed session reads as running"
            );
        }
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_launched_spawn_is_not_a_running_head() {
    let _serial = serial();
    let fx = fixture_spawns("scenario-spawn-classic", 14);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_launched_spawn_is_not_a_running_head(&page.tab, Surface::Classic, &fx);
}

/// A BARE tool result — a `tool_result` with no call before it — reads as the classic page
/// draws it (#122, parity row 3.18): a `Result` row whose target is the first 70 characters of
/// the text and whose body is the ⎿ gutter with the output beside it.
fn scenario_a_bare_result_reads_as_a_result_row(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let row = probe(
        tab,
        match surface {
            Surface::Classic => "(function () { var f = [...document.querySelectorAll('#stream .fold')].find(f => (f.querySelector('.tool-name') || {}).textContent === 'Result'); if (!f) return null; f.querySelector('.fold-h').click(); var b = f.querySelector('.fold-b'); return { name: f.querySelector('.tool-name').textContent, target: (f.querySelector('.tool-target') || {}).textContent || '', mark: (b.querySelector('.result > .lead') || {}).textContent || '', body: (b.querySelector('.result > .resultbox > pre') || {}).textContent || '' }; })()",
            Surface::AppShell => "(function () { var r = [...document.querySelectorAll('.renderer[data-renderer-kind]')].find(r => (r.querySelector('.renderer-title') || {}).textContent === 'Result'); if (!r) return null; r.querySelector('.renderer-head').click(); var b = r.querySelector('.renderer-body'); return { name: r.querySelector('.renderer-title').textContent, target: (r.querySelector('.renderer-target') || {}).textContent || '', mark: (b.querySelector('.renderer-result > .renderer-result-lead') || {}).textContent || '', body: (b.querySelector('.renderer-result > .renderer-result-box > pre') || {}).textContent || '' }; })()",
        },
    );
    assert!(!row.is_null(), "a bare result mounted as its own row");
    assert_eq!(row["name"], "Result", "the row names what it is: {row:?}");
    assert_eq!(
        row["target"], "checked 42 files and found the one that matters, a very long first lin…",
        "…and its target is the first 70 characters: {row:?}"
    );
    assert_eq!(row["mark"], "⎿", "the result gutter: {row:?}");
    assert!(
        row["body"]
            .as_str()
            .unwrap_or("")
            .starts_with("checked 42 files")
            && row["body"].as_str().unwrap_or("").ends_with("third line"),
        "…with the whole text beside it: {row:?}"
    );
}

/// An agent's own question to the reader wears the same card on both pages (#121, parity row
/// 3.17): waiting says where the answer goes, answered shows what it was.
fn scenario_a_request_for_input_is_a_card(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    if surface == Surface::AppShell {
        eval(
            tab,
            "document.querySelectorAll('[data-process-more][aria-expanded=\"false\"]').forEach(b => b.click())",
        );
        settle();
    }
    let (card, strong, answer) = match surface {
        Surface::Classic => (".irq", ".irq-copy > strong", ".irq-answer"),
        Surface::AppShell => (
            ".input-request",
            ".input-request-copy > strong",
            ".input-answer",
        ),
    };
    let cards = probe(
        tab,
        &format!(
            "[...document.querySelectorAll('{card}')].map(c => ({{ state: c.className.replace('{}', '').trim(), title: (c.querySelector('{strong}') || {{}}).textContent || '', text: (c.querySelector('p') || {{}}).textContent || '', answers: [...c.querySelectorAll('{answer}')].map(a => a.textContent) }}))",
            card.trim_start_matches('.')
        ),
    );
    let cards = cards.as_array().cloned().unwrap_or_default();
    assert_eq!(cards.len(), 2, "both questions drew a card: {cards:?}");
    let waiting = cards
        .iter()
        .find(|c| c["state"] == "waiting")
        .unwrap_or_else(|| panic!("no waiting card among {cards:?}"));
    assert_eq!(waiting["title"], "Waiting for user input");
    assert!(
        waiting["text"]
            .as_str()
            .unwrap_or("")
            .contains("Monitor cannot submit this native prompt"),
        "…and says where the answer goes: {waiting:?}"
    );
    assert_eq!(waiting["answers"].as_array().map(Vec::len), Some(0));
    let resolved = cards
        .iter()
        .find(|c| c["state"] == "resolved")
        .unwrap_or_else(|| panic!("no resolved card among {cards:?}"));
    assert_eq!(resolved["title"], "User input received");
    assert_eq!(
        resolved["answers"][0], "Yes, ship itship",
        "the answer and the field it belongs to: {resolved:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_request_for_input_is_a_card() {
    let _serial = serial();
    let fx = fixture_input_requests("scenario-input-classic", 14);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_request_for_input_is_a_card(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_request_for_input_is_a_card() {
    let _serial = serial();
    let fx = fixture_input_requests("scenario-input-app", 14);
    let page = open(Surface::AppShell, &fx, 2899);
    scenario_a_request_for_input_is_a_card(&page.tab, Surface::AppShell, &fx);
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_bare_result_reads_as_a_result_row() {
    let _serial = serial();
    let fx = fixture_bare_result("scenario-bare-classic", 14);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_bare_result_reads_as_a_result_row(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_bare_result_reads_as_a_result_row() {
    let _serial = serial();
    let fx = fixture_bare_result("scenario-bare-app", 14);
    let page = open(Surface::AppShell, &fx, 2898);
    scenario_a_bare_result_reads_as_a_result_row(&page.tab, Surface::AppShell, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_launched_spawn_is_not_a_running_head() {
    let _serial = serial();
    let fx = fixture_spawns("scenario-spawn-app", 14);
    let page = open(Surface::AppShell, &fx, 2897);
    scenario_a_launched_spawn_is_not_a_running_head(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a record's size is its own box, so a prefix sum of sizes is where the next one
//    starts (#128 step 1) ─────────────────────────────────────────────────────────────────────

/// Both pages place records by SUMMING their sizes: the classic page's pads are a prefix sum,
/// the app shell's virtual window is one. That arithmetic reads a record as its border box plus
/// its margins, and it is right only when the distance to the next record's top IS that number —
/// which needs two things of the CSS: nothing carries a margin on its TOP (the sum would be short
/// by exactly that margin), and adjacent margins do not COLLAPSE (the sum would be long by the
/// smaller of each pair). Before #128 the classic page failed both — four block kinds had top
/// margins and `#stream` was ordinary block layout, so on a real transcript 26 of 27 mounted
/// pairs disagreed with their own measure. This holds every mounted pair to it, on both pages,
/// with folds shut and again with folds open (an open fold must not resize the record ABOVE it).
fn scenario_a_record_measures_as_its_own_box(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    let children = match surface {
        Surface::Classic => "document.querySelectorAll('#vwin > .blk')",
        Surface::AppShell => "document.querySelectorAll('.virtual-window > [data-unit-from]')",
    };
    // Every mounted record, in document order: its own measure against the true stacking
    // distance to the next one's top. The last has no successor, so it only reports its margins.
    let js = format!(
        "(function(){{ var k = [].slice.call({children}), out = {{n: k.length, tops: [], bad: []}}; \
         for (var i = 0; i < k.length; i++) {{ var s = getComputedStyle(k[i]), r = k[i].getBoundingClientRect(); \
           var mt = parseFloat(s.marginTop) || 0, mb = parseFloat(s.marginBottom) || 0; \
           if (mt !== 0) out.tops.push({{at: i, mt: mt, cls: k[i].className}}); \
           if (i + 1 < k.length) {{ var d = k[i+1].getBoundingClientRect().top - r.top; \
             if (Math.abs((r.height + mt + mb) - d) > 0.5) out.bad.push({{at: i, size: Math.round((r.height+mt+mb)*10)/10, dist: Math.round(d*10)/10, cls: k[i].className}}); }} }} \
         return out; }})()"
    );
    let check = |what: &str| {
        let seen = harness::probe(tab, &js);
        let n = seen["n"].as_i64().unwrap_or(0);
        assert!(n >= 4, "{what}: records are mounted to measure ({seen})");
        assert_eq!(
            seen["tops"].as_array().map(Vec::len),
            Some(0),
            "{what}: no mounted record carries a margin on its top — a sum of sizes would be short by it ({seen})"
        );
        assert_eq!(
            seen["bad"].as_array().map(Vec::len),
            Some(0),
            "{what}: every record's size is the distance to the next record's top ({seen})"
        );
    };
    // Mid-transcript, where both pages are windowed and the sums are actually load-bearing.
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the jump to land at the tail");
    scroll_by(tab, surface, -4000);
    settle();
    check("scrolled into the middle");
    // Open the folds in view: an open fold grows DOWNWARD, and must not change what the record
    // above it measures — the case that rules out keying spacing on fold state.
    let opened = open_last_fold(tab, surface);
    assert!(opened != -1, "a fold header was found to open ({opened})");
    settle();
    check("with a fold opened under the reader");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_record_measures_as_its_own_box() {
    let _serial = serial();
    let fx = fixture("scenario-measure-classic", 120);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_record_measures_as_its_own_box(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_record_measures_as_its_own_box() {
    let _serial = serial();
    let fx = fixture("scenario-measure-app", 120);
    let page = open(Surface::AppShell, &fx, 2913);
    scenario_a_record_measures_as_its_own_box(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a numbered code row gives its code the width, not the mark column (#146) ────────

/// A fixture whose tail is a `Write` of 40 lines — so the record carries a NUMBERED source part —
/// with a shebang first and one long unbroken line second.
fn fixture_write(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    jsonl += &write_tool_at("wr1", "/tmp/patch.py", 40, "2026-09-01T03:00:00.000Z");
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

/// #146: a numbered source row is `[gutter, code]` — TWO cells — and the row's layout must give
/// the code the room. The app shell laid those two cells into a THREE-column grid built for diff
/// rows (`38px 16px minmax(max-content,1fr)`), so the code landed in the 16px MARK track: with
/// wrapping on it wrapped at about one character per row (the owner's screenshot: a Write of 154
/// lines rendered `#` / `!` / `/` / `u` / `s` / `r` …), and with wrapping off it ran past the
/// card. The classic page never had it — its row is `display:flex` with a fixed-width gutter, so
/// two cells and three cells both lay out — which is why this is written once and run on both:
/// the reference page proves the rule is right before the other page is held to it.
fn scenario_numbered_code_has_the_width(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the jump to land at the tail");
    // Open the ONE fold that owns the numbered rows, and only it. Clicking every head in a single
    // pass does not work on the app shell: the first click re-renders the window, so every later
    // click in that pass lands on a detached node — which is why this measured nothing at all
    // until the rows were reached this way.
    for _ in 0..6 {
        let state = eval(tab, "(function(){ var l = document.querySelector('.nrow, .line'); if (!l) return 'no rows'; var f = l.closest('.renderer, .fold'); if (!f) return 'no fold'; if (!f.classList.contains('closed') && f.dataset.open !== '0') return 'open'; var h = f.querySelector('button.renderer-head') || f.querySelector(':scope > .fold-h'); if (!h) return 'no head'; h.click(); return 'clicked'; })()");
        settle();
        if state == "open" || state == "no rows" {
            break;
        }
    }
    settle();
    // Every VISIBLE numbered row: how wide its code cell is against the row, and whether the row
    // overflows the box that is supposed to hold it.
    let rows = probe(
        tab,
        r#"(function(){ var out = { rows: 0, thin: [], overflow: [] }; var sel = document.querySelectorAll('.nrow, .line'); for (var r of sel) { if (r.offsetParent === null) continue; var code = r.querySelector('.code, .codecell'); if (!code) continue; var rr = r.getBoundingClientRect(), cr = code.getBoundingClientRect(); if (rr.width < 40) continue; out.rows++; if (cr.width < rr.width * 0.5) out.thin.push(Math.round(cr.width) + '/' + Math.round(rr.width)); var box = r.closest('.codebox, .num, .codewrap') || r.parentElement; if (box && box.scrollWidth > box.clientWidth + 1 && box.clientWidth > 0) out.overflow.push((box.className.split(' ')[0] || 'box') + ':' + box.clientWidth + '/' + box.scrollWidth); } out.thin = out.thin.slice(0, 4); out.overflow = out.overflow.slice(0, 3); out.seen = { nrow: document.querySelectorAll('.nrow').length, line: document.querySelectorAll('.line').length, folds: document.querySelectorAll('.fold-h, .renderer-head').length }; return out; })()"#,
    );
    assert!(
        rows["rows"].as_i64().unwrap_or(0) >= 5,
        "the numbered rows are on screen to be measured: {rows}"
    );
    assert_eq!(
        rows["thin"].as_array().map(Vec::len),
        Some(0),
        "every numbered row gives its CODE the width, not the mark column — a code cell under half \
         the row's width is the squeeze that wraps a line one character at a time: {rows}"
    );
    assert_eq!(
        rows["overflow"].as_array().map(Vec::len),
        Some(0),
        "…and no numbered row pushes its own box wider than the box can show: {rows}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_numbered_code_has_the_width() {
    let _serial = serial();
    let fx = fixture_write("scenario-numcode-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_numbered_code_has_the_width(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_numbered_code_has_the_width() {
    let _serial = serial();
    let fx = fixture_write("scenario-numcode-app", 12);
    let page = open(Surface::AppShell, &fx, 2920);
    scenario_numbered_code_has_the_width(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: an image PASTED INTO A PROMPT opens, like one a tool returned (#144) ───────────

/// A fixture whose last turn carries a pasted screenshot: text and an inline base64 image in the
/// same user message, which the engine surfaces as an `attachment` record attached to the prompt.
fn fixture_pasted_image(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    // A REAL screenshot's size, not a 1x1 (harness::BIG_PNG_B64): the owner's own session holds
    // 138 embedded images with a median of 108 KB, and the bug this guards was invisible at the
    // sizes the older image case used.
    jsonl += &harness::pasted_image_sized(
        "here is a screenshot",
        "2026-09-01T04:00:00.000Z",
        harness::BIG_PNG_B64,
    );
    jsonl += &assistant_at("looking", "2026-09-01T04:00:05.000Z");
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

/// #144: an image the reader PASTED into a prompt must open at full size, exactly as one a tool
/// returned does. The two arrive by different routes — a pasted image becomes a prompt
/// attachment, a returned one a row inside a process — and only the second had a case, so the
/// first was free to break: the owner clicked a thumbnail and got "That image cannot be opened".
/// The thumbnail is the proof the bytes are there; what this holds is that CLICKING it works.
fn scenario_a_pasted_image_opens(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    let thumb = match surface {
        Surface::Classic => ".amark img",
        Surface::AppShell => ".prompt-image img",
    };
    until(
        tab,
        &format!("(function(){{ var i = document.querySelector('{thumb}'); return !!i && i.naturalWidth >= 1 && i.getBoundingClientRect().height > 0; }})()"),
        "the pasted image to show as a visible thumbnail",
        Duration::from_secs(20),
        &format!("(function(){{ var i = document.querySelector('{thumb}'); return i ? JSON.stringify({{ src: (i.getAttribute('src')||'').slice(0, 24), natural: i.naturalWidth }}) : 'no thumbnail: ' + document.querySelectorAll('.amark, .prompt-attachment').length + ' attachment cards'; }})()"),
    );
    if surface == Surface::AppShell {
        // The classic page shows a pasted image inline and has nothing to click; the app shell
        // makes the thumbnail the way in, so the click is the rule.
        eval(tab, "document.querySelector('.prompt-image').click(); 'ok'");
        until(
            tab,
            "(function(){ var l = document.querySelector('.image-lightbox'); if (!l || l.hidden) return false; var img = l.querySelector('img'); return l.dataset.state !== 'unavailable' && !!img && !img.hidden && img.naturalWidth >= 1; })()",
            "the click to open the image, not the 'cannot be opened' card",
            Duration::from_secs(10),
            "(function(){ var l = document.querySelector('.image-lightbox'); if (!l) return 'no lightbox'; var i = l.querySelector('img'); return JSON.stringify({ state: l.dataset.state, hidden: l.hidden, src: (i && i.getAttribute('src') || '').slice(0, 24), natural: i ? i.naturalWidth : -1 }); })()",
        );
        // And the rule that made this bug possible at all: the lightbox must never show LESS
        // than the thumbnail is already showing. The card was rendered from data the view held;
        // looking the record up again by id is a second, weaker path to the same bytes, and when
        // it misses — a tail rewrite replacing records, a stale id in a DOM that has not
        // re-rendered — an embedded image has no path or stamp to fall back on. Simulate the miss
        // exactly: point the card at a record id that is not in the stream, and click it.
        eval(
            tab,
            "document.querySelector('.image-lightbox [data-lightbox-close]').click(); document.querySelector('.prompt-image').dataset.attachment = 'no-such-record'; 'ok'",
        );
        settle();
        eval(tab, "document.querySelector('.prompt-image').click(); 'ok'");
        until(
            tab,
            "(function(){ var l = document.querySelector('.image-lightbox'); if (!l || l.hidden) return false; var i = l.querySelector('img'); return l.dataset.state === 'ready' && !!i && i.naturalWidth >= 1; })()",
            "the lightbox to show what the thumbnail showed even when the record lookup misses",
            Duration::from_secs(10),
            "(function(){ var l = document.querySelector('.image-lightbox'); var i = l && l.querySelector('img'); return JSON.stringify({ state: l && l.dataset.state, src: (i && i.getAttribute('src') || '').slice(0, 24), natural: i ? i.naturalWidth : -1 }); })()",
        );
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_pasted_image_opens() {
    let _serial = serial();
    let fx = fixture_pasted_image("scenario-paste-classic", 8);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_pasted_image_opens(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_pasted_image_opens() {
    let _serial = serial();
    let fx = fixture_pasted_image("scenario-paste-app", 8);
    let page = open(Surface::AppShell, &fx, 2921);
    scenario_a_pasted_image_opens(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: descending from a fleet row leaves a way back (#143) ────────────────────────────

/// #143: the app shell navigates IN-PAGE, so the way back to a parent is a hint recorded at the
/// moment of descent — and the hint used to be written at one descent only, the Agents pane. A
/// reader who went down from a workflow's fleet roster arrived with no way back, which is what the
/// owner hit. Every descent now goes through one door (`descendTo`), and this is the behaviour
/// that says so: descend by CLICKING a fleet row, and the parent control must come alive.
///
/// App-shell only, for a reason worth recording rather than hiding: the classic page reloads on a
/// fleet-row click (the row is an `<a href="?session=…">`) and derives its `↑ parent › current`
/// breadcrumb from the transcript's own `ancestors`. It keeps no hint, so it cannot lose one — a
/// mechanism difference, not a missing feature.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_fleet_row_descent_keeps_the_way_back() {
    let _serial = serial();
    let fx = fixture_workflow("scenario-fleet-descent");
    let page = open(Surface::AppShell, &fx, 2923);
    let tab = &page.tab;
    let heights = r#"(function(){var vw=document.querySelector('.virtual-window');var out={};for(const c of vw.children){out[(c.dataset.unitIndex||'?')+':'+(c.dataset.unitKey||'?')]=Math.round(c.getBoundingClientRect().height);}var t=document.querySelectorAll('.virtual-pad');out['__pads']=[t[0].style.height,t[1].style.height];return out;})()"#;
    jump_to_end(tab, Surface::AppShell);
    await_tail(tab, Surface::AppShell, "the jump to land at the tail");
    until(
        tab,
        "!!document.querySelector('.fleet-name')",
        "the workflow call to render its fleet roster",
        Duration::from_secs(20),
        "document.querySelectorAll('.fleet, .fleet-row').length + ' fleet nodes'",
    );
    // A reader clicks the member's name. Anything that navigates by hand instead would test the
    // URL, not the descent — and the descent is where the way back is recorded.
    eval(tab, "document.querySelector('.fleet-name').click(); 'ok'");
    until(
        tab,
        &format!("new URLSearchParams(location.search).get('session') !== {SID:?}"),
        "the click to open the member's own session",
        Duration::from_secs(20),
        "new URLSearchParams(location.search).get('session')",
    );
    until(
        tab,
        "document.getElementById('sessionParent').classList.contains('is-live')",
        "the way back to the parent to be live after descending through a fleet row",
        Duration::from_secs(20),
        "(function(){ var b = document.getElementById('sessionParent'); return JSON.stringify({ live: b.classList.contains('is-live'), parent: b.dataset.parent, session: new URLSearchParams(location.search).get('session') }); })()",
    );
    let control = probe(tab, "(function(){ var b = document.getElementById('sessionParent'); var r = b.getBoundingClientRect(); return { visible: b.offsetParent !== null && r.width >= 24 && r.height >= 20, parent: b.dataset.parent }; })()");
    assert_eq!(
        control["visible"], true,
        "…and it is VISIBLE, not merely present: {control}"
    );
    assert_eq!(
        control["parent"], SID,
        "…and it points at the session the reader came from: {control}"
    );
}

// ── scenario: wide mode is one click from the header, on either page (#142) ───────────────────

/// #142: the owner asked three times where the wide-mode control was. On the classic page it is
/// `#btn-wide`, a plain toolbar button — one click, always visible. The app shell had the same
/// preference buried three levels down: a FUNNEL icon inside the header SEARCH box, opening a
/// popover whose last section, under an unsorted and unbounded tool list, held Reading. Measured
/// at 1500x940 with only five tool rows the popover already needed scrolling; twenty MCP tools
/// push Reading roughly 400px below the fold.
///
/// So the rule this pins is the classic page's, written as a rule rather than as a selector:
/// **a persistently visible header control leads to the wide toggle in at most one click, and
/// toggling it actually widens the transcript.** The classic page satisfies it with a direct
/// button; the app shell now satisfies it with its own reading control. What neither may do is
/// hide it behind a *filter* affordance — so the case also asserts the funnel no longer offers
/// it, which is what makes "one control, one meaning" more than a comment.
///
/// Hit-testing, not rects: a clipped control still measures 34x34 (that is how a control that
/// could not be reached once passed a width assertion), so every step here goes through
/// `elementFromPoint`. And it runs at a narrow window too, because the header is where controls
/// go to be dropped when space runs out.
fn scenario_wide_is_one_click_from_the_header(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // A control is "reachable" only if the point at its centre actually hits it.
    let hittable = r#"function (el) { if (!el) return false; var r = el.getBoundingClientRect(); if (r.width < 4 || r.height < 4) return false; var hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!(hit && (hit === el || el.contains(hit) || hit.contains(el))); }"#;

    for want in [1500.0_f64, 820.0] {
        harness::resize(tab, want, 900.0);
        // Let the resize LAND, then measure what the window really is. `set_bounds` is a
        // request, not a guarantee: on CI's headless Linux it never reached 1500px, and an
        // assertion keyed to the REQUESTED width then tested a viewport that was never there —
        // it read as "wide mode does nothing" (820 -> 821) on a page simply too narrow to
        // widen. Poll until the width stops moving rather than `until`, which would panic on a
        // browser that cannot honour the request at all; every branch below keys off `width`,
        // the measured value.
        let mut width = 0.0;
        for _ in 0..10 {
            settle();
            let now = probe(tab, "({ w: innerWidth })")["w"]
                .as_f64()
                .unwrap_or(0.0);
            if now == width && now > 0.0 {
                break;
            }
            width = now;
        }
        assert!(width > 0.0, "the window reports a width at all");
        // Start each width from the page at rest. Without this the previous width's popover is
        // still open, and step 2's click CLOSES it instead of opening it — which read exactly
        // like the control being unreachable, at the width where a real one had just been found.
        eval(
            tab,
            "(function(){ for (var p of document.querySelectorAll('.navigator-options.open')) p.classList.remove('open'); return 'ok'; })()",
        );
        settle();

        // Step 1: the entry control is visible on the page at rest, with nothing opened first.
        let entry = match surface {
            Surface::Classic => "btn-wide",
            Surface::AppShell => "readingBtn",
        };
        let seen = probe(
            tab,
            &format!(
                "(function(){{ var hittable = {hittable}; var e = document.getElementById('{entry}'); \
                 return {{ present: !!e, reachable: hittable(e), header: !!(e && e.closest('header, .topbar, .bar, .toolbar')) }}; }})()"
            ),
        );
        assert_eq!(
            seen["present"].as_bool(),
            Some(true),
            "at {width}px the header offers a reading control (`#{entry}`) on the page at rest: {seen}"
        );
        assert_eq!(
            seen["reachable"].as_bool(),
            Some(true),
            "…and it is actually clickable there, not merely measured: {seen}"
        );

        // Step 2: at most ONE click reaches a wide toggle that is itself clickable. On the
        // classic page the entry IS the toggle; on the app shell the entry opens the popover
        // that holds it.
        let reach = probe(
            tab,
            &format!(
                "(function(){{ var hittable = {hittable}; \
                 var wide = document.querySelector('[data-reading-toggle=\"wide\"]'); \
                 if (!wide) return {{ clicks: 0, toggle: 'the entry itself' }}; \
                 document.getElementById('{entry}').click(); \
                 return {{ clicks: 1, toggle: 'in the popover', reachable: hittable(document.querySelector('[data-reading-toggle=\"wide\"]')) }}; }})()"
            ),
        );
        settle();
        if reach["clicks"].as_i64() == Some(1) {
            assert_eq!(
                reach["reachable"].as_bool(),
                Some(true),
                "at {width}px one click on `#{entry}` reveals a clickable wide toggle: {reach}"
            );
        }

        // Step 3: the toggle does the thing — asserted only where there is room to widen. At
        // 820px the classic page's stream is ALREADY the full width (measured 532 -> 532), so a
        // widening assertion there would be testing the viewport, not the control. The narrow
        // pass still proves reachability, which is the half that regresses.
        let (before, avail) = transcript_box(tab, surface);
        let wide_before = wide_is_on(tab, surface);
        let toggled = probe(
            tab,
            &format!(
                "(function(){{ var w = document.querySelector('[data-reading-toggle=\"wide\"]'); \
                 if (w) {{ w.click(); return 'popover toggle'; }} \
                 var b = document.getElementById('{entry}'); if (b) {{ b.click(); return 'direct button'; }} \
                 return 'none'; }})()"
            ),
        );
        settle();
        settle();
        let (after, _) = transcript_box(tab, surface);
        let wide_after = wide_is_on(tab, surface);
        // The EFFECT, asserted without reference to the viewport: the click actually flipped the
        // preference. This holds on any window, which is what the rendered-width check below
        // cannot promise — CI's headless Linux would not honour a 1500px resize, and on a column
        // with no slack a perfectly working control moves the transcript by one pixel (820 ->
        // 821), which reads exactly like a broken one.
        assert_ne!(
            wide_before, wide_after,
            "at {width}px the click ({toggled}) actually flipped wide mode: {wide_before} -> {wide_after}"
        );
        // Only assert the widening where the normal-mode transcript is actually being held in
        // by its own max-width — i.e. where there is room to widen. Below that, wide mode has
        // nothing to do and the measurement would be about the viewport, not the control: the
        // classic page's stream is already full width at 820px (measured 532 -> 532). The
        // narrow pass still proves reachability, which is the half that regresses.
        // …and where the column has real slack, the change is visible in the rendering too.
        // The threshold is generous on purpose: `avail` does not subtract every ancestor's
        // padding, so a small positive difference is not evidence of room.
        if avail - before > 120.0 {
            assert!(
                after > before + 8.0,
                "at {width}px, with {avail} available to a {before}-wide transcript, toggling wide \
                 ({toggled}) actually widens it: {before} -> {after}"
            );
        } else {
            assert!(
                after >= before,
                "at {width}px the transcript already fills its column ({before} of {avail}), so \
                 toggling wide ({toggled}) has nothing to widen — but it must never NARROW it: \
                 {before} -> {after}"
            );
        }

        // Put it back, so the next width starts from the same place.
        eval(
            tab,
            &format!(
                "(function(){{ var w = document.querySelector('[data-reading-toggle=\"wide\"]'); \
                 if (w) {{ w.click(); }} else {{ var b = document.getElementById('{entry}'); if (b) b.click(); }} return 'ok'; }})()"
            ),
        );
        settle();
    }

    // Step 4: the FILTER affordance no longer offers a reading preference. This is the half that
    // makes each control mean one thing, and it is app-shell-only because the classic page never
    // had a filter popover to put them in.
    if surface == Surface::AppShell {
        let funnel = probe(
            tab,
            r#"(function(){ var f = document.getElementById('navigatorOptions'); if (!f) return { missing: true }; return { reading: f.querySelectorAll('[data-reading-toggle], [data-reading-size]').length, has: !!document.getElementById('readingOptions') }; })()"#,
        );
        assert_eq!(
            funnel["reading"].as_i64(),
            Some(0),
            "the search-scope funnel holds no reading preferences at all — they live behind the \
             reading control now: {funnel}"
        );
        assert_eq!(
            funnel["has"].as_bool(),
            Some(true),
            "…and that control has its own popover: {funnel}"
        );
    }
}

/// The transcript's content width on either page — what "wide" is supposed to change — and the
/// width AVAILABLE to it inside its scroller.
///
/// Both numbers are needed because "is there room to widen?" is a question about the column, not
/// about the window. CI's headless Linux never honoured a 1500px `set_bounds`, and a check keyed
/// to the viewport still concluded there was room when there was none: the transcript measured
/// 820 of an ~820px column, already at the container's edge, so raising its max-width moved it
/// one pixel. Comparing content against container asks the question directly and gives the same
/// answer on any machine.
/// Whether wide mode is ON, read from where each page actually keeps it — the one thing that is
/// true regardless of how much room the window happens to give the column.
///
/// The two pages express it differently and neither is guessable from geometry: the classic page
/// sets `#main`'s inline `max-width` ("820px" off, "none" on — `applyWide` in export.js); the app
/// shell toggles a `wide` class on its root (`applyReading` in app.js) and caps nothing with
/// max-width at all. Walking the ancestry for a max-width change found "none" at every level in
/// both states, which is how that assertion passed on one page and failed on the other while
/// both controls were working perfectly.
fn wide_is_on(tab: &headless_chrome::Tab, surface: Surface) -> bool {
    let js = match surface {
        Surface::Classic => "(function(){ var m = document.getElementById('main'); return { on: !!m && m.style.maxWidth === 'none' }; })()",
        Surface::AppShell => "(function(){ var a = document.getElementById('app'); return { on: !!a && a.classList.contains('wide') }; })()",
    };
    probe(tab, js)["on"].as_bool().unwrap_or(false)
}

fn transcript_box(tab: &headless_chrome::Tab, surface: Surface) -> (f64, f64) {
    let (content, holder) = match surface {
        Surface::Classic => ("#stream", "#stream"),
        Surface::AppShell => (".virtual-window", ".transcript"),
    };
    let v = probe(
        tab,
        &format!(
            "(function(){{ var e = document.querySelector('{content}'); var h = document.querySelector('{holder}');              var avail = 0;              if (h === e && e) {{ var p = e.parentElement; avail = p ? p.clientWidth : 0; }}              else if (h) {{ var cs = getComputedStyle(h); avail = h.clientWidth - parseFloat(cs.paddingLeft || 0) - parseFloat(cs.paddingRight || 0); }}              return {{ w: e ? e.getBoundingClientRect().width : 0, avail: avail }}; }})()"
        ),
    );
    (
        v["w"].as_f64().unwrap_or(0.0),
        v["avail"].as_f64().unwrap_or(0.0),
    )
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_wide_is_one_click_from_the_header() {
    let _serial = serial();
    let fx = fixture("scenario-wide-classic", 12);
    let page = open(Surface::Classic, &fx, 0);
    scenario_wide_is_one_click_from_the_header(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_wide_is_one_click_from_the_header() {
    let _serial = serial();
    let fx = fixture("scenario-wide-app", 12);
    let page = open(Surface::AppShell, &fx, 2924);
    scenario_wide_is_one_click_from_the_header(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: an attachment card says WHY the file is there, not just what opens (#171) ──────

/// A fixture whose last prompt carries a file the reader had open in their editor. The engine
/// surfaces it as an `attachment` record right after the user message; both pages hang it off
/// that prompt. The name deliberately contains no form of "edit", so an assertion looking for
/// the verb cannot pass on the filename.
fn fixture_edited_attachment(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    jsonl += &harness::user_at("does this read right", "2026-09-08T04:00:00.000Z");
    jsonl += &harness::edited_file_at("/w/notes/crux-on-linux.md", "2026-09-08T04:00:01.000Z");
    jsonl += &assistant_at("reading it now", "2026-09-08T04:00:05.000Z");
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

/// #171: the card must name the REASON the file is attached, not only what a click does. The
/// owner put the two surfaces side by side: the classic page said `edited crux-on-linux.md` —
/// a verb and a name — and the app shell showed a card with an MD badge, the filename and
/// "opens in the preview pane", which describes the affordance and leaves the reason unsaid.
///
/// The field was there the whole time. `att_kind` holds "edited"; the classic page reads it at
/// export.js's `.akind`, the shell's OWN renderer-note fallback reads it, and only the prompt
/// card — the path a previewable file actually takes — dropped it.
///
/// The ranking is #166's: what the agent (or the reader) DID outranks what the UI offers. So
/// the verb sits in the title row beside the name, and the affordance stays one rank below.
fn scenario_an_attachment_says_why_it_is_there(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    // Classic writes the verb into `.akind` beside `.aname`; the shell into the card's title row.
    let (kind, name) = match surface {
        Surface::Classic => (".amark .akind", ".amark .aname"),
        Surface::AppShell => (".prompt-file .prompt-file-kind", ".prompt-file strong"),
    };
    until(
        tab,
        &format!("!!document.querySelector('{name}')"),
        "the attachment to render as a card naming the file",
        Duration::from_secs(20),
        "document.querySelectorAll('.amark, .prompt-attachment').length + ' attachment cards'",
    );
    let seen = probe(
        tab,
        &format!(
            "(function(){{ var k = document.querySelector('{kind}'); var n = document.querySelector('{name}');              var kb = k && k.getBoundingClientRect(), nb = n && n.getBoundingClientRect();              return {{ kind: k ? (k.textContent || '').trim() : null,                       width: kb ? kb.width : 0,                       name: n ? (n.textContent || '').trim() : null,                       nameWidth: nb ? nb.width : 0,                       sameLine: !!(kb && nb) && Math.abs(kb.bottom - nb.bottom) < 4 }}; }})()"
        ),
    );
    assert_eq!(
        seen["kind"].as_str(),
        Some("edited"),
        "{surface:?}: the card must say why the file is there, from att_kind — saw {seen}"
    );
    // A `flex: none` label that a cascade collision has collapsed still has text; only its box
    // says whether the reader can read it (rect-is-not-visibility's cheaper half).
    assert!(
        seen["width"].as_f64().unwrap_or(0.0) > 0.0,
        "{surface:?}: the verb must occupy real width, not be collapsed to nothing — saw {seen}"
    );
    assert_eq!(
        seen["name"].as_str(),
        Some("crux-on-linux.md"),
        "{surface:?}: and the name stays the thing the reader scans for — saw {seen}"
    );
    // The two read as ONE phrase — "edited crux-on-linux.md" — which is the whole point: a verb
    // stacked above its noun is two facts, a verb beside it is a reason. And the name must keep
    // most of the room; the verb is `flex: none` precisely so it can never take the name's.
    assert!(
        seen["sameLine"].as_bool().unwrap_or(false)
            && seen["nameWidth"].as_f64().unwrap_or(0.0) > seen["width"].as_f64().unwrap_or(0.0),
        "{surface:?}: verb and name must share a line with the name leading it — saw {seen}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_an_attachment_says_why_it_is_there() {
    let _serial = serial();
    let fx = fixture_edited_attachment("scenario-attkind-classic", 8);
    let page = open(Surface::Classic, &fx, 0);
    scenario_an_attachment_says_why_it_is_there(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_an_attachment_says_why_it_is_there() {
    let _serial = serial();
    let fx = fixture_edited_attachment("scenario-attkind-app", 8);
    let page = open(Surface::AppShell, &fx, 2925);
    scenario_an_attachment_says_why_it_is_there(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a converge must not outrank a hand on the wheel (#165) ─────────────────────────

/// A session that ends the way the owner's did when they reported this: the agent is working and
/// several prompts are QUEUED behind it. Queued markers are the one thing in the stream that is
/// not append-only — measured with `--dump - --json` on a two-prompt fixture, a pickup rewrites
/// the tail in place rather than extending it:
///
/// ```text
/// before: [user, assistant, queue"second", queue"third"]
/// after:  [user, assistant, queue"third",  user "second"]
/// ```
///
/// So every pickup changes the identity AND the height of every record still queued behind it,
/// once per poll, in exactly the region the reader is trying to scroll away from.
fn fixture_queued_tail(name: &str, turns: u32) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(turns, Shape::default());
    jsonl += &user_at("kick off the long one", &now_minus(300));
    jsonl += &assistant_at("starting on it now", &now_minus(295));
    for (i, text) in QUEUED.iter().enumerate() {
        jsonl += &queued_at(text, &now_minus(290 - (i as u64 * 5)));
    }
    let path = stores.claude_session(SID, &jsonl);
    Fixture { base, path, turns }
}

const QUEUED: [&str; 4] = [
    "queued one: check the formatter",
    "queued two: then the linter",
    "queued three: and the docs",
    "queued four: finally the release notes",
];

/// #165, the owner on v1.234.0: "trying to scroll down the page at the bottom is not smooth. I
/// see page flickering and only after a few trials it would eventually allow me to scroll. Feels
/// like it scrolls up and gets pulled down immediately." Then the clue that made it
/// reproducible: "often happens when there are queued messages."
///
/// NAMED FROM A TRACE, not from reading — the first reproduction attempt PASSED, because a fast
/// gesture (six notches in 540ms) finished between two polls and no apply ever landed inside it.
/// Instrumenting the engine's own decision points showed the real shape, once per poll for the
/// whole five seconds of a slow gesture:
///
/// ```text
/// t=2993 scroll   following=true owns=true gap=21  top=7116  verdict "none"
/// t=3066 converge following=true owns=true gap=103 top=7116  from "apply"
/// t=3067 scroll   following=true owns=true gap=0   top=7219  ← slammed back
/// ```
///
/// The reader's gap NEVER passed 28px, so it never reached the 80px the hysteresis needs to
/// unpin: every apply reset the accumulation before the next notch could add to it. That is the
/// whole bug — `classifyScroll`'s verdicts were correct throughout ("none", `user: true`).
///
/// So the hand here is a SLOW one, 7px every 200ms: well inside the 320ms intent window, so the
/// reader owns the position continuously, and slow enough that clearing the slack takes longer
/// than one poll on either surface (shell 1000ms, classic 2000ms). That guarantees an apply
/// lands mid-gesture, which is the condition the fast version only hit by luck.
fn scenario_a_converge_yields_to_the_reader(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    // The agent takes the queued prompts one at a time — each pickup rewrites what is left.
    let script: Vec<String> = QUEUED
        .iter()
        .enumerate()
        .flat_map(|(i, text)| {
            let at = now_minus(120 - (i as u64 * 10));
            [user_at(text, &at), assistant_at("on it", &at)]
        })
        .collect();
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(1000));
    let scroller = surface.scroller();
    let target = match surface {
        Surface::Classic => "window",
        Surface::AppShell => scroller,
    };
    for _ in 0..25 {
        eval(
            tab,
            &format!(
                "(function(){{ var s = {scroller}; {target}.dispatchEvent(new WheelEvent('wheel', {{deltaY: -7, bubbles: true}})); s.scrollTo({{ top: Math.max(0, s.scrollTop - 7), behavior: 'instant' }}); return 'ok'; }})()"
            ),
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    growth.finish(Duration::from_secs(30));
    let gap = eval(
        tab,
        &format!("(function(){{ var s = {scroller}; return s.scrollHeight - s.clientHeight - s.scrollTop; }})()"),
    )
    .as_f64()
    .unwrap_or(0.0);
    // 175px asked for, 25 notches of 7. Measured on the fix: 222 on the shell, 572 on the classic
    // page (which grows more between polls); measured on the code before it: 0 and 7. The band is
    // wide because the two surfaces legitimately differ, and because what is being asserted is
    // "the reader got away", not a pixel.
    assert!(
        gap > 100.0,
        "{surface:?}: a converge must not outrank a hand on the wheel — the reader asked for 175px \
         and ended {gap}px from the tail (before the fix: 0 on the shell, 7 on the classic page)"
    );
    assert!(
        !at_tail(tab, surface),
        "{surface:?}: and the position they scrolled to is theirs — gap {gap}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_converge_yields_to_the_reader() {
    let _serial = serial();
    let fx = fixture_queued_tail("scenario-qtail-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_converge_yields_to_the_reader(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_converge_yields_to_the_reader() {
    let _serial = serial();
    let fx = fixture_queued_tail("scenario-qtail-app", 40);
    let page = open(Surface::AppShell, &fx, 2926);
    scenario_a_converge_yields_to_the_reader(&page.tab, Surface::AppShell, &fx);
}

// ── scenarios: the three risks the #140 step-4 port had no case for ──────────────────────────
//
// Written BEFORE the port, against the page as it stood, because that is what a regression
// guard is: each one passes on the old code and must still pass on the new. They are here
// because the swap replaces the reference page's most sensitive ~200 lines — its follow state,
// its anchor, its converge and its correction sites — with engine wiring, and each of these
// three is a place where the page's own mechanism and the engine's could plausibly disagree.

/// The element the reader can see, grown by `px` from ABOVE — a growth that fires no scroll
/// event and moves everything below it by its own height. The rule it exercises is rule 5's
/// other half: what the anchor is FOR.
fn grow_above(tab: &headless_chrome::Tab, surface: Surface, px: i64) -> bool {
    let js = match surface {
        Surface::Classic => format!(
            "(function(){{ var es = [...document.querySelectorAll('#vwin > [data-idx]')]; var e = es.reverse().find(function (x) {{ return x.getBoundingClientRect().bottom <= 0; }}); if (!e) return false; e.style.paddingTop = ((parseFloat(e.style.paddingTop) || 0) + {px}) + 'px'; return true; }})()"
        ),
        Surface::AppShell => format!(
            "(function(){{ var s = document.querySelector('.transcript').getBoundingClientRect(); var es = [...document.querySelectorAll('.virtual-window > [data-unit-index]')]; var e = es.reverse().find(function (x) {{ return x.getBoundingClientRect().bottom <= s.top; }}); if (!e) return false; e.style.paddingTop = ((parseFloat(e.style.paddingTop) || 0) + {px}) + 'px'; return true; }})()"
        ),
    };
    eval(tab, &js).as_bool().unwrap_or(false)
}

/// R5. A jump lands a turn at the top of the view, and something ABOVE it then grows — an image
/// decoding, a font arriving, an estimated height replaced by a real one. The reader must still
/// be looking at the turn they asked for.
///
/// The case exists because after the port TWO mechanisms write that offset. The classic page
/// holds a just-landed target for two seconds on a 16ms timer (`holdLanding`, #94), correcting
/// against the target's own rect; the engine holds the READER's anchor, corrected inside the
/// height observer's delivery. They agree here — the anchor after a landing IS the target — but
/// "they agree" was an argument, and an argument about the reference page's landing behaviour
/// is worth a measurement.
fn scenario_a_landing_holds_through_a_growth_above_it(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let target = fx.turns / 2;
    assert!(
        harness::jump_to_turn(tab, surface, target),
        "the pane lists turn {target}"
    );
    settle();
    settle();
    let landed = turn_at_top(tab, surface);
    assert!(
        (landed - target as i64).abs() <= 1,
        "{surface:?}: the jump landed on turn {target}: top {landed}"
    );
    // Inside the hold window on the classic page — the two mechanisms are both live here.
    assert!(
        grow_above(tab, surface, 400),
        "{surface:?}: a mounted record sits above the viewport after a deep jump"
    );
    settle();
    settle();
    let after = turn_at_top(tab, surface);
    assert_eq!(
        after, landed,
        "{surface:?}: 400px appeared above the landing and the reader stayed on turn {landed}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_landing_holds_through_a_growth_above_it() {
    let _serial = serial();
    let fx = fixture("scenario-hold-classic", 120);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_landing_holds_through_a_growth_above_it(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_landing_holds_through_a_growth_above_it() {
    let _serial = serial();
    let fx = fixture("scenario-hold-app", 120);
    let page = open(Surface::AppShell, &fx, 2927);
    scenario_a_landing_holds_through_a_growth_above_it(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a growth AROUND the run displaces the reader too ───────────────────────────────

/// Grow the chrome that sits above the mounted run, inside the scroller but outside the window
/// the engine mounts into. On the classic page that is the session header, whose meta chips wrap
/// to a second line as the live feed reports them; on the app shell the transcript's own top
/// padding. Both move every record down by the same amount and fire no scroll event.
fn grow_around(tab: &headless_chrome::Tab, surface: Surface, px: i64) -> bool {
    let js = match surface {
        Surface::Classic => format!(
            "(function(){{ var h = document.querySelector('.session-header'); if (!h) return false; h.style.paddingBottom = ((parseFloat(h.style.paddingBottom) || 0) + {px}) + 'px'; return true; }})()"
        ),
        Surface::AppShell => format!(
            "(function(){{ var i = document.querySelector('.transcript-inner'); if (!i) return false; i.style.paddingTop = ((parseFloat(getComputedStyle(i).paddingTop) || 0) + {px}) + 'px'; return true; }})()"
        ),
    };
    eval(tab, &js).as_bool().unwrap_or(false)
}

/// R6b. The engine's content observer watches the MOUNTED window, because that element does not
/// hold the pads and measuring writes the pads — an observer over an element containing them
/// feeds itself, and the browser cuts that short by dropping notifications, which loses the very
/// correction the delivery exists to make. So a growth OUTSIDE the run was heard by neither
/// observer, and the classic page had been hearing it since #89/#98 through a `ResizeObserver`
/// on `document.body`.
///
/// Two readers, because the displacement means something different to each: one pinned to the
/// tail, whose tail has just moved away; one reading, whose page has just been pushed down.
///
/// Confirmed RED on the code without the fix: with `outerObserver.observe(mount.content)` taken
/// out, both surfaces fail on the first assertion — "pinned, 320px appeared above the run and the
/// tail is still the tail". The observer watches the BORDER box on purpose: what moves the run is
/// usually the chrome's own padding, and a padding change leaves the content box untouched, so
/// the default box hears nothing at all (that is how the app-shell half of this case failed once
/// the rest was working).
fn scenario_a_growth_around_the_run_displaces_the_reader(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    assert!(
        grow_around(tab, surface, 320),
        "{surface:?}: the chrome above the run can be grown"
    );
    settle();
    settle();
    assert!(
        at_tail(tab, surface),
        "{surface:?}: pinned, 320px appeared above the run and the tail is still the tail"
    );
    // …and the same growth for a reader who is not pinned: it must move nothing they can see.
    let target = fx.turns / 2;
    assert!(
        harness::jump_to_turn(tab, surface, target),
        "the pane lists turn {target}"
    );
    settle();
    settle();
    let reading = turn_at_top(tab, surface);
    // Past the classic page's 2s landing hold, so this measures the ENGINE's anchor and not
    // `holdLanding` standing in for it.
    std::thread::sleep(Duration::from_millis(2400));
    assert!(
        grow_around(tab, surface, 320),
        "{surface:?}: …and grown again"
    );
    settle();
    settle();
    assert_eq!(
        turn_at_top(tab, surface),
        reading,
        "{surface:?}: reading turn {reading}, 320px appeared above the run, and it stayed there"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_growth_around_the_run_displaces_the_reader() {
    let _serial = serial();
    let fx = fixture("scenario-around-classic", 120);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_growth_around_the_run_displaces_the_reader(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_growth_around_the_run_displaces_the_reader() {
    let _serial = serial();
    let fx = fixture("scenario-around-app", 120);
    let page = open(Surface::AppShell, &fx, 2928);
    scenario_a_growth_around_the_run_displaces_the_reader(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a press in the gutter is not a hand on the scrollbar ───────────────────────────

/// R7. Both scrollers centre their content and leave their own background exposed on either
/// side, so a press in a gutter lands on the scroller itself — the exact test each frame used
/// for "the reader has grabbed the thumb". Taking it for a thumb grab puts the engine in drag
/// mode for as long as the button is held, which is the whole of a drag-selection: there every
/// scroll counts as the reader's, no anchor is held at all, and a converge is deferred
/// indefinitely. So a pinned view drops its pin the moment the next record lands.
///
/// The press here has no matching release, because that is the shape of the gesture that hurts.
///
/// Confirmed RED on the code without the fix: with `elementFrame.isScrollbarTarget` back to
/// `event.target === scroller`, the app-shell half fails — the press in the gutter takes the
/// engine into drag mode and the pin is gone by the time the next record lands.
fn scenario_a_press_in_the_gutter_is_not_a_thumb(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    let pressed = match surface {
        // `.layout` is `max-width: 1160px; margin: 0 auto` in a 1400px window, so x = 20 is the
        // left gutter — and what a real press finds there is `body`, not the scrolling element.
        Surface::Classic => eval(tab, "(function(){ var t = document.elementFromPoint(20, 300); if (!t) return 'none'; t.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: 20, clientY: 300, pointerId: 1, buttons: 1, isPrimary: true })); return t.tagName; })()"),
        // `.transcript-inner` is `min(880px, 100% - 76px)`, centred: 38px of gutter each side,
        // and there a press lands on `.transcript` — which IS the scroller.
        Surface::AppShell => eval(tab, "(function(){ var s = document.querySelector('.transcript'); var r = s.getBoundingClientRect(); var x = Math.round(r.left + 8), y = Math.round(r.top + 120); var t = document.elementFromPoint(x, y); if (!t) return 'none'; t.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: x, clientY: y, pointerId: 1, buttons: 1, isPrimary: true })); return t.className || t.tagName; })()"),
    };
    assert_ne!(
        pressed, "none",
        "{surface:?}: the gutter is where it should be"
    );
    // The tail keeps arriving under the held button. A thumb grab would have unpinned on the
    // first of these, and deferred every converge behind a reader who never lets go.
    let script: Vec<String> = (0..4)
        .map(|i| {
            let at = now_minus(60 - i * 10);
            format!(
                "{}{}",
                user_at("another one", &at),
                assistant_at("on it", &at)
            )
        })
        .collect();
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(1200));
    growth.finish(Duration::from_secs(30));
    settle();
    settle();
    assert!(
        at_tail(tab, surface),
        "{surface:?}: a press in the gutter is not a hand on the thumb — the pin survives it"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_press_in_the_gutter_is_not_a_thumb() {
    let _serial = serial();
    let fx = fixture("scenario-gutter-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_press_in_the_gutter_is_not_a_thumb(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_press_in_the_gutter_is_not_a_thumb() {
    let _serial = serial();
    let fx = fixture("scenario-gutter-app", 40);
    let page = open(Surface::AppShell, &fx, 2929);
    scenario_a_press_in_the_gutter_is_not_a_thumb(&page.tab, Surface::AppShell, &fx);
}

/// A fixture with TWO code panes, so a per-block control has something to leave alone.
fn fixture_two_code_blocks(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(20, Shape::default());
    transcript += &user_at("question 20: write both files", &now_minus(90));
    transcript += &write_tool_at("cw1", "/first.py", 10, &now_minus(88));
    transcript += &assistant_at("answer 20: first written", &now_minus(86));
    transcript += &write_tool_at("cw2", "/second.py", 10, &now_minus(84));
    transcript += &assistant_at("answer 21: second written", &now_minus(82));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 22,
    }
}

/// #173. The code size control sits ON a pane, so it must act on THAT pane. It did not: on both
/// pages the bar drove the page-wide preference — global chrome wearing per-block clothes — and
/// on the app shell it did not even reach the pane it sat on (`.codebox .lines` was matched by
/// neither reading rule, and its `wrap` class was a literal in the template). So this case fails
/// on BOTH surfaces before the fix, for OPPOSITE reasons: the classic page moves every pane, the
/// shell moves none. That is the whole of the bug in one assertion.
fn scenario_a_code_control_moves_its_own_pane_only(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    match surface {
        Surface::Classic => {
            eval(tab, "(function(){ document.querySelectorAll('#stream .fold').forEach(function (f) { if (f.dataset.open === '0') { var h = f.querySelector('.fold-h'); if (h) h.click(); } }); return 'opened'; })()");
        }
        Surface::AppShell => {
            for _ in 0..2 {
                eval(tab, "(function(){ document.querySelectorAll('.renderer.closed > button.renderer-head').forEach(function (h) { h.click(); }); document.querySelectorAll('.cap-more-btn').forEach(function (b) { b.click(); }); return 'opened'; })()");
                settle();
            }
        }
    }
    settle();
    // Both pages mark a code pane the same way since #173 — the marker is the contract, so the
    // probe needs no per-surface selector for it. `.codebar` is the pane's own bar on both.
    // Panes are found by each page's OWN long-standing selector, never by `[data-code]`. The
    // marker is what this task ADDS, so probing for it would make the case fail on the old code
    // merely because the attribute is absent — a red in the wrong place, proving the marker is
    // new rather than that the control was broken. These selectors exist on both sides, so the
    // press really happens on the old code and the two behavioural assertions below are what
    // fails there.
    let pane_sel = match surface {
        Surface::Classic => ".numbered, .diff",
        Surface::AppShell => ".codebox .lines",
    };
    // Always reports what it SAW — a bare unwrap hides whether the pane is unmounted, the fold
    // never opened, or the bar is missing, which are three different bugs.
    let probe_js = &format!(
        "(function(){{ \
         var panes = [].slice.call(document.querySelectorAll('{pane_sel}')); \
         var sizeOf = function (p) {{ \
           var cell = p.querySelector('.code, .codecell') || p; \
           return Math.round(parseFloat(getComputedStyle(cell).fontSize) * 100) / 100; }}; \
         return {{ n: panes.length, \
                  marked: document.querySelectorAll('[data-code]').length, \
                  bars: document.querySelectorAll('.codebar').length, \
                  a: panes.length > 0 ? sizeOf(panes[0]) : -1, \
                  b: panes.length > 1 ? sizeOf(panes[1]) : -1 }}; }})()"
    );
    let before = harness::probe(tab, probe_js);
    assert!(
        before["n"].as_i64().unwrap_or(0) >= 2,
        "{surface:?}: the fixture must mount two marked code panes, saw: {before}"
    );
    // Press A− on the FIRST pane's own bar.
    let pressed = eval(
        tab,
        &format!(
            "(function(){{ \
         var pane = document.querySelector('{pane_sel}'); if (!pane) return 'no pane'; \
         var box = pane.closest('.codewrap, .codebox') || pane.parentElement; \
         var btn = box.querySelector('.ms-dn, [data-code-size=\"-1\"]'); \
         if (!btn) return 'no smaller button'; btn.click(); return 'pressed'; }})()"
        ),
    );
    assert_eq!(
        pressed.as_str().unwrap_or(""),
        "pressed",
        "{surface:?}: the first pane carries its own size control: {pressed}"
    );
    settle();
    let after = harness::probe(tab, probe_js);
    let (a0, b0) = (
        before["a"].as_f64().unwrap_or(-1.0),
        before["b"].as_f64().unwrap_or(-1.0),
    );
    let (a1, b1) = (
        after["a"].as_f64().unwrap_or(-1.0),
        after["b"].as_f64().unwrap_or(-1.0),
    );
    assert!(
        a0 > 0.0 && b0 > 0.0 && a1 > 0.0 && b1 > 0.0,
        "{surface:?}: every pane reports a real font size; before {before}, after {after}"
    );
    // The pane whose button was pressed gets smaller. Before #173 the app shell failed HERE:
    // `--code-size` reached three enumerated selectors and `.lines` was in none of them, so the
    // pane the reader was looking at ignored its own control.
    assert!(
        a1 < a0,
        "{surface:?}: the pane whose control was pressed got smaller ({a0} -> {a1}); before: \
         {before}, after: {after}"
    );
    // …and NO OTHER pane moves. Before #173 the classic page failed HERE: the bar wrote the
    // page-wide preference, so pressing it on one pane resized every pane on the page.
    assert!(
        (b1 - b0).abs() < 0.01,
        "{surface:?}: the other pane is untouched ({b0} -> {b1}) — a control on a pane is not a \
         page-wide control; before: {before}, after: {after}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_code_control_moves_its_own_pane_only() {
    let _serial = serial();
    let fx = fixture_two_code_blocks("scenario-codectl-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_code_control_moves_its_own_pane_only(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_code_control_moves_its_own_pane_only() {
    let _serial = serial();
    let fx = fixture_two_code_blocks("scenario-codectl-app");
    let page = open(Surface::AppShell, &fx, 2930);
    scenario_a_code_control_moves_its_own_pane_only(&page.tab, Surface::AppShell, &fx);
}

/// A fixture whose tail holds ONE record with three records nested inside it: a thinking that
/// absorbs three tool calls, which the engine folds into a single `act` carrying a `blocks` part
/// (measured: `act -> blocks with 3 items`). That is the only shape where a mounted item on the
/// classic page contains more than one record.
fn fixture_nested_records(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(18, Shape::default());
    transcript += &user_at("question 18: work through it", &now_minus(120));
    transcript += &thinking_at("deliberation: three steps to run", &now_minus(118));
    for i in 1..=3 {
        transcript += &tool_open_at(&format!("nt{i}"), &now_minus(116 - i * 2));
        transcript += &tool_result_lines(&format!("nt{i}"), 40, &now_minus(115 - i * 2));
    }
    transcript += &assistant_at("answer 18: all three done", &now_minus(100));
    // MORE TURNS AFTER IT. The nested record must sit MID-document: parked at the tail the page
    // is still FOLLOWING, `readerAnchor()` returns null by design, and the engine simply
    // converges to the end — so the anchor path this case exists to test never runs, and the
    // reference sits still for the wrong reason (measured: scrollY 4495 of docH 5252).
    for i in 19..27 {
        transcript += &user_at(
            &format!("question {i}: keep going with more prose to push the tail well clear"),
            &now_minus(96 - (i as u64 - 19) * 4),
        );
        transcript += &assistant_at(
            &format!(
                "answer {i}: {}",
                "more text to make the tail tall. ".repeat(8)
            ),
            &now_minus(94 - (i as u64 - 19) * 4),
        );
    }
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 27,
    }
}

/// #176. The engine's anchor has TWO levels — the mounted item, then the first
/// `[data-block-index]` inside it — and it holds the reader by that inner element's screen
/// position. A mounted item is one record on the classic page and a whole unit on the app shell,
/// but EITHER can contain records nested inside it. The app shell indexes those; the classic page
/// did not, so its anchor could only hold the enclosing fold — whose own top never moves when a
/// child inside it grows. Park the reader below one child, expand a child ABOVE them, and the
/// reader should not travel.
fn scenario_a_growth_in_a_nested_record_holds_the_reader(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // LEAVE FOLLOW WITH THE READER'S OWN SCROLL, and SEARCH for the record rather than guessing
    // a distance. `scrollIntoView` cannot leave follow at all: a programmatic scroll carries no
    // intent, so the page reads it as displacement and HEALS a following view back to the tail
    // (harness `scroll_by` says so in as many words; this case measured it). And a fixed scroll
    // distance marked the wrong record — `long_session` emits activity records of its own, each
    // absorbing exactly ONE tool, while the one this fixture appends absorbs three. So the
    // target is identified BY CONTENT: the only activity record with three nested children.
    // Identify the target by its RECORD ID, not by an attribute stamped on the element. The app
    // shell RE-RENDERS a unit from state when a fold is toggled, which discards any marker put on
    // the DOM — measured: `n: 0` after opening. The classic page only flips `dataset.open`, so a
    // marker survives there; an id-based scope survives on both.
    let target_js = match surface {
        Surface::Classic => "(function(){ var all = document.querySelectorAll('#stream .fold[data-kind=\"act\"]');              for (var i = 0; i < all.length; i++) { var f = all[i];                if (f.querySelectorAll('.fold-b .blk').length >= 3) {                  if (f.dataset.open === '0') { var h = f.querySelector(':scope > .fold-h'); if (h) h.click(); }                  return f.id || 'none'; } } return 'none'; })()",
        Surface::AppShell => "(function(){ var all = document.querySelectorAll('.renderer[data-renderer-kind=\"activity\"]');              for (var i = 0; i < all.length; i++) { var r = all[i];                if (r.querySelectorAll('.renderer-children > .renderer-turn').length >= 3) {                  var h = r.querySelector(':scope > button.renderer-head');                  if (r.classList.contains('closed') && h) h.click();                  return r.dataset.recordId || 'none'; } } return 'none'; })()",
    };
    let mut found = eval(tab, target_js);
    for _ in 0..14 {
        if found.as_str().map(|v| v != "none").unwrap_or(false) {
            break;
        }
        scroll_by(tab, surface, -900);
        settle();
        found = eval(tab, target_js);
    }
    // The eval that FINDS the record also OPENS it, and on the app shell an open re-renders the
    // unit from state and re-anchors it. Reading the DOM in the same breath caught it mid-flight
    // — measured `n: 0` once in five runs, which reads exactly like the record not existing.
    settle();
    let target_id = found.as_str().unwrap_or("none").to_string();
    assert_ne!(
        target_id, "none",
        "{surface:?}: scrolled back to the fixture's own activity record — the one with three \
         nested children, not `long_session`'s single-tool ones"
    );
    let scope = match surface {
        Surface::Classic => format!("#{target_id}"),
        Surface::AppShell => format!("[data-record-id=\"{target_id}\"]"),
    };
    // Nested children by each page's OWN long-standing structure — NOT by `[data-block-index]`,
    // which is what this task adds and would make the red land on a missing attribute rather
    // than on the behaviour (#173's lesson).
    let kids = &match surface {
        Surface::Classic => format!("{scope} .fold-b .blk"),
        Surface::AppShell => format!("{scope} .renderer-children > .renderer-turn"),
    };
    let head_of = match surface {
        Surface::Classic => ":scope > .fold-h",
        Surface::AppShell => ":scope > .renderer > button.renderer-head",
    };
    // A STABLE handle for the reference child. Re-querying `k[k.length - 1]` after the growth
    // was the first version and it was wrong: opening a child changes what the node list holds,
    // so "the last kid" before and after could be two different elements and the measurement
    // compared unrelated rects. Both pages already carry a record identity — use it.
    let id_of = match surface {
        Surface::Classic => "e.id",
        Surface::AppShell => "(e.querySelector('[data-record-id]') || {}).dataset?.recordId",
    };
    let seen = harness::probe(
        tab,
        &format!("(function(){{ var k = document.querySelectorAll('{kids}'); var laid = 0; \
             for (var i = 0; i < k.length; i++) if (k[i].getBoundingClientRect().height > 0) laid++; \
             return {{ n: k.length, laid_out: laid }}; }})()"),
    );
    assert!(
        seen["n"].as_i64().unwrap_or(0) >= 3 && seen["laid_out"].as_i64().unwrap_or(0) >= 3,
        "{surface:?}: three nested records mounted AND laid out inside one item — a hidden one \
         has no geometry and would make every measurement below vacuous, saw: {seen}"
    );
    // Put the LAST child at the top of the viewport, so the two above it are off-screen upward
    // and the reader is genuinely parked below them.
    // Open every child EXCEPT the first, so they are tall. Two reasons: the reader needs room to
    // sit below the growth, and — the point of the case — THE ANCHOR IS THE FIRST VISIBLE ROW.
    // With all three children on screen the anchor is the one that grows, whose own top does not
    // move, so the engine correctly holds it and the case measures nothing. The growth has to be
    // OFF-SCREEN ABOVE, with the first visible row BELOW it. (Measured the wrong way round first:
    // the reference moved 299px both WITH and WITHOUT the fix, because it was never the anchor.)
    eval(
        tab,
        &format!(
            "(function(){{ var k = document.querySelectorAll('{kids}'); \
             for (var i = 1; i < k.length; i++) {{ var h = k[i].querySelector('{head_of}'); if (h) h.click(); }} \
             return 'opened rest'; }})()"
        ),
    );
    settle();
    settle();
    // Opening the record grew it and pushed its children off the top. Bring the reference back
    // into view with the READER'S OWN scroll (a programmatic one would be classified as
    // displacement), bounded so a page that will not converge fails instead of looping.
    for _ in 0..10 {
        let where_now = harness::probe(
            tab,
            &format!(
                "(function(){{ var k = document.querySelectorAll('{kids}'); \
                 if (k.length < 2) return {{ t: 0, firstBottom: 1 }}; \
                 return {{ t: Math.round(k[1].getBoundingClientRect().top), \
                           secondBottom: Math.round(k[1].getBoundingClientRect().bottom), \
                           firstBottom: Math.round(k[0].getBoundingClientRect().bottom) }}; }})()"
            ),
        );
        let t = where_now["t"].as_f64().unwrap_or(0.0);
        let first_bottom = where_now["firstBottom"].as_f64().unwrap_or(1.0);
        let second_bottom = where_now["secondBottom"].as_f64().unwrap_or(0.0);
        // The growing child ENTIRELY above the viewport, and the next one still VISIBLE — which
        // means STRADDLING the top edge, not sitting fully inside it. They are adjacent siblings,
        // so `k0.bottom < 0` and `k1.top > 0` cannot both hold: their edges nearly coincide.
        // `firstVisible` takes the first row whose BOTTOM is past the viewport top, so a
        // straddling row is exactly what the engine anchors to.
        if first_bottom < 0.0 && second_bottom > 40.0 {
            break;
        }
        // Sign matters and it is the opposite of the instinct: a NEGATIVE dy scrolls up, which
        // moves content DOWN and RAISES an element's `top`. Getting it backwards walked the
        // reader to the tail, re-acquired the pin, and froze the measurement at a constant.
        // Proportional, not a fixed stride: a 240px step overshoots a band only tens of pixels
        // wide and the loop ends ten iterations later still outside it.
        // Target a SLIGHTLY NEGATIVE top: the anchor row straddles the edge and the child above
        // it clears the viewport. Steering at +120 was self-defeating — it guarantees the growing
        // child stays visible (measured: firstBottom 118), which is the very thing the break
        // condition forbids, so the loop could never converge on its own target.
        // dy = t - target, because scrolling by dy changes an element's top by -dy.
        let want = (t + 20.0) as i64;
        scroll_by(tab, surface, want.clamp(-900, 900));
        settle();
    }
    // No parking gesture beyond that: the reader is where their own scroll left them. All this
    // needs is the reference child ON SCREEN and BELOW the one about to grow — asserted, not
    // assumed.
    let parked = harness::probe(
        tab,
        &format!(
            "(function(){{ var k = document.querySelectorAll('{kids}'); var e = k[1]; \
             var r = e.getBoundingClientRect(); var f = k[0].getBoundingClientRect(); \
             return {{ id: {id_of}, top: Math.round(r.top), firstBottom: Math.round(f.bottom), \
                       onScreen: r.bottom > 40 && r.top < innerHeight, below: f.bottom <= 0 }}; }})()"
        ),
    );
    assert_eq!(
        parked["onScreen"].as_bool(),
        Some(true),
        "{surface:?}: the reference child is on screen where the reader can see it move: {parked}"
    );
    assert_eq!(
        parked["below"].as_bool(),
        Some(true),
        "{surface:?}: the child that will grow sits ENTIRELY ABOVE the viewport, so the anchor is \
         a row BELOW it: {parked}"
    );
    // Centring must not have walked the reader back onto the tail — that would restore the
    // follow converge and hold the reference for a reason unrelated to the anchor.
    assert!(
        !at_tail(tab, surface),
        "{surface:?}: still off the tail after centring, so the ANCHOR is what holds the reader"
    );
    let ref_id = parked["id"].as_str().unwrap_or("").to_string();
    assert!(
        !ref_id.is_empty(),
        "{surface:?}: the reference child has a stable identity to measure by: {parked}"
    );
    settle();
    settle();
    // The reader must NOT be following. Following returns a null anchor by design and converges
    // to the end on every growth, which holds the reference for a reason that has nothing to do
    // with the anchor under test — the first version of this case measured exactly that.
    assert!(
        !at_tail(tab, surface),
        "{surface:?}: parked away from the tail, so the ANCHOR is what holds the reader and not \
         the follow converge"
    );
    let where_js = format!(
        "(function(){{ var k = document.querySelectorAll('{kids}'); var hit = null; \
         for (var i = 0; i < k.length; i++) {{ var e = k[i]; if (({id_of}) === '{ref_id}') hit = e; }} \
         if (!hit) return {{ err: 'reference child gone' }}; \
         return {{ n: k.length, top: Math.round(hit.getBoundingClientRect().top) }}; }})()"
    );
    let before = harness::probe(tab, &where_js);
    // Grow the FIRST child, above the reader: open its fold. A real gesture, and a real growth.
    let grew = harness::probe(
        tab,
        &format!(
            "(function(){{ var k = document.querySelectorAll('{kids}'); var first = k[0]; \
             var h = first.querySelector('{head_of}'); if (!h) return {{ err: 'no head' }}; \
             var before = first.getBoundingClientRect().height; h.click(); \
             return {{ before: Math.round(before) }}; }})()"
        ),
    );
    assert!(
        grew.get("err").is_none(),
        "{surface:?}: the first nested child has a head to open: {grew}"
    );
    settle();
    settle();
    let after_growth = harness::probe(
        tab,
        &format!(
            "(function(){{ var k = document.querySelectorAll('{kids}'); var first = k[0]; \
             return {{ h: Math.round(first.getBoundingClientRect().height) }}; }})()"
        ),
    );
    assert!(
        after_growth["h"].as_f64().unwrap_or(0.0) > grew["before"].as_f64().unwrap_or(0.0) + 8.0,
        "{surface:?}: opening the first child actually grew it ({grew} -> {after_growth}) — with \
         no growth there is nothing for the anchor to absorb and the case proves nothing"
    );
    let after = harness::probe(tab, &where_js);
    let (t0, t1) = (
        before["top"].as_f64().unwrap_or(f64::NAN),
        after["top"].as_f64().unwrap_or(f64::NAN),
    );
    assert!(
        before.get("err").is_none() && after.get("err").is_none(),
        "{surface:?}: the reference child survives the growth; before {before}, after {after}"
    );
    assert!(
        (t1 - t0).abs() <= 4.0,
        "{surface:?}: the reader stays where they were reading when a record NESTED above them \
         grows ({t0} -> {t1}) — the enclosing item's own top never moves, so an anchor that can \
         only address the item holds nothing"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_growth_in_a_nested_record_holds_the_reader() {
    let _serial = serial();
    let fx = fixture_nested_records("scenario-nested-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_growth_in_a_nested_record_holds_the_reader(&page.tab, Surface::Classic, &fx);
}

/// KNOWN RED (#177): the app shell displaces the reader by 354px here, the same signature the
/// classic page showed before #176. Emitting `data-block-index` on nested children is NOT
/// sufficient — this case found that on the surface assumed to be correct. The gate skips
/// `known_red`; the fix removes the marker, and the case is never weakened to make it pass.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_growth_in_a_nested_record_holds_the_reader() {
    let _serial = serial();
    let fx = fixture_nested_records("scenario-nested-app");
    let page = open(Surface::AppShell, &fx, 2931);
    scenario_a_growth_in_a_nested_record_holds_the_reader(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a growth in the HEAD of a record the reader can SEE holds the head (#178) ─────

/// #178, the STRADDLE GUARD. `captureDomAnchor` refines the anchor twice — the mounted item, then
/// the first `[data-block-index]` row inside it — and `firstVisible` returns the first row in
/// DOCUMENT ORDER whose bottom is past the viewport top, which is always the OUTERMOST one: a
/// parent qualifies whenever a child does. #177 taught the refinement to DESCEND past a mere
/// wrapper far above the reader; the descend is guarded by `row.top < viewportTop`, and THAT is
/// what this case covers.
///
/// The guard protects a case that already worked: when the pick's own top is INSIDE the viewport
/// the reader can SEE it, so it is the better anchor and must stay the anchor. Park the reader ON
/// a record — its own top a little below the viewport top, its open nested children below it —
/// grow the record's HEAD (the content above those children), and the head must not move.
/// Descending unconditionally would re-anchor to a child BELOW the head, and the same growth
/// would then scroll the head the reader is reading off the top of the screen.
///
/// Two things this case had to be built around, both read off the code rather than guessed:
///
/// · WHERE THE READER CAN PARK IS BOUNDED. `firstVisible` takes the first element whose BOTTOM is
///   past the viewport top — the STRADDLER. So a record is only the pick while its own top is
///   within ONE INTER-RECORD GAP of the edge; park further down and the record ABOVE it is the
///   pick, a growth in this one moves nothing the engine holds, and the case would pass while
///   measuring nothing. That gap is ~10px on the classic page (`.uturn { margin: 0 0 10px }`) and
///   the whole `.process-surface` headbar on the app shell, so the target is SEARCHED for, from
///   50px down, and the precondition asserts the pick really did land inside this record.
///
/// · NO LIVE DELTA GROWS A HEAD, on either page. The classic page pushes a thinking's `blocks`
///   part BEFORE its `think` text (`html_export/mod.rs`), so the only thing above the first nested
///   record is the one-line `.fold-h`; the app shell puts a record's output above its children,
///   but that output is uncapped markdown (the `[data-cap-more]` expanders exist only on
///   `pre`/`num`/`diff` parts, which a thinking never carries), and an absorbed tool appends a
///   child BELOW. So the head is grown the way a late reflow grows one — a header gaining a line,
///   a font arriving, an image decoding, all named in the engine's own comments — by writing its
///   height. That fires the identical `ResizeObserver -> measureNow -> measureMounted(this.anchor)
///   -> restoreDomAnchor` path a live delta's growth takes, against the anchor KEPT from the last
///   settle — which is the point: an anchor captured after the growth describes the moved view and
///   corrects nothing.
fn scenario_a_growth_in_a_visible_records_head_holds_it(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    const GROW: f64 = 420.0;
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // The fixture's OWN activity record — the one absorbing three tools — found BY CONTENT and
    // opened, exactly as the #177 scenario finds it: `long_session` emits activity records that
    // absorb one tool each, so a guessed scroll distance marks the wrong one, and a marker
    // stamped on the DOM does not survive the app shell's re-render.
    let target_js = match surface {
        Surface::Classic => "(function(){ var all = document.querySelectorAll('#stream .fold[data-kind=\"act\"]');              for (var i = 0; i < all.length; i++) { var f = all[i];                if (f.querySelectorAll('.fold-b .blk').length >= 3) {                  if (f.dataset.open === '0') { var h = f.querySelector(':scope > .fold-h'); if (h) h.click(); }                  return f.id || 'none'; } } return 'none'; })()",
        Surface::AppShell => "(function(){ var all = document.querySelectorAll('.renderer[data-renderer-kind=\"activity\"]');              for (var i = 0; i < all.length; i++) { var r = all[i];                if (r.querySelectorAll('.renderer-children > .renderer-turn').length >= 3) {                  var h = r.querySelector(':scope > button.renderer-head');                  if (r.classList.contains('closed') && h) h.click();                  return r.dataset.recordId || 'none'; } } return 'none'; })()",
    };
    let mut found = eval(tab, target_js);
    for _ in 0..14 {
        if found.as_str().map(|v| v != "none").unwrap_or(false) {
            break;
        }
        scroll_by(tab, surface, -900);
        settle();
        found = eval(tab, target_js);
    }
    // The eval that FINDS the record also OPENS it, and on the app shell an open re-renders the
    // unit from state and re-anchors it; reading the DOM in the same breath caught it mid-flight.
    settle();
    let target_id = found.as_str().unwrap_or("none").to_string();
    assert_ne!(
        target_id, "none",
        "{surface:?}: scrolled back to the fixture's own activity record — the one with three \
         nested children, not `long_session`'s single-tool ones"
    );
    // Every probe below re-finds the record BY ID. The app shell rebuilds a unit from state on a
    // fold toggle, so an element captured before one names a node that has left the document.
    let rec_js = match surface {
        Surface::Classic => format!("document.getElementById('{target_id}')"),
        Surface::AppShell => format!(
            "(function(){{ var n = document.querySelector('[data-record-id=\"{target_id}\"]'); return n ? n.closest('[data-block-index]') : null; }})()"
        ),
    };
    // Each page's own long-standing structure, NOT `[data-block-index]`: selecting on the very
    // attribute the engine anchors by would land a failure on a missing attribute rather than on
    // the behaviour (#173's lesson).
    let (head_sel, kids_sel, kid_head_sel) = match surface {
        Surface::Classic => (
            ":scope > .fold-h",
            ":scope .fold-b .blk",
            ":scope > .fold-h",
        ),
        Surface::AppShell => (
            ":scope > .renderer > .renderer-head",
            ":scope > .renderer > .renderer-body > .renderer-children > .renderer-turn",
            ":scope > .renderer > button.renderer-head",
        ),
    };
    // The engine's own viewport: the document's client box on the classic page
    // (`documentFrame().viewportTop()` is 0), the `.transcript` scroller's rect on the app shell.
    let (vt_js, vh_js, mount_js) = match surface {
        Surface::Classic => (
            "0",
            "document.scrollingElement.clientHeight",
            "document.getElementById('vwin')",
        ),
        Surface::AppShell => (
            "document.querySelector('.transcript').getBoundingClientRect().top",
            "document.querySelector('.transcript').clientHeight",
            "document.querySelector('.virtual-window')",
        ),
    };
    // ONE probe for everything the case decides on, so the preconditions and the measurement are
    // read off the same layout. `itemHoldsRecord` / `rowInsideRecord` replicate the engine's two
    // PICKS — the mounted item, then the first laid-out `[data-block-index]` row inside it — and
    // deliberately NOT the descend loop: replicating the thing under test would make the
    // precondition tautological with the assertion.
    let geom_js = format!(
        "(function(){{ var vt = {vt_js}; var R = {rec_js}; var mount = {mount_js}; \
         if (!R || !mount) return {{ err: 'record or mount gone' }}; \
         var head = R.querySelector('{head_sel}'); if (!head) return {{ err: 'no head' }}; \
         var kids = R.querySelectorAll('{kids_sel}'); var item = null; \
         for (var i = 0; i < mount.children.length; i++) {{ var c = mount.children[i]; \
             if (c.getBoundingClientRect().bottom > vt + 1) {{ item = c; break; }} }} \
         var row = null; \
         if (item) {{ var rows = item.querySelectorAll('[data-block-index]'); \
             for (var j = 0; j < rows.length; j++) {{ var rr = rows[j].getBoundingClientRect(); \
                 if (rr.height > 0 && rr.bottom > vt + 1) {{ row = rows[j]; break; }} }} }} \
         var hr = head.getBoundingClientRect(); var rb = R.getBoundingClientRect(); \
         var k0 = kids.length ? kids[0].getBoundingClientRect() : null; var laid = 0; \
         for (var m = 0; m < kids.length; m++) if (kids[m].getBoundingClientRect().height > 0) laid++; \
         return {{ headTop: Math.round(hr.top - vt), headH: Math.round(hr.height), \
                   headBottom: Math.round(hr.bottom - vt), recTop: Math.round(rb.top - vt), \
                   kids: kids.length, laid: laid, kidTop: k0 ? Math.round(k0.top - vt) : null, \
                   kidH: k0 ? Math.round(k0.height) : null, \
                   itemHoldsRecord: !!item && item.contains(R), \
                   rowInsideRecord: !!row && (row === R || R.contains(row)), \
                   rowIsRecord: !!row && row === R, \
                   rowTop: row ? Math.round(row.getBoundingClientRect().top - vt) : null, \
                   vh: Math.round({vh_js}) }}; }})()"
    );
    let seen = harness::probe(tab, &geom_js);
    assert!(
        seen["kids"].as_i64().unwrap_or(0) >= 3 && seen["laid"].as_i64().unwrap_or(0) >= 3,
        "{surface:?}: three nested records mounted AND laid out inside the record — a hidden one \
         has no geometry and would make every measurement below vacuous, saw: {seen}"
    );
    // Open every nested child, so there is a real, tall, OPEN child below the head — the shape the
    // guard exists for. The growth is in the HEAD, never in a child: a growth inside the child
    // that is itself the anchor does not move that child's own top, and the engine holds it
    // correctly with or without the guard (#176's sixth silent pass, which nearly cancelled a
    // real bug).
    eval(
        tab,
        &format!(
            "(function(){{ var R = {rec_js}; var k = R.querySelectorAll('{kids_sel}'); \
             for (var i = 0; i < k.length; i++) {{ var h = k[i].querySelector('{kid_head_sel}'); if (h) h.click(); }} \
             return 'opened'; }})()"
        ),
    );
    settle();
    settle();
    // PARK ON THE RECORD, searching for a target rather than assuming one. The reader's own
    // scroll, never `scrollIntoView`: a programmatic scroll carries no intent, so the page reads
    // it as displacement and heals a following view straight back to the tail.
    let mut parked = harness::probe(tab, &geom_js);
    let mut achieved = -1.0f64;
    for target in [50.0f64, 34.0, 22.0, 14.0, 9.0, 6.0, 4.0] {
        // TRAVEL FIRST, JUDGE SECOND. The finder leaves the record MOUNTED, not parked — the
        // window mounts an overscan band around the viewport (1500px on the classic page), so the
        // head can start a screen and a half away, and one clamped step per target would spend the
        // top of the ladder merely travelling and judge only the small targets. So each target
        // gets its own bounded approach, and P5 is read once the head is actually AT it.
        for _ in 0..8 {
            let t = parked["headTop"].as_f64().unwrap_or(0.0);
            if (t - target).abs() <= 3.0 {
                break;
            }
            // dy = t - target, because scrolling by dy changes an element's top by -dy. The sign
            // is the opposite of the instinct: a NEGATIVE dy scrolls UP and RAISES the top.
            // Backwards, it walks the reader to the tail, re-acquires the pin and freezes the
            // measurement at a constant — two byte-identical results were the tell last time.
            scroll_by(tab, surface, ((t - target) as i64).clamp(-900, 900));
            settle();
            parked = harness::probe(tab, &geom_js);
        }
        let top = parked["headTop"].as_f64().unwrap_or(-1.0);
        if top > 2.0
            && parked["itemHoldsRecord"].as_bool() == Some(true)
            && parked["rowInsideRecord"].as_bool() == Some(true)
        {
            achieved = top;
            break;
        }
    }
    assert!(
        achieved > 2.0,
        "{surface:?}: the reader is parked ON the record — its own top INSIDE the viewport, and \
         the row the engine anchors to inside it too. `firstVisible` takes the first element whose \
         BOTTOM is past the viewport top, so a record is only that pick within one inter-record \
         gap of the edge; parked further down, the STRADDLING record above it is the pick and a \
         growth in this one moves nothing the engine holds. Last seen: {parked}"
    );
    assert!(
        achieved < parked["vh"].as_f64().unwrap_or(0.0) / 2.0,
        "{surface:?}: the head sits in the upper half of the viewport, where the reader is reading \
         and where a displacement would carry it off-screen: {parked}"
    );
    assert!(
        parked["kidTop"].as_f64().unwrap_or(0.0)
            > parked["headBottom"].as_f64().unwrap_or(f64::MAX)
            && parked["kidH"].as_f64().unwrap_or(0.0) > 0.0,
        "{surface:?}: the nested child is laid out BELOW the head — with nothing under the head \
         there is no lower row for an unguarded descend to reach, and the case proves nothing: \
         {parked}"
    );
    // Following returns a NULL anchor by design and converges to the end on every growth, which
    // would hold the head for a reason that has nothing to do with the anchor under test.
    assert!(
        !at_tail(tab, surface),
        "{surface:?}: parked away from the tail, so the ANCHOR is what holds the reader and not \
         the follow converge"
    );
    // The engine KEEPS the anchor it captured at the last settle — a growth heard by the observer
    // has already moved the view, and an anchor captured then corrects nothing. These settles are
    // what make the held anchor describe THIS parked view; `userIntentMs` is 300ms on the classic
    // page and 320 on the app shell, so 1.4s also clears the reader-owns-position window that
    // would otherwise leave the correction merely OWED.
    settle();
    settle();
    let before = harness::probe(tab, &geom_js);
    // GROW THE HEAD — the content above the nested children — as a late reflow does.
    let grew = harness::probe(
        tab,
        &format!(
            "(function(){{ var R = {rec_js}; if (!R) return {{ err: 'record gone' }}; \
             var head = R.querySelector('{head_sel}'); if (!head) return {{ err: 'no head' }}; \
             var h = Math.round(head.getBoundingClientRect().height); \
             head.style.minHeight = (h + {GROW}) + 'px'; return {{ was: h }}; }})()"
        ),
    );
    assert!(
        grew.get("err").is_none(),
        "{surface:?}: the record has a head to grow: {grew}"
    );
    settle();
    settle();
    let after = harness::probe(tab, &geom_js);
    assert!(
        before.get("err").is_none() && after.get("err").is_none(),
        "{surface:?}: the record survives the growth; before {before}, after {after}"
    );
    // The growth REALLY HAPPENED. Without this the case would pass green on a page that quietly
    // dropped the height — the app shell rebuilds a unit from state, and a rebuild here would take
    // it with it and leave every measurement below comparing a view with itself.
    assert!(
        after["headH"].as_f64().unwrap_or(0.0) - before["headH"].as_f64().unwrap_or(0.0)
            >= GROW * 0.9,
        "{surface:?}: the head ACTUALLY grew by ~{GROW}px ({} -> {}) — with no growth there is \
         nothing for the anchor to absorb and the case proves nothing; before {before}, after \
         {after}",
        before["headH"],
        after["headH"]
    );
    let (t0, t1) = (
        before["headTop"].as_f64().unwrap_or(f64::NAN),
        after["headTop"].as_f64().unwrap_or(f64::NAN),
    );
    assert!(
        (t1 - t0).abs() <= 4.0,
        "{surface:?}: the reader stays on the head they were reading when the record's OWN head \
         grows ({t0} -> {t1}). The reader can SEE this record's top, so it is the anchor and the \
         growth below it happens in place; an anchor that descends past it to a nested child holds \
         the CHILD instead and drives the head ~{GROW}px off the top of the screen. Before \
         {before}, after {after}"
    );
    // …and the growth really did PROPAGATE below the head. Holding the head is only the right
    // answer if the nested child moved down by it; a head that grew into nothing — clipped,
    // absorbed by a fixed-height ancestor — would leave both tops still and read exactly like a
    // pass.
    assert!(
        after["kidTop"].as_f64().unwrap_or(0.0) - before["kidTop"].as_f64().unwrap_or(0.0)
            >= GROW * 0.9,
        "{surface:?}: the head's growth moved the nested child DOWN by it ({} -> {}) — a growth \
         the layout swallowed would hold both tops still and read as a pass; before {before}, \
         after {after}",
        before["kidTop"],
        after["kidTop"]
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_growth_in_a_visible_records_head_holds_it() {
    let _serial = serial();
    let fx = fixture_nested_records("scenario-visible-head-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_growth_in_a_visible_records_head_holds_it(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_growth_in_a_visible_records_head_holds_it() {
    let _serial = serial();
    let fx = fixture_nested_records("scenario-visible-head-app");
    let page = open(Surface::AppShell, &fx, 2932);
    scenario_a_growth_in_a_visible_records_head_holds_it(&page.tab, Surface::AppShell, &fx);
}

/// #180. Scrolling UP over ground the reader has not visited must move the content by exactly as
/// far as they asked. Above the mounted window every item costs only its FLOOR estimate — 30px on
/// the classic page, 34 on the app shell — against a real height five to twenty times that, so one
/// step of upward scroll mounts a RUN of them and the measure that follows replaces every estimate
/// with the truth. That difference lands ABOVE the reader, and `scrollTop` does not move with it,
/// so the content under them slides down by the whole amount.
///
/// The engine computes exactly that correction in `restoreDomAnchor` and, before #180, threw it
/// away: `readerOwnsPosition()` is true for the whole gesture (a wheel event stamps `lastUserInput`
/// milliseconds earlier), so the write was deferred into `this.owed` — which nothing ever reads
/// back. Measured on the app shell before the fix: +2355px and +2854px of movement for a 900px
/// request, an overshoot of up to 1954px per step, with `scrollHeight` growing by the same amount.
///
/// So the assertion is the reader's own contract: the record you were looking at moves by the
/// distance you scrolled, and by no more. The case walks up in steps and checks EVERY step, and it
/// separately requires that at least one step actually reached unmeasured ground — otherwise it
/// would pass over the region `convergeBottom` already measured and prove nothing.
fn scenario_a_scroll_up_over_fresh_ground_moves_by_what_was_asked(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    let s = surface.scroller();
    let root = match surface {
        // The classic mount is the virtual-window container INSIDE #stream, not #stream itself:
        // `#stream > *` is a single `DIV.vwin` (measured — it reported a constant -15px move while
        // scrollTop fell 900, because the container does not move with its contents).
        Surface::Classic => "(document.getElementById('vwin')||document.querySelector('#stream .vwin')||document.getElementById('stream'))",
        Surface::AppShell => "document.querySelector('.virtual-window')",
    };
    let vt = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{s}.getBoundingClientRect().top"),
    };
    // Hold the reference by the record's own IDENTITY, never by the node. The engine reconciles by
    // REUSING mounted elements, so a node handle silently comes to hold a different record and the
    // measurement compares two unrelated rects — measured while building this case: a constant
    // -900 on the classic page, including over ground where nothing was re-measured.
    let pick = format!(
        r#"(function(){{var vt={vt};var k=[...{root}.children];var p=k.find(function(e){{var r=e.getBoundingClientRect();return r.height>0&&r.bottom>vt+1;}});if(!p)return{{ok:false}};var id=p.id||(p.dataset?p.dataset.unitKey:'');var s={s};return{{ok:!!id,id:id,tag:p.tagName+'.'+p.className,top:Math.round(p.getBoundingClientRect().top),st:Math.round(s.scrollTop),h:Math.round(s.scrollHeight)}};}})()"#
    );
    let reread = |id: &str| {
        format!(
            r#"(function(){{var e=document.getElementById("{id}")||document.querySelector('[data-unit-key="{id}"]');var s={s};if(!e)return{{ok:false,h:Math.round(s.scrollHeight)}};return{{ok:true,top:Math.round(e.getBoundingClientRect().top),st:Math.round(s.scrollTop),h:Math.round(s.scrollHeight)}};}})()"#
        )
    };

    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();

    // LEAVE FOLLOW FIRST, and prove it. Parked at the tail the page is still following, and the
    // #103 hysteresis reads the first scroll inside its slack as displacement and HEALS it back —
    // measured on the classic page: step 1 moved the reference -473px, the wrong way, with
    // scrollHeight unchanged, which is a heal and not the defect this case is about.
    let tail_top = harness::eval(tab, &format!("{s}.scrollTop"))
        .as_f64()
        .unwrap_or(0.0);
    scroll_by(tab, surface, -900);
    settle();
    let left = harness::eval(tab, &format!("{s}.scrollTop"))
        .as_f64()
        .unwrap_or(0.0);
    assert!(
        tail_top - left > 400.0,
        "{surface:?}: the reader's own scroll has to LEAVE the tail before this measures anything          — a following view heals a small scroll straight back and every step would read the heal          instead of the defect. scrollTop {tail_top} -> {left}"
    );

    let step = 900.0;
    let mut reached_fresh = false;
    let mut worst = 0.0_f64;
    for n in 1..=8 {
        let before = harness::probe(tab, &pick);
        if !before["ok"].as_bool().unwrap_or(false) {
            continue;
        }
        let id = before["id"].as_str().unwrap_or("").to_string();
        scroll_by(tab, surface, -(step as i64));
        settle();
        let after = harness::probe(tab, &reread(&id));
        // Scrolled clean past the reference (it left the mounted window): nothing to compare.
        if !after["ok"].as_bool().unwrap_or(false) {
            continue;
        }
        // The page's height CHANGED on this step, so the engine met a record whose real height
        // it did not have — which is what "fresh ground" means. It used to read `grew > 1.0`,
        // because with a constant floor the error was one-signed: every real height was above
        // the floor, so learning one could only grow the page. Since #184 the estimate is a
        // learned mean, so a record shorter than the mean SHRINKS it, and a one-signed test
        // would call a step over fresh ground vacuous.
        let moved_h =
            (after["h"].as_f64().unwrap_or(0.0) - before["h"].as_f64().unwrap_or(0.0)).abs();
        if moved_h > 1.0 {
            reached_fresh = true;
        }
        let moved = after["top"].as_f64().unwrap_or(0.0) - before["top"].as_f64().unwrap_or(0.0);
        let over = (moved - step).abs();
        if over > worst {
            worst = over;
        }
        assert!(
            over <= 12.0,
            "{surface:?}: step {n} asked to scroll up {step}px and the record the reader was on \
             moved {moved}px — an overshoot of {over}px. Above the mounted window every item \
             costs its FLOOR estimate; mounting replaces those with real heights ABOVE the \
             reader, and the correction for it is computed and then dropped because the reader \
             is mid-gesture (#180). scrollHeight {} -> {}",
            before["h"],
            after["h"]
        );
    }
    // Without this the case would pass over already-measured ground and assert nothing: the tail
    // jump leaves roughly 1500px above it measured, and the first steps never leave that.
    assert!(
        reached_fresh,
        "{surface:?}: the walk never reached UNMEASURED ground — scrollHeight never moved, so \
         every step was over heights the engine already knew and the case proved nothing. Worst \
         overshoot seen was {worst}px."
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_a_scroll_up_over_fresh_ground_moves_by_what_was_asked() {
    let _serial = serial();
    let fx = fixture_varied_prose("scenario-fresh-ground-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_scroll_up_over_fresh_ground_moves_by_what_was_asked(
        &page.tab,
        Surface::Classic,
        &fx,
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_scroll_up_over_fresh_ground_moves_by_what_was_asked() {
    let _serial = serial();
    let fx = fixture_varied_prose("scenario-fresh-ground-app");
    let page = open(Surface::AppShell, &fx, 2934);
    scenario_a_scroll_up_over_fresh_ground_moves_by_what_was_asked(
        &page.tab,
        Surface::AppShell,
        &fx,
    );
}

/// Tall enough that a walk upward LEAVES the region the tail jump already measured — the whole
/// point of #180's case. An 18-turn session (~5.5k px) is not: every 900px step stays inside
/// `convergeBottom`'s measured window and reads zero drift.
/// The same 120 turns, but with answers whose heights are all over the place — one line, then
/// forty, then five. A MEAN is right about uniform prose and wrong about this, which is what makes
/// it the right ground for #180: the point of that case is that the reader lands where they asked
/// even when the engine's guess for the run above them is badly wrong, and since #184 the guess
/// over `fixture_tall_prose` is so nearly right that the case could no longer produce the error it
/// exists to survive (measured: `scrollHeight` did not move once across the whole walk, and the
/// case failed its own not-vacuous guard).
///
/// It is also the honest statement of what #184 leaves behind: a running mean shrinks the error on
/// a page whose records are alike, and does much less for one whose records are not.
fn fixture_varied_prose(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = String::new();
    for i in 0..120 {
        transcript += &user_at(
            &format!("question {i}: a prompt with enough words to make a real record"),
            &now_minus(4000 - i as u64 * 30),
        );
        // 1 / 40 / 5 / 18 lines, cycling: no mean fits more than a quarter of them.
        let lines = [1usize, 40, 5, 18][i % 4];
        transcript += &assistant_at(
            &format!(
                "answer {i}: {}",
                "a paragraph of prose to make a line of real height. ".repeat(lines)
            ),
            &now_minus(3990 - i as u64 * 30),
        );
    }
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 120,
    }
}

fn fixture_tall_prose(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = String::new();
    for i in 0..120 {
        transcript += &user_at(
            &format!("question {i}: a prompt with enough words to make a real record"),
            &now_minus(4000 - i as u64 * 30),
        );
        transcript += &assistant_at(
            &format!(
                "answer {i}: {}",
                "prose that is far taller than the 30px floor. ".repeat(10)
            ),
            &now_minus(3990 - i as u64 * 30),
        );
    }
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 120,
    }
}

// ── scenario: the tail is a wall — a scroll DOWN never moves the reader away from it (#179) ──

/// Jump to the end, scroll up a little, then walk back down. Every downward step must bring the
/// reader CLOSER to the bottom, and the walk must land on the bottom and stay there.
///
/// What it caught (#179, app shell, measured on a 120-turn session): from a 62px gap one wheel
/// down landed on the tail and the reader was thrown 262px back UP — then 200 down, then 262 up,
/// for ever, between exactly two positions. The page "lets you keep scrolling" and only ever
/// re-renders the last few records, which is how it was reported.
///
/// `reconcile` forces layout — `measureMounted` reads every mounted child's box — while the PADS
/// still describe the window it is replacing. For the length of that measure the content is short
/// by exactly what the new window drops off its top: one turn, 262px here. A browser clamps
/// `scrollTop` to a page that has just shrunk, so a reader sitting ON the tail is pulled up by
/// the whole difference; the pads are written a moment later and the page is its old height
/// again, with the reader 262px above where they were. Worse, the clamp's own scroll event lands
/// inside the intent window, so the engine reads it as the READER's scroll — and 262px is past
/// the 80px hold slack, so it unfollows on it too.
fn scenario_the_tail_is_a_wall(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    let s = surface.scroller();
    let gap_js = format!(
        "(function(){{var s={s};return Math.round(s.scrollHeight-s.clientHeight-s.scrollTop);}})()"
    );
    let gap = |tab: &headless_chrome::Tab| harness::eval(tab, &gap_js).as_f64().unwrap_or(-1.0);

    jump_to_end(tab, surface);
    await_tail(tab, surface, "the tail before the walk back");
    settle();
    // A LITTLE way up — the report's own gesture. Far enough that the walk down has somewhere to
    // go, near enough that one step of the same size should put the reader back on the tail.
    scroll_by(tab, surface, -200);
    settle();
    let start = gap(tab);
    assert!(
        start > 20.0,
        "{surface:?}: the scroll up never left the tail (gap {start}px), so the walk down would \
         prove nothing"
    );

    let mut steps = vec![start];
    let (mut prev, mut worst) = (start, 0f64);
    for _ in 0..6 {
        scroll_by(tab, surface, 200);
        settle();
        let now = gap(tab);
        steps.push(now);
        worst = worst.max(now - prev);
        prev = now;
    }
    assert!(
        worst <= 4.0,
        "{surface:?}: a scroll DOWN moved the reader {worst}px AWAY from the bottom — the tail \
         pushed back instead of holding. Gap after each step: {steps:?}"
    );
    assert!(
        prev <= 2.0,
        "{surface:?}: the walk down never reached the bottom — it stopped {prev}px short after \
         six steps of 200px from a {start}px gap. Gap after each step: {steps:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_the_tail_is_a_wall() {
    let _serial = serial();
    let fx = fixture_tall_prose("scenario-tail-wall-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_tail_is_a_wall(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_tail_is_a_wall() {
    let _serial = serial();
    let fx = fixture_tall_prose("scenario-tail-wall-app");
    let page = open(Surface::AppShell, &fx, 2938);
    scenario_the_tail_is_a_wall(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: the page stops growing once it knows what a record costs (#184) ────────────────

/// Walk up a long transcript from the tail and watch the page's own height. Every step mounts
/// records the engine has never measured, and the difference between what it GUESSED they cost
/// and what they really cost lands on `scrollHeight`. Over a walk of several thousand pixels that
/// difference has to stay small.
///
/// What it is measuring, and why it is not the same case as #180. #180 asserts the READER ends up
/// where they asked to be — that the correction for the difference LANDS. This asserts the
/// difference itself is small, which is what decides whether that correction is a stutter or
/// invisible. With the old constant floor (30px classic, 34/40/44 on the shell) against records
/// that really run 78 and 184, a 9000px walk grew the page by thousands of pixels and every one
/// of them had to be corrected under the reader. With a running mean seeded from the records the
/// tail jump already measured, a prompt is guessed high and its answer low and the run they form
/// comes out very nearly exact.
fn scenario_the_page_stops_growing_once_it_knows_what_a_record_costs(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    let s = surface.scroller();
    let at = |tab: &headless_chrome::Tab| {
        harness::probe(
            tab,
            &format!("(function(){{var s={s};return {{st:Math.round(s.scrollTop),h:Math.round(s.scrollHeight)}};}})()"),
        )
    };

    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Leave follow first, for #180's reason: parked at the tail the page is still following and
    // heals a small scroll straight back, which would be measured as the engine's own growth.
    scroll_by(tab, surface, -900);
    settle();

    const STEP: i64 = 900;
    const STEPS: i64 = 9;
    let asked = (STEP * STEPS) as f64;
    let start = at(tab);
    let first_turn = turn_at_top(tab, surface);
    for _ in 0..STEPS {
        scroll_by(tab, surface, -STEP);
        settle();
    }
    let end = at(tab);
    let last_turn = turn_at_top(tab, surface);

    let grew = (end["h"].as_f64().unwrap_or(0.0) - start["h"].as_f64().unwrap_or(0.0)).abs();
    // Not vacuous, measured in TURNS rather than in scroll offset. The offset is exactly the
    // quantity the defect corrupts — with a constant floor the page grows above the reader as
    // fast as they climb, so `scrollTop` moved only 2561px of the 8100px they asked for and a
    // guard written on it would fail the good case and the bad one alike. How many records they
    // travelled past is independent of what the engine believed those records weighed.
    let travelled = first_turn - last_turn;
    assert!(
        travelled >= 15,
        "{surface:?}: the walk passed only {travelled} turns (top turn {first_turn} -> \
         {last_turn}), so it never left the region the tail jump had already measured and the \
         case proved nothing."
    );
    assert!(
        grew <= asked * 0.2,
        "{surface:?}: the reader asked to climb {asked}px, passing {travelled} turns, and the \
         page's own height moved {grew}px doing it — {:.0}% of the distance. Every one of those \
         pixels is a record whose guessed height was wrong, landing ABOVE the reader and needing \
         a correction under them (#184). scrollHeight {} -> {}, scrollTop {} -> {}",
        grew / asked * 100.0,
        start["h"],
        end["h"],
        start["st"],
        end["st"]
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn classic_page_the_page_stops_growing_once_it_knows_what_a_record_costs() {
    let _serial = serial();
    let fx = fixture_tall_prose("scenario-learned-height-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_page_stops_growing_once_it_knows_what_a_record_costs(
        &page.tab,
        Surface::Classic,
        &fx,
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_page_stops_growing_once_it_knows_what_a_record_costs() {
    let _serial = serial();
    let fx = fixture_tall_prose("scenario-learned-height-app");
    let page = open(Surface::AppShell, &fx, 2940);
    scenario_the_page_stops_growing_once_it_knows_what_a_record_costs(
        &page.tab,
        Surface::AppShell,
        &fx,
    );
}

/// A long session whose LAST record is a capped tool output — so a "⋯ N more lines" expander sits
/// at the tail, below the reader, which is the position #185 was reported from. The generic
/// `fixture()` has no record long enough to be capped at all.
fn fixture_capped_tail(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let turns = 60u32;
    let mut transcript = long_session(turns, Shape::default());
    transcript += &tool_open_at("t-capped-tail", &now_minus(90));
    transcript += &tool_result_lines("t-capped-tail", 200, &now_minus(80));
    let path = stores.claude_session(SID, &transcript);
    Fixture { base, path, turns }
}

// ── scenario: a fold opened at the tail does not snap the reader back to it (#185) ───────────

/// Park at the tail, open a fold, and stay where the growth left you.
///
/// What it caught (#185, reported by the owner on v1.248.0): "I scroll to the end, then click show
/// more on a block, the block unfolds downward correctly (anything above it is not moved), so now
/// the page is no longer at the bottom. However, apparently the engine did not think so and
/// immediately snaps the page to the bottom."
///
/// The growth is correct and the pin is wrong. Parked at the tail the page is still FOLLOWING; the
/// fold makes it taller BELOW the reader — the anchor doing exactly its job — and the follow rule
/// then does its own job on a page that is no longer at its tail, converges, and scrolls away the
/// very thing the click asked to see. The fix is that growth the reader ASKED for drops the pin
/// (`readerReshaped`), which is a different question from #165's: there the tail really did move,
/// because new content arrived.
///
/// Deliberately not a height test. One was considered and withdrawn — "I think I am fine to just
/// drop the pin regardless how tall the unfolded block is… It is just one scroll away to re-pin
/// the tail, and feels natural" — because the same click doing two things depending on the block
/// is not predictable from the outside.
fn scenario_a_fold_opened_at_the_tail_does_not_snap_back(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    let s = surface.scroller();
    let gap_js = format!(
        "(function(){{var s={s};return Math.round(s.scrollHeight-s.clientHeight-s.scrollTop);}})()"
    );
    let height_js = format!("(function(){{var s={s};return Math.round(s.scrollHeight);}})()");

    // Click a fold whose head is BELOW the reader's anchor — which is what the report describes
    // ("the block unfolds downward correctly, anything above it is not moved"). A head ABOVE the
    // anchor is a different situation with a different right answer: content grew over the row
    // the reader is on, and the anchor moving them down to keep that row still is the anchor
    // WORKING. Measured while building this case: `open_last_fold` takes the last head in the
    // DOM, which inside a tall process unit can sit above the viewport top, and the case then
    // measured the anchor instead of the pin — +1786px on the shell, exactly the growth, and a
    // classic run that passed once and failed the next time on the same code.
    // "⋯ N more lines" — the control the owner was actually clicking, and the one that only ever
    // GROWS. A fold head was tried first and is the wrong instrument twice over: its click is a
    // four-step cycle, so on a head the page had already opened it CLOSES (measured: the classic
    // page shrank 76px and the case reported itself vacuous), and it routes through the shells'
    // re-render path while a cap expander reveals in place and reaches the engine only through
    // the resize observer — which is exactly the path that was left uncovered.
    let head_selector = match surface {
        Surface::Classic => ".morebtn",
        Surface::AppShell => "[data-cap-more]",
    };
    let click_below = format!(
        r#"(function(){{var s={s};var vt=s===document.scrollingElement?0:s.getBoundingClientRect().top;
var vb=vt+s.clientHeight;var hs=[].slice.call(document.querySelectorAll('{head_selector}'));
var pick=null;for(var i=0;i<hs.length;i++){{var r=hs[i].getBoundingClientRect();
if(r.top>vt+4&&r.top<vb-4&&r.height>0)pick=hs[i];}}
if(!pick)return{{ok:false,heads:hs.length,vt:Math.round(vt),vb:Math.round(vb),rects:hs.map(function(h){{var q=h.getBoundingClientRect();return [Math.round(q.top),Math.round(q.height)];}})}};var r=pick.getBoundingClientRect();
var f=pick.closest('.fold')||pick.closest('[data-renderer]');
var was={{open:f?(f.dataset.open||(f.classList.contains('closed')?'0':'1')):'?',cls:f?f.className:'',id:f?(f.id||f.dataset.recordId||''):''}};
pick.click();
return{{ok:true,top:Math.round(r.top-vt),vh:Math.round(s.clientHeight),heads:hs.length,was:was}};}})()"#
    );

    // A capped output lives inside a fold, and a tool fold opens CLOSED — so the expander is in
    // the DOM but has no box. Open the fold first, then jump back to the tail: that is the state
    // the report is from, a reader parked at the end with a long output on screen. The jump is a
    // commanded converge, so it re-pins after the fold's own `readerReshaped` dropped the pin.
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the jump to land at the tail");
    settle();
    // A capped output can sit several folds deep — measured: the expander's own renderer inside a
    // closed parent renderer whose body is `display:none`, so opening the innermost one leaves it
    // with no box at all. Open the whole ancestor chain, outermost first, re-querying each time
    // because a shell re-render replaces the nodes underneath.
    let (fold_sel, head_sel, closed_test) = match surface {
        Surface::Classic => (".fold", ".fold-h", "f.dataset.open==='0'"),
        Surface::AppShell => (
            "[data-renderer]",
            "button.renderer-head",
            "f.classList.contains('closed')",
        ),
    };
    let reveal = format!(
        r#"(function(){{var clicked=0;
for(var pass=0;pass<6;pass++){{
  var cap=document.querySelector('{head_selector}');
  if(!cap)return{{ok:false,why:'no cap expander in the document'}};
  if(cap.getBoundingClientRect().height>0)return{{ok:true,clicked:clicked}};
  var outer=null;for(var e=cap.parentElement;e&&e!==document.body;e=e.parentElement){{
    if(e.matches('{fold_sel}')){{var f=e;if({closed_test})outer=e;}}}}
  if(!outer)return{{ok:false,why:'the expander has no box and no closed fold above it',clicked:clicked}};
  var h=outer.querySelector(':scope > {head_sel}')||outer.querySelector('{head_sel}');
  if(!h)return{{ok:false,why:'a closed fold above the expander has no head',clicked:clicked}};
  h.click();clicked++;}}
return{{ok:false,why:'still hidden after six passes',clicked:clicked}};}})()"#
    );
    let revealed = harness::probe(tab, &reveal);
    assert!(
        revealed["ok"].as_bool().unwrap_or(false),
        "{surface:?}: could not bring the capped output on screen: {revealed}"
    );
    settle();
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the second jump to land back at the tail");
    settle();
    let before_h = harness::eval(tab, &height_js).as_f64().unwrap_or(0.0);

    let opened = harness::probe(tab, &click_below);
    assert!(
        opened["ok"].as_bool().unwrap_or(false),
        "{surface:?}: no fold head sits below the reader and inside the viewport at the tail, so \
         there is nothing to open in the position the report describes: {opened}"
    );
    settle();

    let after_h = harness::eval(tab, &height_js).as_f64().unwrap_or(0.0);
    let gap = harness::eval(tab, &gap_js).as_f64().unwrap_or(0.0);
    // Not vacuous: if the fold added nothing there is no growth to be snapped over, and a page
    // that is still at its tail proves nothing either way.
    assert!(
        after_h - before_h > 40.0,
        "{surface:?}: opening the fold grew the page by only {}px, so there was nothing for the \
         pin to snap over and the case proved nothing. scrollHeight {before_h} -> {after_h}; the \
         head it clicked was {opened}",
        after_h - before_h
    );
    assert!(
        gap > 20.0,
        "{surface:?}: the fold grew the page by {}px below the reader and the engine put them \
         back on the tail anyway — gap {gap}px. Growth the reader ASKED for is not the tail \
         moving away from them (#185); it drops the pin. scrollHeight {before_h} -> {after_h}",
        after_h - before_h
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_fold_opened_at_the_tail_does_not_snap_back() {
    let _serial = serial();
    let fx = fixture_capped_tail("scenario-fold-tail-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_fold_opened_at_the_tail_does_not_snap_back(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_fold_opened_at_the_tail_does_not_snap_back() {
    let _serial = serial();
    let fx = fixture_capped_tail("scenario-fold-tail-app");
    let page = open(Surface::AppShell, &fx, 2942);
    scenario_a_fold_opened_at_the_tail_does_not_snap_back(&page.tab, Surface::AppShell, &fx);
}
