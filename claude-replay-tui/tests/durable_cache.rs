//! **The durable cache, end to end** (#96) — through the real frontend API, not its parts.
//!
//! `claude-replay-agents/tests/crash_consistency.rs` proves the *fold* restores correctly from a
//! record stream. This proves the whole path a frontend actually takes: `admit` → fold → drop the
//! process → `admit` again → resume. The two are complementary, and the composition is where a
//! design like this usually breaks — each piece right, the seam between them wrong.
//!
//! The oracle is always a cold parse. "It resumed" is not the property; "it resumed to exactly
//! what folding from scratch produces" is, because a corrupt-but-plausible resume passes every
//! self-consistency check there is.

use claude_replay_core::engine::meta_stream::Versions;
use claude_replay_core::model::Block;
use claude_replay_core::{parse_session_as, Agent, Transcript};
use claude_replay_present::cache::{
    admit::Origin, Admission, Denial, Holder, Presentation, SessionCache, Unavailable,
};
use claude_replay_tui::store::{ArcLog, TuiNote};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

type Cache = SessionCache<ArcLog, ()>;

fn tmp(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "cr-durable-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn user(t: &str, sec: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}]}},\"timestamp\":\"2026-07-26T10:00:{sec:02}Z\"}}\n"
    )
}
fn assistant(t: &str, sec: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"usage\":{{\"input_tokens\":5,\"output_tokens\":8}}}},\"timestamp\":\"2026-07-26T10:00:{sec:02}Z\"}}\n"
    )
}
fn tool(id: &str, sec: u32) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Bash\",\"input\":{{\"command\":\"ls\"}}}}]}},\"timestamp\":\"2026-07-26T10:00:{sec:02}Z\"}}\n"
    )
}
fn result(id: &str, sec: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"out\"}}]}},\"timestamp\":\"2026-07-26T10:00:{sec:02}Z\"}}\n"
    )
}

/// Two user turns on ONE line — the straddle shape (#96 I5). The drain it triggers cannot carry
/// a resume payload, because the partition would fall inside a line.
fn two_turns_one_line(a: &str, b: &str, sec: u32) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{a}\"}},{{\"type\":\"text\",\"text\":\"{b}\"}}]}},\"timestamp\":\"2026-07-26T10:00:{sec:02}Z\"}}\n"
    )
}

/// A sub-agent spawn and a task create — so `agent_ids` and `tasks` are non-empty. Without them
/// a checkpoint's two map fields are `Default::default()` either way and nothing can tell a
/// dropped one from a correct one. (Learned by mutation: two faults survived a fixture that had
/// neither.)
fn prologue() -> String {
    let mut s = String::new();
    s.push_str("{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_A\",\"name\":\"Task\",\"input\":{\"subagent_type\":\"general-purpose\",\"description\":\"child one\",\"prompt\":\"go\"}}]},\"timestamp\":\"2026-07-26T09:59:01Z\"}\n");
    s.push_str("{\"type\":\"user\",\"toolUseResult\":{\"agentId\":\"aXYZ\",\"status\":\"async_launched\"},\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_A\",\"content\":\"launched\"}]},\"timestamp\":\"2026-07-26T09:59:02Z\"}\n");
    s.push_str("{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tk1\",\"name\":\"TaskCreate\",\"input\":{\"subject\":\"s\",\"description\":\"d\",\"active_form\":\"a\"}}]},\"timestamp\":\"2026-07-26T09:59:03Z\"}\n");
    s.push_str("{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tk1\",\"content\":\"Created task #12: s\"}]},\"timestamp\":\"2026-07-26T09:59:04Z\"}\n");
    s
}

/// A multi-turn transcript: enough turns to have several commit points, with a tool call whose
/// result lands later (a back-patch across the frontier) in the middle, a sub-agent and a task in
/// the prologue, and a **straddling line every third turn** so non-resumable drains are common
/// rather than a coincidence.
fn transcript(path: &Path, turns: usize) {
    let mut s = prologue();
    for i in 0..turns {
        let t = (i * 4) as u32;
        if i % 3 == 2 {
            s.push_str(&two_turns_one_line(
                &format!("ask {i}a"),
                &format!("ask {i}b"),
                t,
            ));
        } else {
            s.push_str(&user(&format!("ask {i}"), t));
        }
        s.push_str(&tool(&format!("b{i}"), t + 1));
        s.push_str(&result(&format!("b{i}"), t + 2));
        s.push_str(&assistant(&format!("reply {i}"), t + 3));
    }
    std::fs::write(path, s).unwrap();
}

