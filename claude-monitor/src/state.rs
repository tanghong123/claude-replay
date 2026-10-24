//! **The agent-state pass** (#194, design/agent-states.md): derive busy / wait / idle
//! per session per scan tick from what the scan already observed, and dump state
//! CHANGES where any other application can consume them:
//!
//! - `<cache_root>/state/events.jsonl` — append-only transitions ([`StateEvent`] per
//!   line, schema-versioned). No heartbeats, no repeats: a session busy for an hour
//!   writes nothing. Rotated at [`EVENTS_ROTATE_BYTES`] (one previous generation kept).
//! - `<cache_root>/state/current.json` — the snapshot, atomically replaced every tick,
//!   `scanned_at` refreshed either way: the monitor's own heartbeat, so a consumer can
//!   tell "all quiet" from "monitor gone".
//!
//! The DERIVATION is the engine's pure [`derive_state`] — nothing here is carried
//! across ticks except hysteresis staging, so the hook-era failure mode (state asserted
//! once, stuck forever) cannot exist. This module only GATHERS: the transcript-side
//! facts through the engine ([`inflight_tools_in_tail`], [`tail_pulse`]) with a
//! per-mtime cache so a quiet session costs nothing, and the one OS fact the engine
//! must not own — does the attributed process have a live, recent CHILD (a tool
//! actually executing) — through a single `ps` per tick, run only when some session
//! needs the answer.
//!
//! **herdr decides where it can see** (#s238, Hong: "herdr's status is very reliable, since the
//! agent's process runs inside it"): for a session in a herdr pane that herdr names as that
//! pane's session, herdr's own status sets the STATE (`herdr_decides`); the derivation keeps
//! only the reason and detail that agree with it, and a wait the derivation observed stands.
//! herdr is read in the background (`HerdrReader`), so a slow or hung herdr never holds the
//! scan, and an answer older than the transcript's last write is not used.

use claude_replay_core::adapter;
use claude_replay_core::liveness::inflight_tools_in_tail;
use claude_replay_core::state::{
    derive_state, tail_pulse, AgentState, PendingTool, StateEvent, StateReason, StateSignals,
    Verdict,
};
use claude_replay_core::Agent;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

/// What the scan hands over per session — everything the derivation needs that the
/// index already computed (growth, attribution), plus identity for the event payload.
pub(crate) struct RowFacts {
    pub sid: String,
    pub agent: Agent,
    pub path: PathBuf,
    pub cwd: Option<String>,
    pub title: String,
    /// The rail's growing signal (content-clock advance within the linger window).
    pub growing: bool,
    /// Seconds since the last observed content activity (falls back to tree mtime).
    pub quiet_secs: u64,
    /// The attributed LIVE agent process, when the link resolved.
    pub pid: Option<u32>,
    /// The controllable terminal target (tmux pane / screen name), when one exists.
    pub term: Option<String>,
    /// The pane a send types into, when the link is in tmux (#133) or herdr (#s226).
    pub pane: Option<crate::index::PaneAddr>,
    /// Whether the process↔session link is PROVEN (#133 §3.1): sid-in-argv, fd held, or
    /// growth-proved — NEVER a cwd heuristic. Only a proven link may be injected into.
    pub confirmed: bool,
    /// The process names the session exactly in its own argv (#s234).
    pub named: bool,
    /// The tree mtime — the cache key for the content-derived facts.
    pub tree_mtime: Option<SystemTime>,
}

/// A session with NO live process that has been quiet longer than this needs no
/// content reads at all: it is `idle · exited` by rule 1, and re-reading hundreds of
/// long-dead transcripts every tick would be the cold-start stall R7 exists to forbid.
const INTEREST_QUIET_SECS: u64 = 3600;

/// Rotate `events.jsonl` past this size, keeping one previous generation.
pub const EVENTS_ROTATE_BYTES: u64 = 4 * 1024 * 1024;

/// A recent child of the agent process = a tool actually executing. "Recent" is judged
/// against the session's quiet time plus this slack, so a long-lived MCP server child
/// (started at session birth) does not read as a running tool, while the child of the
/// pending call — spawned at roughly the last content write — does.
const CHILD_AGE_SLACK_SECS: u64 = 180;

#[derive(Default)]
pub(crate) struct StateTracker {
    /// Content-derived facts, keyed by the tree mtime and the transcript's length that
    /// produced them — a quiet session's tick costs one `stat` and no reads.
    content: HashMap<String, ContentFacts>,
    /// Hysteresis staging + the last PUBLISHED verdict per session.
    staged: HashMap<String, Staged>,
    /// herdr's word on its panes' agents (#s238).
    herdr: HerdrReader,
}

struct ContentFacts {
    at: Option<SystemTime>,
    /// The transcript's length when read. The mtime alone misses an append made in the same
    /// clock tick as the last read: Linux stamps mtimes from a coarse clock (a few ms), and a
    /// session whose final append landed so stayed `wait` after its turn ended (#s136).
    len: Option<u64>,
    pending: Vec<PendingTool>,
    last: claude_replay_core::state::TailLast,
    final_text: Option<String>,
    queued_prompt: bool,
    last_tool_error: bool,
    /// The turn's last word was a failure note (#249) — see `StateSignals::last_failure`.
    last_failure: bool,
    ends_with_question: bool,
}

struct Staged {
    /// The last verdict written to the stream (with its publish timestamp).
    published: Option<(Verdict, String)>,
    /// The previous tick's derivation — the one-tick stability gate.
    candidate: Option<(AgentState, StateReason)>,
    /// herdr decided the published verdict (#s238), as this tick last saw it.
    by_herdr: bool,
}

impl StateTracker {
    /// The latest published verdict for `sid`.
    ///
    /// The monitor index exposes this as additive `agentState*` fields in its existing
    /// `/api/sessions` rows.  Keeping the accessor here means the browser consumes the same
    /// hysteresis-gated verdict that is written to `state/current.json`; it never grows a
    /// second, UI-only state classifier.
    pub(crate) fn current(&self, sid: &str) -> Option<(&Verdict, &str)> {
        self.staged
            .get(sid)
            .and_then(|s| s.published.as_ref())
            .map(|(verdict, since)| (verdict, since.as_str()))
    }

    /// Whether herdr decided `sid`'s published verdict (#s238): the row's `stateSource`.
    pub(crate) fn by_herdr(&self, sid: &str) -> bool {
        self.staged.get(sid).is_some_and(|s| s.by_herdr)
    }

