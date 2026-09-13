//! The sandbox cases (#197 stage B, design/viewport-history.md §5): an exported viewport history —
//! kinds, heights, indices, turns and timings from a real session, no content — rebuilt as a
//! synthetic transcript with the same shape and replayed with the recorded actions on the surface
//! it was recorded on, the engine's states diffed by turn. A report is a reproduction, and a short
//! one. The fixtures under `tests/fixtures/history/` are the #194 probes' own exports.
//!
//! Every case is `#[ignore]`d (a local Chrome, `cargo build --release -p claude-monitor-v2`).

mod harness;

use claude_replay_html::start_server;
use claude_replay_present::Args;
use harness::history::{
    calibration_session, fit, growth, steps, surface_of, synthetic, Calib, Diff, Export, Step,
};
use harness::{base, serial, Kind, Monitor, Stores, Surface};
use std::path::PathBuf;
use std::time::Duration;

const SID: &str = "dddddddd-0000-4000-8000-000000000001";

struct Fixture {
    base: PathBuf,
    path: PathBuf,
}

fn fixture(name: &str, jsonl: &str) -> Fixture {
    let base = base(name);
    let stores = Stores::new(&base);
    let path = stores.claude_session(SID, jsonl);
    Fixture { base, path }
}

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
            harness::until(
                &tab,
                "!!window.__viewportHistory && document.querySelectorAll('#stream .blk').length >= 1",
                "the classic page to render the synthetic session",
                Duration::from_secs(60),
                "document.querySelectorAll('#stream .blk').length",
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
                "!!window.__viewportHistory && !!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length >= 1",
                "the app shell to mount the synthetic session",
                Duration::from_secs(60),
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

/// The export of the page as it stands, parsed.
fn export_now(tab: &headless_chrome::Tab) -> Export {
    Export::from_value(harness::probe(tab, "window.__viewportHistory.export()"))
}

/// Measure the surface's prose model (§5's calibration): a short session of known lengths,
/// every record mounted, heights read back through the export.
fn calibrate(surface: Surface, port: u16, label: &str) -> Calib {
    let fx = fixture(&format!("sandbox-calib-{label}"), &calibration_session());
    let page = open(surface, &fx, port);
    // Everything mounted: the session is five turns, well inside the overscan.
    harness::until(
        &page.tab,
        "window.__viewportHistory.export().session.items.every(function (i) { return i[1] != null; })",
        "every calibration record to be measured",
        Duration::from_secs(30),
        "JSON.stringify(window.__viewportHistory.export().session.items)",
    );
    settle();
    let export = export_now(&page.tab);
    let calib = fit(&export.items, &export.page);
    eprintln!("calibration on {surface:?}: {calib:?}");
    assert!(
        calib.user.1 > 0.0 && calib.assistant.1 > 0.0,
        "{surface:?}: prose grows with its length: {calib:?}"
    );
    calib
}

fn fixture_export(name: &str) -> Export {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/history")
        .join(name);
    Export::load(&path)
}

/// Replay `export` on its own surface: the synthetic session opened, the growth timeline run
/// beside the recorded actions, the engine's state read after each; the diff summarized.
fn replay(export: &Export, surface: Surface, port: u16, label: &str, tolerance: i64) -> Vec<Diff> {
    let calib = calibrate(surface, port, label);
    let profile = export.profile();
    let opened = export.opened_records();
    eprintln!(
        "{label}: {} records in the profile, {} at the open, {} actions, {} deltas",
        profile.len(),
        opened,
        export.actions.len(),
        export.deltas.len()
    );
    let fx = fixture(
        &format!("sandbox-replay-{label}"),
        &synthetic(&profile, opened, &calib),
    );
    let script = growth(export, &profile, &calib);
    let page = open(surface, &fx, port);
    settle();
    // The profile the page built, against the recorded one: total height, and the residual on
    // the mounted records.
    let built = export_now(&page.tab);
    let recorded_sums = export
        .states
        .last()
        .and_then(|s| s["sums"].as_f64())
        .unwrap_or(0.0);
    let built_sums = built
        .states
        .last()
        .and_then(|s| s["sums"].as_f64())
        .unwrap_or(0.0);
    eprintln!(
        "{label}: sums recorded {recorded_sums:.0} built {built_sums:.0} ({:+.1}%)",
        if recorded_sums > 0.0 {
            (built_sums - recorded_sums) / recorded_sums * 100.0
        } else {
            0.0
        }
    );
    // Growth beside the replay, on the recorded timeline.
    let path = fx.path.clone();
    let grower = std::thread::spawn(move || {
        for (gap, records) in script {
            std::thread::sleep(gap);
            for record in records {
                harness::append(&path, &record);
            }
        }
    });
    let mut diffs = Vec::new();
    for (gap, step, t) in steps(export, Duration::from_millis(2000)) {
        std::thread::sleep(gap);
        match &step {
            Step::Wheel(dy) => harness::scroll_by(&page.tab, surface, *dy),
            Step::Key(name) => harness::key(
                &page.tab,
                if name == "Space" { " " } else { name.as_str() },
                false,
            ),
            Step::JumpTo(turn) => {
                let _ = harness::jump_to_turn(&page.tab, surface, (*turn).max(1) as u32);
            }
            Step::End => harness::jump_to_end(&page.tab, surface),
            Step::Fold => {
                let _ = harness::open_last_fold(&page.tab, surface);
            }
            Step::Skip(_) => {}
        }
        std::thread::sleep(Duration::from_millis(250));
        let replayed = harness::history::replayed_state(&page.tab);
        diffs.push(Diff {
            step,
            recorded: harness::history::state_after(&export.states, t),
            replayed,
        });
    }
    let _ = grower.join();
    // How close the rebuilt page came: the item count (a merged record shifts every index after
    // it), and the per-kind residual over the items measured in both.
    let after = export_now(&page.tab);
    eprintln!(
        "{label}: items recorded {} built {}",
        export.items.len(),
        after.items.len()
    );
    // Where the rebuilt page's items first stop matching the recording's, with the record
    // kinds around it (the shell groups records into units; a run split differently is one
    // unit more or fewer from there on).
    if after.items.len() != export.items.len() {
        let first = export
            .items
            .iter()
            .zip(after.items.iter())
            .position(|(a, b)| a.0 != b.0 || a.3 != b.3 || a.4 != b.4);
        if let Some(i) = first {
            let lo = i.saturating_sub(3);
            eprintln!("{label}: items diverge at {i}:");
            for j in lo..(i + 4).min(export.items.len()) {
                let rec = &export.items[j];
                let built = after.items.get(j);
                let kinds: Vec<&str> = export
                    .records
                    .as_ref()
                    .map(|r| {
                        r[rec.3 as usize..=(rec.4 as usize).min(r.len() - 1)]
                            .iter()
                            .map(|k| k.as_deref().unwrap_or("?"))
                            .collect()
                    })
                    .unwrap_or_default();
                eprintln!(
                    "    {j}: recorded {:?} [{}..{}] {:?}  built {:?}",
                    rec.0,
                    rec.3,
                    rec.4,
                    kinds,
                    built.map(|b| (b.0.clone(), b.3, b.4))
                );
            }
        }
    }
    let mut residual: std::collections::HashMap<String, (f64, f64, f64)> = Default::default();
    for (i, item) in export.items.iter().enumerate() {
        if let (Some(kind), Some(h)) = (item.0.as_ref(), item.1) {
            if let Some(b) = after.items.get(i).and_then(|row| row.1) {
                let e = residual.entry(kind.clone()).or_insert((0.0, 0.0, 0.0));
                e.0 += b - h;
                e.1 += (b - h).abs();
                e.2 += 1.0;
            }
        }
    }
    let mut kinds: Vec<_> = residual.into_iter().collect();
    kinds.sort_by(|a, b| b.1 .2.partial_cmp(&a.1 .2).unwrap());
    for (kind, (sum, abs, n)) in kinds {
        eprintln!(
            "  residual {kind:11} n={n:3.0} mean {:+7.1}px  abs {:6.1}px",
            sum / n,
            abs / n
        );
    }
    let violations = harness::probe(&page.tab, "(window.__viewportViolations || []).slice()");
    eprintln!(
        "{label} on {surface:?} (recorded on {:?}): {}",
        surface_of(export),
        harness::history::summarize(&diffs, tolerance)
    );
    for (i, d) in diffs.iter().enumerate() {
        eprintln!(
            "  {i:3} {:?} recorded {:?} replayed {:?}",
            d.step, d.recorded, d.replayed
        );
    }
    assert!(
        violations.as_array().is_some_and(Vec::is_empty),
        "{label}: the replay reported violations: {violations}"
    );
    diffs
}

/// The prose model exists and grows on both surfaces, and a record's height is predicted from
/// its length within a line.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn sandbox_calibration_measures_prose_on_both_surfaces() {
    let _serial = serial();
    let classic = calibrate(Surface::Classic, 0, "classic");
    let app = calibrate(Surface::AppShell, 2985, "app");
    for (surface, calib) in [("classic", &classic), ("app", &app)] {
        assert!(
            calib.user.1 > 0.05 && calib.assistant.1 > 0.05,
            "{surface}: a character costs height: {calib:?}"
        );
        assert!(
            calib.think > 0.0 && calib.act > 0.0,
            "{surface}: folded kinds measure: {calib:?}"
        );
    }
}

