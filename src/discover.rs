//! Locating a session transcript: by explicit path, by session id, or `--latest`.

use anyhow::{anyhow, Result};
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
pub struct Candidate {
    pub path: PathBuf,
    pub mtime: SystemTime,
    pub project: String,    // human-ish project name (last path segment)
    pub snippet: String,    // first user message, truncated
    pub cwd_affinity: bool, // belongs to the current working directory's project
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

/// Directories from the cwd up to (and including) `$HOME` — the ancestors we
/// probe for a matching project, nearest first.
fn ancestor_dirs() -> Vec<PathBuf> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let mut dirs = Vec::new();
    let mut cur: Option<&Path> = Some(cwd.as_path());
    while let Some(d) = cur {
        dirs.push(d.to_path_buf());
        if home.as_deref() == Some(d) {
            break; // don't climb above $HOME
        }
        cur = d.parent();
    }
    dirs
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
        });
    }
    out.sort_by(|a, b| {
        b.cwd_affinity
            .cmp(&a.cwd_affinity)
            .then(b.mtime.cmp(&a.mtime))
    });
    out
}

/// Resolve the transcript to open.
///
/// - `target` that is an existing file path -> that file.
/// - `target` that looks like a session id -> `<projects>/<slug>/<id>.jsonl`.
/// - otherwise, if `latest`, the most-recently-modified transcript anywhere.
pub fn resolve(target: Option<&str>, latest: bool) -> Result<PathBuf> {
    if let Some(t) = target {
        let as_path = PathBuf::from(t);
        if as_path.is_file() {
            return Ok(as_path);
        }
        // Treat as a session id: search for "<id>.jsonl" under the projects dir.
        let needle = format!("{t}.jsonl");
        if let Some(hit) = all_transcripts()
            .into_iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(needle.as_str()))
        {
            return Ok(hit);
        }
        return Err(anyhow!(
            "no transcript found for '{t}' (not a file, and no session id match under {})",
            projects_dir().display()
        ));
    }
    if latest {
        return all_transcripts()
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no transcripts found under {}", projects_dir().display()));
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
