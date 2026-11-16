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
//! or a place from the one before: the tab first goes to a neutral page of the same origin (a public
//! icon, which runs nothing, so the last page's `pagehide` has written what it writes), then storage
//! is cleared, then the size is set, then the cell loads. For each it records the layout classes `#app` and `#sessionView` wear, the
//! box of each part (the session list, the session, the right pane, the outline), the chrome's
//! controls that a pointer cannot reach at their centre (hit-tested with `elementFromPoint`; what
//! sits inside a scroller — the transcript, the session tree, the pane's body, the outline's lists —
//! is left out, and so is a layer parked by its own state, named in `PROBE`: the shut right pane and
//! the shut drawer; each one set aside is listed in the cell's JSON), how
//! far the page and the transcript overflow sideways, how far what the view draws reaches past its
//! edges (a parked layer is not drawn), and the width of the transcript's text column, beside a
//! screenshot.
//!
//! Run it from the workspace root, after building the monitor in release:
//!
//! ```text
//! cargo build --release -p claude-monitor --features claude-monitor/codex-native
//! cargo run -p claude-replay-browser-tests --example study_widths -- [--only <text>] [--run <name>]
//! ```
//!
//! The videos need `ffmpeg` with libx264 (`brew install ffmpeg`; `CR_FFMPEG=<path>` names another).
//! The run looks for it before the first cell and, without one, says so and makes everything but
//! the videos.
//!
//! `--only` keeps the cells whose id holds the text (`app-`, `host-`, `touch`, `w1180`…); `--run`
//! names the output directory (a timestamp by default; a name used before has that run's `cells/`,
//! `shots/` and `frames/` removed first, and nothing else). Everything goes to
//! `target/study-widths/<run>/`, out of git: `report.md` (a table per surface), `contact.html` (the
//! screenshots in a grid), `app-sweep.mp4` and `host-sweep.mp4` (each cell a captioned frame, in the
//! order of the table; the frames are composed by the same Chrome, since ffmpeg's caption filter is
//! not in every build), and `cells/` and `shots/`, one JSON and one PNG per cell, written as each
//! cell finishes. `target/study-widths/latest` points at the last run. It is an example rather than
//! an `#[ignore]`d case so the browser suite's `--ignored` runs never sweep it in. Nothing in it is a
//! verdict: run it again after a change to see the after. A cell the probe could not measure (it
//! answered nothing or threw) is flagged in every output, and the run then exits 1, after writing
//! everything.

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

/// The page a cell starts from: a public icon the monitor serves on the same origin as the app and
/// the host page, an image document that runs none of the app, so nothing on it writes to storage
/// after it has been cleared.
const NEUTRAL: &str = "icon-192.png";

/// Start the cell from nothing (#s517 review): leave the last cell's page for the neutral one FIRST,
/// so its `pagehide` (which writes the session's place back into sessionStorage) has run, then forget
/// what the origin remembers (folds, the pane's share, the outline's default, the theme, the view's
/// place), keep the seed that shows every session and set the theme the cell asks for. The caller
/// sets the size after this, while the neutral page is showing, so no app page reacts to it. Returns
/// a note when any step failed.
fn forget(tab: &headless_chrome::Tab, monitor: &harness::Monitor, dark: bool) -> Option<String> {
    if let Err(e) = tab.navigate_to(&monitor.url(NEUTRAL)) {
        return Some(format!("the neutral page did not load: {e}"));
    }
    let _ = tab.wait_until_navigated();
    let here = format!("location.pathname === '/{NEUTRAL}' && document.readyState === 'complete'");
    if !wait_for(tab, &here, Duration::from_secs(10)) {
        return Some(
            "the neutral page never came; the cell may inherit the last one's state".into(),
        );
    }
    let cleared = harness::eval(
        tab,
        &format!(
            "(function(){{ try {{ localStorage.clear(); sessionStorage.clear(); localStorage.setItem('am-prod-session-filter', JSON.stringify(['recent','blocked','idle'])); if ({dark}) localStorage.setItem('am-demo-theme', 'dark'); return localStorage.length === ({dark} ? 2 : 1) && sessionStorage.length === 0 ? 'ok' : 'storage kept ' + localStorage.length + ' and ' + sessionStorage.length + ' keys'; }} catch (e) {{ return String(e); }} }})()"
        ),
    );
    match cleared.as_str() {
        Some("ok") => None,
        other => Some(format!(
            "storage was not cleared: {}",
            other.unwrap_or("no answer")
        )),
    }
}