fn append(path: &Path, s: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

fn cache(root: &Path) -> Cache {
    Cache::durable(
        Presentation::Tui,
        root.to_path_buf(),
        Versions::current(None),
    )
}

/// The header a session reports — turns, tools, children.
///
/// Compared beside the blocks because **blocks alone are not enough**: the meta stream's counters
/// live outside them, so a resume that double-counted every re-folded commit produced a
/// block-identical session with an inflated header, and every block-only test passed. (Found
/// exactly that way.)
fn meta_of(src: &Path) -> claude_replay_core::engine::SessionMeta {
    let s = parse_session_as(Agent::CLAUDE, src).unwrap();
    let mut m = claude_replay_core::engine::SessionMeta::default();
    for b in s.blocks() {
        m.push(&b);
    }
    m
}

/// Open `id` through the real API and fold to EOF, returning the joined view + the origin.
fn open(c: &Cache, id: &str, src: &Path) -> (Vec<Block>, Origin) {
    c.register(id, Transcript::open(Agent::CLAUDE, src.to_path_buf()));
    let (session, origin) = match c.admit(
        id,
        |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
        |_: &Holder<TuiNote>| false, // no live peer in these tests
    ) {
        Admission::Owned { session, origin } => (session, origin),
        Admission::Denied(_) => panic!("a free entry must be Owned"),
    };
    let d = c
        .poll_view(id, ArcLog::memory)
        .expect("registered")
        .expect("readable");
    let mut blocks: Vec<Block> = session
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    blocks.extend(d.provisional.iter().map(|a| a.as_ref().clone()));
    (blocks, origin)
}

fn cold(src: &Path) -> Vec<Block> {
    parse_session_as(Agent::CLAUDE, src).unwrap().blocks()
}

/// **The equivalence property.** A second run resumes — and resumes to exactly what a cold fold
/// produces. Both halves matter: without the first the cache is useless, without the second it is
/// worse than useless.
#[test]
fn a_second_run_resumes_to_a_block_identical_session() {
    let root = tmp("equiv");
    let src = root.join("t.jsonl");
    transcript(&src, 6);

    let (first, origin) = {
        let c = cache(&root);
        let r = open(&c, "s", &src);
        c.release_all();
        r
    };
    assert!(
        matches!(origin, Origin::Cold(_)),
        "the first run has nothing to resume from"
    );
    assert_eq!(first, cold(&src), "a cold run equals a cold parse");

    let c = cache(&root);
    let (second, origin) = open(&c, "s", &src);
    assert_eq!(
        c.touch("s").unwrap().session_meta(),
        meta_of(&src),
        "the resumed HEADER must match too — the counters live outside the blocks"
    );
    match origin {
        Origin::Resumed {
            committed,
            replay_from,
        } => {
            assert!(committed > 0, "something was restored");
            assert!(replay_from > 0, "and the reader started ABOVE byte 0");
        }
        other => panic!("the second run must resume, got {other:?}"),
    }
    assert_eq!(second, cold(&src), "resumed == cold, block for block");
}

/// A resume that then keeps folding. The interesting case is not "it loaded", but "it loaded and
/// the session it continued into is still right" — the seam between restored and freshly folded
/// blocks is exactly where a wrong `prev_ts`/turn-count/back-patch state would show.
#[test]
fn a_resumed_session_keeps_folding_correctly() {
    let root = tmp("grow");
    let src = root.join("t.jsonl");
    transcript(&src, 4);

    {
        let c = cache(&root);
        open(&c, "s", &src);
        c.release_all();
    }

    // The session moved on while nothing was watching.
    append(&src, &user("later", 40));
    append(&src, &assistant("after the restart", 41));
    append(&src, &user("later still", 42));

    let c = cache(&root);
    let (got, origin) = open(&c, "s", &src);
    assert!(matches!(origin, Origin::Resumed { .. }));
    assert_eq!(got, cold(&src), "restored ++ newly folded == cold");
    assert_eq!(
        c.touch("s").unwrap().session_meta(),
        meta_of(&src),
        "and its header, which no block comparison would notice"
    );
}

/// Resuming REPEATEDLY. One resume being right does not mean a resume from a resume is: the
/// second run's records are authored by a restored writer, whose counter baselines came out of
/// the stream rather than from zero.
#[test]
fn resuming_from_a_resume_stays_identical() {
    let root = tmp("chain");
    let src = root.join("t.jsonl");
    transcript(&src, 3);

    for round in 0..4 {
        let c = cache(&root);
        let (got, origin) = open(&c, "s", &src);
        assert_eq!(got, cold(&src), "round {round}");
        assert_eq!(
            c.touch("s").unwrap().session_meta(),
            meta_of(&src),
            "round {round}: the header must not drift across repeated resumes"
        );
        if round > 0 {
            assert!(
                matches!(origin, Origin::Resumed { .. }),
                "round {round} must resume"
            );
        }
        c.release_all();
        append(&src, &user(&format!("round {round}"), 50 + round));
        append(&src, &assistant("ok", 51 + round));
    }
}

/// **Checkpoints and compaction, end to end** (#96 §6.6). A session long enough to trip
/// `CHECKPOINT_EVERY` writes checkpoints; compaction then throws away everything before the
/// newest one; and the compacted stream still resumes to a block-identical session.
///
/// The resume itself is what proves the writer and the reader agree: `align` validates every
/// checkpoint it passes against the state it folded, so a writer whose materialized view had
/// drifted would come back `Cold(CheckpointMismatch)` instead of `Resumed`.
#[test]
fn checkpoints_are_written_compacted_and_still_resume() {
    use claude_replay_core::engine::meta_stream::CHECKPOINT_EVERY;
    use claude_replay_present::cache::stream::{compact, meta_path, MetaReader};

    let root = tmp("ckpt");
    let src = root.join("t.jsonl");
    // Comfortably more commits than one checkpoint interval, so several land.
    transcript(&src, CHECKPOINT_EVERY * 3);

    let dir = claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "s");
    {
        let c = cache(&root);
        let (got, _) = open(&c, "s", &src);
        assert_eq!(got, cold(&src), "the writing run is correct to begin with");
        c.release_all();
    }

    let records: Vec<_> = MetaReader::open(&dir).unwrap().unwrap().1.collect();
    let checkpoints = records.iter().filter(|r| r.checkpoint.is_some()).count();
    assert!(
        checkpoints >= 2,
        "{} records should carry >=2 checkpoints, got {checkpoints}",
        records.len()
    );
    // Every checkpoint rides a resumable record — otherwise compacting onto one could leave
    // complete state with no `replay_from` anywhere.
    assert!(
        records
            .iter()
            .filter(|r| r.checkpoint.is_some())
            .all(|r| r.resume.is_some()),
        "a checkpoint must ride a resume point"
    );
    // …and the interval counts RESUMABLE drains only. The fixture straddles every third turn, so
    // non-resumable drains are common; if they advanced the clock, the gaps below would come out
    // short. Asserting the gap rather than "it rode a resume point" is what makes that
    // deterministic instead of a coincidence of where the interval happened to land.
    let gaps: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.checkpoint.is_some())
        .map(|(i, _)| {
            records[..=i]
                .iter()
                .rev()
                .skip(1)
                .take_while(|r| r.checkpoint.is_none())
                .filter(|r| r.resume.is_some())
                .count()
                + 1
        })
        .collect();
    assert!(
        gaps.iter().all(|&g| g == CHECKPOINT_EVERY),
        "each checkpoint must follow exactly {CHECKPOINT_EVERY} resumable drains, got {gaps:?}"
    );

    // The content stream's own length IS the committed block count — the unit `resume.id`
    // speaks, and the same number `admit` hands to alignment.
    let committed = std::fs::read_to_string(dir.join("blocks.jsonl"))
        .unwrap()
        .lines()
        .count();
    // Resume from the UNCOMPACTED stream first. This is the run that exercises
    // validate-on-pass: alignment folds its way to a mid-stream checkpoint with state already
    // behind it, and compares. (After compaction the stream OPENS on a checkpoint, so there is
    // nothing behind it and the check is skipped by design — which is why both runs are here.)
    {
        let c = cache(&root);
        let (got, origin) = open(&c, "s", &src);
        assert!(
            matches!(origin, Origin::Resumed { .. }),
            "an uncompacted checkpointed stream must resume, got {origin:?} — a \
             CheckpointMismatch here means the writer's checkpoint disagrees with its own deltas"
        );
        assert_eq!(got, cold(&src), "resumed-with-checkpoints == cold");
        c.release_all();
    }

    // Re-read: the resume above may have fallen back past a straddling drain and re-committed,
    // appending records of its own. (The simpler fixture never did, which hid this.)
    let records: Vec<_> = MetaReader::open(&dir).unwrap().unwrap().1.collect();
    let before = std::fs::metadata(meta_path(&dir)).unwrap().len();
    let dropped = compact(&dir, committed, 1).unwrap();
    assert!(dropped > 0, "compaction should have found a base");
    let after = std::fs::metadata(meta_path(&dir)).unwrap().len();
    assert!(
        after < before,
        "compaction shrinks the stream: {before} -> {after}"
    );
    let kept: Vec<_> = MetaReader::open(&dir).unwrap().unwrap().1.collect();
    assert_eq!(kept.len(), records.len() - dropped);
    assert!(kept[0].checkpoint.is_some(), "it opens ON a checkpoint");

    // …and the compacted stream still resumes, block-identically.
    let c = cache(&root);
    let (got, origin) = open(&c, "s", &src);
    assert!(
        matches!(origin, Origin::Resumed { .. }),
        "a compacted stream must still resume, got {origin:?}"
    );
    assert_eq!(got, cold(&src), "resumed-from-compacted == cold");
}

