//! **The browser-level viewport harness** — the live page's follow/anchor contract, in a
//! REAL engine. The behaviors under test are exactly the ones a DOM stub cannot fake:
//! scroll events the renderer fires, `scrollY` clamping at layout, and the browser's
//! native scroll anchoring — the machinery `export.js`'s follow classifier and viewport
//! anchor (#88/#89/#103) are written against, and the machinery every past scroll
//! regression lived in.
//!
//! The contract pinned here:
//! 1. a fresh live page is PINNED and follows appended turns down;
//! 2. a user scroll away from the bottom UNPINS (the "jump to bottom" pill appears);
//! 3. an unpinned viewport is STABLE: applies — plain appends and provisional tail
//!    reshapes (an open tool call whose result then lands) — must not move `scrollY`;
//! 4. arrivals while unpinned surface in the pill as a count;
//! 5. jump-to-bottom still LANDS after the viewport is resized — the case a resize
//!    breaks, because every height measured at the old width becomes a guess;
//! 6. a turn landing HOLDS through a late reflow — the case images breaking, since they
//!    resize the page after the jump has already finished;
//! 7. stepping through search hits moves the SELECTION, not the page, while the target
//!    is already on screen.
//!
//! `#[ignore]`d like the tmux e2e: it needs a Chrome/Chromium on the machine. Run with
//! `cargo test -p claude-replay-html --test browser_follow -- --ignored`.

use claude_replay_html::start_server;
use claude_replay_present::Args;
use std::time::{Duration, Instant};

mod harness;
use harness::{
    append, assistant, base, serial, thinking, tool_open, tool_result, user, Kind, Monitor, Stores,
};

/// Evaluate `expr` (may be an async IIFE when `await_promise`) and return its value.
/// Everything is passed through `JSON.stringify` on the page side, so only string
/// primitives cross CDP — no RemoteObject preview shape to depend on.
fn eval(tab: &headless_chrome::Tab, expr: &str, await_promise: bool) -> serde_json::Value {
    // For a promise the stringify must ride the chain — stringifying the promise itself
    // yields "{}" before it resolves.
    let wrapped = if await_promise {
        format!("({expr}).then(function (v) {{ return JSON.stringify(v); }})")
    } else {
        format!("JSON.stringify(({expr}))")
    };
    let ro = tab
        .evaluate(&wrapped, await_promise)
        .unwrap_or_else(|e| panic!("evaluate failed: {e}\nexpr: {expr}"));
    let s = ro
        .value
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| panic!("expression produced no string value: {expr}"));
    serde_json::from_str(&s).unwrap()
}

/// One snapshot of everything the assertions consume.
fn view_state(tab: &headless_chrome::Tab) -> serde_json::Value {
    eval(
        tab,
        r#"{
            y: Math.round(window.scrollY),
            h: document.body.scrollHeight,
            gap: Math.round(document.body.scrollHeight - window.innerHeight - window.scrollY),
            following: document.body.classList.contains("following"),
            badge: (document.getElementById("newbadge") || {}).textContent || "",
            badgeOn: /\bon\b/.test((document.getElementById("newbadge") || {className:""}).className),
            blocks: document.querySelectorAll('#stream [data-idx]').length
        }"#,
        false,
    )
}

/// Wait until `pred` holds on the sampled state, or panic with the last state.
fn wait_for(
    tab: &headless_chrome::Tab,
    what: &str,
    timeout: Duration,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let t0 = Instant::now();
    let mut last = serde_json::Value::Null;
    while t0.elapsed() < timeout {
        last = view_state(tab);
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for {what}; last state: {last}");
}

/// [`wait_for`] against a caller-supplied probe. `view_state` carries the viewport contract
/// and should stay that; state that belongs to one test (the artifact overlay's) rides here.
fn wait_probe(
    tab: &headless_chrome::Tab,
    what: &str,
    timeout: Duration,
    expr: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let t0 = Instant::now();
    let mut last = serde_json::Value::Null;
    while t0.elapsed() < timeout {
        last = eval(tab, expr, false);
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for {what}; last state: {last}");
}

/// The user's gesture, as the page classifies one: wheel events mark intent, and the
/// renderer fires the real scroll events for the movement (headless "new" renders, so
/// no synthetic scroll dispatch is needed — that is the point of this harness).
fn user_scroll_by(tab: &headless_chrome::Tab, dy: i64) {
    eval(
        tab,
        &format!(
            r#"(function () {{
                window.dispatchEvent(new WheelEvent("wheel", {{deltaY: {dy}}}));
                window.scrollBy(0, {dy});
                return true;
            }})()"#
        ),
        false,
    );
}

#[test]
#[ignore] // needs a local Chrome/Chromium; see the module docs
fn live_viewport_follows_pinned_and_holds_unpinned() {
    let _serial = serial();
    let base = base("follow");
    // The run's cache home — never the developer's real one (the suite-wide isolation rule).
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("live.jsonl");
    {
        let mut s = String::new();
        for i in 0..30u32 {
            s.push_str(&user(
                &format!(
                    "question {i}: {}",
                    "lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(3)
                ),
                i,
            ));
            s.push_str(&assistant(
                &format!(
                    "answer {i}: {}",
                    "sed do eiusmod tempor incididunt ut labore et dolore. ".repeat(5)
                ),
                i,
            ));
        }
        std::fs::write(&src, s).unwrap();
    }

    // `--no-cache`: the Transient provider — this run coordinates with nothing.
    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            // Never let tab-backgrounding heuristics starve timers or rendering — the
            // page's poll loop and the renderer's scroll events are the test subject.
            .args(vec![
                std::ffi::OsStr::new("--disable-background-timer-throttling"),
                std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
                std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            ])
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();

    // Phase 0 — the fresh live page renders and lands PINNED at the tail.
    // `blocks` counts MATERIALIZED elements — the virtualizer keeps only the window
    // around the viewport in the DOM, so "rendered" is a handful, not the whole session.
    let s0 = wait_for(
        &tab,
        "initial render, pinned at bottom",
        Duration::from_secs(15),
        |s| {
            s["blocks"].as_i64().unwrap_or(-1) > 5
                && s["following"] == true
                && s["gap"].as_i64().unwrap_or(9999) <= 80
        },
    );

    // Phase 1 (premise) — the renderer really fires scroll events here; the whole
    // classifier is dead without them (a hidden tab suppresses them, which is why this
    // harness exists instead of a background-tab drive).
    let fired = eval(
        &tab,
        r#"(async function () {
            var n = 0;
            window.addEventListener("scroll", function () { n++; }, {passive: true});
            window.scrollBy(0, -50);
            await new Promise(function (r) { setTimeout(r, 400); });
            window.scrollBy(0, 50);
            await new Promise(function (r) { setTimeout(r, 400); });
            return n;
        })()"#,
        true,
    );
    assert!(
        fired.as_i64().unwrap_or(0) > 0,
        "premise: headless rendering must fire scroll events (got {fired})"
    );

    // Phase 2 — pinned follows: an appended turn grows the page and the view rides down.
    let h0 = s0["h"].as_i64().unwrap();
    append(&src, &user("appended while pinned", 40));
    append(
        &src,
        &assistant(&"the reply rides the tail down. ".repeat(8), 40),
    );
    wait_for(
        &tab,
        "pinned view to follow the append",
        Duration::from_secs(10),
        |s| {
            s["h"].as_i64().unwrap_or(0) > h0
                && s["following"] == true
                && s["gap"].as_i64().unwrap_or(9999) <= 80
        },
    );

    // Phase 3 — a user scroll AWAY unpins and offers the way back.
    for _ in 0..6 {
        user_scroll_by(&tab, -60);
        std::thread::sleep(Duration::from_millis(80));
    }
    let s3 = wait_for(
        &tab,
        "unpin after user scroll away",
        Duration::from_secs(5),
        |s| s["following"] == false && s["gap"].as_i64().unwrap_or(0) > 80,
    );
    // The pill's visibility is its `on` class; `textContent` lingers from earlier paints.
    assert!(
        s3["badgeOn"] == true && s3["badge"].as_str().unwrap_or("") == "\u{2193} Jump to bottom",
        "away from the bottom with nothing new, the pill offers the jump (got {s3})"
    );
    let y_held = s3["y"].as_i64().unwrap();

    // Phase 4 — THE regression pin (owner, 2026-08-20): an unpinned near-bottom viewport
    // must hold absolutely still through applies. Both apply shapes:
    //   (a) plain committed appends;
    //   (b) provisional tail reshapes — an OPEN tool call rendered provisionally, whose
    //       result then lands and rewrites the tail (`reset` + re-render).
    let h3 = s3["h"].as_i64().unwrap();
    for k in 0..3u32 {
        append(&src, &tool_open(&format!("rp{k}"), 45 + k));
        std::thread::sleep(Duration::from_millis(2600)); // > POLL_MS: its own apply
        let mid = view_state(&tab);
        assert!(
            (mid["y"].as_i64().unwrap() - y_held).abs() <= 2 && mid["following"] == false,
            "unpinned viewport moved on the OPEN-tool apply {k}: held {y_held}, now {mid}"
        );
        append(&src, &tool_result(&format!("rp{k}"), 45 + k));
        append(
            &src,
            &assistant(
                &format!("after result {k}. {}", "steady prose. ".repeat(6)),
                45 + k,
            ),
        );
        std::thread::sleep(Duration::from_millis(2600));
        let s = view_state(&tab);
        assert!(
            (s["y"].as_i64().unwrap() - y_held).abs() <= 2 && s["following"] == false,
            "unpinned viewport moved on the RESHAPE apply {k}: held {y_held}, now {s}"
        );
    }
    let s4 = view_state(&tab);
    assert!(
        s4["h"].as_i64().unwrap() > h3,
        "the storm must actually have grown the page (h {h3} -> {s4})"
    );

    // Phase 4b — the GROWING OPEN TURN (the owner's 2026-08-20 report signature): a turn
    // still being written re-emits its provisional tail taller on every poll with NO new
    // records — `added == 0`, so the pill keeps saying "Jump to bottom" the whole time,
    // exactly as reported. The viewport must still hold.
    let y4 = s4["y"].as_i64().unwrap();
    for k in 0..4u32 {
        append(
            &src,
            &assistant(
                &format!(
                    "streamed continuation {k}. {}",
                    "growing tail prose. ".repeat(10)
                ),
                55,
            ),
        );
        std::thread::sleep(Duration::from_millis(2600));
        let s = view_state(&tab);
        assert!(
            (s["y"].as_i64().unwrap() - y4).abs() <= 2 && s["following"] == false,
            "unpinned viewport moved on the growing-turn apply {k}: held {y4}, now {s}"
        );
    }

    // Phase 4c — viewport INSIDE a tall open turn's provisional zone. The provisional
    // tail re-renders each poll from a clone of the committed emitter state, so its
    // `b{n}` anchors are POSITIONAL within the zone: when a reshape absorbs or coalesces
    // blocks there, an id captured before the apply can name a DIFFERENT block after it —
    // and an anchor restore against the wrong block walks the page. This is the geometry
    // of the 2026-08-20 report (watching a working agent's current turn near the bottom).
    // Build the turn tall enough to fill the viewport, live inside it, then land results
    // and new calls in it.
    append(&src, &user("the last question, whose answer is long", 56));
    append(
        &src,
        &thinking(
            &"a long deliberation that fills real screen height. ".repeat(30),
            56,
        ),
    );
    append(&src, &tool_open("in0", 56));
    append(&src, &tool_result("in0", 56));
    append(
        &src,
        &assistant(&"intermediate reasoning between the calls. ".repeat(12), 56),
    );
    append(&src, &tool_open("in1", 56));
    std::thread::sleep(Duration::from_millis(2600));
    // Pin to the tail, then step up a screenful — the viewport now sits wholly inside
    // the open turn.
    eval(
        &tab,
        "(function () { window.scrollTo({top: document.body.scrollHeight}); return true; })()",
        false,
    );
    std::thread::sleep(Duration::from_millis(600));
    for _ in 0..5 {
        user_scroll_by(&tab, -70);
        std::thread::sleep(Duration::from_millis(80));
    }
    let s5 = wait_for(
        &tab,
        "unpinned inside the open turn",
        Duration::from_secs(5),
        |s| s["following"] == false && s["gap"].as_i64().unwrap_or(0) > 80,
    );
    let y5 = s5["y"].as_i64().unwrap();
    // Instrument: how do the materialized ids churn per apply? (diagnostic print only)
    eval(&tab, "(function () { window.__ids = function () { return Array.from(document.getElementById('stream').children).map(function (e) { return e.id + ':' + Math.round(e.getBoundingClientRect().height); }); }; return true; })()", false);
    for k in 0..3u32 {
        // Each apply reshapes the OPEN turn: the running call's result lands (absorb),
        // prose grows, and a new call opens.
        append(&src, &tool_result(&format!("in{}", k + 1), 57));
        append(
            &src,
            &assistant(
                &format!("progress {k}. {}", "the turn keeps going. ".repeat(10)),
                57,
            ),
        );
        append(&src, &tool_open(&format!("in{}", k + 2), 57));
        let before_ids = eval(&tab, "window.__ids()", false);
        std::thread::sleep(Duration::from_millis(2600));
        let after_ids = eval(&tab, "window.__ids()", false);
        eprintln!("reshape {k}: before {before_ids}\n           after  {after_ids}");
        let s = view_state(&tab);
        assert!(
            (s["y"].as_i64().unwrap() - y5).abs() <= 2 && s["following"] == false,
            "viewport inside the open turn moved on reshape {k}: held {y5}, now {s}"
        );
    }

    // Phase 5 — what arrived while unpinned is offered as a count.
    let badge = s4["badge"].as_str().unwrap_or("").to_string();
    assert!(
        s4["badgeOn"] == true && badge.contains("new message"),
        "arrivals while unpinned surface in the pill (got {s4})"
    );

    drop(tab);
    drop(browser);
    let _ = std::fs::remove_dir_all(&base);
}

/// **Jump-to-bottom must land after a resize.** Reported from the session monitor, whose
/// rail opens and closes over an `<iframe>`: closing it widens the frame, so every height
/// the virtualizer measured at the old width is suddenly wrong. The page then keeps GROWING
/// as those blocks are re-measured on the way down, and a jump that corrected itself only
/// once landed thousands of pixels short — leaving the page "following" but nowhere near
/// the end, with the pill apparently doing nothing. (Measured on the reported session:
/// stranded 7,543 px from the bottom, and nothing retried once the size stopped changing.)
///
/// The fix is convergence rather than a single correction, and this pins it: after a real
/// window resize, from far up the session, one jump ends at the bottom.
#[test]
#[ignore] // needs a local Chrome/Chromium; see the module docs
fn jump_to_bottom_lands_after_a_viewport_resize() {
    let _serial = serial();
    let base = base("resize");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("resize.jsonl");
    {
        // Long enough that most of it is never materialized at once — that is what makes
        // the stale heights matter.
        let mut s = String::new();
        for i in 0..160u32 {
            s.push_str(&user(
                &format!(
                    "question {i}: {}",
                    "lorem ipsum dolor sit amet consectetur. ".repeat(4)
                ),
                i % 60,
            ));
            s.push_str(&assistant(
                &format!(
                    "answer {i}: {}",
                    "sed do eiusmod tempor incididunt ut labore. ".repeat(9)
                ),
                i % 60,
            ));
        }
        std::fs::write(&src, s).unwrap();
    }

    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .window_size(Some((1400, 900)))
            .args(vec![
                std::ffi::OsStr::new("--disable-background-timer-throttling"),
                std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
                std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            ])
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();

    // Land pinned at the tail, with the visible heights measured at THIS width.
    wait_for(
        &tab,
        "initial render, pinned at bottom",
        Duration::from_secs(20),
        |s| {
            s["blocks"].as_i64().unwrap_or(-1) > 5
                && s["following"] == true
                && s["gap"].as_i64().unwrap_or(9999) <= 80
        },
    );

    // The resize: every measured height was taken at the old width and is now a guess.
    tab.set_bounds(headless_chrome::types::Bounds::Normal {
        left: None,
        top: None,
        width: Some(1000.0),
        height: Some(900.0),
    })
    .expect("resize");
    std::thread::sleep(Duration::from_millis(800));

    // Walk far up under real user intent, so the page genuinely unpins.
    for _ in 0..12 {
        user_scroll_by(&tab, -4000);
        std::thread::sleep(Duration::from_millis(60));
    }
    let up = wait_for(
        &tab,
        "unpinned, far from the tail",
        Duration::from_secs(8),
        |s| s["gap"].as_i64().unwrap_or(0) > 5_000,
    );
    assert!(
        up["gap"].as_i64().unwrap() > 5_000,
        "the walk must end far from the bottom: {up}"
    );

    // One jump — the pill's action — has to reach the end and stay there.
    eval(
        &tab,
        "(function () { window.scrollTo({top: document.body.scrollHeight}); return true; })()",
        false,
    );
    let landed = wait_for(
        &tab,
        "jump-to-bottom to land",
        Duration::from_secs(10),
        |s| s["gap"].as_i64().unwrap_or(9999) <= 80,
    );
    assert!(
        landed["gap"].as_i64().unwrap() <= 80,
        "after a resize, the jump still lands at the bottom: {landed}"
    );

    drop(tab);
    drop(browser);
    let _ = std::fs::remove_dir_all(&base);
}

/// **A turn landing must survive late reflow.** Reported from the monitor: clicking a turn
/// in the sidebar landed several turns away, on a stretch full of images, and only a
/// SECOND click worked — because the first had finally measured the region.
///
/// The landing loop converges against the heights that exist the instant it runs. Images
/// decode afterwards, and a screenful of them moves the page by thousands of pixels, so a
/// landing that was correct when it finished is wrong a moment later. The fix holds the
/// target at its landing offset while the page settles; this pins that, by growing a block
/// ABOVE the target after the jump — exactly what an image finishing decode does.
///
/// The hold ticks on a 16 ms timer rather than `requestAnimationFrame`. That is the right choice
/// for a REAL hidden or occluded tab, where rAF stops; it is NOT because rAF is dead here. This
/// comment used to claim it was ("verified — an rAF version recorded zero ticks"), and #140 step 4
/// measured otherwise: 38 rAF ticks in 600 ms, `visibilityState: "visible"`. `chrome()` passes
/// `--disable-renderer-backgrounding` and friends (harness/mod.rs:744-746), so the tab is not
/// backgrounded at all. Left as a timer on purpose, but do not port anything on the old premise.
#[test]
#[ignore] // needs a local Chrome/Chromium; see the module docs
fn a_turn_landing_holds_through_late_reflow() {
    let _serial = serial();
    let base = base("landing");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("landing.jsonl");
    {
        let mut s = String::new();
        for i in 0..80u32 {
            s.push_str(&user(&format!("question {i}"), i % 60));
            s.push_str(&assistant(
                &format!(
                    "answer {i}: {}",
                    "sed do eiusmod tempor incididunt ut labore. ".repeat(8)
                ),
                i % 60,
            ));
        }
        std::fs::write(&src, s).unwrap();
    }

    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .args(vec![
                std::ffi::OsStr::new("--disable-background-timer-throttling"),
                std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
                std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            ])
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    wait_for(&tab, "initial render", Duration::from_secs(15), |s| {
        s["blocks"].as_i64().unwrap_or(-1) > 5
    });

    // Land on a turn well up the session, the way the sidebar does.
    let landed = eval(
        &tab,
        r##"(function () {
            var items = Array.prototype.slice.call(document.querySelectorAll("#turnlist .side-item"));
            var it = items[Math.floor(items.length / 2)];
            it.click();
            var id = it.dataset.t;
            var t = document.getElementById(id);
            return {id: id, delta: t ? Math.round(t.getBoundingClientRect().top - 120) : null};
        })()"##,
        false,
    );
    let id = landed["id"].as_str().expect("a turn id").to_string();
    assert!(
        landed["delta"].as_i64().map(i64::abs).unwrap_or(9999) <= 2,
        "the click lands on the turn to begin with: {landed}"
    );

    // Now the late reflow: a block ABOVE the target grows, as a decoded image would.
    let held = eval(
        &tab,
        &format!(
            r##"(async function () {{
                var above = Array.prototype.slice.call(document.querySelectorAll("#vwin > *"))
                    .filter(function (e) {{ return e.getBoundingClientRect().top < 0; }}).pop();
                if (!above) return {{grew: false}};
                above.style.paddingTop = "2500px";
                await new Promise(function (r) {{ setTimeout(r, 700); }});
                var t = document.getElementById("{id}");
                var d = t ? Math.round(t.getBoundingClientRect().top - 120) : null;
                above.style.paddingTop = "";
                return {{grew: true, delta: d}};
            }})()"##
        ),
        true,
    );
    assert_eq!(held["grew"], true, "the fixture must actually reflow");
    assert!(
        held["delta"].as_i64().map(i64::abs).unwrap_or(9999) <= 2,
        "the target is still at its landing offset after the reflow: {held}"
    );

    drop(tab);
    drop(browser);
    let _ = std::fs::remove_dir_all(&base);
}

/// **Search stepping must not throw away the reader's position.** With a hit at the top of
/// the screen, scrolling down a little brings EARLIER matches into view; stepping back
/// through those used to yank each one up to the same fixed offset, scrolling the page away
/// from what the reader had deliberately positioned. A match already on screen is now just
/// highlighted where it is — and one that is not is still brought in, which the second half
/// of this test pins so the fix cannot become "never scroll".
#[test]
#[ignore] // needs a local Chrome/Chromium; see the module docs
fn stepping_search_hits_keeps_the_viewport_when_the_match_is_visible() {
    let _serial = serial();
    let base = base("search");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("search.jsonl");
    {
        let mut s = String::new();
        for i in 0..60u32 {
            s.push_str(&user(&format!("question {i} about the widget"), i % 60));
            s.push_str(&assistant(
                &format!(
                    "answer {i}: {}",
                    "the widget handles the request the same way. ".repeat(6)
                ),
                i % 60,
            ));
        }
        std::fs::write(&src, s).unwrap();
    }

    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            // A real reading viewport: with a short window the "previous" match lands under
            // the top chrome, where scrolling to it is the CORRECT behaviour and the test
            // would be measuring the wrong thing.
            .window_size(Some((1200, 900)))
            .args(vec![
                std::ffi::OsStr::new("--disable-background-timer-throttling"),
                std::ffi::OsStr::new("--disable-backgrounding-occluded-windows"),
                std::ffi::OsStr::new("--disable-renderer-backgrounding"),
            ])
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    wait_for(&tab, "initial render", Duration::from_secs(15), |s| {
        s["blocks"].as_i64().unwrap_or(-1) > 5
    });

    // Search, then step back onto a hit.
    eval(
        &tab,
        r##"(function () {
            var q = document.getElementById("q");
            q.focus(); q.value = "widget";
            q.dispatchEvent(new Event("input", {bubbles: true}));
            return true;
        })()"##,
        false,
    );
    std::thread::sleep(Duration::from_millis(900));
    eval(
        &tab,
        r##"(function () { document.getElementById("qprev").click(); return true; })()"##,
        false,
    );
    std::thread::sleep(Duration::from_millis(600));

    // The reader nudges the view back a little; earlier matches are now on screen above
    // the current one — the exact position stepping used to discard.
    let held = eval(
        &tab,
        r##"(async function () {
            // Toward the START of the session: that is what puts EARLIER matches on
            // screen above the current one, which is the case at issue.
            window.dispatchEvent(new WheelEvent("wheel", {deltaY: -120}));
            window.scrollBy(0, -150);
            await new Promise(function (r) { setTimeout(r, 400); });
            var before = Math.round(window.scrollY);
            var countBefore = document.getElementById("qcount").textContent;
            document.getElementById("qprev").click();
            await new Promise(function (r) { setTimeout(r, 500); });
            var cur = document.querySelector("#stream mark.hl.cur");
            return {
                before: before,
                after: Math.round(window.scrollY),
                countChanged: document.getElementById("qcount").textContent !== countBefore,
                markTop: cur ? Math.round(cur.getBoundingClientRect().top) : null,
                viewportH: window.innerHeight
            };
        })()"##,
        true,
    );
    assert_eq!(
        held["before"], held["after"],
        "a visible match is highlighted where it is, not scrolled to: {held}"
    );
    assert_eq!(
        held["countChanged"], true,
        "the selection still advanced: {held}"
    );
    let top = held["markTop"].as_i64().expect("a current mark");
    assert!(
        top > 0 && top < held["viewportH"].as_i64().unwrap(),
        "and it really was on screen: {held}"
    );

    // Keep stepping: once the match would sit under the top chrome, the page must move.
    let scrolled = eval(
        &tab,
        r##"(async function () {
            var start = Math.round(window.scrollY);
            for (var i = 0; i < 40; i++) {
                document.getElementById("qprev").click();
                await new Promise(function (r) { setTimeout(r, 60); });
                if (Math.round(window.scrollY) !== start) return {moved: true, from: start, to: Math.round(window.scrollY)};
            }
            return {moved: false, from: start, to: Math.round(window.scrollY)};
        })()"##,
        true,
    );
    assert_eq!(
        scrolled["moved"], true,
        "stepping past the top of the screen still brings the match into view: {scrolled}"
    );

    drop(tab);
    drop(browser);
    let _ = std::fs::remove_dir_all(&base);
}

/// A workflow's fleet is NOT part of the block record — the roster keeps changing after the
/// call that launched it is settled, so it rides the meta and the page hangs it under the
/// launching block on every poll (#38). Only a real engine can say whether that attachment
/// actually lands in the DOM, which is what this asserts.
#[test]
#[ignore = "needs a local Chrome"]
fn a_workflow_fleet_renders_under_its_launching_block() {
    let _serial = serial();
    let base = base("fleet");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("wf.jsonl");
    let run = "wf_browser1";
    let rundir = base
        .join("wf")
        .join("subagents")
        .join("workflows")
        .join(run);
    std::fs::create_dir_all(&rundir).unwrap();
    let dir = rundir.display().to_string();
    std::fs::write(
        &src,
        format!(
            concat!(
                r#"{{"type":"user","cwd":"/r","message":{{"role":"user","content":[{{"type":"text","text":"go"}}]}},"timestamp":"2026-08-26T10:00:00Z"}}"#,
                "\n",
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_W","name":"Workflow","input":{{"script":"x"}}}}]}},"timestamp":"2026-08-26T10:00:01Z"}}"#,
                "\n",
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_W","content":"Workflow launched in background.\nTranscript dir: {dir}\n"}}]}},"toolUseResult":{{"status":"async_launched","workflowName":"demo","runId":"{run}"}},"timestamp":"2026-08-26T10:00:09Z"}}"#,
                "\n",
            ),
            dir = dir.replace('\\', "\\\\"),
            run = run
        ),
    )
    .unwrap();
    for id in ["abrowser1", "abrowser2"] {
        std::fs::write(
            rundir.join(format!("agent-{id}.jsonl")),
            concat!(
                r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"work"}]},"timestamp":"2026-08-26T10:00:02Z"}"#,
                "\n",
            ),
        )
        .unwrap();
    }
    std::fs::write(
        rundir.join("journal.jsonl"),
        "{\"type\":\"started\",\"key\":\"v2:k\",\"agentId\":\"abrowser1\"}\n\
         {\"type\":\"result\",\"key\":\"v2:k\",\"agentId\":\"abrowser1\",\"result\":\"# Found it\\n\"}\n\
         {\"type\":\"started\",\"key\":\"v2:k\",\"agentId\":\"abrowser2\"}\n",
    )
    .unwrap();

    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");
    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();

    let probe = r#"(function () {
      var host = document.querySelector('#stream [data-run]');
      var rows = document.querySelectorAll('.fleet-row');
      var names = [];
      rows.forEach(function (r) {
        var a = r.querySelector('.fleet-name');
        names.push((a ? a.textContent : '') + '|' + (a ? a.getAttribute('href') : ''));
      });
      return JSON.stringify({ host: !!host, run: host ? host.dataset.run : null, names: names });
    })()"#;
    let mut seen = String::new();
    for _ in 0..60 {
        seen = tab
            .evaluate(probe, true)
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        if seen.contains("abrowser2") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let v: serde_json::Value = serde_json::from_str(&seen).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        v["host"], true,
        "the launching block is tagged with its run: {seen}"
    );
    assert_eq!(v["run"], run, "and it is the right run: {seen}");
    let names = v["names"].as_array().cloned().unwrap_or_default();
    assert_eq!(names.len(), 2, "both members rendered: {seen}");
    // A finished member is titled by what it returned; a running one by its launch position.
    assert!(
        names[0]
            .as_str()
            .unwrap_or("")
            .starts_with("Found it|?session=abrowser1"),
        "finished member titled and linked: {seen}"
    );
    assert!(
        names[1]
            .as_str()
            .unwrap_or("")
            .starts_with("agent 2|?session=abrowser2"),
        "running member titled by position and linked: {seen}"
    );
}

