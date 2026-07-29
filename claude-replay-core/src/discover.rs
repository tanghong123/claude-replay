//! Locating a session transcript: by explicit path, by session id, or `--latest`.
//! Discovery spans every agent (Claude + Codex); each session's agent is a
//! property of the file, auto-detected from its contents by [`detect_agent`].
//!
//! This module is the **agent-neutral interface**: the shared [`Candidate`] type, the
//! cwd-ancestor helpers, `session_cwd`/`detect_agent`, and the cross-agent
//! `resolve_any`/`candidates_all` dispatchers. Each agent's actual store lives in its own
//! adapter — [`crate::claude_discover`] (`~/.claude/projects`) and [`crate::codex_discover`].

use crate::Agent;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A pickable session — one transcript on disk plus the metadata the fuzzy session picker
/// shows and ranks by. Produced by [`candidates_all`] / the per-agent discovery.
#[derive(Clone)]
pub struct Candidate {
    /// Absolute path to the transcript `.jsonl` this entry opens (what a selection resolves
    /// to, and what [`detect_agent`] / [`parse_session`](crate::parse_session) are handed).
    pub path: PathBuf,
    /// The transcript file's last-modified time — the recency key the picker sorts by
    /// (most-recent first, after `cwd_affinity`).
    pub mtime: SystemTime,
    /// Which codebase/directory the session was working in, as a short human-recognizable
    /// label — the **leaf name of the session's working directory**, derived from the cwd the
    /// transcript recorded (a session under `/Users/you/code/knack` → `"knack"`). It groups
    /// and labels rows in the picker instead of showing an opaque id or a long path. Not a
    /// path, and not guaranteed unique (two dirs can share a leaf name).
    pub project: String,
    /// A preview of *what the session was about*, so you can recognise it at a glance: its
    /// **first genuine user prompt**, whitespace-collapsed and truncated to ~one line (e.g.
    /// `"add a --width flag to the CLI"`). Host-context / boilerplate messages are skipped;
    /// empty when the session has no user prompt yet.
    pub snippet: String,
    /// Whether this session belongs to the directory you're launching from **right now** —
    /// `true` iff its `project` matches the current working directory's. It's purely a
    /// **ranking hint**: the picker lists affinity sessions first, so "the sessions for *this*
    /// repo" float to the top, above everything else sorted by recency.
    pub cwd_affinity: bool,
    /// Which agent wrote this transcript (Claude / Codex) — shown as a badge and used to
    /// dispatch to the right parser.
    pub agent: Agent,
}

/// Directories from `cwd` up to (and including) `$HOME` — the ancestors we probe
/// for a matching project, nearest first. Never climbs above `$HOME`; for a cwd
/// OUTSIDE `$HOME` the climb stops before the filesystem root — `/`'s slug (`-`)
/// can collide with a real store dir for sessions recorded at `/`, which made a
/// brand-new scratch dir under `/private/tmp` match four unrelated QoderWork
/// sessions (#62). The bare root is only probed when it IS the cwd. Agent-neutral;
/// each adapter maps these to its own store layout.
pub(crate) fn ancestors_of(cwd: &Path) -> Vec<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let root = Path::new("/");
    let mut dirs = vec![cwd.to_path_buf()];
    let mut cur = cwd.parent();
    while let Some(d) = cur {
        if d == root {
            break; // never a CLIMBED ancestor (slug collision with `/`-cwd sessions)
        }
        dirs.push(d.to_path_buf());
        if home.as_deref() == Some(d) {
            break;
        }
        cur = d.parent();
    }
    dirs
}

/// [`ancestors_of`] the current working directory. (Test-only since the scoped-discovery
/// callers all pass an explicit cwd; kept for the cwd-ancestor-chain test.)
#[cfg(test)]
pub(crate) fn ancestor_dirs() -> Vec<PathBuf> {
    match std::env::current_dir() {
        Ok(cwd) => ancestors_of(&cwd),
        Err(_) => Vec::new(),
    }
}

