//! **Records by turn (#s277, `design/slow-links.md` §6, stage 2 of the slow-link work).** A reader
//! over a network reads a long session's earlier turns turn by turn, in the order the view needs
//! them. When the turn index lands, the short "Loading earlier turns" card gives way to the units
//! every earlier turn will make, each a placeholder drawn at its height in the sums; the head is then
//! read in pieces and each piece is filled in where it stands. These cases hold that to the virtual
//! window's invariants (`design/virtual-window-framework.md` §2) with the check mode on: a fill moves
//! no reader, one at or above the reader waits for a still view, a turn tapped while on its way is
//! landed on at once and read first, a link lands on its row once it is here, the panes count the
//! whole session at every point, and a placeholder teaches the estimator nothing.
//!
//! The page takes stage 2 for a reader across a network; a case opens a local monitor with
//! `?windowed=1`, which picks that path, and queues every read of the head (`queue_the_head`) so it
//! decides when each piece arrives. Searching the earlier turns on the server while the head is out
//! (#s278, stage 3) is here too, and so is what a reader keeps of a long session (#s285). Ports
//! 2770–2779, 2782–2783 for what a reader keeps, and 2785–2789 for stage 3 (the sandbox holds
//! 2780–2781).

mod harness;

use harness::{serial, Surface};
use std::time::Duration;

const SID: &str = "5a5a5a5a-0000-4000-8000-000000002770";

/// A phone (or a desktop window) on a session written as `jsonl`, opened tail-first on the stage-2
/// path with the head's reads queued; returns once the tail is drawn.
fn open(
    port: u16,
    case: &str,
    jsonl: &str,
    phone: bool,
    hold_index: bool,
    extra: &str,
) -> (
    harness::Monitor,
    headless_chrome::Browser,
    std::sync::Arc<headless_chrome::Tab>,
    std::path::PathBuf,
) {
    open_with_tasks(port, case, jsonl, phone, hold_index, extra, &[])
}

/// `open`, with the session's task list (`id`, `subject`, `status`).
fn open_with_tasks(
    port: u16,
    case: &str,
    jsonl: &str,
    phone: bool,
    hold_index: bool,
    extra: &str,
    tasks: &[(&str, &str, &str)],
) -> (
    harness::Monitor,
    headless_chrome::Browser,
    std::sync::Arc<headless_chrome::Tab>,
    std::path::PathBuf,
) {
    let base = harness::base(case);
    let stores = harness::Stores::new(&base);
    let path = stores.claude_session(SID, jsonl);
    if !tasks.is_empty() {
        stores.claude_tasks(SID, tasks);
    }
    let m = harness::Monitor::spawn(harness::Kind::V1, port, &base, Some(&stores), true);
    let (browser, tab) = harness::chrome_tab();
    if phone {
        harness::phone(&tab, 390, 844);
    }
    m.pair(&tab);
    harness::queue_the_head(&tab, hold_index);
    // A case's own `tailBudget` or `headPiece` stands (the page reads the first of a name).
    let budget = if extra.contains("tailBudget=") {
        ""
    } else {
        "&tailBudget=8192"
    };
    let piece = if extra.contains("headPiece=") {
        ""
    } else {
        "&headPiece=4096"
    };
    m.open(
        &tab,
        &format!("?ui=app&session={SID}&windowed=1{budget}{piece}{extra}"),
    );
    harness::until(
        &tab,
        "window.__tailFrom != null && !!document.querySelector('.transcript .turn.user')",
        "the tail drawn",
        Duration::from_secs(20),
        STATE,
    );
    (m, browser, tab, path)
}

/// What a case reads of the page: placeholders, the short card, the Turns pane's rows and those
/// still coming, the turn count, the reads held and made, the offset, and the invariants broken.
const STATE: &str = r#"JSON.stringify({
  placeholders: document.querySelectorAll('.transcript .unit-placeholder').length,
  card: !!document.querySelector('.turn.pending-head'),
  rows: document.querySelectorAll('#navigatorTurns .outline-turn-row').length,
  pending: document.querySelectorAll('#navigatorTurns .outline-turn-pending').length,
  count: (document.getElementById('navigatorTurnCount') || {}).textContent,
  held: (window.__heldReads || []).length, reads: (window.__readLog || []).length,
  top: Math.round(document.querySelector('.transcript').scrollTop),
  violations: window.__viewportViolations || []
})"#;

const VIOLATIONS: &str = "JSON.stringify(window.__viewportViolations || [])";

/// Laid out: the index has landed and the placeholders stand where the short card was — the card is
/// gone while the Turns pane still lists turns on their way (a reader at the tail mounts none of the
/// placeholders, which are all above them).
const LAID_OUT: &str = "!document.querySelector('.turn.pending-head') && !!document.querySelector('#navigatorTurns .outline-turn-pending')";

/// Everything is here: no placeholder mounted, no Turns row still coming, no read waiting.
const WHOLE: &str = "!document.querySelector('.transcript .unit-placeholder') && !document.querySelector('#navigatorTurns .outline-turn-pending') && !document.querySelector('.turn.pending-head') && (window.__heldReads || []).length === 0";

fn state(tab: &headless_chrome::Tab) -> serde_json::Value {
    serde_json::from_str(harness::eval(tab, STATE).as_str().unwrap_or("{}")).unwrap_or_default()
}

/// The unit the reader sees whole nearest the top of the transcript: its key and its offset there.
const ANCHOR: &str = r#"(function(){
  var s = document.querySelector('.transcript'), top = s.getBoundingClientRect().top;
  var els = [].slice.call(document.querySelectorAll('.virtual-window > [data-unit-key]'));
  var e = els.filter(function (e) { var r = e.getBoundingClientRect(); return r.top >= top && r.top < top + s.clientHeight / 2; })[0];
  return e ? JSON.stringify({ key: e.dataset.unitKey, top: e.getBoundingClientRect().top - top }) : null;
})()"#;

fn anchor(tab: &headless_chrome::Tab) -> (String, f64) {
    let v: serde_json::Value =
        serde_json::from_str(harness::eval(tab, ANCHOR).as_str().expect("a unit in view")).unwrap();
    (
        v["key"].as_str().unwrap().to_string(),
        v["top"].as_f64().unwrap(),
    )
}

/// Where unit `key` stands now, against the transcript's top, or None when it is not mounted.
fn top_of(tab: &headless_chrome::Tab, key: &str) -> Option<f64> {
    harness::eval(
        tab,
        &format!(
            "(function(){{ var s = document.querySelector('.transcript'); var e = document.querySelector('.virtual-window > [data-unit-key=\"{key}\"]'); return e ? e.getBoundingClientRect().top - s.getBoundingClientRect().top : null; }})()"
        ),
    )
    .as_f64()
}

