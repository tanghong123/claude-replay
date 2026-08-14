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
use claude_replay_core::{adapter, adapters, discover, metrics, Agent};
use claude_replay_present::cache::{admit, MetaReader, Presentation};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
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

pub struct Index {
    /// The monitor's OWN durable root (R5) — counters are read from `html/<sid>/meta.jsonl`
    /// under it; an entry existing at all is what "visited" means.
    cache_root: PathBuf,
    /// Which agents to show (R1); empty = all.
    only: Vec<Agent>,
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
    /// group map is keyed by, so a group's key IS its hide key. Loaded once from
    /// `<cache_root>/ignored.json` and rewritten on every toggle. This is monitor UI state at
    /// the monitor's OWN root (the same place visited entries are written) — it never touches
    /// an agent's data or a terminal, so it stays inside the read-only contract (R8).
    ignored: BTreeSet<String>,
    /// The cost ledger (§14) — every session's equivalent-API cost, folded incrementally
    /// through `MetricsCursor`s persisted at the monitor's own root. Lazily built on the
    /// first scan because it needs `cache_root`.
    ledger: Option<crate::cost::CostLedger>,
    /// Sub-agent spend banked onto each ROOT row's sid (§14): a sub-agent rollout is not a
    /// row (it is excluded from `store_transcripts`), but its cost is real — measured 95%
    /// of one project's total — so the scan prices every sub-agent transcript and chases
    /// `parent_thread_id` up to the main session that spawned it.
    sub_costs: HashMap<String, f64>,
    /// The agent-state pass (#194): hysteresis staging + the events/current dump.
    state_tracker: crate::state::StateTracker,
}

/// Per-session scan state, persistent across cycles — the "previous scan" half of §5's diff.
struct Row {
    path: PathBuf,
    agent: Agent,
    cwd: Option<String>,
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
    /// The session this one was forked from (#142), and whether we have looked. Read once:
    /// a fork's origin is fixed when it is created and no later write changes it.
    fork_from: Option<String>,
    fork_probed: bool,
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
    /// The ledger's answer for this session's OWN transcript, `(cost, partial)` (§14).
    /// Kept on the row so a cycle whose budget defers the fold still shows the last price.
    cost: Option<(f64, bool)>,
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
}