/// **A resumed writer's checkpoints must describe the WHOLE session**, not just what it folded
/// after the resume. The writer builds a checkpoint from its maintained state, so `restore` has
/// to seed that state — and the only thing that notices a gap is a *third* run validating a
/// checkpoint the *second* one wrote.
///
/// That is why this needs three runs and a transcript that grows past a checkpoint interval
/// between them: run 2 must fold enough to emit a checkpoint of its own for run 3 to check.
#[test]
fn a_resumed_writers_checkpoints_cover_the_whole_session() {
    use claude_replay_core::engine::meta_stream::CHECKPOINT_EVERY;
    use claude_replay_present::cache::stream::MetaReader;

    let root = tmp("resumed-ckpt");
    let src = root.join("t.jsonl");
    let dir = claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "s");
    transcript(&src, CHECKPOINT_EVERY + 8);

    {
        let c = cache(&root);
        open(&c, "s", &src);
        c.release_all();
    }
    let after_first: Vec<_> = MetaReader::open(&dir).unwrap().unwrap().1.collect();
    let n1 = after_first
        .iter()
        .filter(|r| r.checkpoint.is_some())
        .count();

    // Grow well past another interval, so the RESUMED run emits checkpoints of its own.
    let mut extra = String::new();
    for i in 1000..(1000 + CHECKPOINT_EVERY + 8) {
        let t = (i % 900) as u32;
        extra.push_str(&user(&format!("more {i}"), t));
        extra.push_str(&assistant(&format!("ok {i}"), t + 1));
    }
    append(&src, &extra);

    let (got, origin) = {
        let c = cache(&root);
        let r = open(&c, "s", &src);
        c.release_all();
        r
    };
    assert!(matches!(origin, Origin::Resumed { .. }), "{origin:?}");
    assert_eq!(got, cold(&src), "run 2 is correct");

    let after_second: Vec<_> = MetaReader::open(&dir).unwrap().unwrap().1.collect();
    let n2 = after_second
        .iter()
        .filter(|r| r.checkpoint.is_some())
        .count();
    assert!(
        n2 > n1,
        "the resumed run must have written a checkpoint of its own ({n1} -> {n2})"
    );

    // Run 3 folds the stream and validates run 2's checkpoint on the way past. A checkpoint that
    // forgot everything before the resume disagrees here and comes back Cold.
    let c = cache(&root);
    let (got, origin) = open(&c, "s", &src);
    assert!(
        matches!(origin, Origin::Resumed { .. }),
        "a resumed writer's checkpoint must validate, got {origin:?}"
    );
    assert_eq!(got, cold(&src));
}

/// **A sub-agent is admitted like anything else.** Sub-agent frames in `-f` mode are
/// `register_new`'d and then polled, and registration alone is not enough: `poll_view` refuses to
/// materialize a session `admit` never granted, because only `admit` takes the lock — so an
/// unadmitted child would register and then silently stop live-tailing.
///
/// It used to be opened cache-less for that reason, on the grounds that a child is small and not
/// the session the user opened. That was the last caller of the escape hatch which let a session
/// be served without owning its entry, and #163 removed both. A child costs one entry and one
/// lock, like every other session.
#[test]
fn a_child_is_admitted_like_any_other_session() {
    let root = tmp("child");
    let src = root.join("child.jsonl");
    transcript(&src, 2);

    let c = cache(&root);
    c.register("child", Transcript::open(Agent::CLAUDE, src.clone()));
    // Registration alone is NOT enough on a durable cache — this is the trap.
    assert!(
        c.poll_view("child", ArcLog::memory).is_none(),
        "a durable cache must not materialize an unadmitted session behind the lock's back"
    );
    assert!(matches!(
        c.admit(
            "child",
            |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
            |_: &Holder<TuiNote>| false,
        ),
        Admission::Owned { .. }
    ));
    let d = c
        .poll_view("child", ArcLog::memory)
        .expect("resident now")
        .expect("readable");
    assert!(
        d.committed_len + d.provisional.len() > 0,
        "the child folds and ticks once it is admitted"
    );
    assert!(
        claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "child").exists(),
        "and it owns a durable entry, like every other session"
    );
}

