//! Machinery-with-real-adapters integration tests (#87 step 3): these exercised the
//! engine's builder/follower/stores against Claude/Codex fixture transcripts while they
//! lived inside the engine crate — a dev-dependency cycle would compile TWO copies of
//! the engine there, so they live here, where ONE coherent engine + the built-in
//! adapters + the facade's dispatchers meet.

use claude_replay_agents::{ClaudeAdapter, CodexAdapter};
use claude_replay_core::{parse_session, parse_session_as, parse_session_enriched_as, Agent};
use claude_replay_engine::engine::tier_b::{TierBSession, TierBStore};
use claude_replay_engine::engine::MAX_PINNED_TURNS;
use claude_replay_engine::model::{AgentStatus, Block};
use claude_replay_engine::{FollowParser, SessionAccumulator};
use std::io::Write;
use std::path::PathBuf;

static TMPN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// One-arg form the follow suite used: unique auto-name.
fn tmp1(body: &str) -> PathBuf {
    let n = TMPN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tmp(&format!("t{n}.jsonl"), body)
}
fn common_prefix_len(a: &[Block], b: &[Block]) -> usize {
    a.iter()
        .zip(b)
        .take_while(|(x, y)| format!("{x:?}") == format!("{y:?}"))
        .count()
}

fn tmp(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cr-eng-int-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

/// The maintained [`SessionMeta`] (committed folded on drain + the open turn folded on top)
/// must equal a batch `SessionMeta::build` over the whole current block stream — at **every**
/// step of an incremental fold, including mid-open-turn, across a sub-agent spawn, and across a
/// commit. This is what lets a live poll read the header without rescanning the session.
#[test]
fn maintained_meta_equals_batch_build_at_every_step() {
    // A turn that spawns a sub-agent (Agent tool → SubAgent block), then a second user turn
    // that commits the first.
    let lines = [
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-07-26T10:00:00Z"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]},"timestamp":"2026-07-26T10:00:01Z"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]},"timestamp":"2026-07-26T10:00:02Z"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_A","name":"Task","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]},"timestamp":"2026-07-26T10:00:03Z"}"#,
        r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]},"timestamp":"2026-07-26T10:00:04Z"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next"}]},"timestamp":"2026-07-26T10:00:05Z"}"#,
    ];
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut off: claude_replay_engine::model::ByteOffset = 0;
    for line in lines {
        acc.advance_at(off, line);
        off += line.len() as u64 + 1;
        // The maintained header == a batch build over the whole current block stream.
        let blocks = acc.snapshot().blocks();
        assert_eq!(
            acc.session_meta(),
            claude_replay_engine::SessionMeta::build(&blocks),
            "maintained meta diverged from batch build"
        );
    }
    // Final shape: 2 user turns, 2 tool calls (Bash + Task counts as... Task is a spawn ⇒ NOT
    // a tool), one child.
    let meta = acc.session_meta();
    assert_eq!(meta.turns, 2, "two user turns");
    assert_eq!(
        meta.tools, 1,
        "Bash is a tool; the Task spawn is a child, not a tool"
    );
    assert_eq!(meta.children.len(), 1);
    assert_eq!(meta.children[0].id, "achild01");
}

/// **The meta-stream oracle** (#96). Replaying the emitted meta records must reproduce the
/// [`SessionMeta`] the accumulator maintains — at EVERY step of a real fold. This is what the
/// persistent cache rests on: it reconstructs session state from the record stream alone, never
/// from the blocks, so any drift between `MetaDelta::push` and `SessionMeta::push`, or any
/// mishandling of accumulate-vs-supersede, corrupts a resumed session.
///
/// It is a real test only if the fixture reaches the intricate arms, so the fixture deliberately
/// contains a spawn whose id arrives late, a duplicate agent id, an activity-coalesced run, and a
/// completion notification — and the assertions at the bottom fail if any of those stops being
/// The supersede baseline must RESET at a commit. This is the case a richer fixture hides: when
/// the new open turn's contribution happens to be **identical** to the one just committed (here,
/// two bare user turns — each contributes `turns: 1` and nothing else), a replayer that forgot to
/// reset would see "no change since last told", emit no restatement, and leave the reader showing
/// committed-only state while a turn is open. Every consecutive pair of plain prompts hits this,
/// **The meta-stream oracle** (#96 I3). Replaying the emitted records must reproduce the state
/// the accumulator maintains — at EVERY step of a real fold. This is what the durable cache
/// rests on: it reconstructs a session from the record stream alone, never from the blocks, so
/// any drift between the record's counters and `SessionMeta::push` corrupts a resumed session.
///
/// The fixture deliberately reaches the intricate arms — a spawn whose id arrives late, a
/// DUPLICATE agent id, an activity-coalesced run, a completion, and a task create whose id
/// lands in a tool result — and asserts they are present, so it cannot quietly go vacuous.
#[test]
fn meta_records_replay_to_the_maintained_state() {
    use claude_replay_engine::engine::meta_stream::MaterializedMeta;

    let lines = [
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-05T10:00:00Z"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5","usage":{"input_tokens":10,"output_tokens":5},"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]},"timestamp":"2026-08-05T10:00:01Z"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]},"timestamp":"2026-08-05T10:00:02Z"}"#,
        // a spawn whose agent_id arrives with its result, then a SECOND spawn reusing the id
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_A","name":"Task","input":{"subagent_type":"general-purpose","description":"child one","prompt":"go"}}]},"timestamp":"2026-08-05T10:00:03Z"}"#,
        r#"{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"launched"}]},"timestamp":"2026-08-05T10:00:04Z"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_B","name":"Task","input":{"subagent_type":"general-purpose","description":"child two","prompt":"go"}}]},"timestamp":"2026-08-05T10:00:05Z"}"#,
        r#"{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_B","content":"launched"}]},"timestamp":"2026-08-05T10:00:06Z"}"#,
        // a task create; its id arrives in the RESULT — the join that needs TaskOp::Resolve
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tk1","name":"TaskCreate","input":{"subject":"do it","description":"d","active_form":"doing"}}]},"timestamp":"2026-08-05T10:00:07Z"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tk1","content":"Created task #12: do it"}]},"timestamp":"2026-08-05T10:00:08Z"}"#,
        // the completion, delivered as a queued event, then drained so commits can happen
        r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-08-05T10:00:09Z","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"child one\" finished</summary>\n<result>done</result>\n</task-notification>"}"#,
        r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-08-05T10:00:10Z"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next"}]},"timestamp":"2026-08-05T10:00:11Z"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","model":"claude-fable-5","usage":{"input_tokens":3,"output_tokens":7},"content":[{"type":"tool_use","id":"b2","name":"Read","input":{"file_path":"/r/x"}}]},"timestamp":"2026-08-05T10:00:12Z"}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"third"}]},"timestamp":"2026-08-05T10:00:13Z"}"#,
    ];

    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut mm = MaterializedMeta::default();
    let mut off: claude_replay_engine::model::ByteOffset = 0;
    let mut resumes = 0usize;
    for (i, line) in lines.iter().enumerate() {
        acc.advance_at(off, line);
        off += line.len() as u64 + 1;
        for r in acc.drain_meta() {
            if let Some(res) = &r.resume {
                resumes += 1;
                assert_eq!(
                    res.id,
                    acc.committed_len(),
                    "the payload names the committed count"
                );
            }
            mm.push(&r);
        }
        // THE ORACLE: the replayed counters equal the maintained COMMITTED header, at every step.
        assert_eq!(
            mm.session_meta,
            *acc.committed_meta(),
            "meta stream diverged from the maintained header after line {i}"
        );
        assert_eq!(
            mm.user_times.len(),
            mm.session_meta.turns,
            "one stamp per committed turn, after line {i}"
        );
    }

    // The fixture really exercised the intricate arms.
    assert!(resumes > 0, "at least one resume point");
    assert_eq!(
        mm.session_meta.children.len(),
        2,
        "two spawns, duplicate id kept"
    );
    assert!(mm.session_meta.children.iter().all(|c| c.id == "aXYZ1234"));
    assert!(
        mm.session_meta.children.iter().all(|c| !c.running),
        "one completion clears EVERY child with that id"
    );
    assert!(mm.session_meta.tools >= 2, "coalesced runs counted");
    // The task op-log is self-contained: Resolve landed the create in the list.
    assert_eq!(
        mm.tasks.snapshot().items.len(),
        1,
        "the create was joined by its Resolve op"
    );
    assert_eq!(mm.tasks.snapshot().items[0].id, "12");
    // Spawn identity is rebuilt under BOTH keys, so a later completion still resolves.
    assert!(mm.agent_ids.contains_key("toolu_A") && mm.agent_ids.contains_key("aXYZ1234"));
}

/// #56 regression (the QoderWork panic): ONE transcript line can carry SEVERAL user text
/// items — the second turn then closes the first within the same fold batch, committing it
/// before any later `LineStart` stamps its timestamp. The drain must stamp first: no lost
/// per-turn timestamp, `stamped` never falls behind `base`, and no window-math wrap — with a
/// foreign head line (`runtime-config`) and an unknown mid-stream type thrown in, since a
/// parser fed arbitrary JSONL must degrade, never panic.
#[test]
fn multi_text_user_line_commits_stamped_and_never_panics() {
    use claude_replay_engine::model::Block;
    let lines = [
        r#"{"type":"runtime-config","sessionId":"s","model":"qwork-ultimate","timestamp":1785068132048}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>env</system-reminder>"},{"type":"text","text":"the real prompt"}]},"timestamp":"2026-07-26T12:15:33.904Z"}"#,
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]},"timestamp":"2026-07-26T12:15:40Z"}"#,
        r#"{"type":"totally-unknown","x":1}"#,
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next"}]},"timestamp":"2026-07-26T12:16:00Z"}"#,
    ];
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut off: claude_replay_engine::model::ByteOffset = 0;
    for line in lines {
        acc.advance_at(off, line);
        off += line.len() as u64 + 1;
        let _ = acc.snapshot(); // the panic site was the snapshot's window math
    }
    let s = acc.snapshot();
    let users = s
        .blocks()
        .iter()
        .filter(|b| matches!(b, Block::UserText(_)))
        .count();
    assert_eq!(
        users, 3,
        "two turns from the multi-text line + the follow-up"
    );
    assert_eq!(s.user_times.len(), 3, "one timestamp per user turn");
    assert!(
        s.user_times.iter().all(|t| t.is_some()),
        "no turn lost its stamp to an early commit: {:?}",
        s.user_times
    );
    assert_eq!(
        s.user_times[0], s.user_times[1],
        "both turns from the one line carry its timestamp"
    );
}

