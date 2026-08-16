//! **Claude's transcript discovery** — the Claude half of the shared `discover` interface
//! (mirrors `codex_discover`). Locates Claude Code's per-project transcripts under
//! `~/.claude/projects/<slug>/<id>.jsonl`, scoped to the cwd or its nearest ancestor with
//! sessions. The agent-neutral pieces — the [`Candidate`] type, `ancestors_below`,
//! `detect_agent`, `first_cwd`, and the cross-agent `resolve_any`/`candidates_all`
//! dispatchers — live in the facade crate's `discover`.

use claude_replay_engine::seam::{Agent, Candidate, CardMemo, CardOutcome, SessionCard};
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
                return s
                    .chars()
                    .take(claude_replay_engine::seam::SNIPPET_CHARS)
                    .collect();
            }
        }
    }
    String::new()
}

/// Every MAIN transcript in a Claude-format store, MACHINE-WIDE (#98): the top-level
/// `*.jsonl` of every project directory. Sub-agent transcripts live under
/// `<project>/<stem>/subagents/` — subdirectories, excluded by construction.
pub(crate) fn store_transcripts_in(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return out;
    };
    for p in projects.flatten() {
        if !p.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(p.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") && path.is_file() {
                out.push(path);
            }
        }
    }
    out
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
/// What Claude's `session_card` remembers between calls: how far it has already scanned, and
/// what it found. Versioned so a future shape change is a cache miss rather than a misread.
#[derive(serde::Serialize, serde::Deserialize)]
struct Memo {
    v: u8,
    /// Byte offset scanned up to — everything below it has been examined.
    at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_prompt: Option<String>,
    /// Whether `title` came from a `custom-title`. A user's name outranks a generated one, so an
    /// incremental scan must not let a later `ai-title` overwrite it.
    #[serde(default)]
    custom: bool,
}

const MEMO_V: u8 = 1;

/// Claude's half of `TranscriptAdapter::session_card`: the session's name and its most recent
/// prompt.
///
/// Claude Code writes three line types and **rewrites them as the session evolves**:
/// `custom-title` (`customTitle` — what the user named it), `ai-title` (`aiTitle` — what the
/// agent named it) and `last-prompt` (`lastPrompt`). Each is taken from its LAST occurrence, and
/// a user's name beats a generated one.
///
/// **Incremental.** With a memo, only the bytes appended since the last call are scanned; with
/// none — or one that cannot be trusted — the last [`TAIL_BYTES`] are. The three cases:
///
/// | file vs `memo.at` | meaning | work |
/// |---|---|---|
/// | equal | nothing appended | `Unchanged` — one `stat` |
/// | longer | appended | scan the append only |
/// | shorter | compacted, or a different file at this path | cold rescan of the tail |
///
/// The shrink case is a **rebuild, never a trust**: a shorter file invalidates the offset, and an
/// offset into a rewritten file names nothing. (The same rule, and the same reason, as #96's
/// resume.)
pub(crate) fn session_card(path: &Path, memo: Option<&CardMemo>) -> CardOutcome {
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
        return CardOutcome::Absent;
    };
    let prev: Option<Memo> = CardMemo::decode(memo).filter(|m: &Memo| m.v == MEMO_V && m.at <= len);

    // Resume where the last scan stopped, or cold-read the tail.
    let from = match &prev {
        Some(m) => m.at,
        None => len.saturating_sub(TAIL_BYTES),
    };
    if let Some(m) = &prev {
        if from == len {
            // Nothing appended. The memo still has to come back: its offset is the thing that
            // makes the NEXT call cheap too.
            return CardOutcome::Unchanged {
                memo: CardMemo::encode(m).unwrap_or_else(|| CardMemo::new(serde_json::Value::Null)),
            };
        }
    }

    let Some(text) = read_from(path, from, len) else {
        return CardOutcome::Absent;
    };
    // A cold read starts mid-file, so its first line is severed — parsing half a record is how a
    // truncated title reaches the UI. A resumed read starts exactly on a boundary we wrote.
    let skip_severed = prev.is_none() && from > 0;

    let mut title = prev.as_ref().and_then(|m| m.title.clone());
    let mut custom = prev.as_ref().is_some_and(|m| m.custom);
    let mut last_prompt = prev.as_ref().and_then(|m| m.last_prompt.clone());

    for l in text.lines().skip(usize::from(skip_severed)) {
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
            Some("custom-title") => {
                if let Some(t) = field("customTitle") {
                    title = Some(t);
                    custom = true;
                }
            }
            // A generated title never displaces one the user chose.
            Some("ai-title") => {
                if !custom {
                    title = field("aiTitle").or(title);
                }
            }
            Some("last-prompt") => last_prompt = field("lastPrompt").or(last_prompt),
            _ => {}
        }
    }

    let card = SessionCard { title, last_prompt };
    let next = CardMemo::encode(&Memo {
        v: MEMO_V,
        at: len,
        title: card.title.clone(),
        last_prompt: card.last_prompt.clone(),
        custom,
    });
    if card.is_empty() {
        // Nothing named yet. Still hand back the memo when we have one, so the next call resumes
        // instead of re-reading this tail — "no title yet" is the normal early state of every
        // session, and it is the one that would otherwise pay full price forever.
        return match next {
            Some(memo) if prev.is_some() => CardOutcome::Unchanged { memo },
            _ => CardOutcome::Absent,
        };
    }
    CardOutcome::Fresh { card, memo: next }
}

