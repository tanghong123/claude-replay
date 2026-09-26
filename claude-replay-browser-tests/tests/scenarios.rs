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
    tool_result_lines, tool_result_text, turn_at_top, until, until_reader_owns_the_view, user_at,
    view_anchor_index, write_tool_at, Kind, LiveGrowth, Monitor, Shape, Stores, Surface,
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
/// `&trace=viewport` when `SCENARIO_TRACE` is set — the same switch `trace_tail` already reads.
/// An intermittent viewport case is diagnosed by running it in a loop with the engine's own trace
/// on and dumping it AT the failing step, not by reasoning about it (#209; #192 built the trace
/// for exactly this).
fn trace_query() -> &'static str {
    if std::env::var_os("SCENARIO_TRACE").is_some() {
        "&trace=viewport"
    } else {
        ""
    }
}

/// The engine's last `n` trace entries as one block of text, for a failure message — empty when
/// the trace is off.
fn trace_lines(tab: &headless_chrome::Tab, n: usize) -> String {
    let js = format!("(function(){{ var t = window.__viewportTrace || []; return t.slice(-{n}).map(function (e) {{ return [e.seq, e.cause, 'lo=' + e.lo, 'hi=' + e.hi, 'p0=' + (e.p0 == null ? '' : e.p0), 'placed=' + (e.placed == null ? '' : e.placed), 'top=' + Math.round(e.top || 0), 'sums=' + Math.round(e.sums || 0), 'pads=' + (e.pads || []).map(Math.round).join('/'), 'est=' + (e.estimate == null ? '' : Math.round(e.estimate)), 'live=' + (e.live == null ? '' : Math.round(e.live)), 'since=' + (e.sinceInput == null ? '' : Math.round(e.sinceInput))].join(' '); }}).join('\n'); }})()");
    harness::eval(tab, &js).as_str().unwrap_or("").to_string()
}

fn sid_of(fx: &Fixture) -> String {
    fx.path.file_stem().unwrap().to_string_lossy().to_string()
}

/// A run whose journal NAMES its members (#241): two phases in launch order plus one agent that
/// ran outside any `phase()` block. Measured across the 68 real runs, 9 record phases and 59 do
/// not, and 742 agents carry none — so the unphased member is not an edge case, it is the bulk.
fn fixture_workflow_phased(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(20, Shape::default());
    transcript += &user_at("question 20: fan the work out", &now_minus(90));
    transcript += &harness::workflow_call_at("wf1", RUN, &now_minus(88));
    transcript += &assistant_at("answer 20: the fleet is on it", &now_minus(80));
    let path = stores.claude_session(SID, &transcript);
    // `Verify` is launched BETWEEN the two `Find` agents on purpose: the grouping must key on
    // the phase, not on adjacency, and must keep first-launch order rather than sorting.
    stores.claude_workflow_run_named(
        SID,
        RUN,
        &[
            (
                "afind1",
                "find:owned-path-plain",
                "Find",
                "Found the plain path.",
            ),
            ("averify", "verify:residency", "Verify", "Confirmed."),
            ("afind2", "find:residency-accounting", "Find", ""),
            ("aloose", "", "", "An agent outside any phase."),
        ],
    );
    for member in ["afind1", "averify", "afind2", "aloose"] {
        stores.claude_session(member, &long_session(2, Shape::default()));
    }
    Fixture {
        base,
        path,
        turns: 21,
    }
}

/// #241 — BOTH pages render a run's phases as groups, in launch order, with each member under
/// the phase it actually ran in and the unphased remainder under the run itself.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_group_a_workflow_fleet_by_its_phases() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 2996)] {
        let fx = fixture_workflow_phased(match surface {
            Surface::Classic => "wf-phased-classic",
            _ => "wf-phased-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        harness::until(
            &page.tab,
            "document.querySelectorAll('.fleet-row').length >= 4",
            "the fleet roster to arrive on the meta",
            std::time::Duration::from_secs(15),
            "document.querySelectorAll('.fleet-row').length",
        );
        let seen: serde_json::Value = eval(
            &page.tab,
            "(function(){ var box = document.querySelector('.fleet'); if (!box) return JSON.stringify({miss:'no fleet'}); \
               return JSON.stringify({ order: [...box.children].map(function (e) { \
                 return e.classList.contains('fleet-phase') ? 'PHASE:' + e.textContent.trim() \
                   : 'row:' + (e.querySelector('.fleet-name') || {}).textContent; }) }); })()",
        )
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
        let order: Vec<String> = seen["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert!(!order.is_empty(), "{surface:?} draws the fleet: {seen}");
        let phases: Vec<&String> = order.iter().filter(|s| s.starts_with("PHASE:")).collect();
        assert_eq!(
            phases,
            ["PHASE:Find", "PHASE:Verify"].iter().collect::<Vec<_>>(),
            "{surface:?} names the phases in LAUNCH order — sorting them would scramble a \
             workflow's own sequence: {order:?}"
        );
        let find_at = order.iter().position(|s| s == "PHASE:Find").unwrap();
        let verify_at = order.iter().position(|s| s == "PHASE:Verify").unwrap();
        let both_finds: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains("find:"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            both_finds.iter().all(|i| *i > find_at && *i < verify_at),
            "{surface:?} groups by PHASE, not by adjacency — `verify:residency` was launched \
             between the two Find agents and must not split them: {order:?}"
        );
        let loose = order
            .iter()
            .position(|s| s.contains("aloose") || s.contains("outside any phase"))
            .or_else(|| order.iter().rposition(|s| s.starts_with("row:")));
        assert!(
            loose.unwrap_or(0) > verify_at,
            "{surface:?} puts an agent that ran outside any phase() under the RUN, last — never \
             filed under a phase it did not run in: {order:?}"
        );
    }
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
        &harness::at("15:01"),
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
    jsonl += &harness::input_request_at("ask-1", "Which shell should stay?", &harness::at("15:01"));
    jsonl += &harness::input_request_at("ask-2", "Ship the release now?", &harness::at("15:02"));
    jsonl += &harness::input_request_answer("ask-2", "ship", "Yes, ship it", &harness::at("15:03"));
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
    open_with(surface, fx, port, "")
}