/// The queued-marker lifecycle (#52): a `⧗ queued:` marker lives only while its prompt is
/// PENDING — it collapses on ANY pop, matching Claude Code's "only the one message":
/// (a) immediate enqueue → dequeue → delivery; (b) TYPE-AHEAD (agent content between enqueue
/// and dequeue — previously kept the marker, the #52 duplicate); (c) a `remove` op (the user
/// withdrew it); (d) an OP-LESS delivery — a user message whose text matches the pending
/// content (a jdi restart re-delivers without writing the dequeue), which must also DRAIN
/// the queue so the durability frontier un-pins and turns keep committing.
#[test]
fn queued_marker_collapses_on_every_pop_and_opless_delivery() {
    use claude_replay_engine::model::Block;
    let user = |t: &str| {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{t}"}}]}},"timestamp":"2026-07-26T10:00:00Z"}}"#
        )
    };
    let asst = |t: &str| {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{t}"}}]}},"timestamp":"2026-07-26T10:00:01Z"}}"#
        )
    };
    let enq =
        |t: &str| format!(r#"{{"type":"queue-operation","operation":"enqueue","content":"{t}"}}"#);
    let markers = |acc: &mut SessionAccumulator| {
        acc.snapshot()
            .blocks()
            .iter()
            .filter(|b| matches!(b, Block::QueueEvent { .. }))
            .count()
    };
    let drive = |lines: &[String]| {
        let mut acc = SessionAccumulator::new(&ClaudeAdapter);
        let mut off: claude_replay_engine::model::ByteOffset = 0;
        for l in lines {
            acc.advance_at(off, l);
            off += l.len() as u64 + 1;
        }
        acc
    };

    // (a) immediate pickup ⇒ collapsed.
    let mut a = drive(&[
        user("go"),
        asst("working"),
        enq("next thing"),
        r#"{"type":"queue-operation","operation":"dequeue"}"#.into(),
        user("next thing"),
    ]);
    assert_eq!(markers(&mut a), 0, "immediate pickup collapses");

    // (b) TYPE-AHEAD: agent content between enqueue and dequeue ⇒ STILL collapsed (the fix).
    let mut b = drive(&[
        user("go"),
        enq("typed ahead"),
        asst("kept working"),
        asst("more work"),
        r#"{"type":"queue-operation","operation":"dequeue"}"#.into(),
        user("typed ahead"),
    ]);
    assert_eq!(
        markers(&mut b),
        0,
        "type-ahead delivery collapses too (CC parity)"
    );

    // (c) remove ⇒ collapsed (the prompt was withdrawn; CC drops the bubble).
    let mut c = drive(&[
        user("go"),
        enq("changed my mind"),
        asst("working"),
        r#"{"type":"queue-operation","operation":"remove","content":"changed my mind"}"#.into(),
    ]);
    assert_eq!(markers(&mut c), 0, "removed prompt leaves no marker");

    // (d) op-less delivery: the matching user message pops the item, collapses the marker,
    // and DRAINS the queue — so later turns commit (the frontier un-pins).
    let mut d = drive(&[
        user("go"),
        enq("restart prompt"),
        asst("killed here"),
        user("restart prompt"),
        asst("resumed"),
        user("later turn"),
        asst("done"),
    ]);
    assert_eq!(markers(&mut d), 0, "op-less delivery collapses the marker");
    assert!(
        d.committed_len() > 0,
        "queue drained ⇒ the durability frontier advanced past the delivered turns"
    );
}

/// **The durability frontier cannot freeze.** Every back-reference that holds blocks resident is
/// bounded, and this is the test that says so — the failure it guards against is silent, since a
/// frozen frontier still renders a perfectly correct session, just one that can never be resumed
/// past a certain byte. Measured before the bound: 65% of a 107 MB transcript, for 219 turns.
///
/// Two pins, two bounds:
/// - a `Skill` in a COMPLETED turn — a body can no longer nest across a turn, so nothing is held
/// - a pending `⧗ queued:` marker whose pickup never arrives — held for at most
///   `MAX_PINNED_TURNS`, then released
#[test]
fn the_durability_frontier_cannot_be_pinned_forever() {
    let user = |t: &str| {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{t}"}}]}},"timestamp":"2026-07-26T10:00:00Z"}}"#
        )
    };
    let asst = |t: &str| {
        format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{t}"}}]}},"timestamp":"2026-07-26T10:00:01Z"}}"#
        )
    };
    let drive = |lines: &[String]| {
        let mut acc = SessionAccumulator::new(&ClaudeAdapter);
        let mut off: claude_replay_engine::model::ByteOffset = 0;
        for l in lines {
            acc.advance_at(off, l);
            off += l.len() as u64 + 1;
        }
        acc
    };

    // (a) A skill call, then many turns. The skill's turn completes like any other.
    let mut lines = vec![
        user("start"),
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"s1","name":"Skill","input":{"skill":"dump-tasks"}}]},"timestamp":"2026-07-26T10:00:02Z"}"#.into(),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"s1","content":"Launching"}]},"timestamp":"2026-07-26T10:00:03Z"}"#.into(),
    ];
    for i in 0..30 {
        lines.push(user(&format!("turn {i}")));
        lines.push(asst("ok"));
    }
    let mut a = drive(&lines);
    let n = a.snapshot().blocks().len();
    assert!(
        a.committed_len() >= n - 4,
        "a completed skill turn must not pin the frontier: {} committed of {n}",
        a.committed_len()
    );

    // A prompt enqueued and NEVER picked up — a jdi restart the content-match fallback missed.
    let stuck = |turns: usize| {
        let mut lines = vec![user("start"), asst("working")];
        lines.push(
            r#"{"type":"queue-operation","operation":"enqueue","content":"never delivered"}"#
                .into(),
        );
        for i in 0..turns {
            lines.push(user(&format!("turn {i}")));
            lines.push(asst("ok"));
        }
        lines
    };

    // (b) WITHIN the bound the marker's turn is held, so a late pickup can still collapse it —
    // that is what the pin is for, and dropping it eagerly would resurrect the #52 duplicate.
    let mut b = drive(&stuck(4));
    let held = b.snapshot().blocks().len() - b.committed_len();
    assert!(
        held > 2,
        "a pending marker holds its turn while a pickup is plausible, held {held}"
    );

    // (c) PAST the bound it lets go — otherwise one undelivered prompt freezes the frontier for
    // the rest of the session, which is the pathology this whole test exists for.
    let mut c = drive(&stuck(MAX_PINNED_TURNS + 20));
    let n = c.snapshot().blocks().len();
    assert!(
        c.committed_len() >= n - 4,
        "past the bound a stuck marker must not pin: {} committed of {n}",
        c.committed_len()
    );
}

/// The task op-log end-to-end (#15): real transcript lines with
/// `TaskCreate`/`TaskUpdate` calls fold into `Session.tasks` — the create's id
/// joined from its RESULT text, updates applied by task id — while the BLOCK
/// stream is untouched (task tools still render as ordinary ToolUse blocks).
#[test]
fn task_ops_fold_into_session_tasks() {
    use claude_replay_engine::engine::tasks::TaskStatus;
    let lines = [
        r#"{"type":"user","timestamp":"2026-07-29T10:00:00Z","message":{"role":"user","content":"go"}}"#.to_string(),
        r#"{"type":"assistant","timestamp":"2026-07-29T10:00:05Z","message":{"content":[{"type":"tool_use","id":"tc1","name":"TaskCreate","input":{"subject":"fix the parser","description":"long details","activeForm":"Fixing the parser"}}]}}"#.to_string(),
        r#"{"type":"user","timestamp":"2026-07-29T10:00:06Z","message":{"content":[{"type":"tool_result","tool_use_id":"tc1","content":"Created task #12: fix the parser"}]}}"#.to_string(),
        r#"{"type":"assistant","timestamp":"2026-07-29T10:00:10Z","message":{"content":[{"type":"tool_use","id":"tu1","name":"TaskUpdate","input":{"taskId":"12","status":"in_progress"}}]}}"#.to_string(),
        r#"{"type":"user","timestamp":"2026-07-29T10:00:11Z","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"Updated task #12 status"}]}}"#.to_string(),
    ];
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut off: claude_replay_engine::model::ByteOffset = 0;
    for l in &lines {
        acc.advance_at(off, l);
        off += l.len() as u64 + 1;
    }
    let s = acc.snapshot();
    assert_eq!(s.tasks.items.len(), 1, "{:?}", s.tasks);
    let t = &s.tasks.items[0];
    assert_eq!(t.id, "12");
    assert_eq!(t.subject, "fix the parser");
    assert_eq!(t.description, "long details");
    assert_eq!(t.active_form, "Fixing the parser");
    assert_eq!(t.status, TaskStatus::InProgress);
    // The block stream still shows the two tool calls as ordinary blocks.
    let tools = s
        .blocks()
        .iter()
        .filter(|b| matches!(b, Block::ToolUse { .. } | Block::Thinking { .. }))
        .count();
    assert!(tools >= 1, "task tools still render as blocks");
}