/// Release the head's reads one at a time, letting each settle, until `done` holds.
fn release_until(tab: &headless_chrome::Tab, done: &str, what: &str) {
    let started = std::time::Instant::now();
    loop {
        if harness::eval(tab, done).as_bool() == Some(true) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{what}: {}",
            harness::eval(tab, STATE)
        );
        harness::eval(tab, "window.__release(1)");
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// Where turn `n` starts in the log, from the turn index the monitor serves.
fn turn_offset(tab: &headless_chrome::Tab, n: i64) -> i64 {
    harness::eval(
        tab,
        &format!("(async function(){{ var r = await fetch('/turn-index?session={SID}&epoch=1&upto=1000000'); var j = await r.json(); var t = (j.turns || []).filter(function (t) {{ return Number(t.turn) === {n}; }})[0]; return t ? t.o : null; }})()"),
    )
    .as_i64()
    .expect("the turn in the index")
}

fn session(turns: u32) -> String {
    harness::long_session(turns, harness::Shape::default())
}

/// The index lands while the reader is scrolled up inside the tail: the short card gives way to the
/// whole session — every turn in the Turns pane, the count whole — and the turn they were reading
/// stays where it was, within a pixel. A piece of a far turn is filled in without redrawing a single
/// mounted element; then the rest of the head arrives, piece by piece, and the reader is still on
/// their turn, with no invariant broken.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_index_lays_the_session_out_and_no_fill_moves_the_reader() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2770, "by-turn-laid-out", &session(60), false, true, "");
    harness::until(
        &tab,
        "document.querySelector('.turn.pending-head') && window.__heldIndex.length === 1",
        "the short card while the index is held",
        Duration::from_secs(10),
        STATE,
    );
    harness::scroll_by(&tab, Surface::AppShell, -260);
    std::thread::sleep(Duration::from_millis(500));
    let (key, top) = anchor(&tab);
    harness::eval(&tab, "window.__releaseIndex(); 1");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(400));
    let laid = state(&tab);
    assert_eq!(laid["rows"], 60, "every turn is in the Turns pane: {laid}");
    assert_eq!(laid["count"], "60", "…and counted: {laid}");
    assert!(
        laid["pending"].as_i64().unwrap() > 40,
        "most still coming: {laid}"
    );
    let after = top_of(&tab, &key).expect("the reader's turn is still mounted");
    assert!(
        (after - top).abs() <= 1.0,
        "the reader's turn {key} stays where it was when the placeholders are laid out: {top} → {after}"
    );

    // Release reads until the next one is the session's first turn (read last: the rest goes from the
    // tail backwards), far above the window; then fill it, and nothing mounted is redrawn.
    release_until(
        &tab,
        "window.__heldReads.length === 1 && window.__heldReads[0].from === 0",
        "the first turn's read to be the one waiting",
    );
    std::thread::sleep(Duration::from_millis(400));
    let (key, top) = anchor(&tab);
    harness::eval(&tab, "window.__marked = [].slice.call(document.querySelectorAll('.virtual-window > [data-unit-key]')); window.__marked.forEach(function (e) { e.__mark = 1; }); 1");
    harness::eval(&tab, "window.__release(1)");
    harness::until(
        &tab,
        "!document.querySelector('#navigatorTurns .outline-turn-pending[data-turn-number=\"1\"]')",
        "the first turn filled",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(300));
    let kept = harness::eval(&tab, "window.__marked.filter(function (e) { return e.isConnected; }).length === window.__marked.length");
    assert_eq!(
        kept.as_bool(),
        Some(true),
        "a fill outside the window redraws no mounted element"
    );
    let after = top_of(&tab, &key).expect("still mounted");
    assert!(
        (after - top).abs() <= 0.5,
        "…and moves nothing: {top} → {after}"
    );

    release_until(&tab, WHOLE, "the whole head to arrive");
    std::thread::sleep(Duration::from_millis(400));
    let whole = state(&tab);
    assert_eq!(whole["rows"], 60, "{whole}");
    assert_eq!(whole["pending"], 0, "{whole}");
    assert_eq!(whole["count"], "60", "{whole}");
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// A reader at the tail stays at the tail, still following, while the whole head is filled in above
/// them piece by piece (framework I13).
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_reader_at_the_tail_stays_there_and_following_through_every_fill() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2771, "by-turn-tail", &session(60), true, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    assert!(
        harness::at_tail(&tab, Surface::AppShell),
        "the open lands at the tail"
    );
    let started = std::time::Instant::now();
    while harness::eval(&tab, WHOLE).as_bool() != Some(true) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the head to arrive: {}",
            harness::eval(&tab, STATE)
        );
        harness::eval(&tab, "window.__release(1)");
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            harness::at_tail(&tab, Surface::AppShell),
            "the reader is at the tail after every fill: {}",
            harness::eval(&tab, STATE)
        );
    }
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// D5 (Hong, #s279): a fill that would redraw a mounted unit at or above the reader waits for a
/// still view. Here the reader sits on the tail's first turn, with the head's last placeholders
/// mounted above it; a piece of them arrives during an iOS glide, and again while a finger rests on
/// the glass. Neither is filled in until the view is still: nothing is written into the glide, and
/// once it lands the reader's turn is where the glide left it.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_fill_at_or_above_the_reader_waits_for_a_still_view() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2772, "by-turn-still", &session(60), true, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    // A fill is a records change naming the run it rewrote: the history keeps each, with its time.
    const FILLS: &str =
        "(window.__viewportHistory.deltas || []).filter(function (d) { return d.to != null; })";
    // Up to the head's edge: the last placeholders mounted above the reader.
    for _ in 0..40 {
        let up = harness::eval(&tab, "(function(){ var s = document.querySelector('.transcript'), top = s.getBoundingClientRect().top; return [].slice.call(document.querySelectorAll('.virtual-window > .unit-placeholder')).some(function (e) { return e.getBoundingClientRect().bottom > top - 600; }); })()");
        if up.as_bool() == Some(true) {
            break;
        }
        harness::touch_glide(&tab, -900);
        std::thread::sleep(Duration::from_millis(500));
    }
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "a read of the turns above to be waiting",
        Duration::from_secs(10),
        STATE,
    );
    let before = harness::eval(&tab, &format!("{FILLS}.length"))
        .as_i64()
        .unwrap();
    harness::eval(
        &tab,
        "setTimeout(function () { window.__release(1); }, 40); 1",
    );
    let (_, writes) = harness::touch_glide(&tab, -240);
    assert_eq!(writes, 0, "nothing is written into the glide");
    let during = harness::eval(&tab, &format!("{FILLS}.filter(function (d) {{ return d.t >= window.__glideT0 && d.t <= window.__glideT1; }}).length")).as_i64().unwrap();
    assert_eq!(
        during,
        0,
        "no fill above the reader lands during the glide: {}",
        harness::eval(&tab, STATE)
    );
    let (key, top) = anchor(&tab);
    harness::until(
        &tab,
        &format!("{FILLS}.length > {before}"),
        "the fill once the view is still",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(300));
    let after = top_of(&tab, &key).expect("the reader's turn is mounted");
    assert!(
        (after - top).abs() <= 1.0,
        "the reader's turn holds through the fill: {top} → {after}"
    );

    // A finger at rest on the glass holds the next one too, until it lifts.
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "another read waiting",
        Duration::from_secs(10),
        STATE,
    );
    let before = harness::eval(&tab, &format!("{FILLS}.length"))
        .as_i64()
        .unwrap();
    harness::eval(&tab, "(function(){ var s = document.querySelector('.transcript'); var t = new Touch({ identifier: 2, target: s, clientX: 200, clientY: 400 }); s.dispatchEvent(new TouchEvent('touchstart', { touches: [t], targetTouches: [t], changedTouches: [t], bubbles: true })); window.__finger = t; window.__release(1); return 1; })()");
    std::thread::sleep(Duration::from_millis(1500));
    let held = harness::eval(&tab, &format!("{FILLS}.length"))
        .as_i64()
        .unwrap();
    harness::eval(&tab, "(function(){ var s = document.querySelector('.transcript'), t = window.__finger; s.dispatchEvent(new TouchEvent('touchend', { touches: [], targetTouches: [], changedTouches: [t], bubbles: true })); return 1; })()");
    harness::until(
        &tab,
        &format!("{FILLS}.length > {before}"),
        "the fill once the finger lifts",
        Duration::from_secs(10),
        STATE,
    );
    assert_eq!(
        held, before,
        "nothing above the reader is filled while a finger is down"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// A turn tapped in the Turns pane while it is on its way is landed on at once — its placeholder
/// shows the prompt's label — and read before anything else; it keeps its top where it landed while
/// it fills, and the subtitle and the Turns pane name it.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_turn_tapped_on_its_way_lands_at_once_and_is_read_first() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2773, "by-turn-tap", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(400));
    let o5 = turn_offset(&tab, 5);
    let held = harness::eval(&tab, "window.__heldReads.length")
        .as_i64()
        .unwrap();
    tab.find_element("#navigatorTurns .outline-turn-pending[data-turn-number=\"5\"]")
        .expect("turn 5 is listed while still coming")
        .click()
        .expect("click it");
    std::thread::sleep(Duration::from_millis(400));
    let (key, top) = (
        "user:t5".to_string(),
        top_of(&tab, "user:t5").expect("turn 5's placeholder is mounted where the jump landed"),
    );
    assert!(top < 120.0, "turn 5 is at the top of the view: {top}");
    let label = harness::eval(&tab, "(document.querySelector('.virtual-window > [data-unit-key=\"user:t5\"]') || {}).textContent || ''");
    assert!(
        label.as_str().unwrap_or("").contains("Turn 5"),
        "its placeholder names the turn: {label}"
    );
    // The read asked for before the tap goes; the next is turn 5's.
    if held > 0 {
        harness::eval(&tab, "window.__release(1)");
    }
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "a read waiting",
        Duration::from_secs(10),
        STATE,
    );
    assert_eq!(
        harness::eval(&tab, "window.__heldReads[0].from").as_i64(),
        Some(o5),
        "turn 5 is read before anything else: {}",
        harness::eval(&tab, "JSON.stringify(window.__readLog)")
    );
    release_until(
        &tab,
        "!document.querySelector('.virtual-window > .unit-placeholder[data-unit-key=\"user:t5\"]')",
        "turn 5's prompt filled",
    );
    std::thread::sleep(Duration::from_millis(400));
    let now = top_of(&tab, &key).expect("turn 5 mounted");
    assert!(
        (now - top).abs() <= 1.0,
        "turn 5 keeps its top while it fills: {top} → {now}"
    );
    let named = harness::eval(&tab, "JSON.stringify([document.getElementById('turnSubtitle').textContent.slice(0, 7), ((document.querySelector('#navigatorTurns .outline-turn-row.current') || {}).dataset || {}).turnNumber])");
    assert_eq!(
        named.as_str(),
        Some("[\"Turn 5 \",\"5\"]"),
        "the subtitle and the Turns pane name it"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// A link to a record nested in a turn still on its way lands on its row, its folds open, once that
/// turn is here — and a link the reader has moved away from before it arrives does not pull them
/// back.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_link_into_a_turn_on_its_way_lands_on_its_row_unless_the_reader_moves() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2774, "by-turn-link", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    // The record a link names: the first call nested in turn 7's first group of steps.
    let facts = harness::eval(&tab, "(async function(){ var r = await fetch('/turn-index?session=SID&epoch=1&upto=1000000'); var j = await r.json(); return JSON.stringify(j.facts || []); })()".replace("SID", SID).as_str());
    let facts: Vec<serde_json::Value> =
        serde_json::from_str(facts.as_str().unwrap_or("[]")).unwrap_or_default();
    let t7 = facts
        .iter()
        .position(|f| f[1] == "t7")
        .expect("turn 7 in the facts");
    let act = facts[t7..]
        .iter()
        .find(|f| f[0] == "act")
        .expect("a group of steps in turn 7");
    let id: u64 = act[1].as_str().unwrap()[1..].parse().unwrap();
    let nested = format!("b{}", id + 1);
    harness::eval(&tab, &format!("location.hash = '{nested}'; 1"));
    harness::until(&tab, "document.getElementById('toast').textContent.indexOf('Still loading') >= 0 || window.__heldReads.length >= 1", "the link's turn asked for", Duration::from_secs(10), STATE);
    release_until(
        &tab,
        &format!("!!document.querySelector('[data-record-id=\"{nested}\"]')"),
        "the link's turn filled",
    );
    harness::until(
        &tab,
        &format!("(function(){{ var e = document.querySelector('[data-record-id=\"{nested}\"]'); if (!e) return false; var s = document.querySelector('.transcript').getBoundingClientRect(), r = e.getBoundingClientRect(); return r.top >= s.top && r.top < s.bottom && r.height > 0; }})()"),
        "the linked row in view, its folds open",
        Duration::from_secs(10),
        STATE,
    );

    // Another link, and the reader moves before its turn arrives: it stays where they moved.
    let t3 = facts.iter().position(|f| f[1] == "t3").expect("turn 3");
    let act3 = facts[t3..].iter().find(|f| f[0] == "act").unwrap()[1]
        .as_str()
        .unwrap()
        .to_string();
    harness::eval(&tab, &format!("location.hash = '{act3}'; 1"));
    std::thread::sleep(Duration::from_millis(300));
    harness::scroll_by(&tab, Surface::AppShell, 400);
    std::thread::sleep(Duration::from_millis(500));
    let (key, top) = anchor(&tab);
    release_until(&tab, WHOLE, "the whole head to arrive");
    std::thread::sleep(Duration::from_millis(500));
    let now = top_of(&tab, &key).expect("the reader's turn is still mounted");
    assert!(
        (now - top).abs() <= 2.0,
        "a link the reader moved away from does not pull them back: {top} → {now}"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// The tail grows and a queued prompt is picked up (#165) while the head is filled in: both apply,
/// the reader at the tail stays there and following, and the Turns pane counts the new turn.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_tail_grows_while_the_head_fills() {
    let _serial = serial();
    let (_m, _b, tab, path) = open(2775, "by-turn-growth", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    for i in 0..6 {
        harness::eval(&tab, "window.__release(1)");
        let now = harness::now_minus(0);
        if i == 2 {
            harness::append(&path, &harness::queued_at("a queued one", &now));
        }
        if i == 4 {
            harness::append(&path, &harness::user_at("a queued one", &now));
            harness::append(&path, &harness::assistant_at("picked up", &now));
        }
        std::thread::sleep(Duration::from_millis(700));
        assert!(
            harness::at_tail(&tab, Surface::AppShell),
            "at the tail after step {i}: {}",
            harness::eval(&tab, STATE)
        );
    }
    release_until(&tab, WHOLE, "the whole head to arrive");
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length === 61",
        "the new turn in the Turns pane",
        Duration::from_secs(10),
        STATE,
    );
    assert!(
        harness::at_tail(&tab, Surface::AppShell),
        "still at the tail"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// The head's read is refused with a stale epoch part way through: the session starts over, and
/// is then read whole.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_stale_epoch_while_the_head_fills_starts_over() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2776, "by-turn-409", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    harness::eval(&tab, "window.__release(2)");
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "a read waiting",
        Duration::from_secs(10),
        STATE,
    );
    harness::eval(&tab, "window.__answer409 = 1; window.__release(1); 1");
    harness::until(
        &tab,
        "window.__tailPulls >= 2",
        "the session to start over: opened tail-first again",
        Duration::from_secs(15),
        STATE,
    );
    release_until(
        &tab,
        WHOLE,
        "the head to be read whole after the start over",
    );
    let whole = state(&tab);
    assert_eq!(whole["rows"], 60, "{whole}");
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// A turn of many records shows its prompt from its first piece: the prompt is whole, and the
/// group of steps behind it stays a placeholder that says how much of it has arrived.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_large_turn_shows_its_prompt_from_its_first_piece() {
    let _serial = serial();
    let mut jsonl = harness::user_at("the big one", &harness::at("00:01"));
    // Forty edits, each a record of its own (Bash and Read would fold into one), so the group of
    // steps behind the prompt spans many pieces.
    for k in 0..40 {
        jsonl += &harness::edit_tool_at(
            &format!("x{k}"),
            &format!("/r/petrel-{k}.rs"),
            &harness::at("00:02"),
        );
        jsonl += &harness::tool_result_at(&format!("x{k}"), &harness::at("00:02"));
    }
    jsonl += &harness::assistant_at("done with the big one", &harness::at("00:03"));
    jsonl += &session(40);
    let (_m, _b, tab, _) = open(2777, "by-turn-large", &jsonl, false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    tab.find_element("#navigatorTurns .outline-turn-row[data-turn-number=\"1\"]")
        .expect("turn 1 listed")
        .click()
        .expect("click it");
    release_until(
        &tab,
        "!!document.querySelector('.virtual-window > [data-unit-key=\"user:t1\"]:not(.unit-placeholder)')",
        "turn 1's prompt filled",
    );
    let process = harness::eval(&tab, "(function(){ var e = document.querySelector('.virtual-window > .unit-placeholder.placeholder-process'); return e ? e.textContent : null; })()");
    assert!(
        process.as_str().is_some_and(|t| t.contains(" of ")),
        "the group of steps behind it is still coming, and says how much of it has arrived: {process}"
    );
    release_until(&tab, WHOLE, "the whole head to arrive");
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// A placeholder teaches the estimator nothing: the means the engine applied before the index
/// landed are the means it holds once dozens of placeholders have been mounted (read from the
/// viewport history's states, design/viewport-history.md).
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn placeholders_teach_the_estimator_nothing() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2778, "by-turn-estimates", &session(60), false, true, "");
    harness::until(
        &tab,
        "document.querySelector('.turn.pending-head') && window.__heldIndex.length === 1",
        "the short card while the index is held",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(600));
    // The history keeps the default kind's means (a group of steps, the kind most placeholders are):
    // the applied estimate and the live mean behind it.
    const MEANS: &str = "JSON.stringify((function(){ var s = window.__viewportHistory.states; var last = s[s.length - 1] || {}; return last.live == null ? null : [last.estimate, last.live]; })())";
    let before = harness::eval(&tab, MEANS);
    harness::eval(&tab, "window.__releaseIndex(); 1");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    // Walk up through the placeholders, mounting dozens of them, and note each one mounted.
    const SEEN: &str = "(function(){ window.__seen = window.__seen || {}; [].forEach.call(document.querySelectorAll('.virtual-window > .unit-placeholder'), function (e) { window.__seen[e.dataset.unitIndex] = 1; }); return Object.keys(window.__seen).length; })()";
    for _ in 0..6 {
        harness::scroll_by(&tab, Surface::AppShell, -1500);
        std::thread::sleep(Duration::from_millis(300));
        harness::eval(&tab, SEEN);
    }
    std::thread::sleep(Duration::from_millis(800));
    let seen = harness::eval(&tab, SEEN).as_i64().unwrap_or(0);
    assert!(seen > 20, "the walk mounted dozens of placeholders: {seen}");
    // None of them was ever measured: the export keeps a measured height per item, null for none.
    let measured = harness::eval(&tab, "(function(){ var items = window.__viewportHistory.export().session.items; return Object.keys(window.__seen).filter(function (i) { return items[i] && items[i][1] != null; }).length; })()");
    assert_eq!(
        measured.as_i64(),
        Some(0),
        "a placeholder is never measured"
    );
    let after = harness::eval(&tab, MEANS);
    assert_ne!(
        before.as_str(),
        Some("null"),
        "the history records the estimates"
    );
    assert_eq!(before, after, "mounting placeholders moved no mean");
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// The committed tail can start in the middle of a long turn: its first steps then join the head's
/// group of steps, keyed by that group's first record, in the head. The group reaches into the tail,
/// so it is drawn with what has arrived — its count saying how many earlier steps are still loading —
/// never as a placeholder (stage 1 folded those steps into the short card); a reader at the tail stays
/// there, following, and once the head is here the group is whole. (A Claude session's open turn is
/// the provisional zone, which starts at its prompt, so a tail with no prompt at all cannot be written
/// as a Claude transcript; the engine's rename of the group's key across the landing is pinned in the
/// node contract.)
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_group_of_steps_reaching_into_the_tail_stays_drawn_while_it_fills() {
    let _serial = serial();
    let mut jsonl = session(20);
    jsonl += &harness::user_at("the long one", &harness::at("50:00"));
    for k in 0..320 {
        jsonl += &harness::edit_tool_at(
            &format!("e{k}"),
            &format!("/r/skua/src/module_{k:03}.rs"),
            &harness::at("50:01"),
        );
        jsonl += &harness::tool_result_at(&format!("e{k}"), &harness::at("50:01"));
    }
    jsonl += &harness::assistant_at("done with the long one", &harness::at("50:02"));
    jsonl += &harness::user_at("and the last one", &harness::at("51:00"));
    jsonl += &harness::assistant_at("noted", &harness::at("51:01"));
    let (_m, _b, tab, _) = open(
        2779,
        "by-turn-live",
        &jsonl,
        false,
        false,
        "&tailBudget=16384",
    );
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        harness::at_tail(&tab, Surface::AppShell),
        "the reader is at the tail"
    );
    const GROUP: &str = r#"(function(){
      var gs = document.querySelectorAll('.virtual-window [data-process-surface]'), g = gs[gs.length - 1];
      var u = g && g.closest('[data-unit-key]');
      return JSON.stringify({ found: !!g, placeholder: !!(u && u.classList.contains('unit-placeholder')), note: (g ? g.textContent.match(/[0-9]+ earlier still loading/) || [''] : [''])[0] });
    })()"#;
    let group: serde_json::Value =
        serde_json::from_str(harness::eval(&tab, GROUP).as_str().unwrap()).unwrap();
    assert_eq!(
        group["found"], true,
        "the long turn's group of steps is drawn above the last turn: {group}"
    );
    assert_eq!(
        group["placeholder"], false,
        "…not as a placeholder: {group}"
    );
    assert!(
        group["note"]
            .as_str()
            .unwrap_or("")
            .ends_with("earlier still loading"),
        "…and says its earlier steps are on their way: {group}"
    );
    release_until(&tab, WHOLE, "the whole head to arrive");
    std::thread::sleep(Duration::from_millis(400));
    let group: serde_json::Value =
        serde_json::from_str(harness::eval(&tab, GROUP).as_str().unwrap()).unwrap();
    assert_eq!(
        group["note"], "",
        "whole, the group says nothing is loading: {group}"
    );
    assert!(
        harness::at_tail(&tab, Surface::AppShell),
        "still at the tail"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// Release the head's reads one at a time until the page stops asking: no read waits, twice, 800 ms
/// apart.
fn release_until_quiet(tab: &headless_chrome::Tab, what: &str) {
    let started = std::time::Instant::now();
    let mut quiet = 0;
    while quiet < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{what}: {}",
            harness::eval(tab, STATE)
        );
        if harness::eval(tab, "window.__heldReads.length").as_i64() == Some(0) {
            quiet += 1;
            std::thread::sleep(Duration::from_millis(800));
        } else {
            quiet = 0;
            harness::eval(tab, "window.__release(1)");
            std::thread::sleep(Duration::from_millis(120));
        }
    }
}

