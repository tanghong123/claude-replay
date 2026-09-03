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
    assistant_at, at_tail, base, jump_to_end, key, last_mounted_turn, long_session, now_minus,
    open_last_fold, scroll_by, serial, turn_at_top, user_at, Kind, LiveGrowth, Monitor, Shape,
    Stores, Surface,
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
    _monitor: Option<Monitor>,
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
                "document.querySelectorAll('#stream [data-turn]').length >= 3 && document.body.scrollHeight > window.innerHeight * 3",
                "the classic page to render the fixture",
                Duration::from_secs(30),
                "document.querySelectorAll('#stream [data-turn]').length",
            );
            Opened {
                tab,
                _browser: browser,
                _server: Some(server),
                _monitor: None,
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
                "document.querySelectorAll('.virtual-window [data-turn]').length >= 3",
                "the app shell to mount the fixture",
                Duration::from_secs(30),
                "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : 'no window'",
            );
            Opened {
                tab,
                _browser: browser,
                _server: None,
                _monitor: Some(monitor),
            }
        }
    }
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

/// A fresh open lands pinned at the tail and STAYS there while the transcript grows; after the
/// reader scrolls up, growth leaves the turn at the top where it was.
fn scenario_growth(tab: &headless_chrome::Tab, surface: Surface, fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    let last_before = last_mounted_turn(tab, surface);
    let script: Vec<String> = (0..4)
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
        .collect();
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(2600));
    let appended = growth.finish(Duration::from_secs(40));
    assert_eq!(appended, 8, "the driver appended the whole script");
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
    let last = turn_at_top(tab, surface);
    scroll_by(tab, surface, -900);
    scroll_by(tab, surface, -900);
    settle();
    let held = turn_at_top(tab, surface);
    assert!(
        held >= 0 && held < last.max(1),
        "the reader scrolled up: top turn {last} -> {held}"
    );
    let more: Vec<String> = (0..3)
        .map(|k| {
            assistant_at(
                &format!("later tail {k}: {}", "more prose. ".repeat(12)),
                &now_minus(20 - k * 5),
            )
        })
        .collect();
    let growth = LiveGrowth::start(fx.path.clone(), more, Duration::from_millis(2600));
    let appended = growth.finish(Duration::from_secs(30));
    assert_eq!(appended, 3);
    std::thread::sleep(Duration::from_millis(3000));
    let after = turn_at_top(tab, surface);
    assert_eq!(
        after, held,
        "unpinned: growth did not move the reader ({held} -> {after})"
    );
    assert!(!at_tail(tab, surface), "…and did not re-pin");
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_follows_the_tail_when_pinned_and_holds_when_not() {
    let _serial = serial();
    let fx = fixture("scenario-growth-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_growth(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_follows_the_tail_when_pinned_and_holds_when_not_known_red_70() {
    let _serial = serial();
    let fx = fixture("scenario-growth-app", 40);
    let page = open(Surface::AppShell, &fx, 2852);
    scenario_growth(&page.tab, Surface::AppShell, &fx);
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
fn app_shell_pane_follows_the_transcript_known_red_52() {
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
fn classic_page_search_survives_growth_known_red_71() {
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
