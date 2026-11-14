//! #s378: a reply the safety classifier stopped is marked where the client's note to the model
//! was, and that has to hold across a durable resume as well as a cold fold. The fold remembers
//! the stopped reply for its note without the meta stream, because a resume begins at a line that
//! opens a turn and the note follows its reply inside the turn; this pins that every resume point
//! folds what a cold fold does. #s470: the same holds for Claude Code 2.1.322's other form of the
//! stop, its API-error record naming the reply, right after the reply or after the stopped call's
//! result. A resume can also begin INSIDE a turn (a queued prompt picked up), so the fold's
//! stopped replies ride the meta stream, and a pickup between a stopped reply and the record
//! naming it is walked too.
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
fn a_stopped_reply_is_marked_across_every_resume_point() {
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

    let resumed = resume_everywhere(&lines, &expected);
    // A resume with the first mark already committed and the second reply still to come is the
    // one that shows the fold's memory of a stopped reply is not needed across a resume.
    assert!(
        resumed.iter().any(|&(split, committed)| split <= 8
            && marks(&expected[..committed.min(expected.len())]) == 1),
        "the test must resume between the two stopped replies: {resumed:?}"
    );
}

/// #s470: a reply Claude Code 2.1.322 stopped as two records of one message id (thinking, then a
/// call), followed by its synthetic API-error record naming the reply, then the call's result, or
/// the result first and the record after it, as measured.
fn stopped_by_error(n: u32, at: u32, result_first: bool) -> Vec<String> {
    let id = format!("msg_s470_{n}");
    let call = format!("toolu_s470_{n}");
    let stop = json!({"type": "refusal", "category": "reasoning_extraction", "explanation": "Flagged in a fixture."});
    let reply = |content: serde_json::Value| {
        json!({
            "type": "assistant", "version": "2.1.322",
            "timestamp": format!("2026-11-06T01:00:{at:02}.100Z"),
            "message": {"id": id, "role": "assistant", "model": "claude-opus-5-5",
                        "stop_reason": "refusal", "stop_details": stop, "content": [content]},
        })
        .to_string()
    };
    let error = json!({
        "type": "assistant", "version": "2.1.322",
        "timestamp": format!("2026-11-06T01:00:{at:02}.300Z"),
        "isApiErrorMessage": true, "error": "invalid_request", "refusedMessageId": id,
        "message": {"id": format!("msg_err{n}"), "role": "assistant", "model": "<synthetic>",
                    "stop_reason": "refusal", "stop_details": stop,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                    "content": [{"type": "text", "text": format!("API Error: stop {n} in this fixture.")}]},
    })
    .to_string();
    let result = json!({
        "type": "user", "version": "2.1.322",
        "timestamp": format!("2026-11-06T01:00:{at:02}.400Z"),
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": call, "content": "the call did not run"}]},
    })
    .to_string();
    let mut out = vec![
        reply(
            json!({"type": "thinking", "thinking": format!("Counting nests {n}."), "signature": "x"}),
        ),
        reply(
            json!({"type": "tool_use", "id": call, "name": "Bash", "input": {"command": "ls nests"}}),
        ),
    ];
    if result_first {
        out.extend([result, error]);
    } else {
        out.extend([error, result]);
    }
    out
}

fn error_marks(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::SafetyStop {
                category: Some(c),
                note,
            } => Some(format!("{c}: {note}")),
            _ => None,
        })
        .collect()
}

#[test]
fn an_api_error_naming_a_stopped_reply_is_its_mark_across_every_resume_point() {
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let mut lines = vec![prompt("count the petrel nests", 1)];
    lines.extend(stopped_by_error(1, 2, false));
    lines.push(reply(3));
    lines.push(prompt("and the skua burrows", 4));
    lines.extend(stopped_by_error(2, 5, true));
    lines.push(reply(6));
    // The third: a queued prompt picked up between the call's result and the record naming the
    // reply, a line a resume can begin at (the review of #s470).
    lines.push(prompt("and the terns", 7));
    let mut third = stopped_by_error(3, 8, true);
    let error = third.pop().unwrap();
    lines.extend(third);
    lines.push(
        json!({"type": "queue-operation", "operation": "enqueue", "timestamp": "2026-11-06T01:00:08.410Z",
               "content": "and the gulls too"})
        .to_string(),
    );
    lines.push(
        json!({"type": "attachment", "timestamp": "2026-11-06T01:00:08.420Z",
               "attachment": {"type": "queued_command", "commandMode": "prompt", "prompt": "and the gulls too"}})
        .to_string(),
    );
    lines.push(error);
    lines.push(reply(9));

    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(
        error_marks(&expected),
        [
            "reasoning_extraction: API Error: stop 1 in this fixture.",
            "reasoning_extraction: API Error: stop 2 in this fixture.",
            "reasoning_extraction: API Error: stop 3 in this fixture."
        ],
        "{expected:#?}"
    );
    assert!(
        !expected
            .iter()
            .any(|b| matches!(b, Block::ToolResult(t) if t.starts_with("API Error"))),
        "{expected:#?}"
    );
    let resumed = resume_everywhere(&lines, &expected);
    // A resume with the first mark committed and the second stop still to come shows a reply and
    // its record folded together after a resume; one with the third reply committed and its mark
    // not (at the pickup) shows the fold's stopped replies carried across it.
    let head = |committed: usize| &expected[..committed.min(expected.len())];
    assert!(
        resumed
            .iter()
            .any(|&(_, committed)| error_marks(head(committed)).len() == 1),
        "the test must resume between the first two stops: {resumed:?}"
    );
    let third_reply = |blocks: &[Block]| {
        blocks
            .iter()
            .any(|b| matches!(b, Block::Thinking { text, .. } if text == "Counting nests 3."))
    };
    assert!(
        resumed
            .iter()
            .any(|&(_, committed)| third_reply(head(committed))
                && error_marks(head(committed)).len() == 2),
        "the test must resume between the third reply and the record naming it: {resumed:?}"
    );
}
