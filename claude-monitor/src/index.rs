//! The monitor's index: scan → diff → card → state → one JSON snapshot for the rail.
//!
//! Everything here respects the two prohibitions the design is built on (#98): **no BLOCK
//! fold on the index path** (R7 — rows are born from bounded reads; counters come from
//! visited sessions' meta streams, read lock-free) and **no background sweep** (§3 — the
//! durable entry for a session is written by SERVING it, never by the monitor itself).
//! COST is the one deliberate carve-out (§14): it comes from the engine's cursor-resumable
//! metrics fold via [`crate::cost::CostLedger`] — bounded, budgeted, and never producing a
//! durable entry — because cost gated on visits under-reported a project 20× (measured:
//! $121 shown of $2,421 real).

use anyhow::Result;
use claude_replay_core::engine::meta_stream::{MaterializedMeta, FOLD_VERSION};
use claude_replay_core::liveness::{inflight_tool_in_tail, latest_tree_activity};
use claude_replay_core::{adapters, discover, metrics, Agent};
use claude_replay_present::cache::{admit, MetaReader, Presentation};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long after the last observed growth a row keeps reading **growing**. An agent's
/// writes land in BURSTS — a long generation appends nothing and has no tool in flight, so
/// gaps of 30–120 s between writes are the working norm — and a linger shorter than that
/// gap makes an actively-working session flap growing→idle→growing, hopping around a
/// sort that puts growing first. A minute absorbs the cadence; a session that truly
/// stopped reads idle at most a minute late.
const GROW_LINGER: Duration = Duration::from_secs(60);

/// Only transcripts touched this recently get the in-flight tail read (256 KiB): a session
/// idle for an hour is not mid-tool, and reading every historical transcript's tail each
/// cycle is exactly the class of cost the scan must not have.
const INFLIGHT_WINDOW: Duration = Duration::from_secs(30 * 60);

/// How often the process table is refreshed. Liveness is the SECONDARY signal (§5.1) —
/// it only splits idle-alive from finished — so it does not need the scan's cadence.
const PROC_REFRESH: Duration = Duration::from_secs(10);

/// Scan floor (§8): N open tabs cost one scan.
const SCAN_FLOOR: Duration = Duration::from_secs(2);

/// The ordering's ONE moving part (owner rule, 2026-08-08), applied at BOTH levels —
/// groups, and the sessions inside each group: anything active within this window sits in
/// a top bucket sorted BY NAME (active items are all "tied", so write jitter cannot
/// reorder them), and everything else sorts by recency, which is stable by construction
/// because stale mtimes are frozen. An item moves only by crossing this line.
const ACTIVE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A resolved, allowed send-prompt target (#133 idle slice): everything the spawn needs.
pub struct SendTarget {
    pub sid: String,
    pub agent: Agent,
    pub cwd: Option<String>,
}

/// A resolved, allowed TMUX send target (#133 tmux slice): the pane to inject into, plus
/// the pid the consent is keyed by (a pane outlives its process, so consent must not carry
/// over to whatever occupies it next).
pub struct TmuxTarget {
    pub sid: String,
    pub pid: u32,
    pub sock: Option<String>,
    pub pane: String,
}

/// Why a send-prompt was refused — each a distinct, reportable fact (mirrors the §3.1/§4
/// refusal reasons). Never an injection: the route turns these into a 4xx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendRefusal {
    /// No session with that id in any store.
    NoSuchSession,
    /// The session has a LIVE attributed process — the tmux path, not a resume-spawn.
    SessionIsLive,
    /// Another session in the same project (cwd) is active — resuming would fork off
    /// history that live work is still writing (the owner's constraint).
    ProjectHasActiveSession,
    /// The agent's headless resume shape is not verified here (only claude, codex).
    UnsupportedAgent,
    /// (tmux path) the session has no live attributed process to inject into.
    SessionNotLive,
    /// (tmux path) the link is a cwd HEURISTIC, not proven (§3.1) — injecting could hit a
    /// different agent's pane.
    UnprovenLink,
    /// (tmux path) the live agent is not in a tmux pane (screen/plain tty — no injection).
    NotInTmux,
    /// (tmux path) no consent has been granted for this pane/pid, or it expired (§3.4).
    NoConsent,
}

/// The tmux-injection decision (#133 tmux slice), PURE over one scan's link data — split out
/// so the §3.1 refusal ladder is unit-tested without a live process or a tmux server. The
/// order is the security order: unsupported agent, then not-live, then UNPROVEN (a cwd guess
/// must never be injected), then not-in-tmux. Returns the `(pid, sock, pane)` to inject into.
fn tmux_target_from(
    agent: Agent,
    link: &SendLink,
) -> Result<(u32, Option<String>, String), SendRefusal> {
    if agent != Agent::CLAUDE && agent != Agent::CODEX {
        return Err(SendRefusal::UnsupportedAgent);
    }
    let pid = link.pid.ok_or(SendRefusal::SessionNotLive)?;
    if !link.confirmed {
        return Err(SendRefusal::UnprovenLink);
    }
    let (sock, pane) = link.tmux.clone().ok_or(SendRefusal::NotInTmux)?;
    Ok((pid, sock, pane))
}

/// Does ANOTHER session in the same project (cwd) have a live process? PURE over one scan's
/// data — the owner's constraint 2: with a live sibling we cannot tell which session drives
/// the project, so neither a resume (would fork the live work) nor an injection (could hit the
/// wrong pane) is safe. Both send paths gate on this; `cwd_of` maps a sid to its cwd.
///
/// A headless worker the target STARTED is part of it, not another session (#s53, the owner):
/// a coordinator whose `claude -p` workers run in its repo keeps its compose box. `started_by`
/// maps a sid to the session that started it.
fn project_has_other_live(
    sid: &str,
    cwd: Option<&str>,
    links: &HashMap<String, SendLink>,
    cwd_of: impl Fn(&str) -> Option<String>,
    started_by: impl Fn(&str) -> Option<String>,
) -> bool {
    let Some(cwd) = cwd else { return false };
    links.iter().any(|(other, l)| {
        other != sid
            && l.pid.is_some()
            && cwd_of(other).as_deref() == Some(cwd)
            && started_by(other).as_deref() != Some(sid)
    })
}

/// The liveness half of an IDLE send decision (#133 resume path), pure over one scan's data:
/// the target must have NO live pid, and no OTHER session sharing its cwd may have one. Split
/// out so the owner's suppress-when-active rule is unit-tested without a live process.
fn send_liveness_ok(
    sid: &str,
    cwd: Option<&str>,
    links: &HashMap<String, SendLink>,
    cwd_of: impl Fn(&str) -> Option<String>,
    started_by: impl Fn(&str) -> Option<String>,
) -> Result<(), SendRefusal> {
    if links.get(sid).and_then(|l| l.pid).is_some() {
        return Err(SendRefusal::SessionIsLive);
    }
    if project_has_other_live(sid, cwd, links, cwd_of, started_by) {
        return Err(SendRefusal::ProjectHasActiveSession);
    }
    Ok(())
}

impl SendRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoSuchSession => "no such session",
            Self::SessionIsLive => "the session is live — attach to its terminal instead",
            Self::ProjectHasActiveSession => {
                "another session in this project is active — resume would fork live work"
            }
            Self::UnsupportedAgent => "sending is supported only for claude and codex sessions",
            Self::SessionNotLive => "the session is not live — use send, which resumes it",
            Self::UnprovenLink => "which pane this session drives is not proven — cannot inject",
            Self::NotInTmux => "the live agent is not in tmux — cannot inject",
            Self::NoConsent => "grant consent to send into this session's terminal first",
        }
    }
}

pub struct Index {
    /// The monitor's OWN durable root (R5) — counters are read from `html/<sid>/meta.jsonl`
    /// under it; an entry existing at all is what "visited" means.
    cache_root: PathBuf,
    /// Which agents to show (R1); empty = all.
    only: Vec<Agent>,
    /// Where this index's STATE lives — captured at construction, never re-read from the
    /// environment (#153). `state_dir()` resolves process-global env at call time, so an
    /// `Index` that re-derived it on every save could write to a directory its constructor
    /// never named: the unit tests built one over a scratch cache root and their hide/unhide
    /// pair persisted `[]` over the developer's real `~/.local/state/…/ignored.json`. Holding
    /// the path makes that structurally impossible — an instance writes where it was told to.
    state_dir: PathBuf,
    state: std::sync::Mutex<State>,
}

#[derive(Default)]
struct State {
    rows: HashMap<String, Row>,
    scanned_at: Option<Instant>,
    snapshot: String,
    procs: Vec<Proc>,
    procs_at: Option<Instant>,
    /// The user's hide list (#113): keys are `s:<sid>` (one session), `p:<cwd>` (a whole
    /// project group), or `a:<label>` (a whole desktop-agent group) — the SAME strings the
    /// group map is keyed by, so a group's key IS its hide key. Loaded once from the monitor's
    /// STATE dir (`<state_dir>/ignored.json`, #197 — user intent that cannot be recomputed
    /// belongs in XDG state, not the deletable cache) and rewritten on every toggle. It never
    /// touches an agent's data or a terminal, so it stays inside the read-only contract (R8).
    ignored: BTreeSet<String>,
    /// The cost ledger (§14) — every session's equivalent-API cost, folded incrementally
    /// through `MetricsCursor`s persisted at the monitor's own root. Lazily built on the
    /// first scan because it needs `cache_root`.
    ledger: Option<crate::cost::CostLedger>,
    /// Each sub-agent transcript's last authoritative result, with the root row it currently
    /// resolves to. A cycle can exhaust its byte budget before every child advances, so this cache
    /// preserves `Keep` entries individually; rebuilding only a root subtotal would make deferred
    /// children disappear for one poll and then jump back.
    sub_cost_entries: HashMap<PathBuf, (String, crate::cost::CostSummary)>,
    /// Sub-agent spend banked onto each ROOT row's sid (§14): derived from `sub_cost_entries` after
    /// every scan. A sub-agent rollout is not a row (it is excluded from `store_transcripts`), but
    /// its cost is real — measured 95% of one project's total. The summary retains partial state so
    /// an unpriced child cannot make the parent's subtotal look exact.
    sub_costs: HashMap<String, crate::cost::CostSummary>,
    /// The agent-state pass (#194): hysteresis staging + the events/current dump.
    state_tracker: crate::state::StateTracker,
    /// Per-session attributed link from the last scan (#133): the live pid (if any), the
    /// tmux control address, and whether the link is proven. The send-prompt route reads
    /// this for both transports — idle-resume (no live pid) and tmux (a proven live link).
    send_links: HashMap<String, SendLink>,
    /// Which session started each headless worker (#s53), worker sid → starter sid: banked from a
    /// running worker's environment (`bank_started_by`) and kept in `<cache_root>/started-by.json`,
    /// because the edge is visible only while the worker runs and its transcript never records it.
    started_by_edges: BTreeMap<String, String>,
}

impl State {
    /// The session that started `sid` as a headless worker (#s53), when this index knows it: the
    /// edge banked from the worker's environment, or — needing no sighting — a one-shot session's
    /// #373 scratch owner (knack's workers run in worktrees under their coordinator's scratchpad).
    /// `None` for a starter that is not a row here, so such a worker stays a row of its own.
    fn started_by(&self, sid: &str) -> Option<&str> {
        let row = self.rows.get(sid)?;
        let parent = self
            .started_by_edges
            .get(sid)
            .map(String::as_str)
            .or_else(|| row.one_shot.then_some(row.spawned_by.as_deref()).flatten())?;
        (parent != sid && self.rows.contains_key(parent)).then_some(parent)
    }
}

/// One session's attributed link, banked from the scan for the send routes (#133).
#[derive(Clone, Default)]
struct SendLink {
    /// The live agent pid, or `None` when the session is finished.
    pid: Option<u32>,
    /// `(socket basename, pane)` when the live agent is in tmux.
    tmux: Option<(Option<String>, String)>,
    /// Whether the process↔session link is proven (§3.1) — required to inject.
    confirmed: bool,
}

/// Per-session scan state, persistent across cycles — the "previous scan" half of §5's diff.
struct Row {
    path: PathBuf,
    agent: Agent,
    /// The session's EXACT working directory (`discover::project_path`) — subdir-precise,
    /// because every process-matching path keys off it: `newest_by_cwd`/`siblings` (the #145/#146
    /// heuristic doubt count), `live_sids_by_cwd` (injection gating), and `proved_pid` all compare
    /// it against a live process's `lsof` cwd, which is the real cwd, never the repo root.
    cwd: Option<String>,
    /// The git repo this session belongs to (`discover::repo_root`) — `cwd` normalized up to its
    /// `.git`. GROUPING keys off this so subdir sessions (whid's `.whid/run-…`) collapse into one
    /// project row; `cwd` stays the real dir for the process matching above.
    repo: Option<String>,
    title: String,
    /// Tree mtime at the last scan — the cheap CHANGE TRIGGER, and deliberately nothing
    /// more: an attached-but-idle agent client re-touches its transcript without appending
    /// (measured: mtime today, last content three weeks old), so mtime is when the FILE
    /// moved, not when the SESSION did.
    tree_mtime: Option<SystemTime>,
    /// The last CONTENT timestamp in the transcript's tail — what "activity" actually
    /// means. Re-derived only when the mtime trigger fires; drives the display, the
    /// active-bucket ordering, and the growth diff.
    last_event: Option<u64>,
    /// The FIRST content timestamp — the session's start, for the rail's span display
    /// (#129), and `start_probed` so it is derived exactly ONCE: an append-only log's head
    /// never changes, and a miss costs the wide window (a re-probe per mtime tick would
    /// re-read a megabyte forever for the one session that has no head timestamp).
    first_event: Option<u64>,
    start_probed: bool,
    /// The transcript's head says a one-shot run wrote it (`"entrypoint":"sdk-cli"`, what
    /// `claude -p` records) — read once, with the start. Only such a session can pair with a
    /// one-shot process (#s50).
    one_shot: bool,
    /// A one-shot session's BRIEF (#s53): the first line of the prompt it was started with, read
    /// once with the start. A worker's own title is its project's — on aries-black every knack
    /// worker read "knack" — so the Agents pane names a worker by what it was asked to do.
    brief: Option<String>,
    /// The session this one was forked from (#142), and whether we have looked. Read once:
    /// a fork's origin is fixed when it is created and no later write changes it.
    fork_from: Option<String>,
    fork_probed: bool,
    /// The session whose scratch this one's cwd lies in (#373) — a knack worktree under its
    /// scratchpad, a job's tmp — and whether we have looked. Read once: a cwd never moves. Groups
    /// the session under that session's project (`assemble`), never as a project of its own.
    spawned_by: Option<String>,
    spawn_probed: bool,
    /// The agent process this session was matched to by GROWTH (#146), and that process's
    /// cwd at the time. Growth is the strongest signal available for a no-id launch — a
    /// transcript only advances because its own agent wrote to it — so once a session is the
    /// only grower in its directory the pairing is banked and outlives the growth that
    /// proved it. Dropped as soon as the pid is gone or has moved.
    proved_pid: Option<(u32, String)>,
    /// When growth was last OBSERVED (scan clock) — drives the linger.
    grew_at: Option<Instant>,
    /// Counter fold of the visited entry's meta stream, keyed by the stream's mtime so a
    /// quiet session costs a `stat`, not a re-read.
    counters: Option<(SystemTime, Counters)>,
    /// Title re-derives when the transcript mtime moves past this (§4.1 under lazy: the
    /// mtime IS the refresh trigger).
    title_mtime: Option<SystemTime>,
    /// The ledger's answer for this session's OWN transcript (§14). Kept on the row so a cycle
    /// whose budget defers the fold still shows the last known subtotal or explicit unpriced state.
    cost: Option<crate::cost::CostSummary>,
}

fn merge_cost(
    left: Option<crate::cost::CostSummary>,
    right: Option<crate::cost::CostSummary>,
) -> Option<crate::cost::CostSummary> {
    match (left, right) {
        (Some(mut left), Some(right)) => {
            left.merge(right);
            Some(left)
        }
        (Some(summary), None) | (None, Some(summary)) => Some(summary),
        (None, None) => None,
    }
}

fn apply_sub_cost_update(
    entries: &mut HashMap<PathBuf, (String, crate::cost::CostSummary)>,
    path: &Path,
    root: String,
    update: crate::cost::CostUpdate,
) {
    match update {
        crate::cost::CostUpdate::Keep => {
            if let Some((old_root, _)) = entries.get_mut(path) {
                *old_root = root;
            }
        }
        crate::cost::CostUpdate::Replace(Some(summary)) => {
            entries.insert(path.to_path_buf(), (root, summary));
        }
        crate::cost::CostUpdate::Invalidate | crate::cost::CostUpdate::Replace(None) => {
            entries.remove(path);
        }
    }
}

fn aggregate_sub_costs(
    entries: &HashMap<PathBuf, (String, crate::cost::CostSummary)>,
) -> HashMap<String, crate::cost::CostSummary> {
    let mut totals = HashMap::new();
    for (root, summary) in entries.values() {
        totals
            .entry(root.clone())
            .and_modify(|total: &mut crate::cost::CostSummary| total.merge(*summary))
            .or_insert(*summary);
    }
    totals
}

#[derive(Clone)]
struct Counters {
    turns: usize,
    tools: usize,
    subs: usize,
    child_running: bool,
}

/// One process, with the session-mapping and terminal facts #112's link resolution
/// consumes — all gathered per the VERIFIED mechanisms of design/session-liveness-probe.md.
/// The expensive facts (env, fds, tty) are filled for AGENT processes only.
struct Proc {
    pid: u32,
    argv: String,
    exe_base: String,
    /// Working directory (`lsof -Fpfn`, the `cwd` fd) — the Claude heuristic link.
    cwd: Option<String>,
    /// Open `.jsonl` paths — Codex holds its rollout open, the probe's exact fd link.
    open_jsonl: Vec<String>,
    /// Controlling tty (`ps -o tty=`); `None` when detached (`??`).
    tty: Option<String>,
    /// `TMUX_PANE=%N` from the process ENVIRONMENT (`ps eww`) — the probe's finding: the
    /// multiplexer is visible nowhere else, and `%N` is the injection target.
    pane: Option<String>,
    /// The socket path from `TMUX=/path,pid,idx` — pane ids are unique per SERVER, so two
    /// servers each have a `%0` and the socket is what disambiguates the target.
    tmux_sock: Option<String>,
    /// `STY=<name>` — GNU screen's equivalent.
    screen: Option<String>,
    /// When the process started, in epoch seconds (`ps -o etime=`, so to the second) — what pairs a
    /// one-shot `claude -p` with the session it created (#s50).
    started: Option<u64>,
    /// The session whose Bash started this process (#s53), from its inherited ENVIRONMENT: Claude
    /// Code exports `CLAUDE_CODE_SESSION_ID` (its own session) and `CLAUDE_CODE_CHILD_SESSION=1`
    /// into every Bash tool command, and a `claude -p` started there carries both — measured on a
    /// running worker whose parent pid was 1, so the process tree alone would not have said. Set
    /// only when both are present.
    parent_session: Option<String>,
}

/// How a live agent process maps to a session row, and what hosts it.
struct AgentLink {
    pid: u32,
    /// Exact link (sid in argv, or the transcript held open) vs the cwd+recency heuristic.
    confirmed: bool,
    terminal: Terminal,
}

/// What hosts the agent — and therefore whether it can be CONTROLLED (§3 of the probe:
/// injection is the multiplexer's property; a bare tty is not controllable, deliberately).
enum Terminal {
    Tmux {
        pane: String,
        /// Socket basename when it is not the default server — the disambiguator a
        /// `tmux -L <name>` target needs.
        sock: Option<String>,
    },
    Screen(String),
    Tty,
    Detached,
}

impl Terminal {
    fn of(p: &Proc) -> Terminal {
        match (&p.pane, &p.screen, &p.tty) {
            (Some(pane), _, _) => Terminal::Tmux {
                pane: pane.clone(),
                sock: p
                    .tmux_sock
                    .as_deref()
                    .and_then(|s| s.rsplit('/').next())
                    .filter(|b| *b != "default")
                    .map(str::to_string),
            },
            (None, Some(sty), _) => Terminal::Screen(sty.clone()),
            (None, None, Some(_)) => Terminal::Tty,
            (None, None, None) => Terminal::Detached,
        }
    }
    fn kind(&self) -> &'static str {
        match self {
            Terminal::Tmux { .. } => "tmux",
            Terminal::Screen(_) => "screen",
            Terminal::Tty => "tty",
            Terminal::Detached => "detached",
        }
    }
    /// The controllable target — a tmux pane or a screen session name; `None` when the
    /// host shape has no supported control channel.
    fn target(&self) -> Option<&str> {
        match self {
            Terminal::Tmux { pane, .. } => Some(pane),
            Terminal::Screen(name) => Some(name),
            _ => None,
        }
    }
    /// The tmux control address `(socket basename, pane)` — the only host the send-prompt
    /// tmux path (#133) drives (screen/tty are out of scope for injection).
    fn tmux_target(&self) -> Option<(Option<String>, String)> {
        match self {
            Terminal::Tmux { pane, sock } => Some((sock.clone(), pane.clone())),
            _ => None,
        }
    }
}

impl Index {
    /// Build the index over a cache root and a STATE directory. Both are passed in rather
    /// than resolved here (#153): a binary passes [`state_dir()`], a test passes its own
    /// scratch, and neither can reach the other's files by accident.
    pub fn new(cache_root: PathBuf, state_dir: PathBuf, only: Vec<Agent>) -> Self {
        // Reads the state path and NOTHING else (#154). Adopting a pre-#197 list is a one-time
        // migration, so it belongs at startup — see [`migrate_hide_list`] — not in a constructor
        // that runs on every launch and would have to reach outside the paths it was given.
        let ignored = read_ignored(&state_dir.join("ignored.json"))
            .unwrap_or_else(|e| {
                eprintln!(
                    "warning: the hide list at {} is unreadable ({e}) — NOT treating it as \
                     \"nothing hidden\"; the list is left alone until it can be read",
                    state_dir.join("ignored.json").display()
                );
                None
            })
            .unwrap_or_default();
        let started_by_edges = read_started_by(&cache_root.join(STARTED_BY_FILE));
        Self {
            cache_root,
            only,
            state_dir,
            state: std::sync::Mutex::new(State {
                ignored,
                started_by_edges,
                ..Default::default()
            }),
        }
    }

    /// This index's hide list — under the state dir it was CONSTRUCTED with (#153).
    fn hide_list(&self) -> PathBuf {
        self.state_dir.join("ignored.json")
    }

    /// Toggle a hide key (#113): `add` inserts, else removes. Persists the set to the
    /// monitor's own root and re-assembles the snapshot in place so the very next
    /// `/api/sessions` (the client re-polls right after) reflects the change. Returns a tiny
    /// JSON ack with the current hide count.
    pub fn set_ignore(&self, key: &str, add: bool) -> String {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let changed = if add {
            st.ignored.insert(key.to_string())
        } else {
            st.ignored.remove(key)
        };
        if changed {
            save_ignored(&self.hide_list(), &st.ignored);
            // Re-derive the snapshot from the unchanged rows under the new hide set (no
            // rescan needed — hiding is a view filter, not a discovery change). The
            // state pass does not re-run: hiding changes the view, not any state.
            if st.scanned_at.is_some() {
                // …finished the SAME way the scan finishes it. Re-deriving the raw snapshot here
                // and skipping the state annotation is how hiding one session silently stripped
                // `agentState*` from every row until the next scan.
                let mut snap = self.assemble(&st, &mut Vec::new());
                annotate_agent_state(&mut snap, &st.state_tracker);
                st.snapshot = snap.to_string();
            }
        }
        json!({ "ok": true, "ignored": st.ignored.len() }).to_string()
    }