/// **A session's display title must never become its cache key.** The two were one variable in
/// the viewer until the title stopped being the transcript stem; keying by it would mean two
/// sessions the user named the same thing share one cache entry and one lock — silently serving
/// one session's blocks under the other's name.
///
/// This asserts the property at the level that matters: two DIFFERENT transcripts carrying the
/// SAME title get separate entries.
#[test]
fn two_sessions_with_the_same_title_do_not_share_an_entry() {
    let root = tmp("sametitle");
    let title = "{\"type\":\"custom-title\",\"customTitle\":\"fix the parser\"}\n";
    let (a, b) = (root.join("aaa.jsonl"), root.join("bbb.jsonl"));
    for (p, who) in [(&a, "alpha"), (&b, "bravo")] {
        transcript(p, 2);
        append(p, &user(who, 50));
        append(p, title);
    }
    // Both really do carry the same name — otherwise this proves nothing.
    let card = |p: &Path| {
        claude_replay_core::discover::session_card(Agent::CLAUDE, p).and_then(|c| c.title)
    };
    assert_eq!(card(&a).as_deref(), Some("fix the parser"));
    assert_eq!(card(&b).as_deref(), Some("fix the parser"));

    let c = cache(&root);
    open(&c, "aaa", &a);
    open(&c, "bbb", &b);
    c.release_all();

    for id in ["aaa", "bbb"] {
        let d = claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, id);
        assert!(d.join("meta.jsonl").exists(), "{id} has its own entry");
    }
    // …and they are genuinely distinct sessions, not one entry read twice.
    assert_ne!(cold(&a), cold(&b));
}

/// A rewritten source must be REJECTED — the false-accept class, which produces a wrong session
/// rather than a slow one. Asserted on the REASON, not merely on "it rebuilt".
#[test]
fn a_rewritten_source_rebuilds_cold_and_stays_correct() {
    let root = tmp("rewrite");
    let src = root.join("t.jsonl");
    transcript(&src, 4);
    {
        let c = cache(&root);
        open(&c, "s", &src);
        c.release_all();
    }

    // Same path, different session entirely.
    let mut s = String::new();
    for i in 0..3 {
        s.push_str(&user(&format!("unrelated {i}"), (i * 2) as u32));
        s.push_str(&assistant("different", (i * 2 + 1) as u32));
    }
    std::fs::write(&src, s).unwrap();

    let c = cache(&root);
    let (got, origin) = open(&c, "s", &src);
    assert_eq!(
        origin,
        Origin::Cold(claude_replay_present::cache::ColdReason::SourceRewritten)
    );
    assert_eq!(got, cold(&src), "and the rebuild is a correct session");
}

/// A fold-version bump invalidates: resuming across one would splice blocks built by two
/// different folds into a single session, with no visible seam.
#[test]
fn a_fold_version_bump_rebuilds_cold() {
    let root = tmp("ver");
    let src = root.join("t.jsonl");
    transcript(&src, 3);
    {
        let c = cache(&root);
        open(&c, "s", &src);
        c.release_all();
    }

    let newer = Versions {
        fold: Versions::current(None).fold + 1,
        ..Versions::current(None)
    };
    let c = Cache::durable(Presentation::Tui, root.clone(), newer);
    let (got, origin) = open(&c, "s", &src);
    assert_eq!(
        origin,
        Origin::Cold(claude_replay_present::cache::ColdReason::VersionChanged)
    );
    assert_eq!(got, cold(&src));
}

/// **The single-writer invariant.** A live holder denies, and the denial opens NOTHING — the
/// property the two-outcome admission rests on. The note reaches the peer so its refusal can name
/// the pane.
#[test]
fn a_live_holder_denies_the_second_process() {
    let root = tmp("held");
    let src = root.join("t.jsonl");
    transcript(&src, 2);

    // A holder from ANOTHER process. It has to be a foreign pid: a lock naming *this* pid is
    // reclaimed by design, so that one process re-admitting a session it already owns (after
    // dropping a poisoned resident, say) does not deny itself.
    let dir = claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "s");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        claude_replay_present::cache::lock::lock_path(&dir),
        serde_json::to_string(&Holder {
            pid: 999_999u32,
            dir: dir.clone(),
            note: Some(TuiNote {
                pane: Some("%42".into()),
            }),
        })
        .unwrap(),
    )
    .unwrap();

    let c = cache(&root);
    c.register("s", Transcript::open(Agent::CLAUDE, src.clone()));
    match c.admit(
        "s",
        |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
        |_: &Holder<TuiNote>| true, // it is alive
    ) {
        Admission::Denied(Denial::Held(h)) => {
            assert_eq!(h.pid, 999_999);
            assert_eq!(h.note.unwrap().pane.unwrap(), "%42", "the note reaches it");
        }
        _ => panic!("a live holder must deny"),
    }
    assert!(
        c.touch("s").is_none(),
        "a denial must open NOTHING — that is what makes two outcomes honest"
    );
    assert!(
        !dir.join("blocks.jsonl").exists(),
        "and it must not even have opened the backing"
    );

    // A DEAD holder's lock is reclaimed instead — otherwise a crash would pin the session.
    assert!(matches!(
        c.admit(
            "s",
            |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
            |_: &Holder<TuiNote>| false
        ),
        Admission::Owned { .. }
    ));
}

/// An ephemeral cache denies with `NoCacheFlag` and writes nothing.
///
/// No flag selects one any more: `--no-cache` builds a real cache at its own root (#165), and a
/// denial no longer has a cache-less path behind it (#163). What is left is the type-level state
/// — a cache with no durable wiring — and its one guarantee: it denies, and it touches nothing.
#[test]
fn an_ephemeral_cache_denies_and_writes_nothing() {
    let root = tmp("ephem");
    let src = root.join("t.jsonl");
    transcript(&src, 2);

    let c = Cache::ephemeral();
    c.register("s", Transcript::open(Agent::CLAUDE, src.clone()));
    assert!(matches!(
        c.admit(
            "s",
            |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
            |_: &Holder<TuiNote>| false
        ),
        Admission::Denied(Denial::Unavailable(Unavailable::NoCacheFlag))
    ));
    assert!(
        !root.join("tui").exists(),
        "nothing was created for a session that was never admitted"
    );
}

