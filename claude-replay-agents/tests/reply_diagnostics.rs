//! Content-free native-envelope drift observations, with complete fallback controls.
use claude_replay_agents::{ClaudeAdapter, CodexAdapter};
use claude_replay_engine::{adapter::TranscriptAdapter, model::Block, unknown, SessionAccumulator};
use serde_json::json;

#[test]
fn known_reply_prefix_rejections_are_reported_once_per_native_record() {
    let codex = include_str!("fixtures/codex-quoted-reply.txt");
    for agent in ["claude", "codex"] {
        let adapter: &'static dyn TranscriptAdapter = if agent == "claude" {
            &ClaudeAdapter
        } else {
            &CodexAdapter
        };
        let valid = if agent == "claude" {
            "<!-- reply 1 -->\n> selected\n\ncomment"
        } else {
            codex
        };
        let invalid = if agent == "claude" {
            valid.replace("reply 1", "reply 3")
        } else {
            valid.replace("Each item contains", "Each item includes")
        };
        for (text, fragmented, reported) in [
            (valid.to_string(), false, false),
            (invalid.clone(), false, true),
            (valid.to_string(), true, true),
            (
                format!("Discuss this literal example: {invalid}"),
                false,
                false,
            ),
            (format!("```text\n{invalid}\n```"), false, false),
        ] {
            for scalar in [false, true] {
                if agent == "codex" && scalar || fragmented && scalar {
                    continue;
                }
                unknown::reset();
                let parts = if fragmented {
                    let at = text.len() / 2;
                    vec![&text[..at], &text[at..]]
                } else {
                    vec![text.as_str()]
                };
                let content = if scalar {
                    json!(text)
                } else {
                    json!(parts.iter().map(|t| json!({"type":if agent=="claude" {"text"} else {"input_text"},"text":t})).collect::<Vec<_>>())
                };
                let row = if agent == "claude" {
                    json!({"type":"user","version":"2.1.295","sessionId":"synthetic-quoted-reply","cwd":"/synthetic","message":{"role":"user","content":content}})
                } else {
                    json!({"type":"response_item","payload":{"type":"message","role":"user","content":content}})
                };
                let mut acc = SessionAccumulator::new(adapter);
                if agent == "codex" {
                    acc.advance_at(0,&json!({"type":"session_meta","payload":{"id":"synthetic-codex-reply","cwd":"/synthetic","cli_version":"0.147.0"}}).to_string());
                }
                acc.advance_at(256, &row.to_string());
                let output = acc.fold().0;
                let diagnostics: Vec<_> = unknown::snapshot()
                    .into_iter()
                    .filter(|x| x.at.as_str() == "text.envelope")
                    .collect();
                assert_eq!(
                    diagnostics.len(),
                    usize::from(reported),
                    "{agent}, scalar={scalar}, fragmented={fragmented}, text={text:?}"
                );
                if reported {
                    let seen = &diagnostics[0];
                    assert_eq!(seen.agent, agent);
                    assert_eq!(seen.count, 1);
                    assert_eq!(
                        seen.name,
                        if agent == "claude" {
                            "quoted_reply"
                        } else {
                            "response_annotations"
                        }
                    );
                    if agent == "claude" {
                        assert_eq!(seen.version.as_deref(), Some("2.1.295"));
                        assert_eq!(seen.example.as_deref(), Some("synthetic-quoted-reply"));
                    } else {
                        assert_eq!(seen.version.as_deref(), Some("0.147.0"));
                        assert_eq!(seen.example.as_deref(), Some("synthetic-codex-reply"));
                    }
                    assert!(
                        matches!(&output[0],Block::UserText(raw) if raw==&text),
                        "{output:?}"
                    );
                    assert!(!format!("{seen:?}").contains("selected"));
                }
            }
        }
    }
    // Header provenance must survive the same serialized preprocessor cursor used on resume.
    let mut before = CodexAdapter.line_preprocessor();
    before.process(&json!({"type":"session_meta","payload":{"id":"synthetic-restored-reply","cwd":"/synthetic","cli_version":"0.147.0"}}).to_string());
    let saved = serde_json::to_string(&before.state()).unwrap();
    let mut resumed = CodexAdapter.line_preprocessor();
    resumed.restore(&serde_json::from_str(&saved).unwrap());
    unknown::reset();
    resumed.process(&json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":codex.replace("Each item contains","Each item includes")}]}}).to_string());
    let seen = unknown::snapshot()
        .into_iter()
        .find(|x| x.at.as_str() == "text.envelope")
        .unwrap();
    assert_eq!(seen.count, 1);
    assert_eq!(seen.version.as_deref(), Some("0.147.0"));
    assert_eq!(seen.example.as_deref(), Some("synthetic-restored-reply"));
    unknown::reset();
}