/// The probe: what the layout is and what a pointer can reach. Wrapped so a throw is recorded. It is
/// a function of the cell (`{pane}`), called by `probe_call`.
///
/// A control is set aside before it is counted only where a layer is parked by its OWN state, named
/// here, never by where a layer happens to sit (a layer placed against the window rather than the
/// view must still count in a narrow host): the right pane while the view wears `preview-off`
/// (`.preview`: past the view's edge where it lies over the session, a 0px track where it is a
/// column); the phone's drawer while `#app` wears `app-drawer` and `mobile-detail` (`.sidebar`
/// pushed off the left edge), except on a wide phone held sideways, where the list is a column and
/// `mobile-detail` parks nothing. (A phone view hides the outline outright, which `shown()`
/// already leaves out; elsewhere `navigator-off` is the glyphs' rail, on the screen, so it parks
/// nothing either.) Each control set aside so is listed in
/// `skipped` with its reason, and the rest of what is not checked is counted in `notChecked`.
const PROBE: &str = r#"(function(cell){ try {
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
var checked = 0, missed = [], underPane = [], skipped = [], notChecked = { inScroller: 0, hidden: 0, noPointer: 0, tiny: 0 };
var pv = document.getElementById('preview');
var has = function (n, c) { return !!n && n.classList.contains(c); };
var WIDE = matchMedia('(pointer:coarse) and (max-height:500px) and (min-width:900px)').matches;
var paneShut = has(v, 'preview-off');
var sidebar = app && app.querySelector(':scope > .sidebar');
var parked = function (e) {
  if (paneShut && pv && pv.contains(e)) return 'the right pane, shut (preview-off)';
  if (sidebar && has(app, 'app-drawer') && has(app, 'mobile-detail') && !WIDE && sidebar.contains(e)) return 'the drawer, shut (mobile-detail)';
  return null;
};
// Where the open pane lies OVER the session, a control it covers is set apart rather than missed:
// that is the layout's choice. pane-layout.js gives every phone view `pl-pane-over`, but on a short
// touch view (`pl-coarse.pl-max-h-500`, a phone held sideways) the open pane is a column beside the
// session at half the width, unless it is full (`preview-full`) or a wide phone shows its list as a
// column, where the pane covers the session's area. Anywhere else the pane is beside the session,
// and a control under it is missed like any other.
var paneOver = null;
if (cell && cell.pane && v && pv && !paneShut) {
  var sideways = has(v, 'pl-coarse') && has(v, 'pl-max-h-500');
  if (has(v, 'pl-phone')) {
    if (!sideways) paneOver = 'an upright phone view: the open pane fills it';
    else if (has(v, 'preview-full')) paneOver = 'a phone view held sideways, the pane full';
    else if (app && WIDE && !has(app, 'phone-column-off')) paneOver = 'a wide phone held sideways beside its list: the pane covers the session';
  } else if (has(v, 'pl-pane-over')) paneOver = 'pl-pane-over: the pane lies over the session';
}
Array.prototype.forEach.call(root.querySelectorAll(SEL), function (e) {
  if (e.closest(SKIP)) { notChecked.inScroller++; return; }
  if (!e.getClientRects().length || !shown(e)) { notChecked.hidden++; return; }
  if (getComputedStyle(e).pointerEvents === 'none') { notChecked.noPointer++; return; }
  var r = e.getBoundingClientRect(); if (r.width < 2 || r.height < 2) { notChecked.tiny++; return; }
  var park = parked(e); if (park) { skipped.push({ control: name(e), why: park }); return; }
  checked++;
  var cx = r.left + r.width / 2, cy = r.top + r.height / 2, why = null;
  if (cx < 0 || cx >= W || cy < 0 || cy >= H) why = 'centre off the window';
  else if (!app && (cx < vr.left || cx > vr.right || cy < vr.top || cy > vr.bottom)) why = 'centre outside the view';
  else { var hit = document.elementFromPoint(cx, cy);
    if (!hit) why = 'nothing at its centre';
    else if (hit === e || e.contains(hit)) { var cut = []; if (r.left < -0.5 || (!app && r.left < vr.left - 0.5)) cut.push('left'); if (r.right > W + 0.5 || (!app && r.right > vr.right + 0.5)) cut.push('right'); if (r.top < -0.5 || (!app && r.top < vr.top - 0.5)) cut.push('top'); if (r.bottom > H + 0.5 || (!app && r.bottom > vr.bottom + 0.5)) cut.push('bottom'); if (cut.length) why = 'cut at the ' + cut.join(' and ') + ' edge'; }
    else if (hit.contains(e)) why = 'clipped inside ' + named(hit);
    else if (paneOver && pv.contains(hit) && !pv.contains(e)) { underPane.push(name(e)); return; }
    else why = 'covered by ' + named(hit); }
  if (why) missed.push({ control: name(e), why: why, at: Math.round(cx) + ',' + Math.round(cy) });
});
// What is DRAWN past the view's left or right edge: a box that reaches past it, shown, cut to what the
// ancestors between it and the view let through (an ancestor that clips sideways, by overflow or
// paint containment; a fixed box escapes every ancestor up to the one that holds fixed boxes), and
// outside a layer parked by its own state. The view's own clip is left out on purpose: what it cuts
// is still a part laid out past its edge. Its `scrollWidth` is kept as `viewScroll`, for reference
// only, since it counts the shut pane parked past the edge, which nobody sees.
var holdsFixed = function (cs) { return cs.transform !== 'none' || cs.filter !== 'none' || (cs.backdropFilter || cs.webkitBackdropFilter || 'none') !== 'none' || cs.perspective !== 'none' || /paint|layout|strict|content/.test(cs.contain) || /transform|filter|perspective/.test(cs.willChange); };
var drawn = function (e) { var r = e.getBoundingClientRect(), l = r.left, rt = r.right, free = getComputedStyle(e).position === 'fixed';
  for (var n = e.parentElement; n && n !== v && rt > l; n = n.parentElement) { var cs = getComputedStyle(n);
    if (free) { if (!holdsFixed(cs)) continue; free = false; }
    if (cs.overflowX !== 'visible' || /paint|strict|content/.test(cs.contain)) { var nr = n.getBoundingClientRect(); l = Math.max(l, nr.left + n.clientLeft); rt = Math.min(rt, nr.left + n.clientLeft + n.clientWidth); }
    if (cs.position === 'fixed') free = true; }
  return { l: l, r: rt }; };