/// Sessions for the current directory across **every** agent, filtered to `only`
/// when set (else all agents), sorted cwd-matches-first then most-recent.
pub fn candidates_all(only: Option<Agent>) -> Vec<Candidate> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut out: Vec<Candidate> = Vec::new();
    // Each agent is scoped to cwd-or-nearest-ancestor-with-sessions, with no global
    // fallback — so a session for an unrelated directory never shows here.
    for a in crate::adapter::adapters() {
        if only.is_none() || only == Some(a.agent()) {
            out.extend(a.candidates_scoped(&cwd));
        }
    }
    out.sort_by(|a, b| {
        b.cwd_affinity
            .cmp(&a.cwd_affinity)
            .then(b.mtime.cmp(&a.mtime))
    });
    out
}

/// The working directory a session ran in, read from the transcript head — the
/// top-level `cwd` (Claude) or `payload.cwd` of `session_meta` (Codex). Used to
/// resolve a header's relativized path back to an absolute one (for reveal-in-
/// file-manager). `None` when no cwd is recorded. Agent-neutral: it accepts both shapes.
pub fn session_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(50)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
            return Some(PathBuf::from(cwd));
        }
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(cwd) = v.pointer("/payload/cwd").and_then(Value::as_str) {
                return Some(PathBuf::from(cwd));
            }
        }
    }
    None
}

/// The session id recorded in the transcript head — Claude's top-level `sessionId` or
/// Codex's `payload.id` of `session_meta`. `None` when absent (a caller then falls back to
/// the file stem). Agent-neutral, mirroring [`session_cwd`].
pub fn session_id(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(50)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(id) = v.get("sessionId").and_then(Value::as_str) {
            return Some(id.to_string());
        }
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(id) = v.pointer("/payload/id").and_then(Value::as_str) {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Auto-detect which agent wrote a transcript by sniffing its first lines — asking
/// each registered adapter's `sniff` claim (#59). An [`Owns`](crate::adapter::SniffClaim)
/// claim (a distinctive head: Codex's `session_meta`, QoderWork's `runtime-config`)
/// wins immediately; a mere `CanParse` (Claude's adapter can parse any Claude-format
/// lines, including derived agents') is remembered and only wins if NO adapter owns
/// any of the sniffed lines — so a new Claude-format agent is labeled by its owner
/// marker, never mislabeled by the first can-parse adapter, and Claude's sniff needs
/// no per-agent carve-outs. Defaults to Claude.
pub fn detect_agent(path: &Path) -> Agent {
    detect_agent_claimed(path).0
}

/// [`detect_agent`] plus whether the label is OWNERSHIP-PROVEN (#66): `true` when a
/// distinctive marker decided it, `false` for a merely-compatible (can-parse)
/// fallback — an unknown Claude-format-derived agent's transcript parses fine but
/// is honestly "compatible", not known to be Claude's. Presenters may badge the
/// unowned case; parse dispatch uses the agent either way.
pub fn detect_agent_claimed(path: &Path) -> (Agent, bool) {
    use crate::adapter::SniffClaim;
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return (Agent::Claude, false);
    };
    let mut can_parse: Option<Agent> = None;
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(5)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        for a in crate::adapter::adapters() {
            match a.sniff(&v) {
                SniffClaim::Owns => return (a.agent(), true),
                SniffClaim::CanParse => {
                    if can_parse.is_none() {
                        can_parse = Some(a.agent());
                    }
                }
                SniffClaim::No => {}
            }
        }
    }
    (can_parse.unwrap_or(Agent::Claude), false)
}

/// The **transcript file path** of sub-agent `child_id` spawned under the session at `root`,
/// for the given `agent` (Claude's flat `<root-stem>/subagents/agent-<id>.jsonl` layout).
/// Resolves a path; it does **not** parse. `None` if the agent has no sub-agent tree (Codex)
/// or the child file doesn't exist. Routes to the agent adapter's `subagent_source` hook.
///
/// The path is **derived from that on-disk layout, not read from the transcript**: a spawn
/// records the child's `agentId` and (for an async spawn) its result `outputFile`, but *not*
/// the child transcript's path — so it must be reconstructed from `root` + `child_id`, which
/// is what this does.
///
/// This is the **lazy, on-demand** route — the presentation layer uses it to open a child from
/// its *own* file (descend-and-live-tail in the TUI, the HTML server's deep links, the
/// `--dump-all-html` BFS). It's distinct from the **eager**
/// [`parse_session_enriched`](crate::parse_session_enriched), which
/// walks the same `subagents/` dir at parse time to load each child's *blocks* into its
/// `SubAgent` spawn. Same on-disk layout, eager-into-blocks vs. lazy-by-path.
pub fn subagent_source(agent: Agent, root: &Path, child_id: &str) -> Option<PathBuf> {
    crate::adapter::adapter(agent).subagent_source(root, child_id)
}

