//! Machinery-with-real-adapters integration tests (#87 step 3): these exercised the
//! engine's builder/follower/stores against Claude/Codex fixture transcripts while they
//! lived inside the engine crate — a dev-dependency cycle would compile TWO copies of
//! the engine there, so they live here, where ONE coherent engine + the built-in
//! adapters + the facade's dispatchers meet.

use claude_replay_agents::{ClaudeAdapter, CodexAdapter};
use claude_replay_core::{parse_session, parse_session_as, parse_session_enriched_as, Agent};
use claude_replay_engine::engine::tier_b::{TierBSession, TierBStore};
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
        claude_replay_engine::discover::session_cwd(&path),
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

// Persist a tier-b session to disk and reload it: every block, the rebuilt index, metrics,
// user_times, and sub_agents must match the pre-persist session exactly — a restart survives
// without re-folding the transcript.

#[test]
fn persist_then_load_reconstructs_the_session() {
    use claude_replay_engine::engine::session::BlockAccess;
    use claude_replay_engine::engine::SessionAccumulator;
    use std::io::Cursor;

    let jsonl = r##"
{"type":"user","message":{"content":[{"type":"text","text":"start"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"code-reviewer","description":"review","prompt":"review it"}}]}}
{"type":"user","toolUseResult":{"agentId":"aXYZ1234","status":"async_launched","outputFile":"/t/aXYZ1234.output"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>aXYZ1234</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"review\" finished</summary>\n<result>Two gaps.</result>\n</task-notification>"}
{"type":"user","message":{"content":[{"type":"text","text":"thanks"}]}}
"##;

    let mut acc = SessionAccumulator::with_store(&ClaudeAdapter, TierBStore::new());
    acc.advance_reader(&mut Cursor::new(jsonl.as_bytes()))
        .unwrap();
    let session = acc.snapshot();
    let backing = acc.into_store().into_backing();
    let before = TierBSession::new(session, backing);

    let dir = std::env::temp_dir().join(format!("tierb-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    before.persist(&dir).unwrap();
    let after = TierBSession::load(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(before.session.block_count(), after.session.block_count());
    assert!(before.session.block_count() >= 5);
    for i in 0..before.session.block_count() {
        assert_eq!(
            before.block(i),
            after.block(i),
            "block {i} survives persist→load"
        );
    }
    assert_eq!(before.session.agent, after.session.agent);
    assert_eq!(before.session.cwd, after.session.cwd);
    assert_eq!(before.session.user_times, after.session.user_times);
    assert_eq!(before.session.metrics, after.session.metrics);
    assert_eq!(before.session.sub_agents, after.session.sub_agents);
    // The index is rebuilt on load — must equal the original (compared via Debug; no PartialEq).
    assert_eq!(
        format!("{:?}", before.session.index),
        format!("{:?}", after.session.index),
        "rebuilt index equals the persisted session's"
    );
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