    /// Resolve a send-prompt target (#133 idle slice): the session must EXIST, have NO live
    /// attributed process, and its PROJECT (cwd) must have no active session — so a resume
    /// never forks off shared history that a live session is still writing, and never
    /// collides with a live writer. A fresh scan is forced (never act on stale liveness).
    pub fn resolve_send(&self, sid: &str) -> Result<SendTarget, SendRefusal> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Liveness must be current — a session that went live one second ago must not be
        // resumed. Always rescan for a send (it is rare and consequential).
        self.scan(&mut st, &|_| {});
        st.scanned_at = Some(Instant::now());

        let row = st.rows.get(sid).ok_or(SendRefusal::NoSuchSession)?;
        let agent = row.agent;
        let cwd = row.cwd.clone();
        // Only claude and codex have a verified headless resume shape (agent-jdi); others
        // are refused rather than driven with an unverified CLI.
        if agent != Agent::CLAUDE && agent != Agent::CODEX {
            return Err(SendRefusal::UnsupportedAgent);
        }
        // The target must be finished and its project quiet (pure check, unit-tested).
        send_liveness_ok(
            sid,
            cwd.as_deref(),
            &st.send_links,
            // #s59: a sibling of another agent drives nothing of this session.
            |o| {
                st.rows
                    .get(o)
                    .filter(|r| r.agent == agent)
                    .and_then(|r| r.cwd.clone())
            },
            |o| st.started_by(o).map(str::to_string),
        )?;
        Ok(SendTarget {
            sid: sid.to_string(),
            agent,
            cwd,
        })
    }

    /// Resolve a TMUX send target (#133 tmux slice): the session must be LIVE with a PROVEN
    /// link (§3.1 — never a cwd guess; injecting into the wrong pane runs your text in a
    /// different agent) that is in tmux, AND it must be the ONLY live session in its project
    /// (constraint 2, mirrored from the resume path): with a live sibling we cannot tell which
    /// session drives the project, so we refuse rather than pick. A fresh scan is forced.
    pub fn resolve_tmux_send(&self, sid: &str) -> Result<TmuxTarget, SendRefusal> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.scan(&mut st, &|_| {});
        st.scanned_at = Some(Instant::now());

        let (agent, cwd) = {
            let row = st.rows.get(sid).ok_or(SendRefusal::NoSuchSession)?;
            (row.agent, row.cwd.clone())
        };
        let link = st.send_links.get(sid).cloned().unwrap_or_default();
        let (pid, sock, pane) = tmux_target_from(agent, &link)?;
        // The project must have no OTHER live session — else injecting here while another
        // session drives the same cwd forks divergent work (constraint 2).
        if project_has_other_live(
            sid,
            cwd.as_deref(),
            &st.send_links,
            // #s59: a sibling of another agent drives nothing of this session.
            |o| {
                st.rows
                    .get(o)
                    .filter(|r| r.agent == agent)
                    .and_then(|r| r.cwd.clone())
            },
            |o| st.started_by(o).map(str::to_string),
        ) {
            return Err(SendRefusal::ProjectHasActiveSession);
        }
        Ok(TmuxTarget {
            sid: sid.to_string(),
            pid,
            sock,
            pane,
        })
    }

    /// The `/api/sessions` body: a cached snapshot, re-scanned on a ~2 s floor (§8) so any
    /// number of open tabs cost one scan. `register` is called for every session the scan
    /// finds, so a click on any row can be served.
    pub fn sessions_json(&self, register: impl Fn(&Path)) -> String {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st
            .scanned_at
            .is_some_and(|t| t.elapsed() < SCAN_FLOOR && !st.snapshot.is_empty())
        {
            return st.snapshot.clone();
        }
        self.scan(&mut st, &register);
        st.scanned_at = Some(Instant::now());
        st.snapshot.clone()
    }

    /// One scan cycle: incremental by mtime (§8) — a session whose tree did not move costs
    /// two `stat`s and nothing else.
    fn scan(&self, st: &mut State, register: &dyn Fn(&Path)) {
        // Liveness refresh on its own slower clock (§5.1).
        if st.procs_at.is_none_or(|t| t.elapsed() > PROC_REFRESH) {
            st.procs = scan_procs();
            st.procs_at = Some(Instant::now());
        }

        // Discovery: every agent's machine-wide store (R1).
        let mut seen: Vec<String> = Vec::new();
        for a in adapters() {
            if !self.only.is_empty() && !self.only.contains(&a.agent()) {
                continue;
            }
            for path in a.store_transcripts() {
                let sid = stem_of(&path);
                seen.push(sid.clone());
                register(&path);
                let now_mtime = latest_tree_activity(&path);
                let row = st.rows.entry(sid).or_insert_with(|| Row {
                    path: path.clone(),
                    agent: a.agent(),
                    cwd: discover::project_path(&path).map(|p| p.display().to_string()),
                    repo: discover::repo_root(&path).map(|p| p.display().to_string()),
                    title: String::new(),
                    tree_mtime: None,
                    last_event: None,
                    first_event: None,
                    start_probed: false,
                    one_shot: false,
                    brief: None,
                    fork_from: None,
                    fork_probed: false,
                    spawned_by: None,
                    spawn_probed: false,
                    proved_pid: None,
                    grew_at: None,
                    counters: None,
                    title_mtime: None,
                    cost: None,
                });
                // The mtime is only the TRIGGER. Growth — and the activity clock — come
                // from the transcript's CONTENT: an attached idle client touches the file
                // without appending, and trusting mtime made three-week-old sessions read
                // "13m" and flip growing on housekeeping. The first sighting sets the
                // baseline without claiming growth — a monitor started over an idle
                // machine must not paint everything green.
                if row.tree_mtime != now_mtime {
                    let prev_event = row.last_event;
                    row.last_event = last_event_ts(&row.path).or(row.last_event);
                    if !row.start_probed {
                        row.first_event = first_event_ts(&row.path);
                        row.brief = head_one_shot(&row.path);
                        row.one_shot = row.brief.is_some();
                        row.start_probed = true;
                    }
                    match (prev_event, row.last_event) {
                        // The honest signal: the content clock advanced.
                        (Some(prev), Some(now_ev)) if now_ev > prev => {
                            row.grew_at = Some(Instant::now());
                        }
                        (Some(_), Some(_)) => {} // touched, nothing new said — NOT growth
                        // No content clock at all (a transcript format with no timestamps):
                        // fall back to the mtime diff rather than never showing growth.
                        (None, None) => {
                            if let (Some(prev), Some(now_m)) = (row.tree_mtime, now_mtime) {
                                if now_m > prev {
                                    row.grew_at = Some(Instant::now());
                                }
                            }
                        }
                        _ => {} // the clock just appeared — a baseline, not growth
                    }
                }
                row.tree_mtime = now_mtime;
                // #142: which session this one was forked from. Read ONCE — a fork's origin
                // is fixed when it is created, so no later write changes the answer.
                if !row.fork_probed {
                    row.fork_from = discover::fork_origin(row.agent, &path);
                    row.fork_probed = true;
                }
                // #373: started in another session's scratch? Asked once the cwd is known — the
                // block below may be what first learns it, so the probe also runs after it.
                if !row.spawn_probed {
                    if let Some(cwd) = row.cwd.as_deref() {
                        row.spawned_by = discover::scratch_owner(row.agent, Path::new(cwd));
                        row.spawn_probed = true;
                    }
                }
                // The card re-derives when the transcript moves (§4.1 under lazy) — a
                // bounded tail read, so mtime-triggered is affordable.
                let t_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if row.title.is_empty() || (t_mtime.is_some() && t_mtime != row.title_mtime) {
                    // The viewer's title carries an " · <agent>" suffix to keep browser
                    // tabs distinct; here the group and the agent chip already say it.
                    let full = claude_replay_html::display_title(row.agent, &path);
                    row.title = full
                        .strip_suffix(&format!(" · {}", row.agent.label()))
                        .unwrap_or(&full)
                        .to_string();
                    row.title_mtime = t_mtime;
                    if row.cwd.is_none() {
                        row.cwd = discover::project_path(&path).map(|p| p.display().to_string());
                    }
                    if row.repo.is_none() {
                        row.repo = discover::repo_root(&path).map(|p| p.display().to_string());
                    }
                    if !row.spawn_probed {
                        if let Some(cwd) = row.cwd.as_deref() {
                            row.spawned_by = discover::scratch_owner(row.agent, Path::new(cwd));
                            row.spawn_probed = true;
                        }
                    }
                }
                // Counters from the VISITED entry's meta stream (§2: fold-free read, keyed
                // by the stream's mtime).
                let meta = admit::entry_dir(&self.cache_root, Presentation::Html, &stem_of(&path))
                    .join("meta.jsonl");
                if let Ok(m) = std::fs::metadata(&meta).and_then(|m| m.modified()) {
                    if row.counters.as_ref().map(|(at, _)| *at) != Some(m) {
                        if let Some(c) = fold_counters(meta.parent().unwrap_or(Path::new(""))) {
                            row.counters = Some((m, c));
                        }
                    }
                }
            }
        }
        // Presence comes from the scan (§13): a deleted transcript's row vanishes.
        st.rows.retain(|sid, _| seen.contains(sid));

        // ── The cost pass (§14, cost.rs) ─────────────────────────────────────────────
        // Budgeted per CYCLE across all files: a cold start streams prices in over a few
        // polls instead of stalling the first paint; steady-state appends are a few KiB
        // and never feel the cap. A deferred fold keeps the row's previous price.
        let mut budget = crate::cost::COST_BUDGET_BYTES;
        let ledger = st
            .ledger
            .get_or_insert_with(|| crate::cost::CostLedger::new(&self.cache_root));
        for row in st.rows.values_mut() {
            match ledger.cost(row.agent, &row.path, &mut budget) {
                crate::cost::CostUpdate::Keep => {}
                crate::cost::CostUpdate::Invalidate => row.cost = None,
                crate::cost::CostUpdate::Replace(cost) => row.cost = cost,
            }
        }
        // Sub-agent roll-up (§14): price every sub-agent rollout and bank it on the MAIN
        // row that (transitively) spawned it. Rows are keyed by file STEM while a rollout
        // names its parent by bare uuid, so the uuid embedded in each stem is the bridge;
        // and a parent may itself be a sub-agent, so the chain is chased — with the same
        // 64-hop cap as `family_root` — until it lands on a row.
        let mut root_of: HashMap<String, String> = HashMap::new();
        for sid in st.rows.keys() {
            if let Some(u) = trailing_uuid(sid) {
                root_of.insert(u, sid.clone());
            }
        }
        let mut seen_sub_costs = HashSet::new();
        for a in adapters() {
            if !self.only.is_empty() && !self.only.contains(&a.agent()) {
                continue;
            }
            let subs = a.store_subagent_transcripts();
            let parent: HashMap<&str, &str> = subs
                .iter()
                .map(|(_, own, up)| (own.as_str(), up.as_str()))
                .collect();
            for (path, _own, first_up) in &subs {
                let mut cur = first_up.as_str();
                let mut root = None;
                for _ in 0..64 {
                    if let Some(stem) = root_of.get(cur) {
                        root = Some(stem.clone());
                        break;
                    }
                    match parent.get(cur) {
                        Some(&next) if next != cur => cur = next,
                        _ => break, // dangling lineage: its spend has no row to land on
                    }
                }
                let Some(root) = root else { continue };
                seen_sub_costs.insert(path.clone());
                let update = ledger.cost(a.agent(), path, &mut budget);
                apply_sub_cost_update(&mut st.sub_cost_entries, path, root, update);
            }
        }
        // Presence and lineage come from this scan. Deleted or now-dangling child transcripts must
        // disappear, while an extant child's `Keep` result retains its last authoritative summary.
        st.sub_cost_entries
            .retain(|path, _| seen_sub_costs.contains(path));
        st.sub_costs = aggregate_sub_costs(&st.sub_cost_entries);

        self.prove_by_growth(st);
        self.note_forks_from_argv(st);
        self.bank_started_by(st);

        let mut facts = Vec::new();
        let mut snapshot = self.assemble(st, &mut facts);
        // Bank the per-session link the assemble pass resolved (#133) for the send
        // routes: pid (finished when None), tmux address, and proven-ness.
        st.send_links = facts
            .iter()
            .map(|f| {
                (
                    f.sid.clone(),
                    SendLink {
                        pid: f.pid,
                        tmux: f.tmux.clone(),
                        confirmed: f.confirmed,
                    },
                )
            })
            .collect();
        // The agent-state pass (#194): derive busy/wait/idle from what this tick just
        // observed and dump transitions + the snapshot under `<cache_root>/state/`.
        st.state_tracker.tick(&self.cache_root, &facts);
        annotate_agent_state(&mut snapshot, &st.state_tracker);
        st.snapshot = snapshot.to_string();
    }

    /// Bank the pairing that GROWTH proves (#146).
    ///
    /// A no-id launch leaves nothing on disk naming its session — no fd, no argv id — so the
    /// directory match alone cannot say WHICH session an agent is driving (#145). Growth can:
    /// a transcript advances only because its own agent wrote to it. So when a directory has
    /// exactly ONE growing session and exactly ONE agent process, the pairing is forced, and
    /// it is remembered rather than recomputed — the evidence appears while the user is
    /// working and would otherwise evaporate the moment they stop typing, taking the row back
    /// to "which of these N is it?".
    ///
    /// Deliberately strict: more than one grower, or more than one candidate process, proves
    /// nothing and banks nothing. The record is dropped as soon as the pid is gone or its cwd
    /// has moved, so a reused pid cannot inherit another session's identity.
    /// #269: a live fork's ENGINE names its parent — `--session-id <child> --fork-session
    /// --resume <parent.jsonl>` — and nothing else does: the Claude adapter has no
    /// `fork_origin`, and the fork's transcript mentions its parent only inside file-history
    /// snapshots of the parent's scratchpad. So while the engine lives, the process table is
    /// how a Claude fork joins its #142 family, and the edge is kept once seen (a fork's origin
    /// is fixed at creation). Independent of `fork_probed`, which the transcript probe sets to
    /// `true` with `None` on the first scan.
    fn note_forks_from_argv(&self, st: &mut State) {
        let edges: Vec<(String, String)> = st
            .procs
            .iter()
            .filter_map(|p| fork_parent_from_argv(&p.argv))
            .collect();
        for (child, parent) in edges {
            if child == parent {
                continue;
            }
            if let Some(row) = st.rows.get_mut(&child) {
                if row.fork_from.is_none() {
                    row.fork_from = Some(parent);
                }
            }
        }
    }

    /// Bank which session started each running one-shot worker (#s53): the worker's process is
    /// found as the link finds it — paired by start time, or named by its `--resume` — and its
    /// inherited `CLAUDE_CODE_SESSION_ID` names the starter. Runs BEFORE `assemble`, so the first
    /// snapshot that shows a worker already shows it under its starter rather than moving it a tick
    /// later. Only a one-shot SESSION takes an edge: an interactive session someone started from an
    /// agent's Bash carries that agent's id too, and the owner's own sessions are never nested.
    /// Persisted only when an edge is new; an edge is fixed at creation, like a fork's origin.
    fn bank_started_by(&self, st: &mut State) {
        let paired = pair_one_shots(
            &st.procs,
            st.rows
                .iter()
                .filter(|(_, r)| r.one_shot)
                .filter_map(|(sid, r)| Some((sid.as_str(), r.cwd.as_deref()?, r.first_event?))),
        );
        let named = st
            .procs
            .iter()
            .filter(|p| is_one_shot(&p.argv))
            .filter_map(|p| match session_ref(&p.argv) {
                Some(SessionRef::Exact(sid)) => Some((sid, p.pid)),
                _ => None,
            });
        let found: Vec<(String, u32)> = paired
            .into_iter()
            .map(|(sid, (pid, _))| (sid, pid))
            .chain(named)
            .collect();
        let mut fresh = false;
        for (sid, pid) in found {
            let Some(parent) = st
                .procs
                .iter()
                .find(|p| p.pid == pid)
                .and_then(|p| p.parent_session.clone())
            else {
                continue;
            };
            let one_shot = st.rows.get(&sid).is_some_and(|r| r.one_shot);
            if parent != sid && one_shot && !st.started_by_edges.contains_key(&sid) {
                st.started_by_edges.insert(sid, parent);
                fresh = true;
            }
        }
        if fresh {
            // Bounded by the store: a worker whose transcript is gone takes its edge with it.
            let rows = &st.rows;
            st.started_by_edges.retain(|sid, _| rows.contains_key(sid));
            write_started_by(&self.cache_root.join(STARTED_BY_FILE), &st.started_by_edges);
        }
    }

    fn prove_by_growth(&self, st: &mut State) {
        let mut growers: HashMap<String, Vec<String>> = HashMap::new();
        for (sid, row) in &st.rows {
            if row.grew_at.is_some_and(|t| t.elapsed() < GROW_LINGER) {
                if let Some(cwd) = row.cwd.as_deref() {
                    growers
                        .entry(cwd.to_string())
                        .or_default()
                        .push(sid.clone());
                }
            }
        }
        for (cwd, sids) in growers {
            if sids.len() != 1 {
                continue; // two sessions writing in one directory prove nothing
            }
            let Some(agent) = st.rows.get(&sids[0]).map(|r| r.agent) else {
                continue;
            };
            let mut cands = st.procs.iter().filter(|p| {
                is_agent_exe(&p.exe_base, &p.argv)
                    && serves(p, agent)
                    && !is_helper(&p.argv)
                    && !is_one_shot(&p.argv)
                    && p.cwd.as_deref() == Some(cwd.as_str())
            });
            let (Some(p), None) = (cands.next(), cands.next()) else {
                continue; // zero or several candidates — no forced pairing
            };
            let pid = p.pid;
            if let Some(row) = st.rows.get_mut(&sids[0]) {
                row.proved_pid = Some((pid, cwd.clone()));
            }
        }
        // Forget a proof whose process is gone or has moved on.
        let alive: Vec<(u32, Option<String>)> =
            st.procs.iter().map(|p| (p.pid, p.cwd.clone())).collect();
        for row in st.rows.values_mut() {
            if let Some((pid, cwd)) = &row.proved_pid {
                let still = alive
                    .iter()
                    .any(|(q, c)| q == pid && c.as_deref() == Some(cwd.as_str()));
                if !still {
                    row.proved_pid = None;
                }
            }
        }
    }

    /// Rows → grouped JSON. Grouping is per agent KIND (§4.2): workspace-anchored agents by
    /// project, desktop agents under the agent itself.
    fn assemble(&self, st: &State, facts: &mut Vec<crate::state::RowFacts>) -> Value {
        #[derive(Default)]
        struct Group {
            kind: &'static str,
            /// The group map's key AND its hide key (#113): `p:<cwd>` / `a:<label>`.
            key: String,
            label: String,
            secondary: String,
            rows: Vec<Value>,
            cost: Option<crate::cost::CostSummary>,
            latest: u64,
            growing: usize,
            idle: usize,
            /// Any session in this group whose live agent is in a controllable terminal —
            /// the group-level badge (owner request).
            has_term: bool,
        }
        let now = SystemTime::now();
        // #133 tmux slice: the live grants, loaded ONCE for this snapshot. A row is marked
        // `consented` by matching its own `(sock, pane, sid, pid)` against these — the same
        // quadruple the send re-checks — so the badge never claims consent a send would then
        // refuse (e.g. after a restart mints a new pid).
        let grants = crate::consent::ConsentStore::open().active_grants();
        let consented = |sock: Option<&str>, pane: &str, sid: &str, pid: u32| -> bool {
            grants.iter().any(|g| {
                g.sock.as_deref() == sock && g.pane == pane && g.sid == sid && g.pid == pid
            })
        };
        // #133 constraint 2 (UI hint): a row whose project has ANOTHER live session offers no
        // send affordance — the resume path would refuse it (ProjectHasActiveSession) and the
        // inject path is now suppressed the same way. Precompute the live sids per cwd (from the
        // prior scan's links — a ~2 s-stale hint; the route re-checks liveness on a fresh scan).
        // #s59: siblings of the same AGENT only — a live Codex session cannot be writing a Claude
        // session's history nor driving its pane, so it withholds nothing from it.
        let mut live_sids_by_cwd: HashMap<(Agent, &str), Vec<&str>> = HashMap::new();
        for (osid, l) in &st.send_links {
            if l.pid.is_some() {
                if let Some(r) = st.rows.get(osid) {
                    if let Some(c) = r.cwd.as_deref() {
                        live_sids_by_cwd
                            .entry((r.agent, c))
                            .or_default()
                            .push(osid.as_str());
                    }
                }
            }
        }
        let proj_has_other_live = |sid: &str, agent: Agent, cwd: Option<&str>| -> bool {
            cwd.and_then(|c| live_sids_by_cwd.get(&(agent, c)))
                .is_some_and(|v| v.iter().any(|s| *s != sid && st.started_by(s) != Some(sid)))
        };
        // #142: every session's FAMILY root — follow `fork_from` until a session that is not
        // itself a fork. A fork's transcript is 82–99% a replay of its origin's, so the rail
        // shows one row per family rather than a dozen near-identical ones.
        //
        // Guarded against the two ways the chain can fail to reach a root: a dangling edge
        // (the origin is not in the store — pruned, or a different agent) and a cycle, which
        // the data should never contain but which would hang the scan. Either way the session
        // becomes its own root, which is exactly the "unknown provenance" answer.
        let family_root = |sid: &str| -> String {
            let mut cur = sid;
            for _ in 0..64 {
                match st.rows.get(cur).and_then(|r| r.fork_from.as_deref()) {
                    Some(next) if st.rows.contains_key(next) && next != cur => cur = next,
                    _ => break,
                }
            }
            cur.to_string()
        };
        // A cwd's NEWEST session is the one allowed to claim a process heuristically — and
        // `siblings` counts how many sessions that cwd holds, which is the size of the doubt
        // (#145). One session in the directory means the heuristic has nothing to get wrong;
        // several means the claim is a pick among them.
        //
        // #s50: a one-shot worker is paired with the session it created FIRST, and a paired
        // session is not the directory's to pick — so it is neither the newest nor a sibling, and
        // a coordinator keeps its own process however recently its workers wrote.
        let paired = pair_one_shots(
            &st.procs,
            st.rows
                .iter()
                .filter(|(_, r)| r.one_shot)
                .filter_map(|(sid, r)| Some((sid.as_str(), r.cwd.as_deref()?, r.first_event?))),
        );
        let mut newest_by_cwd: HashMap<&str, (&str, SystemTime)> = HashMap::new();
        let mut siblings: HashMap<&str, usize> = HashMap::new();
        for (sid, row) in &st.rows {
            if paired.contains_key(sid.as_str()) {
                continue;
            }
            if let Some(cwd) = row.cwd.as_deref() {
                *siblings.entry(cwd).or_insert(0) += 1;
                if let Some(m) = row.tree_mtime {
                    let e = newest_by_cwd.entry(cwd).or_insert((sid, m));
                    if m > e.1 {
                        *e = (sid, m);
                    }
                }
            }
        }
        // #269: `claude attach <prefix>` names a session by its first characters; a prefix
        // links only when exactly one known session starts with it.
        let sids: Vec<&str> = st.rows.keys().map(String::as_str).collect();
        let prefix_unique = |pf: &str| sids.iter().filter(|s| s.starts_with(pf)).count() == 1;
        let mut groups: HashMap<String, Group> = HashMap::new();
        for (sid, row) in &st.rows {
            // The group key IS the persisted `p:`/`a:` hide key (#27, #113): compute it in ONE
            // place — see `discover::session_key_from` for the full rule (repo wins over cwd, so
            // hiding a project hides the whole repo, subdirs included). The `_from` form, not the
            // transcript one, because the scan already resolved `repo`/`cwd` onto the row, so this
            // hot rebuild loop stays I/O-free.
            let discover::SessionKey {
                key,
                kind: key_kind,
                label,
                path: group_path,
            } = {
                // #373: a session started in another session's scratch (knack's `claude -p` in a
                // worktree under its scratchpad, a background job's tmp) belongs to the PROJECT of
                // the session that started it — the owner: "group them under knack". Followed up
                // the chain to the first session that is not one, and only through sessions this
                // index knows: an id that merely looks like a session's leaves the row where it is.
                let mut home = row;
                for _ in 0..8 {
                    match home
                        .spawned_by
                        .as_deref()
                        .filter(|p| *p != sid.as_str())
                        .and_then(|p| st.rows.get(p))
                    {
                        Some(parent) if !std::ptr::eq(parent, home) => home = parent,
                        _ => break,
                    }
                }
                discover::session_key_from(
                    row.agent,
                    home.repo.as_deref().map(Path::new),
                    home.cwd.as_deref().map(Path::new),
                )
            };
            let (kind, secondary) = match key_kind {
                // Leaf as the label (in `label`), FULL path as the secondary line (§4.2) — the
                // leaf-merge hedge: two checkouts sharing a leaf stay distinguishable one line
                // below. `path` is always `Some` for a project key.
                discover::SessionKeyKind::Project => (
                    "project",
                    tilde(
                        &group_path
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    ),
                ),
                discover::SessionKeyKind::Agent => {
                    ("agent", "desktop agent · no workspace".to_string())
                }
            };

            let growing = row.grew_at.is_some_and(|t| t.elapsed() < GROW_LINGER)
                || (row
                    .tree_mtime
                    .and_then(|m| now.duration_since(m).ok())
                    .is_some_and(|d| d < INFLIGHT_WINDOW)
                    && inflight_tool_in_tail(&row.path));
            // The process link now resolves for EVERY row (#112): growing rows need it as
            // the prerequisite for the controllable-terminal fact, idle rows for the
            // alive/finished split it always drove.
            let heuristic_ok = row
                .cwd
                .as_deref()
                .and_then(|c| newest_by_cwd.get(c))
                .is_some_and(|(newest, _)| *newest == sid.as_str());
            // A pairing that growth proved (#146) outranks the directory heuristic — it is
            // evidence about THIS session, not a pick among the directory's sessions.
            let link = row
                .proved_pid
                .as_ref()
                .and_then(|(pid, cwd)| {
                    st.procs
                        .iter()
                        .find(|p| p.pid == *pid && p.cwd.as_deref() == Some(cwd.as_str()))
                })
                .map(|p| AgentLink {
                    pid: p.pid,
                    confirmed: true,
                    terminal: Terminal::of(p),
                })
                .or_else(|| {
                    let &(pid, rivals) = paired.get(sid.as_str())?;
                    st.procs.iter().find(|p| p.pid == pid).map(|p| AgentLink {
                        pid: p.pid,
                        confirmed: rivals == 0,
                        terminal: Terminal::Detached,
                    })
                })
                .or_else(|| {
                    link(
                        &st.procs,
                        sid,
                        row.agent,
                        &row.path,
                        row.cwd.as_deref(),
                        heuristic_ok,
                        &prefix_unique,
                    )
                });
            let (state, conf) = if growing {
                ("growing", "")
            } else {
                match &link {
                    Some(l) if l.confirmed => ("idle", "confirmed"),
                    Some(_) => ("idle", "unconfirmed"),
                    None => ("finished", ""),
                }
            };
            // #145: how many sessions the heuristic was choosing BETWEEN. Launching without a
            // session id is the common case (measured: 5 of 8 live agents carry no uuid in
            // argv), and `claude --resume` then offers a PICKER — so the agent may be driving
            // any session in the directory, not the newest. Nothing on disk records which:
            // the process holds no fd to its transcript (measured: 0 `.jsonl` fds across every
            // live agent), and start-time does not separate them either (measured: in the one
            // ambiguous directory here, BOTH sessions have activity after the process began).
            // So the count is the honest statement — the size of the doubt, not a guess.
            let ambiguity = match link.as_ref().filter(|l| !l.confirmed) {
                Some(_) if paired.contains_key(sid.as_str()) => paired
                    .get(sid.as_str())
                    .map_or(1, |&(_, rivals)| rivals + 1),
                Some(_) => row
                    .cwd
                    .as_deref()
                    .and_then(|c| siblings.get(c).copied())
                    .unwrap_or(1),
                None => 1,
            };

            let visited = row.counters.is_some();
            let mtime_secs = row.last_event.unwrap_or_else(|| {
                row.tree_mtime
                    .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            // The agent-state pass consumes exactly what this loop already resolved
            // (#194) — growth, the process link, the activity clock — plus identity.
            let now_secs = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            facts.push(crate::state::RowFacts {
                sid: sid.clone(),
                agent: row.agent,
                path: row.path.clone(),
                cwd: row.cwd.clone(),
                title: row.title.clone(),
                growing,
                quiet_secs: now_secs.saturating_sub(mtime_secs),
                pid: link.as_ref().map(|l| l.pid),
                term: link
                    .as_ref()
                    .and_then(|l| l.terminal.target().map(str::to_string)),
                tmux: link.as_ref().and_then(|l| l.terminal.tmux_target()),
                confirmed: link.as_ref().map(|l| l.confirmed).unwrap_or(false),
                tree_mtime: row.tree_mtime,
            });
            // #113: a row is hidden if its OWN key is on the list, or its whole group is.
            let row_key = format!("s:{sid}");
            let hidden = st.ignored.contains(&row_key) || st.ignored.contains(&key);
            let mut j = json!({
                "id": sid,
                "name": row.title,
                "agent": row.agent.label(),
                "state": state,
                "conf": conf,
                "visited": visited,
                "activityTs": mtime_secs,
                "activity": human_age(
                    (mtime_secs > 0)
                        .then(|| SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs)),
                    now,
                ),
                "ignoreKey": row_key,
                "hidden": hidden,
            });
            if let Some(start) = row.first_event {
                j["startTs"] = json!(start);
            }
            if ambiguity > 1 {
                j["ambig"] = json!(ambiguity);
            }
            // #142: the family this session belongs to. Emitted on EVERY row (a session with
            // no forks is a family of one) so the client groups by one rule, not two.
            let root = family_root(sid);
            j["family"] = json!(root);
            if root != *sid {
                j["isFork"] = json!(true);
            }
            // #s53: a headless worker this index knows the starter of — the shell lists it in that
            // session's Agents pane instead of the session list.
            if let Some(parent) = st.started_by(sid) {
                j["startedBy"] = json!(parent);
                if let Some(brief) = row.brief.as_deref().filter(|b| !b.is_empty()) {
                    j["brief"] = json!(brief);
                }
            }
            // #133 constraint 2: another live session in this project → no send affordance on
            // this row (the resume path refuses it, the inject path is suppressed). The rail
            // hides `✎` when `projActive`, so the button and the route's rule agree.
            let proj_active = proj_has_other_live(sid, row.agent, row.cwd.as_deref());
            if proj_active {
                j["projActive"] = json!(true);
            }
            if let Some(l) = &link {
                j["pid"] = json!(l.pid);
                j["term"] = json!(l.terminal.kind());
                if let Terminal::Tmux {
                    sock: Some(sock), ..
                } = &l.terminal
                {
                    j["sock"] = json!(sock);
                }
                if let Some(t) = l.terminal.target() {
                    // The controllable target (#112): a tmux pane or screen session name.
                    j["target"] = json!(t);
                }
                // #133 tmux slice: this row can be INJECTED into iff the link is PROVEN
                // (§3.1 — never a cwd guess), it is in tmux, the agent has a driven shape
                // (claude/codex), AND its project has no other live session (constraint 2). The
                // rail offers the terminal-send affordance only on these rows; `consented` says
                // whether a standing grant already covers this exact pane/pid (send now) or one
                // is needed first.
                let driven = matches!(row.agent, Agent::CLAUDE | Agent::CODEX);
                if let (true, Some((sock, pane))) = (
                    l.confirmed && driven && !proj_active,
                    l.terminal.tmux_target(),
                ) {
                    j["injectable"] = json!(true);
                    j["consented"] = json!(consented(sock.as_deref(), &pane, sid, l.pid));
                }
            }
            if let Some((_, c)) = &row.counters {
                j["turns"] = json!(c.turns);
                j["tools"] = json!(c.tools);
                j["subs"] = json!(c.subs);
                j["child"] = json!(c.child_running);
            }
            // Cost from the LEDGER (§14), not the visit-gated meta stream: the row's own
            // transcript plus every sub-agent rollout banked on it. `costPartial` survives every
            // aggregation level: with a known subtotal it means `≥`; without one it means the
            // whole token-bearing mix is explicitly `unpriced`. `costOwn` is deliberately
            // separate from the total: the shells may split a complete roll-up only when the root
            // itself has priced usage, never by manufacturing `$0.00` from total == children.
            let sub = st.sub_costs.get(sid).copied();
            let total_cost = merge_cost(row.cost, sub);
            if let Some(total) = total_cost {
                if let Some(known) = total.known_usd {
                    j["cost"] = json!(known);
                }
                if total.partial {
                    j["costPartial"] = json!(true);
                }
                if let Some(own) = row.cost.and_then(|summary| summary.known_usd) {
                    j["costOwn"] = json!(own);
                }
                if let Some(sub) = sub {
                    if let Some(known) = sub.known_usd {
                        j["costSubs"] = json!(known);
                    }
                    if sub.partial {
                        j["costSubsPartial"] = json!(true);
                    }
                }
            }

            let g = groups.entry(key.clone()).or_insert_with(|| Group {
                kind,
                key,
                label,
                secondary,
                ..Default::default()
            });
            g.cost = merge_cost(g.cost, total_cost);
            g.latest = g.latest.max(mtime_secs);
            g.growing += usize::from(state == "growing");
            g.idle += usize::from(state == "idle");
            g.has_term |= link.as_ref().is_some_and(|l| l.terminal.target().is_some());
            g.rows.push(j);
        }

        let mut gs: Vec<Group> = groups.into_values().collect();
        // ORDER MUST BE A PURE FUNCTION OF THE DATA — the collections come out of HashMaps,
        // so every comparison bottoms out in a stable unique key — and it must be CALM.
        // State-first ordering was rejected by the owner (2026-08-08): growing flaps with
        // an agent's bursty write cadence, and an absolute growing-first rule turns that
        // flap into rows hopping. Instead, TWO BUCKETS on one line (`ACTIVE_WINDOW`):
        // active items are all tied and sort by NAME; stale items sort by recency, which
        // is stable because their mtimes are frozen. State still paints the dot and the
        // tint — it just no longer drives position.
        let now_secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let active = |ts: u64| now_secs.saturating_sub(ts) < ACTIVE_WINDOW.as_secs();
        gs.sort_by(|a, b| {
            // Active bucket first; recency compares only between two STALE items (an
            // active pair passes 0 == 0 through to the name), name asc, secondary asc.
            (
                active(b.latest),
                if active(a.latest) && active(b.latest) {
                    0
                } else {
                    b.latest
                },
                a.label.to_lowercase(),
                &a.secondary,
            )
                .cmp(&(
                    active(a.latest),
                    if active(a.latest) && active(b.latest) {
                        0
                    } else {
                        a.latest
                    },
                    b.label.to_lowercase(),
                    &b.secondary,
                ))
        });
        for g in &mut gs {
            g.rows.sort_by(|a, b| {
                let (ta, tb) = (
                    a["activityTs"].as_u64().unwrap_or(0),
                    b["activityTs"].as_u64().unwrap_or(0),
                );
                let name = |r: &Value| r["name"].as_str().unwrap_or("").to_lowercase();
                let both_active = active(ta) && active(tb);
                (
                    active(tb),
                    if both_active { 0 } else { tb },
                    name(a),
                    a["id"].as_str(),
                )
                    .cmp(&(
                        active(ta),
                        if both_active { 0 } else { ta },
                        name(b),
                        b["id"].as_str(),
                    ))
            });
        }
        // #113: a session is hidden by its own key OR its group's — count once for the
        // "Hidden (N)" reveal, and mark each group so the client can grey a whole hidden group.
        let mut hidden_count = 0usize;
        let out: Vec<Value> = gs
            .into_iter()
            .map(|g| {
                let cost_label = g.cost.map(|summary| match summary.known_usd {
                    Some(known) if summary.partial => format!("≥${known:.2}"),
                    Some(known) => format!("~${known:.2}"),
                    None => "unpriced".to_string(),
                });
                let meta = cost_label.as_ref().map_or_else(
                    || g.rows.len().to_string(),
                    |c| format!("{c} · {}", g.rows.len()),
                );
                let total = g.rows.len();
                hidden_count += g
                    .rows
                    .iter()
                    .filter(|r| r["hidden"].as_bool().unwrap_or(false))
                    .count();
                let group_hidden = st.ignored.contains(&g.key);
                let mut out = json!({
                    "kind": g.kind,
                    "label": g.label,
                    "secondary": g.secondary,
                    "metaLine": meta,
                    "hasTerm": g.has_term,
                    "growing": g.growing,
                    "idle": g.idle,
                    "total": total,
                    "ignoreKey": g.key,
                    "hidden": group_hidden,
                    "rows": g.rows,
                });
                if let Some(summary) = g.cost {
                    if let Some(known) = summary.known_usd {
                        out["cost"] = json!(known);
                    }
                    if summary.partial {
                        out["costPartial"] = json!(true);
                    }
                }
                out
            })
            .collect();
        json!({ "groups": out, "ignoredCount": hidden_count })
    }
}