/// The head's turns from the index the monitor serves, each with whether the Turns pane lists it as
/// here (`held`) and its bytes in the head: `{turns: [[number, bytes, held]…], last}`, `last` the head's
/// last turn.
const HEAD_TURNS: &str = r#"(async function(){
  var r = await fetch('/turn-index?session=SID&epoch=1&upto=1000000'), j = await r.json(), tail = window.__tailFrom;
  var turns = (j.turns || []).filter(function (t) { return t.o < tail; }).map(function (t) {
    var row = document.querySelector('#navigatorTurns .outline-turn-row[data-turn-number="' + t.turn + '"]');
    return [Number(t.turn), Math.min(t.end, tail) - t.o, !!row && !row.classList.contains('outline-turn-pending')];
  });
  return JSON.stringify({ turns: turns, last: turns.length ? turns[turns.length - 1][0] : null });
})()"#;

/// The bytes of the head the page holds, by the Turns pane, and the head's last turn.
fn head_held(tab: &headless_chrome::Tab) -> (i64, i64, serde_json::Value) {
    let v: serde_json::Value = serde_json::from_str(
        harness::eval(tab, &HEAD_TURNS.replace("SID", SID))
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let held = v["turns"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t[2].as_bool() == Some(true))
        .map(|t| t[1].as_i64().unwrap())
        .sum();
    (held, v["last"].as_i64().unwrap(), v)
}

/// #s285 (design/slow-links.md §6.6; Hong on #s290: "Read up to about 16 MB, then only what I
/// visit"). With what the page keeps set small (`?keepBytes=`), a reader at the tail has the head read
/// behind them only until the turns read hold that much: there the background read stops, the earliest
/// turns still on their way. A tap on turn 1 reads it and the turns around it, and the turns read first,
/// now the farthest from the reader, are let go: back to placeholders, listed as coming again, so what
/// the page holds stays near its budget. The reader's turn does not move, nothing reads the turns let go
/// back while the reader stays, and a tap on one of them reads it again, as a fill.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_reader_across_a_network_keeps_about_its_budget_of_a_long_session() {
    const KEEP: i64 = 24 * 1024;
    // What a held count may run past the budget by once the reader visits: the turns the window holds
    // that the page could not let go of.
    const SLACK: i64 = 4096 + 4096;
    let _serial = serial();
    // The last turn is a long run of steps the tail starts in the middle of: the head's part of it is
    // the live turn's, never let go.
    let mut jsonl = session(120);
    jsonl += &harness::user_at("the long one", &harness::at("50:00"));
    for k in 0..30 {
        jsonl += &harness::edit_tool_at(
            &format!("e{k}"),
            &format!("/r/skua/src/module_{k:03}.rs"),
            &harness::at("50:01"),
        );
        jsonl += &harness::tool_result_at(&format!("e{k}"), &harness::at("50:01"));
    }
    let (_m, _b, tab, _) = open(
        2782,
        "by-turn-keep",
        &jsonl,
        false,
        false,
        &format!("&keepBytes={KEEP}"),
    );
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    release_until_quiet(&tab, "the background read to stop");
    let (held, last, turns) = head_held(&tab);
    let head: i64 = turns["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t[1].as_i64().unwrap())
        .sum();
    assert!(
        head > 4 * KEEP,
        "the head is several budgets long: {head} bytes"
    );
    assert!(
        (KEEP - 2048..=KEEP).contains(&held),
        "the background read stops before the turn that would pass the budget: {held} of {KEEP}, {turns}"
    );
    let gaps = turns["turns"]
        .as_array()
        .unwrap()
        .windows(2)
        .filter(|w| w[0][2].as_bool() == Some(true) && w[1][2].as_bool() == Some(false))
        .count();
    assert_eq!(
        gaps, 0,
        "…and what it read is one run behind the tail, with no turn let go: {turns}"
    );
    assert!(
        state(&tab)["pending"].as_i64().unwrap() > 0,
        "the earliest turns are still on their way"
    );
    assert!(
        harness::at_tail(&tab, Surface::AppShell),
        "the reader is still at the tail"
    );

    // A visit to the session's first turn reads it, and the turns read first are let go.
    tab.find_element("#navigatorTurns .outline-turn-pending[data-turn-number=\"1\"]")
        .expect("turn 1 is listed while still coming")
        .click()
        .expect("click it");
    release_until_quiet(&tab, "turn 1 and its neighbours to be read");
    harness::until(
        &tab,
        "!!document.querySelector('.virtual-window > [data-unit-key=\"user:t1\"]:not(.unit-placeholder)')",
        "turn 1 filled",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(400));
    let (held, _, turns) = head_held(&tab);
    assert!(
        held <= KEEP + SLACK,
        "what the page holds stays near its budget: {held} of {KEEP}, {turns}"
    );
    let dropped = last - 1;
    let gone = format!(
        "!!document.querySelector('#navigatorTurns .outline-turn-pending[data-turn-number=\"{dropped}\"]')"
    );
    assert_eq!(
        harness::eval(&tab, &gone).as_bool(),
        Some(true),
        "turn {dropped}, read first and now the farthest, is let go: {turns}"
    );
    assert_eq!(
        harness::eval(
            &tab,
            &format!("(function(){{ var row = document.querySelector('#navigatorTurns .outline-turn-row[data-turn-number=\"{last}\"]'); return !!row && !row.classList.contains('outline-turn-pending'); }})()")
        )
        .as_bool(),
        Some(true),
        "…but never the head's last turn, which the tail goes on with"
    );
    // Nothing reads them back while the reader stays.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        harness::eval(&tab, "window.__heldReads.length").as_i64(),
        Some(0),
        "no read asked for while the reader stays: {}",
        harness::eval(&tab, "JSON.stringify(window.__readLog)")
    );

    // A reader who passes through turn 80 and goes on to turn 35 before its neighbours arrive has
    // them read for nobody: what the window asked for there lapses. Only turn 80, already in flight
    // when they moved on, is read. (A window of placeholders reaches about 20 turns either way, so
    // turn 35's own asks stop short of 62.)
    let tap = |n: i64| {
        tab.find_element(&format!(
            "#navigatorTurns .outline-turn-pending[data-turn-number=\"{n}\"]"
        ))
        .unwrap_or_else(|_| panic!("turn {n} is listed while still coming"))
        .click()
        .expect("click it");
    };
    tap(80);
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "turn 80's read asked for",
        Duration::from_secs(10),
        STATE,
    );
    std::thread::sleep(Duration::from_millis(300));
    tap(35);
    std::thread::sleep(Duration::from_millis(300));
    release_until(
        &tab,
        "!!document.querySelector('.virtual-window > [data-unit-key=\"user:t35\"]:not(.unit-placeholder)')",
        "turn 35's prompt filled",
    );
    std::thread::sleep(Duration::from_millis(400));
    // The turns read around turn 35 take the page past its budget, and the turns let go for them
    // include turns ABOVE the reader: their units keep their heights, so the reader does not move.
    let top = top_of(&tab, "user:t35").expect("turn 35 mounted");
    let (_, _, before) = head_held(&tab);
    release_until_quiet(&tab, "turn 35 and its neighbours to be read");
    let (_, _, turns) = head_held(&tab);
    let was_held = |v: &serde_json::Value, n: i64| {
        v["turns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t[0].as_i64() == Some(n) && t[2].as_bool() == Some(true))
    };
    let above: Vec<i64> = (1..35)
        .filter(|&n| was_held(&before, n) && !was_held(&turns, n))
        .collect();
    assert!(
        !above.is_empty(),
        "turns above the reader are let go: {before} → {turns}"
    );
    let now = top_of(&tab, "user:t35").expect("turn 35 still mounted");
    assert!(
        (now - top).abs() <= 1.0,
        "letting turns {above:?} go above the reader moves nothing: {top} → {now}"
    );
    let near80: Vec<i64> = turns["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t[0].as_i64().unwrap(), t[2].as_bool() == Some(true)))
        .filter(|&(n, held)| (62..=98).contains(&n) && n != 80 && held)
        .map(|(n, _)| n)
        .collect();
    assert!(
        near80.is_empty(),
        "no turn near 80 is read for a reader who moved on, but {near80:?}: {turns}"
    );
    harness::until(
        &tab,
        "!!document.querySelector('.virtual-window > [data-unit-key=\"user:t35\"]:not(.unit-placeholder)')",
        "turn 35 filled",
        Duration::from_secs(10),
        STATE,
    );

    // A visit to a turn let go reads it again.
    let offset = turn_offset(&tab, dropped);
    tab.find_element(&format!(
        "#navigatorTurns .outline-turn-pending[data-turn-number=\"{dropped}\"]"
    ))
    .expect("the turn let go is listed as coming")
    .click()
    .expect("click it");
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "a read asked for",
        Duration::from_secs(10),
        STATE,
    );
    assert_eq!(
        harness::eval(&tab, "window.__heldReads[0].from").as_i64(),
        Some(offset),
        "the turn let go is read first: {}",
        harness::eval(&tab, "JSON.stringify(window.__readLog)")
    );
    release_until_quiet(&tab, "the turn let go to be read again");
    harness::until(
        &tab,
        &format!("!!document.querySelector('.virtual-window > [data-unit-key=\"user:t{dropped}\"]:not(.unit-placeholder)')"),
        "the turn let go filled again",
        Duration::from_secs(10),
        STATE,
    );
    let (held, _, turns) = head_held(&tab);
    assert!(
        held <= KEEP + SLACK,
        "and the page still holds about its budget: {held} of {KEEP}, {turns}"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// #s285: a task whose status was recorded in a turn the page never read (the background read stops
/// at what the page keeps, far below it) is still found: the task's "Go to the turn" asks the server
/// where it was last recorded, reads that turn and lands there. Before, it said "still loading" for as
/// long as the head was out, which over the budget is for good.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_task_recorded_in_a_turn_never_read_is_found_on_the_server() {
    const SUBJECT: &str = "settle the drawer offsets";
    let _serial = serial();
    // Turn 3 records the task's status.
    let mut lines: Vec<String> = session(120).lines().map(|l| format!("{l}\n")).collect();
    let at = lines
        .iter()
        .position(|l| l.contains("question 2:"))
        .expect("turn 3's prompt");
    let ts = lines[at]
        .split("\"timestamp\":\"")
        .nth(1)
        .and_then(|t| t.split('"').next())
        .expect("its timestamp")
        .to_string();
    lines.insert(
        at + 1,
        harness::named_tool_at("tu3", "TaskUpdate", SUBJECT, &ts),
    );
    let (_m, _b, tab, _) = open_with_tasks(
        2783,
        "by-turn-task",
        &lines.concat(),
        false,
        false,
        "&keepBytes=24576",
        &[("1", SUBJECT, "in_progress")],
    );
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    release_until_quiet(&tab, "the background read to stop");
    assert_eq!(
        harness::eval(&tab, "!!document.querySelector('#navigatorTurns .outline-turn-pending[data-turn-number=\"3\"]')").as_bool(),
        Some(true),
        "turn 3 is never read: {}",
        harness::eval(&tab, STATE)
    );
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'");
    harness::show_every_pane_row(&tab);
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorWork .work-task').length === 1",
        "the task listed",
        Duration::from_secs(20),
        "document.getElementById('navigatorWork').innerText.slice(0, 200)",
    );
    harness::eval(
        &tab,
        "document.querySelector('#navigatorWork .work-task-head').click(); 'ok'",
    );
    harness::until(
        &tab,
        "!document.getElementById('taskPopover').hidden",
        "the task's details",
        Duration::from_secs(5),
        "document.getElementById('taskPopover').innerHTML.slice(0, 300)",
    );
    assert_eq!(
        harness::eval(&tab, "(function(){ var b = document.querySelector('#taskPopover .task-popover-jump'); return !!b && !b.disabled && b.hasAttribute('data-task-find'); })()").as_bool(),
        Some(true),
        "the jump is offered, to be found on the server: {}",
        harness::eval(&tab, "document.getElementById('taskPopover').innerText")
    );
    let o3 = turn_offset(&tab, 3);
    harness::eval(
        &tab,
        "document.querySelector('#taskPopover [data-task-find]').click(); 'ok'",
    );
    harness::until(
        &tab,
        "window.__heldReads.length >= 1",
        "turn 3's read asked for",
        Duration::from_secs(10),
        STATE,
    );
    assert_eq!(
        harness::eval(&tab, "window.__heldReads[0].from").as_i64(),
        Some(o3),
        "the turn that recorded the task is read: {}",
        harness::eval(&tab, "JSON.stringify(window.__readLog)")
    );
    release_until_quiet(&tab, "turn 3 to be read");
    harness::until(
        &tab,
        "((document.querySelector('#navigatorTurns .outline-turn-row.current') || {}).dataset || {}).turnNumber === '3'",
        "the reader on turn 3",
        Duration::from_secs(10),
        STATE,
    );
    let shown = harness::eval(&tab, &format!("(function(){{ var s = document.querySelector('.transcript').getBoundingClientRect(); return [].some.call(document.querySelectorAll('.virtual-window *'), function (e) {{ if (e.children.length || e.textContent.indexOf('{SUBJECT}') < 0) return false; var r = e.getBoundingClientRect(); return r.bottom > s.top && r.top < s.bottom; }}); }})()"));
    assert_eq!(
        shown.as_bool(),
        Some(true),
        "the record that names the task is on the screen"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// Type a query into the box and press Enter, as a reader does where search runs on Enter (#s278:
/// while a records-by-turn head is out), then wait until the count is an answer, not "Searching…".
fn search_on_enter(tab: &headless_chrome::Tab, query: &str) -> String {
    type_and_enter(tab, query);
    harness::until(
        tab,
        "/^\\d+ (hit|match)/.test(document.getElementById('transcriptSearchCount').textContent)",
        "the count to be an answer",
        Duration::from_secs(20),
        "document.getElementById('transcriptSearchCount').textContent",
    );
    harness::eval(
        tab,
        "document.getElementById('transcriptSearchCount').textContent",
    )
    .as_str()
    .unwrap_or("")
    .to_string()
}

fn type_and_enter(tab: &headless_chrome::Tab, query: &str) {
    harness::eval(
        tab,
        &format!("(function(){{ var i = document.getElementById('transcriptSearchInput'); i.focus(); i.value = {query:?}; i.dispatchEvent(new Event('input', {{ bubbles: true }})); i.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', bubbles: true }})); return 1; }})()"),
    );
}

fn count_text(tab: &headless_chrome::Tab) -> String {
    harness::eval(
        tab,
        "document.getElementById('transcriptSearchCount').textContent",
    )
    .as_str()
    .unwrap_or("")
    .to_string()
}

/// #s278 (`design/slow-links.md` §7.3): with the head of a records-by-turn open still out, a search
/// runs on Enter and counts the WHOLE session — the server answers for the records the page has not
/// fetched — and the count is the one the page gives once every record is here.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_search_while_the_head_is_out_counts_the_whole_session() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2785, "by-turn-search", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    harness::eval(&tab, "(function(){ var i = document.getElementById('transcriptSearchInput'); i.focus(); i.value = 'lorem'; i.dispatchEvent(new Event('input', { bubbles: true })); return 1; })()");
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        count_text(&tab).contains('⏎'),
        "while the head is out a search waits for Enter: {}",
        count_text(&tab)
    );
    let held = search_on_enter(&tab, "lorem");
    assert!(
        harness::eval(
            &tab,
            "document.querySelectorAll('#navigatorTurns .outline-turn-pending').length"
        )
        .as_i64()
        .unwrap_or(0)
            > 40,
        "most turns were still on their way when it counted"
    );
    release_until(&tab, WHOLE, "the whole head to arrive");
    std::thread::sleep(Duration::from_millis(600));
    let whole = count_text(&tab);
    let n = |t: &str| t.split_whitespace().next().unwrap_or("").to_string();
    assert_eq!(
        n(&held),
        n(&whole),
        "the count with the head out is the whole session's: {held} vs {whole}"
    );
    assert!(
        n(&held).parse::<u32>().unwrap_or(0) >= 60,
        "every turn's prompt: {held}"
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// #s278: a step onto a hit whose turn is still on its way reads that turn and lands on the hit's
/// own mark once it is here.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn a_step_to_a_hit_still_on_its_way_lands_on_its_mark() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2786, "by-turn-step", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    let count = search_on_enter(&tab, "question 5:");
    assert!(count.starts_with("1 hit"), "one turn's prompt: {count}");
    release_until(
        &tab,
        "(function(){ var m = document.querySelector('mark.search-mark.current'); if (!m) return false; var s = document.querySelector('.transcript').getBoundingClientRect(), r = m.getBoundingClientRect(); return r.top >= s.top && r.bottom <= s.bottom; })()",
        "the step to land on the hit's mark once its turn is here",
    );
    let mark = harness::eval(
        &tab,
        "(document.querySelector('mark.search-mark.current') || {}).textContent || ''",
    );
    assert_eq!(
        mark.as_str().map(str::to_lowercase).as_deref(),
        Some("question 5:")
    );
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// #s278: the filter's matches (`o(B):`, Bash calls) count the records still on their way, as the
/// server finds them, and equal the count once every record is here.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_filter_counts_records_still_on_their_way() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2787, "by-turn-filter", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    // While the server's answer is on its way the count says so, never the tail's part of it.
    harness::eval(&tab, "window.__holdSearch = true");
    type_and_enter(&tab, "o(B):");
    harness::until(
        &tab,
        "window.__heldSearch.length > 0",
        "the filter to ask the server",
        Duration::from_secs(10),
        "document.getElementById('transcriptSearchCount').textContent",
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        count_text(&tab),
        "Searching…",
        "no partial count while the answer is out"
    );
    assert_eq!(
        harness::eval(&tab, "window.__heldSearch.length").as_i64(),
        Some(1),
        "one ask for one filter"
    );
    harness::eval(&tab, "window.__releaseSearch()");
    harness::until(
        &tab,
        "/^\\d+ (hit|match)/.test(document.getElementById('transcriptSearchCount').textContent)",
        "the count to be an answer",
        Duration::from_secs(20),
        "document.getElementById('transcriptSearchCount').textContent",
    );
    let held = count_text(&tab);
    release_until(&tab, WHOLE, "the whole head to arrive");
    std::thread::sleep(Duration::from_millis(600));
    let whole = count_text(&tab);
    let n = |t: &str| t.split_whitespace().next().unwrap_or("").to_string();
    assert_eq!(
        n(&held),
        n(&whole),
        "the filter's count with the head out is the whole session's: {held} vs {whole}"
    );
    assert!(n(&held).parse::<u32>().unwrap_or(0) > 0, "{held}");
    assert_eq!(
        harness::eval(&tab, VIOLATIONS).as_str(),
        Some("[]"),
        "no invariant broken"
    );
}

/// A search a control starts submits the box as it stands (#s278): text typed but not entered, then
/// whole words pressed, is searched — and its count lands with the server's answer, never left at
/// "Searching…" by the typed text it submitted.
#[test]
#[ignore]
fn a_search_a_control_starts_counts_once_its_answer_lands() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2788, "by-turn-control", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    harness::eval(&tab, "(function(){ var i = document.getElementById('transcriptSearchInput'); i.focus(); i.value = 'lorem'; i.dispatchEvent(new Event('input', { bubbles: true })); return 1; })()");
    std::thread::sleep(Duration::from_millis(400));
    assert!(count_text(&tab).contains('⏎'), "{}", count_text(&tab));
    harness::eval(&tab, "window.__holdSearch = true");
    harness::eval(
        &tab,
        "document.getElementById('transcriptWholeWords').click()",
    );
    harness::until(
        &tab,
        "window.__heldSearch.length > 0",
        "whole words to ask the server",
        Duration::from_secs(10),
        "document.getElementById('transcriptSearchCount').textContent",
    );
    assert_eq!(count_text(&tab), "Searching…");
    harness::eval(&tab, "window.__releaseSearch()");
    harness::until(
        &tab,
        "/^\\d+ (hit|match)/.test(document.getElementById('transcriptSearchCount').textContent)",
        "the count to land with the answer",
        Duration::from_secs(20),
        "document.getElementById('transcriptSearchCount').textContent",
    );
    let held = count_text(&tab);
    release_until(&tab, WHOLE, "the whole head to arrive");
    std::thread::sleep(Duration::from_millis(600));
    let n = |t: &str| t.split_whitespace().next().unwrap_or("").to_string();
    assert_eq!(
        n(&held),
        n(&count_text(&tab)),
        "the count the control's search landed is the whole session's"
    );
    assert!(n(&held).parse::<u32>().unwrap_or(0) >= 60, "{held}");
}

