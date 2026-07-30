//! The **discovery vocabulary** — the agent-free half of locating sessions (#87 step 3):
//! the shared [`Candidate`] type, the cwd-ancestor scoping helpers, and the format-neutral
//! transcript-head readers `session_cwd`/`session_id`. The REGISTRY half — `detect_agent`,
//! `resolve_any`, `candidates_all`, the per-adapter dispatch — lives in the facade crate
//! (`claude-replay-core`), which wires the agents in; adapters build on THIS half through
//! the seam.

use crate::Agent;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A pickable session — one transcript on disk plus the metadata the fuzzy session picker
/// shows and ranks by. Produced by the facade's `candidates_all` / the per-agent discovery.
#[derive(Clone)]
pub struct Candidate {
    /// Absolute path to the transcript `.jsonl` this entry opens (what a selection resolves
    /// to, and what the facade's `detect_agent` / `parse_session` are handed).
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

/// Directories from `cwd` up to (but **never including**) `home`, nearest first —
/// the ancestors we probe for a matching project. Cwd-based auto-discovery is
/// scoped to the user's home directory (#69): a cwd that is not strictly inside
/// `home` — including `home` itself, `/tmp`, a missing `$HOME` — yields NOTHING,
/// and the probe never reaches `home`'s own slug. Both halves exist because
/// misbehaving agents record sessions against directories a probe must never
/// match: QoderWork writes some sessions' project dir as `$HOME` itself (its store
/// grows a `-Users-<name>` dir) and others as `/` (the `-` dir, #62); scoping the
/// climb strictly below home makes both unreachable. Explicit paths/ids are
/// unaffected — only cwd inference is scoped. Agent-neutral; each adapter maps
/// these to its own store layout.
pub fn ancestors_below(cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    if home.as_os_str().is_empty() || cwd == home || !cwd.starts_with(home) {
        return Vec::new();
    }
    let mut dirs = vec![cwd.to_path_buf()];
    let mut cur = cwd.parent();
    while let Some(d) = cur {
        if d == home {
            break; // probe strict subdirectories only — never home's own slug
        }
        dirs.push(d.to_path_buf());
        cur = d.parent();
    }
    dirs
}

/// The process's `$HOME`, if set and non-empty — the home every public scoped
/// lookup passes to [`ancestors_below`].
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
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