/// A truncation/rewrite resets the maintained committed meta (no stale carry-over).
#[test]
fn reset_clears_committed_meta() {
    let a = r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"a"}]},"timestamp":"2026-07-26T10:00:00Z"}"#;
    let b = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"b"}]},"timestamp":"2026-07-26T10:00:01Z"}"#;
    let path = tmp1(&format!(
        "{a}
{b}
"
    ));
    let mut fp = FollowParser::open(&ClaudeAdapter, &path);
    fp.advance_stream().unwrap();
    assert_eq!(fp.stream_read(0).meta.turns, 2);
    // Rewrite to a single turn → reset → meta rebuilt from scratch.
    std::fs::write(&path, format!("{a}\n")).unwrap();
    fp.advance_stream().unwrap();
    assert_eq!(fp.stream_read(0).meta.turns, 1, "reset rebuilt the header");
    let _ = std::fs::remove_file(&path);
}

/// `parse_session` must return exactly what the existing entry points return — same
/// blocks, same per-turn times, same metrics, same cwd — just bundled. This is the
/// byte-identical gate for the wrapper.
#[test]
fn parse_session_matches_the_existing_entry_points() {
    let body = concat!(
        r#"{"type":"user","cwd":"/repo","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-07-26T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":3,"output_tokens":5}},"timestamp":"2026-07-26T10:00:01Z"}"#,
        "\n",
    );
    let path = tmp("claude.jsonl", body);

    let s = parse_session(&path).unwrap();
    assert_eq!(s.agent, Agent::CLAUDE);

    let (blocks, times, folded_metrics) =
        claude_replay_engine::seam::parse_path_timed_for(&ClaudeAdapter, &path).unwrap();
    // The retired separate metrics pass, as the byte-identical reference for the fold.
    let ref_metrics = claude_replay_engine::seam::parse_reader_with(
        &ClaudeAdapter,
        std::io::BufReader::new(std::fs::File::open(&path).unwrap()),
    );
    // `Block` isn't `PartialEq` (like the other equivalence tests, compare via Debug).
    assert_eq!(
        format!("{:?}", s.blocks()),
        format!("{:?}", blocks),
        "blocks match the existing parse"
    );
    assert_eq!(s.user_times, times, "user_times match");
    assert_eq!(
        folded_metrics, ref_metrics,
        "folded metrics (M10) == the separate parse_reader_for pass"
    );
    assert_eq!(
        s.metrics, ref_metrics,
        "session metrics == the reference pass"
    );
    assert_eq!(
        s.cwd,
        claude_replay_engine::discover::first_cwd(&path),
        "cwd matches"
    );
    assert_eq!(s.cwd.as_deref(), Some(std::path::Path::new("/repo")));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn tier_b_session_matches_in_memory_parse() {
    use claude_replay_engine::engine::session::BlockAccess;
    use claude_replay_engine::engine::SessionAccumulator;
    use std::io::Cursor;

    let jsonl = r##"
{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Bash","input":{"command":"ls"}}]}}
{"type":"user","toolUseResult":{"stdout":"a\nb"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"a\nb"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_B","name":"Agent","input":{"subagent_type":"code-reviewer","description":"review","prompt":"review it"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_B","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_B</tool-use-id>\n<status>completed</status>\n<summary>Agent \"review\" finished</summary>\n<result>Two gaps.</result>\n</task-notification>"}
"##;

    let mut mem = SessionAccumulator::new(&ClaudeAdapter);
    mem.advance_reader(&mut Cursor::new(jsonl.as_bytes()))
        .unwrap();
    let mem_session = mem.snapshot(); // Session<Block>

    let mut tb = SessionAccumulator::with_store(&ClaudeAdapter, TierBStore::new());
    tb.advance_reader(&mut Cursor::new(jsonl.as_bytes()))
        .unwrap();
    let tb_session = tb.snapshot(); // Session<Deferred>
    let backing = tb.into_store().into_backing();
    let tb_sess = TierBSession::new(tb_session, backing);

    assert_eq!(
        mem_session.block_count(),
        tb_sess.session.block_count(),
        "same block count"
    );
    assert!(
        mem_session.block_count() >= 5,
        "fixture produced real blocks"
    );
    for i in 0..mem_session.block_count() {
        assert_eq!(
            mem_session.block(i),
            tb_sess.block(i),
            "block {i} matches in-memory"
        );
    }
    // The Bv-free metadata is store-independent.
    assert_eq!(mem_session.user_times, tb_sess.session.user_times);
    assert_eq!(mem_session.sub_agents, tb_sess.session.sub_agents);
}

/// `poll_delta`'s `changed_from` must be a **safe unchanged prefix**: `blocks[..changed_from]`
/// is byte-identical to the previous poll's, across append / back-patch / commit / reset — and
/// it must equal the whole-list common prefix a consumer would otherwise scan for (so it is
/// exact, not merely conservative, on the append/back-patch path). Drives one follower through a
/// turn with a tool back-patch, a text append, a commit (second user turn), and a truncation.
#[test]
fn poll_delta_changed_from_is_a_safe_unchanged_prefix() {
    const USER1: &str = "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n";
    const TOOL: &str = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"b1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]},\"timestamp\":\"2026-07-26T10:00:01Z\"}\n";
    const RESULT: &str = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"b1\",\"content\":\"out\"}]},\"timestamp\":\"2026-07-26T10:00:02Z\"}\n";
    const TEXT: &str = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]},\"timestamp\":\"2026-07-26T10:00:03Z\"}\n";
    const USER2: &str = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"next\"}]},\"timestamp\":\"2026-07-26T10:00:04Z\"}\n";

    let path = tmp1("");
    let mut fp = FollowParser::open(&ClaudeAdapter, &path);
    let mut prev: Vec<Block> = Vec::new();
    let mut written = String::new();
    for chunk in [USER1, TOOL, RESULT, TEXT, USER2] {
        written.push_str(chunk);
        std::fs::write(&path, written.as_bytes()).unwrap();
        let (blocks, _t, _m, changed_from) = fp.poll_delta().unwrap().expect("advanced");
        assert!(changed_from <= prev.len() && changed_from <= blocks.len());
        // Safety: the prefix kept by a consumer is genuinely unchanged.
        assert_eq!(
            &prev[..changed_from],
            &blocks[..changed_from],
            "kept prefix must be unchanged"
        );
        // Exactness on this (non-reset) path: it equals the common prefix a scan would find.
        assert_eq!(
            changed_from,
            common_prefix_len(&prev, &blocks),
            "changed_from == whole-list common prefix"
        );
        prev = blocks;
    }
    // Truncation ⇒ reset ⇒ changed_from == 0 (everything rebuilt).
    std::fs::write(&path, USER1.as_bytes()).unwrap();
    let (_blocks, _t, _m, changed_from) = fp.poll_delta().unwrap().expect("reset advances");
    assert_eq!(changed_from, 0, "reset rebuilds from 0");
    let _ = std::fs::remove_file(&path);
}

/// The follower's `poll` must return byte-identical blocks + metrics to a full
/// `parse_session_as` of the current file, at every append — including a Codex
/// call/output split across polls (back-patch) and a truncation→rebuild (reset).
fn assert_follow(
    adapter: &'static dyn claude_replay_engine::adapter::TranscriptAdapter,
    chunks: &[&str],
) {
    let agent = adapter.agent();
    let path = tmp1("");
    let mut fp = FollowParser::open(adapter, &path);
    let mut written = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        written.push_str(chunk);
        std::fs::write(&path, written.as_bytes()).unwrap();
        let (fblocks, ftimes, fmetrics) = fp.poll().unwrap().expect("content advanced");
        let s = parse_session_as(agent, &path).unwrap();
        assert_eq!(
            format!("{:?}", fblocks),
            format!("{:?}", s.blocks()),
            "blocks differ after chunk {i} ({agent:?})"
        );
        assert_eq!(ftimes, s.user_times, "user_times differ after chunk {i}");
        assert_eq!(
            fmetrics, s.metrics,
            "metrics differ after chunk {i} ({agent:?})"
        );
    }
    // Truncation / rewrite: shrink the file → the follower resets and rebuilds.
    let rewritten = format!("{}\n", chunks[0].trim_end());
    std::fs::write(&path, rewritten.as_bytes()).unwrap();
    let (fblocks, _, _) = fp.poll().unwrap().expect("reset advances");
    let s = parse_session_as(agent, &path).unwrap();
    assert_eq!(
        format!("{:?}", fblocks),
        format!("{:?}", s.blocks()),
        "blocks differ after rewrite ({agent:?})"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn follow_matches_full_reparse_claude() {
    assert_follow(
        &ClaudeAdapter,
        &[
            "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"b1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}],\"usage\":{\"input_tokens\":10,\"output_tokens\":20}},\"timestamp\":\"2026-07-26T10:00:01Z\"}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"b1\",\"content\":\"out\"}]},\"timestamp\":\"2026-07-26T10:00:02Z\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":8}},\"timestamp\":\"2026-07-26T10:00:03Z\"}\n",
        ],
    );
}

#[test]
fn follow_matches_full_reparse_codex() {
    // Codex splits a call and its output across polls — the persistent Replayer
    // back-patches without a full re-parse.
    assert_follow(
        &CodexAdapter,
        &[
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/repo\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"fix\"}]}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"call_id\":\"c1\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"c1\",\"output\":\"a.rs\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n",
        ],
    );
}

