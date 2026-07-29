//! Small OS/path helpers shared by BOTH frontends (the TUI's dump path and the
//! HTML exporters/server) — no terminal or HTML dependencies.

use claude_replay_core::discover;
use std::path::Path;

/// Reveal a path in the OS file manager (a benign, read-only side effect from an
/// explicit click). macOS: `open -R <file>` selects it in Finder / `open <dir>`
/// for a directory. Linux: `xdg-open` the containing directory. Spawned detached
/// so it never blocks or disturbs the TUI; failures are ignored (no file manager).
pub fn reveal_in_file_manager(path: &Path) {
    let is_dir = path.is_dir();
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if is_dir {
            cmd.arg(path);
        } else {
            cmd.arg("-R").arg(path);
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No generic reveal-and-select on Linux/other; open the folder itself.
        let dir = if is_dir {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Deduce the default dump stem: `<basename>-<pathhash>-<sessionid>-<width>` where
/// basename/pathhash come from the session's project cwd, sessionid is its first 6
/// chars, and width is the render width. cwd/sessionId are read from the transcript via
/// the agent-neutral `discover` helpers (so a Codex rollout — whose cwd/id live under
/// `payload` — deduces a correct stem, not the `"session"` fallback a Claude-only scan gave).
pub fn deduce_stem(path: &Path, width: Option<usize>) -> String {
    use std::hash::{Hash, Hasher};
    let cwd = discover::session_cwd(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let basename = Path::new(&cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("session");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    let pathhash: String = format!("{:016x}", h.finish())[..6].to_string();
    let sid = discover::session_id(path).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    });
    let sid6: String = sid.chars().take(6).collect();
    // `--dump` suffixes the render width (its output is width-specific); the HTML
    // export reflows in the browser, so it passes `None` and omits it.
    match width {
        Some(w) => format!("{basename}-{pathhash}-{sid6}-{w}"),
        None => format!("{basename}-{pathhash}-{sid6}"),
    }
}
