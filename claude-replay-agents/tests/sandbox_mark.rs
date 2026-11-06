//! #s381: a Bash call that ran outside the sandbox is marked only in a transcript that ran with
//! the sandbox on, and the transcript's head that says so (`sandbox_instructions`) sits far above
//! most calls. So the fold's knowledge of it has to hold across a durable resume whose committed
//! part holds the head: the meta stream carries it. This pins that every resume point folds what
//! a cold fold does, and that one of them falls between the head and the marked call.
use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, SessionAccumulator};
use serde_json::json;

fn prompt(text: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-11-06T01:00:{at:02}.000Z"),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

/// The client telling the model how its Bash sandbox works, at the session's head.
fn head(at: u32) -> String {
    json!({
        "type": "attachment",
        "timestamp": format!("2026-11-06T01:00:{at:02}.100Z"),
        "attachment": {"type": "sandbox_instructions",
                       "content": "## Bash command sandbox\n\nCommands run inside a sandbox that limits writes to the workspace."},
    })
    .to_string()
}

fn reply(at: u32) -> String {
    json!({
        "type": "assistant",
        "timestamp": format!("2026-11-06T01:00:{at:02}.900Z"),
        "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "noted"}]},
    })
    .to_string()
}

/// A Bash call and its result, which ran inside the sandbox or not.
fn bash(id: &str, inside: bool, at: u32) -> [String; 2] {
    [
        json!({
            "type": "assistant",
            "timestamp": format!("2026-11-06T01:00:{at:02}.200Z"),
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "Bash",
                        "input": {"command": "curl https://tern.example/nests", "dangerouslyDisableSandbox": !inside}}]},
        })
        .to_string(),
        json!({
            "type": "user",
            "timestamp": format!("2026-11-06T01:00:{at:02}.300Z"),
            "sandboxed": inside,
            "toolUseResult": {"stdout": "42", "stderr": "", "interrupted": false},
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "42"}]},
        })
        .to_string(),
    ]
}

fn marked(blocks: &[Block]) -> usize {
    fn walk(blocks: &[Block], n: &mut usize) {
        for b in blocks {
            match b {
                Block::ToolUse { execution, .. } => {
                    if execution.is_some_and(|e| e.outside_sandbox) {
                        *n += 1;
                    }
                }
                Block::Thinking { tools, .. } => walk(tools, n),
                _ => {}
            }
        }
    }
    let mut n = 0;
    walk(blocks, &mut n);
    n
}

#[test]
fn an_outside_call_is_marked_across_every_resume_point() {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let [a1, a2] = bash("toolu_out", false, 5);
    let lines = vec![
        prompt("count the tern nests", 1),
        head(1),
        reply(1),
        prompt("and the skua burrows", 2),
        reply(2),
        prompt("fetch the colony count", 5),
        a1,
        a2,
        reply(5),
    ];
    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(marked(&expected), 1, "{expected:#?}");

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
        resumed.push((split, aligned.resume.replay_from));
    }
    // A resume that starts after the head (line 2) and before the outside call (line 7) is the
    // one only the meta stream's gauge gets right: without it the call would not be marked.
    let head_end: u64 = lines[..2].iter().map(|l| l.len() as u64 + 1).sum();
    let call_at: u64 = lines[..6].iter().map(|l| l.len() as u64 + 1).sum();
    assert!(
        resumed
            .iter()
            .any(|&(_, from)| from >= head_end && from <= call_at),
        "the test must resume between the head and the outside call: {resumed:?}"
    );
}
