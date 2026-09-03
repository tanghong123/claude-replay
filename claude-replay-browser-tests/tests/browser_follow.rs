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
            blocks: (document.getElementById("stream") || {childElementCount:-1}).childElementCount
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
/// Note the hold ticks on a 16 ms timer rather than `requestAnimationFrame`, which is what
/// makes this testable: this harness drives the page in a background tab, where rAF never
/// ticks at all (verified — an rAF version of the hold recorded zero ticks here).
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
                var above = Array.prototype.slice.call(document.querySelectorAll("#stream > *"))
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
        m.pair(&tab);
        // The affordance under test is the classic splice's: ask for it by name, since the
        // app shell is what `/` serves by default.
        m.open(&tab, "?ui=classic");
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let buttons = harness::eval(&tab, "document.querySelectorAll('.v2send').length")
            .as_i64()
            .unwrap_or(-1);
        let rows = harness::eval(&tab, "document.querySelectorAll('.v2row').length")
            .as_i64()
            .unwrap_or(0);
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
    until(&tab, "!![...document.querySelectorAll('#navigatorSession .session-info-row')].find(r => r.querySelector('span') && r.querySelector('span').textContent === 'status')", "the app shell's info pane");
    let status = eval(&tab, "([...document.querySelectorAll('#navigatorSession .session-info-row')].find(r => r.querySelector('span').textContent === 'status') || {}).querySelector('strong').textContent");
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
    let migrated = eval(&tab, "JSON.stringify({ms: getComputedStyle(document.documentElement).getPropertyValue('--ms').trim(), key: localStorage.getItem('am-prod-reading'), old: localStorage.getItem('claude-replay-export-ms')})");
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
