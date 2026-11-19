//! #s594: a call the client refused before it ran reads Declined, keeping the person's reason, and
//! that has to hold across a durable resume as well as a cold fold: a resume can begin between a
//! call and its result, where the fold marks a call it restored from the cache.
use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::{
    adapter::TranscriptAdapter,
    model::{Block, ToolStatus},
    SessionAccumulator,
};
use serde_json::{json, Value};

fn prompt(text: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-11-19T08:00:{at:02}.000Z"),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

fn reply(at: u32) -> String {
    json!({
        "type": "assistant",
        "timestamp": format!("2026-11-19T08:00:{at:02}.900Z"),
        "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "understood"}]},
    })
    .to_string()
}

/// A Bash call, and its result record carrying `keys` at the record's top level.
fn refused(id: &str, at: u32, text: &str, tur: &str, keys: Value) -> [String; 2] {
    let mut result = json!({
        "type": "user",
        "timestamp": format!("2026-11-19T08:00:{at:02}.500Z"),
        "toolUseResult": tur,
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "is_error": true, "content": text}]},
    });
    for (k, v) in keys.as_object().unwrap() {
        result[k] = v.clone();
    }
    [
        json!({
            "type": "assistant",
            "timestamp": format!("2026-11-19T08:00:{at:02}.100Z"),
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Bash",
                        "input": {"command": "make deploy"}}]},
        })
        .to_string(),
        result.to_string(),
    ]
}

/// Each Bash call's status and output, in order, wherever the fold grouped it.
fn calls(blocks: &[Block]) -> Vec<(Option<ToolStatus>, Option<String>)> {
    fn walk(blocks: &[Block], out: &mut Vec<(Option<ToolStatus>, Option<String>)>) {
        for b in blocks {
            match b {
                Block::ToolUse {
                    execution, output, ..
                } => out.push((execution.and_then(|e| e.status), output.clone())),
                Block::Thinking { tools, .. } => walk(tools, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

#[test]
fn a_call_refused_before_it_ran_is_declined_across_every_resume_point() {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let [a1, a2] = refused(
        "b1",
        2,
        "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). STOP what you are doing and wait for the user to tell you how to proceed.",
        "User rejected tool use",
        json!({"toolDenialKind": "user-rejected", "toolDenialBeforeRun": true}),
    );
    let [b1, b2] = refused(
        "b2",
        5,
        "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user said: use the staging host instead",
        "Error: Permission for this tool use was denied.",
        json!({"toolDenialKind": "user-rejected", "userFeedback": "use the staging host instead",
               "permissionDecision": {"decision": "reject", "source": "user_reject"}}),
    );
    let lines = vec![
        prompt("deploy it", 1),
        a1,
        a2,
        reply(3),
        prompt("try again", 4),
        b1,
        b2,
        reply(6),
    ];
    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(
        calls(&expected),
        [
            (Some(ToolStatus::Declined), None),
            (
                Some(ToolStatus::Declined),
                Some("use the staging host instead".to_string())
            ),
        ],
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
    // A resume after the first refusal committed is the one that restores a Declined call from the
    // cache's bytes rather than folding it again.
    assert!(
        resumed.iter().any(
            |&(_, committed)| calls(&expected[..committed.min(expected.len())])
                .first()
                .is_some_and(|c| c.0 == Some(ToolStatus::Declined))
        ),
        "the test must resume after the first refusal committed: {resumed:?}"
    );
}
