//! Approved-RFC examples and one-change controls through native adapters.
use claude_replay_agents::{ClaudeAdapter, CodexAdapter};
use claude_replay_engine::{
    adapter::TranscriptAdapter,
    model::{Block, ReplyResponse},
    SessionAccumulator,
};
use serde_json::json;

#[test]
fn durable_resume_matches_cold_parse_at_each_native_message_boundary() {
    use claude_replay_engine::engine::{meta_stream::align, session::InMemoryStore};
    for (agent, text) in [("codex", CODEX), ("claude", CLAUDE)] {
        let adapter: &'static dyn TranscriptAdapter = if agent == "codex" {
            &CodexAdapter
        } else {
            &ClaudeAdapter
        };
        let lines = [
            line(agent, text, false),
            line(agent, text, false),
            line(agent, "next", false),
        ];
        let mut cold = SessionAccumulator::new(adapter);
        let mut offset = 0;
        for line in &lines {
            cold.advance_at(offset, line);
            offset += line.len() as u64 + 1;
        }
        let expected = cold.fold().0;
        let mut resumed = 0;
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
            // Simulate the actual persisted block/meta read, rather than retaining live structs.
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
            assert_eq!(run.fold().0, expected, "{agent} split {split}");
            resumed += 1;
        }
        assert!(
            resumed > 0,
            "{agent}: the test must exercise a real resume point"
        );
    }
}

#[test]
fn an_incomplete_native_record_never_exposes_a_partial_reply() {
    use claude_replay_engine::FollowParser;
    use std::io::Write;
    for (agent, text) in [("codex", CODEX), ("claude", CLAUDE)] {
        let adapter: &'static dyn TranscriptAdapter = if agent == "codex" {
            &CodexAdapter
        } else {
            &ClaudeAdapter
        };
        let jsonl = line(agent, text, false) + "\n";
        let path = std::env::temp_dir().join(format!(
            "cr-quoted-partial-{}-{agent}.jsonl",
            std::process::id()
        ));
        for split in [1, jsonl.len() / 2, jsonl.len() - 1] {
            std::fs::write(&path, &jsonl[..split]).unwrap();
            let mut follower = FollowParser::open(adapter, &path);
            follower.advance_stream().unwrap();
            assert!(
                follower.open_finalized().0.is_empty(),
                "{agent} split {split}"
            );
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(&jsonl.as_bytes()[split..])
                .unwrap();
            follower.advance_stream().unwrap();
            let (committed, _) = (follower.committed_tail(0), follower.open_finalized());
            let mut got = committed;
            got.extend(follower.open_finalized().0);
            assert_eq!(got, blocks(agent, text, false));
        }
        std::fs::remove_file(path).unwrap();
    }
}

const CODEX: &str = include_str!("fixtures/codex-quoted-reply.txt");

