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
//! decides when each piece arrives. Ports 2770–2779.

mod harness;

use harness::{serial, Surface};
use std::time::Duration;

const SID: &str = "5a5a5a5a-0000-4000-8000-000000002770";

/// Queue the head's reads: every `/records` read below the tail's own offset (the first one the page
/// makes) waits until `window.__release(n)`; `window.__answer409 = n` answers the next `n` released
/// reads with a stale epoch. With `hold_index`, the turn index waits too, until
/// `window.__releaseIndex()`. `window.__readLog` lists every head read's offset, in order.
fn queue_the_head(tab: &headless_chrome::Tab, hold_index: bool) {
    use headless_chrome::protocol::cdp::Page;
    let source = r#"(function(){
      var real = window.fetch;
      window.__tailFrom = null; window.__heldReads = []; window.__readLog = []; window.__answer409 = 0;
      window.__holdIndex = HOLD; window.__heldIndex = []; window.__tailPulls = 0;
      window.__release = function (n) { var k = n == null ? Infinity : n; while (k-- > 0 && window.__heldReads.length) window.__heldReads.shift().go(); return window.__heldReads.length; };
      window.__releaseIndex = function () { window.__holdIndex = false; while (window.__heldIndex.length) window.__heldIndex.shift()(); };
      window.fetch = function (input, init) {
        var url = String(input && input.url || input), self = this;
        if (/\/pull\?/.test(url) && /[?&]tail=/.test(url)) window.__tailPulls++;
        if (/\/turn-index\?/.test(url) && window.__holdIndex) return new Promise(function (res) { window.__heldIndex.push(function () { res(real.call(self, input, init)); }); });
        if (/\/records\?/.test(url)) {
          var from = Number(new URL(url, location.href).searchParams.get('from'));
          if (window.__tailFrom == null) { window.__tailFrom = from; return real.call(self, input, init); }
          if (from < window.__tailFrom) {
            window.__readLog.push(from);
            return new Promise(function (res) { window.__heldReads.push({ from: from, go: function () {
              if (window.__answer409 > 0) { window.__answer409--; res(new Response('stale epoch', { status: 409 })); }
              else res(real.call(self, input, init));
            } }); });
          }
        }
        return real.call(this, input, init);
      };
    })();"#
        .replace("HOLD", if hold_index { "true" } else { "false" });
    tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
        source,
        world_name: None,
        include_command_line_api: None,
        run_immediately: None,
    })
    .expect("the head's reads are queued");
}

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
    let base = harness::base(case);
    let stores = harness::Stores::new(&base);
    let path = stores.claude_session(SID, jsonl);
    let m = harness::Monitor::spawn(harness::Kind::V1, port, &base, Some(&stores), true);
    let (browser, tab) = harness::chrome_tab();
    if phone {
        harness::phone(&tab, 390, 844);
    }
    m.pair(&tab);
    queue_the_head(&tab, hold_index);
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
