//! The monitor's index: scan → diff → card → state → one JSON snapshot for the rail.
//!
//! Everything here respects the two prohibitions the design is built on (#98): **no fold on
//! the index path** (R7 — rows are born from bounded reads; counters come from visited
//! sessions' meta streams, read lock-free) and **no background sweep** (§3 — the durable
//! entry for a session is written by SERVING it, never by the monitor itself).

use anyhow::Result;
use claude_replay_core::engine::meta_stream::MaterializedMeta;
use claude_replay_core::liveness::{inflight_tool_in_tail, latest_tree_activity};
use claude_replay_core::{adapter, adapters, discover, metrics, Agent};
use claude_replay_present::cache::{admit, MetaReader, Presentation};
use serde_json::{json, Value};
use std::collections::HashMap;
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
}

/// Per-session scan state, persistent across cycles — the "previous scan" half of §5's diff.
struct Row {
    path: PathBuf,
    agent: Agent,
    cwd: Option<String>,
    title: String,
    /// Tree mtime at the last scan — growth is this moving.
    tree_mtime: Option<SystemTime>,
    /// When growth was last OBSERVED (scan clock) — drives the linger.
    grew_at: Option<Instant>,
    /// Counter fold of the visited entry's meta stream, keyed by the stream's mtime so a
    /// quiet session costs a `stat`, not a re-read.
    counters: Option<(SystemTime, Counters)>,
    /// Title re-derives when the transcript mtime moves past this (§4.1 under lazy: the
    /// mtime IS the refresh trigger).
    title_mtime: Option<SystemTime>,
}

#[derive(Clone)]
struct Counters {
    turns: usize,
    tools: usize,
    subs: usize,
    child_running: bool,
    cost: Option<f64>,
}

/// One row of the process table: pid, executable basename, full argv, cwd (filled lazily).
struct Proc {
    argv: String,
    exe_base: String,
    cwd: Option<String>,
}

impl Index {
    pub fn new(cache_root: PathBuf, only: Vec<Agent>) -> Self {
        Self {
            cache_root,
            only,
            state: std::sync::Mutex::new(State::default()),
        }
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
                    grew_at: None,
                    counters: None,
                    title_mtime: None,
                });
                // Growth = the tree mtime MOVED since the previous scan (§5). The first
                // sighting of a session sets the baseline without claiming growth — a
                // monitor started over an idle machine must not paint everything green.
                if let (Some(prev), Some(now)) = (row.tree_mtime, now_mtime) {
                    if now > prev {
                        row.grew_at = Some(Instant::now());
                    }
                }
                row.tree_mtime = now_mtime;
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

        st.snapshot = self.assemble(st);
    }

    /// Rows → grouped JSON. Grouping is per agent KIND (§4.2): workspace-anchored agents by
    /// project, desktop agents under the agent itself.
    fn assemble(&self, st: &State) -> String {
        #[derive(Default)]
        struct Group {
            kind: &'static str,
            label: String,
            secondary: String,
            rows: Vec<Value>,
            cost: f64,
            latest: u64,
            growing: usize,
        }
        let now = SystemTime::now();
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
            let (state, conf) = if growing {
                ("growing", "")
            } else {
                match liveness(&st.procs, sid, row.cwd.as_deref()) {
                    Liveness::Confirmed => ("idle", "confirmed"),
                    Liveness::Heuristic => ("idle", "unconfirmed"),
                    Liveness::None => ("finished", ""),
                }
            };

            let visited = row.counters.is_some();
            let mtime_secs = row
                .tree_mtime
                .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut j = json!({
                "id": sid,
                "name": row.title,
                "agent": row.agent.label(),
                "state": state,
                "conf": conf,
                "visited": visited,
                "activityTs": mtime_secs,
                "activity": human_age(row.tree_mtime, now),
            });
            if let Some((_, c)) = &row.counters {
                j["turns"] = json!(c.turns);
                j["tools"] = json!(c.tools);
                j["subs"] = json!(c.subs);
                j["child"] = json!(c.child_running);
                if let Some(cost) = c.cost {
                    j["cost"] = json!(cost);
                }
            }

            let g = groups.entry(key).or_insert_with(|| Group {
                kind,
                label,
                secondary,
                ..Default::default()
            });
            if let Some((_, c)) = &row.counters {
                g.cost += c.cost.unwrap_or(0.0);
            }
            g.latest = g.latest.max(mtime_secs);
            g.growing += usize::from(state == "growing");
            g.rows.push(j);
        }

        let mut gs: Vec<Group> = groups.into_values().collect();
        // ORDER MUST BE A PURE FUNCTION OF THE DATA. Both collections come out of HashMaps,
        // whose iteration order is randomized per rebuild — without a total tiebreak, rows
        // tied on the key (same state, same mtime second) swap places between polls with
        // nothing changing on disk, and the rail looks like it reshuffles for no reason.
        //
        // Groups: any-growing first, then most recent activity, then label as the tiebreak.
        gs.sort_by(|a, b| {
            ((b.growing > 0), b.latest, &a.label, &a.secondary).cmp(&(
                (a.growing > 0),
                a.latest,
                &b.label,
                &b.secondary,
            ))
        });
        // Rows: the §4.2 state TIERS — growing, then idle, then finished — most recent
        // first within a tier, id as the total tiebreak.
        fn tier(r: &Value) -> u8 {
            match r["state"].as_str() {
                Some("growing") => 2,
                Some("idle") => 1,
                _ => 0,
            }
        }
        for g in &mut gs {
            g.rows.sort_by(|a, b| {
                (tier(b), b["activityTs"].as_u64(), a["id"].as_str()).cmp(&(
                    tier(a),
                    a["activityTs"].as_u64(),
                    b["id"].as_str(),
                ))
            });
        }
        let out: Vec<Value> = gs
            .into_iter()
            .map(|g| {
                let meta = if g.cost > 0.0 {
                    format!("${:.2} · {}", g.cost, g.rows.len())
                } else {
                    g.rows.len().to_string()
                };
                json!({
                    "kind": g.kind,
                    "label": g.label,
                    "secondary": g.secondary,
                    "metaLine": meta,
                    "rows": g.rows,
                })
            })
            .collect();
        json!({ "groups": out }).to_string()
    }
}