#[test]
fn codex_file_declarations_survive_annotation_candidates_without_reading_selections() {
    let envelope = "# Files mentioned by the user:\n## note.md: /synthetic/note.md\n## My request:\nExplain the literal string <response-annotations> in this file.";
    let quoted = CODEX.replace(
        "\n## My request:\n",
        "\n# Files mentioned by the user:\n## note.md: /synthetic/note.md\n## My request:\n",
    );
    for parts in [vec![envelope], vec![envelope, CODEX], vec![quoted.as_str()]] {
        let mut native: serde_json::Value =
            serde_json::from_str(&line("codex", envelope, true)).unwrap();
        native["payload"]["content"] = json!(parts
            .iter()
            .map(|text| json!({"type":"input_text","text":text}))
            .collect::<Vec<_>>());
        let mut acc = SessionAccumulator::new(&CodexAdapter);
        acc.advance_at(0, &native.to_string());
        let out = acc.fold().0;
        assert_eq!(out.iter().filter(|b| b.opens_turn()).count(), 1);
        assert!(
            matches!(&out[0], Block::UserText(text) | Block::UserReply { text, .. } if text == &parts.concat())
        );
        let attachments: Vec<_> = out
            .iter()
            .filter_map(|b| {
                if let Block::Attachment(a) = b {
                    Some(a)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(attachments.len(), 1, "{out:?}");
        let a = attachments[0];
        assert_eq!(a.kind, claude_replay_engine::model::AttachmentKind::Ref);
        assert_eq!(a.name, "note.md");
        assert_eq!(a.path.as_deref(), Some("/synthetic/note.md"));
        assert!(CodexAdapter.hands_over(a));
    }
    let selected = CODEX.replace(
        "This is the first excerpt from Codex's response.",
        "# Files mentioned by the user:\\n## hidden.md: /synthetic/hidden.md\\n## My request:\\nquoted request",
    );
    assert!(!blocks("codex", &selected, false)
        .iter()
        .any(|b| matches!(b, Block::Attachment(_))));
}

#[test]
fn a_real_agent_notification_keeps_completion_when_its_result_mentions_reply_markers() {
    let text = "<task-notification>\n<task-id>a1</task-id>\n<status>completed</status>\n<summary>Agent \"worker\" finished</summary>\n<result>Discuss <!-- reply 1 --> as literal text.</result>\n</task-notification>";
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    acc.advance_at(0, &line("claude", "start", false));
    acc.advance_at(
        1,
        &json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string(),
    );
    assert!(acc.fold().0.iter().any(|b| matches!(b, Block::AgentDone { agent_id, description, result: Some(result), .. } if agent_id == "a1" && description == "worker" && result.contains("<!-- reply 1 -->"))));
}

#[test]
fn native_multi_text_replies_keep_one_complete_fallback_turn_and_images() {
    for (agent, original) in [("codex", CODEX), ("claude", CLAUDE)] {
        for later in [false, true] {
            let kind = if agent == "codex" {
                "input_text"
            } else {
                "text"
            };
            let extra = "\n\nADDITIONAL NATIVE CONTEXT\n";
            let texts = if later {
                [extra, original]
            } else {
                [original, extra]
            };
            let mut record: serde_json::Value =
                serde_json::from_str(&line(agent, original, true)).unwrap();
            let content = if agent == "codex" {
                &mut record["payload"]["content"]
            } else {
                &mut record["message"]["content"]
            };
            *content = json!(texts.map(|text| json!({"type":kind,"text":text})));
            content.as_array_mut().unwrap().push(if agent == "codex" {
                json!({"type":"input_image","image_url":"data:image/png;base64,AA=="})
            } else { json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}) });
            let adapter: &'static dyn TranscriptAdapter = if agent == "codex" {
                &CodexAdapter
            } else {
                &ClaudeAdapter
            };
            let mut acc = SessionAccumulator::new(adapter);
            acc.advance_at(0, &record.to_string());
            let blocks = acc.fold().0;
            assert_eq!(
                blocks.iter().filter(|block| block.opens_turn()).count(),
                1,
                "{agent} later={later}"
            );
            assert!(matches!(&blocks[0], Block::UserText(text) if text == &texts.concat()));
            assert_eq!(
                blocks
                    .iter()
                    .filter(|block| matches!(block, Block::Attachment(_)))
                    .count(),
                1
            );
            assert!(!blocks
                .iter()
                .any(|block| matches!(block, Block::UserReply { .. })));
        }
    }
}

#[test]
fn malformed_prefixes_preserve_quoted_tags_before_transport_cleanup() {
    for text in [
        "<!-- Reply 1 -->\n> <local-command-caveat>KEEP THIS QUOTATION</local-command-caveat>\n\nMy comment",
        "<!--\nreply 1 -->\n> <local-command-caveat>KEEP THIS QUOTATION</local-command-caveat>\n\nMy comment",
        "<!--\tRePlY 1 -->\n> <local-command-caveat>KEEP THIS QUOTATION</local-command-caveat>\n\nMy comment",
        "<!- reply 1 -->\n> <command-name>/status</command-name>\n\nPlease explain this tag.",
        "!-- reply 1 -->\n> <command-name>/status</command-name>\n\nPlease explain this tag.",
        "<!-\r\nreply 1 -->\r\n> <local-command-caveat>KEEP THIS QUOTATION</local-command-caveat>\r\n\r\nMy comment",
        "<!-- changed marker -->\n> <command-name>literal</command-name>\n\nMy comment",
        "INTRO\n<!-- reply 1 -->\n> <pasted_content id=\"1\">literal</pasted_content id=\"1\">\n\nMy comment",
    ] {
        fallback("claude", text);
        assert_eq!(blocks("claude", text, true), vec![Block::UserText(text.into())]);
    }
    fallback(
        "codex",
        &CODEX.replace("# Response annotations:", "# Files mentioned by the user:"),
    );
    fallback(
        "codex",
        &CODEX.replace(
            "# Response annotations:",
            "# AGENTS.md instructions for /synthetic",
        ),
    );
}

