//! **#99 prototype — session liveness, terminal attachment, input injection.**
//!
//! Deliberately an *example*, not a binary: this is a knowledge instrument, not a shipped
//! feature. Nothing here is wired into the CLI, and §3 in particular must not be productised
//! without the consent story in `design/session-liveness-probe.md`.
//!
//! ```text
//! cargo run --example session_probe            # every discoverable session
//! cargo run --example session_probe -- <id>    # one session id (prefix match)
//! ```
//!
//! Answers three questions per `<agent, session>`:
//!   1. is it running?          — four signals, cross-checked (see `Liveness`)
//!   2. is it on a terminal?    — controlling tty + multiplexer, from the process table
//!   3. can input be injected?  — capability, decided by the multiplexer, never by guessing

use claude_replay::jdi::{inflight_tool_in_tail, latest_tree_activity};
use claude_replay_core::discover::session_id;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Idle beyond this and we stop calling a session "working" on mtime alone.
const FRESH: Duration = Duration::from_secs(90);

#[derive(Debug, PartialEq)]
enum Liveness {
    /// A live process AND recent evidence of work (fresh tree write, or a tool in flight).
    Running,
    /// A live process, but quiet — sitting at the prompt waiting for you.
    IdleAlive,
    /// No process holds this session; the transcript is history.
    Finished,
}

struct Probe {
    id: String,
    path: Option<PathBuf>,
    id_confirmed: bool,
    agent: String,
    pid: Option<u32>,
    tty: Option<String>,
    mux: Mux,
    tree_age: Option<Duration>,
    inflight: bool,
}

#[derive(Debug, PartialEq)]
enum Mux {
    Tmux { pane: String },
    Screen { sty: String },
    BareTty,
    None,
}

impl Probe {
    fn liveness(&self) -> Liveness {
        if self.pid.is_none() {
            return Liveness::Finished;
        }
        // A tool in flight outranks every clock: the agent is blocked in a call whose
        // output has not landed, so nothing in the tree has been written for a while.
        if self.inflight {
            return Liveness::Running;
        }
        match self.tree_age {
            Some(d) if d < FRESH => Liveness::Running,
            _ => Liveness::IdleAlive,
        }
    }

    /// What it would take to push a keystroke into this session, decided by the
    /// multiplexer. See the note's capability matrix for why the bare-tty answer is "no".
    fn injection(&self) -> String {
        match &self.mux {
            Mux::Tmux { pane } => format!("yes — tmux send-keys -t {pane} '<text>' Enter"),
            Mux::Screen { sty } => format!("yes — screen -S {sty} -X stuff '<text>\\n'"),
            Mux::BareTty => "no — TIOCSTI into another process's tty is denied".into(),
            Mux::None => "n/a — no controlling terminal".into(),
        }
    }
}

/// Every live agent process, as `(pid, argv)`.
///
/// Matched on the basename of **argv[0]** — deliberately neither of the two obvious
/// alternatives:
///   * not argv *anywhere*: an agent's own tool shells carry "claude" in their argv and
///     would all look like agents (the trap jdi documents);
///   * not `comm` from this bulk listing: the multi-column form pads/**truncates** comm to
///     a fixed width (`/Users/hong/.loc`), so every agent launched by absolute path is
///     silently dropped. jdi is unaffected — it reads `ps -o comm= -p <pid>` per pid, which
///     does not truncate.
///
/// argv[0] is the executable path and is not truncated, and a tool shell's argv[0] is its
/// shell — so it carries the precision of a name match without the truncation.
fn agent_processes() -> Vec<(u32, String)> {
    let out = match Command::new("ps").args(["-axo", "pid=,args="]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, args) = l.trim().split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let exe = args.split_whitespace().next()?;
            let name = Path::new(exe).file_name()?.to_str()?;
            matches!(name, "claude" | "codex" | "qoderwork").then(|| (pid, args.to_string()))
        })
        .collect()
}