/// The lock does not outlive the process even on an ERROR path, where nobody called
/// `release_all` — `Drop` covers it. A leaked lock would deny the session to the next run until
/// the pid died, which for a recycled pid can be never.
#[test]
fn dropping_a_cache_releases_its_locks() {
    let root = tmp("drop");
    let src = root.join("t.jsonl");
    transcript(&src, 2);

    {
        let c = cache(&root);
        open(&c, "s", &src); // no release_all — the drop must do it
    }

    let c = cache(&root);
    c.register("s", Transcript::open(Agent::CLAUDE, src.clone()));
    assert!(
        matches!(
            c.admit(
                "s",
                |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
                |_: &Holder<TuiNote>| true // even believing any holder is alive
            ),
            Admission::Owned { .. }
        ),
        "a dropped cache leaves no lock behind"
    );
}

// ── retention: residency that outlives ownership (#109) ───────────────────────────────────────

/// Bytes in an entry's two streams — the state a released session must not touch.
fn stream_lens(root: &Path, id: &str) -> (u64, u64) {
    let dir = claude_replay_present::cache::admit::entry_dir(root, Presentation::Tui, id);
    let len = |p: PathBuf| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    (len(dir.join("blocks.jsonl")), len(dir.join("meta.jsonl")))
}

/// `admit` + fold to EOF, keeping the session handle (the `open` helper drops it).
fn admit_and_fold(c: &Cache, id: &str, src: &Path) -> (Session, Origin) {
    c.register(id, Transcript::open(Agent::CLAUDE, src.to_path_buf()));
    let (session, origin) = match c.admit(
        id,
        |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
        |_: &Holder<TuiNote>| false,
    ) {
        Admission::Owned { session, origin } => (session, origin),
        Admission::Denied(_) => panic!("a free entry must be Owned"),
    };
    c.poll_view(id, ArcLog::memory);
    (session, origin)
}

type Session = std::sync::Arc<claude_replay_present::cache::SharedSession<ArcLog>>;

/// **The retention property.** Releasing a session gives up the LOCK, not the blocks: it stays
/// resident, and re-admitting it is `Retained` — the same session object, never rebuilt.
///
/// The `Arc::ptr_eq` is the whole point. A rebuilt session that happened to fold to the same
/// blocks would pass a content check while paying exactly the cost this exists to avoid.
#[test]
fn a_released_session_is_re_admitted_without_rebuilding() {
    let root = tmp("retain");
    let src = root.join("t.jsonl");
    transcript(&src, 6);

    let c = cache(&root);
    let (first, origin) = admit_and_fold(&c, "s", &src);
    assert!(matches!(origin, Origin::Cold(_)), "first open is cold");
    let committed = first.counters().2;
    assert!(committed > 0, "something committed to retain");

    c.release("s");
    assert!(first.frozen(), "released ⇒ quiesced");

    let (again, origin) = admit_and_fold(&c, "s", &src);
    assert_eq!(
        origin,
        Origin::Retained { committed },
        "the entry was untouched, so nothing is loaded, aligned or folded"
    );
    assert!(
        std::sync::Arc::ptr_eq(&first, &again),
        "retained means the SAME session, not an identical one"
    );
    assert!(!again.frozen(), "re-admitting thaws it");

    // And it is still a correct session: folding on from here matches a cold parse.
    append(&src, &user("more", 900));
    append(&src, &assistant("ok", 901));
    c.poll_view("s", ArcLog::memory);
    let mut blocks: Vec<Block> = again
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    blocks.extend(again.pull_delta(again.epoch(), blocks.len()).provisional);
    assert_eq!(blocks, cold(&src), "a retained session keeps folding right");
    c.release_all();
}

/// **Quiescence** (#109 / #96's single-writer rule). A released session must not fold: its writer
/// is detached, and `put` would append blocks to an entry another process may now own.
///
/// Checked at the streams, not at a flag — the flag is the mechanism, "the bytes did not move" is
/// the property. Before this, `release` flushed but left the writer attached, so a session that
/// kept ticking kept writing to an entry it no longer held.
#[test]
fn a_released_session_writes_nothing() {
    let root = tmp("quiesce");
    let src = root.join("t.jsonl");
    transcript(&src, 4);

    let c = cache(&root);
    let (ss, _) = admit_and_fold(&c, "s", &src);
    c.release("s");
    let before = stream_lens(&root, "s");

    // The transcript grows by two whole turns — plenty to commit, had it been folding.
    let committed = ss.counters().2;
    for i in 0..2 {
        append(&src, &user("after release", 900 + i * 2));
        append(&src, &assistant("reply", 901 + i * 2));
    }
    assert!(
        c.poll_view("s", ArcLog::memory).is_none(),
        "a frozen session is idle, however much the source grew"
    );
    assert!(!ss.advance().unwrap(), "and so is a direct advance");
    assert_eq!(ss.counters().2, committed, "nothing was folded");
    assert_eq!(stream_lens(&root, "s"), before, "and nothing was written");
    c.release_all();
}