impl Index {
    pub fn new(cache_root: PathBuf, only: Vec<Agent>) -> Self {
        let ignored = load_ignored(&ignore_path(&cache_root));
        Self {
            cache_root,
            only,
            state: std::sync::Mutex::new(State {
                ignored,
                ..Default::default()
            }),
        }
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
            save_ignored(&ignore_path(&self.cache_root), &st.ignored);
            // Re-derive the snapshot from the unchanged rows under the new hide set (no
            // rescan needed — hiding is a view filter, not a discovery change). The
            // state pass does not re-run: hiding changes the view, not any state.
            if st.scanned_at.is_some() {
                let snap = self.assemble(&st, &mut Vec::new());
                st.snapshot = snap;
            }
        }
        json!({ "ok": true, "ignored": st.ignored.len() }).to_string()
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
                    cwd: discover::session_cwd(&path).map(|p| p.display().to_string()),
                    title: String::new(),
                    tree_mtime: None,
                    last_event: None,
                    first_event: None,
                    start_probed: false,
                    fork_from: None,
                    fork_probed: false,
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
                        row.cwd = discover::session_cwd(&path).map(|p| p.display().to_string());
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
            if let Some(c) = ledger.cost(row.agent, &row.path, &mut budget) {
                row.cost = Some(c);
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
        st.sub_costs.clear();
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
                if let Some((c, _)) = ledger.cost(a.agent(), path, &mut budget) {
                    *st.sub_costs.entry(root).or_default() += c;
                }
            }
        }

        self.prove_by_growth(st);

        let mut facts = Vec::new();
        st.snapshot = self.assemble(st, &mut facts);
        // The agent-state pass (#194): derive busy/wait/idle from what this tick just
        // observed and dump transitions + the snapshot under `<cache_root>/state/`.
        st.state_tracker.tick(&self.cache_root, &facts);
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
            let mut cands = st.procs.iter().filter(|p| {
                is_agent_exe(&p.exe_base, &p.argv) && p.cwd.as_deref() == Some(cwd.as_str())
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
    fn assemble(&self, st: &State, facts: &mut Vec<crate::state::RowFacts>) -> String {
        #[derive(Default)]
        struct Group {
            kind: &'static str,
            /// The group map's key AND its hide key (#113): `p:<cwd>` / `a:<label>`.
            key: String,
            label: String,
            secondary: String,
            rows: Vec<Value>,
            cost: f64,
            latest: u64,
            growing: usize,
            idle: usize,
            /// Any session in this group whose live agent is in a controllable terminal —
            /// the group-level badge (owner request).
            has_term: bool,
        }
        let now = SystemTime::now();
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
        let mut newest_by_cwd: HashMap<&str, (&str, SystemTime)> = HashMap::new();
        let mut siblings: HashMap<&str, usize> = HashMap::new();
        for (sid, row) in &st.rows {
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
        let mut groups: HashMap<String, Group> = HashMap::new();
        for (sid, row) in &st.rows {
            let anchored = adapter(row.agent).workspace_anchored();
            let (key, kind, label, secondary) = if anchored {
                let cwd = row.cwd.clone().unwrap_or_else(|| "(unknown)".into());
                let leaf = cwd.rsplit('/').next().unwrap_or(&cwd).to_string();
                // Leaf as the label, FULL cwd as the secondary line (§4.2) — the leaf-merge
                // hedge: two checkouts sharing a leaf stay distinguishable one line below.
                (format!("p:{cwd}"), "project", leaf, tilde(&cwd))
            } else {
                (
                    format!("a:{}", row.agent.label()),
                    "agent",
                    row.agent.label().to_string(),
                    "desktop agent · no workspace".into(),
                )
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
                .or_else(|| link(&st.procs, sid, &row.path, row.cwd.as_deref(), heuristic_ok));
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
                    // NAMED, never used — the monitor is read-only (R8); §4 of the probe
                    // gates any future injection on per-target consent.
                    j["target"] = json!(t);
                }
            }
            if let Some((_, c)) = &row.counters {
                j["turns"] = json!(c.turns);
                j["tools"] = json!(c.tools);
                j["subs"] = json!(c.subs);
                j["child"] = json!(c.child_running);
            }
            // Cost from the LEDGER (§14), not the visit-gated meta stream: the row's own
            // transcript plus every sub-agent rollout banked on it. `costPartial` says some
            // model in the mix was unpriced — the number is a `≥` lower bound.
            let own = row.cost.map(|(c, _)| c).unwrap_or(0.0);
            let sub = st.sub_costs.get(sid).copied().unwrap_or(0.0);
            if row.cost.is_some() || sub > 0.0 {
                j["cost"] = json!(own + sub);
                if sub > 0.0 {
                    j["costSubs"] = json!(sub);
                }
                if row.cost.is_some_and(|(_, partial)| partial) {
                    j["costPartial"] = json!(true);
                }
            }

            let g = groups.entry(key.clone()).or_insert_with(|| Group {
                kind,
                key,
                label,
                secondary,
                ..Default::default()
            });
            g.cost += own + sub;
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
                let meta = if g.cost > 0.0 {
                    format!("${:.2} · {}", g.cost, g.rows.len())
                } else {
                    g.rows.len().to_string()
                };
                let total = g.rows.len();
                hidden_count += g
                    .rows
                    .iter()
                    .filter(|r| r["hidden"].as_bool().unwrap_or(false))
                    .count();
                let group_hidden = st.ignored.contains(&g.key);
                json!({
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
                })
            })
            .collect();
        json!({ "groups": out, "ignoredCount": hidden_count }).to_string()
    }
}

