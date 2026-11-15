//! How agent-monitor lays itself out at each size: a width study (#s517), not a gate.
//!
//! One hermetic monitor (fixture sessions written by the harness's builders, nothing of this
//! machine's own) and one headless Chrome. Two surfaces:
//!
//! - `app`: the app shell, the window at a row of sizes that straddle every edge
//!   `design/pane-layout.md` names (the phone reader under 490px, the drawer under 554px, the rail,
//!   the list narrowing, the outline folding, the pane beside or over the session, the 760px phone
//!   query and the 1180px band), with a mouse; phones upright and sideways and tablets with touch;
//!   the right pane shut and open; a few cells in the dark theme.
//! - `host`: the session view mounted alone in the test host page
//!   (`tests/fixtures/host/session-view-host.html`), the window held at 1700×1000 and the host's
//!   PANE given each width (and two short touch panes), the right pane shut and open, mouse and
//!   touch.
//!
//! Every cell is a fresh load with the page's remembered choices cleared, so no cell inherits a fold
//! from the one before. For each it records the layout classes `#app` and `#sessionView` wear, the
//! box of each part (the session list, the session, the right pane, the outline), the chrome's
//! controls that a pointer cannot reach at their centre (hit-tested with `elementFromPoint`; what
//! sits inside a scroller — the transcript, the session tree, the pane's body, the outline's lists —
//! is left out, as is a layer parked off the window), how far the page, the view and the transcript
//! overflow sideways, and the width of the transcript's text column, beside a screenshot.
//!
//! Run it from the workspace root, after building the monitor in release:
//!
//! ```text
//! cargo build --release -p claude-monitor --features claude-monitor/codex-native
//! cargo run -p claude-replay-browser-tests --example study_widths -- [--only <text>] [--run <name>]
//! ```
//!
//! `--only` keeps the cells whose id holds the text (`app-`, `host-`, `touch`, `w1180`…); `--run`
//! names the output directory (a timestamp by default). Everything goes to
//! `target/study-widths/<run>/`, out of git: `report.md` (a table per surface), `contact.html` (the
//! screenshots in a grid), `app-sweep.mp4` and `host-sweep.mp4` (each cell a captioned frame, in the
//! order of the table; the frames are composed by the same Chrome, since ffmpeg's caption filter is
//! not in every build), and `cells/` and `shots/`, one JSON and one PNG per cell, written as each
//! cell finishes. `target/study-widths/latest` points at the last run. It is an example rather than
//! an `#[ignore]`d case so the browser suite's `--ignored` runs never sweep it in. Nothing in it is a
//! verdict: run it again after a change to see the after.

#[path = "../tests/harness/mod.rs"]
mod harness;

use serde_json::{json, Value};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A port no case uses.
const PORT: u16 = 3517;
/// The session every cell opens.
const SID: &str = "51700000-0000-4000-8000-000000000517";
/// The window the host's pane is placed in.
const HOST_WINDOW: (u32, u32) = (1700, 1000);
/// The host pane's left edge: the host's own page shows on both sides of it.
const HOST_LEFT: u32 = 40;
/// The host page's bar above its pane.
const HOST_TOP: u32 = 40;
/// A desktop window's height.
const DESKTOP_H: u32 = 900;

#[derive(Clone, Copy, PartialEq)]
enum Surface {
    App,
    Host,
}

impl Surface {
    fn name(self) -> &'static str {
        match self {
            Surface::App => "app",
            Surface::Host => "host",
        }
    }
}

/// How the screen is driven: a mouse, or touch at a device scale, with the safe area of a phone.
#[derive(Clone, Copy)]
enum Input {
    Mouse,
    Touch {
        dpr: f64,
        mobile: bool,
        insets: (u32, u32, u32, u32),
    },
}

#[derive(Clone)]
struct Cell {
    surface: Surface,
    id: String,
    caption: String,
    /// The window (app) or the host's pane (host).
    w: u32,
    h: u32,
    input: Input,
    pane: bool,
    dark: bool,
}

const MOUSE_WIDTHS: [u32; 30] = [
    360, 390, 430, 480, 500, 540, 554, 600, 640, 700, 760, 761, 800, 820, 860, 900, 960, 1012,
    1024, 1040, 1080, 1100, 1180, 1181, 1280, 1340, 1440, 1500, 1561, 1700,
];
const HOST_WIDTHS: [u32; 17] = [
    320, 380, 460, 489, 490, 560, 600, 700, 760, 761, 880, 900, 1000, 1100, 1180, 1400, 1600,
];

