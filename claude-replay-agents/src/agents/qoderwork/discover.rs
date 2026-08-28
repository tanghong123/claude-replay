//! **QoderWork's transcript discovery** — the QoderWork half of the shared `discover`
//! interface. QoderWork is a Claude-Code-format client whose store layout is identical
//! (`~/.qoderwork/projects/<slug>/<id>.jsonl`, same slug convention), so this is a thin
//! wrapper over `claude_discover`'s root-parameterized internals — only the root (and the
//! agent tag on candidates) differ. Parsing likewise delegates to the Claude implementations
//! (see the `QoderWorkAdapter` in `adapter.rs`); the one format difference is the
//! `runtime-config` head line, which drives detection.

use claude_replay_engine::seam::{Agent, Candidate, CardMemo, CardOutcome, SessionCard, SpawnLink};
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

/// A transcript smaller than this is single-prompt junk (measured: QoderWork's throwaway
/// sessions are ~2–3 KB with one user line; the smallest REAL session on this store is
/// ~60 KB). A cheap `stat` gate that cuts the noise the monitor's desktop-agent group drowns
/// in (#111).
const MIN_TRANSCRIPT_BYTES: u64 = 4096;

/// The store slug for `$HOME` — e.g. `/Users/hong` → `-Users-hong`.
///
/// A QoderWork session filed under it did NOT run in a workspace: QoderWork's own workspaces
/// live at `~/.qoderwork/workspace/<id>`, so `$HOME` is where everything *else* lands. On this
/// machine that is 31 of 68 visible sessions, and every one is scheduled automation — a
/// memory-reflection worker, three stereotyped prompts, four of them byte-identical repeats
/// (design/qoderwork-rail-noise.md). They are an order of magnitude smaller than real sessions
/// on every axis: median 6 user turns against 70, 84 KB against 910 KB.
///
/// Excluded on WHERE they ran, never on what they said: a prompt-text rule would work today
/// and break silently the moment the user renames their own tooling, and it would bake one
/// person's automation into a general viewer. The slug comparison needs no file read.
fn home_slug() -> Option<String> {
    let home = claude_replay_engine::seam::home_dir()?;
    Some(home.to_string_lossy().replace('/', "-"))
}

/// Whether a project-dir slug is a MOUNTED-account/session artifact — `-accounts-*-mnt` or
/// `-sessions-*-mnt`. QoderWork writes these as SYMLINKS into the real workspace dirs, so
/// walking them double-counts sessions under a junk slug (owner decision #111: hide them).
fn is_mount_slug(name: &str) -> bool {
    (name.starts_with("-accounts-") || name.starts_with("-sessions-")) && name.ends_with("-mnt")
}

/// Every MAIN transcript in the QoderWork store the monitor should show (#111) — its OWN
/// walk, not the Claude shim, because QoderWork's store has junk the others do not:
///   - `-accounts-*-mnt` / `-sessions-*-mnt` project slugs are skipped ([`is_mount_slug`]);
///   - symlinked project dirs are skipped outright (the mount slugs are symlinks, and
///     following them re-walks the workspace sessions they point at);
///   - transcripts below [`MIN_TRANSCRIPT_BYTES`] are dropped as single-prompt junk.
///
/// It walks top-level `<project>/*.jsonl` only — the shape the picker already discovers. The
/// deeper `<project>/<uuid>/<uuid>.jsonl` sessions (QoderWork's `$HOME`-cwd sessions, #69) are
/// left for a later coverage pass: surfacing them would ADD rows to an already-crowded group
/// rather than cut the noise this task targets.
pub(crate) fn store_transcripts() -> Vec<PathBuf> {
    let root = projects_dir();
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(&root) else {
        return out;
    };
    let home = home_slug();
    for proj in projects.flatten() {
        let slug = proj.file_name().to_string_lossy().to_string();
        if is_mount_slug(&slug) {
            continue;
        }
        // Sessions that ran outside any workspace — automation, not conversations (#151).
        if home.as_deref() == Some(slug.as_str()) {
            continue;
        }
        let dir = proj.path();
        if std::fs::symlink_metadata(&dir)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            continue; // a symlinked project dir duplicates a real one
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            let big_enough = f
                .metadata()
                .map(|m| m.len() >= MIN_TRANSCRIPT_BYTES)
                .unwrap_or(false);
            if big_enough
                && path.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            {
                out.push(path);
            }
        }
    }
    out
}

