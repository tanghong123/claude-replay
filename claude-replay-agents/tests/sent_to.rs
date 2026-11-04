//! #s355: a message to another session names who it went to by the name a peer message from that
//! address arrived under, and that has to hold across a durable resume as well as a cold fold: a
//! reply whose peer message sits in the committed part is named from the persisted senders.
use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, SessionAccumulator};
use serde_json::json;

/// A message from skua-HD, delivered between turns.
fn peer(at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-11-04T08:00:{at:02}.000Z"),
        "isMeta": true,
        "promptSource": "system",
        "turnOrigin": "peer",
        "origin": {"kind": "peer", "from": "uds:/tmp/cc-socks/101.sock", "verifiedPeerPid": 101,
                   "msg_id": format!("p{at}"), "name": "skua-HD", "fromMode": "bypass",
                   "body": "where does the write-up live?"},
        "message": {"role": "user", "content": "<cross-session-message from=\"uds:/tmp/cc-socks/101.sock\" from-name=\"skua-HD\" from-mode=\"bypass\">\nwhere does the write-up live?\n</cross-session-message>"},
    })
    .to_string()
}

fn prompt(text: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-11-04T08:00:{at:02}.000Z"),
        "message": {"role": "user", "content": text},
    })
    .to_string()
}

fn reply(at: u32) -> String {
    json!({
        "type": "assistant",
        "timestamp": format!("2026-11-04T08:00:{at:02}.500Z"),
        "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "noted"}]},
    })
    .to_string()
}

/// A reply to skua-HD's address, as the client tells the agent to write it.
fn send(id: &str, at: u32) -> [String; 2] {
    [
        json!({
            "type": "assistant",
            "timestamp": format!("2026-11-04T08:00:{at:02}.100Z"),
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "SendMessage",
                        "input": {"to": "uds:/tmp/cc-socks/101.sock", "summary": "where the write-up lives",
                                  "message": "In the notes repo.", "type": "message"}}]},
        })
        .to_string(),
        json!({
            "type": "user",
            "timestamp": format!("2026-11-04T08:00:{at:02}.200Z"),
            "toolUseResult": {"success": true, "message": "sent"},
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "{\"success\":true,\"message\":\"sent\"}"}]},
        })
        .to_string(),
    ]
}

fn recipients(blocks: &[Block]) -> Vec<String> {
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                Block::ToolUse {
                    sent_to: Some(to), ..
                } => out.push(to.clone()),
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
fn a_reply_names_its_recipient_across_every_resume_point() {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let [a1, a2] = send("m1", 2);
    let [b1, b2] = send("m2", 4);
    let lines = vec![
        peer(1),
        a1,
        a2,
        reply(2),
        prompt("and the next thing", 3),
        reply(3),
        prompt("tell skua-HD again", 4),
        b1,
        b2,
        reply(4),
    ];
    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(
        recipients(&expected),
        ["skua-HD", "skua-HD"],
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
    // A resume point AFTER the peer message committed and BEFORE the second reply is the one
    // only the persisted senders get right: without them that reply would name the address.
    assert!(
        resumed.iter().any(|&(split, committed)| split < 8
            && expected[..committed.min(expected.len())]
                .iter()
                .any(|b| matches!(b, Block::PeerText { .. }))),
        "the test must resume between the peer message and the later reply: {resumed:?}"
    );
}