var beyond = [], past = 0;
// The outermost box past the edge is named; the walk goes on inside it for the number alone, since
// a part of it may reach further still.
if (v) { var walk = function (e, naming) { Array.prototype.forEach.call(e.children, function (c) {
  if (c.matches(SKIP) || parked(c)) return;
  var r = c.getBoundingClientRect();
  if (c.getClientRects().length && r.width >= 1 && r.height >= 1 && (r.right > vr.right + 1 || r.left < vr.left - 1)) {
    if (!shown(c)) return;
    var d = drawn(c), over = Math.max(d.r - vr.right, vr.left - d.l);
    if (d.r > d.l && over > 1) { past = Math.max(past, Math.round(over)); if (naming) beyond.push(name(c) + ' ' + Math.round(d.l) + '\u2013' + Math.round(d.r)); walk(c, false); return; }
  }
  walk(c, naming); }); }; walk(v, true); }
return {
  size: W + 'x' + H, dpr: devicePixelRatio, coarse: matchMedia('(pointer:coarse)').matches,
  app: app ? Array.prototype.slice.call(app.classList) : null,
  view: v ? Array.prototype.slice.call(v.classList) : null,
  b: v ? v.style.getPropertyValue('--pl-b') : null,
  parts: { list: box(q('#app>.sidebar')), session: box(q('#sessionView>.workspace')), transcript: box(q('#sessionView .transcript')), pane: box(q('#preview')), outline: box(q('#sessionView .session-navigator')) },
  text: text, transcript: tr ? Math.round(tr.clientWidth) : null,
  overflow: { page: docW - W, view: v ? past : null, transcript: tr ? tr.scrollWidth - tr.clientWidth : null }, viewScroll: v ? v.scrollWidth - v.clientWidth : null,
  checked: checked, missed: missed, paneOver: paneOver, underPane: underPane, skipped: skipped, notChecked: notChecked, beyond: beyond.slice(0, 6), viewOverflowX: v ? getComputedStyle(v).overflowX : null
}; } catch (error) { return { error: String(error && error.stack || error) }; } })"#;

