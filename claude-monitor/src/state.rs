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

use claude_replay_core::adapter;
use claude_replay_core::liveness::inflight_tools_in_tail;
use claude_replay_core::state::{
    derive_state, tail_pulse, AgentState, PendingTool, StateEvent, StateReason, StateSignals,
    Verdict,
};
use claude_replay_core::Agent;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    /// The tmux control address `(socket basename, pane)` when the link is in tmux (#133).
    pub tmux: Option<(Option<String>, String)>,
    /// Whether the process↔session link is PROVEN (#133 §3.1): sid-in-argv, fd held, or
    /// growth-proved — NEVER a cwd heuristic. Only a proven link may be injected into.
    pub confirmed: bool,
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
    /// Content-derived facts, keyed by the tree mtime that produced them — a quiet
    /// session's tick costs zero reads.
    content: HashMap<String, ContentFacts>,
    /// Hysteresis staging + the last PUBLISHED verdict per session.
    staged: HashMap<String, Staged>,
}

struct ContentFacts {
    at: Option<SystemTime>,
    pending: Vec<PendingTool>,
    last: claude_replay_core::state::TailLast,
    final_text: Option<String>,
    queued_prompt: bool,
    last_tool_error: bool,
    ends_with_question: bool,
}

struct Staged {
    /// The last verdict written to the stream (with its publish timestamp).
    published: Option<(Verdict, String)>,
    /// The previous tick's derivation — the one-tick stability gate.
    candidate: Option<(AgentState, StateReason)>,
}

impl StateTracker {
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

        for f in facts {
            let interesting = f.pid.is_some() || f.quiet_secs < INTEREST_QUIET_SECS;
            let verdict = if !interesting {
                // Rule 1 without any reads: long-quiet and processless is exited.
                Verdict {
                    state: AgentState::Idle,
                    reason: StateReason::Exited,
                    detail: String::new(),
                    confidence: claude_replay_core::state::Confidence::Observed,
                }
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
                };
                derive_state(&signals)
            };

            let staged = self.staged.entry(f.sid.clone()).or_insert(Staged {
                published: None,
                candidate: None,
            });
            let key = (verdict.state, verdict.reason);
            let differs = staged
                .published
                .as_ref()
                .is_none_or(|(p, _)| (p.state, p.reason) != key);
            // Wait and attention-idle transitions publish immediately — they are what a
            // consumer is waiting to hear. Everything else needs one stable tick, the
            // anti-flap the growth hold gives the rail.
            let immediate = matches!(
                verdict.reason,
                StateReason::Question
                    | StateReason::PlanApproval
                    | StateReason::Exited
                    | StateReason::ExitedMidWork
            );
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

    /// The content-derived facts, recomputed only when the tree mtime moved.
    fn content_facts(&mut self, f: &RowFacts) -> &ContentFacts {
        let stale = self
            .content
            .get(&f.sid)
            .is_none_or(|c| c.at != f.tree_mtime);
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
                    pending,
                    last: pulse.last,
                    final_text: pulse.final_text,
                    queued_prompt: pulse.queued_prompt,
                    last_tool_error: pulse.last_tool_error,
                    ends_with_question,
                },
            );
        }
        self.content.get(&f.sid).expect("just inserted")
    }
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
        map.entry(ppid).or_default().push((pid, parse_etime(etime)));
    }
    map
}

/// `ps` etime (`[[dd-]hh:]mm:ss`) → seconds. Unparseable reads as 0 (recent) — the
/// direction that keeps a session busy rather than inventing a permission wait.
fn parse_etime(s: &str) -> u64 {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    let hms = match parts.as_slice() {
        [h, m, sec] => h * 3600 + m * 60 + sec,
        [m, sec] => m * 60 + sec,
        [sec] => *sec,
        _ => 0,
    };
    days * 86400 + hms
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
            tmux: None,
            confirmed: false,
            tree_mtime: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
        }
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
        assert_eq!(parse_etime("05"), 5);
        assert_eq!(parse_etime("01:05"), 65);
        assert_eq!(parse_etime("02:01:05"), 7265);
        assert_eq!(parse_etime("3-02:01:05"), 3 * 86400 + 7265);
    }
}