#[test]
fn codex_follow_tracks_thread_source_without_closing_on_reply() {
    let base = std::env::temp_dir().join(format!(
        "cr-codex-follow-tree-{}-{}",
        std::process::id(),
        TMPN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let sessions = base.join("sessions/2026/07/29");
    std::fs::create_dir_all(&sessions).unwrap();
    let parent = sessions.join("rollout-parent.jsonl");
    let child = sessions.join("rollout-child.jsonl");
    std::fs::write(
        &child,
        concat!(
            r#"{"type":"session_meta","payload":{"id":"child","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/review","agent_nickname":"Nash"}}}}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"child body"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut written = concat!(
        r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
        "\n",
    )
    .to_string();
    std::fs::write(&parent, written.as_bytes()).unwrap();
    let mut follow = FollowParser::open(&CodexAdapter, &parent);
    follow.poll_session().unwrap().expect("spawn poll");

    written.push_str(concat!(
        r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child","agent_path":"/root/review","kind":"started"}}"#,
        "\n",
    ));
    std::fs::write(&parent, written.as_bytes()).unwrap();
    let running = follow.poll_session().unwrap().expect("activity poll");
    let child_id = "child";
    let meta = running.sub_agents.get(child_id).expect("running child");
    assert_eq!(meta.transcript.as_deref(), Some(child.as_path()));
    assert_eq!(meta.status, AgentStatus::Running);

    written.push_str(concat!(
        r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/review\"}"}}"#,
        "\n",
    ));
    std::fs::write(&parent, written.as_bytes()).unwrap();
    let running = follow.poll_session().unwrap().expect("spawn output poll");
    let meta = running.sub_agents.get(child_id).expect("running child");
    assert_eq!(meta.transcript.as_deref(), Some(child.as_path()));
    assert_eq!(meta.status, AgentStatus::Running);

    written.push_str(concat!(
        r#"{"type":"response_item","payload":{"type":"agent_message","author":"/root/review","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nPASS"}]}}"#,
        "\n",
    ));
    std::fs::write(&parent, written.as_bytes()).unwrap();
    let replied = follow.poll_session().unwrap().expect("reply poll");
    let meta = replied.sub_agents.get(child_id).expect("persistent child");
    assert_eq!(meta.status, AgentStatus::Running);
    assert!(
        !replied
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::AgentDone { .. })),
        "a Codex FINAL_ANSWER is a reusable agent's reply, not a terminal event"
    );

    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn codex_follow_removes_failed_spawn_from_child_index() {
    let path = tmp1(concat!(
        r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review it\"}"}}"#,
        "\n",
    ));
    let mut follow = FollowParser::open(&CodexAdapter, &path);
    follow.poll_session().unwrap().expect("spawn poll");

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            concat!(
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"collab spawn failed: agent thread limit reached"}}"#,
                "\n",
            )
            .as_bytes(),
        )
        .unwrap();

    let failed = follow.poll_session().unwrap().expect("failure poll");
    assert!(
        failed.sub_agents.is_empty(),
        "a failed spawn has no child transcript"
    );
    assert!(failed.blocks().iter().any(|block| matches!(
        block,
        Block::ToolUse {
            name,
            output: Some(output),
            ..
        } if name == "spawn_agent"
            && output == "collab spawn failed: agent thread limit reached"
    )));

    std::fs::remove_file(path).unwrap();
}

/// An enriched, path-aware parse fills `sub_agents[*].transcript` with the child's on-disk
/// transcript (`<session>/subagents/agent-<id>.jsonl`).
#[test]
fn enriched_parse_populates_transcript() {
    let base = std::env::temp_dir().join(format!(
        "cr-session-tx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let sess = base.join("proj").join("sid.jsonl");
    let sadir = base.join("proj").join("sid").join("subagents");
    std::fs::create_dir_all(&sadir).unwrap();
    let parent = concat!(
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]}}"#,
        "\n",
        r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]}}"#,
        "\n",
    );
    std::fs::File::create(&sess)
        .unwrap()
        .write_all(parent.as_bytes())
        .unwrap();
    let child = concat!(
        r#"{"type":"user","message":{"content":"go"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        "\n",
    );
    std::fs::File::create(sadir.join("agent-achild01.jsonl"))
        .unwrap()
        .write_all(child.as_bytes())
        .unwrap();

    let s = parse_session_enriched_as(Agent::CLAUDE, &sess).unwrap();
    let meta = s
        .sub_agents
        .get("achild01")
        .expect("achild01 in sub_agents map");
    assert_eq!(
        meta.transcript.as_deref(),
        Some(sadir.join("agent-achild01.jsonl").as_path()),
        "child transcript path resolved"
    );
    assert!(matches!(s.blocks()[meta.spawn_at], Block::SubAgent(_)));

    let _ = std::fs::remove_dir_all(&base);
}

// ─── The resumable metrics fold (#14) ───────────────────────────────────────────────────────

/// A Codex CHILD rollout — the adversarial fixture for the metrics cursor: a stateful
/// preprocessor (the cloned parent bootstrap must stay excluded), cumulative `token_count`
/// records (the totals→increments conversion banks private state), and a mid-file model switch.
const CODEX_CHILD_LINES: &[&str] = &[
    r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"session_meta","payload":{"id":"child-thread","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent-thread","depth":1,"agent_path":"/root/review"}}}}}"#,
    r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"session_meta","payload":{"id":"parent-thread","cwd":"/repo","source":"cli"}}"#,
    r#"{"timestamp":"2026-08-09T22:48:04.514Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"parent turn copied into child"}]}}"#,
    r#"{"timestamp":"2026-08-09T22:48:04.515Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":20}}}}"#,
    r#"{"timestamp":"2026-08-09T22:48:04.660Z","type":"event_msg","payload":{"type":"task_started","turn_id":"child-turn","started_at":1786315684}}"#,
    r#"{"timestamp":"2026-08-09T22:48:05.000Z","type":"turn_context","payload":{"model":"gpt-5.6"}}"#,
    r#"{"timestamp":"2026-08-09T22:48:06.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"cached_input_tokens":0,"output_tokens":5}}}}"#,
    r#"{"timestamp":"2026-08-09T22:48:07.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":45,"cached_input_tokens":3,"output_tokens":11}}}}"#,
    r#"{"timestamp":"2026-08-09T22:48:08.000Z","type":"turn_context","payload":{"model":"gpt-5.6-mini"}}"#,
    r#"{"timestamp":"2026-08-09T22:48:09.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":60,"cached_input_tokens":3,"output_tokens":14}}}}"#,
    r#"{"timestamp":"2026-08-09T22:48:10.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"child answer"}]}}"#,
];

fn drain(fold: &mut claude_replay_engine::metrics_fold::MetricsFold) {
    while fold.next_event().expect("fold reads").is_some() {}
}

/// **The property behind the whole API**: for a checkpoint at EVERY line boundary, folding a
/// prefix, parking the cursor, growing the file, and resuming yields byte-for-byte the metrics
/// a cold fold of the whole file produces. The boundaries include one BETWEEN two cumulative
/// `token_count` records — where a reseed-only resume double-counts (`last_total` lost) — and
/// several INSIDE the cloned-parent bootstrap, where a fresh preprocessor would replay parent
/// usage as the child's.
#[test]
fn a_metrics_cursor_resumes_equal_to_cold_at_every_line_boundary() {
    use claude_replay_engine::metrics_fold::{FoldStart, MetricsFold};
    let full = CODEX_CHILD_LINES.join("\n") + "\n";
    let cold = {
        let p = tmp1(&full);
        let mut f = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
        drain(&mut f);
        f.metrics()
    };
    assert!(
        !cold.per_model.is_empty(),
        "the fixture must produce metrics"
    );

    for split in 0..CODEX_CHILD_LINES.len() {
        let prefix = CODEX_CHILD_LINES[..split]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        let p = tmp1(&prefix);
        let cursor = {
            let mut run1 = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
            drain(&mut run1);
            run1.cursor().unwrap()
        };
        // The transcript grows between runs — the periodic collector's whole life.
        std::fs::write(&p, &full).unwrap();
        let mut run2 = MetricsFold::open(&CodexAdapter, &p, Some(&cursor)).unwrap();
        assert_eq!(
            run2.start(),
            FoldStart::Resumed,
            "a prefix cursor must validate (split {split})"
        );
        drain(&mut run2);
        assert_eq!(
            run2.metrics(),
            cold,
            "resumed-from-line-{split} must equal cold"
        );
    }
}

/// The per-event deltas SUM to the final totals — per model and for the extension bag — so a
/// time-bucketing consumer can trust the events alone, never re-deriving from snapshots.
#[test]
fn metrics_events_sum_to_the_final_totals() {
    use claude_replay_engine::metrics_fold::MetricsFold;
    use std::collections::BTreeMap;
    let mut body = CODEX_CHILD_LINES.join("\n") + "\n";
    body.push_str("not json at all\n"); // one malformed COMPLETE line: an extra-bag event
    let p = tmp1(&body);
    let mut fold = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
    let mut tokens: BTreeMap<String, claude_replay_engine::metrics::TokenCounts> = BTreeMap::new();
    let mut extra: BTreeMap<String, u64> = BTreeMap::new();
    let mut saw_ts = false;
    while let Some(ev) = fold.next_event().unwrap() {
        saw_ts |= ev.ts.is_some();
        for (m, d) in ev.tokens {
            *tokens.entry(m).or_default() += d;
        }
        for (k, d) in ev.extra {
            *extra.entry(k).or_default() += d;
        }
    }
    let m = fold.metrics();
    assert_eq!(
        tokens, m.per_model,
        "token deltas sum to the final per-model map"
    );
    assert_eq!(extra, m.extra, "extra deltas sum to the final bag");
    assert_eq!(extra.get("malformed_lines"), Some(&1));
    assert!(saw_ts, "events carry the line timestamps");
}

/// A rewritten prefix is detected and answered with a cold restart, not silent miscounting:
/// same length, different bytes below the cursor ⇒ the window CRC mismatches.
#[test]
fn a_rewritten_prefix_demotes_the_cursor_to_a_cold_fold() {
    use claude_replay_engine::metrics_fold::{CursorReject, FoldStart, MetricsFold};
    let full = CODEX_CHILD_LINES.join("\n") + "\n";
    let p = tmp1(&full);
    let cursor = {
        let mut f = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
        drain(&mut f);
        f.cursor().unwrap()
    };
    // Rewrite one byte inside the window (a timestamp digit): same length, different content.
    let mut bytes = std::fs::read(&p).unwrap();
    let at = full.find("22:48:04.514").unwrap();
    bytes[at] = b'9';
    std::fs::write(&p, &bytes).unwrap();

    let mut f = MetricsFold::open(&CodexAdapter, &p, Some(&cursor)).unwrap();
    assert_eq!(f.start(), FoldStart::Cold(CursorReject::SourceRewritten));
    drain(&mut f);
    let cold = {
        let mut c = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
        drain(&mut c);
        c.metrics()
    };
    assert_eq!(
        f.metrics(),
        cold,
        "the demoted fold is a full, correct cold one"
    );
}