/// The LIVE on-disk task list for the session at `path` (#15) — the agent-neutral
/// facade over each adapter's `load_tasks` hook (Claude reads
/// `~/.claude/tasks/<session-id>/*.json`; agents with no task store return `None`).
/// Complements the transcript-derived op-log state in
/// [`Session::tasks`](crate::Session); merge with
/// [`tasks::merged`](crate::engine::tasks::merged) — disk wins per id, the op-log
/// backfills pruned files.
pub fn session_tasks(agent: Agent, path: &Path) -> Option<crate::engine::tasks::TaskList> {
    crate::adapter::adapter(agent).load_tasks(path)
}

/// Is the `agent` label for `path` OWNERSHIP-PROVEN (#66)? True when the sniff
/// owned it (a distinctive in-band marker) OR the file sits inside that agent's own
/// transcript store (provenance — a normal `~/.claude/projects` session is Claude's
/// without any marker). False = merely-compatible: an arbitrary path whose format
/// parses but whose author is honestly unknown.
pub fn detection_owned(agent: Agent, path: &Path) -> bool {
    detect_agent_claimed(path).1 || crate::adapter::adapter(agent).store_contains(path)
}

/// Resolve which transcript to open, across agents, and return its path.
///
/// - `target` — an explicit selection, **either a filesystem path to a `.jsonl` transcript OR
///   a bare session id**, tried in that order: if the string names an existing file, that file
///   is used (and its agent is auto-detected on open); otherwise it's looked up as a session id
///   in each in-scope agent's store. `None` means "no explicit target — fall back to `latest`".
/// - `only` — restrict the search to a single agent's store; `None` searches every agent.
/// - `latest` — used only when `target` is `None`: pick the most-recent transcript for the
///   current directory (or its nearest ancestor that has sessions; cwd-matches first, no global
///   fallback) instead of erroring.
///
/// Precedence: `target` (as a path, then as a session id) → else `latest` → else: when the
/// cwd-scoped session set has exactly ONE candidate it is auto-selected (#51 — selection is
/// only demanded on genuine ambiguity; the non-interactive `--dump*` paths land here, while
/// the interactive default opens the picker and never reaches this branch), several
/// candidates `Err` listing them, zero `Err` with the no-session message.
pub fn resolve_any(only: Option<Agent>, target: Option<&str>, latest: bool) -> Result<PathBuf> {
    if let Some(t) = target {
        let as_path = PathBuf::from(t);
        if as_path.is_file() {
            return Ok(as_path);
        }
        // Session id: look in each in-scope agent's store via its adapter.
        for a in crate::adapter::adapters() {
            if only.is_none() || only == Some(a.agent()) {
                if let Some(hit) = a.resolve_id(t) {
                    return Ok(hit);
                }
            }
        }
        return Err(anyhow!(
            "no transcript found for '{t}' (not a file, and no session id match)"
        ));
    }
    // Scoped to the cwd or its nearest ancestor that has sessions — NOT the
    // global newest. `candidates_all` sorts cwd-matches first, then most-recent,
    // with no global fallback, so an unrelated directory's session never wins.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cands = candidates_all(only);
    if latest {
        return cands
            .into_iter()
            .next()
            .map(|c| c.path)
            .ok_or_else(|| anyhow!("no session found for {} or its ancestors", cwd.display()));
    }
    resolve_lone(&mut cands, &cwd)
}