/// Fold a visited entry's meta stream into row counters — `MaterializedMeta` over every
/// record (§2.2: the index displays, it never resumes, so it does not align).
fn fold_counters(dir: &Path) -> Option<Counters> {
    let (_, reader) = MetaReader::open(dir).ok()??;
    let mut mm = MaterializedMeta::default();
    for r in reader {
        mm.push(&r);
    }
    let (cost, _partial) = metrics::total_cost(&mm.tokens);
    Some(Counters {
        turns: mm.session_meta.turns,
        tools: mm.session_meta.tools,
        subs: mm.session_meta.children.len(),
        child_running: mm.session_meta.children.iter().any(|c| c.running),
        cost,
    })
}

enum Liveness {
    Confirmed,
    Heuristic,
    None,
}

/// §5.2's honest process→session link: an argv carrying the session UUID is exact
/// (confirmed); an agent binary whose cwd matches the session's is a heuristic
/// (unconfirmed — the hollow dot). No argv keyword matching, ever: an agent's own tool
/// shells carry `claude` in theirs, which is the trap that loses agents silently.
fn liveness(procs: &[Proc], sid: &str, cwd: Option<&str>) -> Liveness {
    if procs.iter().any(|p| p.argv.contains(sid)) {
        return Liveness::Confirmed;
    }
    if let Some(cwd) = cwd {
        if procs
            .iter()
            .any(|p| is_agent_exe(&p.exe_base) && p.cwd.as_deref() == Some(cwd))
        {
            return Liveness::Heuristic;
        }
    }
    Liveness::None
}

fn is_agent_exe(base: &str) -> bool {
    matches!(base, "claude" | "codex" | "qoderwork" | "qoder")
}

/// The process table, full argv per pid (`command=`, NEVER `comm=` — bulk `comm` truncates
/// absolute paths and silently drops agents launched by one, #99's measured trap), plus cwd
/// for the agent-binary rows via one batched `lsof`.
fn scan_procs() -> Vec<Proc> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let mut procs: Vec<(u32, Proc)> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
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
        procs.push((
            pid,
            Proc {
                argv: argv.to_string(),
                exe_base,
                cwd: None,
            },
        ));
    }
    // cwd only for the handful of agent-binary rows — one lsof, not one per process.
    let agent_pids: Vec<String> = procs
        .iter()
        .filter(|(_, p)| is_agent_exe(&p.exe_base))
        .map(|(pid, _)| pid.to_string())
        .collect();
    if !agent_pids.is_empty() {
        if let Ok(out) = std::process::Command::new("lsof")
            .args(["-a", "-d", "cwd", "-p", &agent_pids.join(","), "-Fpn"])
            .output()
        {
            let mut cur: Option<u32> = None;
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(pid) = line.strip_prefix('p') {
                    cur = pid.parse().ok();
                } else if let Some(cwd) = line.strip_prefix('n') {
                    if let Some(pid) = cur {
                        if let Some((_, p)) = procs.iter_mut().find(|(id, _)| *id == pid) {
                            p.cwd = Some(cwd.to_string());
                        }
                    }
                }
            }
        }
    }
    procs.into_iter().map(|(_, p)| p).collect()
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
        std::env::set_var("CODEX_HOME", base.join("codex"));

        let sid = "11111111-2222-3333-4444-555555555555";
        let t = proj.join(format!("{sid}.jsonl"));
        std::fs::write(
            &t,
            "{\"sessionId\":\"x\",\"type\":\"user\",\"cwd\":\"/tmp/fixture-repo\",\"message\":{\"role\":\"user\",\"content\":\"build the thing\"}}\n",
        )
        .unwrap();

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

        // Growth: the tree mtime moves between two scans.
        std::thread::sleep(Duration::from_millis(2100)); // past the scan floor
        let f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        let r = find(&idx.sessions_json(|_| {}));
        assert_eq!(r["state"], "growing", "an mtime that moved is growth: {r}");

        // A visit: a meta stream at the monitor root turns the row visited, with counters
        // folded from the stream (§2 — no transcript read, no alignment).
        let entry = admit::entry_dir(&root, Presentation::Html, sid);
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join("meta.jsonl"),
            "{\"anchor\":1,\"versions\":{\"format\":1,\"fold\":1}}\n\
             {\"turns\":3,\"tools\":7}\n\
             {\"turns\":2,\"tools\":1}\n",
        )
        .unwrap();
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

        let _ = std::fs::remove_dir_all(&base);
    }
}