/// Find a QoderWork transcript by session id (`<id>.jsonl`) anywhere under its projects dir.
pub fn transcript_by_id(id: &str) -> Option<PathBuf> {
    crate::agents::claude::discover::transcript_by_id_in(&projects_dir(), id)
}

/// Where QoderWork keeps its chat metadata. Overridable so a test — or a non-standard install —
/// can point elsewhere; `None` when there is no database to read.
///
/// macOS-only because QoderWork is an Electron app that ships there; a build on another
/// platform simply finds nothing and every session falls back to its snippet.
#[cfg(target_os = "macos")]
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
    /// The title source's `updated_at` when the title was read — the sidecar's, or the
    /// database's when there is no sidecar. The title lives OUTSIDE the transcript either
    /// way, so this is what "unchanged" means for it: the transcript can sit still while a
    /// rename lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

/// Bumped to 2 when the title source gained the sidecar (#143): a v1 memo's stamp came from
/// the database, so comparing it against a sidecar stamp would be meaningless.
const MEMO_V: u8 = 2;

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

    // The title half: the sidecar first, the database as the fallback (#143).
    let (title, title_at) = match sidecar_title(path).or_else(|| db_title(path)) {
        Some((t, at)) => (Some(t), Some(at)),
        None => (None, None),
    };
    let title_changed = prev.as_ref().map(|m| m.title_at) != Some(title_at);

    let card = SessionCard { title, last_prompt };
    let next = CardMemo::encode(&Memo {
        v: MEMO_V,
        tail: tail_next.map(|m| m.value().clone()),
        title_at,
        title: card.title.clone(),
    });

    if prev.is_some() && !tail_changed && !title_changed {
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

/// The title QoderWork writes beside the transcript, as `<sid>-session.json` (#143), with its
/// `updated_at` as the staleness stamp.
///
/// The sidecar is checked first because it costs nothing this crate does not already have (a
/// plain JSON read, works on every platform) — but it is now the LEGACY source: QoderWork
/// stopped writing it around July 2026 (a new session gets only an encrypted `<sid>/state.json`
/// dir), so for every session since, the database is the only place the title exists at all.
///
/// A forked session's own title is `"<root title> (Fork)"`, which is what it should read as;
/// grouping forks into families is a separate concern (design/qoderwork-fork-families.md).
/// The session this one was forked from, from the same sidecar (#142).
///
/// `fork_from` is QoderWork's own record of the copy, so the family is exact rather than
/// inferred from overlapping content — which matters, because a fork's transcript shares no
/// BYTES with its origin (every line carries its own ids and timestamps) even where 99% of
/// the conversation is identical.
///
/// An empty value means "not a fork" and is normalized to `None`, so a root never claims to
/// descend from nothing-in-particular.
pub(crate) fn fork_origin(path: &Path) -> Option<String> {
    let v = sidecar(path)?;
    let from = v.get("fork_from")?.as_str()?.trim();
    (!from.is_empty()).then(|| from.to_string())
}

/// The `<sid>-session.json` beside a transcript, parsed. `None` when absent or unreadable —
/// a store that predates the sidecar simply has no metadata here.
fn sidecar(path: &Path) -> Option<serde_json::Value> {
    let stem = path.file_stem()?.to_str()?;
    let side = path.with_file_name(format!("{stem}-session.json"));
    serde_json::from_str(&std::fs::read_to_string(side).ok()?).ok()
}

fn sidecar_title(path: &Path) -> Option<(String, i64)> {
    let v = sidecar(path)?;
    let title = v.get("title")?.as_str()?.trim();
    if title.is_empty() {
        return None;
    }
    // `updated_at` is epoch millis, the same shape the DB's column has, so the memo's
    // staleness comparison is unchanged.
    let at = v.get("updated_at").and_then(serde_json::Value::as_i64);
    Some((title.to_string(), at.unwrap_or(0)))
}

/// `sub_chats.name` + `updated_at` for the session at `path`, or `None`.
///
/// Exact join first (`session_id` = the transcript stem — verified to resolve every row on a real
/// install). Falls back to the containing chat's name via the workspace slug, which covers
/// sessions the newer table does not know about; that title belongs to the CHAT and so may be
/// shared by several sessions, which is still better than a bare UUID.
#[cfg(target_os = "macos")]
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

/// Off macOS there is no database to read (`db_path()` is a macOS path), so QoderWork sessions
/// carry only what the transcript and any legacy sidecar say.
#[cfg(not(target_os = "macos"))]
fn db_title(_path: &Path) -> Option<(String, i64)> {
    None
}

// ── The spawn → child relation, which QoderWork keeps OUT OF BAND ─────────────────────
//
// Claude Code names a child in the parent transcript — `toolUseResult.agentId` — and the shared
// enrichment resolves `subagents/agent-<agentId>.jsonl` from it. QoderWork writes that same key,
// but ONLY on the result: a spawn that is still running has no id anywhere in the transcript, so
// its `SubAgent` folds anonymous and the viewer shows an inert chip with no child behind it —
// exactly when watching the child is most useful. (Verified on a live session: five running
// `Agent` spawns, zero occurrences of `agentId`; and on 402 finished spawns elsewhere in the
// same store, every one of them carrying it.)
//
// The id is on disk from the start all the same, beside the child it names:
//
//   <session>/subagents/task-<agent>.json        { parentToolUseId, agentId, agentType, … }
//   <session>/subagents/agent-<agent>.meta.json  { toolUseId, agentType, … }  ← id in the name
//
// Both key on the parent's `tool_use` id, which every spawn carries in-band from its first line.
// The id they yield is the SAME one the eventual result carries, so a child keeps one identity
// for its whole life: adopting it early only makes the link exist sooner, and when the result
// finally lands it confirms what the viewer already showed.

/// `<transcript-dir>/<session-id>/subagents`, when it exists.
fn subagents_dir(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?.join(stem).join("subagents");
    dir.is_dir().then_some(dir)
}

/// This session's out-of-band spawn identity, read from the sidecars beside its children.
///
/// The `is_dir` probe short-circuits every session that spawned nothing — one `stat`, which is
/// what a live follower pays per poll for asking.
///
/// `task-*.json` is preferred because it names the agent id outright; `*.meta.json` is the
/// fallback, where the id is the file's own stem. A sidecar that parses but does not name the
/// parent is skipped rather than guessed at.
pub(crate) fn spawn_links(path: &Path) -> Vec<SpawnLink> {
    let Some(dir) = subagents_dir(path) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<SpawnLink> = Vec::new();
    let mut metas: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("task-") && name.ends_with(".json") {
            if let Some(link) = task_link(&path) {
                out.push(link);
            }
        } else if name.starts_with("agent-") && name.ends_with(".meta.json") {
            metas.push(path);
        }
    }
    // Only for spawns no task file claimed — the meta file's id comes from its name.
    for path in metas {
        let Some(link) = meta_link(&path) else {
            continue;
        };
        if !out.iter().any(|l| l.tool_use_id == link.tool_use_id) {
            out.push(link);
        }
    }
    out
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A `task-*.json`: the parent's tool-use id and the child's id, both named outright.
fn task_link(path: &Path) -> Option<SpawnLink> {
    let v = read_json(path)?;
    Some(SpawnLink {
        tool_use_id: v.get("parentToolUseId")?.as_str()?.to_string(),
        agent_id: v.get("agentId")?.as_str()?.to_string(),
    })
}

/// An `agent-<id>.meta.json`: the parent's tool-use id inside, the child's id in the file name.
fn meta_link(path: &Path) -> Option<SpawnLink> {
    let v = read_json(path)?;
    let agent_id = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".meta.json"))
        .and_then(|n| n.strip_prefix("agent-"))?
        .to_string();
    Some(SpawnLink {
        tool_use_id: v.get("toolUseId")?.as_str()?.to_string(),
        agent_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `QODERWORK_PROJECTS_DIR` is a PROCESS-global override, and cargo runs these tests on
    /// parallel threads in one binary: without this, whichever test called `remove_var`
    /// first unset it for the other mid-walk, and that one silently scanned the developer's
    /// REAL store (68 live transcripts where the fixture has 1) — a flake that failed a
    /// different test on each run. Hold this for the whole env-scoped window.
    static STORE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// PROVENANCE-FIRST detection (#20 follow-up): a session under `~/.qoderwork/projects`
    /// keeps its QoderWork identity even with the KEYED `runtime-config` head — real
    /// QoderWork stores write `reasoningEffort`/`contextWindow` too (72/72 heads on the
    /// store that surfaced #20's misdetection), so only the store can tell it apart from
    /// Qoder CLI. This is the head shape #20 originally claimed for Qoder alone.
    #[test]
    fn detection_attributes_the_qoderwork_store_by_provenance() {
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("qw-prov-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let slug = root.join("-Users-dev-proj");
        std::fs::create_dir_all(&slug).unwrap();
        let p = slug.join("qw1.jsonl");
        std::fs::write(
            &p,
            concat!(
                r#"{"type":"runtime-config","sessionId":"qw1","model":"","reasoningEffort":null,"contextWindow":null,"timestamp":1784282861519}"#,
                "\n",
            ),
        )
        .unwrap();

        std::env::set_var("QODERWORK_PROJECTS_DIR", &root);
        let claimed = claude_replay_core::discover::detect_agent_claimed(&p);
        std::env::remove_var("QODERWORK_PROJECTS_DIR");
        assert_eq!(claimed, (Agent::QODERWORK, true));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The delegation guarantee: parsing a QoderWork transcript AS QoderWork is byte-identical
    /// to parsing it as Claude (same blocks, times, metrics) — the adapter adds detection and a
    /// store, never a format fork. Fixture mirrors the real shape: runtime-config head,
    /// multi-text user line, a tool call + result.
    /// The #111 store walk: `-accounts-*-mnt` / `-sessions-*-mnt` slugs and symlinked
    /// project dirs are skipped, and transcripts below the junk floor are dropped — only
    /// the real, big-enough workspace sessions come back.
    #[test]
    fn store_walk_skips_mounts_symlinks_and_junk() {
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        // Distinct from the other env-scoped test's root: they used to share one path and
        // delete each other's fixtures.
        let root = std::env::temp_dir().join(format!("qw-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::env::set_var("QODERWORK_PROJECTS_DIR", &root);
        let big = vec![b'x'; (MIN_TRANSCRIPT_BYTES + 10) as usize];

        // A real workspace project with one real and one junk transcript.
        let ws = root.join("-Users-hong--qoderwork-workspace-abc");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("real.jsonl"), &big).unwrap();
        std::fs::write(ws.join("junk.jsonl"), b"{\"type\":\"user\"}\n").unwrap();

        // A mounted-account slug with a big transcript — excluded by name.
        let acct = root.join("-accounts-deadbeef-mnt");
        std::fs::create_dir_all(&acct).unwrap();
        std::fs::write(acct.join("x.jsonl"), &big).unwrap();

        // A mounted-session slug that is a SYMLINK into the real workspace — excluded, and
        // must not re-surface real.jsonl under the junk slug.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&ws, root.join("-sessions-cafe-mnt")).unwrap();

        let got = store_transcripts();
        assert_eq!(
            got.len(),
            1,
            "only the one real workspace transcript: {got:?}"
        );
        assert!(got[0].ends_with("real.jsonl"));

        std::env::remove_var("QODERWORK_PROJECTS_DIR");
        let _ = std::fs::remove_dir_all(&root);
    }

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

    /// Current QoderWork transcripts can carry the same credit-billed usage shape as Qoder
    /// CLI. Store provenance must preserve the QoderWork identity while the shared Claude
    /// metrics fold exposes both native credits and their subscription-rate USD conversion.
    /// Historical QoderWork sessions without usage remain unpriced; this test pins the newer
    /// on-disk shape observed on 2026-08-27.
    #[test]
    fn current_qoderwork_credits_are_priced_through_the_shared_fold() {
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("qw-credits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let slug = root.join("-Users-dev--qoderwork-workspace-current");
        std::fs::create_dir_all(&slug).unwrap();
        let p = slug.join("qw-credits.jsonl");
        std::fs::write(
            &p,
            concat!(
                r#"{"type":"runtime-config","sessionId":"qw-credits","model":"qwork-ultimate","reasoningEffort":null,"contextWindow":null,"timestamp":1787792414000}"#,
                "\n",
                r#"{"type":"assistant","message":{"id":"m1","role":"assistant","model":"qwork-ultimate","content":[{"type":"text","text":"one"}],"usage":{"input_tokens":0,"output_tokens":0,"credits":2.5,"original_credits":2.5,"billable":true,"request_id":"r1"}},"timestamp":"2026-08-27T01:00:00Z"}"#,
                "\n",
                r#"{"type":"assistant","message":{"id":"m2","role":"assistant","model":"qwork-ultimate","content":[{"type":"text","text":"two"}],"usage":{"input_tokens":0,"output_tokens":0,"credits":0.75,"original_credits":0.75,"billable":true,"request_id":"r2"}},"timestamp":"2026-08-27T01:01:00Z"}"#,
                "\n",
            ),
        )
        .unwrap();

        std::env::set_var("QODERWORK_PROJECTS_DIR", &root);
        let session = claude_replay_core::parse_session(&p).unwrap();
        std::env::remove_var("QODERWORK_PROJECTS_DIR");

        assert_eq!(session.agent, Agent::QODERWORK, "store provenance wins");
        assert_eq!(session.metrics.extra.get("credits_micro"), Some(&3_250_000));
        assert!((session.metrics.cost_usd.expect("priced from credits") - 0.0325).abs() < 1e-12);
        assert!(
            session.metrics.footer().contains("~$0.03")
                && session.metrics.footer().contains("~3.25 credits"),
            "footer: {}",
            session.metrics.footer()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// #143: the title comes from `<sid>-session.json` beside the transcript — no database,
    /// no feature flag, no platform assumption. A blank or absent sidecar yields nothing so
    /// the DB fallback still gets its turn.
    #[test]
    fn the_sidecar_supplies_the_title() {
        let d = std::env::temp_dir().join(format!("qw-side-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let t = d.join("abc123.jsonl");
        std::fs::write(&t, b"{}\n").unwrap();

        assert_eq!(sidecar_title(&t), None, "no sidecar, no title");

        std::fs::write(
            d.join("abc123-session.json"),
            r#"{"id":"abc123","title":"安装MCP服务器 (Fork)","updated_at":1780315233809,
                "fork_from":"fcd5fe45"}"#,
        )
        .unwrap();
        assert_eq!(
            sidecar_title(&t),
            Some(("安装MCP服务器 (Fork)".to_string(), 1780315233809)),
            "the fork's own title, with updated_at as the staleness stamp"
        );

        // A blank title must not shadow the database fallback.
        std::fs::write(
            d.join("abc123-session.json"),
            br#"{"id":"abc123","title":"   ","updated_at":1}"#,
        )
        .unwrap();
        assert_eq!(sidecar_title(&t), None, "blank is not a title");

        // A sidecar with no updated_at is still usable — the stamp just never moves.
        std::fs::write(
            d.join("abc123-session.json"),
            br#"{"id":"abc123","title":"no stamp"}"#,
        )
        .unwrap();
        assert_eq!(sidecar_title(&t), Some(("no stamp".to_string(), 0)));

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Discovery over a fake QoderWork store: candidates come back tagged `QoderWork`, scoped
    /// to the cwd's slug, and a bare id resolves to its transcript — the picker/`--latest`/
    /// bare-id surface the Claude store already has, on the QoderWork root.
    #[test]
    fn discovers_and_resolves_from_the_qoderwork_store() {
        let _env = STORE_ENV.lock().unwrap_or_else(|e| e.into_inner());
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

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    fn card_of(src: &Path, memo: Option<&CardMemo>) -> CardOutcome {
        session_card(src, memo)
    }

    /// The exact join: `sub_chats.session_id` IS the transcript stem (verified against a real
    /// install, where it resolved every row). The transcript half still supplies `last_prompt`.
    #[cfg(target_os = "macos")]
    #[test]
    fn title_comes_from_sub_chats_and_last_prompt_from_the_transcript() {
        let _g = DB_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let (src, db) = fixture("exact");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "insert into sub_chats values ('s1','重构解析器模块','mchat01','aaaa-bbbb',100)",
                [],
            )
            .unwrap();
        std::env::set_var("QODERWORK_DB", &db);

        let CardOutcome::Fresh { card, .. } = card_of(&src, None) else {
            panic!("named")
        };
        assert_eq!(card.title.as_deref(), Some("重构解析器模块"));
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
    #[cfg(target_os = "macos")]
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
    #[cfg(target_os = "macos")]
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

    /// No database — an install that has none, or a non-macOS build — degrades to the
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

    /// The out-of-band spawn table: `task-*.json` names the child outright; a `*.meta.json`
    /// covers a spawn no task file claimed, taking the id from its own file name. A session
    /// that spawned nothing answers with an empty table and one `stat`.
    #[test]
    fn sidecars_name_the_children_a_running_transcript_cannot() {
        let d = std::env::temp_dir().join(format!("qw-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let sa = d.join("s1").join("subagents");
        std::fs::create_dir_all(&sa).unwrap();
        let t = d.join("s1.jsonl");
        std::fs::write(&t, b"{}\n").unwrap();

        assert!(
            spawn_links(&d.join("nosuch.jsonl")).is_empty(),
            "a session with no subagents dir has no links"
        );

        std::fs::write(
            sa.join("task-aExplore-11.json"),
            br#"{"parentToolUseId":"call_a","agentId":"aExplore-11","agentType":"Explore"}"#,
        )
        .unwrap();
        // Only a meta file for this one — the id has to come from the file name.
        std::fs::write(
            sa.join("agent-ageneral-22.meta.json"),
            br#"{"toolUseId":"call_b","agentType":"general-purpose"}"#,
        )
        .unwrap();
        // Both forms for a third: the task file claims it, the meta file is ignored.
        std::fs::write(
            sa.join("task-aExplore-33.json"),
            br#"{"parentToolUseId":"call_c","agentId":"aExplore-33","agentType":"Explore"}"#,
        )
        .unwrap();
        std::fs::write(
            sa.join("agent-aExplore-33.meta.json"),
            br#"{"toolUseId":"call_c","agentType":"stale"}"#,
        )
        .unwrap();
        // Neither names a parent — skipped, never guessed at.
        std::fs::write(sa.join("task-orphan.json"), br#"{"agentId":"aOrphan"}"#).unwrap();

        let mut links = spawn_links(&t);
        links.sort_by(|a, b| a.tool_use_id.cmp(&b.tool_use_id));
        let seen: Vec<(String, String)> = links
            .into_iter()
            .map(|l| (l.tool_use_id, l.agent_id))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("call_a".into(), "aExplore-11".into()),
                ("call_b".into(), "ageneral-22".into()),
                ("call_c".into(), "aExplore-33".into()),
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
