//! #s378: a reply the safety classifier stopped is marked where the client's note to the model
//! was, and that has to hold across a durable resume as well as a cold fold. The fold remembers
//! the stopped reply for its note without the meta stream, because a resume begins at a line that
//! opens a turn and the note follows its reply inside the turn; this pins that every resume point
//! folds what a cold fold does.
use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, SessionAccumulator};
use serde_json::json;

const NOTE: &str = "Your response above was stopped by a safety classifier — this is not a tool or API error. The rest of it was withheld, and tool calls in it that had not finished did not run. Do not produce that content again, even reworded.";

fn prompt(text: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-11-06T01:00:{at:02}.000Z"),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

/// A reply stopped by the classifier: its thinking alone, as every one measured was.
fn stopped(id: &str, at: u32) -> String {
    json!({
        "type": "assistant",
        "version": "2.1.318",
        "timestamp": format!("2026-11-06T01:00:{at:02}.100Z"),
        "message": {"id": id, "role": "assistant", "model": "claude-opus-5-5", "stop_reason": "refusal",
                    "stop_details": {"type": "refusal", "category": "cyber", "explanation": "This request was flagged."},
                    "content": [{"type": "thinking", "thinking": "The burrows sit above the tern colony.", "signature": "x"}]},
    })
    .to_string()
}

/// The notice Claude Code 2.1.318 writes for the person between the reply and the note.
fn notice(at: u32) -> String {
    json!({
        "type": "system",
        "subtype": "informational",
        "level": "notice",
        "isMeta": false,
        "timestamp": format!("2026-11-06T01:00:{at:02}.200Z"),
        "content": "The safeguards stopped the response above · continuing",
    })
    .to_string()
}

fn note(id: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-11-06T01:00:{at:02}.300Z"),
        "isMeta": true,
        "turnCompanion": true,
        "refusedMessageId": id,
        "message": {"role": "user", "content": NOTE},
    })
    .to_string()
}

fn reply(at: u32) -> String {
    json!({
        "type": "assistant",
        "timestamp": format!("2026-11-06T01:00:{at:02}.500Z"),
        "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "noted"}]},
    })
    .to_string()
}

fn marks(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter(|b| {
            matches!(b, Block::SafetyStop { category: Some(c), note } if c == "cyber" && note == NOTE)
        })
        .count()
}

#[test]
fn a_stopped_reply_is_marked_across_every_resume_point() {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let lines = vec![
        prompt("chart the skua burrows", 1),
        stopped("msg_a", 1),
        notice(1),
        note("msg_a", 1),
        reply(1),
        prompt("and the petrel nests", 2),
        reply(2),
        prompt("count them again", 3),
        stopped("msg_b", 3),
        note("msg_b", 3),
        reply(3),
    ];
    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(marks(&expected), 2, "{expected:#?}");
    assert!(
        !expected
            .iter()
            .any(|b| matches!(b, Block::ToolResult(t) if t == NOTE)),
        "{expected:#?}"
    );

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
        for line in &lines {
            if offset >= aligned.resume.replay_from {
                run.advance_at(offset, line);
            }
            offset += line.len() as u64 + 1;
        }
        assert_eq!(run.fold().0, expected, "resumed at line {split}");
        resumed.push((split, aligned.committed));
    }
    // A resume with the first mark already committed and the second reply still to come is the
    // one that shows the fold's memory of a stopped reply is not needed across a resume.
    assert!(
        resumed.iter().any(|&(split, committed)| split <= 8
            && marks(&expected[..committed.min(expected.len())]) == 1),
        "the test must resume between the two stopped replies: {resumed:?}"
    );
}