fn cells() -> Vec<Cell> {
    let mut out = Vec::new();
    let mut push =
        |surface: Surface, what: &str, w: u32, h: u32, input: Input, pane: bool, dark: bool| {
            let kind = match input {
                Input::Mouse => "mouse",
                Input::Touch { .. } => "touch",
            };
            let id = format!(
                "{}-{kind}-w{w}x{h}-pane-{}{}",
                surface.name(),
                if pane { "open" } else { "shut" },
                if dark { "-dark" } else { "" }
            );
            let caption = format!(
                "{} {w}×{h} · {what} · {kind} · pane {}{}",
                match surface {
                    Surface::App => "app shell, window",
                    Surface::Host => "view in a host pane",
                },
                if pane { "open" } else { "shut" },
                if dark { " · dark" } else { "" }
            );
            out.push(Cell {
                surface,
                id,
                caption,
                w,
                h,
                input,
                pane,
                dark,
            });
        };
    let phone = |insets| Input::Touch {
        dpr: 3.0,
        mobile: true,
        insets,
    };
    let tablet = Input::Touch {
        dpr: 2.0,
        mobile: true,
        insets: (0, 0, 0, 0),
    };
    // The app shell: a desktop window narrowing, the pane shut then open, so the sweep reads as one
    // window closing in.
    for pane in [false, true] {
        for &w in MOUSE_WIDTHS.iter().rev() {
            push(
                Surface::App,
                "desktop",
                w,
                DESKTOP_H,
                Input::Mouse,
                pane,
                false,
            );
        }
    }
    let touch: [(&str, u32, u32, Input); 8] = [
        ("phone upright", 390, 844, phone(harness::PORTRAIT_INSETS)),
        ("phone upright", 430, 932, phone(harness::PORTRAIT_INSETS)),
        ("phone sideways", 844, 390, phone(harness::SIDEWAYS_INSETS)),
        ("phone sideways", 932, 430, phone(harness::SIDEWAYS_INSETS)),
        ("phone sideways", 956, 440, phone(harness::SIDEWAYS_INSETS)),
        ("tablet upright", 744, 1133, tablet),
        ("tablet upright", 820, 1180, tablet),
        ("tablet sideways", 1180, 820, tablet),
    ];
    for pane in [false, true] {
        for (what, w, h, input) in touch {
            push(Surface::App, what, w, h, input, pane, false);
        }
    }
    for (what, w, h, input, pane) in [
        ("desktop", 1440, DESKTOP_H, Input::Mouse, false),
        ("desktop", 1440, DESKTOP_H, Input::Mouse, true),
        ("desktop", 820, DESKTOP_H, Input::Mouse, true),
        (
            "phone upright",
            390,
            844,
            phone(harness::PORTRAIT_INSETS),
            true,
        ),
    ] {
        push(Surface::App, what, w, h, input, pane, true);
    }
    // The view in a host's pane: the window held, the pane narrowing.
    let host_h = HOST_WINDOW.1 - HOST_TOP;
    let host_touch = Input::Touch {
        dpr: 1.0,
        mobile: false,
        insets: (0, 0, 0, 0),
    };
    for input in [Input::Mouse, host_touch] {
        for pane in [false, true] {
            for &w in HOST_WIDTHS.iter().rev() {
                push(Surface::Host, "pane", w, host_h, input, pane, false);
            }
        }
    }
    for pane in [false, true] {
        for (w, h) in [(932, 430), (700, 440)] {
            push(Surface::Host, "short pane", w, h, host_touch, pane, false);
        }
    }
    for pane in [false, true] {
        push(
            Surface::Host,
            "pane",
            1000,
            host_h,
            Input::Mouse,
            pane,
            true,
        );
    }
    out
}

// ── the world ───────────────────────────────────────────────────────────────────────────────

