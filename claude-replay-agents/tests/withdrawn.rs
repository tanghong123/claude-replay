//! #s500: a reply Claude Code withdrew for a broken tool call folds under one mark where the
//! client's retry note was, and that has to hold across a durable resume as well as a cold fold.
//! The fold remembers where each record's blocks begin for the note without the meta stream: a
//! resume begins at a line that opens a turn, and the rule takes only a reply whose blocks run
//! straight up to the note, so a turn opener between them (a queued prompt picked up, where a
//! resume can begin inside a turn) leaves the note as it is on every path. This pins that every
//! resume point folds what a cold fold does, a pickup between a reply and its note included.
use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, SessionAccumulator};
use serde_json::json;

const NOTE: &str =
    "The previous response failed to produce a valid tool call. Please retry the tool call now.";

fn prompt(text: &str, at: u32) -> String {
    json!({
        "type": "user", "version": "2.1.325", "uuid": format!("u-{at}"),
        "timestamp": format!("2026-11-14T17:00:{at:02}.000Z"),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

/// One record of a reply the client will withdraw: its thinking, or its text.
fn withdrawn(uuid: &str, at: u32, content: serde_json::Value) -> String {
    json!({
        "type": "assistant", "version": "2.1.325", "uuid": uuid,
        "timestamp": format!("2026-11-14T17:00:{at:02}.100Z"),
        "message": {"id": format!("msg-{uuid}"), "role": "assistant", "model": "claude-opus-5-5",
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 3, "output_tokens": 410},
                    "content": [content]},
    })
    .to_string()
}

fn thinking(text: &str) -> serde_json::Value {
    json!({"type": "thinking", "thinking": text, "signature": "x"})
}

fn note(ids: &[&str], at: u32) -> String {
    json!({
        "type": "user", "version": "2.1.325", "uuid": format!("n-{at}"),
        "timestamp": format!("2026-11-14T17:00:{at:02}.200Z"),
        "isMeta": true, "turnCompanion": true, "retractedMessageUuids": ids,
        "message": {"role": "user", "content": NOTE},
    })
    .to_string()
}

/// The retry: a call, its result, then the answer.
fn retry(n: u32, at: u32) -> Vec<String> {
    let call = format!("toolu_s500_{n}");
    vec![
        json!({
            "type": "assistant", "version": "2.1.325", "uuid": format!("r-{n}"),
            "timestamp": format!("2026-11-14T17:00:{at:02}.300Z"),
            "message": {"id": format!("msg-r{n}"), "role": "assistant", "model": "claude-opus-5-5",
                        "stop_reason": "tool_use",
                        "content": [{"type": "tool_use", "id": call, "name": "Bash", "input": {"command": "wc -l nests.md"}}]},
        })
        .to_string(),
        json!({
            "type": "user", "version": "2.1.325", "uuid": format!("t-{n}"),
            "timestamp": format!("2026-11-14T17:00:{at:02}.400Z"),
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": call, "content": "40 nests.md"}]},
        })
        .to_string(),
        json!({
            "type": "assistant", "version": "2.1.325", "uuid": format!("e-{n}"),
            "timestamp": format!("2026-11-14T17:00:{at:02}.500Z"),
            "message": {"id": format!("msg-e{n}"), "role": "assistant", "model": "claude-opus-5-5",
                        "stop_reason": "end_turn", "content": [{"type": "text", "text": format!("Forty nests ({n}).")}]},
        })
        .to_string(),
    ]
}

fn marks(blocks: &[Block]) -> Vec<usize> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Withdrawn { note, reply } if note == NOTE => Some(reply.len()),
            _ => None,
        })
        .collect()
}