    /// One tick: derive every interesting session's state, publish transitions, and
    /// rewrite the snapshot. `facts` is what the scan's assemble pass observed.
    pub(crate) fn tick(&mut self, cache_root: &Path, facts: &[RowFacts]) {
        let dir = cache_root.join("state");
        let _ = std::fs::create_dir_all(&dir);
        let now_ts = rfc3339_now();

        // The children probe is one `ps` for the whole tick, and only when some session
        // actually needs rule 5's answer (pending non-interactive tool, not growing).
        let mut children: Option<HashMap<u32, Vec<(u32, u64)>>> = None;

        let mut snapshot: Vec<serde_json::Value> = Vec::new();
        let mut events: Vec<StateEvent> = Vec::new();
        // herdr's last answer per server, read once per tick (#s238).
        let mut herdr_answers: HashMap<String, Option<HerdrAnswer>> = HashMap::new();

        for f in facts {
            let interesting = f.pid.is_some() || f.quiet_secs < INTEREST_QUIET_SECS;
            // herdr's status for this session, when it sits in a live herdr pane, herdr names it
            // as that pane's session, and herdr looked after the transcript last moved: a pane
            // whose agent was replaced or never reported, or an answer older than the newest
            // write, is left to the derivation.
            let herdr_status = match (&f.pane, f.pid) {
                (
                    Some(crate::index::PaneAddr::Herdr {
                        socket: Some(socket),
                        pane,
                    }),
                    Some(_),
                ) => herdr_answers
                    .entry(socket.clone())
                    .or_insert_with(|| self.herdr.answer(socket))
                    .as_ref()
                    .filter(|a| f.tree_mtime.is_none_or(|m| m <= a.asked_at))
                    .and_then(|a| {
                        a.panes.iter().find(|p| {
                            &p.pane == pane && p.session.as_deref() == Some(f.sid.as_str())
                        })
                    })
                    .map(|p| p.status.clone()),
                _ => None,
            };
            let (verdict, by_herdr) = if !interesting {
                (
                    // Rule 1 without any reads: long-quiet and processless is exited.
                    Verdict {
                        state: AgentState::Idle,
                        reason: StateReason::Exited,
                        detail: String::new(),
                        confidence: claude_replay_core::state::Confidence::Observed,
                    },
                    false,
                )
            } else {
                let content = self.content_facts(f);
                let needs_children =
                    f.pid.is_some() && !f.growing && content.pending.iter().any(|t| !t.interactive);
                let tool_children = if needs_children {
                    let table = children.get_or_insert_with(ps_children);
                    f.pid.map(|pid| {
                        has_recent_child(table, pid, f.quiet_secs + CHILD_AGE_SLACK_SECS)
                    })
                } else {
                    None
                };
                // #252, gathered the same way `tool_children` is: only when it could change
                // the verdict. A session that is growing, or has a tool pending, is already
                // busy or already waiting — the fleet question only matters for one that would
                // otherwise read idle. `spawn_rosters` short-circuits on an `is_dir` probe, so
                // the sessions that never ran a workflow (42 of 53 measured here) pay one stat.
                // `f.pid.is_some()` matters for CORRECTNESS, not just cost: a workflow's agents
                // run INSIDE the parent process, so if that process is gone the run is gone
                // too — and its journal is frozen with members that never recorded a result,
                // which would otherwise read as a live fleet forever. A dead process reaches
                // the `Exited` rule before the fleet rule anyway; this stops us paying for the
                // probe, and stops the signal being a lie.
                let fleet_running = f.pid.is_some()
                    && !f.growing
                    && content.pending.is_empty()
                    && adapter(f.agent)
                        .spawn_rosters(&f.path)
                        .iter()
                        .flat_map(|r| &r.members)
                        .any(|m| !m.status.is_terminal());
                let signals = StateSignals {
                    process_alive: f.pid.is_some(),
                    tool_children,
                    grew_recently: f.growing,
                    quiet_secs: f.quiet_secs,
                    pending: content.pending.clone(),
                    queued_prompt: content.queued_prompt,
                    last: content.last,
                    ends_with_question: content.ends_with_question,
                    final_line: content.final_text.as_deref().map(first_line_snippet),
                    last_tool_error: content.last_tool_error,
                    last_failure: content.last_failure,
                    fleet_running,
                    // What the client says it waits on, outside its transcript (#s76): asked of
                    // a live process only, since a dead one's status file speaks for no one.
                    client_wait: f.pid.and_then(|_| adapter(f.agent).client_wait(&f.path)),
                };
                let derived = derive_state(&signals);
                match herdr_status
                    .as_deref()
                    .and_then(|status| herdr_decides(&derived, status))
                {
                    Some(decided) => (decided, true),
                    None => (derived, false),
                }
            };

            let staged = self.staged.entry(f.sid.clone()).or_insert(Staged {
                published: None,
                candidate: None,
                by_herdr: false,
            });
            let key = (verdict.state, verdict.reason);
            let differs = staged
                .published
                .as_ref()
                .is_none_or(|(p, _)| (p.state, p.reason) != key);
            // Wait and attention-idle transitions publish immediately — they are what a
            // consumer is waiting to hear. Everything else needs one stable tick, the
            // anti-flap the growth hold gives the rail. A wait herdr SAW is no inference to
            // steady either (#s238).
            let immediate = matches!(
                verdict.reason,
                StateReason::Question
                    | StateReason::PlanApproval
                    | StateReason::Exited
                    | StateReason::ExitedMidWork
            ) || (by_herdr && verdict.state == AgentState::Wait);
            let stable = staged.candidate == Some(key);
            if differs && (immediate || stable) {
                // A first sighting that is merely idle·exited is bookkeeping, not news:
                // publishing hundreds of them on every monitor restart would bury the
                // signal consumers subscribe for.
                let first = staged.published.is_none();
                let newsworthy = !(first && verdict.reason == StateReason::Exited);
                if newsworthy {
                    events.push(StateEvent {
                        v: 1,
                        ts: now_ts.clone(),
                        sid: f.sid.clone(),
                        agent: f.agent.label().to_string(),
                        cwd: f.cwd.clone(),
                        title: f.title.clone(),
                        state: verdict.state,
                        prev: staged.published.as_ref().map(|(p, _)| p.state),
                        reason: verdict.reason,
                        detail: verdict.detail.clone(),
                        confidence: verdict.confidence,
                        pid: f.pid,
                        term: f.term.clone(),
                    });
                }
                staged.published = Some((verdict.clone(), now_ts.clone()));
            }
            if !differs {
                // The same state and reason, now seen (herdr's, or the client's own wait): the
                // published verdict says how it is known, and no event is written for it.
                if let Some((p, _)) = staged.published.as_mut() {
                    p.confidence = verdict.confidence;
                }
            }
            if !differs
                || staged
                    .published
                    .as_ref()
                    .is_some_and(|(p, _)| p == &verdict)
            {
                staged.by_herdr = by_herdr;
            }
            staged.candidate = Some(key);

            if let Some((p, since)) = &staged.published {
                snapshot.push(serde_json::json!({
                    "sid": f.sid,
                    "agent": f.agent.label(),
                    "cwd": f.cwd,
                    "title": f.title,
                    "state": p.state,
                    "reason": p.reason,
                    "detail": p.detail,
                    "confidence": p.confidence,
                    "since": since,
                    "pid": f.pid,
                    "term": f.term,
                }));
            }
        }
        // Sessions that vanished from the store drop out of staging with them.
        let live: std::collections::HashSet<&str> = facts.iter().map(|f| f.sid.as_str()).collect();
        self.staged.retain(|sid, _| live.contains(sid.as_str()));
        self.content.retain(|sid, _| live.contains(sid.as_str()));

        append_events(&dir.join("events.jsonl"), &events);
        write_current(&dir.join("current.json"), &now_ts, &snapshot);
    }

