//! **#96 §12.1 — crash consistency.**
//!
//! Both streams are append-only and written under one lock, so every crash leaves a **prefix**
//! of what a clean run would have written. That makes the failure space enumerable rather than
//! hopeful: build the complete pair once, then truncate both — surgically first, then randomly
//! — and assert every survivor resumes to the same place.
//!
//! **The oracle is a cold parse.** For a transcript `T`, a resumed session folded to EOF must
//! equal `cold(T)` **block for block**, not merely in totals. Totals hide exactly the
//! corruption that matters: a resume that drops a block and gains another still counts right.
//!
//! Surgical before random, because a random-only harness reports "seed 41 failed" instead of
//! naming the shape that broke.
//!
//! **What this harness does NOT cover, stated so nobody assumes it does.** Mutating away the
//! restored fold clocks (`Resume::{prev_ts, pending_ts}`) is *not* detected, and probing showed
//! why: `replay_from` is always a turn-opening line, a `Thinking` block can never sit on one, and
//! the re-read re-establishes both clocks from lines below the first block that consumes them.
//! The design justifies persisting them as "without `prev_ts` a `Thinking` on the first re-read
//! line renders `None` where a cold fold gives `Some`" — which appears unreachable, since that
//! line cannot carry a `Thinking`. They are kept as 16 defensive bytes rather than removed on an
//! "I could not construct it" argument, but this harness does not prove them necessary and no
//! future change should assume it does.

use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::engine::meta_stream::{align, MetaRecord};
use claude_replay_engine::model::Block;
use claude_replay_engine::SessionAccumulator;

/// A transcript exercising the shapes the design calls out: a coalesced run, a spawn whose id
/// arrives late, a duplicate agent id, a completion through the queue, a task create joined by
/// its result, a model switch, and several commits.
fn transcript() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    let mut t = 0;
    let mut ts = || {
        t += 1;
        format!("2026-08-05T10:{:02}:{:02}Z", t / 60, t % 60)
    };
    v.push(format!(r#"{{"type":"user","cwd":"/r","message":{{"role":"user","content":[{{"type":"text","text":"go"}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"assistant","message":{{"role":"assistant","model":"claude-opus-5","usage":{{"input_tokens":10,"output_tokens":5}},"content":[{{"type":"tool_use","id":"b1","name":"Bash","input":{{"command":"ls"}}}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"b1","content":"out"}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_A","name":"Task","input":{{"subagent_type":"general-purpose","description":"child one","prompt":"go"}}}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"user","toolUseResult":{{"agentId":"aXYZ","status":"async_launched"}},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_A","content":"launched"}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"tk1","name":"TaskCreate","input":{{"subject":"s","description":"d","active_form":"a"}}}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tk1","content":"Created task #12: s"}}]}},"timestamp":"{}"}}"#, ts()));
    for i in 0..6 {
        v.push(format!(r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"turn {i}"}}]}},"timestamp":"{}"}}"#, ts()));
        v.push(format!(r#"{{"type":"assistant","message":{{"role":"assistant","model":"claude-fable-5","usage":{{"input_tokens":2,"output_tokens":3}},"content":[{{"type":"text","text":"reply {i}"}}]}},"timestamp":"{}"}}"#, ts()));
    }
    v.push(format!(r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"{}","content":"<task-notification>\n<task-id>aXYZ</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"child one\" finished</summary>\n<result>done</result>\n</task-notification>"}}"#, ts()));
    v.push(format!(
        r#"{{"type":"queue-operation","operation":"dequeue","timestamp":"{}"}}"#,
        ts()
    ));
    v.push(format!(r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"last"}}]}},"timestamp":"{}"}}"#, ts()));
    // These must come AFTER the final turn, because that turn's line is `replay_from`: only a
    // block built on a line at/after it exercises the RESTORED clocks. A thinking block placed
    // before it is re-folded from re-read context and would hide a dropped clock entirely
    // (learned by mutation — it did).
    v.push(format!(r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"thinking","thinking":"weighing it up"}}]}},"timestamp":"{}"}}"#, ts()));
    v.push(format!(r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"b9","name":"Read","input":{{"file_path":"/r/deep/file.rs"}}}}]}},"timestamp":"{}"}}"#, ts()));
    v
}

fn offsets(lines: &[String]) -> Vec<u64> {
    let mut off = 0u64;
    lines
        .iter()
        .map(|l| {
            let at = off;
            off += l.len() as u64 + 1;
            at
        })
        .collect()
}

/// A full run: the committed blocks and the meta records a clean session would have written.
fn clean_run(lines: &[String]) -> (Vec<Block>, Vec<MetaRecord>) {
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut recs = Vec::new();
    for (l, off) in lines.iter().zip(offsets(lines)) {
        acc.advance_at(off, l);
        recs.extend(acc.drain_meta());
    }
    (acc.committed_tail(0), recs)
}

/// Everything a session knows about itself. Blocks ALONE are not enough: the task list, the
/// per-turn stamps and the metrics live outside them, so a resume that dropped any of those
/// would pass a block-only comparison. (Learned by mutation — a block-only oracle missed four
/// of six injected faults.)
#[derive(Debug, PartialEq)]
struct FullState {
    blocks: Vec<Block>,
    meta: claude_replay_engine::SessionMeta,
    tasks: claude_replay_engine::engine::tasks::TaskList,
    user_times: Vec<Option<f64>>,
    tokens: String,
}

