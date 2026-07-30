//! **Claude's transcript discovery** — the Claude half of the shared `discover` interface
//! (mirrors `codex_discover`). Locates Claude Code's per-project transcripts under
//! `~/.claude/projects/<slug>/<id>.jsonl`, scoped to the cwd or its nearest ancestor with
//! sessions. The agent-neutral pieces — the [`Candidate`] type, `ancestors_below`,
//! `detect_agent`, `session_cwd`, and the cross-agent `resolve_any`/`candidates_all`
//! dispatchers — live in [`crate::discover`].

use crate::engine::seam::{Agent, Candidate};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Root under which Claude Code writes per-project transcript dirs.
pub(crate) fn projects_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_PROJECTS_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".claude").join("projects")
}

/// All transcript files under a store root — shared with the QoderWork store, whose
/// on-disk layout (`<root>/<slug>/<id>.jsonl`) is identical to Claude Code's.
pub(crate) fn all_transcripts_in(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<(SystemTime, PathBuf)> = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
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

/// Transcript files inside one project dir (`<root>/slug`), with mtimes.
fn transcripts_in_project(root: &Path, slug: &str) -> Vec<(SystemTime, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join(slug)) else {
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

/// The `(mtime, path)` transcripts of the **nearest ancestor of `cwd`** (up the directory
/// chain, strictly inside `home` — #69) that owns any — the "no global fallback" scoping both
/// scoped Claude lookups share (a directory with no session history up its chain yields
/// nothing, so unrelated projects never leak in). Mirrors
/// `codex_discover::nearest_ancestor_sessions`.
fn nearest_project_transcripts(
    root: &Path,
    cwd: &Path,
    home: Option<&Path>,
) -> Vec<(SystemTime, PathBuf)> {
    crate::engine::seam::ancestors_below(cwd, home)
        .into_iter()
        .map(|dir| transcripts_in_project(root, &slug_for(&dir)))
        .find(|t| !t.is_empty())
        .unwrap_or_default()
}

/// Claude sessions scoped strictly to `cwd` or its nearest ancestor that has sessions — no
/// global fallback (see `nearest_project_transcripts`).
pub fn candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    candidates_scoped_in(
        &projects_dir(),
        Agent::Claude,
        cwd,
        crate::engine::seam::home_dir().as_deref(),
    )
}

/// [`candidates_scoped`] over an arbitrary Claude-layout store root, tagging candidates with
/// `agent` — shared with the QoderWork store. `home` bounds the ancestor probe (#69).
pub(crate) fn candidates_scoped_in(
    root: &Path,
    agent: Agent,
    cwd: &Path,
    home: Option<&Path>,
) -> Vec<Candidate> {
    let cwd_slug = slug_for(cwd);
    let mut scoped = nearest_project_transcripts(root, cwd, home);
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
                agent,
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

/// The newest Claude transcript for `cwd` **or its nearest ancestor that has one**:
/// the session id (filename stem), path, and mtime — never a session from an
/// unrelated directory. Used by the `agent-jdi` Claude adapter to pick a resume
/// target, so `resume` in a directory with no history fails cleanly rather than
/// grabbing some other project's session.
pub fn latest_for_cwd(cwd: &Path) -> Option<(String, PathBuf, SystemTime)> {
    let mut ts = nearest_project_transcripts(
        &projects_dir(),
        cwd,
        crate::engine::seam::home_dir().as_deref(),
    );
    ts.sort_by_key(|(m, _)| std::cmp::Reverse(*m));
    ts.into_iter().next().map(|(m, p)| {
        let id = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        (id, p, m)
    })
}

/// The deterministic transcript path for a Claude `session_id` in `cwd` — the file
/// Claude Code *will* write. May not exist yet (used by `agent-jdi start` to follow
/// a fresh run whose id was pinned via `--session-id`).
pub fn transcript_path(cwd: &Path, id: &str) -> PathBuf {
    projects_dir()
        .join(slug_for(cwd))
        .join(format!("{id}.jsonl"))
}

/// Find a Claude transcript by session id (`<id>.jsonl`) anywhere under the projects
/// dir. This is Claude's half of `discover::resolve_any`'s id lookup (mirroring
/// `codex_discover::resolve`).
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    transcript_by_id_in(&projects_dir(), id)
}

/// [`transcript_by_id`] over an arbitrary Claude-layout store root.
pub(crate) fn transcript_by_id_in(root: &Path, id: &str) -> Option<PathBuf> {
    let needle = format!("{id}.jsonl");
    all_transcripts_in(root)
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(needle.as_str()))
}

