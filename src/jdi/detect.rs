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

/// If the bash `claude-jdi` is **actively supervising** `cwd` right now, return its
/// live worker pid — so `agent-jdi` can refuse to also manage the same directory
/// (two supervisors on one session would fight). Matches on a `cwd=` line in each
/// legacy `meta` whose recorded `pid` is alive. `None` = no conflict.
pub fn claude_jdi_live_for_cwd(cwd: &Path) -> Option<u32> {
    let wanted = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let entries = std::fs::read_dir(legacy_home()).ok()?;
    for e in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path().join("meta")) else {
            continue;
        };
        let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k));
        let Some(mcwd) = field("cwd=") else {
            continue;
        };
        let mcwd = std::fs::canonicalize(mcwd).unwrap_or_else(|_| PathBuf::from(mcwd));
        if mcwd != wanted {
            continue;
        }
        if let Some(pid) = field("pid=").and_then(|p| p.trim().parse::<u32>().ok()) {
            if super::state::pid_alive(pid) {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_jdi_live_is_detected_only_when_its_pid_is_alive() {
        let base = std::env::temp_dir().join(format!("ajdi-legacy-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let home = base.join("claude-jdi");
        let cwd = base.join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let sess = home.join("sess-abc");
        std::fs::create_dir_all(&sess).unwrap();
        std::env::set_var("CLAUDE_JDI_HOME", &home);

        // A live claude-jdi session for cwd (our own pid) → conflict.
        std::fs::write(
            sess.join("meta"),
            format!("cwd={}\npid={}\n", cwd.display(), std::process::id()),
        )
        .unwrap();
        assert_eq!(claude_jdi_live_for_cwd(&cwd), Some(std::process::id()));

        // A dead pid → no conflict.
        std::fs::write(
            sess.join("meta"),
            format!("cwd={}\npid=999999999\n", cwd.display()),
        )
        .unwrap();
        assert_eq!(claude_jdi_live_for_cwd(&cwd), None);

        // A different cwd → no conflict.
        assert_eq!(claude_jdi_live_for_cwd(&base.join("other")), None);

        std::env::remove_var("CLAUDE_JDI_HOME");
        std::fs::remove_dir_all(&base).ok();
    }
}