fn full_state(acc: &mut SessionAccumulator) -> FullState {
    let snap = acc.snapshot();
    let m = acc.open_read().metrics;
    FullState {
        blocks: snap.blocks(),
        meta: acc.session_meta(),
        tasks: acc.tasks().clone(),
        user_times: acc.open_finalized().1,
        // Per-model totals as a stable string — the cost figure itself depends on pricing,
        // which is not what this harness is testing.
        tokens: format!("{:?}", m.per_model),
    }
}

/// The oracle: a from-scratch fold.
fn cold(lines: &[String]) -> FullState {
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    for (l, off) in lines.iter().zip(offsets(lines)) {
        acc.advance_at(off, l);
    }
    full_state(&mut acc)
}

/// Load a truncated pair, resume, fold the rest of `T`, and return the final block list —
/// or `None` when nothing was resumable (a legitimate cold rebuild).
fn resume_and_finish(
    lines: &[String],
    committed: &[Block],
    recs: &[MetaRecord],
) -> Option<(FullState, usize)> {
    let a = align(recs, committed.len())?;
    let mut acc = SessionAccumulator::restore(
        &ClaudeAdapter,
        claude_replay_engine::engine::session::InMemoryStore,
        committed[..a.committed].to_vec(),
        a.meta,
        &a.resume,
    );
    // Feed only the lines at or after the partition — the whole point of a resume.
    let offs = offsets(lines);
    let mut parsed = 0;
    for (l, off) in lines.iter().zip(&offs) {
        if *off >= a.resume.replay_from {
            acc.advance_at(*off, l);
            parsed += 1;
        }
    }
    Some((full_state(&mut acc), parsed))
}

/// Every truncation of both streams must resume to a block-identical session.
///
/// Surgical shapes, each named so a failure says WHICH case broke:
/// records × committed, including meta-ahead-of-content and content-ahead-of-meta (the writer
/// died between the two appends, in each order), and the degenerate empty/header-only ends.
#[test]
fn every_truncation_pair_resumes_to_an_identical_session() {
    let lines = transcript();
    let want = cold(&lines);
    let (committed, recs) = clean_run(&lines);
    assert!(
        recs.iter().any(|r| r.resume.is_some()),
        "fixture must commit"
    );
    assert!(committed.len() > 3, "fixture must commit several blocks");

    let mut resumed = 0;
    for nr in 0..=recs.len() {
        for nc in 0..=committed.len() {
            // `None` means no resume point survived — a cold rebuild, which is always correct.
            if let Some((got, _)) = resume_and_finish(&lines, &committed[..nc], &recs[..nr]) {
                resumed += 1;
                assert_eq!(
                    got, want,
                    "records[..{nr}] + committed[..{nc}] resumed to a DIFFERENT session"
                );
            }
        }
    }
    assert!(
        resumed > 5,
        "only {resumed} pairs were resumable — harness is not exercising resume"
    );
}

/// A resume must actually SKIP work — otherwise the cache silently degrades to
/// cold-rebuild-every-time and every equality assertion above still passes.
#[test]
fn a_full_cache_reparses_only_the_open_turn() {
    let lines = transcript();
    let (committed, recs) = clean_run(&lines);
    let (_, parsed) = resume_and_finish(&lines, &committed, &recs).expect("resumable");
    assert!(
        parsed < lines.len(),
        "resumed but re-read all {} lines — no work was saved",
        lines.len()
    );
}

/// A torn tail costs at most the LAST commit (§2). If dropping one record ever cost more, the
/// cache would quietly fall back further and further while every equality test still passed.
#[test]
fn a_torn_tail_costs_at_most_one_commit() {
    let lines = transcript();
    let (committed, recs) = clean_run(&lines);
    let full = align(&recs, committed.len()).expect("resumable");
    let ids: Vec<usize> = recs
        .iter()
        .filter_map(|r| r.resume.as_ref().map(|x| x.id))
        .collect();
    let torn = align(&recs[..recs.len() - 1], committed.len());
    if let Some(t) = torn {
        let prev = ids
            .iter()
            .rev()
            .find(|&&i| i < full.committed)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            t.committed, prev,
            "a torn tail must fall back exactly one commit"
        );
    }
}

/// Alignment is a pure function, so the disagreement cases need no filesystem: meta describing
/// commits the content stream cannot corroborate must be IGNORED, never trusted.
#[test]
fn meta_ahead_of_content_is_ignored() {
    let lines = transcript();
    let (committed, recs) = clean_run(&lines);
    let full = align(&recs, committed.len()).expect("resumable");
    // Pretend the content stream lost its tail: alignment must not pick a later record.
    let short = align(&recs, committed.len() - 1);
    if let Some(s) = short {
        assert!(
            s.committed < committed.len(),
            "aligned to {} commits with only {} blocks on disk",
            s.committed,
            committed.len() - 1
        );
        assert!(s.committed <= full.committed);
    }
}
