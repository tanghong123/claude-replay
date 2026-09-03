//! **The structured block stream** (#34) — `--dump --json`'s emission: one JSON object per
//! [`Block`], the content half of the shell-out vocabulary (`--paths --all` is the
//! discovery half). A consumer that cannot link this crate gets the SAME normalized
//! stream every frontend renders — which tool ran, did it fail, when, what the human
//! asked — instead of writing one transcript parser per agent and, in practice, one.
//!
//! Contract (the issue's, restated):
//! - `kind` is [`block_kind`]`(b).html()` — the existing fine classification the HTML
//!   type filter uses. No third vocabulary.
//! - Timestamps are **per TURN**, because that is what the model holds: the turn cursor
//!   advances only on `UserText`/`Command` (exactly [`crate::SessionIndex`]'s build rule), and
//!   every block carries its enclosing `turn` plus that turn's `turn_ts` — named so the
//!   granularity is impossible to misread as per-block. Blocks before the first turn
//!   carry `turn: null`.
//! - [`crate::model::ToolExecution`] facts ride along where the source recorded them (`status`,
//!   `exit`, `ms`) — failure detection without re-inferring it from output text.
//! - This is a SECOND emission of the stream, not a re-render: the text `--dump` bytes
//!   are untouched (the byte gate holds).
//!
//! Deliberately lean: a `ToolUse`'s `diffs`/`patch` internals and never-rendered `cwd`
//! stay out; `Attachment` emits its locator facts, never content bytes; a `SubAgent`
//! emits spawn facts and its `agent_id` — the child transcript is its own session,
//! discoverable via `--paths --all`, not an inline sub-stream.

use crate::model::{block_kind, AgentStatus, Block, ToolStatus};
use crate::Session;
use serde_json::{json, Map, Value};

/// Write `session`'s blocks as JSON Lines: one object per block, `\n`-terminated.
pub fn write_block_stream<W: std::io::Write>(
    session: &Session,
    out: &mut W,
) -> std::io::Result<()> {
    let times = &session.user_times;
    let mut turn: Option<usize> = None;
    for (i, b) in session.blocks().iter().enumerate() {
        if matches!(b, Block::UserText(_) | Block::Command { .. }) {
            turn = Some(turn.map_or(0, |t| t + 1));
        }
        let mut o = Map::new();
        o.insert("i".into(), json!(i));
        o.insert("turn".into(), json!(turn));
        o.insert(
            "turn_ts".into(),
            json!(turn.and_then(|t| times.get(t).copied().flatten())),
        );
        o.insert("kind".into(), json!(block_kind(b).html()));
        block_fields(b, &mut o);
        out.write_all(Value::Object(o).to_string().as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// The per-variant payload, appended to the envelope. Shared with the span's inner
/// tool list (which gets `kind` + these, no envelope — a span's tools have no index
/// or turn of their own).
fn block_fields(b: &Block, o: &mut Map<String, Value>) {
    match b {
        Block::UserText(t) | Block::AssistantText(t) | Block::ToolResult(t) => {
            o.insert("text".into(), json!(t));
        }
        Block::QueueEvent { text } => {
            o.insert("text".into(), json!(text));
        }
        Block::AssistantMessage {
            text,
            phase,
            inferred,
        } => {
            o.insert("text".into(), json!(text));
            o.insert(
                "phase".into(),
                json!(match phase {
                    crate::model::AssistantPhase::Commentary => "commentary",
                    crate::model::AssistantPhase::Final => "final",
                }),
            );
            if *inferred {
                o.insert("phaseInferred".into(), json!(true));
            }
        }
        Block::Thinking {
            text,
            duration_secs,
            tools,
        } => {
            o.insert("text".into(), json!(text));
            if let Some(d) = duration_secs {
                o.insert("duration_secs".into(), json!(d));
            }
            if !tools.is_empty() {
                let inner: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        let mut m = Map::new();
                        m.insert("kind".into(), json!(block_kind(t).html()));
                        block_fields(t, &mut m);
                        Value::Object(m)
                    })
                    .collect();
                o.insert("tools".into(), json!(inner));
            }
        }
        Block::ToolUse {
            name,
            target,
            output,
            read_lines,
            execution,
            ..
        } => {
            o.insert("name".into(), json!(name));
            o.insert("target".into(), json!(target));
            if let Some(out) = output {
                o.insert("output".into(), json!(out));
            }
            if let Some(n) = read_lines {
                o.insert("read_lines".into(), json!(n));
            }
            if let Some(e) = execution {
                if let Some(s) = e.status {
                    o.insert("status".into(), json!(tool_status(s)));
                }
                if let Some(c) = e.exit_code {
                    o.insert("exit".into(), json!(c));
                }
                if let Some(d) = e.duration {
                    o.insert(
                        "ms".into(),
                        json!(d.secs * 1000 + u64::from(d.nanos) / 1_000_000),
                    );
                }
            }
        }
        Block::Attachment(a) => {
            o.insert("label".into(), json!(a.kind.as_str()));
            o.insert("name".into(), json!(a.name));
            if let Some(p) = &a.path {
                o.insert("path".into(), json!(p));
            }
        }
        Block::SubAgent(sa) => {
            o.insert("agent_id".into(), json!(sa.agent_id));
            o.insert("agent_type".into(), json!(sa.agent_type));
            o.insert("description".into(), json!(sa.description));
            o.insert("prompt".into(), json!(sa.prompt));
            o.insert("status".into(), json!(agent_status(sa.status)));
            if let Some(r) = &sa.result {
                o.insert("result".into(), json!(r));
            }
        }
        Block::AgentDone {
            agent_id,
            agent_type,
            description,
            status,
            result,
        } => {
            o.insert("agent_id".into(), json!(agent_id));
            o.insert("agent_type".into(), json!(agent_type));
            o.insert("description".into(), json!(description));
            o.insert("status".into(), json!(agent_status(*status)));
            o.insert("done".into(), json!(true));
            if let Some(r) = result {
                o.insert("result".into(), json!(r));
            }
        }
        Block::Command { name, args, output } => {
            o.insert("name".into(), json!(name));
            o.insert("args".into(), json!(args));
            o.insert("output".into(), json!(output));
        }
        Block::Compaction {
            trigger,
            pre_tokens,
            post_tokens,
            summary,
        } => {
            o.insert("trigger".into(), json!(trigger.as_str()));
            o.insert("pre_tokens".into(), json!(pre_tokens));
            o.insert("post_tokens".into(), json!(post_tokens));
            o.insert("summary".into(), json!(summary));
        }
    }
}

/// [`ToolStatus`] as a stable lowercase word.
fn tool_status(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
        ToolStatus::Declined => "declined",
        ToolStatus::Cancelled => "cancelled",
        ToolStatus::Unknown => "unknown",
    }
}