    /// The content-derived facts, recomputed only when the tree mtime or the transcript's
    /// length moved.
    fn content_facts(&mut self, f: &RowFacts) -> &ContentFacts {
        let len = std::fs::metadata(&f.path).map(|m| m.len()).ok();
        let stale = self
            .content
            .get(&f.sid)
            .is_none_or(|c| c.at != f.tree_mtime || c.len != len);
        if stale {
            let a = adapter(f.agent);
            let pending: Vec<PendingTool> = inflight_tools_in_tail(&f.path)
                .into_iter()
                .map(|t| PendingTool {
                    interactive: a.tool_is_interactive(&t.name),
                    id: t.id,
                    name: t.name,
                })
                .collect();
            let pulse = tail_pulse(a, &f.path);
            let ends_with_question = pulse
                .final_text
                .as_deref()
                .is_some_and(|t| a.ends_with_question(t));
            self.content.insert(
                f.sid.clone(),
                ContentFacts {
                    at: f.tree_mtime,
                    len,
                    pending,
                    last: pulse.last,
                    final_text: pulse.final_text,
                    queued_prompt: pulse.queued_prompt,
                    last_tool_error: pulse.last_tool_error,
                    last_failure: pulse.last_failure,
                    ends_with_question,
                },
            );
        }
        self.content.get(&f.sid).expect("just inserted")
    }
}

/// How often herdr is asked while a session sits in one of its panes.
const HERDR_ASK: std::time::Duration = std::time::Duration::from_secs(2);

/// How long herdr's last answer stands. A server that stopped answering (gone, hung, its socket
/// no longer this user's) leaves its sessions to the derivation after this.
const HERDR_FRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a server is still asked after the last tick that wanted it (no page is polling).
const HERDR_WANTED: std::time::Duration = std::time::Duration::from_secs(30);

type HerdrRead = dyn Fn(&str) -> anyhow::Result<Vec<crate::control::HerdrPane>> + Send + Sync;

/// herdr's last answer for one server: its panes, and when the ask began, on the wall clock a
/// transcript's mtime is read on, so a session whose transcript moved after herdr looked is left
/// to the derivation (the newer evidence wins).
#[derive(Clone)]
pub(crate) struct HerdrAnswer {
    asked_at: SystemTime,
    got: Instant,
    panes: Vec<crate::control::HerdrPane>,
}

/// herdr's word on its panes' agents (#s238): one `herdr agent list` per server every
/// `HERDR_ASK`, on a thread of its own while a tick has wanted that server within `HERDR_WANTED`.
/// The scan holds the state lock and a herdr step may take seconds (`HERDR_STEP`), so a tick
/// never asks: it takes the last answer while it is fresh, and the first tick that meets a server
/// has none yet and derives as before. One asker per server, and it alone forgets its server,
/// as it ends.
pub(crate) struct HerdrReader {
    read: Arc<HerdrRead>,
    servers: Arc<Mutex<HashMap<String, HerdrServer>>>,
    askers: std::sync::atomic::AtomicU64,
    every: std::time::Duration,
    fresh: std::time::Duration,
    wanted_for: std::time::Duration,
}

#[derive(Default)]
struct HerdrServer {
    /// The asker thread serving it, 0 for none.
    asker: u64,
    wanted: Option<Instant>,
    answer: Option<HerdrAnswer>,
}

impl Default for HerdrReader {
    fn default() -> Self {
        Self::with(Arc::new(crate::control::herdr_agent_list))
    }
}

impl HerdrReader {
    fn with(read: Arc<HerdrRead>) -> Self {
        Self::timed(read, HERDR_ASK, HERDR_FRESH, HERDR_WANTED)
    }

    fn timed(
        read: Arc<HerdrRead>,
        every: std::time::Duration,
        fresh: std::time::Duration,
        wanted_for: std::time::Duration,
    ) -> Self {
        HerdrReader {
            read,
            servers: Arc::default(),
            askers: std::sync::atomic::AtomicU64::new(1),
            every,
            fresh,
            wanted_for,
        }
    }

    /// The server's last answer while it is fresh, starting its asker when it has none. A failed
    /// ask keeps the answer before it until that ages out.
    fn answer(&self, socket: &str) -> Option<HerdrAnswer> {
        let mut servers = self.servers.lock().unwrap_or_else(|e| e.into_inner());
        let server = servers.entry(socket.to_string()).or_default();
        server.wanted = Some(Instant::now());
        if server.asker == 0 {
            let id = self
                .askers
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            server.asker = id;
            let (read, servers, socket) =
                (self.read.clone(), self.servers.clone(), socket.to_string());
            let (every, wanted_for) = (self.every, self.wanted_for);
            let spawned = std::thread::Builder::new()
                .name("herdr-asker".into())
                .spawn(move || {
                    // However the asker ends — unwanted, or a read that panicked — it forgets its
                    // server, so the next tick that wants it starts another.
                    let _forget = ForgetOnExit {
                        servers: servers.clone(),
                        socket: socket.clone(),
                        id,
                    };
                    loop {
                        let asked_at = SystemTime::now();
                        let answer = read(&socket);
                        let mut all = servers.lock().unwrap_or_else(|e| e.into_inner());
                        let Some(server) = all.get_mut(&socket).filter(|s| s.asker == id) else {
                            return;
                        };
                        if let Ok(panes) = answer {
                            server.answer = Some(HerdrAnswer {
                                asked_at,
                                got: Instant::now(),
                                panes,
                            });
                        }
                        if server.wanted.is_none_or(|w| w.elapsed() > wanted_for) {
                            return;
                        }
                        drop(all);
                        std::thread::sleep(every);
                    }
                });
            if spawned.is_err() {
                server.asker = 0;
            }
        }
        server
            .answer
            .as_ref()
            .filter(|a| a.got.elapsed() <= self.fresh)
            .cloned()
    }
}

