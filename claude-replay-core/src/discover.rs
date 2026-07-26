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

/// Directories from `cwd` up to (and including) `$HOME` — the ancestors we probe
/// for a matching project, nearest first. Never climbs above `$HOME`. Agent-neutral;
/// each adapter maps these to its own store layout.
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

/// [`ancestors_of`] the current working directory.
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
    if only != Some(Agent::Codex) {
        out.extend(crate::claude_discover::claude_candidates_scoped(&cwd));
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

/// Auto-detect which agent wrote a transcript by sniffing its first lines: a
/// Codex rollout opens with a `session_meta` event and wraps events in `payload`;
/// a Claude transcript has top-level `sessionId`/`message`. Defaults to Claude.
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
        // Session id: look in each in-scope agent's store via its adapter.
        if only != Some(Agent::Codex) {
            if let Some(hit) = crate::claude_discover::transcript_by_id(t) {
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
