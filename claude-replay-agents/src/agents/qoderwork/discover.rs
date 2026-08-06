//! **QoderWork's transcript discovery** — the QoderWork half of the shared `discover`
//! interface. QoderWork is a Claude-Code-format client whose store layout is identical
//! (`~/.qoderwork/projects/<slug>/<id>.jsonl`, same slug convention), so this is a thin
//! wrapper over `claude_discover`'s root-parameterized internals — only the root (and the
//! agent tag on candidates) differ. Parsing likewise delegates to the Claude implementations
//! (see the `QoderWorkAdapter` in `adapter.rs`); the one format difference is the
//! `runtime-config` head line, which drives detection.

use claude_replay_engine::seam::{Agent, Candidate, CardMemo, CardOutcome, SessionCard};
use std::path::{Path, PathBuf};

/// Root under which QoderWork writes per-project transcript dirs.
pub(crate) fn projects_dir() -> PathBuf {
    if let Ok(p) = std::env::var("QODERWORK_PROJECTS_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".qoderwork").join("projects")
}

/// QoderWork sessions scoped strictly to `cwd` or its nearest ancestor that has sessions —
/// the same no-global-fallback scoping as the Claude store.
pub fn candidates_scoped(cwd: &Path) -> Vec<Candidate> {
    crate::agents::claude::discover::candidates_scoped_in(
        &projects_dir(),
        Agent::QODERWORK,
        cwd,
        claude_replay_engine::seam::home_dir().as_deref(),
    )
}

/// Find a QoderWork transcript by session id (`<id>.jsonl`) anywhere under its projects dir.
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    crate::agents::claude::discover::transcript_by_id_in(&projects_dir(), id)
}

/// Where QoderWork keeps its chat metadata. Overridable so a test — or a non-standard install —
/// can point elsewhere; `None` when there is no database to read.
///
/// macOS-only by default because QoderWork is an Electron app that ships there; a build on
/// another platform simply finds nothing and every session falls back to its snippet.
#[cfg(feature = "qoderwork-titles")]
pub(crate) fn db_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("QODERWORK_DB").map(PathBuf::from) {
        return p.exists().then_some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let p = home.join("Library/Application Support/QoderWork/data/agents.db");
    p.exists().then_some(p)
}