#[test]
fn punctuation_changes_in_reply_markers_preserve_source_and_fragmented_queue_identity() {
    let header = "<!-- reply 1 -->";
    let mut markers = Vec::new();
    for (at, character) in header
        .char_indices()
        .filter(|(_, c)| c.is_ascii_punctuation())
    {
        let mut replacement = header.to_string();
        replacement.replace_range(at..at + character.len_utf8(), "?");
        markers.push(replacement);
        let mut deletion = header.to_string();
        deletion.remove(at);
        markers.push(deletion);
    }
    for marker in markers {
        let first = format!("INTRO\n{marker}\n> <local-command-caveat>KEEP SELECTED TEXT</local-command-caveat>\n\n");
        let text = first.clone() + "My comment";
        for array in [false, true] {
            assert_eq!(
                blocks("claude", &text, array),
                vec![Block::UserText(text.clone())],
                "{marker}"
            );
        }
        let mut acc = SessionAccumulator::new(&ClaudeAdapter);
        let mut offset = 0;
        let mut advance = |record: String| {
            acc.advance_at(offset, &record);
            offset += record.len() as u64 + 1;
        };
        advance(line("claude", "start", false));
        advance(json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string());
        advance(json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}).to_string());
        advance(json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":first},{"type":"text","text":"My comment"}]}}).to_string());
        advance(line("claude", "after", false));
        let out = acc.fold().0;
        assert!(
            !out.iter().any(|b| matches!(b, Block::QueueEvent { .. })),
            "{marker}: {out:?}"
        );
        let source = |b: &Block| matches!(b, Block::UserText(body) if body == &text);
        assert_eq!(
            out.iter().filter(|b| source(b)).count(),
            1,
            "{marker}: {out:?}"
        );
        assert!(
            acc.committed_tail(0).iter().any(source),
            "{marker}: delivery must commit"
        );
    }
}

#[test]
fn pasted_reply_examples_keep_nested_native_tags_literal_in_every_delivery_shape() {
    for literal in [
        "<command-name>/status</command-name><command-args>literal</command-args>",
        "<local-command-stdout>literal stdout</local-command-stdout>",
        "<local-command-caveat>KEEP SELECTED TEXT</local-command-caveat>",
        "<task-notification><summary>Agent \"quoted\" finished</summary><status>completed</status></task-notification>",
    ] {
        let inner = format!("<!-- reply 1 -->\n> {literal}");
        let pasted = format!("<pasted_content id=\"1\">\n{inner}\n</pasted_content id=\"1\">");
        for (text, expected) in [
            (pasted.clone(), format!("```pasted\n{inner}\n```")),
            (format!("Inspect:\n{pasted}\nKeep this context."), format!("Inspect:\n```pasted\n{inner}\n```\nKeep this context.")),
        ] {
            for shape in ["string", "array", "attachment", "fragmented"] {
                let mut acc = SessionAccumulator::new(&ClaudeAdapter);
                let mut offset = 0;
                let mut advance = |record: String| {
                    acc.advance_at(offset, &record);
                    offset += record.len() as u64 + 1;
                };
                advance(line("claude", "start", false));
                advance(json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string());
                advance(json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}).to_string());
                let delivery = match shape {
                    "attachment" => json!({"type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","prompt":text}}).to_string(),
                    "fragmented" => {
                        let parts: Vec<_> = text.chars().map(|c| json!({"type":"text","text":c.to_string()})).collect();
                        json!({"type":"user","message":{"role":"user","content":parts}}).to_string()
                    }
                    _ => line("claude", &text, shape == "array"),
                };
                advance(delivery);
                advance(line("claude", "after", false));
                let out = acc.fold().0;
                assert!(!out.iter().any(|b| matches!(b, Block::QueueEvent { .. } | Block::AgentDone { .. } | Block::Command { .. })), "{shape}: {out:?}");
                let is_paste = |b: &Block| matches!(b, Block::UserText(body) if body == &expected);
                assert_eq!(out.iter().filter(|b| is_paste(b)).count(), 1, "{shape}: {out:?}");
                assert!(acc.committed_tail(0).iter().any(is_paste), "{shape}: delivery must commit");
            }
        }
    }
}

#[test]
fn real_commands_ignore_open_and_closing_tags_inside_pasted_examples() {
    let pasted = "<pasted_content id=\"1\">\n<!-- reply 1 -->\n> <command-name>/fake</command-name>\n</pasted_content id=\"1\">";
    let args =
        "<pasted_content id=\"2\">literal </command-args>\n<?-- reply 1 -->\n> selected\n\nliteral</pasted_content id=\"2\">";
    let source =
        format!("{pasted}\n<command-name>/real</command-name><command-args>{args}</command-args>");
    assert!(
        matches!(&blocks("claude", &source, false)[..], [Block::Command { name, args: actual, .. }] if name == "/real" && actual == args)
    );
}

