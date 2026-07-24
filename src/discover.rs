//! Locating a session transcript: by explicit path, by session id, or `--latest`.
//! Discovery spans every agent (Claude + Codex); each session's agent is a
//! property of the file, auto-detected from its contents by [`detect_agent`].

use crate::Agent;
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Root under which Claude Code writes per-project transcript dirs.
pub fn projects_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_PROJECTS_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude").join("projects")
}

/// All transcript files under the projects dir, newest first (by mtime).
pub fn all_transcripts() -> Vec<PathBuf> {
    let mut out: Vec<(SystemTime, PathBuf)> = Vec::new();
    let root = projects_dir();
    let Ok(projects) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    for proj in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((mtime, p));
            }
        }
    }
    out.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
    out.into_iter().map(|(_, p)| p).collect()
}

/// A pickable session, with metadata for the picker UI.
#[derive(Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub project: String,    // human-ish project name (last path segment)
    pub snippet: String,    // first user message, truncated
    pub cwd_affinity: bool, // belongs to the current working directory's project
    pub agent: Agent,       // which agent produced this session
}

/// The slug Claude Code uses for a directory: '/' and '.' replaced by '-'.
fn slug_for(dir: &Path) -> String {
    dir.to_string_lossy().replace(['/', '.'], "-")
}

/// Transcript files inside one project dir (`projects_dir()/slug`), with mtimes.
fn transcripts_in_project(slug: &str) -> Vec<(SystemTime, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects_dir().join(slug)) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((mtime, p));
        }
    }
    out
}

/// Directories from `cwd` up to (and including) `$HOME` — the ancestors we probe
/// for a matching project, nearest first. Never climbs above `$HOME`.
pub(crate) fn ancestors_of(cwd: &Path) -> Vec<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut dirs = Vec::new();
    let mut cur: Option<&Path> = Some(cwd);
    while let Some(d) = cur {
        dirs.push(d.to_path_buf());
        if home.as_deref() == Some(d) {
            break;
        }
        cur = d.parent();
    }
    dirs
}

fn ancestor_dirs() -> Vec<PathBuf> {
    match std::env::current_dir() {
        Ok(cwd) => ancestors_of(&cwd),
        Err(_) => Vec::new(),
    }
}

/// Claude sessions scoped strictly to `cwd` or its **nearest ancestor that has
/// sessions** — no global fallback (a directory with no session history up its
/// chain yields nothing, so unrelated projects never leak in).
pub(crate) fn claude_candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    let cwd_slug = slug_for(cwd);
    let mut scoped: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in ancestors_of(cwd) {
        let t = transcripts_in_project(&slug_for(&dir));
        if !t.is_empty() {
            scoped = t;
            break;
        }
    }
    scoped.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    scoped
        .into_iter()
        .map(|(mtime, path)| {
            let proj_slug = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let project = proj_slug
                .rsplit('-')
                .next()
                .unwrap_or(&proj_slug)
                .to_string();
            Candidate {
                path: path.clone(),
                mtime,
                project,
                snippet: first_user_snippet(&path),
                cwd_affinity: proj_slug == cwd_slug,
                agent: Agent::Claude,
            }
        })
        .collect()
}

fn first_user_snippet(path: &Path) -> String {
    use std::io::{BufRead, BufReader};
    let Ok(f) = std::fs::File::open(path) else {
        return String::new();
    };
    for line in BufReader::new(f).lines().take(80).map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("user") {
            if let Some(s) = v.pointer("/message/content").and_then(|c| c.as_str()) {
                let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                return s.chars().take(72).collect();
            }
        }
    }
    String::new()
}

/// All sessions as pickable candidates, ranked most-recent first.
///
/// To avoid reading a snippet from *every* transcript on the machine, discovery
/// is scoped: walk from the cwd up to `$HOME` and use the **nearest ancestor
/// directory that has any sessions**. Only if nothing matches up to `$HOME` do we
/// fall back to scanning every project.
pub fn candidates() -> Vec<Candidate> {
    let cwd_slug = std::env::current_dir().ok().map(|d| slug_for(&d));

    // Nearest ancestor (cwd → … → $HOME) that owns any sessions.
    let mut scoped: Vec<(SystemTime, PathBuf)> = Vec::new();
    for dir in ancestor_dirs() {
        let t = transcripts_in_project(&slug_for(&dir));
        if !t.is_empty() {
            scoped = t;
            break;
        }
    }

    let entries: Vec<PathBuf> = if scoped.is_empty() {
        all_transcripts() // fallback: nothing local up to $HOME
    } else {
        scoped.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
        scoped.into_iter().map(|(_, p)| p).collect()
    };

    let mut out: Vec<Candidate> = Vec::new();
    for path in entries {
        let mtime = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let proj_slug = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let project = proj_slug
            .rsplit('-')
            .next()
            .unwrap_or(&proj_slug)
            .to_string();
        let cwd_affinity = cwd_slug.as_deref() == Some(proj_slug.as_str());
        out.push(Candidate {
            path: path.clone(),
            mtime,
            project,
            snippet: first_user_snippet(&path),
            cwd_affinity,
            agent: Agent::Claude,
        });
    }
    out.sort_by(|a, b| {
        b.cwd_affinity
            .cmp(&a.cwd_affinity)
            .then(b.mtime.cmp(&a.mtime))
    });
    out
}