/// The walk export from the owner's session (#194's probe), replayed on the surface it was
/// recorded on: the engine's turn under P after each recorded action, against the recording.
#[test]
#[ignore = "needs a local Chrome"]
fn sandbox_walk_classic_replays() {
    let _serial = serial();
    let export = fixture_export("walk-classic.json");
    let diffs = replay(&export, surface_of(&export), 0, "walk-classic", 3);
    let worst = worst_delta(&diffs);
    assert!(
        worst <= 3,
        "walk-classic: the replay's turn stays within 3 of the recording (worst {worst})"
    );
}

/// How far apart the two pages put the reader after the same wheels, in turns, measured on the
/// classic walk replayed on the shell (§9): the bound is the measured range's top, not a guess.
const CROSS_SURFACE_DRIFT: i64 = 6;

fn worst_delta(diffs: &[Diff]) -> i64 {
    diffs
        .iter()
        .filter_map(Diff::turn_delta)
        .map(i64::abs)
        .max()
        .unwrap_or(0)
}

/// The parity instrument: the classic walk export replayed on the SHELL — the same synthetic
/// session, the same gestures, the turn under P on the other surface against the classic
/// recording. A commanded move names a turn, so after the jump the shell shows the classic page's
/// turn within the within-surface tolerance. A wheel is pixels, and the two pages lay out
/// different heights by design (the shell's process rows are shorter), so the same pixels cover
/// more turns on the shell: that drift is reported at every step and bounded at what was measured
/// (design/viewport-history.md §9), two-sided, so a change to either page's wheel handling that
/// moves the two apart — or a shell that stops moving — shows up here.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn sandbox_walk_classic_export_replays_on_the_shell() {
    let _serial = serial();
    let export = fixture_export("walk-classic.json");
    let diffs = replay(
        &export,
        Surface::AppShell,
        2987,
        "walk-classic-on-shell",
        CROSS_SURFACE_DRIFT,
    );
    // Every step compared: a shell that shows no turn at all would pass both bounds vacuously.
    let compared = diffs.iter().filter(|d| d.turn_delta().is_some()).count();
    assert_eq!(
        compared,
        diffs.len(),
        "the shell reports a turn after every one of the walk's steps"
    );
    let commanded: Vec<i64> = diffs
        .iter()
        .filter(|d| !matches!(d.step, Step::Wheel(_) | Step::Skip(_)))
        .filter_map(Diff::turn_delta)
        .map(i64::abs)
        .collect();
    assert!(!commanded.is_empty(), "the walk has a commanded move");
    let worst_commanded = commanded.iter().copied().max().unwrap_or(0);
    assert!(
        worst_commanded <= 3,
        "after a commanded move the shell shows the classic page's turn within 3 (worst {worst_commanded})"
    );
    // The wheels: the shell is the compact page, so after the same pixels it is AHEAD of the
    // classic page — never behind it by more than the within-surface tolerance (a shell that moved
    // less than the classic page is broken), and ahead by no more than the measured drift.
    let (mut behind, mut ahead) = (0i64, 0i64);
    for d in diffs.iter().filter(|d| matches!(d.step, Step::Wheel(_))) {
        if let Some(x) = d.turn_delta() {
            behind = behind.min(x);
            ahead = ahead.max(x);
        }
    }
    assert!(
        behind >= -3,
        "after a wheel the shell is never behind the classic page by more than 3 turns (behind by {})",
        -behind
    );
    assert!(
        ahead <= CROSS_SURFACE_DRIFT,
        "over the walk's wheels the shell runs ahead of the classic page by at most {CROSS_SURFACE_DRIFT} turns (ahead by {ahead})"
    );
}

