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

/// An ephemeral cache denies with `NoCacheFlag` and writes nothing — `--no-cache` is a real path,
/// not a degraded one, and it must leave the durable root untouched.
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

    // The explicit cache-less path still serves a correct session.
    let ss = c.open_uncached("s", ArcLog::memory()).expect("registered");
    let d = c.poll_view("s", ArcLog::memory).unwrap().unwrap();
    let mut got: Vec<Block> = ss
        .committed_arcs()
        .iter()
        .map(|a| a.as_ref().clone())
        .collect();
    got.extend(d.provisional.iter().map(|a| a.as_ref().clone()));
    assert_eq!(got, cold(&src), "cache-less is still a correct session");
    assert!(
        !root.join("tui").exists(),
        "and it still wrote nothing durable"
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