/// `[from, to)` of `path` as text, or `None` if it cannot be read.
fn read_from(path: &Path, from: u64, to: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::with_capacity((to.saturating_sub(from)) as usize);
    f.take(to.saturating_sub(from)).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub(crate) fn load_tasks(path: &Path) -> Option<claude_replay_engine::seam::TaskList> {
    load_tasks_in(&tasks_root(), path)
}

/// [`load_tasks`] over an arbitrary tasks-store root — shared with the Qoder store, whose
/// layout (`<root>/<sessionId>/<n>.json`) is identical to Claude's `~/.claude/tasks`.
pub(crate) fn load_tasks_in(
    root: &Path,
    path: &Path,
) -> Option<claude_replay_engine::seam::TaskList> {
    let id = path.file_stem()?.to_str()?;
    let dir = root.join(id);
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

    /// A picker title must not stop short of a wide terminal. The snippet cap is a MEMORY bound,
    /// not a display width — it used to be 72 chars, which with the picker's ~35 fixed columns
    /// made every title end around column 107 however wide the window was. The picker fits each
    /// row to the terminal itself, so discovery's job is only to keep enough for it to fit.
    #[test]
    fn a_long_first_prompt_survives_past_one_terminal_row() {
        let root = std::env::temp_dir().join(format!("cr-snippet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Path::new("/Users/dev");
        let cwd = home.join("w").join("repo");
        // 400 characters of prompt — longer than any single row, shorter than the cap.
        let prompt: String = std::iter::repeat_n("word ", 80).collect::<String>();
        let prompt = prompt.trim();
        let dir = root.join(slug_for(&cwd));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("s1.jsonl"),
            format!(
                "{{\"sessionId\":\"s1\",\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{prompt}\"}}}}\n"
            ),
        )
        .unwrap();

        let cands = candidates_scoped_in(&root, Agent::CLAUDE, &cwd, Some(home));
        assert_eq!(cands.len(), 1);
        assert_eq!(
            cands[0].snippet.chars().count(),
            prompt.chars().count(),
            "a prompt under the cap must survive whole — a 200-column terminal has room for it"
        );
        assert!(
            cands[0].snippet.chars().count() > 72,
            "the old cap would have stopped here"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `session_card` with no memo, unwrapped to the card — the shape the pre-memo tests wrote
    /// against, kept so they keep asserting the same behaviour.
    fn cold(p: &Path) -> Option<SessionCard> {
        match session_card(p, None) {
            CardOutcome::Fresh { card, .. } => Some(card),
            _ => None,
        }
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
        let c = cold(&p).expect("named");
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
        assert_eq!(cold(&p).unwrap().title.as_deref(), Some("current"));
    }

    /// A session with only a prompt has no name, and `label` degrades to it — which is what a
    /// consumer with room for one line shows.
    #[test]
    fn session_card_degrades_to_the_last_prompt() {
        let p = card_tmp(
            "lastonly",
            "{\"type\":\"last-prompt\",\"lastPrompt\":\"do the thing\"}\n",
        );
        let c = cold(&p).unwrap();
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
        assert!(cold(&p).is_none());
        assert!(cold(Path::new("/nope/missing.jsonl")).is_none());
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
        assert_eq!(cold(&p).unwrap().title.as_deref(), Some("Real"));
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
        let c = cold(&p).expect("the tail names it");
        assert_eq!(
            c.title.as_deref(),
            Some("in the tail"),
            "a title outside the window is out of scope, and the severed first line is skipped"
        );
    }

    fn append_to(p: &Path, s: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }
    const AI: &str = "{\"type\":\"ai-title\",\"aiTitle\":\"first\"}\n";
    const LP: &str = "{\"type\":\"last-prompt\",\"lastPrompt\":\"ask one\"}\n";

    /// **Nothing appended ⇒ `Unchanged`, and the memo comes back.** This is the case the whole
    /// interface exists for: the common answer, at the cost of one `stat`.
    #[test]
    fn an_unchanged_file_answers_unchanged_and_returns_its_memo() {
        let p = card_tmp("memo-idle", &format!("{AI}{LP}"));
        let CardOutcome::Fresh { card, memo } = session_card(&p, None) else {
            panic!("first call is Fresh")
        };
        assert_eq!(card.title.as_deref(), Some("first"));
        let memo = memo.expect("Claude always memoizes");

        match session_card(&p, Some(&memo)) {
            CardOutcome::Unchanged { memo: back } => {
                assert_eq!(back, memo, "the memo survives an unchanged call unchanged")
            }
            other => panic!("expected Unchanged, got {other:?}"),
        }
    }

    /// An append with **no** title line must keep the title the memo remembers — the incremental
    /// scan sees only the new bytes, so anything it does not re-find has to come from the memo.
    #[test]
    fn an_append_without_a_title_keeps_the_remembered_one() {
        let p = card_tmp("memo-keep", &format!("{AI}{LP}"));
        let CardOutcome::Fresh { memo, .. } = session_card(&p, None) else {
            panic!()
        };
        append_to(
            &p,
            "{\"type\":\"assistant\",\"message\":{\"content\":\"work\"}}\n",
        );

        let CardOutcome::Fresh { card, .. } = session_card(&p, memo.as_ref()) else {
            panic!("the file grew, so this is Fresh")
        };
        assert_eq!(card.title.as_deref(), Some("first"), "carried by the memo");
        assert_eq!(card.last_prompt.as_deref(), Some("ask one"));
    }

    /// An append that DOES rename the session wins over the memo — the memo is a starting point,
    /// not an answer.
    #[test]
    fn an_append_with_a_new_title_supersedes_the_memo() {
        let p = card_tmp("memo-new", &format!("{AI}{LP}"));
        let CardOutcome::Fresh { memo, .. } = session_card(&p, None) else {
            panic!()
        };
        append_to(&p, "{\"type\":\"ai-title\",\"aiTitle\":\"second\"}\n");
        let CardOutcome::Fresh { card, .. } = session_card(&p, memo.as_ref()) else {
            panic!()
        };
        assert_eq!(card.title.as_deref(), Some("second"));
    }

    /// A user's name outranks a generated one **across calls** too: the memo has to carry the
    /// fact that the title was user-set, or the next `ai-title` in an append silently wins.
    #[test]
    fn a_custom_title_survives_a_later_ai_title_in_an_append() {
        let p = card_tmp(
            "memo-custom",
            "{\"type\":\"custom-title\",\"customTitle\":\"Mine\"}\n",
        );
        let CardOutcome::Fresh { card, memo } = session_card(&p, None) else {
            panic!()
        };
        assert_eq!(card.title.as_deref(), Some("Mine"));
        append_to(&p, "{\"type\":\"ai-title\",\"aiTitle\":\"generated\"}\n");
        let CardOutcome::Fresh { card, .. } = session_card(&p, memo.as_ref()) else {
            panic!()
        };
        assert_eq!(
            card.title.as_deref(),
            Some("Mine"),
            "a generated title must not displace the user's, even a call later"
        );
    }

    /// **A shrunk file is a rebuild, never a trust.** An offset into a rewritten file names
    /// nothing, so the memo is discarded and the tail rescanned.
    #[test]
    fn a_shrunk_file_discards_the_memo_and_rescans() {
        let p = card_tmp("memo-shrink", &format!("{AI}{LP}"));
        let CardOutcome::Fresh { memo, .. } = session_card(&p, None) else {
            panic!()
        };
        // Compaction: a different, shorter file at the same path, with a different name.
        std::fs::write(&p, "{\"type\":\"ai-title\",\"aiTitle\":\"rebuilt\"}\n").unwrap();
        let CardOutcome::Fresh { card, .. } = session_card(&p, memo.as_ref()) else {
            panic!()
        };
        assert_eq!(
            card.title.as_deref(),
            Some("rebuilt"),
            "the stale offset must not be believed"
        );
    }

    /// A memo that is foreign, stale-format, or garbage is a **cache miss**, never an error —
    /// the rule that makes the memo safe to persist across upgrades.
    #[test]
    fn an_unusable_memo_falls_back_to_the_cold_path() {
        let p = card_tmp("memo-junk", &format!("{AI}{LP}"));
        for junk in [
            serde_json::json!("not an object"),
            serde_json::json!({"v": 99, "at": 0}),
            serde_json::json!({"at": "not a number"}),
            serde_json::json!({}),
        ] {
            let m = CardMemo::new(junk.clone());
            match session_card(&p, Some(&m)) {
                CardOutcome::Fresh { card, .. } => {
                    assert_eq!(
                        card.title.as_deref(),
                        Some("first"),
                        "cold path still works"
                    )
                }
                other => panic!("{junk} should cold-path, got {other:?}"),
            }
        }
    }

    /// A session with no title yet still memoizes — that is the state most sessions are in early,
    /// and the one that would otherwise re-read its tail on every single refresh forever.
    #[test]
    fn an_unnamed_session_still_memoizes_after_the_first_call() {
        let p = card_tmp("memo-unnamed", "{\"type\":\"user\",\"m\":1}\n");
        assert!(
            matches!(session_card(&p, None), CardOutcome::Absent),
            "nothing named, and nothing to resume from yet"
        );
        // Once it HAS been scanned with a memo in hand, an unchanged unnamed file is Unchanged.
        let m = CardMemo::encode(&Memo {
            v: MEMO_V,
            at: std::fs::metadata(&p).unwrap().len(),
            title: None,
            last_prompt: None,
            custom: false,
        })
        .unwrap();
        assert!(matches!(
            session_card(&p, Some(&m)),
            CardOutcome::Unchanged { .. }
        ));
    }

    /// The incremental path must reach the same answer as a cold read — the equivalence that
    /// makes memoization safe rather than merely fast.
    #[test]
    fn incremental_equals_cold() {
        let p = card_tmp("memo-equiv", &format!("{AI}{LP}"));
        let mut memo = match session_card(&p, None) {
            CardOutcome::Fresh { memo, .. } => memo,
            _ => panic!(),
        };
        for i in 0..12 {
            append_to(&p, &format!("{{\"type\":\"user\",\"n\":{i}}}\n"));
            if i % 4 == 0 {
                append_to(
                    &p,
                    &format!("{{\"type\":\"ai-title\",\"aiTitle\":\"t{i}\"}}\n"),
                );
            }
            if i % 3 == 0 {
                append_to(
                    &p,
                    &format!("{{\"type\":\"last-prompt\",\"lastPrompt\":\"p{i}\"}}\n"),
                );
            }
            memo = match session_card(&p, memo.as_ref()) {
                CardOutcome::Fresh { memo, .. } => memo,
                CardOutcome::Unchanged { memo } => Some(memo),
                CardOutcome::Absent => None,
            };
            let incremental = match session_card(&p, memo.as_ref()) {
                CardOutcome::Fresh { card, .. } => Some(card),
                CardOutcome::Unchanged { .. } => cold(&p), // unchanged ⇒ the caller's card stands
                CardOutcome::Absent => None,
            };
            assert_eq!(incremental, cold(&p), "step {i}: incremental == cold");
        }
    }
}
