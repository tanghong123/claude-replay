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
    assistant_at, at_tail, base, click_session_id, copied_text, jump_to_end, key,
    last_mounted_turn, long_session, now_minus, open_last_fold, queued_at, queued_text, scroll_by,
    serial, session_id_chip, stub_clipboard, turn_at_top, until, user_at, Kind, LiveGrowth,
    Monitor, Shape, Stores, Surface,
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
            monitor.open(&tab, &format!("?ui=app&session={SID}"));
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
        Surface::Classic => (format!("?ui=classic&session={SID}"), "document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3", "document.querySelectorAll('#stream [data-turn]').length"),
        Surface::AppShell => (format!("?ui=app&session={SID}"), "document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3", "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : 'no window'"),
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
        if ticks % 9 == 0 {
            scroll_by(tab, surface, -400);
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
    std::thread::sleep(Duration::from_millis(4000));
    let said = harness::new_messages_pill(tab, surface);
    assert_eq!(said, 8, "the pill says how many records arrived");
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

/// The header shows the session id short (the shared form: the UUID's first eight hex
/// digits); a click copies the transcript's path on disk and says so for a moment.
fn scenario_session_id_copies_the_transcript_path(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    until(
        tab,
        &format!(
            "(document.getElementById('{}') || {{}}).textContent === '{}'",
            match surface {
                Surface::Classic => "sid",
                Surface::AppShell => "sessionId",
            },
            &SID[..8]
        ),
        "the header to show the short session id",
        Duration::from_secs(20),
        &format!(
            "(document.getElementById('{}') || {{textContent: 'no element'}}).textContent",
            match surface {
                Surface::Classic => "sid",
                Surface::AppShell => "sessionId",
            }
        ),
    );
    assert_eq!(
        session_id_chip(tab, surface),
        &SID[..8],
        "the short id is the UUID's first eight hex digits"
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
    until(
        tab,
        &format!(
            "(document.getElementById('{}') || {{}}).textContent === 'copied transcript path'",
            match surface {
                Surface::Classic => "sid",
                Surface::AppShell => "sessionId",
            }
        ),
        "the chip to say it copied",
        Duration::from_secs(5),
        &format!(
            "(document.getElementById('{}') || {{}}).textContent",
            match surface {
                Surface::Classic => "sid",
                Surface::AppShell => "sessionId",
            }
        ),
    );
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
