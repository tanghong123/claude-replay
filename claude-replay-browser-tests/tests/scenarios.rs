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
    last_mounted_turn, long_session, now_minus, open_last_fold, open_turn_session, probe,
    queued_at, queued_text, read_tool_at, scroll_by, selection_text, serial, session_id_chip,
    stub_clipboard, tap_console, tool_open_at, tool_result_at, tool_result_lines, tool_result_text,
    turn_at_top, until, user_at, view_anchor_index, Kind, LiveGrowth, Monitor, Shape, Stores,
    Surface,
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

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_filter_takes_in_what_arrives() {
    let _serial = serial();
    let fx = fixture("scenario-livefilter-app", 14);
    let page = open(Surface::AppShell, &fx, 2905);
    scenario_the_filter_takes_in_what_arrives(&page.tab, Surface::AppShell, &fx);
}

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
            // The reader's own scroll is not over when the wheel stops: a fling is still
            // travelling, and while it is, a correction owed by growth is deliberately not
            // written (#132 step 3 — writing under momentum stutters or kills it). Take the
            // baseline once that window has lapsed, so this measures what the rule is about:
            // between the reader's OWN movements, nothing moves on its own.
            std::thread::sleep(Duration::from_millis(500));
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

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_tool_filter_hides_and_lands() {
    let _serial = serial();
    let fx = filter_fixture("scenario-filter-app");
    let page = open(Surface::AppShell, &fx, 2884);
    scenario_tool_filter_hides_and_lands(&page.tab, Surface::AppShell, &fx);
}

/// Twelve turns of Bash, then a turn with one Read among the Bash calls.
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
            "(function(){ var v = [...document.querySelectorAll('#stream .codebar .ms-val')].pop(); return v ? Number(v.textContent) : -1; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-up')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-wrap')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .ms-wrap')].pop(); return b ? b.textContent : ''; })()",
            "(function(){ var b = [...document.querySelectorAll('#stream .codebar .cpy-code')].pop(); if (!b) return 'none'; b.click(); return 'clicked'; })()",
        ),
        Surface::AppShell => (
            "(function(){ var g = [...document.querySelectorAll('.codebox .ln')].pop(); return g ? getComputedStyle(g).userSelect : 'none-found'; })()",
            "(function(){ var v = [...document.querySelectorAll('.codebox [data-code-size-val]')].pop(); return v ? Number(v.textContent) : -1; })()",
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
    let size = eval(tab, size_val).as_f64().unwrap_or(-1.0);
    assert!(size > 0.0, "the pane's bar shows the code size: {size}");
    assert_eq!(
        eval(tab, size_up),
        "clicked",
        "the pane's bar steps the size"
    );
    settle();
    let after = eval(tab, size_val).as_f64().unwrap_or(-1.0);
    assert!(
        (after - size - 0.5).abs() < 0.01,
        "…by half a pixel: {size} → {after}"
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
