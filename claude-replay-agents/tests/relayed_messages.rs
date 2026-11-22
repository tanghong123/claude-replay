//! #s272: a relayed message from the person is theirs only the first time its id appears in a
//! session, and that has to hold across a durable resume as well as a cold fold.
use claude_replay_agents::{claude_inbox, ClaudeAdapter};
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, SessionAccumulator};
use serde_json::json;

fn zero_key() -> Option<claude_inbox::InboxKey> {
    Some(claude_inbox::InboxKey {
        key: vec![0u8; 32],
        dir: "/tmp/inbox-fixture".into(),
    })
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

/// As Claude Code 2.1.318 records a message another session sends (#s655, aries-tell's): the
/// body in `origin`, the record's own text wrapping it.
fn sent(text: &str, at: u32) -> String {
    json!({
        "type": "user",
        "timestamp": format!("2026-10-26T08:00:{at:02}.000Z"),
        "isMeta": true,
        "promptSource": "system",
        "turnOrigin": "peer",
        "origin": {"kind": "peer", "from": "uds:/tmp/cc-socks/9.sock", "msg_id": format!("m{at}"), "name": "aries-tell", "fromMode": "bypass", "body": format!("{text}\n")},
        "message": {"role": "user", "content": format!("Another Claude session sent a message:\n<cross-session-message from=\"uds:/tmp/cc-socks/9.sock\" from-name=\"aries-tell\" from-mode=\"bypass\">\n{text}\n</cross-session-message>\n\nThis came from another Claude session.")},
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

/// Folds `lines` cold, then resumed at every point a durable resume can take, and checks each
/// resume folds to the cold blocks; returns them and where each resume was taken.
fn every_resume_point(lines: &[String]) -> (Vec<Block>, Vec<(usize, usize)>) {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    let adapter: &'static dyn TranscriptAdapter = &ClaudeAdapter;
    let mut cold = SessionAccumulator::new(adapter);
    let mut offset = 0;
    for line in lines {
        cold.advance_at(offset, line);
        offset += line.len() as u64 + 1;
    }
    let expected = cold.fold().0;
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
        for line in lines {
            if offset >= aligned.resume.replay_from {
                run.advance_at(offset, line);
            }
            offset += line.len() as u64 + 1;
        }
        assert_eq!(run.fold().0, expected, "resumed at line {split}");
        resumed.push((split, aligned.committed));
    }
    (expected, resumed)
}

#[test]
fn a_copy_of_a_relayed_message_stays_a_copy_across_every_resume_point() {
    claude_inbox::install_key_source(zero_key);
    let (first, second) = (message("dash-1"), message("dash-2"));
    let lines = [
        prompt(&first, 1),
        reply(1),
        prompt(&second, 2),
        reply(2),
        prompt(&first, 3),
        reply(3),
    ];
    let (expected, resumed) = every_resume_point(&lines);
    assert_eq!(inbox_ids(&expected), ["dash-1", "dash-2"], "{expected:#?}");
    assert!(
        matches!(expected.iter().rfind(|b| b.opens_turn()), Some(Block::UserText(t)) if t.contains("```pasted")),
        "the later copy reads as the text it is: {expected:#?}"
    );
    // A resume point AFTER the first message committed and BEFORE its copy is the one only the
    // persisted ids get right: without them the copy would be taken as his again.
    assert!(
        resumed.iter().any(|&(split, committed)| split < 5
            && inbox_ids(&expected[..committed.min(expected.len())]).contains(&"dash-1")),
        "the test must resume between the message and its copy: {resumed:?}"
    );
}

/// #s655: his message sent as a session message is his the first time its id is met, typed or sent
/// alike, and a later copy sent the same way is the session's message across every resume point.
#[test]
fn a_sent_copy_of_a_relayed_message_stays_the_sessions_across_every_resume_point() {
    claude_inbox::install_key_source(zero_key);
    let (first, second) = (message("dash-31"), message("dash-32"));
    let lines = [
        sent(&first, 1),
        reply(1),
        prompt(&second, 2),
        reply(2),
        sent(&first, 3),
        reply(3),
        sent(&second, 4),
        reply(4),
    ];
    let (expected, resumed) = every_resume_point(&lines);
    assert_eq!(
        inbox_ids(&expected),
        ["dash-31", "dash-32"],
        "{expected:#?}"
    );
    let peers: Vec<&Block> = expected
        .iter()
        .filter(|b| matches!(b, Block::PeerText { from, .. } if from == "aries-tell"))
        .collect();
    assert_eq!(
        peers.len(),
        2,
        "both sent copies are the session's: {expected:#?}"
    );
    assert!(
        resumed.iter().any(|&(split, committed)| split < 5
            && inbox_ids(&expected[..committed.min(expected.len())]).contains(&"dash-31")),
        "the test must resume between the message and its copy: {resumed:?}"
    );
}

/// #s655: a verified message's card carries no address, so the fold learns no name for it, and
/// a sent copy of it, a `PeerText`, teaches the name: either way a message this session sends back
/// there is named the same, cold or resumed after the card (and the copy) committed.
#[test]
fn a_reply_to_a_verified_senders_address_is_named_alike_cold_and_resumed() {
    claude_inbox::install_key_source(zero_key);
    let first = message("dash-41");
    replied(vec![sent(&first, 1), reply(1)], None);
    replied(
        vec![sent(&first, 1), reply(1), sent(&first, 2), reply(2)],
        Some("aries-tell"),
    );
}

/// `before`, then a SendMessage to the sender's address, every resume point folding alike; the
/// reply is named `named`, else by its address.
fn replied(before: Vec<String>, named: Option<&str>) {
    let mut lines = before;
    lines.extend([
        prompt("and reply to it", 3),
        json!({
            "type": "assistant",
            "timestamp": "2026-10-26T08:00:03.100Z",
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_r1", "name": "SendMessage",
                        "input": {"to": "uds:/tmp/cc-socks/9.sock", "summary": "got it", "message": "On it.", "type": "message"}}]},
        })
        .to_string(),
        json!({
            "type": "user",
            "timestamp": "2026-10-26T08:00:03.200Z",
            "toolUseResult": {"success": true, "message": "sent"},
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_r1", "content": "{\"success\":true,\"message\":\"sent\"}"}]},
        })
        .to_string(),
        reply(4),
    ]);
    let (expected, resumed) = every_resume_point(&lines);
    assert_eq!(inbox_ids(&expected), ["dash-41"], "{expected:#?}");
    let to = sent_to(&expected);
    assert_eq!(to.len(), 1, "{expected:#?}");
    match named {
        Some(name) => assert!(to[0].contains(name), "{to:?}"),
        None => assert!(!to[0].contains("aries-tell"), "{to:?}"),
    }
    let copies = lines.len() - 4;
    assert!(
        resumed
            .iter()
            .any(|&(split, committed)| split >= copies && committed >= 1),
        "the test must resume after what came before the reply committed: {resumed:?}"
    );
}

/// Whom each message this session sent went to, as its head names it.
fn sent_to(blocks: &[Block]) -> Vec<String> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::ToolUse {
                sent_to: Some(to), ..
            } => out.push(to.clone()),
            Block::Thinking { tools, .. } => out.extend(sent_to(tools)),
            _ => {}
        }
    }
    out
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