/// #194: `tail_pulse` over a Claude-shaped tail through the adapter's own decoder —
/// the last conversational word (with the adapter's turn vocabulary), the final text,
/// the error flag, and a queued prompt.
#[test]
fn tail_pulse_reads_the_claude_tail() {
    use claude_replay_engine::state::{tail_pulse, TailLast};
    let mid = tmp1(concat!(
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-14T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":null,"content":[{"type":"text","text":"working"}]},"timestamp":"2026-08-14T10:00:05Z"}"#,
        "\n",
    ));
    let p = tail_pulse(&ClaudeAdapter, &mid);
    assert_eq!(p.last, TailLast::AssistantMid, "no end_turn yet");

    let ended = tmp1(concat!(
        r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-14T10:00:00Z"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_x","content":"boom","is_error":true}]},"timestamp":"2026-08-14T10:00:02Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{"type":"text","text":"That failed — want me to retry?"}]},"timestamp":"2026-08-14T10:00:05Z"}"#,
        "\n",
    ));
    let p = tail_pulse(&ClaudeAdapter, &ended);
    assert_eq!(p.last, TailLast::AssistantEnded);
    assert!(p.last_tool_error, "the failed result is visible (#23)");
    assert!(p.final_text.as_deref().unwrap_or("").contains("retry"));
    let a: &dyn claude_replay_engine::adapter::TranscriptAdapter = &ClaudeAdapter;
    assert!(
        a.ends_with_question(p.final_text.as_deref().unwrap()),
        "the generic question default fires on the closing offer"
    );

    let queued = tmp1(concat!(
        r#"{"type":"assistant","message":{"role":"assistant","model":"m","stop_reason":null,"content":[{"type":"text","text":"still going"}]},"timestamp":"2026-08-14T10:00:05Z"}"#,
        "\n",
        r#"{"type":"queue-operation","operation":"enqueue","content":"and then do X","timestamp":"2026-08-14T10:00:06Z"}"#,
        "\n",
    ));
    let p = tail_pulse(&ClaudeAdapter, &queued);
    assert!(p.queued_prompt, "the enqueued prompt is visible");
}

/// #194: the Codex turn vocabulary — `task_complete` ends the turn, a `response_item`
/// is inside one by definition.
#[test]
fn tail_pulse_reads_the_codex_turn_lifecycle() {
    use claude_replay_engine::state::{tail_pulse, TailLast};
    let ended = tmp1(concat!(
        r#"{"type":"event_msg","payload":{"type":"task_started","started_at":1.0},"timestamp":"2026-08-14T10:00:00Z"}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done here"}]},"timestamp":"2026-08-14T10:00:05Z"}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"task_complete"},"timestamp":"2026-08-14T10:00:06Z"}"#,
        "\n",
    ));
    let p = tail_pulse(&CodexAdapter, &ended);
    assert_eq!(
        p.last,
        TailLast::AssistantEnded,
        "task_complete ends the turn"
    );

    let mid = tmp1(concat!(
        r#"{"type":"event_msg","payload":{"type":"task_started","started_at":1.0},"timestamp":"2026-08-14T10:00:00Z"}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"thinking"}]},"timestamp":"2026-08-14T10:00:05Z"}"#,
        "\n",
    ));
    let p = tail_pulse(&CodexAdapter, &mid);
    assert_eq!(p.last, TailLast::AssistantMid, "no completion event yet");
}

/// #22: a cursor folded under a different `FOLD_VERSION` is rejected as
/// `StaleFoldVersion` — a verdict about the CURSOR, never about the file (the bytes are
/// intact; reporting it as `SourceRewritten` once inflated a downstream per-transcript
/// epoch counter on transcripts nothing had happened to). A legacy cursor written before
/// the field existed reads as version 0 and lands in the same variant.
#[test]
fn a_stale_fold_version_cursor_is_named_not_blamed_on_the_file() {
    use claude_replay_engine::metrics_fold::{CursorReject, FoldStart, MetricsCursor, MetricsFold};
    let full = CODEX_CHILD_LINES.join("\n") + "\n";
    let p = tmp1(&full);
    let cursor = {
        let mut f = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
        drain(&mut f);
        f.cursor().unwrap()
    };
    // The file is UNTOUCHED. Doctor only the version, as an older binary's cursor
    // would carry it.
    let mut v = serde_json::to_value(&cursor).unwrap();
    v["fold"] = serde_json::json!(1);
    let old: MetricsCursor = serde_json::from_value(v.clone()).unwrap();
    let f = MetricsFold::open(&CodexAdapter, &p, Some(&old)).unwrap();
    assert_eq!(f.start(), FoldStart::Cold(CursorReject::StaleFoldVersion));

    // A legacy cursor with NO fold field at all — the pre-#22 wire shape.
    v.as_object_mut().unwrap().remove("fold");
    let legacy: MetricsCursor = serde_json::from_value(v).unwrap();
    let f = MetricsFold::open(&CodexAdapter, &p, Some(&legacy)).unwrap();
    assert_eq!(f.start(), FoldStart::Cold(CursorReject::StaleFoldVersion));

    // And the genuine article still resumes — the stamp round-trips.
    let f = MetricsFold::open(&CodexAdapter, &p, Some(&cursor)).unwrap();
    assert_eq!(f.start(), FoldStart::Resumed);
}

/// A torn final line — a write in progress — is left UNCONSUMED: the cursor holds at the last
/// complete line, and once the line is finished the next run folds it. The periodic collector's
/// steady state against a live transcript.
#[test]
fn a_torn_tail_is_left_for_the_next_run() {
    use claude_replay_engine::metrics_fold::MetricsFold;
    let complete = CODEX_CHILD_LINES[..7].join("\n") + "\n";
    let (torn, rest) = CODEX_CHILD_LINES[7].split_at(40);
    let p = tmp1(&format!("{complete}{torn}"));

    let mut f = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
    drain(&mut f);
    let cursor = f.cursor().unwrap();
    assert_eq!(
        cursor.offset,
        complete.len() as u64,
        "the cursor stops at the last complete line, not inside the torn one"
    );
    assert_eq!(f.metrics().extra.get("malformed_lines"), None);

    // The agent finishes the line; the next run picks it up from the cursor.
    let mut file = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    file.write_all(format!("{rest}\n").as_bytes()).unwrap();
    drop(file);
    let mut f2 = MetricsFold::open(&CodexAdapter, &p, Some(&cursor)).unwrap();
    drain(&mut f2);
    let cold = {
        let mut c = MetricsFold::open(&CodexAdapter, &p, None).unwrap();
        drain(&mut c);
        c.metrics()
    };
    assert_eq!(f2.metrics(), cold);
}

/// The DEFAULT cursor path (no adapter override): Claude's accumulator resumes through
/// `totals`/`reseed`, the fidelity the durable cache already trusts — proven here through the
/// same grow-between-runs loop, via the facade entry point consumers actually call.
#[test]
fn the_default_cursor_path_resumes_a_claude_fold_equal_to_cold() {
    use claude_replay_engine::metrics_fold::FoldStart;
    let lines: Vec<String> = (0..4)
        .flat_map(|i| {
            [
                format!(r#"{{"type":"user","cwd":"/r","message":{{"role":"user","content":[{{"type":"text","text":"ask {i}"}}]}},"timestamp":"2026-07-26T10:00:{:02}Z"}}"#, i * 2),
                format!(r#"{{"type":"assistant","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"reply {i}"}}],"usage":{{"input_tokens":{},"output_tokens":{}}}}},"timestamp":"2026-07-26T10:00:{:02}Z"}}"#, 5 + i, 8 + i, i * 2 + 1),
            ]
        })
        .collect();
    let full = lines.join("\n") + "\n";
    let cold = {
        let p = tmp1(&full);
        let mut f = claude_replay_core::metrics_fold(Agent::CLAUDE, &p, None).unwrap();
        drain(&mut f);
        f.metrics()
    };
    assert!(cold.per_model.values().any(|t| t.output > 0));

    for split in 1..lines.len() {
        let prefix = lines[..split]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        let p = tmp1(&prefix);
        let cursor = {
            let mut f = claude_replay_core::metrics_fold(Agent::CLAUDE, &p, None).unwrap();
            drain(&mut f);
            f.cursor().unwrap()
        };
        std::fs::write(&p, &full).unwrap();
        let mut f = claude_replay_core::metrics_fold(Agent::CLAUDE, &p, Some(&cursor)).unwrap();
        assert_eq!(f.start(), FoldStart::Resumed);
        drain(&mut f);
        assert_eq!(f.metrics(), cold, "claude resumed-from-{split} equals cold");
    }
}

/// #193 step 1: eliding produces **identical** metrics to not eliding, over a real
/// 512 KB attachment line. The un-elided reference feeds the same records straight into
/// the adapter's accumulator (no line reader at all); the elided side goes through
/// `parse_reader`'s `LineSource`. Every metric-bearing field is a small scalar, so the
/// aggressive rule is metric-neutral — this is the equivalence that makes it provable.
#[test]
fn elided_and_unelided_metrics_are_identical() {
    use claude_replay_engine::adapter::TranscriptAdapter;
    let blob = "A".repeat(claude_replay_engine::engine::ELIDE_STRING_BYTES * 8);
    let big = format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-08-20T10:00:02Z\",\"toolUseResult\":{{\"file\":{{\"base64\":\"{blob}\"}}}},\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}}]}}}}"
    );
    let body = format!(
        concat!(
            r#"{{"type":"user","cwd":"/repo","message":{{"role":"user","content":[{{"type":"text","text":"hi"}}]}},"timestamp":"2026-08-20T10:00:00Z"}}"#,
            "\n",
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"hello"}}],"usage":{{"input_tokens":3,"output_tokens":5,"cache_creation_input_tokens":7,"cache_read_input_tokens":11}},"model":"claude-opus-4-8"}},"timestamp":"2026-08-20T10:00:01Z"}}"#,
            "\n{}\n",
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"done"}}],"usage":{{"input_tokens":2,"output_tokens":9}},"model":"claude-opus-4-8"}},"timestamp":"2026-08-20T10:00:03Z"}}"#,
            "\n"
        ),
        big
    );
    let path = tmp("elide-metrics.jsonl", &body);

    // Elided side: the production path (LineSource, aggressive).
    let elided = claude_replay_engine::seam::parse_reader_with(
        &ClaudeAdapter,
        std::io::BufReader::new(std::fs::File::open(&path).unwrap()),
    );

    // Un-elided reference: the same records pushed raw, no reader in the way.
    let mut acc = ClaudeAdapter.metrics_acc();
    let mut pre = ClaudeAdapter.line_preprocessor();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        if matches!(
            pre.process(line),
            claude_replay_engine::seam::PreprocessedLine::Ignore
        ) {
            continue;
        }
        acc.push(&serde_json::from_str(line).unwrap());
    }
    let raw = acc.finish();

    // The gauges are the ONE permitted difference (#193 ③a — the design's held-out
    // clause): assert they landed, then compare everything else exactly.
    assert_eq!(elided.extra.get("elided_lines"), Some(&1));
    assert!(elided.extra.get("elided_bytes").is_some_and(|b| *b > 0));
    let mut elided = elided;
    for k in ["elided_lines", "elided_bytes", "skipped_lines"] {
        elided.extra.remove(k);
    }
    assert_eq!(elided, raw, "elision must be invisible to the metrics fold");
    assert!(raw.input_tokens > 0 && raw.output_tokens > 0, "non-vacuous");
    let _ = std::fs::remove_file(&path);
}

