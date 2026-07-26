//! **Claude's transcript discovery** — the Claude half of the shared `discover` interface
//! (mirrors `codex_discover`). Locates Claude Code's per-project transcripts under
//! `~/.claude/projects/<slug>/<id>.jsonl`, scoped to the cwd or its nearest ancestor with
//! sessions. The agent-neutral pieces — the [`Candidate`] type, `ancestors_of`/`ancestor_dirs`,
//! `detect_agent`, `session_cwd`, and the cross-agent `resolve_any`/`candidates_all`
//! dispatchers — live in [`crate::discover`].

use crate::discover::{ancestor_dirs, ancestors_of, Candidate};
use crate::Agent;
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

/// Claude sessions scoped strictly to `cwd` or its **nearest ancestor that has
/// sessions** — no global fallback (a directory with no session history up its
/// chain yields nothing, so unrelated projects never leak in).
pub fn claude_candidates_scoped(cwd: &Path) -> Vec<Candidate> {
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

/// All Claude sessions as pickable candidates, ranked most-recent first.
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
/// dir. This is Claude's half of `discover::resolve_any`'s id lookup (mirroring
/// `codex_discover::resolve`).
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    let needle = format!("{id}.jsonl");
    all_transcripts()
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(needle.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_code_convention() {
        let p = Path::new("/Users/dev/projects/claude-toolbox");
        assert_eq!(slug_for(p), "-Users-dev-projects-claude-toolbox");
    }
}