/// The probe called for `cell`.
fn probe_call(cell: &Cell) -> String {
    format!("{PROBE}({})", json!({ "pane": cell.pane }))
}

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
    if let Some(note) = forget(tab, monitor, cell.dark) {
        notes.push(note);
    }
    emulate(tab, win_w, win_h, cell.input);
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
            let mounted = harness::eval(
                tab,
                &format!(
                    "(function(){{ try {{ var p = document.getElementById('hostPane'); p.style.left = '{HOST_LEFT}px'; p.style.right = 'auto'; p.style.width = '{}px'; {height} if (!window.__host) return 'no host on the page'; if (!window.__host.mount) return 'the host offers no mount: ' + String(window.__host.error); return String(window.__host.mount()); }} catch (e) {{ return 'the mount threw: ' + String(e && e.stack || e); }} }})()",
                    cell.w
                ),
            );
            if mounted.as_str() != Some("mounted") {
                notes.push(format!(
                    "the host's mount answered {}",
                    mounted.as_str().map_or(mounted.to_string(), String::from)
                ));
            }
            if !wait_for(
                tab,
                "!!document.querySelector('#sessionView .transcript .turn.user')",
                Duration::from_secs(25),
            ) {
                notes.push("the view never drew the session".into());
            }
            // What the host page caught: its own setup's error, and every error the page threw.
            let caught = harness::probe(
                tab,
                "window.__host ? { error: window.__host.error, errors: window.__host.errors } : null",
            );
            if let Some(e) = caught["error"].as_str() {
                notes.push(format!("the host page's error: {e}"));
            }
            for e in caught["errors"].as_array().into_iter().flatten() {
                notes.push(format!(
                    "the page threw: {}",
                    e.as_str().map_or(e.to_string(), String::from)
                ));
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
    let measured = harness::probe(tab, &probe_call(cell));
    // A probe that answered nothing or threw measured nothing: say so beside whatever else was noted,
    // so the cell is flagged rather than read as clean.
    if measured.is_null() {
        notes.push("the probe answered nothing: this cell was not measured".into());
    } else if let Some(e) = measured["error"].as_str() {
        notes.push(format!("the probe threw: {e}"));
    }
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
        "measured_ok": is_measured(&measured),
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

/// Whether the probe measured the cell: it answered, and without an error (its own early `{error}`
/// return included). The one test the report, the contact sheet, stderr and the exit status use.
fn is_measured(m: &Value) -> bool {
    m.is_object() && m.get("error").is_none()
}

fn part(v: &Value) -> String {
    if v.is_null() {
        return "·".into();
    }
    if v["on"] != Value::Bool(true) {
        return "—".into();
    }
    format!("{}–{} ({})", v["l"], v["r"], v["w"])
}

/// The controls the open right pane covers where it lies over the session: counted among those
/// checked, and set aside rather than missed. Empty where there are none.
fn under_pane(m: &Value) -> String {
    let n = m["underPane"].as_array().map_or(0, Vec::len);
    if n == 0 {
        return String::new();
    }
    format!(
        " ({n} of the {} lie under the open right pane, which covers the session here, and are not counted as unreachable)",
        m["checked"]
    )
}

fn missed_list(m: &Value) -> String {
    if !is_measured(m) {
        return "**not measured**".into();
    }
    let list = m["missed"].as_array().cloned().unwrap_or_default();
    let under = under_pane(m);
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
    if !is_measured(m) {
        return "**not measured**".into();
    }
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

/// A table cell is one line of Markdown that shows the page's text as it is: a `|` would split it,
/// a line break (a probe's or a host's JavaScript stack) would end the table, and a `<…>` (a stack's
/// `at <anonymous>:3:7`) would be read as a tag and dropped.
fn md_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "/")
        .replace("\r\n", "<br>")
        .replace(['\r', '\n'], "<br>")
}