/// The no-target-no-`--latest` fallback (#51): exactly one cwd-scoped candidate is
/// unambiguous — use it; several demand a selection (the error names them, newest
/// first); zero keeps the no-session message. Split from [`resolve_any`] so the
/// three-way rule is unit-testable without a real store.
fn resolve_lone(cands: &mut Vec<Candidate>, cwd: &Path) -> Result<PathBuf> {
    match cands.len() {
        1 => Ok(cands.remove(0).path),
        0 => Err(anyhow!(
            "no session found for {} or its ancestors — give a session id or a path",
            cwd.display()
        )),
        n => {
            let mut msg =
                format!("{n} sessions match this directory — give a session id or use --latest:");
            for c in cands.iter().take(10) {
                let id = c
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<unnamed>");
                let age = match c.mtime.elapsed() {
                    Ok(d) if d.as_secs() < 3600 => format!("{}m ago", d.as_secs() / 60),
                    Ok(d) if d.as_secs() < 86_400 => format!("{}h ago", d.as_secs() / 3600),
                    Ok(d) => format!("{}d ago", d.as_secs() / 86_400),
                    Err(_) => String::new(),
                };
                let snippet: String = c.snippet.chars().take(60).collect();
                msg.push_str(&format!("\n  {id}  [{}] {age}  {snippet}", c.agent.label()));
            }
            if n > 10 {
                msg.push_str(&format!("\n  … and {} more", n - 10));
            }
            Err(anyhow!(msg))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The no-target-no-`--latest` rule (#51): one cwd-scoped candidate is
    /// unambiguous and auto-selects; several error NAMING each (id, agent, snippet)
    /// so the user can pick; zero keeps the no-session error.
    #[test]
    fn lone_candidate_auto_selects_and_ambiguity_names_the_choices() {
        let cand = |stem: &str, snippet: &str| Candidate {
            path: PathBuf::from(format!("/store/{stem}.jsonl")),
            mtime: SystemTime::now(),
            project: "proj".into(),
            snippet: snippet.into(),
            cwd_affinity: true,
            agent: Agent::Claude,
        };
        let cwd = PathBuf::from("/w/proj");
        // Exactly one → auto-selected.
        let mut one = vec![cand("aaaa-1111", "fix the parser")];
        assert_eq!(
            resolve_lone(&mut one, &cwd).unwrap(),
            PathBuf::from("/store/aaaa-1111.jsonl")
        );
        // Two → error names both ids and the snippets.
        let mut two = vec![
            cand("aaaa-1111", "fix the parser"),
            cand("bbbb-2222", "add the exporter"),
        ];
        let err = resolve_lone(&mut two, &cwd).unwrap_err().to_string();
        assert!(err.contains("2 sessions match"), "{err}");
        assert!(
            err.contains("aaaa-1111") && err.contains("bbbb-2222"),
            "{err}"
        );
        assert!(
            err.contains("fix the parser") && err.contains("--latest"),
            "{err}"
        );
        // Zero → the no-session error.
        let err = resolve_lone(&mut Vec::new(), &cwd).unwrap_err().to_string();
        assert!(err.contains("no session found"), "{err}");
    }

    #[test]
    fn detect_agent_sniffs_transcript_shape() {
        let dir = std::env::temp_dir();
        let codex = dir.join(format!("detect-codex-{}.jsonl", std::process::id()));
        std::fs::write(
            &codex,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"cwd\":\"/x\"}}\n",
        )
        .unwrap();
        let claude = dir.join(format!("detect-claude-{}.jsonl", std::process::id()));
        std::fs::write(
            &claude,
            "{\"sessionId\":\"abc\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();

        // QoderWork: Claude-format lines under a `runtime-config` head — the head also
        // carries `sessionId`, so Claude's adapter CAN parse it, but QoderWork OWNS the
        // distinctive head and ownership outranks can-parse (#59) — no carve-out in
        // Claude's sniff, and detection stays order-independent.
        let qoderwork = dir.join(format!("detect-qw-{}.jsonl", std::process::id()));
        std::fs::write(
            &qoderwork,
            "{\"type\":\"runtime-config\",\"sessionId\":\"abc\",\"model\":\"qwork-ultimate\",\"timestamp\":1785068132048}\n",
        )
        .unwrap();

        assert_eq!(detect_agent(&codex), Agent::Codex);
        assert_eq!(detect_agent(&claude), Agent::Claude);
        assert_eq!(detect_agent(&qoderwork), Agent::QoderWork);
        // The claim levels behind that: Claude claims CanParse on QW's head; QW Owns it.
        {
            use crate::adapter::{adapter, SniffClaim};
            let head: Value =
                serde_json::from_str("{\"type\":\"runtime-config\",\"sessionId\":\"abc\"}")
                    .unwrap();
            assert_eq!(adapter(Agent::Claude).sniff(&head), SniffClaim::CanParse);
            assert_eq!(adapter(Agent::QoderWork).sniff(&head), SniffClaim::Owns);
        }
        // A missing/empty file falls back to Claude.
        assert_eq!(detect_agent(Path::new("/nonexistent.jsonl")), Agent::Claude);

        std::fs::remove_file(&codex).ok();
        std::fs::remove_file(&claude).ok();
        std::fs::remove_file(&qoderwork).ok();
    }

    #[test]
    fn ancestors_start_at_cwd_are_parent_chain_and_stop_at_home() {
        let dirs = ancestor_dirs();
        assert!(!dirs.is_empty(), "should include at least the cwd");
        assert_eq!(
            dirs[0],
            std::env::current_dir().unwrap(),
            "nearest first = cwd"
        );
        // Each entry is the parent of the previous.
        for w in dirs.windows(2) {
            assert_eq!(w[1], w[0].parent().unwrap(), "not a parent chain: {w:?}");
        }
        // If $HOME is on the chain, it is the last entry (we don't climb above it).
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            if dirs.contains(&home) {
                assert_eq!(*dirs.last().unwrap(), home, "should stop at $HOME");
            }
        }
    }

    /// #66: ownership is sniff-marker OR store-provenance. An arbitrary-path
    /// Claude-format file is merely compatible (parse dispatch unaffected); the
    /// same bytes inside Claude's own store are ownership-proven; a Codex head is
    /// sniff-owned anywhere.
    #[test]
    fn detection_ownership_combines_sniff_and_provenance() {
        let dir = std::env::temp_dir();
        let stray = dir.join(format!("own-stray-{}.jsonl", std::process::id()));
        std::fs::write(
            &stray,
            "{\"sessionId\":\"abc\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let (agent, sniff_owned) = detect_agent_claimed(&stray);
        assert_eq!(agent, Agent::Claude);
        assert!(!sniff_owned, "claude format alone proves nothing");
        assert!(
            !detection_owned(Agent::Claude, &stray),
            "arbitrary path: merely compatible"
        );
        let codex = dir.join(format!("own-codex-{}.jsonl", std::process::id()));
        std::fs::write(
            &codex,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s1\",\"cwd\":\"/x\"}}\n",
        )
        .unwrap();
        assert!(
            detection_owned(Agent::Codex, &codex),
            "codex head is sniff-owned anywhere"
        );
        // Store provenance: the same Claude-format bytes inside the Claude store.
        let in_store = crate::claude_discover::projects_dir()
            .join("own-test-proj")
            .join("own-in-store.jsonl");
        if std::fs::create_dir_all(in_store.parent().unwrap()).is_ok()
            && std::fs::copy(&stray, &in_store).is_ok()
        {
            assert!(
                detection_owned(Agent::Claude, &in_store),
                "store provenance proves ownership"
            );
            let _ = std::fs::remove_dir_all(in_store.parent().unwrap());
        }
        std::fs::remove_file(&stray).ok();
        std::fs::remove_file(&codex).ok();
    }

    /// #62: for a cwd OUTSIDE `$HOME`, the climb stops BEFORE the filesystem root —
    /// `/`'s slug (`-`) collides with store dirs for sessions recorded at `/`,
    /// which leaked unrelated sessions into a scratch dir's candidate set. The bare
    /// root is only probed when it IS the cwd.
    #[test]
    fn ancestors_outside_home_never_reach_the_bare_root() {
        let dirs = ancestors_of(Path::new("/private/tmp/some/scratch"));
        assert_eq!(dirs[0], Path::new("/private/tmp/some/scratch"));
        assert!(
            !dirs.iter().any(|d| d == Path::new("/")),
            "climbed to the bare root: {dirs:?}"
        );
        assert_eq!(*dirs.last().unwrap(), Path::new("/private"), "{dirs:?}");
        // Degenerate: the root as the STARTING cwd still probes itself.
        assert_eq!(ancestors_of(Path::new("/")), vec![PathBuf::from("/")]);
    }
}