/// One `ps -o <field>=` read for a pid.
fn ps_field(pid: u32, field: &str) -> Option<String> {
    let out = Command::new("ps")
        .args([&format!("-o{field}="), "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty() && s != "??" && s != "-").then_some(s)
}

/// The multiplexer a pid sits in, read from its ENVIRONMENT (`ps eww`) — the only place
/// that distinguishes a tmux pane from a bare terminal, since both have a real tty.
fn mux_of(pid: u32, tty: Option<&String>) -> Mux {
    let env = Command::new("ps")
        .args(["eww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let var = |k: &str| {
        env.split_whitespace()
            .find_map(|t| t.strip_prefix(k).map(str::to_string))
    };
    if let Some(pane) = var("TMUX_PANE=") {
        return Mux::Tmux { pane };
    }
    if let Some(sty) = var("STY=") {
        return Mux::Screen { sty };
    }
    if tty.is_some() {
        Mux::BareTty
    } else {
        Mux::None
    }
}

/// The session id an agent process is resuming, from its argv. Mirrors jdi's
/// `session_id_from_argv`: the value after `--resume` / `--session-id`.
fn session_of_argv(args: &str) -> Option<String> {
    let toks: Vec<&str> = args.split_whitespace().collect();
    toks.iter()
        .position(|t| {
            matches!(*t, "--resume" | "--session-id" | "-r") || t.starts_with("--resume=")
        })
        .and_then(|i| {
            toks[i]
                .strip_prefix("--resume=")
                .map(str::to_string)
                .or_else(|| toks.get(i + 1).map(|s| s.to_string()))
        })
        .filter(|s| !s.starts_with('-'))
}

/// Where each agent keeps transcripts. A session id resolves to `<root>/**/<id>.jsonl`.
/// Deliberately NOT `candidates_all`: discovery is cwd-scoped (#69), and this question is
/// machine-wide — the live agents are mostly in other projects.
fn store_roots() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    vec![
        home.join(".claude/projects"),
        home.join(".codex/sessions"),
        home.join(".qoderwork/projects"),
    ]
}

/// Find `<id>.jsonl` anywhere under the store roots (breadth-first, bounded depth).
fn transcript_of(id: &str) -> Option<PathBuf> {
    let want = format!("{id}.jsonl");
    let mut queue: Vec<PathBuf> = store_roots();
    let mut depth = 0;
    while !queue.is_empty() && depth < 4 {
        let mut next = Vec::new();
        for dir in queue {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    next.push(p);
                } else if p.file_name().is_some_and(|n| n == want.as_str()) {
                    return Some(p);
                }
            }
        }
        queue = next;
        depth += 1;
    }
    None
}

fn main() {
    let want = std::env::args().nth(1);
    let procs = agent_processes();

    // PROCESS-FIRST: every live agent, then resolve its transcript. The inverse
    // (discover transcripts, then look for a process) misses every session outside
    // the current cwd, which is nearly all of them.
    let mut probes: Vec<Probe> = Vec::new();
    for (pid, args) in &procs {
        let Some(id) = session_of_argv(args) else {
            continue;
        };
        if want.as_ref().is_some_and(|w| !id.starts_with(w.as_str())) {
            continue;
        }
        let path = transcript_of(&id);
        let tty = ps_field(*pid, "tty");
        let mux = mux_of(*pid, tty.as_ref());
        probes.push(Probe {
            tree_age: path
                .as_deref()
                .and_then(latest_tree_activity)
                .and_then(|t| SystemTime::now().duration_since(t).ok()),
            inflight: path.as_deref().is_some_and(inflight_tool_in_tail),
            agent: path
                .as_deref()
                .and_then(|p| p.to_str())
                .map(|p| {
                    if p.contains("/.codex/") {
                        "codex"
                    } else if p.contains("/.qoderwork/") {
                        "qoderwork"
                    } else {
                        "claude"
                    }
                })
                .unwrap_or("?")
                .to_string(),
            // Cross-check: the transcript's recorded id must equal the argv id, or the
            // resolution is by filename coincidence.
            id_confirmed: path
                .as_deref()
                .and_then(session_id)
                .is_some_and(|s| s == id),
            id,
            path,
            pid: Some(*pid),
            tty,
            mux,
        });
    }

    probes.sort_by_key(|p| p.id.clone());
    let live: Vec<&Probe> = probes.iter().collect();

    println!(
        "live agent processes: {} · with a resolvable session: {}\n",
        procs.len(),
        live.len()
    );
    for p in &live {
        println!("{} · {}", p.agent, p.id);
        println!("  status     {:?}", p.liveness());
        println!(
            "    signals  pid={:?} tree_age={} inflight={}",
            p.pid,
            p.tree_age
                .map_or("—".into(), |d| format!("{}s", d.as_secs())),
            p.inflight
        );
        println!(
            "  terminal   {} · {}",
            p.tty.as_deref().unwrap_or("none"),
            match &p.mux {
                Mux::Tmux { pane } => format!("tmux {pane}"),
                Mux::Screen { sty } => format!("screen {sty}"),
                Mux::BareTty => "bare tty".into(),
                Mux::None => "detached".into(),
            }
        );
        println!("  injection  {}", p.injection());
        println!(
            "  transcript {}{}\n",
            p.path
                .as_ref()
                .map_or("(unresolved)".into(), |p| p.display().to_string()),
            if p.path.is_some() && !p.id_confirmed {
                "  [id UNCONFIRMED]"
            } else {
                ""
            }
        );
    }

    let unresolved = procs.len() - live.len();
    if unresolved > 0 {
        println!(
            "{unresolved} live agent(s) carried no --resume id (fresh session, id not in argv)"
        );
    }
    if !live.is_empty() {
        println!(
            "\nNOTE: injection is a capability report, not an invitation — see \
             design/session-liveness-probe.md on consent before anything productises it."
        );
    }
}
