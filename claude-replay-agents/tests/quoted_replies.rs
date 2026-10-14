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