/// Removes an asker's server from the reader as the asker ends, unless a newer asker serves it.
struct ForgetOnExit {
    servers: Arc<Mutex<HashMap<String, HerdrServer>>>,
    socket: String,
    id: u64,
}

impl Drop for ForgetOnExit {
    fn drop(&mut self) {
        let mut all = self.servers.lock().unwrap_or_else(|e| e.into_inner());
        if all.get(&self.socket).is_some_and(|s| s.asker == self.id) {
            all.remove(&self.socket);
        }
    }
}

/// The verdict once herdr's status for the session's pane is heard (#s238). herdr draws the pane,
/// so it sees what the transcript cannot (a dialog open, a spinner running) and its status sets
/// the STATE over the derivation's INFERENCES; the derivation keeps the reason and detail that
/// agree with it, and otherwise the plainest reason for that state stands in. A wait the
/// derivation OBSERVED (a pending question or plan, the client's own wait) stands whatever herdr
/// says, herdr deciding it only when it agrees (`blocked`): the client said so itself, and a
/// herdr rule that missed its dialog must not hide a session that needs a person.
///
/// - `working` is busy: a derived busy reason stays (a tool and its name); the inferred
///   permission wait on a named tool is that tool running; anything else is thinking.
/// - `blocked` is a wait: a derived wait stays; a tool the derivation had running is that tool
///   awaiting permission; anything else a permission.
/// - `idle` and `done` (finished since the pane was last looked at) are idle: a derived idle
///   reason stays (a turn that ended on a question, an error or a failure still asks for a
///   person), except a stall, which is the derivation's guess at a turn that should be running;
///   and a workflow fleet stays busy, since its members run where herdr's screen does not show.
///   Anything else is done.
/// - `unknown`, or anything else, leaves the derivation as it is: `None`.
///
/// Whatever herdr decided is `Observed`.
pub(crate) fn herdr_decides(derived: &Verdict, status: &str) -> Option<Verdict> {
    use claude_replay_core::state::Confidence;
    let keep = |v: &Verdict| Verdict {
        confidence: Confidence::Observed,
        ..v.clone()
    };
    let seen = |state, reason, detail: &str| Verdict {
        state,
        reason,
        detail: detail.to_string(),
        confidence: Confidence::Observed,
    };
    if !matches!(status, "working" | "blocked" | "idle" | "done") {
        return None;
    }
    if derived.state == AgentState::Wait && derived.confidence == Confidence::Observed {
        // herdr agreeing (blocked) says so; disagreeing, it decides nothing.
        return (status == "blocked").then(|| derived.clone());
    }
    Some(match status {
        "working" => match (derived.state, derived.reason) {
            (AgentState::Busy, _) => keep(derived),
            (AgentState::Wait, StateReason::Permission) if !derived.detail.is_empty() => {
                seen(AgentState::Busy, StateReason::Tool, &derived.detail)
            }
            _ => seen(AgentState::Busy, StateReason::Thinking, ""),
        },
        "blocked" => match (derived.state, derived.reason) {
            (AgentState::Wait, _) => keep(derived),
            (AgentState::Busy, StateReason::Tool) => {
                seen(AgentState::Wait, StateReason::Permission, &derived.detail)
            }
            _ => seen(AgentState::Wait, StateReason::Permission, ""),
        },
        _ => match (derived.state, derived.reason) {
            (AgentState::Idle, StateReason::Stalled) => {
                seen(AgentState::Idle, StateReason::Done, "")
            }
            (AgentState::Idle, _) | (AgentState::Busy, StateReason::Fleet) => keep(derived),
            _ => seen(AgentState::Idle, StateReason::Done, ""),
        },
    })
}

fn first_line_snippet(t: &str) -> String {
    let line = t.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut s: String = line.chars().take(160).collect();
    if s.len() < line.len() {
        s.push('…');
    }
    s
}

fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_epoch(secs)
}

/// Epoch seconds → `YYYY-MM-DDTHH:MM:SSZ`, no dependencies (civil-from-days,
/// the Hinnant algorithm).
fn rfc3339_from_epoch(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// One `ps` for the whole tick: pid → children as `(child_pid, age_secs)`.
fn ps_children() -> HashMap<u32, Vec<(u32, u64)>> {
    let mut map: HashMap<u32, Vec<(u32, u64)>> = HashMap::new();
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,etime="])
        .output()
    else {
        return map;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(etime)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        // Unparseable reads as 0 (recent): the direction that keeps a session busy rather than
        // inventing a permission wait.
        map.entry(ppid)
            .or_default()
            .push((pid, parse_etime(etime).unwrap_or(0)));
    }
    map
}

/// `ps` etime (`[[dd-]hh:]mm:ss`, macOS and procps alike) → seconds; `None` when it does not
/// parse. Shared with the process table's start times (`index`, #s50), which must not guess.
pub(crate) fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let hms = match parts.as_slice() {
        [h, m, sec] => h * 3600 + m * 60 + sec,
        [m, sec] => m * 60 + sec,
        [sec] => *sec,
        _ => return None,
    };
    Some(days * 86400 + hms)
}

/// Whether `pid` has a DIRECT child younger than `max_age_secs` — the "a tool is
/// actually executing" fact of rule 5.
fn has_recent_child(table: &HashMap<u32, Vec<(u32, u64)>>, pid: u32, max_age_secs: u64) -> bool {
    table
        .get(&pid)
        .is_some_and(|kids| kids.iter().any(|(_, age)| *age <= max_age_secs))
}