#[test]
fn fragmented_skill_reference_bodies_keep_the_existing_note_classification() {
    for inner in ["literal example", "<!-- reply 1 -->\n> literal"] {
        let body = format!("Base directory for this skill: /synthetic/skill\n\n<pasted_content id=\"1\">\n{inner}\n</pasted_content id=\"1\">");
        for trailing in ["", "Additional instructions."] {
            let native = json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":body},{"type":"text","text":trailing}]}});
            let mut acc = SessionAccumulator::new(&ClaudeAdapter);
            acc.advance_at(0, &native.to_string());
            let mut expected = vec![Block::ToolResult(body.clone())];
            if !trailing.is_empty() {
                expected.push(Block::UserText(trailing.into()));
            }
            assert_eq!(
                acc.fold().0,
                expected,
                "reference input must retain its existing routing"
            );
        }
    }
}

#[test]
fn skill_headers_inside_fragmented_literal_payloads_keep_one_queued_user_turn() {
    let inner = "Base directory for this skill: /synthetic/skill\n\n<!-- reply 1 -->\n> literal";
    for parts in [
        vec![
            "<pasted_content id=\"1\">\n",
            inner,
            "\n</pasted_content id=\"1\">",
        ],
        vec![
            "<command-args>",
            inner,
            "\n<pasted_content id=\"1\">example</pasted_content id=\"1\"></command-args>",
        ],
        vec!["<!-- reply 1 -->\n> ", inner, "\n\nMy comment"],
    ] {
        let text = parts.concat();
        let expected = if text.starts_with("<pasted_content") {
            format!("```pasted\n{inner}\n```")
        } else if text.starts_with("<command-args>") {
            format!("<command-args>{inner}\n```pasted\nexample\n```\n</command-args>")
        } else {
            text.clone()
        };
        let mut acc = SessionAccumulator::new(&ClaudeAdapter);
        let mut offset = 0;
        let mut advance = |record: String| {
            acc.advance_at(offset, &record);
            offset += record.len() as u64 + 1;
        };
        advance(line("claude", "start", false));
        advance(json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string());
        advance(json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}).to_string());
        assert!(acc
            .fold()
            .0
            .iter()
            .any(|b| matches!(b, Block::QueueEvent { .. })));
        let native = json!({"type":"user","message":{"role":"user","content":parts.iter().map(|text| json!({"type":"text","text":text})).collect::<Vec<_>>()}});
        acc.advance_at(offset, &native.to_string());
        offset += native.to_string().len() as u64 + 1;
        acc.advance_at(offset, &line("claude", "after", false));
        let out = acc.fold().0;
        assert!(
            !out.iter().any(|b| matches!(
                b,
                Block::QueueEvent { .. } | Block::ToolResult(_) | Block::UserReply { .. }
            )),
            "{out:?}"
        );
        let source = |b: &Block| matches!(b, Block::UserText(body) if body == &expected);
        assert_eq!(out.iter().filter(|b| source(b)).count(), 1, "{out:?}");
        assert_eq!(
            out.iter()
                .filter(|b| matches!(b, Block::UserText(_)))
                .count(),
            3,
            "one delivery opens one turn"
        );
        assert!(
            acc.committed_tail(0).iter().any(source),
            "a consumed queue cannot pin delivery"
        );
    }
    // A real later-item Skill body stays outside the human input, preserving legacy routing.
    let body = "Base directory for this skill: /synthetic/skill\n<pasted_content id=\"1\">example</pasted_content id=\"1\">";
    let native = json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"Intro"},{"type":"text","text":body}]}});
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    acc.advance_at(0, &native.to_string());
    assert_eq!(
        acc.fold().0,
        vec![
            Block::UserText("Intro".into()),
            Block::ToolResult(body.into())
        ]
    );
}