/// Add the richer, hysteresis-gated agent verdict to each session row of an assembled
/// snapshot, IN PLACE — the tracker's verdicts exist only after its tick, which needs the
/// facts `assemble` gathers, so annotation is the last step before the one serialization.
/// (It used to take the serialized string, parse the whole machine-wide index back into a
/// tree, add five fields per row and re-emit it — on every scan tick. #26.)
///
/// `state`/`conf` are the long-standing process/index fields and remain byte-for-byte in
/// meaning.  The new names are deliberately separate: consumers can adopt busy/wait/idle and
/// its reason without breaking an older client that switches on growing/idle/finished.
fn annotate_agent_state(root: &mut Value, tracker: &crate::state::StateTracker) {
    let Some(groups) = root.get_mut("groups").and_then(Value::as_array_mut) else {
        return;
    };
    for row in groups
        .iter_mut()
        .filter_map(|g| g.get_mut("rows").and_then(Value::as_array_mut))
        .flatten()
    {
        let Some(sid) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if let Some((verdict, since)) = tracker.current(sid) {
            row["agentState"] = serde_json::to_value(verdict.state).unwrap_or(Value::Null);
            row["stateReason"] = json!(verdict.reason.as_str());
            row["stateDetail"] = json!(verdict.detail);
            row["stateConfidence"] =
                serde_json::to_value(verdict.confidence).unwrap_or(Value::Null);
            row["stateSince"] = json!(since);
        } else {
            // Non-immediate states pass through a one-tick stability gate. Keep the additive
            // wire shape deterministic during that first scan, while labeling the coarse
            // legacy-state fallback as inferred rather than presenting it as a verdict.
            let legacy = row
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("idle")
                .to_owned();
            row["agentState"] = json!(if legacy == "growing" { "busy" } else { "idle" });
            row["stateReason"] = json!(if legacy == "finished" {
                "exited"
            } else if legacy == "growing" {
                "starting"
            } else {
                "idle"
            });
            row["stateDetail"] = json!("state derivation is stabilizing");
            row["stateConfidence"] = json!("inferred");
            row["stateSince"] = Value::Null;
        }
    }
}

/// Decode a `%XX`-percent-encoded query value (hide keys arrive via `encodeURIComponent` —
/// a `p:<cwd>` key carries `/`, `:` and spaces). Unknown/short escapes pass through literally.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            ) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The monitor's STATE directory — where the hide list belongs (#197): it is user INTENT
/// that cannot be recomputed, and `~/.cache` is XDG-deletable at any time. `$XDG_STATE_HOME`
/// (else `~/.local/state`), overridable by `$AGENT_MONITOR_STATE` (legacy
/// `$CLAUDE_MONITOR_STATE` honored — mirroring the cache var, for tests and for
/// `agent-metrics`' matching lookup). The directory name follows [`renamed_dir`]'s
/// migration rule: an existing `claude-monitor` keeps being used; fresh installs
/// create `agent-monitor`.
/// `state_dir()` reads PROCESS-GLOBAL env, and cargo runs a crate's tests in parallel threads
/// of ONE process — so every test that redirects it must hold this lock, or it silently
/// redirects the tests running beside it. (Observed: a `ui` test setting `AGENT_MONITOR_STATE`
/// pointed `control`'s token test at the wrong directory, and only when run together.)
#[cfg(test)]
pub(crate) static STATE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The state-env guard every test that touches the state dir must hold (#153). It does three
/// things the hand-rolled `set_var`/`remove_var` pairs did not:
///
/// 1. **Sets BOTH names.** `state_dir()` prefers `AGENT_MONITOR_STATE` over
///    `CLAUDE_MONITOR_STATE`, so a test that set only the legacy name was still pointed at the
///    developer's real directory whenever their shell exported the new one.
/// 2. **Restores what was there**, instead of clearing to "no override" — clearing hands the
///    rest of the process the real state dir even though the developer had redirected it.
/// 3. **Holds the lock for exactly the redirected window**, so the restore cannot land while a
///    neighbouring test is mid-read (cargo runs a crate's tests as threads of ONE process).
#[cfg(test)]
pub(crate) struct StateEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    prev: [(&'static str, Option<std::ffi::OsString>); 2],
}

#[cfg(test)]
impl StateEnv {
    const VARS: [&'static str; 2] = ["AGENT_MONITOR_STATE", "CLAUDE_MONITOR_STATE"];

    /// Redirect the state dir at `dir` for as long as the guard lives.
    pub(crate) fn set(dir: impl AsRef<Path>) -> Self {
        let lock = STATE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let prev = Self::VARS.map(|v| (v, std::env::var_os(v)));
        for v in Self::VARS {
            std::env::set_var(v, dir.as_ref());
        }
        Self { _lock: lock, prev }
    }
}

#[cfg(test)]
impl Drop for StateEnv {
    fn drop(&mut self) {
        for (v, was) in &self.prev {
            match was {
                Some(val) => std::env::set_var(v, val),
                None => std::env::remove_var(v),
            }
        }
    }
}

pub fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENT_MONITOR_STATE")
        .or_else(|| std::env::var_os("CLAUDE_MONITOR_STATE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return p;
    }
    // #153, enforced rather than documented: under `cargo test` this crate must never resolve
    // the machine's real state dir. Reaching here means a test would read — and, one
    // `set_ignore` later, OVERWRITE — the developer's own hide list, consent grants and token.
    // That is not hypothetical: it is how `[]` replaced a live `ignored.json`.
    #[cfg(test)]
    panic!(
        "state_dir() resolved the REAL state directory inside a test — hold an \
         `index::StateEnv::set(<scratch>)` guard for the window that touches it (#153)"
    );
    #[cfg(not(test))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
            })
            .unwrap_or_else(|| PathBuf::from(".").join(".local").join("state"));
        renamed_dir(&base, "claude-monitor", "agent-monitor")
    }
}

/// The binary-rename migration rule for an on-disk directory (owner, 2026-08-22): an
/// existing OLD-named directory keeps being used — real state lives there and nothing
/// moves it — otherwise the NEW name is chosen (created by whoever writes first). When
/// BOTH exist the old one still wins (its data predates the rename; a stray new dir is
/// most likely an intermediate run's empty leftover) and a once-per-process warning
/// names both paths so the user can consolidate.
pub fn renamed_dir(base: &std::path::Path, old: &str, new: &str) -> PathBuf {
    let (old_p, new_p) = (base.join(old), base.join(new));
    if !old_p.exists() {
        return new_p;
    }
    if new_p.exists() {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            eprintln!(
                "warning: both {} and {} exist — using the former; \
                 consolidate into one (the newer name) to silence this",
                old_p.display(),
                new_p.display()
            );
        });
    }
    old_p
}

/// Read one hide-list file: `Ok(None)` = ABSENT (normal first run — silent), `Ok(Some)` =
/// parsed, `Err` = present but UNREADABLE or MALFORMED (a real problem, never "nothing is
/// hidden"). Splitting the two is the #197 fix: the old code collapsed them into an empty
/// set, so a corrupt or unreadable list silently un-hid everything.
fn read_ignored(path: &Path) -> std::io::Result<Option<BTreeSet<String>>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_str::<Vec<String>>(&raw)
        .map(|v| Some(v.into_iter().collect()))
        .map_err(std::io::Error::other)
}

/// Adopt a pre-#197 hide list into the state dir, once, if the state file is ABSENT (#154).
///
/// The candidates are passed IN rather than derived here, for the reason #153 exists: a
/// function that resolves `~/.cache` on its own is a function a test cannot keep away from the
/// developer's real files. Both binaries pass their own cache root first and the machine's
/// env-free default second ([`xdg_cache_root`]).
///
/// The second candidate is the whole fix. A monitor started under its own
/// `$AGENT_MONITOR_CACHE` shares the STATE dir by design but has no legacy list of its own, so
/// with only the first candidate it concluded "nothing to migrate", started with an empty set,
/// and its first hide-then-unhide persisted `[]` over the shared state path — losing a list it
/// had never even read. The old copies are left in place, as #197 chose, so a downgrade is safe.
pub fn migrate_hide_list(state_dir: &Path, candidates: &[PathBuf]) {
    let state = state_dir.join("ignored.json");
    match read_ignored(&state) {
        Ok(Some(_)) => return, // already migrated (or genuinely empty by the user's choice)
        Ok(None) => {}         // absent — the only case a migration may act on
        Err(e) => {
            // Unreadable is NOT the same as absent: overwriting it with a legacy copy would
            // silently discard whatever it holds. Leave it and say so.
            eprintln!(
                "warning: the hide list at {} exists but cannot be read ({e}) — not migrating \
                 over it; fix or remove the file",
                state.display()
            );
            return;
        }
    }
    for candidate in candidates {
        match read_ignored(candidate) {
            Ok(Some(set)) if !set.is_empty() => {
                save_ignored(&state, &set);
                return;
            }
            Ok(_) => {}
            Err(e) => eprintln!(
                "warning: the previous hide list at {} is unreadable ({e}) — skipping it",
                candidate.display()
            ),
        }
    }
}

/// Persist the hide list (best-effort: a write failure leaves the in-memory set authoritative
/// for this run rather than crashing the server). Writes the STATE path; the old cache copy
/// is LEFT in place (#197) so a downgrade does not lose the list — a later release removes it.
fn save_ignored(path: &Path, set: &BTreeSet<String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let keys: Vec<&String> = set.iter().collect();
    if let Ok(json) = serde_json::to_string(&keys) {
        let _ = std::fs::write(path, json);
    }
}

/// Fold a visited entry's meta stream into row counters — `MaterializedMeta` over every
/// record (§2.2: the index displays, it never resumes, so it does not align).
///
/// A stream folded by a DIFFERENT fold version is refused (#144). The viewer never had this
/// problem — `admit::recover` compares the header's versions and re-authors on a mismatch —
/// but the rail reads the stream directly, and discarding the header meant a `fold: 1` stream
/// was happily folded by version-3 code. The row then showed counters that a bump had already
/// invalidated, labelled as current. Returning `None` is the honest answer: the row reads
/// "tbd" (#134) until a visit re-authors the entry at the current version.
fn fold_counters(dir: &Path) -> Option<Counters> {
    let (header, reader) = MetaReader::open(dir).ok()??;
    if header.versions.fold != FOLD_VERSION {
        return None;
    }
    let mut mm = MaterializedMeta::default();
    for r in reader {
        mm.push(&r);
    }
    Some(Counters {
        turns: mm.session_meta.turns,
        tools: mm.session_meta.tools,
        subs: mm.session_meta.children.len(),
        child_running: mm.session_meta.children.iter().any(|c| c.running),
    })
}

