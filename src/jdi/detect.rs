//! Auto-detect which agent to operate on for a directory, and the deprecation
//! handoff from the bash `claude-jdi`.

use super::agent;
use crate::Agent;
use std::path::{Path, PathBuf};

/// Pick the agent for `cwd`: honor `forced` if given, else the agent whose newest
/// resumable session is the most recent (smallest idle). `None` if neither agent has
/// a resumable session here.
pub fn agent_for(cwd: &Path, forced: Option<Agent>) -> Option<Agent> {
    if let Some(a) = forced {
        return Some(a);
    }
    let mut best: Option<(Agent, u64)> = None;
    for a in [Agent::Claude, Agent::Codex] {
        if let Ok(s) = agent::adapter(a).discover_resumable(cwd) {
            if best.map(|(_, idle)| s.idle_secs < idle).unwrap_or(true) {
                best = Some((a, s.idle_secs));
            }
        }
    }
    best.map(|(a, _)| a)
}

/// The bash claude-jdi's state root (honoring its legacy env names).
fn legacy_home() -> PathBuf {
    for var in ["CLAUDE_JDI_HOME", "CLAUDE_KEEP_HOME"] {
        if let Some(p) = std::env::var_os(var) {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude").join("claude-jdi")
}

/// The bash claude-jdi session dir that manages `cwd`, if any — found by matching a
/// `cwd=` line in each legacy `meta` (avoids replicating its sha1 slot-key).
fn legacy_session_for_cwd(cwd: &Path) -> Option<PathBuf> {
    let wanted = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let entries = std::fs::read_dir(legacy_home()).ok()?;
    for e in entries.flatten() {
        let meta = e.path().join("meta");
        let Ok(text) = std::fs::read_to_string(&meta) else {
            continue;
        };
        if let Some(recorded) = text.lines().find_map(|l| l.strip_prefix("cwd=")) {
            let recorded =
                std::fs::canonicalize(recorded).unwrap_or_else(|_| PathBuf::from(recorded));
            if recorded == wanted {
                return Some(e.path());
            }
        }
    }
    None
}

/// If `cwd` was previously managed by the bash `claude-jdi`, drop a marker so that
/// tool warns the user to switch to `agent-jdi`. Returns whether a marker was set.
pub fn mark_legacy_superseded(cwd: &Path) -> bool {
    let Some(dir) = legacy_session_for_cwd(cwd) else {
        return false;
    };
    let marker = dir.join(".superseded-by-agent-jdi");
    if !marker.exists() {
        std::fs::write(&marker, "This session is now managed by agent-jdi.\n").ok();
    }
    true
}