/// Decode a `%XX`-percent-encoded query value (hide keys arrive via `encodeURIComponent` —
/// a `p:<cwd>` key carries `/`, `:` and spaces). Unknown/short escapes pass through literally.
pub(crate) fn percent_decode(s: &str) -> String {
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

/// Where the hide list lives (#113): a plain JSON array at the monitor's OWN root, beside the
/// `html/` durable entries — never the viewer's root, never an agent's data (R5/R8).
fn ignore_path(cache_root: &Path) -> PathBuf {
    cache_root.join("ignored.json")
}

/// Load the hide list; a missing or unparsable file is simply an empty set (never fatal —
/// the monitor must start regardless of a corrupt preference file).
fn load_ignored(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

/// Persist the hide list (best-effort: a write failure leaves the in-memory set authoritative
/// for this run rather than crashing the server).
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

/// Resolve which live agent process (if any) is behind session `sid` — the probe's §1
/// precedence, each mechanism verified there:
///   1. `sid` in a process's argv (`--resume <uuid>`) — exact.
///   2. The session's transcript held OPEN by an agent process — exact (Codex holds its
///      rollout `.jsonl` open; Claude appends-and-closes, so this never fires for it).
///   3. An agent-binary process whose cwd matches the session's — the heuristic. Two
///      exclusions keep it honest: a process whose argv carries a uuid belongs to THAT
///      session and never heuristically claims another; and `heuristic_ok` is true only for
///      the NEWEST session of its cwd, without which one process claims every row of its
///      project.
///
/// Step 3 is the COMMON path, not a fallback: launching without a session id is normal
/// (measured: 5 of 8 live agents here have no uuid in argv), and Claude never holds its
/// transcript open, so steps 1–2 cannot fire for them.
///
/// **"Newest session of the cwd" is a tie-break, not a truth.** `claude --resume` with no id
/// opens a PICKER, so the user may resume any session in that directory — the newest is
/// merely the likeliest. Nothing available resolves it: the process holds no fd naming its
/// session (measured: 0 `.jsonl` fds across every live agent), and its start time does not
/// separate the candidates either (measured: in the one ambiguous directory on this machine,
/// BOTH sessions have activity postdating the process). So the link stays `confirmed: false`
/// and the row reports how many sessions it was choosing between (#145) rather than implying
/// a certainty the data cannot support.
fn link(
    procs: &[Proc],
    sid: &str,
    transcript: &Path,
    cwd: Option<&str>,
    heuristic_ok: bool,
) -> Option<AgentLink> {
    let mk = |p: &Proc, confirmed: bool| AgentLink {
        pid: p.pid,
        confirmed,
        terminal: Terminal::of(p),
    };
    if let Some(p) = procs.iter().find(|p| p.argv.contains(sid)) {
        return Some(mk(p, true));
    }
    let t = transcript.to_string_lossy();
    if let Some(p) = procs
        .iter()
        .find(|p| p.open_jsonl.iter().any(|f| f.as_str() == t))
    {
        return Some(mk(p, true));
    }
    if let Some(cwd) = cwd.filter(|_| heuristic_ok) {
        // Several agent processes can share a cwd — a leftover from an earlier run, a helper,
        // and the one the user is actually sitting in front of. Taking the FIRST match meant
        // taking the lowest pid, i.e. usually the oldest: a real knack session hosted in a
        // `tmux -L knack` pane reported "detached" because a stale sibling won. Rank by how
        // the process is HOSTED — a multiplexer target beats a bare tty beats detached — and
        // break ties on pid so the choice stays a pure function of the data.
        if let Some(p) = procs
            .iter()
            .filter(|p| {
                is_agent_exe(&p.exe_base, &p.argv)
                    && p.cwd.as_deref() == Some(cwd)
                    && !has_uuid(&p.argv)
            })
            .max_by_key(|p| {
                let host = match Terminal::of(p) {
                    Terminal::Tmux { .. } | Terminal::Screen(_) => 2,
                    Terminal::Tty => 1,
                    Terminal::Detached => 0,
                };
                (host, std::cmp::Reverse(p.pid))
            })
        {
            return Some(mk(p, false));
        }
    }
    None
}

/// Whether `s` contains a UUID (8-4-4-4-12 hex) anywhere — the exclusion that keeps a
/// resumed-elsewhere process out of the cwd heuristic.
fn has_uuid(s: &str) -> bool {
    s.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .any(is_uuid)
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
const AGENT_PATTERNS_ENV: &str = "CLAUDE_MONITOR_AGENT_PATTERNS";

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
            .map(|spec| AgentPattern::parse(&spec))
            .unwrap_or_default()
    })
}