/// How far BEFORE a one-shot's recorded start its session's first record may fall: `etime` is
/// whole seconds, so the start is known only to the second (#s50).
const ONE_SHOT_SLACK_SECS: u64 = 3;
/// How long after a one-shot starts its session's first record may arrive. Measured here
/// (2026-10-08, three runs of `claude -p`): 0.7–0.9 s; the rest is room for a loaded machine.
const ONE_SHOT_WINDOW_SECS: u64 = 60;

/// Pair each one-shot process with the session it created (#s50): sid → (pid, rivals).
///
/// A no-id launch leaves nothing on disk naming its session (#145), and the directory rule gives
/// a process to the NEWEST session of its cwd only. So with a coordinator and several `claude -p`
/// workers in one repo, a worker deep in a long command was not the newest, lost its process, and
/// read "exited with Bash pending" until its next write — knack on aries-black, 2026-10-07, where
/// one worker flapped busy → exited-mid-work → busy on the same pid. A one-shot is not a pick,
/// though: it starts a NEW session, whose first record lands within a second of the process. So
/// each one-shot whose argv names no session takes, in start order, the earliest unclaimed
/// one-shot session of its cwd that began inside its window. `rivals` counts the other one-shots
/// of that cwd that could have taken the same session instead (their window holds its first
/// record, and the session they took, if any, sits in this one's window too); the link is
/// confirmed only at zero, and made either way, since every candidate is alive.
fn pair_one_shots<'a>(
    procs: &[Proc],
    sessions: impl IntoIterator<Item = (&'a str, &'a str, u64)>,
) -> HashMap<String, (u32, usize)> {
    let mut by_cwd: HashMap<&str, Vec<(u64, &str)>> = HashMap::new();
    for (sid, cwd, first) in sessions {
        by_cwd.entry(cwd).or_default().push((first, sid));
    }
    let mut shots: HashMap<&str, Vec<(u64, u32)>> = HashMap::new();
    for p in procs {
        let headless_new = is_agent_exe(&p.exe_base, &p.argv)
            && serves(p, Agent::CLAUDE)
            && is_one_shot(&p.argv)
            && !is_helper(&p.argv)
            && session_ref(&p.argv).is_none();
        if let (true, Some(cwd), Some(start)) = (headless_new, p.cwd.as_deref(), p.started) {
            shots.entry(cwd).or_default().push((start, p.pid));
        }
    }
    let within = |start: u64, first: u64| {
        first + ONE_SHOT_SLACK_SECS >= start && first <= start + ONE_SHOT_WINDOW_SECS
    };
    let mut out = HashMap::new();
    for (cwd, mut here) in shots {
        let Some(sessions) = by_cwd.get_mut(cwd) else {
            continue;
        };
        sessions.sort_unstable();
        here.sort_unstable();
        let mut taken = vec![false; sessions.len()];
        // Each one-shot's claim, in start order: the index of the session it took, if any.
        let mut claims: Vec<Option<usize>> = Vec::with_capacity(here.len());
        for &(start, _) in &here {
            let claim = (0..sessions.len()).find(|&i| !taken[i] && within(start, sessions[i].0));
            if let Some(i) = claim {
                taken[i] = true;
            }
            claims.push(claim);
        }
        // A rival is another one-shot that could have taken this session INSTEAD: one with no
        // claim whose window holds it, or one whose claim this process could have taken in turn.
        // A worker that began well before another already holds its own earlier session, and
        // the two cannot be swapped.
        for (k, &(start, pid)) in here.iter().enumerate() {
            let Some(i) = claims[k] else { continue };
            let (first, sid) = sessions[i];
            let rivals = here
                .iter()
                .zip(&claims)
                .enumerate()
                .filter(|&(j, (&(s, _), claim))| {
                    j != k && within(s, first) && claim.is_none_or(|c| within(start, sessions[c].0))
                })
                .count();
            out.insert(sid.to_string(), (pid, rivals));
        }
    }
    out
}

/// Where the worker → starter edges live (#s53), under the monitor's cache root: a cache, so an
/// unreadable file is an empty map and a deleted one only forgets workers no longer running.
const STARTED_BY_FILE: &str = "started-by.json";

fn read_started_by(path: &Path) -> BTreeMap<String, String> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write the edges whole, through a temporary and a rename, so a reader never sees half a map.
fn write_started_by(path: &Path, edges: &BTreeMap<String, String>) {
    let Ok(bytes) = serde_json::to_vec_pretty(edges) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Whether the transcript's head says a one-shot run wrote it — Claude Code records
/// `"entrypoint":"sdk-cli"` for `claude -p` (measured 2026-10-08) where an interactive session
/// says `cli` — and if so, its BRIEF: the first line of its first prompt, at most 120 characters
/// (`Some("")` when the head holds no prompt). `None` for any other session. 64 KB is the head of
/// any transcript: the first prompt record carries the entrypoint.
fn head_one_shot(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut buf)
        .ok()?;
    // Parsed, never matched as text: a writer that spaces its JSON (`"entrypoint": "sdk-cli"`) is
    // the same record. The last line of a cut head may be partial and simply fails to parse.
    let records: Vec<Value> = String::from_utf8_lossy(&buf)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let entry = records
        .iter()
        .find_map(|r| r.get("entrypoint").and_then(Value::as_str))?;
    if entry != "sdk-cli" {
        return None;
    }
    let prompt = records.iter().find_map(|rec| {
        if rec.get("type").and_then(Value::as_str) != Some("user") {
            return None;
        }
        let text = match rec.pointer("/message/content")? {
            Value::String(t) => t.as_str(),
            Value::Array(parts) => parts
                .iter()
                .find_map(|p| p.get("text").and_then(Value::as_str))?,
            _ => return None,
        };
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        Some(line.chars().take(120).collect::<String>())
    });
    Some(prompt.unwrap_or_default())
}

/// Resolve which live agent process (if any) is behind session `sid` — the probe's §1
/// precedence, re-cut for Claude Code 2.1.278's daemon topology (#269), where the process in
/// the pane is a `claude attach <prefix>` CLIENT and the session's engine runs detached in a
/// `--bg-pty-host`. Each mechanism was measured on this machine before it was written down:
///   1. A process that NAMES `sid` — `session_ref` on its argv: `--session-id <uuid>`, a
///      `--resume <uuid|path>`, `attach <prefix>` (a prefix links only when exactly one known
///      session starts with it), or a bare uuid TOKEN. Never a substring: a background job's
///      scratchpad path carries its session's uuid, and `argv.contains(sid)` once linked a
///      session to that `bash` — "confirmed", detached — while the agent sat in a tmux pane.
///      Agent exes only. When several name the same session (the engine by `--session-id`, the
///      pane's client by prefix) the best-HOSTED wins: tmux/screen, then a tty, then detached —
///      so the pane, which is where a keystroke can go, beats the engine, which is where the
///      session merely runs. Exact.
///   2. The session's transcript held OPEN by an agent process — exact (Codex holds its
///      rollout `.jsonl` open; Claude appends-and-closes, so this never fires for it).
///   3. An agent process whose cwd matches the session's and whose argv names no session —
///      the directory heuristic, for the NEWEST session of that cwd only (`heuristic_ok`),
///      never a daemon helper. **Confirmed when it is the only such process in the
///      directory** — the owner's rule (2026-09-23): "when there is only one active claude
///      process associated with a claude session, we would pair them even if the session id
///      is not in argv." With one process there is one pane, and that pane is the injection
///      target whichever session the row is labelled with; the label self-corrects on the
///      next append, because the driven session becomes the newest. Two or more processes in
///      one directory stay unconfirmed, and the row reports how many sessions it was choosing
///      between (#145).
///
/// Step 3 is the COMMON path, not a fallback: `claude --resume` with no id (the picker) is how
/// most of these sessions were launched (measured: 9 of 12 live agents here), and Claude never
/// holds its transcript open, so steps 1–2 cannot fire for them.
fn link(
    procs: &[Proc],
    sid: &str,
    agent: Agent,
    transcript: &Path,
    cwd: Option<&str>,
    heuristic_ok: bool,
    prefix_unique: &dyn Fn(&str) -> bool,
) -> Option<AgentLink> {
    let mk = |p: &Proc, confirmed: bool| AgentLink {
        pid: p.pid,
        confirmed,
        // A helper's pane is the one it INHERITED from the client that spawned it, never
        // where a session's UI is — see `is_helper`.
        terminal: if is_helper(&p.argv) || is_one_shot(&p.argv) {
            Terminal::Detached
        } else {
            Terminal::of(p)
        },
    };
    // Rank by how the process is HOSTED — a multiplexer target beats a bare tty beats
    // detached, and a helper ranks as detached whatever it inherited — then break ties on pid
    // so the choice stays a pure function of the data. Taking the FIRST match once meant the
    // lowest pid, i.e. usually the oldest: a real knack session hosted in a `tmux -L knack`
    // pane reported "detached" because a stale sibling won.
    let host = |p: &Proc| {
        let rank = match Terminal::of(p) {
            Terminal::Tmux { .. } | Terminal::Screen(_) => 2,
            Terminal::Tty => 1,
            Terminal::Detached => 0,
        };
        (
            if is_helper(&p.argv) || is_one_shot(&p.argv) {
                0
            } else {
                rank
            },
            std::cmp::Reverse(p.pid),
        )
    };
    let names = |p: &Proc| match session_ref(&p.argv) {
        Some(SessionRef::Exact(u)) => u == sid,
        Some(SessionRef::Prefix(pf)) => sid.starts_with(pf.as_str()) && prefix_unique(&pf),
        None => false,
    };
    if let Some(p) = procs
        .iter()
        .filter(|p| is_agent_exe(&p.exe_base, &p.argv) && names(p))
        .max_by_key(|p| host(p))
    {
        return Some(mk(p, true));
    }
    let t = transcript.to_string_lossy();
    if let Some(p) = procs.iter().find(|p| {
        is_agent_exe(&p.exe_base, &p.argv) && p.open_jsonl.iter().any(|f| f.as_str() == t)
    }) {
        return Some(mk(p, true));
    }
    if let Some(cwd) = cwd.filter(|_| heuristic_ok) {
        let cands: Vec<&Proc> = procs
            .iter()
            .filter(|p| {
                is_agent_exe(&p.exe_base, &p.argv)
                    && serves(p, agent)
                    && !is_helper(&p.argv)
                    && !is_one_shot(&p.argv)
                    && p.cwd.as_deref() == Some(cwd)
                    && session_ref(&p.argv).is_none()
            })
            .collect();
        if let Some(p) = cands.iter().copied().max_by_key(|p| host(p)) {
            return Some(mk(p, cands.len() == 1));
        }
    }
    None
}

/// What a process's argv says about WHICH session it drives (#269) — Claude Code's launch
/// shapes as of 2.1.278, each measured on this machine:
///
/// * `--session-id <uuid>` — the engine: `claude --bg-pty-host … -- <bin> --session-id X
///   --fork-session --resume <parent.jsonl>`. Exact, and it wins over everything after it,
///   because under `--fork-session` the `--resume` names the PARENT the fork was copied from.
/// * `--fork-session` with no `--session-id` — a new id the argv cannot name. `None`.
/// * `--resume <uuid>` or `--resume <path whose stem is a uuid>` — the same session
///   continued. Exact. `--resume` followed by anything else (the picker, a flag) names nothing.
/// * `attach <hex>` — the pane's CLIENT, which names its session by the first eight characters
///   (`claude attach f7e03d40`). A prefix; exact only when it is the whole uuid.
/// * a bare uuid token — the legacy shape, whatever flag it followed.
///
/// A whole TOKEN, never a substring — see `link`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionRef {
    Exact(String),
    Prefix(String),
}

fn session_ref(argv: &str) -> Option<SessionRef> {
    let toks: Vec<&str> = argv.split_whitespace().collect();
    let uuid_of = |t: &str| -> Option<String> {
        if is_uuid(t) {
            return Some(t.to_string());
        }
        let stem = t.rsplit('/').next().unwrap_or(t);
        let stem = stem.strip_suffix(".jsonl").unwrap_or(stem);
        is_uuid(stem).then(|| stem.to_string())
    };
    let after = |flag: &str| {
        toks.iter()
            .position(|t| *t == flag)
            .and_then(|i| toks.get(i + 1))
            .copied()
    };
    if let Some(v) = after("--session-id").and_then(uuid_of) {
        return Some(SessionRef::Exact(v));
    }
    if toks.contains(&"--fork-session") {
        return None;
    }
    if let Some(v) = after("--resume").and_then(uuid_of) {
        return Some(SessionRef::Exact(v));
    }
    // A one-shot's argv carries its PROMPT (#s50): a session id the prompt merely mentions
    // must not name the process. Only the flags above can.
    if is_one_shot(argv) {
        return None;
    }
    if toks.get(1) == Some(&"attach") {
        if let Some(id) = toks.get(2) {
            if is_uuid(id) {
                return Some(SessionRef::Exact(id.to_string()));
            }
            if id.len() >= 8 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
                return Some(SessionRef::Prefix(id.to_string()));
            }
        }
    }
    toks.iter()
        .find(|t| is_uuid(t))
        .map(|t| SessionRef::Exact(t.to_string()))
}

/// Claude Code 2.1.27x's daemon topology, as the process table shows it: `claude daemon run`
/// supervises, `bg-pty-host` (also the app's `claude --bg-pty-host …` form) is the pty a
/// session's engine runs in, `bg-spare` is a pre-warmed pty waiting to be claimed. None of
/// them hosts a UI, and the daemon INHERITS `TMUX_PANE` from the client that spawned it —
/// measured: `claude daemon run --spawned-by {"pid":17630}` read as pane %0 on the knack
/// server, which is that client's pane, not the daemon's. So a helper is never a candidate for
/// the directory heuristic, never counts as a process a session could be driven from, and when
/// one does name a session (the engine, by `--session-id`) it links for LIVENESS only, with a
/// terminal of Detached — a headless daemon session reading "finished" would be offered a
/// resume, which forks a session that is in fact running.
fn is_helper(argv: &str) -> bool {
    let mut toks = argv.split_whitespace();
    let _exe = toks.next();
    matches!(toks.next(), Some("daemon" | "bg-pty-host" | "bg-spare"))
        || argv
            .split_whitespace()
            .any(|t| t == "--bg-pty-host" || t == "--bg-spare")
}

/// A one-shot run, `claude -p` / `--print` (#s50): it answers one prompt and exits. It reads no
/// pane — a worker a session started from its Bash INHERITS that session's `TMUX_PANE` (measured
/// on aries-black: knack's workers read as pane %0, their coordinator's) — so, like a helper, it
/// is never a directory candidate and its terminal is Detached whatever it inherited; a send into
/// that pane would type into the coordinator. Unless its flags resume a session it starts a NEW
/// one, which is what pairs it (`pair_one_shots`).
fn is_one_shot(argv: &str) -> bool {
    argv.split_whitespace().any(|t| t == "-p" || t == "--print")
}

/// The agent a process IS, by its executable — or `None` for one the monitor cannot place (a
/// configured extra pattern), which stays a candidate for any agent's session (#s59).
fn proc_agent(p: &Proc) -> Option<Agent> {
    match p.exe_base.to_ascii_lowercase().as_str() {
        "claude" => Some(Agent::CLAUDE),
        "codex" => Some(Agent::CODEX),
        "qoderwork" => Some(Agent::QODERWORK),
        "qoder" => Some(Agent::QODER),
        "qwenwork" | "qwenworkcn" => Some(Agent::QWENWORK),
        _ => None,
    }
}

/// Whether `p` can be driving a session of `agent` (#s59): a Codex process never drives a Claude
/// session. On hong-devserver a Codex pane beside a Claude one in the same repo was taken as the
/// Claude session's process — the lower pid won the tie — and as a second candidate it made the
/// link unconfirmed, so the session read as neither attached nor writable.
fn serves(p: &Proc, agent: Agent) -> bool {
    proc_agent(p).is_none_or(|a| a == agent)
}

/// The one marker of a Claude FORK the machine offers (#269): the engine's argv,
/// `--session-id <child> --fork-session --resume <parent.jsonl>`. Returns `(child, parent)`.
fn fork_parent_from_argv(argv: &str) -> Option<(String, String)> {
    let toks: Vec<&str> = argv.split_whitespace().collect();
    if !toks.contains(&"--fork-session") {
        return None;
    }
    let after = |flag: &str| {
        toks.iter()
            .position(|t| *t == flag)
            .and_then(|i| toks.get(i + 1))
            .copied()
    };
    let child = after("--session-id").filter(|t| is_uuid(t))?.to_string();
    let parent = after("--resume")
        .map(|t| t.rsplit('/').next().unwrap_or(t))
        .map(|st| st.strip_suffix(".jsonl").unwrap_or(st))
        .filter(|st| is_uuid(st))?
        .to_string();
    Some((child, parent))
}

/// The UUID a row's file STEM ends with — a Codex stem is `rollout-<ts>-<uuid>` and a
/// Claude stem is bare. This is the bridge the sub-agent roll-up needs (§14): a rollout
/// names its parent by bare uuid, but the index keys rows by stem.
fn trailing_uuid(stem: &str) -> Option<String> {
    let tail = stem.len().checked_sub(36).and_then(|i| stem.get(i..))?;
    is_uuid(tail).then(|| tail.to_string())
}

fn is_uuid(t: &str) -> bool {
    t.len() == 36
        && t.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Environment variable holding extra agent-recognition patterns, comma-separated. Each entry
/// is `basename:<name>`, `argv:<substring>`, or a bare `<name>` (same as `basename:`).
const AGENT_PATTERNS_ENV: &str = "AGENT_MONITOR_AGENT_PATTERNS";
const AGENT_PATTERNS_ENV_LEGACY: &str = "CLAUDE_MONITOR_AGENT_PATTERNS";

/// What an extra recognition pattern is matched against.
///
/// A bare entry means the BASENAME, deliberately: an argv substring is the loose end here —
/// `node` or `sh` would claim every shell on the machine as an agent, and everything
/// downstream (the growth proof, the cwd heuristic) then has a phantom candidate to pick
/// among — so widening a pattern to the whole command line has to be asked for by name.
enum AgentPattern {
    /// Executable basename, compared case-insensitively like the built-ins.
    Basename(String),
    /// Substring of the full command line — the only way to see a wrapper whose basename is
    /// the interpreter (`npx codex`, `node ./node_modules/.bin/codex`).
    Argv(String),
}

impl AgentPattern {
    /// Parse the variable's value. Entries are comma-separated with no escape, so a pattern
    /// cannot contain a comma; match either side of it instead. Empty entries are skipped so
    /// a trailing comma is harmless.
    fn parse(spec: &str) -> Vec<Self> {
        spec.split(',')
            .filter_map(|entry| {
                let entry = entry.trim();
                let (make, value): (fn(String) -> Self, &str) = match entry.split_once(':') {
                    Some(("argv", v)) => (Self::Argv, v.trim()),
                    Some(("basename", v)) => (Self::Basename, v.trim()),
                    _ => (Self::Basename, entry),
                };
                (!value.is_empty()).then(|| make(value.to_string()))
            })
            .collect()
    }

    fn matches(&self, exe_base: &str, argv: &str) -> bool {
        match self {
            Self::Basename(name) => exe_base.eq_ignore_ascii_case(name),
            Self::Argv(needle) => argv.contains(needle.as_str()),
        }
    }
}

/// The parsed patterns, read ONCE. `is_agent_exe` is called for every process in the table on
/// every refresh, and re-reading plus re-splitting the variable each time would pay that cost
/// for an answer that cannot change within a run.
fn extra_agent_patterns() -> &'static [AgentPattern] {
    static PATTERNS: std::sync::OnceLock<Vec<AgentPattern>> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        std::env::var(AGENT_PATTERNS_ENV)
            .or_else(|_| std::env::var(AGENT_PATTERNS_ENV_LEGACY))
            .map(|spec| AgentPattern::parse(&spec))
            .unwrap_or_default()
    })
}

fn is_agent_exe(exe_base: &str, argv: &str) -> bool {
    // Qwenwork's desktop app runs as `QwenWorkCN`; its own CLI as `qwenwork` (#358).
    const BUILTINS: &[&str] = &[
        "claude",
        "codex",
        "qoderwork",
        "qoder",
        "qwenwork",
        "qwenworkcn",
    ];
    BUILTINS.iter().any(|b| exe_base.eq_ignore_ascii_case(b))
        || extra_agent_patterns()
            .iter()
            .any(|p| p.matches(exe_base, argv))
}

/// The process table (full `command=` argv — NEVER bulk `comm=`, which truncates absolute
/// paths and silently drops agents launched by one; the probe measured losing 2 of 4), plus
/// the per-AGENT-pid facts: tty, environment multiplexer markers, cwd and open `.jsonl`
/// fds — each from the probe's verified source, all batched so a refresh is a fixed handful
/// of subprocesses however many sessions exist.
fn scan_procs() -> Vec<Proc> {
    let run = |args: &[&str]| -> String {
        std::process::Command::new(args[0])
            .args(&args[1..])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut procs = parse_ps(&run(&["ps", "-axo", "pid=,command="]));
    let agent_pids: Vec<String> = procs
        .iter()
        .filter(|p| is_agent_exe(&p.exe_base, &p.argv))
        .map(|p| p.pid.to_string())
        .collect();
    if agent_pids.is_empty() {
        return procs;
    }
    let pids = agent_pids.join(",");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    apply_tty(
        &mut procs,
        &run(&["ps", "-o", "pid=,tty=,etime=", "-p", &pids]),
        now,
    );
    // The multiplexer is visible ONLY in the environment (§2 of the probe): `ps eww`
    // appends `K=V` pairs after the command.
    apply_env(
        &mut procs,
        &run(&["ps", "eww", "-o", "pid=,command=", "-p", &pids]),
    );
    apply_lsof(&mut procs, &run(&["lsof", "-p", &pids, "-Fpfn"]));
    procs
}

/// `pid command…` lines → bare [`Proc`]s (identity facts only).
fn parse_ps(out: &str) -> Vec<Proc> {
    let mut procs = Vec::new();
    for line in out.lines() {
        let line = line.trim_start();
        let Some((pid, argv)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let exe_base = argv
            .split_whitespace()
            .next()
            .and_then(|exe| exe.rsplit('/').next())
            .unwrap_or("")
            .to_string();
        procs.push(Proc {
            pid,
            argv: argv.to_string(),
            exe_base,
            cwd: None,
            open_jsonl: Vec::new(),
            tty: None,
            pane: None,
            tmux_sock: None,
            screen: None,
            started: None,
            parent_session: None,
        });
    }
    procs
}

/// `pid tty etime` lines → the controlling tty (`??` means detached and stays `None`) and the
/// start, `now` less the elapsed time.
fn apply_tty(procs: &mut [Proc], out: &str, now: u64) {
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(tty)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let Some(p) = procs.iter_mut().find(|p| p.pid == pid) else {
            continue;
        };
        if tty != "??" {
            p.tty = Some(tty.to_string());
        }
        p.started = it
            .next()
            .and_then(crate::state::parse_etime)
            .map(|e| now.saturating_sub(e));
    }
}

/// `ps eww` lines (command + environment, space-separated) → `TMUX_PANE` / `STY` markers.
fn apply_env(procs: &mut [Proc], out: &str) {
    for line in out.lines() {
        let line = line.trim_start();
        let Some((pid, rest)) = line.split_once(' ') else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        let Some(p) = procs.iter_mut().find(|p| p.pid == pid) else {
            continue;
        };
        let (mut session, mut child) = (None, false);
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("TMUX_PANE=") {
                p.pane = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("TMUX=") {
                p.tmux_sock = v.split(',').next().map(str::to_string);
            } else if let Some(v) = tok.strip_prefix("STY=") {
                p.screen = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("CLAUDE_CODE_SESSION_ID=") {
                session = is_uuid(v).then(|| v.to_string());
            } else if tok == "CLAUDE_CODE_CHILD_SESSION=1" {
                child = true;
            }
        }
        p.parent_session = session.filter(|_| child);
    }
}

/// `lsof -Fpfn` records → per-pid cwd (the `cwd` fd) and open `.jsonl` paths (the Codex
/// rollout link). Field format: `p<pid>`, then repeating `f<fd>` + `n<name>` pairs.
fn apply_lsof(procs: &mut [Proc], out: &str) {
    let mut cur: Option<u32> = None;
    let mut fd = String::new();
    for line in out.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            cur = pid.parse().ok();
        } else if let Some(f) = line.strip_prefix('f') {
            fd = f.to_string();
        } else if let Some(name) = line.strip_prefix('n') {
            let Some(pid) = cur else { continue };
            let Some(p) = procs.iter_mut().find(|p| p.pid == pid) else {
                continue;
            };
            if fd == "cwd" {
                p.cwd = Some(name.to_string());
            } else if name.ends_with(".jsonl") && fd.chars().all(|c| c.is_ascii_digit()) {
                p.open_jsonl.push(name.to_string());
            }
        }
    }
}