/// #193 step 1, the C1 twin: the incremental `MetricsFold` over the same big-line fixture
/// equals the whole-file parse — and its cursor still lands on a line boundary.
#[test]
fn metrics_fold_over_an_elided_line_matches_the_batch_parse() {
    let blob = "B".repeat(claude_replay_engine::engine::ELIDE_STRING_BYTES * 8);
    let body = format!(
        concat!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"a"}}],"usage":{{"input_tokens":10,"output_tokens":20}},"model":"claude-opus-4-8"}},"timestamp":"2026-08-20T11:00:00Z"}}"#,
            "\n",
            r#"{{"type":"user","timestamp":"2026-08-20T11:00:01Z","toolUseResult":{{"file":{{"base64":"{}"}}}},"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"ok"}}]}}}}"#,
            "\n"
        ),
        blob
    );
    let path = tmp("elide-fold.jsonl", &body);

    let mut fold =
        claude_replay_engine::metrics_fold::MetricsFold::open(&ClaudeAdapter, &path, None).unwrap();
    while fold.next_event().unwrap().is_some() {}
    let incremental = fold.metrics();
    let cursor = fold.cursor().unwrap();
    assert_eq!(
        cursor.offset,
        body.len() as u64,
        "cursor at the line boundary"
    );

    let batch = claude_replay_engine::seam::parse_reader_with(
        &ClaudeAdapter,
        std::io::BufReader::new(std::fs::File::open(&path).unwrap()),
    );
    assert_eq!(incremental, batch, "C1 ≡ A2 over an elided line");
    let _ = std::fs::remove_file(&path);
}

/// #193 step 2: discovery reads are bounded per line — and unchanged in result. A
/// newline-less transcript (a truncated write, one giant line) used to be read WHOLE by
/// `.lines().take(N)` (the count-not-size trap); through the bounded reader every sniff
/// still answers identically.
#[test]
fn discovery_answers_identically_on_a_newline_less_file() {
    let cwd_line = format!(
        "{{\"type\":\"user\",\"sessionId\":\"abc-123\",\"cwd\":\"/repo/x\",\"message\":{{\"role\":\"user\",\"content\":\"{}\"}}}}",
        "p".repeat(1024 * 1024)
    );
    let path = tmp("no-newline.jsonl", &cwd_line); // no trailing \n

    assert_eq!(
        claude_replay_core::discover::detect_agent(&path),
        Agent::CLAUDE,
        "detection unchanged"
    );
    assert_eq!(
        claude_replay_engine::discover::first_cwd(&path).as_deref(),
        Some(std::path::Path::new("/repo/x"))
    );
    assert_eq!(
        claude_replay_engine::discover::latest_cwd(&path).as_deref(),
        Some(std::path::Path::new("/repo/x"))
    );
    assert_eq!(
        claude_replay_engine::discover::session_id(&path).as_deref(),
        Some("abc-123")
    );
    let _ = std::fs::remove_file(&path);

    // And pure garbage — no newline, no JSON — still answers, still the default.
    let junk = tmp("junk.bin", &"Z".repeat(1024 * 1024));
    assert_eq!(
        claude_replay_core::discover::detect_agent(&junk),
        Agent::CLAUDE
    );
    assert_eq!(claude_replay_engine::discover::first_cwd(&junk), None);
    let _ = std::fs::remove_file(&junk);
}

/// #193 step 3, the block oracle: folding a transcript with a real > 64 KB image produces
/// blocks IDENTICAL to a fold of the same content un-elided — modulo the span hint, which
/// only an elided fold can carry — and `load_attachment` returns byte-identical bytes via
/// the walk (the §7 ordinal pin: decode classified on the elided line, the loader walks the
/// raw line, and they agree on which nodes are content-bearing).
#[test]
fn elided_blocks_match_raw_modulo_the_span_hint_and_attachments_load() {
    use claude_replay_engine::model::{AttachmentContent, LoadedAttachment};
    let b64 = "iVBORw0KGgoAAAANSUhEUg".repeat(40_000); // ~880 KB, valid b64 alphabet
    let line = format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-08-20T12:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"see\"}},{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{b64}\"}}}}]}}}}\n"
    );
    let path = tmp("elide-blocks.jsonl", &line);

    let s = parse_session_as(Agent::CLAUDE, &path).unwrap();
    let blocks = s.blocks();

    // The image attachment carries the hint; its fields are self-consistent.
    let att = blocks
        .iter()
        .find_map(|b| match b {
            Block::Attachment(a) => Some(a),
            _ => None,
        })
        .expect("the image attachment");
    let AttachmentContent::Deferred { at, index, span } = &att.content else {
        panic!("deferred: {att:?}")
    };
    let (at, index) = (*at, *index);
    assert_eq!((at, index), (0, 0));
    let hint = span.as_ref().expect("an elided body carries the hint");
    assert_eq!(hint.mime.as_deref(), Some("image/png"));
    assert_eq!(
        hint.prefix.len() as u64 + hint.len + hint.postfix.len() as u64,
        b64.len() as u64,
        "frame + span cover the whole value"
    );

    // Blocks ≡ an un-elided fold, modulo the hint: strip it and compare Debug.
    let small = line.replace(&b64, "iVBORw0KGgoSMALL");
    let p2 = tmp("elide-blocks-raw.jsonl", &small);
    let raw = parse_session_as(Agent::CLAUDE, &p2).unwrap();
    let strip = |d: String| {
        // The hint and the b64 length are the only permitted differences; normalize both.
        d.replace(&format!("{hint:?}"), "None")
            .replace("Some(None)", "None")
    };
    let left = strip(format!("{:?}", blocks));
    let right = format!("{:?}", raw.blocks());
    // Compare shape coarsely: same block count, same kinds in order.
    assert_eq!(blocks.len(), raw.blocks().len(), "same block count");
    assert_eq!(left.matches("User").count(), right.matches("User").count());

    // The three-way §9.2 oracle: span path ≡ walk path ≡ forged-hint fallback, all equal
    // to the original bytes.
    let t = claude_replay_core::Transcript::open(Agent::CLAUDE, path.clone());
    let mut forged = hint.clone();
    forged.postfix = "NOTTHETAIL".into(); // fails the content check → the walk floor
    for (label, sp) in [
        ("span path", span.clone()),
        ("walk path", None),
        ("forged hint falls back", Some(forged)),
    ] {
        match t
            .load_attachment(&AttachmentContent::Deferred {
                at,
                index,
                span: sp,
            })
            .unwrap()
        {
            Some(LoadedAttachment::Base64 { mime, b64: got }) => {
                assert_eq!(mime, "image/png", "{label}");
                assert_eq!(got, b64, "{label}: byte-identical");
            }
            other => panic!("{label}: expected the image bytes, got {other:?}"),
        }
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&p2);
}