/// The runaway export (the #194 probe's live case: the tail grows while the reader wheels),
/// replayed with its growth timeline — the deltas as timed appends — on the surface it was
/// recorded on.
#[test]
#[ignore = "needs a local Chrome"]
fn sandbox_runaway_classic_live_replays() {
    let _serial = serial();
    let export = fixture_export("runaway-classic-live.json");
    let diffs = replay(&export, Surface::Classic, 0, "runaway-classic-live", 3);
    let worst = worst_delta(&diffs);
    assert!(
        worst <= 3,
        "runaway-classic-live: the replay's turn stays within 3 of the recording after every action (worst {worst})"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn sandbox_runaway_app_live_replays() {
    let _serial = serial();
    let export = fixture_export("runaway-app-live.json");
    let diffs = replay(&export, Surface::AppShell, 2988, "runaway-app-live", 3);
    let worst = worst_delta(&diffs);
    assert!(
        worst <= 3,
        "runaway-app-live: the replay's turn stays within 3 of the recording after every action (worst {worst})"
    );
}

#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn sandbox_walk_app_replays() {
    let _serial = serial();
    let export = fixture_export("walk-app.json");
    let diffs = replay(&export, surface_of(&export), 2986, "walk-app", 3);
    let worst = worst_delta(&diffs);
    assert!(
        worst <= 3,
        "walk-app: the replay's turn stays within 3 of the recording (worst {worst})"
    );
}

/// The fixtures are the probes' own exports of the owner's sessions: kinds, heights, indices,
/// turns and timings, and nothing that names the session — no uuid, no path, no transcript
/// file, no string long enough to be prose. Runs without Chrome.
#[test]
fn history_fixtures_carry_no_content() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/history");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("the fixtures directory") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        seen += 1;
        let raw = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["format"], "viewport-history/1", "{path:?}");
        let mut strings = Vec::new();
        fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
            match v {
                serde_json::Value::String(s) => out.push(s.clone()),
                serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
                serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
                _ => {}
            }
        }
        walk(&value, &mut strings);
        let uuid = |s: &str| {
            s.len() >= 36
                && s.as_bytes().windows(36).any(|w| {
                    w.iter().enumerate().all(|(i, b)| match i {
                        8 | 13 | 18 | 23 => *b == b'-',
                        _ => b.is_ascii_hexdigit(),
                    })
                })
        };
        for s in &strings {
            assert!(
                !uuid(s) && !s.contains("/Users/") && !s.contains(".jsonl") && s.len() <= 80,
                "{path:?} carries content: {s:?}"
            );
        }
    }
    assert!(seen >= 2, "the walk's two exports are the first fixtures");
}
