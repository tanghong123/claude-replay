//! Path relativization shared by every agent parser (was duplicated in `model.rs`
//! and `codex_model.rs`).

/// Make an absolute path relative to the session's cwd when it sits under it
/// (matching how Claude Code shows tool targets — relative to the cwd recorded in
/// the transcript, NOT peek's runtime cwd); else leave it as-is.
pub fn relativize(p: &str, base: &str) -> String {
    relativize_with(p, base, std::env::var("HOME").ok().as_deref())
}

/// Make `p` relative to the session cwd `base` when it sits under it; else
/// abbreviate a `$HOME` prefix to `~` (matching Claude Code, which shows
/// out-of-project paths as `~/…`); else leave it absolute.
pub fn relativize_with(p: &str, base: &str, home: Option<&str>) -> String {
    let path = std::path::Path::new(p);
    if !base.is_empty() {
        if let Ok(r) = path.strip_prefix(base) {
            return r.display().to_string();
        }
    }
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        if let Ok(r) = path.strip_prefix(home) {
            return format!("~/{}", r.display());
        }
    }
    p.to_string()
}