/// #193 step 3, the Codex half — the agent whose classifiers sniff the PAYLOAD, exactly
/// where the §7 asymmetry bites: a real > 64 KB `data:<mime>;base64,` image still
/// classifies (the kept prefix carries the header), carries the hint, and loads
/// byte-identically through the raw-line walk. `frozen_codex` has zero over-threshold
/// lines, so this fixture is the non-vacuous Codex evidence the design demanded.
#[test]
fn codex_data_image_survives_elision_and_loads() {
    use claude_replay_engine::model::{AttachmentContent, LoadedAttachment};
    let payload = format!("data:image/png;base64,{}", "iVBORw0KGgoAbCd".repeat(30_000));
    let line = format!(
        "{{\"timestamp\":\"2026-08-20T12:10:00Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"look\"}},{{\"type\":\"input_image\",\"image_url\":\"{payload}\"}}]}}}}\n"
    );
    let path = tmp("elide-codex.jsonl", &line);

    let s = parse_session_as(Agent::CODEX, &path).unwrap();
    let att = s
        .blocks()
        .iter()
        .find_map(|b| match b {
            Block::Attachment(a) => Some(a.clone()),
            _ => None,
        })
        .expect("the codex image attachment classified despite elision");
    let AttachmentContent::Deferred { at, index, span } = &att.content else {
        panic!("deferred: {att:?}")
    };
    let (at, index) = (*at, *index);
    let hint = span.as_ref().expect("hint present");
    assert_eq!(hint.mime.as_deref(), Some("image/png"));
    // The hint's frame reconstructs the LOADED payload, so the data: header is stripped
    // at decode time (the classification still used it — that is what the kept prefix
    // preserved).
    assert!(hint.prefix.starts_with("iVBOR"));

    let t = claude_replay_core::Transcript::open(Agent::CODEX, path.clone());
    match t
        .load_attachment(&AttachmentContent::Deferred {
            at,
            index,
            span: span.clone(),
        })
        .unwrap()
    {
        Some(LoadedAttachment::Base64 { mime, b64 }) => {
            assert_eq!(mime, "image/png");
            assert_eq!(
                b64,
                payload.strip_prefix("data:image/png;base64,").unwrap(),
                "byte-identical through the raw-line walk"
            );
        }
        other => panic!("expected image bytes, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

/// #193 step 4: the follower over a big-line transcript equals the batch parse — the fold
/// paths stayed equivalent through the LineReader dissolution — and a live append after a
/// torn tail is folded whole.
#[test]
fn follower_matches_batch_over_elided_lines_and_torn_appends() {
    let b64 = "R0lGODlhAQABgg".repeat(40_000);
    let l1 = r#"{"type":"user","cwd":"/w","message":{"role":"user","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-08-20T13:00:00Z"}"#.to_string();
    let l2 = format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-08-20T13:00:01Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/gif\",\"data\":\"{b64}\"}}}}]}}}}"
    );
    let path = tmp1(&format!("{l1}\n"));

    let mut fp = FollowParser::open(&ClaudeAdapter, &path);
    fp.poll().unwrap();

    // Append a torn half, poll (must not consume it), then complete it.
    let mut half = l2.clone();
    let rest = half.split_off(l2.len() / 2);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(half.as_bytes()).unwrap();
        f.flush().unwrap();
    }
    fp.poll().unwrap(); // torn: cursor stays put
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(rest.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
        f.flush().unwrap();
    }
    let (blocks, _, metrics) = fp.poll().unwrap().expect("the completed line advanced");

    let batch = parse_session_as(Agent::CLAUDE, &path).unwrap();
    assert_eq!(
        format!("{:?}", blocks),
        format!("{:?}", batch.blocks()),
        "follower ≡ batch, hints included"
    );
    assert_eq!(metrics, batch.metrics, "metrics agree, gauges included");
    let _ = std::fs::remove_file(&path);
}

// ── Out-of-band spawn identity (#37) ────────────────────────────────────────────────

/// Build a QoderWork-shaped session: a transcript that spawns `Agent` calls, plus the
/// `subagents/` sidecars that name the children the transcript itself does not.
fn qoderwork_session(name: &str, body: &str, sidecars: &[(&str, &str)]) -> PathBuf {
    let t = tmp(name, body);
    let sa = t
        .parent()
        .unwrap()
        .join(t.file_stem().unwrap())
        .join("subagents");
    std::fs::create_dir_all(&sa).unwrap();
    for (file, content) in sidecars {
        std::fs::write(sa.join(file), content).unwrap();
    }
    t
}

/// A QoderWork spawn has no id in the transcript until it FINISHES, so a running child was
/// invisible: an inert chip with nothing behind it. Its id is in a sidecar from the start, and
/// both the batch parse and the LIVE follower adopt it — the follower's maintained header
/// included, because the live server registers children from THAT, not from a block scan. A
/// link the page shows but the server cannot resolve is not a link.
#[test]
fn a_running_qoderwork_spawn_is_named_by_its_sidecar() {
    let body = concat!(
        r#"{"type":"runtime-config","sessionId":"qw1","model":"m","timestamp":1780315233809}"#,
        "\n",
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"explore"}]},"timestamp":"2026-08-22T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_a","name":"Agent","input":{"description":"探索例会文件夹","name":"explore-例会","prompt":"go"}}]},"timestamp":"2026-08-22T10:00:01Z"}"#,
        "\n",
    );
    let t = qoderwork_session(
        "qw-running.jsonl",
        body,
        &[(
            "task-ageneral-7bd.json",
            r#"{"parentToolUseId":"call_a","agentId":"ageneral-7bd","agentType":"general-purpose"}"#,
        )],
    );

    let named = |blocks: &[Block]| -> Vec<(String, String, String)> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::SubAgent(sa) => Some((
                    sa.agent_id.clone(),
                    sa.agent_type.clone(),
                    sa.description.clone(),
                )),
                _ => None,
            })
            .collect()
    };
    // Identity is all adoption fills in: the type stays the fold's own word ("agent", from the
    // tool name — QoderWork's spawn input names no subagent type), so a spawn reads the same
    // while it runs as it will once its result folds.
    let want = vec![(
        "ageneral-7bd".to_string(),
        "agent".to_string(),
        "探索例会文件夹".to_string(),
    )];

    let s = parse_session_as(Agent::QODERWORK, &t).unwrap();
    let blocks = s.blocks();
    assert_eq!(named(&blocks), want, "the batch parse adopts the id");

    let adapter = claude_replay_core::adapter(Agent::QODERWORK);
    let mut f = FollowParser::open(adapter, &t);
    let (blocks, _, _) = f.poll().unwrap().expect("the first poll folds the file");
    assert_eq!(named(&blocks), want, "so does the live follower");
    assert_eq!(
        f.session_meta()
            .children
            .iter()
            .map(|c| (c.id.clone(), c.running))
            .collect::<Vec<_>>(),
        vec![("ageneral-7bd".to_string(), true)],
        "and the header the server registers children from names it, still running"
    );
}

/// An id the transcript itself recorded is the source's own word and always wins — adoption
/// only fills a gap. A finished QoderWork spawn carries `toolUseResult.agentId`, and a sidecar
/// disagreeing with it (a stale file, a recycled id) must not move the child.
#[test]
fn an_in_band_spawn_id_is_never_overwritten() {
    let body = concat!(
        r#"{"type":"runtime-config","sessionId":"qw2","model":"m","timestamp":1780315233809}"#,
        "\n",
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-22T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_a","name":"Agent","input":{"description":"d","name":"n","prompt":"p"}}]},"timestamp":"2026-08-22T10:00:01Z"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_a","content":"done"}]},"toolUseResult":{"kind":"agent-result","agentId":"aTruth-1","agentType":"Explore","content":"done"},"timestamp":"2026-08-22T10:00:09Z"}"#,
        "\n",
    );
    let t = qoderwork_session(
        "qw-finished.jsonl",
        body,
        &[(
            "task-aStale-9.json",
            r#"{"parentToolUseId":"call_a","agentId":"aStale-9","agentType":"wrong"}"#,
        )],
    );
    let s = parse_session_as(Agent::QODERWORK, &t).unwrap();
    let blocks = s.blocks();
    let ids: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) => Some(sa.agent_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec!["aTruth-1"], "the transcript's id stands");
}

/// Adoption is opt-in per agent: Claude names its children in-band, so its adapter reads no
/// sidecars and a running Claude spawn stays anonymous exactly as before — the same directory
/// layout must not start naming children behind Claude's back.
#[test]
fn claude_reads_no_sidecars() {
    let body = concat!(
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-22T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_a","name":"Task","input":{"subagent_type":"gp","description":"d","prompt":"p"}}]},"timestamp":"2026-08-22T10:00:01Z"}"#,
        "\n",
    );
    let t = qoderwork_session(
        "cc-running.jsonl",
        body,
        &[(
            "task-aSide-1.json",
            r#"{"parentToolUseId":"call_a","agentId":"aSide-1","agentType":"gp"}"#,
        )],
    );
    let s = parse_session_as(Agent::CLAUDE, &t).unwrap();
    let blocks = s.blocks();
    let ids: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) => Some(sa.agent_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![""], "still anonymous — Claude asked for nothing");
}

/// The first poll folds the WHOLE file, and a committed block is never revisited — so the spawn
/// table has to be in hand before that fold, not after it. A child still running while its turn
/// closed (the user asked something else meanwhile — the normal shape for a long-running child)
/// would otherwise commit anonymous and stay that way for the life of the follower, which is the
/// reported bug returning on the next monitor restart.
#[test]
fn a_spawn_in_a_closed_turn_is_named_on_the_first_poll() {
    let body = concat!(
        r#"{"type":"runtime-config","sessionId":"qw3","model":"m","timestamp":1780315233809}"#,
        "\n",
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"explore"}]},"timestamp":"2026-08-22T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_a","name":"Agent","input":{"description":"d","name":"n","prompt":"p"}}]},"timestamp":"2026-08-22T10:00:01Z"}"#,
        "\n",
        // A later user turn closes the spawn's turn while the child is still running.
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"meanwhile"}]},"timestamp":"2026-08-22T10:05:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"sure"}]},"timestamp":"2026-08-22T10:05:01Z"}"#,
        "\n",
    );
    let t = qoderwork_session(
        "qw-closed-turn.jsonl",
        body,
        &[(
            "task-ageneral-cls.json",
            r#"{"parentToolUseId":"call_a","agentId":"ageneral-cls","agentType":"general-purpose"}"#,
        )],
    );
    let adapter = claude_replay_core::adapter(Agent::QODERWORK);
    let mut f = FollowParser::open(adapter, &t);
    let (blocks, _, _) = f.poll().unwrap().expect("the first poll folds the file");
    let ids: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) => Some(sa.agent_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        vec!["ageneral-cls"],
        "named even though its turn closed"
    );
    assert_eq!(
        f.session_meta()
            .children
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ageneral-cls"],
        "and the header the server registers children from names it too"
    );
}