#[test]
fn queued_reply_candidates_match_delivery_without_dequeue_or_recursive_tag_cleanup() {
    for text in [
        "<!-- reply 1 -->\n> selected\n\n<pasted_content id=\"1\">literal</pasted_content id=\"1\">",
        "<!-- reply 3 -->\n> selected\n\n<pasted_content id=\"1\">literal</pasted_content id=\"1\">",
        "<!-- reply 1 -->\n> <task-notification><summary>Agent \"quoted\" finished</summary><status>completed</status></task-notification>\n\nliteral",
    ] {
        for shape in ["string", "array", "attachment"] {
            let mut acc = SessionAccumulator::new(&ClaudeAdapter);
            let mut offset = 0;
            let mut advance = |acc: &mut SessionAccumulator, native: String| {
                acc.advance_at(offset, &native);
                offset += native.len() as u64 + 1;
            };
            advance(&mut acc, line("claude", "start", false));
            advance(&mut acc, json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string());
            advance(&mut acc, json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}).to_string());
            assert!(acc.fold().0.iter().any(|block| matches!(block, Block::QueueEvent { .. })), "{shape}: enqueue must visibly wait");
            let delivery = if shape == "attachment" {
                json!({"type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","prompt":text}}).to_string()
            } else { line("claude", text, shape == "array") };
            advance(&mut acc, delivery);
            let shown = acc.fold().0;
            assert!(!shown.iter().any(|block| matches!(block, Block::QueueEvent { .. })), "{shape}: already-delivered input must not remain pending");
            assert!(!shown.iter().any(|block| matches!(block, Block::AgentDone { .. })), "{shape}: selected tags are literal content");
            let is_source = |block: &Block| matches!(block, Block::UserText(source) | Block::UserReply {text:source, ..} if source == text);
            assert_eq!(shown.iter().filter(|block| is_source(block)).count(), 1);
            advance(&mut acc, line("claude", "after", false));
            assert!(acc.committed_tail(0).iter().any(is_source), "{shape}: the consumed queue must not pin the replied turn's durability frontier");
        }
    }
}

#[test]
fn ordinary_quote_first_pastes_and_command_stdout_keep_native_normalization() {
    let cases = [
        ("<pasted_content id=\"1\">\n<!-- reply 1 -->\n> selected\n</pasted_content id=\"1\">", "```pasted\n<!-- reply 1 -->\n> selected\n```"),
        ("<pasted_content id=\"1\">\n> ordinary quote\n</pasted_content id=\"1\">", "```pasted\n> ordinary quote\n```"),
        ("- Discuss this passage:\n> ordinary quote\n\nInspect <pasted_content id=\"1\">**literal**</pasted_content id=\"1\">", "- Discuss this passage:\n> ordinary quote\n\nInspect \n```pasted\n**literal**\n```"),
    ];
    for (source, expected) in cases {
        for array in [false, true] {
            assert_eq!(
                blocks("claude", source, array),
                vec![Block::UserText(expected.into())]
            );
        }
    }
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let command = line("claude", "<command-name>/status</command-name>", false);
    acc.advance_at(0, &command);
    acc.advance_at(
        command.len() as u64 + 1,
        &line(
            "claude",
            "<local-command-stdout>\n> quoted command output\n</local-command-stdout>",
            false,
        ),
    );
    let out = acc.fold().0;
    assert!(
        matches!(&out[..], [Block::Command { output, .. }] if output.iter().any(|line| line.contains("> quoted command output"))),
        "{out:?}"
    );
}

#[test]
fn native_envelopes_keep_commands_notes_and_skill_bodies_with_literal_reply_examples() {
    let args = "Explain <!-- reply 1 --> as literal text.";
    let command =
        format!("<command-name>/status</command-name><command-args>{args}</command-args>");
    assert!(
        matches!(&blocks("claude", &command, false)[..], [Block::Command { name, args: actual, .. }] if name == "/status" && actual == args)
    );
    let skill = "Base directory for this skill: /synthetic/skill\n\nExample: <!-- reply 1 -->";
    for array in [false, true] {
        assert_eq!(
            blocks("claude", skill, array),
            vec![Block::ToolResult(skill.into())]
        );
    }
    let notification = "<task-notification><status>completed</status><summary>Agent worker finished</summary><result>Example <!-- reply 1 --></result></task-notification>";
    assert_eq!(
        blocks("claude", notification, false),
        vec![Block::ToolResult("Agent worker finished".into())]
    );
    assert!(blocks(
        "claude",
        "<local-command-caveat>Example <!-- reply 1 --></local-command-caveat>",
        false
    )
    .is_empty());
}