/// Line `"type"`s that CARRY a timestamp but are not session ACTIVITY — housekeeping the
/// agent writes long after the user stopped. The bug they cause: a session last worked at
/// 00:55 grew a `file-history-snapshot` (a git snapshot) at 21:55, and the rail read it as
/// activity 11 h ago instead of the true ~32 h. Denylist, not allowlist: an unknown agent's
/// real turns still count; only these named noise rows are skipped.
const NON_ACTIVITY_TYPES: &[&str] = &["file-history-snapshot", "summary"];

/// The FIRST activity timestamp — `last_event_ts`'s head-side sibling, the start of the
/// rail's session span (#129). Escalating windows, because the head is where an agent
/// parks its bulk housekeeping: one real transcript here opens with 22 `file-history-
/// snapshot` lines of ~25 KiB each and its first real line sits 424 KiB in. The cheap
/// window resolves every other session on this machine; the wide one is the fallback, and
/// the caller probes ONCE per session (an append-only log's head never changes).
fn first_event_ts(path: &Path) -> Option<u64> {
    [64 * 1024, 1024 * 1024]
        .into_iter()
        .find_map(|cap| first_event_within(path, cap))
}

fn first_event_within(path: &Path, cap: usize) -> Option<u64> {
    use std::io::Read;
    let mut buf = vec![0u8; cap];
    let mut f = std::fs::File::open(path).ok()?;
    let mut n = 0;
    while n < cap {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return None,
        }
    }
    buf.truncate(n);
    let text = String::from_utf8_lossy(&buf);
    // The last line of a truncated window may be cut mid-record — never trust it.
    let complete = text.len() == n && n < cap;
    let mut lines: Vec<&str> = text.lines().collect();
    if !complete {
        lines.pop();
    }
    for line in lines {
        let ty = field_after(line, "\"type\":\"").next();
        if ty.is_some_and(|t| NON_ACTIVITY_TYPES.contains(&t)) {
            continue;
        }
        for ts in field_after(line, "\"timestamp\":\"") {
            if let Some(secs) = metrics::parse_ts(ts).filter(|s| *s > 0) {
                return Some(secs as u64);
            }
        }
    }
    None
}

/// Claude Code's word that the compaction it closes ran while the session sat IDLE at its
/// prompt (#s68): a `system` `informational` notice — "Compacted while idle, before the prompt
/// cache expired" — after the boundary, the summary and the attachments, about an hour after the
/// last answer. Nobody did anything, so none of that compaction is activity, and the clock reads
/// back to the last activity before its boundary. Counted, it set the row growing, published
/// `busy` for the linger, and restarted the clock a stall is measured from.
const IDLE_COMPACTION_NOTICE: &str = "Compacted while idle";

/// The last ACTIVITY timestamp in a transcript's tail: scan the final 32 KiB line-wise and
/// keep the latest timestamp on a line that is NOT [housekeeping](NON_ACTIVITY_TYPES) nor part
/// of an [idle compaction](IDLE_COMPACTION_NOTICE). Each agent's format goes through the shared
/// `parse_ts`; `None` when the window holds no parseable activity timestamp. An idle compaction
/// whose boundary lies beyond that window — 194 KB and 254 KB behind its notice, measured — is
/// read again through 4 MiB.
fn last_event_ts(path: &Path) -> Option<u64> {
    match last_event_within(path, 32 * 1024) {
        Activity::At(secs) => Some(secs),
        Activity::Nothing => None,
        Activity::Behind => match last_event_within(path, 4 * 1024 * 1024) {
            Activity::At(secs) => Some(secs),
            Activity::Nothing | Activity::Behind => None,
        },
    }
}

/// What one [`last_event_ts`] window says.
enum Activity {
    At(u64),
    Nothing,
    /// The window ends in an idle compaction whose boundary — or the activity before it — lies
    /// further back than the window reaches.
    Behind,
}

fn last_event_within(path: &Path, window: u64) -> Activity {
    use std::io::{Read, Seek, SeekFrom};
    let read = || -> Option<Vec<u8>> {
        let mut f = std::fs::File::open(path).ok()?;
        let len = f.metadata().ok()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(window))).ok()?;
        let mut raw = Vec::new();
        f.read_to_end(&mut raw).ok()?;
        Some(raw)
    };
    let Some(raw) = read() else {
        return Activity::Nothing;
    };
    // Lossy: a seek into a multi-byte character costs that one severed line, not the window.
    let buf = String::from_utf8_lossy(&raw);
    let mut last: Option<u64> = None;
    // The clock as it stood at the newest compaction boundary in the window.
    let mut at_boundary: Option<Option<u64>> = None;
    let mut behind = false;
    for line in buf.lines() {
        // Field-level (no per-line JSON parse — pure cost here). Skip housekeeping rows by
        // their `"type"`; take the newest timestamp on everything else.
        let ty = field_after(line, "\"type\":\"").next();
        if ty.is_some_and(|t| NON_ACTIVITY_TYPES.contains(&t)) {
            continue;
        }
        if line.contains("\"subtype\":\"compact_boundary\"") {
            at_boundary = Some(last);
        } else if line.contains("\"subtype\":\"informational\"")
            && line.contains(IDLE_COMPACTION_NOTICE)
        {
            // Everything since the boundary was the idle compaction: back to before it.
            match at_boundary.take() {
                Some(Some(before)) => last = Some(before),
                _ => {
                    last = None;
                    behind = true;
                }
            }
            continue;
        }
        for ts in field_after(line, "\"timestamp\":\"") {
            if let Some(secs) = metrics::parse_ts(ts).filter(|s| *s > 0) {
                last = Some(last.map_or(secs as u64, |cur: u64| cur.max(secs as u64)));
            }
        }
    }
    match last {
        Some(secs) => Activity::At(secs),
        None if behind => Activity::Behind,
        None => Activity::Nothing,
    }
}

/// Every quoted value following `pat` on `line`.
fn field_after<'a>(line: &'a str, pat: &str) -> impl Iterator<Item = &'a str> {
    let mut rest = line;
    let mut out = Vec::new();
    while let Some(i) = rest.find(pat) {
        let v = &rest[i + pat.len()..];
        if let Some(end) = v.find('"') {
            out.push(&v[..end]);
            rest = &v[end..];
        } else {
            break;
        }
    }
    out.into_iter()
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string()
}