/// `open`, with extra query parameters — `mountall=1` for the parity audit, which needs every
/// record in the DOM on BOTH pages before it can compare them record by record (#232).
fn open_with(surface: Surface, fx: &Fixture, port: u16, extra: &str) -> Opened {
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
            let url = if extra.is_empty() {
                url
            } else {
                format!(
                    "{url}{}{}",
                    if url.contains('?') { "&" } else { "?" },
                    extra
                )
            };
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
            monitor.open(
                &tab,
                &format!(
                    "?ui=app&session={}{}{}",
                    sid_of(fx),
                    trace_query(),
                    if extra.is_empty() {
                        String::new()
                    } else {
                        format!("&{extra}")
                    }
                ),
            );
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
        "timed out waiting for {what} (top turn {}); {}",
        turn_at_top(tab, surface),
        harness::renderer_verdict(tab)
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
        &harness::at("15:01"),
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

/// While the reader is moving the view — a wheel still travelling as a fling — growth arriving
/// must not move what they are looking at. Until #196 this case asserted the POLICY that held it:
/// no write to `scrollTop` while the intent window is open, the correction owed and paid at rest.
/// #196 stage 2 replaced the policy with the invariant it stood for (framework I7): a change that
/// has moved the DOM is placed back at once, and the write undoes exactly the engine's own
/// displacement — so a write under the wheel is a no-op for the reader, and withholding it is the
/// displacement (#180). What the reader can measure is what is asserted: the record under them
/// and its screen offset are the same after the storm as before, to the pixel. The writes are
/// still counted and printed, never asserted: measured on the committed stage-2 engine, both pages
/// wrote none during the storm (the growth lands below the reader and displaces nothing).
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
    let (key_before, top_before) = harness::view_anchor(tab, surface);
    assert!(
        !key_before.is_empty(),
        "{surface:?}: the reader is on a record before the storm"
    );
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
    println!("{surface:?}: the page wrote the scroll offset {during} time(s) during the storm");
    // The reader's view did not move: the same record, at the same screen offset, through growth
    // that arrived while the intent window was open the whole time. Nothing was owed and nothing
    // was dropped (#138): a placement that reads where they are replays nothing.
    let (key_during, top_during) = harness::view_anchor(tab, surface);
    assert_eq!(
        key_during, key_before,
        "{surface:?}: the record under the reader is the one they had before the growth"
    );
    assert!(
        (top_during - top_before).abs() <= 1.0,
        "{surface:?}: …at the same screen offset: {top_before} -> {top_during} (writes: {during})"
    );
    settle();
    settle();
    let (key, top) = harness::view_anchor(tab, surface);
    assert_eq!(
        key, key_before,
        "{surface:?}: once the motion stops the reader is still holding that record"
    );
    assert!(
        (top - top_before).abs() <= 1.0,
        "{surface:?}: …where it was: {top_before} -> {top} (writes during: {during})"
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
        let was = scroll_now(tab, surface);
        scroll_by(tab, surface, 320);
        settle();
        // The loop STOPS at the first reading of `turn >= 7` and then asserts it is exactly 7,
        // so a single early sample that reports 8 fails the case outright — there is no second
        // chance. Measured 4 failures in 20 runs, all here rather than at the click below. The
        // bar must therefore be read from a view that has finished placing, not merely stopped.
        until_move_settles(tab, surface, was);
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
    // The bar is a spy on a view that is still placing after three wheels; reading it early
    // names a turn the reader has already left (#209, #227). Additive to the settle above.
    until_engine_quiet(tab);
    let (later, later_text) =
        harness::sticky_turn(tab, surface).expect("the bar still names the turn being read");
    assert!(
        later > turn,
        "reading on moves the bar forward: {turn} -> {later} ({later_text})"
    );
    let before_click = scroll_now(tab, surface);
    harness::click_sticky_turn(tab, surface);
    settle();
    // The click COMMANDS a move, so waiting for the engine to go quiet is not enough: the move
    // may not have started when the sleep elapses, and a scroller that has not begun moving
    // reads as one that has finished. Measured 4 failures in 10 runs with only settles, and
    // still 3 in 10 with a quiet-check alone — always reading the turn the reader had scrolled
    // to rather than the one the click was returning to.
    until_move_settles(tab, surface, before_click);
    let landed = turn_at_top(tab, surface);
    if (landed - later).abs() > 1 {
        let after = scroll_now(tab, surface);
        let bar_now = harness::sticky_turn(tab, surface);
        eprintln!(
            "TURNBAR bar named {later} ({later_text}); scroll {before_click} -> {after}; \
             turn_at_top {landed}; bar now {bar_now:?}"
        );
        history_tail(tab, "after clicking the sticky turn", 12);
    }
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

// ── scenario: a text field keeps the image viewer's zoom keys (#268) ─────────────────────────

/// `0` `1` `-` `=` (and the shifted `_` `+`) are the enlarged image viewer's zoom keys, bound on
/// the document by the shared engine. They are also ordinary characters, so a reader typing into a
/// text field must get them. The app shell builds its lightbox at LOAD, so before #268 that
/// document listener called `preventDefault` on every one of these keys for the whole page — with
/// no image open at all — and they never reached the compose box, the passcode field or a search
/// box. The classic page builds its viewer on demand, so it never had the bug and is the green
/// reference here. Typed with REAL key events (`type_str`): the defect is a `preventDefault` on a
/// trusted keydown, which a synthetic `dispatchEvent` (what `harness::key` sends) cannot reproduce.
fn scenario_text_fields_keep_the_zoom_keys(tab: &headless_chrome::Tab, surface: Surface) {
    let input = match surface {
        Surface::Classic => "q",
        Surface::AppShell => "transcriptSearchInput",
    };
    // No image is open anywhere. Focus the search box and empty it.
    let focused = eval(
        tab,
        &format!(
            "(function(){{ var i = document.getElementById({input:?}); if (!i) return false; \
             i.focus(); i.value = ''; return document.activeElement === i; }})()"
        ),
    );
    assert_eq!(
        focused,
        serde_json::json!(true),
        "the {surface:?} search box took focus"
    );
    // The unshifted keys the viewer claims (out/in/fit/actual), each a character the input accepts.
    tab.type_str("0-1=").unwrap();
    let value = eval(tab, &format!("document.getElementById({input:?}).value"))
        .as_str()
        .unwrap_or("")
        .to_string();
    for ch in ['0', '-', '1', '='] {
        assert!(
            value.contains(ch),
            "{ch:?} typed into the {surface:?} search box reached it (got {value:?}) — the image \
             viewer's document key handler must yield to a focused text field (#268)"
        );
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_text_fields_keep_the_zoom_keys() {
    let _serial = serial();
    let fx = fixture("scenario-zoomkeys-classic", 30);
    let page = open(Surface::Classic, &fx, 0);
    scenario_text_fields_keep_the_zoom_keys(&page.tab, Surface::Classic);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_text_fields_keep_the_zoom_keys() {
    let _serial = serial();
    let fx = fixture("scenario-zoomkeys-app", 30);
    let page = open(Surface::AppShell, &fx, 2916);
    scenario_text_fields_keep_the_zoom_keys(&page.tab, Surface::AppShell);
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
    // A step is measured by the spy — the turn the bar and the pane name — which moves by exactly
    // one; the viewport's first header stays within a few turns (no leap). The two are not the
    // same reading (#199): the spy names the turn the reader is INSIDE, while `turn_at_top` reads
    // the first header that starts near the top edge, and after real paging the next header can
    // sit just below the spy's line — `]` then lands THAT header under the bar, a short forward
    // hop the first-header reading cannot see. (Space pages only on the app shell here; the
    // classic page scrolls natively on Space, which a synthetic key does not drive.)
    let named = |what: &str| -> i64 {
        // Both readings must come from the SAME resting view. Paging leaves the engine still
        // placing, and a view that has stopped moving is not one that has finished (#227), so
        // without this the pane and the bar are sampled at two different moments and disagree
        // by a turn or two — measured 3 failures in 6 runs, always as a small off-by-N.
        until_engine_quiet(tab);
        let pane = harness::pane_focus_turn(tab, surface);
        let bar = harness::sticky_turn(tab, surface)
            .map(|b| b.0)
            .unwrap_or(-1);
        assert_eq!(
            pane, bar,
            "{what}: the pane and the bar agree on the turn being read"
        );
        pane
    };
    let at = named("after paging");
    key(tab, "]", false);
    settle();
    let next = turn_at_top(tab, surface);
    let stepped = named("after `]`");
    assert!(
        next >= paged && next <= paged + 3,
        "`]` stays near where it was: {paged} -> {next}"
    );
    assert_eq!(
        stepped,
        at + 1,
        "`]` steps to the next turn: the spy named {at}, now {stepped}"
    );
    key(tab, "[", false);
    settle();
    let back = turn_at_top(tab, surface);
    let stepped_back = named("after `[`");
    assert!(
        back <= next && back + 3 >= next,
        "`[` stays near where it was: {next} -> {back}"
    );
    assert_eq!(
        stepped_back,
        stepped - 1,
        "`[` steps back one turn: the spy named {stepped}, now {stepped_back}"
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
/// The image fixture with a REAL screenshot's payload (360×360) instead of the 1×1.
///
/// Zoom is only observable on an image that can outgrow its stage: the 1×1 sits at 100% fit and
/// is still 8px wide at the 800% ceiling, so "did it become pannable" has no answer there. This
/// is the same session shape, with bytes that have a size.
/// The same session with a WIDE, short image — the shape the owner's screenshot has. Centring is
/// decided per axis, so an image that fits on one axis and overflows the other is exactly where a
/// centring fault hides; every image fixture before this was square.
fn wide_image_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question 12: read the screenshot", &now_minus(40));
    transcript += &assistant_at("answer 12a: let me look at it", &now_minus(39));
    transcript += &harness::tool_open_at("t-pre", &now_minus(38));
    transcript += &harness::tool_result_at("t-pre", &now_minus(37));
    transcript += &read_tool_at("t-img", "/tmp/wide.png", &now_minus(36));
    transcript += &harness::image_result_sized("t-img", &now_minus(32), harness::WIDE_PNG_B64);
    transcript += &assistant_at("answer 12: the screenshot shows the deck", &now_minus(28));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

fn big_image_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question 12: read the screenshot", &now_minus(40));
    transcript += &assistant_at("answer 12a: let me look at it", &now_minus(39));
    transcript += &harness::tool_open_at("t-pre", &now_minus(38));
    transcript += &harness::tool_result_at("t-pre", &now_minus(37));
    transcript += &read_tool_at("t-img", "/tmp/shot.png", &now_minus(36));
    transcript += &harness::image_result_sized("t-img", &now_minus(32), harness::BIG_PNG_B64);
    transcript += &assistant_at("answer 12: the screenshot shows the deck", &now_minus(28));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

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

/// The reader's anchor once the page is done moving: the engine idle, then two reads a beat
/// apart that agree.
///
/// The engine's own books come FIRST and do the real work; the two agreeing samples are only a
/// backstop for paint landing after the last transaction. Stillness alone is not enough — see
/// `until_engine_quiet` for the plateau that fooled two rounds of this — and neither half reads
/// where the view IS, so a broken engine settles in the wrong place and the caller's assertion
/// still fails. Used for the baseline as well as the result, because a case that measures a
/// change has to start from a page that has stopped.
fn until_anchor_at_rest(tab: &headless_chrome::Tab, surface: Surface) -> (i64, f64) {
    until_engine_quiet(tab);
    let t0 = std::time::Instant::now();
    let mut last = view_anchor_index(tab, surface);
    while t0.elapsed() < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_millis(250));
        let now = view_anchor_index(tab, surface);
        if now.0 == last.0 && (now.1 - last.1).abs() <= 0.5 {
            return now;
        }
        last = now;
    }
    last
}

/// Wait until the engine has no queued work AND has stopped transacting.
///
/// A PAUSE is not the end (#227). The engine answers a height change in two movements: the
/// layout reaction at once, and then the estimator, which by design takes a new estimate only
/// once the reader is at rest (#194) — so between them the view sits still on a plateau that
/// two samples a quarter-second apart happily agree about. Measured: a case that stopped on
/// that plateau read the top record as 282 and called the view moved, while the reader's own
/// record had not budged from -12 and landed back at -12 a moment later. What separates a
/// plateau from the end is the engine's own books: `pending` names work it still owes, and a
/// states count that stops growing means it has stopped working. Neither reads the view's
/// POSITION, so a broken engine still comes to rest in the wrong place and the caller's
/// assertion still fails.
/// Wait for a COMMANDED move to finish: first for it to START, then for it to stop.
///
/// `until_engine_quiet` is not enough for a click or a jump. A commanded move may be smooth, and
/// a fixed sleep — or a quiet-check taken too early — can both elapse before the browser has
/// begun animating, at which point the scroller is still sitting at its old offset and reads as
/// "at rest". That is the trap the memory note names: two equal samples miss a transition that
/// has not started. So this takes the offset BEFORE the command, waits for it to move off that
/// value, and only then waits for it to settle. A move that never starts (the command was a
/// no-op because the target was already in view) falls through the start-wait on its deadline
/// and the settle-wait then returns immediately, so it is safe to call unconditionally.
fn until_move_settles(tab: &headless_chrome::Tab, surface: Surface, was: f64) {
    let read = format!("{}.scrollTop", surface.scroller());
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        let now = eval(tab, &read).as_f64().unwrap_or(was);
        if (now - was).abs() > 1.0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    let mut last = f64::NAN;
    let t1 = std::time::Instant::now();
    while t1.elapsed() < Duration::from_secs(10) {
        let now = eval(tab, &read).as_f64().unwrap_or(f64::NAN);
        if now == last {
            break;
        }
        last = now;
        std::thread::sleep(Duration::from_millis(120));
    }
    until_engine_quiet(tab);
}

/// The scroller's current offset — the `was` a [`until_move_settles`] call is measured against.
fn scroll_now(tab: &headless_chrome::Tab, surface: Surface) -> f64 {
    eval(tab, &format!("{}.scrollTop", surface.scroller()))
        .as_f64()
        .unwrap_or(f64::NAN)
}

fn until_engine_quiet(tab: &headless_chrome::Tab) {
    let count = "(function(){ var h = window.__viewportHistory; if (!h || !h.states || !h.states.length) return -1; var s = h.states[h.states.length - 1]; return s.pending ? -1 : h.states.length; })()";
    let t0 = std::time::Instant::now();
    let mut last = -1i64;
    while t0.elapsed() < Duration::from_secs(15) {
        let now = eval(tab, count).as_i64().unwrap_or(-1);
        if now > 0 && now == last {
            return;
        }
        last = now;
        std::thread::sleep(Duration::from_millis(400));
    }
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
    // Read the BEFORE only once the page has stopped moving (#227). Two rounds of evidence put
    // the fragility here rather than after the growth: repairing only the trailing wait moved
    // nothing (2 of 8, 1 of 8), and the `before` of one failing run was the `after` of the
    // previous one to the pixel — the setup's jump, expand and 1800px scroll were still
    // converging, the growth landed on a moving page, and the case blamed the growth for the
    // motion it inherited. Follow first, because until the engine hands the view to the reader
    // it places at the tail and is right to.
    until_reader_owns_the_view(tab);
    let before = until_anchor_at_rest(tab, surface);
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
    let scroller = surface.scroller();
    let tall = format!("(function(){{ var s = {scroller}; return s ? s.scrollHeight : 0; }})()");
    let before_height = eval(tab, &tall).as_f64().unwrap_or(0.0);
    let before_probe = if std::env::var("CR_DUMP").is_ok() {
        let p = match surface {
            Surface::Classic => format!("(function(){{ var e=document.querySelector('#stream [data-idx=\"{}\"]'); var h=window.__viewportHistory, s=h&&h.states[h.states.length-1]; return JSON.stringify({{readerRecordTop: e?Math.round(e.getBoundingClientRect().top):null, scrollTop: Math.round(document.scrollingElement.scrollTop), anchor: s&&s.position, belief: s&&Math.round(s.top), pads: s&&s.pads, est: s&&s.estimate}}); }})()", before.0),
            Surface::AppShell => format!("(function(){{ var t=document.querySelector('.transcript'); var e=document.querySelector('.virtual-window [data-block-index=\"{}\"]'); var h=window.__viewportHistory, s=h&&h.states[h.states.length-1]; return JSON.stringify({{readerRecordTop: e?Math.round(e.getBoundingClientRect().top-t.getBoundingClientRect().top):null, scrollTop: Math.round(t.scrollTop), anchor: s&&s.position, belief: s&&Math.round(s.top), pads: s&&s.pads, est: s&&s.estimate}}); }})()", before.0),
        };
        eval(tab, &p).to_string()
    } else {
        String::new()
    };
    assert_eq!(
        eval(tab, &grow),
        "grown",
        "the record four above the reader is mounted"
    );
    // Three things have to happen before the view can be read: the 300px reaches layout, the
    // engine is told, and its correction is written. A fixed sleep guessed at all three at once
    // and on a loaded machine guessed wrong — 2 of 6 and 3 of 6 here, while CI's run of this very
    // case was green (#227). The two waits below are ADDITIVE, and neither can be satisfied by
    // the view being RIGHT: the first says the stimulus landed, the second says the engine
    // stopped moving. Where it came to rest is the assertion's business, below.
    until(
        tab,
        &format!(
            "(function(){{ var s = {scroller}; return !!s && s.scrollHeight >= {}; }})()",
            before_height + 250.0
        ),
        "the 300px growth to reach layout",
        Duration::from_secs(10),
        &tall,
    );
    let after = until_anchor_at_rest(tab, surface);
    let dump = if std::env::var("CR_DUMP").is_ok() {
        // The one question the geometry above cannot answer: did the record the reader was ON
        // move, or did it hold while "the first visible record" picked a different element?
        let probe = match surface {
            Surface::Classic => format!("(function(){{ var e=document.querySelector('#stream [data-idx=\"{}\"]'); var g=document.querySelector('#stream [data-idx=\"{target}\"]'); var h=window.__viewportHistory, s=h&&h.states[h.states.length-1]; return JSON.stringify({{readerRecordTop: e?Math.round(e.getBoundingClientRect().top):null, grownTop: g?Math.round(g.getBoundingClientRect().top):null, grownH: g?Math.round(g.getBoundingClientRect().height):null, scrollTop: Math.round(document.scrollingElement.scrollTop), anchor: s&&s.position, belief: s&&Math.round(s.top), pads: s&&s.pads, est: s&&s.estimate}}); }})()", before.0),
            Surface::AppShell => format!("(function(){{ var t=document.querySelector('.transcript'); var e=document.querySelector('.virtual-window [data-block-index=\"{}\"]'); var g=document.querySelector('.virtual-window [data-block-index=\"{target}\"]'); var h=window.__viewportHistory, s=h&&h.states[h.states.length-1]; return JSON.stringify({{readerRecordTop: e?Math.round(e.getBoundingClientRect().top-t.getBoundingClientRect().top):null, grownTop: g?Math.round(g.getBoundingClientRect().top-t.getBoundingClientRect().top):null, grownH: g?Math.round(g.getBoundingClientRect().height):null, scrollTop: Math.round(t.scrollTop), anchor: s&&s.position, belief: s&&Math.round(s.top), pads: s&&s.pads, est: s&&s.estimate}}); }})()", before.0),
        };
        eval(tab, &probe).to_string()
    } else {
        String::new()
    };
    assert!(after.0 == before.0 && (after.1 - before.1).abs() <= 4.0, "the view held through a 300px growth above it in the same turn: before {before:?}, after {after:?}\n  BEFORE {before_probe}\n  AFTER  {dump}");
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
        // The fixtures are stamped in the builders' fixed past hour, so both sessions are Idle —
        // out of the shell's default filter (#202). This half reads the TREE, so it asks for
        // every bucket first; the reload the helper does is where the shell picks the set up.
        harness::show_every_session(tab, &tab.get_url());
        until(
            tab,
            "document.querySelectorAll('.tree-row.session').length >= 2",
            "both sessions in the tree",
            Duration::from_secs(20),
            "document.querySelectorAll('.tree-row.session').length",
        );
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
    // #217, the owner: "hide tasks on the tasks pane that says no title recorded in this session.
    // It is pointless to show those tasks." That is about the app shell's OUTLINE pane, which sits
    // beside the transcript and is SCANNED; the classic page's task panel is the board itself, and
    // a board that silently dropped a task would be lying about what the session holds. So the two
    // surfaces answer differently here, deliberately, and each says which: the pane lists nothing
    // and reports the count, the panel lists the row and opens the card that explains the absence
    // (#187/#188).
    if surface == Surface::AppShell {
        let pane = probe(
            tab,
            "(function(){ var w = document.getElementById('navigatorWork'); if (!w) return null; var titles = [...w.querySelectorAll('.work-task strong')].map(function (e) { return e.textContent.trim(); }); return { rows: titles.length, untitled: titles.filter(function (t) { return /no title recorded/.test(t); }).length, empty: (w.querySelector('.activity-empty') || {}).textContent || '' }; })()",
        );
        assert_eq!(
            pane["untitled"], 0,
            "the outline pane does not list a task the session recorded no title for: {pane}"
        );
        assert_eq!(
            pane["rows"], 0,
            "…and this fixture's only task is that one, so the pane has nothing to list: {pane}"
        );
        let empty = pane["empty"].as_str().unwrap_or("");
        assert!(
            empty.contains("no recorded title"),
            "…and it says what it is holding back rather than looking like missing data: {pane}"
        );
        return;
    }
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

/// A LONG session whose tail turn is a process surface with many events (#190). Three shapes
/// were tried before this one produced a "Show N more" at all: thinking + tool pairs collapse
/// into one `act`, bare consecutive tool calls fold into one activity record, and only assistant
/// COMMENTARY carrying a tool_use keeps each event its own view (view-model.js groups them and
/// flushes on an assistant record only when `phase !== "commentary"`). The classic page has no
/// process surface; its equivalent for this case is a fold head in the same tail turn.
fn fixture_process_tail(name: &str, turns: usize, events: usize) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = String::new();
    for i in 0..turns {
        t += &user_at(
            &format!("question {i}: a prompt with enough words to be real"),
            &now_minus(90000 - i as u64 * 40),
        );
        let lines = [1usize, 40, 5, 18][i % 4];
        t += &assistant_at(
            &format!(
                "answer {i}: {}",
                "a paragraph of prose to make a line of real height. ".repeat(lines)
            ),
            &now_minus(89990 - i as u64 * 40),
        );
        if i % 3 == 0 {
            let id = format!("p{i}");
            t += &tool_open_at(&id, &now_minus(89985 - i as u64 * 40));
            t += &tool_result_lines(
                &id,
                [4usize, 90, 12, 40][i % 4],
                &now_minus(89980 - i as u64 * 40),
            );
        }
    }
    t += &user_at("question last: work through all of it", &now_minus(600));
    for k in 0..events {
        let id = format!("tail-{k}");
        let ts = now_minus(590 - k as u64 * 3);
        t += &format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Step {k}: checking the next thing before moving on.\"}},{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo step {k}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n"
        );
        t += &tool_result_lines(&id, 30 + (k % 7) * 20, &now_minus(589 - k as u64 * 3));
    }
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: turns as u32 + 1,
    }
}

/// #190. A control the reader CLICKS at the tail must not strand them in blank space.
///
/// The owner's report: pinned at turn 1204, click "Show 8 more" on the process surface, and the
/// view goes blank — the sticky header walks 1202, 1181, nothing, 1182 over several seconds with
/// no further input. Measured on their own monitor: the top pad shrank by 14,243px as the
/// re-render re-measured records and moved the estimate, scrollTop moved +828, and the last
/// mounted record's bottom sat 13,195px ABOVE the viewport. Even "Show 2 more" does it.
///
/// THE CAUSE WAS A RULE MEANT FOR SCROLLING, applied to a click. `noteIntent` binds pointerdown, so
/// a click starts the `userIntentMs` window in which `readerOwnsPosition()` is true; the
/// correction `restoreDomAnchor` would have written was deferred as `owed` (#132 step 3) and
/// `scheduleSettle` DROPPED it (#138: never replay an old position after the reader moves) — all
/// three deleted by #196 stage 2, which places a change that has moved the DOM at once and lets
/// the intent window govern only the tail. It looked right for a scroll. But a click on a control
/// moves nothing — no scroll event fires — and what is
/// withheld is the engine's own re-measure, which #180 already named on the scroll path: "Not
/// writing does not leave the reader alone — it displaces them by exactly the correction being
/// withheld." At a 1200-turn scale that correction is fourteen thousand pixels.
///
/// WHY NO CASE CAUGHT IT: a synthetic `element.click()` fires `click` and nothing else, so no
/// probe ever registered intent and the correction always landed at once. Ten fixture shapes,
/// four real sessions and the owner's own monitor all held for exactly that reason. This case
/// dispatches the `pointerdown` a finger does.
fn scenario_a_clicked_control_at_the_tail_does_not_strand_the_reader(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // A reader who has BEEN in the session: the height guesses are live. #184's per-type mean
    // stays on its floor until eight samples have been measured, and on the floor the click's
    // re-measure moves no estimate, so no pad moves and nothing is ever owed — a fresh open
    // pinned straight at the tail is green on the old engine for that reason alone (#193).
    // Six screens up and back mounts the dozens of units the owner's session had measured.
    for _ in 0..6 {
        scroll_by(tab, surface, -2500);
        settle();
    }
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the return to the tail");
    settle();
    settle();
    let follow = match surface {
        Surface::Classic => "(function(){ var s = document.scrollingElement; return { following: document.body.classList.contains('following'), gap: Math.round(s.scrollHeight - innerHeight - s.scrollTop) }; })()",
        Surface::AppShell => "(function(){ var j = document.getElementById('jumpToBottom'); var s = document.querySelector('.transcript'); return { following: j ? j.getAttribute('aria-hidden') === 'true' : null, gap: Math.round(s.scrollHeight - s.clientHeight - s.scrollTop) }; })()",
    };
    for _ in 0..10 {
        let f = probe(tab, follow);
        if f["following"].as_bool() == Some(true) && f["gap"].as_f64().unwrap_or(99.0) < 3.0 {
            break;
        }
        jump_to_end(tab, surface);
        settle();
    }
    let pinned = probe(tab, follow);
    assert_eq!(
        pinned["following"].as_bool(),
        Some(true),
        "{surface:?}: the reader is PINNED before the click — the report begins at the tail: {pinned}"
    );
    // What the reader can see: the record at the middle of the viewport, and whether anything is
    // there at all. `elementFromPoint` at three heights — a rect is not visibility (#98).
    let state = match surface {
        Surface::Classic => "(function(){ var s = document.scrollingElement; var at = function (f) { var e = document.elementFromPoint(Math.round(innerWidth/2), Math.round(innerHeight*f)); if (!e) return 'NOTHING'; var b = e.closest('#stream .blk'); if (b) return 'rec:' + (b.dataset.turn || b.id); return 'BLANK:' + (e.id || e.className || e.tagName); }; var kids = [...document.querySelectorAll('#stream .blk')]; var handles = [0.3, 0.5, 0.7].map(function (f) { var e = document.elementFromPoint(Math.round(innerWidth/2), Math.round(innerHeight*f)); var t = e ? (e.textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 80) : ''; return { f: f, text: t, top: e ? Math.round(e.getBoundingClientRect().top) : null }; }); return { top: Math.round(s.scrollTop), h: Math.round(s.scrollHeight), at05: at(0.05), at50: at(0.5), at95: at(0.95), mounted: kids.length, handles: handles }; })()",
        Surface::AppShell => "(function(){ var s = document.querySelector('.transcript'); var r = s.getBoundingClientRect(); var at = function (f) { var e = document.elementFromPoint(Math.round(r.left + r.width/2), Math.round(r.top + r.height*f)); if (!e) return 'NOTHING'; var t = e.closest('[data-turn]') || e.closest('[data-record-id]'); if (t) return 'rec:' + (t.dataset.turn || t.dataset.recordId); return 'BLANK:' + (e.id || String(e.className).split(' ')[0] || e.tagName); }; var kids = [...document.querySelector('.virtual-window').children]; var last = kids[kids.length-1]; var handles = [0.3, 0.5, 0.7].map(function (f) { var e = document.elementFromPoint(Math.round(r.left + r.width/2), Math.round(r.top + r.height*f)); var t = e ? (e.textContent || '').trim().replace(/\\s+/g, ' ').slice(0, 80) : ''; return { f: f, text: t, top: e ? Math.round(e.getBoundingClientRect().top) : null }; }); return { top: Math.round(s.scrollTop), h: Math.round(s.scrollHeight), at05: at(0.05), at50: at(0.5), at95: at(0.95), mounted: kids.length, lastBottom: last ? Math.round(last.getBoundingClientRect().bottom - r.top) : -1, handles: handles }; })()",
    };
    let before = probe(tab, state);
    // LIVE, as the owner's session was: records arriving make the pull loop re-range the window
    // after the click, and it is THAT correction — mounting records above the reader whose
    // estimate was wrong (#180) — that the intent window swallows. On a static mock the shell's
    // range never moves and nothing is ever owed; measured, and the case was green for the
    // wrong reason.
    let mut script: Vec<String> = Vec::new();
    for k in 0..12u64 {
        let id = format!("live-{k}");
        script.push(format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"Live step {k}: still working through it.\"}},{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo live {k}\"}}}}]}},\"timestamp\":\"{}\"}}\n",
            now_minus(120 - k * 2)
        ));
        script.push(tool_result_lines(
            &id,
            40 + (k as usize % 5) * 25,
            &now_minus(119 - k * 2),
        ));
    }
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(300));
    // The click A FINGER MAKES: a pointerdown, then the click. The pointerdown is the whole case.
    let clicked = eval(
        tab,
        match surface {
            Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-open=\"0\"]')].filter(function (x) { var hh = x.querySelector(':scope > .fold-h'); if (!hh) return false; var r = hh.getBoundingClientRect(); return r.height > 0 && r.top >= 96 && r.bottom <= innerHeight; }).pop(); if (!f) return 'none'; var h = f.querySelector(':scope > .fold-h'); if (!h) return 'none'; h.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true, pointerType: 'mouse', buttons: 1 })); h.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, cancelable: true, pointerType: 'mouse' })); h.click(); return 'fold ' + f.id; })()",
            Surface::AppShell => "(function(){ var b = [...document.querySelectorAll('.virtual-window [data-process-more]')].map(function (x) { var m = /Show (\\d+) more/.exec(x.textContent || ''); return { el: x, n: m ? Number(m[1]) : 0 }; }).sort(function (p, q) { return q.n - p.n; })[0]; if (!b || !b.el) return 'none'; var l = b.el.textContent.trim().slice(0, 18); b.el.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true, pointerType: 'mouse', buttons: 1 })); b.el.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, cancelable: true, pointerType: 'mouse' })); b.el.click(); return l; })()",
        },
    );
    assert_ne!(
        clicked.as_str().unwrap_or("none"),
        "none",
        "{surface:?}: the fixture offers a control to click at the tail"
    );
    // Give the settle its whole window and then some: the drop happens when the intent timer
    // fires, `userIntentMs` after the click, and the owner's walk took several seconds.
    std::thread::sleep(Duration::from_millis(1500));
    let after = probe(tab, state);
    println!("STRAND {surface:?} clicked {clicked}\n  before {before}\n  after  {after}");
    // Stranded means a probe lands in a PAD — the space the engine holds for records it has not
    // mounted, which is what the owner saw as blank. The sticky bar, the "new" badge and the
    // page's end padding are chrome, and a probe that lands on them says nothing either way.
    let in_pad = |v: &serde_json::Value| {
        ["at05", "at50", "at95"]
            .iter()
            .filter(|k| {
                v[*k]
                    .as_str()
                    .map(|s| s.starts_with("BLANK:vpad") || s.starts_with("BLANK:virtual-pad"))
                    .unwrap_or(false)
            })
            .count()
    };
    assert_eq!(
        in_pad(&after),
        0,
        "{surface:?}: the reader is stranded in BLANK space (a pad) after clicking `{}` — the \
         re-render's correction was deferred as reader intent and then dropped, so the content \
         moved and scrollTop did not (#190).\n  before: {before}\n  after:  {after}",
        clicked.as_str().unwrap_or("")
    );
    // …and what was under the reader's eye is still there, at the same height: the text at 30,
    // 50 and 70% of the viewport before the click is the text there after it, within 2px. On
    // the classic page the rows at the tail are 32px tall, on the app shell a whole turn is one
    // `data-turn`; text is the one measure that is exact on both.
    for (b, a) in before["handles"]
        .as_array()
        .unwrap()
        .iter()
        .zip(after["handles"].as_array().unwrap())
    {
        if b["text"].as_str().unwrap_or("").is_empty() {
            continue;
        }
        assert_eq!(
            a["text"],
            b["text"],
            "{surface:?}: the text at {} of the viewport changed after clicking `{}` — the reader \
             was displaced (#190).\n  before: {before}\n  after:  {after}",
            b["f"],
            clicked.as_str().unwrap_or("")
        );
        let drift = (a["top"].as_f64().unwrap_or(1e9) - b["top"].as_f64().unwrap_or(0.0)).abs();
        assert!(
            drift <= 2.0,
            "{surface:?}: the text at {} of the viewport moved {drift}px after clicking `{}` \
             (#190).\n  before: {before}\n  after:  {after}",
            b["f"],
            clicked.as_str().unwrap_or("")
        );
    }
    assert_eq!(
        after["at50"], before["at50"],
        "{surface:?}: …and the record at the middle of the viewport is the one they were reading: \
         before {before}, after {after}"
    );
    drop(growth);
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_clicked_control_at_the_tail_does_not_strand_the_reader() {
    let _serial = serial();
    let fx = fixture_process_tail("scenario-strand-classic", 1200, 22);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_clicked_control_at_the_tail_does_not_strand_the_reader(
        &page.tab,
        Surface::Classic,
        &fx,
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_clicked_control_at_the_tail_does_not_strand_the_reader() {
    let _serial = serial();
    let fx = fixture_process_tail("scenario-strand-app", 1200, 22);
    let page = open(Surface::AppShell, &fx, 2948);
    scenario_a_clicked_control_at_the_tail_does_not_strand_the_reader(
        &page.tab,
        Surface::AppShell,
        &fx,
    );
}

/// #191. A long jump into unmeasured ground lands the reader on content, not in a pad.
///
/// Found by the #190 precaution audit. A wheel jump from the tail — −40,000px, and on the classic
/// page even −6,000 — left every probe in a PAD at rest, on both surfaces, until the reader scrolled
/// again: the mounted window sat 1,944px below the viewport on the classic page and 6,709px on the
/// shell. `updateWindow` captures its anchor before the mount; after a jump that size no old item is
/// visible, the anchor is null, and when the measure moves the `HeightGuess` mean the top pad grows
/// by thousands of pixels under a `scrollTop` nobody corrects. The engine now holds the MODEL
/// position — the record the sums named, and how far into it — through the mount (#191).
///
/// The probes are pad-aware (#190): "stranded" is a probe in `.vpad` / `.virtual-pad`; the gaps
/// between turns and the page's chrome are not blank space.
fn scenario_a_long_jump_lands_on_content(tab: &headless_chrome::Tab, surface: Surface, dy: i64) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    settle();
    let state = match surface {
        Surface::Classic => {
            r#"(function(){ var s = document.scrollingElement; var at = function (f) { var e = document.elementFromPoint(Math.round(innerWidth/2), Math.round(innerHeight*f)); if (!e) return 'NOTHING'; var b = e.closest('#stream .blk'); if (b) return 'rec:' + (b.dataset.turn || b.id); return 'BLANK:' + (e.id || String(e.className).split(' ')[0] || e.tagName); }; var units = [...document.querySelectorAll('[data-unit-index]')]; var w = units.length ? units[0].parentElement.getBoundingClientRect() : null; return { top: Math.round(s.scrollTop), h: Math.round(s.scrollHeight), at20: at(0.2), at50: at(0.5), at80: at(0.8), units: units.length, lo: units.length ? units[0].dataset.unitIndex : null, winTop: w ? Math.round(w.top) : null, winBottom: w ? Math.round(w.bottom) : null }; })()"#
        }
        Surface::AppShell => {
            r#"(function(){ var s = document.querySelector('.transcript'); var r = s.getBoundingClientRect(); var at = function (f) { var e = document.elementFromPoint(Math.round(r.left + r.width/2), Math.round(r.top + r.height*f)); if (!e) return 'NOTHING'; var t = e.closest('[data-record-id]') || e.closest('[data-turn]'); if (t) return 'rec:' + (t.dataset.recordId || t.dataset.turn); return 'BLANK:' + (e.id || String(e.className).split(' ')[0] || e.tagName); }; var win = document.querySelector('.virtual-window'); var units = [...win.children]; var w = win.getBoundingClientRect(); return { top: Math.round(s.scrollTop), h: Math.round(s.scrollHeight), at20: at(0.2), at50: at(0.5), at80: at(0.8), units: units.length, lo: units.length ? units[0].dataset.unitIndex : null, winTop: Math.round(w.top - r.top), winBottom: Math.round(w.bottom - r.top) }; })()"#
        }
    };
    let tail = probe(tab, state);
    let before = scroll_now(tab, surface);
    scroll_by(tab, surface, dy);
    settle();
    settle();
    // A jump is a COMMANDED move and may animate, so two fixed sleeps can elapse before the
    // browser has begun — leaving the scroller at its old offset, reading as "at rest" (#247).
    // Without this the pair below compares "two settles" against "four settles" and a move
    // still in flight is indistinguishable from the engine moving the reader; observed failing
    // once as `rested.top != landed.top`. Waiting first makes the claim the one it states:
    // once the reader is AT REST, nothing moves them.
    until_move_settles(tab, surface, before);
    let landed = probe(tab, state);
    settle();
    settle();
    let rested = probe(tab, state);
    println!("JUMP {surface:?} {dy}px\n  tail   {tail}\n  landed {landed}\n  rested {rested}");
    let count = |v: &serde_json::Value, pred: &dyn Fn(&str) -> bool| {
        ["at20", "at50", "at80"]
            .iter()
            .filter(|k| v[*k].as_str().map(pred).unwrap_or(false))
            .count()
    };
    let in_pad = |s: &str| s.starts_with("BLANK:vpad") || s.starts_with("BLANK:virtual-pad");
    let on_content = |s: &str| s.starts_with("rec:");
    assert_eq!(
        count(&landed, &in_pad),
        0,
        "{surface:?}: a {dy}px jump left the reader in a PAD — the mount's measure moved the \
         estimates under a scroll offset nothing corrected (#191): {landed}"
    );
    assert_eq!(
        count(&rested, &in_pad),
        0,
        "{surface:?}: …and they are still in a pad at rest: {rested}"
    );
    assert!(
        count(&rested, &on_content) >= 2,
        "{surface:?}: the viewport shows content at rest after a {dy}px jump: {rested}"
    );
    assert_eq!(
        rested["top"], landed["top"],
        "{surface:?}: nothing moves the reader once they have come to rest: {landed} → {rested}"
    );
    // Up the session and not off it: the first mounted unit is earlier than it was at the tail
    // and still past the start. (Scroll offsets cannot say this — the mount's measure moves the
    // whole page's height, and a −6000 jump legitimately lands at a LARGER offset than the tail's.)
    let lo = |v: &serde_json::Value| {
        v["lo"]
            .as_str()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(-1)
    };
    assert!(
        lo(&rested) > 0 && lo(&rested) < lo(&tail),
        "{surface:?}: the jump moved the reader up the session and not off it: mounted from {} \
         (the tail mounted from {})",
        lo(&rested),
        lo(&tail)
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_short_jump_lands_on_content() {
    let _serial = serial();
    let fx = fixture_process_tail("scenario-jump-short-classic", 400, 22);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_long_jump_lands_on_content(&page.tab, Surface::Classic, -6000);
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_long_jump_lands_on_content() {
    let _serial = serial();
    let fx = fixture_process_tail("scenario-jump-long-classic", 400, 22);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_long_jump_lands_on_content(&page.tab, Surface::Classic, -40000);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_short_jump_lands_on_content() {
    let _serial = serial();
    let fx = fixture_process_tail("scenario-jump-short-app", 400, 22);
    let page = open(Surface::AppShell, &fx, 2949);
    scenario_a_long_jump_lands_on_content(&page.tab, Surface::AppShell, -6000);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_long_jump_lands_on_content() {
    let _serial = serial();
    let fx = fixture_process_tail("scenario-jump-long-app", 400, 22);
    let page = open(Surface::AppShell, &fx, 2950);
    scenario_a_long_jump_lands_on_content(&page.tab, Surface::AppShell, -40000);
}

/// #192. The viewport trace: off by default, on by `?trace=viewport`, and what it records is what
/// the engine did — every reconcile, scroll verdict and anchor decision, with the geometry it saw.
///
/// The owner asked for this after #190 took five screenshots, a live session and a probe harness
/// to reproduce: the page kept no record of what the engine had done. Now it can say.
fn scenario_the_trace_records_what_the_engine_did(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // Off by default: no buffer, nothing recorded — the flag costs one boolean.
    assert_eq!(
        eval(tab, "typeof window.__viewportTrace"),
        "undefined",
        "{surface:?}: without the flag the page records nothing"
    );
    // On, by the URL, for this load.
    eval(
        tab,
        "location.href = location.href + (location.search ? '&' : '?') + 'trace=viewport'; 'ok'",
    );
    let mounted = match surface {
        Surface::Classic => "document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3",
        Surface::AppShell => "document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3",
    };
    harness::until(
        tab,
        mounted,
        "the page to mount with the trace on",
        Duration::from_secs(30),
        "location.search",
    );
    settle();
    let opened = eval(
        tab,
        "window.__viewportTrace ? window.__viewportTrace.length : -1",
    )
    .as_i64()
    .unwrap_or(-1);
    assert!(
        opened > 0,
        "{surface:?}: with `?trace=viewport` the load itself is recorded (#192): {opened} entries"
    );
    // An entry carries the geometry a bug report needs: where the window was, where the reader
    // was, what the pads held, how long since the reader last touched anything.
    let fields = eval(
        tab,
        "(function(){ var e = window.__viewportTrace.find(function (x) { return x.event === 'reconciled'; }); return e ? Object.keys(e).sort().join(',') : 'none'; })()",
    );
    let fields = fields.as_str().unwrap_or("none").to_string();
    for want in [
        "seq",
        "t",
        "event",
        "following",
        "lo",
        "hi",
        "count",
        "top",
        "height",
        "pads",
        "sinceInput",
        "anchor",
        "fresh",
        "estimate",
    ] {
        assert!(
            fields.split(',').any(|f| f == want),
            "{surface:?}: a reconcile entry carries `{want}`: {fields}"
        );
    }
    // The reader's own scroll is recorded as one: its verdict, and the window update it drove.
    scroll_by(tab, surface, -1200);
    settle();
    let events = eval(
        tab,
        "(function(){ return Array.from(new Set(window.__viewportTrace.map(function (x) { return x.event; }))).sort().join(','); })()",
    );
    let events = events.as_str().unwrap_or("").to_string();
    for want in ["scroll", "update", "reconciled"] {
        assert!(
            events.split(',').any(|e| e == want),
            "{surface:?}: a scroll leaves `{want}` in the trace: {events}"
        );
    }
    let verdict = eval(
        tab,
        "(function(){ var e = window.__viewportTrace.filter(function (x) { return x.event === 'scroll'; }).pop(); return e ? e.user + '/' + e.verdict : 'none'; })()",
    );
    assert!(
        verdict.as_str().unwrap_or("none").starts_with("true/"),
        "{surface:?}: the wheel is classified as the reader's own, and the trace says so: {verdict}"
    );
    // Sequence numbers are monotonic and the buffer is a ring.
    let shape = eval(
        tab,
        "(function(){ var t = window.__viewportTrace; var mono = t.every(function (x, i) { return i === 0 || x.seq > t[i-1].seq; }); return mono + '/' + (t.length <= 500); })()",
    );
    assert_eq!(
        shape.as_str().unwrap_or(""),
        "true/true",
        "{surface:?}: sequence numbers climb and the ring holds at most 500 entries: {shape}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_trace_records_what_the_engine_did() {
    let _serial = serial();
    let fx = fixture("scenario-trace-classic", 30);
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_trace_records_what_the_engine_did(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_trace_records_what_the_engine_did() {
    let _serial = serial();
    let fx = fixture("scenario-trace-app", 30);
    let page = open(Surface::AppShell, &fx, 2947);
    scenario_the_trace_records_what_the_engine_did(&page.tab, Surface::AppShell, &fx);
}

/// A fixture whose tail holds a Bash call whose command runs far past the head's one line.
fn fixture_long_command(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut jsonl = long_session(14, Shape::default());
    let command = "cargo test -p claude-replay-browser-tests --test scenarios -- --ignored --skip known_red app_shell --nocapture 2>&1 | grep -E 'the needle in a very long pipeline that keeps going and going past any reasonable head width' | sed -e 's/one thing/another thing entirely/' -e 's/and yet another substitution/to make quite sure this line cannot fit/' | sort -u | head -20";
    jsonl += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"long-1\",\"name\":\"Bash\",\"input\":{{\"command\":\"{command}\"}}}}]}},\"timestamp\":\"{stamp}\"}}\n",
        stamp = harness::at("15:01")
    );
    jsonl += &tool_result_text("long-1", "one line of output", &harness::at("15:02"));
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
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"mid-1\",\"name\":\"Bash\",\"input\":{{\"command\":\"{command}\"}}}}]}},\"timestamp\":\"{stamp}\"}}\n",
        stamp = harness::at("15:01")
    );
    jsonl += &tool_result_text("mid-1", "one line of output", &harness::at("15:02"));
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
    let _ = probe(
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
            // RE-PINNED by #234, as a decision rather than a drift. The pill used to be built
            // from two fields (`failed · exit 1`) and DISCARDED everything else the chips
            // carried — on this fixture the classic chip is `exit 1 · 2.50s`, so the duration
            // was reaching one page and not the other. The pill is now chip-faithful, which
            // means it gains the duration here and the two pages finally say the same thing.
            // The old claim still holds: it names the failure and its exit.
            assert_eq!(
                (failed["state"].as_str(), failed["pill"].as_str()),
                (Some("failed"), Some("failed · 1 lines · exit 1 · 2.50s")),
                "a failed call's pill names the failure and everything its chips carry — this \
                 fixture's classic head shows a `1 lines` chip AND an `exit 1 · 2.50s` chip, and \
                 the shell now carries both; the word leads because these chips never say it"
            );
            assert_eq!(long["state"].as_str(), Some("completed"));
            assert!(
                long["pill"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with("exit 0 · 1m 5s"),
                "a long call's pill shows its duration: {long:?}"
            );
            // Re-pinned with the above: the classic chip is `declined · 42ms`, and the shell
            // said only `declined`. The failure word is already IN the chips here, so the pill
            // is the chip text unchanged — no word is said twice.
            assert_eq!(
                (declined["state"].as_str(), declined["pill"].as_str()),
                (Some("failed"), Some("declined · 42ms"))
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
    // Driven from INSIDE the page: a harness round trip per notch put a nominal 200ms cadence past
    // the classic page's 300ms window under load (measured: two of three runs alone, the reader
    // "ended 0px from the tail"), and a notch later than the window is, by the rule this case
    // holds, a hand that has paused — the converge it then lets through is the policy, not the
    // bug. In-page timing keeps the cadence the case describes.
    eval(
        tab,
        &format!(
            "(function(){{ var s = {scroller}; var t = {target}; window.__crawl = {{ n: 0, done: false }}; var id = setInterval(function(){{ t.dispatchEvent(new WheelEvent('wheel', {{deltaY: -7, bubbles: true}})); s.scrollTo({{ top: Math.max(0, s.scrollTop - 7), behavior: 'instant' }}); if (++window.__crawl.n >= 25) {{ clearInterval(id); window.__crawl.done = true; }} }}, 200); return 'started'; }})()"
        ),
    );
    harness::until(
        tab,
        "!!(window.__crawl && window.__crawl.done)",
        "the slow crawl to finish its 25 notches",
        Duration::from_secs(20),
        "window.__crawl ? window.__crawl.n : -1",
    );
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
fn grow_above_js(surface: Surface, px: i64) -> String {
    match surface {
        Surface::Classic => format!(
            "(function(){{ var es = [...document.querySelectorAll('#vwin > [data-idx]')]; var e = es.reverse().find(function (x) {{ return x.getBoundingClientRect().bottom <= 0; }}); if (!e) return false; e.style.paddingTop = ((parseFloat(e.style.paddingTop) || 0) + {px}) + 'px'; return true; }})()"
        ),
        Surface::AppShell => format!(
            "(function(){{ var s = document.querySelector('.transcript').getBoundingClientRect(); var es = [...document.querySelectorAll('.virtual-window > [data-unit-index]')]; var e = es.reverse().find(function (x) {{ return x.getBoundingClientRect().bottom <= s.top; }}); if (!e) return false; e.style.paddingTop = ((parseFloat(e.style.paddingTop) || 0) + {px}) + 'px'; return true; }})()"
        ),
    }
}

fn grow_above(tab: &headless_chrome::Tab, surface: Surface, px: i64) -> bool {
    eval(tab, &grow_above_js(surface, px))
        .as_bool()
        .unwrap_or(false)
}

/// Every mounted record root's top (px from the viewport's top), by identity.
fn roots_map(
    tab: &headless_chrome::Tab,
    surface: Surface,
) -> std::collections::HashMap<String, f64> {
    let js = match surface {
        Surface::Classic => "(function(){ return [...document.querySelectorAll('#vwin > [data-idx]')].map(function (x) { return [x.id, x.getBoundingClientRect().top]; }); })()",
        Surface::AppShell => "(function(){ var vt = document.querySelector('.transcript').getBoundingClientRect().top; return [...document.querySelectorAll('.virtual-window > [data-unit-key]')].map(function (x) { return [x.dataset.unitKey, x.getBoundingClientRect().top - vt]; }); })()",
    };
    let mut out = std::collections::HashMap::new();
    if let Some(rows) = harness::probe(tab, js).as_array() {
        for row in rows {
            if let (Some(key), Some(top)) = (row[0].as_str(), row[1].as_f64()) {
                out.insert(key.to_string(), top);
            }
        }
    }
    out
}

/// For a failing step: the mounted record roots around the viewport's top — identity, top and
/// bottom (px from the viewport's top) — so a growth can be placed against what it moved.
fn roots_near_top(tab: &headless_chrome::Tab, surface: Surface) -> String {
    let js = match surface {
        Surface::Classic => "(function(){ return [...document.querySelectorAll('#vwin > [data-idx]')].map(function (x) { var r = x.getBoundingClientRect(); return [x.id, Math.round(r.top), Math.round(r.bottom)]; }).filter(function (t) { return t[2] > -900 && t[1] < 900; }); })()",
        Surface::AppShell => "(function(){ var vt = document.querySelector('.transcript').getBoundingClientRect().top; return [...document.querySelectorAll('.virtual-window > [data-unit-key]')].map(function (x) { var r = x.getBoundingClientRect(); return [x.dataset.unitKey, Math.round(r.top - vt), Math.round(r.bottom - vt)]; }).filter(function (t) { return t[2] > -900 && t[1] < 900; }); })()",
    };
    harness::probe(tab, js).to_string()
}

/// The mounted record root at the top of the viewport — its identity and where its top sits
/// (px below the viewport's top). What a reader can see, to compare across a change: the offset
/// itself is no measure of the reader's position, because an estimate applied above them moves
/// `scrollTop` by the sums' shift while the content under them stays put.
fn root_at_top(tab: &headless_chrome::Tab, surface: Surface) -> (String, f64) {
    let js = match surface {
        Surface::Classic => "(function(){ var k = [...document.querySelectorAll('#vwin > [data-idx]')]; var e = k.find(function (x) { var r = x.getBoundingClientRect(); return r.height > 0 && r.bottom > 1; }); return e ? { key: e.id, top: e.getBoundingClientRect().top } : null; })()",
        Surface::AppShell => "(function(){ var vt = document.querySelector('.transcript').getBoundingClientRect().top; var k = [...document.querySelectorAll('.virtual-window > [data-unit-key]')]; var e = k.find(function (x) { var r = x.getBoundingClientRect(); return r.height > 0 && r.bottom > vt + 1; }); return e ? { key: e.dataset.unitKey, top: e.getBoundingClientRect().top - vt } : null; })()",
    };
    let v = harness::probe(tab, js);
    (
        v["key"].as_str().unwrap_or("").to_string(),
        v["top"].as_f64().unwrap_or(f64::NAN),
    )
}

/// Where the record root `key` sits now (px below the viewport's top), or NaN if it is gone.
fn top_of(tab: &headless_chrome::Tab, surface: Surface, key: &str) -> f64 {
    let js = match surface {
        Surface::Classic => format!(
            "(function(){{ var e = document.getElementById({key:?}); return e ? e.getBoundingClientRect().top : null; }})()"
        ),
        Surface::AppShell => format!(
            "(function(){{ var vt = document.querySelector('.transcript').getBoundingClientRect().top; var e = document.querySelector('[data-unit-key=\"' + {key:?} + '\"]'); return e ? e.getBoundingClientRect().top - vt : null; }})()"
        ),
    };
    eval(tab, &js).as_f64().unwrap_or(f64::NAN)
}

/// `SCENARIO_TRACE=1`: reopen the page with the viewport trace on (#192), so a failing step can
/// print what the engine did (`trace_tail`). Off, nothing changes.
fn trace_on(tab: &headless_chrome::Tab, surface: Surface) {
    if std::env::var_os("SCENARIO_TRACE").is_none() {
        return;
    }
    eval(tab, "(function(){ try { localStorage.viewportTrace = '1'; } catch (e) {} location.reload(); return 'ok'; })()");
    let ready = match surface {
        Surface::Classic => "!!window.__viewportTrace && document.querySelectorAll('#stream .blk').length >= 3",
        Surface::AppShell => "!!window.__viewportTrace && !!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3",
    };
    harness::until(
        tab,
        ready,
        "the page to reopen with the trace on",
        Duration::from_secs(60),
        "location.href",
    );
    settle();
}

/// The always-on history (#197) at a failing step, env-gated like `trace_tail`. The TRACE needs
/// `?trace=viewport` and most cases do not pass it; the HISTORY is always recorded, so this is
/// what an intermittent viewport case can actually dump at the moment it goes wrong.
fn history_tail(tab: &headless_chrome::Tab, label: &str, n: usize) {
    if std::env::var_os("SCENARIO_TRACE").is_none() {
        return;
    }
    let js = format!(
        "(function(){{ var h = window.__viewportHistory || {{}}; var take = function (a) {{ \
           return (a || []).slice(-{n}); }}; \
         return {{ states: take(h.states), deltas: take(h.deltas), actions: take(h.actions), \
                  violations: take(h.violations) }}; }})()"
    );
    eprintln!("HISTORY {label}:");
    let seen = harness::probe(tab, &js);
    for key in ["actions", "deltas", "states", "violations"] {
        if let Some(rows) = seen[key].as_array() {
            eprintln!("  -- {key} ({})", rows.len());
            for r in rows {
                eprintln!("     {r}");
            }
        }
    }
}

fn trace_tail(tab: &headless_chrome::Tab, label: &str, n: usize) {
    if std::env::var_os("SCENARIO_TRACE").is_none() {
        return;
    }
    let js = format!("(window.__viewportTrace || []).slice(-{n})");
    eprintln!("TRACE {label}:");
    if let Some(entries) = harness::probe(tab, &js).as_array() {
        for e in entries {
            eprintln!("  {e}");
        }
    }
}

/// The reader's wheel and a growth above them in the SAME task, so the growth's observer runs
/// before the scroll batch's deferred window update — the ordering a hold released by the offset
/// alone got wrong (framework §4.10).
fn wheel_then_grow_above(tab: &headless_chrome::Tab, surface: Surface, dy: i64, px: i64) -> bool {
    let scroller = match surface {
        Surface::Classic => "document.scrollingElement",
        Surface::AppShell => "document.querySelector('.transcript')",
    };
    let grow = grow_above_js(surface, px);
    let js = format!(
        "(function(){{ var s = {scroller}; s.dispatchEvent(new WheelEvent('wheel', {{deltaY: {dy}, bubbles: true}})); s.scrollTop += {dy}; return {grow}; }})()"
    );
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
/// out, both surfaces failed on the first assertion — "pinned, 320px appeared above the run and
/// the tail is still the tail". The observer watches the BORDER box on purpose: what moves the run
/// is usually the chrome's own padding, and a padding change leaves the content box untouched, so
/// the default box hears nothing at all (that is how the app-shell half of this case failed once
/// the rest was working).
///
/// Measured again on 2026-09-26 (#289), when the waits went in: it is the UNPINNED reader that
/// catches a missing observer every time now. Without it, the app shell puts a pinned reader back
/// at the tail by another path within about 100ms, and the classic page does so in some runs and
/// not in others; the unpinned reader is never put back (turn 59 at the top where 60 was read).
/// So the case was red in all nine runs without the observer — failing in either half — and green
/// in all six with it.
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
    // The correction is delivered by a ResizeObserver, at FRAME time, and a headless tab on a
    // busy machine produces frames lazily (#204): one full run on 2026-09-26 read the tail before
    // it had landed. So the settles stay, a bounded wait follows them — a correction that never
    // comes still fails, with the renderer's verdict — and the page is read once more after a
    // further settle, because the claim is that the reader STAYS where they were.
    settle();
    settle();
    await_tail(
        tab,
        surface,
        &format!(
            "{surface:?}: pinned, 320px appeared above the run and the tail is still the tail"
        ),
    );
    settle();
    assert!(
        at_tail(tab, surface),
        "{surface:?}: pinned, 320px appeared above the run and the tail stayed the tail"
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
    await_turn_at_top(
        tab,
        surface,
        reading,
        &format!("{surface:?}: reading turn {reading}, 320px appeared above the run, and it stayed there"),
    );
    settle();
    assert_eq!(
        turn_at_top(tab, surface),
        reading,
        "{surface:?}: reading turn {reading}, 320px appeared above the run, and it stays there"
    );
}

/// Wait for `turn` to be the turn at the viewport top, or fail naming the turn that is there
/// instead — [`await_tail`]'s counterpart for a reader who is not pinned.
fn await_turn_at_top(tab: &headless_chrome::Tab, surface: Surface, turn: i64, what: &str) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        if turn_at_top(tab, surface) == turn {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "timed out waiting for {what} (top turn {}); {}",
        turn_at_top(tab, surface),
        harness::renderer_verdict(tab)
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
    // is still FOLLOWING, its position is the tail rather than an anchor by design (`positionFor`),
    // and the engine simply
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
///   height. That fires the identical `ResizeObserver -> measureNow -> transact("measure") ->
///   place(P)` path a live delta's growth takes, against the position KEPT from the last
///   transaction (`syncPosition`) — which is the point: a position captured after the growth
///   describes the moved view and corrects nothing.
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
/// The engine computed exactly that correction in `restoreDomAnchor` (`place` since #196) and, before #180, threw it
/// away: `readerOwnsPosition()` is true for the whole gesture (a wheel event stamps `lastUserInput`
/// milliseconds earlier), so the write was deferred into `this.owed` — which nothing ever reads
/// back. Measured on the app shell before the fix: +2355px and +2854px of movement for a 900px
/// request, an overshoot of up to 1954px per step, with `scrollHeight` growing by the same amount.
///
/// So the assertion is the reader's own contract: the record you were looking at moves by the
/// distance you scrolled, and by no more. The case walks up in steps and checks EVERY step, and it
/// separately requires that at least one step actually reached unmeasured ground — otherwise it
/// would pass over the region `convergeBottom` already measured and prove nothing.
///
/// "Reached unmeasured ground" is read two ways, either sufficing: the page's height moved on the
/// step (an estimate replaced by a truth that differed), or the window's lowest mounted index fell
/// below anything mounted since the open (records met for the first time). The height alone
/// stopped being enough with #201: until then the classic page's last mounted record lost a 16px
/// margin at the window's bottom edge, which moved `scrollHeight` on every step and fed this guard
/// whether or not the estimates had anything to learn; without that flip, a mounted run whose
/// heights average out against the learned mean replaces its estimates with a net change under a
/// pixel, and the guard called a walk over 8,000px of never-mounted records vacuous.
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

    // The lowest mounted index — the classic page stamps `data-idx`, the app shell
    // `data-unit-index` — read after the leave, so only the asserted steps can count as fresh.
    let lo_js = format!(
        "(function(){{var k=[...{root}.children].map(function(e){{return +(e.dataset.idx!=null?e.dataset.idx:e.dataset.unitIndex);}}).filter(function(n){{return !isNaN(n);}});return k.length?Math.min.apply(null,k):-1;}})()"
    );
    let lo_of = |tab: &headless_chrome::Tab| harness::eval(tab, &lo_js).as_i64().unwrap_or(-1);
    let mut min_lo = lo_of(tab);
    let mut lows = vec![min_lo];
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
        let lo_now = lo_of(tab);
        lows.push(lo_now);
        if lo_now >= 0 && lo_now < min_lo {
            reached_fresh = true;
            min_lo = lo_now;
        }
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
        "{surface:?}: the walk never reached UNMEASURED ground — scrollHeight never moved and \
         the window's lowest mounted index never fell below anything mounted since the open \
         ({lows:?}), so every step was over heights the engine already knew and the case proved \
         nothing. Worst overshoot seen was {worst}px."
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

/// A long session whose last turn is still open, opened at its tail: hundreds of records above
/// the reader that the page has never measured — the shape of a working session (#194).
fn fixture_long_open_turn(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, &harness::long_open_turn_session(300, 20));
    Fixture {
        base,
        path,
        turns: 301,
    }
}

/// More of the open turn where every step is TALL — a long answer, a big tool result — which is
/// the shape of a working agent's tail, and the shape that moves a running mean the most: on the
/// classic page the ordinary growth records are about as tall as its mean, and re-learning them
/// moved it by too little for the mock to fail (the #193 lesson, again).
fn tall_open_turn_growth(from_step: u32, steps: u32) -> Vec<String> {
    let mut script = Vec::new();
    for k in from_step..from_step + steps {
        script.push(thinking_at(
            &format!(
                "late step {k} deliberation: {}",
                "still weighing it. ".repeat(10)
            ),
            &now_minus(40),
        ));
        script.push(tool_open_at(&format!("late{k}"), &now_minus(35)));
        script.push(tool_result_lines(&format!("late{k}"), 40, &now_minus(30)));
        script.push(assistant_at(
            &format!(
                "late step {k} note: {}",
                "and on it goes, at some length, because the result had a great deal in it. "
                    .repeat(30)
            ),
            &now_minus(25),
        ));
    }
    script
}

/// #194: a reader parked above a growing tail is moved by their own wheel and by nothing else.
///
/// Measured on a copy of the owner's live session with the #192 trace: with no input at all the
/// engine re-measured the open turn's tail on every delta, each measure taught the running mean
/// AGAIN (337→362 in ten deltas with the record count flat at 615), every never-measured record
/// above re-estimated by the difference, the top pad grew ~1.7k px a second and `scrollTop` was
/// rewritten to match. At rest the anchor restore hides that; under the wheel the restore is
/// deferred and the next user scroll drops it, so the content is simply somewhere else — "every
/// time I attempt to unfold and read, then scroll, the page jumps to somewhere else". Two rules,
/// then: a record measured again replaces its own share of the mean rather than adding one, and
/// the sums take a new estimate only while the reader is at rest, holding the anchor as they do.
fn scenario_a_reader_above_a_growing_tail_moves_only_by_their_wheel(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let s = surface.scroller();
    let root = match surface {
        Surface::Classic => "(document.getElementById('vwin')||document.querySelector('#stream .vwin')||document.getElementById('stream'))",
        Surface::AppShell => "document.querySelector('.virtual-window')",
    };
    let vt = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{s}.getBoundingClientRect().top"),
    };
    // The reference is held by the record's IDENTITY, never by the node (the engine reuses nodes).
    let pick = format!(
        r#"(function(){{var vt={vt};var k=[...{root}.children];var p=k.find(function(e){{var r=e.getBoundingClientRect();return r.height>0&&r.bottom>vt+1;}});if(!p)return{{ok:false}};var id=p.id||(p.dataset?p.dataset.unitKey:'');var s={s};return{{ok:!!id,id:id,tag:p.tagName+'.'+p.className,top:Math.round(p.getBoundingClientRect().top),st:Math.round(s.scrollTop),h:Math.round(s.scrollHeight)}};}})()"#
    );
    let reread = |id: &str| {
        format!(
            r#"(function(){{var e=document.getElementById("{id}")||document.querySelector('[data-unit-key="{id}"]');var s={s};if(!e)return{{ok:false,h:Math.round(s.scrollHeight)}};return{{ok:true,top:Math.round(e.getBoundingClientRect().top),st:Math.round(s.scrollTop),h:Math.round(s.scrollHeight)}};}})()"#
        )
    };

    // `CR_VIEWPORT_TRACE=1` re-opens the page with the #192 trace on and, on a failing wheel,
    // prints every engine decision between that wheel and the measurement — the diagnosis is in
    // the run, not in a rerun.
    let traced = std::env::var_os("CR_VIEWPORT_TRACE").is_some();
    if traced {
        eval(tab, "(function(){ try { localStorage.viewportTrace = '1'; } catch (e) {} location.reload(); return 'ok'; })()");
        std::thread::sleep(Duration::from_millis(500));
        let mounted = match surface {
            Surface::Classic => "!!window.__viewportTrace && document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3",
            Surface::AppShell => "!!window.__viewportTrace && !!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3",
        };
        harness::until(
            tab,
            mounted,
            "the page to mount with the trace on",
            Duration::from_secs(60),
            "location.href",
        );
        settle();
    }
    let seq_now = |tab: &headless_chrome::Tab| -> i64 {
        eval(tab, "(function(){ var t = window.__viewportTrace; return t && t.length ? t[t.length-1].seq : 0; })()").as_i64().unwrap_or(0)
    };
    let dump_since = |tab: &headless_chrome::Tab, seq: i64| {
        let text = eval(tab, &format!("(function(){{ var t = window.__viewportTrace || []; return t.filter(function(e){{ return e.seq > {seq}; }}).map(function(e){{ return JSON.stringify(e); }}).join('\\n'); }})()"));
        eprintln!(
            "--- trace since seq {seq} ---\n{}\n---",
            text.as_str().unwrap_or("")
        );
    };

    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Leave follow first, and prove it: parked at the tail the page is still following, and the
    // #103 hysteresis heals a small scroll straight back.
    // Two short steps, not a long climb: the tail has to stay INSIDE the mounted window's
    // overscan, or the page never re-measures it and the case measures nothing (the classic
    // page's margin is 1500px — climbed 2700px, its tail was unmounted and the mock stayed green).
    let tail_top = harness::scroll_top(tab, surface);
    for _ in 0..2 {
        scroll_by(tab, surface, -600);
        settle();
    }
    let left = harness::scroll_top(tab, surface);
    assert!(
        tail_top - left > 900.0,
        "{surface:?}: the reader has to LEAVE the tail before this measures anything: scrollTop {tail_top} -> {left}"
    );

    // The tail grows under them: sixteen more steps of the open turn, a line every 250ms.
    let growth = LiveGrowth::start(
        fx.path.clone(),
        tall_open_turn_growth(20, 16),
        Duration::from_millis(250),
    );
    std::thread::sleep(Duration::from_millis(1500));

    // A slow, continuous wheel up — each step lands inside the intent window of the last, so the
    // deltas arrive mid-gesture. Every wheel moves the record the reader was on by what was asked.
    let step = 300.0;
    let mut measured = 0;
    let mut worst = 0.0_f64;
    for n in 1..=24 {
        let before = probe(tab, &pick);
        if !before["ok"].as_bool().unwrap_or(false) {
            continue;
        }
        let id = before["id"].as_str().unwrap_or("").to_string();
        let seq_before = if traced { seq_now(tab) } else { 0 };
        scroll_by(tab, surface, -(step as i64));
        let after = probe(tab, &reread(&id));
        if !after["ok"].as_bool().unwrap_or(false) {
            continue;
        }
        measured += 1;
        let moved = after["top"].as_f64().unwrap_or(0.0) - before["top"].as_f64().unwrap_or(0.0);
        let over = (moved - step).abs();
        if over > worst {
            worst = over;
        }
        if over > 12.0 && traced {
            eprintln!("wheel {n}: before {before} after {after}");
            dump_since(tab, seq_before);
        }
        assert!(
            over <= 12.0,
            "{surface:?}: wheel {n} asked for {step}px up and the record the reader was on moved \
             {moved}px — over by {over}px — while the tail grew ({} lines so far). A delta's \
             measure moved the estimate of every unmeasured record above the reader, the pad \
             above shifted, and the correction for it was deferred under the gesture and dropped \
             by the next wheel (#194). scrollHeight {} -> {}",
            growth.count(),
            before["h"],
            after["h"]
        );
    }
    assert!(
        measured >= 12,
        "{surface:?}: only {measured} of 24 wheels could be measured — the reference kept leaving \
         the mounted window (worst overshoot seen {worst}px)"
    );

    // Hands off: the content under the reader stays put while the tail keeps growing.
    let rest = probe(tab, &pick);
    assert!(
        rest["ok"].as_bool().unwrap_or(false),
        "{surface:?}: a record under the reader at rest"
    );
    let id = rest["id"].as_str().unwrap_or("").to_string();
    std::thread::sleep(Duration::from_millis(3000));
    let later = probe(tab, &reread(&id));
    assert!(
        later["ok"].as_bool().unwrap_or(false),
        "{surface:?}: the record under the resting reader is still mounted"
    );
    let drift = (later["top"].as_f64().unwrap_or(0.0) - rest["top"].as_f64().unwrap_or(0.0)).abs();
    assert!(
        drift <= 2.0,
        "{surface:?}: at rest, the record under the reader moved {drift}px while the tail grew — \
         a correction paid late, or an estimate applied without holding the anchor (#194)"
    );
    let appended = growth.finish(Duration::from_secs(20));
    assert!(
        appended >= 24,
        "{surface:?}: only {appended} of 64 growth lines landed — the tail hardly grew under this case"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_reader_above_a_growing_tail_moves_only_by_their_wheel() {
    let _serial = serial();
    let fx = fixture_long_open_turn("scenario-growing-tail-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_reader_above_a_growing_tail_moves_only_by_their_wheel(
        &page.tab,
        Surface::Classic,
        &fx,
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_reader_above_a_growing_tail_moves_only_by_their_wheel() {
    let _serial = serial();
    let fx = fixture_long_open_turn("scenario-growing-tail-app");
    let page = open(Surface::AppShell, &fx, 2969);
    scenario_a_reader_above_a_growing_tail_moves_only_by_their_wheel(
        &page.tab,
        Surface::AppShell,
        &fx,
    );
}

/// A long session with strongly VARIED record heights — answers from one line to sixty, tool
/// results from one line to a hundred — the shape of a real session, where one page-wide mean
/// moves a lot with every newly measured record (#194).
fn fixture_varied_long(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let turns = 400u32;
    let mut out = String::new();
    for k in 0..turns {
        let ago = (turns - k) as u64 * 60 + 600;
        out += &user_at(
            &format!(
                "question {k}: {}",
                "what comes next? ".repeat(1 + (k % 5) as usize)
            ),
            &now_minus(ago),
        );
        let lines = 1 + (k * 37) % 60;
        let mut note = format!("answer {k}:");
        for l in 0..lines {
            note += &format!(
                "\\n\\nline {l} of the answer, {}",
                "with some words in it. ".repeat(1 + (l % 3) as usize)
            );
        }
        out += &assistant_at(&note, &now_minus(ago - 10));
        if k % 2 == 0 {
            out += &tool_open_at(&format!("t{k}"), &now_minus(ago - 20));
            out += &tool_result_lines(
                &format!("t{k}"),
                1 + (k as usize * 53) % 100,
                &now_minus(ago - 25),
            );
        }
        // A pasted screenshot every third turn: an image decodes AFTER its record is mounted and
        // measured, so the record is measured a second time, late — the one thing on the classic
        // page that changes a mounted height after the fact (the owner's session carries 206).
        if k % 3 == 0 {
            out += &harness::pasted_image_sized(
                &format!("here is what it looks like, take {k}"),
                &now_minus(ago - 5),
                BAND_PNG_B64,
            );
        }
    }
    let path = stores.claude_session(SID, &out);
    Fixture { base, path, turns }
}

/// A 640x360 PNG (two flat bands): small on the wire, a real 360px-tall image once decoded.
const BAND_PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAoAAAAFoCAIAAABIUN0GAAAEh0lEQVR42u3VMQ0AAAgEMSQjBUkvDRcMpEkV3HLVEwDgWEkAAAYMAAYMABgwABgwAGDAAGDAAIABA4ABAwAGDAAGDAAGDAAYMAAYMABgwABgwACAAQOAAQMABgwABgwABgwAGDAAGDAAYMAAYMAAgAEDgAEDAAYMAAYMAAYMABgwABgwAGDAAGDAAIABA4ABAwAGDAAGDAAGDAAYMAAYMABgwABgwACAAQOAAQMABgwABgwABgwAGDAAGDAAYMAAYMAAgAEDgAEDAAYMAAYMAAYMABgwABgwAGDAAGDAAIABA4ABAwAGDAAGDAAGDAAYMAAYMABgwABgwACAAQOAAQMABgwABgwABgwAGDAAGDAAYMAAYMAAgAEDgAEDAAYMAAYMAAasAgAYMAAYMABgwABgwACAAQOAAQMABgwABgwAGDAAGDAAGDAAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwABgwAGDAAGDAAIABA4ABAwAGDAAGDAAYMAAYMAAYMABgwABgwACAAQOAAQMABgwABgwAGDAAGDAAGDAAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwABgwAGDAAGDAAIABA4ABAwAGDAAGDAAYMAAYMAAYMABgwABgwACAAQOAAQMABgwABgwAGDAAGDAAGDAAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwABgwAGDAAGDAAIABA4ABAwAGDAAGDAAYMAAYMAAYsAoAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAGDAAIABA8CjAWcaADhmwABgwABgwACAAQOAAQMABgwABgwAGDAAGDAAYMAAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAGDAAIABA4ABAwAGDAAGDAAYMAAYMABgwABgwABgwACAAQOAAQMABgwABgwAGDAAGDAAYMAAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAGDAAIABA4ABAwAGDAAGDAAYMAAYMABgwABgwABgwACAAQOAAQMABgwABgwAGDAAGDAAYMAAYMAAYMAAgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAGDAAIABA4ABAwAGDAAGDAAYMAAYMABgwABgwABgwBIAgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAIABA4ABA4ABAwAGDAAGDAAYMAAYMABgwABgwACAAQOAAQOAAQMABgwABgwAGDAAGDAAYMAAYMAAgAEDgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAIABA4ABA4ABAwAGDAAGDAAYMAAYMABgwABgwACAAQOAAQOAAQMABgwABgwAGDAAGDAAYMAAYMAAgAEDgAEDgAEDAAYMAAYMABgwABgwAGDAAGDAAIABA4ABA4ABAwAGDAAGDAAYMAAYMABgwABgwACAAQOAAQOAAQMABgwABgwAGDAAGDAAYMAAYMAAgAEDgAEDgAGrAAAGDAAGDAAYMAAYMABgwABgwACAAQOAAQMABgwABgwABgwAGDAAPLLwrFxKgP0KGwAAAABJRU5ErkJggg==";

/// #194, the owner's classic-page repro: "scroll back to turn 766, then scroll forward gradually,
/// it will go down to turn 767, 768, 769, and right after scrolling to 769 [it] jumps back to
/// turn 767 … and it will forever cycle through 767 to 769 if I just smoothly scroll". Replayed
/// on a copy of that session with the trace on: every freshly measured record moved the page-wide
/// mean, every never-measured record above re-estimated by the difference — the page swung by
/// ±10k px per 400px wheel — and at one pause the record under the reader fell out of the window,
/// so the model anchor wrote a position from the shifted sums: six turns back. A gradual forward
/// walk keeps its place: every wheel moves the record under the reader by what was asked, that
/// record stays mounted, and the turn under the reader never goes backward. Written after the fix
/// and NOT red on the engine before it: this fixture's records are measured too evenly to swing
/// the mean the way the real session did (the replay is in `design/one-engine-two-pages.md`),
/// so it holds the walk invariant going forward rather than proving the repair.
fn scenario_a_gradual_walk_forward_keeps_its_place(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let s = surface.scroller();
    let root = match surface {
        Surface::Classic => "(document.getElementById('vwin')||document.querySelector('#stream .vwin')||document.getElementById('stream'))",
        Surface::AppShell => "document.querySelector('.virtual-window')",
    };
    let vt = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{s}.getBoundingClientRect().top"),
    };
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
    // Into unmeasured ground, half way up: hundreds of records above that carry the estimate.
    let target = fx.turns / 2;
    assert!(
        harness::jump_to_turn(tab, surface, target),
        "{surface:?}: the pane offers turn {target}"
    );
    std::thread::sleep(Duration::from_millis(1500));
    let start = harness::sticky_turn(tab, surface)
        .map(|t| t.0)
        .unwrap_or(-1);
    assert!(
        (start - target as i64).abs() <= 1,
        "{surface:?}: the jump landed on turn {start}, asked for {target}"
    );

    // A gradual forward walk: a 400px wheel, then a pause long enough for the page to settle.
    let step = 400.0;
    let mut last_turn = start;
    let mut worst = 0.0_f64;
    for n in 1..=30 {
        let before = probe(tab, &pick);
        assert!(
            before["ok"].as_bool().unwrap_or(false),
            "{surface:?}: step {n}: a record under the reader before the wheel"
        );
        let id = before["id"].as_str().unwrap_or("").to_string();
        scroll_by(tab, surface, step as i64);
        std::thread::sleep(Duration::from_millis(450));
        let after = probe(tab, &reread(&id));
        assert!(
            after["ok"].as_bool().unwrap_or(false),
            "{surface:?}: step {n}: the record the reader was on ({id}) is no longer mounted after \
             one 400px wheel — the sums moved under the walk and the window was rebuilt somewhere \
             else (#194). scrollHeight {} -> {}",
            before["h"],
            after["h"]
        );
        let moved = after["top"].as_f64().unwrap_or(0.0) - before["top"].as_f64().unwrap_or(0.0);
        let over = (moved + step).abs();
        if over > worst {
            worst = over;
        }
        assert!(
            over <= 12.0,
            "{surface:?}: step {n} asked for {step}px down and the record under the reader moved \
             {moved}px — over by {over}px (#194). scrollHeight {} -> {}",
            before["h"],
            after["h"]
        );
        let turn = harness::sticky_turn(tab, surface)
            .map(|t| t.0)
            .unwrap_or(-1);
        if turn >= 0 && turn < last_turn {
            // #209: the trace AT the step, when it is on (`CR_TRACE=1`), plus what the bar reads
            // from — the bar names the last unit whose top is above the landing line, so the
            // question is always which unit that was and what the engine had just written.
            let tail = trace_lines(tab, 14);
            // Is the bar STALE, or does it genuinely disagree with the engine? Nudge the scroller
            // by a pixel and back: that repaints the bar from the settled DOM without moving the
            // reader. If it then agrees with the engine's own belief, the bar had latched a value
            // from a transient state and nothing repainted it.
            let repainted = harness::probe(tab, "(function(){ var s = document.querySelector('.transcript') || document.scrollingElement; var bar = document.getElementById('turnStickyBar') || document.getElementById('stickybar'); var before = bar ? bar.innerText.replace(/\\s+/g, ' ').slice(0, 28) : ''; s.scrollTop += 1; s.dispatchEvent(new Event('scroll')); s.scrollTop -= 1; s.dispatchEvent(new Event('scroll')); return { before: before }; })()");
            std::thread::sleep(Duration::from_millis(400));
            let after_nudge = harness::sticky_turn(tab, surface)
                .map(|t| t.0)
                .unwrap_or(-1);
            // The always-on viewport history (#197) is the record of what the engine did — its
            // state after every transaction, with the turn it believed the reader was on. No flag
            // needed; the trace above adds the seams when `SCENARIO_TRACE` is set.
            let dom = harness::probe(tab, "(function(){ var s = document.querySelector('.transcript') || document.scrollingElement; var w = document.querySelector('.virtual-window') || document.getElementById('vwin'); var vt = s.getBoundingClientRect().top; var kids = w ? [...w.children] : []; var bar = document.getElementById('turnStickyBar') || document.getElementById('stickybar'); var h = window.__viewportHistory; var states = h ? h.states.slice(-8).map(function (e) { return [e.cause, 'lo=' + e.lo, 'hi=' + e.hi, 'turn=' + e.turn, 'top=' + Math.round(e.top || 0), 'sums=' + Math.round(e.sums || 0), 'pads=' + (e.pads || []).map(Math.round).join('/'), 'est=' + Math.round(e.estimate || 0), 'live=' + Math.round(e.live || 0), 'pending=' + (e.pending || ''), 'follow=' + e.following].join(' '); }) : ['(no history)']; var actions = h ? h.actions.slice(-4).map(function (a) { return a.kind + (a.dy ? ':' + a.dy : '') + '@' + Math.round(a.t); }) : []; return { bar: bar && bar.classList.contains('on') ? bar.innerText.replace(/\\s+/g, ' ').slice(0, 28) : '(off)', top: Math.round(s.scrollTop), height: Math.round(s.scrollHeight), mounted: kids.length, firstKeys: kids.slice(0, 3).map(function (e) { return (e.dataset.unitKey || e.id || '?') + '@' + Math.round(e.getBoundingClientRect().top - vt); }), actions: actions, states: states }; })()");
            panic!(
                "{surface:?}: step {n}: the turn under the reader went BACKWARD, {last_turn} -> {turn}, \
                 on a forward walk: the re-estimated sums put the reader in a pad and the model anchor \
                 wrote a position from them (#194)\n  after a 1px nudge the bar reads {after_nudge} ({repainted})\n  {dom:#}\n  trace:\n{tail}"
            );
        }
        if turn >= 0 {
            last_turn = turn;
        }
    }
    assert!(
        last_turn > start,
        "{surface:?}: thirty wheels of {step}px did not advance the turn under the reader ({start} -> {last_turn}; worst overshoot {worst}px)"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_gradual_walk_forward_keeps_its_place() {
    let _serial = serial();
    let fx = fixture_varied_long("scenario-gradual-walk-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_gradual_walk_forward_keeps_its_place(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_gradual_walk_forward_keeps_its_place() {
    let _serial = serial();
    let fx = fixture_varied_long("scenario-gradual-walk-app");
    let page = open(Surface::AppShell, &fx, 2971);
    scenario_a_gradual_walk_forward_keeps_its_place(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: at the bottom the last turn is current (#199) ─────────────────────────────────

/// A session whose last turn is one short exchange: at the bottom, several earlier turns share
/// the viewport with it, and no header of its own can reach the line (#199).
fn fixture_short_last_turn(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut out = long_session(29, Shape::default());
    out += &user_at("question 29: and in one line?", &now_minus(60));
    out += &assistant_at("answer 29: done.", &now_minus(50));
    let path = stores.claude_session(SID, &out);
    Fixture {
        base,
        path,
        turns: 30,
    }
}

/// A session whose last two turns each carry an answer taller than the viewport (about 900px
/// against a 650px window — tall enough to span it, short enough that the engine's overscan still
/// mounts the units above it): inside one, the unit under the reader STARTS above the top edge,
/// and at the bottom nothing starts in the viewport at all (#199).
fn fixture_two_tall_turns(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut out = long_session(28, Shape::default());
    for k in 28..30 {
        out += &user_at(
            &format!("question {k}: the long one, tell me everything"),
            &now_minus(400 - (k as u64 - 27) * 100),
        );
        out += &assistant_at(
            &format!(
                "answer {k}: {}",
                "a paragraph of the long answer, one of many, each on its own line.\\n\\n"
                    .repeat(25)
            ),
            &now_minus(390 - (k as u64 - 27) * 100),
        );
    }
    let path = stores.claude_session(SID, &out);
    Fixture {
        base,
        path,
        turns: 30,
    }
}

/// #199, the owner's app-shell report under #194: "scrolling to the bottom (the active turn), the
/// turns pane lost focus (should be focused on the last turn)" and "jumping to the bottom, still
/// the last turn was not selected in the turns view". At the document bottom no further header
/// can cross the line, so the classic page's spy hands the last turn the focus (#89); the app
/// shell named the turn of the first unit that STARTS in the viewport — an earlier turn's, when
/// the last turn is short. Both pages: at the tail, reached by a jump and again by the wheel, the
/// pane and the bar name the last turn; away from it they name the turn at the top again.
fn scenario_at_the_bottom_the_last_turn_is_current(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let last = fx.turns as i64;
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    assert_eq!(
        harness::last_mounted_turn(tab, surface),
        last,
        "{surface:?}: the last turn is mounted at the tail"
    );
    assert_eq!(
        harness::pane_focus_turn(tab, surface),
        last,
        "{surface:?}: jumped to the bottom, the pane names the last turn"
    );
    let bar = harness::sticky_turn(tab, surface);
    assert_eq!(
        bar.as_ref().map(|b| b.0),
        Some(last),
        "{surface:?}: …and so does the turn bar: {bar:?}"
    );
    // Up a few screens: the end rule lets go and the pane names the turn at the top again.
    for _ in 0..3 {
        scroll_by(tab, surface, -700);
        settle();
    }
    assert!(
        !at_tail(tab, surface),
        "{surface:?}: 2,100px up leaves the tail"
    );
    let top = turn_at_top(tab, surface);
    let named = harness::pane_focus_turn(tab, surface);
    assert!(
        (named - top).abs() <= 1 && named < last,
        "{surface:?}: away from the bottom the pane names the turn at the top: pane {named}, viewport {top}"
    );
    // Back down by the wheel, the reader's own way to the bottom.
    for _ in 0..12 {
        scroll_by(tab, surface, 300);
        settle();
        if at_tail(tab, surface) {
            break;
        }
    }
    assert!(
        at_tail(tab, surface),
        "{surface:?}: wheeling down reaches the bottom again"
    );
    assert_eq!(
        harness::pane_focus_turn(tab, surface),
        last,
        "{surface:?}: scrolled to the bottom, the pane names the last turn"
    );
    let bar = harness::sticky_turn(tab, surface);
    assert_eq!(
        bar.as_ref().map(|b| b.0),
        Some(last),
        "{surface:?}: …and so does the bar: {bar:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_at_the_bottom_the_last_turn_is_current() {
    let _serial = serial();
    let fx = fixture_short_last_turn("scenario-endrule-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_at_the_bottom_the_last_turn_is_current(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_at_the_bottom_the_last_turn_is_current() {
    let _serial = serial();
    let fx = fixture_short_last_turn("scenario-endrule-app");
    let page = open(Surface::AppShell, &fx, 2973);
    scenario_at_the_bottom_the_last_turn_is_current(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a turn taller than the viewport names itself (#199) ───────────────────────────

/// The other half of #199: the spy must name the turn the reader is INSIDE, which is the last
/// header above the line (the classic rule) — not the first unit that starts below it, and not
/// nothing when no unit starts in the viewport at all. At the bottom of a tall last answer the
/// app shell had no current row; a screen above, with the next turn's header in view, it named
/// that next turn while the reader was still reading the one before it.
fn scenario_a_turn_taller_than_the_viewport_names_itself(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    let last = fx.turns as i64;
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // At the bottom the viewport lies inside the last answer: no unit starts in it.
    assert_eq!(
        harness::pane_focus_turn(tab, surface),
        last,
        "{surface:?}: at the bottom of a tall last answer the pane names the last turn"
    );
    let bar = harness::sticky_turn(tab, surface);
    assert_eq!(
        bar.as_ref().map(|b| b.0),
        Some(last),
        "{surface:?}: …and so does the bar: {bar:?}"
    );
    // Up, until the last turn's header is in view BELOW the top edge — the reader's top line is
    // then inside the penultimate turn's tall answer. Found by wheel rather than by arithmetic,
    // so the case does not depend on a paragraph's exact height.
    let s = surface.scroller();
    let vt = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{s}.getBoundingClientRect().top"),
    };
    let header_below = format!(
        "(function(){{ var e = document.querySelector('[data-turn=\"{last}\"]'); if (!e) return -1; return Math.round(e.getBoundingClientRect().top - ({vt})); }})()"
    );
    let mut found = false;
    for _ in 0..40 {
        scroll_by(tab, surface, -200);
        settle();
        let at = eval(tab, &header_below).as_i64().unwrap_or(-1);
        if at > 160 && at < 500 {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "{surface:?}: a position with the last turn's header a few lines below the top edge"
    );
    assert!(!at_tail(tab, surface), "{surface:?}: …and off the tail");
    assert_eq!(
        harness::pane_focus_turn(tab, surface),
        last - 1,
        "{surface:?}: inside the tall penultimate answer, with the next header in view, the pane names the turn being read"
    );
    let bar = harness::sticky_turn(tab, surface);
    assert_eq!(
        bar.as_ref().map(|b| b.0),
        Some(last - 1),
        "{surface:?}: …and so does the bar: {bar:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_turn_taller_than_the_viewport_names_itself() {
    let _serial = serial();
    let fx = fixture_two_tall_turns("scenario-spanning-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_turn_taller_than_the_viewport_names_itself(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_turn_taller_than_the_viewport_names_itself() {
    let _serial = serial();
    let fx = fixture_two_tall_turns("scenario-spanning-app");
    let page = open(Surface::AppShell, &fx, 2974);
    scenario_a_turn_taller_than_the_viewport_names_itself(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: growth above the reader during a momentum fling (#196, stage 0) ───────────────

/// The premise of the framework's I7: a change that has already moved the DOM is placed
/// synchronously, gesture or no gesture — not writing IS the displacement. Today the observer's
/// restore is DEFERRED while the reader owns the position (#132 step 3) and the next scroll drops
/// the debt (#138), so a row that grows above a reader mid-fling moves them by the growth and
/// nothing puts it back. The fling is a decaying wheel sequence driven from inside the page (one
/// timer, not a CDP round trip per step, so the steps land inside the intent window the way a
/// trackpad's do); a mounted element entirely above the viewport grows by 300px at the fourth
/// step. The reader must end up exactly where their own wheels put them. This case proves the
/// POSITION holds through a write mid-fling; whether momentum FEELS right under that write is
/// the owner's trackpad to judge, not this harness. Red on both surfaces on the engine before
/// #196 (measured: 503px of motion for 780px of wheel on the app shell, 484 on the classic page —
/// the 300px growth, never placed back); `known_red_196` until stage 2 of that task lands.
fn scenario_growth_above_the_reader_during_a_fling_holds(
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
    let s = surface.scroller();
    let (items, top, target) = match surface {
        Surface::Classic => (
            "document.querySelectorAll('#stream [data-idx]')",
            "0".to_string(),
            "window".to_string(),
        ),
        Surface::AppShell => (
            "document.querySelector('.virtual-window').children",
            format!("{s}.getBoundingClientRect().top"),
            s.to_string(),
        ),
    };
    // Arm: the first visible element is the anchor, the last element entirely above it grows.
    let armed = probe(
        tab,
        &format!("(function(){{ var top = {top}; var els = [...{items}]; var anchor = null, above = null; for (var e of els) {{ var r = e.getBoundingClientRect(); if (r.bottom < top - 10) above = e; else if (!anchor && r.bottom > top) anchor = e; }} if (!anchor || !above) return {{ ok: false, els: els.length }}; window.__flingAbove = above; window.__flingAnchor = anchor; return {{ ok: true, base: anchor.getBoundingClientRect().top - top, key: anchor.dataset.unitKey || anchor.id }}; }})()"),
    );
    assert_eq!(
        armed["ok"], true,
        "{surface:?}: a mounted element above the viewport and an anchor to hold: {armed}"
    );
    let base = armed["base"].as_f64().unwrap();
    // The fling: twelve decaying wheels, 24ms apart, 780px in all; the growth lands at step 4.
    let deltas = [120, 110, 100, 90, 80, 70, 60, 50, 40, 30, 20, 10];
    let asked: i64 = deltas.iter().sum();
    eval(
        tab,
        &format!("(function(){{ var s = {s}; var t = {target}; var deltas = {deltas:?}; var i = 0; window.__fling = {{ done: false, steps: [] }}; function step() {{ if (i >= deltas.length) {{ window.__fling.done = true; return; }} var d = deltas[i]; if (i === 3) {{ window.__flingAbove.style.paddingBottom = '300px'; window.__fling.grewAt = s.scrollTop; }} t.dispatchEvent(new WheelEvent('wheel', {{ deltaY: d, bubbles: true }})); s.scrollTo({{ top: s.scrollTop + d, behavior: 'instant' }}); window.__fling.steps.push(Math.round(s.scrollTop)); i++; setTimeout(step, 24); }} step(); return 'started'; }})()"),
    );
    until(
        tab,
        "window.__fling && window.__fling.done",
        "the fling to run its twelve steps",
        Duration::from_secs(5),
        "JSON.stringify(window.__fling)",
    );
    settle();
    settle();
    let after = probe(
        tab,
        &format!("(function(){{ var a = window.__flingAnchor; return {{ mounted: !!(a && a.isConnected), top: a ? a.getBoundingClientRect().top - ({top}) : null, grown: window.__flingAbove.style.paddingBottom }}; }})()"),
    );
    assert_eq!(
        after["mounted"], true,
        "{surface:?}: the record the reader was on is still mounted: {after}"
    );
    let moved = base - after["top"].as_f64().unwrap();
    assert!(
        (moved - asked as f64).abs() <= 2.0,
        "{surface:?}: a 300px growth above the reader mid-fling moved them by {moved:.0}px for {asked}px of wheel — the growth was not placed back (base {base:.0}, now {}, steps {})",
        after["top"],
        eval(tab, "JSON.stringify(window.__fling.steps)").as_str().unwrap_or("?")
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_holds_through_growth_above_the_reader_during_a_fling() {
    let _serial = serial();
    let fx = fixture("scenario-fling-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_growth_above_the_reader_during_a_fling_holds(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_holds_through_growth_above_the_reader_during_a_fling() {
    let _serial = serial();
    let fx = fixture("scenario-fling-app", 40);
    let page = open(Surface::AppShell, &fx, 2975);
    scenario_growth_above_the_reader_during_a_fling_holds(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a held landing yields to the reader's wheel (#196 stage 4) ──────────────────────

/// R5, continued. Since #196 stage 4 a jump is one transaction and the engine HOLDS its landing:
/// the target stays where it landed through everything that settles under it — the two re-land
/// loops and the classic page's 2s `holdLanding` timer, stated once. The hold must end on the
/// reader's own signal and never undo their wheel. Jump, grow above (the hold works), wheel down,
/// grow above twice more: the record under the reader stays where the wheel put it, to the pixel,
/// and the offset moves by exactly each growth — never back to the landing.
///
/// The first growth after the wheel lands in the SAME task as the wheel, so its observer runs
/// before the scroll batch's deferred window update. That ordering is what a hold released by the
/// offset alone got wrong (§4.10's first draft): the spontaneous transaction placed with the
/// wheel's drift (a no-op) and refreshed the offset the hold was read at, the update then found
/// nothing to re-read, and the NEXT growth re-landed the target — the reader's wheel undone.
fn scenario_a_held_landing_yields_to_the_readers_wheel(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    trace_on(tab, surface);
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
    assert!(
        grow_above(tab, surface, 300),
        "{surface:?}: a mounted record sits above the viewport after the jump"
    );
    settle();
    settle();
    assert_eq!(
        turn_at_top(tab, surface),
        landed,
        "{surface:?}: held — 300px appeared above the landing and turn {landed} is still at the top"
    );
    // What the reader sees is the measure throughout — never the offset, which an estimate
    // applied above them moves while the content stays. Every mounted root's top now, by identity.
    let before = roots_map(tab, surface);
    if std::env::var_os("SCENARIO_TRACE").is_some() {
        eprintln!(
            "roots near the top after the landing: {}",
            roots_near_top(tab, surface)
        );
    }
    // The reader wheels 300px down, and 250px grows above them in the same task. (The growth
    // lands on the last root fully above the viewport — after a 300px wheel that can be the landed
    // record itself, whose own top then moves up by its growth while the reader's view holds; so
    // the reference is the root under the reader AFTER the wheel, which a growth never picks.)
    assert!(
        wheel_then_grow_above(tab, surface, 300, 250),
        "{surface:?}: a mounted record sits above the viewport after the wheel"
    );
    settle();
    settle();
    let (key, top1) = root_at_top(tab, surface);
    assert!(
        !key.is_empty(),
        "{surface:?}: a record root at the top after the wheel"
    );
    let was = before.get(&key).copied().unwrap_or(f64::NAN);
    assert!(
        (top1 - (was - 300.0)).abs() <= 2.0,
        "{surface:?}: the record under the reader moved by the wheel's 300px and by nothing else — the growth above compensated, the landing not restored: {was} -> {top1}"
    );
    // A second growth, on its own: still where the wheel left it.
    assert!(
        grow_above(tab, surface, 250),
        "{surface:?}: a record above to grow, again"
    );
    settle();
    settle();
    let top2 = top_of(tab, surface, &key);
    if std::env::var_os("SCENARIO_TRACE").is_some() {
        eprintln!(
            "roots near the top after the second growth: {}",
            roots_near_top(tab, surface)
        );
    }
    trace_tail(tab, "held landing, after the wheel and two growths", 60);
    // Two pixels: a placement writes an integer offset against fractional rects, so each hold
    // can leave a record within a pixel of where it was.
    assert!(
        (top2 - top1).abs() <= 2.0,
        "{surface:?}: 250px more above, and the record under the reader sits where the wheel left it: {top1} -> {top2}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_held_landing_yields_to_the_readers_wheel() {
    let _serial = serial();
    let fx = fixture("scenario-heldwheel-classic", 120);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_held_landing_yields_to_the_readers_wheel(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_held_landing_yields_to_the_readers_wheel() {
    let _serial = serial();
    let fx = fixture("scenario-heldwheel-app", 120);
    let page = open(Surface::AppShell, &fx, 2976);
    scenario_a_held_landing_yields_to_the_readers_wheel(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a smooth step yields to the reader's wheel (#196 stage 4) ──────────────────────

/// The engine owns smooth motion since #196 stage 4: a stepped head is revealed with the
/// browser's own animation, the engine walks its scroll events as its own, and the reader's wheel
/// mid-flight is the reader interrupting it — the browser cancels the animation on their input,
/// the engine drops its record of the write and the hold on the head, and the events after it are
/// theirs. From the tail, `j` steps to the first mounted head, far above; before the animation can
/// arrive the reader scrolls back to where they were. They win: the offset is theirs to the pixel,
/// the head is not re-landed, and a growth above afterwards holds the record under them, not the
/// head. (Headless Chrome may finish the animation before the wheel lands; the case then still
/// holds the same contract — no re-landing after the reader moves — and prints what it saw.)
fn scenario_a_smooth_step_yields_to_the_readers_wheel(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    trace_on(tab, surface);
    harness::jump_to_end(tab, surface);
    settle();
    settle();
    let start = harness::scroll_top(tab, surface);
    let scroller = match surface {
        Surface::Classic => "document.scrollingElement",
        Surface::AppShell => "document.querySelector('.transcript')",
    };
    // `j`: the next fold head, which from the tail is the first mounted one, well above.
    harness::key(tab, "j", false);
    let mid = harness::scroll_top(tab, surface);
    // The reader's wheel: 400px up from where they were — not the tail, not the head's landing.
    // Twice, a frame apart: a synthetic wheel never reaches the compositor, which applies one more
    // frame of its animation after the first instant scroll (measured: 56px) before the
    // cancellation lands; a real wheel cancels it there at once. The second scroll is the reader
    // still moving, and the record at the top of their view is read in the same task as it.
    let interrupt = format!(
        "(function(){{ var s = {scroller}; s.dispatchEvent(new WheelEvent('wheel', {{deltaY: -1, bubbles: true}})); s.scrollTop = {start} - 400; var vt = {vt}; var k = [...document.querySelectorAll({roots:?})]; var e = k.find(function (x) {{ var r = x.getBoundingClientRect(); return r.height > 0 && r.bottom > vt + 1; }}); return e ? {{ key: {ident}, top: e.getBoundingClientRect().top - vt }} : null; }})()",
        vt = match surface { Surface::Classic => "0", Surface::AppShell => "document.querySelector('.transcript').getBoundingClientRect().top" },
        roots = match surface { Surface::Classic => "#vwin > [data-idx]", Surface::AppShell => ".virtual-window > [data-unit-key]" },
        ident = match surface { Surface::Classic => "e.id", Surface::AppShell => "e.dataset.unitKey" },
    );
    harness::probe(tab, &interrupt);
    std::thread::sleep(Duration::from_millis(50));
    let seen = harness::probe(tab, &interrupt);
    let key = seen["key"].as_str().unwrap_or("").to_string();
    let top0 = seen["top"].as_f64().unwrap_or(f64::NAN);
    assert!(
        !key.is_empty(),
        "{surface:?}: a record root at the top after the reader's scroll"
    );
    settle();
    settle();
    // The wheel interrupted a smooth step, and a view that has stopped MOVING is not one that
    // has FINISHED (#227). Until the engine is at rest both reads below are a stale spy (#209)
    // and the growth lands mid-transition, where its compensation is lost — which is the whole
    // of this case's flakiness: it failed 11 of 12 runs without this wait and passed 6 of 6 when
    // an unrelated probe happened to delay the same spot. Additive, never a replacement for the
    // settles above.
    until_engine_quiet(tab);
    let top1 = top_of(tab, surface, &key);
    let head = eval(
        tab,
        &format!(
            "(function(){{ var h = document.activeElement; if (!h || !h.getBoundingClientRect) return null; return h.getBoundingClientRect().top - {vt}; }})()",
            vt = match surface { Surface::Classic => "0", Surface::AppShell => "document.querySelector('.transcript').getBoundingClientRect().top" },
        ),
    );
    eprintln!("{surface:?}: start {start}, mid-flight read {mid}, the reader's record at {top0} then {top1}, the stepped head at {head}");
    trace_tail(tab, "smooth step, after the reader's wheel", 60);
    assert!(
        (top1 - top0).abs() <= 1.0,
        "{surface:?}: the reader's wheel wins over the step's animation — the record under them stays where they put it: {top0} -> {top1} (mid-flight {mid})"
    );
    if let Some(h) = head.as_f64() {
        assert!(
            (h - 160.0).abs() > 2.0,
            "{surface:?}: the stepped head was not re-landed at 160 after the reader moved: {h}"
        );
    }
    assert!(
        grow_above(tab, surface, 200),
        "{surface:?}: a record above to grow"
    );
    settle();
    settle();
    // Symmetric with the wait before the growth: the compensation is a transaction like any
    // other, and reading its result before it lands measures the transition, not the outcome.
    until_engine_quiet(tab);
    let top2 = top_of(tab, surface, &key);
    if (top2 - top1).abs() > 4.0 {
        history_tail(
            tab,
            "growth above after the reader's wheel interrupted a smooth step",
            14,
        );
    }
    // Four pixels: measured 1.7px and 2.7px across runs — a placement writes an integer offset
    // against fractional rects, once for the measure that heard the growth and once for the
    // estimates it moved — against a failure of 200px (the growth lost) or 400px (re-landed).
    assert!(
        (top2 - top1).abs() <= 4.0,
        "{surface:?}: after a growth above, the record under the reader sits where the wheel left it, not the stepped head: {top1} -> {top2}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_smooth_step_yields_to_the_readers_wheel() {
    let _serial = serial();
    let fx = fixture("scenario-smoothstep-classic", 40);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_smooth_step_yields_to_the_readers_wheel(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_smooth_step_yields_to_the_readers_wheel() {
    let _serial = serial();
    let fx = fixture("scenario-smoothstep-app", 40);
    let page = open(Surface::AppShell, &fx, 2977);
    scenario_a_smooth_step_yields_to_the_readers_wheel(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: a mounted unit's height does not depend on the window's edge (#201) ─────────────
/// The engine's sums treat a unit's height as a property of the unit. The app shell's demo
/// stylesheet pads the transcript's first turn less (`.turn:first-child{padding-top:8px}` against
/// the turn's own padding), and the first MOUNTED turn of `.virtual-window` is that first child —
/// so a turn at the window's top edge measured about 10px shorter than the same turn once a unit
/// was mounted above it. #196 stage 2 met it twice (11px flips per wheel in the unfold probe
/// while two windows were mounted per transaction; the rendering audit watching a formerly-first
/// turn re-pad and its absolutely positioned buttons move when a remeasure mounted one more
/// record above it), and stage 5's tail window — the end's screenful, one more record than the
/// landing shape mounted — put it in front of the audit's width-and-theme case. The rule the
/// case found was not the first-child one the task named but the demo's
/// `.process-surface + .turn.assistant{padding-top:4px}` (15px at the edge, 4px with the process
/// mounted), and production's own `*:has(+ .process-surface){margin-bottom:8px}` is the same
/// thing at the bottom edge. The claim, on both pages: every mounted unit measures the same
/// height — as `itemHeight` measures it, margins included — before and after its neighbours are
/// mounted. Eight window edges up, then eight down, so the edge unit is of every kind.
fn scenario_a_unit_height_does_not_depend_on_the_window_edge(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    // Every mounted unit: its key, index, height as the engine measures it (the rect plus both
    // margins — `itemHeight`) and, for the failure message, its class, padding-top and margins.
    let mounted = match surface {
        Surface::Classic => "(function(){ return [...document.querySelectorAll('#stream [data-idx]')].map(function(e){ var r = e.getBoundingClientRect(), s = getComputedStyle(e); return { key: e.dataset.idx, index: +e.dataset.idx, height: r.height + (parseFloat(s.marginTop) || 0) + (parseFloat(s.marginBottom) || 0), cls: e.className, pad: s.paddingTop + '/' + s.marginBottom }; }); })()",
        Surface::AppShell => "(function(){ return [...document.querySelectorAll('.virtual-window > [data-unit-key]')].map(function(e){ var r = e.getBoundingClientRect(), s = getComputedStyle(e); return { key: e.dataset.unitKey, index: +e.dataset.unitIndex, height: r.height + (parseFloat(s.marginTop) || 0) + (parseFloat(s.marginBottom) || 0), cls: e.className, pad: s.paddingTop + '/' + s.marginBottom }; }); })()",
    };
    let snapshot = |tab: &headless_chrome::Tab| -> Vec<serde_json::Value> {
        harness::probe(tab, mounted)
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    jump_to_end(tab, surface);
    await_tail(tab, surface, "the tail before the walk up");
    settle();
    settle();
    let start = snapshot(tab);
    assert!(
        start.first().and_then(|u| u["index"].as_i64()).unwrap_or(0) > 0,
        "{surface:?}: the window's top edge sits above record 0, so a unit CAN be mounted above \
         it: {start:?}"
    );
    let mut violations = Vec::new();
    let mut edges = 0;
    let mut before = start;
    // Up, mounting above (the top edge), then down, mounting below (the bottom edge). A unit
    // still mounted after a step must measure what it measured before it.
    for (direction, dy) in [("up", -250), ("down", 250)] {
        let mut moved = 0;
        for step in 0..40 {
            scroll_by(tab, surface, dy);
            settle();
            let after = snapshot(tab);
            let edge = |units: &Vec<serde_json::Value>| -> (i64, i64) {
                (
                    units
                        .first()
                        .and_then(|u| u["index"].as_i64())
                        .unwrap_or(-1),
                    units.last().and_then(|u| u["index"].as_i64()).unwrap_or(-1),
                )
            };
            let (lo0, hi0) = edge(&before);
            let (lo1, hi1) = edge(&after);
            if lo1 != lo0 || hi1 != hi0 {
                moved += 1;
            }
            for unit in &before {
                let key = unit["key"].as_str().unwrap_or("");
                if let Some(now) = after.iter().find(|u| u["key"].as_str() == Some(key)) {
                    let h0 = unit["height"].as_f64().unwrap_or(0.0);
                    let h1 = now["height"].as_f64().unwrap_or(0.0);
                    if (h1 - h0).abs() > 0.5 {
                        violations.push(format!(
                            "{direction} step {step}: unit {key} (index {}, {}) measured {h0}px \
                             with the window at [{lo0}, {hi0}], {h1}px with it at [{lo1}, {hi1}] \
                             (padding-top/margin-bottom {} -> {})",
                            unit["index"], unit["cls"], unit["pad"], now["pad"]
                        ));
                    }
                }
            }
            before = after;
            if moved >= 8
                || (direction == "up" && lo1 <= 0)
                || (direction == "down" && harness::at_tail(tab, surface))
            {
                break;
            }
        }
        edges += moved;
    }
    assert!(
        edges >= 6,
        "{surface:?}: the walks moved the window's edges only {edges} times, so this measured \
         almost nothing"
    );
    assert!(
        violations.is_empty(),
        "{surface:?}: a mounted unit's height depends on where the window's edge sits — the \
         sums assume it is a property of the unit (#201):\n{}",
        violations.join("\n")
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_unit_height_does_not_depend_on_the_window_edge() {
    let _serial = serial();
    let fx = fixture("scenario-edge-unit-classic", 60);
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_unit_height_does_not_depend_on_the_window_edge(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_unit_height_does_not_depend_on_the_window_edge() {
    let _serial = serial();
    let fx = fixture("scenario-edge-unit-app", 60);
    let page = open(Surface::AppShell, &fx, 2980);
    scenario_a_unit_height_does_not_depend_on_the_window_edge(&page.tab, Surface::AppShell, &fx);
}

/// What the engine's check mode has reported so far (framework §4.12): the ring at
/// `window.__viewportViolations`, always on, trace or no trace.
fn violations(tab: &headless_chrome::Tab) -> Vec<serde_json::Value> {
    harness::probe(tab, "(window.__viewportViolations || []).slice()")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Framework §4.12: the engine checks its own invariants at the end of every transaction — the
/// reader is where `P` says, the record under `P` is mounted, a landing shows content, the sums
/// are the heights, one share per record, follow changes only by the reader, nothing is written
/// under a drag — and reports a `violation` for each failure. A workout that reaches every
/// transaction kind (a jump, wheels both ways, a fold, paging, growth above and at the tail, the
/// end, the wheel back up) must report none. A violation here is a finding, never a reason to
/// weaken the check.
fn scenario_the_engine_holds_its_invariants(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    trace_on(tab, surface);
    let before = violations(tab);
    assert!(
        before.is_empty(),
        "the open page must already be clean: {before:?}"
    );
    assert!(
        harness::jump_to_turn(tab, surface, 60),
        "jump into the middle"
    );
    settle();
    for _ in 0..6 {
        scroll_by(tab, surface, 400);
        settle();
    }
    for _ in 0..6 {
        scroll_by(tab, surface, -400);
        settle();
    }
    // A fold opened and closed where the wheels left the reader (the tail of the generic fixture
    // ends on prose and mounts no fold header on the classic page).
    let opened = harness::open_last_fold(tab, surface);
    assert!(opened != -1, "a fold opens where the reader is");
    settle();
    let closed = harness::open_last_fold(tab, surface);
    assert!(closed != -1, "…and closes again");
    settle();
    assert!(grow_above(tab, surface, 300), "a growth above the reader");
    settle();
    // A page down and up. Not a synthetic Space: the classic page pages NATIVELY on Space
    // (`export.js`: "this page scrolls natively on Space"), which a dispatched key event does
    // not trigger, so the wheel is the one gesture that pages both surfaces.
    scroll_by(tab, surface, 800);
    settle();
    scroll_by(tab, surface, -800);
    settle();
    harness::jump_to_end(tab, surface);
    settle();
    let growth = LiveGrowth::start(fx.path.clone(), growth_script(), Duration::from_millis(900));
    let _ = growth.finish(Duration::from_secs(30));
    settle();
    harness::jump_to_end(tab, surface);
    settle();
    for _ in 0..3 {
        scroll_by(tab, surface, -400);
        settle();
    }
    let found = violations(tab);
    trace_tail(tab, "invariants", 40);
    assert!(
        found.is_empty(),
        "the engine reported {} violation(s) on {surface:?}:\n{}",
        found.len(),
        found
            .iter()
            .map(|v| format!("  {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_engine_holds_its_invariants() {
    let _serial = serial();
    let fx = fixture("scenario-invariants-classic", 120);
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_engine_holds_its_invariants(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_engine_holds_its_invariants() {
    let _serial = serial();
    let fx = fixture("scenario-invariants-app", 120);
    let page = open(Surface::AppShell, &fx, 2978);
    scenario_the_engine_holds_its_invariants(&page.tab, Surface::AppShell, &fx);
}

/// One stream of the viewport history (design/viewport-history.md), read from the page.
fn history(tab: &headless_chrome::Tab, stream: &str) -> Vec<serde_json::Value> {
    harness::probe(
        tab,
        &format!("(window.__viewportHistory ? window.__viewportHistory.{stream} : []).slice()"),
    )
    .as_array()
    .cloned()
    .unwrap_or_default()
}

/// design/viewport-history.md (#197): always on, the engine keeps the last hour of the reader's
/// actions, its state after every transaction and the shape of every records change, built from
/// what it already knows and exportable without content. A workout — wheels, a jump, a fold, live
/// growth — leaves the three streams holding what happened, in order; the export has the format
/// and carries no text; and with a small bound, entries age out.
fn scenario_the_history_records_what_happened(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    // The open itself is recorded: the first records transaction that brought records in is a
    // delta from nothing (the shell clears its units first — an empty-to-empty delta, recorded
    // as the transaction it is), and the engine's state after it is a state entry.
    let opened = history(tab, "deltas");
    assert!(!opened.is_empty(), "{surface:?}: the open is a delta");
    let first = opened
        .iter()
        .find(|d| d["count1"].as_i64().unwrap_or(0) > 0)
        .unwrap_or_else(|| panic!("{surface:?}: a delta brought the fixture in: {opened:?}"));
    assert_eq!(
        first["count0"], 0,
        "the first delta starts from nothing: {first}"
    );
    assert_eq!(first["from"], 0, "…and is a rewrite from 0: {first}");
    let count_open = first["count1"].as_i64().unwrap_or(0);
    assert!(count_open > 0, "…into the fixture: {first}");
    assert!(
        first["kinds"].as_array().is_some_and(|k| !k.is_empty()),
        "a delta names the kinds it brought in: {first}"
    );
    assert!(
        !history(tab, "states").is_empty(),
        "{surface:?}: the open's transactions are states"
    );
    // Three wheel gestures, each settled, are three wheel actions with their deltaY (the harness
    // dispatches its wheel within the coalescing window of a retry, so a gesture may count two).
    for _ in 0..3 {
        scroll_by(tab, surface, 400);
        settle();
    }
    let actions = history(tab, "actions");
    let wheels: Vec<&serde_json::Value> = actions.iter().filter(|a| a["kind"] == "wheel").collect();
    assert!(
        wheels.len() >= 3,
        "{surface:?}: three settled wheels are three actions, got {}: {actions:?}",
        wheels.len()
    );
    assert!(
        wheels
            .iter()
            .all(|w| w["dy"].as_i64().unwrap_or(0) >= 400 && w["n"].as_i64().unwrap_or(0) >= 1),
        "{surface:?}: a wheel action carries its summed deltaY and its count: {wheels:?}"
    );
    // A jump the page offers is a commanded move with its target.
    let before_jump = history(tab, "actions").len();
    assert!(
        harness::jump_to_turn(tab, surface, 20),
        "{surface:?}: jump to turn 20"
    );
    settle();
    let actions = history(tab, "actions");
    let jump = actions[before_jump..]
        .iter()
        .find(|a| a["kind"] == "jump" || a["kind"] == "reveal");
    assert!(
        jump.is_some_and(|j| j["index"].is_number() || j["key"].is_string()),
        "{surface:?}: the jump is an action with its target: {:?}",
        &actions[before_jump..]
    );
    // A fold the reader opens is the page's own word, through `noteAction`.
    let before_fold = history(tab, "actions").len();
    let opened_fold = harness::open_last_fold(tab, surface);
    assert!(
        opened_fold != -1,
        "{surface:?}: a fold opens where the reader is"
    );
    settle();
    let actions = history(tab, "actions");
    let fold = actions[before_fold..].iter().find(|a| a["kind"] == "fold");
    assert!(
        fold.is_some_and(|f| f["target"]["open"] == true),
        "{surface:?}: the fold is an action naming what opened: {:?}",
        &actions[before_fold..]
    );
    // Live growth is a delta per apply: appended, the tail named before and after.
    let deltas_before = history(tab, "deltas").len();
    let count_before = history(tab, "states")
        .last()
        .and_then(|s| s["count"].as_i64())
        .unwrap_or(0);
    let growth = LiveGrowth::start(fx.path.clone(), growth_script(), Duration::from_millis(900));
    let _ = growth.finish(Duration::from_secs(30));
    settle();
    let deltas = history(tab, "deltas");
    assert!(
        deltas.len() > deltas_before,
        "{surface:?}: growth is recorded as deltas ({} before, {} after)",
        deltas_before,
        deltas.len()
    );
    let grew: Vec<&serde_json::Value> = deltas[deltas_before..]
        .iter()
        .filter(|d| d["count1"].as_i64() > d["count0"].as_i64())
        .collect();
    assert!(
        !grew.is_empty(),
        "{surface:?}: a delta grew the count: {deltas:?}"
    );
    for d in &grew {
        let (count0, count1, from) = (
            d["count0"].as_i64().unwrap(),
            d["count1"].as_i64().unwrap(),
            d["from"].as_i64().unwrap(),
        );
        assert!(
            count0 >= count_before,
            "growth starts from the count the states saw: {d}"
        );
        assert!(
            from <= count0,
            "the rewritten index is never past the count before: {d}"
        );
        assert_eq!(
            d["tail1"]["index"],
            count1 - 1,
            "the tail after is the last record: {d}"
        );
        assert!(d["tail1"]["kind"].is_string(), "…with its kind: {d}");
        let kinds = d["kinds"].as_array().map(Vec::len).unwrap_or(0) as i64;
        let more = d["more"].as_i64().unwrap_or(0);
        assert_eq!(
            kinds + more,
            count1 - from,
            "the kinds cover [from, count1): {d}"
        );
    }
    // A tool call and then a queued prompt: two top-level records that are neither a prompt nor
    // prose (consecutive tool calls nest into ONE record, #176), so on the shell one process unit
    // spans two records — which is what the export's record-level shape exists for.
    let deltas_seen = history(tab, "deltas").len();
    harness::append(&fx.path, &tool_open_at("hist-a", &now_minus(5)));
    harness::append(&fx.path, &tool_result_at("hist-a", &now_minus(4)));
    harness::append(
        &fx.path,
        &queued_at("a question queued behind the tool call", &now_minus(3)),
    );
    harness::until(
        tab,
        &format!("window.__viewportHistory.deltas.length > {deltas_seen}"),
        "the tool call and the queued prompt to arrive as a delta",
        Duration::from_secs(20),
        "window.__viewportHistory.deltas.length",
    );
    settle();
    // A state is the engine after a transaction, from what it knows: the window, the count, its
    // belief of the offset and the turn under `P`.
    let states = history(tab, "states");
    let last = states.last().expect("a state after the growth");
    for field in [
        "cause",
        "lo",
        "hi",
        "count",
        "following",
        "sums",
        "pads",
        "estimate",
        "live",
    ] {
        assert!(
            !last[field].is_null(),
            "{surface:?}: a state carries `{field}`: {last}"
        );
    }
    assert!(
        last["top"].is_number(),
        "{surface:?}: a state carries the engine's belief of the offset: {last}"
    );
    assert!(
        last["turn"].is_number(),
        "{surface:?}: a state names the turn under P: {last}"
    );
    let count_now = last["count"].as_i64().unwrap_or(0);
    // The export: the format, the session's shape and no content.
    let export = harness::probe(tab, "window.__viewportHistory.export()");
    assert_eq!(
        export["format"], "viewport-history/1",
        "{surface:?}: the export's format"
    );
    assert_eq!(
        export["page"],
        match surface {
            Surface::Classic => "classic",
            Surface::AppShell => "app",
        },
        "{surface:?}: the export names its page"
    );
    for key in [
        "frame",
        "session",
        "actions",
        "states",
        "deltas",
        "violations",
        "exported",
        "elapsed",
    ] {
        assert!(
            !export[key].is_null(),
            "{surface:?}: the export carries `{key}`"
        );
    }
    assert_eq!(
        export["session"]["count"], count_now,
        "{surface:?}: the session's count is the engine's"
    );
    let items = export["session"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        items.len() as i64,
        count_now,
        "{surface:?}: one item per engine index"
    );
    for item in &items {
        let row = item.as_array().expect("an item is a row");
        assert_eq!(row.len(), 5, "kind, height, turn, from, to: {item}");
        assert!(row[0].is_string(), "an item's kind: {item}");
        assert!(
            row[1].is_null() || row[1].is_number(),
            "an item's measured height or null: {item}"
        );
        assert!(
            row[3].is_number() && row[4].is_number(),
            "an item's record range: {item}"
        );
    }
    assert!(
        items.iter().any(|i| i[1].is_number()),
        "{surface:?}: the mounted items carry measured heights"
    );
    match surface {
        Surface::Classic => assert!(
            export["session"]["records"].is_null(),
            "one item is one record on the classic page"
        ),
        Surface::AppShell => assert!(
            export["session"]["records"]
                .as_array()
                .is_some_and(|r| r.len() as i64 > count_now && r.iter().all(|k| k.is_string())),
            "the shell's process unit spans two records, so the record-level kinds are listed: {} (items {:?})",
            export["session"]["records"],
            &items[items.len().saturating_sub(4)..]
        ),
    }
    let text = serde_json::to_string(&export).unwrap();
    for phrase in ["lorem", "eiusmod", "question ", "answer ", SID] {
        assert!(
            !text.contains(phrase),
            "{surface:?}: the export carries no content, found {phrase:?}"
        );
    }
    // The bound: reopened with a small one, what the reader did before the bound ages out when
    // the next entry is pushed.
    eval(
        tab,
        "location.href = location.href + (location.search ? '&' : '?') + 'historyMs=1500'; 'ok'",
    );
    let mounted = match surface {
        Surface::Classic => "!!window.__viewportHistory && document.querySelectorAll('#stream .blk').length >= 3 && document.body.scrollHeight > window.innerHeight * 3",
        Surface::AppShell => "!!window.__viewportHistory && !!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3",
    };
    harness::until(
        tab,
        mounted,
        "the page to reopen with a 1.5s bound",
        Duration::from_secs(60),
        "location.href",
    );
    settle();
    scroll_by(tab, surface, 400);
    settle();
    assert!(
        history(tab, "actions").iter().any(|a| a["kind"] == "wheel"),
        "{surface:?}: the wheel is recorded under the small bound"
    );
    std::thread::sleep(Duration::from_millis(2000));
    assert!(
        harness::jump_to_turn(tab, surface, 10),
        "{surface:?}: a jump after the bound"
    );
    settle();
    let aged = history(tab, "actions");
    assert!(
        aged.iter().all(|a| a["kind"] != "wheel"),
        "{surface:?}: the wheel aged out of a 1.5s history: {aged:?}"
    );
    assert!(
        aged.iter()
            .any(|a| a["kind"] == "jump" || a["kind"] == "reveal"),
        "{surface:?}: …and the jump that pushed it out remains: {aged:?}"
    );
    let states = history(tab, "states");
    let (min_t, max_t) = states.iter().fold((i64::MAX, i64::MIN), |(lo, hi), s| {
        let t = s["t"].as_i64().unwrap_or(0);
        (lo.min(t), hi.max(t))
    });
    assert!(
        max_t - min_t <= 1500,
        "{surface:?}: every state is within the bound of the newest ({min_t}..{max_t})"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_history_records_what_happened() {
    let _serial = serial();
    let fx = fixture("scenario-history-classic", 60);
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_history_records_what_happened(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_history_records_what_happened() {
    let _serial = serial();
    let fx = fixture("scenario-history-app", 60);
    let page = open(Surface::AppShell, &fx, 2981);
    scenario_the_history_records_what_happened(&page.tab, Surface::AppShell, &fx);
}

/// #207: a file the agent DELIVERED is offered like any other path — the header names it,
/// relativized, and carries the capability stamp a reveal needs. The owner asked for exactly
/// this: "agent-replay should recognize it as a file path and handle it like any other file
/// paths", and "I wonder why we did not shorten the path with ~/". Written once, run on both
/// pages: the classic page renders the head's `path`/`sig` as a link, the app shell renders the
/// same pair as its renderer target.
fn scenario_a_delivered_file_is_offered_like_any_path(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    let js = match surface {
        Surface::Classic => "(function(){ var a = [...document.querySelectorAll('#stream a[data-path]')].find(function (e) { return /tour\\.mp4$/.test(e.dataset.path || ''); }); if (!a) return null; return { text: a.textContent.trim(), path: a.dataset.path, sig: !!a.dataset.sig, title: a.title }; })()",
        Surface::AppShell => "(function(){ var e = [...document.querySelectorAll('[data-reference-path]')].find(function (x) { return /tour\\.mp4$/.test(x.dataset.referencePath || ''); }); if (!e) return null; return { text: e.textContent.trim(), path: e.dataset.referencePath, sig: !!e.dataset.referenceSig, title: e.title }; })()",
    };
    until(
        tab,
        &format!("!!{js}"),
        "the delivered file to be offered as a path",
        Duration::from_secs(20),
        "document.body.innerText.slice(0, 200)",
    );
    let link = harness::probe(tab, js);
    assert!(
        link["text"]
            .as_str()
            .unwrap_or("")
            .ends_with("video/tour.mp4"),
        "{surface:?}: the header names the file it delivered: {link}"
    );
    assert!(
        !link["text"].as_str().unwrap_or("").starts_with('/'),
        "{surface:?}: …relativized, not the absolute path the tool printed: {link}"
    );
    assert!(
        link["sig"].as_bool().unwrap_or(false),
        "{surface:?}: …stamped, so the click can reach the reveal route: {link}"
    );
    // What the click DOES is the page's own choice — the classic page titles the link with the
    // action and the path ("Reveal /…/tour.mp4"), the shell with the action alone ("Open in the
    // preview pane"), because the render policy decides whether the bytes may be shown. Either
    // way the reader is told before they click.
    let title = link["title"].as_str().unwrap_or("").to_lowercase();
    assert!(
        title.contains("reveal") || title.contains("open") || title.contains("preview"),
        "{surface:?}: …and the title says what the click will do: {link}"
    );
}

/// A session that delivers a file: the call names it in `files`, the result is the tool's own
/// prose. Written by hand — nothing here comes from a real session.
fn delivered_file_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let dir = base.join("video");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("tour.mp4");
    std::fs::write(&file, b"not really an mp4").unwrap();
    let abs = file.to_string_lossy().to_string();
    let cwd = base.to_string_lossy().to_string();
    let mut jsonl = String::new();
    jsonl += &format!(
        "{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"send me the tour\"}}]}},\"timestamp\":\"{}\"}}\n",
        harness::at("00:00")
    );
    jsonl += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"f1\",\"name\":\"SendUserFile\",\"input\":{{\"files\":[\"{abs}\"],\"caption\":\"the tour\",\"status\":\"normal\"}}}}]}},\"timestamp\":\"{}\"}}\n",
        harness::at("00:01")
    );
    jsonl += &format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"f1\",\"content\":\"1 file delivered to user.\\n  {abs} → file_uuid: 11111111-2222-4333-8444-555555555555\"}}]}},\"timestamp\":\"{}\"}}\n",
        harness::at("00:02")
    );
    jsonl += &long_session(8, Shape::default());
    let path = stores.claude_session(SID, &jsonl);
    Fixture {
        base,
        path,
        turns: 9,
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_delivered_file_is_offered_like_any_path() {
    let _serial = serial();
    let fx = delivered_file_fixture("scenario-delivered-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_delivered_file_is_offered_like_any_path(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_delivered_file_is_offered_like_any_path() {
    let _serial = serial();
    let fx = delivered_file_fixture("scenario-delivered-app");
    let page = open(Surface::AppShell, &fx, 2982);
    scenario_a_delivered_file_is_offered_like_any_path(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: an image is ONE click away, and enlarging it zooms and pans (#228) ───────────

/// The owner: "currently it takes two clicks to see the image, which is one click too many."
///
/// The two were the record's fold and the "Show image" toggle inside it. An attachment record
/// starts open now, so the toggle is the FIRST thing a reader can press. The second half of the
/// rule is what must not change: the bytes stay behind that press, because the owner's reason
/// for choosing this shape over "unfolding shows the image" was "when user clicks expand all,
/// that would lead to all images being downloaded". So this asserts both — one click to the
/// image, and nothing fetched before it.
fn scenario_an_image_is_one_click_away(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    if surface == Surface::Classic {
        // The classic page never had the problem: the bytes are already in the payload, so it
        // draws the image inline and one click enlarges it. It is the reference for the shell.
        until(tab, "(function(){ var i = document.querySelector('.amark img'); return !!i && i.naturalWidth >= 1 && i.getBoundingClientRect().height > 0; })()", "the classic page to show the image with no clicks at all", Duration::from_secs(20), "document.querySelectorAll('.amark').length + ' attachment blocks'");
        return;
    }
    until(
        tab,
        "!!document.querySelector('[data-image-toggle]')",
        "the image row to render inside the process",
        Duration::from_secs(20),
        "document.querySelectorAll('.renderer-note, .renderer-image').length + ' attachment views'",
    );
    // No click yet: the record is open on its own, and its toggle is really visible — a rect is
    // not visibility, so the control is hit-tested where it claims to be (the rect-is-not-
    // visibility memory; a clipped control still measures 34×34).
    let ready = eval(tab, "(function(){ var t = document.querySelector('[data-image-toggle]'); var r = t.closest('.renderer'); var b = t.getBoundingClientRect(); var hit = document.elementFromPoint(Math.round(b.left + b.width / 2), Math.round(b.top + b.height / 2)); return JSON.stringify({ closed: r.classList.contains('closed'), height: Math.round(b.height), reachable: !!hit && (hit === t || t.contains(hit)) }); })()");
    let ready: serde_json::Value =
        serde_json::from_str(ready.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        ready["closed"],
        serde_json::json!(false),
        "the attachment record starts open, so its one affordance needs no click to reach: {ready}"
    );
    assert!(
        ready["height"].as_f64().unwrap_or(0.0) > 0.0,
        "the toggle is drawn: {ready}"
    );
    assert_eq!(
        ready["reachable"],
        serde_json::json!(true),
        "the toggle answers a click where it is painted: {ready}"
    );
    // …and nothing has been fetched for it.
    assert_eq!(
        eval(
            tab,
            "document.querySelectorAll('.renderer-image img').length"
        ),
        0,
        "an open record has loaded no image bytes"
    );
    // The performance property the owner chose this shape FOR: expand everything, and still no
    // image is fetched. This is the assertion that would fail had the fix been "unfolding shows
    // the image" instead.
    eval(tab, "(function(){ var b = document.getElementById('sessionFoldAll'); if (b) b.click(); return 'ok'; })()");
    settle();
    assert_eq!(
        eval(
            tab,
            "document.querySelectorAll('.renderer-image img').length"
        ),
        0,
        "expand-all fetches no images: the bytes stay behind the per-image click"
    );
    // One click, and the image is there.
    eval(
        tab,
        "document.querySelector('[data-image-toggle]').click(); 'ok'",
    );
    until(tab, "(function(){ var i = document.querySelector('.renderer-image-thumb img'); return !!i && i.naturalWidth >= 1; })()", "one click to show the image", Duration::from_secs(10), "(function(){ var i = document.querySelector('.renderer-image-thumb img'); return i ? 'natural ' + i.naturalWidth : 'no img'; })()");
}

/// The owner: "when see the enlarged image, all me to have zoom control and move control (when
/// the image is too big to fit). By default, zoom to fit on screen."
///
/// One shared engine drives every viewer (html/shared/image-view.js), so both pages are asked
/// the same three questions: does it open fitted, does zooming in change the scale, and does an
/// image larger than its stage become pannable when it could not pan before.
fn scenario_the_enlarged_image_zooms(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    let (open_it, stage) = match surface {
        Surface::Classic => (
            "(function(){ var i = document.querySelector('.amark img'); if (!i) return 'no inline image'; i.click(); return 'ok'; })()",
            ".lightbox .lb-stage",
        ),
        Surface::AppShell => (
            "(function(){ var t = document.querySelector('[data-image-toggle]'); if (!t) return 'no toggle'; t.click(); return 'ok'; })()",
            ".image-lightbox .image-lightbox-stage",
        ),
    };
    if surface == Surface::AppShell {
        until(
            tab,
            "!!document.querySelector('[data-image-toggle]')",
            "the image row",
            Duration::from_secs(20),
            "'no toggle'",
        );
        assert_eq!(eval(tab, open_it), "ok", "the image shows");
        until(tab, "(function(){ var i = document.querySelector('.renderer-image-thumb img'); return !!i && i.naturalWidth >= 1; })()", "the thumbnail", Duration::from_secs(10), "'no thumb'");
        eval(
            tab,
            "document.querySelector('.renderer-image-thumb').click(); 'ok'",
        );
    } else {
        until(tab, "(function(){ var i = document.querySelector('.amark img'); return !!i && i.naturalWidth >= 1; })()", "the inline image", Duration::from_secs(20), "'no inline image'");
        assert_eq!(eval(tab, open_it), "ok", "the image enlarges");
    }
    until(
        tab,
        &format!("(function(){{ var s = document.querySelector('{stage}'); return !!s && s.dataset.zoom != null; }})()"),
        "the enlarged image to report a zoom level",
        Duration::from_secs(10),
        &format!("(function(){{ var s = document.querySelector('{stage}'); return s ? JSON.stringify(s.dataset) : 'no stage'; }})()"),
    );
    let fitted = eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); var i = s.querySelector('img'); var b = s.getBoundingClientRect(); var r = i.getBoundingClientRect(); return JSON.stringify({{ zoom: Number(s.dataset.zoom), pannable: s.dataset.pannable, fitsW: r.width <= b.width + 1, fitsH: r.height <= b.height + 1 }}); }})()"));
    let fitted: serde_json::Value =
        serde_json::from_str(fitted.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    // Fit means the whole image is inside the stage, and an image that fits cannot pan — the
    // grab cursor is never offered for a move that would do nothing.
    assert_eq!(
        fitted["fitsW"],
        serde_json::json!(true),
        "it opens fitted across: {fitted}"
    );
    assert_eq!(
        fitted["fitsH"],
        serde_json::json!(true),
        "it opens fitted down: {fitted}"
    );
    assert_eq!(
        fitted["pannable"],
        serde_json::json!("no"),
        "an image that fits is not draggable: {fitted}"
    );
    let before = fitted["zoom"].as_f64().unwrap_or(0.0);
    assert!(before > 0.0, "the stage reports a zoom level: {fitted}");
    // FIT CENTRES (#230). The owner: "the fit did not center the image." It did not, because the
    // stage centred by LAYOUT and a centred grid/flex item larger than its box is aligned to
    // start instead under overflow:hidden. Assert where the image actually IS, not what the
    // engine believes: its centre against the stage's, in pixels.
    let centred = eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); var i = s.querySelector('img'); var b = s.getBoundingClientRect(); var r = i.getBoundingClientRect(); return JSON.stringify({{ dx: Math.round((r.left + r.width / 2) - (b.left + s.clientLeft + s.clientWidth / 2)), dy: Math.round((r.top + r.height / 2) - (b.top + s.clientTop + s.clientHeight / 2)) }}); }})()"));
    let centred: serde_json::Value =
        serde_json::from_str(centred.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert!(
        centred["dx"].as_f64().unwrap_or(999.0).abs() <= 2.0
            && centred["dy"].as_f64().unwrap_or(999.0).abs() <= 2.0,
        "at fit the image sits in the middle of its stage: {centred}"
    );
    // Zoom in far enough that the image must exceed the stage, and it becomes pannable.
    eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); for (var n = 0; n < 8; n++) {{ var b = s.querySelector('[data-zoom=\"in\"], .lb-zoom-btn'); }} return 'ok'; }})()"));
    for _ in 0..8 {
        eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); var btns = s.querySelectorAll('button'); for (var b of btns) {{ if (b.title && b.title.indexOf('Zoom in') === 0) {{ b.click(); return 'ok'; }} }} return 'no zoom-in button'; }})()"));
    }
    let zoomed = eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); var i = s.querySelector('img'); var b = s.getBoundingClientRect(); var r = i.getBoundingClientRect(); return JSON.stringify({{ zoom: Number(s.dataset.zoom), pannable: s.dataset.pannable, wider: r.width > b.width + 1 || r.height > b.height + 1 }}); }})()"));
    let zoomed: serde_json::Value =
        serde_json::from_str(zoomed.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert!(
        zoomed["zoom"].as_f64().unwrap_or(0.0) > before,
        "zooming in raises the scale: {before} -> {zoomed}"
    );
    assert_eq!(
        zoomed["wider"],
        serde_json::json!(true),
        "the zoomed image outgrows its stage: {zoomed}"
    );
    assert_eq!(
        zoomed["pannable"],
        serde_json::json!("yes"),
        "an image too big to fit can be moved — the owner's 'move control': {zoomed}"
    );
    // PANNING ACTUALLY MOVES IT (#230). The owner: "there is no way to move the image." The old
    // case asserted `dataset.pannable === "yes"` — a flag this code sets itself, which stayed
    // true while dragging did nothing. Drag for real and measure the image's rect: a flag is not
    // evidence, pixels are.
    let moved = eval(tab, &format!("(function(){{         var s = document.querySelector('{stage}'); var i = s.querySelector('img');         var b = s.getBoundingClientRect();         var x = Math.round(b.left + s.clientLeft + s.clientWidth / 2), y = Math.round(b.top + s.clientTop + s.clientHeight / 2);         var before = i.getBoundingClientRect().left;         function ev(type, px, py) {{ s.dispatchEvent(new PointerEvent(type, {{ pointerId: 7, clientX: px, clientY: py, button: 0, buttons: 1, bubbles: true, cancelable: true }})); }}         ev('pointerdown', x, y); ev('pointermove', x - 60, y); ev('pointermove', x - 120, y); ev('pointerup', x - 120, y);         return JSON.stringify({{ shift: Math.round(i.getBoundingClientRect().left - before) }});       }})()"));
    let moved: serde_json::Value =
        serde_json::from_str(moved.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert!(
        moved["shift"].as_f64().unwrap_or(0.0) <= -100.0,
        "a drag of 120px moves the zoomed image with the pointer: {moved}"
    );

    // ZOOM TRACKS THE CURSOR (#230). The owner: "make sure the pinch zoom use the mouse position
    // as the center for zoom." Pick a point well away from the centre, note what sits under it in
    // IMAGE coordinates, wheel-zoom there, and require the same image point to still be under it.
    let tracked = eval(tab, &format!("(function(){{         var s = document.querySelector('{stage}'); var i = s.querySelector('img');         var b = s.getBoundingClientRect();         var px = Math.round(b.left + s.clientLeft + s.clientWidth * 0.28), py = Math.round(b.top + s.clientTop + s.clientHeight * 0.32);         function atPoint() {{ var r = i.getBoundingClientRect(); return {{ u: (px - r.left) / r.width, v: (py - r.top) / r.height }}; }}         var before = atPoint();         s.dispatchEvent(new WheelEvent('wheel', {{ deltaY: -240, clientX: px, clientY: py, bubbles: true, cancelable: true }}));         var after = atPoint();         return JSON.stringify({{ du: Math.round((after.u - before.u) * 1000) / 1000, dv: Math.round((after.v - before.v) * 1000) / 1000, u: Math.round(before.u * 100) / 100 }});       }})()"));
    let tracked: serde_json::Value =
        serde_json::from_str(tracked.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert!(
        tracked["du"].as_f64().unwrap_or(1.0).abs() <= 0.02
            && tracked["dv"].as_f64().unwrap_or(1.0).abs() <= 0.02,
        "the image point under the cursor stays under the cursor through a wheel zoom: {tracked}"
    );

    // And the way back is one press: fit returns the default the viewer opened at.
    eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); var btns = s.querySelectorAll('button'); for (var b of btns) {{ if (b.title && b.title.indexOf('Fit') === 0) {{ b.click(); return 'ok'; }} }} return 'no fit button'; }})()"));
    let refit = eval(tab, &format!("(function(){{ var s = document.querySelector('{stage}'); return JSON.stringify({{ zoom: Number(s.dataset.zoom), pannable: s.dataset.pannable }}); }})()"));
    let refit: serde_json::Value =
        serde_json::from_str(refit.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        refit["zoom"].as_f64().unwrap_or(-1.0),
        before,
        "Fit returns to the scale it opened at: {refit}"
    );
    assert_eq!(
        refit["pannable"],
        serde_json::json!("no"),
        "and a fitted image is once again not draggable: {refit}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_an_image_is_one_click_away() {
    let _serial = serial();
    let fx = image_fixture("scenario-oneclick-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_an_image_is_one_click_away(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_an_image_is_one_click_away() {
    let _serial = serial();
    let fx = image_fixture("scenario-oneclick-app");
    let page = open(Surface::AppShell, &fx, 2983);
    scenario_an_image_is_one_click_away(&page.tab, Surface::AppShell, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_wide_image_opens_centred() {
    let _serial = serial();
    let fx = wide_image_fixture("wide-centre-app");
    let page = open(Surface::AppShell, &fx, 2995);
    scenario_the_enlarged_image_zooms(&page.tab, Surface::AppShell, &fx);
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_wide_image_opens_centred() {
    let _serial = serial();
    let fx = wide_image_fixture("wide-centre-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_enlarged_image_zooms(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_the_enlarged_image_zooms() {
    let _serial = serial();
    let fx = big_image_fixture("scenario-zoom-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_the_enlarged_image_zooms(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_enlarged_image_zooms() {
    let _serial = serial();
    let fx = big_image_fixture("scenario-zoom-app");
    let page = open(Surface::AppShell, &fx, 2984);
    scenario_the_enlarged_image_zooms(&page.tab, Surface::AppShell, &fx);
}

// ── scenario: command output is READABLE in both themes (#229) ──────────────────────────────

/// The owner: "the output of bash is shown in a light apple green, is it intentional? Feels like
/// hard to read with light theme."
///
/// It was not intentional. The demo's terminal was a near-black slab and its output colour was
/// picked against that; #150 rethemed the box and left the two text colours behind, so the light
/// page drew #8fc59f on white — 1.97:1, against the 4.5:1 a paragraph of text needs.
///
/// This asserts the PROPERTY, not the hex. A future palette change is free to pick any colour it
/// likes; what it may not do is make output unreadable again, on either page or in either theme.
/// Measuring also catches what reading the stylesheet cannot: the colour that actually wins the
/// cascade, over the background that is actually painted behind it.
fn scenario_command_output_is_readable_in_both_themes(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // A tool call sits inside an activity fold on both pages and its record starts closed, so
    // there is nothing to measure until the reader opens it — outermost first, by its head, the
    // way `scenario_output_caps_expand_and_remember` does it.
    for _ in 0..6 {
        let step = match surface {
            Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"bash\"]')].pop(); if (!f) return 'none'; var chain = []; for (var e = f; e; e = e.parentElement.closest('.fold')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.dataset.open === '0'; }); if (!closed) return 'open'; closed.querySelector('.fold-h').click(); return 'clicked'; })()"),
            Surface::AppShell => eval(tab, "(function(){ var t = [...document.querySelectorAll('.renderer-turn[data-tool-name=\"Bash\"] > .renderer')].pop(); if (!t) return 'none'; var chain = []; for (var e = t; e; e = e.parentElement && e.parentElement.closest('.renderer')) chain.push(e); var closed = chain.reverse().find(function (x) { return x.classList.contains('closed'); }); if (!closed) return 'open'; closed.querySelector('button.renderer-head').click(); return 'clicked'; })()"),
        };
        settle();
        let step = step.as_str().unwrap_or("").to_string();
        if step == "open" || step == "none" {
            break;
        }
    }
    let output = match surface {
        Surface::Classic => ".result",
        Surface::AppShell => ".renderer-terminal .output",
    };
    // The contrast of the output's own colour against the first opaque background behind it —
    // the pair a reader's eye actually resolves, whatever the cascade did to get there.
    let measure = format!(
        "(function(){{ \
           function rgb(s) {{ var m = String(s).match(/[\\d.]+/g); return m ? m.slice(0, 3).map(Number) : null; }} \
           function lum(c) {{ var a = c.map(function (v) {{ v /= 255; return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }}); return 0.2126 * a[0] + 0.7152 * a[1] + 0.0722 * a[2]; }} \
           var els = [...document.querySelectorAll('{output}')].filter(function (e) {{ return e.getBoundingClientRect().height > 0 && e.textContent.trim(); }}); \
           if (!els.length) return JSON.stringify({{ error: 'no visible output' }}); \
           var el = els[els.length - 1]; \
           var fg = rgb(getComputedStyle(el).color); \
           var bg = null; \
           for (var p = el; p && p !== document.documentElement; p = p.parentElement) {{ \
             var c = rgb(getComputedStyle(p).backgroundColor); \
             var alpha = String(getComputedStyle(p).backgroundColor).match(/[\\d.]+/g); \
             if (c && (!alpha || alpha.length < 4 || Number(alpha[3]) > 0.95)) {{ bg = c; break; }} \
           }} \
           if (!bg) bg = rgb(getComputedStyle(document.body).backgroundColor) || [255, 255, 255]; \
           var lf = lum(fg), lb = lum(bg); \
           var ratio = (Math.max(lf, lb) + 0.05) / (Math.min(lf, lb) + 0.05); \
           return JSON.stringify({{ ratio: Math.round(ratio * 100) / 100, fg: fg, bg: bg }}); \
         }})()"
    );
    for theme in ["light", "dark"] {
        eval(
            tab,
            &format!(
                "(function(){{ document.documentElement.setAttribute('data-theme', '{theme}'); return 'ok'; }})()"
            ),
        );
        settle();
        let seen = eval(tab, &measure);
        let seen: serde_json::Value =
            serde_json::from_str(seen.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
        assert!(
            seen["error"].is_null(),
            "{surface:?}/{theme}: the case found no command output to measure: {seen}"
        );
        let ratio = seen["ratio"].as_f64().unwrap_or(0.0);
        assert!(
            ratio >= 4.5,
            "{surface:?}/{theme}: command output must clear 4.5:1 for body text, measured {ratio}:1 — {seen}"
        );
    }
    eval(
        tab,
        "(function(){ document.documentElement.removeAttribute('data-theme'); return 'ok'; })()",
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_command_output_is_readable_in_both_themes() {
    let _serial = serial();
    let fx = caps_fixture("scenario-contrast-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_command_output_is_readable_in_both_themes(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_command_output_is_readable_in_both_themes() {
    let _serial = serial();
    let fx = caps_fixture("scenario-contrast-app");
    let page = open(Surface::AppShell, &fx, 2985);
    scenario_command_output_is_readable_in_both_themes(&page.tab, Surface::AppShell, &fx);
}

// ── the information-parity probe (#231): what each page SAYS about the same record ──────────

/// A session whose records each carry a distinct, checkable fact: a path, a command, a name.
fn parity_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut transcript = long_session(12, Shape::default());
    transcript += &user_at("question p: do the work", &now_minus(200));
    transcript += &assistant_at("Reading the file first.", &now_minus(195));
    transcript += &read_tool_at(
        "p-read",
        "/Users/demo/project/src/engine/reducer.rs",
        &now_minus(190),
    );
    transcript += &tool_result_lines("p-read", 12, &now_minus(185));
    transcript += &assistant_at("Reading a slice of another file.", &now_minus(184));
    transcript += &harness::read_tool_ranged_at(
        "p-read2",
        "/Users/demo/project/src/engine/window.rs",
        1560,
        80,
        &now_minus(183),
    );
    transcript += &tool_result_lines("p-read2", 9, &now_minus(182));
    transcript += &assistant_at("Now the shell.", &now_minus(180));
    transcript += &tool_open_at("p-bash", &now_minus(175));
    transcript += &tool_result_lines("p-bash", 6, &now_minus(170));
    transcript += &assistant_at("And an edit.", &now_minus(165));
    transcript += &write_tool_at(
        "p-write",
        "/Users/demo/project/src/engine/out.py",
        8,
        &now_minus(160),
    );
    transcript += &tool_result_at("p-write", &now_minus(155));
    transcript += &assistant_at("answer p: done", &now_minus(150));
    let path = stores.claude_session(SID, &transcript);
    Fixture {
        base,
        path,
        turns: 7,
    }
}

/// Dump, per record, every string the page puts on screen for it — the head's own words and the
/// first line of its body. Compared between the pages, the difference IS the parity gap.
fn rendered_facts(tab: &headless_chrome::Tab, surface: Surface) -> serde_json::Value {
    let js = match surface {
        Surface::Classic => "(function(){ var out = []; document.querySelectorAll('#stream .fold').forEach(function (f) { var h = f.querySelector('.fold-h'); if (!h) return; out.push({ kind: f.dataset.kind || '', head: (h.textContent || '').replace(/\\s+/g, ' ').trim() }); }); return JSON.stringify(out); })()",
        Surface::AppShell => "(function(){ var out = []; document.querySelectorAll('.renderer[data-renderer]').forEach(function (r) { var h = r.querySelector('.renderer-head'); if (!h) return; out.push({ kind: r.dataset.rendererKind || '', head: (h.textContent || '').replace(/\\s+/g, ' ').trim() }); }); return JSON.stringify(out); })()",
    };
    serde_json::from_str(eval(tab, &js).as_str().unwrap_or("[]")).unwrap_or(serde_json::Value::Null)
}

/// A throwaway reporter: prints what each page says about the same records, side by side.
#[test]
#[ignore = "probe: CR_PARITY=1 cargo test --test scenarios parity_probe -- --ignored --exact --nocapture"]
fn parity_probe() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = parity_fixture(match surface {
            Surface::Classic => "parity-probe-classic",
            Surface::AppShell => "parity-probe-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 2986 };
        let page = open(surface, &fx, port);
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        // Open everything, so a fact hidden behind a fold still counts as rendered.
        for _ in 0..8 {
            let js = match surface {
                Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(function (x) { return x.dataset.open === '0'; }); if (!f) return 'done'; f.querySelector('.fold-h').click(); return 'clicked'; })()",
                Surface::AppShell => "(function(){ var r = [...document.querySelectorAll('.renderer.closed')].shift(); if (!r) return 'done'; var h = r.querySelector('button.renderer-head'); if (!h) return 'done'; h.click(); return 'clicked'; })()",
            };
            if eval(&page.tab, js).as_str() == Some("done") {
                break;
            }
            settle();
        }
        println!("=== {surface:?}");
        for row in rendered_facts(&page.tab, surface)
            .as_array()
            .unwrap_or(&vec![])
        {
            println!(
                "  [{}] {}",
                row["kind"].as_str().unwrap_or(""),
                row["head"].as_str().unwrap_or("")
            );
        }
        // Is the path TRUNCATED, and if so at which end? A deep path outruns any column; what
        // matters is whether the file NAME survives the clip.
        let sel = match surface {
            Surface::Classic => ".fold[data-kind=\"read\"] .fold-h",
            Surface::AppShell => ".renderer[data-renderer-kind=\"read\"] .renderer-target",
        };
        for width in [1400.0_f64, 1000.0, 760.0] {
            let _ = page.tab.set_bounds(headless_chrome::types::Bounds::Normal {
                left: None,
                top: None,
                width: Some(width),
                height: Some(900.0),
            });
            settle();
            let seen = eval(&page.tab, &format!("(function(){{ var e = document.querySelector('{sel}'); if (!e) return JSON.stringify({{ miss: true }}); var cs = getComputedStyle(e); return JSON.stringify({{ w: Math.round(e.getBoundingClientRect().width), clipped: e.scrollWidth > e.clientWidth + 1, dir: cs.direction, text: (e.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 70) }}); }})()"));
            println!("    width {width}: {seen}");
        }
    }
}

/// Row of the information-parity audit (#231): a Read says WHICH FILE it read, on both pages, at
/// every width a reader might use.
///
/// The owner: "the read message does not have the file path it is reading (classic view has)."
/// Measured before the fix, on an ordinary absolute path at a 1000px window: the app shell gave
/// the target 67px and clipped it; the classic page showed the path whole at every width.
///
/// The assertion is that the FILE NAME is on screen — not that the text is present in the DOM
/// (it always was) and not that nothing is clipped (a deep path outruns any column). Which end
/// gets clipped is the whole question, and `textContent` cannot answer it: a Range measured
/// against the element's own box can.
fn scenario_a_read_says_which_file(tab: &headless_chrome::Tab, surface: Surface, _fx: &Fixture) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // A tool call sits inside an activity fold, so open everything first — a head with no width
    // is a head nobody is looking at, and measuring it answers a question the case is not asking.
    for _ in 0..10 {
        let js = match surface {
            Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(function (x) { return x.dataset.open === '0'; }); if (!f) return 'done'; f.querySelector('.fold-h').click(); return 'clicked'; })()",
            Surface::AppShell => "(function(){ var r = [...document.querySelectorAll('.renderer.closed')].shift(); if (!r) return 'done'; var h = r.querySelector('button.renderer-head'); if (!h) return 'done'; h.click(); return 'clicked'; })()",
        };
        if eval(tab, js).as_str() == Some("done") {
            break;
        }
        settle();
    }
    let sel = match surface {
        Surface::Classic => ".fold[data-kind=\"read\"] .fold-h",
        Surface::AppShell => ".renderer[data-renderer-kind=\"read\"] .renderer-target",
    };
    // The visible span of the LAST characters of the path — the file name — against the box that
    // clips them. Rendered outside it, the reader cannot see what was read.
    let probe = format!(
        "(function(){{ \
           var e = document.querySelector('{sel}'); if (!e) return JSON.stringify({{ miss: true }}); \
           var text = (e.textContent || ''); var name = 'reducer.rs'; \
           var at = text.lastIndexOf(name); if (at < 0) return JSON.stringify({{ absent: true, text: text.slice(0, 80) }}); \
           var walker = document.createTreeWalker(e, NodeFilter.SHOW_TEXT), node, seen = 0, range = document.createRange(), done = false; \
           while ((node = walker.nextNode())) {{ \
             var len = node.nodeValue.length; \
             if (!done && seen + len >= at + name.length) {{ \
               range.setStart(node, Math.max(0, at - seen)); range.setEnd(node, Math.min(len, at - seen + name.length)); done = true; break; \
             }} \
             seen += len; \
           }} \
           if (!done) return JSON.stringify({{ unmeasurable: true }}); \
           var r = range.getBoundingClientRect(), b = e.getBoundingClientRect(); \
           return JSON.stringify({{ visible: r.width > 0 && r.left >= b.left - 1 && r.right <= b.right + 1, nameLeft: Math.round(r.left - b.left), boxW: Math.round(b.width) }}); \
         }})()"
    );
    for width in [1400.0_f64, 1100.0, 900.0] {
        let _ = tab.set_bounds(headless_chrome::types::Bounds::Normal {
            left: None,
            top: None,
            width: Some(width),
            height: Some(900.0),
        });
        settle();
        let seen = eval(tab, &probe);
        let seen: serde_json::Value =
            serde_json::from_str(seen.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
        assert!(
            seen["miss"].is_null() && seen["absent"].is_null(),
            "{surface:?} at {width}px: the Read head names no file: {seen}"
        );
        assert_eq!(
            seen["visible"],
            serde_json::json!(true),
            "{surface:?} at {width}px: the file NAME must be on screen — a deep path may lose its \
             prefix, never its tail: {seen}"
        );
    }
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_read_says_which_file() {
    let _serial = serial();
    let fx = parity_fixture("parity-read-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_read_says_which_file(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_read_says_which_file() {
    let _serial = serial();
    let fx = parity_fixture("parity-read-app");
    let page = open(Surface::AppShell, &fx, 2987);
    scenario_a_read_says_which_file(&page.tab, Surface::AppShell, &fx);
}

/// Information parity (#231): a record NAMES ITS TARGET whether or not it has a body to unfold.
///
/// The owner's screenshots: the classic page showed `Read /private/tmp/…/scratchpad/pre…` and the
/// app shell showed a bare `Read` with nothing after it. Every Read in that session read an
/// IMAGE, so its result became a separate attachment record and the Read itself was left with no
/// body — `noninteractive`, whose head hard-coded an empty target span while the interactive one
/// interpolated the summary. What a record is about must not depend on whether it has something
/// to open.
fn scenario_a_bodyless_record_still_names_its_target(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    for _ in 0..10 {
        let js = match surface {
            Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(function (x) { return x.dataset.open === '0'; }); if (!f) return 'done'; f.querySelector('.fold-h').click(); return 'clicked'; })()",
            Surface::AppShell => "(function(){ var r = [...document.querySelectorAll('.renderer.closed')].shift(); if (!r) return 'done'; var h = r.querySelector('button.renderer-head'); if (!h) return 'done'; h.click(); return 'clicked'; })()",
        };
        if eval(tab, js).as_str() == Some("done") {
            break;
        }
        settle();
    }
    // The Read that reads an image: on the app shell it has no body of its own, which is exactly
    // the head variant that used to drop the path.
    let seen = match surface {
        Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold[data-kind=\"read\"]')].pop(); if (!f) return JSON.stringify({ miss: true }); var h = f.querySelector('.fold-h'); return JSON.stringify({ text: (h.textContent || '').replace(/\\s+/g, ' ').trim() }); })()"),
        Surface::AppShell => eval(tab, "(function(){ var r = [...document.querySelectorAll('.renderer[data-renderer-kind=\"read\"]')].pop(); if (!r) return JSON.stringify({ miss: true }); var t = r.querySelector('.renderer-target'); return JSON.stringify({ interactive: !!r.querySelector('button.renderer-head'), text: t ? (t.textContent || '').trim() : null }); })()"),
    };
    let seen: serde_json::Value =
        serde_json::from_str(seen.as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null);
    assert!(
        seen["miss"].is_null(),
        "{surface:?}: no Read record found: {seen}"
    );
    let text = seen["text"].as_str().unwrap_or("");
    assert!(
        text.contains("shot.png"),
        "{surface:?}: the Read names the file it read, body or no body: {seen}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_bodyless_record_still_names_its_target() {
    let _serial = serial();
    let fx = image_fixture("parity-bodyless-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_bodyless_record_still_names_its_target(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_bodyless_record_still_names_its_target() {
    let _serial = serial();
    let fx = image_fixture("parity-bodyless-app");
    let page = open(Surface::AppShell, &fx, 2988);
    scenario_a_bodyless_record_still_names_its_target(&page.tab, Surface::AppShell, &fx);
}

// ── the information-parity audit (#232) ─────────────────────────────────────────────────────
//
// `rendering_audit.rs` (#174) measures what a CONTROL reaches, in computed style. This measures
// what the page SAYS at rest: per record, the words a reader can read.
//
// The virtual window is not an excuse. The app shell mounts only its window's slice, so a naive
// comparison reports every unmounted record as a gap and buries the real ones — an early probe
// read 13 rows on the classic page and 10 on the shell, and all three differences were
// windowing. Both pages open with `?mountall=1`, the engine's test knob, which forces `renderAll`
// and mounts the lot. The fixture is deliberately small: that knob is safe only on a session
// that fits in a tab.
//
// The fixture mimics STRUCTURAL PATTERNS rather than any real session — shapes counted from the
// live transcripts on this machine, by kind, never by content.

/// Every structural pattern the survey found, once each, in one small session.
fn pattern_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(3, Shape::default());
    t += &user_at("question: work through every shape", &now_minus(300));
    // The three dominant single-kind assistant turns.
    t += &assistant_at("Starting with the file.", &now_minus(295));
    t += &thinking_at("Considering which file to open first.", &now_minus(294));
    t += &read_tool_at(
        "k-read",
        "/Users/demo/proj/src/engine/reducer.rs",
        &now_minus(293),
    );
    t += &tool_result_lines("k-read", 10, &now_minus(292));
    // A Read whose result is an IMAGE: the record keeps no body of its own, the shape that lost
    // its path on the app shell (#231).
    t += &assistant_at("Now the screenshot.", &now_minus(285));
    t += &read_tool_at("k-shot", "/Users/demo/proj/shot.png", &now_minus(284));
    t += &image_result_at("k-shot", &now_minus(283));
    // A result carrying an image AND text — 309 in the survey, and neither page had a fixture.
    t += &assistant_at("And one that returns both.", &now_minus(275));
    t += &named_tool_at(
        "k-mixed",
        "Monitor",
        "/Users/demo/proj/render.log",
        &now_minus(274),
    );
    t += &harness::mixed_result_at("k-mixed", "rendered 3 frames", &now_minus(273));
    // Text, thinking and a call in ONE assistant message — where a page must choose an order.
    t += &harness::combined_turn_at(
        "k-comb",
        "Checking the other half.",
        "The second file is the one that matters.",
        "Read",
        "/Users/demo/proj/src/engine/window.rs",
        &now_minus(265),
    );
    t += &tool_result_lines("k-comb", 6, &now_minus(264));
    // A command with real output, an edit, and a write.
    t += &assistant_at("Running the checks.", &now_minus(255));
    t += &tool_open_at("k-bash", &now_minus(254));
    t += &tool_result_lines("k-bash", 24, &now_minus(253));
    t += &assistant_at("Applying the change.", &now_minus(245));
    t += &edit_tool_at(
        "k-edit",
        "/Users/demo/proj/src/engine/reducer.rs",
        &now_minus(244),
    );
    t += &tool_result_at("k-edit", &now_minus(243));
    t += &assistant_at("And writing the new one.", &now_minus(235));
    t += &write_tool_at(
        "k-write",
        "/Users/demo/proj/src/engine/out.py",
        6,
        &now_minus(234),
    );
    t += &tool_result_at("k-write", &now_minus(233));
    // An MCP tool, whose name outruns any column.
    t += &assistant_at("Driving the browser.", &now_minus(225));
    t += &named_tool_at(
        "k-mcp",
        "mcp__claude-in-chrome__javascript_tool",
        "document.querySelectorAll('.row').length",
        &now_minus(224),
    );
    t += &tool_result_text("k-mcp", "42", &now_minus(223));
    // BATCH 2 — the shapes the deeper survey turned up.
    // A call that FAILED: 1837 results across the surveyed transcripts carry `is_error`, and a
    // page has to say the call failed, not merely what it returned.
    t += &assistant_at("Trying the one that breaks.", &now_minus(219));
    t += &tool_open_at("k-fail", &now_minus(218));
    t += &harness::error_result_at(
        "k-fail",
        "exit 1: no such file or directory",
        &now_minus(217),
    );
    // Markdown is INFORMATION: a table or a code block the reader cannot see on one page is a
    // gap, not a preference. Heading, bullets, table, fence, CJK and a 340-character line.
    t += &harness::markdown_answer_at(&now_minus(210));
    // BATCH 3 — the records that are not a tool call at all, and that every page draws its own
    // way: a compaction boundary, a prompt queued while the agent was busy, and an agent asking
    // the reader a question. 13039 queue operations and 5565 sidechain records in the survey.
    t += &harness::compaction_at(&now_minus(208));
    t += &queued_at("and then check the tests", &now_minus(206));
    t += &harness::input_request_at("k-ask", "Which branch should I use?", &now_minus(204));
    t += &assistant_at("answer: every shape is above", &now_minus(200));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 4,
    }
}

/// Every FACT a page shows for one record: the words in its head, the words in its body, and the
/// handful of computed properties that decide whether those words are readable.
///
/// Keyed by the record's own identity rather than its position, so the two pages can be compared
/// even where one of them groups differently. `kind` is normalised across the pages' two spellings
/// (`act` / `activity`); everything else is what the reader sees.
fn record_facts(tab: &headless_chrome::Tab, surface: Surface) -> serde_json::Value {
    let js = match surface {
        Surface::Classic => {
            r#"(function(){
            var out = {};
            document.querySelectorAll('#stream .fold, #stream .qmarker, #stream .amark').forEach(function (f) {
              var h = f.querySelector('.fold-h') || f;
              var kind = (f.dataset.kind || '').replace(/^act$/, 'activity');
              var head = (h.textContent || '').replace(/\s+/g, ' ').replace(/^[▸▾▴◂]\s*/, '').replace(/#$/, '').trim();
              var body = f.querySelector('.fold-b, .foldbody, .blkbody');
              var cs = getComputedStyle(h);
              var key = kind + '|' + head;
              out[key] = { kind: kind, head: head,
                body: body ? (body.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 400) : '',
                color: cs.color, fontFamily: cs.fontFamily.split(',')[0].replace(/["']/g, '') };
            });
            return JSON.stringify(out);
          })()"#
        }
        Surface::AppShell => {
            r#"(function(){
            var out = {};
            document.querySelectorAll('.renderer[data-renderer]').forEach(function (r) {
              var h = r.querySelector('.renderer-head'); if (!h) return;
              var kind = (r.dataset.rendererKind || '').replace(/^act$/, 'activity');
              var head = (h.textContent || '').replace(/\s+/g, ' ').trim();
              var body = r.querySelector('.renderer-output');
              var cs = getComputedStyle(h);
              var key = kind + '|' + head;
              out[key] = { kind: kind, head: head,
                body: body ? (body.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 400) : '',
                color: cs.color, fontFamily: cs.fontFamily.split(',')[0].replace(/["']/g, '') };
            });
            return JSON.stringify(out);
          })()"#
        }
    };
    serde_json::from_str(eval(tab, js).as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null)
}

/// Per record: its kind, and the words the page shows in its head.
fn head_facts(tab: &headless_chrome::Tab, surface: Surface) -> Vec<(String, String)> {
    let js = match surface {
        Surface::Classic => "(function(){ var out = []; document.querySelectorAll('#stream .fold, #stream .qmarker, #stream .amark').forEach(function (e) { var h = e.querySelector('.fold-h') || e; var kind = e.dataset.kind || (e.classList.contains('qmarker') ? 'queue' : e.classList.contains('amark') ? 'attachment' : ''); out.push([kind, (h.textContent || '').replace(/\\s+/g, ' ').replace(/#$/, '').trim()]); }); return JSON.stringify(out); })()",
        Surface::AppShell => "(function(){ var out = []; document.querySelectorAll('.renderer[data-renderer]').forEach(function (r) { var h = r.querySelector('.renderer-head'); if (!h) return; out.push([r.dataset.rendererKind || '', (h.textContent || '').replace(/\\s+/g, ' ').trim()]); }); return JSON.stringify(out); })()",
    };
    let raw: Vec<Vec<String>> =
        serde_json::from_str(eval(tab, &js).as_str().unwrap_or("[]")).unwrap_or_default();
    raw.into_iter()
        .filter_map(|row| Some((row.first()?.clone(), row.get(1)?.clone())))
        .collect()
}

/// Open every fold, so nothing is hidden behind one when the page is read.
fn open_everything(tab: &headless_chrome::Tab, surface: Surface) {
    for _ in 0..40 {
        let js = match surface {
            Surface::Classic => "(function(){ var f = [...document.querySelectorAll('#stream .fold')].find(function (x) { return x.dataset.open === '0'; }); if (!f) return 'done'; f.querySelector('.fold-h').click(); return 'clicked'; })()",
            Surface::AppShell => "(function(){ var r = [...document.querySelectorAll('.renderer.closed')].shift(); if (!r) return 'done'; var h = r.querySelector('button.renderer-head'); if (!h) return 'done'; h.click(); return 'clicked'; })()",
        };
        if eval(tab, js).as_str() == Some("done") {
            break;
        }
        settle();
    }
}

/// The reporter: prints what each page says about every record, and what only one of them says.
#[test]
#[ignore = "audit: cargo test --test scenarios information_parity_audit -- --ignored --exact --nocapture"]
fn information_parity_audit() {
    let _serial = serial();
    let mut seen: Vec<(Surface, Vec<(String, String)>)> = Vec::new();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = pattern_fixture(match surface {
            Surface::Classic => "parity-audit-classic",
            Surface::AppShell => "parity-audit-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 2989 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        // Head AND body: a page may carry a fact in either, and only the pair says whether the
        // reader can see it at all. The queued prompt is the case in point — classic puts its
        // words in the marker, the shell puts a label in the head and the words underneath.
        let bodies = record_facts(&page.tab, surface);
        seen.push((surface, head_facts(&page.tab, surface)));
        println!("\n=== {surface:?} BODIES");
        if let Some(map) = bodies.as_object() {
            for (key, v) in map {
                let body = v["body"].as_str().unwrap_or("");
                if !body.is_empty() {
                    println!("  {key} => {}", body.chars().take(90).collect::<String>());
                }
            }
        }
    }
    let (_, classic) = &seen[0];
    let (_, shell) = &seen[1];
    println!(
        "\n=== rows: classic {} | app shell {}",
        classic.len(),
        shell.len()
    );
    println!("\n=== CLASSIC");
    for (kind, text) in classic {
        println!("  [{kind}] {text}");
    }
    println!("\n=== APP SHELL");
    for (kind, text) in shell {
        println!("  [{kind}] {text}");
    }
    // What one page names and the other does not, by kind — the gap list.
    let kinds_of = |rows: &Vec<(String, String)>| {
        rows.iter()
            .map(|(k, _)| k.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let ck = kinds_of(classic);
    let sk = kinds_of(shell);
    println!(
        "\n=== KINDS only on classic: {:?}",
        ck.difference(&sk).collect::<Vec<_>>()
    );
    println!(
        "=== KINDS only on the shell: {:?}",
        sk.difference(&ck).collect::<Vec<_>>()
    );
}

/// Markdown is INFORMATION, not decoration: a table rendered as a table on one page and as raw
/// pipes on the other is a parity gap the reader pays for. Counted across the surveyed
/// transcripts, an answer carries a heading 1283 times, a bullet list 2294, a fenced block 1238,
/// a table 1060, CJK 3220, and a line past 300 characters 33763 times.
///
/// The structures are counted, not compared as text: each page is free to style them, and free
/// to differ in whitespace, but not to LOSE one.
fn prose_structures(tab: &headless_chrome::Tab, surface: Surface) -> serde_json::Value {
    let root = match surface {
        Surface::Classic => "#stream",
        Surface::AppShell => ".virtual-window",
    };
    let js = format!(
        "(function(){{ \
           var r = document.querySelector('{root}'); if (!r) return JSON.stringify({{ miss: true }}); \
           var t = r.textContent || ''; \
           return JSON.stringify({{ \
             headings: r.querySelectorAll('h1,h2,h3,h4,h5,h6,[class^=\"md-h\"],[class*=\" md-h\"]').length, \
             listItems: r.querySelectorAll('li').length, \
             tables: r.querySelectorAll('table').length, \
             cells: r.querySelectorAll('td,th').length, \
             codeBlocks: r.querySelectorAll('pre code, pre.code, code.block').length, \
             cjk: (t.match(/[\\u4e00-\\u9fff]/g) || []).length, \
             longRun: (t.match(/x{{300,}}/g) || []).length \
           }}); }})()"
    );
    serde_json::from_str(eval(tab, &js).as_str().unwrap_or("{}")).unwrap_or(serde_json::Value::Null)
}

/// A compaction that carried five files over, then one that carried a single file (#254).
fn carried_files_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question: keep going after the compaction", &now_minus(200));
    t += &harness::compaction_at(&now_minus(198));
    for (i, f) in [
        "packages/core/test/renames.test.ts",
        "packages/core/src/renames.ts",
        "packages/web/src/strings.zh-Hans.ts",
        "packages/web/test/app.test.tsx",
        "packages/web/test/i18n.test.ts",
    ]
    .iter()
    .enumerate()
    {
        t += &harness::carried_file_at(&format!("/w/{f}"), f, &now_minus(196 - i as u64));
    }
    t += &assistant_at("Carrying on from the summary.", &now_minus(188));
    // …and a LONE one, which must stay a plain line rather than become a run of one.
    t += &harness::carried_file_at(
        "/w/packages/cli/src/bin.ts",
        "packages/cli/src/bin.ts",
        &now_minus(186),
    );
    t += &assistant_at("And the last of it.", &now_minus(184));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #254 — a file carried over by a compaction is a POINTER, and must not be dressed as a record
/// with something inside. The owner saw four of them after a compaction and read them as four
/// updates stripped of their content: each wore a bold filename, a chevron, a card and two
/// buttons, and carried nothing at all.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_carried_files_collapse_and_wear_no_card() {
    let _serial = serial();
    let fx = carried_files_fixture("carried-files-app");
    let page = open_with(Surface::AppShell, &fx, 3004, "mountall=1");
    jump_to_end(&page.tab, Surface::AppShell);
    await_tail(
        &page.tab,
        Surface::AppShell,
        "a fresh open to land at the tail",
    );
    settle();
    let seen: serde_json::Value = eval(
        &page.tab,
        "(function(){ var runs = [...document.querySelectorAll('.pointer-run')]; \
           return JSON.stringify({ \
             runs: runs.length, \
             counted: runs.map(function (r) { return Number(r.dataset.pointerRun || 0); }), \
             files: runs.map(function (r) { return r.querySelectorAll('.pointer-run-file').length; }), \
             lead: runs.length ? runs[0].querySelector('.pointer-run-lead').textContent : '', \
             cards: document.querySelectorAll('[data-renderer-kind=\"attachment\"] .renderer-note').length, \
             loneLines: [...document.querySelectorAll('[data-renderer-kind=\"attachment\"]')].filter(function (r) { return r.classList.contains('noninteractive'); }).length \
           }); })()",
    )
    .as_str()
    .and_then(|s| serde_json::from_str(s).ok())
    .unwrap_or(serde_json::Value::Null);

    assert_eq!(
        seen["runs"].as_i64(),
        Some(1),
        "the five files the compaction carried collapse into ONE line, and the lone one that \
         follows does NOT become a run of one — wrapping a single light line in a summary would \
         be more furniture, not less: {seen}"
    );
    assert_eq!(
        seen["counted"][0].as_i64(),
        Some(5),
        "…and the line says how many: {seen}"
    );
    assert_eq!(
        seen["files"][0].as_i64(),
        Some(5),
        "…while still naming every one of them, so nothing is hidden by the collapse: {seen}"
    );
    assert!(
        seen["lead"].as_str().unwrap_or("").contains("carried"),
        "…and says WHY they are there: {seen:?}"
    );
    assert_eq!(
        seen["cards"].as_i64(),
        Some(0),
        "no pointer wears a card with a heading and buttons — that furniture is what made these \
         read as emptied-out updates: {seen}"
    );
    assert!(
        seen["loneLines"].as_i64().unwrap_or(0) >= 1,
        "the lone carried file still renders, as a single head-only line: {seen}"
    );
}

/// A session that ran `/context` (#235). The client records a slash command on a
/// `system`/`local_command` record and its stdout on the next one; the server parses that stdout
/// into a `ctx` report. Written as the client writes it, so the parse path is the real one.
fn context_report_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question: how full is the context", &now_minus(60));
    t += "{\"type\":\"system\",\"subtype\":\"local_command\",\"cwd\":\"/w\",\"timestamp\":\"2026-09-18T01:00:01.000Z\",\"content\":\"<command-name>/context</command-name>\\n<command-message>context</command-message>\\n<command-args></command-args>\"}\n";
    // Real glyphs, not `\\u{...}`: JSON has no such escape, and an unparseable line is a DROPPED
    // record — which looks exactly like the bug this case exists to catch.
    t += concat!(
        "{\"type\":\"system\",\"subtype\":\"local_command\",\"cwd\":\"/w\",",
        "\"timestamp\":\"2026-09-18T01:00:02.000Z\",\"content\":\"<local-command-stdout> Context Usage",
        "\\n⛁ ⛶   Opus 5 (1M context)",
        "\\n⛶ ⛶   99.2k/1m tokens (10%)",
        "\\n⛶ ⛶   ⛁ Messages: 69.7k tokens (7.0%)",
        "\\n⛶ ⛶   ⛶ Free space: 867.1k (86.7%)</local-command-stdout>\"}\n"
    );
    // Work AFTER the report: the #245 regression was invisible from the report itself — the
    // throw took the eleven records that FOLLOWED it off the page, and this is what sees that.
    t += &assistant_at("answer: plenty of room left", &now_minus(40));
    t += &read_tool_at("ctx-read", "/Users/demo/proj/src/main.rs", &now_minus(38));
    t += &tool_result_lines("ctx-read", 6, &now_minus(37));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 14,
    }
}

/// #235/#245 — BOTH pages draw `/context` as a report, AND keep rendering afterwards.
///
/// The second half is the point. #235 shipped this rendering with no browser case, and the
/// classic branch called `host.appendChild` where every sibling branch uses `into`; `var host`
/// is declared by the `pre` branch further down and `var` hoists, so the name existed, was
/// `undefined`, and threw a TypeError mid-render. The report itself looked fine in the places
/// anyone checked — what vanished was the ELEVEN RECORDS AFTER IT, which is why this case
/// asserts on what follows the report rather than on the report alone.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_render_a_context_report_and_keep_going() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 2994)] {
        let fx = context_report_fixture(match surface {
            Surface::Classic => "ctx-report-classic",
            _ => "ctx-report-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        settle();
        // A command card is collapsed by default on the shell (`button.command-head`,
        // aria-expanded=false) and `open_everything` only reaches `.renderer.closed`, so the
        // output has to be asked for the way a reader would ask for it.
        for _ in 0..10 {
            let clicked = eval(
                &page.tab,
                "(function(){ var b = [...document.querySelectorAll('[data-prompt-toggle]')].find(function (x) { return x.getAttribute('aria-expanded') === 'false'; }); if (!b) return 'done'; b.click(); return 'clicked'; })()",
            );
            if clicked.as_str() == Some("done") {
                break;
            }
            settle();
        }
        let js = "(function(){ var r = document.querySelector('.ctx-report'); \
             return JSON.stringify({ report: !!r, \
               rows: document.querySelectorAll('.ctx-table tr').length, \
               reads: document.body.textContent.indexOf('main.rs') >= 0, \
               text: r ? r.textContent.slice(0, 200) : '' }); })()";
        let got: serde_json::Value = eval(&page.tab, js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            got["report"],
            serde_json::Value::Bool(true),
            "{surface:?} draws /context as a report rather than terminal art: {got}"
        );
        assert!(
            got["text"].as_str().unwrap_or("").contains("Messages"),
            "{surface:?} shows the categories the client reported: {got}"
        );
        assert_eq!(
            got["reads"],
            serde_json::Value::Bool(true),
            "{surface:?} KEEPS RENDERING after the report — the #245 regression threw here and \
             silently dropped every record that followed, so the Read below it is the assertion \
             that matters: {got}"
        );
    }
}

/// A session the agent CLIENT recorded its own cost for (#240). Claude Code writes `cost-state`
/// per CLI PROCESS, so the two epochs here are what a resumed session looks like: one run that
/// reached $7.50 and a second that RESTARTED its counter at $2.25. The session's own traffic
/// runs on either side of both windows, which is what makes the tally a partial figure — and
/// the whole point of the feature is that a partial figure is never shown bare.
fn client_cost_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    // Tall enough to scroll: the page is not "rendered" until it exceeds three viewports, and
    // a short fixture times out in the opener rather than failing on the assertion.
    let mut t = long_session(12, Shape::default());
    // Traffic BEFORE the client ever started counting.
    t += &user_at("question: what did this cost", &now_minus(600));
    t += &assistant_at("answer: let me total it up", &now_minus(599));
    // 2026-08-27T00:00:00Z for two hours, then a second run a day later for one hour. Fixed
    // instants, not `now_minus`: the assertion is about the DATES the page prints.
    const RUN_ONE: i64 = 1_787_788_800_000;
    const RUN_TWO: i64 = 1_787_788_800_000 + 86_400_000;
    for (start, usd, dur) in [
        (RUN_ONE, 3.10_f64, 3_600_000_i64),
        (RUN_ONE, 7.50, 7_200_000),
        (RUN_TWO, 2.25, 3_600_000),
    ] {
        t += &format!(
            "{{\"type\":\"cost-state\",\"sessionId\":\"{SID}\",\"startTime\":{start},\
             \"totalCostUSD\":{usd},\"totalDuration\":{dur},\"hasUnknownModelCost\":false}}\n"
        );
    }
    // ...and traffic AFTER it stopped, so the window cannot cover the session.
    t += &assistant_at("answer: and here is the rest of the work", &now_minus(30));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// Read the client-cost row off whichever page is in front of us. Classic writes it into the
/// usage box as its own row; the shell puts it in the session-info group. Both must show the
/// figure AND the window it covers.
fn client_cost_row(tab: &headless_chrome::Tab, surface: Surface) -> serde_json::Value {
    // No regex in the probe: the escaping layers between this Rust string and the page eat a
    // backslash, so a `\s` written here arrives as a literal-backslash match and the eval
    // returns nothing at all. Whitespace is flattened in Rust below, where it is visible. The
    // page JSON-stringifies its own answer for the same reason.
    let js = match surface {
        Surface::Classic => "(function(){ var r = document.querySelector('#usage .urow.reported'); if (!r) return JSON.stringify({ found: false, box: (document.getElementById('usage')||{}).innerText || '' }); return JSON.stringify({ found: true, text: r.innerText, title: r.title || '' }); })()",
        // `textContent`, not `innerText`: the shell's info group can be rendered while folded,
        // and innerText answers "" for a row that is not laid out — which reads exactly like a
        // row that rendered blank.
        _ => "(function(){ var out = null; document.querySelectorAll('#navigatorSession .session-info-row').forEach(function(r){ var k = r.querySelector('span'), v = r.querySelector('strong'); if (k && k.textContent.trim() === 'client cost') out = { found: true, text: k.textContent + ' ' + (v ? v.textContent : ''), title: r.title || '' }; }); return JSON.stringify(out || { found: false, box: (document.getElementById('navigatorSession')||{}).textContent || '' }); })()",
    };
    let raw = eval(tab, js);
    let mut v: serde_json::Value = raw
        .as_str()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .unwrap_or(serde_json::Value::Null);
    if let Some(flat) = v
        .get("text")
        .and_then(|t| t.as_str())
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
    {
        v["text"] = serde_json::Value::String(flat);
    }
    v
}

/// #240 — BOTH pages show what the client recorded, and neither shows it bare. The figure is
/// the SUM over process epochs of each epoch's highest reading ($7.50 + $2.25), never the last
/// record ($2.25): on a real session that difference was 31x.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_show_the_clients_own_cost_with_the_window_it_covers() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 2992)] {
        let fx = client_cost_fixture(match surface {
            Surface::Classic => "client-cost-classic",
            _ => "client-cost-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        if surface != Surface::Classic {
            // The shell keeps the rows behind the session card; open it and wait for a render.
            harness::eval(&page.tab, "var c = document.querySelector('[data-nav-card=\"session\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"session\"]').click(); 'ok'");
            harness::until(
                &page.tab,
                "document.querySelectorAll('#navigatorSession .session-info-row').length > 0",
                "the session-info rows to render",
                std::time::Duration::from_secs(10),
                "document.getElementById('navigatorSession').innerText.slice(0, 200)",
            );
        }
        let row = client_cost_row(&page.tab, surface);
        assert_eq!(
            row["found"],
            serde_json::Value::Bool(true),
            "{surface:?} shows the client's own recorded cost: {row}"
        );
        let text = row["text"].as_str().unwrap_or("");
        assert!(
            text.contains("$9.75"),
            "{surface:?} sums the PER-EPOCH MAXIMA ($7.50 + $2.25 = $9.75). $2.25 alone means it \
             took the last record, which is one CLI run's subtotal; $12.85 means it summed every \
             record instead of each epoch's highest. Got: {text:?}"
        );
        // The dates are formatted in the VIEWER's locale, so the assertion reads the day
        // numbers and our own word rather than "Aug" — this Chrome renders "8月27日".
        // Both pages must also carry the EXPLANATION, not just the dates — an explanation one
        // shell has and the other does not is an information gap between them.
        let title = row["title"].as_str().unwrap_or("");
        assert!(
            title.contains("part of the session"),
            "{surface:?} explains on hover why the figure is partial. Got: {title:?}"
        );
        assert!(
            text.contains("counted") && text.contains("27") && text.contains("28"),
            "{surface:?} prints the WINDOW beside the figure — a bare number reads as the session \
             total, and this one covers two runs (Aug 27 and Aug 28) out of a longer session. \
             Got: {text:?}"
        );
    }
}

/// The audit as an ASSERTION: every structure one page renders, the other renders too.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_render_the_same_prose_structures() {
    let _serial = serial();
    let mut seen = Vec::new();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = pattern_fixture(match surface {
            Surface::Classic => "parity-prose-classic",
            Surface::AppShell => "parity-prose-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 2990 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        seen.push(prose_structures(&page.tab, surface));
    }
    let (classic, shell) = (&seen[0], &seen[1]);
    assert!(
        classic["miss"].is_null() && shell["miss"].is_null(),
        "both pages have a stream to read: classic {classic}, shell {shell}"
    );
    for key in [
        "headings",
        "listItems",
        "tables",
        "cells",
        "codeBlocks",
        "cjk",
        "longRun",
    ] {
        let c = classic[key].as_i64().unwrap_or(-1);
        let s = shell[key].as_i64().unwrap_or(-1);
        assert!(
            c > 0 || s > 0,
            "the fixture exercises `{key}` on at least one page — a count of zero on both means \
             the pattern never reached the page and the row proves nothing: classic {classic}, \
             shell {shell}"
        );
        assert_eq!(
            c, s,
            "`{key}`: both pages render the same markdown structures — classic {c}, shell {s}\n  \
             classic {classic}\n  shell   {shell}"
        );
    }
}

// ── does a reader's unfold survive the turn growing under it? (#233) ────────────────────────

/// A live turn that keeps producing activity, as the owner described: "the agent is running a
/// long turn and producing a sequence of activities. User unfolds the tailing turn, but they
/// observe the turn would re-fold when new messages arrive."
fn growing_turn_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(10, Shape::default());
    t += &user_at("question live: keep working for a while", &now_minus(120));
    t += &assistant_at("Starting the long stretch.", &now_minus(118));
    t += &tool_open_at("g-1", &now_minus(116));
    t += &tool_result_lines("g-1", 8, &now_minus(115));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 11,
    }
}

/// A session whose tail turn ALREADY holds more events than the cap when the page opens — the
/// historical case (#250). Nothing here grew under the reader, so nothing may be stamped.
fn settled_big_turn_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(10, Shape::default());
    t += &user_at("question: do a long stretch of work", &now_minus(400));
    t += &assistant_at("Working through it.", &now_minus(398));
    // WRITES, not Bash: consecutive Bash/Read/thinking calls COALESCE into a single activity
    // event, so ten of them render as one and sit far under the cap — the assertion would then
    // pass for the wrong reason. Edit/Write/Skill stand alone, which is what gives ten events.
    for k in 0..10u64 {
        t += &write_tool_at(
            &format!("s-{k}"),
            &format!("/w/src/mod_{k}.py"),
            5,
            &now_minus(396 - k * 4),
        );
        t += &tool_result_lines(&format!("s-{k}"), 2, &now_minus(395 - k * 4));
    }
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 11,
    }
}

/// #250 — the tail turn is LIST-ALL while it is live, and stays that way; a turn that was
/// already finished when the page opened is CONCISE.
///
/// The reader's intention to watch is not a control and not a preference: it is the turn being
/// live in front of them. So a block that GROWS while the page is open shows every top-level
/// message, with no "Show N more" and no click — and keeps it once the turn ends, because the
/// stamp is sticky. A block that was complete before the page opened never grew under the
/// reader and is left alone, which is what bounds the whole thing to how long they watched.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_live_turn_lists_everything_and_a_settled_one_does_not() {
    let _serial = serial();
    // (1) The historical half: a big turn that was over before the page opened stays concise.
    let settled = settled_big_turn_fixture("live-mode-settled");
    // `mountall=1`: the shell virtualizes, and the block under test is one of many — without
    // this the probe reads whichever surfaces happened to be mounted and proves nothing.
    let page = open_with(Surface::AppShell, &settled, 3002, "mountall=1");
    jump_to_end(&page.tab, Surface::AppShell);
    await_tail(
        &page.tab,
        Surface::AppShell,
        "a fresh open to land at the tail",
    );
    settle();
    // The BIGGEST block is the one built above; the padding turns each hold a single call.
    let cold = eval(&page.tab, "(function(){ var ps = [...document.querySelectorAll('[data-process-surface]')]; \
         if (!ps.length) return JSON.stringify({miss:'none'}); \
         var p = ps.reduce(function (a, b) { return b.querySelectorAll('.process-event').length > a.querySelectorAll('.process-event').length ? b : a; }); \
         return JSON.stringify({ events: p.querySelectorAll('.process-event').length, \
           hiddenEvents: p.querySelectorAll('.process-event.progressive-hidden').length, \
           more: p.querySelectorAll('[data-process-more]').length }); })()");
    let cold: serde_json::Value = cold
        .as_str()
        .and_then(|x| serde_json::from_str(x).ok())
        .unwrap_or(serde_json::Value::Null);
    assert!(
        cold["events"].as_i64().unwrap_or(0) > 7,
        "the fixture's big turn is past the cap, or this proves nothing: {cold}"
    );
    assert_eq!(
        cold["more"].as_i64(),
        Some(1),
        "a turn that was already finished when the page opened renders CONCISE — it never grew \
         under the reader, so nothing stamped it and its cap stands: {cold}"
    );
    assert!(
        cold["hiddenEvents"].as_i64().unwrap_or(0) > 0,
        "…with the messages past the cap actually hidden: {cold}"
    );
    drop(page);

    // (2) The live half: a turn that grows under the reader lists everything, with no click.
    let fx = growing_turn_fixture("live-mode-growing");
    let page = open(Surface::AppShell, &fx, 3003);
    let tab = &page.tab;
    jump_to_end(tab, Surface::AppShell);
    await_tail(tab, Surface::AppShell, "a fresh open to land at the tail");
    settle();
    // Writes again, for the same reason: Bash calls would coalesce into one event and the turn
    // would never pass the cap, so the rule would have nothing to demonstrate.
    let script: Vec<String> = (0..9u64)
        .flat_map(|k| {
            vec![
                write_tool_at(
                    &format!("g-live-{k}"),
                    &format!("/w/src/live_{k}.py"),
                    4,
                    &now_minus(40 - k * 3),
                ),
                tool_result_lines(&format!("g-live-{k}"), 2, &now_minus(39 - k * 3)),
            ]
        })
        .collect();
    let n = script.len();
    let growth = LiveGrowth::start(fx.path.clone(), script, Duration::from_millis(700));
    assert_eq!(
        growth.finish(Duration::from_secs(90)),
        n,
        "the driver appended the whole script"
    );
    settle();
    settle();
    let live: serde_json::Value = eval(
        tab,
        "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); \
           if (!p) return JSON.stringify({miss:'no section'}); \
           return JSON.stringify({ events: p.querySelectorAll('.process-event').length, \
             hiddenEvents: p.querySelectorAll('.process-event.progressive-hidden').length, \
             more: p.querySelectorAll('[data-process-more]').length }); })()",
    )
    .as_str()
    .and_then(|s| serde_json::from_str(s).ok())
    .unwrap_or(serde_json::Value::Null);
    assert!(
        live["events"].as_i64().unwrap_or(0) > 7,
        "the turn grew past the cap, so there is something for the rule to do: {live}"
    );
    assert_eq!(
        live["hiddenEvents"].as_i64(),
        Some(0),
        "a LIVE turn hides no top-level message — that is list-all, and it happens with no \
         click at all: {live}"
    );

    // (3) …and the reader's own word beats the rule. Collapse it, then let more work arrive:
    // it must STAY collapsed, or a block could never be made to stay shut while a turn runs.
    let pressed = eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); var b = p.querySelector('[data-process-more]'); if (!b) return 'no cap control'; b.click(); return 'ok'; })()");
    assert_eq!(
        pressed.as_str(),
        Some("ok"),
        "the expanded block still offers the cap control, to fold it back: {pressed}"
    );
    settle();
    let more = LiveGrowth::start(
        fx.path.clone(),
        vec![
            tool_open_at("g-after", &now_minus(4)),
            tool_result_lines("g-after", 5, &now_minus(3)),
        ],
        Duration::from_millis(900),
    );
    assert_eq!(
        more.finish(Duration::from_secs(40)),
        2,
        "the second batch landed"
    );
    settle();
    settle();
    let after = eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); return p.querySelectorAll('.process-event.progressive-hidden').length; })()");
    assert!(
        after.as_i64().unwrap_or(0) > 0,
        "the reader folded this block back, so the records that arrived afterwards are hidden \
         behind the cap again — the rule never overrides a choice the reader made: {after}"
    );
}

/// More activity arriving in the SAME open turn — appends, the ordinary live case.
fn growing_turn_script() -> Vec<String> {
    (0..5u64)
        .flat_map(|k| {
            vec![
                tool_open_at(&format!("g-live-{k}"), &now_minus(40 - k * 6)),
                tool_result_lines(&format!("g-live-{k}"), 6, &now_minus(37 - k * 6)),
            ]
        })
        .collect()
}

fn scenario_an_unfold_survives_the_turn_growing(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Unfold the LAST record of the tail turn, the way a reader watching work does.
    let opened = match surface {
        Surface::Classic => eval(tab, "(function(){ var f = [...document.querySelectorAll('#stream .fold')].pop(); if (!f) return 'none'; if (f.dataset.open === '0') f.querySelector('.fold-h').click(); return f.id || f.dataset.kind || 'ok'; })()"),
        Surface::AppShell => eval(tab, "(function(){ var r = [...document.querySelectorAll('.renderer[data-renderer]')].pop(); if (!r) return 'none'; if (r.classList.contains('closed')) r.querySelector('button.renderer-head').click(); return r.dataset.recordId || 'ok'; })()"),
    };
    let opened = opened.as_str().unwrap_or("none").to_string();
    assert_ne!(
        opened, "none",
        "{surface:?}: the tail has a record to unfold"
    );
    settle();
    let state_of = format!(
        "(function(){{ {} }})()",
        match surface {
            Surface::Classic => format!("var f = document.getElementById('{opened}') || [...document.querySelectorAll('#stream .fold')].pop(); return f ? f.dataset.open : 'gone';"),
            Surface::AppShell => format!("var r = document.querySelector('[data-record-id=\"{opened}\"]') || [...document.querySelectorAll('.renderer[data-renderer]')].pop(); return r ? (r.classList.contains('closed') ? '0' : '1') : 'gone';"),
        }
    );
    let before = eval(tab, &state_of).as_str().unwrap_or("").to_string();
    assert_eq!(
        before, "1",
        "{surface:?}: the reader's unfold took effect before any growth"
    );

    // …and now the turn grows under it, as a working agent makes it grow.
    let growth = LiveGrowth::start(
        fx.path.clone(),
        growing_turn_script(),
        Duration::from_millis(1400),
    );
    assert_eq!(
        growth.finish(Duration::from_secs(60)),
        10,
        "the driver appended the whole script"
    );
    settle();
    settle();
    let after = eval(tab, &state_of).as_str().unwrap_or("").to_string();
    assert_eq!(
        after, "1",
        "{surface:?}: a record the reader opened stays open while the turn grows — it was `{before}` \
         before the growth and `{after}` after. A fold is the reader's choice; new records arriving \
         is not a reason to undo it."
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_an_unfold_survives_the_turn_growing() {
    let _serial = serial();
    let fx = growing_turn_fixture("grow-fold-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_an_unfold_survives_the_turn_growing(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_an_unfold_survives_the_turn_growing() {
    let _serial = serial();
    let fx = growing_turn_fixture("grow-fold-app");
    let page = open(Surface::AppShell, &fx, 2991);
    scenario_an_unfold_survives_the_turn_growing(&page.tab, Surface::AppShell, &fx);
}

/// Diagnostic for #233: what happens to the reader's fold choices across live growth.
#[test]
#[ignore = "probe: cargo test --test scenarios fold_growth_probe -- --ignored --exact --nocapture"]
fn fold_growth_probe() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = growing_turn_fixture(match surface {
            Surface::Classic => "grow-probe-classic",
            Surface::AppShell => "grow-probe-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 2992 };
        let page = open(surface, &fx, port);
        let tab = &page.tab;
        jump_to_end(tab, surface);
        await_tail(tab, surface, "a fresh open to land at the tail");
        settle();
        // Open EVERY record in the tail turn, and remember exactly which ids the reader opened.
        let list = match surface {
            Surface::Classic => "(function(){ return JSON.stringify([...document.querySelectorAll('#stream .fold')].slice(-6).map(function (f) { return f.id || ''; })); })()",
            Surface::AppShell => "(function(){ return JSON.stringify([...document.querySelectorAll('.renderer[data-renderer]')].slice(-6).map(function (r) { return r.dataset.recordId || ''; })); })()",
        };
        let ids: Vec<String> =
            serde_json::from_str(eval(tab, list).as_str().unwrap_or("[]")).unwrap_or_default();
        for id in &ids {
            let js = match surface {
                Surface::Classic => format!("(function(){{ var f = document.getElementById('{id}'); if (!f) return 'gone'; if (f.dataset.open === '0') f.querySelector('.fold-h').click(); return 'ok'; }})()"),
                Surface::AppShell => format!("(function(){{ var r = document.querySelector('[data-record-id=\"{id}\"]'); if (!r) return 'gone'; if (r.classList.contains('closed')) {{ var h = r.querySelector('button.renderer-head'); if (h) h.click(); }} return 'ok'; }})()"),
            };
            eval(tab, &js);
            settle();
        }
        settle();
        let snapshot = |tab: &headless_chrome::Tab| {
            match surface {
            Surface::Classic => eval(tab, "(function(){ var o = {}; document.querySelectorAll('#stream .fold').forEach(function (f) { o[f.id || '?'] = f.dataset.open; }); return JSON.stringify(o); })()"),
            Surface::AppShell => eval(tab, "(function(){ var o = {}; document.querySelectorAll('.renderer[data-renderer]').forEach(function (r) { o[r.dataset.recordId || '?'] = r.classList.contains('closed') ? '0' : '1'; }); return JSON.stringify(o); })()"),
        }
        };
        let before: serde_json::Value =
            serde_json::from_str(snapshot(tab).as_str().unwrap_or("{}")).unwrap_or_default();
        let growth = LiveGrowth::start(
            fx.path.clone(),
            growing_turn_script(),
            Duration::from_millis(1400),
        );
        let appended = growth.finish(Duration::from_secs(60));
        settle();
        settle();
        let after: serde_json::Value =
            serde_json::from_str(snapshot(tab).as_str().unwrap_or("{}")).unwrap_or_default();
        println!("\n=== {surface:?} (appended {appended})");
        println!("  reader opened: {ids:?}");
        let empty = serde_json::Map::new();
        let b = before.as_object().unwrap_or(&empty);
        let a = after.as_object().unwrap_or(&empty);
        for id in &ids {
            let was = b.get(id).and_then(|v| v.as_str()).unwrap_or("absent");
            let now = a.get(id).and_then(|v| v.as_str()).unwrap_or("ABSENT");
            let verdict = if was == "1" && now == "0" {
                "  <-- RE-FOLDED"
            } else if now == "ABSENT" {
                "  <-- id gone"
            } else {
                ""
            };
            println!("    {id}: {was} -> {now}{verdict}");
        }
        println!("  rows before {} / after {}", b.len(), a.len());
    }
}

/// #233: the reader's folds across a REWRITE, not an append.
///
/// `design`/CLAUDE.md names the one thing that routinely rewrites a live stream: a queued prompt
/// being picked up. The marker for it disappears and every marker behind it shifts up a slot, and
/// record ids are `b{n}` from a positional counter — so after the rewrite the same id names a
/// different record. Fold state is keyed by that id (`state.folds`, `view.id`), which is exactly
/// the shape of the owner's report: the reader opens something, work arrives, and it is shut
/// again — or worse, something else is open instead.
fn queue_rewrite_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(10, Shape::default());
    t += &user_at("question q: start the long job", &now_minus(120));
    t += &assistant_at("Working on it.", &now_minus(118));
    t += &tool_open_at("q-1", &now_minus(116));
    t += &tool_result_lines("q-1", 8, &now_minus(115));
    t += &assistant_at("Still working.", &now_minus(112));
    t += &tool_open_at("q-2", &now_minus(110));
    t += &tool_result_lines("q-2", 8, &now_minus(109));
    // Two prompts queued behind the work, as a reader types while the agent runs.
    t += &queued_at("second thing to do", &now_minus(105));
    t += &queued_at("third thing to do", &now_minus(104));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 11,
    }
}

/// The pickup: the queued prompt becomes a real user turn, so the marker for it goes and
/// everything behind it moves.
fn queue_pickup_script() -> Vec<String> {
    vec![
        user_at("second thing to do", &now_minus(100)),
        assistant_at("Picking that up now.", &now_minus(98)),
        tool_open_at("q-3", &now_minus(96)),
        tool_result_lines("q-3", 6, &now_minus(95)),
    ]
}

fn scenario_folds_survive_a_queue_pickup(
    tab: &headless_chrome::Tab,
    surface: Surface,
    fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    // Open the tail records one at a time — each toggle re-renders, so a batch of clicks
    // collected up front would land on detached nodes and silently do nothing.
    let list = match surface {
        Surface::Classic => "(function(){ return JSON.stringify([...document.querySelectorAll('#stream .fold')].slice(-4).map(function (f) { return f.id || ''; })); })()",
        Surface::AppShell => "(function(){ return JSON.stringify([...document.querySelectorAll('.renderer[data-renderer]')].slice(-4).map(function (r) { return r.dataset.recordId || ''; })); })()",
    };
    let ids: Vec<String> =
        serde_json::from_str(eval(tab, list).as_str().unwrap_or("[]")).unwrap_or_default();
    assert!(!ids.is_empty(), "{surface:?}: the tail has records to open");
    for id in &ids {
        let js = match surface {
            Surface::Classic => format!("(function(){{ var f = document.getElementById('{id}'); if (!f) return 'gone'; if (f.dataset.open === '0') f.querySelector('.fold-h').click(); return 'ok'; }})()"),
            Surface::AppShell => format!("(function(){{ var r = document.querySelector('[data-record-id=\"{id}\"]'); if (!r) return 'gone'; if (r.classList.contains('closed')) {{ var h = r.querySelector('button.renderer-head'); if (h) h.click(); }} return 'ok'; }})()"),
        };
        eval(tab, &js);
        settle();
    }
    // What the reader can SEE open, by the text of each open record — identity that survives a
    // re-numbering, which an id by construction does not.
    let open_texts = match surface {
        Surface::Classic => "(function(){ return JSON.stringify([...document.querySelectorAll('#stream .fold')].filter(function (f) { return f.dataset.open === '1'; }).map(function (f) { var h = f.querySelector('.fold-h'); return (h ? h.textContent : '').replace(/\\s+/g, ' ').replace(/#$/, '').trim(); })); })()",
        Surface::AppShell => "(function(){ return JSON.stringify([...document.querySelectorAll('.renderer[data-renderer]')].filter(function (r) { return !r.classList.contains('closed'); }).map(function (r) { var h = r.querySelector('.renderer-head'); return (h ? h.textContent : '').replace(/\\s+/g, ' ').trim(); })); })()",
    };
    let before: Vec<String> =
        serde_json::from_str(eval(tab, open_texts).as_str().unwrap_or("[]")).unwrap_or_default();
    assert!(
        !before.is_empty(),
        "{surface:?}: the reader has something open before the pickup"
    );

    let growth = LiveGrowth::start(
        fx.path.clone(),
        queue_pickup_script(),
        Duration::from_millis(1500),
    );
    assert_eq!(
        growth.finish(Duration::from_secs(60)),
        4,
        "the driver appended the whole pickup"
    );
    settle();
    settle();
    let after: Vec<String> =
        serde_json::from_str(eval(tab, open_texts).as_str().unwrap_or("[]")).unwrap_or_default();
    // PROVE THE REWRITE HAPPENED. A case that asserts folds survived a renumbering, on a stream
    // that was only appended to, proves nothing at all — so the queue marker must actually be
    // gone and the ids must actually have moved.
    let shape = r#"(function(){ var q = document.querySelectorAll('#stream .qmarker, .renderer[data-renderer-kind="queue"]').length; var ids = [...document.querySelectorAll('#stream .fold, .renderer[data-renderer]')].map(function (e) { return e.id || e.dataset.recordId || ''; }); return JSON.stringify({ queues: q, ids: ids }); })()"#;
    let shape_after: serde_json::Value =
        serde_json::from_str(eval(tab, shape).as_str().unwrap_or("{}")).unwrap_or_default();
    assert_eq!(
        shape_after["queues"].as_i64().unwrap_or(-1),
        1,
        "{surface:?}: one of the two queued prompts was picked up, so exactly one marker is left          — without that this case is asserting against a plain append: {shape_after}"
    );
    // Everything the reader opened is still open. New records may be open or shut by their own
    // default — that is not the reader's choice being undone.
    let lost: Vec<&String> = before.iter().filter(|t| !after.contains(t)).collect();
    assert!(
        lost.is_empty(),
        "{surface:?}: a queued prompt being picked up rewrites the stream and renumbers records, \
         but it must not shut what the reader opened. Closed by the rewrite: {lost:?}\n  before \
         {before:?}\n  after  {after:?}"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_folds_survive_a_queue_pickup() {
    let _serial = serial();
    let fx = queue_rewrite_fixture("queue-fold-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_folds_survive_a_queue_pickup(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_folds_survive_a_queue_pickup() {
    let _serial = serial();
    let fx = queue_rewrite_fixture("queue-fold-app");
    let page = open(Surface::AppShell, &fx, 2993);
    scenario_folds_survive_a_queue_pickup(&page.tab, Surface::AppShell, &fx);
}

/// #233: "expand all" on an open turn means the turn, not the records that happen to be on
/// screen when it is pressed.
///
/// The owner: "I selected 'expand all folds' for an agent process block, but future messages are
/// still coming folded. It feels more logical to expand the meaning of 'unfold all' for the open
/// turn to apply to all future messages" — and, clarifying, "to apply to all future messages
/// still for the same turn."
///
/// #251 — the same control, pressed on a section that is CLOSED. A collapsed section hides its
/// whole body (`.process-surface.closed .process-surface-body{display:none}`), and the handler
/// used to expand every record inside while leaving `processFolds` alone — so the reader saw
/// nothing happen, and the state silently desynced: a second press then COLLAPSED them, also
/// invisibly. Expanding now opens the section it is expanding.
///
/// A case that presses this on an already-OPEN section cannot see the bug at all, which is why
/// it survived #233.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_expand_all_opens_the_section_it_expands() {
    let _serial = serial();
    let fx = growing_turn_fixture("bulk-closed-app");
    let page = open(Surface::AppShell, &fx, 2998);
    let tab = &page.tab;
    jump_to_end(tab, Surface::AppShell);
    await_tail(tab, Surface::AppShell, "a fresh open to land at the tail");
    settle();
    // Collapse the last agent-process section first — the state this bug lives in.
    let shut = eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); if (!p) return 'none'; var t = p.querySelector('[data-process-toggle]'); if (!t) return 'no toggle'; if (!p.classList.contains('closed')) t.click(); return 'ok'; })()");
    assert_eq!(
        shut.as_str(),
        Some("ok"),
        "the tail has a collapsible agent-process section: {shut}"
    );
    settle();
    let closed = eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); return !!(p && p.classList.contains('closed')); })()");
    assert_eq!(
        closed,
        serde_json::Value::Bool(true),
        "…and it is actually closed before the press: {closed}"
    );
    // Now press "expand every detail in this section".
    eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); p.querySelector('[data-process-bulk]').click(); return 'ok'; })()");
    settle();
    let seen: serde_json::Value = eval(
        tab,
        "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); \
           if (!p) return JSON.stringify({miss:'no section'}); \
           var rs = [...p.querySelectorAll('[data-renderer]:not(.noninteractive)')]; \
           var body = p.querySelector('.process-surface-body'); \
           return JSON.stringify({ closed: p.classList.contains('closed'), \
             bodyShown: !!(body && getComputedStyle(body).display !== 'none'), \
             renderers: rs.length, shut: rs.filter(function (r) { return r.classList.contains('closed'); }).length, \
             more: p.querySelectorAll('[data-process-more][aria-expanded=\"false\"]').length }); })()",
    )
    .as_str()
    .and_then(|s| serde_json::from_str(s).ok())
    .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        seen["closed"],
        serde_json::Value::Bool(false),
        "expanding OPENS the section — otherwise every record inside expands where the reader \
         cannot see any of it: {seen}"
    );
    assert_eq!(
        seen["bodyShown"],
        serde_json::Value::Bool(true),
        "…and the body is actually displayed, not merely un-classed: {seen}"
    );
    assert!(
        seen["renderers"].as_i64().unwrap_or(0) > 0 && seen["shut"].as_i64().unwrap_or(-1) == 0,
        "…with every record inside expanded: {seen}"
    );
    assert_eq!(
        seen["more"].as_i64().unwrap_or(-1),
        0,
        "…and the `Show N more` cap lifted, as #233 requires of this control: {seen}"
    );
}

/// The control used to sweep `[data-renderer]` under the section and write a fold entry for each
/// one it found. A record that arrived afterwards had no entry, so it fell back to its own
/// folded default — and the reader, who had asked for this turn to be open, watched work arrive
/// shut. The intent is now held on the section and inherited by whatever the turn produces next.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_expand_all_reaches_what_the_turn_produces_next() {
    let _serial = serial();
    let fx = growing_turn_fixture("bulk-live-app");
    let page = open(Surface::AppShell, &fx, 2994);
    let tab = &page.tab;
    jump_to_end(tab, Surface::AppShell);
    await_tail(tab, Surface::AppShell, "a fresh open to land at the tail");
    settle();
    // Press "expand every detail" on the last agent-process section, as a reader watching work.
    let pressed = eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); if (!p) return 'none'; var b = p.querySelector('[data-process-bulk]'); if (!b) return 'no control'; b.click(); return p.dataset.processKey || 'ok'; })()");
    assert!(
        pressed
            .as_str()
            .map(|s| s != "none" && s != "no control")
            .unwrap_or(false),
        "the tail has an agent-process section with an expand-all control: {pressed}"
    );
    settle();
    let shut = "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); if (!p) return -1; return p.querySelectorAll('[data-renderer].closed').length; })()";
    assert_eq!(
        eval(tab, shut).as_i64().unwrap_or(-1),
        0,
        "pressing it opens everything already in the section"
    );

    // …and now the turn keeps working.
    let growth = LiveGrowth::start(
        fx.path.clone(),
        growing_turn_script(),
        Duration::from_millis(1400),
    );
    assert_eq!(
        growth.finish(Duration::from_secs(60)),
        10,
        "the driver appended the whole script"
    );
    settle();
    settle();
    // ONE block, not two. The owner: "future messages would then form a separate agent process
    // block (of the same turn), instead of joining the same agent process block. And if I refresh
    // the page, then it becomes one agent process block." A reload rebuilt from zero and
    // coalesced correctly, so the incremental append was the thing that was wrong.
    let blocks_after = eval(tab, "(function(){ return JSON.stringify([...document.querySelectorAll('[data-process-surface]')].map(function (p) { return p.dataset.turn; })); })()");
    let blocks_after: Vec<String> =
        serde_json::from_str(blocks_after.as_str().unwrap_or("[]")).unwrap_or_default();
    let tail_turn = blocks_after.last().cloned().unwrap_or_default();
    let same_turn = blocks_after.iter().filter(|t| **t == tail_turn).count();
    assert_eq!(
        same_turn, 1,
        "the records a turn produces join its existing Agent Process rather than starting a          second one for the same turn: blocks {blocks_after:?}"
    );
    // …and the key survived that rebuild, which is what lets the standing intent still apply.
    assert_eq!(
        pressed.as_str().unwrap_or(""),
        eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); return p ? p.dataset.processKey : ''; })()").as_str().unwrap_or(""),
        "the section keeps its identity across the append, or the reader's choice would be          keyed to a section that no longer exists"
    );
    let grew = eval(tab, "(function(){ var p = [...document.querySelectorAll('[data-process-surface]')].pop(); if (!p) return -1; return p.querySelectorAll('[data-renderer]').length; })()");
    assert!(
        grew.as_i64().unwrap_or(0) > 0,
        "the section still holds records after the growth: {grew}"
    );
    assert_eq!(
        eval(tab, shut).as_i64().unwrap_or(-1),
        0,
        "…and what the turn produced next is open too: the reader asked for this section, not for \
         the records that happened to be in it at the time"
    );
}

/// A screenshot taken in the MIDDLE of an activity run — the shape #256 reported.
fn image_in_a_run_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at(
        "look at the page and tell me what is wrong",
        &now_minus(200),
    );
    t += &harness::named_tool_at("r1", "Bash", "ls", &now_minus(198));
    t += &harness::tool_result_text("r1", "src", &now_minus(196));
    t += &harness::named_tool_at(
        "r2",
        "mcp__claude-in-chrome__computer",
        "screenshot",
        &now_minus(194),
    );
    t += &harness::image_result_at("r2", &now_minus(192));
    t += &harness::named_tool_at("r3", "Bash", "pwd", &now_minus(190));
    t += &harness::tool_result_text("r3", "/w", &now_minus(188));
    t += &assistant_at("The header is misaligned.", &now_minus(186));
    let path = stores.claude_session(name, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #256 — an image a TOOL produced renders where it happened, inside the run, and not hoisted
/// to the front of it.
///
/// The owner saw two screenshots parked directly under their prompt while Claude Code had shown
/// them in the middle of the work. The cause was in the shared fold, so both pages had it: a
/// coalesced span buffers its calls and flushes them at the end, and an attachment went straight
/// out — "in place", except the span had not been written yet, so it landed ahead of the whole
/// run. The further into a turn a screenshot was taken, the further back it was thrown.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_put_an_images_where_the_tool_took_it_not_under_the_prompt() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3006)] {
        let fx = image_in_a_run_fixture(match surface {
            Surface::Classic => "img-run-classic",
            _ => "img-run-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        // An attachment is NOT foldable (`model::foldable`), so on the classic page it carries
        // no `data-kind` — its marker is `.amark`. Ask each surface for its records in document
        // order and normalise to one kind vocabulary.
        let js = match surface {
            Surface::Classic => "(function(){ var rs = [...document.querySelectorAll('#stream .fold[data-kind], #stream .amark')];                var kinds = rs.map(function (e) { return e.dataset.kind || 'attachment'; });                var at = kinds.lastIndexOf('attachment');                return JSON.stringify({ kinds: kinds, at: at,                  before: at > 0 ? kinds.slice(0, at) : [] }); })()",
            Surface::AppShell => "(function(){ var rs = [...document.querySelectorAll('.virtual-window [data-record-kind]')];                var kinds = rs.map(function (e) { return e.dataset.recordKind || ''; });                var at = kinds.lastIndexOf('attachment');                return JSON.stringify({ kinds: kinds, at: at,                  before: at > 0 ? kinds.slice(0, at).filter(function (k) { return k; }) : [] }); })()",
        };
        let seen: serde_json::Value = eval(&page.tab, js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let at = seen["at"].as_i64().unwrap_or(-1);
        assert!(
            at >= 0,
            "{surface:?} draws the screenshot at all — if this fails the image was swallowed \
             rather than moved, which is worse than the bug: {seen}"
        );
        let before: Vec<String> = seen["before"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let last_user = before.iter().rposition(|k| k == "user");
        let tool_between = before
            .iter()
            .skip(last_user.map(|i| i + 1).unwrap_or(0))
            .any(|k| k == "bash" || k == "tool" || k == "act");
        assert!(
            tool_between,
            "{surface:?}: at least one CALL stands between the prompt and the image — the image \
             was taken by the third tool of the run, so nothing between them means it was \
             hoisted to the front of the run again (#256): {before:?}"
        );
    }
}

/// A turn that recorded how long it took, and one that did not (#257).
fn turn_duration_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    // A turn the transcript timed: two and a half minutes.
    t += &user_at("question 13: how long did that take?", &now_minus(300));
    t += &harness::named_tool_at("d1", "Bash", "cargo build", &now_minus(298));
    t += &harness::tool_result_text("d1", "Finished", &now_minus(296));
    t += &assistant_at("Built it.", &now_minus(200));
    t += &harness::turn_duration_at(151_000, &now_minus(199));
    // …and a turn it did not, which must show nothing rather than a zero.
    t += &user_at("question 14: and this one?", &now_minus(150));
    t += &assistant_at("No record of this turn's length.", &now_minus(140));
    let path = stores.claude_session(name, &t);
    Fixture {
        base,
        path,
        turns: 14,
    }
}

/// #257 — a turn says how long it took, on BOTH pages, from the one shared formatter.
///
/// The record closes a turn rather than opening one, so the engine back-patches it onto the head
/// already stamped. 19% of real turns carry no such record, so the other half of the rule is
/// that an untimed turn shows nothing at all: a chip reading "0m" would be a number that says
/// nothing while claiming to.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_say_how_long_a_turn_took_and_stay_silent_when_unrecorded() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3008)] {
        let fx = turn_duration_fixture(match surface {
            Surface::Classic => "turn-dur-classic",
            _ => "turn-dur-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        let js = match surface {
            Surface::Classic => "(function(){ var c = [...document.querySelectorAll('.ts')].filter(function (e) { return e.title === 'How long this turn took'; }); \
               return JSON.stringify({ chips: c.length, texts: c.map(function (e) { return e.textContent.trim(); }) }); })()",
            Surface::AppShell => "(function(){ var c = [...document.querySelectorAll('.turn-time')].filter(function (e) { return e.title === 'How long this turn took'; }); \
               return JSON.stringify({ chips: c.length, texts: c.map(function (e) { return e.textContent.trim(); }) }); })()",
        };
        let seen: serde_json::Value = eval(&page.tab, js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let texts: Vec<String> = seen["texts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            seen["chips"].as_i64(),
            Some(1),
            "{surface:?}: exactly ONE turn was timed, so exactly one chip — an untimed turn \
             shows nothing rather than a zero: {seen}"
        );
        assert_eq!(
            texts,
            vec!["3m".to_string()],
            "{surface:?}: 151s reads as minutes through the shared formatter, so the two pages \
             cannot word it differently: {seen}"
        );
    }
}

/// An `AskUserQuestion` call and the answers that came back (#255).
fn ask_question_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: ask me about the release", &now_minus(200));
    t += &harness::ask_question_at("ask1", &now_minus(198));
    t += &harness::ask_question_answer("ask1", &now_minus(150));
    t += &assistant_at("Holding, with engine and tui.", &now_minus(140));
    let path = stores.claude_session(name, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #255 — the card shows every question that was put and every option that was offered, with
/// the pick marked, on BOTH pages.
///
/// The owner: "does the transcript carry all the questions as well as corresponding choices
/// being offered to the user? I don't see them in the agent-monitor." It did carry all of it.
/// The card showed the state, the first question's text and the labels that came back — so on
/// this two-question call, 2 of the 8 things the asker wrote, and none of the options that were
/// DECLINED, which is where the trade-off is written.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_show_every_question_and_every_option_that_was_offered() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3010)] {
        let fx = ask_question_fixture(match surface {
            Surface::Classic => "ask-opts-classic",
            _ => "ask-opts-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        // Read the CARD, not the page text: the raw result prose also contains the answers, and
        // the card sits inside a fold whose text `innerText` would skip entirely.
        let (card, chip) = match surface {
            Surface::Classic => (".irq", ".irq-answer"),
            Surface::AppShell => (".input-request", ".input-answer"),
        };
        let js = format!(
            "(function(){{ var c = document.querySelector('{card}'); if (!c) return JSON.stringify({{ miss: 'no card' }}); \
               var t = c.textContent; \
               return JSON.stringify({{ \
                 headers: ['Release','Crates'].filter(function (h) {{ return t.indexOf(h) >= 0; }}), \
                 options: ['Cut now','Hold','engine','html','tui'].filter(function (o) {{ return t.indexOf(o) >= 0; }}), \
                 descriptions: ['Tag and push both remotes.','Leave it unreleased.','the terminal.'].filter(function (d) {{ return t.indexOf(d) >= 0; }}), \
                 ticked: [...c.querySelectorAll('{chip}')].filter(function (e) {{ return e.textContent.indexOf('✓') >= 0; }}).length }}); }})()"
        );
        let seen: serde_json::Value = eval(&page.tab, &js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let got = |k: &str| -> Vec<String> {
            seen[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        };
        assert_eq!(
            got("headers").len(),
            2,
            "{surface:?}: BOTH questions are named, not just the first: {seen}"
        );
        assert_eq!(
            got("options").len(),
            5,
            "{surface:?}: every option is shown, including the ones declined: {seen}"
        );
        assert_eq!(
            got("descriptions").len(),
            3,
            "{surface:?}: an option's description comes with it — that is where the asker wrote \
             the trade-off: {seen}"
        );
        assert_eq!(
            seen["ticked"].as_i64(),
            Some(3),
            "{surface:?}: exactly the three chosen options are ticked — Hold, engine and tui — \
             so a multi-select answer marks both of its picks and the single-select marks one: \
             {seen}"
        );
    }
}

/// An `AskUserQuestion` answered three ways at once: a pick with notes, typed words, notes alone.
fn ask_replied_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: ask me about init", &now_minus(200));
    t += &harness::ask_replied_at("ask1", &now_minus(198));
    t += &harness::ask_replied_answer("ask1", &now_minus(150));
    t += &assistant_at(
        "Exit 2, a switch on both, and the flag named after sync.",
        &now_minus(140),
    );
    let path = stores.claude_session(name, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #280 — every question on the card is drawn the same way, and every answer the reader gave is
/// on it, on BOTH pages.
///
/// The owner, with a screenshot of a two-question call: "bug: agent asks two questions, but the
/// two questions are not rendered equally" — and then: "you also missed that the rendering of the
/// #104 question uses a different font size/style than the question for #101". Two defects:
///
///   - the card's headline was the call's TARGET, which is the first question (" +1"), so the
///     first question was printed twice — once at reading size, once in the small note type every
///     question's lead used — and read as THE question while the second read as a footnote;
///   - an answer that named no option — the reader's own words, typed in the client — was drawn
///     nowhere: no option ticked, and the words themselves gone. The card had only ever matched
///     answers against labels.
///
/// So: each question's text appears exactly once, every element carrying one is typeset alike,
/// the typed words are there verbatim with a tick and no option is ticked for them (a typed
/// "Both - …" is not the option "Both"), the notes are there, and the client's `(notes only)`
/// placeholder is not — it is the client's word, not the reader's.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_draw_every_question_alike_and_every_answer_given() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3024)] {
        let fx = ask_replied_fixture(match surface {
            Surface::Classic => "ask-replied-classic",
            _ => "ask-replied-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        let card = match surface {
            Surface::Classic => ".irq",
            Surface::AppShell => ".input-request",
        };
        let lit = |s: &str| serde_json::to_string(s).unwrap();
        let [q1, q2, q3] = harness::REPLIED_QUESTIONS;
        let [n1, n3] = harness::REPLIED_NOTES;
        // An element "carries" a string when one of its OWN text nodes contains it — so a
        // question's holder is the innermost element that prints it, whatever the markup.
        let js = format!(
            "(function(){{ var c = document.querySelector('{card}'); if (!c) return JSON.stringify({{ miss: 'no card' }}); \
               var t = c.textContent; \
               var holders = function (s) {{ return [...c.querySelectorAll('*')].filter(function (e) {{ \
                 return [...e.childNodes].some(function (n) {{ return n.nodeType === 3 && n.textContent.indexOf(s) >= 0; }}); }}); }}; \
               var type = function (e) {{ var s = getComputedStyle(e); return [s.fontSize, s.fontFamily, s.fontWeight, s.fontStyle, s.color].join(' | '); }}; \
               var qs = [{q1}, {q2}, {q3}]; \
               return JSON.stringify({{ \
                 counts: qs.map(function (q) {{ return t.split(q).length - 1; }}), \
                 types: qs.map(function (q) {{ return holders(q).map(type); }}), \
                 typed: t.indexOf({typed}) >= 0, \
                 notes: [{n1}, {n3}].map(function (n) {{ return t.indexOf(n) >= 0; }}), \
                 placeholder: t.indexOf('(notes only)') >= 0, \
                 ticked: holders('✓').map(function (e) {{ return e.textContent.replace('✓', '').trim(); }}).filter(function (x) {{ return x; }}), \
                 title: (c.querySelector('strong') || {{}}).textContent || '' }}); }})()",
            q1 = lit(q1),
            q2 = lit(q2),
            q3 = lit(q3),
            typed = lit(harness::REPLIED_TYPED),
            n1 = lit(n1),
            n3 = lit(n3),
        );
        let seen: serde_json::Value = eval(&page.tab, &js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            seen["counts"],
            serde_json::json!([1, 1, 1]),
            "{surface:?}: each question is printed exactly once — the first one is not promoted \
             into the headline and then listed again: {seen}"
        );
        let types: Vec<String> = seen["types"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|holders| holders.as_array().cloned().unwrap_or_default())
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();
        assert!(
            types.len() == 3 && types.iter().all(|t| t == &types[0]),
            "{surface:?}: every question is typeset alike — size, family, weight, style and \
             colour: {seen}"
        );
        assert_eq!(
            seen["typed"],
            serde_json::json!(true),
            "{surface:?}: the reader's own words are on the card, verbatim: {seen}"
        );
        let ticked: Vec<String> = seen["ticked"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            ticked,
            vec![
                "(a) exit 2 (Recommended)".to_string(),
                harness::REPLIED_TYPED.to_string()
            ],
            "{surface:?}: the pick is ticked, the typed words are ticked, and nothing else — \
             not the option \"Both\" the typed answer happens to start with: {seen}"
        );
        assert_eq!(
            seen["notes"],
            serde_json::json!([true, true]),
            "{surface:?}: the notes the reader attached are on the card, beside a pick and on \
             their own: {seen}"
        );
        assert_eq!(
            seen["placeholder"],
            serde_json::json!(false),
            "{surface:?}: `(notes only)` is the client's placeholder, never shown as an answer: \
             {seen}"
        );
        assert_eq!(
            seen["title"],
            serde_json::json!("User input received"),
            "{surface:?}: an answered call is not waiting: {seen}"
        );
    }
}

/// Two questions that came back WITHOUT an answer: one the client stopped waiting on, one the
/// reader dismissed.
fn ask_unanswered_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: ask me twice", &now_minus(200));
    t += &harness::ask_question_at("ask-t", &now_minus(198));
    t += &harness::ask_timed_out_answer("ask-t", &now_minus(138));
    t += &harness::ask_question_at("ask-d", &now_minus(130));
    t += &harness::ask_declined_answer("ask-d", &now_minus(120));
    t += &assistant_at("Understood; I will stop and wait.", &now_minus(110));
    let path = stores.claude_session(name, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #281 — a question that came back without an answer is settled, and says why, on BOTH pages.
///
/// The card drawn while a question waits was replaced only by a parsed ANSWER, so a question the
/// client stopped waiting on after 60s, and one the reader dismissed, both said "Waiting for user
/// input — please return to the agent client" forever: 25 of the owner's 223 questions, one in
/// nine. A call that has come back is not waiting.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_say_why_a_question_went_unanswered() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3026)] {
        let fx = ask_unanswered_fixture(match surface {
            Surface::Classic => "ask-unanswered-classic",
            _ => "ask-unanswered-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        let card = match surface {
            Surface::Classic => ".irq",
            Surface::AppShell => ".input-request",
        };
        let js = format!(
            "JSON.stringify([...document.querySelectorAll('{card}')].map(function (c) {{ return {{ \
               state: c.className, title: (c.querySelector('strong') || {{}}).textContent || '', \
               text: (c.querySelector('p') || {{}}).textContent || '', \
               ticked: c.textContent.split('✓').length - 1, \
               options: ['Cut now', 'Hold', 'engine', 'tui'].filter(function (o) {{ return c.textContent.indexOf(o) >= 0; }}).length }}; }}))"
        );
        let cards: Vec<serde_json::Value> = eval(&page.tab, &js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        assert_eq!(
            cards.len(),
            2,
            "{surface:?}: both questions drew a card: {cards:?}"
        );
        for c in &cards {
            let state = c["state"].as_str().unwrap_or("");
            assert!(
                state.split_whitespace().any(|w| w == "unanswered")
                    && !state.split_whitespace().any(|w| w == "waiting"),
                "{surface:?}: a question that came back is settled, not waiting: {c}"
            );
            assert_eq!(c["title"], "No answer", "{surface:?}: {c}");
            assert_eq!(
                c["ticked"], 0,
                "{surface:?}: nothing ticked — nothing was chosen: {c}"
            );
            assert_eq!(
                c["options"], 4,
                "{surface:?}: …and what was asked is still there to read: {c}"
            );
        }
        let texts: Vec<&str> = cards.iter().filter_map(|c| c["text"].as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "The agent client stopped waiting after 60s; the agent went on without an answer.",
                "Declined in the agent client; the agent was told not to proceed.",
            ],
            "{surface:?}: each says why, in the order they were asked: {cards:?}"
        );
    }
}

/// A `SendUserFile` that delivered three files — a page, an image and text — each of which exists.
fn delivered_files_fixture(name: &str) -> (Fixture, Vec<String>) {
    let base = base(name);
    let stores = Stores::new(&base);
    let repo = base.join("delivered");
    std::fs::create_dir_all(&repo).unwrap();
    let files: Vec<String> = [
        ("deck.html", "<h1>deck</h1>"),
        ("notes.txt", "notes"),
        ("more.md", "# more"),
    ]
    .iter()
    .map(|(f, body)| {
        let p = repo.join(f);
        std::fs::write(&p, body).unwrap();
        p.display().to_string()
    })
    .collect();
    let refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: send me the files", &now_minus(200));
    t += &harness::send_user_file_at("send1", &refs, &now_minus(190));
    t += &assistant_at("Sent all three.", &now_minus(180));
    let path = stores.claude_session(name, &t);
    (
        Fixture {
            base,
            path,
            turns: 13,
        },
        files,
    )
}

/// #275 — every file a multi-file `SendUserFile` delivered can be acted on, on BOTH pages.
///
/// The header names the first file and counts the rest (`~/…/deck.html +2`), and only that first
/// path was stamped: the "+2" were a number with nothing behind it, so two of the three delivered
/// files could be neither opened nor revealed. Half the owner's sends deliver several files (31 of
/// 64). Each file now carries its own stamps and a path element the page's own click machinery acts
/// on — the classic page's `.tool-path`, the app shell's `[data-reference-path]`.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_offer_every_file_a_send_delivered() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3028)] {
        let (fx, files) = delivered_files_fixture(match surface {
            Surface::Classic => "delivered-classic",
            _ => "delivered-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        let (selector, path_attr) = match surface {
            Surface::Classic => (".tool-path[data-path][data-sig]", "path"),
            Surface::AppShell => ("[data-reference-path][data-reference-sig]", "referencePath"),
        };
        let js = format!(
            "JSON.stringify([...new Set([...document.querySelectorAll('{selector}')].map(function (e) {{ return e.dataset.{path_attr}; }}))])"
        );
        let stamped: Vec<String> = eval(&page.tab, &js)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let offered: Vec<&String> = files.iter().filter(|f| stamped.contains(f)).collect();
        assert_eq!(
            offered.len(),
            files.len(),
            "{surface:?}: each delivered file has a stamped path the page acts on, not only the \
             first — offered {offered:?} of {files:?}; every stamped path on the page: {stamped:?}"
        );
    }
}

/// A session that READ two files from agent scratch: one under its own project's scratch, one
/// under another project's. Returns the fixture and the two paths. Plain text, because Markdown
/// opens in mdrev's viewer (#270) — the same guards, a different page.
fn scratch_fixture(name: &str) -> (Fixture, String, String) {
    let base = base(name);
    let stores = Stores::new(&base);
    let scratch = base.join("stores").join("claude-scratch");
    let write = |rel: &str, body: &str| {
        let p = scratch.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p.display().to_string()
    };
    // The fixture's store slug is `-r` (`Stores::claude_session`), so its scratch is `<root>/-r`.
    let own = write(
        "-r/7efa38c0-0000-4000-8000-000000000283/scratchpad/notes.txt",
        "scratch notes of this project",
    );
    let other = write(
        "-elsewhere/c8e30428-0000-4000-8000-000000000283/scratchpad/other.txt",
        "another project's notes",
    );
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: read the scratch", &now_minus(200));
    t += &read_tool_at("r1", &own, &now_minus(190));
    t += &tool_result_at("r1", &now_minus(190));
    t += &read_tool_at("r2", &other, &now_minus(185));
    t += &tool_result_at("r2", &now_minus(185));
    t += &assistant_at("Read both.", &now_minus(180));
    let path = stores.claude_session(name, &t);
    (
        Fixture {
            base,
            path,
            turns: 13,
        },
        own,
        other,
    )
}

/// #283 — a file under the agent's OWN scratch for this project opens in the page, on BOTH pages;
/// one under ANOTHER project's scratch still does not.
///
/// The owner: "also allow inline rendering of files under agent's scratch directory whose paths are
/// in the transcript. E.g. /private/tmp/claude-502". The render policy decides which offered paths
/// get a file stamp, but `/file` also asks whether a hosted session EXPLAINS the path (its cwd, its
/// project, its transcript's directory) — and no session explained its scratch, so a scratchpad
/// file the transcript named was refused whatever the policy said. A session now explains its own
/// project's scratch (`TranscriptAdapter::scratch_dirs`: `<scratch root>/<project slug>/`, where a
/// session's spawned agents keep their scratch too). The classic page falls back to revealing a
/// refused file, which is how the refusal shows there.
///
/// Both pages come from ONE v2 monitor: the classic page is its splice (`?ui=classic`), because
/// the html server this file's other classic cases use renders no file inline at all — it only
/// reveals — so it could not show the difference.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_open_a_file_from_the_session_s_own_scratch() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 3032), (Surface::AppShell, 3030)] {
        let (fx, own, other) = scratch_fixture(match surface {
            Surface::Classic => "scratch-classic",
            _ => "scratch-app",
        });
        let browser = harness::chrome();
        let tab = browser.new_tab().unwrap();
        let stores = Stores {
            root: fx.base.join("stores"),
        };
        let monitor = Monitor::spawn(Kind::V2, port, &fx.base, Some(&stores), true);
        monitor.pair(&tab);
        let (ui, ready) = match surface {
            Surface::Classic => ("classic", "document.querySelectorAll('#stream .blk').length >= 3"),
            _ => ("app", "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3"),
        };
        monitor.open(
            &tab,
            &format!("?ui={ui}&session={}&mountall=1", sid_of(&fx)),
        );
        harness::until(
            &tab,
            ready,
            "the page to render the fixture",
            Duration::from_secs(30),
            "document.body.innerText.slice(0, 200)",
        );
        settle();
        // Record every reveal, never send one: `open -R` must not run here.
        eval(
            &tab,
            "window.__reveals = []; var real = window.fetch; window.fetch = function (u, o) { var s = String(u); if (/__reveal\\?/.test(s)) { window.__reveals.push(s); return Promise.resolve(new Response('', {status: 200})); } return real(u, o); }; 'ok'",
        );
        let (link, shown) = match surface {
            Surface::Classic => (".tool-path[data-path={p}]", ".lightbox pre.lb-text"),
            Surface::AppShell => (
                "[data-reference-path={p}]",
                "#previewBody pre.artifact-text",
            ),
        };
        let click = |p: &str| {
            let sel = link.replace("{p}", &format!("{p:?}"));
            eval(
                &tab,
                &format!("document.querySelector({sel:?}).click(); 'ok'"),
            );
        };
        click(&own);
        until(
            &tab,
            &format!(
                "[...document.querySelectorAll({shown:?})].some(function (e) {{ return e.textContent.indexOf('scratch notes of this project') >= 0; }})"
            ),
            "the session's own scratch file, shown in the page",
            Duration::from_secs(10),
            "JSON.stringify({ reveals: window.__reveals, shown: [...document.querySelectorAll('pre')].map(function (e) { return e.className + ':' + e.textContent.slice(0, 40); }).slice(-4) })",
        );
        click(&other);
        let refused = match surface {
            // The classic page reveals what `/file` refuses.
            Surface::Classic => {
                "(window.__reveals || []).some(function (u) { return u.indexOf('other.txt') >= 0; })"
            }
            // The app shell's pane says it cannot read the file, and offers the file manager.
            Surface::AppShell => "!!document.querySelector('#previewBody [data-preview-reveal]')",
        };
        until(
            &tab,
            refused,
            "another project's scratch file refused",
            Duration::from_secs(10),
            "JSON.stringify({ reveals: window.__reveals, pane: (document.getElementById('previewBody') || {}).className })",
        );
        assert_eq!(
            eval(
                &tab,
                &format!(
                    "[...document.querySelectorAll({shown:?})].some(function (e) {{ return e.textContent.indexOf(\"another project's notes\") >= 0; }})"
                )
            )
            .as_bool(),
            Some(false),
            "{surface:?}: a session does not explain another project's scratch"
        );
    }
}

/// An `AskUserQuestion` whose options came with the asker's drawings, answered with the first.
fn ask_previewed_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: ask me about the layout", &now_minus(200));
    t += &harness::ask_previewed_at("ask1", &now_minus(198));
    t += &harness::ask_previewed_answer("ask1", &now_minus(150));
    t += &assistant_at("Board it is.", &now_minus(140));
    let path = stores.claude_session(name, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #282 — the drawings an asker gave its options are on the card, on BOTH pages: on demand, but
/// for the chosen option's, which opens with the card.
///
/// 260 of the 832 options the owner's sessions offered came with a preview — an ASCII mockup, a
/// file tree, a code sketch — drawn so the reader could compare them, and the card dropped every
/// one: the options it listed were the labels and descriptions alone.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_show_the_drawings_an_asker_gave_its_options() {
    let _serial = serial();
    for (surface, port) in [(Surface::Classic, 0), (Surface::AppShell, 3034)] {
        let fx = ask_previewed_fixture(match surface {
            Surface::Classic => "ask-previewed-classic",
            _ => "ask-previewed-app",
        });
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        let card = match surface {
            Surface::Classic => ".irq",
            Surface::AppShell => ".input-request",
        };
        let read = format!(
            "JSON.stringify([...(document.querySelector('{card}') || document).querySelectorAll('{card} details')].map(function (d) {{ return {{ open: d.open, summary: d.querySelector('summary').textContent, drawing: (d.querySelector('pre') || {{}}).textContent || '' }}; }}))"
        );
        let seen: Vec<serde_json::Value> = eval(&page.tab, &read)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        assert_eq!(
            seen.len(),
            2,
            "{surface:?}: one preview per option that has one: {seen:?}"
        );
        assert_eq!(
            (
                seen[0]["open"].clone(),
                seen[0]["drawing"].clone(),
                seen[1]["open"].clone(),
                seen[1]["drawing"].clone()
            ),
            (
                serde_json::json!(true),
                serde_json::json!(harness::PREVIEW_BOARD),
                serde_json::json!(false),
                serde_json::json!(harness::PREVIEW_LIST)
            ),
            "{surface:?}: the chosen option's drawing opens with the card, verbatim; the other \
             waits: {seen:?}"
        );
        // …and opens when the reader asks for it.
        eval(
            &page.tab,
            &format!("document.querySelectorAll('{card} details')[1].querySelector('summary').click(); 'ok'"),
        );
        assert_eq!(
            eval(
                &page.tab,
                &format!("document.querySelectorAll('{card} details')[1].open")
            )
            .as_bool(),
            Some(true),
            "{surface:?}: a closed preview opens on a click"
        );
    }
}

/// #260 — the outline column holds only the offset the CHAIN sold it.
///
/// The owner saw it in a recording and could not reproduce it: "the tasks drawer overlaps with
/// the turns drawer … This should never happen."
///
/// The drawer model says a push is not a scroll: it is a budget spent closing drawers from the
/// top, and the column's own `scrollTop` moves only with whatever is left once they are shut — by
/// which point the content is heads and gaps, and there is nothing left to reveal. So under the
/// model the column never rests scrolled, and that is exactly what keeps the sticky heads
/// (z-index rising downward) in order.
///
/// `scrollTop` has writers the chain never sees, though: `scrollIntoView` on a card or a row, a
/// focus ring following a click, a scrollbar drag. The demo tape framed each pane with
/// `scrollIntoView`, the column took an offset no drawer had paid for, Turns caught its slot and
/// held while Tasks kept coming — and Tasks came to rest 106px INSIDE the Turns body, cutting a
/// turn row in half. The trackpad goes through the chain, which never sells that offset, which is
/// why the owner's own window looked right.
///
/// So this asserts the model rather than a layout: the offset is given back, the gaps survive it,
/// a full push leaves the column at rest with every head reachable, and the short-window case
/// where the breathing room has to go stays a CORNER — off at every ordinary size.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_the_outline_column_holds_only_the_offset_the_chain_sold() {
    let _serial = serial();
    let fx = fixture_workflow_phased("outline-offset");
    let page = open_with(Surface::AppShell, &fx, 3012, "mountall=1");
    harness::show_every_session(&page.tab, &page.tab.get_url());
    await_tail(
        &page.tab,
        Surface::AppShell,
        "a fresh open to land at the tail",
    );
    settle();
    // The column, as the model talks about it: where it rests, whether any card has come to sit
    // inside the body above it, and whether every head is still reachable.
    const COLUMN: &str = "(function(){var n=document.getElementById('sessionNavigator'); \
         if(!n) return JSON.stringify({miss:1}); var v=n.getBoundingClientRect(); \
         var cards=[...n.querySelectorAll('.outline-card')]; var gaps=[]; \
         for(var i=1;i<cards.length;i++) gaps.push(Math.round(cards[i].getBoundingClientRect().top-cards[i-1].getBoundingClientRect().bottom)); \
         var heads=cards.map(function(c){var h=c.querySelector(':scope > .outline-card-head').getBoundingClientRect(); \
           return h.top>=v.top-1 && h.bottom<=v.bottom+1;}); \
         return JSON.stringify({scroll:Math.round(n.scrollTop), gaps:gaps, \
           minGap:gaps.length?Math.min.apply(null,gaps):999, cards:cards.length, \
           open:n.querySelectorAll('.outline-card.open').length, \
           headsReachable:heads.every(Boolean), tight:n.classList.contains('heads-tight')});})()";
    let read = |tab: &headless_chrome::Tab| -> serde_json::Value {
        eval(tab, COLUMN)
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null)
    };
    let open_every_pane = |tab: &headless_chrome::Tab| {
        eval(
            tab,
            "(function(){[...document.querySelectorAll('.outline-card:not(.open) > .outline-card-head')].forEach(function(h){h.click();});return 'ok';})()",
        );
        harness::until_drawers_settle(tab);
        settle();
    };
    // Ordinary windows and one no reader would choose. 1366x768 is SHORTER than 1280x800, so the
    // sweep is by height as much as by width; 950x520 is the corner where the breathing room has
    // to go for the shut chain to fit at all.
    for (w, h, corner) in [
        (1680.0, 1050.0, false),
        (1440.0, 900.0, false),
        (1366.0, 768.0, false),
        (1280.0, 800.0, false),
        (1152.0, 720.0, false),
        (1024.0, 640.0, false),
        (950.0, 520.0, true),
    ] {
        harness::resize(&page.tab, w, h);
        harness::until_preview_parked(&page.tab);
        settle();
        open_every_pane(&page.tab);

        // 1. What the tape did: frame a card with `scrollIntoView`. The column takes the offset
        //    for an instant and hands it straight back, because no drawer paid for it.
        eval(
            &page.tab,
            "(function(){var c=document.querySelectorAll('.outline-card'); if(c.length) c[c.length-1].scrollIntoView({block:'center'}); return 'ok';})()",
        );
        settle();
        let seen = read(&page.tab);
        assert!(
            seen["cards"].as_i64().unwrap_or(0) >= 3 && seen["open"].as_i64().unwrap_or(0) >= 3,
            "the case needs every pane open to be about anything: {seen}"
        );
        assert_eq!(
            seen["scroll"].as_i64(),
            Some(0),
            "at {w}x{h} `scrollIntoView` on a card buys no offset — the chain never sold it: \
             {seen}"
        );
        assert!(
            seen["minGap"].as_i64().unwrap_or(-1) >= 0,
            "at {w}x{h} no card has come to rest inside the body above it: {seen}"
        );

        // 2. And a bare write, which is the same hole with the politeness removed.
        eval(
            &page.tab,
            "(function(){document.getElementById('sessionNavigator').scrollTop=200;return 'ok';})()",
        );
        settle();
        let seen = read(&page.tab);
        assert_eq!(
            seen["scroll"].as_i64(),
            Some(0),
            "at {w}x{h} a bare `scrollTop` write is given back too: {seen}"
        );
        assert!(
            seen["minGap"].as_i64().unwrap_or(-1) >= 0,
            "at {w}x{h} …and the gaps survive it: {seen}"
        );

        // 3. The chain itself still works, and ends where the model says it ends: every drawer
        //    shut, the column at rest, every head reachable. This is the requirement the owner
        //    stated — "the column needs to fit all the header portion of the panes + some gap
        //    space between them" — observed rather than computed.
        eval(
            &page.tab,
            "(function(){var n=document.getElementById('sessionNavigator'); \
               n.dispatchEvent(new WheelEvent('wheel',{deltaY:6000,bubbles:true,cancelable:true})); return 'ok';})()",
        );
        harness::until_drawers_settle(&page.tab);
        settle();
        let seen = read(&page.tab);
        assert_eq!(
            seen["scroll"].as_i64(),
            Some(0),
            "at {w}x{h} a push that spends the whole budget leaves the column at rest: {seen}"
        );
        assert_eq!(
            seen["headsReachable"],
            serde_json::json!(true),
            "at {w}x{h} every head is inside the column once the chain has spent everything: \
             {seen}"
        );
        assert!(
            seen["minGap"].as_i64().unwrap_or(-1) >= 0,
            "at {w}x{h} …with the gaps intact: {seen}"
        );
        assert_eq!(
            seen["tight"],
            serde_json::json!(corner),
            "at {w}x{h} the breathing room goes only where the shut chain needs it — this is a \
             corner case and stays one: {seen}"
        );
    }
}

/// A compaction that put five files BACK INTO CONTEXT, each with its bytes and its line count.
fn restored_files_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at(
        "question 13: keep going after the compaction",
        &now_minus(200),
    );
    t += &harness::compaction_at(&now_minus(198));
    // The shape the owner photographed: a run of them, and a display path long enough that a
    // head which does not contain it runs out over the page's own ground.
    for (i, stem) in [
        "btfg1gosc",
        "ba413r7ny",
        "bxnz7o3q6",
        "bykgu2o75",
        "b4pbkjf4i",
    ]
    .iter()
    .enumerate()
    {
        let path = format!("/private/tmp/agent-502/-w-demo/39c9c62e-e288-406a-923e-424ff29d6abd/tasks/{stem}.output");
        let display = format!("../../../../private/tmp/agent-502/-w-demo/39c9c62e-e288-406a-923e-424ff29d6abd/tasks/{stem}.output");
        t += &harness::restored_file_at(&path, &display, 9 + i as u32, &now_minus(196 - i as u64));
    }
    t += &assistant_at("All five are back in context.", &now_minus(188));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #261 — a file a compaction put back into context says WHAT it is and HOW BIG, and its path
/// stays inside the box drawn for it.
///
/// The owner compared the two, screenshot to screenshot: Claude Code's own TUI prints
/// `Read …/btfg1gosc.output (9 lines)`, and agent-monitor drew five bold paths with no word at
/// all beside them — "bare file names with no verb. Additionally, the path goes out of the
/// boxes." Measured before the fix, at 1440x900: the head's title WAS the path, its target was
/// empty, and the head ran 92px past its own record's right edge.
///
/// Two claims, and the second is the one a stylesheet can quietly break again: every row names
/// its kind and its line count, and NOTHING inside a row is wider than the row. Both are
/// asserted at two widths, because a head that fits at 1440 is not a head that fits.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn scenario_a_restored_file_names_itself_and_stays_in_its_box() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = restored_files_fixture(match surface {
            Surface::Classic => "restored-files-classic",
            Surface::AppShell => "restored-files-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 3014 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        // Each page holds the row differently — the classic page draws one `.amark` line, the
        // shell a record whose head is a title and a target — so the QUESTION is the same and
        // only the selector differs: what word names the row, what size does it state, and does
        // anything inside it stick out of it.
        let js = match surface {
            Surface::Classic => "(function(){ var rows = [...document.querySelectorAll('.amark')]; \
                 return JSON.stringify({ n: rows.length, rows: rows.map(function(r){ \
                   var rb = r.getBoundingClientRect(); \
                   var out = [...r.querySelectorAll('*')].filter(function(e){ var b = e.getBoundingClientRect(); \
                     return b.width > 0 && (b.right > rb.right + 1 || b.left < rb.left - 1); }).length; \
                   var k = r.querySelector('.akind'), s = r.querySelector('.asize'); \
                   return { kind: k ? k.textContent.trim() : '', size: s ? s.textContent.trim() : '', \
                     text: r.textContent.trim(), outside: out }; }) }); })()",
            Surface::AppShell => "(function(){ var rows = [...document.querySelectorAll('[data-record-kind=\"attachment\"]')]; \
                 return JSON.stringify({ n: rows.length, rows: rows.map(function(r){ \
                   var rb = r.getBoundingClientRect(); \
                   var out = [...r.querySelectorAll('*')].filter(function(e){ var b = e.getBoundingClientRect(); \
                     return b.width > 0 && (b.right > rb.right + 1 || b.left < rb.left - 1); }).length; \
                   var k = r.querySelector('.renderer-title'), s = r.querySelector('.renderer-state'); \
                   return { kind: k ? k.textContent.trim() : '', size: s ? s.textContent.trim() : '', \
                     text: r.textContent.trim(), outside: out }; }) }); })()",
        };
        for width in [1440.0, 1024.0] {
            harness::resize(&page.tab, width, 900.0);
            if surface == Surface::AppShell {
                harness::until_preview_parked(&page.tab);
            }
            settle();
            let seen: serde_json::Value = eval(&page.tab, js)
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Null);
            let rows = seen["rows"].as_array().cloned().unwrap_or_default();
            assert!(
                rows.len() >= 5,
                "{surface:?} at {width}: the five restored files are all drawn: {seen}"
            );
            for (i, row) in rows.iter().take(5).enumerate() {
                let kind = row["kind"].as_str().unwrap_or("");
                let size = row["size"].as_str().unwrap_or("");
                let text = row["text"].as_str().unwrap_or("");
                assert!(
                    !kind.is_empty() && !kind.contains('/'),
                    "{surface:?} at {width}: row {i} names what it IS rather than leading with \
                     the path: {row}"
                );
                assert!(
                    text.contains(&format!("{} lines", 9 + i)),
                    "{surface:?} at {width}: row {i} states the size the transcript recorded \
                     ({} lines) — the count Claude Code prints and we used to drop: {row}",
                    9 + i
                );
                assert!(
                    !size.is_empty() || text.contains("lines"),
                    "{surface:?} at {width}: the size is somewhere a reader can see: {row}"
                );
                assert_eq!(
                    row["outside"].as_i64(),
                    Some(0),
                    "{surface:?} at {width}: nothing inside row {i} is wider than the row — the \
                     path used to run 92px past the record's own right edge: {row}"
                );
            }
        }
    }
}

/// A Bash call whose command is far wider than any head, with the needle deep inside the
/// clipped part and once more in the output.
fn clipped_target_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: run the long one", &now_minus(200));
    t += &harness::long_command_at(NEEDLE_262, "call-262", &now_minus(198));
    t += &format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"call-262\",\"content\":\"start\\n{NEEDLE_262}\\ndone\"}}]}},\"timestamp\":\"{}\"}}\n",
        now_minus(196)
    );
    t += &assistant_at("That is the long one.", &now_minus(194));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// A word that occurs nowhere else in a fixture, so a count is a fact about this case alone.
const NEEDLE_262: &str = "NEEDLE-IN-THE-HEAD";

/// #262 — every hit the counter claims can actually be SEEN.
///
/// The owner searched a real session for "WIDTH ONLY", got 4 hits, and could not find one of
/// them. It was not scrolled off: it sat inside the record head's one-line target — `nowrap`
/// with `overflow-x:hidden`, measured 571px of box over a 44,784px command — about 9,000px
/// outside a box with no scrollbar whose `scrollLeft` nobody writes. Counted, marked, and
/// unreachable by anything the reader could do. BOTH pages had it, so the classic page is not
/// the reference here.
///
/// Asserted with `elementFromPoint` at the mark's centre, never with a bounding rect: a clipped
/// mark still measures a perfectly good rectangle inside the viewport, which is exactly why
/// this went unnoticed (the repo has a memory about it — "rect is not visibility").
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn scenario_every_counted_search_hit_can_be_seen() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = clipped_target_fixture(match surface {
            Surface::Classic => "clipped-target-classic",
            Surface::AppShell => "clipped-target-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 3016 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        let hits = harness::search(&page.tab, surface, NEEDLE_262);
        assert_eq!(
            hits, 2,
            "{surface:?}: the needle is in the command and in the output, and the page counts \
             both — the case is about what those two counts are WORTH"
        );
        // The current hit, hit-tested rather than measured. `clip` names an ancestor that both
        // clips AND has the mark outside its box — the page itself overflows by 4px and would
        // otherwise be reported as the culprit — so a regression says WHAT hid the hit rather
        // than just "not visible".
        let js = match surface {
            Surface::Classic => "mark.hl.cur",
            Surface::AppShell => "mark.search-mark.current",
        };
        let look = format!(
            "(function(){{ var m = document.querySelector('{js}'); \
               if (!m) return JSON.stringify({{ none: true }}); \
               var r = m.getBoundingClientRect(); \
               var e = document.elementFromPoint(Math.round(r.left + 2), Math.round(r.top + r.height / 2)); \
               var clip = null; \
               for (var p = m.parentElement; p && p !== document.body; p = p.parentElement) {{ \
                 var cs = getComputedStyle(p); \
                 if ((cs.overflowX === 'hidden' || cs.overflowX === 'clip') && p.scrollWidth > p.clientWidth + 1) {{ \
                   var pb = p.getBoundingClientRect(); \
                   if (r.right <= pb.left + 1 || r.left >= pb.right - 1) {{ \
                     clip = p.tagName + '.' + String(p.className).slice(0, 24) + ' ' + p.clientWidth + '/' + p.scrollWidth; break; }} }} }} \
               return JSON.stringify({{ left: Math.round(r.left), top: Math.round(r.top), \
                 seen: !!e && (e === m || m.contains(e) || e.contains(m)), clip: clip }}); }})()"
        );
        // Land on the first hit the way a reader does: typing MARKS the hits, stepping is what
        // takes you to one. The classic page has no current mark until then.
        harness::search_next(&page.tab, surface);
        settle();
        // Every hit in turn, and once more round: the FIRST landing is its own path on the
        // classic page (it rebuilds the block and throws away the reveal), and it was the one
        // that stayed hidden after the stepping path had already been fixed.
        for step in 0..=2 {
            let seen: serde_json::Value = eval(&page.tab, &look)
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(serde_json::Value::Null);
            assert!(
                seen["none"].as_bool() != Some(true),
                "{surface:?} step {step}: a counted hit has a current mark to land on: {seen}"
            );
            assert_eq!(
                seen["clip"],
                serde_json::Value::Null,
                "{surface:?} step {step}: nothing is clipping the hit the reader was sent to — \
                 the head's target opens out to show it, the way its own third click step \
                 would: {seen}"
            );
            assert_eq!(
                seen["seen"],
                serde_json::json!(true),
                "{surface:?} step {step}: the hit is REALLY there — hit-tested, because a \
                 clipped mark still reports a fine rectangle: {seen}"
            );
            harness::search_next(&page.tab, surface);
            settle();
        }
    }
}

/// A Bash command that edited files, with the diff the transcript records for it.
fn bash_edit_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: make the edit", &now_minus(200));
    t += &harness::bash_call_at("python3 rewrite.py", "bash-263", &now_minus(198));
    t += &harness::bash_edit_diff_at("bash-263", &now_minus(196));
    t += &assistant_at("Rewritten.", &now_minus(194));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #263 — a Bash command that edits files shows the diff the transcript recorded for it.
///
/// The owner, comparing Claude Code's own TUI with agent-monitor on the same turn: "why the
/// diff were not rendered in agent-monitor? That seems a huge regression?" It was not a
/// regression — `git log -S bashEditDiff` is empty, so no line of this codebase had ever read
/// the field. Claude Code began writing it on 2026-09-13 and 975 records across the owner's
/// sessions were carrying a diff the page dropped in silence.
///
/// What this holds is the SHAPE, measured over all 975 rather than read off one example: a
/// file with several hunks, a file that changed but cannot be diffed, and a file named only
/// because `files[]` is capped at five. Every file the record names is accounted for, because
/// a Bash call's head names the COMMAND and nothing else would say which file a row belongs to.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn scenario_a_bash_command_that_edits_shows_its_diff() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = bash_edit_fixture(match surface {
            Surface::Classic => "bash-edit-classic",
            Surface::AppShell => "bash-edit-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 3018 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        settle();
        let seen: serde_json::Value = eval(
            &page.tab,
            "(function(){ var t = document.body.innerText; \
               var add = [...document.querySelectorAll('.add')].map(function(e){ return e.textContent.trim(); }); \
               var del = [...document.querySelectorAll('.del')].map(function(e){ return e.textContent.trim(); }); \
               return JSON.stringify({ \
                 edited: t.indexOf('CHANGELOG.md · Added 2 lines, removed 2 lines') >= 0, \
                 binary: t.indexOf('bundle.tar.gz · changed') >= 0, \
                 capped: t.indexOf('past-the-cap.txt · changed') >= 0, \
                 fresh: add.some(function(r){ return r.indexOf('fresh line') >= 0; }), \
                 removed: del.some(function(r){ return r.indexOf('gone line') >= 0; }), \
                 tail: del.some(function(r){ return r.indexOf('removed tail') >= 0; }), \
                 numbered: add.some(function(r){ return /^\\s*10\\s*\\+/.test(r); }), \
                 adds: add.length, dels: del.length }); })()",
        )
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            seen["edited"],
            serde_json::json!(true),
            "{surface:?}: the edited file names ITSELF and its counts — the head names the \
             command, so nothing else would: {seen}"
        );
        assert_eq!(
            seen["fresh"],
            serde_json::json!(true),
            "{surface:?}: an added line is drawn as an addition: {seen}"
        );
        assert_eq!(
            seen["removed"],
            serde_json::json!(true),
            "{surface:?}: …and a removed line as a removal: {seen}"
        );
        assert_eq!(
            seen["tail"],
            serde_json::json!(true),
            "{surface:?}: the file's SECOND hunk is drawn too — a file is several groups, as an \
             Edit's is (measured: 1..23 hunks per file across the corpus): {seen}"
        );
        assert_eq!(
            seen["numbered"],
            serde_json::json!(true),
            "{surface:?}: the rows carry the transcript's real line numbers, not a local count: \
             {seen}"
        );
        assert_eq!(
            seen["binary"],
            serde_json::json!(true),
            "{surface:?}: a file that changed but cannot be diffed is NAMED — the record knows \
             it changed and that is worth saying: {seen}"
        );
        assert_eq!(
            seen["capped"],
            serde_json::json!(true),
            "{surface:?}: and so is a file past `files[]`'s five-file cap, recovered from \
             `changedFiles` so `moreFiles` is a name rather than a number: {seen}"
        );
    }
}

/// A session whose run changed model half way through.
fn model_fallback_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: keep going", &now_minus(200));
    t += &assistant_at("Starting on it.", &now_minus(198));
    t += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[\
{{\"type\":\"fallback\",\"from\":{{\"model\":\"claude-fable-5\"}},\"to\":{{\"model\":\"claude-opus-4-8\"}}}}]}},\"timestamp\":\"{}\"}}\n",
        now_minus(196)
    );
    t += &assistant_at("Carrying on.", &now_minus(194));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #265 — a run that changed model says so.
///
/// Found by #264's unknown-shape log the first time it was pointed at the largest sessions: a
/// `fallback` content block, five of them across three sessions and four client versions,
/// arriving unnoticed since at least 2.1.220 and rendered nowhere. It matters because the page
/// names ONE model for a session, and after a fallback that answer is wrong for every turn
/// that follows — which also changes what the cost means.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn scenario_a_model_fallback_is_named_on_the_page() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = model_fallback_fixture(match surface {
            Surface::Classic => "model-fallback-classic",
            Surface::AppShell => "model-fallback-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 3020 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        settle();
        let seen: serde_json::Value = eval(
            &page.tab,
            "(function(){ var t = document.body.innerText; \
               return JSON.stringify({ \
                 named: t.indexOf('Model fallback') >= 0, \
                 from: t.indexOf('claude-fable-5') >= 0, \
                 to: t.indexOf('claude-opus-4-8') >= 0 }); })()",
        )
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            seen["named"],
            serde_json::json!(true),
            "{surface:?}: the page says the run changed model: {seen}"
        );
        assert_eq!(
            seen["from"],
            serde_json::json!(true),
            "{surface:?}: …naming the model it left: {seen}"
        );
        assert_eq!(
            seen["to"],
            serde_json::json!(true),
            "{surface:?}: …and the one it went to, which is what every turn after this ran on: \
             {seen}"
        );
    }
}

/// An edit deep in a big file, so every line number in the rail is five digits.
fn deep_line_numbers_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: edit deep in the big file", &now_minus(200));
    t += &harness::deep_edit_at("/w/big.rs", 13144, 10, "deep-1", &now_minus(198));
    t += &assistant_at("Edited.", &now_minus(196));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 13,
    }
}

/// #266 — the diff's line-number rail holds its number on ONE line.
///
/// The owner photographed a diff whose numbers read "1314" over "5". Two causes, and fixing
/// either alone leaves the bug reachable: the rail was a FIXED four-digit width (38px on the
/// shell, 42px classic), and the wrap control reached into it — wrap is for long lines of code,
/// and a line number has no break worth taking. Measured before: the shell's rail went 22px →
/// 43px with wrap on (97px at code-size 18), and the classic page did it out of the box,
/// because wrap is its default there.
///
/// Asserted against the row's own line height rather than a pixel constant, so a font change
/// cannot quietly make this pass.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn scenario_a_five_digit_line_number_stays_on_one_line() {
    let _serial = serial();
    for surface in [Surface::Classic, Surface::AppShell] {
        let fx = deep_line_numbers_fixture(match surface {
            Surface::Classic => "deep-lines-classic",
            Surface::AppShell => "deep-lines-app",
        });
        let port = if surface == Surface::Classic { 0 } else { 3022 };
        let page = open_with(surface, &fx, port, "mountall=1");
        jump_to_end(&page.tab, surface);
        await_tail(&page.tab, surface, "a fresh open to land at the tail");
        settle();
        open_everything(&page.tab, surface);
        settle();
        let gut = match surface {
            Surface::Classic => ".gut",
            Surface::AppShell => ".ln",
        };
        // Wrap ON is the state the owner was in, and the classic page's default. Two code sizes,
        // because a rail that fits at 12px is not a rail that fits.
        for size in [12, 18] {
            eval(
                &page.tab,
                &format!(
                    "(function(){{ var a = document.getElementById('app'); \
                       if (a) {{ a.classList.add('wrap-code'); a.style.setProperty('--code-size', '{size}px'); }} \
                       else {{ document.documentElement.style.setProperty('--code-size', '{size}px'); }} \
                       return 'ok'; }})()"
                ),
            );
            settle();
            let seen: serde_json::Value = eval(
                &page.tab,
                &format!(
                    "(function(){{ var g = [...document.querySelectorAll('{gut}')].filter(function(e){{ return e.textContent.trim(); }}); \
                       if (!g.length) return JSON.stringify({{ none: true }}); \
                       var worst = null; \
                       g.forEach(function(e){{ var r = e.getBoundingClientRect(); \
                         var lh = parseFloat(getComputedStyle(e).lineHeight) || 16; \
                         var ratio = r.height / lh; \
                         if (!worst || ratio > worst.ratio) worst = {{ text: e.textContent.trim(), \
                           h: Math.round(r.height), lh: Math.round(lh), w: Math.round(r.width), ratio: ratio }}; }}); \
                       return JSON.stringify({{ n: g.length, worst: worst }}); }})()"
                ),
            )
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(serde_json::Value::Null);
            assert!(
                seen["n"].as_i64().unwrap_or(0) >= 5,
                "{surface:?} at {size}px: the diff drew its rail: {seen}"
            );
            let worst = &seen["worst"];
            assert!(
                worst["text"].as_str().unwrap_or("").len() >= 5,
                "{surface:?} at {size}px: the fixture's numbers really are five digits — a \
                 four-digit rail would pass this case for the wrong reason: {seen}"
            );
            assert!(
                worst["ratio"].as_f64().unwrap_or(9.0) < 1.5,
                "{surface:?} at {size}px: every number sits on ONE line — the tallest cell is \
                 {} against a {}px line: {seen}",
                worst["h"],
                worst["lh"]
            );
        }
    }
}

// ── scenario: a query and a tool facet NARROW each other (#292) ──────────────────────────────

/// design/in-session-search.md §4. The two axes were a mode switch: on the app shell a query
/// returned the TEXT matches and a tool filter its own, so typing replaced the filter
/// (`activeMatches`), and on the classic page a search counted hits in records the filter had
/// taken off the page. Now `tool:` is part of the one grammar and everything narrows: the fixture
/// says "needle" in a Bash command, in a Read's target and in an assistant's prose, so each axis
/// alone finds more than the two together.
fn scenario_a_query_and_a_tool_facet_narrow_each_other(
    tab: &headless_chrome::Tab,
    surface: Surface,
    _fx: &Fixture,
) {
    jump_to_end(tab, surface);
    await_tail(tab, surface, "a fresh open to land at the tail");
    settle();
    let (box_sel, count_sel) = match surface {
        Surface::Classic => (
            "document.getElementById('q')",
            "document.getElementById('qcount')",
        ),
        Surface::AppShell => (
            "document.getElementById('transcriptSearchInput')",
            "document.getElementById('transcriptSearchCount')",
        ),
    };
    // Type into the box the way a reader does: the page's own input handler, then Enter for the
    // pages that search on it.
    let typed = |value: &str| {
        assert_eq!(
            eval(
                tab,
                &format!(
                    "(function(){{ var b = {box_sel}; b.value = {value:?}; b.dispatchEvent(new Event('input', {{bubbles: true}})); b.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', bubbles: true}})); return 'typed'; }})()"
                ),
            ),
            "typed",
            "{surface:?}: the box takes {value:?}"
        );
        settle();
        settle();
    };
    let hits = |label: &str| -> i64 {
        let text = eval(tab, &format!("{count_sel}.textContent.trim()"))
            .as_str()
            .unwrap_or("")
            .to_string();
        let n = text
            .split_whitespace()
            .next()
            .and_then(|w| w.split('/').last())
            .and_then(|w| w.parse::<i64>().ok())
            .unwrap_or(-1);
        assert!(
            n >= 0,
            "{surface:?}: {label}: the box says a number: {text:?}"
        );
        n
    };
    typed("needle");
    let text_only = hits("text alone");
    assert!(
        text_only >= 3,
        "{surface:?}: the fixture says needle in a Bash command, a Read target and some prose: {text_only}"
    );
    typed("tool:Bash needle");
    let both = hits("text and facet");
    assert!(
        both >= 1 && both < text_only,
        "{surface:?}: `tool:Bash needle` is the Bash calls' own needles — fewer than every \
         needle ({both} of {text_only}), and not zero"
    );
    typed("tool:Read needle");
    let read = hits("the other tool");
    assert!(
        read >= 1 && read < text_only,
        "{surface:?}: …and another tool finds its own ({read} of {text_only})"
    );
    typed("needle");
    assert_eq!(
        hits("text alone again"),
        text_only,
        "{surface:?}: dropping the facet brings every hit back"
    );
}

#[test]
#[ignore = "needs a local Chrome"]
fn classic_page_a_query_and_a_tool_facet_narrow_each_other() {
    let _serial = serial();
    let fx = needle_fixture("scenario-narrow-classic");
    let page = open(Surface::Classic, &fx, 0);
    scenario_a_query_and_a_tool_facet_narrow_each_other(&page.tab, Surface::Classic, &fx);
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn app_shell_a_query_and_a_tool_facet_narrow_each_other() {
    let _serial = serial();
    let fx = needle_fixture("scenario-narrow-app");
    let page = open(Surface::AppShell, &fx, 3036);
    scenario_a_query_and_a_tool_facet_narrow_each_other(&page.tab, Surface::AppShell, &fx);
}

/// One word, "needle", in three places one facet can tell apart: a Bash command, a Read's target
/// and an assistant's prose. Prose between the calls so each is its own record.
fn needle_fixture(name: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let mut t = long_session(10, Shape::default());
    t += &user_at("where is the needle?", &now_minus(90));
    t += &assistant_at("Looking for the needle in the haystack.", &now_minus(85));
    t += &harness::bash_call_at("grep -rn needle src", "t-narrow-bash", &now_minus(80));
    t += &tool_result_lines("t-narrow-bash", 3, &now_minus(78));
    t += &assistant_at("Now the file itself.", &now_minus(70));
    t += &harness::read_tool_at("t-narrow-read", "/tmp/needle.txt", &now_minus(60));
    t += &tool_result_lines("t-narrow-read", 4, &now_minus(58));
    t += &assistant_at("answer narrow: the needle was in src.", &now_minus(40));
    let path = stores.claude_session(SID, &t);
    Fixture {
        base,
        path,
        turns: 11,
    }
}

/// A background session whose agent worked in its JOB tmp (#291): one file there, and one in
/// another session's job tmp. Returns the fixture and the two paths. Plain text, as `scratch_fixture`: Markdown opens
/// in mdrev's viewer (#270), a different page with the same guards.
fn job_tmp_fixture(name: &str, sid: &str) -> (Fixture, String, String) {
    let base = base(name);
    let stores = Stores::new(&base);
    // `job_home_and_session` derives the claude home from the transcript's own path — the parent
    // of the store that holds the project slugs — so here the jobs live beside the store itself.
    let home = base.join("stores");
    let job = |prefix: &str, names: &str, rel: &str, body: &str| -> String {
        let dir = home.join("jobs").join(prefix);
        std::fs::create_dir_all(dir.join("tmp")).unwrap();
        std::fs::write(dir.join("state.json"), names).unwrap();
        let p = dir.join("tmp").join(rel);
        std::fs::write(&p, body).unwrap();
        p.display().to_string()
    };
    let mine = job(
        &sid[..8],
        &format!(
            "{{\"sessionId\":\"{sid}\",\"resumeSessionId\":\"{sid}\",\"backend\":\"daemon\"}}"
        ),
        "work.txt",
        "the job's own workspace",
    );
    // Another job, of another session. (A job whose NAME shares this session's prefix while its
    // state names someone else cannot be built here — one directory, one name — so that collision
    // is held by the unit test in `claude/discover.rs`, which can write the two states in turn.)
    let other_sid = "c07adf79-0000-4000-8000-000000000291";
    let theirs = job(
        "c07adf79",
        &format!("{{\"sessionId\":\"{other_sid}\",\"resumeSessionId\":\"{other_sid}\"}}"),
        "theirs.txt",
        "another job's workspace",
    );
    let mut t = long_session(12, Shape::default());
    t += &user_at("question 13: read the job workspace", &now_minus(200));
    t += &read_tool_at("j1", &mine, &now_minus(190));
    t += &tool_result_at("j1", &now_minus(190));
    t += &read_tool_at("j2", &theirs, &now_minus(185));
    t += &tool_result_at("j2", &now_minus(185));
    t += &assistant_at("Read both.", &now_minus(180));
    let path = stores.claude_session(sid, &t);
    (
        Fixture {
            base,
            path,
            turns: 13,
        },
        mine,
        theirs,
    )
}

/// #291 — a file in the session's OWN job workspace (`~/.claude/jobs/<sid[..8]>/tmp`) opens in the
/// page, on BOTH pages; one in another session's job tmp does not. (That the eight characters are
/// checked against the job's `state.json`, and not taken as an identity, is the unit test's claim:
/// two states cannot share one directory name here.)
///
/// The owner found 32 GB in one such directory and asked what the transcript said about it: its
/// agent had been using `tmp/` as a workspace for days, and 749 of its lines named paths there —
/// none of which could render, because `/file` only serves what a hosted session EXPLAINS (#283)
/// and a job tmp was not among those places. It is now, on one condition: the job's `state.json`
/// must NAME this session, since eight characters are a prefix and not an identity.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn both_shells_open_a_file_from_the_session_s_job_workspace() {
    let _serial = serial();
    let sid = "b0bb9596-4060-4a9b-9834-a2bc736d2f6c";
    for (surface, port) in [(Surface::Classic, 3038), (Surface::AppShell, 3040)] {
        let (fx, mine, theirs) = job_tmp_fixture(
            match surface {
                Surface::Classic => "jobtmp-classic",
                _ => "jobtmp-app",
            },
            sid,
        );
        let browser = harness::chrome();
        let tab = browser.new_tab().unwrap();
        let stores = Stores {
            root: fx.base.join("stores"),
        };
        let monitor = Monitor::spawn(Kind::V2, port, &fx.base, Some(&stores), true);
        monitor.pair(&tab);
        let (ui, ready) = match surface {
            Surface::Classic => ("classic", "document.querySelectorAll('#stream .blk').length >= 3"),
            _ => ("app", "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 3"),
        };
        monitor.open(
            &tab,
            &format!("?ui={ui}&session={}&mountall=1", sid_of(&fx)),
        );
        harness::until(
            &tab,
            ready,
            "the page to render the fixture",
            Duration::from_secs(30),
            "document.body.innerText.slice(0, 200)",
        );
        settle();
        // Record every reveal, never send one: `open -R` must not run on this machine.
        eval(
            &tab,
            "window.__reveals = []; var real = window.fetch; window.fetch = function (u, o) { var s = String(u); if (/__reveal\\?/.test(s)) { window.__reveals.push(s); return Promise.resolve(new Response('', {status: 200})); } return real(u, o); }; 'ok'",
        );
        let (link, shown) = match surface {
            Surface::Classic => (".tool-path[data-path={p}]", ".lightbox pre.lb-text"),
            Surface::AppShell => (
                "[data-reference-path={p}]",
                "#previewBody pre.artifact-text",
            ),
        };
        let click = |p: &str| {
            let sel = link.replace("{p}", &format!("{p:?}"));
            eval(
                &tab,
                &format!("document.querySelector({sel:?}).click(); 'ok'"),
            );
        };
        click(&mine);
        until(
            &tab,
            &format!(
                "[...document.querySelectorAll({shown:?})].some(function (e) {{ return e.textContent.indexOf(\"the job's own workspace\") >= 0; }})"
            ),
            "the session's own job workspace, shown in the page",
            Duration::from_secs(10),
            "JSON.stringify({ reveals: window.__reveals, shown: [...document.querySelectorAll('pre')].map(function (e) { return e.className + ':' + e.textContent.slice(0, 40); }).slice(-4) })",
        );
        click(&theirs);
        let refused = match surface {
            Surface::Classic => {
                "(window.__reveals || []).some(function (u) { return u.indexOf('theirs.txt') >= 0; })"
            }
            Surface::AppShell => "!!document.querySelector('#previewBody [data-preview-reveal]')",
        };
        until(
            &tab,
            refused,
            "another job's workspace refused",
            Duration::from_secs(10),
            "JSON.stringify({ reveals: window.__reveals, pane: (document.getElementById('previewBody') || {}).className })",
        );
        assert_eq!(
            eval(
                &tab,
                &format!(
                    "[...document.querySelectorAll({shown:?})].some(function (e) {{ return e.textContent.indexOf(\"another job's workspace\") >= 0; }})"
                )
            )
            .as_bool(),
            Some(false),
            "{surface:?}: a session explains its OWN job workspace and no other's"
        );
        drop(monitor);
    }
}