#[test]
fn fragmented_damaged_replies_match_the_complete_enqueue_and_commit() {
    let parts = ["INTRO\n\n", "<!- reply 1 -->\n> selected\n\n<pasted_content id=\"1\">**literal**</pasted_content id=\"1\">"];
    let text = parts.concat();
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut offset = 0;
    let mut advance = |acc: &mut SessionAccumulator, record: String| {
        acc.advance_at(offset, &record);
        offset += record.len() as u64 + 1;
    };
    advance(&mut acc, line("claude", "start", false));
    advance(
        &mut acc,
        json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string(),
    );
    advance(&mut acc, json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}).to_string());
    assert!(acc
        .fold()
        .0
        .iter()
        .any(|b| matches!(b, Block::QueueEvent { .. })));
    let mut delivery: serde_json::Value =
        serde_json::from_str(&line("claude", &text, true)).unwrap();
    delivery["message"]["content"] = json!(parts
        .iter()
        .map(|text| json!({"type":"text","text":text}))
        .collect::<Vec<_>>());
    advance(&mut acc, delivery.to_string());
    let out = acc.fold().0;
    assert!(
        !out.iter().any(|b| matches!(b, Block::QueueEvent { .. })),
        "{out:?}"
    );
    assert_eq!(
        out.iter()
            .filter(|b| matches!(b, Block::UserText(source) if source == &text))
            .count(),
        1
    );
    advance(&mut acc, line("claude", "after", false));
    assert!(acc
        .committed_tail(0)
        .iter()
        .any(|b| matches!(b, Block::UserText(source) if source == &text)));
}

#[test]
fn fragmented_markers_and_literal_pasted_examples_keep_one_consistent_turn() {
    for (agent, text) in [("codex", CODEX), ("claude", CLAUDE)] {
        let mut native: serde_json::Value = serde_json::from_str(&line(agent, text, true)).unwrap();
        let kind = if agent == "codex" {
            "input_text"
        } else {
            "text"
        };
        let parts: Vec<_> = text
            .chars()
            .map(|character| json!({"type":kind,"text":character.to_string()}))
            .collect();
        let adapter: &dyn TranscriptAdapter = if agent == "codex" {
            native["payload"]["content"] = json!(parts);
            &CodexAdapter
        } else {
            native["message"]["content"] = json!(parts);
            &ClaudeAdapter
        };
        let mut acc = SessionAccumulator::new(adapter);
        acc.advance_at(0, &native.to_string());
        assert_eq!(acc.fold().0, vec![Block::UserText(text.into())]);
    }
    let parts = [
        "<pasted_content id=\"1\">\n",
        "<!- reply 1 -->\n> selected",
        "\n</pasted_content id=\"1\">",
    ];
    let text = parts.concat();
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut offset = 0;
    let mut advance = |record: String| {
        acc.advance_at(offset, &record);
        offset += record.len() as u64 + 1;
    };
    advance(line("claude", "start", false));
    advance(json!({"type":"queue-operation","operation":"enqueue","content":text}).to_string());
    advance(json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]}}).to_string());
    advance(json!({"type":"user","message":{"role":"user","content":parts.iter().map(|text| json!({"type":"text","text":text})).collect::<Vec<_>>()}}).to_string());
    let out = acc.fold().0;
    assert!(
        !out.iter().any(|b| matches!(b, Block::QueueEvent { .. })),
        "{out:?}"
    );
    assert!(out.iter().any(|b| matches!(b, Block::UserText(body) if body == "```pasted\n<!- reply 1 -->\n> selected\n```")), "{out:?}");
}

const CLAUDE: &str = "<!-- reply 2 -->\n> Selected first\n> code | table\n\nFirst comment\n\n<!-- reply -->\n> Selected second\n\nLast comment\n\n\nOverall-looking text";