/// What QoderWork's `session_card` remembers: the tail scan's state (delegated to Claude's, since
/// the transcript format is Claude's) plus the database row version the title came from.
#[derive(serde::Serialize, serde::Deserialize)]
struct Memo {
    v: u8,
    /// Claude's memo for the transcript half — `last_prompt` comes from the tail, as it does for
    /// Claude, because QoderWork writes `last-prompt` lines too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tail: Option<serde_json::Value>,
    /// `sub_chats.updated_at` at the time the title was read. The title lives in the DATABASE, so
    /// this is what "unchanged" means for it — the transcript can sit still while a rename lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    db_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

const MEMO_V: u8 = 1;

/// QoderWork's half of `TranscriptAdapter::session_card`.
///
/// The title is **not in the transcript** — QoderWork keeps it in SQLite, `sub_chats.name`, joined
/// on `session_id` = the transcript's stem. The most recent prompt *is* in the transcript, in the
/// same `last-prompt` lines Claude writes, so that half delegates to Claude's scanner rather than
/// being written twice.
///
/// That split is why the staleness rule cannot live in the caller: **a QoderWork title changes
/// when the database changes, with the transcript untouched.** Both halves are therefore checked,
/// and `Unchanged` needs both to agree.
pub(crate) fn session_card(path: &Path, memo: Option<&CardMemo>) -> CardOutcome {
    let prev: Option<Memo> = CardMemo::decode(memo).filter(|m: &Memo| m.v == MEMO_V);

    // The transcript half, via Claude's incremental scanner.
    let tail_memo = prev
        .as_ref()
        .and_then(|m| m.tail.clone())
        .map(CardMemo::new);
    let tail = crate::agents::claude::discover::session_card(path, tail_memo.as_ref());

    let (last_prompt, tail_next, tail_changed) = match tail {
        CardOutcome::Fresh { card, memo } => (card.last_prompt, memo, true),
        // "The card you have" — and the half of it we own is recorded in the memo, so recover it
        // from there rather than re-reading the file we were just told did not change.
        CardOutcome::Unchanged { memo } => {
            let lp = tail_last_prompt(&memo);
            (lp, Some(memo), false)
        }
        CardOutcome::Absent => (None, None, prev.is_some()),
    };

    // The title half, from the database.
    let (title, db_at) = match db_title(path) {
        Some((t, at)) => (Some(t), Some(at)),
        None => (None, None),
    };
    let db_changed = prev.as_ref().map(|m| m.db_at) != Some(db_at);

    let card = SessionCard { title, last_prompt };
    let next = CardMemo::encode(&Memo {
        v: MEMO_V,
        tail: tail_next.map(|m| m.value().clone()),
        db_at,
        title: card.title.clone(),
    });

    if prev.is_some() && !tail_changed && !db_changed {
        if let Some(memo) = next {
            return CardOutcome::Unchanged { memo };
        }
    }
    if card.is_empty() {
        return match next {
            Some(memo) if prev.is_some() => CardOutcome::Unchanged { memo },
            _ => CardOutcome::Absent,
        };
    }
    CardOutcome::Fresh { card, memo: next }
}

/// The `last_prompt` Claude's scanner stored in the memo it just handed back — an `Unchanged`
/// answer means "the card you have", and this recovers that half of it from the memo rather than
/// re-reading the file.
fn tail_last_prompt(memo: &CardMemo) -> Option<String> {
    memo.value()
        .get("last_prompt")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// `sub_chats.name` + `updated_at` for the session at `path`, or `None`.
///
/// Exact join first (`session_id` = the transcript stem — verified to resolve every row on a real
/// install). Falls back to the containing chat's name via the workspace slug, which covers
/// sessions the newer table does not know about; that title belongs to the CHAT and so may be
/// shared by several sessions, which is still better than a bare UUID.
#[cfg(feature = "qoderwork-titles")]
fn db_title(path: &Path) -> Option<(String, i64)> {
    let db = db_path()?;
    // Read-only, so a running QoderWork is never disturbed and we can never write its store.
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    let stem = path.file_stem()?.to_str()?;
    let exact: Option<(String, i64)> = conn
        .query_row(
            "select name, coalesce(updated_at, 0) from sub_chats where session_id = ?1",
            [stem],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    if let Some(hit) = exact.filter(|(n, _)| !n.trim().is_empty()) {
        return Some(hit);
    }
    // Fallback: the chat that owns this workspace directory.
    let slug = path.parent()?.file_name()?.to_str()?;
    let chat_id = slug.rsplit_once("workspace-")?.1;
    conn.query_row(
        "select name, coalesce(updated_at, 0) from chats where id = ?1",
        [chat_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
    .filter(|(n, _): &(String, i64)| !n.trim().is_empty())
}

/// Without the `qoderwork-titles` feature there is no database reader, so QoderWork sessions
/// carry only what the transcript says — exactly as they did before this existed.
#[cfg(not(feature = "qoderwork-titles"))]
fn db_title(_path: &Path) -> Option<(String, i64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The delegation guarantee: parsing a QoderWork transcript AS QoderWork is byte-identical
    /// to parsing it as Claude (same blocks, times, metrics) — the adapter adds detection and a
    /// store, never a format fork. Fixture mirrors the real shape: runtime-config head,
    /// multi-text user line, a tool call + result.
    #[test]
    fn qoderwork_parse_is_byte_identical_to_claude_parse() {
        let f = std::env::temp_dir().join(format!("qw-equiv-{}.jsonl", std::process::id()));
        std::fs::write(&f, concat!(
            r#"{"type":"runtime-config","sessionId":"s","model":"qwork-ultimate","timestamp":1785068132048}"#, "
",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>env</system-reminder>"},{"type":"text","text":"do it"}]},"timestamp":"2026-07-26T12:15:33Z"}"#, "
",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}],"model":"qwork-ultimate"},"timestamp":"2026-07-26T12:15:40Z"}"#, "
",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]},"timestamp":"2026-07-26T12:15:41Z"}"#, "
",
        )).unwrap();
        let qw = claude_replay_core::parse_session_as(Agent::QODERWORK, &f).unwrap();
        let cl = claude_replay_core::parse_session_as(Agent::CLAUDE, &f).unwrap();
        assert_eq!(
            format!("{:?}", qw.blocks()),
            format!("{:?}", cl.blocks()),
            "blocks identical under delegation"
        );
        assert_eq!(qw.user_times, cl.user_times);
        assert_eq!(qw.metrics, cl.metrics);
        assert_eq!(
            qw.agent,
            Agent::QODERWORK,
            "identity is the only difference"
        );
        let _ = std::fs::remove_file(&f);
    }

    /// Discovery over a fake QoderWork store: candidates come back tagged `QoderWork`, scoped
    /// to the cwd's slug, and a bare id resolves to its transcript — the picker/`--latest`/
    /// bare-id surface the Claude store already has, on the QoderWork root.
    #[test]
    fn discovers_and_resolves_from_the_qoderwork_store() {
        let root = std::env::temp_dir().join(format!("qw-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = Path::new("/Users/dev/proj");
        let slug = "-Users-dev-proj";
        std::fs::create_dir_all(root.join(slug)).unwrap();
        let mut f = std::fs::File::create(root.join(slug).join("abc123.jsonl")).unwrap();
        writeln!(f, r#"{{"type":"runtime-config","sessionId":"abc123","model":"qwork-ultimate","timestamp":1785068132048}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"hello qoderwork"}}]}},"timestamp":"2026-07-26T12:15:33Z"}}"#).unwrap();

        // Env-scoped: the override is process-global, so serialize against other env users.
        std::env::set_var("QODERWORK_PROJECTS_DIR", &root);
        // #69: through the PUBLIC surface (env $HOME), a cwd outside the real home
        // discovers nothing — even though its slug exists in the store.
        assert!(
            candidates_scoped(cwd).is_empty(),
            "cwd outside $HOME must not auto-discover"
        );
        let by_id = transcript_by_id("abc123");
        std::env::remove_var("QODERWORK_PROJECTS_DIR");
        // With the matching home bound, the store discovers and scopes normally.
        let cands = crate::agents::claude::discover::candidates_scoped_in(
            &root,
            Agent::QODERWORK,
            cwd,
            Some(Path::new("/Users/dev")),
        );

        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].agent, Agent::QODERWORK);
        assert!(cands[0].cwd_affinity, "scoped to the cwd's own slug");
        assert_eq!(
            by_id.as_deref(),
            Some(root.join(slug).join("abc123.jsonl").as_path())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Env vars are process-global; these tests point `QODERWORK_DB` at a fixture, so they must
    /// not run concurrently with each other.
    static DB_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(feature = "qoderwork-titles")]
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let d = std::env::temp_dir().join(format!("cr-qw-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // The project slug carries the chat id, which is the `chats` fallback's join key.
        let proj = d
            .join("projects")
            .join("-Users-x--qoderwork-workspace-mchat01");
        std::fs::create_dir_all(&proj).unwrap();
        let src = proj.join("aaaa-bbbb.jsonl");
        std::fs::write(
            &src,
            "{\"type\":\"last-prompt\",\"lastPrompt\":\"do the thing\"}\n",
        )
        .unwrap();
        let db = d.join("agents.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "create table sub_chats (id text, name text, chat_id text, session_id text, updated_at integer);
             create table chats (id text, name text, updated_at integer);",
        )
        .unwrap();
        (src, db)
    }

    #[cfg(feature = "qoderwork-titles")]
    fn card_of(src: &Path, memo: Option<&CardMemo>) -> CardOutcome {
        session_card(src, memo)
    }

    /// The exact join: `sub_chats.session_id` IS the transcript stem (verified against a real
    /// install, where it resolved every row). The transcript half still supplies `last_prompt`.
    #[cfg(feature = "qoderwork-titles")]
    #[test]
    fn title_comes_from_sub_chats_and_last_prompt_from_the_transcript() {
        let _g = DB_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let (src, db) = fixture("exact");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "insert into sub_chats values ('s1','初筛候选人简历','mchat01','aaaa-bbbb',100)",
                [],
            )
            .unwrap();
        std::env::set_var("QODERWORK_DB", &db);

        let CardOutcome::Fresh { card, .. } = card_of(&src, None) else {
            panic!("named")
        };
        assert_eq!(card.title.as_deref(), Some("初筛候选人简历"));
        assert_eq!(
            card.last_prompt.as_deref(),
            Some("do the thing"),
            "the prompt still comes from the transcript, as it does for Claude"
        );
        std::env::remove_var("QODERWORK_DB");
    }

    /// **The property the whole memo design rests on**: a QoderWork title changes when the
    /// DATABASE changes, with the transcript untouched. A caller-side mtime rule would pin the
    /// old title forever; the adapter notices because it checks the row version too.
    #[cfg(feature = "qoderwork-titles")]
    #[test]
    fn a_rename_is_seen_even_though_the_transcript_never_moved() {
        let _g = DB_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let (src, db) = fixture("rename");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "insert into sub_chats values ('s1','before','mchat01','aaaa-bbbb',100)",
            [],
        )
        .unwrap();
        std::env::set_var("QODERWORK_DB", &db);

        let CardOutcome::Fresh { card, memo } = card_of(&src, None) else {
            panic!()
        };
        assert_eq!(card.title.as_deref(), Some("before"));
        let len_before = std::fs::metadata(&src).unwrap().len();

        // Nothing at all happens ⇒ Unchanged.
        assert!(
            matches!(card_of(&src, memo.as_ref()), CardOutcome::Unchanged { .. }),
            "quiet session, quiet database"
        );

        // Rename in the DB only. The transcript is byte-for-byte the same.
        conn.execute(
            "update sub_chats set name='after', updated_at=200 where id='s1'",
            [],
        )
        .unwrap();
        assert_eq!(std::fs::metadata(&src).unwrap().len(), len_before);

        let CardOutcome::Fresh { card, .. } = card_of(&src, memo.as_ref()) else {
            panic!("a database rename must be noticed")
        };
        assert_eq!(card.title.as_deref(), Some("after"));
        std::env::remove_var("QODERWORK_DB");
    }

    /// The fallback: a session the newer table does not know about still gets its CHAT's name,
    /// via the workspace slug. Shared across that chat's sessions, and better than a bare UUID.
    #[cfg(feature = "qoderwork-titles")]
    #[test]
    fn an_unknown_session_falls_back_to_its_chats_name() {
        let _g = DB_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let (src, db) = fixture("chatfb");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "insert into chats values ('mchat01','Prepare interview',7)",
                [],
            )
            .unwrap();
        std::env::set_var("QODERWORK_DB", &db);
        let CardOutcome::Fresh { card, .. } = card_of(&src, None) else {
            panic!()
        };
        assert_eq!(card.title.as_deref(), Some("Prepare interview"));
        std::env::remove_var("QODERWORK_DB");
    }

    /// No database — an install that has none, or a build without the feature — degrades to the
    /// transcript alone. Never an error.
    #[test]
    fn without_a_database_only_the_transcript_speaks() {
        let _g = DB_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("QODERWORK_DB", "/nope/definitely/not/here.db");
        let d = std::env::temp_dir().join(format!("cr-qw-nodb-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let src = d.join("x.jsonl");
        std::fs::write(
            &src,
            "{\"type\":\"last-prompt\",\"lastPrompt\":\"only this\"}\n",
        )
        .unwrap();
        match session_card(&src, None) {
            CardOutcome::Fresh { card, .. } => {
                assert_eq!(card.title, None, "no database, no title");
                assert_eq!(card.last_prompt.as_deref(), Some("only this"));
            }
            other => panic!("expected the transcript half, got {other:?}"),
        }
        std::env::remove_var("QODERWORK_DB");
    }
}