/// Folds `lines` resumed at every point a resume can begin, through the bytes a cache holds,
/// checks each against the cold fold `expected`, and returns each resume's line and committed
/// block count.
fn resume_everywhere(lines: &[String], expected: &[Block]) -> Vec<(usize, usize)> {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let mut resumed = Vec::new();
    for split in 1..=lines.len() {
        let mut acc = SessionAccumulator::new(adapter);
        let mut records = Vec::new();
        let mut offset = 0;
        for line in &lines[..split] {
            acc.advance_at(offset, line);
            offset += line.len() as u64 + 1;
            records.extend(acc.drain_meta());
        }
        let committed = acc.committed_tail(0);
        let Some(aligned) = align(&records, committed.len()).unwrap() else {
            continue;
        };
        // Through the bytes a cache holds, as a restart reads them.
        let saved = serde_json::to_vec(&committed).unwrap();
        let loaded: Vec<Block> = serde_json::from_slice(&saved).unwrap();
        let records: Vec<_> = records
            .iter()
            .map(|r| serde_json::from_slice(&serde_json::to_vec(r).unwrap()).unwrap())
            .collect();
        let aligned = align(&records, committed.len()).unwrap().unwrap_or(aligned);
        let mut run = SessionAccumulator::restore(
            adapter,
            InMemoryStore,
            loaded[..aligned.committed].to_vec(),
            aligned.meta,
            &aligned.resume,
        );
        let mut offset = 0;
        for line in lines {
            if offset >= aligned.resume.replay_from {
                run.advance_at(offset, line);
            }
            offset += line.len() as u64 + 1;
        }
        assert_eq!(run.fold().0, expected, "resumed at line {split}");
        resumed.push((split, aligned.committed));
    }
    resumed
}

#[test]
fn a_withdrawn_reply_is_marked_across_every_resume_point() {
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    // The measured shape: a reply of one record, thinking alone, then the note.
    let mut lines = vec![prompt("count the tern nests", 1)];
    lines.push(withdrawn(
        "w1",
        2,
        thinking("The nest log sits in nests.md."),
    ));
    lines.push(note(&["w1"], 2));
    lines.extend(retry(1, 3));
    // A turn with no withdrawal between the two.
    lines.push(prompt("and the skua burrows", 4));
    lines.push(
        json!({"type": "assistant", "version": "2.1.325", "uuid": "plain",
               "timestamp": "2026-11-14T17:00:05.000Z",
               "message": {"id": "msg-plain", "role": "assistant", "model": "claude-opus-5-5",
                           "stop_reason": "end_turn", "content": [{"type": "text", "text": "Twelve burrows."}]}})
        .to_string(),
    );
    // A reply of two records, its second holding text (the leaked-invocation case).
    lines.push(prompt("count them again", 6));
    lines.push(withdrawn("w2", 7, thinking("Again, from the log.")));
    lines.push(withdrawn(
        "w2t",
        7,
        json!({"type": "text", "text": "Bash(wc -l nests.md)"}),
    ));
    lines.push(note(&["w2", "w2t"], 7));
    lines.extend(retry(2, 8));
    // A queued prompt picked up between a reply and its note, a line a resume can begin at: the
    // prompt sits between them, so the note stays the note it is on every path.
    lines.push(prompt("and the petrels", 9));
    lines.push(withdrawn("w3", 10, thinking("Petrels next.")));
    lines.push(
        json!({"type": "queue-operation", "operation": "enqueue", "timestamp": "2026-11-14T17:00:10.110Z",
               "content": "and the gulls too"})
        .to_string(),
    );
    lines.push(
        json!({"type": "attachment", "timestamp": "2026-11-14T17:00:10.120Z",
               "attachment": {"type": "queued_command", "commandMode": "prompt", "prompt": "and the gulls too"}})
        .to_string(),
    );
    lines.push(note(&["w3"], 10));
    lines.extend(retry(3, 11));

    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(
        marks(&expected),
        [1, 2],
        "the two withdrawals, one record and two: {expected:#?}"
    );
    assert_eq!(
        expected
            .iter()
            .filter(|b| matches!(b, Block::ToolResult(t) if t == NOTE))
            .count(),
        1,
        "the note behind a pickup stays the note it is: {expected:#?}"
    );
    assert!(
        expected
            .iter()
            .any(|b| matches!(b, Block::Thinking { text, .. } if text.contains("Petrels next."))),
        "and its reply stays where it was: {expected:#?}"
    );

    let resumed = resume_everywhere(&lines, &expected);
    // A resume with the first mark already committed and the second reply still to come shows the
    // fold's memory of a reply is not needed across a resume.
    assert!(
        resumed
            .iter()
            .any(|&(_, committed)| marks(&expected[..committed.min(expected.len())]) == [1]),
        "the test must resume between the two withdrawals: {resumed:?}"
    );
}
