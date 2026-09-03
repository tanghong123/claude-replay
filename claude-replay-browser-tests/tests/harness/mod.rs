//! The browser harness's kit (#53): what every real-Chrome case needs and none should
//! re-spell — scratch roots, record builders, hermetic stores, a monitor under test, Chrome,
//! and the small vocabulary of actions and probes over a tab.
//!
//! It is an integration-test module (`mod harness;` from each test file), not library code,
//! so the crate's dev-dependencies stay where they are and nothing here reaches
//! `cargo doc --workspace`. Every case in this crate is `#[ignore]`d and runs only under
//! `cargo test -p claude-replay-browser-tests -- --ignored`, which needs a local Chrome and,
//! for the monitor cases, `cargo build --release -p claude-monitor -p claude-monitor-v2`.
//!
//! Conventions the cases rely on:
//! - A case takes [`serial`] first: every case binds fixed loopback ports and shares
//!   `CLAUDE_MONITOR_STATE`, so two cannot overlap.
//! - A case builds its world under [`base`] and points every store env var into it
//!   ([`Stores`]); nothing a case measures comes from this machine's own sessions.
//! - A monitor under test is a [`Monitor`]: it is reaped on drop, its token is read from its
//!   scratch state dir, and [`Monitor::pair`] is the first navigation of every tab.
//! - Probes go through [`probe`] (a JSON value) or [`eval`] (a primitive); waits go through
//!   [`until`], which panics with the last thing it saw — never a vacuous return.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ── scratch, serialization, reaping ─────────────────────────────────────────────────────────