/// The fixture: one session worth reading (prose, Markdown, tool calls, a file it read, a pasted
/// image, a sub-agent, tasks) and a few others in three projects, so the list has rows. Every word
/// is written here; the paths are the case's own scratch.
fn world(base: &Path) -> (harness::Stores, String) {
    let stores = harness::Stores::new(base);
    let projects = ["harbour", "orchard", "lighthouse"];
    for p in projects {
        std::fs::create_dir_all(base.join(p)).unwrap();
    }
    let file = base.join("harbour").join("tide-notes.md");
    std::fs::write(
        &file,
        "# Tide notes\n\nThe harbour's tide table, kept by hand.\n\n## This week\n\n| Day | High water | Low water |\n| --- | --- | --- |\n| Monday | 06:12 | 12:30 |\n| Tuesday | 06:58 | 13:15 |\n| Wednesday | 07:41 | 13:59 |\n\n## Notes\n\n- The spring tide peaks on Thursday.\n- The north mooring dries at low water; move the dinghy by Wednesday evening.\n- Keep the old table for comparison until the new one has been checked twice.\n",
    )
    .unwrap();
    let path = file.display().to_string();
    let ts = |m: u32, s: u32| format!("2026-11-15T09:{m:02}:{s:02}.000Z");
    let cwd = |jsonl: String, project: &str| {
        jsonl.replace(
            "\"cwd\":\"/r\"",
            &format!("\"cwd\":\"{}\"", base.join(project).display()),
        )
    };
    let main = [
        harness::user_at(
            "sketch a plan for the tide table page: what it shows, and how it reads on a phone",
            &ts(0, 0),
        ),
        harness::thinking_at(
            "The page needs the week's tides first, then the notes.",
            &ts(0, 5),
        ),
        harness::markdown_answer_at(&ts(0, 20)),
        harness::user_at("read the notes and summarise them", &ts(2, 0)),
        harness::read_tool_at("r1", &path, &ts(2, 5)),
        harness::tool_result_at("r1", &ts(2, 6)),
        harness::assistant_at(
            "The notes keep the week's high and low water by day, flag Thursday's spring tide, and ask for the dinghy to be moved off the north mooring before Wednesday evening. The old table stays until the new one has been checked twice.",
            &ts(2, 30),
        ),
        harness::user_at("run the checks", &ts(4, 0)),
        harness::bash_call_at("cargo test --workspace", "b1", &ts(4, 5)),
        harness::tool_result_lines("b1", 40, &ts(4, 40)),
        harness::bash_call_at("git status --short", "b2", &ts(4, 45)),
        harness::tool_result_at("b2", &ts(4, 46)),
        harness::assistant_at("All forty checks pass, and the tree is clean.", &ts(5, 0)),
        harness::user_at("tighten the spacing under the headings", &ts(6, 0)),
        harness::edit_tool_at("e1", &base.join("harbour/style.css").display().to_string(), &ts(6, 10)),
        harness::tool_result_text("e1", "The file has been updated.", &ts(6, 11)),
        harness::assistant_at(
            "Done: the headings keep 0.6em below them now, down from 1.2em.",
            &ts(6, 30),
        ),
        harness::pasted_image_at("this is how it looks on the phone", &ts(8, 0)),
        harness::agent_spawn_at("a1", "Explore", &ts(8, 10)),
        harness::framed_agent_result_at(
            "a1",
            "agent-0517",
            "Explore",
            "The table reads well upright; sideways the third column wraps.",
            &ts(9, 0),
        ),
        harness::assistant_at(
            "The explorer found the table reads well upright, but held sideways the third column wraps. I would let the table scroll sideways inside its own box rather than wrap.",
            &ts(9, 20),
        ),
        harness::user_at(
            "yes, do that, and then write a short release note that says what changed for someone reading the page on a phone, in two or three sentences, without naming the files",
            &ts(11, 0),
        ),
        harness::assistant_at(
            "The tide table now scrolls sideways inside its own box on a narrow screen, so every column keeps its width and no time wraps onto a second line. Headings sit closer to the text below them, so more of the week fits on one screen.",
            &ts(11, 40),
        ),
    ]
    .concat();
    stores.claude_session(SID, &cwd(main, "harbour"));
    stores.claude_tasks(
        SID,
        &[
            ("1", "Draw the tide table", "completed"),
            ("2", "Check the phone layout", "in_progress"),
            ("3", "Write the release note", "pending"),
        ],
    );
    let others = [
        ("harbour", "move the mooring schedule into the table"),
        ("harbour", "why does the low-water column round up?"),
        ("orchard", "list the trees that need pruning before spring"),
        ("orchard", "draft the picking rota"),
        ("lighthouse", "check the lamp's service log"),
        ("lighthouse", "write up the keeper's notes"),
    ];
    for (n, (project, prompt)) in others.iter().enumerate() {
        let t = [
            harness::user_at(prompt, &ts(20 + n as u32, 0)),
            harness::assistant_at("Done, and noted in the log.", &ts(20 + n as u32, 30)),
        ]
        .concat();
        stores.claude_session(
            &format!("51700000-0000-4000-8000-0000000001{n:02}"),
            &cwd(t, project),
        );
    }
    (stores, path)
}

// ── driving the page ────────────────────────────────────────────────────────────────────────

fn emulate(tab: &headless_chrome::Tab, w: u32, h: u32, input: Input) {
    use headless_chrome::protocol::cdp::Emulation;
    let (dpr, mobile, touch, insets) = match input {
        Input::Mouse => (1.0, false, false, (0, 0, 0, 0)),
        Input::Touch {
            dpr,
            mobile,
            insets,
        } => (dpr, mobile, true, insets),
    };
    tab.call_method(Emulation::SetTouchEmulationEnabled {
        enabled: touch,
        max_touch_points: touch.then_some(5),
    })
    .expect("touch emulation");
    tab.call_method(Emulation::SetDeviceMetricsOverride {
        width: w,
        height: h,
        device_scale_factor: dpr,
        mobile,
        scale: None,
        screen_width: Some(w),
        screen_height: Some(h),
        position_x: None,
        position_y: None,
        dont_set_visible_size: None,
        screen_orientation: None,
        viewport: None,
        display_feature: None,
        device_posture: None,
    })
    .expect("metrics");
    harness::safe_area(tab, insets);
}