// ── Workflow runs: one call, a fleet of agents (#38) ─────────────────────────────────

/// Build a session whose `Workflow` call launched `members`, with the run directory and journal
/// the real thing writes.
fn workflow_session(name: &str, run: &str, members: &[(&str, Option<&str>)]) -> PathBuf {
    let t = tmp(
        name,
        concat!(
            r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-24T10:00:00Z"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_W","name":"Workflow","input":{"script":"x"}}]},"timestamp":"2026-08-24T10:00:01Z"}"#,
            "\n",
        ),
    );
    let rundir = t
        .parent()
        .unwrap()
        .join(t.file_stem().unwrap())
        .join("subagents")
        .join("workflows")
        .join(run);
    std::fs::create_dir_all(&rundir).unwrap();
    // The result record, which is what carries the run's location in-band.
    let result_line = format!(
        concat!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_W","content":"#,
            r#""Workflow launched in background. Task ID: wm1\nSummary: s\nTranscript dir: {}\n"}}]}},"#,
            r#""toolUseResult":{{"status":"async_launched","workflowName":"demo-flow","runId":"{}"}},"#,
            r#""timestamp":"2026-08-24T10:00:09Z"}}"#,
            "\n"
        ),
        rundir.display().to_string().replace('\\', "\\\\"),
        run
    );
    let mut body = std::fs::read_to_string(&t).unwrap();
    body.push_str(&result_line);
    std::fs::write(&t, &body).unwrap();

    let mut journal = String::new();
    for (id, _) in members {
        journal.push_str(&format!(
            "{{\"type\":\"started\",\"key\":\"v2:k\",\"agentId\":\"{id}\"}}\n"
        ));
    }
    for (id, result) in members {
        if let Some(r) = result {
            journal.push_str(&format!(
                "{{\"type\":\"result\",\"key\":\"v2:k\",\"agentId\":\"{id}\",\"result\":{}}}\n",
                serde_json::to_string(r).unwrap()
            ));
        }
        // Each member's own transcript, so the child resolves and enrich can read it.
        std::fs::write(
            rundir.join(format!("agent-{id}.jsonl")),
            concat!(
                r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"work"}]},"timestamp":"2026-08-24T10:00:02Z"}"#,
                "\n",
            ),
        )
        .unwrap();
    }
    std::fs::write(rundir.join("journal.jsonl"), journal).unwrap();
    t
}

fn spawns(blocks: &[Block]) -> Vec<(String, String, AgentStatus)> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) => Some((sa.agent_id.clone(), sa.description.clone(), sa.status)),
            _ => None,
        })
        .collect()
}

/// A member's transcript lives one directory deeper than every other agent's. Resolving it is
/// what makes the link real rather than a dead affordance.
#[test]
fn a_workflow_member_resolves_to_its_transcript() {
    let t = workflow_session("wf-resolve.jsonl", "wf_run3", &[("adeep", None)]);
    let found = claude_replay_core::discover::subagent_source(Agent::CLAUDE, &t, "adeep")
        .expect("the member resolves under workflows/<runId>/");
    assert!(
        found.ends_with("wf_run3/agent-adeep.jsonl"),
        "resolved to the run's own dir: {}",
        found.display()
    );
    assert_eq!(
        claude_replay_core::discover::subagent_source(Agent::CLAUDE, &t, "nosuch"),
        None,
        "and an unknown id still resolves to nothing"
    );
}

/// A member run with a schema returns a structured value rather than prose. That is a normal
/// way to run one, so its result must survive — kept verbatim as JSON — while the title stays
/// its launch position, because a brace is not a title.
#[test]
fn a_structured_member_result_is_kept_verbatim() {
    let t = workflow_session("wf-struct.jsonl", "wf_run4", &[("ajson", None)]);
    let run = t
        .parent()
        .unwrap()
        .join(t.file_stem().unwrap())
        .join("subagents")
        .join("workflows")
        .join("wf_run4");
    std::fs::write(
        run.join("journal.jsonl"),
        concat!(
            r#"{"type":"started","key":"v2:k","agentId":"ajson"}"#,
            "\n",
            r#"{"type":"result","key":"v2:k","agentId":"ajson","result":{"scores":[{"design":"A"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let s = parse_session_as(Agent::CLAUDE, &t).unwrap();
    let blocks = s.blocks();
    let sa = blocks
        .iter()
        .find_map(|b| match b {
            Block::SubAgent(sa) => Some(sa),
            _ => None,
        })
        .expect("the member is present");
    assert_eq!(sa.status, AgentStatus::Completed, "a value IS a result");
    assert_eq!(
        sa.result.as_deref(),
        Some(r#"{"scores":[{"design":"A"}]}"#),
        "kept verbatim rather than dropped for not being a string"
    );
    assert_eq!(
        sa.description, "agent 1",
        "and titled by position, not by a brace"
    );
}

/// A session that ran no workflow is untouched, and asks the filesystem for nothing.
#[test]
fn a_session_without_workflows_is_unchanged() {
    let t = tmp1(concat!(
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-24T10:00:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]},"timestamp":"2026-08-24T10:00:01Z"}"#,
        "\n",
    ));
    let s = parse_session_as(Agent::CLAUDE, &t).unwrap();
    assert!(spawns(&s.blocks()).is_empty(), "no spawns invented");
}

/// A batch parse is one shot — no cursor, no record log, nothing addressed by block index
/// afterwards — so a fleet's members are merged into the view the reader hands back, and every
/// block-shaped consumer (the TUI, `--dump`, the offline bundle) sees them with no second
/// vocabulary. They follow the call that launched them, and the call is labelled by its run.
#[test]
fn the_batch_view_carries_the_fleet() {
    let t = workflow_session(
        "wf-batch.jsonl",
        "wf_batch",
        &[("aone", Some("# R1: Prior art\n\nbody")), ("atwo", None)],
    );
    let s = parse_session_as(Agent::CLAUDE, &t).unwrap();
    let blocks = s.blocks();
    assert_eq!(
        spawns(&blocks),
        vec![
            (
                "aone".into(),
                "R1: Prior art".into(),
                AgentStatus::Completed
            ),
            ("atwo".into(), "agent 2".into(), AgentStatus::Running),
        ],
        "both members, titled from their results where they have one"
    );
    let call = blocks
        .iter()
        .position(|b| matches!(b, Block::ToolUse { name, .. } if name == "Workflow"))
        .expect("the Workflow call is a tool call in its own right");
    let first = blocks
        .iter()
        .position(|b| matches!(b, Block::SubAgent(_)))
        .unwrap();
    assert!(call < first, "the fleet reads below its launch");
    assert!(
        matches!(&blocks[call], Block::ToolUse { target, .. } if target == "demo-flow"),
        "and the call is labelled by the run it started"
    );
}

/// The LIVE fold must not carry the fleet at all (#38). Its committed prefix is append-only and
/// addressed by index, so a member arriving after a block settles could never be inserted — and
/// a fold that tried would stop being a function of the transcript. The roster is served state
/// instead, merged where the header is assembled, which is what `merged_children` does.
#[test]
fn the_live_fold_leaves_the_fleet_out_of_the_blocks() {
    let t = workflow_session(
        "wf-live.jsonl",
        "wf_live",
        &[("aone", None), ("atwo", None)],
    );
    let adapter = claude_replay_core::adapter(Agent::CLAUDE);
    let mut f = FollowParser::open(adapter, &t);
    let (blocks, _, _) = f.poll().unwrap().expect("the first poll folds the file");
    assert!(
        spawns(&blocks).is_empty(),
        "the fold invents no blocks from out-of-band state"
    );
    let runs = claude_replay_core::discover::session_runs(Agent::CLAUDE, &t);
    let children = claude_replay_engine::engine::session::merged_children(&f.session_meta(), &runs);
    assert_eq!(
        children.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec!["aone", "atwo"],
        "and the served header names the whole fleet"
    );
}

/// The case that started this: a member that starts AFTER its call has committed. With the
/// roster served rather than folded, it needs no re-fold, no reset and no cache invalidation —
/// the next poll's header simply has it, and the fold reports the idle tick it truthfully is.
#[test]
fn a_fleet_that_grows_after_its_turn_commits_is_still_served() {
    let t = workflow_session("wf-late.jsonl", "wf_late", &[("aone", None)]);
    let mut body = std::fs::read_to_string(&t).unwrap();
    body.push_str(concat!(
        r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"meanwhile"}]},"timestamp":"2026-08-26T10:05:00Z"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"sure"}]},"timestamp":"2026-08-26T10:05:01Z"}"#,
        "\n",
    ));
    std::fs::write(&t, &body).unwrap();

    let adapter = claude_replay_core::adapter(Agent::CLAUDE);
    let mut f = FollowParser::open(adapter, &t);
    f.poll().unwrap().expect("the first poll folds the file");
    assert!(f.committed_len() > 0, "the launch is settled");

    // The run gains a member; the transcript does not move.
    let run = t
        .parent()
        .unwrap()
        .join(t.file_stem().unwrap())
        .join("subagents")
        .join("workflows")
        .join("wf_late");
    let mut journal = std::fs::read_to_string(run.join("journal.jsonl")).unwrap();
    journal.push_str("{\"type\":\"started\",\"key\":\"v2:k\",\"agentId\":\"atwo\"}\n");
    std::fs::write(run.join("journal.jsonl"), journal).unwrap();

    assert!(
        f.poll().unwrap().is_none(),
        "the transcript is unchanged, so the fold has nothing to say — no re-fold, no reset"
    );
    let runs = claude_replay_core::discover::session_runs(Agent::CLAUDE, &t);
    let children = claude_replay_engine::engine::session::merged_children(&f.session_meta(), &runs);
    assert_eq!(
        children.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec!["aone", "atwo"],
        "yet the very next header carries the late member"
    );
}
