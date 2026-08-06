//! **Claude's transcript discovery** — the Claude half of the shared `discover` interface
//! (mirrors `codex_discover`). Locates Claude Code's per-project transcripts under
//! `~/.claude/projects/<slug>/<id>.jsonl`, scoped to the cwd or its nearest ancestor with
//! sessions. The agent-neutral pieces — the [`Candidate`] type, `ancestors_below`,
//! `detect_agent`, `session_cwd`, and the cross-agent `resolve_any`/`candidates_all`
//! dispatchers — live in the facade crate's `discover`.

use claude_replay_engine::seam::{Agent, Candidate, SessionCard};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How much of a transcript's tail to scan for the title lines.
///
/// They are appended *repeatedly* as a session evolves, so the current value is the LAST
/// occurrence — which makes this a tail read, not a head read. 256 KiB is the same window
/// `agent-jdi` already uses for its in-flight-tool check, and it comfortably spans the last
/// several turns of even a very chatty session.
const TAIL_BYTES: u64 = 256 * 1024;

/// Root under which Claude Code writes per-project transcript dirs.
pub fn projects_dir() -> PathBuf {
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
    claude_replay_engine::seam::ancestors_below(cwd, home)
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
        Agent::CLAUDE,
        cwd,
        claude_replay_engine::seam::home_dir().as_deref(),
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
        claude_replay_engine::seam::home_dir().as_deref(),
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
/// Claude's half of `TranscriptAdapter::session_card`: the session's name and its most recent
/// prompt, from the tail of the transcript.
///
/// Claude Code writes three line types and **rewrites them as the session evolves**:
/// `custom-title` (`customTitle` — what the user named it), `ai-title` (`aiTitle` — what the
/// agent named it) and `last-prompt` (`lastPrompt`). Each is taken from its LAST occurrence, and
/// a user's name beats a generated one.
///
/// Bounded: the last [`TAIL_BYTES`] only, and the first (possibly severed) line of that window is
/// discarded rather than parsed. `None` when the file is unreadable or names nothing — a session
/// with no title is the normal early state, not an error.
pub(crate) fn session_card(path: &Path) -> Option<SessionCard> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::with_capacity((len - from) as usize);
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut card = SessionCard::default();
    let mut custom: Option<String> = None;
    // Skip a severed first line when the window started mid-file: it cannot be parsed, and
    // guessing at half a record is how a truncated title reaches the UI.
    let lines = text.lines().skip(usize::from(from > 0));
    for l in lines {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
            continue;
        };
        let field = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => custom = field("customTitle").or(custom),
            Some("ai-title") => card.title = field("aiTitle").or(card.title.take()),
            Some("last-prompt") => {
                card.last_prompt = field("lastPrompt").or(card.last_prompt.take())
            }
            _ => {}
        }
    }
    // A name the user chose outranks one the agent generated.
    if custom.is_some() {
        card.title = custom;
    }
    (!card.is_empty()).then_some(card)
}