/// The newest Claude transcript for `cwd` **or its nearest ancestor that has one**:
/// the session id (filename stem), path, and mtime — never a session from an
/// unrelated directory. Used by the `agent-jdi` Claude adapter to pick a resume
/// target, so `resume` in a directory with no history fails cleanly rather than
/// grabbing some other project's session.
pub fn latest_for_cwd(cwd: &Path) -> Option<(String, PathBuf, SystemTime)> {
    for anc in ancestors_of(cwd) {
        let mut ts = transcripts_in_project(&slug_for(&anc));
        ts.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
        if let Some((m, p)) = ts.into_iter().next() {
            let id = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            return Some((id, p, m));
        }
    }
    None
}

/// The deterministic transcript path for a Claude `session_id` in `cwd` — the file
/// Claude Code *will* write. May not exist yet (used by `agent-jdi start` to follow
/// a fresh run whose id was pinned via `--session-id`).
pub fn claude_transcript_path(cwd: &Path, id: &str) -> PathBuf {
    projects_dir()
        .join(slug_for(cwd))
        .join(format!("{id}.jsonl"))
}

/// Find a Claude transcript by session id (`<id>.jsonl`) anywhere under the projects
/// dir.
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    let needle = format!("{id}.jsonl");
    all_transcripts()
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(needle.as_str()))
}

/// Sessions for the current directory across **every** agent, filtered to `only`
/// when set (else all agents), sorted cwd-matches-first then most-recent.
pub fn candidates_all(only: Option<Agent>) -> Vec<Candidate> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut out: Vec<Candidate> = Vec::new();
    // Each agent is scoped to cwd-or-nearest-ancestor-with-sessions, with no global
    // fallback — so a session for an unrelated directory never shows here.
    if only != Some(Agent::Codex) {
        out.extend(claude_candidates_scoped(&cwd));
    }
    if only != Some(Agent::Claude) {
        out.extend(crate::codex_discover::candidates_scoped(&cwd));
    }
    out.sort_by(|a, b| {
        b.cwd_affinity
            .cmp(&a.cwd_affinity)
            .then(b.mtime.cmp(&a.mtime))
    });
    out
}

/// Auto-detect which agent wrote a transcript by sniffing its first lines: a
/// Codex rollout opens with a `session_meta` event and wraps events in `payload`;
/// a Claude transcript has top-level `sessionId`/`message`. Defaults to Claude.
/// The working directory a session ran in, read from the transcript head — the
/// top-level `cwd` (Claude) or `payload.cwd` of `session_meta` (Codex). Used to
/// resolve a header's relativized path back to an absolute one (for reveal-in-
/// file-manager). `None` when no cwd is recorded.
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

pub fn detect_agent(path: &Path) -> Agent {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return Agent::Claude;
    };
    for line in std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .take(5)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let ty = v.get("type").and_then(Value::as_str);
        if ty == Some("session_meta")
            || (v.get("payload").is_some()
                && matches!(ty, Some("response_item" | "turn_context" | "event_msg")))
        {
            return Agent::Codex;
        }
        if v.get("sessionId").is_some() || v.get("message").is_some() {
            return Agent::Claude;
        }
    }
    Agent::Claude
}

/// Resolve a transcript across agents (honoring the `only` filter): an existing
/// file path (agent auto-detected on open), a session id searched in each agent's
/// store, or — with `latest` — the most-recent transcript across agents.
pub fn resolve_any(only: Option<Agent>, target: Option<&str>, latest: bool) -> Result<PathBuf> {
    if let Some(t) = target {
        let as_path = PathBuf::from(t);
        if as_path.is_file() {
            return Ok(as_path);
        }
        // Session id: look in each in-scope agent's store.
        if only != Some(Agent::Codex) {
            let needle = format!("{t}.jsonl");
            if let Some(hit) = all_transcripts()
                .into_iter()
                .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(needle.as_str()))
            {
                return Ok(hit);
            }
        }
        if only != Some(Agent::Claude) {
            if let Ok(hit) = crate::codex_discover::resolve(Some(t), false) {
                return Ok(hit);
            }
        }
        return Err(anyhow!(
            "no transcript found for '{t}' (not a file, and no session id match)"
        ));
    }
    if latest {
        // Scoped to the cwd or its nearest ancestor that has sessions — NOT the
        // global newest. `candidates_all` sorts cwd-matches first, then most-recent,
        // with no global fallback, so an unrelated directory's session never wins.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return candidates_all(only)
            .into_iter()
            .next()
            .map(|c| c.path)
            .ok_or_else(|| anyhow!("no session found for {} or its ancestors", cwd.display()));
    }
    Err(anyhow!(
        "give a session id or a path, or use --latest (no session picker yet)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_code_convention() {
        let p = Path::new("/Users/dev/projects/claude-toolbox");
        assert_eq!(slug_for(p), "-Users-dev-projects-claude-toolbox");
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

        assert_eq!(detect_agent(&codex), Agent::Codex);
        assert_eq!(detect_agent(&claude), Agent::Claude);
        // A missing/empty file falls back to Claude.
        assert_eq!(detect_agent(Path::new("/nonexistent.jsonl")), Agent::Claude);

        std::fs::remove_file(&codex).ok();
        std::fs::remove_file(&claude).ok();
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
}