/// Polls `js` until it is true or `timeout` passes; never panics, so a run never dies part-way.
fn wait_for(tab: &headless_chrome::Tab, js: &str, timeout: Duration) -> bool {
    let t0 = Instant::now();
    loop {
        if harness::eval(tab, js) == Value::Bool(true) {
            return true;
        }
        if t0.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn settle(tab: &headless_chrome::Tab) {
    harness::eval(
        tab,
        "window.__settle ? window.__settle(1500).then(function(){ return true; }) : true",
    );
}

/// Forget what the page remembers (folds, the pane's share, the outline's default, the theme), keep
/// the seed that shows every session, and set the theme the cell asks for.
fn forget(tab: &headless_chrome::Tab, dark: bool) {
    harness::eval(
        tab,
        &format!(
            "(function(){{ try {{ Object.keys(localStorage).forEach(function(k){{ localStorage.removeItem(k); }}); sessionStorage.clear(); localStorage.setItem('am-prod-session-filter', JSON.stringify(['recent','blocked','idle'])); if ({dark}) localStorage.setItem('am-demo-theme', 'dark'); }} catch (e) {{}} return true; }})()"
        ),
    );
}

/// The probe: what the layout is and what a pointer can reach. Wrapped so a throw is recorded.
const PROBE: &str = r#"(function(){ try {
var W = innerWidth, H = innerHeight;
var app = document.getElementById('app'), v = document.getElementById('sessionView');
var root = app || v;
if (!root) return { error: 'no #app and no #sessionView' };
var vr = v ? v.getBoundingClientRect() : { left: 0, top: 0, right: W, bottom: H };
var shown = function (e) { for (var n = e; n && n.nodeType === 1; n = n.parentElement) { var cs = getComputedStyle(n); if (cs.display === 'none' || cs.visibility === 'hidden' || cs.visibility === 'collapse' || parseFloat(cs.opacity) < 0.05 || n.hasAttribute('hidden') || n.hasAttribute('inert')) return false; } return true; };
var box = function (e) { if (!e) return null; var r = e.getBoundingClientRect(); return { on: e.getClientRects().length > 0 && r.width >= 1 && r.height >= 1 && r.right > 0 && r.left < W && r.bottom > 0 && r.top < H && shown(e), l: Math.round(r.left), r: Math.round(r.right), w: Math.round(r.width), t: Math.round(r.top), h: Math.round(r.height) }; };
var q = function (s) { return document.querySelector(s); };
var name = function (e) { if (!e) return 'nothing'; if (e.id) return '#' + e.id; var l = e.getAttribute && (e.getAttribute('aria-label') || e.getAttribute('title')); if (l) return '"' + l.trim().slice(0, 40) + '"'; var t = (e.textContent || '').trim().replace(/\s+/g, ' '); var c = e.tagName.toLowerCase() + (e.classList.length ? '.' + Array.prototype.slice.call(e.classList, 0, 2).join('.') : ''); return t && t.length <= 24 && e.children.length < 3 ? c + ' "' + t + '"' : c; };
var named = function (e) { var n = e && e.closest ? e.closest('button, a, [role=button], [id], label, aside, section, header, nav') : null; return name(n || e); };
var ti = v && v.querySelector('.transcript-inner'), tr = v && v.querySelector('.transcript');
var text = null; if (ti && ti.getClientRects().length) { var cs = getComputedStyle(ti); text = Math.round(ti.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight)); }
var docW = Math.max(document.documentElement.scrollWidth, document.body ? document.body.scrollWidth : 0);
var SKIP = '.transcript-inner, .tree, #previewBody, .outline-card-body';
var SEL = 'button, a[href], input:not([type=hidden]), select, textarea, summary, [role=button], [role=tab], [role=menuitem], [tabindex]:not([tabindex="-1"])';
var checked = 0, missed = [], underPane = [], pv = document.getElementById('preview');
Array.prototype.forEach.call(root.querySelectorAll(SEL), function (e) {
  if (e.closest(SKIP) || !e.getClientRects().length || !shown(e)) return;
  if (getComputedStyle(e).pointerEvents === 'none') return;
  var r = e.getBoundingClientRect(); if (r.width < 2 || r.height < 2) return;
  var layer = e; while (layer && layer !== root) { var lc = getComputedStyle(layer); if (lc.position === 'fixed' || lc.transform !== 'none') break; layer = layer.parentElement; }
  if (layer && layer !== root) { var lr = layer.getBoundingClientRect(); if (lr.right <= 0 || lr.left >= W || lr.bottom <= 0 || lr.top >= H) return; }
  checked++;
  var cx = r.left + r.width / 2, cy = r.top + r.height / 2, why = null;
  if (cx < 0 || cx >= W || cy < 0 || cy >= H) why = 'centre off the window';
  else if (!app && (cx < vr.left || cx > vr.right || cy < vr.top || cy > vr.bottom)) why = 'centre outside the view';
  else { var hit = document.elementFromPoint(cx, cy);
    if (!hit) why = 'nothing at its centre';
    else if (hit === e || e.contains(hit)) { if (r.left < -0.5 || r.right > W + 0.5 || (!app && (r.left < vr.left - 0.5 || r.right > vr.right + 0.5))) why = 'cut at the edge'; }
    else if (hit.contains(e)) why = 'clipped inside ' + named(hit);
    else if (pv && pv.contains(hit) && !pv.contains(e)) { underPane.push(name(e)); return; }
    else why = 'covered by ' + named(hit); }
  if (why) missed.push({ control: name(e), why: why, at: Math.round(cx) + ',' + Math.round(cy) });
});
var beyond = [];
if (v) { var walk = function (e) { Array.prototype.forEach.call(e.children, function (c) { if (c.matches(SKIP)) return; var r = c.getBoundingClientRect(); if (c.getClientRects().length && r.width >= 1 && r.height >= 1 && (r.right > vr.right + 1 || r.left < vr.left - 1)) { if (shown(c)) beyond.push(name(c) + ' ' + Math.round(r.left) + '\u2013' + Math.round(r.right)); return; } walk(c); }); }; walk(v); }
return {
  size: W + 'x' + H, dpr: devicePixelRatio, coarse: matchMedia('(pointer:coarse)').matches,
  app: app ? Array.prototype.slice.call(app.classList) : null,
  view: v ? Array.prototype.slice.call(v.classList) : null,
  b: v ? v.style.getPropertyValue('--pl-b') : null,
  parts: { list: box(q('#app>.sidebar')), session: box(q('#sessionView>.workspace')), transcript: box(q('#sessionView .transcript')), pane: box(q('#preview')), outline: box(q('#sessionView .session-navigator')) },
  text: text, transcript: tr ? Math.round(tr.clientWidth) : null,
  overflow: { page: docW - W, view: v ? v.scrollWidth - v.clientWidth : null, transcript: tr ? tr.scrollWidth - tr.clientWidth : null },
  checked: checked, missed: missed, underPane: underPane, beyond: beyond.slice(0, 6), viewOverflowX: v ? getComputedStyle(v).overflowX : null
}; } catch (error) { return { error: String(error && error.stack || error) }; } })()"#;