/// A fresh scratch directory for one case, under the workspace's own temp dir.
pub fn base(name: &str) -> PathBuf {
    hermetic_state();
    let d = std::env::temp_dir().join(format!("cr-browser-follow-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The html server's state dir, once per process: a render policy that offers files, and
/// `CLAUDE_MONITOR_STATE` pointed away from the machine's real one.
pub fn hermetic_state() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let d = std::env::temp_dir().join(format!("cr-browser-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("render-policy.json"), b"{\"mode\":\"offered\"}");
        std::env::set_var("CLAUDE_MONITOR_STATE", &d);
    });
}

/// Every case holds this: fixed ports and a shared state dir mean one case at a time.
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A child process that dies with its guard — a panicking case never strands a server on
/// its port for the next one.
pub struct Reap(pub std::process::Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ── records ─────────────────────────────────────────────────────────────────────────────────
// Claude-format lines. `s` is the minute of a fixed day, so a fixture's clock is stable and
// the index reads these sessions as finished (state derives from the CONTENT clock, not the
// file's mtime). A growth scenario that must read as live uses [`user_at`] & co with a
// now-relative timestamp instead.

const DAY: &str = "2026-08-21T10";

fn stamp(s: u32) -> String {
    format!("{DAY}:{:02}:00Z", s % 60)
}

/// A user turn.
pub fn user(t: &str, s: u32) -> String {
    user_at(t, &stamp(s))
}
/// An assistant text block.
pub fn assistant(t: &str, s: u32) -> String {
    assistant_at(t, &stamp(s))
}
/// A tool call opening (a `Bash` head the transcript renders as a fold header).
pub fn tool_open(id: &str, s: u32) -> String {
    tool_open_at(id, &stamp(s))
}
/// The matching tool result.
pub fn tool_result(id: &str, s: u32) -> String {
    tool_result_at(id, &stamp(s))
}
/// A thinking block.
pub fn thinking(t: &str, s: u32) -> String {
    thinking_at(t, &stamp(s))
}

pub fn user_at(t: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn assistant_at(t: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":10,\"output_tokens\":20}}}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn tool_open_at(id: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"echo {id}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn tool_result_at(id: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"out line\\nout line\\nout line\\n\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}
pub fn thinking_at(t: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"thinking\",\"thinking\":\"{t}\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// A sub-agent spawn: the `Agent` tool call the parent makes (the spawn chip).
pub fn agent_spawn(call_id: &str, subagent_type: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{call_id}\",\"name\":\"Agent\",\"input\":{{\"subagent_type\":\"{subagent_type}\",\"description\":\"look around\",\"prompt\":\"look around\"}}}}]}},\"timestamp\":\"{}\"}}\n",
        stamp(s)
    )
}
/// The spawn's result, naming the child `agent_id` whose transcript lives at
/// `<sid>/subagents/agent-<agent_id>.jsonl` — what links a parent to its child.
pub fn agent_result(call_id: &str, agent_id: &str, subagent_type: &str, s: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"toolUseResult\":{{\"kind\":\"agent-result\",\"agentId\":\"{agent_id}\",\"agentType\":\"{subagent_type}\",\"content\":\"done\"}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{call_id}\",\"content\":\"done\"}}]}},\"timestamp\":\"{}\"}}\n",
        stamp(s)
    )
}

/// An ISO timestamp `secs_ago` seconds before now — for records that must read as live.
pub fn now_minus(secs_ago: u64) -> String {
    let t = std::time::SystemTime::now() - Duration::from_secs(secs_ago);
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    // Civil time from the epoch, UTC — enough for a timestamp the parsers accept.
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Append to a transcript and flush — a live tail, as an agent writes it.
pub fn append(path: &Path, s: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(s.as_bytes()).unwrap();
    f.flush().unwrap();
}

/// A long session: `turns` user/assistant pairs, every `shape.tool_every`th turn carrying a
/// tool call + result (a fold header the keys walk), every `shape.think_every`th a thinking
/// block. Long enough to scroll on every surface from a few dozen turns.
#[derive(Clone, Copy)]
pub struct Shape {
    pub tool_every: u32,
    pub think_every: u32,
    pub prose_repeat: usize,
}
impl Default for Shape {
    fn default() -> Self {
        Shape {
            tool_every: 3,
            think_every: 5,
            prose_repeat: 6,
        }
    }
}
pub fn long_session(turns: u32, shape: Shape) -> String {
    let mut out = String::new();
    for i in 0..turns {
        out += &user(
            &format!(
                "question {i}: {}",
                "lorem ipsum dolor sit amet, consectetur. ".repeat(shape.prose_repeat / 2 + 1)
            ),
            i,
        );
        if shape.think_every > 0 && i % shape.think_every == 0 {
            out += &thinking(
                &format!(
                    "deliberation {i}: {}",
                    "weighing the options carefully. ".repeat(shape.prose_repeat)
                ),
                i,
            );
        }
        if shape.tool_every > 0 && i % shape.tool_every == 0 {
            out += &tool_open(&format!("t{i}"), i);
            out += &tool_result(&format!("t{i}"), i);
        }
        out += &assistant(
            &format!(
                "answer {i}: {}",
                "sed do eiusmod tempor incididunt ut labore. ".repeat(shape.prose_repeat)
            ),
            i,
        );
    }
    out
}

// ── stores ──────────────────────────────────────────────────────────────────────────────────

/// A hermetic world of agent stores under a case's scratch root. Every store env var the
/// monitors and the adapters honour points into it, so a monitor under test sees ONLY what
/// the case wrote (the same knobs claude-monitor's index tests use).
pub struct Stores {
    pub root: PathBuf,
}

impl Stores {
    pub fn new(base: &Path) -> Stores {
        let root = base.join("stores");
        for d in ["claude", "qoderwork", "qoder", "codex"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        Stores { root }
    }

    /// The env a monitor (or an adapter) needs to see only these stores.
    pub fn envs(&self) -> Vec<(&'static str, PathBuf)> {
        vec![
            ("CLAUDE_PROJECTS_DIR", self.root.join("claude")),
            ("QODERWORK_PROJECTS_DIR", self.root.join("qoderwork")),
            ("QODER_PROJECTS_DIR", self.root.join("qoder")),
            ("CODEX_HOME", self.root.join("codex")),
        ]
    }

    /// A Claude session `sid` under the project slug `-r` (the builders' cwd), with `jsonl`.
    /// Returns the transcript path — a live-growth scenario appends to it.
    pub fn claude_session(&self, sid: &str, jsonl: &str) -> PathBuf {
        let proj = self.root.join("claude").join("-r");
        std::fs::create_dir_all(&proj).unwrap();
        let path = proj.join(format!("{sid}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    /// A sub-agent transcript of `parent_sid`: `<slug>/<sid>/subagents/agent-<agent>.jsonl`.
    /// Lineage is the PATH alone (the adapter reads no file to know the parent).
    pub fn claude_child(&self, parent_sid: &str, agent: &str, jsonl: &str) -> PathBuf {
        let dir = self
            .root
            .join("claude")
            .join("-r")
            .join(parent_sid)
            .join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("agent-{agent}.jsonl"));
        std::fs::write(&path, jsonl).unwrap();
        path
    }

    /// A QoderWork fork FAMILY (#142): a root session and one forked from it, related through
    /// the `<sid>-session.json` sidecar's `fork_from`, both past the adapter's junk-size gate.
    /// Returns `(root_id, fork_id)`.
    pub fn qoderwork_family(&self) -> (&'static str, &'static str) {
        let qw = self.root.join("qoderwork").join("-r");
        std::fs::create_dir_all(&qw).unwrap();
        let root_id = "aaaaaaaa-0000-4000-8000-000000000001";
        let fork_id = "aaaaaaaa-0000-4000-8000-000000000002";
        let transcript = |salt: &str| -> String {
            let mut out = String::new();
            for i in 0..30u32 {
                out += &user(
                    &format!("prompt {salt} {i} — a line long enough to matter"),
                    i,
                );
                out += &assistant(
                    &format!("reply {salt} {i} — a line long enough to matter"),
                    i,
                );
            }
            assert!(
                out.len() > 4096,
                "past QoderWork's MIN_TRANSCRIPT_BYTES gate"
            );
            out
        };
        std::fs::write(qw.join(format!("{root_id}.jsonl")), transcript("root")).unwrap();
        std::fs::write(
            qw.join(format!("{root_id}-session.json")),
            r#"{"title":"Family root","updated_at":1756800000000}"#,
        )
        .unwrap();
        std::fs::write(qw.join(format!("{fork_id}.jsonl")), transcript("fork")).unwrap();
        std::fs::write(
            qw.join(format!("{fork_id}-session.json")),
            format!(r#"{{"title":"Family root (Fork)","fork_from":"{root_id}","updated_at":1756800100000}}"#),
        )
        .unwrap();
        (root_id, fork_id)
    }

    /// One FINISHED Claude session (old timestamps, no process) — the shape the compose
    /// affordance resumes. Returns its id.
    pub fn claude_finished(&self) -> &'static str {
        let sid = "bbbbbbbb-0000-4000-8000-000000000001";
        let mut out = String::new();
        for i in 0..12u32 {
            out += &user(&format!("prompt {i} — a line long enough to matter"), i);
            out += &assistant(&format!("reply {i} — a line long enough to matter"), i);
        }
        self.claude_session(sid, &out);
        sid
    }
}

// ── the monitor under test ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// `agent-monitor` — the v1 binary: its classic page is the rail.
    V1,
    /// `agent-monitor-v2` — its classic page is the splice shell.
    V2,
}

impl Kind {
    fn binary(self) -> &'static str {
        match self {
            Kind::V1 => "agent-monitor",
            Kind::V2 => "agent-monitor-v2",
        }
    }
    fn package(self) -> &'static str {
        match self {
            Kind::V1 => "claude-monitor",
            Kind::V2 => "claude-monitor-v2",
        }
    }
}

/// A monitor binary running on a fixed loopback port over a scratch state dir, reaped on
/// drop. Missing binary → a PANIC naming the build, never a silent skip: a skipped case
/// reads as green, and a blank shell has passed as 13/16 that way (#53).
pub struct Monitor {
    pub kind: Kind,
    pub port: u16,
    pub state: PathBuf,
    child: Reap,
}

impl Monitor {
    pub fn spawn(
        kind: Kind,
        port: u16,
        base: &Path,
        stores: Option<&Stores>,
        paired: bool,
    ) -> Monitor {
        let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("target/release")
            .join(kind.binary());
        assert!(
            bin.is_file(),
            "{} is not built — run `cargo build --release -p {}` first",
            bin.display(),
            kind.package()
        );
        let state = base.join(format!("state-{port}"));
        std::fs::create_dir_all(&state).unwrap();
        let mut cmd = std::process::Command::new(&bin);
        cmd.args(["--port", &port.to_string()])
            .env("XDG_CACHE_HOME", base)
            .env("CLAUDE_MONITOR_CACHE", base.join(format!("cache-{port}")))
            .env("CLAUDE_MONITOR_STATE", &state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if paired {
            cmd.arg("--pair");
        }
        if kind == Kind::V1 {
            cmd.arg("--no-open");
        }
        if let Some(stores) = stores {
            for (k, v) in stores.envs() {
                cmd.env(k, v);
            }
        }
        let child = Reap(
            cmd.spawn()
                .unwrap_or_else(|e| panic!("{} starts: {e}", kind.binary())),
        );
        std::thread::sleep(Duration::from_millis(1500));
        Monitor {
            kind,
            port,
            state,
            child,
        }
    }

    pub fn url(&self, path_and_query: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}",
            self.port,
            path_and_query.trim_start_matches('/')
        )
    }

    /// The token `--pair` minted, or `None` when unpaired.
    pub fn token(&self) -> Option<String> {
        std::fs::read_to_string(self.state.join("auth-token"))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    /// The first navigation of every tab: `?token=` sets the cookie and redirects to a bare
    /// `/`, dropping every other query — so pair first, then ask for a page.
    pub fn pair(&self, tab: &headless_chrome::Tab) {
        let token = self
            .token()
            .map(|t| format!("?token={t}"))
            .unwrap_or_default();
        tab.navigate_to(&self.url(&token)).unwrap();
        tab.wait_until_navigated().unwrap();
    }

    /// Navigate the tab to a page of this monitor and wait for the navigation.
    pub fn open(&self, tab: &headless_chrome::Tab, path_and_query: &str) {
        tab.navigate_to(&self.url(path_and_query)).unwrap();
        tab.wait_until_navigated().unwrap();
    }
}

// ── chrome, actions, probes ─────────────────────────────────────────────────────────────────

/// Headless Chrome with timer throttling off — a throttled background tab misses polls and
/// reads exactly like a positioning bug.
pub fn chrome() -> headless_chrome::Browser {
    headless_chrome::Browser::new(
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
    .expect("chrome launches")
}

/// A JS expression's PRIMITIVE result (string, number, bool) — objects come back Null; use
/// [`probe`] for those. A promise is awaited.
pub fn eval(tab: &headless_chrome::Tab, js: &str) -> serde_json::Value {
    tab.evaluate(js, true)
        .ok()
        .and_then(|r| r.value)
        .unwrap_or(serde_json::Value::Null)
}

/// A JS expression's result as JSON — the way an OBJECT crosses the CDP boundary by value.
pub fn probe(tab: &headless_chrome::Tab, js: &str) -> serde_json::Value {
    serde_json::from_str(
        eval(tab, &format!("JSON.stringify({js})"))
            .as_str()
            .unwrap_or("null"),
    )
    .unwrap_or(serde_json::Value::Null)
}

/// Poll a boolean JS predicate until true, or PANIC with `what` and a diagnostic — never a
/// vacuous return. `diag` is a JS expression evaluated on timeout (a string).
pub fn until(tab: &headless_chrome::Tab, js: &str, what: &str, timeout: Duration, diag: &str) {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if eval(tab, js).as_bool() == Some(true) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let seen = eval(tab, diag);
    panic!("timed out waiting for {what}; seen: {seen}");
}

/// A key press on the document (the app shell's and the classic page's handlers both listen
/// there); `shift` for the ⇧ variants.
pub fn key(tab: &headless_chrome::Tab, k: &str, shift: bool) {
    eval(tab, &format!("document.dispatchEvent(new KeyboardEvent('keydown', {{key: {k:?}, shiftKey: {shift}, bubbles: true, cancelable: true}})); 'ok'"));
}

/// The reader's intent, then a scroll: a programmatic scroll alone reads as the renderer's
/// own and the follow logic re-pins the tail; a wheel event first is what unpins.
pub fn wheel_scroll(tab: &headless_chrome::Tab, scroller: &str, to: &str) {
    eval(tab, &format!("(function(){{ var s = {scroller}; s.dispatchEvent(new WheelEvent('wheel', {{deltaY: -1, bubbles: true}})); {to}; return 'ok'; }})()"));
}

/// The app shell's transcript scroller.
pub const APP_SCROLLER: &str = "document.querySelector('.transcript')";

/// The app shell: the `data-unit-from` of the first mounted unit at (or within a line above)
/// the viewport top — the unit the reader is "at".
pub fn app_unit_index(tab: &headless_chrome::Tab) -> i64 {
    eval(tab, "(function(){ var s=document.querySelector('.transcript'); if (!s) return -1; var top=s.getBoundingClientRect().top; for (var c of document.querySelector('.virtual-window').children) { if (c.getBoundingClientRect().top >= top - 24) return Number(c.dataset.unitFrom); } return -1; })()")
        .as_i64()
        .unwrap_or(-1)
}

/// The app shell: whether the transcript sits at its tail (within 2px).
pub fn app_at_tail(tab: &headless_chrome::Tab) -> bool {
    eval(tab, "(function(){ var s=document.querySelector('.transcript'); if (!s || !document.querySelector('.virtual-window').children.length) return false; return s.scrollHeight - s.clientHeight - s.scrollTop <= 2; })()")
        .as_bool()
        .unwrap_or(false)
}

/// The classic page (the html server's, or a monitor's classic view): the viewport's state —
/// scrollY, document height, the gap to the bottom, whether it follows, the pill's text.
pub fn classic_view_state(tab: &headless_chrome::Tab) -> serde_json::Value {
    probe(
        tab,
        r#"({
            y: Math.round(window.scrollY),
            h: document.body.scrollHeight,
            gap: Math.round(document.body.scrollHeight - window.innerHeight - window.scrollY),
            following: document.body.classList.contains("following"),
            badge: (document.getElementById("newbadge") || {}).textContent || "",
            badgeOn: /\bon\b/.test((document.getElementById("newbadge") || {className:""}).className),
            blocks: (document.getElementById("stream") || {childElementCount:-1}).childElementCount
        })"#,
    )
}

// ── the two surfaces, one vocabulary ────────────────────────────────────────────────────────
// A scenario is written once and run against both pages. The classic page (the html server's
// `export.js`, the reference) scrolls the DOCUMENT and marks turns with `data-turn` on the
// turn card; the app shell scrolls `.transcript`, virtualizes units and marks user turns with
// `data-turn` on `.turn.user`. The probes below speak in USER-TURN ORDINALS, which both name,
// never in pixels or DOM indexes.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    /// `export.js` on the html server: the reference.
    Classic,
    /// The monitor's app shell (`?ui=app`).
    AppShell,
}

impl Surface {
    /// The scroller's JS expression.
    pub fn scroller(self) -> &'static str {
        match self {
            Surface::Classic => "document.scrollingElement",
            Surface::AppShell => "document.querySelector('.transcript')",
        }
    }
    /// The element whose children carry the turns.
    fn turns_root(self) -> &'static str {
        match self {
            Surface::Classic => "document.getElementById('stream')",
            Surface::AppShell => "document.querySelector('.virtual-window')",
        }
    }
    /// The fold headers a reader opens: the classic page's `.fold-h`, the app shell's
    /// interactive `button.renderer-head`.
    pub fn fold_head(self) -> &'static str {
        match self {
            Surface::Classic => ".fold-h",
            Surface::AppShell => "button.renderer-head",
        }
    }
}

/// The user-turn ordinal at the viewport top: the first `[data-turn]` element at (or within a
/// line above) the top edge of the scroller. -1 when nothing is mounted there yet.
pub fn turn_at_top(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let (scroller, root) = (surface.scroller(), surface.turns_root());
    let top = match surface {
        Surface::Classic => "0".to_string(),
        Surface::AppShell => format!("{scroller}.getBoundingClientRect().top"),
    };
    eval(tab, &format!("(function(){{ var root = {root}; if (!root) return -1; var top = {top}; var els = root.querySelectorAll('[data-turn]'); for (var e of els) {{ if (e.getBoundingClientRect().top >= top - 24) return Number(e.dataset.turn); }} return -1; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

/// Whether the scroller sits at its tail (within 2px).
pub fn at_tail(tab: &headless_chrome::Tab, surface: Surface) -> bool {
    let s = surface.scroller();
    eval(tab, &format!("(function(){{ var s = {s}; if (!s) return false; return s.scrollHeight - s.clientHeight - s.scrollTop <= 2; }})()"))
        .as_bool()
        .unwrap_or(false)
}

/// The reader's scroll: a wheel event first (intent — a bare programmatic scroll reads as the
/// renderer's own and the follow logic re-pins), then a scroll by `dy` pixels.
pub fn scroll_by(tab: &headless_chrome::Tab, surface: Surface, dy: i64) {
    let s = surface.scroller();
    let target = match surface {
        Surface::Classic => "window",
        Surface::AppShell => s,
    };
    eval(tab, &format!("(function(){{ var s = {s}; {target}.dispatchEvent(new WheelEvent('wheel', {{deltaY: {dy}, bubbles: true}})); s.scrollTop = Math.max(0, s.scrollTop + ({dy})); return 'ok'; }})()"));
}

/// Jump to the end the way the page offers it: the classic page's pill / a scroll to the
/// bottom with intent; the app shell's jump-to-bottom control.
pub fn jump_to_end(tab: &headless_chrome::Tab, surface: Surface) {
    match surface {
        Surface::Classic => {
            eval(tab, "(function(){ window.dispatchEvent(new WheelEvent('wheel', {deltaY: 120})); window.scrollTo(0, document.scrollingElement.scrollHeight); return 'ok'; })()");
        }
        Surface::AppShell => {
            eval(tab, "(function(){ var b = document.getElementById('jumpToBottom'); if (b) b.click(); else { var s = document.querySelector('.transcript'); s.scrollTop = s.scrollHeight; } return 'ok'; })()");
        }
    }
}

/// Open the LAST fold header currently in the DOM (near the end after a jump) and return the
/// turn it belongs to, or -1 when there is none mounted.
pub fn open_last_fold(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let head = surface.fold_head();
    eval(tab, &format!("(function(){{ var hs = document.querySelectorAll('{head}'); if (!hs.length) return -1; var h = hs[hs.length - 1]; var t = h.closest('[data-turn]'); h.click(); return t ? Number(t.dataset.turn) : -2; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

/// The number of user turns mounted right now (the app shell mounts a window; the classic
/// page mounts everything it has rendered).
pub fn mounted_turns(tab: &headless_chrome::Tab, surface: Surface) -> i64 {
    let root = surface.turns_root();
    eval(tab, &format!("(function(){{ var r = {root}; return r ? r.querySelectorAll('[data-turn]').length : -1; }})()"))
        .as_i64()
        .unwrap_or(-1)
}

// ── live growth ─────────────────────────────────────────────────────────────────────────────

/// A transcript growing while a page watches it: a thread appends the script's records one
/// per `interval`. The interval must exceed the slower consumer's poll (the app shell's
/// record store polls every 1 s; the classic page's tick is its POLL_MS) or assertions race
/// the apply. The thread stops on drop — drop the driver before the next case takes
/// [`serial`], or it appends into a store another case is measuring.
pub struct LiveGrowth {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub appended: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl LiveGrowth {
    pub fn start(path: PathBuf, script: Vec<String>, interval: Duration) -> LiveGrowth {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let appended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (stop2, appended2) = (stop.clone(), appended.clone());
        let thread = std::thread::spawn(move || {
            for record in script {
                if stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(interval);
                if stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                append(&path, &record);
                appended2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        });
        LiveGrowth {
            stop,
            thread: Some(thread),
            appended,
        }
    }

    /// How many records have been appended so far.
    pub fn count(&self) -> usize {
        self.appended.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Wait until the whole script has been appended (or `timeout`).
    pub fn finish(mut self, timeout: Duration) -> usize {
        let t0 = Instant::now();
        while self.thread.as_ref().map_or(false, |t| !t.is_finished()) && t0.elapsed() < timeout {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.count()
    }
}

impl Drop for LiveGrowth {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