fn line(agent: &str, text: &str, array: bool) -> String {
    let record = if agent == "codex" {
        json!({"type":"response_item","timestamp":"2026-10-10T10:00:00Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}})
    } else {
        json!({"type":"user","timestamp":"2026-10-10T10:00:00Z","version":"2.1.295","entrypoint":"claude-desktop","message":{"role":"user","content":if array { json!([{"type":"text","text":text}]) } else { json!(text) }}})
    };
    record.to_string()
}
fn blocks(agent: &str, text: &str, array: bool) -> Vec<Block> {
    let adapter: &dyn TranscriptAdapter = if agent == "codex" {
        &CodexAdapter
    } else {
        &ClaudeAdapter
    };
    let mut acc = SessionAccumulator::new(adapter);
    acc.advance_at(0, &line(agent, text, array));
    acc.fold().0
}
fn fallback(agent: &str, text: &str) {
    assert_eq!(
        blocks(agent, text, false),
        vec![Block::UserText(text.into())],
        "{text}"
    );
}

#[test]
fn approved_codex_pairs_preserve_native_source_and_request() {
    let out = blocks("codex", CODEX, false);
    assert_eq!(out.len(), 1);
    let Block::UserReply { text, context } = &out[0] else {
        panic!("{out:?}")
    };
    assert_eq!(text, CODEX);
    assert!(out[0].opens_turn());
    assert_eq!(context.version, 1);
    assert_eq!(context.items.len(), 2);
    assert!(context.items[0].response.is_none());
    assert_eq!(
        context.items[0].quote,
        "This is the first excerpt from Codex's response."
    );
    assert_eq!(
        context.items[1].response,
        Some(ReplyResponse::Comment {
            text: "Please clarify this point.".into()
        })
    );
    assert_eq!(
        context.request.as_deref(),
        Some("Please provide a detailed example covering both excerpts.")
    );
}
#[test]
fn claude_pairs_keep_final_following_text_and_whitespace_in_both_native_shapes() {
    for array in [false, true] {
        let out = blocks("claude", CLAUDE, array);
        let Block::UserReply { text, context } = &out[0] else {
            panic!("{out:?}")
        };
        assert_eq!(out.len(), 1);
        assert_eq!(text, CLAUDE);
        assert_eq!(context.items[0].quote, "Selected first\ncode | table");
        assert_eq!(
            context.items[1].response,
            Some(ReplyResponse::FollowingText {
                text: "Last comment\n\n\nOverall-looking text".into()
            })
        );
        assert!(context.request.is_none());
    }
}
#[test]
fn claude_final_unclosed_code_fence_keeps_both_quotation_items() {
    for fence in ["```rust", "~~~rust"] {
        let following = format!("Please explain this code:\n{fence}\nlet value = 1;");
        let text = format!(
            "<!-- reply 2 -->\n> First selection\n\nFirst comment\n\n<!-- reply -->\n> Second selection\n\n{following}"
        );
        for array in [false, true] {
            let out = blocks("claude", &text, array);
            let [Block::UserReply {
                text: source,
                context,
            }] = &out[..]
            else {
                panic!("{out:?}")
            };
            assert_eq!(source, &text);
            assert!(out[0].opens_turn());
            assert_eq!(context.items.len(), 2);
            assert_eq!(context.items[0].quote, "First selection");
            assert_eq!(context.items[1].quote, "Second selection");
            assert_eq!(
                context.items[1].response,
                Some(ReplyResponse::FollowingText {
                    text: following.clone()
                })
            );
        }
        // A marker inside the first span's unclosed fence is still literal content.
        fallback(
            "claude",
            &format!("<!-- reply 2 -->\n> First selection\n\n{fence}\n<!-- reply -->\n> Second selection\n\n{following}"),
        );
        fallback(
            "claude",
            &format!("<!-- reply 2 -->\n> First selection\n\n{following}"),
        );
    }
}
#[test]
fn codex_one_change_controls_fall_back_completely() {
    let cases = [
        CODEX.replacen("\n# Response", "# Response", 1),
        CODEX.replace("Each item contains", "Each item includes"),
        CODEX.replace("response-annotations>", "Response-annotations>"),
        CODEX.replace('\n', "\r\n"),
        CODEX.replace("[{", "[\n{"),
        CODEX.replace("\"text\":", "\"unknown\":"),
        CODEX.replace("\"startOffset\":100", "\"startOffset\":-1"),
        CODEX.replace("\"endOffset\":146", "\"endOffset\":100"),
        CODEX.replace("\"startOffset\":100", "\"startOffset\":100.0"),
        CODEX.replace("\"startOffset\":100", "\"startOffset\":true"),
        CODEX.replace("\"messageId\":\"msg_synthetic_response\"", "\"messageId\":\"\""),
        CODEX.replace("\"annotation\":\"Please clarify this point.\"", "\"annotation\":\"\""),
        CODEX.replace("\"annotation\":\"Please clarify this point.\"", "\"annotation\":null"),
        CODEX.replace("\"text\":\"This is the first excerpt from Codex's response.\"", "\"text\":\"\""),
        CODEX.replace("Codex's", "Codex\\u0027s"),
        CODEX.replace("\"text\":\"This is the second excerpt from Codex's response.\",\"annotation\":\"Please clarify this point.\"",
            "\"annotation\":\"Please clarify this point.\",\"text\":\"This is the second excerpt from Codex's response.\""),
        CODEX.replace("\"source\":{", "\"source\":{\"extra\":1,"),
    ];
    for text in cases {
        fallback("codex", &text);
    }
}
#[test]
fn claude_boundary_controls_fall_back_completely() {
    for text in [
        CLAUDE.replace("reply 2", "reply 3"),
        CLAUDE.replace("reply 2", "reply"),
        CLAUDE.replace("> Selected first", ">Selected first"),
        CLAUDE.replace("\n\nFirst comment", "\nFirst comment"),
        CLAUDE.replace("First comment\n\n<!--", "First comment\n<!--"),
        CLAUDE.replace('\n', "\r\n"),
        format!("{CLAUDE}\n\n<!-- reply -->\n> third\n\ntext"),
        "<!-- reply 1 -->\n> selected".into(),
        "<!-- reply 1 -->\n> selected\n\ncomment\n\n<!-- Reply -->\n> second\n\ntext".into(),
        "<!-- reply 1 -->\n> \n\n".into(),
        format!(" {CLAUDE}"),
    ] {
        fallback("claude", &text);
    }
}
#[test]
fn quote_only_and_literal_marker_content_are_preserved() {
    for text in [
        "<!-- reply 1 -->\n> selected\n\n",
        "<!-- reply 1 -->\n> selected\rtext\n\ncomment\rtext",
        "<!-- reply 2 -->\n> first\n\n<!-- reply -->\n> second\n\n",
        "<!-- reply 1 -->\n> <!-- reply -->\n\n```text\n<!-- reply -->\n```\n",
        "<!-- reply 1 -->\n> <local-command-caveat>literal</local-command-caveat>\n\n<pasted_content id=\"1\">literal</pasted_content id=\"1\">",
    ] {
        let out = blocks("claude", text, false);
        assert!(matches!(&out[..], [Block::UserReply {text: source, ..}] if source == text), "{out:?}");
    }
    for agent in ["codex", "claude"] {
        fallback(agent, "> ordinary quote\n\nComment");
        fallback(agent, "Prose about <!-- reply 1 -->");
        fallback(agent, r#"{"text":"<!-- reply 1 -->\\n> selection\\n\\n"}"#);
    }
}
#[test]
fn codex_tail_and_multiline_selection_are_never_reconstructed_or_lost() {
    let text = CODEX
        .replace(
            "This is the first excerpt from Codex's response.",
            "line one\\n```rust\\n  code\\n```\\n| a | b |",
        )
        .replace(
            "\n## My request:",
            "\n# Files mentioned by the user:\n## note.md: /synthetic/note.md\n## My request:",
        );
    let out = blocks("codex", &text, false);
    let Block::UserReply {
        text: source,
        context,
    } = &out[0]
    else {
        panic!("{out:?}")
    };
    assert_eq!(source, &text);
    assert_eq!(
        context.items[0].quote,
        "line one\n```rust\n  code\n```\n| a | b |"
    );
    assert!(context.context.contains("/synthetic/note.md"));
    assert_eq!(context.items.len(), 2);
}
#[test]
fn canonical_javascript_offsets_and_future_client_versions() {
    let canonical = CODEX
        .replace("\"startOffset\":100", "\"startOffset\":1e+21")
        .replace("\"endOffset\":146", "\"endOffset\":2e+21");
    assert!(matches!(
        blocks("codex", &canonical, false)[0],
        Block::UserReply { .. }
    ));
    let noncanonical = CODEX
        .replace("\"startOffset\":100", "\"startOffset\":9007199254740993")
        .replace("\"endOffset\":146", "\"endOffset\":9007199254741000");
    fallback("codex", &noncanonical);
}
#[test]
fn incremental_and_durable_blocks_retain_reply_metadata() {
    for (agent, text) in [("codex", CODEX), ("claude", CLAUDE)] {
        let adapter: &dyn TranscriptAdapter = if agent == "codex" {
            &CodexAdapter
        } else {
            &ClaudeAdapter
        };
        let mut acc = SessionAccumulator::new(adapter);
        acc.advance_at(0, &line(agent, "first prompt", false));
        let mut expected = vec![Block::UserText("first prompt".into())];
        expected.extend(blocks(agent, text, false));
        acc.advance_at(0, &line(agent, text, false));
        assert_eq!(acc.fold().0, expected);
        let saved = serde_json::to_vec(&expected).unwrap();
        let restored: Vec<Block> = serde_json::from_slice(&saved).unwrap();
        assert_eq!(restored, expected);
        assert_eq!(restored.iter().filter(|b| b.opens_turn()).count(), 2);
    }
}

#[test]
fn an_ordinary_html_comment_keeps_the_existing_pasted_card_normalization() {
    let text =
        "<!-- note -->\n\nInspect <pasted_content id=\"1\">**literal**</pasted_content id=\"1\">";
    for array in [false, true] {
        let shown = blocks("claude", text, array);
        assert!(
            matches!(&shown[..], [Block::UserText(value)] if value.contains("```pasted\n**literal**\n```") && !value.contains("<pasted_content"))
        );
    }
}