pub(crate) fn load_tasks(path: &Path) -> Option<claude_replay_engine::seam::TaskList> {
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
        if let Some(t) = claude_replay_engine::seam::task_from_json(&v) {
            items.push(t);
        }
    }
    (!items.is_empty()).then_some(claude_replay_engine::seam::TaskList { items })
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
            candidates_scoped_in(&root, Agent::QODERWORK, &cwd, Some(home)).is_empty(),
            "home-recorded sessions leaked into a subdir cwd"
        );
        // Sanity: the same store WITH a real project dir for the cwd still discovers it.
        std::fs::create_dir_all(root.join(slug_for(&cwd))).unwrap();
        std::fs::write(
            root.join(slug_for(&cwd)).join("h2.jsonl"),
            format!("{line}\n"),
        )
        .unwrap();
        let cands = candidates_scoped_in(&root, Agent::QODERWORK, &cwd, Some(home));
        assert_eq!(cands.len(), 1);
        assert!(cands[0].cwd_affinity);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn card_tmp(name: &str, body: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-card-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        std::fs::write(&p, body).unwrap();
        p
    }

    /// The three line types, and the two rules that decide between them: a title the USER set
    /// beats one the agent generated, and the LAST occurrence of each wins — Claude Code rewrites
    /// all three as the session evolves, so an earlier one is simply stale.
    #[test]
    fn session_card_prefers_the_users_title_and_the_latest_of_each() {
        let p = card_tmp(
            "prec",
            concat!(
                r#"{"type":"ai-title","aiTitle":"first guess"}"#,
                "\n",
                r#"{"type":"last-prompt","lastPrompt":"an early ask"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"My Name"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"second guess"}"#,
                "\n",
                r#"{"type":"last-prompt","lastPrompt":"the latest ask"}"#,
                "\n",
            ),
        );
        let c = session_card(&p).expect("named");
        assert_eq!(c.title.as_deref(), Some("My Name"), "the user's name wins");
        assert_eq!(
            c.last_prompt.as_deref(),
            Some("the latest ask"),
            "latest wins"
        );
        assert_eq!(c.label(), Some("My Name"));
    }

    /// With no user title, the agent's is the name — and it is still the LAST one.
    #[test]
    fn session_card_falls_back_to_the_agents_title() {
        let p = card_tmp(
            "ai",
            concat!(
                r#"{"type":"ai-title","aiTitle":"early"}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"current"}"#,
                "\n",
            ),
        );
        assert_eq!(session_card(&p).unwrap().title.as_deref(), Some("current"));
    }

    /// A session with only a prompt has no name, and `label` degrades to it — which is what a
    /// consumer with room for one line shows.
    #[test]
    fn session_card_degrades_to_the_last_prompt() {
        let p = card_tmp(
            "lastonly",
            "{\"type\":\"last-prompt\",\"lastPrompt\":\"do the thing\"}\n",
        );
        let c = session_card(&p).unwrap();
        assert_eq!(c.title, None);
        assert_eq!(c.label(), Some("do the thing"));
    }

    /// Nothing to say ⇒ `None`, not an empty card: a card with neither field is indistinguishable
    /// from no card, and a consumer must not have to check both.
    #[test]
    fn session_card_is_none_when_nothing_is_named() {
        let p = card_tmp(
            "plain",
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        );
        assert!(session_card(&p).is_none());
        assert!(session_card(Path::new("/nope/missing.jsonl")).is_none());
    }

    /// Blank values are not names. An agent that writes `""` must not blank out the row.
    #[test]
    fn session_card_ignores_empty_values() {
        let p = card_tmp(
            "blank",
            concat!(
                r#"{"type":"custom-title","customTitle":"Real"}"#,
                "\n",
                r#"{"type":"custom-title","customTitle":"   "}"#,
                "\n",
            ),
        );
        assert_eq!(session_card(&p).unwrap().title.as_deref(), Some("Real"));
    }

    /// **The tail is bounded, and its first line is severed.** A transcript larger than the
    /// window is read from the end, so the first line in view is half a record — parsing it is
    /// how a truncated title reaches the UI. The title must come from the tail regardless.
    #[test]
    fn session_card_reads_a_bounded_tail_and_skips_the_severed_line() {
        let filler = format!("{{\"type\":\"user\",\"pad\":\"{}\"}}\n", "x".repeat(4096));
        let mut body = String::new();
        while body.len() < (TAIL_BYTES as usize) + 64 * 1024 {
            body.push_str(&filler);
        }
        // A title BELOW the window must not be found; one inside it must.
        let mut early = String::from("{\"type\":\"custom-title\",\"customTitle\":\"too old\"}\n");
        early.push_str(&body);
        early.push_str("{\"type\":\"ai-title\",\"aiTitle\":\"in the tail\"}\n");
        let p = card_tmp("tail", &early);
        assert!(
            std::fs::metadata(&p).unwrap().len() > TAIL_BYTES,
            "fixture must exceed the window"
        );
        let c = session_card(&p).expect("the tail names it");
        assert_eq!(
            c.title.as_deref(),
            Some("in the tail"),
            "a title outside the window is out of scope, and the severed first line is skipped"
        );
    }
}