/// **The witness.** Retention is only sound while the entry is untouched, so a peer that wrote to
/// it while we were released must defeat it — and the fallback must still produce a correct
/// session, not a spliced one.
///
/// The peer here is a second cache over the same root: it resumes the entry, folds the transcript
/// the first session never saw, and writes both streams. The first cache's blocks are then a
/// PREFIX of the entry rather than the whole of it, which is exactly the case
/// `Backing::Retained`'s length check exists to catch.
#[test]
fn a_peer_writing_while_released_defeats_retention() {
    let root = tmp("peer");
    let src = root.join("t.jsonl");
    transcript(&src, 5);

    let c = cache(&root);
    let (ours, _) = admit_and_fold(&c, "s", &src);
    c.release("s");

    // A peer takes the released entry and folds two more turns into it.
    {
        let peer = cache(&root);
        append(&src, &user("peer wrote this", 900));
        append(&src, &assistant("and this", 901));
        append(&src, &user("committing turn", 902));
        let (_, origin) = admit_and_fold(&peer, "s", &src);
        assert!(
            matches!(origin, Origin::Resumed { .. }),
            "the peer resumes our entry, so it really does write to it"
        );
        peer.release_all();
    }

    let (again, origin) = admit_and_fold(&c, "s", &src);
    assert!(
        !matches!(origin, Origin::Retained { .. }),
        "the backing moved under us: retention must be refused, got {origin:?}"
    );
    assert!(
        !std::sync::Arc::ptr_eq(&ours, &again),
        "and the stale session must be replaced, not re-armed"
    );
    let mut blocks: Vec<Block> = again
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    blocks.extend(again.pull_delta(again.epoch(), blocks.len()).provisional);
    assert_eq!(
        blocks,
        cold(&src),
        "however it rebuilt, it must equal a cold fold"
    );
    c.release_all();
}

/// A fold-version bump must defeat retention **through the wired path**, at an unchanged backing
/// length — the one case the length comparison cannot see, and the worst one, since retaining
/// here would splice blocks built by two different folds into one session with no visible seam.
///
/// The peer is simulated by rewriting the stream HEADER in place (same fold, same bytes below
/// it), which is exactly the state a differently-versioned binary would leave while the content
/// stream happens to end at the same offset.
#[test]
fn a_version_change_defeats_retention_end_to_end() {
    let root = tmp("retain-ver-e2e");
    let src = root.join("t.jsonl");
    transcript(&src, 4);

    let c = cache(&root);
    let (ours, _) = admit_and_fold(&c, "s", &src);
    c.release("s");
    let before = stream_lens(&root, "s");

    // Rewrite ONLY the header's fold version; every record and every block byte stays put.
    let dir = claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "s");
    let meta = dir.join("meta.jsonl");
    let raw = std::fs::read_to_string(&meta).unwrap();
    let (head, rest) = raw.split_once('\n').unwrap();
    let mut h: serde_json::Value = serde_json::from_str(head).unwrap();
    h["versions"]["fold"] = serde_json::json!(u16::MAX);
    std::fs::write(
        &meta,
        format!("{}\n{rest}", serde_json::to_string(&h).unwrap()),
    )
    .unwrap();
    assert_eq!(
        stream_lens(&root, "s").0,
        before.0,
        "the content stream is untouched — only the header moved"
    );

    let (again, origin) = admit_and_fold(&c, "s", &src);
    assert_eq!(
        origin,
        Origin::Cold(claude_replay_present::cache::ColdReason::VersionChanged),
        "a foreign fold must rebuild cold, never be retained"
    );
    assert!(!std::sync::Arc::ptr_eq(&ours, &again));
    let mut blocks: Vec<Block> = again
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    blocks.extend(again.pull_delta(again.epoch(), blocks.len()).provisional);
    assert_eq!(blocks, cold(&src), "and the rebuild equals a cold fold");
    c.release_all();
}

/// The guard itself, in isolation: `stream_unchanged` accepts this build's versions and rejects
/// any other. The end-to-end test above is what pins the WIRING; this pins the predicate.
#[test]
fn a_version_change_defeats_retention() {
    let root = tmp("retain-ver");
    let src = root.join("t.jsonl");
    transcript(&src, 4);

    let c = cache(&root);
    admit_and_fold(&c, "s", &src);
    c.release("s");

    // A cache on the same root, folding with a different version, must not retain the resident it
    // shares the process with.
    let newer: Cache = SessionCache::durable(
        Presentation::Tui,
        root.clone(),
        Versions {
            format: 1,
            fold: u16::MAX,
            flavor: None,
        },
    );
    newer.register("s", Transcript::open(Agent::CLAUDE, src.clone()));
    // The resident belongs to `c`, so `newer` cannot retain it in any case; what this pins is the
    // check itself — the header a re-admission reads no longer describes this fold.
    assert!(
        !claude_replay_present::cache::admit::stream_unchanged(
            &claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "s"),
            &src,
            &Versions {
                format: 1,
                fold: u16::MAX,
                flavor: None,
            },
        ),
        "a different fold version is not the entry we left"
    );
    assert!(
        claude_replay_present::cache::admit::stream_unchanged(
            &claude_replay_present::cache::admit::entry_dir(&root, Presentation::Tui, "s"),
            &src,
            &Versions::current(None),
        ),
        "and the matching one still is"
    );
    c.release_all();
}

/// **One admission, however many callers ask at once** (#169).
///
/// `admit` was not atomic: between a caller finding no resident and the cache installing one,
/// every other caller found no resident either. Each opened its own store on the same backing,
/// and `lock::acquire` denied none of them — none is a different process. So a session whose
/// first admission is slow (a cold fold of a large transcript, with a browser polling every two
/// seconds) got folded and WRITTEN by every request that arrived meanwhile. On disk that reads as
/// a record log whose lines carry several records each, scrambled and duplicated — one line held
/// six. Nothing detects it server-side; the page just never renders.
///
/// The property is exact and cheap to state: `make_store` runs ONCE, and every caller comes away
/// holding the same session.
#[test]
fn concurrent_admissions_of_one_session_open_exactly_one_store() {
    let root = tmp("race");
    let src = root.join("t.jsonl");
    transcript(&src, 6);

    let c = cache(&root);
    c.register("s", Transcript::open(Agent::CLAUDE, src.clone()));
    let opened = AtomicUsize::new(0);

    let sessions: Vec<_> = std::thread::scope(|scope| {
        let hands: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    match c.admit(
                        "s",
                        |dir| {
                            opened.fetch_add(1, Ordering::SeqCst);
                            ArcLog::open_append(&dir.join("blocks.jsonl"))
                        },
                        |_: &Holder<TuiNote>| false,
                    ) {
                        Admission::Owned { session, .. } => session,
                        Admission::Denied(_) => panic!("a free entry must be Owned"),
                    }
                })
            })
            .collect();
        hands.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert_eq!(
        opened.load(Ordering::SeqCst),
        1,
        "one backing, one writer — however many callers raced for it"
    );
    let first = &sessions[0];
    for s in &sessions {
        assert!(
            std::sync::Arc::ptr_eq(first, s),
            "every caller comes away with the SAME session"
        );
    }
    c.release_all();
    let _ = std::fs::remove_dir_all(&root);
}

