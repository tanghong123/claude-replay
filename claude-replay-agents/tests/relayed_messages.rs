//! #s272: a relayed message from the person is theirs only the first time its id appears in a
//! session, and that has to hold across a durable resume as well as a cold fold.
use claude_replay_agents::{claude_inbox, ClaudeAdapter};
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, SessionAccumulator};
use serde_json::json;

fn zero_key() -> Option<Vec<u8>> {
    Some(vec![0u8; 32])
}

/// A relay from the dashboard, signed with the format's zero test key.
fn message(id: &str) -> String {
    claude_inbox::sign_for_test(
        &[0; 32],
        &format!(
            "<aries-inbox v=\"1\" from=\"Hong\" via=\"dashboard\" kind=\"message\" at=\"2026-10-26T16:35:20+08:00\" id=\"{id}\" sig=\"-\">\n<do>answer him</do>\nwhich tern is it?\nthe second line\n</aries-inbox>"
        ),
    )
}

/// As Claude Code records a paste of four lines or more: the whole prompt.
fn prompt(text: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-10-26T08:00:{at:02}.000Z"),
        "message": {"role": "user", "content": format!("\n\n<pasted_content id=\"608f\">\n{text}\n</pasted_content id=\"608f\">\n")},
    })
    .to_string()
}

fn reply(at: u32) -> String {
    json!({
        "type": "assistant",
        "timestamp": format!("2026-10-26T08:00:{at:02}.500Z"),
        "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "noted"}]},
    })
    .to_string()
}

fn inbox_ids(blocks: &[Block]) -> Vec<&str> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::UserInbox { inbox, .. } => Some(inbox.id.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_copy_of_a_relayed_message_stays_a_copy_across_every_resume_point() {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    claude_inbox::install_key_source(zero_key);
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let (first, second) = (message("dash-1"), message("dash-2"));
    let lines = [
        prompt(&first, 1),
        reply(1),
        prompt(&second, 2),
        reply(2),
        prompt(&first, 3),
        reply(3),
    ];
    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in &lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
    assert_eq!(inbox_ids(&expected), ["dash-1", "dash-2"], "{expected:#?}");
    assert!(
        matches!(expected.iter().rfind(|b| b.opens_turn()), Some(Block::UserText(t)) if t.contains("```pasted")),
        "the later copy reads as the text it is: {expected:#?}"
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
        let saved = serde_json::to_vec(&committed).unwrap();
        let loaded: Vec<Block> = serde_json::from_slice(&saved).unwrap();
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
    // A resume point AFTER the first message committed and BEFORE its copy is the one only the
    // persisted ids get right: without them the copy would be taken as his again.
    assert!(
        resumed.iter().any(|&(split, committed)| split < 5
            && inbox_ids(&expected[..committed.min(expected.len())]).contains(&"dash-1")),
        "the test must resume between the message and its copy: {resumed:?}"
    );
}

/// A live tail folds a poll at a time: the copy arriving in a later poll is still a copy.
#[test]
fn a_copy_arriving_in_a_later_poll_is_a_copy() {
    claude_inbox::install_key_source(zero_key);
    let first = message("dash-9");
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut offset = 0;
    for line in [prompt(&first, 1), reply(1)] {
        acc.advance_at(offset, &line);
        offset += line.len() as u64 + 1;
    }
    assert_eq!(inbox_ids(&acc.fold().0), ["dash-9"]);
    for line in [prompt(&first, 2), reply(2)] {
        acc.advance_at(offset, &line);
        offset += line.len() as u64 + 1;
    }
    let blocks = acc.fold().0;
    assert_eq!(inbox_ids(&blocks), ["dash-9"], "{blocks:#?}");
    assert_eq!(blocks.iter().filter(|b| b.opens_turn()).count(), 2);
}