/// The Claude tasks store root: `~/.claude/tasks`, or the `CLAUDE_JDI_TASKS_ROOT`
/// override (kept name-compatible with agent-jdi's, which reads the same store).
fn tasks_root() -> PathBuf {
    std::env::var_os("CLAUDE_JDI_TASKS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".claude").join("tasks")
        })
}

/// The LIVE on-disk task list for the session transcript at `path` (#15): the session
/// id (== the `.jsonl` stem) names `~/.claude/tasks/<id>/<n>.json`, one file per task,
/// ordered numerically (a string sort gives 18, 19, 2, 20). `None` when the dir is
/// missing or nothing parses — the caller then falls back to the transcript's op-log.
/// This is Claude's half of `discover::session_tasks` (the `TranscriptAdapter::load_tasks`
/// hook); files may be pruned/gc'd, which the op-log merge backfills.
pub(crate) fn load_tasks(path: &Path) -> Option<crate::engine::seam::TaskList> {
    let id = path.file_stem()?.to_str()?;
    let dir = tasks_root().join(id);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    entries.sort_by_key(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    });
    let mut items = Vec::new();
    for p in entries {
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(t) = crate::engine::seam::task_from_json(&v) {
            items.push(t);
        }
    }
    (!items.is_empty()).then_some(crate::engine::seam::TaskList { items })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_code_convention() {
        let p = Path::new("/Users/dev/projects/claude-toolbox");
        assert_eq!(slug_for(p), "-Users-dev-projects-claude-toolbox");
    }

    /// #69: a store dir recorded AT the home directory (QoderWork writes some
    /// sessions' project cwd as `$HOME`, growing a `-Users-<name>` dir) must never
    /// match — the ancestor probe stops strictly below home, so a cwd inside home
    /// with no session history of its own finds NOTHING rather than home's sessions.
    #[test]
    fn sessions_recorded_at_home_never_match_a_subdir_cwd() {
        let root = std::env::temp_dir().join(format!("cr-home-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Path::new("/Users/dev");
        let cwd = home.join("w").join("repo");
        let line = r#"{"sessionId":"h1","type":"user","message":{"role":"user","content":"hi"}}"#;
        // The misbehaving store: ONLY a project dir for home itself (and one for `/`, #62).
        for slug in [slug_for(home), "-".to_string()] {
            std::fs::create_dir_all(root.join(&slug)).unwrap();
            std::fs::write(root.join(&slug).join("h1.jsonl"), format!("{line}\n")).unwrap();
        }
        assert!(
            candidates_scoped_in(&root, Agent::QoderWork, &cwd, Some(home)).is_empty(),
            "home-recorded sessions leaked into a subdir cwd"
        );
        // Sanity: the same store WITH a real project dir for the cwd still discovers it.
        std::fs::create_dir_all(root.join(slug_for(&cwd))).unwrap();
        std::fs::write(
            root.join(slug_for(&cwd)).join("h2.jsonl"),
            format!("{line}\n"),
        )
        .unwrap();
        let cands = candidates_scoped_in(&root, Agent::QoderWork, &cwd, Some(home));
        assert_eq!(cands.len(), 1);
        assert!(cands[0].cwd_affinity);
        let _ = std::fs::remove_dir_all(&root);
    }
}