fn is_agent_exe(exe_base: &str, argv: &str) -> bool {
    const BUILTINS: &[&str] = &["claude", "codex", "qoderwork", "qoder"];
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
    apply_tty(&mut procs, &run(&["ps", "-o", "pid=,tty=", "-p", &pids]));
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
        });
    }
    procs
}

/// `pid tty` lines → the controlling tty; `??` means detached and stays `None`.
fn apply_tty(procs: &mut [Proc], out: &str) {
    for line in out.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(tty)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else {
            continue;
        };
        if tty != "??" {
            if let Some(p) = procs.iter_mut().find(|p| p.pid == pid) {
                p.tty = Some(tty.to_string());
            }
        }
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
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("TMUX_PANE=") {
                p.pane = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("TMUX=") {
                p.tmux_sock = v.split(',').next().map(str::to_string);
            } else if let Some(v) = tok.strip_prefix("STY=") {
                p.screen = Some(v.to_string());
            }
        }
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

/// The last ACTIVITY timestamp in a transcript's tail: scan the final 32 KiB line-wise and
/// keep the latest timestamp on a line that is NOT [housekeeping](NON_ACTIVITY_TYPES). Each
/// agent's format goes through the shared `parse_ts`; `None` when the window holds no
/// parseable activity timestamp.
fn last_event_ts(path: &Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 32 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return None; // a seek into a multi-byte char — treat as unknown
    }
    let mut last = None;
    for line in buf.lines() {
        // Field-level (no per-line JSON parse — pure cost here). Skip housekeeping rows by
        // their `"type"`; take the newest timestamp on everything else.
        let ty = field_after(line, "\"type\":\"").next();
        if ty.is_some_and(|t| NON_ACTIVITY_TYPES.contains(&t)) {
            continue;
        }
        for ts in field_after(line, "\"timestamp\":\"") {
            if let Some(secs) = metrics::parse_ts(ts).filter(|s| *s > 0) {
                last = Some(last.map_or(secs as u64, |cur: u64| cur.max(secs as u64)));
            }
        }
    }
    last
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

/// Resolve the monitor's cache root (R5): `$CLAUDE_MONITOR_CACHE`, else
/// `$XDG_CACHE_HOME/claude-monitor`, else `~/.cache/claude-monitor`.
pub fn default_root() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_MONITOR_CACHE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(p);
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .ok_or_else(|| anyhow::anyhow!("no $HOME — nowhere to keep the monitor's cache"))?;
    Ok(base.join("claude-monitor"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let base = std::env::temp_dir().join(format!("cm-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = base.join("projects");
        let proj = store.join("-tmp-fixture-repo");
        std::fs::create_dir_all(&proj).unwrap();
        std::env::set_var("CLAUDE_PROJECTS_DIR", &store);
        // Point the OTHER stores away from the real machine, so the fixture is the world.
        std::env::set_var("QODERWORK_PROJECTS_DIR", base.join("qw"));
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
        let idx = Index::new(root.clone(), Vec::new());
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
        let main_id = "eeeeeeee-0000-0000-0000-00000000000e";
        let sub1 = "eeeeeeee-1111-0000-0000-00000000000e";
        let sub2 = "eeeeeeee-2222-0000-0000-00000000000e";
        let arch_id = "ffffffff-0000-0000-0000-00000000000f";
        // Main: usage FIRST, model named after — $1.25 only if the blank bucket is
        // attributed, $0 under the old per-model re-derivation.
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
        std::fs::write(
            archive.join(format!("rollout-2026-08-12T01-01-00-{arch_id}.jsonl")),
            format!("{}{named}{usage_1m}", meta_main(arch_id)),
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
            (main["cost"].as_f64().unwrap() - 3.75).abs() < 1e-9,
            "own $1.25 (blank bucket attributed) + two sub-agents chased to the root: {main}"
        );
        assert!(
            (main["costSubs"].as_f64().unwrap() - 2.50).abs() < 1e-9,
            "the sub-agent share is named: {main}"
        );
        let archived = row(arch_id);
        assert!(
            (archived["cost"].as_f64().unwrap() - 1.25).abs() < 1e-9,
            "an archived session is a row, and priced: {archived}"
        );
        let codex_group = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["label"] == "codex-repo")
            .expect("codex group");
        assert_eq!(
            codex_group["metaLine"], "$5.00 · 2",
            "the group sums own + rolled-up spend"
        );

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

        apply_tty(&mut procs, "  101 ttys006\n  102 ??\n  103 ttys009\n");
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
            Path::new("/nope.jsonl"),
            None,
            false,
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
            Path::new("/Users/x/.codex/sessions/2026/08/08/rollout-abc.jsonl"),
            None,
            false,
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
            Path::new("/n.jsonl"),
            Some("/Users/x/other"),
            true,
        )
        .expect("cwd link");
        assert!(
            link(
                &procs,
                "some-other-sid",
                Path::new("/n.jsonl"),
                Some("/Users/x/other"),
                false,
            )
            .is_none(),
            "only the NEWEST session of a cwd may claim a process heuristically"
        );
        assert!(!l.confirmed, "cwd+recency is the hedged match");
        assert_eq!(l.pid, 103);
        assert_eq!(l.terminal.target(), Some("1234.pts-0.host"));
        // 102 (codex, no uuid in argv) heuristically matches its own cwd…
        let l = link(
            &procs,
            "zzz",
            Path::new("/n.jsonl"),
            Some("/Users/x/code/repo"),
            true,
        )
        .expect("codex cwd heuristic");
        assert!(!l.confirmed);
        assert_eq!(l.pid, 102);
        // …while 101, which carries ANOTHER session's uuid, is excluded from the heuristic
        // pool entirely (the probe's UNCONFIRMED cross-check as a hard rule).
        assert!(has_uuid(&procs[0].argv) && !has_uuid(&procs[1].argv));
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
            title: "t".into(),
            tree_mtime: None,
            last_event: None,
            first_event: None,
            start_probed: true,
            fork_from: None,
            fork_probed: true,
            proved_pid: None,
            grew_at: growing.then(Instant::now),
            counters: None,
            title_mtime: None,
            cost: None,
        }
    }

    #[test]
    fn growth_proves_which_session_an_agent_is_driving() {
        let idx = Index::new(std::env::temp_dir().join("cm-proof"), Vec::new());
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
            Path::new("/n.jsonl"),
            Some("/Users/hong/code/knack"),
            true,
        )
        .expect("cwd link");
        assert_eq!(l.pid, 900, "the tmux-hosted process, not the first match");
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
            Path::new("/n.jsonl"),
            Some("/Users/hong/code/knack"),
            true,
        )
        .expect("cwd link");
        assert_eq!(
            l.pid, 700,
            "ties break on pid, whatever order ps listed them"
        );
    }

    #[test]
    fn is_agent_exe_recognizes_the_builtin_basenames_only() {
        assert!(is_agent_exe("claude", "claude"));
        assert!(is_agent_exe("codex", "codex --model gpt-5"));
        assert!(is_agent_exe("qoderwork", "qoderwork"));
        assert!(is_agent_exe("qoder", "qoder"));
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
}