/// [`AgentStatus`] as a stable lowercase word.
fn agent_status(s: AgentStatus) -> &'static str {
    match s {
        AgentStatus::Running => "running",
        AgentStatus::AsyncLaunched => "async_launched",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::Killed => "killed",
        AgentStatus::Stopped => "stopped",
        AgentStatus::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolDuration, ToolExecution};
    use crate::{parse_session_as, Agent};
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-blockjson-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// The envelope: one line per block, each valid JSON; `kind` is the existing fine
    /// classification; `turn` advances only on user turns and `turn_ts` is that TURN's
    /// time, repeated on every block the turn encloses.
    #[test]
    fn envelope_turns_and_timestamps_follow_the_model() {
        let p = tmp("turns.jsonl");
        std::fs::write(&p, concat!(
            "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"first\"}]},\"timestamp\":\"2026-08-21T10:00:00Z\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"reply one\"}]},\"timestamp\":\"2026-08-21T10:00:01Z\"}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"second\"}]},\"timestamp\":\"2026-08-21T10:01:40Z\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"reply two\"}]},\"timestamp\":\"2026-08-21T10:01:41Z\"}\n",
        )).unwrap();
        let s = parse_session_as(Agent::CLAUDE, &p).unwrap();
        let mut buf = Vec::new();
        write_block_stream(&s, &mut buf).unwrap();
        let lines: Vec<Value> = String::from_utf8(buf)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("every line is one JSON object"))
            .collect();
        assert_eq!(lines.len(), s.blocks().len(), "one line per block");

        for (i, (v, b)) in lines.iter().zip(s.blocks().iter()).enumerate() {
            assert_eq!(v["i"], i, "positions are the stream's");
            assert_eq!(
                v["kind"],
                block_kind(b).html(),
                "kind IS the existing classification"
            );
        }
        // Turn 0 opens at the first user block; its reply shares turn AND turn_ts.
        let t0 = lines[0]["turn_ts"].as_f64().expect("turn 0 has a time");
        assert_eq!(lines[0]["turn"], 0);
        assert_eq!(lines[1]["turn"], 0, "the reply belongs to the same turn");
        assert_eq!(
            lines[1]["turn_ts"].as_f64(),
            Some(t0),
            "and carries ITS time"
        );
        // The second user turn advances and gets its own (later) time.
        let second = lines
            .iter()
            .find(|v| v["text"] == "second")
            .expect("second turn present");
        assert_eq!(second["turn"], 1);
        let t1 = second["turn_ts"].as_f64().expect("turn 1 has a time");
        assert!(t1 > t0, "per-turn times are the turns' own: {t0} then {t1}");
        let _ = std::fs::remove_file(&p);
    }

    /// The execution facts: `status`/`exit`/`ms` from [`ToolExecution`], names stable and
    /// lowercase, `ms` a plain integer — nothing re-inferred from output text.
    #[test]
    fn tool_execution_facts_ride_along() {
        let b = Block::ToolUse {
            name: "Bash".into(),
            target: "cargo test".into(),
            diffs: Vec::new(),
            output: Some("2 passed".into()),
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: Some(ToolExecution {
                status: Some(ToolStatus::Failed),
                exit_code: Some(101),
                duration: Some(ToolDuration {
                    secs: 0,
                    nanos: 530_000_000,
                }),
            }),
            published: None,
        };
        let mut o = Map::new();
        block_fields(&b, &mut o);
        let v = Value::Object(o);
        assert_eq!(v["name"], "Bash");
        assert_eq!(v["target"], "cargo test");
        assert_eq!(v["output"], "2 passed");
        assert_eq!(v["status"], "failed");
        assert_eq!(v["exit"], 101);
        assert_eq!(v["ms"], 530);
    }
}