/// **Resume-equals-cold for a Codex CHILD rollout** (PR #13). The Codex preprocessor is
/// stateful — the child's identity comes from the rollout's FIRST line — and a durable resume
/// starts above `replay_from`, where that line is never re-read. The adapter's comment promises
/// the author-prefix fallback classifies a post-resume `agent_message` equivalently; this pins
/// the promise: the follow-up assignment lands in the APPENDED region, after the resume point,
/// so it can only be classified by the fallback — and the result must equal a cold fold of the
/// whole file. `Origin::Resumed` is asserted so a silent cold rebuild cannot fake the pass.
#[test]
fn a_codex_child_rollout_resumes_equal_to_cold() {
    // Bootstrap (excluded from the child's view) + `task_started` + N completed child turns.
    // Several turns, because the durability frontier trails the open window — a rollout with
    // too few turns commits nothing, and a resume needs a committed prefix to resume FROM.
    fn head(turns: usize) -> String {
        let mut s = String::from(concat!(
            r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"session_meta","payload":{"id":"child-thread","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-thread","depth":1,"agent_path":"/root/review"}}}}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"session_meta","payload":{"id":"parent-thread","cwd":"/repo","source":"cli"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.514Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"parent turn copied into child"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-08-09T22:48:04.660Z","type":"event_msg","payload":{"type":"task_started","turn_id":"child-turn","started_at":1786315684}}"#,
            "\n",
        ));
        for i in 0..turns {
            let t = 5 + i * 2;
            s.push_str(&format!(
                concat!(
                    r#"{{"timestamp":"2026-08-09T22:48:{:02}.000Z","type":"response_item","payload":{{"type":"agent_message","author":"/root","recipient":"/root/review","content":[{{"type":"input_text","text":"Message Type: TASK_{}\nPayload:\nstep {}"}}]}}}}"#,
                    "\n",
                    r#"{{"timestamp":"2026-08-09T22:48:{:02}.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"answer {}"}}]}}}}"#,
                    "\n",
                ),
                t, i, i, t + 1, i
            ));
        }
        s
    }
    const TAIL: &str = concat!(
        r#"{"timestamp":"2026-08-09T22:48:40.000Z","type":"response_item","payload":{"type":"agent_message","author":"/root","recipient":"/root/review","content":[{"type":"input_text","text":"Message Type: FOLLOW_UP\nPayload:\nalso check the tests"}]}}"#,
        "\n",
        r#"{"timestamp":"2026-08-09T22:48:41.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":40,"cached_input_tokens":0,"output_tokens":9}}}}"#,
        "\n",
        r#"{"timestamp":"2026-08-09T22:48:42.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"tests reviewed"}]}}"#,
        "\n",
    );

    let root = tmp("codex-child");
    let src = root.join("rollout-child.jsonl");
    std::fs::write(&src, head(4)).unwrap();

    // Run 1: fold to EOF — the parent snapshot is excluded here, and only here — then let go.
    {
        let c = cache(&root);
        c.register("child", Transcript::open(Agent::CODEX, src.clone()));
        assert!(matches!(
            c.admit(
                "child",
                |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
                |_: &Holder<TuiNote>| false,
            ),
            Admission::Owned { .. }
        ));
        let _ = c
            .poll_view("child", ArcLog::memory)
            .expect("registered")
            .expect("readable");
        c.release_all();
    }

    // The rollout grows while nobody holds it: a follow-up assignment plus its answer.
    append(&src, TAIL);

    // Run 2: must RESUME — and resume to exactly what a cold fold of the whole file produces.
    let c = cache(&root);
    c.register("child", Transcript::open(Agent::CODEX, src.clone()));
    let (session, origin) = match c.admit(
        "child",
        |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
        |_: &Holder<TuiNote>| false,
    ) {
        Admission::Owned { session, origin } => (session, origin),
        Admission::Denied(_) => panic!("a free entry must be Owned"),
    };
    assert!(
        matches!(origin, Origin::Resumed { .. }),
        "must resume, not silently re-fold (a cold rebuild would fake the equality): {origin:?}"
    );
    let d = c
        .poll_view("child", ArcLog::memory)
        .expect("registered")
        .expect("readable");
    let mut got: Vec<Block> = session
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    got.extend(d.provisional.iter().map(|a| a.as_ref().clone()));

    let cold = parse_session_as(Agent::CODEX, &src).unwrap().blocks();
    assert_eq!(
        got, cold,
        "a resumed Codex child fold must equal a cold one"
    );
    assert!(
        got.iter()
            .any(|b| matches!(b, Block::UserText(t) if t.contains("FOLLOW_UP"))),
        "the post-resume assignment must classify as a user turn via the fallback"
    );
    assert!(
        !got.iter()
            .any(|b| matches!(b, Block::UserText(t) if t.contains("parent turn copied"))),
        "and the cloned parent bootstrap must stay excluded"
    );
    c.release_all();
    let _ = std::fs::remove_dir_all(&root);
}