/// The step arrows enter text typed and not yet entered, as Enter does (#s278): ↓ over typed text
/// counts the whole session and lands on a hit, rather than stepping the last query's hits.
#[test]
#[ignore]
fn an_arrow_over_typed_text_enters_it_first() {
    let _serial = serial();
    let (_m, _b, tab, _) = open(2789, "by-turn-arrow", &session(60), false, false, "");
    harness::until(
        &tab,
        LAID_OUT,
        "the placeholders laid out",
        Duration::from_secs(10),
        STATE,
    );
    harness::eval(&tab, "(function(){ var i = document.getElementById('transcriptSearchInput'); i.focus(); i.value = 'lorem'; i.dispatchEvent(new Event('input', { bubbles: true })); return 1; })()");
    std::thread::sleep(Duration::from_millis(400));
    assert!(count_text(&tab).contains('⏎'), "{}", count_text(&tab));
    harness::eval(&tab, "document.getElementById('findNext').click()");
    harness::until(
        &tab,
        "/^\\d+ (hit|match)/.test(document.getElementById('transcriptSearchCount').textContent) && !!document.querySelector('mark.search-mark.current')",
        "the arrow to enter the text, count it and land on a hit",
        Duration::from_secs(20),
        "document.getElementById('transcriptSearchCount').textContent",
    );
    let n = count_text(&tab)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .parse::<u32>()
        .unwrap_or(0);
    assert!(n >= 60, "the whole session's hits: {}", count_text(&tab));
}
