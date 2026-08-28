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
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Each test gets its OWN scratch: cargo runs them on parallel threads, and a shared
/// directory that every test wipes on entry means whichever starts second deletes the
/// other's fixture mid-run.
fn base(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cr-browser-follow-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Serialize the suite.
///
/// Every test here points the run at its own cache root with `std::env::set_var`, and that is
/// PROCESS-global — so two tests in parallel overwrite each other's root and one of them ends up
/// serving from a directory the other is tearing down. `base(name)` gave each test its own
/// directory; it could not give them their own environment. The tests are each tens of seconds
/// of real browser work, so serializing costs little and removes a flake that reads like a
/// viewport regression.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn user(t: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}]}},\"timestamp\":\"2026-08-21T10:{s:02}:00Z\"}}\n"
    )
}
fn assistant(t: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":5,\"output_tokens\":8}}}},\"timestamp\":\"2026-08-21T10:{s:02}:00Z\"}}\n"
    )
}
fn tool_open(id: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo {id}\"}}}}]}},\"timestamp\":\"2026-08-21T10:{s:02}:00Z\"}}\n"
    )
}
fn tool_result(id: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"out line\\nout line\\nout line\\n\"}}]}},\"timestamp\":\"2026-08-21T10:{s:02}:00Z\"}}\n"
    )
}
fn thinking(t: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"thinking\",\"thinking\":\"{t}\"}}]}},\"timestamp\":\"2026-08-21T10:{s:02}:00Z\"}}\n"
    )
}

fn append(path: &Path, s: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(s.as_bytes()).unwrap();
    f.flush().unwrap();
}

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
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/release/agent-monitor-v2");
    if !bin.is_file() {
        eprintln!("skip: build agent-monitor-v2 --release first");
        return;
    }
    let mut child = std::process::Command::new(&bin)
        .args(["--port", "2831"])
        .env("XDG_CACHE_HOME", &base)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("v2 starts");
    std::thread::sleep(std::time::Duration::from_millis(1500));

    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .unwrap(),
    )
    .expect("chrome launches");
    let tab = browser.new_tab().unwrap();
    // Any session the machine has will do — the assertion is about LAYOUT, not content.
    tab.navigate_to("http://127.0.0.1:2831/api/sessions")
        .unwrap();
    tab.wait_until_navigated().unwrap();
    let listing = tab
        .evaluate("document.body.innerText", true)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let id = serde_json::from_str::<serde_json::Value>(&listing)
        .ok()
        .and_then(|v| v["sessions"][0]["id"].as_str().map(str::to_string));
    let Some(id) = id else {
        let _ = child.kill();
        let _ = child.wait(); // reap it: a killed child left unwaited is a zombie for the run
        eprintln!("skip: no sessions on this machine to compose");
        return;
    };
    tab.navigate_to(&format!("http://127.0.0.1:2831/?session={id}"))
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
        noFrame: document.querySelectorAll('iframe').length === 0
      });
    })()"#;
    let seen = tab
        .evaluate(probe, true)
        .ok()
        .and_then(|r| r.value)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait(); // reap it: a killed child left unwaited is a zombie for the run
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
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target/release/agent-monitor-v2");
    if !bin.is_file() {
        eprintln!("skip: build agent-monitor-v2 --release first");
        return;
    }
    let browser = headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .unwrap(),
    )
    .expect("chrome launches");

    // `paired` says whether to pass `--pair`; both runs share an isolated state dir, so the
    // second one finds the token the first never minted.
    let probe = |paired: bool, port: &str| -> (bool, i64) {
        let mut cmd = std::process::Command::new(&bin);
        cmd.args(["--port", port])
            .env("XDG_CACHE_HOME", &base)
            .env("AGENT_MONITOR_STATE", base.join("state"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if paired {
            cmd.arg("--pair");
        }
        let mut child = cmd.spawn().expect("v2 starts");
        std::thread::sleep(std::time::Duration::from_millis(1800));
        let tab = browser.new_tab().unwrap();
        // Paired, the bare URL 401s — the token file is the one the run just minted, and it
        // rides the URL exactly as the printed one does.
        let token = std::fs::read_to_string(base.join("state").join("auth-token"))
            .map(|t| format!("?token={}", t.trim()))
            .unwrap_or_default();
        tab.navigate_to(&format!("http://127.0.0.1:{port}/{token}"))
            .unwrap();
        tab.wait_until_navigated().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2500));
        // `PAIRED` is a closure-local var in the shell's IIFE, so the AFFORDANCE is what gets
        // counted — which is the thing the rule is actually about anyway.
        let buttons = tab
            .evaluate("document.querySelectorAll('.v2send').length", true)
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        let rows = tab
            .evaluate("document.querySelectorAll('.v2row').length", true)
            .ok()
            .and_then(|r| r.value)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let _ = tab.close(true);
        let _ = child.kill();
        let _ = child.wait();
        (rows > 0, buttons)
    };

    let (had_rows, unpaired_buttons) = probe(false, "2841");
    if !had_rows {
        eprintln!("skip: no sessions on this machine to compose with");
        return;
    }
    assert_eq!(
        unpaired_buttons, 0,
        "unpaired: every write route 401s, so no row offers to send"
    );
    let (_, paired_buttons) = probe(true, "2842");
    assert!(
        paired_buttons > 0,
        "paired: the sessions that can be resumed or injected offer it ({paired_buttons})"
    );
}