fn report(dir: &Path, records: &[Value], took: Duration, videos: &Videos) {
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# Width study: agent-monitor's app shell and the session view in a host pane\n\n\
         Run {} cells in {} s (`claude-replay-browser-tests/examples/study_widths.rs`, #s517). \
         A study, not a gate. Sizes are CSS pixels. A part's box is `left–right (width)`; *session* is the session's whole column, the outline included; `—` is \
         not on the screen, `·` not in the page. *Text* is the transcript's text column. \
         *Unreachable* lists the chrome's controls whose centre a pointer cannot reach \
         (`elementFromPoint`), out of those checked. Where the open right pane lies over the session (`pl-pane-over`, \
         an upright phone view, or a phone held sideways with the pane full or beside its list), a \
         control it covers is one of those checked but set aside, not counted as unreachable; where \
         the pane is beside the session, a control under it is unreachable like any other. \
         A control in a layer parked by its own state (the shut right pane, the shut drawer) \
         is not checked; each cell's JSON lists them under `skipped`. *Overflow* is how far the page and the \
         transcript scroll sideways and how far what the view DRAWS reaches past its left or right \
         edge (a shown box, cut by the ancestors that clip it; a parked layer such as the shut \
         right pane is not drawn and does not count; the cell's JSON keeps the view's raw \
         `scrollWidth` excess as `viewScroll`), with the parts that reach past it named. The screenshots are in `shots/`, and `contact.html` shows \
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
            let notes = notes.join("; ");
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
                md_escape(&overflow(m)),
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
    let _ = writeln!(md, "\n## Videos\n");
    for v in &videos.made {
        let name = Path::new(v)
            .file_name()
            .map_or(v.clone(), |n| n.to_string_lossy().into_owned());
        let _ = writeln!(md, "- [{name}]({name})");
    }
    if videos.made.is_empty() {
        let _ = writeln!(md, "- none made by this run");
    }
    for surface in ["app", "host"] {
        let file = format!("{surface}-sweep.mp4");
        if dir.join(&file).is_file() && !videos.made.iter().any(|v| v.ends_with(&file)) {
            let _ = writeln!(
                md,
                "- `{file}` is STALE: an earlier run under this name made it, and this run did not"
            );
        }
    }
    for note in &videos.notes {
        let _ = writeln!(md, "- {}", md_escape(note));
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
            let mut flags = Vec::new();
            if is_measured(m) {
                let missed = m["missed"].as_array().map_or(0, Vec::len);
                let over = overflow(m).replace("**", "");
                if missed > 0 {
                    flags.push(format!("{missed} control(s) unreachable"));
                }
                if over != "none" {
                    flags.push(format!("overflow: {over}"));
                }
            } else {
                flags.push("NOT MEASURED".to_string());
            }
            if let Some(a) = r["notes"].as_array() {
                flags.extend(a.iter().filter_map(|x| x.as_str().map(String::from)));
            }
            let under = m["underPane"].as_array().map_or(0, Vec::len);
            let text = format!(
                "{}{}",
                m["text"]
                    .as_i64()
                    .map_or(String::new(), |t| format!(" · text {t}px")),
                if under > 0 {
                    format!(" · {under} control(s) under the open pane, set aside")
                } else {
                    String::new()
                }
            );
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

/// What `videos` made, and what it could not: a cell with no frame, a video ffmpeg did not make.
#[derive(Default)]
struct Videos {
    made: Vec<String>,
    notes: Vec<String>,
}

/// Each cell as a frame on one canvas, its caption above the screenshot at 1:1, composed in the same
/// Chrome; then ffmpeg makes one MP4 per surface, a second a frame. Frames are numbered by what was
/// written, so a cell whose frame failed leaves no gap (ffmpeg's `%03d` input stops at the first
/// missing number and still exits 0); each such cell is noted.
fn videos(tab: &headless_chrome::Tab, dir: &Path, records: &[Value], ffmpeg: &str) -> Videos {
    let mut out = Videos::default();
    let page = dir.join("frame.html");
    std::fs::write(
        &page,
        "<!doctype html><meta charset=\"utf-8\"><style>html,body{margin:0;background:#2a2c30}.cap{height:56px;box-sizing:border-box;padding:0 20px;display:flex;align-items:center;background:#111;color:#fff;font:600 24px/1 -apple-system,BlinkMacSystemFont,Helvetica,Arial,sans-serif;white-space:nowrap}img{display:block}</style><div class=\"cap\" id=\"cap\"></div><img id=\"img\" alt=\"\"><script>var p=new URLSearchParams(location.search);document.getElementById('cap').textContent=p.get('cap');var i=document.getElementById('img');i.width=+p.get('w');i.height=+p.get('h');i.onload=i.onerror=function(){document.title='ready';};i.src=p.get('img');</script>",
    )
    .unwrap();
    for surface in ["app", "host"] {
        let mut rows: Vec<&Value> = Vec::new();
        for r in records.iter().filter(|r| r["surface"] == surface) {
            if dir.join(r["shot"].as_str().unwrap_or("")).is_file() {
                rows.push(r);
            } else {
                out.notes.push(format!(
                    "{surface}: cell {} has no screenshot, so no frame",
                    r["n"]
                ));
            }
        }
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
        let mut written = 0usize;
        for r in &rows {
            let url = format!(
                "file://{}?img={}&w={}&h={}&cap={}",
                encode(&page.display().to_string()),
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
            if let Err(e) = tab.navigate_to(&url) {
                out.notes.push(format!(
                    "{surface}: cell {} skipped, its frame page did not load: {e}",
                    r["n"]
                ));
                continue;
            }
            let _ = tab.wait_until_navigated();
            if !wait_for(tab, "document.title === 'ready'", Duration::from_secs(10)) {
                out.notes.push(format!(
                    "{surface}: cell {}'s frame never said it was ready; captured as it stood",
                    r["n"]
                ));
            }
            match tab.capture_screenshot(
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
                Ok(bytes) => {
                    std::fs::write(frames.join(format!("{surface}-{written:03}.png")), bytes)
                        .unwrap();
                    written += 1;
                }
                Err(e) => out.notes.push(format!(
                    "{surface}: cell {} skipped, its frame was not captured: {e}",
                    r["n"]
                )),
            }
        }
        if written == 0 {
            out.notes
                .push(format!("{surface}: no frame was written, so no video"));
            continue;
        }
        let video = dir.join(format!("{surface}-sweep.mp4"));
        let status = std::process::Command::new(ffmpeg)
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
            .arg(&video)
            .stdin(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => out.made.push(video.display().to_string()),
            other => out
                .notes
                .push(format!("{surface}: ffmpeg made no video: {other:?}")),
        }
    }
    for note in &out.notes {
        eprintln!("study-widths: video: {note}");
    }
    out
}

/// The ffmpeg the videos need: `CR_FFMPEG`, else Homebrew's, else the one on `PATH`, asked for its
/// version before any cell runs, so a run without one says so at its start rather than after the
/// sweep. `Err` says why there is none.
fn find_ffmpeg() -> Result<String, String> {
    let ffmpeg = std::env::var("CR_FFMPEG").unwrap_or_else(|_| {
        if Path::new("/opt/homebrew/bin/ffmpeg").exists() {
            "/opt/homebrew/bin/ffmpeg".into()
        } else {
            "ffmpeg".into()
        }
    });
    match std::process::Command::new(&ffmpeg)
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(ffmpeg),
        Ok(s) => Err(format!("`{ffmpeg} -version` exited {s}")),
        Err(e) => Err(format!("no ffmpeg at `{ffmpeg}` ({e})")),
    }
}

/// Percent-encodes everything but the unreserved characters and `/`, so a path (the frame page's,
/// a screenshot's) and a caption both go into a `file://` URL whole.
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
    // A run starts from empty subdirectories: a name used before keeps none of that run's cells,
    // shots or frames. Only those three, each by its absolute path, are removed, and only under a
    // name that is one plain directory under `target/study-widths/` (never `latest`, the link). The
    // run's files are rewritten, except a video this run does not make, which the report names.
    if name.is_empty()
        || name == "."
        || name == ".."
        || name == "latest"
        || name.contains(['/', '\\'])
    {
        eprintln!("study-widths: --run takes a plain name, not {name:?}");
        std::process::exit(2);
    }
    let dir = study.join(&name);
    assert!(dir.is_absolute(), "the run's directory is not absolute");
    if let Ok(meta) = std::fs::symlink_metadata(&dir) {
        if !meta.is_dir() {
            eprintln!(
                "study-widths: {} is not a directory (a link?); not reusing it",
                dir.display()
            );
            std::process::exit(2);
        }
        for sub in ["cells", "shots", "frames"] {
            let d = dir.join(sub);
            if d.exists() {
                std::fs::remove_dir_all(&d)
                    .unwrap_or_else(|e| panic!("clearing {}: {e}", d.display()));
            }
        }
        eprintln!(
            "study-widths: {} was used before; its cells, shots and frames are cleared (a video this run does not make is left from the earlier run)",
            dir.display()
        );
    }
    for sub in ["cells", "shots", "frames"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let run = Run { dir: dir.clone() };
    let cells: Vec<Cell> = cells()
        .into_iter()
        .filter(|c| only.as_ref().is_none_or(|o| c.id.contains(o.as_str())))
        .collect();
    eprintln!("study-widths: {} cells into {}", cells.len(), dir.display());
    let ffmpeg = find_ffmpeg();
    if let Err(why) = &ffmpeg {
        eprintln!(
            "study-widths: {why}: the videos will be skipped (install ffmpeg, or name one with CR_FFMPEG=<path>)"
        );
    }
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
        // One language for every run: the machine's own locale reached mdrev's relative times
        // ("6 分钟前") and would make two runs on two Macs differ for no reason in the page.
        {
            use headless_chrome::protocol::cdp::Emulation;
            tab.call_method(Emulation::SetLocaleOverride {
                locale: Some("en-US".into()),
            })
            .expect("the locale override");
            // And one clock face: the machine's own timezone reaches every time the page draws.
            tab.call_method(Emulation::SetTimezoneOverride {
                timezone_id: "UTC".into(),
            })
            .expect("the timezone override");
        }
        harness::track_idle(&tab);
        harness::mouse_window(&tab, 1280, DESKTOP_H);
        monitor.pair(&tab);
        // The host page as the cases serve it, given the pinned mdrev version, so its right pane
        // shows a Markdown file in mdrev as the app's does (the fixture names none: its cases
        // read no document there).
        let fixture = include_str!("../tests/fixtures/host/session-view-host.html");
        let handle = "backendHandle({ paired: true })";
        assert!(
            fixture.contains(handle),
            "the host fixture makes its backend handle another way"
        );
        let page = fixture.replace("<!--host-policy-->", "").replace(
            handle,
            &format!(
                "backendHandle({{ paired: true, mdrevVersion: {:?} }})",
                harness::mdrev_pin()
            ),
        );
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
            if is_measured(m) {
                eprintln!(
                    "{:3} {:44} text={:>5} missed={} overflow={} {}",
                    n + 1,
                    cell.id,
                    m["text"].to_string(),
                    m["missed"].as_array().map_or(0, Vec::len),
                    overflow(m).replace("**", ""),
                    record["notes"]
                );
            } else {
                eprintln!(
                    "{:3} {:44} NOT MEASURED {}",
                    n + 1,
                    cell.id,
                    record["notes"]
                );
            }
            done.push(record);
        }
        made = match &ffmpeg {
            Ok(ffmpeg) => videos(&tab, &dir, &done, ffmpeg),
            Err(why) => Videos {
                made: Vec::new(),
                notes: vec![format!("skipped: {why}")],
            },
        };
        records = done;
        drop(monitor);
    }
    report(&dir, &records, t0.elapsed(), &made);
    let latest = study.join("latest");
    let _ = std::fs::remove_file(&latest);
    let _ = std::os::unix::fs::symlink(&name, &latest);
    println!("report:  {}", dir.join("report.md").display());
    println!("contact: {}", dir.join("contact.html").display());
    for m in &made.made {
        println!("video:   {m}");
    }
    println!("{} cells in {} s", records.len(), t0.elapsed().as_secs());
    // Everything is written; a cell the probe could not measure makes the run fail, so a script (or
    // a reader of the exit status) never takes it for a clean sweep.
    let unmeasured: Vec<&str> = records
        .iter()
        .filter(|r| !is_measured(&r["measured"]))
        .filter_map(|r| r["id"].as_str())
        .collect();
    if !unmeasured.is_empty() {
        eprintln!(
            "study-widths: {} cell(s) not measured: {}",
            unmeasured.len(),
            unmeasured.join(", ")
        );
        std::process::exit(1);
    }
}