/// `~`-abbreviate a home-rooted path for the group's secondary line.
fn tilde(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

/// Compact "how long ago" for the row's right edge.
fn human_age(mtime: Option<SystemTime>, now: SystemTime) -> String {
    let Some(m) = mtime else { return "—".into() };
    let Ok(d) = now.duration_since(m) else {
        return "just now".into();
    };
    let s = d.as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => {
            let h = s / 3600;
            let m = (s % 3600) / 60;
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{}d", s / 86_400),
    }
}

/// Resolve the monitor's cache root (R5): `$AGENT_MONITOR_CACHE` (legacy
/// `$CLAUDE_MONITOR_CACHE` honored), else [`renamed_dir`]'s migration rule under
/// `$XDG_CACHE_HOME`/`~/.cache`: an existing `claude-monitor` keeps being used;
/// fresh installs create `agent-monitor`; both existing warns once.
pub fn default_root() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_MONITOR_CACHE")
        .or_else(|| std::env::var_os("CLAUDE_MONITOR_CACHE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(p);
    }
    xdg_cache_root()
}

/// The cache root this machine would use with NO env override — `$XDG_CACHE_HOME` (else
/// `~/.cache`) under [`renamed_dir`]'s migration rule.
///
/// Split out of [`default_root`] for #154. The hide list's one-time migration has to look
/// where the entries actually ARE, and for an instance running under its own
/// `$AGENT_MONITOR_CACHE` that is not its own root: `default_root()` would hand it back the
/// override it was started with, which is empty, and the migration would conclude there was
/// nothing to migrate.
pub fn xdg_cache_root() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| anyhow::anyhow!("no $HOME — nowhere to keep the monitor's cache"))?;
    Ok(renamed_dir(&base, "claude-monitor", "agent-monitor"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_merge_keeps_known_subtotals_and_unpriced_state() {
        let priced = crate::cost::CostSummary {
            known_usd: Some(12.0),
            partial: false,
        };
        let unpriced = crate::cost::CostSummary {
            known_usd: None,
            partial: true,
        };
        assert_eq!(merge_cost(None, None), None);
        assert_eq!(merge_cost(Some(priced), None), Some(priced));
        assert_eq!(
            merge_cost(Some(priced), Some(unpriced)),
            Some(crate::cost::CostSummary {
                known_usd: Some(12.0),
                partial: true,
            })
        );
    }

    #[test]
    fn sub_cost_entries_survive_keep_and_are_removed_by_clear_updates() {
        let priced = crate::cost::CostSummary {
            known_usd: Some(4.0),
            partial: false,
        };
        let unpriced = crate::cost::CostSummary {
            known_usd: None,
            partial: true,
        };
        let (p1, p2) = (PathBuf::from("child-1"), PathBuf::from("child-2"));
        let mut entries = HashMap::new();
        apply_sub_cost_update(
            &mut entries,
            &p1,
            "root-a".into(),
            crate::cost::CostUpdate::Replace(Some(priced)),
        );
        apply_sub_cost_update(
            &mut entries,
            &p2,
            "root-a".into(),
            crate::cost::CostUpdate::Replace(Some(unpriced)),
        );
        assert_eq!(
            aggregate_sub_costs(&entries).get("root-a"),
            Some(&crate::cost::CostSummary {
                known_usd: Some(4.0),
                partial: true,
            })
        );

        apply_sub_cost_update(
            &mut entries,
            &p1,
            "root-b".into(),
            crate::cost::CostUpdate::Keep,
        );
        let moved = aggregate_sub_costs(&entries);
        assert_eq!(moved.get("root-b"), Some(&priced));
        assert_eq!(moved.get("root-a"), Some(&unpriced));

        apply_sub_cost_update(
            &mut entries,
            &p1,
            "root-b".into(),
            crate::cost::CostUpdate::Invalidate,
        );
        assert!(!entries.contains_key(&p1));
        assert_eq!(aggregate_sub_costs(&entries).get("root-a"), Some(&unpriced));

        apply_sub_cost_update(
            &mut entries,
            &p2,
            "root-a".into(),
            crate::cost::CostUpdate::Replace(None),
        );
        assert!(!entries.contains_key(&p2));
        assert!(aggregate_sub_costs(&entries).is_empty());
    }

    /// The rename migration rule (owner, 2026-08-22): an existing `claude-monitor` dir
    /// keeps being used — even when `agent-monitor` also exists (old data predates the
    /// rename; the warn covers the ambiguity) — and only a fresh install gets the new name.
    #[test]
    fn renamed_dir_prefers_existing_old_else_new() {
        let base = std::env::temp_dir().join(format!("cr-renamed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Neither exists: the NEW name (fresh install).
        assert_eq!(
            renamed_dir(&base, "claude-monitor", "agent-monitor"),
            base.join("agent-monitor")
        );
        // Old exists: keep using it.
        std::fs::create_dir_all(base.join("claude-monitor")).unwrap();
        assert_eq!(
            renamed_dir(&base, "claude-monitor", "agent-monitor"),
            base.join("claude-monitor")
        );
        // Both exist: old still wins (plus a once-per-process warning).
        std::fs::create_dir_all(base.join("agent-monitor")).unwrap();
        assert_eq!(
            renamed_dir(&base, "claude-monitor", "agent-monitor"),
            base.join("claude-monitor")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// #133 send liveness (the owner's constraints, pure): a finished session in a quiet
    /// project is sendable; a session with its OWN live process is refused (use tmux); a
    /// session whose PROJECT has any other live session is refused (would fork live work).
    #[test]
    fn send_is_refused_for_a_live_session_or_an_active_project() {
        let cwd_of = |s: &str| match s {
            "idle-a" | "live-b" | "idle-c" => Some("/proj".to_string()),
            "elsewhere" => Some("/other".to_string()),
            _ => None,
        };
        let link = |pid: Option<u32>| SendLink {
            pid,
            ..Default::default()
        };
        let mut live: HashMap<String, SendLink> = HashMap::new();
        live.insert("idle-a".into(), link(None));
        live.insert("idle-c".into(), link(None));
        live.insert("elsewhere".into(), link(Some(999))); // live, but a DIFFERENT project

        // A finished session in a quiet project: OK.
        assert!(send_liveness_ok("idle-a", Some("/proj"), &live, cwd_of, |_| None).is_ok());
        // The target itself is live → refuse (tmux path).
        live.insert("idle-a".into(), link(Some(123)));
        assert_eq!(
            send_liveness_ok("idle-a", Some("/proj"), &live, cwd_of, |_| None),
            Err(SendRefusal::SessionIsLive)
        );
        // Target finished, but a SIBLING in the same project is live → refuse.
        live.insert("idle-a".into(), link(None));
        live.insert("live-b".into(), link(Some(456)));
        assert_eq!(
            send_liveness_ok("idle-c", Some("/proj"), &live, cwd_of, |_| None),
            Err(SendRefusal::ProjectHasActiveSession)
        );
        // A live session in ANOTHER project does not block this one.
        live.remove("live-b");
        assert!(send_liveness_ok("idle-c", Some("/proj"), &live, cwd_of, |_| None).is_ok());
    }

    /// #133 tmux slice — the §3.1 refusal ladder (pure): only a live, PROVEN, in-tmux,
    /// claude/codex link resolves to an injectable target. Each failure is its OWN reason,
    /// and an UNPROVEN link (the cwd guess) is refused BEFORE its pane is ever revealed.
    #[test]
    fn tmux_injection_requires_a_live_proven_tmux_link() {
        let tmux = |confirmed: bool, in_tmux: bool| SendLink {
            pid: Some(777),
            tmux: in_tmux.then(|| (Some("knack".into()), "%3".into())),
            confirmed,
        };

        // The happy path: live + proven + in tmux + a driven agent.
        assert_eq!(
            tmux_target_from(Agent::CLAUDE, &tmux(true, true)),
            Ok((777, Some("knack".into()), "%3".into()))
        );
        // An agent with no verified drive shape is refused first.
        assert_eq!(
            tmux_target_from(Agent::QODERWORK, &tmux(true, true)),
            Err(SendRefusal::UnsupportedAgent)
        );
        // Not live (no pid) → resume territory, not injection.
        assert_eq!(
            tmux_target_from(Agent::CLAUDE, &SendLink::default()),
            Err(SendRefusal::SessionNotLive)
        );
        // Live but the link is a cwd GUESS → refuse (never inject on a heuristic).
        assert_eq!(
            tmux_target_from(Agent::CODEX, &tmux(false, true)),
            Err(SendRefusal::UnprovenLink)
        );
        // Live and proven, but not in tmux (a bare tty / screen) → nothing to inject into.
        assert_eq!(
            tmux_target_from(Agent::CLAUDE, &tmux(true, false)),
            Err(SendRefusal::NotInTmux)
        );
    }

    /// #133 constraint 2 (pure) — the sibling rule BOTH send paths share: a project is "busy"
    /// for the target when a DIFFERENT session in the same cwd is live. Used to refuse a
    /// resume (would fork live work) and to suppress an injection (can't tell which drives it).
    #[test]
    fn a_live_sibling_in_the_same_project_is_detected() {
        let cwd_of = |s: &str| match s {
            "a" | "b" => Some("/proj".to_string()),
            "far" => Some("/other".to_string()),
            _ => None,
        };
        let link = |pid: Option<u32>| SendLink {
            pid,
            ..Default::default()
        };
        let mut links: HashMap<String, SendLink> = HashMap::new();
        links.insert("a".into(), link(Some(1))); // the target, live (an inject candidate)
        links.insert("b".into(), link(None)); // a quiet sibling

        // Only the target is live in /proj → no other-live sibling.
        assert!(!project_has_other_live(
            "a",
            Some("/proj"),
            &links,
            cwd_of,
            |_| None
        ));
        // A second live session appears in the same project → other-live is true for BOTH,
        // so neither is injectable (ambiguous which drives the project — refuse, don't guess).
        links.insert("b".into(), link(Some(2)));
        assert!(project_has_other_live(
            "a",
            Some("/proj"),
            &links,
            cwd_of,
            |_| None
        ));
        assert!(project_has_other_live(
            "b",
            Some("/proj"),
            &links,
            cwd_of,
            |_| None
        ));
        // A live session in a DIFFERENT project does not count.
        links.insert("far".into(), link(Some(3)));
        links.insert("b".into(), link(None));
        assert!(!project_has_other_live(
            "a",
            Some("/proj"),
            &links,
            cwd_of,
            |_| None
        ));
        // A target with no cwd can have no project siblings.
        assert!(!project_has_other_live("a", None, &links, cwd_of, |_| None));
    }

    /// #133 resume shapes (verified against agent-jdi): claude resumes the SAME id with `-p`
    /// and skip-permissions; codex resumes by id; other agents have no shape.
    #[test]
    fn resume_command_shapes_match_the_agents() {
        let claude = SendTarget {
            sid: "sid-x".into(),
            agent: Agent::CLAUDE,
            cwd: None,
        };
        let (prog, args) = crate::control::resume_command(&claude, "do the thing").unwrap();
        assert_eq!(prog, "claude");
        assert!(args.windows(2).any(|w| w == ["--resume", "sid-x"]));
        assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert_eq!(args.last().unwrap(), "do the thing");
        let codex = SendTarget {
            agent: Agent::CODEX,
            ..claude
        };
        let (prog, args) = crate::control::resume_command(&codex, "go").unwrap();
        assert_eq!(prog, "codex");
        assert!(args.iter().any(|a| a == "resume") && args.contains(&"sid-x".to_string()));
    }

    /// #153, the regression that matters: an `Index` writes its hide list ONLY under the
    /// state directory it was CONSTRUCTED with — never one the ambient environment names.
    ///
    /// The bug this pins: `set_ignore` used to resolve `state_dir()` at save time, so the
    /// hide/unhide pair below (it is the shape of a real test in this very file) persisted an
    /// empty list over the developer's own `~/.local/state/…/ignored.json`. Reproduced from a
    /// clean machine: `HOME=<scratch> cargo test -p claude-monitor --lib` left exactly one
    /// file behind, `.local/state/agent-monitor/ignored.json`, two bytes, `[]`.
    ///
    /// The assertion is deliberately two-sided. Writing to the right place is half of it; the
    /// other half is that NOTHING lands anywhere else, which is what a captured path buys and
    /// an ambient one cannot.
    #[test]
    fn an_index_writes_its_hide_list_only_where_it_was_constructed() {
        let base = std::env::temp_dir().join(format!("cm-hide-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mine = base.join("mine");
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();

        // The environment points at `elsewhere` — standing in for the developer's real state
        // dir. The index is told `mine`. What the index was TOLD must win.
        let _env = StateEnv::set(&elsewhere);
        let idx = Index::new(base.join("cache"), mine.clone(), Vec::new());

        idx.set_ignore("p:/some/project", true);
        assert_eq!(
            std::fs::read_to_string(mine.join("ignored.json")).unwrap(),
            r#"["p:/some/project"]"#,
            "the hide goes to the constructed state dir"
        );
        assert!(
            !elsewhere.join("ignored.json").exists(),
            "and nothing at all reaches the ambient one"
        );

        // Unhiding back to empty is the exact sequence that wrote `[]` over a live list.
        idx.set_ignore("p:/some/project", false);
        assert_eq!(
            std::fs::read_to_string(mine.join("ignored.json")).unwrap(),
            "[]"
        );
        assert!(
            !elsewhere.join("ignored.json").exists(),
            "emptying the list still writes only where the index was told to"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// #197 migration (the case the owner asked for): a hide list that exists ONLY at the old
    /// cache location is adopted into the STATE dir, and the set survives from there alone — an
    /// existing user re-hides nothing. The old copy is left in place so a downgrade is safe.
    ///
    /// #154 moved this out of `Index::new` and into an explicit startup step, so the test now
    /// drives the function that actually migrates.
    #[test]
    fn the_hide_list_migrates_from_cache_to_state() {
        let d = std::env::temp_dir().join(format!("cm-hide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state_dir = d.join("state");
        let legacy = d.join("cache").join("ignored.json");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        // A user who hid a project and a session under the old (cache) location:
        std::fs::write(&legacy, r#"["p:/a","s:sid-1"]"#).unwrap();

        migrate_hide_list(&state_dir, std::slice::from_ref(&legacy));
        let want: BTreeSet<String> = ["p:/a".to_string(), "s:sid-1".to_string()]
            .into_iter()
            .collect();
        assert_eq!(
            read_ignored(&state_dir.join("ignored.json")).unwrap(),
            Some(want.clone()),
            "the cache-only list is adopted into the state dir"
        );
        assert!(legacy.exists(), "the cache copy is left for a downgrade");

        // It is ONCE. A state file that exists wins over every candidate, so a later
        // migration cannot resurrect entries the user has since removed.
        save_ignored(&state_dir.join("ignored.json"), &BTreeSet::new());
        migrate_hide_list(&state_dir, std::slice::from_ref(&legacy));
        assert_eq!(
            read_ignored(&state_dir.join("ignored.json")).unwrap(),
            Some(BTreeSet::new()),
            "an existing state file is never overwritten by a legacy copy"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #154: the bug. A second monitor under its OWN cache root shares the state dir but has no
    /// legacy list of its own — so a migration that only looked at that root concluded there was
    /// nothing to migrate, started empty, and its first hide-then-unhide persisted `[]` over the
    /// shared state path, losing a list it had never read.
    ///
    /// The fix is the second candidate: the machine's env-free default cache location, where the
    /// entries actually are. Passing the candidates in is what keeps this test hermetic — nothing
    /// here resolves `~/.cache`, exactly as #153 stopped anything resolving `~/.local/state`.
    #[test]
    fn a_second_monitor_migrates_from_the_machine_default_not_its_own_root() {
        let d = std::env::temp_dir().join(format!("cm-second-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state_dir = d.join("state");
        // The machine's real hide list, at the DEFAULT cache location.
        let default_cache = d.join("default-cache").join("ignored.json");
        std::fs::create_dir_all(default_cache.parent().unwrap()).unwrap();
        std::fs::write(&default_cache, r#"["p:/kept","s:sid-kept"]"#).unwrap();
        // This instance's own root — fresh, and therefore empty.
        let own = d.join("own-cache").join("ignored.json");
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();

        // THE BUG, stated first: with only its own root to look at, the migration finds nothing
        // and the state file stays absent — which is how an instance started empty and then
        // persisted that emptiness over a list it had never read.
        migrate_hide_list(&state_dir, std::slice::from_ref(&own));
        assert_eq!(
            read_ignored(&state_dir.join("ignored.json")).unwrap(),
            None,
            "its own fresh root has nothing to offer — this is the state the bug started from"
        );

        // THE FIX: the machine's env-free default is the second candidate.
        migrate_hide_list(&state_dir, &[own.clone(), default_cache]);
        let got = read_ignored(&state_dir.join("ignored.json")).unwrap();
        assert_eq!(
            got,
            Some(
                ["p:/kept".to_string(), "s:sid-kept".to_string()]
                    .into_iter()
                    .collect()
            ),
            "the second monitor finds the list where it actually lives"
        );

        // And the index built afterwards sees it — which is the whole point: the next hide or
        // unhide now writes a SUPERSET of what was there, not an empty list over the top of it.
        let idx = Index::new(d.join("own-cache"), state_dir.clone(), Vec::new());
        idx.set_ignore("p:/added", true);
        let after = read_ignored(&state_dir.join("ignored.json"))
            .unwrap()
            .unwrap();
        assert!(
            after.contains("p:/kept") && after.contains("s:sid-kept") && after.contains("p:/added"),
            "the migrated entries survive the next toggle: {after:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #197: a MALFORMED or unreadable hide list is reported and never silently read as
    /// "nothing hidden" — the difference `read_ignored` draws between absent (Ok(None)) and
    /// broken (Err). An absent file is the normal first run.
    #[test]
    fn a_broken_hide_list_is_an_error_not_an_empty_set() {
        let d = std::env::temp_dir().join(format!("cm-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let absent = d.join("absent.json");
        let broken = d.join("broken.json");
        std::fs::write(&broken, "{ this is not a json array").unwrap();

        assert!(
            matches!(read_ignored(&absent), Ok(None)),
            "absent is not an error"
        );
        assert!(
            read_ignored(&broken).is_err(),
            "malformed is an error, not empty"
        );
        // #154: a broken STATE file is never migrated OVER. Overwriting it with a legacy copy
        // would silently discard whatever it holds — the file is unreadable, not known-empty.
        let broken_state = d.join("broken-state");
        std::fs::create_dir_all(&broken_state).unwrap();
        std::fs::write(broken_state.join("ignored.json"), "{ not an array").unwrap();
        let good_legacy = d.join("good.json");
        std::fs::write(&good_legacy, r#"["p:/keep"]"#).unwrap();
        migrate_hide_list(&broken_state, std::slice::from_ref(&good_legacy));
        assert_eq!(
            std::fs::read_to_string(broken_state.join("ignored.json")).unwrap(),
            "{ not an array",
            "a broken state file is left exactly as it was, for a human to fix"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Format a `SystemTime` as the ISO-8601 UTC shape transcripts carry
    /// (`2026-08-08T10:00:00Z`) — the inverse of `time::epoch_secs` (civil_from_days).
    /// The fixtures need timestamps RELATIVE to real now: the activity clock is the
    /// content clock, and the two-bucket rule compares it against real time, so a
    /// hardcoded stamp ages out of `ACTIVE_WINDOW` and time-bombs the test.
    fn iso_utc(st: SystemTime) -> String {
        let e = st.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
        let (days, rem) = (e.div_euclid(86_400), e.rem_euclid(86_400));
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    /// One env-configured fixture run covering the index end to end — a SINGLE test because
    /// env vars are process-global and these assertions share a store. Asserts: a row is
    /// born from the card pass alone (unvisited, no counters — R7/§3); growth is a two-scan
    /// mtime diff, with the FIRST sighting never claiming growth; and a meta stream
    /// appearing at the monitor root turns the row visited with folded counters.
    #[test]
    fn index_end_to_end_on_a_fixture_store() {
        // The store env vars are process-global: hold the same lock every env-setting test
        // holds (ui, control, routes), or a concurrent test's scratch stores replace the
        // fixture mid-scan.
        let base = std::env::temp_dir().join(format!("cm-index-{}", std::process::id()));
        // #153: redirect the STATE dir too, not just the stores. The scan reads consent
        // grants (`consent_path()`), so without this the fixture's view of the world depended
        // on which projects this machine's owner had granted — and `set_ignore` below wrote
        // its hide list. The guard restores whatever the developer's environment had.
        let _env = StateEnv::set(base.join("state"));
        let _ = std::fs::remove_dir_all(&base);
        let store = base.join("projects");
        let proj = store.join("-tmp-fixture-repo");
        std::fs::create_dir_all(&proj).unwrap();
        std::env::set_var("CLAUDE_PROJECTS_DIR", &store);
        // Point the OTHER stores away from the real machine, so the fixture is the world.
        std::env::set_var("QODERWORK_PROJECTS_DIR", base.join("qw"));
        std::env::set_var("QWENWORK_PROJECTS_DIR", base.join("qwen"));
        std::env::set_var("QODER_PROJECTS_DIR", base.join("qoder"));
        std::env::set_var("CODEX_HOME", base.join("codex"));

        let sid = "11111111-2222-3333-4444-555555555555";
        let t = proj.join(format!("{sid}.jsonl"));
        // A realistic transcript CARRIES a timestamp — activity comes from that content
        // clock, not the file mtime (an attached idle client re-touches without appending).
        let line = |content: &str, ts: &str| {
            format!(
                "{{\"sessionId\":\"x\",\"type\":\"user\",\"cwd\":\"/tmp/fixture-repo\",\"timestamp\":\"{ts}\",\"message\":{{\"role\":\"user\",\"content\":\"{content}\"}}}}\n"
            )
        };
        let ts0 = iso_utc(SystemTime::now() - Duration::from_secs(7200));
        let ts1 = iso_utc(SystemTime::now() - Duration::from_secs(6900));
        std::fs::write(&t, line("build the thing", &ts0)).unwrap();

        let root = base.join("monitor-cache");
        // #153: the state dir is the fixture's own, never this machine's. The hide-list
        // assertions below toggle keys, and a toggle SAVES — under the ambient resolver this
        // very test wrote `[]` over the developer's real `~/.local/state/…/ignored.json`.
        let idx = Index::new(root.clone(), base.join("state"), Vec::new());
        let find = |json: &str| -> Value {
            let v: Value = serde_json::from_str(json).unwrap();
            v["groups"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["rows"].as_array().unwrap().clone())
                .find(|r| r["id"] == sid)
                .expect("fixture row present")
        };

        // First sighting: a row from the card pass alone — never growing, never visited.
        let r = find(&idx.sessions_json(|_| {}));
        assert_eq!(
            r["state"], "finished",
            "first sighting sets the baseline: {r}"
        );
        assert_eq!(r["visited"], false);
        assert!(r.get("turns").is_none(), "no counters without a visit (R7)");
        assert_eq!(
            r["agentState"], "idle",
            "the additive state contract comes from the shared tracker: {r}"
        );
        assert_eq!(r["stateReason"], "exited");
        assert_eq!(r["stateConfidence"], "observed");
        assert!(
            r.get("stateDetail").is_some() && r.get("stateSince").is_some(),
            "all agentState companion fields are present: {r}"
        );

        // Hiding re-derives the snapshot WITHOUT rescanning, and that second producer has to
        // carry the same enrichment as the scan. It did not, so one click stripped `agentState*`
        // off every row until the next scan happened to put them back.
        idx.set_ignore("a:nothing-matches-this", true);
        let r = find(&idx.sessions_json(|_| {}));
        assert_eq!(
            r["agentState"], "idle",
            "a hide must not strip the state contract: {r}"
        );
        assert_eq!(r["stateReason"], "exited");
        idx.set_ignore("a:nothing-matches-this", false);

        // A bare TOUCH — mtime moves, content clock does NOT — is not growth (the crux
        // bug: an attached idle client re-touches its weeks-old transcript).
        std::thread::sleep(Duration::from_millis(2100)); // past the scan floor
        std::fs::OpenOptions::new()
            .append(true)
            .open(&t)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        let r = find(&idx.sessions_json(|_| {}));
        assert_ne!(
            r["state"], "growing",
            "a touch with no new content is not growth: {r}"
        );
        // …and the reported activity is the CONTENT time, not "just now".
        assert_eq!(
            r["activityTs"].as_u64().unwrap(),
            crate::index::metrics::parse_ts(&ts0).unwrap() as u64,
            "activity tracks the last event, not the file mtime: {r}"
        );

        // Real growth: a new line with a LATER content timestamp.
        std::thread::sleep(Duration::from_millis(2100));
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
            f.write_all(line("more", &ts1).as_bytes()).unwrap();
            f.set_modified(SystemTime::now() + Duration::from_secs(4))
                .unwrap();
        }
        let r = find(&idx.sessions_json(|_| {}));
        assert_eq!(
            r["state"], "growing",
            "a later content clock IS growth: {r}"
        );

        // A visit: a meta stream at the monitor root turns the row visited, with counters
        // folded from the stream (§2 — no transcript read, no alignment).
        let entry = admit::entry_dir(&root, Presentation::Html, sid);
        std::fs::create_dir_all(&entry).unwrap();
        let stream = |fold: u16| {
            format!(
                "{{\"anchor\":1,\"versions\":{{\"format\":1,\"fold\":{fold}}}}}\n\
                 {{\"turns\":3,\"tools\":7}}\n\
                 {{\"turns\":2,\"tools\":1}}\n"
            )
        };
        // #144: a stream folded by ANOTHER version is refused — its numbers were produced by
        // logic this build no longer runs, and showing them as current is the bug.
        std::fs::write(
            entry.join("meta.jsonl"),
            stream(FOLD_VERSION.wrapping_add(1)),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(2100));
        let r = find(&idx.sessions_json(|_| {}));
        assert!(
            r.get("turns").is_none(),
            "a stale-fold stream yields no counters: {r}"
        );

        std::fs::write(entry.join("meta.jsonl"), stream(FOLD_VERSION)).unwrap();
        std::thread::sleep(Duration::from_millis(2100));
        let r = find(&idx.sessions_json(|_| {}));
        assert_eq!(r["visited"], true, "{r}");
        assert_eq!(r["turns"], 5, "counters are the folded deltas: {r}");
        assert_eq!(r["tools"], 8);

        // ── ordering is a pure function of the data ──────────────────────────────
        // Two sessions with IDENTICAL mtimes: their relative order must be the id
        // tiebreak, and two consecutive scans must serve the SAME order — the HashMap
        // iteration behind the rows must never leak (the reshuffling-rail bug).
        let same = SystemTime::now() - Duration::from_secs(3600);
        for tie in [
            "aaaaaaaa-0000-0000-0000-000000000001",
            "bbbbbbbb-0000-0000-0000-000000000002",
        ] {
            let p = proj.join(format!("{tie}.jsonl"));
            std::fs::write(
                &p,
                "{\"sessionId\":\"x\",\"type\":\"user\",\"cwd\":\"/tmp/fixture-repo\",\"message\":{\"role\":\"user\",\"content\":\"tied\"}}\n",
            )
            .unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&p)
                .unwrap()
                .set_modified(same)
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(2100));
        let order = |json: &str| -> Vec<String> {
            let v: Value = serde_json::from_str(json).unwrap();
            v["groups"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["rows"].as_array().unwrap().clone())
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect()
        };
        let first = order(&idx.sessions_json(|_| {}));
        std::thread::sleep(Duration::from_millis(2100));
        let second = order(&idx.sessions_json(|_| {}));
        assert_eq!(first, second, "order must not move when nothing changed");
        let a = first
            .iter()
            .position(|x| x.starts_with("aaaaaaaa"))
            .expect("tied row a listed");
        let b = first
            .iter()
            .position(|x| x.starts_with("bbbbbbbb"))
            .expect("tied row b listed");
        assert!(a < b, "identical mtimes break the tie by id: {first:?}");

        // ── the two-bucket rule (owner, 2026-08-08) ──────────────────────────────
        // An ACTIVE group (any activity within 10 min) sits above every stale one even
        // when the stale one wins alphabetically; and inside the active bucket order is
        // BY NAME, not by which mtime is newer — active items are deliberately tied.
        let mk = |slug: &str, sid: &str, when: SystemTime| {
            let d = store.join(slug);
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join(format!("{sid}.jsonl"));
            let cwd = format!("/tmp/{}", slug.trim_start_matches("-tmp-"));
            std::fs::write(
                &p,
                format!(
                    "{{\"sessionId\":\"x\",\"type\":\"user\",\"cwd\":\"{cwd}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n"
                ),
            )
            .unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&p)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        let now2 = SystemTime::now();
        mk(
            "-tmp-aaa-repo",
            "cccccccc-0000-0000-0000-000000000001",
            now2 - Duration::from_secs(7200),
        );
        mk(
            "-tmp-zzz-repo",
            "dddddddd-0000-0000-0000-000000000002",
            now2 + Duration::from_secs(5),
        );
        // Make the original fixture group active too — via the CONTENT clock (a bare
        // mtime touch cannot activate a transcript whose lines carry timestamps), and
        // deliberately OLDER than zzz's clock: the active bucket must STILL order
        // fixture before zzz, by name, not recency.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
            f.write_all(line("ping", &iso_utc(now2 - Duration::from_secs(60))).as_bytes())
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(2100));
        let v: Value = serde_json::from_str(&idx.sessions_json(|_| {})).unwrap();
        let labels: Vec<String> = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["label"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            labels,
            ["fixture-repo", "zzz-repo", "aaa-repo"],
            "active bucket (by name, mtime-blind) above stale (by recency)"
        );

        // ── Cost (§14): the ledger prices WITHOUT a visit, rolls sub-agents up, and sees
        // the archive ─────────────────────────────────────────────────────────────────
        // A Codex fixture store: a main rollout in the dated tree whose usage lands
        // BEFORE any model is named (the blank-model bucket — priced via the
        // accumulator's finish() attribution, #16); a sub-agent under it; that
        // sub-agent's OWN sub-agent retired into the flat archive (the roll-up must
        // chase the chain); and an archived MAIN session, which must be a row at all.
        let codex = base.join("codex");
        let dated = codex.join("sessions/2026/08/12");
        let archive = codex.join("archived_sessions");
        std::fs::create_dir_all(&dated).unwrap();
        std::fs::create_dir_all(&archive).unwrap();
        let meta_main = |id: &str| {
            format!(
                "{{\"timestamp\":\"2026-08-12T01:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/tmp/codex-repo\"}}}}\n"
            )
        };
        let meta_sub = |id: &str, parent: &str| {
            format!(
                "{{\"timestamp\":\"2026-08-12T01:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"cwd\":\"/tmp/codex-repo\",\"thread_source\":\"subagent\",\"parent_thread_id\":\"{parent}\"}}}}\n"
            )
        };
        let usage_1m = "{\"timestamp\":\"2026-08-12T01:00:01Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":1000000,\"cached_input_tokens\":0,\"output_tokens\":0}}}}\n";
        let named = "{\"timestamp\":\"2026-08-12T01:00:02Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6\"}}\n";
        let unpriced = "{\"timestamp\":\"2026-08-12T01:00:02Z\",\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-codex\"}}\n";
        let main_id = "eeeeeeee-0000-0000-0000-00000000000e";
        let sub1 = "eeeeeeee-1111-0000-0000-00000000000e";
        let sub2 = "eeeeeeee-2222-0000-0000-00000000000e";
        let sub3 = "eeeeeeee-3333-0000-0000-00000000000e";
        let arch_id = "ffffffff-0000-0000-0000-00000000000f";
        let unpriced_id = "99999999-0000-0000-0000-000000000009";
        let sub_only_root_id = "77777777-0000-0000-0000-000000000007";
        let sub_only_child_id = "77777777-1111-0000-0000-000000000007";
        // Main: usage FIRST, model named after — $4 only if the blank bucket is attributed,
        // $0 under the old per-model re-derivation.
        std::fs::write(
            dated.join(format!("rollout-2026-08-12T01-00-00-{main_id}.jsonl")),
            format!("{}{usage_1m}{named}", meta_main(main_id)),
        )
        .unwrap();
        std::fs::write(
            dated.join(format!("rollout-2026-08-12T01-00-10-{sub1}.jsonl")),
            format!("{}{named}{usage_1m}", meta_sub(sub1, main_id)),
        )
        .unwrap();
        std::fs::write(
            archive.join(format!("rollout-2026-08-12T01-00-20-{sub2}.jsonl")),
            format!("{}{named}{usage_1m}", meta_sub(sub2, sub1)),
        )
        .unwrap();
        // A wholly unpriced child contributes no invented dollars but makes the root and group
        // lower bounds. Dropping this flag would make the known $12 look complete.
        std::fs::write(
            archive.join(format!("rollout-2026-08-12T01-00-30-{sub3}.jsonl")),
            format!("{}{unpriced}{usage_1m}", meta_sub(sub3, main_id)),
        )
        .unwrap();
        std::fs::write(
            archive.join(format!("rollout-2026-08-12T01-01-00-{arch_id}.jsonl")),
            format!("{}{named}{usage_1m}", meta_main(arch_id)),
        )
        .unwrap();
        std::fs::write(
            archive.join(format!("rollout-2026-08-12T01-02-00-{unpriced_id}.jsonl")),
            format!(
                "{{\"timestamp\":\"2026-08-12T01:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{unpriced_id}\",\"cwd\":\"/tmp/unpriced-repo\"}}}}\n{unpriced}{usage_1m}"
            ),
        )
        .unwrap();
        std::fs::write(
            dated.join(format!(
                "rollout-2026-08-12T01-03-00-{sub_only_root_id}.jsonl"
            )),
            meta_main(sub_only_root_id),
        )
        .unwrap();
        std::fs::write(
            archive.join(format!(
                "rollout-2026-08-12T01-03-10-{sub_only_child_id}.jsonl"
            )),
            format!(
                "{}{named}{usage_1m}",
                meta_sub(sub_only_child_id, sub_only_root_id)
            ),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(2100));
        let v: Value = serde_json::from_str(&idx.sessions_json(|_| {})).unwrap();
        let row = |id: &str| -> Value {
            v["groups"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["rows"].as_array().unwrap().clone())
                .find(|r| r["id"].as_str().unwrap().ends_with(id))
                .unwrap_or_else(|| panic!("row for {id}"))
        };
        let main = row(main_id);
        assert_eq!(main["visited"], false, "never visited, yet priced: {main}");
        assert!(
            (main["cost"].as_f64().unwrap() - 12.0).abs() < 1e-9,
            "own $4 (blank bucket attributed) + two sub-agents chased to the root: {main}"
        );
        assert!(
            (main["costOwn"].as_f64().unwrap() - 4.0).abs() < 1e-9,
            "the priced root share is an explicit fact: {main}"
        );
        assert!(
            (main["costSubs"].as_f64().unwrap() - 8.0).abs() < 1e-9,
            "the known sub-agent share is named: {main}"
        );
        assert_eq!(
            main["costPartial"], true,
            "an unpriced child makes the root total a lower bound: {main}"
        );
        assert_eq!(
            main["costSubsPartial"], true,
            "the sub-agent subtotal carries its own partial state: {main}"
        );
        let sub_only_root = row(sub_only_root_id);
        assert!(
            (sub_only_root["cost"].as_f64().unwrap() - 4.0).abs() < 1e-9,
            "the priced child still rolls up to its root: {sub_only_root}"
        );
        assert!(
            (sub_only_root["costSubs"].as_f64().unwrap() - 4.0).abs() < 1e-9,
            "the total is entirely the child subtotal: {sub_only_root}"
        );
        assert!(
            sub_only_root.get("costOwn").is_none(),
            "a usage-free root must not acquire an own zero-cost component: {sub_only_root}"
        );
        let archived = row(arch_id);
        assert!(
            (archived["cost"].as_f64().unwrap() - 4.0).abs() < 1e-9,
            "an archived session is a row, and priced: {archived}"
        );
        let wholly_unpriced = row(unpriced_id);
        assert!(
            wholly_unpriced.get("cost").is_none(),
            "no guessed dollar figure is emitted: {wholly_unpriced}"
        );
        assert_eq!(
            wholly_unpriced["costPartial"], true,
            "a wholly unpriced row is explicit: {wholly_unpriced}"
        );
        let codex_group = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["label"] == "codex-repo")
            .expect("codex group");
        assert_eq!(
            codex_group["metaLine"], "≥$20.00 · 3",
            "the group keeps every known subtotal and marks the unpriced child"
        );
        assert_eq!(codex_group["costPartial"], true);
        let unpriced_group = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["label"] == "unpriced-repo")
            .expect("unpriced group");
        assert_eq!(unpriced_group["metaLine"], "unpriced · 1");
        assert!(unpriced_group.get("cost").is_none());
        assert_eq!(unpriced_group["costPartial"], true);

        let _ = std::fs::remove_dir_all(&base);
    }
    /// The session START (#129) is found even when the transcript opens with a wall of
    /// housekeeping — the real shape that motivated the escalating windows: 22
    /// `file-history-snapshot` lines of ~25 KiB each, first real line 424 KiB in. Covers
    /// both hazards: the 64 KiB pass must not return a snapshot's timestamp, and a real
    /// line STRADDLING a window boundary must not be read from its truncated half.
    #[test]
    fn session_start_survives_a_wall_of_housekeeping() {
        let d = std::env::temp_dir().join(format!("cm-start-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();

        // A real line whose first byte lands just before 64 KiB, so it spans the boundary.
        let straddle = d.join("straddle.jsonl");
        let pad = 64 * 1024 - 200;
        let filler = format!(
            "{{\"type\":\"file-history-snapshot\",\"timestamp\":\"2026-08-01T00:00:00Z\",\"pad\":\"{}\"}}\n",
            "x".repeat(pad)
        );
        std::fs::write(
            &straddle,
            format!(
                "{filler}{{\"type\":\"user\",\"timestamp\":\"2026-08-02T10:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"go\"}}}}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            first_event_ts(&straddle),
            metrics::parse_ts("2026-08-02T10:00:00Z").map(|s| s as u64),
            "a real line spanning the 64 KiB boundary resolves via the wide window, and \
             the snapshot's own timestamp is never mistaken for the start"
        );

        // Beyond even the wide window there is nothing to find — and saying so is correct
        // (the rail then shows no span rather than a wrong one).
        let far = d.join("far.jsonl");
        std::fs::write(&far, filler.repeat(45)).unwrap();
        assert_eq!(first_event_ts(&far), None, "housekeeping only: no start");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// A `file-history-snapshot` (git housekeeping Claude writes long after the last turn)
    /// must NOT count as activity — the kwire bug: last real turn 00:55, a snapshot at
    /// 21:55, and the rail read 11 h ago instead of the true ~32 h.
    #[test]
    fn housekeeping_lines_do_not_advance_the_activity_clock() {
        let d = std::env::temp_dir().join(format!("cm-ts-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        std::fs::write(
            &p,
            "{\"type\":\"user\",\"timestamp\":\"2026-08-06T16:55:00Z\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n\
             {\"type\":\"assistant\",\"timestamp\":\"2026-08-06T16:55:05Z\",\"message\":{\"role\":\"assistant\",\"content\":\"ok\"}}\n\
             {\"type\":\"file-history-snapshot\",\"timestamp\":\"2026-08-07T13:55:48Z\"}\n",
        )
        .unwrap();
        let got = last_event_ts(&p).expect("has activity");
        assert_eq!(
            got as i64,
            metrics::parse_ts("2026-08-06T16:55:05Z").unwrap(),
            "the last TURN wins, not the later snapshot"
        );
        // A file with ONLY housekeeping has no activity clock at all.
        std::fs::write(
            &p,
            "{\"type\":\"file-history-snapshot\",\"timestamp\":\"2026-08-07T13:55:48Z\"}\n",
        )
        .unwrap();
        assert!(last_event_ts(&p).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// #s68: Claude Code compacts a session left at its prompt about an hour after its last
    /// answer, and says so in a notice. None of that compaction is activity — counted, it set
    /// the row growing for a minute, published `busy`, and restarted the clock a stall is
    /// measured from — so the clock is the last activity before its boundary, even with the
    /// boundary far beyond the first window. A compaction that says no such thing is the agent
    /// carrying on, and counts as it always did.
    #[test]
    fn an_idle_compaction_does_not_advance_the_activity_clock() {
        let d = std::env::temp_dir().join(format!("cm-idle-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        let answer = "{\"type\":\"user\",\"timestamp\":\"2026-10-09T01:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"go\"}}\n\
             {\"type\":\"assistant\",\"timestamp\":\"2026-10-09T01:15:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"done\"}}\n";
        let compaction = format!(
            "{{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"timestamp\":\"2026-10-09T02:06:00Z\"}}\n\
             {{\"type\":\"user\",\"isCompactSummary\":true,\"timestamp\":\"2026-10-09T02:06:01Z\",\"message\":{{\"role\":\"user\",\"content\":\"continued\"}}}}\n\
             {{\"type\":\"attachment\",\"timestamp\":\"2026-10-09T02:06:01Z\",\"attachment\":{{\"type\":\"invoked_skills\",\"content\":\"{}\"}}}}\n\
             {{\"type\":\"attachment\",\"timestamp\":\"2026-10-09T02:06:01Z\",\"attachment\":{{\"type\":\"model\"}}}}\n",
            "x".repeat(150 * 1024)
        );
        let notice = "{\"type\":\"system\",\"subtype\":\"informational\",\"level\":\"notice\",\"content\":\"Compacted while idle, before the prompt cache expired\",\"timestamp\":\"2026-10-09T02:06:02Z\"}\n\
             {\"type\":\"last-prompt\",\"lastPrompt\":\"go\"}\n";
        let ts = |s: &str| metrics::parse_ts(s).unwrap() as u64;

        std::fs::write(&p, format!("{answer}{compaction}{notice}")).unwrap();
        assert_eq!(
            last_event_ts(&p),
            Some(ts("2026-10-09T01:15:00Z")),
            "compacted while idle: the clock is the answer's"
        );

        let prompt = "{\"type\":\"user\",\"timestamp\":\"2026-10-09T03:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"next\"}}\n";
        std::fs::write(&p, format!("{answer}{compaction}{notice}{prompt}")).unwrap();
        assert_eq!(
            last_event_ts(&p),
            Some(ts("2026-10-09T03:00:00Z")),
            "a prompt after it is activity again"
        );

        std::fs::write(&p, format!("{answer}{compaction}")).unwrap();
        assert_eq!(
            last_event_ts(&p),
            Some(ts("2026-10-09T02:06:01Z")),
            "a compaction that says nothing of idleness is the agent working"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The #112 parsers and link precedence, against fixture strings shaped exactly like
    /// the probe's verified sources — no live processes needed.
    #[test]
    fn probe_parsers_and_link_precedence() {
        let mut procs = parse_ps(
            "  101 /Users/x/.local/bin/claude --resume 99999999-1111-2222-3333-444444444444\n\
             102 /opt/homebrew/bin/codex exec\n\
             103 /Users/x/.local/bin/claude\n\
             104 bash -c cargo test claude\n",
        );
        assert_eq!(procs.len(), 4);
        assert_eq!(procs[0].exe_base, "claude");
        assert_eq!(
            procs[3].exe_base, "bash",
            "a tool shell is not an agent exe"
        );

        apply_tty(&mut procs, "  101 ttys006\n  102 ??\n  103 ttys009\n", 0);
        assert_eq!(procs[0].tty.as_deref(), Some("ttys006"));
        assert!(procs[1].tty.is_none(), "?? is detached");

        apply_env(
            &mut procs,
            "  101 claude TERM=xterm TMUX=/tmp/tmux-501/work,89,0 TMUX_PANE=%7 HOME=/Users/x\n\
             103 claude STY=1234.pts-0.host TERM=screen\n",
        );
        assert_eq!(procs[0].pane.as_deref(), Some("%7"));
        assert_eq!(procs[2].screen.as_deref(), Some("1234.pts-0.host"));

        apply_lsof(
            &mut procs,
            "p102\nfcwd\nn/Users/x/code/repo\nf12\nn/Users/x/.codex/sessions/2026/08/08/rollout-abc.jsonl\np103\nfcwd\nn/Users/x/other\n",
        );
        assert_eq!(procs[1].cwd.as_deref(), Some("/Users/x/code/repo"));
        assert_eq!(procs[1].open_jsonl.len(), 1, "the Codex rollout fd");
        assert!(procs[2].open_jsonl.is_empty());

        // Precedence 1: sid in argv — exact, tmux terminal from the env.
        let l = link(
            &procs,
            "99999999-1111-2222-3333-444444444444",
            Agent::CLAUDE,
            Path::new("/nope.jsonl"),
            None,
            false,
            &|_| true,
        )
        .expect("argv link");
        assert!(l.confirmed);
        assert_eq!(l.pid, 101);
        assert_eq!(l.terminal.target(), Some("%7"));
        assert_eq!(l.terminal.kind(), "tmux");
        assert!(
            matches!(&l.terminal, Terminal::Tmux { sock: Some(s), .. } if s == "work"),
            "a non-default socket names the server the pane id is scoped to"
        );

        // Precedence 2: the transcript held open — exact even with no argv id (Codex).
        let l = link(
            &procs,
            "rollout-abc",
            Agent::CODEX,
            Path::new("/Users/x/.codex/sessions/2026/08/08/rollout-abc.jsonl"),
            None,
            false,
            &|_| true,
        )
        .expect("fd link");
        assert!(l.confirmed);
        assert_eq!(l.pid, 102);
        assert_eq!(l.terminal.kind(), "detached", "?? tty, no multiplexer");

        // Precedence 3: cwd heuristic — unconfirmed, screen terminal; and pid 101 (which
        // carries ANOTHER session's uuid) must never heuristically claim this one.
        let l = link(
            &procs,
            "some-other-sid",
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            Some("/Users/x/other"),
            true,
            &|_| true,
        )
        .expect("cwd link");
        assert!(
            link(
                &procs,
                "some-other-sid",
                Agent::CLAUDE,
                Path::new("/n.jsonl"),
                Some("/Users/x/other"),
                false,
                &|_| true
            )
            .is_none(),
            "only the NEWEST session of a cwd may claim a process heuristically"
        );
        assert!(
            l.confirmed,
            "the owner's rule (#269): a lone agent process in the directory is paired, not hedged"
        );
        assert_eq!(l.pid, 103);
        assert_eq!(l.terminal.target(), Some("1234.pts-0.host"));
        // 102 (codex, no uuid in argv) heuristically matches its own cwd…
        let l = link(
            &procs,
            "zzz",
            Agent::CODEX,
            Path::new("/n.jsonl"),
            Some("/Users/x/code/repo"),
            true,
            &|_| true,
        )
        .expect("codex cwd heuristic");
        assert!(l.confirmed, "alone in its cwd (#269)");
        assert_eq!(l.pid, 102);
        // …while 101, which carries ANOTHER session's uuid, is excluded from the heuristic
        // pool entirely (the probe's UNCONFIRMED cross-check as a hard rule).
        assert!(session_ref(&procs[0].argv).is_some() && session_ref(&procs[1].argv).is_none());
    }

    /// #269 (a): a process that merely CONTAINS a session's uuid — a background job's
    /// scratchpad path — never links, and the agent that IS in the pane is found by the
    /// directory instead: alone there, so paired (the owner's rule), in tmux, injectable.
    #[test]
    fn a_script_carrying_the_uuid_never_links_and_the_pane_agent_is_paired() {
        let sid = "4d5c259b-466e-401c-a9e9-0ab8b3fb7316";
        let mut procs = parse_ps(&format!(
            "  14121 bash /tmp/claude-502/-Users-x-crux-web/{sid}/scratchpad/flakehunt.sh /tmp/o\n\
             21496 claude --dangerously-skip-permissions --resume\n"
        ));
        apply_tty(&mut procs, "  14121 ??\n  21496 ttys021\n", 0);
        apply_env(
            &mut procs,
            "  21496 claude TMUX=/private/tmp/tmux-502/crux-web-c0b3b1,5801,0 TMUX_PANE=%0\n",
        );
        apply_lsof(&mut procs, "p21496\nfcwd\nn/Users/x/code/crux-web\n");
        let cwd = Some("/Users/x/code/crux-web");
        let l = link(
            &procs,
            sid,
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            cwd,
            true,
            &|_| true,
        )
        .expect("link");
        assert_eq!(
            l.pid, 21496,
            "the agent in the pane, never the bash that names the uuid"
        );
        assert!(l.confirmed, "alone in its directory: paired");
        assert_eq!(l.terminal.kind(), "tmux");
        assert_eq!(l.terminal.target(), Some("%0"));
        // And when this session is not the directory's newest, nothing links — certainly not
        // the script, which is what `argv.contains(sid)` used to hand back as "confirmed".
        assert!(link(
            &procs,
            sid,
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            cwd,
            false,
            &|_| true
        )
        .is_none());
    }

    /// #269 (b)(c)(d): `claude attach <8 chars>` names the session by a prefix — linked when
    /// exactly one known session starts with it, refused when two do, and a hex run inside an
    /// unrelated token is not a name at all.
    #[test]
    fn an_attach_prefix_links_only_when_it_is_unambiguous() {
        let sid = "f7e03d40-e8c1-4fcb-a60c-31b68cc21817";
        let mut procs =
            parse_ps("  20277 claude attach f7e03d40\n  24423 claude attach 6a22e5fb\n");
        apply_tty(&mut procs, "  20277 ttys018\n  24423 ttys023\n", 0);
        apply_env(
            &mut procs,
            "  20277 claude TMUX=/private/tmp/tmux-502/claude-replay-d36797,5407,0 TMUX_PANE=%0\n\
             24423 claude TMUX=/private/tmp/tmux-502/mdviewer-28920a,6467,0 TMUX_PANE=%0\n",
        );
        let l = link(
            &procs,
            sid,
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            None,
            false,
            &|pf| pf == "f7e03d40",
        )
        .expect("prefix link");
        assert!(l.confirmed);
        assert_eq!(l.pid, 20277);
        assert!(
            matches!(&l.terminal, Terminal::Tmux { sock: Some(s), .. } if s == "claude-replay-d36797")
        );
        // (c) two known sessions share the prefix: a pick, refused.
        assert!(link(
            &procs,
            sid,
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            None,
            false,
            &|_| false
        )
        .is_none());
        // (d) the shapes, one by one.
        assert_eq!(
            session_ref("claude attach f7e03d40"),
            Some(SessionRef::Prefix("f7e03d40".into()))
        );
        assert_eq!(
            session_ref(&format!("claude attach {sid}")),
            Some(SessionRef::Exact(sid.into()))
        );
        assert_eq!(
            session_ref(&format!("claude --resume {sid}")),
            Some(SessionRef::Exact(sid.into()))
        );
        assert_eq!(
            session_ref(&format!(
                "claude --resume /Users/x/.claude/projects/p/{sid}.jsonl"
            )),
            Some(SessionRef::Exact(sid.into())),
            "a path names the session its stem is"
        );
        assert_eq!(
            session_ref(&format!(
                "claude --bg-pty-host /tmp/cc/pty/f7e03d40.sock 173 46 -- /bin --session-id {sid} --fork-session --resume /p/99999999-1111-2222-3333-444444444444.jsonl"
            )),
            Some(SessionRef::Exact(sid.into())),
            "--session-id wins; under --fork-session the --resume is the PARENT"
        );
        assert_eq!(
            session_ref("claude --fork-session --resume 99999999-1111-2222-3333-444444444444"),
            None,
            "a fork with no --session-id has an id the argv cannot name"
        );
        for picker in [
            "claude --dangerously-skip-permissions --resume",
            "claude --resume",
            "claude --resume --verbose",
            "claude",
            "claude --resume /tmp/f7e03d40-notes.txt",
        ] {
            assert_eq!(session_ref(picker), None, "{picker:?} names nothing");
        }
    }

    /// #269 (e)(f) and the helpers: one agent process in a directory is paired; two are a
    /// pick; a `bg-spare` or the daemon in that directory is neither — and the daemon's
    /// inherited TMUX_PANE is not a pane a session can be reached through.
    #[test]
    fn a_lone_agent_is_paired_and_daemon_helpers_never_count() {
        let cwd = Some("/Users/x/proj");
        let lone = |ps: &str, lsof: &str| {
            let mut procs = parse_ps(ps);
            apply_lsof(&mut procs, lsof);
            procs
        };
        // (e) one process, no id anywhere: paired.
        let procs = lone("  900 claude\n", "p900\nfcwd\nn/Users/x/proj\n");
        let l = link(
            &procs,
            "s",
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            cwd,
            true,
            &|_| true,
        )
        .expect("lone");
        assert!(l.confirmed && l.pid == 900);
        // (f) two: a pick, unconfirmed (the case the knack test pins from the other side).
        let procs = lone(
            "  700 claude\n  900 claude\n",
            "p700\nfcwd\nn/Users/x/proj\np900\nfcwd\nn/Users/x/proj\n",
        );
        assert!(
            !link(
                &procs,
                "s",
                Agent::CLAUDE,
                Path::new("/n.jsonl"),
                cwd,
                true,
                &|_| true
            )
            .unwrap()
            .confirmed
        );
        // A pre-warmed spare pty in the same directory does not make it two.
        let mut procs = lone(
            "  900 claude\n  19654 claude bg-spare --bg-spare /tmp/cc/spare/d3acb794.claim.sock\n",
            "p900\nfcwd\nn/Users/x/proj\np19654\nfcwd\nn/Users/x/proj\n",
        );
        apply_tty(&mut procs, "  900 ??\n  19654 ttys034\n", 0);
        let l = link(
            &procs,
            "s",
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            cwd,
            true,
            &|_| true,
        )
        .expect("lone");
        assert!(
            l.confirmed && l.pid == 900,
            "the spare is a helper, not a candidate"
        );
        // The daemon inherited the client's pane (measured); it is never a link, and the engine
        // that names a session links DETACHED — liveness, not a target.
        let sid = "39c9c62e-e288-406a-923e-424ff29d6abd";
        let mut procs = lone(
            &format!(
                "  52638 /Users/x/.local/bin/claude daemon run --origin transient --spawned-by {{\"pid\":17630}}\n\
                 15974 /Users/x/ClaudeCode.app/Contents/MacOS/claude --bg-pty-host /tmp/cc/pty/39c9c62e.sock 173 46 -- /bin --session-id {sid} --name knack\n"
            ),
            "p52638\nfcwd\nn/Users/x/proj\np15974\nfcwd\nn/Users/x/proj\n",
        );
        apply_env(
            &mut procs,
            "  52638 claude TMUX=/private/tmp/tmux-502/knack-98db47,5228,0 TMUX_PANE=%0\n",
        );
        assert!(is_helper(&procs[0].argv) && is_helper(&procs[1].argv));
        let l = link(
            &procs,
            sid,
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            cwd,
            true,
            &|_| true,
        )
        .expect("engine");
        assert_eq!(l.pid, 15974);
        assert!(l.confirmed, "the engine names its session exactly");
        assert_eq!(
            l.terminal.kind(),
            "detached",
            "an engine is where a session runs, not where it is typed into"
        );
        assert!(
            link(
                &procs,
                "other",
                Agent::CLAUDE,
                Path::new("/n.jsonl"),
                cwd,
                true,
                &|_| true
            )
            .is_none(),
            "neither helper is a directory candidate for any other session"
        );
    }

    /// #269: a fork's engine names its parent, and that edge reaches the row even though the
    /// transcript probe already answered `None` — so `knack deve (2)` folds under `knack deve`.
    #[test]
    fn a_fork_joins_its_family_from_the_engines_argv() {
        let child = "39c9c62e-e288-406a-923e-424ff29d6abd";
        let parent = "b0bb9596-4060-4a9b-9834-a2bc736d2f6c";
        let engine = format!(
            "claude --bg-pty-host /tmp/cc/pty/39c9c62e.sock 173 46 -- /bin --session-id {child} --fork-session --resume /Users/x/.claude/projects/p/{parent}.jsonl --name knack"
        );
        assert_eq!(
            fork_parent_from_argv(&engine),
            Some((child.to_string(), parent.to_string()))
        );
        assert_eq!(
            fork_parent_from_argv(&format!("claude --resume {child}")),
            None
        );
        assert_eq!(
            fork_parent_from_argv(&format!(
                "claude --session-id {child} --resume /p/{parent}.jsonl"
            )),
            None,
            "a resume that is not a fork names no parent"
        );

        let scratch = std::env::temp_dir().join(format!("cm-fork-{}", std::process::id()));
        let idx = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let mut st = State {
            procs: parse_ps(&format!("  15974 {engine}\n")),
            ..State::default()
        };
        st.rows
            .insert(child.into(), growth_row("/Users/x/knack", false));
        st.rows
            .insert(parent.into(), growth_row("/Users/x/knack", false));
        assert!(st.rows[child].fork_probed && st.rows[child].fork_from.is_none());
        idx.note_forks_from_argv(&mut st);
        assert_eq!(st.rows[child].fork_from.as_deref(), Some(parent));
        assert!(st.rows[parent].fork_from.is_none());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// #146: growth is the one signal that says WHICH session a no-id agent is driving —
    /// a transcript advances only because its own agent wrote to it. Strict on purpose: a
    /// forced pairing needs exactly one grower AND exactly one candidate, and the record is
    /// dropped the moment the process is gone.
    fn growth_row(cwd: &str, growing: bool) -> Row {
        Row {
            path: PathBuf::from("/tmp/x.jsonl"),
            agent: Agent::CLAUDE,
            cwd: Some(cwd.to_string()),
            // This helper exercises cwd-based process matching, not repo grouping; the group
            // path falls back to `cwd` when `repo` is None, so the rows still group by `cwd`.
            repo: None,
            title: "t".into(),
            tree_mtime: None,
            last_event: None,
            first_event: None,
            start_probed: true,
            one_shot: false,
            brief: None,
            fork_from: None,
            fork_probed: true,
            spawned_by: None,
            spawn_probed: true,
            proved_pid: None,
            grew_at: growing.then(Instant::now),
            counters: None,
            title_mtime: None,
            cost: None,
        }
    }

    /// Sessions in different SUBDIRS of one repo collapse into a single project group
    /// (keyed by `repo`), while a session under no repo stands alone (fallback to `cwd`). The
    /// grouping keys off `repo`; `cwd` is untouched, so the process matching that keys off it
    /// (siblings/proved_pid) is unaffected.
    #[test]
    fn subdir_sessions_group_under_their_repo() {
        let scratch = std::env::temp_dir().join(format!("cm-group-{}", std::process::id()));
        // `assemble` reads the consent store, which resolves `state_dir()` — so this test must
        // pin it to a scratch (#153), not lean on whatever OTHER concurrent test happened to have
        // the env set. It was the one assemble-caller without the guard, and it flaked under a
        // parallel run whose scheduling left no sibling guard live at the moment it called through.
        let _env = StateEnv::set(scratch.join("state"));
        let idx = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let mut st = State::default();
        let mut a = growth_row("/repo/crate-a", false);
        a.repo = Some("/repo".into());
        let mut b = growth_row("/repo/crate-b", false);
        b.repo = Some("/repo".into());
        let solo = growth_row("/loose", false); // repo: None ⇒ falls back to cwd
        st.rows.insert("a".into(), a);
        st.rows.insert("b".into(), b);
        st.rows.insert("solo".into(), solo);

        let v: Value = idx.assemble(&st, &mut Vec::new());
        let groups = v["groups"].as_array().unwrap();

        let repo = groups
            .iter()
            .find(|g| g["ignoreKey"] == "p:/repo")
            .expect("the two subdir sessions share one repo group");
        assert_eq!(repo["total"].as_u64(), Some(2));
        let ids: Vec<&str> = repo["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"a") && ids.contains(&"b"));
        // A subdir never forms its own group…
        assert!(groups.iter().all(|g| g["ignoreKey"] != "p:/repo/crate-a"));
        // …and a repo-less session still stands alone under its own cwd.
        assert!(groups.iter().any(|g| g["ignoreKey"] == "p:/loose"));
    }

    /// #373: a session started in another session's scratch — knack's `claude -p` in a worktree
    /// under its scratchpad — is listed in THAT session's project group (the owner: "group them
    /// under knack"), not as a project of its own, also two levels down; a session whose recorded
    /// owner this index does not know keeps its own group.
    #[test]
    fn a_session_started_in_another_s_scratch_groups_under_its_project() {
        let scratch = std::env::temp_dir().join(format!("cm-scratch-group-{}", std::process::id()));
        let _env = StateEnv::set(scratch.join("state"));
        let idx = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let mut st = State::default();
        let parent = "96b453d7-0d7b-4e63-af27-4f7e0ef030eb";
        let mut knack = growth_row("/w/knack", false);
        knack.repo = Some("/w/knack".into());
        let wt = format!("/private/tmp/claude-502/-w-knack/{parent}/scratchpad/wt-240");
        let mut child = growth_row(&wt, false);
        child.repo = Some(wt.clone()); // a worktree is a git root of its own
        child.spawned_by = Some(parent.into());
        let child_id = "2eecdbdc-4a29-42ae-9177-ba338c8c34ab";
        let wt2 = format!("/private/tmp/claude-502/-w-knack/{child_id}/scratchpad/wt-ci");
        let mut grandchild = growth_row(&wt2, false);
        grandchild.repo = Some(wt2.clone());
        grandchild.spawned_by = Some(child_id.into());
        let mut orphan = growth_row(
            "/private/tmp/claude-502/-w-x/aaaaaaaa-0000-4000-8000-000000000000/scratchpad/wt-9",
            false,
        );
        orphan.spawned_by = Some("aaaaaaaa-0000-4000-8000-000000000000".into()); // no such session here
        st.rows.insert(parent.into(), knack);
        st.rows.insert(child_id.into(), child);
        st.rows.insert("grandchild".into(), grandchild);
        st.rows.insert("orphan".into(), orphan);

        let v: Value = idx.assemble(&st, &mut Vec::new());
        let groups = v["groups"].as_array().unwrap();
        let knack = groups
            .iter()
            .find(|g| g["ignoreKey"] == "p:/w/knack")
            .expect("the knack group");
        let ids: Vec<&str> = knack["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert!(
            ids.contains(&parent) && ids.contains(&child_id) && ids.contains(&"grandchild"),
            "the worktree sessions sit in the knack group, two levels down too: {ids:?}"
        );
        assert!(
            groups.iter().all(
                |g| !g["ignoreKey"].as_str().unwrap_or("").contains("wt-240")
                    && !g["ignoreKey"].as_str().unwrap_or("").contains("wt-ci")
            ),
            "no worktree forms a project group: {:?}",
            groups.iter().map(|g| &g["ignoreKey"]).collect::<Vec<_>>()
        );
        assert!(
            groups.iter().any(|g| g["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == "orphan")
                && g["ignoreKey"] != "p:/w/knack"),
            "an owner this index does not know leaves the row where its own cwd puts it"
        );
    }

    #[test]
    fn growth_proves_which_session_an_agent_is_driving() {
        let scratch = std::env::temp_dir().join(format!("cm-proof-{}", std::process::id()));
        let idx = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let cwd = "/Users/x/proj";
        let procs = |spec: &str, lsof: &str| {
            let mut p = parse_ps(spec);
            apply_lsof(&mut p, lsof);
            p
        };

        // One grower, one candidate — the pairing is forced.
        let mut st = State {
            procs: procs(
                "  900 claude
",
                "p900
fcwd
n/Users/x/proj
",
            ),
            ..State::default()
        };
        st.rows.insert("live".into(), growth_row(cwd, true));
        st.rows.insert("old".into(), growth_row(cwd, false));
        idx.prove_by_growth(&mut st);
        assert_eq!(
            st.rows["live"].proved_pid.as_ref().map(|(p, _)| *p),
            Some(900),
            "the growing session is the one being written"
        );
        assert!(
            st.rows["old"].proved_pid.is_none(),
            "the quiet one proves nothing"
        );

        // The proof OUTLIVES the growth that established it — that is the point.
        st.rows.get_mut("live").unwrap().grew_at = None;
        idx.prove_by_growth(&mut st);
        assert!(
            st.rows["live"].proved_pid.is_some(),
            "banked, not recomputed"
        );

        // …but not the process. A dead pid takes its proof with it.
        st.procs.clear();
        idx.prove_by_growth(&mut st);
        assert!(st.rows["live"].proved_pid.is_none(), "no process, no proof");

        // Two growers in one directory force nothing.
        let mut st2 = State {
            procs: procs(
                "  900 claude
",
                "p900
fcwd
n/Users/x/proj
",
            ),
            ..State::default()
        };
        st2.rows.insert("a".into(), growth_row(cwd, true));
        st2.rows.insert("b".into(), growth_row(cwd, true));
        idx.prove_by_growth(&mut st2);
        assert!(
            st2.rows["a"].proved_pid.is_none() && st2.rows["b"].proved_pid.is_none(),
            "ambiguous growth is not evidence"
        );

        // Neither do two candidate processes.
        let mut st3 = State {
            procs: procs(
                "  900 claude
  901 claude
",
                "p900
fcwd
n/Users/x/proj
p901
fcwd
n/Users/x/proj
",
            ),
            ..State::default()
        };
        st3.rows.insert("live".into(), growth_row(cwd, true));
        idx.prove_by_growth(&mut st3);
        assert!(
            st3.rows["live"].proved_pid.is_none(),
            "which process wrote it?"
        );
    }

    /// The knack bug: a session really hosted in a `tmux -L knack` pane reported "detached"
    /// because an OLDER agent process sharing its cwd matched the heuristic first. Among
    /// equally-eligible candidates the one with a real terminal wins.
    #[test]
    fn the_cwd_heuristic_prefers_a_terminal_hosted_process() {
        let mut procs = parse_ps("  700 claude\n  900 claude\n");
        apply_lsof(
            &mut procs,
            "p700\nfcwd\nn/Users/hong/code/knack\np900\nfcwd\nn/Users/hong/code/knack\n",
        );
        // The older pid is detached; the newer one is the pane the user is sitting in.
        apply_env(
            &mut procs,
            "  900 claude TMUX=/private/tmp/tmux-502/knack-98db47,8436,0 TMUX_PANE=%0\n",
        );

        let l = link(
            &procs,
            "sid",
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            Some("/Users/hong/code/knack"),
            true,
            &|_| true,
        )
        .expect("cwd link");
        assert_eq!(l.pid, 900, "the tmux-hosted process, not the first match");
        assert!(
            !l.confirmed,
            "two agent processes in one directory: a pick, not a proof (#269)"
        );
        assert_eq!(l.terminal.kind(), "tmux");
        assert_eq!(l.terminal.target(), Some("%0"));
        assert!(
            matches!(&l.terminal, Terminal::Tmux { sock: Some(s), .. } if s == "knack-98db47"),
            "a per-project tmux server names the socket its pane id is scoped to"
        );

        // With no terminal anywhere the choice must still be deterministic, not ps order.
        let mut bare = parse_ps("  900 claude\n  700 claude\n");
        apply_lsof(
            &mut bare,
            "p700\nfcwd\nn/Users/hong/code/knack\np900\nfcwd\nn/Users/hong/code/knack\n",
        );
        let l = link(
            &bare,
            "sid",
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            Some("/Users/hong/code/knack"),
            true,
            &|_| true,
        )
        .expect("cwd link");
        assert_eq!(
            l.pid, 700,
            "ties break on pid, whatever order ps listed them"
        );
        assert!(!l.confirmed, "still two candidates");
    }

    #[test]
    fn is_agent_exe_recognizes_the_builtin_basenames_only() {
        assert!(is_agent_exe("claude", "claude"));
        assert!(is_agent_exe("codex", "codex --model gpt-5"));
        assert!(is_agent_exe("qoderwork", "qoderwork"));
        assert!(is_agent_exe("qoder", "qoder"));
        assert!(is_agent_exe(
            "QwenWorkCN",
            "/Applications/QwenWorkCN.app/Contents/MacOS/QwenWorkCN"
        ));
        assert!(is_agent_exe("qwenwork", "qwenwork"));
        // A wrapper is NOT an agent without a configured pattern: the interpreter's basename
        // says nothing, and matching its command line by default would claim every `node`.
        assert!(!is_agent_exe("npx", "npx codex --model gpt-5"));
        assert!(!is_agent_exe("node", "node ./node_modules/.bin/codex"));
    }

    #[test]
    fn agent_patterns_parse_by_kind_and_default_to_basename() {
        // The parsed form is tested directly rather than through $CLAUDE_MONITOR_AGENT_PATTERNS:
        // the variable is read once per process, and a test that mutates the environment would
        // race every other test in the binary for a value none of them can restore in time.
        let pats = AgentPattern::parse("argv:npx codex, basename:my-agent ,my-other-agent, ,");
        assert_eq!(pats.len(), 3, "empty entries are skipped");

        // argv: sees the wrapper's command line, whatever the interpreter is called.
        assert!(pats[0].matches("npx", "npx codex --model gpt-5"));
        assert!(!pats[0].matches("npx", "npx tsc"));

        // basename: stays out of the command line entirely.
        assert!(pats[1].matches("MY-AGENT", "anything"), "case-insensitive");
        assert!(!pats[1].matches("bash", "bash /usr/local/bin/my-agent"));

        // A bare entry is a basename, not a substring of argv.
        assert!(pats[2].matches("my-other-agent", ""));
        assert!(!pats[2].matches("bash", "bash /usr/local/bin/my-other-agent"));
    }

    /// The process table's start time is `now` less `ps`'s elapsed time (#s50).
    #[test]
    fn a_process_start_is_now_less_its_elapsed_time() {
        let mut procs = parse_ps("  7 claude -p hi\n");
        apply_tty(&mut procs, "  7 ?? 01:40\n", 1_000);
        assert_eq!(procs[0].started, Some(900));
        assert!(procs[0].tty.is_none());
    }

    /// #s50: a one-shot carries its PROMPT in argv, so a session id the prompt mentions never
    /// names it; its flags still do.
    #[test]
    fn a_one_shot_is_named_by_its_flags_never_by_its_prompt() {
        let sid = "19fe604e-d16f-4bbe-adfc-c6ff07463983";
        let brief =
            format!("claude -p Review the run of {sid} and report --output-format stream-json");
        assert!(is_one_shot(&brief));
        assert_eq!(session_ref(&brief), None);
        let resumed = format!("claude -p next --resume {sid} --output-format stream-json");
        assert_eq!(session_ref(&resumed), Some(SessionRef::Exact(sid.into())));
        assert!(!is_one_shot(
            "claude --dangerously-skip-permissions --resume"
        ));
        // Linked by its flag, a resumed one-shot is still headless: never the pane it inherited.
        let mut procs = parse_ps(&format!("  401 {resumed}\n"));
        apply_env(
            &mut procs,
            "  401 claude TMUX=/private/tmp/tmux-501/default,88,0 TMUX_PANE=%0\n",
        );
        let l = link(
            &procs,
            sid,
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            None,
            false,
            &|_| true,
        )
        .expect("link");
        assert!(l.confirmed);
        assert_eq!(l.terminal.kind(), "detached");
    }

    /// #s50: two one-shots started inside each other's window each take a session — both are
    /// alive — but neither pairing is confirmed.
    #[test]
    fn one_shots_started_together_pair_unconfirmed() {
        let mut procs = parse_ps("  301 claude -p a\n  302 claude -p b\n  303 claude -p c\n");
        apply_tty(
            &mut procs,
            "  301 ?? 10:00\n  302 ?? 09:58\n  303 ?? 01:00\n",
            10_000,
        );
        apply_lsof(
            &mut procs,
            "p301\nfcwd\nn/w\np302\nfcwd\nn/w\np303\nfcwd\nn/w\n",
        );
        let pairs = pair_one_shots(
            &procs,
            [
                ("s1", "/w", 9_401),
                ("s2", "/w", 9_403),
                ("s3", "/w", 9_941),
            ],
        );
        assert_eq!(pairs["s1"], (301, 1));
        assert_eq!(pairs["s2"], (302, 1));
        assert_eq!(pairs["s3"], (303, 0), "alone in its window: confirmed");
        // Two workers started fourteen seconds apart (measured here, 2026-10-08): the second's
        // session lies in the first's window, but the first holds its own earlier session, which
        // the second could never have begun. No swap, so both are confirmed.
        let mut apart = parse_ps("  501 claude -p a\n  502 claude -p b\n");
        apply_tty(&mut apart, "  501 ?? 01:00\n  502 ?? 00:46\n", 2_060);
        apply_lsof(&mut apart, "p501\nfcwd\nn/w\np502\nfcwd\nn/w\n");
        let pairs = pair_one_shots(&apart, [("a", "/w", 2_001), ("b", "/w", 2_015)]);
        assert_eq!(pairs["a"], (501, 0));
        assert_eq!(pairs["b"], (502, 0));
        // A session that began long before any live one-shot is nobody's.
        let none = pair_one_shots(&procs, [("old", "/w", 2_000)]);
        assert!(none.is_empty());
    }

    /// #s50, as aries-black showed it: a coordinator and two `claude -p` workers in one repo, both
    /// workers carrying the coordinator's `TMUX_PANE`. The first worker, deep in a long Bash, is
    /// not the directory's newest session; the second wrote last. Each worker keeps its OWN
    /// process, the coordinator keeps its pane (alone among the directory's candidates, so
    /// confirmed), a finished worker session stays finished, and no worker is offered the pane.
    #[test]
    fn one_shot_workers_pair_with_their_sessions_and_leave_the_coordinator_its_pane() {
        let scratch = std::env::temp_dir().join(format!("cm-one-shot-{}", std::process::id()));
        let _env = StateEnv::set(scratch.join("state"));
        let idx = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let now = 1_791_400_000u64;
        let mut procs = parse_ps(
            "  100 claude --dangerously-skip-permissions --resume\n\
             201 claude -p You are a builder on B74 --permission-mode bypassPermissions --output-format stream-json --verbose\n\
             202 claude -p You are a reviewer on B71 --permission-mode bypassPermissions --output-format stream-json --verbose\n",
        );
        apply_tty(
            &mut procs,
            "  100 ttys003 02:00:00\n  201 ?? 10:00\n  202 ?? 02:00\n",
            now,
        );
        let pane = "TMUX=/private/tmp/tmux-501/default,88,0 TMUX_PANE=%0";
        apply_env(
            &mut procs,
            &format!("  100 claude {pane}\n  201 claude {pane}\n  202 claude {pane}\n"),
        );
        apply_lsof(
            &mut procs,
            "p100\nfcwd\nn/w/knack\np201\nfcwd\nn/w/knack\np202\nfcwd\nn/w/knack\n",
        );
        let mut st = State {
            procs,
            ..Default::default()
        };
        let at = |secs_ago: u64| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(now - secs_ago));
        let row = |first_ago: u64, wrote_ago: u64, one_shot: bool| {
            let mut r = growth_row("/w/knack", false);
            r.first_event = Some(now - first_ago);
            r.tree_mtime = at(wrote_ago);
            r.last_event = Some(now - wrote_ago);
            r.one_shot = one_shot;
            r
        };
        st.rows.insert("coord".into(), row(7_200, 300, false));
        st.rows.insert("w1".into(), row(599, 240, true)); // mid-Bash: quiet for four minutes
        st.rows.insert("w2".into(), row(119, 5, true)); // the directory's newest
        st.rows.insert("w0".into(), row(5_000, 4_000, true)); // an earlier worker, finished
        let mut facts = Vec::new();
        idx.assemble(&st, &mut facts);
        let fact = |sid: &str| facts.iter().find(|f| f.sid == sid).expect("a row");
        assert_eq!(
            fact("w1").pid,
            Some(201),
            "the quiet worker keeps its own process"
        );
        assert_eq!(fact("w2").pid, Some(202));
        assert!(fact("w1").confirmed && fact("w2").confirmed);
        assert_eq!(fact("w0").pid, None, "a finished worker stays finished");
        assert_eq!(
            fact("coord").pid,
            Some(100),
            "the coordinator keeps its process"
        );
        assert!(
            fact("coord").confirmed,
            "alone among the directory's candidates"
        );
        assert_eq!(
            fact("coord").tmux.as_ref().map(|t| t.1.as_str()),
            Some("%0")
        );
        for w in ["w1", "w2"] {
            assert!(
                fact(w).tmux.is_none() && fact(w).term.is_none(),
                "a worker is never offered the pane it inherited"
            );
        }
    }

    /// #s53: the starter is read from the environment only when Claude Code marked the process a
    /// child session; a stray `CLAUDE_CODE_SESSION_ID` alone, or a malformed one, names nobody.
    #[test]
    fn a_process_names_its_starter_only_as_a_marked_child() {
        let parent = "491ffb04-574b-4635-bad7-74d06fa212c6";
        let mut procs = parse_ps("  1 claude -p a\n  2 claude -p b\n  3 claude -p c\n");
        apply_env(
            &mut procs,
            &format!(
                "  1 claude CLAUDE_CODE_SESSION_ID={parent} CLAUDE_CODE_CHILD_SESSION=1 CLAUDE_PID=46771\n\
                 2 claude CLAUDE_CODE_SESSION_ID={parent}\n\
                 3 claude CLAUDE_CODE_SESSION_ID=not-a-uuid CLAUDE_CODE_CHILD_SESSION=1\n"
            ),
        );
        assert_eq!(procs[0].parent_session.as_deref(), Some(parent));
        assert_eq!(procs[1].parent_session, None, "not marked a child");
        assert_eq!(procs[2].parent_session, None, "not a session id");
    }

    /// #s53, the whole path: a running worker's environment names its coordinator, the edge is
    /// banked before the snapshot is drawn, the row says `startedBy`, a monitor started afresh
    /// reads the edge back after the worker is gone, and the coordinator's compose box survives
    /// its own live worker but not an unrelated live session.
    #[test]
    fn a_worker_is_banked_under_its_starter_and_kept_across_a_restart() {
        let scratch = std::env::temp_dir().join(format!("cm-started-by-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let _env = StateEnv::set(scratch.join("state"));
        let coord = "491ffb04-574b-4635-bad7-74d06fa212c6";
        let worker = "622a5571-da63-47a2-945c-e5c65feccc13";
        let now = 1_791_400_000u64;
        let mut procs = parse_ps(
            "  100 claude --dangerously-skip-permissions --resume\n\
             201 claude -p You are a builder --output-format stream-json --verbose\n",
        );
        apply_tty(&mut procs, "  100 ttys003 02:00:00\n  201 ?? 05:00\n", now);
        apply_env(
            &mut procs,
            &format!(
                "  100 claude TMUX=/private/tmp/tmux-501/default,88,0 TMUX_PANE=%0\n\
                 201 claude TMUX=/private/tmp/tmux-501/default,88,0 TMUX_PANE=%0 \
                 CLAUDE_CODE_SESSION_ID={coord} CLAUDE_CODE_CHILD_SESSION=1\n"
            ),
        );
        apply_lsof(&mut procs, "p100\nfcwd\nn/w/knack\np201\nfcwd\nn/w/knack\n");
        let row = |first_ago: u64, one_shot: bool| {
            let mut r = growth_row("/w/knack", false);
            r.first_event = Some(now - first_ago);
            r.tree_mtime = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(now - 10));
            r.one_shot = one_shot;
            r
        };
        let idx = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let mut st = State {
            procs,
            ..Default::default()
        };
        st.rows.insert(coord.into(), row(7_200, false));
        st.rows.insert(worker.into(), row(299, true));
        idx.bank_started_by(&mut st);
        assert_eq!(st.started_by(worker), Some(coord));
        assert_eq!(
            st.started_by(coord),
            None,
            "the owner's own session is nobody's worker"
        );
        let v: Value = idx.assemble(&st, &mut Vec::new());
        let rows: Vec<&Value> = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["rows"].as_array().unwrap())
            .collect();
        let started =
            |sid: &str| rows.iter().find(|r| r["id"] == sid).unwrap()["startedBy"].clone();
        assert_eq!(started(worker), json!(coord));
        assert_eq!(started(coord), Value::Null);
        // The coordinator's own live worker is part of it; an unrelated live session is not.
        let links: HashMap<String, SendLink> =
            [(coord, Some(100)), (worker, Some(201)), ("other", None)]
                .into_iter()
                .map(|(s, pid)| {
                    (
                        s.to_string(),
                        SendLink {
                            pid,
                            ..Default::default()
                        },
                    )
                })
                .collect();
        let cwd_of = |_: &str| Some("/w/knack".to_string());
        let starter = |o: &str| st.started_by(o).map(str::to_string);
        assert!(!project_has_other_live(
            coord,
            Some("/w/knack"),
            &links,
            cwd_of,
            starter
        ));
        let mut busy = links.clone();
        busy.insert(
            "other".into(),
            SendLink {
                pid: Some(300),
                ..Default::default()
            },
        );
        assert!(project_has_other_live(
            coord,
            Some("/w/knack"),
            &busy,
            cwd_of,
            starter
        ));
        // A fresh monitor, the worker long gone: the edge comes back from the cache root.
        let again = Index::new(scratch.join("cache"), scratch.join("state"), Vec::new());
        let mut st2 = again.state.lock().unwrap();
        st2.rows.insert(coord.into(), row(7_200, false));
        st2.rows.insert(worker.into(), row(299, true));
        assert_eq!(st2.started_by(worker), Some(coord), "kept across a restart");
        // A starter this index does not know leaves the worker a row of its own.
        st2.rows.remove(coord);
        assert_eq!(st2.started_by(worker), None);
    }

    /// #s53 with no sighting at all: a one-shot session started in another session's scratch
    /// (#373: knack's workers run in worktrees under their coordinator's scratchpad) is that
    /// session's worker. An INTERACTIVE session there is not — it is the owner's own.
    #[test]
    fn a_one_shot_session_in_a_session_s_scratch_is_its_worker() {
        let mut st = State::default();
        let coord = "96b453d7-0d7b-4e63-af27-4f7e0ef030eb";
        st.rows.insert(coord.into(), growth_row("/w/knack", false));
        let mut worker = growth_row("/tmp/claude-502/-w-knack/x/scratchpad/wt-1", false);
        worker.spawned_by = Some(coord.into());
        worker.one_shot = true;
        st.rows.insert("w".into(), worker);
        let mut own = growth_row("/tmp/claude-502/-w-knack/x/scratchpad/wt-2", false);
        own.spawned_by = Some(coord.into());
        st.rows.insert("own".into(), own);
        assert_eq!(st.started_by("w"), Some(coord));
        assert_eq!(st.started_by("own"), None);
    }

    /// #s53: a one-shot session's head names it and its brief (the first line of its first
    /// prompt, a string or text parts); an interactive session has no brief at all.
    #[test]
    fn a_one_shot_head_gives_its_brief() {
        let dir = std::env::temp_dir().join(format!("cm-brief-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, body: &str| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        let queued = r#"{"type":"queue-operation","operation":"enqueue","sessionId":"x","timestamp":"2026-10-08T10:00:00Z"}"#;
        let worker = file(
            "w.jsonl",
            &format!(
                "{queued}\n{}\n",
                r#"{"type":"user","entrypoint":"sdk-cli","message":{"role":"user","content":"\n  You are a builder on B74.\nRead the rules first."},"timestamp":"2026-10-08T10:00:01Z"}"#
            ),
        );
        assert_eq!(
            head_one_shot(&worker).as_deref(),
            Some("You are a builder on B74.")
        );
        let parts = file(
            "p.jsonl",
            r#"{"type":"user","entrypoint":"sdk-cli","message":{"role":"user","content":[{"type":"text","text":"review B71"}]},"timestamp":"2026-10-08T10:00:01Z"}"#,
        );
        assert_eq!(head_one_shot(&parts).as_deref(), Some("review B71"));
        let spaced = file(
            "s.jsonl",
            r#"{"type": "user", "entrypoint": "sdk-cli", "message": {"role": "user", "content": "spaced out"}}"#,
        );
        assert_eq!(
            head_one_shot(&spaced).as_deref(),
            Some("spaced out"),
            "the record, not its spelling"
        );
        let own = file(
            "o.jsonl",
            r#"{"type":"user","entrypoint":"cli","message":{"role":"user","content":"hi"},"timestamp":"2026-10-08T10:00:01Z"}"#,
        );
        assert_eq!(head_one_shot(&own), None);
    }

    /// #s59, measured on hong-devserver: a Claude session and a Codex session in one repo, the
    /// Claude client in one tmux pane and Codex in another, neither naming its session in argv. Each
    /// session links to its OWN agent's process — confirmed, since it is the only one of its agent in
    /// the directory — so the Claude session is writable in its pane. Before, the Codex process
    /// (the lower pid) won the Claude session, unconfirmed, with no pane to write to.
    #[test]
    fn each_session_links_to_its_own_agents_process() {
        let mut procs = parse_ps(
            "  51668 claude --dangerously-skip-permissions --resume\n  9807 codex resume --yolo\n",
        );
        apply_tty(&mut procs, "  51668 ttys005\n  9807 ttys007\n", 0);
        apply_env(
            &mut procs,
            "  51668 claude TMUX=/private/tmp/tmux-502/agent-metrics-0b78ae,76226,0 TMUX_PANE=%0\n\
             9807 codex TMUX=/private/tmp/tmux-502/agent-metrics-0b78ae,76226,0 TMUX_PANE=%2\n",
        );
        apply_lsof(
            &mut procs,
            "p51668\nfcwd\nn/Users/x/code/agent-metrics\np9807\nfcwd\nn/Users/x/code/agent-metrics\n",
        );
        let cwd = Some("/Users/x/code/agent-metrics");
        let claude = link(
            &procs,
            "d755c274-ca98-4e2f-be95-435c76353e7b",
            Agent::CLAUDE,
            Path::new("/n.jsonl"),
            cwd,
            true,
            &|_| true,
        )
        .expect("the Claude session links");
        assert_eq!(claude.pid, 51668, "to the claude process, never codex");
        assert!(
            claude.confirmed,
            "the only Claude process in the directory: confirmed"
        );
        assert_eq!(claude.terminal.target(), Some("%0"));
        let codex = link(
            &procs,
            "019c0000-0000-7000-8000-000000000000",
            Agent::CODEX,
            Path::new("/r.jsonl"),
            cwd,
            true,
            &|_| true,
        )
        .expect("the Codex session links");
        assert_eq!((codex.pid, codex.confirmed), (9807, true));
        assert_eq!(codex.terminal.target(), Some("%2"));
    }
}