/// The pane shows the notes: the right pane on the screen and the document drawn in it.
const PANE_SHOWS: &str = "(function(){ var p = document.getElementById('preview'), b = document.getElementById('previewBody'); if (!p || !b) return false; var r = p.getBoundingClientRect(); return r.width > 40 && r.right > 0 && r.left < innerWidth && /This week/.test(b.innerText || ''); })()";

struct Run {
    dir: PathBuf,
}

fn run_cell(
    tab: &headless_chrome::Tab,
    monitor: &harness::Monitor,
    cell: &Cell,
    path: &str,
    n: usize,
    run: &Run,
) -> Value {
    let t0 = Instant::now();
    let mut notes: Vec<String> = Vec::new();
    let (win_w, win_h) = match cell.surface {
        Surface::App => (cell.w, cell.h),
        Surface::Host => HOST_WINDOW,
    };
    emulate(tab, win_w, win_h, cell.input);
    forget(tab, cell.dark);
    match cell.surface {
        Surface::App => {
            tab.navigate_to(&monitor.url(&format!("?ui=app&session={SID}")))
                .unwrap();
            let _ = tab.wait_until_navigated();
            if !wait_for(
                tab,
                &format!(
                    "!!document.querySelector('.transcript .turn.user') && innerWidth === {win_w}"
                ),
                Duration::from_secs(25),
            ) {
                notes.push("the session never drew".into());
            }
        }
        Surface::Host => {
            let theme = if cell.dark { "&theme=dark" } else { "" };
            tab.navigate_to(
                &monitor.url(&format!("session-view-host?mount=0&session={SID}{theme}")),
            )
            .unwrap();
            let _ = tab.wait_until_navigated();
            if !wait_for(
                tab,
                "!!(window.__host && (window.__host.mounted || window.__host.error))",
                Duration::from_secs(25),
            ) {
                notes.push("the host page never mounted".into());
            }
            let height = if cell.h < HOST_WINDOW.1 - HOST_TOP {
                format!("p.style.bottom = 'auto'; p.style.height = '{}px';", cell.h)
            } else {
                String::new()
            };
            harness::eval(
                tab,
                &format!(
                    "(function(){{ var p = document.getElementById('hostPane'); p.style.left = '{HOST_LEFT}px'; p.style.right = 'auto'; p.style.width = '{}px'; {height} return window.__host.mount ? window.__host.mount() : String(window.__host.error); }})()",
                    cell.w
                ),
            );
            if !wait_for(
                tab,
                "!!document.querySelector('#sessionView .transcript .turn.user')",
                Duration::from_secs(25),
            ) {
                notes.push("the view never drew the session".into());
            }
        }
    }
    settle(tab);
    if !wait_for(tab, harness::VIEW_CAUGHT_UP, Duration::from_secs(10)) {
        notes.push("the view's layout never caught up with its box".into());
    }
    if cell.pane {
        let link = format!(
            "document.querySelector('[data-reference-path={}]')",
            serde_json::to_string(path).unwrap()
        );
        if wait_for(tab, &format!("!!{link}"), Duration::from_secs(15)) {
            harness::eval(tab, &format!("{link}.click(); true"));
            if !wait_for(tab, PANE_SHOWS, Duration::from_secs(25)) {
                notes.push("the right pane never showed the notes".into());
            }
        } else {
            notes.push("no link to the notes in the transcript".into());
        }
        // The pane's slide and the view's re-decision after it.
        std::thread::sleep(Duration::from_millis(500));
        settle(tab);
        wait_for(tab, harness::VIEW_CAUGHT_UP, Duration::from_secs(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    settle(tab);
    let measured = harness::probe(tab, PROBE);
    // The screenshot, in CSS pixels whatever the device's scale; a host cell shows its pane and a
    // margin of the host's page around it.
    let (dpr, shot_w) = match (cell.surface, cell.input) {
        (Surface::App, Input::Touch { dpr, .. }) => (dpr, win_w),
        (Surface::App, Input::Mouse) => (1.0, win_w),
        (Surface::Host, Input::Touch { dpr, .. }) => (dpr, (HOST_LEFT * 2 + cell.w).min(win_w)),
        (Surface::Host, Input::Mouse) => (1.0, (HOST_LEFT * 2 + cell.w).min(win_w)),
    };
    let shot_h = match cell.surface {
        Surface::App => win_h,
        Surface::Host => (HOST_TOP + cell.h + HOST_LEFT).min(win_h),
    };
    let png = tab.capture_screenshot(
        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
        None,
        Some(headless_chrome::protocol::cdp::Page::Viewport {
            x: 0.0,
            y: 0.0,
            width: f64::from(shot_w),
            height: f64::from(shot_h),
            scale: 1.0 / dpr,
        }),
        true,
    );
    let shot = format!("shots/{}-{n:03}-{}.png", cell.surface.name(), cell.id);
    match png {
        Ok(bytes) => std::fs::write(run.dir.join(&shot), bytes).unwrap(),
        Err(e) => notes.push(format!("no screenshot: {e}")),
    }
    let record = json!({
        "n": n,
        "id": cell.id,
        "surface": cell.surface.name(),
        "caption": cell.caption,
        "w": cell.w,
        "h": cell.h,
        "pane": cell.pane,
        "dark": cell.dark,
        "touch": matches!(cell.input, Input::Touch { .. }),
        "shot": shot,
        "shot_w": shot_w,
        "shot_h": shot_h,
        "measured": measured,
        "notes": notes,
        "ms": t0.elapsed().as_millis() as u64,
    });
    std::fs::write(
        run.dir.join(format!(
            "cells/{}-{n:03}-{}.json",
            cell.surface.name(),
            cell.id
        )),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
    record
}

// ── the outputs ─────────────────────────────────────────────────────────────────────────────

fn part(v: &Value) -> String {
    if v.is_null() {
        return "·".into();
    }
    if v["on"] != Value::Bool(true) {
        return "—".into();
    }
    format!("{}–{} ({})", v["l"], v["r"], v["w"])
}

fn missed_list(m: &Value) -> String {
    let list = m["missed"].as_array().cloned().unwrap_or_default();
    let under = m["underPane"].as_array().map_or(0, Vec::len);
    let under = if under > 0 {
        format!(" ({under} more under the open right pane)")
    } else {
        String::new()
    };
    if list.is_empty() {
        return format!("0 of {}{under}", m["checked"]);
    }
    let named: Vec<String> = list
        .iter()
        .map(|x| {
            format!(
                "{}: {}",
                x["control"].as_str().unwrap_or("?"),
                x["why"].as_str().unwrap_or("?")
            )
        })
        .collect();
    format!(
        "**{} of {}**: {}{under}",
        list.len(),
        m["checked"],
        named.join("; ")
    )
}

fn overflow(m: &Value) -> String {
    let o = &m["overflow"];
    let mut bits = Vec::new();
    for k in ["page", "view", "transcript"] {
        if let Some(px) = o[k].as_i64() {
            if px > 0 {
                bits.push(format!("{k} +{px}"));
            }
        }
    }
    let past: Vec<&str> = m["beyond"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if bits.is_empty() && past.is_empty() {
        return "none".into();
    }
    let mut out = if bits.is_empty() {
        "0".to_string()
    } else {
        format!("**{}**", bits.join(", "))
    };
    if !past.is_empty() {
        let _ = write!(
            out,
            " (view {}; past its edge: {})",
            m["viewOverflowX"].as_str().unwrap_or("?"),
            past.join(", ")
        );
    }
    out
}

fn classes(v: &Value, keep: impl Fn(&str) -> bool) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str())
                .filter(|c| keep(c))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_else(|| "·".into())
}

fn md_escape(s: &str) -> String {
    s.replace('|', "/")
}

fn report(dir: &Path, records: &[Value], took: Duration) {
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# Width study: agent-monitor's app shell and the session view in a host pane\n\n\
         Run {} cells in {} s (`claude-replay-browser-tests/examples/study_widths.rs`, #s517). \
         A study, not a gate. Sizes are CSS pixels. A part's box is `left–right (width)`; *session* is the session's whole column, the outline included; `—` is \
         not on the screen, `·` not in the page. *Text* is the transcript's text column. \
         *Unreachable* lists the chrome's controls whose centre a pointer cannot reach \
         (`elementFromPoint`), out of those checked; a control the open right pane lies over is counted apart. *Overflow* is how far the page, the view and \
         the transcript scroll sideways, and what is drawn in the view past its left or right edge \
         (the shut right pane, parked past the view's edge and not drawn, adds to the view's \
         number without being named). The screenshots are in `shots/`, and `contact.html` shows \
         them side by side.\n",
        records.len(),
        took.as_secs()
    );
    for (surface, title) in [
        ("app", "The app shell, by window"),
        (
            "host",
            "The session view in a host's pane, by pane (window 1700×1000)",
        ),
    ] {
        let rows: Vec<&Value> = records.iter().filter(|r| r["surface"] == surface).collect();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(md, "\n## {title}\n");
        let _ = writeln!(
            md,
            "| # | size | input | pane | theme | list | session | transcript | outline | right pane | text | overflow | unreachable | notes |"
        );
        let _ = writeln!(
            md,
            "| ---: | --- | --- | --- | --- | --- | --- | --- | --- | --- | ---: | --- | --- | --- |"
        );
        for r in &rows {
            let m = &r["measured"];
            let notes: Vec<String> = r["notes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut notes = notes.join("; ");
            if let Some(e) = m["error"].as_str() {
                notes = format!("probe: {e}");
            }
            let _ = writeln!(
                md,
                "| [{}]({}) | {}×{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                r["n"],
                r["shot"].as_str().unwrap_or(""),
                r["w"],
                r["h"],
                if r["touch"] == Value::Bool(true) {
                    "touch"
                } else {
                    "mouse"
                },
                if r["pane"] == Value::Bool(true) {
                    "open"
                } else {
                    "shut"
                },
                if r["dark"] == Value::Bool(true) {
                    "dark"
                } else {
                    "light"
                },
                part(&m["parts"]["list"]),
                part(&m["parts"]["session"]),
                part(&m["parts"]["transcript"]),
                part(&m["parts"]["outline"]),
                part(&m["parts"]["pane"]),
                m["text"],
                overflow(m),
                md_escape(&missed_list(m)),
                md_escape(&notes),
            );
        }
        let _ = writeln!(md, "\n### Layout classes\n");
        let _ = writeln!(md, "| # | size | pane | `#app` | `#sessionView` | B |");
        let _ = writeln!(md, "| ---: | --- | --- | --- | --- | --- |");
        for r in &rows {
            let m = &r["measured"];
            let _ = writeln!(
                md,
                "| {} | {}×{}{} | {} | {} | {} | {} |",
                r["n"],
                r["w"],
                r["h"],
                if r["touch"] == Value::Bool(true) {
                    " touch"
                } else {
                    ""
                },
                if r["pane"] == Value::Bool(true) {
                    "open"
                } else {
                    "shut"
                },
                classes(&m["app"], |_| true),
                classes(&m["view"], |c| c.starts_with("pl-")),
                m["b"].as_str().unwrap_or("·"),
            );
        }
    }
    std::fs::write(dir.join("report.md"), md).unwrap();

    let mut html = String::from(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Width study</title>\n<style>\n:root{color-scheme:light dark;--bg:#f6f6f4;--fg:#1d1d1b;--mute:#6b6b66;--card:#fff;--line:#ddd;--flag:#b3261e}\n@media (prefers-color-scheme:dark){:root{--bg:#18191b;--fg:#ececea;--mute:#9a9a95;--card:#232427;--line:#3a3b3f;--flag:#ff8a80}}\nbody{margin:0;padding:24px 16px;background:var(--bg);color:var(--fg);font:15px/1.45 -apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif}\nh1{font-size:22px;margin:0 0 4px}h2{font-size:18px;margin:32px 0 12px}p{color:var(--mute);margin:0 0 16px;max-width:70ch}\n.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(280px,1fr));gap:16px}\nfigure{margin:0;background:var(--card);border:1px solid var(--line);border-radius:8px;padding:8px;display:flex;flex-direction:column;gap:6px}\nfigure a{display:flex;justify-content:center;align-items:flex-start;background:repeating-conic-gradient(var(--line) 0 25%,transparent 0 50%) 0 0/16px 16px;border-radius:4px;overflow:hidden;height:260px}\nfigure img{max-width:100%;max-height:260px;object-fit:contain;object-position:top}\nfigcaption{font-size:13px}figcaption b{display:block;font-size:14px}.flag{color:var(--flag)}.mute{color:var(--mute)}\n</style></head><body>\n<h1>Width study</h1>\n<p>agent-monitor's app shell by window, and the session view by host pane. Each caption gives the size, the input, the right pane and what the measures flag; <code>report.md</code> has the numbers.</p>\n",
    );
    for (surface, title) in [
        ("app", "The app shell, by window"),
        ("host", "The session view in a host's pane, by pane"),
    ] {
        let rows: Vec<&Value> = records.iter().filter(|r| r["surface"] == surface).collect();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(html, "<h2>{title}</h2>\n<div class=\"grid\">");
        for r in rows {
            let m = &r["measured"];
            let missed = m["missed"].as_array().map_or(0, Vec::len);
            let over = overflow(m).replace("**", "");
            let mut flags = Vec::new();
            if missed > 0 {
                flags.push(format!("{missed} control(s) unreachable"));
            }
            if over != "none" {
                flags.push(format!("overflow: {over}"));
            }
            if let Some(a) = r["notes"].as_array() {
                flags.extend(a.iter().filter_map(|x| x.as_str().map(String::from)));
            }
            let text = m["text"]
                .as_i64()
                .map_or(String::new(), |t| format!(" · text {t}px"));
            let _ = writeln!(
                html,
                "<figure><a href=\"{shot}\"><img loading=\"lazy\" src=\"{shot}\" alt=\"\"></a><figcaption><b>{n}. {caption}</b><span class=\"mute\">{view}{text}</span>{flags}</figcaption></figure>",
                shot = r["shot"].as_str().unwrap_or(""),
                n = r["n"],
                caption = html_escape(r["caption"].as_str().unwrap_or("")),
                view = html_escape(&classes(&m["view"], |c| c == "pl-phone").replace("pl-phone", "phone reader · ")),
                flags = if flags.is_empty() {
                    String::new()
                } else {
                    format!("<br><span class=\"flag\">{}</span>", html_escape(&flags.join("; ")))
                },
            );
        }
        html.push_str("</div>\n");
    }
    html.push_str("</body></html>\n");
    std::fs::write(dir.join("contact.html"), html).unwrap();
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Each cell as a frame on one canvas, its caption above the screenshot at 1:1, composed in the same
/// Chrome; then ffmpeg makes one MP4 per surface, a second a frame.
fn videos(tab: &headless_chrome::Tab, dir: &Path, records: &[Value]) -> Vec<String> {
    let page = dir.join("frame.html");
    std::fs::write(
        &page,
        "<!doctype html><meta charset=\"utf-8\"><style>html,body{margin:0;background:#2a2c30}.cap{height:56px;box-sizing:border-box;padding:0 20px;display:flex;align-items:center;background:#111;color:#fff;font:600 24px/1 -apple-system,BlinkMacSystemFont,Helvetica,Arial,sans-serif;white-space:nowrap}img{display:block}</style><div class=\"cap\" id=\"cap\"></div><img id=\"img\" alt=\"\"><script>var p=new URLSearchParams(location.search);document.getElementById('cap').textContent=p.get('cap');var i=document.getElementById('img');i.width=+p.get('w');i.height=+p.get('h');i.onload=i.onerror=function(){document.title='ready';};i.src=p.get('img');</script>",
    )
    .unwrap();
    let ffmpeg = std::env::var("CR_FFMPEG").unwrap_or_else(|_| {
        if Path::new("/opt/homebrew/bin/ffmpeg").exists() {
            "/opt/homebrew/bin/ffmpeg".into()
        } else {
            "ffmpeg".into()
        }
    });
    let mut made = Vec::new();
    for surface in ["app", "host"] {
        let rows: Vec<&Value> = records
            .iter()
            .filter(|r| {
                r["surface"] == surface && dir.join(r["shot"].as_str().unwrap_or("")).is_file()
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        let even = |x: u64| x + (x & 1);
        let cw = even(
            rows.iter()
                .filter_map(|r| r["shot_w"].as_u64())
                .max()
                .unwrap_or(800),
        );
        let ch = even(
            rows.iter()
                .filter_map(|r| r["shot_h"].as_u64())
                .max()
                .unwrap_or(600)
                + 56,
        );
        let frames = dir.join("frames");
        emulate(tab, cw as u32, ch as u32, Input::Mouse);
        for (k, r) in rows.iter().enumerate() {
            let url = format!(
                "file://{}?img={}&w={}&h={}&cap={}",
                page.display(),
                encode(&format!(
                    "file://{}",
                    dir.join(r["shot"].as_str().unwrap()).display()
                )),
                r["shot_w"],
                r["shot_h"],
                encode(&format!(
                    "{}. {}",
                    r["n"],
                    r["caption"].as_str().unwrap_or("")
                )),
            );
            if tab.navigate_to(&url).is_err() {
                continue;
            }
            let _ = tab.wait_until_navigated();
            wait_for(tab, "document.title === 'ready'", Duration::from_secs(10));
            if let Ok(bytes) = tab.capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                Some(headless_chrome::protocol::cdp::Page::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: cw as f64,
                    height: ch as f64,
                    scale: 1.0,
                }),
                true,
            ) {
                std::fs::write(frames.join(format!("{surface}-{k:03}.png")), bytes).unwrap();
            }
        }
        let out = dir.join(format!("{surface}-sweep.mp4"));
        let status = std::process::Command::new(&ffmpeg)
            .args(["-y", "-loglevel", "error", "-framerate", "1", "-i"])
            .arg(frames.join(format!("{surface}-%03d.png")))
            .args([
                "-vf",
                "fps=10,format=yuv420p",
                "-c:v",
                "libx264",
                "-crf",
                "22",
                "-movflags",
                "+faststart",
            ])
            .arg(&out)
            .stdin(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => made.push(out.display().to_string()),
            other => eprintln!("study-widths: ffmpeg for {surface} failed: {other:?}"),
        }
    }
    made
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let only = arg("--only");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let name = arg("--run").unwrap_or_else(|| harness::rfc3339(now).replace([':', '-'], ""));
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let study = workspace.join("target/study-widths");
    let dir = study.join(&name);
    for sub in ["cells", "shots", "frames"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let run = Run { dir: dir.clone() };
    let cells: Vec<Cell> = cells()
        .into_iter()
        .filter(|c| only.as_ref().is_none_or(|o| c.id.contains(o.as_str())))
        .collect();
    eprintln!("study-widths: {} cells into {}", cells.len(), dir.display());
    let t0 = Instant::now();
    let records;
    let made;
    {
        // The guard goes last: it reaps this run's browsers and removes its scratch.
        let _serial = harness::serial();
        let base = harness::base("study-widths");
        let (stores, path) = world(&base);
        let monitor = harness::Monitor::spawn(harness::Kind::V1, PORT, &base, Some(&stores), true);
        let (_browser, tab) = harness::chrome_tab();
        harness::track_idle(&tab);
        harness::mouse_window(&tab, 1280, DESKTOP_H);
        monitor.pair(&tab);
        let page = include_str!("../tests/fixtures/host/session-view-host.html")
            .replace("<!--host-policy-->", "");
        harness::answer_in_browser(
            &tab,
            vec![harness::Answer {
                pattern: "*/session-view-host*".into(),
                path: "/session-view-host".into(),
                content_type: "text/html; charset=utf-8".into(),
                body: page,
            }],
        );
        let mut done = Vec::new();
        for (n, cell) in cells.iter().enumerate() {
            let record = run_cell(&tab, &monitor, cell, &path, n + 1, &run);
            let m = &record["measured"];
            eprintln!(
                "{:3} {:44} text={:>5} missed={} overflow={} {}",
                n + 1,
                cell.id,
                m["text"].to_string(),
                m["missed"].as_array().map_or(0, Vec::len),
                overflow(m).replace("**", ""),
                record["notes"]
            );
            done.push(record);
        }
        made = videos(&tab, &dir, &done);
        records = done;
        drop(monitor);
    }
    report(&dir, &records, t0.elapsed());
    let latest = study.join("latest");
    let _ = std::fs::remove_file(&latest);
    let _ = std::os::unix::fs::symlink(&name, &latest);
    println!("report:  {}", dir.join("report.md").display());
    println!("contact: {}", dir.join("contact.html").display());
    for m in made {
        println!("video:   {m}");
    }
    println!("{} cells in {} s", records.len(), t0.elapsed().as_secs());
}