/// A modern Codex session learns from its first `session_meta` that JavaScript `exec` wrappers
/// are transport and that the following `CommandExecution` is the user-visible action. A durable
/// resume starts above that first line. Losing the adapter's opaque preprocessor state there made
/// the monitor render `const r = await tools.exec_command(...)` as Bash and ignore the semantic
/// Read/Grep/List classification, even though a cold standalone replay was correct.
#[test]
fn a_resumed_codex_session_keeps_semantic_exec_adapter_state() {
    fn push(s: &mut String, value: serde_json::Value) {
        s.push_str(&value.to_string());
        s.push('\n');
    }
    fn has_tool(blocks: &[Block], name: &str, target: &str) -> bool {
        blocks.iter().any(|block| match block {
            Block::ToolUse {
                name: got_name,
                target: got_target,
                ..
            } => got_name == name && got_target == target,
            Block::Thinking { tools, .. } => has_tool(tools, name, target),
            _ => false,
        })
    }
    fn has_transport_wrapper(blocks: &[Block]) -> bool {
        blocks.iter().any(|block| match block {
            Block::ToolUse { target, .. } => target.contains("tools.exec_command"),
            Block::Thinking { tools, .. } => has_transport_wrapper(tools),
            _ => false,
        })
    }

    let root = tmp("codex-semantic-resume");
    let src = root.join("rollout-modern.jsonl");
    let mut head = String::new();
    push(
        &mut head,
        serde_json::json!({
            "timestamp":"2026-08-19T07:00:00Z",
            "type":"session_meta",
            "payload":{"id":"modern","cwd":"/repo","cli_version":"0.147.0","source":"cli"}
        }),
    );
    push(
        &mut head,
        serde_json::json!({
            "timestamp":"2026-08-19T07:00:00Z",
            "type":"turn_context",
            "payload":{"model":"gpt-5.6-sol"}
        }),
    );
    push(
        &mut head,
        serde_json::json!({
            "timestamp":"2026-08-19T07:00:00Z",
            "type":"event_msg",
            "payload":{"type":"token_count","info":{
                "model_context_window":258400,
                "last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110},
                "total_token_usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":10}
            }}
        }),
    );
    for i in 0..5 {
        push(
            &mut head,
            serde_json::json!({
                "timestamp":format!("2026-08-19T07:00:{:02}Z", i * 2 + 1),
                "type":"response_item",
                "payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("ask {i}")}]}
            }),
        );
        push(
            &mut head,
            serde_json::json!({
                "timestamp":format!("2026-08-19T07:00:{:02}Z", i * 2 + 2),
                "type":"response_item",
                "payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("answer {i}")}]}
            }),
        );
    }
    std::fs::write(&src, head).unwrap();

    // Run 1 establishes a committed prefix and a resume point above session_meta.
    {
        let c = cache(&root);
        c.register("modern", Transcript::open(Agent::CODEX, src.clone()));
        assert!(matches!(
            c.admit(
                "modern",
                |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
                |_: &Holder<TuiNote>| false,
            ),
            Admission::Owned { .. }
        ));
        let _ = c
            .poll_view("modern", ArcLog::memory)
            .expect("registered")
            .expect("readable");
        c.release_all();
    }

    let mut tail = String::new();
    push(
        &mut tail,
        serde_json::json!({
            "timestamp":"2026-08-19T07:01:00Z",
            "type":"response_item",
            "payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"inspect it"}]}
        }),
    );
    push(
        &mut tail,
        serde_json::json!({
            "timestamp":"2026-08-19T07:01:01Z",
            "type":"event_msg",
            "payload":{"type":"token_count","info":{
                "model_context_window":258400,
                "last_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55},
                "total_token_usage":{"input_tokens":150,"cached_input_tokens":120,"output_tokens":15}
            }}
        }),
    );
    push(
        &mut tail,
        serde_json::json!({
            "timestamp":"2026-08-19T07:01:01Z",
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call","name":"exec","call_id":"outer",
                "input":"const r = await tools.exec_command({ cmd: \"sed -n '1,20p' src/lib.rs\", workdir: \"/repo\" }); text(r.output);"
            }
        }),
    );
    push(
        &mut tail,
        serde_json::json!({
            "timestamp":"2026-08-19T07:01:02Z",
            "type":"response_item",
            "payload":{"type":"custom_tool_call_output","call_id":"outer","output":"Script completed\nOutput:\nbody\n"}
        }),
    );
    push(
        &mut tail,
        serde_json::json!({
            "timestamp":"2026-08-19T07:01:03Z",
            "type":"event_msg",
            "payload":{"type":"item_completed","item":{
                "type":"CommandExecution","id":"exec-read",
                "command":["/bin/zsh","-lc","sed -n '1,20p' src/lib.rs"],
                "cwd":"file:///repo","status":"completed","exit_code":0,"stdout":"body\n",
                "parsed_cmd":[{"type":"read","cmd":"sed -n '1,20p' src/lib.rs","path":"src/lib.rs"}]
            }}
        }),
    );
    push(
        &mut tail,
        serde_json::json!({
            "timestamp":"2026-08-19T07:01:04Z",
            "type":"response_item",
            "payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}
        }),
    );
    append(&src, &tail);

    let c = cache(&root);
    c.register("modern", Transcript::open(Agent::CODEX, src.clone()));
    let (session, origin) = match c.admit(
        "modern",
        |dir| ArcLog::open_append(&dir.join("blocks.jsonl")),
        |_: &Holder<TuiNote>| false,
    ) {
        Admission::Owned { session, origin } => (session, origin),
        Admission::Denied(_) => panic!("a free entry must be Owned"),
    };
    assert!(matches!(origin, Origin::Resumed { .. }), "{origin:?}");
    let d = c
        .poll_view("modern", ArcLog::memory)
        .expect("registered")
        .expect("readable");
    let resumed_metrics = d.metrics.clone();
    let mut got: Vec<Block> = session
        .committed_arcs()
        .iter()
        .map(|block| block.as_ref().clone())
        .collect();
    got.extend(d.provisional.iter().map(|block| block.as_ref().clone()));

    let cold = parse_session_as(Agent::CODEX, &src).unwrap();
    assert_eq!(
        got,
        cold.blocks(),
        "resumed modern Codex must equal its cold fold"
    );
    assert_eq!(
        resumed_metrics, cold.metrics,
        "the HTML/live accumulator must price the same Codex usage as a cold metrics fold"
    );
    assert!(!resumed_metrics.per_model.contains_key(""));
    assert!(resumed_metrics.cost_usd.is_some());
    assert!(has_tool(&got, "Read", "src/lib.rs"));
    assert!(!has_transport_wrapper(&got));
    c.release_all();
    let _ = std::fs::remove_dir_all(&root);
}