fn append_events(path: &Path, events: &[StateEvent]) {
    if events.is_empty() {
        return;
    }
    // Rotate BEFORE appending, so a consumer tailing the live file sees a clean break.
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > EVENTS_ROTATE_BYTES {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    use std::io::Write;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    for e in events {
        if let Ok(line) = serde_json::to_string(e) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Atomic replace: consumers never observe a torn snapshot.
fn write_current(path: &Path, scanned_at: &str, sessions: &[serde_json::Value]) {
    let body = serde_json::json!({ "v": 1, "scanned_at": scanned_at, "sessions": sessions });
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cm-state-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn facts(sid: &str, path: &Path, pid: Option<u32>, quiet: u64) -> RowFacts {
        RowFacts {
            sid: sid.into(),
            agent: Agent::CLAUDE,
            path: path.to_path_buf(),
            cwd: Some("/w".into()),
            title: "t".into(),
            growing: false,
            quiet_secs: quiet,
            pid,
            term: None,
            pane: None,
            confirmed: false,
            named: false,
            tree_mtime: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
        }
    }

    /// #252 end to end — a session whose workflow fleet is still running is BUSY, read off
    /// the real journal on disk rather than a synthesised signal. The launching turn has ENDED
    /// and nothing is pending, which is exactly the shape that used to read `Done`.
    #[test]
    fn a_live_fleet_keeps_the_session_busy() {
        let root = scratch("fleet");
        let t = root.join("s1.jsonl");
        std::fs::write(&t, concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"fan it out"}]},"timestamp":"2026-08-14T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{"type":"text","text":"Launched the run."}]},"timestamp":"2026-08-14T10:00:05Z"}"#, "\n",
        )).unwrap();

        // No journal yet: the launching turn ended and nothing is pending, so this is `Done`.
        // `current.json` is the full picture every tick; `events.jsonl` only records CHANGES,
        // so the snapshot is what a state assertion should read.
        let verdict = |root: &Path| -> (String, String) {
            let cur: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(root.join("state/current.json")).unwrap(),
            )
            .unwrap();
            (
                cur["sessions"][0]["state"].as_str().unwrap_or("?").into(),
                cur["sessions"][0]["reason"].as_str().unwrap_or("?").into(),
            )
        };

        let mut tr = StateTracker::default();
        tr.tick(&root, &[facts("s1", &t, Some(7371), 5)]);
        tr.tick(&root, &[facts("s1", &t, Some(7371), 6)]);
        assert_eq!(
            verdict(&root),
            ("idle".to_string(), "done".to_string()),
            "the shape of the bug: a launched fleet leaves the parent looking finished"
        );

        // Now the run's journal says two members STARTED and neither has returned.
        let runs = root.join("s1/subagents/workflows/wf_1");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(
            runs.join("journal.jsonl"),
            concat!(
                r#"{"type":"launched"}"#,
                "\n",
                r#"{"type":"started","agentId":"a1","label":"find:one","phase":"Find"}"#,
                "\n",
                r#"{"type":"started","agentId":"a2","label":"find:two","phase":"Find"}"#,
                "\n",
            ),
        )
        .unwrap();
        tr.tick(&root, &[facts("s1", &t, Some(7371), 900)]);
        tr.tick(&root, &[facts("s1", &t, Some(7371), 901)]);
        assert_eq!(
            verdict(&root),
            ("busy".to_string(), "fleet".to_string()),
            "…and with members running it is busy — note the 900s of quiet, which without this \
             would have aged the session into `Stalled`, i.e. into the BLOCKED bucket"
        );

        // A member returning ends it: the run is over and the session is idle again.
        std::fs::write(
            runs.join("journal.jsonl"),
            concat!(
                r#"{"type":"launched"}"#,
                "\n",
                r#"{"type":"started","agentId":"a1","label":"find:one","phase":"Find"}"#,
                "\n",
                r#"{"type":"started","agentId":"a2","label":"find:two","phase":"Find"}"#,
                "\n",
                r#"{"type":"result","agentId":"a1","result":"done one"}"#,
                "\n",
                r#"{"type":"result","agentId":"a2","result":"done two"}"#,
                "\n",
            ),
        )
        .unwrap();
        tr.tick(&root, &[facts("s1", &t, Some(7371), 902)]);
        tr.tick(&root, &[facts("s1", &t, Some(7371), 903)]);
        assert_eq!(
            verdict(&root).1,
            "done",
            "every member returned, so the fleet no longer holds the session busy"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The end-to-end pass on a real (fixture) transcript: a pending AskUserQuestion
    /// with a live process publishes `wait · question` IMMEDIATELY; answering it and
    /// ending the turn publishes `idle · ended-question` after one stable tick; the
    /// files land under `<root>/state/` in the documented shapes.
    #[test]
    fn transitions_are_derived_published_and_dumped() {
        let root = scratch("e2e");
        let t = root.join("s1.jsonl");
        std::fs::write(&t, concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-14T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":null,"content":[{"type":"text","text":"Which color?"},{"type":"tool_use","id":"toolu_q1","name":"AskUserQuestion","input":{"questions":[{"question":"Which color?"}]}}]},"timestamp":"2026-08-14T10:00:05Z"}"#, "\n",
        )).unwrap();

        let mut tr = StateTracker::default();
        tr.tick(&root, &[facts("s1", &t, Some(4242), 5)]);
        let ev = std::fs::read_to_string(root.join("state/events.jsonl")).unwrap();
        assert_eq!(ev.lines().count(), 1, "one immediate wait event:\n{ev}");
        let e: StateEvent = serde_json::from_str(ev.lines().next().unwrap()).unwrap();
        assert_eq!(
            (e.state, e.reason),
            (AgentState::Wait, StateReason::Question)
        );
        assert!(e.detail.contains("Which color?"), "detail: {}", e.detail);
        assert_eq!(e.pid, Some(4242));

        // The question is answered and the turn ends.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_q1","content":"blue","is_error":false}}]}},"timestamp":"2026-08-14T10:00:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{{"type":"text","text":"Blue it is — want me to apply it?"}}]}},"timestamp":"2026-08-14T10:00:35Z"}}"#).unwrap();
        drop(f);
        let fresh = facts("s1", &t, Some(4242), 5);
        tr.tick(&root, std::slice::from_ref(&fresh)); // candidate tick (non-immediate)
        tr.tick(&root, std::slice::from_ref(&fresh)); // stable → published
        let ev = std::fs::read_to_string(root.join("state/events.jsonl")).unwrap();
        assert_eq!(ev.lines().count(), 2, "the idle transition landed:\n{ev}");
        let e: StateEvent = serde_json::from_str(ev.lines().nth(1).unwrap()).unwrap();
        assert_eq!(
            (e.state, e.reason, e.prev),
            (
                AgentState::Idle,
                StateReason::EndedQuestion,
                Some(AgentState::Wait)
            )
        );

        // The snapshot heartbeat exists and carries the session.
        let cur: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("state/current.json")).unwrap(),
        )
        .unwrap();
        assert!(cur["scanned_at"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(cur["sessions"][0]["sid"], "s1");
        assert_eq!(cur["sessions"][0]["state"], "idle");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #s136: an append that leaves the mtime where it was is still read. Linux stamps mtimes
    /// from a coarse clock, so a turn's last append inside the tick of the previous read kept
    /// the cached content: the session stayed `wait · question` after its turn ended (a Code
    /// runner failed the case above this way). The mtime is held here, so it fails anywhere.
    #[test]
    fn an_append_within_one_mtime_tick_is_read() {
        let root = scratch("tick");
        let t = root.join("s1.jsonl");
        std::fs::write(&t, concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-14T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":null,"content":[{"type":"text","text":"Which color?"},{"type":"tool_use","id":"toolu_q1","name":"AskUserQuestion","input":{"questions":[{"question":"Which color?"}]}}]},"timestamp":"2026-08-14T10:00:05Z"}"#, "\n",
        )).unwrap();
        let held = std::fs::metadata(&t).unwrap().modified().unwrap();

        let mut tr = StateTracker::default();
        tr.tick(&root, &[facts("s1", &t, Some(4242), 5)]);

        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_q1","content":"blue","is_error":false}}]}},"timestamp":"2026-08-14T10:00:30Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","message":{{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{{"type":"text","text":"Blue it is."}}]}},"timestamp":"2026-08-14T10:00:35Z"}}"#).unwrap();
        // The append landed in the same tick as the first read.
        f.set_modified(held).unwrap();
        drop(f);
        let fresh = facts("s1", &t, Some(4242), 5);
        assert_eq!(fresh.tree_mtime, Some(held), "the mtime is held");
        tr.tick(&root, std::slice::from_ref(&fresh));
        tr.tick(&root, std::slice::from_ref(&fresh));

        let ev = std::fs::read_to_string(root.join("state/events.jsonl")).unwrap();
        assert_eq!(ev.lines().count(), 2, "the idle transition landed:\n{ev}");
        let e: StateEvent = serde_json::from_str(ev.lines().nth(1).unwrap()).unwrap();
        assert_eq!((e.state, e.reason), (AgentState::Idle, StateReason::Done));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #s68 end to end: a session Claude Code compacted while idle at its prompt stays
    /// `idle · done`, idle since its answer. The compaction put that answer far beyond the
    /// tail's first window, and read there the session was a turn with no word yet:
    /// `busy · thinking`, then — the clock run out — `idle · stalled`, the BLOCKED bucket.
    #[test]
    fn an_idle_compaction_leaves_the_session_idle_since_its_answer() {
        let root = scratch("idlecompact");
        let t = root.join("s1.jsonl");
        std::fs::write(&t, concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-10-09T01:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{"type":"text","text":"Shipped it."}]},"timestamp":"2026-10-09T01:15:00Z"}"#, "\n",
        )).unwrap();
        let published = |root: &Path| -> (String, String, String) {
            let cur: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(root.join("state/current.json")).unwrap(),
            )
            .unwrap();
            let s = &cur["sessions"][0];
            let field = |k: &str| s[k].as_str().unwrap_or("?").to_string();
            (field("state"), field("reason"), field("since"))
        };
        let mut tr = StateTracker::default();
        tr.tick(&root, &[facts("s1", &t, Some(5151), 5)]);
        tr.tick(&root, &[facts("s1", &t, Some(5151), 6)]);
        let idle = published(&root);
        assert_eq!((idle.0.as_str(), idle.1.as_str()), ("idle", "done"));

        // Fifty-one minutes on, Claude Code compacts it while idle.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        writeln!(f, r#"{{"type":"system","subtype":"compact_boundary","level":"info","content":"Conversation compacted","timestamp":"2026-10-09T02:06:00Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{{"role":"user","content":"This session is being continued."}},"timestamp":"2026-10-09T02:06:01Z"}}"#).unwrap();
        writeln!(f, r#"{{"type":"attachment","attachment":{{"type":"invoked_skills","content":"{}"}},"timestamp":"2026-10-09T02:06:01Z"}}"#, "x".repeat(150 * 1024)).unwrap();
        writeln!(f, r#"{{"type":"system","subtype":"informational","level":"notice","content":"Compacted while idle, before the prompt cache expired","timestamp":"2026-10-09T02:06:02Z"}}"#).unwrap();
        drop(f);
        tr.tick(&root, &[facts("s1", &t, Some(5151), 3060)]);
        tr.tick(&root, &[facts("s1", &t, Some(5151), 3061)]);
        assert_eq!(
            published(&root),
            idle,
            "still idle · done, and idle since the answer"
        );
        let ev = std::fs::read_to_string(root.join("state/events.jsonl")).unwrap();
        assert_eq!(ev.lines().count(), 1, "nothing new to report:\n{ev}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A long-dead session costs no content reads and publishes nothing — a monitor
    /// restart over a machine of old transcripts must not bury consumers in
    /// `idle · exited` bookkeeping.
    #[test]
    fn dead_history_is_silent_and_read_free() {
        let root = scratch("dead");
        let t = root.join("old.jsonl");
        std::fs::write(&t, "not even valid json\n").unwrap();
        let mut tr = StateTracker::default();
        tr.tick(&root, &[facts("old", &t, None, INTEREST_QUIET_SECS + 1)]);
        assert!(
            !root.join("state/events.jsonl").exists(),
            "no events for dead history"
        );
        assert!(tr.content.is_empty(), "no content reads were spent");
        // The snapshot still carries it, so `current.json` is the full picture.
        let cur: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("state/current.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cur["sessions"][0]["state"], "idle");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Rotation: a full events file is renamed to `.1` before the next append.
    #[test]
    fn events_rotate_with_one_generation() {
        let root = scratch("rot");
        let dir = root.join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(&path, vec![b'x'; (EVENTS_ROTATE_BYTES + 1) as usize]).unwrap();
        let e = StateEvent {
            v: 1,
            ts: "2026-08-14T00:00:00Z".into(),
            sid: "s".into(),
            agent: "claude".into(),
            cwd: None,
            title: String::new(),
            state: AgentState::Busy,
            prev: None,
            reason: StateReason::Tool,
            detail: "Bash".into(),
            confidence: claude_replay_core::state::Confidence::Observed,
            pid: None,
            term: None,
        };
        append_events(&path, std::slice::from_ref(&e));
        assert!(
            path.with_extension("jsonl.1").exists(),
            "rotated generation"
        );
        let live = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            live.lines().count(),
            1,
            "the live file holds only the new event"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The dependency-free RFC3339 formatter against known instants.
    #[test]
    fn rfc3339_formats_known_instants() {
        assert_eq!(rfc3339_from_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_epoch(1_786_666_739), "2026-08-14T00:18:59Z");
    }

    /// The etime parser across `ps`'s shapes.
    #[test]
    fn etime_parses_all_shapes() {
        assert_eq!(parse_etime("05"), Some(5));
        assert_eq!(parse_etime("01:05"), Some(65));
        assert_eq!(parse_etime("02:01:05"), Some(7265));
        assert_eq!(parse_etime("3-02:01:05"), Some(3 * 86400 + 7265));
        assert_eq!(parse_etime("a:b"), None);
        assert_eq!(parse_etime("1:2:3:4"), None);
    }

    fn verdict(state: AgentState, reason: StateReason, detail: &str) -> Verdict {
        Verdict {
            state,
            reason,
            detail: detail.into(),
            confidence: claude_replay_core::state::Confidence::Inferred,
        }
    }

    fn seen(state: AgentState, reason: StateReason, detail: &str) -> Verdict {
        Verdict {
            confidence: claude_replay_core::state::Confidence::Observed,
            ..verdict(state, reason, detail)
        }
    }

    /// #s238: herdr's status sets the state over the derivation's inferences; the derivation keeps
    /// only the reason and detail that agree with it, an observed wait stands, and whatever herdr
    /// decided is observed.
    #[test]
    fn herdr_decides_the_state_and_keeps_the_reasons_that_agree() {
        use AgentState::{Busy, Idle, Wait};
        use StateReason::*;
        let cases = [
            // working
            (seen(Busy, Tool, "Bash"), "working", (Busy, Tool, "Bash")),
            (
                seen(Busy, QueuedPrompt, ""),
                "working",
                (Busy, QueuedPrompt, ""),
            ),
            (
                verdict(Wait, Permission, "Bash"),
                "working",
                (Busy, Tool, "Bash"),
            ),
            (
                verdict(Wait, Permission, ""),
                "working",
                (Busy, Thinking, ""),
            ),
            (
                seen(Idle, Done, "All set."),
                "working",
                (Busy, Thinking, ""),
            ),
            (seen(Idle, Stalled, ""), "working", (Busy, Thinking, "")),
            // blocked
            (
                verdict(Wait, Permission, "Bash"),
                "blocked",
                (Wait, Permission, "Bash"),
            ),
            (
                seen(Busy, Tool, "Bash"),
                "blocked",
                (Wait, Permission, "Bash"),
            ),
            (seen(Busy, Thinking, ""), "blocked", (Wait, Permission, "")),
            (
                seen(Idle, Done, "All set."),
                "blocked",
                (Wait, Permission, ""),
            ),
            // idle and done
            (
                seen(Idle, Done, "All set."),
                "idle",
                (Idle, Done, "All set."),
            ),
            (
                seen(Idle, EndedQuestion, "Apply it?"),
                "done",
                (Idle, EndedQuestion, "Apply it?"),
            ),
            (
                seen(Idle, Failed, "API error"),
                "idle",
                (Idle, Failed, "API error"),
            ),
            (seen(Idle, Stalled, ""), "idle", (Idle, Done, "")),
            (
                seen(Busy, Fleet, "2 running"),
                "idle",
                (Busy, Fleet, "2 running"),
            ),
            (seen(Busy, Tool, "Bash"), "done", (Idle, Done, "")),
            (seen(Busy, Starting, ""), "idle", (Idle, Done, "")),
            (verdict(Wait, Permission, "Bash"), "idle", (Idle, Done, "")),
        ];
        for (derived, status, (state, reason, detail)) in cases {
            let v = herdr_decides(&derived, status).expect(status);
            assert_eq!(
                (v.state, v.reason, v.detail.as_str()),
                (state, reason, detail),
                "{derived:?} + herdr {status}"
            );
            assert_eq!(
                v.confidence,
                claude_replay_core::state::Confidence::Observed
            );
        }
        // A wait the derivation observed (the client's own, a pending question or plan) stands:
        // herdr decides it only by agreeing.
        for derived in [
            seen(Wait, Question, "Which?"),
            seen(Wait, PlanApproval, "plan ready for approval"),
            seen(Wait, Permission, ""),
        ] {
            assert_eq!(herdr_decides(&derived, "blocked").as_ref(), Some(&derived));
            for status in ["working", "idle", "done"] {
                assert_eq!(
                    herdr_decides(&derived, status),
                    None,
                    "{derived:?} + herdr {status}"
                );
            }
        }
        for status in ["unknown", "", "sleeping"] {
            assert_eq!(
                herdr_decides(&seen(Busy, Tool, "Bash"), status),
                None,
                "{status}"
            );
            assert_eq!(
                herdr_decides(&seen(Wait, Question, "?"), status),
                None,
                "{status}"
            );
        }
    }

    fn pane(pane: &str, session: &str, status: &str) -> crate::control::HerdrPane {
        crate::control::HerdrPane {
            pane: pane.into(),
            session: Some(session.into()),
            status: status.into(),
        }
    }

    fn ms(n: u64) -> std::time::Duration {
        std::time::Duration::from_millis(n)
    }

    /// Wait until the reader's answer for `socket` passes `ok` (it asks in the background).
    fn heard(
        reader: &HerdrReader,
        socket: &str,
        ok: impl Fn(&HerdrAnswer) -> bool,
    ) -> Option<HerdrAnswer> {
        let until = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(a) = reader.answer(socket).filter(|a| ok(a)) {
                return Some(a);
            }
            if Instant::now() > until {
                return None;
            }
            std::thread::sleep(ms(10));
        }
    }

    /// #s238: herdr is asked on a clock of its own, one ask at a time per server, so a look
    /// never waits and an answer is never a page poll old; a failed ask keeps the answer before
    /// it; the asker stops once no tick wants the server; and a server forgotten and met again
    /// still has one asker.
    #[test]
    fn the_herdr_reader_asks_on_its_own_clock_one_ask_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let (asks, inside, most) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let (a, i, m) = (asks.clone(), inside.clone(), most.clone());
        let reader = HerdrReader::timed(
            Arc::new(move |_: &str| {
                let now = i.fetch_add(1, SeqCst) + 1;
                m.fetch_max(now, SeqCst);
                let n = a.fetch_add(1, SeqCst);
                if n == 0 {
                    // The first ask holds until the test lets it go.
                    let _ = gate.lock().unwrap().recv();
                }
                // Each ask takes longer than the clock's step.
                std::thread::sleep(ms(80));
                i.fetch_sub(1, SeqCst);
                match n {
                    0 => Ok(vec![pane("w1:p1", "s1", "working")]),
                    _ => anyhow::bail!("herdr did not answer"),
                }
            }),
            ms(50),
            std::time::Duration::from_secs(5),
            ms(1000),
        );
        assert!(reader.answer("/s/herdr.sock").is_none(), "no answer yet");
        // The look returned while the ask it started is still out: it never waits on herdr.
        let until = Instant::now() + std::time::Duration::from_secs(5);
        while inside.load(SeqCst) == 0 && Instant::now() < until {
            std::thread::sleep(ms(5));
        }
        assert_eq!(inside.load(SeqCst), 1, "the ask is out");
        assert!(reader.answer("/s/herdr.sock").is_none(), "still no answer");
        go.send(()).unwrap();
        let first = heard(&reader, "/s/herdr.sock", |_| true).expect("an answer");
        assert_eq!(first.panes, vec![pane("w1:p1", "s1", "working")]);
        // Asked again with no look in between, and every failure keeps the first answer.
        std::thread::sleep(ms(400));
        assert!(
            asks.load(SeqCst) >= 2,
            "asked on its own clock: {}",
            asks.load(SeqCst)
        );
        assert_eq!(
            reader.answer("/s/herdr.sock").map(|a| a.panes),
            Some(first.panes.clone()),
            "a failed ask keeps the answer before it"
        );
        assert_eq!(most.load(SeqCst), 1, "never two asks at once");
        // Unwanted for longer than `wanted_for`, the asker forgets the server and stops.
        std::thread::sleep(ms(1600));
        assert!(
            reader.servers.lock().unwrap().is_empty(),
            "forgotten by its asker"
        );
        let stopped = asks.load(SeqCst);
        std::thread::sleep(ms(300));
        assert_eq!(asks.load(SeqCst), stopped, "no tick wants it: no more asks");
        assert_eq!(most.load(SeqCst), 1);
    }

    /// #s238: an asker whose read panics forgets its server, so the next look starts another
    /// rather than leaving herdr silently off for that server.
    #[test]
    fn a_herdr_asker_that_panics_leaves_its_server_to_a_new_one() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let asks = Arc::new(AtomicUsize::new(0));
        let a = asks.clone();
        let reader = HerdrReader::timed(
            Arc::new(move |_: &str| {
                if a.fetch_add(1, SeqCst) == 0 {
                    panic!("a read that panics");
                }
                Ok(vec![pane("w1:p1", "s1", "idle")])
            }),
            ms(50),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5),
        );
        assert!(reader.answer("/s/herdr.sock").is_none());
        let until = Instant::now() + std::time::Duration::from_secs(5);
        while !reader.servers.lock().unwrap().is_empty() && Instant::now() < until {
            std::thread::sleep(ms(10));
        }
        assert!(
            reader.servers.lock().unwrap().is_empty(),
            "the dead asker's server is forgotten"
        );
        assert_eq!(
            heard(&reader, "/s/herdr.sock", |_| true).map(|a| a.panes),
            Some(vec![pane("w1:p1", "s1", "idle")]),
            "the next look starts a new asker"
        );
    }

    fn herdr_world(tag: &str) -> (PathBuf, PathBuf) {
        let root = scratch(tag);
        let t = root.join("s1.jsonl");
        std::fs::write(&t, concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-14T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{"type":"text","text":"All set."}]},"timestamp":"2026-08-14T10:00:05Z"}"#, "\n",
        )).unwrap();
        // The transcript's last write is well before herdr's first look.
        std::fs::File::options()
            .write(true)
            .open(&t)
            .unwrap()
            .set_modified(SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();
        (root, t)
    }

    fn in_pane(sid: &str, t: &Path, at: &str) -> RowFacts {
        RowFacts {
            pane: Some(crate::index::PaneAddr::Herdr {
                socket: Some("/s/herdr.sock".into()),
                pane: at.into(),
            }),
            ..facts(sid, t, Some(4242), 5)
        }
    }

    fn now_is(tr: &StateTracker, sid: &str) -> (AgentState, StateReason, bool) {
        let (v, _) = tr.current(sid).expect(sid);
        (v.state, v.reason, tr.by_herdr(sid))
    }

    /// #s238 end to end: a session herdr names in its pane takes herdr's status and the row says
    /// so; a pane whose session herdr names differently is left to the derivation; a wait herdr
    /// sees publishes on the tick that hears it.
    #[test]
    fn a_session_herdr_names_in_its_pane_takes_herdrs_status() {
        let (root, t) = herdr_world("herdr");
        let status = Arc::new(Mutex::new("working"));
        let said = status.clone();
        let mut tr = StateTracker {
            herdr: HerdrReader::timed(
                Arc::new(move |_: &str| {
                    let status = *said.lock().unwrap();
                    Ok(vec![
                        pane("w1:p1", "s1", status),
                        pane("w1:p2", "other", "blocked"),
                    ])
                }),
                ms(30),
                std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(10),
            ),
            ..StateTracker::default()
        };
        assert!(heard(&tr.herdr, "/s/herdr.sock", |_| true).is_some());

        // The transcript says the turn ended; herdr sees the agent working.
        let rows = [in_pane("s1", &t, "w1:p1"), in_pane("s2", &t, "w1:p2")];
        tr.tick(&root, &rows);
        tr.tick(&root, &rows);
        assert_eq!(
            now_is(&tr, "s1"),
            (AgentState::Busy, StateReason::Thinking, true)
        );
        assert_eq!(
            now_is(&tr, "s2"),
            (AgentState::Idle, StateReason::Done, false),
            "herdr names another session in that pane: the derivation stands"
        );

        // herdr sees a dialog: the wait publishes on the first tick that hears it.
        *status.lock().unwrap() = "blocked";
        assert!(heard(&tr.herdr, "/s/herdr.sock", |a| a.panes[0].status
            == "blocked")
        .is_some());
        tr.tick(&root, &rows);
        assert_eq!(
            now_is(&tr, "s1"),
            (AgentState::Wait, StateReason::Permission, true)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #s238: herdr's answer is older than the transcript's last write (a question answered, a
    /// prompt submitted after herdr looked), so the newer evidence, the derivation, decides.
    #[test]
    fn a_transcript_newer_than_herdrs_answer_is_left_to_the_derivation() {
        let (root, t) = herdr_world("herdr-stale");
        let mut tr = StateTracker {
            // Asked once: the next ask is a minute away.
            herdr: HerdrReader::timed(
                Arc::new(|_: &str| Ok(vec![pane("w1:p1", "s1", "blocked")])),
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(60),
                std::time::Duration::from_secs(60),
            ),
            ..StateTracker::default()
        };
        assert!(heard(&tr.herdr, "/s/herdr.sock", |_| true).is_some());
        let rows = [in_pane("s1", &t, "w1:p1")];
        tr.tick(&root, &rows);
        assert_eq!(
            now_is(&tr, "s1"),
            (AgentState::Wait, StateReason::Permission, true)
        );
        // The transcript moves after herdr looked.
        std::fs::File::options()
            .write(true)
            .open(&t)
            .unwrap()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let rows = [in_pane("s1", &t, "w1:p1")];
        tr.tick(&root, &rows);
        tr.tick(&root, &rows);
        assert_eq!(
            now_is(&tr, "s1"),
            (AgentState::Idle, StateReason::Done, false)
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