/// **Monitor v2's composition.** The rail is injected into the session document instead of
/// wrapping it in an `<iframe>`, and lays itself out `position: fixed` — so the transcript keeps
/// the DOCUMENT scroller, which is the entire reason this shape was chosen: every
/// `window.scrollY` in `export.js` (follow, pin, jump-to-bottom, turn landing, search stepping)
/// keeps working untouched. Only a real engine can say whether that is true, so this asserts the
/// three things the composition could plausibly have broken: the rail does not scroll away, the
/// document is what scrolls, and the view still lands pinned at the tail.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_v2_shell_keeps_the_document_scroller() {
    let _serial = serial();
    let base = base("v2shell");
    let src = base.join("v2.jsonl");
    {
        let mut s = String::new();
        for i in 0..40u32 {
            s.push_str(&user(
                &format!("question {i}: {}", "lorem ipsum ".repeat(8)),
                i,
            ));
            s.push_str(&assistant(
                &format!("answer {i}: {}", "dolor sit amet ".repeat(12)),
                i,
            ));
        }
        std::fs::write(&src, s).unwrap();
    }
    // v2 lists sessions from the real store, so point it at this fixture by deep link: the
    // shell route registers an unknown id on demand. It needs the transcript inside a store
    // it scans, so this test drives the id the server reports for the file it was given.
    let stores = Stores::new(&base);
    // A hermetic tall session: the assertion is about LAYOUT, not content.
    let sid = "cccccccc-0000-4000-8000-000000000001".to_string();
    stores.claude_session(&sid, &harness::long_session(40, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2831, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    // Any session the machine has will do — the assertion is about LAYOUT, not content.
    let id = sid.clone();
    // `?ui=classic` explicitly: the app shell is the default at `/` now, and the layout
    // contract this test holds — one document scroller, a rail that does not scroll away, no
    // iframe — belongs to the CLASSIC splice shell. Both shells are supported while the new one
    // is being validated, so this assertion names the one it is about instead of riding the
    // default and going quiet the moment the default moves (it has gone quiet twice before).
    tab.navigate_to(&format!("http://127.0.0.1:2831/?ui=classic&session={id}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    // A cold cache renders the whole session before there is anything to scroll — wait for
    // the page to exceed the viewport rather than guessing at a sleep.
    for _ in 0..80 {
        let tall = tab
            .evaluate(
                "document.body.scrollHeight > window.innerHeight + 200",
                true,
            )
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if tall {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let probe = r#"(function () {
      var rail = document.getElementById('v2rail');
      var stream = document.getElementById('stream');
      if (!rail || !stream) return JSON.stringify({ ok: false });
      var before = rail.getBoundingClientRect().top;
      window.scrollTo({ top: 400 });
      var scrolled = window.scrollY;
      var after = rail.getBoundingClientRect().top;
      return JSON.stringify({
        ok: true,
        docScrolls: scrolled > 0,                       // the DOCUMENT is the scroller
        railFixed: Math.abs(after - before) < 1,        // …and the rail does not move with it
        streamOffset: stream.getBoundingClientRect().left >= rail.getBoundingClientRect().right - 1,
        noFrame: document.querySelectorAll('iframe').length === 0,
        // seam 0: the served classic page carries the inlined shared modules.
        shared: typeof (window.__shared && window.__shared.groupSessions) === 'function'
      });
    })()"#;
    let seen = tab
        .evaluate(probe, true)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    drop(monitor);
    let v: serde_json::Value = serde_json::from_str(&seen).unwrap_or(serde_json::Value::Null);
    assert_eq!(v["ok"], true, "the shell composed both panes: {seen}");
    assert_eq!(v["docScrolls"], true, "the document scrolls: {seen}");
    assert_eq!(
        v["railFixed"], true,
        "the rail stays put while it does: {seen}"
    );
    assert_eq!(
        v["streamOffset"], true,
        "the transcript clears the rail: {seen}"
    );
    assert_eq!(v["noFrame"], true, "no iframe anywhere: {seen}");
    assert_eq!(
        v["shared"], true,
        "the inlined shared modules reach the served page: {seen}"
    );
}

/// The app shell can hide a session and get it back (parity #1). The classic rail always
/// could; the first app shell filtered `row.hidden` out with no control in either direction,
/// which made hiding a one-way trap. This drives the real thing: the row action calls
/// `/api/ignore` with the server's key, the tree re-polls and the row is gone; "Hidden (n)"
/// appears, reveals it dimmed, and its restore action brings it back. Hide state lives in
/// the scratch STATE dir, never the user's, so nothing on the machine is actually hidden.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_hides_and_restores_a_session() {
    let _serial = serial();
    let base = base("appshell-hide");
    let stores = Stores::new(&base);
    stores.claude_session(
        "cccccccc-0000-4000-8000-000000000001",
        &harness::long_session(12, harness::Shape::default()),
    );
    let store_rows = 1;
    let monitor = Monitor::spawn(Kind::V2, 2832, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to("http://127.0.0.1:2832/?ui=app").unwrap();
    tab.wait_until_navigated().unwrap();

    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    let mut first = String::new();
    for _ in 0..120 {
        let v = eval(
            &tab,
            "(document.querySelector('.tree-row.session[data-session]')||{}).dataset?.session||''",
        );
        if let Some(id) = v.as_str().filter(|s| !s.is_empty()) {
            first = id.to_string();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // On failure say WHICH module the browser could not load: a served-list miss 404s one
    // import and takes the whole graph down, and nothing on the node side can see it.
    let painted = eval(&tab, "JSON.stringify({hiddenBtn: !!document.getElementById('hiddenBtn'), tree: (document.getElementById('tree')||{innerHTML:''}).innerHTML.slice(0, 160), modules: performance.getEntriesByType('resource').filter(e => e.name.includes('/monitor-ui/')).map(e => [e.name.split('/').pop(), e.responseStatus])})");
    assert!(
        !first.is_empty(),
        "the store has {store_rows} rows but the app shell painted none in 30s — did app.js throw? {painted}"
    );
    let row = format!("document.querySelector('.tree-row.session[data-session=\"{first}\"]')");
    let before = eval(
        &tab,
        "Number(document.getElementById('hiddenCount').textContent)||0",
    )
    .as_i64()
    .unwrap_or(0);
    // Hide it through the row's own action (it is a real button; hover only affects opacity).
    eval(
        &tab,
        &format!("{row}.querySelector('[data-ignore-op=\"add\"]').click(); 'ok'"),
    );
    let mut gone = false;
    for _ in 0..40 {
        if eval(&tab, &format!("{row} === null")).as_bool() == Some(true) {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let after_hide = eval(&tab, "JSON.stringify({shown: !document.getElementById('hiddenBtn').hidden, n: Number(document.getElementById('hiddenCount').textContent)||0})");
    // Reveal, then restore through the revealed row's action.
    eval(&tab, "document.getElementById('hiddenBtn').click(); 'ok'");
    let mut revealed = false;
    for _ in 0..20 {
        if eval(
            &tab,
            &format!("!!({row}) && {row}.classList.contains('is-hidden')"),
        )
        .as_bool()
            == Some(true)
        {
            revealed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    eval(
        &tab,
        &format!("{row}.querySelector('[data-ignore-op=\"remove\"]').click(); 'ok'"),
    );
    let mut restored = false;
    for _ in 0..40 {
        if eval(
            &tab,
            &format!("!!({row}) && !{row}.classList.contains('is-hidden')"),
        )
        .as_bool()
            == Some(true)
        {
            restored = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let after_restore = eval(
        &tab,
        "Number(document.getElementById('hiddenCount').textContent)||0",
    )
    .as_i64()
    .unwrap_or(-1);
    drop(monitor);
    assert!(gone, "the hidden row left the tree");
    let v: serde_json::Value = serde_json::from_str(after_hide.as_str().unwrap_or("null"))
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(v["shown"], true, "Hidden (n) appeared: {after_hide}");
    assert_eq!(
        v["n"].as_i64().unwrap_or(-1),
        before + 1,
        "the count rose by one: {after_hide}"
    );
    assert!(revealed, "the reveal showed the row, dimmed");
    assert!(restored, "restore brought it back undimmed");
    assert_eq!(after_restore, before, "the count fell back");
}

/// A sub-agent child opened directly has a way back up, and stays open (parity #3). The
/// child is never a list row, so the first app shell declared it "gone" on the next index
/// poll — a child could be looked at for at most five seconds. Now the parent control goes
/// live from the child's own meta, Back lands on the parent, and the poll leaves a child
/// alone. Needs a parent with children in the real store; skips, with a diagnostic, when
/// none of the first sessions has one.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_walks_a_child_back_to_its_parent() {
    let _serial = serial();
    let base = base("appshell-parent");
    let stores = Stores::new(&base);
    // A parent with one sub-agent child, related by PATH alone (<sid>/subagents/agent-<id>.jsonl).
    let parent = "dddddddd-0000-4000-8000-000000000001".to_string();
    // The parent SPAWNS the child (an `Agent` call whose result names the agent id): that is
    // what lists it under `meta.children`; the file under `<sid>/subagents/` is where the
    // child is then read from.
    let mut transcript = harness::long_session(6, harness::Shape::default());
    transcript += &harness::agent_spawn("call_1", "Explore", 7);
    transcript += &harness::agent_result("call_1", "aExplore-1", "Explore", 7);
    transcript += &harness::long_session(6, harness::Shape::default());
    stores.claude_session(&parent, &transcript);
    stores.claude_child(
        &parent,
        "aExplore-1",
        &harness::long_session(6, harness::Shape::default()),
    );
    let monitor = Monitor::spawn(Kind::V2, 2833, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    // Find a parent with children by asking the same routes the shell uses — first the
    // listing (blocking on the cold scan), then `/pull` per session for its meta.
    tab.navigate_to("http://127.0.0.1:2833/?ui=app").unwrap();
    tab.wait_until_navigated().unwrap();
    // The child's id is whatever the index gave the sub-agent transcript: read it off the
    // parent's meta rather than guessing the naming.
    let listed = eval(&tab, &format!("fetch('/pull?session={parent}&cursor=', {{cache:'no-store'}}).then(r => r.json()).then(j => JSON.stringify((j.meta && j.meta.children || []).map(c => c.id))).catch(() => '[]')"));
    let children: Vec<String> =
        serde_json::from_str(listed.as_str().unwrap_or("[]")).unwrap_or_default();
    let child_id = children
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the fixture parent lists its sub-agent child: {listed}"));
    tab.navigate_to(&format!("http://127.0.0.1:2833/?ui=app&session={child_id}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    let mut live = false;
    for _ in 0..80 {
        if eval(
            &tab,
            "document.getElementById('sessionParent').classList.contains('is-live')",
        )
        .as_bool()
            == Some(true)
        {
            live = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let header = eval(&tab, "JSON.stringify({title: document.getElementById('sessionTitle').textContent, crumb: document.getElementById('sessionCrumb').textContent, parent: document.getElementById('sessionParent').dataset.parent, empty: (document.querySelector('.monitor-empty:not([hidden])')||{}).textContent || ''})");
    // Outlive one index poll (5 s): the child must still be the open session afterwards.
    std::thread::sleep(std::time::Duration::from_millis(6500));
    let survived = eval(&tab, "JSON.stringify({sel: new URLSearchParams(location.search).get('session'), title: document.getElementById('sessionTitle').textContent, gone: !!document.querySelector('.monitor-empty:not([hidden])')})");
    // #82: the way back is VISIBLE — a size, its label — before it is used; a JavaScript click
    // works on a hidden element, which is how this case passed while the control was hidden.
    let control = eval(&tab, "JSON.stringify((function(){ var b = document.getElementById('sessionParent'); var r = b.getBoundingClientRect(); return { visible: b.offsetParent !== null && r.width >= 24 && r.height >= 20, label: b.textContent.trim() }; })())");
    let control: serde_json::Value =
        serde_json::from_str(control.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        control["visible"], true,
        "the parent control is visible: {control}"
    );
    assert!(
        control["label"]
            .as_str()
            .unwrap_or("")
            .contains("Parent session"),
        "…and labelled: {control}"
    );
    eval(
        &tab,
        "document.getElementById('sessionParent').click(); 'ok'",
    );
    let mut landed = String::new();
    for _ in 0..40 {
        let v = eval(
            &tab,
            "new URLSearchParams(location.search).get('session') || ''",
        );
        if v.as_str() == Some(parent.as_str()) {
            landed = parent.clone();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    drop(monitor);
    assert!(
        live,
        "the parent control went live for child {child_id} of {parent}: {header}"
    );
    let h: serde_json::Value =
        serde_json::from_str(header.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        h["parent"], parent,
        "the control points at the parent: {header}"
    );
    assert_ne!(
        h["title"], "Agent Monitor",
        "the child has its own title, not the empty header: {header}"
    );
    let s: serde_json::Value = serde_json::from_str(survived.as_str().unwrap_or("null"))
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        s["sel"], child_id,
        "the child stayed selected across an index poll: {survived}"
    );
    assert_eq!(s["gone"], false, "…and was not declared gone: {survived}");
    assert_eq!(landed, parent, "Back landed on the parent");
}

/// The app shell remembers where a session was scrolled to and lands there again after a
/// reload (parity #6) — unless the reader was following the tail, in which case the tail is
/// the position and it comes back following. Only a real engine can say this: the memory is
/// the viewport's own DOM anchor, and the restore has to wait for that unit to stream in.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_restores_the_scroll_position_across_a_reload() {
    let _serial = serial();
    let base = base("appshell-scroll");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000001".to_string();
    stores.claude_session(&sid, &harness::long_session(80, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2834, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    tab.navigate_to(&format!("http://127.0.0.1:2834/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "document.querySelector('.transcript') && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3", "the fixture session to be tall enough to scroll", std::time::Duration::from_secs(20), "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : -1");
    // Scroll with USER intent (the viewport only treats a scroll as the reader's after a
    // wheel/pointer event), to roughly the middle, and read the viewport's own anchor.
    let anchor_js = r#"(function(){ var s=document.querySelector('.transcript'), top=s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { var r=c.getBoundingClientRect(); if (r.bottom > top + 1) return JSON.stringify({key: c.dataset.unitKey, top: Math.round(r.top - top)}); } return 'null'; })()"#;
    eval(&tab, "(function(){ var s=document.querySelector('.transcript'); s.dispatchEvent(new WheelEvent('wheel', {deltaY: 1})); s.scrollTop = Math.floor(s.scrollHeight * 0.45); return 'ok'; })()");
    std::thread::sleep(std::time::Duration::from_millis(900));
    let before = eval(&tab, anchor_js);
    let b: serde_json::Value =
        serde_json::from_str(before.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
    assert!(
        b["key"].is_string(),
        "an anchor was captured before the reload: {before}"
    );
    tab.reload(false, None).unwrap();
    tab.wait_until_navigated().unwrap();
    let mut after = serde_json::Value::Null;
    for _ in 0..80 {
        let v = eval(&tab, anchor_js);
        let a: serde_json::Value =
            serde_json::from_str(v.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
        if a["key"] == b["key"] {
            after = a;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert_eq!(
        after["key"], b["key"],
        "after the reload the same unit is at the top: before {before} after {after}"
    );
    let drift = (after["top"].as_i64().unwrap_or(9999) - b["top"].as_i64().unwrap_or(0)).abs();
    assert!(
        drift <= 6,
        "…within a few pixels: before {before} after {after}"
    );
    // Now follow the tail, reload, and come back following: at the bottom, not the old offset.
    eval(
        &tab,
        "document.getElementById('jumpToBottom').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(900));
    tab.reload(false, None).unwrap();
    tab.wait_until_navigated().unwrap();
    let mut at_tail = false;
    for _ in 0..80 {
        let gap = eval(&tab, "(function(){ var s=document.querySelector('.transcript'); if (!s || !document.querySelector('.virtual-window').children.length) return 1e9; return s.scrollHeight - s.clientHeight - s.scrollTop; })()");
        if gap.as_f64().map(|g| g <= 2.0) == Some(true) {
            at_tail = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    drop(monitor);
    assert!(
        at_tail,
        "a followed session comes back following, at the tail"
    );
}

/// The keymap lands where it says (parity #11): `]` moves to the next turn, `[` back, `j`
/// puts focus on a tool head — and none of it fires while typing in the search box. Only a
/// real engine can say where a key-driven jump actually scrolled to.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_keys_step_turns_and_heads() {
    let _serial = serial();
    let base = base("appshell-keys");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000001".to_string();
    stores.claude_session(
        &sid,
        &harness::long_session(
            40,
            harness::Shape {
                tool_every: 2,
                think_every: 5,
                prose_repeat: 6,
            },
        ),
    );
    let monitor = Monitor::spawn(Kind::V2, 2835, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    tab.navigate_to(&format!("http://127.0.0.1:2835/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "(function(){ var u=document.querySelectorAll('.virtual-window .turn.user').length, h=document.querySelectorAll('.virtual-window button.renderer-head').length; return u >= 3 && h >= 1; })()", "three user turns and a tool head mounted", std::time::Duration::from_secs(20), "document.querySelectorAll('.virtual-window .turn.user').length + ' turns, ' + document.querySelectorAll('.virtual-window button.renderer-head').length + ' heads'");
    let key = |k: &str, shift: bool| {
        format!("document.dispatchEvent(new KeyboardEvent('keydown', {{key: {k:?}, shiftKey: {shift}, bubbles: true, cancelable: true}})); 'ok'")
    };
    // Start at the top, then `]` twice: the anchor must move to a LATER user turn each time.
    let top_turn = "(function(){ var s=document.querySelector('.transcript'), top=s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { var r=c.getBoundingClientRect(); if (r.bottom > top + 1) return c.dataset.unitKey || ''; } return ''; })()";
    eval(&tab, "(function(){ var s=document.querySelector('.transcript'); s.dispatchEvent(new WheelEvent('wheel',{deltaY:-1})); s.scrollTop = 0; return 'ok'; })()");
    std::thread::sleep(std::time::Duration::from_millis(500));
    // Position is the unit's RECORD index (`data-unit-from`), never its place among the
    // mounted children: the window is virtualized and re-mounted around every jump.
    let unit_index = |tab: &headless_chrome::Tab| -> i64 {
        eval(tab, "(function(){ var s=document.querySelector('.transcript'), top=s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { if (c.getBoundingClientRect().top >= top - 24) return Number(c.dataset.unitFrom); } return -1; })()")
            .as_i64()
            .unwrap_or(-1)
    };
    let start = unit_index(&tab);
    eval(&tab, &key("]", false));
    std::thread::sleep(std::time::Duration::from_millis(600));
    let after_one = unit_index(&tab);
    eval(&tab, &key("]", false));
    std::thread::sleep(std::time::Duration::from_millis(600));
    let after_two = unit_index(&tab);
    let top_after_two = eval(&tab, top_turn);
    eval(&tab, &key("[", false));
    std::thread::sleep(std::time::Duration::from_millis(600));
    let back = unit_index(&tab);
    // `j` focuses a tool head; typing into the search box must not step anything.
    eval(&tab, &key("j", false));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let focused = eval(
        &tab,
        "document.activeElement && document.activeElement.classList.contains('renderer-head')",
    );
    eval(
        &tab,
        "document.getElementById('transcriptSearchInput').focus(); 'ok'",
    );
    let before_typing = unit_index(&tab);
    eval(&tab, "document.getElementById('transcriptSearchInput').dispatchEvent(new KeyboardEvent('keydown', {key: ']', bubbles: true, cancelable: true})); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let while_typing = unit_index(&tab);
    drop(monitor);
    assert!(
        after_one > start,
        "`]` moved to a later unit: {start} -> {after_one}"
    );
    assert!(
        after_two > after_one,
        "…and again: {after_one} -> {after_two} (top {top_after_two})"
    );
    assert!(back < after_two, "`[` moved back: {after_two} -> {back}");
    assert_eq!(focused, true, "`j` put focus on a tool head");
    assert_eq!(
        while_typing, before_typing,
        "keys do nothing while typing in the search box"
    );
}

/// The app shell's layout contract, the counterpart of the v2 classic case (task #27): on a
/// fresh open the view lands pinned at the tail; the TRANSCRIPT is the scroller, and scrolling
/// it leaves the header and the sidebar exactly where they were; there is no iframe anywhere.
/// The other app-shell cases exercise hide/restore, child→parent, scroll memory and the keys;
/// this one is the plain "it composes" assertion that was missing.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_composes_one_scroller_under_fixed_chrome() {
    let _serial = serial();
    let base = base("appshell-layout");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000001".to_string();
    stores.claude_session(&sid, &harness::long_session(80, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2836, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    tab.navigate_to(&format!("http://127.0.0.1:2836/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "(function(){ var s=document.querySelector('.transcript'); return !!s && document.querySelector('.virtual-window').children.length > 0 && s.scrollHeight > s.clientHeight * 2; })()", "the fixture session to be tall enough to scroll", std::time::Duration::from_secs(20), "document.querySelector('.virtual-window') ? document.querySelector('.virtual-window').children.length : -1");
    // Landed pinned at the tail, on its own, without anyone scrolling.
    let mut at_tail = false;
    for _ in 0..40 {
        let gap = eval(&tab, "(function(){ var s=document.querySelector('.transcript'); return s.scrollHeight - s.clientHeight - s.scrollTop; })()");
        if gap.as_f64().map(|g| g <= 2.0) == Some(true) {
            at_tail = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let probe = r#"(function () {
      var s = document.querySelector('.transcript'), side = document.querySelector('.sidebar'), head = document.querySelector('header');
      var before = { side: side.getBoundingClientRect().top, head: head.getBoundingClientRect().top, doc: window.scrollY };
      s.dispatchEvent(new WheelEvent('wheel', {deltaY: -1}));
      s.scrollTop = Math.max(0, s.scrollTop - 600);
      var after = { side: side.getBoundingClientRect().top, head: head.getBoundingClientRect().top, doc: window.scrollY, moved: s.scrollTop };
      return JSON.stringify({
        transcriptScrolls: after.moved < s.scrollHeight - s.clientHeight,   // the TRANSCRIPT moved
        documentStill: before.doc === 0 && after.doc === 0,                  // …the document did not
        sideFixed: Math.abs(after.side - before.side) < 1,
        headFixed: Math.abs(after.head - before.head) < 1,
        noFrame: document.querySelectorAll('iframe').length === 0
      });
    })()"#;
    let seen = eval(&tab, probe);
    drop(monitor);
    assert!(at_tail, "a fresh open lands pinned at the tail");
    let v: serde_json::Value =
        serde_json::from_str(seen.as_str().unwrap_or("null")).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        v["transcriptScrolls"], true,
        "the transcript is the scroller: {seen}"
    );
    assert_eq!(
        v["documentStill"], true,
        "the document does not scroll: {seen}"
    );
    assert_eq!(v["sideFixed"], true, "the sidebar stays put: {seen}");
    assert_eq!(v["headFixed"], true, "the header stays put: {seen}");
    assert_eq!(v["noFrame"], true, "no iframe anywhere: {seen}");
}

/// Browser-served artifacts: on a page whose host asked for them (`artifacts=1` ⇒
/// `data-artifacts` on `<body>`), clicking a file path in a tool header SHOWS the file's
/// bytes over the page instead of asking the server to open a Finder window.
///
/// A real engine is the only place this is provable end to end: the click rides the same
/// delegated handler that folds blocks (so the fold must NOT toggle), the reply's
/// `content-type` decides between an `<img>` and a text pane, and the bytes come from the
/// `/file` route's containment guard — which is the point of serving them at all. The Rust
/// side proves what the route refuses; this proves what the page does with what it gets.
#[test]
#[ignore] // needs a local Chrome/Chromium; see the module docs
fn a_clicked_file_path_opens_its_content_in_the_page() {
    let _serial = serial();
    let base = base("artifacts");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    // The file the session "read" — inside the session's own cwd, which is what makes it
    // servable at all.
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let target = repo.join("hello.txt");
    std::fs::write(&target, "artifact body line one\nline two\n").unwrap();

    let src = base.join("art.jsonl");
    let abs = target.display().to_string();
    let cwd = repo.display().to_string();
    let mut s = String::new();
    s.push_str(&format!(
        "{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"read it\"}}]}},\"timestamp\":\"2026-08-21T10:00:00Z\"}}\n"
    ));
    s.push_str(&format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Read\",\"input\":{{\"file_path\":\"{abs}\"}}}}]}},\"timestamp\":\"2026-08-21T10:00:01Z\"}}\n"
    ));
    s.push_str(
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"artifact body line one\\nline two\\n\"}]},\"timestamp\":\"2026-08-21T10:00:02Z\"}\n",
    );
    std::fs::write(&src, s).unwrap();

    // A PAIRED server, stood up from the public API — `/file` is offered only to a client
    // holding the token (owner, 2026-08-27), so an unpaired `--html` server is the wrong
    // subject: it would exercise the fallback, not the feature. This is the same composition
    // `agent-monitor-v2` uses: one `SessionService`, `service_routes` for everything, and the
    // gate carrying the token.
    let service = std::sync::Arc::new(
        claude_replay_html::SessionService::new(claude_replay_html::ServiceConfig {
            cache_root: Some(base.join("cache")),
            presentation: claude_replay_present::cache::Presentation::Html,
            fold: Default::default(),
            scratch: base.join("scratch"),
            root_lock: claude_replay_html::RootLock::PerSession,
        })
        .expect("service"),
    );
    let id = service.register_root(&src);
    let dir = base.join("scratch");
    let handler = {
        let service = service.clone();
        std::sync::Arc::new(move |req: &claude_replay_html::Request| {
            claude_replay_html::service_routes(Some(&service), &dir, req)
        })
    };
    let token = "browser-test-token";
    let port = claude_replay_html::spawn_listener_gated(
        0,
        handler,
        claude_replay_html::AuthGate::with_token(token),
    )
    .expect("listener binds");
    // `/session?id=…&artifacts=1` is the same page `--html` serves, with the host's opt-in —
    // the flag is a page mode, not a separate renderer. The token rides the first URL and
    // comes back as the cookie every later fetch carries.
    let url = format!("http://127.0.0.1:{port}/session?id={id}&artifacts=1&token={token}");

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();

    // Everything this test asserts, in one page-side probe.
    const PROBE: &str = r#"{
        paths: document.querySelectorAll('.tool-path').length,
        artifacts: document.body.dataset.artifacts === "1",
        text: (document.querySelector('.lightbox .lb-text') || {}).textContent || null,
        boxes: document.querySelectorAll('.lightbox').length,
        open: (function () {
            var tp = document.querySelector('.tool-path');
            var f = tp && tp.closest('.fold');
            return f ? String(f.dataset.open) : "none";
        })()
    }"#;

    let ready = wait_probe(
        &tab,
        "the tool header with its file path rendered",
        Duration::from_secs(15),
        PROBE,
        |s| s["paths"].as_i64().unwrap_or(0) == 1,
    );
    assert_eq!(
        ready["artifacts"], true,
        "the host's opt-in reached the page"
    );
    let fold_before = ready["open"].clone();

    // Click it exactly as a reader would, then wait for the overlay to hold the FILE's
    // bytes — not the tool result's rendering of them.
    eval(
        &tab,
        r#"(function () { document.querySelector('.tool-path').click(); return 1; })()"#,
        false,
    );
    let shown = wait_probe(
        &tab,
        "the file's content over the page",
        Duration::from_secs(10),
        PROBE,
        |s| s["text"].as_str().is_some_and(|t| t.contains("line two")),
    );
    assert_eq!(
        shown["text"].as_str().unwrap(),
        "artifact body line one\nline two\n",
        "the overlay shows the file, whole"
    );
    assert_eq!(
        shown["open"], fold_before,
        "and the click did not also toggle the fold it sits in"
    );

    // Escape tears it down — the modal owns the key while it is up.
    tab.press_key("Escape").unwrap();
    let gone = wait_probe(
        &tab,
        "the overlay closes",
        Duration::from_secs(5),
        PROBE,
        |s| s["boxes"].as_i64().unwrap_or(1) == 0,
    );
    assert_eq!(gone["boxes"], 0, "closed");
}

/// Pairing is the MASTER SWITCH for v2's write capability (#133 §7.1), and the rail must say
/// so with its affordances: unpaired, no row offers a compose button, because every write
/// route 401s and offering one would offer a dead end; paired, the same rows grow one.
///
/// Worth a browser test rather than a unit test because the rule lives in three places that
/// have to agree — the server's `{{PAIRED}}` substitution, the rail's `canCompose`, and the
/// route's own `deny_write` — and only a real page exercises the first two together. The run
/// is isolated by `AGENT_MONITOR_STATE`, so it neither reads nor writes the developer's own
/// pairing token.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_compose_affordance_appears_only_once_paired() {
    let _serial = serial();
    let base = base("v2pair");
    let stores = Stores::new(&base);
    stores.claude_finished();
    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .unwrap(),
    )
    .expect("chrome launches");

    // `paired` says whether to pass `--pair`; both runs share an isolated state dir, so the
    // second one finds the token the first never minted.
    let probe = |paired: bool, port: u16| -> (bool, i64) {
        let m = Monitor::spawn(Kind::V2, port, &base, Some(&stores), paired);
        let tab = browser.new_tab().unwrap();
        let console = harness::tap_console(&tab);
        m.pair(&tab);
        // The affordance under test is the classic splice's: ask for it by name, since the
        // app shell is what `/` serves by default.
        m.open(&tab, "?ui=classic");
        // Wait for the rows, not a fixed beat: the index scan and the first paint take longer
        // on a loaded machine, and the affordance follows the rows by another round trip.
        let rows_js = "document.querySelectorAll('.v2row').length";
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline
            && harness::eval(&tab, rows_js).as_i64().unwrap_or(0) == 0
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let buttons_js = "document.querySelectorAll('.v2send').length";
        let settle_until =
            std::time::Instant::now() + std::time::Duration::from_secs(if paired { 20 } else { 2 });
        while std::time::Instant::now() < settle_until
            && (harness::eval(&tab, buttons_js).as_i64().unwrap_or(0) > 0) != paired
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let buttons = harness::eval(&tab, buttons_js).as_i64().unwrap_or(-1);
        let rows = harness::eval(&tab, rows_js).as_i64().unwrap_or(0);
        if rows == 0 {
            let body = harness::eval(&tab, "location.href + ' | ' + document.title + ' | ' + document.body.innerText.replace(/\\s+/g, ' ').slice(0, 300)");
            eprintln!(
                "no rows on port {port}: {body} console: {:?}",
                console.lock().unwrap()
            );
        }
        let _ = tab.close(true);
        drop(m);
        (rows > 0, buttons)
    };

    let (had_rows, unpaired_buttons) = probe(false, 2841);
    assert!(had_rows, "the fixture session is listed");
    assert_eq!(
        unpaired_buttons, 0,
        "unpaired: every write route 401s, so no row offers to send"
    );
    let (_, paired_buttons) = probe(true, 2842);
    assert!(
        paired_buttons > 0,
        "paired: the sessions that can be resumed or injected offer it ({paired_buttons})"
    );
}

/// The "Artifacts ▾" roster groups REPUBLISHES back into artifacts.
///
/// A page that listed every publish would be useless for the case that motivated this: one
/// real session made 20 `Artifact` calls addressing 2 decks. The roster keys on the URL — the
/// artifact's stable identity — so those become two rows carrying a count, and the LATEST
/// publish supplies the description, that being the one the artifact currently has.
///
/// A browser test because the roster is derived CLIENT-side from the records the page holds
/// (deliberately: a `SessionMeta` roster would cost a fold-version bump and a machine-wide
/// cache rebuild), so the grouping exists nowhere else to test.
#[test]
#[ignore] // needs a local Chrome/Chromium; see the module docs
fn the_artifact_roster_groups_republishes_by_url() {
    let _serial = serial();
    let base = base("artroster");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("decks.jsonl");
    const DECK: &str = "https://claude.ai/code/artifact/f37a45eb-a40c-48b9-9cc0-81f27c9811f5";
    const ZH: &str = "https://claude.ai/code/artifact/e4eb4b14-da62-4571-87bd-cc2966bfdaac";
    let publish = |i: u32, stem: &str, url: &str, desc: &str| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-08-28T10:{i:02}:00Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"a{i}\",\"name\":\"Artifact\",\"input\":{{\"file_path\":\"/w/{stem}.html\",\"description\":\"{desc}\",\"favicon\":\"🧭\"}}}}]}}}}\n\
             {{\"type\":\"user\",\"timestamp\":\"2026-08-28T10:{i:02}:03Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"a{i}\",\"content\":\"Published /w/{stem}.html at {url}\\n\\nTo update: republish the same path.\"}}]}}}}\n"
        )
    };
    let mut s = String::from(
        "{\"type\":\"user\",\"cwd\":\"/w\",\"timestamp\":\"2026-08-28T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"build the deck\"}]}}\n",
    );
    // Three publishes of the deck, one of the translation, then a fourth deck publish whose
    // description differs — the roster must show the LAST one.
    for i in 1..=3 {
        s.push_str(&publish(i, "rowt-deck", DECK, "A 24-slide tour."));
    }
    s.push_str(&publish(4, "rowt-deck-zh", ZH, "The Chinese edition."));
    s.push_str(&publish(5, "rowt-deck", DECK, "A 25-slide tour, final."));
    std::fs::write(&src, s).unwrap();

    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .unwrap(),
    )
    .expect("chrome launches (install Chrome/Chromium to run this harness)");
    let tab = browser.new_tab().unwrap();
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();

    const PROBE: &str = r#"{
        label: (document.querySelector('#btn-artifacts .tf-label') || {}).textContent || "",
        shown: !!document.getElementById('artifactnav') &&
               getComputedStyle(document.getElementById('artifactnav')).display !== 'none',
        disabled: !!document.querySelector('#btn-artifacts.disabled'),
        links: document.querySelectorAll('.artifact-link').length,
        rows: [].map.call(document.querySelectorAll('.artifact-item'), function (a) {
            return {
                name: (a.querySelector('.artifact-name') || {}).textContent || "",
                desc: (a.querySelector('.artifact-desc') || {}).textContent || "",
                count: (a.querySelector('.artifact-count') || {}).textContent || "",
                href: a.getAttribute('href')
            };
        })
    }"#;
    let seen = wait_probe(
        &tab,
        "the artifact roster",
        Duration::from_secs(15),
        PROBE,
        |s| s["rows"].as_array().is_some_and(|r| r.len() == 2),
    );
    assert_eq!(seen["shown"], true, "the control is present");
    assert_eq!(
        seen["disabled"], false,
        "and live, this session having published"
    );
    assert_eq!(seen["label"], "Artifacts (2) ▾", "{seen}");
    assert_eq!(
        seen["links"], 5,
        "every publish still links to its artifact"
    );

    let rows = seen["rows"].as_array().unwrap();
    assert_eq!(rows[0]["name"], "rowt-deck");
    assert_eq!(rows[0]["href"], DECK, "grouped by URL");
    assert_eq!(rows[0]["count"], "×4", "four publishes, one row");
    assert_eq!(
        rows[0]["desc"], "A 25-slide tour, final.",
        "the latest publish describes the artifact"
    );
    assert_eq!(rows[1]["name"], "rowt-deck-zh");
    assert_eq!(rows[1]["count"], "", "a single publish needs no count");

    // A session that published NOTHING keeps the control, grayed — the rule that makes "I
    // don't see it" mean something. Hidden-when-empty made an inapplicable control and a
    // broken one look identical, which is how this assertion came to exist.
    let plain = base.join("plain.jsonl");
    std::fs::write(
        &plain,
        user("nothing to publish here", 0) + &assistant("Right.", 1),
    )
    .unwrap();
    let server2 = start_server(&args, std::slice::from_ref(&plain)).expect("server starts");
    let tab2 = browser.new_tab().unwrap();
    tab2.navigate_to(&server2.url_for_root(0).expect("hosted"))
        .unwrap();
    tab2.wait_until_navigated().unwrap();
    let bare = wait_probe(
        &tab2,
        "the grayed control on a session with no artifacts",
        Duration::from_secs(15),
        PROBE,
        |s| s["shown"] == true,
    );
    assert_eq!(bare["disabled"], true, "grayed, not gone: {bare}");
    assert_eq!(bare["label"], "Artifacts ▾", "and uncounted: {bare}");
    assert_eq!(bare["rows"].as_array().map(|r| r.len()), Some(0));
    let _ = tab2.close(true);
}

/// The classic rail (v1 `agent-monitor`, `?ui=classic`) on the hermetic family store: rows
/// render, the fork family clusters into ONE row whose ⑂ chip opens the fork, and hide /
/// restore round-trip through `/api/ignore` — the server's `hidden` flag flips, the rail's
/// "Hidden (n)" toggle reveals the row, and restoring puts the family back. The first rail
/// case (#43): the baseline the shared-module change is measured against.
#[test]
#[ignore]
fn the_classic_rail_clusters_a_family_and_hides_and_restores_a_row() {
    let _serial = serial();
    let base = base("rail-family");
    let stores = Stores::new(&base);
    let (root_id, fork_id) = stores.qoderwork_family();
    let monitor = Monitor::spawn(Kind::V1, 2837, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    // Poll a predicate: the rail re-renders on its own 2.5 s poll and right after an ignore.
    let until = |tab: &headless_chrome::Tab, js: &str, what: &str| {
        for _ in 0..40 {
            if eval(tab, js).as_bool() == Some(true) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let rows = eval(tab, "[...document.querySelectorAll('.row')].map(r => r.dataset.id + ':' + r.className).join(' | ')");
        panic!("timed out waiting for {what}; rows: {rows}");
    };
    // An OBJECT result comes back by value only as JSON text, so probes stringify it.
    let probe = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        serde_json::from_str(
            eval(tab, &format!("JSON.stringify({js})"))
                .as_str()
                .unwrap_or("null"),
        )
        .unwrap_or(serde_json::Value::Null)
    };
    let row = |id: &str| format!(".row[data-id=\"{id}\"]");
    // The server's word on a row, fetched from the page so the rail's own state is untouched.
    let server_hidden = |tab: &headless_chrome::Tab, id: &str| -> Option<bool> {
        let js = format!("(async () => {{ const j = await (await fetch('/api/sessions', {{cache: 'no-store'}})).json(); for (const g of j.groups || []) for (const r of g.rows || []) if (r.id === {id:?}) return !!r.hidden; return null; }})()");
        eval(tab, &js).as_bool()
    };

    monitor.pair(&tab);
    tab.navigate_to("http://127.0.0.1:2837/?ui=classic")
        .unwrap();
    tab.wait_until_navigated().unwrap();

    // 1. Rows render, and the two sessions are ONE family row: the root represents it, the
    //    ⑂ chip counts the fork, and no fork row is open yet.
    until(
        &tab,
        "document.querySelectorAll('.row[data-id]').length >= 1",
        "the rail's first render",
    );
    let first = probe(&tab, "(function(){ var rows=[...document.querySelectorAll('.row')]; var chip=document.querySelector('.row button.forks'); return {rows: rows.length, rep: rows[0] && rows[0].dataset.id, chip: chip ? chip.textContent.trim() : null, forkrows: document.querySelectorAll('.row.forkrow').length}; })()");
    assert_eq!(first["rows"], 1, "one row for the family: {first}");
    assert_eq!(
        first["rep"], root_id,
        "the root represents the family: {first}"
    );
    assert_eq!(first["chip"], "⑂ 1", "the chip counts one fork: {first}");
    assert_eq!(first["forkrows"], 0, "no fork row open: {first}");

    // 1b. Seam (f) (#44): the rail's tooltip opens with the same state label the app shell's
    //     info pane shows for the same row — one table, both shells, one server.
    let tip = eval(
        &tab,
        &format!(
            "(document.querySelector('{}') || {{}}).title || ''",
            row(root_id)
        ),
    );
    let tip = tip.as_str().unwrap_or("").to_string();
    tab.navigate_to(&format!("http://127.0.0.1:2837/?ui=app&session={root_id}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    until(&tab, "!!document.getElementById('statusChip') && !!document.getElementById('statusChip').textContent.trim()", "the app shell's status chip");
    // The chip carries a dot and, when the state was derived rather than stated, a "· inferred"
    // suffix. The LABEL is the part the rail's tooltip is supposed to agree with.
    let status = eval(
        &tab,
        "document.getElementById('statusChip').textContent.split('·')[0].trim()",
    );
    let status = status.as_str().unwrap_or("").to_string();
    assert!(
        !status.is_empty() && tip.starts_with(&format!("{status} — ")),
        "one state table: rail tooltip {tip:?} vs app-shell status {status:?}"
    );
    tab.navigate_to("http://127.0.0.1:2837/?ui=classic")
        .unwrap();
    tab.wait_until_navigated().unwrap();
    until(
        &tab,
        "document.querySelectorAll('.row[data-id]').length >= 1",
        "the rail again",
    );

    // 2. The chip opens the family: the fork appears as an indented member row.
    eval(
        &tab,
        "document.querySelector('.row button.forks').click(); 'ok'",
    );
    until(
        &tab,
        "document.querySelectorAll('.row.forkrow').length === 1",
        "the fork row to open",
    );
    let open = probe(&tab, "(function(){ var f=document.querySelector('.row.forkrow'); return {id: f && f.dataset.id, rows: document.querySelectorAll('.row').length}; })()");
    assert_eq!(open["id"], fork_id, "the member row is the fork: {open}");
    assert_eq!(open["rows"], 2, "root + fork: {open}");

    // 3. Hide the root: the server records it (/api/ignore), the row leaves the list, and the
    //    fork — the family's only visible member — now represents it, without a chip.
    eval(
        &tab,
        &format!(
            "document.querySelector('{} button.rowx[data-hide]').click(); 'ok'",
            row(root_id)
        ),
    );
    until(
        &tab,
        &format!(
            "!document.querySelector('{}') && !!document.querySelector('{}')",
            row(root_id),
            row(fork_id)
        ),
        "the root to hide and the fork to represent",
    );
    assert_eq!(
        server_hidden(&tab, root_id),
        Some(true),
        "the server hid the root"
    );
    assert_eq!(
        eval(
            &tab,
            "document.querySelectorAll('.row button.forks').length"
        ),
        0,
        "a family of one visible member has no chip"
    );
    let toggle = probe(&tab, "(function(){ var t=document.getElementById('hiddentoggle'); return {shown: getComputedStyle(t).display !== 'none', text: t.textContent.trim()}; })()");
    assert_eq!(
        toggle["shown"], true,
        "the reveal toggle appears once something is hidden: {toggle}"
    );
    assert_eq!(toggle["text"], "Hidden (1)", "{toggle}");

    // 4. Reveal: the hidden root comes back dimmed, with a restore control.
    eval(
        &tab,
        "document.getElementById('hiddentoggle').click(); 'ok'",
    );
    until(
        &tab,
        &format!(
            "!!document.querySelector('{}.hidden button.rowx[data-show]')",
            row(root_id)
        ),
        "the hidden root to show with a restore control",
    );

    // 5. Restore: the server forgets the key, the root is plain again and represents the family.
    eval(
        &tab,
        &format!(
            "document.querySelector('{} button.rowx[data-show]').click(); 'ok'",
            row(root_id)
        ),
    );
    until(&tab, &format!("(function(){{ var r=document.querySelector('{}'); return !!r && !r.classList.contains('hidden') && !!r.querySelector('button.forks'); }})()", row(root_id)), "the root to be restored with its chip");
    assert_eq!(
        server_hidden(&tab, root_id),
        Some(false),
        "the server restored the root"
    );
    drop(monitor);
}

/// The classic page after #45: `]` still steps to the next turn, `w` still toggles wrapping —
/// resolved through the shared key table — and the preferences persist under the one key the
/// app shell uses (`am-prod-reading`), with a pre-#45 size folded in once.
#[test]
#[ignore]
fn the_classic_page_keys_resolve_through_the_shared_table() {
    let _serial = serial();
    let base = base("classic-keys");
    std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
    let src = base.join("keys.jsonl");
    {
        let mut s = String::new();
        for i in 0..40u32 {
            s.push_str(&user(
                &format!("question {i}: {}", "lorem ipsum dolor sit amet. ".repeat(6)),
                i % 60,
            ));
            s.push_str(&assistant(
                &format!("answer {i}: {}", "sed do eiusmod tempor. ".repeat(12)),
                i % 60,
            ));
        }
        std::fs::write(&src, s).unwrap();
    }
    let args = Args {
        no_cache: true,
        ..Default::default()
    };
    let server = start_server(&args, std::slice::from_ref(&src)).expect("server starts");
    let url = server.url_for_root(0).expect("hosted");
    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .window_size(Some((1200, 800)))
            .build()
            .unwrap(),
    )
    .expect("chrome launches");
    let tab = browser.new_tab().unwrap();
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    // A pre-#45 reader: a stored size and wrapping turned off, under the classic page's old keys.
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    eval(&tab, "localStorage.clear(); localStorage.setItem('claude-replay-export-ms', '14'); localStorage.setItem('claude-replay-export-wrap', '0'); 'ok'");
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    for _ in 0..40 {
        if eval(&tab, "document.querySelectorAll('.turn').length >= 10").as_bool() == Some(true) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let migrated = eval(&tab, "JSON.stringify({ms: getComputedStyle(document.documentElement).getPropertyValue('--code-size').trim(), key: localStorage.getItem('am-prod-reading'), old: localStorage.getItem('claude-replay-export-ms')})");
    let migrated: serde_json::Value =
        serde_json::from_str(migrated.as_str().unwrap_or("null")).unwrap_or_default();
    assert_eq!(
        migrated["ms"], "14px",
        "the pre-#45 size is applied: {migrated}"
    );
    assert_eq!(
        migrated["old"],
        serde_json::Value::Null,
        "the legacy key is gone after the one-time fold: {migrated}"
    );
    let prefs: serde_json::Value =
        serde_json::from_str(migrated["key"].as_str().unwrap_or("null")).unwrap_or_default();
    assert_eq!(
        prefs["size"], 14.0,
        "…and lives under the one key: {migrated}"
    );
    assert_eq!(prefs["wrap"], false, "{migrated}");
    // `]` steps to a later turn; `w` toggles wrapping and persists it.
    let key = |k: &str| {
        format!("document.dispatchEvent(new KeyboardEvent('keydown', {{key: {k:?}, bubbles: true, cancelable: true}})); 'ok'")
    };
    // A served page opens at its tail (it always follows), so start from the top, where a
    // later turn exists for `]` to reach.
    // A programmatic scroll reads as the renderer's own and the follow logic re-pins the tail;
    // a wheel event first is the reader's intent, which unpins (the contract every classic
    // case relies on).
    eval(&tab, "window.dispatchEvent(new WheelEvent('wheel', {deltaY: -120})); window.scrollTo(0, 0); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let before = eval(&tab, "window.scrollY").as_f64().unwrap_or(0.0);
    eval(&tab, &key("]"));
    std::thread::sleep(std::time::Duration::from_millis(600));
    eval(&tab, &key("]"));
    std::thread::sleep(std::time::Duration::from_millis(600));
    let after = eval(&tab, "window.scrollY").as_f64().unwrap_or(0.0);
    assert!(
        after > before,
        "`]` moved down the page: {before} -> {after}"
    );
    eval(&tab, &key("w"));
    std::thread::sleep(std::time::Duration::from_millis(200));
    let wrapped: serde_json::Value = serde_json::from_str(
        eval(&tab, "localStorage.getItem('am-prod-reading')")
            .as_str()
            .unwrap_or("null"),
    )
    .unwrap_or_default();
    assert_eq!(
        wrapped["wrap"], true,
        "`w` turned wrapping on and persisted it under the one key: {wrapped}"
    );
    drop(server);
}

/// The classic rail's compose bar after #48: paired, a finished Claude session offers ✎; the
/// bar opens with the shared protocol's words ("Send to: …", "Send prompt"), and a send runs
/// the shared flow — against a stubbed /api/send, so nothing is resumed — and reports the
/// shared outcome ("sent — the session is resuming").
#[test]
#[ignore]
fn the_classic_rail_composes_through_the_shared_protocol() {
    let _serial = serial();
    let base = base("rail-compose");
    let stores = Stores::new(&base);
    let sid = stores.claude_finished();
    let monitor = Monitor::spawn(Kind::V1, 2838, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    let eval = |tab: &headless_chrome::Tab, js: &str| -> serde_json::Value {
        tab.evaluate(js, true)
            .ok()
            .and_then(|r| r.value)
            .unwrap_or(serde_json::Value::Null)
    };
    let until = |tab: &headless_chrome::Tab, js: &str, what: &str| {
        for _ in 0..40 {
            if eval(tab, js).as_bool() == Some(true) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let seen = eval(tab, "(document.getElementById('composemsg') || {}).textContent + ' | ' + [...document.querySelectorAll('.row')].map(r => r.dataset.id + ':' + r.innerHTML.length).join(' ')");
        panic!("timed out waiting for {what}; seen: {seen}");
    };
    monitor.pair(&tab);
    tab.navigate_to("http://127.0.0.1:2838/?ui=classic")
        .unwrap();
    tab.wait_until_navigated().unwrap();
    let row = format!(".row[data-id=\"{sid}\"]");
    until(
        &tab,
        &format!("!!document.querySelector('{row} button.rowsend[data-compose]')"),
        "the finished session's ✎ (paired, resumable)",
    );
    // No real send: /api/send is answered in the page, so the flow's outcome is what is measured.
    eval(&tab, "window.__sent = []; const orig = window.fetch.bind(window); window.fetch = (u, o) => { const url = String(u); if (url.startsWith('/api/send')) { window.__sent.push([url, o && o.body]); return Promise.resolve(new Response('{\"ok\":true}', { status: 200, headers: { 'Content-Type': 'application/json' } })); } return orig(u, o); }; 'ok'");
    eval(
        &tab,
        &format!("document.querySelector('{row} button.rowsend').click(); 'ok'"),
    );
    until(
        &tab,
        "!document.getElementById('composebar').hidden",
        "the compose bar to open",
    );
    let words = eval(&tab, "JSON.stringify({to: document.getElementById('composeto').textContent, button: document.getElementById('composesend').textContent, notice: document.getElementById('composemsg').textContent, placeholder: document.getElementById('composetext').placeholder})");
    let words: serde_json::Value =
        serde_json::from_str(words.as_str().unwrap_or("null")).unwrap_or_default();
    assert!(
        words["to"].as_str().unwrap_or("").starts_with("Send to: "),
        "the shared words: {words}"
    );
    assert_eq!(words["button"], "Send prompt", "{words}");
    assert_eq!(
        words["notice"], "",
        "a resume carries no consent notice: {words}"
    );
    assert!(
        words["placeholder"]
            .as_str()
            .unwrap_or("")
            .contains("resumes"),
        "{words}"
    );
    eval(&tab, "document.getElementById('composetext').value = 'carry on'; document.getElementById('composesend').click(); 'ok'");
    until(
        &tab,
        "document.getElementById('composemsg').textContent === 'sent — the session is resuming'",
        "the shared outcome",
    );
    let sent = eval(&tab, "JSON.stringify(window.__sent)");
    assert_eq!(
        sent.as_str().unwrap_or(""),
        format!("[[\"/api/send?target={sid}\",\"carry on\"]]"),
        "one send, the shared query, the prompt as the body"
    );
    drop(monitor);
}

/// #66: the classic page's file preview (the lightbox) offers "Reveal in file manager" in its
/// caption; the request it sends must carry the path's reveal STAMP, or the server refuses it
/// by design. `fetch` is stubbed in the page: `file?` answers with text so the preview opens
/// without touching the disk, `__reveal` records what was asked and answers ok, and everything
/// else (the feed) passes through.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_classic_lightbox_reveal_carries_the_stamp() {
    let _serial = serial();
    let base = base("classic-lightbox-reveal");
    let stores = Stores::new(&base);
    let note = base.join("note.txt");
    std::fs::write(&note, "a note the page may preview\n").unwrap();
    let note_s = note.to_string_lossy().to_string();
    let sid = "cccccccc-0000-4000-8000-000000000066";
    let mut jsonl = String::new();
    jsonl.push_str(&harness::user_at(
        "question 0: read the note",
        &harness::now_minus(60),
    ));
    jsonl.push_str(&harness::read_tool_at(
        "t-read",
        &note_s,
        &harness::now_minus(55),
    ));
    jsonl.push_str(&harness::tool_result_at("t-read", &harness::now_minus(50)));
    jsonl.push_str(&harness::assistant_at(
        "answer 0: read it",
        &harness::now_minus(45),
    ));
    stores.claude_session(sid, &jsonl);
    let monitor = Monitor::spawn(Kind::V2, 2839, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=classic&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('a.tool-path')",
        "the offered path to render",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 200)",
    );
    let offered = harness::probe(&tab, "(function(){ var a = document.querySelector('a.tool-path'); return {path: a.dataset.path, sig: !!a.dataset.sig, fsig: !!a.dataset.fsig, artifacts: document.body.dataset.artifacts, title: a.title}; })()");
    assert_eq!(
        offered["sig"], true,
        "the path was offered with a reveal stamp: {offered}"
    );
    assert_eq!(
        offered["fsig"], true,
        "…and a file stamp (default render policy): {offered}"
    );
    assert_eq!(
        offered["artifacts"], "1",
        "the served page offers artifacts: {offered}"
    );
    harness::eval(&tab, "window.__reqs = []; var real = window.fetch; window.fetch = function (u, o) { var s = String(u); window.__reqs.push(s); if (/^file\\?/.test(s)) return Promise.resolve(new Response('a note', {status: 200, headers: {'content-type': 'text/plain'}})); if (/__reveal\\?/.test(s)) return Promise.resolve(new Response('', {status: 200})); return real(u, o); }; 'ok'");
    harness::eval(&tab, "document.querySelector('a.tool-path').click(); 'ok'");
    harness::until(
        &tab,
        "!!document.querySelector('.lightbox .lb-act')",
        "the preview to open",
        std::time::Duration::from_secs(15),
        "JSON.stringify(window.__reqs)",
    );
    harness::eval(
        &tab,
        "document.querySelector('.lightbox .lb-act').click(); 'ok'",
    );
    harness::until(
        &tab,
        "window.__reqs.some(function (s) { return /__reveal\\?/.test(s); })",
        "the caption's reveal request",
        std::time::Duration::from_secs(10),
        "JSON.stringify(window.__reqs)",
    );
    let reveal = harness::eval(
        &tab,
        "window.__reqs.filter(function (s) { return /__reveal\\?/.test(s); }).pop()",
    );
    let reveal = reveal.as_str().unwrap_or("").to_string();
    let sig = reveal.split("sig=").nth(1).unwrap_or("").to_string();
    assert!(
        !sig.is_empty(),
        "the caption's reveal carries the path's stamp, got {reveal:?}"
    );
    assert!(
        reveal.contains(&format!("path={}", urlencoding(&note_s))),
        "…for the previewed path, got {reveal:?}"
    );
    harness::until(
        &tab,
        "document.querySelector('.lightbox .lb-act').textContent === 'revealed ✓'",
        "the caption to report the outcome",
        std::time::Duration::from_secs(5),
        "document.querySelector('.lightbox .lb-act').textContent",
    );
    drop(monitor);
}

/// `encodeURIComponent` for a path, as the page writes it into a query.
fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// #54: the sidebar collapses into a 64px rail of icons — expand, write, search, attention and
/// one button per agent — the layout stays one scroller under fixed chrome, the choice
/// survives a reload, and the rail expands back by key and by click.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_collapses_the_sidebar_into_a_rail() {
    let _serial = serial();
    let base = base("appshell-rail");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000054".to_string();
    stores.claude_session(&sid, &harness::long_session(30, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2843, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let url = format!("http://127.0.0.1:2843/?ui=app&session={sid}");
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    let mounted = "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0";
    harness::until(
        &tab,
        mounted,
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    let collapsed = "document.getElementById('app').classList.contains('sidebar-off')";
    assert_eq!(
        harness::eval(&tab, collapsed),
        false,
        "a fresh shell opens with the sidebar expanded"
    );
    // #76 / #96: the control is there to be SEEN — a glyph, a size, a name, inside the sidebar
    // it belongs to, and answering a hit test at its own centre. A rect alone says nothing: the
    // head clips what overflows it, and for four releases this button sat 91px past the clip,
    // rect and all, which is why the owner went looking for it and found nothing (#96).
    let seen = "(function(){ var b = document.getElementById('sidebarCollapse'); var side = document.querySelector('.sidebar'); var r = b.getBoundingClientRect(), sr = side.getBoundingClientRect(); var hit = document.elementFromPoint((r.left + r.right) / 2, (r.top + r.bottom) / 2); return { visible: b.offsetParent !== null && r.width >= 24 && r.height >= 24, inside: r.left >= sr.left - 0.5 && r.right <= sr.right + 0.5 && r.top >= sr.top - 0.5, hits: !!hit && (hit === b || b.contains(hit)), glyph: !!b.querySelector('svg'), label: b.getAttribute('aria-label') || '', width: Math.round(sr.width) }; })()";
    for narrow in [false, true] {
        if narrow {
            // The owner's own window is not the widest one: the control has to survive there too.
            tab.set_bounds(headless_chrome::types::Bounds::Normal {
                left: None,
                top: None,
                width: Some(1000.0),
                height: Some(800.0),
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let control = harness::probe(&tab, seen);
        assert_eq!(
            control["visible"], true,
            "the sidebar collapse control is visible (narrow={narrow}): {control}"
        );
        assert_eq!(
            control["inside"], true,
            "…inside the sidebar, not clipped past its edge (narrow={narrow}): {control}"
        );
        assert_eq!(
            control["hits"], true,
            "…and reachable by a click at its own centre (narrow={narrow}): {control}"
        );
        assert_eq!(control["glyph"], true, "…with a glyph: {control}");
        assert!(
            control["label"]
                .as_str()
                .unwrap_or("")
                .contains("Collapse the session list"),
            "…and a name: {control}"
        );
    }
    harness::eval(
        &tab,
        "document.getElementById('sidebarCollapse').click(); 'ok'",
    );
    harness::until(
        &tab,
        collapsed,
        "the sidebar to collapse",
        std::time::Duration::from_secs(5),
        "document.getElementById('app').className",
    );
    // The grid animates its columns (.2s); measure once the rail has settled.
    let settled = |w: &str| {
        format!("Math.round(document.querySelector('.sidebar').getBoundingClientRect().width) {w}")
    };
    harness::until(
        &tab,
        &settled("=== 64"),
        "the rail to settle at 64px",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    let rail = harness::probe(&tab, "(function(){ var side = document.querySelector('.sidebar'), mini = document.getElementById('sidebarMini'); var buttons = [...mini.querySelectorAll('button')].filter(function (b) { return !b.hidden && b.offsetParent !== null; }); return { width: Math.round(side.getBoundingClientRect().width), display: getComputedStyle(mini).display, buttons: buttons.length, ids: buttons.map(function (b) { return b.id || b.className; }), agents: mini.querySelectorAll('.sidebar-mini-agent').length, titles: buttons.map(function (b) { return b.title || b.getAttribute('aria-label') || ''; }) }; })()");
    assert_eq!(rail["width"], 64, "the rail is 64px wide: {rail}");
    assert_eq!(rail["display"], "flex", "the rail shows: {rail}");
    assert!(
        rail["buttons"].as_u64().unwrap_or(0) >= 5,
        "expand, write, search, attention and one agent: {rail}"
    );
    assert_eq!(
        rail["agents"], 1,
        "one agent button for the one agent with sessions: {rail}"
    );
    assert!(
        rail["titles"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| !t.as_str().unwrap_or("").is_empty()),
        "every rail button names itself: {rail}"
    );
    // One scroller under fixed chrome, collapsed as well as expanded.
    let probe = r#"(function () {
      var s = document.querySelector('.transcript'), side = document.querySelector('.sidebar'), head = document.querySelector('header');
      var before = { side: side.getBoundingClientRect().top, head: head.getBoundingClientRect().top, doc: window.scrollY };
      s.scrollTop = Math.max(0, s.scrollTop - 600);
      var after = { side: side.getBoundingClientRect().top, head: head.getBoundingClientRect().top, doc: window.scrollY };
      return { documentStill: before.doc === 0 && after.doc === 0, sideFixed: Math.abs(after.side - before.side) < 1, headFixed: Math.abs(after.head - before.head) < 1 };
    })()"#;
    let v = harness::probe(&tab, probe);
    assert_eq!(
        v["documentStill"], true,
        "the document does not scroll: {v}"
    );
    assert_eq!(v["sideFixed"], true, "the rail stays put: {v}");
    assert_eq!(v["headFixed"], true, "the header stays put: {v}");
    // The choice survives a reload.
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        mounted,
        "the reloaded shell to mount",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    assert_eq!(
        harness::eval(&tab, collapsed),
        true,
        "the rail is remembered across a reload"
    );
    harness::until(
        &tab,
        &settled("=== 64"),
        "the remembered rail at its width",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    // The key expands it — and collapses it again — from the view.
    let press = r#"document.body.focus(); document.dispatchEvent(new KeyboardEvent('keydown', { key: '\\', bubbles: true, cancelable: true })); 'ok'"#;
    harness::eval(&tab, press);
    harness::until(
        &tab,
        &format!("!({collapsed})"),
        "the key to expand the sidebar",
        std::time::Duration::from_secs(5),
        "document.getElementById('app').className",
    );
    harness::eval(&tab, press);
    harness::until(
        &tab,
        collapsed,
        "the key to collapse it again",
        std::time::Duration::from_secs(5),
        "document.getElementById('app').className",
    );
    // The rail's own expand button.
    harness::eval(
        &tab,
        "document.getElementById('sidebarMiniExpand').click(); 'ok'",
    );
    harness::until(
        &tab,
        &format!("!({collapsed})"),
        "the rail's expand button",
        std::time::Duration::from_secs(5),
        "document.getElementById('app').className",
    );
    harness::until(
        &tab,
        &settled("> 200"),
        "the sidebar back at its width",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    // #91: the attention filter reads as PRESSED — it takes the amber it filters for, where the
    // shell's own `.navbtn.on` is the faint hover tint every row shares.
    let attention = |t: &headless_chrome::Tab| {
        harness::probe(t, "(function(){ var b = document.getElementById('attentionBtn'); var c = b.querySelector('.attention-count'); var m = document.getElementById('sidebarMiniAttention'); return { bg: getComputedStyle(b).backgroundColor, shadow: getComputedStyle(b).boxShadow, count: getComputedStyle(c).backgroundColor, mini: getComputedStyle(m).backgroundColor, pressed: b.getAttribute('aria-pressed') }; })()")
    };
    let off = attention(&tab);
    harness::eval(
        &tab,
        "document.getElementById('attentionBtn').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let on = attention(&tab);
    assert_eq!(on["pressed"], "true", "the filter is pressed: {on}");
    assert_ne!(
        on["bg"], off["bg"],
        "…and says so with a fill of its own: {off} -> {on}"
    );
    assert_ne!(on["shadow"], off["shadow"], "…and a border: {on}");
    assert_ne!(
        on["count"], off["count"],
        "…with the count inverted onto it: {on}"
    );
    assert_ne!(on["mini"], off["mini"], "…and the rail's button too: {on}");
    drop(monitor);
}

/// #55/#148: the outline pane has TWO states — open, and collapsed to its icon rail — and the
/// vocabulary that walks them is complete without a third.
///
/// This case guarded #55's HIDDEN state: the pane gone outright, no rail, the transcript taking
/// the whole width. The owner asked for that state back out, so the case is rewritten rather than
/// deleted — what it protected is still worth protecting, and most of it never depended on the
/// third state at all: the reader's view is held through the reflow, the choice survives a
/// reload, and the key works from anywhere. Those move onto the open↔rail transition, which is
/// where they now live.
///
/// What it asserts about the retirement is that both directions stay reachable BY CLICKING, at a
/// narrow window as well as a wide one. That is the fact the whole decision rested on, and it was
/// measured rather than read: below 900px the demo's stylesheet turns the OPEN navigator into an
/// absolute overlay, which is what made the top-bar button look load-bearing — but the COLLAPSED
/// navigator is a sticky 56px column there (`.workspace.navigator-off .session-navigator
/// {position:sticky;width:56px}`), so the rail and its expand button are on screen at every
/// width. Reading the CSS said the opposite; hit-testing said this.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_walks_the_outline_between_its_two_states() {
    let _serial = serial();
    let base = base("appshell-two-states");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000055".to_string();
    stores.claude_session(&sid, &harness::long_session(60, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2844, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let url = format!("http://127.0.0.1:2844/?ui=app&session={sid}");
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    let mounted = "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3";
    harness::until(
        &tab,
        mounted,
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );

    // The retired state leaves nothing behind: no class, no control, no stored preference.
    let retired = harness::probe(
        &tab,
        r#"(function(){ return { klass: document.querySelector('.workspace').classList.contains('navigator-hidden'), railX: !!document.getElementById('navigatorRailHide'), topToggle: !!document.getElementById('navigatorToggle'), stored: localStorage.getItem('am-prod-navigator-hidden') }; })()"#,
    );
    assert_eq!(
        retired["klass"], false,
        "#148: nothing carries the retired hidden class: {retired}"
    );
    assert_eq!(retired["railX"], false, "…the rail's X is gone: {retired}");
    assert_eq!(
        retired["topToggle"], false,
        "…and so is the top-bar toggle whose only remaining job was undoing it: {retired}"
    );
    assert!(
        retired["stored"].is_null(),
        "…and nothing is remembered for it: {retired}"
    );

    let widths = "(function(){ var t = document.querySelector('.transcript').getBoundingClientRect(), m = document.querySelector('.session-main').getBoundingClientRect(), n = document.querySelector('.session-navigator'); var nr = n.getBoundingClientRect(); return { transcript: Math.round(t.width), main: Math.round(m.width), navigator: getComputedStyle(n).display === 'none' ? 0 : Math.round(nr.width) }; })()";
    let open = harness::probe(&tab, widths);
    assert!(
        open["navigator"].as_f64().unwrap_or(0.0) > 150.0,
        "a fresh shell opens with the pane at its width: {open}"
    );

    // Read from the middle of the session, so the anchor is a real unit and not the tail.
    harness::scroll_by(&tab, harness::Surface::AppShell, -2400);
    std::thread::sleep(std::time::Duration::from_millis(600));
    let anchor_before = harness::view_anchor(&tab, harness::Surface::AppShell);

    // The key collapses to the rail — from anywhere, no control focused.
    let off = "document.querySelector('.workspace').classList.contains('navigator-off')";
    let press = "document.body.focus(); document.dispatchEvent(new KeyboardEvent('keydown', { key: 'o', bubbles: true, cancelable: true })); 'ok'";
    harness::eval(&tab, press);
    harness::until(
        &tab,
        off,
        "the key to collapse the pane to its rail",
        std::time::Duration::from_secs(5),
        "document.querySelector('.workspace').className",
    );
    let railed = harness::probe(&tab, widths);
    assert!(
        railed["transcript"].as_f64().unwrap() > open["transcript"].as_f64().unwrap() + 100.0,
        "the transcript takes the width the pane gave up: {open} → {railed}"
    );

    // The reader's place survives the reflow — the half of #55 that was never about hiding.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let anchor_after = harness::view_anchor(&tab, harness::Surface::AppShell);
    assert_eq!(
        anchor_after.0, anchor_before.0,
        "the unit at the top of the view is the same one through the reflow"
    );
    assert!(
        (anchor_after.1 - anchor_before.1).abs() <= 24.0,
        "…at its place (±24px): {} → {}",
        anchor_before.1,
        anchor_after.1
    );

    // The choice survives a reload.
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        mounted,
        "the reloaded shell to mount",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    assert_eq!(
        harness::eval(&tab, off),
        true,
        "the collapsed pane is remembered across a reload"
    );

    // Both directions, by CLICK, at a wide window and a narrow one — the fact the decision to
    // retire the third state rested on. Hit-tested: a control that cannot be reached still
    // reports a perfect rectangle.
    let hittable = r#"function (el) { if (!el) return false; var r = el.getBoundingClientRect(); if (r.width < 4 || r.height < 4) return false; var hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return !!(hit && (hit === el || el.contains(hit) || hit.contains(el))); }"#;
    for (label, w) in [("wide", 1500.0), ("narrow", 820.0)] {
        harness::resize(&tab, w, 900.0);
        std::thread::sleep(std::time::Duration::from_millis(700));

        // From the rail: the expand button is there and clickable.
        let expand = harness::probe(
            &tab,
            &format!("(function(){{ var hittable = {hittable}; var e = document.getElementById('navigatorRailExpand'); return {{ off: {off}, reachable: hittable(e) }}; }})()"),
        );
        assert_eq!(
            expand["off"], true,
            "at the {label} window the pane is collapsed to start this leg: {expand}"
        );
        assert_eq!(
            expand["reachable"], true,
            "at the {label} window the rail's expand button is genuinely clickable — this is what              makes the top-bar toggle unnecessary rather than merely redundant: {expand}"
        );
        harness::eval(
            &tab,
            "document.getElementById('navigatorRailExpand').click(); 'ok'",
        );
        harness::until(
            &tab,
            &format!("!({off})"),
            "the rail's button to open the pane",
            std::time::Duration::from_secs(5),
            "document.querySelector('.workspace').className",
        );

        // …and back, from the caption.
        let close = harness::probe(
            &tab,
            &format!("(function(){{ var hittable = {hittable}; return {{ reachable: hittable(document.getElementById('navigatorClose')) }}; }})()"),
        );
        assert_eq!(
            close["reachable"], true,
            "at the {label} window the caption's collapse button is clickable too: {close}"
        );
        harness::eval(
            &tab,
            "document.getElementById('navigatorClose').click(); 'ok'",
        );
        harness::until(
            &tab,
            off,
            "the caption's button to collapse the pane",
            std::time::Duration::from_secs(5),
            "document.querySelector('.workspace').className",
        );
    }
    drop(monitor);
}

/// #56: the tasks pane orders a session's tasks by group — completed, running, pending — and by
/// id within a group, with a boundary between groups, whatever order the store filed them in.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_orders_tasks_by_group_then_id() {
    let _serial = serial();
    let base = base("appshell-task-order");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000056".to_string();
    stores.claude_session(&sid, &harness::long_session(12, harness::Shape::default()));
    stores.claude_tasks(
        &sid,
        &[
            ("10", "ten, pending", "pending"),
            ("2", "two, done", "completed"),
            ("u4", "user four, pending", "pending"),
            ("7", "seven, running", "in_progress"),
            ("1", "one, done", "completed"),
            ("3", "three, pending", "pending"),
        ],
    );
    let monitor = Monitor::spawn(Kind::V2, 2845, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2845/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the app shell to mount the fixture", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    // The tasks card is closed by default; open it.
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'");
    // #186: this case is about the whole board, which is what the live-only filter hides.
    harness::show_every_pane_row(&tab);
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorWork .work-task').length === 6",
        "the six tasks to render",
        std::time::Duration::from_secs(20),
        "document.getElementById('navigatorWork').innerText.slice(0, 200)",
    );
    let seen = harness::probe(&tab, "(function(){ var rows = [...document.querySelectorAll('#navigatorWork > *')]; return { order: rows.map(function (r) { return r.classList.contains('work-group') ? r.dataset.taskGroup : (r.querySelector('.work-tail') || {}).textContent; }), groups: [...document.querySelectorAll('#navigatorWork .work-group')].map(function (g) { return g.textContent.trim(); }) }; })()");
    assert_eq!(
        seen["order"],
        serde_json::json!([
            "completed",
            "#1",
            "#2",
            "in_progress",
            "#7",
            "pending",
            "#3",
            "#10",
            "#u4"
        ]),
        "groups in order, ids as numbers within a group, the user tier after: {seen}"
    );
    assert_eq!(
        seen["groups"].as_array().map(|g| g.len()),
        Some(3),
        "three visible boundaries: {seen}"
    );
    drop(monitor);
}

/// The pane geometry probe shared by the two scroller cases: the list's own overflow, whether
/// the current row sits inside the list's viewport, and that neither the window nor the
/// transcript moved.
const PANE_PROBE: &str = r#"(function (listId) {
  var list = document.getElementById(listId), lr = list.getBoundingClientRect();
  var cur = list.querySelector('.current') || list.querySelector('[aria-current]');
  var cr = cur ? cur.getBoundingClientRect() : null;
  return { overflow: list.scrollHeight - list.clientHeight, top: list.scrollTop, height: Math.round(lr.height),
           currentInView: cr ? (cr.top >= lr.top - 1 && cr.bottom <= lr.bottom + 1) : null,
           windowY: window.scrollY, transcriptTop: document.querySelector('.transcript').scrollTop };
})"#;

/// #58: with many turns, the turns pane scrolls itself — the list overflows inside the fixed
/// chrome, stepping keeps the focused turn inside the pane's own viewport, and the window and
/// the document never scroll.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_turns_pane_scrolls_itself() {
    let _serial = serial();
    let base = base("appshell-turns-scroller");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000058".to_string();
    stores.claude_session(&sid, &harness::long_session(160, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2848, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2848/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length >= 160",
        "the turns pane to list the session",
        std::time::Duration::from_secs(60),
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
    );
    let probe = |tab: &headless_chrome::Tab| {
        harness::probe(tab, &format!("({PANE_PROBE})('navigatorTurns')"))
    };
    let start = probe(&tab);
    assert!(
        start["overflow"].as_f64().unwrap_or(0.0) > 400.0,
        "the turns list overflows its own bounded scroller: {start}"
    );
    assert!(
        start["height"].as_f64().unwrap_or(0.0) > 96.0,
        "…which has real height inside the pane: {start}"
    );
    harness::jump_to_end(&tab, harness::Surface::AppShell);
    std::thread::sleep(std::time::Duration::from_millis(800));
    let at_end = probe(&tab);
    assert_eq!(
        at_end["currentInView"], true,
        "at the tail, the current (last) turn is inside the pane's viewport: {at_end}"
    );
    assert!(
        at_end["top"].as_f64().unwrap_or(0.0) > 300.0,
        "…because the PANE scrolled, not the window: {at_end}"
    );
    assert_eq!(at_end["windowY"], 0, "the window never scrolls: {at_end}");
    // Step back through turns with the key: the focus stays in the pane's view.
    for _ in 0..6 {
        harness::eval(&tab, "document.body.focus(); document.dispatchEvent(new KeyboardEvent('keydown', { key: '[', bubbles: true, cancelable: true })); 'ok'");
        std::thread::sleep(std::time::Duration::from_millis(350));
    }
    let stepped = probe(&tab);
    assert_eq!(
        stepped["currentInView"], true,
        "after stepping, the focused turn is still inside the pane's viewport: {stepped}"
    );
    assert_eq!(
        stepped["windowY"], 0,
        "the window still never scrolls: {stepped}"
    );
    drop(monitor);
}

/// #59: with many tasks, the tasks pane scrolls itself; scrolling it moves neither the window nor
/// the transcript.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_tasks_pane_scrolls_itself() {
    let _serial = serial();
    let base = base("appshell-tasks-scroller");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000059".to_string();
    stores.claude_session(&sid, &harness::long_session(12, harness::Shape::default()));
    let mut tasks: Vec<(String, String, &str)> = Vec::new();
    for i in 1..=40 {
        tasks.push((
            format!("{i}"),
            format!("task number {i} with a subject long enough to wrap onto a second line"),
            if i <= 20 {
                "completed"
            } else if i <= 23 {
                "in_progress"
            } else {
                "pending"
            },
        ));
    }
    let borrowed: Vec<(&str, &str, &str)> = tasks
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), *c))
        .collect();
    stores.claude_tasks(&sid, &borrowed);
    let monitor = Monitor::spawn(Kind::V2, 2849, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2849/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the app shell to mount the fixture", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'");
    // #186: this case is about the whole board, which is what the live-only filter hides.
    harness::show_every_pane_row(&tab);
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorWork .work-task').length === 40",
        "the tasks to render",
        std::time::Duration::from_secs(20),
        "document.querySelectorAll('#navigatorWork .work-task').length",
    );
    let probe = |tab: &headless_chrome::Tab| {
        harness::probe(tab, &format!("({PANE_PROBE})('navigatorWork')"))
    };
    let start = probe(&tab);
    assert!(
        start["overflow"].as_f64().unwrap_or(0.0) > 300.0,
        "the tasks list overflows its own bounded scroller: {start}"
    );
    assert!(
        start["height"].as_f64().unwrap_or(0.0) > 96.0,
        "…which has real height inside the pane: {start}"
    );
    harness::eval(
        &tab,
        "document.getElementById('navigatorWork').scrollTop = 300; 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let scrolled = probe(&tab);
    assert!(
        scrolled["top"].as_f64().unwrap_or(0.0) >= 250.0,
        "the pane scrolled: {scrolled}"
    );
    assert_eq!(scrolled["windowY"], 0, "the window did not: {scrolled}");
    assert_eq!(
        scrolled["transcriptTop"], start["transcriptTop"],
        "…nor did the transcript: {scrolled}"
    );
    drop(monitor);
}

/// #57: the tasks pane's control scrolls the pane so the running tasks sit at its center — and,
/// with none running, the boundary between done and pending — without touching the transcript.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_centers_the_tasks_pane_on_the_running_tasks() {
    let _serial = serial();
    let base = base("appshell-task-center");
    let stores = Stores::new(&base);
    // Enough tasks to overflow the pane: 14 done, 3 running, 16 pending.
    let sid = "cccccccc-0000-4000-8000-000000000057".to_string();
    stores.claude_session(&sid, &harness::long_session(12, harness::Shape::default()));
    let mut tasks: Vec<(String, String, &str)> = Vec::new();
    for i in 1..=14 {
        tasks.push((format!("{i}"), format!("done task {i}"), "completed"));
    }
    for i in 15..=17 {
        tasks.push((format!("{i}"), format!("running task {i}"), "in_progress"));
    }
    for i in 18..=33 {
        tasks.push((format!("{i}"), format!("pending task {i}"), "pending"));
    }
    let borrowed: Vec<(&str, &str, &str)> = tasks
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), *c))
        .collect();
    stores.claude_tasks(&sid, &borrowed);
    let monitor = Monitor::spawn(Kind::V2, 2846, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2846/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the app shell to mount the fixture", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'");
    // #186: this case is about the whole board, which is what the live-only filter hides.
    harness::show_every_pane_row(&tab);
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorWork .work-task').length === 33",
        "the tasks to render",
        std::time::Duration::from_secs(20),
        "document.querySelectorAll('#navigatorWork .work-task').length",
    );
    let geometry = r#"(function(){
      var rows = [...document.querySelectorAll('#navigatorWork .work-task')];
      var running = rows.filter(function (r) { return r.querySelector('.task-state.running'); });
      var pane = null; for (var n = rows[0].parentElement; n; n = n.parentElement) { var o = getComputedStyle(n).overflowY; if ((o === 'auto' || o === 'scroll') && n.scrollHeight > n.clientHeight) { pane = n; break; } }
      if (!pane) return { pane: false };
      var p = pane.getBoundingClientRect(), mid = p.top + p.height / 2;
      var firstRun = running[0].getBoundingClientRect(), lastRun = running[running.length - 1].getBoundingClientRect();
      var boundary = rows[14].getBoundingClientRect().top;
      return { pane: true, overflow: pane.scrollHeight - pane.clientHeight, rowH: rows[0].getBoundingClientRect().height, runMid: (firstRun.top + lastRun.bottom) / 2 - mid, boundary: boundary - mid, transcriptTop: document.querySelector('.transcript').scrollTop, centered: (document.getElementById('tasksCenter') || {}).dataset ? document.getElementById('tasksCenter').dataset.centered : null };
    })()"#;
    let before = harness::probe(&tab, geometry);
    assert_eq!(
        before["pane"], true,
        "the tasks pane has its own scroller: {before}"
    );
    assert!(
        before["overflow"].as_f64().unwrap_or(0.0) > 200.0,
        "the fixture overflows the pane: {before}"
    );
    let transcript_before = before["transcriptTop"].clone();
    harness::eval(&tab, "document.getElementById('tasksCenter').click(); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let after = harness::probe(&tab, geometry);
    let row_h = after["rowH"].as_f64().unwrap_or(38.0);
    assert!(
        after["runMid"].as_f64().unwrap().abs() <= row_h,
        "the running rows' middle sits at the pane's center, within a row: {after}"
    );
    assert_eq!(
        after["centered"], "running",
        "…and the control says why: {after}"
    );
    assert_eq!(
        after["transcriptTop"], transcript_before,
        "the transcript did not move: {after}"
    );
    // The key does the same after the pane is scrolled away.
    harness::eval(&tab, "(function(){ var r = document.querySelectorAll('#navigatorWork .work-task')[0]; for (var n = r.parentElement; n; n = n.parentElement) { var o = getComputedStyle(n).overflowY; if ((o === 'auto' || o === 'scroll') && n.scrollHeight > n.clientHeight) { n.scrollTop = 0; break; } } })(); document.body.focus(); document.dispatchEvent(new KeyboardEvent('keydown', { key: 'c', bubbles: true, cancelable: true })); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let keyed = harness::probe(&tab, geometry);
    assert!(
        keyed["runMid"].as_f64().unwrap().abs() <= row_h,
        "the key centers the running rows too: {keyed}"
    );
    drop(monitor);
    // The boundary case: no running task — the first pending row's top edge is the center.
    let base2 = base.join("boundary");
    let stores2 = Stores::new(&base2);
    let sid2 = "cccccccc-0000-4000-8000-000000000058".to_string();
    stores2.claude_session(&sid2, &harness::long_session(12, harness::Shape::default()));
    let mut tasks2: Vec<(String, String, &str)> = Vec::new();
    for i in 1..=14 {
        tasks2.push((format!("{i}"), format!("done task {i}"), "completed"));
    }
    for i in 15..=33 {
        tasks2.push((format!("{i}"), format!("pending task {i}"), "pending"));
    }
    let borrowed2: Vec<(&str, &str, &str)> = tasks2
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), *c))
        .collect();
    stores2.claude_tasks(&sid2, &borrowed2);
    let monitor2 = Monitor::spawn(Kind::V2, 2847, &base2, Some(&stores2), true);
    let tab2 = browser.new_tab().unwrap();
    monitor2.pair(&tab2);
    tab2.navigate_to(&format!("http://127.0.0.1:2847/?ui=app&session={sid2}"))
        .unwrap();
    tab2.wait_until_navigated().unwrap();
    harness::until(&tab2, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the second shell to mount", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    harness::eval(&tab2, "var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'");
    // #186: this case is about the whole board, which is what the live-only filter hides.
    harness::show_every_pane_row(&tab2);
    harness::until(
        &tab2,
        "document.querySelectorAll('#navigatorWork .work-task').length === 33",
        "the tasks to render",
        std::time::Duration::from_secs(20),
        "document.querySelectorAll('#navigatorWork .work-task').length",
    );
    harness::eval(
        &tab2,
        "document.getElementById('tasksCenter').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let boundary = harness::probe(
        &tab2,
        geometry
            .replace(
                "running[0].getBoundingClientRect()",
                "rows[14].getBoundingClientRect()",
            )
            .replace(
                "running[running.length - 1].getBoundingClientRect()",
                "rows[14].getBoundingClientRect()",
            )
            .as_str(),
    );
    let row_h2 = boundary["rowH"].as_f64().unwrap_or(38.0);
    assert!(boundary["boundary"].as_f64().unwrap().abs() <= row_h2, "with nothing running, the done/pending boundary sits at the center, within a row: {boundary}");
    assert_eq!(
        boundary["centered"], "boundary",
        "…and the control says why: {boundary}"
    );
    drop(monitor2);
}

/// #60: a click on a task opens its details — subject, status, the description's paragraphs,
/// blocked-by — without moving the transcript; Escape closes it and focus returns to the row;
/// the jump is an explicit action, disabled when the stream kept no target.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_opens_a_task_details_popover() {
    let _serial = serial();
    let base = base("appshell-task-popover");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000060".to_string();
    stores.claude_session(&sid, &harness::long_session(30, harness::Shape::default()));
    let dir = stores.claude_tasks(
        &sid,
        &[
            ("1", "one, done", "completed"),
            ("2", "two, running", "in_progress"),
            ("3", "three, pending", "pending"),
        ],
    );
    // Give the running task a two-paragraph description and a dependency, as taskq writes them.
    std::fs::write(dir.join("2.json"), r#"{"id":"2","subject":"two, running","description":"First paragraph of the task.\n\nSecond paragraph, with detail.","activeForm":"Doing two","status":"in_progress","blocks":["3"],"blockedBy":["1"]}"#).unwrap();
    let monitor = Monitor::spawn(Kind::V2, 2867, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2867/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the app shell to mount the fixture", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"tasks\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'");
    // #186: this case is about the whole board, which is what the live-only filter hides.
    harness::show_every_pane_row(&tab);
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorWork .work-task').length === 3",
        "the tasks to render",
        std::time::Duration::from_secs(20),
        "document.getElementById('navigatorWork').innerText.slice(0, 200)",
    );
    harness::jump_to_end(&tab, harness::Surface::AppShell);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let before = harness::eval(&tab, "document.querySelector('.transcript').scrollTop");
    // The running task is the second row (completed, running, pending).
    harness::eval(
        &tab,
        "document.querySelectorAll('#navigatorWork .work-task-head')[1].click(); 'ok'",
    );
    harness::until(
        &tab,
        "!document.getElementById('taskPopover').hidden",
        "the details to open",
        std::time::Duration::from_secs(5),
        "document.getElementById('taskPopover') ? 'hidden' : 'no popover'",
    );
    // The popover shows the shared task card (#125): a glyph, chips and labelled sections,
    // where it used to show a definition list of facts.
    let shown = harness::probe(&tab, "(function(){ var p = document.getElementById('taskPopover'); var c = p.querySelector('.task-card'); return { subject: p.querySelector('.task-popover-head strong').textContent, glyph: (c.querySelector('.task-card-glyph')||{}).textContent, chips: [...c.querySelectorAll('.task-chip')].map(function (e) { return e.textContent.trim(); }), sections: [...c.querySelectorAll('.task-card-label')].map(function (e) { return e.textContent; }), jumpDisabled: p.querySelector('.task-popover-jump').disabled, transcriptTop: document.querySelector('.transcript').scrollTop, focusInside: p.contains(document.activeElement) }; })()");
    assert_eq!(shown["subject"], "two, running", "{shown}");
    assert_eq!(
        shown["glyph"], "◐",
        "a running task wears its glyph: {shown}"
    );
    let chips: Vec<String> = shown["chips"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        chips.iter().any(|c| c.contains("in progress")),
        "its state: {chips:?}"
    );
    assert!(
        chips.iter().any(|c| c.contains("blocked by #1"))
            && chips.iter().any(|c| c.contains("blocks #3")),
        "what blocks it and what it blocks: {chips:?}"
    );
    assert_eq!(
        shown["sections"],
        serde_json::json!(["description"]),
        "and the description it carries: {shown}"
    );
    assert_eq!(
        shown["jumpDisabled"], true,
        "tasks from the disk store have no record target, so the jump says so: {shown}"
    );
    assert_eq!(
        shown["transcriptTop"], before,
        "the transcript did not move: {shown}"
    );
    assert_eq!(
        shown["focusInside"], true,
        "focus moved into the dialog: {shown}"
    );
    // #187: the title has to be a LINE, not a column of letters. A rect is not visibility (the
    // lesson from #98) and this is that lesson one level further on — a heading squeezed to
    // 12px still opens, still measures a rect, and still contains exactly the right text. It
    // set itself one character per line for as long as `.task-popover-head` declared four grid
    // columns for a head with two children, so the subject sat in the FIXED 12px track.
    let title = harness::probe(&tab, "(function(){ var e = document.querySelector('#taskPopover .task-popover-head strong'); var r = e.getBoundingClientRect(); var cs = getComputedStyle(e); var size = parseFloat(cs.fontSize); var line = parseFloat(cs.lineHeight) || size * 1.5; return { w: Math.round(r.width), h: Math.round(r.height), size: size, lines: Math.round(r.height / line), chars: e.textContent.length }; })()");
    let width = title["w"].as_f64().unwrap_or(0.0);
    let size = title["size"].as_f64().unwrap_or(14.0);
    assert!(
        width > size * 4.0,
        "the popover's title is {width}px wide at {size}px type — narrower than four characters,          so it is setting itself down the page rather than across it (#187): {title}"
    );
    assert_eq!(
        title["lines"], 1,
        "…and a twelve-character subject fits on one line: {title}"
    );

    harness::eval(&tab, "document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); 'ok'");
    harness::until(&tab, "document.getElementById('taskPopover').hidden && document.activeElement === document.querySelectorAll('#navigatorWork .work-task-head')[1]", "Escape to close it and return focus to the row", std::time::Duration::from_secs(5), "String(document.getElementById('taskPopover').hidden) + ' ' + (document.activeElement && document.activeElement.className)");

    drop(monitor);
}

/// #61: clicking a sub-agent in the agents pane switches the whole view to that sub-agent's
/// transcript (the header names the child and its parent); the row's secondary control jumps
/// to the spawn point in the parent instead.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_agents_pane_switches_to_the_sub_agent() {
    let _serial = serial();
    let base = base("appshell-agent-switch");
    let stores = Stores::new(&base);
    let parent = "dddddddd-0000-4000-8000-000000000061".to_string();
    let mut transcript = harness::long_session(20, harness::Shape::default());
    transcript += &harness::agent_spawn("call_61", "Explore", 27);
    transcript += &harness::agent_result("call_61", "aExplore-61", "Explore", 27);
    transcript += &harness::long_session(20, harness::Shape::default());
    stores.claude_session(&parent, &transcript);
    stores.claude_child(
        &parent,
        "aExplore-61",
        &harness::long_session(6, harness::Shape::default()),
    );
    let monitor = Monitor::spawn(Kind::V2, 2850, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let url = format!("http://127.0.0.1:2850/?ui=app&session={parent}");
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the parent to mount", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"agents\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"agents\"]').click(); 'ok'");
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorAgents .outline-agent').length === 1",
        "the one sub-agent to list",
        std::time::Duration::from_secs(20),
        "document.getElementById('navigatorAgents').innerText.slice(0, 200)",
    );
    let row = harness::probe(&tab, "(function(){ var r = document.querySelector('#navigatorAgents .outline-agent'); var s = document.querySelector('#navigatorAgents .outline-agent-spawn'); return { child: r.dataset.childOutline, spawn: s ? s.dataset.agentRecord : null }; })()");
    let child = row["child"].as_str().unwrap_or("").to_string();
    assert!(!child.is_empty(), "the row names the child session: {row}");
    assert!(
        row["spawn"].is_string(),
        "the record stream kept the spawn point, so the secondary control shows: {row}"
    );
    // The primary click: the whole view is the child's now.
    harness::eval(
        &tab,
        "document.querySelector('#navigatorAgents .outline-agent').click(); 'ok'",
    );
    harness::until(&tab, &format!("new URLSearchParams(location.search).get('session') === {child:?} && /sub-agent of/.test(document.getElementById('sessionCrumb').textContent)"), "the view to switch to the sub-agent", std::time::Duration::from_secs(20), "location.search + ' | ' + document.getElementById('sessionCrumb').textContent");
    // Back on the parent, the secondary control jumps to the spawn point instead.
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0 && document.querySelectorAll('#navigatorAgents .outline-agent-spawn').length === 1", "the parent again, with the spawn control", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    harness::jump_to_end(&tab, harness::Surface::AppShell);
    std::thread::sleep(std::time::Duration::from_millis(500));
    harness::eval(
        &tab,
        "document.querySelector('#navigatorAgents .outline-agent-spawn').click(); 'ok'",
    );
    harness::until(&tab, "(function(){ var a = document.querySelector('.virtual-window .renderer-agent, .virtual-window [data-kind=\"agent\"]'); if (!a) return false; var r = a.getBoundingClientRect(), t = document.querySelector('.transcript').getBoundingClientRect(); return r.top >= t.top - 2 && r.top < t.top + t.height * 0.6; })()", "the spawn point to be brought into view in the parent", std::time::Duration::from_secs(10), "location.search");
    assert_eq!(
        harness::eval(&tab, "new URLSearchParams(location.search).get('session')")
            .as_str()
            .unwrap_or(""),
        parent,
        "the secondary control stays in the parent"
    );
    drop(monitor);
}

/// #74: the outline panes are plain — a head click folds or unfolds ITS pane and nothing else,
/// a folded pane is its head alone, the panes stack top to bottom, and at a short window the
/// column scrolls as a whole with the caption and the heads sticking in a stack under it.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_outline_panes_toggle_independently_and_stack() {
    let _serial = serial();
    let base = base("appshell-outline-panes");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000074".to_string();
    let mut transcript = harness::long_session(40, harness::Shape::default());
    transcript += &harness::agent_spawn("call_74", "Explore", 47);
    transcript += &harness::agent_result("call_74", "aExplore-74", "Explore", 47);
    stores.claude_session(&sid, &transcript);
    stores.claude_child(
        &sid,
        "aExplore-74",
        &harness::long_session(4, harness::Shape::default()),
    );
    let mut tasks: Vec<(String, String, &str)> = Vec::new();
    for i in 1..=24 {
        tasks.push((
            format!("{i}"),
            format!("task {i}"),
            if i <= 12 { "completed" } else { "pending" },
        ));
    }
    let borrowed: Vec<(&str, &str, &str)> = tasks
        .iter()
        .map(|(a, b, c)| (a.as_str(), b.as_str(), *c))
        .collect();
    stores.claude_tasks(&sid, &borrowed);
    let monitor = Monitor::spawn(Kind::V2, 2869, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    tab.set_bounds(headless_chrome::types::Bounds::Normal {
        left: Some(0),
        top: Some(0),
        width: Some(1400.0),
        height: Some(620.0),
    })
    .unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2869/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length >= 40",
        "the turns to list",
        std::time::Duration::from_secs(30),
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
    );
    // #186: this case is about panes COMPETING for room, and its fixture carries 12 completed
    // tasks precisely to make the tasks pane tall enough to create that pressure. The live-only
    // filter removes those rows and with them the pressure, so the case has to ask for the whole
    // board — the same line the other board cases carry.
    harness::show_every_pane_row(&tab);
    let state = r#"(function(){ var nav = document.querySelector('.session-navigator'), nr = nav.getBoundingClientRect(); var cap = nav.querySelector('.outline-caption'); return { open: [...document.querySelectorAll('.outline-card')].map(function (c) { return c.dataset.navCard + ':' + (c.classList.contains('open') ? 'open' : 'folded'); }), bodies: [...document.querySelectorAll('.outline-card')].map(function (c) { var body = c.querySelector('.outline-card-body'); return c.dataset.navCard + ':' + (body.offsetParent === null ? 0 : Math.round(body.getBoundingClientRect().height)); }), heads: [...document.querySelectorAll('.outline-card > .outline-card-head')].map(function (h) { var r = h.getBoundingClientRect(); return { key: h.dataset.navCardToggle, top: Math.round(r.top), bottom: Math.round(r.bottom), visible: r.top >= nr.top - 1 && r.bottom <= nr.bottom + 1 }; }), caption: cap ? { top: Math.round(cap.getBoundingClientRect().top), bottom: Math.round(cap.getBoundingClientRect().bottom) } : null, navTop: Math.round(nr.top + parseFloat(getComputedStyle(nav).paddingTop || '0')), navBottom: Math.round(nr.bottom), navScroll: nav.scrollTop, overflow: nav.scrollHeight - nav.clientHeight, windowY: window.scrollY }; })()"#;
    // #87: the "Outline" caption row, collapse control included, sits on the page background.
    let caption_bg = harness::eval(&tab, "getComputedStyle(document.querySelector('.session-navigator > .outline-caption')).backgroundColor");
    assert_eq!(
        caption_bg, "rgb(255, 255, 255)",
        "the caption takes the page background (white in the light theme)"
    );
    // 1 + 2: a head click folds or unfolds its own pane; the others keep their state.
    let before = harness::probe(&tab, state);
    assert_eq!(
        before["open"],
        serde_json::json!(["turns:open", "tasks:folded", "agents:folded"]),
        "a fresh shell: the turns pane open, the others folded: {before}"
    );
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card-toggle=\"tasks\"]').click(); 'ok'",
    );
    harness::until(
        &tab,
        "document.querySelector('[data-nav-card=\"tasks\"]').classList.contains('open')",
        "the tasks head to unfold its pane",
        std::time::Duration::from_secs(5),
        "document.querySelector('[data-nav-card=\"tasks\"]').className",
    );
    let opened = harness::probe(&tab, state);
    assert_eq!(
        opened["open"],
        serde_json::json!(["turns:open", "tasks:open", "agents:folded"]),
        "only the tasks pane changed: {opened}"
    );
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card-toggle=\"turns\"] strong').click(); 'ok'",
    );
    harness::until(
        &tab,
        "(function(){ var c = document.querySelector('[data-nav-card=\"turns\"]'); var b = c.querySelector(':scope > .outline-card-body'); return !c.classList.contains('open') && Math.round(b.getBoundingClientRect().height) === 0; })()",
        "the turns head (its label) to fold its pane, all the way shut",
        std::time::Duration::from_secs(5),
        "(function(){ var c = document.querySelector('[data-nav-card=\"turns\"]'); return c.className + ' h=' + Math.round(c.querySelector(':scope > .outline-card-body').getBoundingClientRect().height); })()",
    );
    let folded = harness::probe(&tab, state);
    assert_eq!(
        folded["open"],
        serde_json::json!(["turns:folded", "tasks:open", "agents:folded"]),
        "folding turns left tasks open: {folded}"
    );
    // 3 + 4: a folded pane is its head alone — no body, no rows.
    let bodies: Vec<String> = folded["bodies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        bodies.iter().any(|b| b == "turns:0") && bodies.iter().any(|b| b == "agents:0"),
        "folded panes show nothing: {folded}"
    );
    assert!(
        bodies
            .iter()
            .any(|b| b.starts_with("tasks:") && b != "tasks:0"),
        "the open pane shows its body: {folded}"
    );
    // Open every pane: 5, they stack top to bottom in order; 6, the column scrolls as a whole.
    harness::eval(&tab, "['turns', 'agents', 'tasks'].forEach(function (k) { var c = document.querySelector('[data-nav-card=\"' + k + '\"]'); if (!c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"' + k + '\"]').click(); }); 'ok'");
    harness::until(
        &tab,
        "document.querySelectorAll('.outline-card.open').length === 3",
        "all three panes to open",
        std::time::Duration::from_secs(5),
        "document.querySelectorAll('.outline-card.open').length",
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let all = harness::probe(&tab, state);
    let heads = all["heads"].as_array().unwrap();
    for w in heads.windows(2) {
        assert!(
            w[1]["top"].as_f64().unwrap() >= w[0]["bottom"].as_f64().unwrap() - 1.0,
            "heads stack top to bottom in order: {all}"
        );
    }
    assert!(
        all["overflow"].as_f64().unwrap_or(0.0) > 100.0,
        "at a short window with every pane open the column overflows, so it scrolls: {all}"
    );
    assert!(
        all["bodies"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| !b.as_str().unwrap().ends_with(":0")),
        "every open pane shows its body: {all}"
    );
    // Scroll the column: the caption sticks at the top, the turns head sticks under it, and the
    // next head sits under the previous one; the window never scrolls.
    harness::eval(
        &tab,
        "(function(){ var nav = document.querySelector('.session-navigator'); nav.dispatchEvent(new WheelEvent('wheel', { deltaY: 400, bubbles: true, cancelable: true })); return 'ok'; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let scrolled = harness::probe(&tab, state);
    // #157: a push spends on the DRAWERS before it scrolls anything, so the column's own offset
    // stays where it was until they are all shut — what moved is the panes. The sticky
    // assertions below are the point of this case and are unchanged: the pile is the browser's
    // own mechanism, and it is now the only thing stacking these heads.
    let moved = scrolled["bodies"].as_array().unwrap();
    assert!(
        moved[0].as_str().unwrap().ends_with(":0"),
        "the push shut the top pane: {scrolled}"
    );
    assert!(
        moved[1..]
            .iter()
            .any(|b| !b.as_str().unwrap().ends_with(":0")),
        "…and stopped before shutting them all, so the stack still has bodies in it: {scrolled}"
    );
    assert_eq!(
        scrolled["navScroll"], 0,
        "…while the column's own offset never moved: {scrolled}"
    );
    assert_eq!(scrolled["windowY"], 0, "the window did not: {scrolled}");
    let cap = &scrolled["caption"];
    assert!(
        (cap["top"].as_f64().unwrap() - scrolled["navTop"].as_f64().unwrap()).abs() <= 1.0,
        "the caption is stuck at the top of the column: {scrolled}"
    );
    let heads = scrolled["heads"].as_array().unwrap();
    assert!(
        (heads[0]["top"].as_f64().unwrap() - cap["bottom"].as_f64().unwrap()).abs() <= 1.0,
        "the turns head is stuck right under the caption: {scrolled}"
    );
    for w in heads.windows(2) {
        assert!(
            w[1]["top"].as_f64().unwrap() >= w[0]["bottom"].as_f64().unwrap() - 1.0,
            "each head sits under the one before it: {scrolled}"
        );
    }
    // A head is never lost above the fold: what is not visible lies below, reachable by scrolling on.
    let nav_bottom = scrolled["navBottom"].as_f64().unwrap();
    assert!(
        heads
            .iter()
            .all(|h| h["visible"] == true || h["top"].as_f64().unwrap() > nav_bottom - 1.0),
        "a head is either in the stack or still below, never scrolled out at the top: {scrolled}"
    );
    drop(monitor);
}

/// #88, the owner's report on the outline column: a pane's body must never show above its own
/// head, and opening a pane must land its body directly under its head. Today each head is
/// shifted down into its stack slot by a transform while the body stays in flow, so between two
/// heads the reader sees the top of a body above the head it belongs to.
///
/// #139: the options popover has to fit on screen. A session that used many tools grew the
/// tool-type list past the bottom of the window, and neither the list nor the popover scrolled —
/// so the tools below the fold could not be reached at all.
///
/// #142 retired half of what this case used to assert. The Reading section no longer lives in
/// this popover at all — it moved behind its own header control, precisely BECAUSE reaching it
/// meant scrolling past an unbounded tool list — so the old "scroll down to the Wide switch"
/// half is replaced by its opposite: Reading must not be in here. The reachability of the
/// reading preferences themselves is `scenario_wide_is_one_click_from_the_header` in
/// `scenarios.rs`, which holds BOTH pages to it. What survives here is the rule #139 was
/// really about, and it is still this popover's own: it fits inside the window, and its tool
/// list scrolls instead of running off the end. The new popover is held to the fitting half
/// too — it is anchored 45px below the top of the header and could as easily run off the
/// bottom of a short window.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_options_popover_fits_and_scrolls() {
    let _serial = serial();
    let base = base("appshell-options-scroll");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000139".to_string();
    let mut transcript = harness::long_session(4, harness::Shape::default());
    for (i, name) in [
        "Bash",
        "Read",
        "Write",
        "Edit",
        "Glob",
        "Grep",
        "Task",
        "WebFetch",
        "WebSearch",
        "NotebookEdit",
        "TodoWrite",
        "Skill",
        "Workflow",
        "MultiEdit",
        "LS",
        "BashOutput",
    ]
    .iter()
    .enumerate()
    {
        transcript += &harness::named_tool_at(
            &format!("t-many-{i}"),
            name,
            "/tmp/x",
            &harness::now_minus(200 - i as u64 * 4),
        );
    }
    transcript += &harness::assistant_at("done with all of those", &harness::now_minus(20));
    stores.claude_session(&sid, &transcript);
    let monitor = Monitor::spawn(Kind::V2, 2876, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.virtual-window')",
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 120)",
    );
    // A short window, so the list has more tools than room — the reader's situation.
    harness::resize(&tab, 1280.0, 700.0);
    harness::until(
        &tab,
        "innerHeight <= 760",
        "the window to shorten",
        std::time::Duration::from_secs(10),
        "innerHeight",
    );
    harness::eval(
        &tab,
        "document.getElementById('filterTranscriptBtn').click(); 'ok'",
    );
    harness::until(
        &tab,
        "document.querySelectorAll('#filterOptions .tool-type-option').length >= 8",
        "the tool list to fill",
        std::time::Duration::from_secs(10),
        "document.querySelectorAll('#filterOptions .tool-type-option').length",
    );
    let fit = harness::probe(&tab, "(function(){ var pop = document.getElementById('navigatorOptions'); var list = document.getElementById('filterOptions'); var pr = pop.getBoundingClientRect(); return { tools: list.querySelectorAll('.tool-type-option').length, listScrolls: list.scrollHeight > list.clientHeight + 1, popBottom: Math.round(pr.bottom), viewport: Math.round(innerHeight), popFits: pr.bottom <= innerHeight + 1, readingRows: pop.querySelectorAll('[data-reading-toggle], [data-reading-size]').length }; })()");
    assert!(
        fit["tools"].as_i64().unwrap_or(0) >= 8,
        "the fixture gives the filter more tools than fit: {fit}"
    );
    assert_eq!(
        fit["listScrolls"], true,
        "the tool list scrolls rather than running off the popover: {fit}"
    );
    assert_eq!(
        fit["popFits"], true,
        "the popover ends inside the window: {fit}"
    );
    // #142: and it holds filters ONLY. A reading preference behind a funnel is what made Wide
    // transcript unfindable — the owner asked for it three times — so its absence here is the
    // rule, not an omission.
    assert_eq!(
        fit["readingRows"].as_i64(),
        Some(0),
        "the filter popover carries no reading preferences at all: {fit}"
    );

    // The reading popover is subject to the same short-window rule, from its own anchor.
    harness::eval(
        &tab,
        "(function(){ document.getElementById('navigatorOptions').classList.remove('open'); document.getElementById('readingBtn').click(); return 'ok'; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let reading = harness::probe(&tab, "(function(){ var pop = document.getElementById('readingOptions'); if (!pop) return { there: false }; var r = pop.getBoundingClientRect(); var wide = pop.querySelector('[data-reading-toggle=\"wide\"]'); var wr = wide ? wide.getBoundingClientRect() : null; var hit = wr ? document.elementFromPoint(wr.left + wr.width / 2, wr.top + wr.height / 2) : null; return { there: true, fits: r.bottom <= innerHeight + 1 && r.top >= 0, wideInView: !!wr && wr.top >= 0 && wr.bottom <= innerHeight + 1, wideHittable: !!(hit && (hit === wide || wide.contains(hit))), viewport: Math.round(innerHeight), popBottom: Math.round(r.bottom) }; })()");
    assert_eq!(
        reading["there"], true,
        "the reading control has its own popover: {reading}"
    );
    assert_eq!(
        reading["fits"], true,
        "…which ends inside a SHORT window rather than running off the bottom: {reading}"
    );
    assert_eq!(
        reading["wideInView"], true,
        "…with the Wide transcript switch on screen, no scrolling at all: {reading}"
    );
    // Hit-tested, not measured: a covered control still reports a perfect rectangle.
    assert_eq!(
        reading["wideHittable"], true,
        "…and actually clickable there: {reading}"
    );
    // #155: the filter popover has to be usable at a NARROW window too, not just a short one.
    // This went untested, and the gap is why a stray measurement was enough to make me believe
    // the preview panel was covering it — filed as a bug that measurement then disproved (the
    // panel is parked off-screen at every width, `transform: translateX(619.91px)` at 820px).
    // The rule is real even though that bug was not, so it gets a guard rather than a comment.
    for width in [820.0_f64, 680.0] {
        harness::resize(&tab, width, 900.0);
        std::thread::sleep(std::time::Duration::from_millis(600));
        harness::eval(
            &tab,
            "(function(){ var p = document.getElementById('navigatorOptions'); if (!p.classList.contains('open')) document.getElementById('filterTranscriptBtn').click(); return 'ok'; })()",
        );
        std::thread::sleep(std::time::Duration::from_millis(400));
        let narrow = harness::probe(
            &tab,
            r#"(function(){ var pop = document.getElementById('navigatorOptions'); var r = pop.getBoundingClientRect(); var row = pop.querySelector('[data-scope]'); var rr = row ? row.getBoundingClientRect() : null; var hit = rr ? document.elementFromPoint(rr.left + rr.width / 2, rr.top + rr.height / 2) : null; return { open: pop.classList.contains('open'), onScreen: r.left >= -1 && r.right <= innerWidth + 1 && r.top >= 0, rowHittable: !!(hit && row && (hit === row || row.contains(hit) || hit.contains(row))), hitTag: hit ? (hit.tagName + '.' + (hit.className || '').toString().split(' ')[0]) : null, rect: [Math.round(r.left), Math.round(r.top), Math.round(r.width), Math.round(r.height)] }; })()"#,
        );
        assert_eq!(
            narrow["open"], true,
            "at {width}px the funnel still opens its popover: {narrow}"
        );
        assert_eq!(
            narrow["onScreen"], true,
            "…inside the window rather than off its edge: {narrow}"
        );
        assert_eq!(
            narrow["rowHittable"], true,
            "…and a scope row answers a click at its own centre — nothing is painted over it: \
             {narrow}"
        );
    }
    drop(monitor);
}

/// #137/#148: the control that brings the outline back has to be THERE — visible, hittable, and
/// the same size at every width. A control that comes and goes with the window cannot be found.
///
/// #137 found this on the top-bar toggle: the generated reference stylesheet carries
/// `.navigator-toggle{display:none!important}` (the demo has no outline to toggle) and nothing in
/// production put it back, so the one control that returned a hidden pane measured 0x0 at
/// 1280/1440/1800 — no box, no hit — and the only way back was a key the reader had to already
/// know. #148 then retired the hidden state and the toggle with it, at the owner's request.
///
/// The RULE outlived both. It now falls on the rail's expand button, which is what returns a
/// collapsed outline, so that is what this case sweeps — same three widths, same two questions
/// (does it have a box a pointer can hit, and does the point at its centre actually reach it),
/// and then the click itself. Keeping the sweep is the point: the bug #137 caught was a control
/// that existed in the DOM and answered `getBoundingClientRect` while being unclickable, which is
/// exactly what a rect assertion cannot see.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_can_bring_a_collapsed_outline_back() {
    let _serial = serial();
    let base = base("appshell-outline-return");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000137".to_string();
    stores.claude_session(&sid, &harness::long_session(12, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2877, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.outline-card')",
        "the outline column to render",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 120)",
    );
    // Collapse to the rail the way a reader does — the caption's own button.
    let off = "document.querySelector('.workspace').classList.contains('navigator-off')";
    let shown = "!document.querySelector('.workspace').classList.contains('navigator-off') && !!document.querySelector('.session-navigator .outline-card')";
    harness::eval(
        &tab,
        "document.getElementById('navigatorClose').click(); 'ok'",
    );
    harness::until(
        &tab,
        off,
        "the outline to collapse to its rail",
        std::time::Duration::from_secs(5),
        "document.querySelector('.workspace').className",
    );

    let seen = "(function(){ var b = document.getElementById('navigatorRailExpand'); if (!b) return { there: false }; var r = b.getBoundingClientRect(); var hit = document.elementFromPoint((r.left + r.right) / 2, (r.top + r.bottom) / 2); return { there: true, w: Math.round(r.width), h: Math.round(r.height), hits: !!hit && (hit === b || b.contains(hit) || hit.contains(b)), title: b.title || '' }; })()";
    for width in [1280.0, 1440.0, 1800.0] {
        harness::resize(&tab, width, 900.0);
        harness::until(
            &tab,
            &format!("Math.abs(innerWidth - {width}) < 60"),
            "the window to resize",
            std::time::Duration::from_secs(10),
            "innerWidth",
        );
        std::thread::sleep(std::time::Duration::from_millis(400));
        let control = harness::probe(&tab, seen);
        assert_eq!(
            control["there"], true,
            "the control that reopens the outline exists: {control}"
        );
        assert!(
            control["w"].as_f64().unwrap_or(0.0) >= 24.0
                && control["h"].as_f64().unwrap_or(0.0) >= 24.0,
            "…with a size a pointer can hit at {width}px: {control}"
        );
        assert_eq!(
            control["hits"], true,
            "…and it answers a click at its own centre at {width}px: {control}"
        );
    }
    // And it does the thing.
    harness::eval(
        &tab,
        "document.getElementById('navigatorRailExpand').click(); 'ok'",
    );
    harness::until(
        &tab,
        shown,
        "the rail's button to bring the outline back",
        std::time::Duration::from_secs(5),
        "document.querySelector('.workspace').className",
    );
    drop(monitor);
}

/// #88: the outline column is an accordion. Each card sticks at its own slot with a rising
/// z-index, so no pane ever shows its body above its own head, and a pane opened while the
/// column is scrolled lands its head AT that slot with the body directly below it.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_outline_is_an_accordion() {
    let _serial = serial();
    let base = base("appshell-outline-accordion");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000088".to_string();
    stores.claude_session(&sid, &harness::long_session(40, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2907, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.outline-card')",
        "the outline column to render",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 120)",
    );
    harness::eval(&tab, "['turns', 'agents', 'tasks'].forEach(function (k) { var c = document.querySelector('[data-nav-card=\"' + k + '\"]'); if (c && !c.classList.contains('open')) c.querySelector('[data-nav-card-toggle]').click(); }); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(500));
    harness::eval(
        &tab,
        "(function(){ var nav = document.querySelector('.session-navigator'); nav.dispatchEvent(new WheelEvent('wheel', { deltaY: 400, bubbles: true, cancelable: true })); return 'ok'; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let above = harness::probe(
        &tab,
        "(function(){ var nav = document.querySelector('.session-navigator'); var out = []; document.querySelectorAll('.outline-card').forEach(function (c) { var h = c.querySelector(':scope > .outline-card-head'); var b = c.querySelector(':scope > .outline-card-body'); if (!h || !b || b.offsetParent === null) return; var hr = h.getBoundingClientRect(), br = b.getBoundingClientRect(), nr = nav.getBoundingClientRect(); var visibleTop = Math.max(br.top, nr.top); if (visibleTop < hr.top - 1 && br.bottom > nr.top) out.push(c.dataset.navCard + ':' + Math.round(hr.top - visibleTop)); }); return out; })()",
    );
    assert_eq!(
        above.as_array().map(Vec::len),
        Some(0),
        "no pane shows its body above its own head: {above}"
    );
    // Expanding from a scrolled column: shut the Session drawer, slide the chain away, open it
    // again. #88 landed the head at its slot by scrolling the column; #139 has no landing to do —
    // a drawer opens where it already is — but the rule it was there for still holds, and is the
    // one asserted: the opened drawer's body is directly below its own head, and it is really
    // OPEN, at its natural height, not immediately eaten again by a budget the slide had already
    // spent. (That is what `slideDrawersTo(min(s, prefix))` is for.)
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card=\"agents\"] [data-nav-card-toggle]').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    harness::eval(
        &tab,
        "(function(){ var nav = document.querySelector('.session-navigator'); nav.dispatchEvent(new WheelEvent('wheel', { deltaY: 600, bubbles: true, cancelable: true })); return 'ok'; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card=\"agents\"] [data-nav-card-toggle]').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(900));
    let landed = harness::probe(&tab, "(function(){ var card = document.querySelector('[data-nav-card=\"agents\"]'); var head = card.querySelector(':scope > .outline-card-head'); var body = card.querySelector(':scope > .outline-card-body'); var was = body.style.height; body.style.height = 'auto'; var natural = body.offsetHeight; body.style.height = was; return { open: card.classList.contains('open'), height: Math.round(body.getBoundingClientRect().height), natural: natural, bodyBelow: Math.round(body.getBoundingClientRect().top - head.getBoundingClientRect().bottom) }; })()");
    assert_eq!(landed["open"], true, "the drawer opened: {landed}");
    assert!(
        (landed["height"].as_f64().unwrap_or(0.0) - landed["natural"].as_f64().unwrap_or(-1.0))
            .abs()
            <= 2.0,
        "…all the way, not into a budget that shuts it again: {landed}"
    );
    assert!(
        landed["bodyBelow"].as_f64().unwrap_or(999.0).abs() <= 2.0,
        "…with its body directly below its own head: {landed}"
    );
    drop(monitor);
}

/// #89: the info pane's three subsections fold on their label, and the choice is the READER's —
/// it survives switching to another session and a reload, because it is one key per viewer and
/// not a property of the session being looked at.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_info_subsections_fold_and_the_choice_persists() {
    let _serial = serial();
    let base = base("appshell-info-folds");
    let stores = Stores::new(&base);
    let first = "cccccccc-0000-4000-8000-000000000089".to_string();
    let second = "cccccccc-0000-4000-8000-000000000090".to_string();
    stores.claude_session(&first, &harness::long_session(6, harness::Shape::default()));
    stores.claude_session(
        &second,
        &harness::long_session(4, harness::Shape::default()),
    );
    let monitor = Monitor::spawn(Kind::V2, 2908, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={first}"));
    let open_info = "(function(){ var t = document.querySelector('.outline-footer-info'); if (t && !document.getElementById('infoPopover').classList.contains('open')) t.click(); return 'ok'; })()";
    let usage_rows = "(function(){ var g = document.querySelector('[data-info-group=\"usage\"]'); return g ? g.querySelectorAll('.session-info-row').length : -1; })()";
    let usage_expanded = "(function(){ var b = document.querySelector('[data-info-fold=\"usage\"]'); return b ? b.getAttribute('aria-expanded') : 'missing'; })()";
    harness::eval(&tab, open_info);
    harness::until(
        &tab,
        &format!("({usage_rows}) > 0"),
        "the Usage subsection to render its rows",
        std::time::Duration::from_secs(20),
        usage_rows,
    );
    assert_eq!(
        harness::eval(&tab, usage_expanded),
        "true",
        "open to begin with"
    );
    // Fold Usage on its label.
    harness::eval(
        &tab,
        "document.querySelector('[data-info-fold=\"usage\"]').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        harness::eval(&tab, usage_rows),
        0,
        "a folded group shows no rows"
    );
    assert_eq!(harness::eval(&tab, usage_expanded), "false");
    // Another session: still folded — the choice is the reader's, not the session's.
    monitor.open(&tab, &format!("?ui=app&session={second}"));
    harness::eval(&tab, open_info);
    harness::until(
        &tab,
        &format!("({usage_expanded}) === 'false'"),
        "Usage still folded in the next session",
        std::time::Duration::from_secs(20),
        usage_expanded,
    );
    assert_eq!(
        harness::eval(&tab, usage_rows),
        0,
        "…and still showing no rows"
    );
    // A reload keeps it.
    monitor.open(&tab, &format!("?ui=app&session={second}"));
    harness::eval(&tab, open_info);
    harness::until(
        &tab,
        &format!("({usage_expanded}) === 'false'"),
        "Usage still folded after a reload",
        std::time::Duration::from_secs(20),
        usage_expanded,
    );
    // Unfolding is remembered the same way.
    harness::eval(
        &tab,
        "document.querySelector('[data-info-fold=\"usage\"]').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        harness::eval(&tab, usage_rows).as_i64().unwrap_or(0) > 0,
        "unfolded again"
    );
    monitor.open(&tab, &format!("?ui=app&session={first}"));
    harness::eval(&tab, open_info);
    harness::until(
        &tab,
        &format!("({usage_rows}) > 0"),
        "…and it stays unfolded",
        std::time::Duration::from_secs(20),
        usage_expanded,
    );
    drop(monitor);
}

/// #67 / #68 / #69: the info pane carries status, counts, tokens with the cached reads and the
/// compaction summary — and none of the title / agent / project the header already shows; the
/// turns pane shows the compaction as an epoch tick between the tenth and eleventh turns.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_info_and_turns_panes_show_the_compaction() {
    let _serial = serial();
    let base = base("appshell-compaction-panes");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000069".to_string();
    let mut transcript = String::new();
    for i in 0..10u64 {
        transcript += &harness::user_at(
            &format!("question {i}: before the compaction"),
            &harness::now_minus(600 - i * 20),
        );
        transcript += &harness::assistant_at(
            &format!("answer {i}: before"),
            &harness::now_minus(590 - i * 20),
        );
    }
    transcript += &harness::compaction_at(&harness::now_minus(395));
    for i in 10..20u64 {
        transcript += &harness::user_at(
            &format!("question {i}: after the compaction"),
            &harness::now_minus(600 - i * 20),
        );
        transcript += &harness::assistant_at(
            &format!("answer {i}: after"),
            &harness::now_minus(590 - i * 20),
        );
    }
    stores.claude_session(&sid, &transcript);
    let monitor = Monitor::spawn(Kind::V2, 2868, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2868/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length === 20",
        "the turns to list",
        std::time::Duration::from_secs(30),
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
    );
    let turns = harness::probe(&tab, "(function(){ var rows = [...document.querySelectorAll('#navigatorTurns > *')]; var tick = document.querySelector('#navigatorTurns .outline-epoch'); return { kinds: rows.map(function (r) { return r.classList.contains('outline-epoch') ? 'epoch' : 'turn'; }), glyph: tick ? (tick.querySelector('.outline-epoch-glyph') || {}).textContent : null, sizes: tick ? (tick.querySelector('.outline-epoch-sizes') || {}).textContent : null, lines: tick ? tick.querySelectorAll('.outline-epoch-line').length : 0, prose: /context compacted/i.test(document.getElementById('navigatorTurns').textContent), font: tick ? getComputedStyle(tick).fontSize + ' ' + getComputedStyle(tick).fontFamily.split(',')[0] : null, rowFont: (function(){ var l = document.querySelector('#navigatorTurns .outline-label'); return l ? getComputedStyle(l).fontSize + ' ' + getComputedStyle(l).fontFamily.split(',')[0] : null; })() }; })()");
    let kinds: Vec<String> = turns["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds.len(), 21, "twenty turns and one tick: {turns}");
    assert_eq!(
        kinds.iter().position(|k| k == "epoch"),
        Some(10),
        "the tick sits between the tenth and the eleventh turn: {turns}"
    );
    // #86: a hairline with the auto glyph and the sizes from → to, in the rows' own type, no prose.
    assert_eq!(
        turns["glyph"], "⟳",
        "an automatic compaction shows its glyph: {turns}"
    );
    assert_eq!(
        turns["sizes"], "594.7K → 8.6K",
        "…and the context size from → to: {turns}"
    );
    assert_eq!(turns["lines"], 2, "…between two hairlines: {turns}");
    assert_eq!(
        turns["prose"], false,
        "…with no 'context compacted' text: {turns}"
    );
    assert_eq!(
        turns["font"], turns["rowFont"],
        "…in the turn rows' own type: {turns}"
    );
    // The info pane: open its card and read the rows.
    harness::eval(&tab, "var c = document.querySelector('[data-nav-card=\"session\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"session\"]').click(); 'ok'");
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorSession .session-info-row').length > 0",
        "the info pane to render",
        std::time::Duration::from_secs(10),
        "document.getElementById('navigatorSession').innerText.slice(0, 200)",
    );
    let info = harness::probe(&tab, "(function(){ var rows = {}; document.querySelectorAll('#navigatorSession .session-info-row').forEach(function (r) { rows[r.querySelector('span').textContent] = r.querySelector('strong').textContent; }); return rows; })()");
    for gone in ["title", "agent", "project"] {
        assert!(
            info.get(gone).is_none(),
            "the {gone} row is gone — the header shows it: {info}"
        );
    }
    for gone in ["status", "turns", "children"] {
        assert!(
            info.get(gone).is_none(),
            "the {gone} row is gone too (#170) — the topbar chip and the pane heads carry it: {info}"
        );
    }
    assert!(
        info.get("cache read").is_some(),
        "the cached-read row is there: {info}"
    );
    let compacted = info.get("compacted").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        compacted.contains("compacted"),
        "the compaction summary reads as the classic panel's: {info}"
    );
    // The tick jumps to the compaction record: the transcript leaves the tail.
    harness::jump_to_end(&tab, harness::Surface::AppShell);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let at_tail = harness::eval(&tab, "document.querySelector('.transcript').scrollTop");
    harness::eval(
        &tab,
        "document.querySelector('#navigatorTurns .outline-epoch').click(); 'ok'",
    );
    harness::until(
        &tab,
        &format!(
            "document.querySelector('.transcript').scrollTop < {} - 200",
            at_tail.as_f64().unwrap_or(0.0)
        ),
        "the tick to jump the transcript",
        std::time::Duration::from_secs(5),
        "document.querySelector('.transcript').scrollTop",
    );
    drop(monitor);
}

/// #75: the theme toggle in the sidebar head is a visible button with a glyph, and it toggles
/// the theme.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_theme_toggle_is_visible_and_works() {
    let _serial = serial();
    let base = base("appshell-theme-toggle");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000075".to_string();
    stores.claude_session(&sid, &harness::long_session(6, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2870, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2870/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        "!!document.getElementById('themeBtn')",
        "the shell to render",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    let probe = "(function(){ var b = document.getElementById('themeBtn'); var r = b.getBoundingClientRect(); return { visible: b.offsetParent !== null && r.width >= 24 && r.height >= 24, glyph: !!b.querySelector('svg'), label: b.getAttribute('aria-label') || '', theme: document.documentElement.dataset.theme || '' }; })()";
    let before = harness::probe(&tab, probe);
    assert_eq!(
        before["visible"], true,
        "the theme toggle is visible: {before}"
    );
    assert_eq!(before["glyph"], true, "…with a glyph: {before}");
    assert!(
        before["label"].as_str().unwrap_or("").contains("Toggle"),
        "…and a name: {before}"
    );
    let was_dark = before["theme"] == "dark";
    harness::eval(&tab, "document.getElementById('themeBtn').click(); 'ok'");
    harness::until(
        &tab,
        &format!(
            "(document.documentElement.dataset.theme || '') {} 'dark'",
            if was_dark { "!==" } else { "===" }
        ),
        "the click to toggle the theme",
        std::time::Duration::from_secs(5),
        "document.documentElement.dataset.theme",
    );
    // The collapse-all control beside it is drawn too (#75 covers both empty buttons).
    let collapse_all = harness::probe(&tab, "(function(){ var b = document.getElementById('collapseBtn'); return { glyph: !!b.querySelector('svg'), label: b.getAttribute('aria-label') || '' }; })()");
    assert_eq!(
        collapse_all["glyph"], true,
        "collapse-all has its glyph too: {collapse_all}"
    );
    drop(monitor);
}

/// #84: the records a session opens with are never "new". A fresh open lands at the tail with
/// no count; scrolled up and reloaded, the remembered position comes back and still nothing is
/// counted; eight records arriving afterwards are "8 new".
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_open_counts_nothing_as_new() {
    let _serial = serial();
    let base = base("appshell-open-count");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000084".to_string();
    let path = stores.claude_session(&sid, &harness::long_session(40, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2871, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let url = format!("http://127.0.0.1:2871/?ui=app&session={sid}");
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    let mounted = "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0 && document.querySelector('.transcript').scrollHeight > document.querySelector('.transcript').clientHeight * 3";
    harness::until(
        &tab,
        mounted,
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    harness::until(&tab, "(function(){ var s = document.querySelector('.transcript'); return s.scrollHeight - s.clientHeight - s.scrollTop <= 2; })()", "a fresh open to land at the tail", std::time::Duration::from_secs(10), "document.querySelector('.transcript').scrollTop");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        harness::new_messages_pill(&tab, harness::Surface::AppShell) <= 0,
        "a fresh open counts nothing: {}",
        harness::new_messages_pill(&tab, harness::Surface::AppShell)
    );
    // Scroll up, let the position be remembered, reload.
    harness::scroll_by(&tab, harness::Surface::AppShell, -1500);
    harness::scroll_by(&tab, harness::Surface::AppShell, -1500);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let anchor = harness::view_anchor(&tab, harness::Surface::AppShell);
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        mounted,
        "the reloaded shell to mount",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    // Wait for the restore rather than guessing at it: the remembered unit has to stream in
    // before it can be landed on, and how long that takes is the machine's business (a fixed
    // 3s sleep failed once on CI and never locally). The assertion is the same; only the
    // deadline replaces the guess.
    let at_top = "(function(){ var s = document.querySelector('.transcript'); var top = s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { var r = c.getBoundingClientRect(); if (r.bottom > top + 1) return c.dataset.unitKey || ''; } return ''; })()";
    harness::until(
        &tab,
        &format!("{at_top} === {:?}", anchor.0),
        "the reload to restore the remembered position",
        std::time::Duration::from_secs(20),
        at_top,
    );
    let restored = harness::view_anchor(&tab, harness::Surface::AppShell);
    assert_eq!(
        restored.0, anchor.0,
        "the reload restored the remembered position (the same unit at the top)"
    );
    assert!(
        harness::new_messages_pill(&tab, harness::Surface::AppShell) <= 0,
        "a re-open at a remembered position counts nothing: {}",
        harness::new_messages_pill(&tab, harness::Surface::AppShell)
    );
    // Growth after the open is what the pill is for.
    let script: Vec<String> = (0..4)
        .flat_map(|k| {
            vec![
                harness::user_at(
                    &format!("question {}: after the open", 900 + k),
                    &harness::now_minus(30 - k * 6),
                ),
                harness::assistant_at(
                    &format!("answer {}: after the open", 900 + k),
                    &harness::now_minus(27 - k * 6),
                ),
            ]
        })
        .collect();
    let growth =
        harness::LiveGrowth::start(path.clone(), script, std::time::Duration::from_millis(2600));
    assert_eq!(
        growth.finish(std::time::Duration::from_secs(40)),
        8,
        "the driver appended the whole script"
    );
    harness::await_pill(
        &tab,
        harness::Surface::AppShell,
        8,
        "only the eight that arrived count",
    );
    drop(monitor);
}

/// #82: a child opened FIRST — a deep link, no parent pulled — still has its way back: the
/// server names the root it found the child under, the control shows, and it lands on the parent.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_finds_the_parent_from_a_child_opened_first() {
    let _serial = serial();
    let base = base("appshell-child-first");
    let stores = Stores::new(&base);
    let parent = "dddddddd-0000-4000-8000-000000000082".to_string();
    let mut transcript = harness::long_session(8, harness::Shape::default());
    transcript += &harness::agent_spawn("call_82", "Explore", 9);
    transcript += &harness::agent_result("call_82", "aExplore-82", "Explore", 9);
    transcript += &harness::long_session(4, harness::Shape::default());
    stores.claude_session(&parent, &transcript);
    stores.claude_child(
        &parent,
        "aExplore-82",
        &harness::long_session(6, harness::Shape::default()),
    );
    let monitor = Monitor::spawn(Kind::V2, 2872, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to("http://127.0.0.1:2872/?ui=app&session=aExplore-82")
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the child to mount from its deep link", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 160)");
    harness::until(&tab, "document.getElementById('sessionParent').classList.contains('is-live')", "the parent control to appear", std::time::Duration::from_secs(15), "JSON.stringify({cls: document.getElementById('sessionParent').className, crumb: document.getElementById('sessionCrumb').textContent})");
    let control = harness::probe(&tab, "(function(){ var b = document.getElementById('sessionParent'); var r = b.getBoundingClientRect(); return { visible: b.offsetParent !== null && r.width >= 24, parent: b.dataset.parent, label: b.textContent.trim() }; })()");
    assert_eq!(
        control["visible"], true,
        "the way back is visible: {control}"
    );
    assert_eq!(
        control["parent"], parent,
        "…and names the root the child was found under: {control}"
    );
    harness::eval(
        &tab,
        "document.getElementById('sessionParent').click(); 'ok'",
    );
    harness::until(
        &tab,
        &format!("new URLSearchParams(location.search).get('session') === {parent:?}"),
        "the click to land on the parent",
        std::time::Duration::from_secs(15),
        "location.search",
    );
    drop(monitor);
}

/// #77: collapse-all folds every group of the session list; expand-all opens them again; the
/// choice survives a reload.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_expands_every_group_again() {
    let _serial = serial();
    let base = base("appshell-expand-all");
    let stores = Stores::new(&base);
    for n in 1..=3u32 {
        stores.claude_session(
            &format!("cccccccc-0000-4000-8000-0000000000{n:02}"),
            &harness::long_session(4, harness::Shape::default()),
        );
    }
    let monitor = Monitor::spawn(Kind::V2, 2874, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let url = "http://127.0.0.1:2874/?ui=app";
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    let toggles = "document.querySelectorAll('[data-toggle]').length";
    harness::until(
        &tab,
        &format!("{toggles} >= 2"),
        "the tree to list an agent and a project",
        std::time::Duration::from_secs(30),
        toggles,
    );
    let state = "(function(){ var t = [...document.querySelectorAll('[data-toggle]')]; return { total: t.length, open: t.filter(function (e) { return e.getAttribute('aria-expanded') === 'true'; }).length, expandVisible: !!document.getElementById('expandBtn') && document.getElementById('expandBtn').offsetParent !== null && !!document.getElementById('expandBtn').querySelector('svg') }; })()";
    let start = harness::probe(&tab, state);
    assert_eq!(
        start["expandVisible"], true,
        "the expand-all control is there with its glyph: {start}"
    );
    harness::eval(&tab, "document.getElementById('collapseBtn').click(); 'ok'");
    harness::until(&tab, "[...document.querySelectorAll('[data-toggle]')].every(function (e) { return e.getAttribute('aria-expanded') === 'false'; })", "collapse-all to fold every group", std::time::Duration::from_secs(5), state);
    harness::eval(&tab, "document.getElementById('expandBtn').click(); 'ok'");
    harness::until(&tab, "[...document.querySelectorAll('[data-toggle]')].every(function (e) { return e.getAttribute('aria-expanded') === 'true'; })", "expand-all to open every group", std::time::Duration::from_secs(5), state);
    // Remembered: fold all, reload, still folded; expand all, reload, still open.
    harness::eval(&tab, "document.getElementById('collapseBtn').click(); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        &format!("{toggles} >= 1"),
        "the reloaded tree",
        std::time::Duration::from_secs(30),
        toggles,
    );
    assert_eq!(
        harness::probe(&tab, state)["open"],
        0,
        "the folds are remembered across a reload"
    );
    harness::eval(&tab, "document.getElementById('expandBtn').click(); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    tab.navigate_to(url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        &format!("{toggles} >= 2"),
        "the reloaded tree again",
        std::time::Duration::from_secs(30),
        toggles,
    );
    let after = harness::probe(&tab, state);
    assert_eq!(
        after["open"], after["total"],
        "expand-all is remembered across a reload: {after}"
    );
    drop(monitor);
}

/// #96: the session list is resizable — a handle on the sidebar's own right edge, a width that
/// is clamped, remembered across a reload, and reset by a double-click. The head reflows with
/// it: the controls that share a row with the brand only when there is room for them.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_resizes_the_session_list() {
    let _serial = serial();
    let base = base("appshell-sidebar-resize");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000096".to_string();
    stores.claude_session(&sid, &harness::long_session(10, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2876, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let url = format!("http://127.0.0.1:2876/?ui=app&session={sid}");
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    let mounted = "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0";
    harness::until(
        &tab,
        mounted,
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    let state = "(function(){ var side = document.querySelector('.sidebar'), head = document.querySelector('.side-head'); var handle = document.getElementById('sidebarResizer'); var hr = handle ? handle.getBoundingClientRect() : null; var sr = side.getBoundingClientRect(); var brand = head.querySelector('.brand').getBoundingClientRect(), acts = head.querySelector('.head-actions').getBoundingClientRect(); return { width: Math.round(sr.width), handle: hr ? [Math.round(hr.right - sr.right), Math.round(hr.width)] : null, sameRow: Math.abs(acts.top - brand.top) <= 10, actionsInside: acts.right <= sr.right + 0.5, stored: localStorage.getItem('am-sidebar-width'), role: handle ? handle.getAttribute('role') : '' }; })()";
    let start = harness::probe(&tab, state);
    assert_eq!(
        start["width"], 300,
        "the list opens at its default width: {start}"
    );
    assert_eq!(
        start["handle"]
            .as_array()
            .map(|h| (h[0].as_i64(), h[1].as_i64())),
        Some((Some(0), Some(6))),
        "the handle sits on the sidebar's right edge: {start}"
    );
    assert_eq!(start["role"], "separator", "…named for what it is: {start}");
    // A drag widens it. The head reflows: at 300px its five controls take their own row, and
    // with the room a wider list gives they come back up beside the brand.
    assert_eq!(
        start["sameRow"], false,
        "at the default width the head's controls take their own row rather than overflow: {start}"
    );
    assert_eq!(
        start["actionsInside"], true,
        "…and every one of them is inside the sidebar: {start}"
    );
    let drag = |x: i32| {
        format!("(function(){{ var r = document.getElementById('sidebarResizer'); var rect = r.getBoundingClientRect(); r.dispatchEvent(new PointerEvent('pointerdown', {{ clientX: rect.left + 3, clientY: 300, bubbles: true, pointerId: 1 }})); dispatchEvent(new PointerEvent('pointermove', {{ clientX: {x}, clientY: 300, bubbles: true, pointerId: 1 }})); dispatchEvent(new PointerEvent('pointerup', {{ clientX: {x}, clientY: 300, bubbles: true, pointerId: 1 }})); return 'ok'; }})()")
    };
    harness::eval(&tab, &drag(430));
    harness::until(
        &tab,
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width) === 430",
        "the list to follow the drag",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    let wide = harness::probe(&tab, state);
    assert_eq!(wide["stored"], "430", "the width is the viewer's: {wide}");
    assert_eq!(
        wide["sameRow"], true,
        "the head's controls come back up beside the brand once there is room: {wide}"
    );
    // Clamped at both ends — a drag off the left edge cannot leave a sliver, nor eat the page.
    harness::eval(&tab, &drag(60));
    harness::until(
        &tab,
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width) === 232",
        "the list to stop at its minimum",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    harness::eval(&tab, &drag(900));
    harness::until(
        &tab,
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width) === 520",
        "the list to stop at its maximum",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    // Remembered across a reload, and a double-click puts it back.
    tab.navigate_to(&url).unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(
        &tab,
        mounted,
        "the reloaded shell",
        std::time::Duration::from_secs(30),
        "document.body.innerText.slice(0, 120)",
    );
    harness::until(
        &tab,
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width) === 520",
        "the width to survive the reload",
        std::time::Duration::from_secs(10),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    harness::eval(&tab, "document.getElementById('sidebarResizer').dispatchEvent(new MouseEvent('dblclick', { bubbles: true })); 'ok'");
    harness::until(
        &tab,
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width) === 300",
        "a double-click to restore the default width",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('.sidebar').getBoundingClientRect().width)",
    );
    // The rail has no width to drag: collapsed, the handle is gone.
    harness::eval(
        &tab,
        "document.getElementById('sidebarCollapse').click(); 'ok'",
    );
    harness::until(
        &tab,
        "document.getElementById('sidebarResizer').offsetParent === null",
        "the handle to leave with the list",
        std::time::Duration::from_secs(5),
        "document.getElementById('app').className",
    );
    drop(monitor);
}

/// #78 / #95: the roster of what the session published lives in the right pane — one row per
/// URL, the republish counted, in a new-tab link — with the count on the pane's button while
/// the pane is hidden, and a jump that lands the transcript on the publishing record.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_lists_the_published_artifacts() {
    let _serial = serial();
    let base = base("appshell-artifacts");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000078".to_string();
    let mut transcript = harness::long_session(6, harness::Shape::default());
    transcript += &harness::user_at("question 6: publish the decks", &harness::now_minus(80));
    transcript += &harness::assistant_at("answer 6a: publishing", &harness::now_minus(78));
    transcript += &harness::artifact_publish_at(
        "art1",
        "/w/deck.html",
        "One Deck",
        "📊",
        "https://claude.ai/code/artifact/aaaa-1",
        &harness::now_minus(76),
    );
    transcript += &harness::artifact_publish_at(
        "art2",
        "/w/notes.html",
        "Notes",
        "📝",
        "https://claude.ai/code/artifact/bbbb-2",
        &harness::now_minus(70),
    );
    transcript += &harness::artifact_publish_at(
        "art3",
        "/w/deck.html",
        "One Deck",
        "📊",
        "https://claude.ai/code/artifact/aaaa-1",
        &harness::now_minus(64),
    );
    transcript += &harness::assistant_at("answer 6: both published", &harness::now_minus(60));
    // Room BELOW the publishing records, so a jump can actually put one at the top of the
    // viewport — at the tail of the transcript the scroller has nowhere left to go.
    for i in 0..5u64 {
        transcript += &harness::user_at(
            &format!(
                "question {}: {}",
                7 + i,
                "lorem ipsum dolor sit amet, consectetur. ".repeat(6)
            ),
            &harness::now_minus(50 - i * 10),
        );
        transcript += &harness::assistant_at(
            &format!(
                "answer {}: {}",
                7 + i,
                "sed do eiusmod tempor incididunt ut labore. ".repeat(8)
            ),
            &harness::now_minus(48 - i * 10),
        );
    }
    stores.claude_session(&sid, &transcript);
    let monitor = Monitor::spawn(Kind::V2, 2875, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    tab.navigate_to(&format!("http://127.0.0.1:2875/?ui=app&session={sid}"))
        .unwrap();
    tab.wait_until_navigated().unwrap();
    harness::until(&tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the app shell to mount the fixture", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
    assert_eq!(
        harness::probe(&tab, "!!document.getElementById('artifactsBtn') || !!document.getElementById('artifactsMenu')"),
        false,
        "the header control is gone (#95)"
    );
    // Hidden pane, and the button says there is something behind it.
    harness::until(
        &tab,
        "(document.querySelector('#previewBtn .preview-badge') || {}).textContent === '2'",
        "the pane's button to carry the published count",
        std::time::Duration::from_secs(20),
        "(document.querySelector('#previewBtn .preview-badge') || {textContent: 'no badge'}).textContent",
    );
    assert_eq!(
        harness::probe(
            &tab,
            "document.getElementById('app').classList.contains('preview-off')"
        ),
        true,
        "the pane is still hidden — the badge is what reaches the reader"
    );
    harness::eval(&tab, "document.getElementById('previewBtn').click(); 'ok'");
    harness::until(
        &tab,
        "!document.getElementById('app').classList.contains('preview-off') && !!document.querySelector('#previewTabs .preview-tab.pinned.on')",
        "the pane to open on its pinned roster tab",
        std::time::Duration::from_secs(10),
        "document.getElementById('previewTabs').innerText",
    );
    assert_eq!(
        harness::probe(
            &tab,
            "document.querySelector('#previewTabs .preview-tab.pinned').innerText.trim()"
        ),
        "Artifacts (2)",
        "the pinned tab counts them"
    );
    let rows = harness::probe(&tab, "(function(){ return [...document.querySelectorAll('#previewBody .artifacts-row')].map(function (r) { var a = r.querySelector('a'); return { href: a.getAttribute('href'), target: a.getAttribute('target'), name: (r.querySelector('.artifacts-name') || {}).textContent, count: (r.querySelector('.artifacts-count') || {}).textContent || '', at: r.querySelector('[data-artifact-record]').dataset.artifactRecord }; }); })()");
    assert_eq!(
        rows.as_array().map(|r| r.len()),
        Some(2),
        "one row per URL: {rows}"
    );
    assert_eq!(
        rows[0]["href"], "https://claude.ai/code/artifact/aaaa-1",
        "first-seen first: {rows}"
    );
    assert_eq!(rows[0]["count"], "×2", "the republish is counted: {rows}");
    assert_eq!(rows[0]["name"], "One Deck", "{rows}");
    assert_eq!(
        rows[0]["target"], "_blank",
        "a row opens in a new tab: {rows}"
    );
    assert_eq!(
        rows[1]["href"], "https://claude.ai/code/artifact/bbbb-2",
        "{rows}"
    );
    assert_eq!(
        rows[1]["count"], "",
        "a single publish carries no count: {rows}"
    );
    // The jump lands the transcript on the publishing record — from the tail, and with the pane
    // left open behind it (a pane is not a menu that dismisses itself).
    harness::jump_to_end(&tab, harness::Surface::AppShell);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let at = rows[1]["at"].as_str().unwrap_or_default().to_string();
    harness::eval(
        &tab,
        "document.querySelector('#previewBody .artifacts-row:nth-child(2) .artifacts-jump').click(); 'ok'",
    );
    // Where a jump lands is the scroller's own `scroll-padding-top` (the turn bar's height,
    // #123) or 18px when nothing sticky sits there — read it off the page rather than pinning
    // a number the chrome above the transcript is free to change.
    let landed = format!("(function(){{ var t = document.querySelector('.transcript'); var e = document.querySelector('[data-block-index=\"{at}\"]'); if (!e) return false; var want = parseFloat(getComputedStyle(t).scrollPaddingTop) || 18; var top = e.getBoundingClientRect().top - t.getBoundingClientRect().top; return Math.abs(top - want) <= 8; }})()");
    harness::until(
        &tab,
        &landed,
        "the transcript to land on the record that published it",
        std::time::Duration::from_secs(10),
        &format!("(function(){{ var t = document.querySelector('.transcript'); var e = document.querySelector('[data-block-index=\"{at}\"]'); return e ? (e.getBoundingClientRect().top - t.getBoundingClientRect().top) + ' (want ' + (parseFloat(getComputedStyle(t).scrollPaddingTop) || 18) + ')' : 'not rendered'; }})()"),
    );
    assert_eq!(
        harness::probe(
            &tab,
            "document.getElementById('app').classList.contains('preview-off')"
        ),
        false,
        "the pane stays open after a jump"
    );
    drop(monitor);
}

/// #139: the outline column is a stack of DRAWERS (design/outline-drawers.md). Scrolling it spends
/// a BUDGET on them in order — the top drawer closes first, because it is the one with the least
/// friction — the heads do not move while that happens, the gaps between cards are the same at
/// every openness, the column's scroll extent does not change under the reader's thumb, and
/// scrolling back up retraces exactly, because openness is a pure function of the offset.
/// (App-shell only: the classic page has no outline column, so there is no second surface here.)
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_outline_panes_are_drawers() {
    let _serial = serial();
    let base = base("appshell-drawers");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000139".to_string();
    stores.claude_session(&sid, &harness::long_session(60, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2914, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    tab.set_bounds(headless_chrome::types::Bounds::Normal {
        left: Some(0),
        top: Some(0),
        width: Some(1400.0),
        height: Some(620.0),
    })
    .unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length >= 40",
        "the turns to list",
        std::time::Duration::from_secs(30),
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
    );
    // Every drawer open, so the whole chain is there to be pushed.
    harness::eval(&tab, "['turns', 'tasks', 'agents', 'session'].forEach(function (k) { var c = document.querySelector('[data-nav-card=\"' + k + '\"]'); if (!c.classList.contains('open')) c.querySelector('[data-nav-card-toggle]').click(); }); 'ok'");
    // Wait on the app's OWN signal, not a guess. `drawers-animating` gates the
    // `transition:height .2s` on the card bodies (production.css:331) and app.js clears it 260ms
    // after the last toggle, so its absence means every drawer has reached its final height. The
    // fixed 700ms this replaces was enough on an idle machine and not enough on a busy one: a
    // still-animating body measures 0, which reads exactly like a drawer that never opened, and
    // the case failed three runs in a row under load then passed on the next with no code change
    // (#183). Waiting for the heights to be NON-ZERO would make the assertion below tautological;
    // waiting for the animation to END keeps it honest, because a drawer that really stayed shut
    // settles at 0 and still fails.
    harness::until(
        &tab,
        "!document.querySelector('.session-navigator').classList.contains('drawers-animating')",
        "the drawer open/close animation to finish",
        std::time::Duration::from_secs(10),
        "document.querySelector('.session-navigator').className",
    );
    std::thread::sleep(std::time::Duration::from_millis(120));
    let state = r#"(function(){ var nav = document.querySelector('.session-navigator'); var cards = [...nav.querySelectorAll(':scope > .outline-card')]; var rect = function (e) { return e.getBoundingClientRect(); }; return { scroll: Math.round(nav.scrollTop), extent: Math.round(nav.scrollHeight - nav.clientHeight), keys: cards.map(function (c) { return c.dataset.navCard; }), bodies: cards.map(function (c) { return Math.round(rect(c.querySelector(':scope > .outline-card-body')).height); }), heads: cards.map(function (c) { return Math.round(rect(c.querySelector(':scope > .outline-card-head')).top); }), gaps: cards.slice(1).map(function (c, i) { return Math.round(rect(c).top - rect(cards[i]).bottom); }) }; })()"#;
    let open = harness::probe(&tab, state);
    let bodies = |v: &serde_json::Value| -> Vec<f64> {
        v["bodies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_f64().unwrap())
            .collect()
    };
    let at_rest = bodies(&open);
    assert!(
        at_rest.iter().all(|b| *b > 0.0),
        "every drawer starts open: {open}"
    );
    assert!(
        open["extent"].as_f64().unwrap_or(0.0) > 200.0,
        "with every drawer open the column has room to close them: {open}"
    );
    // Push the chain a little: the TOP drawer takes all of it, and only it.
    harness::eval(
        &tab,
        "(function(){ var nav = document.querySelector('.session-navigator'); nav.dispatchEvent(new WheelEvent('wheel', { deltaY: 200, bubbles: true, cancelable: true })); return 'ok'; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let pushed = harness::probe(&tab, state);
    let now = bodies(&pushed);
    assert!(
        (at_rest[0] - now[0] - 200.0).abs() <= 2.0,
        "the top drawer takes the whole push: {open} -> {pushed}"
    );
    assert_eq!(
        &now[1..],
        &at_rest[1..],
        "…and no other drawer has begun to close: {pushed}"
    );
    assert_eq!(
        pushed["heads"][0], open["heads"][0],
        "the top drawer's head does not move: {open} -> {pushed}"
    );
    assert_eq!(
        pushed["gaps"], open["gaps"],
        "the gaps between drawers are rigid — the same at every openness: {open} -> {pushed}"
    );
    // #157: the extent shrinks by exactly what the drawers gave up. The old model held it
    // CONSTANT, with an invisible spacer re-adding the height the bodies lost — which is what
    // made the scroll offset unable to express the state and left three shut panes unreachable.
    // The reader's input is a push now, not a drag, so the content is simply as tall as it is.
    assert_eq!(
        open["extent"].as_f64().unwrap_or(0.0) - pushed["extent"].as_f64().unwrap_or(0.0),
        at_rest[0] - now[0],
        "the column is shorter by exactly what closed — no spacer standing in for it: {open} -> {pushed}"
    );
    // Push past the top drawer's whole body: it is shut, and the SECOND one starts.
    harness::eval(
        &tab,
        &format!(
            "(function(){{ document.querySelector('.session-navigator').dispatchEvent(new WheelEvent('wheel', {{ deltaY: {}, bubbles: true, cancelable: true }})); return 'ok'; }})()",
            // Relative, not absolute: 200 of the top drawer is already spent, so this is what
            // is left of it plus a little, which is where the SECOND drawer starts.
            at_rest[0] as i64 - 200 + 60
        ),
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let deep = harness::probe(&tab, state);
    let now = bodies(&deep);
    assert_eq!(now[0], 0.0, "the top drawer is shut: {deep}");
    assert!(
        now[1] < at_rest[1] && now[1] > 0.0,
        "…and only then does the second begin to close: {deep}"
    );
    assert_eq!(
        deep["heads"][0], open["heads"][0],
        "the top head is still fixed: {deep}"
    );
    assert_eq!(
        deep["gaps"], open["gaps"],
        "the gaps are still rigid: {deep}"
    );
    // And back: a pull gives back in the reverse order it took, so the column retraces exactly.
    harness::eval(
        &tab,
        "(function(){ var nav = document.querySelector('.session-navigator'); nav.dispatchEvent(new WheelEvent('wheel', { deltaY: -4000, bubbles: true, cancelable: true })); return 'ok'; })()",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let back = harness::probe(&tab, state);
    assert_eq!(
        bodies(&back),
        at_rest,
        "scrolling back opens them again, bottom-first, to exactly where they were: {back}"
    );
    // And the chain stops when there is nothing left to gain: at the very end of the column,
    // either a drawer is still part-way (the budget could not be fully spent — the model's clamp,
    // the bottom drawer need not shut while its bottom is visible) or the shut stack itself
    // overflows and there is stack left to scroll. Anything else is scroll over which NOTHING
    // moves, which is what #88's floor box used to add (measured: 140px of it).
    harness::eval(
        &tab,
        "var n = document.querySelector('.session-navigator'); n.scrollTop = n.scrollHeight; 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let bottom = harness::probe(
        &tab,
        r#"(function(){ var nav = document.querySelector('.session-navigator'); var cards = [...nav.querySelectorAll(':scope > .outline-card')]; var stack = 0; cards.forEach(function (c) { stack += c.querySelector(':scope > .outline-card-head').getBoundingClientRect().height + (parseFloat(getComputedStyle(c).marginBottom) || 0); }); var cs = getComputedStyle(nav); var cap = nav.querySelector(':scope > .outline-caption'); return { openBodies: cards.filter(function (c) { return c.querySelector(':scope > .outline-card-body').getBoundingClientRect().height > 1; }).length, shutStack: Math.round(stack + cap.getBoundingClientRect().height + (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0)), clientH: nav.clientHeight }; })()"#,
    );
    assert!(
        bottom["openBodies"].as_i64().unwrap_or(0) > 0
            || bottom["shutStack"].as_f64().unwrap_or(0.0)
                > bottom["clientH"].as_f64().unwrap_or(0.0),
        "at the end of the column there is no scroll left over which nothing moves: {bottom}"
    );
    drop(monitor);
}

/// #139 rule 5: the toggle is the only EXPLICIT open/close, and the only thing that acts on one
/// drawer. On the frontier — the single drawer that is ever part-way — it completes the movement
/// the slide was making; at either endpoint it opens or shuts that drawer alone, so shutting
/// Session never shuts Turns.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_outline_toggle_completes_the_slide() {
    let _serial = serial();
    let base = base("appshell-drawer-toggle");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000140".to_string();
    stores.claude_session(&sid, &harness::long_session(60, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2915, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    tab.set_bounds(headless_chrome::types::Bounds::Normal {
        left: Some(0),
        top: Some(0),
        width: Some(1400.0),
        height: Some(620.0),
    })
    .unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length >= 40",
        "the turns to list",
        std::time::Duration::from_secs(30),
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
    );
    harness::eval(&tab, "['turns', 'tasks', 'agents', 'session'].forEach(function (k) { var c = document.querySelector('[data-nav-card=\"' + k + '\"]'); if (!c.classList.contains('open')) c.querySelector('[data-nav-card-toggle]').click(); }); 'ok'");
    // Wait on the app's OWN signal, not a guess. `drawers-animating` gates the
    // `transition:height .2s` on the card bodies (production.css:331) and app.js clears it 260ms
    // after the last toggle, so its absence means every drawer has reached its final height. The
    // fixed 700ms this replaces was enough on an idle machine and not enough on a busy one: a
    // still-animating body measures 0, which reads exactly like a drawer that never opened, and
    // the case failed three runs in a row under load then passed on the next with no code change
    // (#183). Waiting for the heights to be NON-ZERO would make the assertion below tautological;
    // waiting for the animation to END keeps it honest, because a drawer that really stayed shut
    // settles at 0 and still fails.
    harness::until(
        &tab,
        "!document.querySelector('.session-navigator').classList.contains('drawers-animating')",
        "the drawer open/close animation to finish",
        std::time::Duration::from_secs(10),
        "document.querySelector('.session-navigator').className",
    );
    std::thread::sleep(std::time::Duration::from_millis(120));
    let heights = r#"(function(){ var cards = [...document.querySelectorAll('.session-navigator > .outline-card')]; return cards.map(function (c) { return c.dataset.navCard + ':' + Math.round(c.querySelector(':scope > .outline-card-body').getBoundingClientRect().height); }); })()"#;
    let rest = harness::probe(&tab, heights);
    let read = |v: &serde_json::Value, key: &str| -> f64 {
        v.as_array()
            .unwrap()
            .iter()
            .find_map(|r| {
                let s = r.as_str().unwrap();
                s.strip_prefix(&format!("{key}:"))
                    .map(|n| n.parse::<f64>().unwrap())
            })
            .unwrap()
    };
    let turns_natural = read(&rest, "turns");
    // 1. At rest, the toggle shuts THAT drawer and no other.
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card-toggle=\"agents\"]').click(); 'ok'",
    );
    harness::until(
        &tab,
        "Math.round(document.querySelector('[data-nav-card=\"agents\"] > .outline-card-body').getBoundingClientRect().height) === 0",
        "the agents drawer to shut, all the way",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('[data-nav-card=\"agents\"] > .outline-card-body').getBoundingClientRect().height)",
    );
    let one = harness::probe(&tab, heights);
    assert_eq!(read(&one, "agents"), 0.0, "the agents drawer shut: {one}");
    assert_eq!(
        read(&one, "turns"),
        turns_natural,
        "…and the turns drawer, at the other end of the chain, did not move: {one}"
    );
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card-toggle=\"session\"]').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(600));
    // 2. Slide the chain so the TOP drawer is part-way — the frontier — with closing behind it,
    //    then click its toggle: it finishes closing, and nothing below it moves.
    harness::eval(
        &tab,
        &format!(
            "(function(){{ document.querySelector('.session-navigator').dispatchEvent(new WheelEvent('wheel', {{ deltaY: {}, bubbles: true, cancelable: true }})); return 'ok'; }})()",
            (turns_natural * 0.4) as i64
        ),
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    let partway = harness::probe(&tab, heights);
    let mid = read(&partway, "turns");
    assert!(
        mid > 0.0 && mid < turns_natural,
        "the top drawer is part-way: {partway}"
    );
    let tasks_before = read(&partway, "tasks");
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card-toggle=\"turns\"]').click(); 'ok'",
    );
    harness::until(
        &tab,
        "Math.round(document.querySelector('[data-nav-card=\"turns\"] > .outline-card-body').getBoundingClientRect().height) === 0",
        "the toggle to finish the close the slide had started",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('[data-nav-card=\"turns\"] > .outline-card-body').getBoundingClientRect().height)",
    );
    let finished = harness::probe(&tab, heights);
    assert_eq!(
        read(&finished, "tasks"),
        tasks_before,
        "…and the drawer below it did not begin to close: {finished}"
    );
    // 3. Clicking it again gives its budget back: the drawer it shut is the one that re-opens.
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card-toggle=\"turns\"]').click(); 'ok'",
    );
    harness::until(
        &tab,
        &format!("Math.abs(document.querySelector('[data-nav-card=\"turns\"] > .outline-card-body').getBoundingClientRect().height - {turns_natural}) <= 2"),
        "the toggle to open the drawer the slide had shut",
        std::time::Duration::from_secs(5),
        "Math.round(document.querySelector('[data-nav-card=\"turns\"] > .outline-card-body').getBoundingClientRect().height)",
    );
    drop(monitor);
}

// ── the outline's info pane and the reading controls (#158, #160) ────────────────────────────

/// Spawn a small app-shell session and return the monitor and its tab, mounted.
fn shell_with_a_session(
    name: &str,
    port: u16,
) -> (
    Monitor,
    headless_chrome::Browser,
    std::sync::Arc<headless_chrome::Tab>,
) {
    let base = base(name);
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000160".to_string();
    let mut transcript = harness::long_session(4, harness::Shape::default());
    transcript += &harness::user_at(
        "a raw   turn\\n    with its own spacing",
        &harness::now_minus(60),
    );
    transcript += &harness::assistant_at("noted", &harness::now_minus(50));
    stores.claude_session(&sid, &transcript);
    let monitor = Monitor::spawn(Kind::V2, port, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.virtual-window')",
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 120)",
    );
    (monitor, browser, tab)
}

/// #158. Session / Usage / Runtime are SUBSECTION labels inside one outline card, so they have
/// to read as the column's small-caps labels — not as headings competing with the card heads
/// above them. The regression this pins is a CSS one with no markup to see: the labels became
/// buttons when the groups learned to fold (#89), and the button reset in production.css carried
/// `font:inherit` + `letter-spacing:inherit` + `text-transform:inherit` + `color:inherit`, which
/// loads after reference.css at equal specificity and so replaced all four with the body type.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_info_pane_labels_stay_smaller_than_the_card_heads() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-info-labels", 2878);
    // The info card has to be open for its groups to exist.
    harness::eval(&tab, "(function(){ var card = document.querySelector('[data-nav-card=\"session\"]'); if (card && !card.classList.contains('open')) card.querySelector('.outline-card-head').click(); return 'ok'; })()");
    harness::until(
        &tab,
        "!!document.querySelector('.session-info-label')",
        "the info pane's subsection labels",
        std::time::Duration::from_secs(10),
        "document.querySelector('[data-nav-card=\"session\"]') ? document.querySelector('[data-nav-card=\"session\"]').className : 'no card'",
    );
    let seen = harness::probe(&tab, "(function(){ var label = document.querySelector('.session-info-label'); var head = document.querySelector('[data-nav-card=\"session\"] .outline-card-head strong') || document.querySelector('.outline-card-head strong'); var ls = getComputedStyle(label), hs = getComputedStyle(head); return { label: parseFloat(ls.fontSize), head: parseFloat(hs.fontSize), transform: ls.textTransform, tracking: ls.letterSpacing, text: label.textContent.trim(), n: document.querySelectorAll('.session-info-label').length }; })()");
    assert!(
        seen["n"].as_i64().unwrap_or(0) >= 2,
        "the info pane has its subsection labels: {seen}"
    );
    let label = seen["label"].as_f64().unwrap_or(0.0);
    let head = seen["head"].as_f64().unwrap_or(0.0);
    assert!(label > 0.0 && head > 0.0, "both were measured: {seen}");
    assert!(
        label < head,
        "a subsection label is smaller than the card head it sits under: {seen}"
    );
    assert_eq!(
        seen["transform"].as_str().unwrap_or(""),
        "uppercase",
        "…and keeps the column's small-caps treatment: {seen}"
    );
    assert!(
        seen["tracking"].as_str().unwrap_or("normal") != "normal",
        "…including its tracking: {seen}"
    );
}

/// #160. The reading button draws its own glyph, because the icon set is generated and has
/// nothing for text preferences — so the thing to hold is that it draws it the SET'S way: the
/// same `.icon` class on the same 24 grid, with the weight and the size coming from the shared
/// rule rather than from inline attributes. Measured against a real neighbour on the bar, since
/// "consistent" is about what renders, not about what the markup says.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_reading_glyph_matches_its_neighbours() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-reading-glyph", 2881);
    let seen = harness::probe(&tab, "(function(){ var mine = document.querySelector('#readingBtn svg'); var bar = mine.closest('.topbar, header, .toolbar') || document.body; var others = [...bar.querySelectorAll('.iconbtn > svg.icon')].filter(function (s) { return s !== mine && s.getBoundingClientRect().width > 0; }); var sizes = others.map(function (s) { var b = s.getBoundingClientRect(); return Math.round(b.width * 10) / 10 + 'x' + Math.round(b.height * 10) / 10 + ':' + (s.closest('.iconbtn').id || s.closest('.iconbtn').className); }); var r = mine.getBoundingClientRect(); var o = others.length ? others[0].getBoundingClientRect() : null; var cs = getComputedStyle(mine); return { klass: mine.getAttribute('class'), viewBox: mine.getAttribute('viewBox'), inlineStroke: mine.getAttribute('stroke-width'), w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10, ow: o ? Math.round(o.width * 10) / 10 : -1, oh: o ? Math.round(o.height * 10) / 10 : -1, weight: cs.strokeWidth, others: others.length, sizes: sizes.slice(0, 6) }; })()");
    assert_eq!(
        seen["klass"].as_str().unwrap_or(""),
        "icon",
        "the glyph joins the shared icon rule: {seen}"
    );
    assert_eq!(
        seen["viewBox"].as_str().unwrap_or(""),
        "0 0 24 24",
        "…on the set's own grid: {seen}"
    );
    assert!(
        seen["inlineStroke"].is_null(),
        "…with no inline weight of its own to escape it: {seen}"
    );
    assert!(
        seen["others"].as_i64().unwrap_or(0) >= 1,
        "there is a neighbour to compare against: {seen}"
    );
    assert_eq!(
        seen["w"], seen["ow"],
        "…and it renders at exactly the neighbour's width: {seen}"
    );
    assert_eq!(seen["h"], seen["oh"], "…and its height: {seen}");
}

/// #160. The size control is a RELATIVE adjustment, so it must not print an absolute pixel
/// count: that pins the reader to one base size and forecloses variable sizes later.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_size_control_reads_as_a_relative_step() {
    let _serial = serial();
    // #173 moved this control: the reading popover's "Code size" row is gone, and the step now
    // lives on the bar attached to each code pane. So the fixture needs a code pane — the shared
    // `shell_with_a_session` has none, which is why the old case could only assert "or absent".
    let base = base("appshell-size-step");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000173".to_string();
    let mut transcript = harness::long_session(4, harness::Shape::default());
    transcript += &harness::user_at("write it out", &harness::now_minus(60));
    transcript += &harness::write_tool_at("cw1", "/step.py", 12, &harness::now_minus(58));
    transcript += &harness::assistant_at("written", &harness::now_minus(50));
    stores.claude_session(&sid, &transcript);
    let _monitor = Monitor::spawn(Kind::V2, 2879, &base, Some(&stores), true);
    let _browser = harness::chrome();
    let tab = _browser.new_tab().unwrap();
    _monitor.pair(&tab);
    _monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.virtual-window')",
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 120)",
    );
    // The pane is a fold; open it so its bar is in the DOM.
    harness::eval(&tab, "(function(){ document.querySelectorAll('.renderer.closed > button.renderer-head').forEach(function (h) { h.click(); }); return 'ok'; })()");
    harness::until(
        &tab,
        "!!document.querySelector('[data-code-size-val]')",
        "a code pane with its bar",
        std::time::Duration::from_secs(10),
        "document.body.innerText.slice(0, 200)",
    );
    let at_rest = harness::eval(
        &tab,
        "document.querySelector('[data-code-size-val]').textContent.trim()",
    );
    let at_rest = at_rest.as_str().unwrap_or("").to_string();
    assert!(
        !at_rest.to_lowercase().contains("px"),
        "the size reads as a step, not a pixel count: {at_rest:?}"
    );
    assert_eq!(at_rest, "0", "…and is 0 at the default: {at_rest:?}");
    harness::eval(
        &tab,
        "document.querySelector('[data-code-size=\"1\"]').click()",
    );
    let bigger = harness::eval(
        &tab,
        "document.querySelector('[data-code-size-val]').textContent.trim()",
    );
    assert_eq!(
        bigger.as_str().unwrap_or(""),
        "+1",
        "…and a step up says so: {bigger}"
    );
    // #173: and the step is THIS BLOCK's, not the page's — the baseline it is measured against
    // has not moved, which is the whole difference between the old control and this one.
    let persisted = harness::eval(&tab, "localStorage.getItem('am-prod-reading')");
    assert!(
        persisted.is_null(),
        "a per-block step writes NOTHING page-wide — the shared #45 key stays untouched \
         until a reader moves the baseline itself: {persisted}"
    );
}

/// #160. Raw is about the TEXT — showing a user turn as typed must not also restyle the turn
/// into a different kind of object. The classic page, the reference, has always drawn it as mono
/// type on the turn's own surface.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_raw_user_text_keeps_the_turns_own_surface() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-raw-surface", 2880);
    harness::eval(&tab, "document.getElementById('readingBtn').click()");
    harness::until(
        &tab,
        "!!document.querySelector('[data-reading-toggle=\"rawUser\"]')",
        "the reading popover",
        std::time::Duration::from_secs(10),
        "document.body.innerText.slice(0, 120)",
    );
    harness::eval(
        &tab,
        "document.querySelector('[data-reading-toggle=\"rawUser\"]').click()",
    );
    harness::until(
        &tab,
        "!!document.querySelector('.turn-raw')",
        "a user turn rendered raw",
        std::time::Duration::from_secs(10),
        "document.body.innerText.slice(0, 120)",
    );
    let seen = harness::probe(&tab, "(function(){ var raw = document.querySelector('.turn-raw'); var s = getComputedStyle(raw); return { bg: s.backgroundColor, border: parseFloat(s.borderTopWidth) + parseFloat(s.borderLeftWidth), mono: /mono|Mono|Menlo|Consolas|ui-monospace|SFMono/.test(s.fontFamily), pre: s.whiteSpace }; })()");
    let bg = seen["bg"].as_str().unwrap_or("").to_string();
    assert!(
        bg == "rgba(0, 0, 0, 0)" || bg == "transparent",
        "raw text sits on the turn's own surface, with no background of its own: {seen}"
    );
    assert_eq!(
        seen["border"].as_f64().unwrap_or(-1.0),
        0.0,
        "…and no box drawn around it: {seen}"
    );
    assert_eq!(
        seen["mono"], true,
        "…while still being the monospace the preference is FOR: {seen}"
    );
}

/// #163. "Show Hidden" is a toggle, and the bug was that nothing PAINTED its pressed state: the
/// class and `aria-pressed` both flipped correctly, but the only rule reaching them was the
/// generic `.navbtn.on`, which left the ordinary ink and no ring — measured on the old code as
/// a lit ground of rgba(255,255,255,.48), ink unchanged, `box-shadow: none`. So this reads the
/// RESOLVED colours rather than the class, which is the only way to see the difference: lit has
/// to differ from unlit, carry the accent and the ring, and not be the hover colour either.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_show_hidden_reads_as_a_lit_toggle() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-show-hidden", 2882);
    // Hide something, so the control has a reason to exist.
    harness::until(
        &tab,
        "!!document.querySelector('[data-ignore-op]')",
        "a tree row with a hide action",
        std::time::Duration::from_secs(15),
        "document.body.innerText.slice(0, 160)",
    );
    harness::eval(&tab, "document.querySelector('[data-ignore-op]').click()");
    harness::until(
        &tab,
        "(function(){ var b = document.getElementById('hiddenBtn'); return !!b && !b.hidden; })()",
        "the Show Hidden control to appear",
        std::time::Duration::from_secs(15),
        "(function(){ var b = document.getElementById('hiddenBtn'); return b ? 'hidden=' + b.hidden : 'absent'; })()",
    );
    let read = "(function(){ var b = document.getElementById('hiddenBtn'); var probe = document.createElement('div'); probe.style.backgroundColor = getComputedStyle(document.documentElement).getPropertyValue('--hover').trim(); document.body.appendChild(probe); var hover = getComputedStyle(probe).backgroundColor; probe.remove(); var m = document.getElementById('sidebarMiniHidden'); return { label: b.querySelector('.label').textContent.trim(), bg: getComputedStyle(b).backgroundColor, ring: getComputedStyle(b).boxShadow, ink: getComputedStyle(b).color, hover: hover, pressed: b.getAttribute('aria-pressed'), mini: m ? m.classList.contains('on') : null }; })()";
    let unlit = harness::probe(&tab, read);
    assert_eq!(
        unlit["label"].as_str().unwrap_or(""),
        "Show Hidden",
        "the label says what pressing it does: {unlit}"
    );
    assert_eq!(
        unlit["pressed"].as_str().unwrap_or(""),
        "false",
        "it starts unpressed: {unlit}"
    );
    harness::eval(&tab, "document.getElementById('hiddenBtn').click()");
    harness::until(
        &tab,
        "document.getElementById('hiddenBtn').getAttribute('aria-pressed') === 'true'",
        "the toggle to go on",
        std::time::Duration::from_secs(10),
        "document.getElementById('hiddenBtn').getAttribute('aria-pressed')",
    );
    let lit = harness::probe(&tab, read);
    assert_ne!(
        lit["bg"], unlit["bg"],
        "lit differs from unlit: {unlit} -> {lit}"
    );
    assert_ne!(
        lit["bg"], lit["hover"],
        "…and lit is NOT the hover colour, or a pressed toggle looks like a pointed-at one: {lit}"
    );
    assert_ne!(
        lit["ink"], unlit["ink"],
        "…the label takes the accent too: {unlit} -> {lit}"
    );
    assert!(
        lit["ring"].as_str().unwrap_or("none") != "none",
        "…and it carries the inset ring the other lit filter has: {lit}"
    );
    assert_eq!(
        lit["mini"], true,
        "the collapsed rail's button is the same control, so it lights too: {lit}"
    );
}

/// #162, the owner's third report: "the show-as-raw ({}) sign is always present for user-messages,
/// which is a different behavior from agent messages". Both kinds carry the control on this shell,
/// and `rawUser` makes every USER turn raw by default — so every user toggle was `.on` and pinned
/// open while the agent ones stayed ghosts. One rule now: the raw toggle reveals on hover or
/// focus, whatever kind of turn it is and whatever its state, and the ANCHOR is the one that
/// stays. (The classic page has no inconsistency to fix here — only its user turns carry a raw
/// button at all — which is why this case is the shell's alone.)
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_raw_toggle_reveals_the_same_way_on_every_turn() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-raw-visibility", 2884);
    // Turn the global raw preference ON — the state that used to pin every user turn's toggle.
    harness::eval(&tab, "document.getElementById('readingBtn').click()");
    harness::until(
        &tab,
        "!!document.querySelector('[data-reading-toggle=\"rawUser\"]')",
        "the reading popover",
        std::time::Duration::from_secs(10),
        "document.body.innerText.slice(0, 120)",
    );
    harness::eval(
        &tab,
        "document.querySelector('[data-reading-toggle=\"rawUser\"]').click()",
    );
    harness::eval(&tab, "document.getElementById('readingBtn').click()");
    harness::until(
        &tab,
        "!!document.querySelector('.turn-raw')",
        "the turns to render raw",
        std::time::Duration::from_secs(10),
        "document.body.innerText.slice(0, 120)",
    );
    let seen = harness::probe(&tab, "(function(){ function rest(sel){ var els = [...document.querySelectorAll(sel)].filter(function (e) { var r = e.getBoundingClientRect(); return r.width > 0; }); return { n: els.length, shown: els.filter(function (e) { return Number(getComputedStyle(e).opacity) > 0.05; }).length }; } return { hoverless: matchMedia('(hover: none)').matches, userRaw: rest('.turn.user .spot-link.raw-toggle'), agentRaw: rest('.turn.assistant .spot-link.raw-toggle'), userAnchor: rest('.turn.user .spot-link:not(.raw-toggle)'), agentAnchor: rest('.turn.assistant .spot-link:not(.raw-toggle)') }; })()");
    assert!(
        seen["userRaw"]["n"].as_i64().unwrap_or(0) >= 1
            && seen["agentRaw"]["n"].as_i64().unwrap_or(0) >= 1,
        "both kinds of turn carry a raw toggle, which is what makes them comparable: {seen}"
    );
    // The rule is that the two kinds behave the SAME — that is what the owner reported, and it
    // holds everywhere. Whether the toggle is hidden at rest depends on the DEVICE: a pointer
    // that cannot hover has no way to reveal it, so `@media (hover: none)` keeps it visible.
    // CI's headless Chrome matches that query and a developer's does not, which is exactly the
    // difference that made this case pass locally and fail there.
    let hoverless = seen["hoverless"].as_bool().unwrap_or(false);
    let user_all = seen["userRaw"]["shown"] == seen["userRaw"]["n"];
    let agent_all = seen["agentRaw"]["shown"] == seen["agentRaw"]["n"];
    assert_eq!(
        user_all, agent_all,
        "a user turn's raw toggle is revealed by the same rule as an agent turn's: {seen}"
    );
    if hoverless {
        assert!(
            user_all && agent_all,
            "with no hover to reveal them, both kinds keep their toggles reachable: {seen}"
        );
    } else {
        assert_eq!(
            seen["userRaw"]["shown"].as_i64().unwrap_or(-1),
            0,
            "no raw toggle is pinned open on a user turn, even with the global preference on: {seen}"
        );
        assert_eq!(
            seen["agentRaw"]["shown"].as_i64().unwrap_or(-1),
            0,
            "…and agent turns behave identically, which is the whole point: {seen}"
        );
    }
    // #167 reversed #162 here: the anchor is NOT drawn at rest either. The owner asked for the
    // chip, not a column of them — "too many #s" — so both controls follow one rule, and the
    // reader sees the pair only on the block they are pointing at. Because #162 gave each a real
    // SLOT the reveal costs no reflow, which is why hiding them is safe to do.
    if !hoverless {
        assert_eq!(
            seen["userAnchor"]["shown"].as_i64().unwrap_or(-1),
            0,
            "no anchor is drawn at rest on a user turn either: {seen}"
        );
        assert_eq!(
            seen["agentAnchor"]["shown"].as_i64().unwrap_or(-1),
            0,
            "…nor on an agent turn: {seen}"
        );
    }
}

/// #159. The reader chooses which panes the outline has at all, from a control on its caption.
/// A pane turned off must cost NOTHING — the owner's reason for wanting this is that a drawer
/// which is shut forever still charges you for its head — so the case checks that the card is
/// gone from the layout, not merely collapsed, and that the drawer machinery below it no longer
/// counts it. Turning it back on restores it open.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_chooses_which_outline_panes_exist() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-pane-picker", 2889);
    harness::until(
        &tab,
        "!!document.getElementById('navigatorPanesTrigger')",
        "the Outline title, which is the pane selector's trigger",
        std::time::Duration::from_secs(15),
        "document.querySelector('.outline-caption') ? document.querySelector('.outline-caption').innerHTML.slice(0, 120) : 'no caption'",
    );
    let read = "(function(){ var cards = [...document.querySelectorAll('#sessionNavigator > .outline-card')]; var live = cards.filter(function (c) { return c.getBoundingClientRect().height > 0; }); return { total: cards.length, live: live.length, keys: live.map(function (c) { return c.dataset.navCard; }), stack: Math.round(live.reduce(function (a, c) { return a + c.getBoundingClientRect().height; }, 0)) }; })()";
    let before = harness::probe(&tab, read);
    assert!(
        before["live"].as_i64().unwrap_or(0) >= 3,
        "the column starts with its panes: {before}"
    );
    // Open it the way a reader does — by hovering the word "Outline".
    harness::eval(&tab, "document.getElementById('navigatorPanesTrigger').dispatchEvent(new PointerEvent('pointerenter', { bubbles: false })); 'ok'");
    harness::until(
        &tab,
        "!!document.querySelector('[data-pane-toggle=\"tasks\"]')",
        "the pane list",
        std::time::Duration::from_secs(10),
        "document.getElementById('navigatorPanesMenu') ? document.getElementById('navigatorPanesMenu').className : 'absent'",
    );
    harness::eval(
        &tab,
        "document.querySelector('[data-pane-toggle=\"tasks\"]').click()",
    );
    let after = harness::probe(&tab, read);
    assert_eq!(
        after["live"].as_i64().unwrap_or(-1),
        before["live"].as_i64().unwrap_or(0) - 1,
        "the pane is gone from the column: {before} -> {after}"
    );
    assert!(
        !after["keys"]
            .as_array()
            .map(|k| k.iter().any(|v| v == "tasks"))
            .unwrap_or(true),
        "…and it is Tasks that went: {after}"
    );
    // The column is a fixed-height scroller, so its own scrollHeight says nothing. What says the
    // head cost nothing is the STACK: the cards that remain take less room than they did.
    assert!(
        after["stack"].as_i64().unwrap_or(0) + 20 < before["stack"].as_i64().unwrap_or(0),
        "…and the cards that remain take less room, so its head cost nothing: {before} -> {after}"
    );
    // Back on, and open — never restored into a state the reader has to hunt for.
    harness::eval(
        &tab,
        "document.querySelector('[data-pane-toggle=\"tasks\"]').click()",
    );
    let back = harness::probe(&tab, read);
    assert_eq!(
        back["live"], before["live"],
        "turning it back on restores it: {back}"
    );
    let open = harness::eval(&tab, "(function(){ var c = document.querySelector('[data-nav-card=\"tasks\"]'); return c ? c.classList.contains('open') : 'absent'; })()");
    assert_eq!(open, true, "…and it comes back open: {open}");
}

/// #157, the first bug the owner reported: "when a pane is explicitly toggled shut, we cannot
/// re-open it via scrolling down." It could not, because the old model read a toggle-shut pane's
/// natural height as 0 — it took no budget, so there was none to give back, and no amount of
/// scrolling reached it. One number per pane fixes that by construction: a pane the toggle shut
/// is a pane at 0, and the walk finds it like any other.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_reopens_a_toggled_shut_pane_by_pulling() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-reopen-by-pull", 2892);
    let body_of = "(function(k){ var b = document.querySelector('[data-nav-card=\"' + k + '\"] > .outline-card-body'); return b ? Math.round(b.getBoundingClientRect().height) : -1; })";
    // Every pane open, so the only thing a pull can act on is the one we shut.
    harness::eval(&tab, "(function(){ ['turns','tasks','agents','session'].forEach(function (k) { var c = document.querySelector('[data-nav-card=\"' + k + '\"]'); if (c && !c.classList.contains('open')) c.querySelector('[data-nav-card-toggle]').click(); }); return 'ok'; })()");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let opened = harness::eval(&tab, &format!("{body_of}('tasks')"));
    assert!(
        opened.as_f64().unwrap_or(0.0) > 0.0,
        "the tasks pane starts open: {opened}"
    );
    // Shut it with its own toggle — the gesture that used to make it unreachable.
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card=\"tasks\"] [data-nav-card-toggle]').click(); 'ok'",
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let shut = harness::eval(&tab, &format!("{body_of}('tasks')"));
    assert_eq!(
        shut.as_f64().unwrap_or(-1.0),
        0.0,
        "the toggle shut it: {shut}"
    );
    // Now pull. Under the old model this did nothing at all, for ever.
    harness::eval(&tab, "(function(){ document.querySelector('.session-navigator').dispatchEvent(new WheelEvent('wheel', { deltaY: -4000, bubbles: true, cancelable: true })); return 'ok'; })()");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let back = harness::eval(&tab, &format!("{body_of}('tasks')"));
    assert!(
        (back.as_f64().unwrap_or(0.0) - opened.as_f64().unwrap_or(-1.0)).abs() <= 2.0,
        "pulling brings it back, all the way: {opened} -> {shut} -> {back}"
    );
}

/// #157, the second bug: "when a pane is shut via scrolling up, a click on the header does not
/// pop it open." Now there is one state, so the head reads the same number the gesture wrote —
/// at either end the toggle simply flips it.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_opens_a_pushed_shut_pane_from_its_head() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-open-from-head", 2893);
    let body_of = "(function(k){ var b = document.querySelector('[data-nav-card=\"' + k + '\"] > .outline-card-body'); return b ? Math.round(b.getBoundingClientRect().height) : -1; })";
    harness::eval(&tab, "(function(){ ['turns','tasks','agents','session'].forEach(function (k) { var c = document.querySelector('[data-nav-card=\"' + k + '\"]'); if (c && !c.classList.contains('open')) c.querySelector('[data-nav-card-toggle]').click(); }); return 'ok'; })()");
    std::thread::sleep(std::time::Duration::from_millis(500));
    let opened = harness::eval(&tab, &format!("{body_of}('turns')"));
    assert!(
        opened.as_f64().unwrap_or(0.0) > 0.0,
        "the turns pane starts open: {opened}"
    );
    // Push far enough to shut the top pane and no further than the next one needs.
    harness::eval(&tab, &format!("(function(){{ document.querySelector('.session-navigator').dispatchEvent(new WheelEvent('wheel', {{ deltaY: {}, bubbles: true, cancelable: true }})); return 'ok'; }})()", opened.as_f64().unwrap_or(0.0) as i64 + 10));
    std::thread::sleep(std::time::Duration::from_millis(400));
    let pushed = harness::eval(&tab, &format!("{body_of}('turns')"));
    assert_eq!(
        pushed.as_f64().unwrap_or(-1.0),
        0.0,
        "the push shut it: {opened} -> {pushed}"
    );
    // Its head. Under the old model this branch could not tell a slide-shut pane from an open
    // one, because the boolean it read had never changed.
    harness::eval(
        &tab,
        "document.querySelector('[data-nav-card=\"turns\"] [data-nav-card-toggle]').click(); 'ok'",
    );
    let want = opened.as_f64().unwrap_or(-1.0);
    harness::until(
        &tab,
        &format!("Math.abs({body_of}('turns') - {want}) <= 2"),
        "the head to pop the pane open all the way",
        std::time::Duration::from_secs(5),
        &format!("{body_of}('turns') + ' (was shut at ' + {pushed} + ')'"),
    );
}

/// #157, the regression the owner hit: "I am no longer able to scroll the content in any pane
/// now." The first version of the push model swallowed every wheel at the column, so a pane's own
/// list — its `.navigator-list`, a scroller in its own right — never saw one.
///
/// The rule the fix encodes is the owner's, and it is about INTENTION FLOWING. A run of wheel
/// events with no real pause is ONE gesture and it owns whatever it started on; stopping lets the
/// next gesture aim afresh. So a list under the pointer takes the push, and the chain only gets
/// it once the list has nothing left to give — and even then not immediately, because there is
/// friction to overcome first, so a fling through a long list cannot carry on and shut every pane
/// behind it.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_a_panes_own_list_takes_the_wheel() {
    let _serial = serial();
    let base = base("appshell-pane-list-wheel");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000157".to_string();
    // Enough turns that the Turns list overflows its own max-height and is genuinely scrollable.
    stores.claude_session(&sid, &harness::long_session(60, harness::Shape::default()));
    let monitor = Monitor::spawn(Kind::V2, 2896, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length >= 60",
        "the turns to list",
        std::time::Duration::from_secs(30),
        "document.querySelectorAll('#navigatorTurns .outline-turn-row').length",
    );
    let state = "(function(){ var list = document.querySelector('[data-nav-card=\"turns\"] .navigator-list'); var body = document.querySelector('[data-nav-card=\"turns\"] > .outline-card-body'); return { listTop: Math.round(list.scrollTop), listRoom: Math.round(list.scrollHeight - list.clientHeight), body: Math.round(body.getBoundingClientRect().height) }; })()";
    let push = "(function(dy){ var list = document.querySelector('[data-nav-card=\"turns\"] .navigator-list'); list.dispatchEvent(new WheelEvent('wheel', { deltaY: dy, bubbles: true, cancelable: true })); return 'ok'; })";
    let before = harness::probe(&tab, state);
    assert!(
        before["listRoom"].as_f64().unwrap_or(0.0) > 100.0,
        "the turns list is long enough to scroll inside: {before}"
    );
    // A push with the pointer over the list scrolls the LIST, and leaves the drawer alone.
    harness::eval(&tab, &format!("({push})(120)"));
    std::thread::sleep(std::time::Duration::from_millis(200));
    let scrolled = harness::probe(&tab, state);
    assert!(
        scrolled["listTop"].as_f64().unwrap_or(0.0) > 0.0,
        "the list scrolled: {before} -> {scrolled}"
    );
    assert_eq!(
        scrolled["body"], before["body"],
        "…and the drawer did not move while the list still had room: {before} -> {scrolled}"
    );
    // Send the list to its end and let the gesture lapse, so the next push aims afresh with the
    // list unable to take it. The chain is still reachable — which is the other half of the rule,
    // and the reason the list must not contain its own overscroll.
    //
    // (The FRICTION is deliberately not asserted here. It guards the handover WITHIN a gesture —
    // a fling that exhausts the list mid-run must not carry on into the panes — whereas a push
    // begun after a pause is the reader aiming again, and there is no transition to resist. The
    // contract pins the friction directly, which is the honest place for a rule about a timer.)
    harness::eval(&tab, "(function(){ var l = document.querySelector('[data-nav-card=\"turns\"] .navigator-list'); l.scrollTop = l.scrollHeight; return 'ok'; })()");
    std::thread::sleep(std::time::Duration::from_millis(400));
    for _ in 0..4 {
        harness::eval(&tab, &format!("({push})(80)"));
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let handed = harness::probe(&tab, state);
    assert!(
        handed["body"].as_f64().unwrap_or(1e9) < before["body"].as_f64().unwrap_or(0.0),
        "…and once the list has nothing left to give, the chain takes the push: {before} -> {handed}"
    );
    drop(monitor);
}

/// #168. A record with nothing to show renders nothing. The owner photographed an Activity fold
/// whose body was a bordered card containing only "No additional details recorded." — a card
/// whose sole content is a sentence saying it has no content, costing a border, a ground and
/// ~50px of column to repeat what the head already said.
///
/// The second half matters as much: a head with nothing under it must not present itself as
/// openable, or removing the placeholder just trades a useless card for a fold that opens onto
/// emptiness.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_renders_nothing_for_a_record_with_nothing_to_show() {
    let _serial = serial();
    let base = base("appshell-empty-body");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000168".to_string();
    // A tool call whose result carries no text at all — the shape that produced the screenshot.
    let mut transcript = harness::long_session(8, harness::Shape::default());
    transcript += &harness::user_at("run the thing", &harness::now_minus(90));
    transcript += &harness::tool_open_at("t-empty", &harness::now_minus(80));
    transcript += &harness::tool_result_text("t-empty", "", &harness::now_minus(70));
    transcript += &harness::assistant_at("done", &harness::now_minus(60));
    stores.claude_session(&sid, &transcript);
    let monitor = Monitor::spawn(Kind::V2, 2897, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.virtual-window')",
        "the app shell to mount the fixture",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 120)",
    );
    // Open everything, so an empty body would have been rendered if there were one.
    for _ in 0..2 {
        harness::eval(&tab, "(function(){ document.querySelectorAll('.process-surface.closed [data-process-toggle]').forEach(function (b) { b.click(); }); document.querySelectorAll('.renderer.closed > button.renderer-head').forEach(function (h) { h.click(); }); return 'ok'; })()");
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let seen = harness::probe(&tab, "(function(){ var t = document.querySelector('.transcript'); var empties = [...t.querySelectorAll('.renderer-output')].filter(function (o) { return !o.innerHTML.trim(); }).length; var openable = [...t.querySelectorAll('.renderer')].filter(function (r) { var body = r.querySelector(':scope > .renderer-body'); var head = r.querySelector(':scope > button.renderer-head'); return head && body && !body.textContent.trim(); }).length; return { placeholder: t.innerText.indexOf('No additional details recorded.'), emptyOutputs: empties, openableButEmpty: openable }; })()");
    assert_eq!(
        seen["placeholder"].as_i64().unwrap_or(0),
        -1,
        "nothing says it has nothing to say: {seen}"
    );
    assert_eq!(
        seen["openableButEmpty"].as_i64().unwrap_or(-1),
        0,
        "…and no head offers to open onto emptiness: {seen}"
    );
    drop(monitor);
}

/// #166. The transcript's ranking, read at a glance: what the agent CHOSE to tell the reader —
/// its Progress commentary — outranks what it did to get there, the Thinking and Activity that
/// carried it. The owner reported the inverse: "the progress blocks are the ones that agents
/// intend to communicate to the user, while thinking and activities are really background."
///
/// Asserted as CONTRAST against the ground rather than as darkness, so the same case holds in
/// both themes: in light the message is darker than the background labels, in dark it is
/// lighter, and in both it stands further from the page than they do.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_ranks_progress_above_thinking_and_activity() {
    let _serial = serial();
    let base = base("appshell-progress-rank");
    let stores = Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000166".to_string();
    let mut transcript = harness::long_session(6, harness::Shape::default());
    transcript += &harness::user_at("do the thing", &harness::now_minus(120));
    transcript += &harness::thinking_at("weighing how to start", &harness::now_minus(110));
    // COMMENTARY, and the signal for it is `stop_reason: "tool_use"` — Claude writes no phase
    // field, but a completed message that stopped to call a tool has introduced more work in
    // this turn, which is what makes its prose Progress rather than the answer
    // (`assistant_phase`, claude/model.rs). The harness's own builder sets no stop_reason, so
    // this record is written here rather than borrowed.
    transcript += &format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"text\",\"text\":\"Reading the config first.\"}}]}},\"timestamp\":\"{}\"}}\n",
        harness::now_minus(100)
    );
    transcript += &harness::tool_open_at("t-rank", &harness::now_minus(90));
    transcript += &harness::tool_result_at("t-rank", &harness::now_minus(80));
    transcript += &harness::assistant_at("done", &harness::now_minus(70));
    stores.claude_session(&sid, &transcript);
    let monitor = Monitor::spawn(Kind::V2, 2898, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    monitor.open(&tab, &format!("?ui=app&session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('.process-commentary-copy') && !!document.querySelector('.renderer[data-renderer-kind=\"thinking\"] .renderer-title')",
        "a progress commentary and a thinking head to render",
        std::time::Duration::from_secs(20),
        "document.body.innerText.slice(0, 160)",
    );
    let seen = harness::probe(&tab, "(function(){ function lum(c){ var m = c.match(/[\\d.]+/g).map(Number); var f = m.slice(0,3).map(function (v) { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }); return 0.2126*f[0] + 0.7152*f[1] + 0.0722*f[2]; } function read(sel){ var e = document.querySelector(sel); if (!e) return null; var cs = getComputedStyle(e); return { size: parseFloat(cs.fontSize), lum: lum(cs.color) }; } function ground(el){ for (var e = el; e; e = e.parentElement) { var c = getComputedStyle(e).backgroundColor; var m = c.match(/[\\d.]+/g); if (m && (m.length < 4 || Number(m[3]) > 0)) return lum(c); } return 1; } var bg = ground(document.querySelector('.process-commentary-copy')); var msg = read('.process-commentary-copy'); var label = read('.process-commentary-label'); var think = read('.renderer[data-renderer-kind=\"thinking\"] .renderer-title'); return { bg: bg, msgSize: msg.size, msgContrast: Math.abs(msg.lum - bg), labelSize: label.size, thinkSize: think.size, thinkContrast: Math.abs(think.lum - bg) }; })()");
    assert!(
        seen["msgContrast"].as_f64().unwrap_or(0.0) > seen["thinkContrast"].as_f64().unwrap_or(1.0),
        "the message stands further from the page than the background labels do: {seen}"
    );
    assert!(
        seen["msgSize"].as_f64().unwrap_or(0.0) >= seen["thinkSize"].as_f64().unwrap_or(99.0),
        "…and is at least as large: {seen}"
    );
    assert!(
        seen["labelSize"].as_f64().unwrap_or(0.0) >= seen["thinkSize"].as_f64().unwrap_or(99.0),
        "…as is the Progress label itself: {seen}"
    );
    drop(monitor);
}

/// #170. Info leaves the drawer stack, and what was LIVE in it stays visible. The design review
/// caught the regression this could have been: `row.cost` had exactly two homes in the app shell,
/// both inside the Info pane, and the classic page — the reference — carries cost on every session
/// card. Hiding it behind a hover would have made the shell worse than the reference on the one
/// number a reader watches through a long run, and it would have killed the derivative: glance,
/// glance, glance gives you the RATE for free; a self-hiding popup gives two unrelated scalars.
///
/// So cost and context-left live in a one-line strip at the foot of the column, and the reference
/// material goes behind the glyph beside them — peek on hover, PIN on click, because a surface you
/// read inside cannot close when the pointer leaves.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_keeps_cost_visible_with_info_behind_a_glyph() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-info-footer", 2899);
    harness::until(
        &tab,
        "!!document.getElementById('outlineFooter') && !!document.getElementById('infoPopover')",
        "the outline footer strip and its info popover",
        std::time::Duration::from_secs(15),
        "document.querySelector('.session-navigator') ? 'no footer' : 'no column'",
    );
    let state = "(function(){ var f = document.getElementById('outlineFooter'); var pop = document.getElementById('infoPopover'); var r = f.getBoundingClientRect(); return { cards: [...document.querySelectorAll('#sessionNavigator > .outline-card')].map(function (c) { return c.dataset.navCard; }), footerH: Math.round(r.height), costShown: !!f.querySelector('.outline-footer-cost').textContent.trim(), short: f.querySelector('.outline-footer-cost').dataset.short, infoInside: !!pop.querySelector('#navigatorSession'), open: pop.classList.contains('open') }; })()";
    let seen = harness::probe(&tab, state);
    // Info is not a pane any more, and no pane was lost in the move.
    assert!(
        !seen["cards"]
            .as_array()
            .map(|c| c.iter().any(|v| v == "session"))
            .unwrap_or(true),
        "Info is out of the drawer stack: {seen}"
    );
    assert!(
        seen["cards"].as_array().map(Vec::len).unwrap_or(0) >= 3,
        "…and the other panes are still there: {seen}"
    );
    // What was live is still visible without asking for it.
    assert!(
        seen["footerH"].as_f64().unwrap_or(0.0) > 10.0 && seen["costShown"] == true,
        "the strip carries cost, on screen, with nothing to open: {seen}"
    );
    assert!(
        seen["short"].as_str().map(|v| !v.is_empty()).unwrap_or(false),
        "…and an abbreviated form for the 40px rail, so folding does not stop the reader watching it: {seen}"
    );
    assert_eq!(
        seen["infoInside"], true,
        "the reference material moved wholesale, so its folding groups keep working: {seen}"
    );
    assert_eq!(seen["open"], false, "…and it starts shut: {seen}");
    // Peek on hover.
    harness::eval(&tab, "document.querySelector('.outline-footer-info').dispatchEvent(new PointerEvent('pointerenter', { bubbles: false })); 'ok'");
    harness::until(
        &tab,
        "document.getElementById('infoPopover').classList.contains('open')",
        "the glyph to peek",
        std::time::Duration::from_secs(5),
        "document.getElementById('infoPopover').className",
    );
    // Leaving closes a peek…
    harness::eval(&tab, "document.querySelector('.outline-footer-info').dispatchEvent(new PointerEvent('pointerleave', { bubbles: false })); 'ok'");
    harness::until(
        &tab,
        "!document.getElementById('infoPopover').classList.contains('open')",
        "the peek to close when the pointer leaves",
        std::time::Duration::from_secs(5),
        "document.getElementById('infoPopover').className",
    );
    // …but a PIN survives it, which is what makes the panel readable and its values selectable.
    harness::eval(
        &tab,
        "document.querySelector('.outline-footer-info').click(); 'ok'",
    );
    harness::eval(&tab, "document.querySelector('.outline-footer-info').dispatchEvent(new PointerEvent('pointerleave', { bubbles: false })); 'ok'");
    std::thread::sleep(std::time::Duration::from_millis(400));
    let pinned = harness::probe(&tab, state);
    assert_eq!(
        pinned["open"], true,
        "a pinned panel stays open after the pointer leaves: {pinned}"
    );
}

/// #159 layout, reported with a screenshot: the Outline title had drifted to the centre and the
/// selector's rows were 24px squares with their labels spilling outside the menu.
///
/// One cause for both, and it is a cascade collision no static check could see:
/// `.outline-caption button` in reference.css is a DESCENDANT rule — `margin-left:auto` plus a
/// 24x24 grid box — written for the single icon button the caption used to hold. Making the title
/// a button (#159) and putting the menu inside the caption handed both of them that styling.
/// Measured, not asserted from the CSS, because that is the only way this class of bug shows.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_outline_caption_and_its_menu_lay_out() {
    let _serial = serial();
    let (_monitor, _browser, tab) = shell_with_a_session("appshell-caption-layout", 2901);
    harness::until(
        &tab,
        "!!document.getElementById('navigatorPanesTrigger')",
        "the Outline title",
        std::time::Duration::from_secs(15),
        "document.querySelector('.outline-caption') ? document.querySelector('.outline-caption').innerHTML.slice(0, 120) : 'no caption'",
    );
    harness::eval(&tab, "document.getElementById('navigatorPanesTrigger').dispatchEvent(new PointerEvent('pointerenter', { bubbles: false })); 'ok'");
    harness::until(
        &tab,
        "!!document.querySelector('#navigatorPanesMenu .pane-option')",
        "the pane rows",
        std::time::Duration::from_secs(10),
        "document.getElementById('navigatorPanesMenu') ? document.getElementById('navigatorPanesMenu').className : 'absent'",
    );
    let seen = harness::probe(&tab, "(function(){ function box(e){ var r = e.getBoundingClientRect(); return { l: Math.round(r.left), r: Math.round(r.right), w: Math.round(r.width), h: Math.round(r.height) }; } var cap = document.querySelector('.outline-caption'); var title = document.getElementById('navigatorPanesTrigger'); var menu = document.getElementById('navigatorPanesMenu'); var rows = [...menu.querySelectorAll('.pane-option')]; return { titleLeftAligned: Math.abs(title.getBoundingClientRect().left - (cap.getBoundingClientRect().left + parseFloat(getComputedStyle(cap).paddingLeft))) <= 2, title: box(title), rowsInside: rows.every(function (a) { var b = a.getBoundingClientRect(), m = menu.getBoundingClientRect(); return b.left >= m.left - 1 && b.right <= m.right + 1; }), narrowest: Math.min.apply(null, rows.map(function (a) { return Math.round(a.getBoundingClientRect().width); })), rowCount: rows.length, labels: rows.map(function (a) { return a.textContent.trim(); }) }; })()");
    assert_eq!(
        seen["titleLeftAligned"], true,
        "the Outline title sits at the caption's left edge, not adrift in the middle: {seen}"
    );
    assert!(
        seen["title"]["w"].as_f64().unwrap_or(0.0) > 30.0,
        "…and is sized to its word, not squeezed into an icon button's 24px box: {seen}"
    );
    assert_eq!(
        seen["rowsInside"], true,
        "every row of the menu lies inside the menu: {seen}"
    );
    assert!(
        seen["narrowest"].as_f64().unwrap_or(0.0) > 100.0,
        "…at a readable width rather than an icon's: {seen}"
    );
    // #170 moved Info out of the stack, so it is not a pane the reader can turn off any more.
    assert!(
        !seen["labels"]
            .as_array()
            .map(|l| l.iter().any(|v| v == "Info"))
            .unwrap_or(true),
        "Info is not listed as a pane: {seen}"
    );
}

/// #186: the tasks and agents panes open on what is LIVE, and say what they are holding back.
///
/// The owner's report was that a pane listing everything "renders it useless" on a long session.
/// So the default is live-only — running and pending tasks, running sub-agents — with one control
/// per pane for the rest. Two things this case insists on beyond the filtering itself: the head's
/// counts keep naming BOTH halves, so what is hidden is never a secret; and the choice survives a
/// reload, because a filter a reader has to set again every time is a filter they stop using.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor-v2"]
fn the_app_shell_panes_open_on_what_is_live() {
    let _serial = serial();
    let base = base("appshell-live-only");
    let stores = Stores::new(&base);
    let sid = "eeeeeeee-0000-4000-8000-000000000186".to_string();
    // One sub-agent that FINISHED and one still out. What closes a spawn is the completion
    // NOTIFICATION, not the tool result — measured while building this: a spawn plus its
    // `agent-result` leaves the agent `running`, because the result names the child without
    // saying it is done. And a spawn with NO result is not a child at all: `collect_child_refs`
    // keeps only a `SubAgent` with a non-empty `agent_id`, and the id is what the result carries.
    let mut transcript = harness::long_session(20, harness::Shape::default());
    transcript += &harness::agent_spawn("call_done", "Explore", 21);
    transcript += &harness::agent_result("call_done", "aDone-186", "Explore", 22);
    transcript += &harness::agent_finished("aDone-186", "look around", 23);
    transcript += &harness::agent_spawn("call_live", "general-purpose", 24);
    transcript += &harness::agent_result("call_live", "aLive-186", "general-purpose", 25);
    transcript += &harness::long_session(6, harness::Shape::default());
    stores.claude_session(&sid, &transcript);
    for child in ["aDone-186", "aLive-186"] {
        stores.claude_child(
            &sid,
            child,
            &harness::long_session(4, harness::Shape::default()),
        );
    }
    stores.claude_tasks(
        &sid,
        &[
            ("1", "one, done", "completed"),
            ("2", "two, done", "completed"),
            ("3", "three, running", "in_progress"),
            ("4", "four, pending", "pending"),
        ],
    );
    let monitor = Monitor::spawn(Kind::V2, 2885, &base, Some(&stores), true);
    let browser = harness::chrome();
    let tab = browser.new_tab().unwrap();
    monitor.pair(&tab);
    let open = |tab: &headless_chrome::Tab| {
        tab.navigate_to(&format!("http://127.0.0.1:2885/?ui=app&session={sid}"))
            .unwrap();
        tab.wait_until_navigated().unwrap();
        harness::until(tab, "!!document.querySelector('.virtual-window') && document.querySelector('.virtual-window').children.length > 0", "the app shell to mount the fixture", std::time::Duration::from_secs(30), "document.body.innerText.slice(0, 120)");
        // Both panes have to EXIST and be open before anything can be counted in them.
        harness::eval(tab, "for (const key of ['tasks', 'agents']) { var c = document.querySelector('[data-nav-card=\"' + key + '\"]'); if (c && !c.classList.contains('open')) document.querySelector('[data-nav-card-toggle=\"' + key + '\"]').click(); } 'ok'");
        harness::until(
            tab,
            "!!document.getElementById('tasksLiveOnly') && !!document.getElementById('agentsLiveOnly')",
            "both live-only controls to be built",
            std::time::Duration::from_secs(10),
            "document.querySelector('.session-navigator').innerText.slice(0, 200)",
        );
    };
    let state = "(function(){ var q = function (s) { return [...document.querySelectorAll(s)].map(function (e) { return e.textContent.trim(); }); }; return { tasks: q('#navigatorWork .work-task strong'), groups: q('#navigatorWork .work-group span:first-child'), agents: q('#navigatorAgents .outline-agent-copy strong'), taskCount: document.getElementById('navigatorWorkCount').textContent.replace(/\\s+/g, ' ').trim(), agentCount: document.getElementById('navigatorAgentCount').textContent.replace(/\\s+/g, ' ').trim(), tasksOn: document.getElementById('tasksLiveOnly').getAttribute('aria-pressed'), agentsOn: document.getElementById('agentsLiveOnly').getAttribute('aria-pressed'), agentsEmpty: (document.querySelector('#navigatorAgents .activity-empty') || {}).textContent || '' }; })()";

    open(&tab);
    let live = harness::probe(&tab, state);
    assert_eq!(live["tasksOn"], "true", "live-only is the default: {live}");
    assert_eq!(live["agentsOn"], "true", "…in both panes: {live}");
    let tasks: Vec<String> = live["tasks"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        tasks,
        vec!["three, running".to_string(), "four, pending".to_string()],
        "the two completed tasks are held back, the running and the pending one are not — a \
         PENDING task is live work, it just has not started: {live}"
    );
    let agents: Vec<String> = live["agents"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(agents.len(), 1, "only the sub-agent still out: {live}");
    // The head must keep naming both halves, or a short list is indistinguishable from no data.
    assert!(
        live["taskCount"].as_str().unwrap_or("").contains('2'),
        "the head still counts what is hidden: {live}"
    );

    // …and the rest is one click away.
    harness::eval(&tab, "document.getElementById('tasksLiveOnly').click(); document.getElementById('agentsLiveOnly').click(); 'ok'");
    let all = harness::probe(&tab, state);
    assert_eq!(all["tasksOn"], "false", "the control flips: {all}");
    assert_eq!(
        all["tasks"].as_array().map(|a| a.len()),
        Some(4),
        "every task is back: {all}"
    );
    assert_eq!(
        all["agents"].as_array().map(|a| a.len()),
        Some(2),
        "…and both sub-agents: {all}"
    );

    // A filter a reader has to set again on every reload is a filter they stop using.
    open(&tab);
    let after = harness::probe(&tab, state);
    assert_eq!(
        after["tasksOn"], "false",
        "the choice survived the reload: {after}"
    );
    assert_eq!(
        after["tasks"].as_array().map(|a| a.len()),
        Some(4),
        "…and so did what it shows: {after}"
    );
    drop(monitor);
}
