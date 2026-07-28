//! Incremental live follower (M16): fold only the newly-appended lines through a persistent
//! `Replayer`, instead of re-parsing the whole file each poll. The `snapshot` is proven
//! byte-identical to a full re-parse (`follow_matches_full_reparse`), at O(delta) work per
//! poll — no whole-file re-read/re-decode. On a truncation/rewrite (compaction) the tail
//! resets and the Replayer rebuilds from scratch (a full replay of the new content).

use std::path::{Path, PathBuf};

use crate::adapter::{adapter, MetricsAccumulator, TranscriptAdapter};
use crate::engine::message::Message;
use crate::engine::replay::Replayer;
use crate::metrics::Metrics;
use crate::model::Block;
use crate::tail::TailReader;
use crate::{Agent, SessionGraph};

/// Follows a transcript file, folding only newly-appended lines each poll. Everything
/// agent-specific — the L1 decoder, the L2 `Shaping`, the metrics accumulator — comes from
/// the agent's `TranscriptAdapter`, so the follower itself is agent-agnostic.
pub struct FollowParser {
    adapter: &'static dyn TranscriptAdapter,
    tail: TailReader,
    replayer: Replayer<'static>,
    cwd: String,
    metrics: Box<dyn MetricsAccumulator>,
    source: PathBuf,
    graph: SessionGraph,
}

impl FollowParser {
    /// Follow `path` from the beginning: the first `poll` folds the whole current file, then
    /// subsequent polls fold only appends.
    pub fn open(agent: Agent, path: &Path) -> Self {
        Self::open_with_graph(agent, path, SessionGraph::open(agent, path))
    }

    /// Follow with a relationship graph shared by the surrounding operation.
    pub fn open_with_graph(agent: Agent, path: &Path, graph: SessionGraph) -> Self {
        let adapter = adapter(agent);
        Self {
            adapter,
            tail: TailReader::open_at_start(path),
            replayer: Replayer::new(adapter.shaping(), std::collections::HashSet::new()),
            cwd: String::new(),
            metrics: adapter.metrics_acc(),
            source: path.to_path_buf(),
            graph,
        }
    }

    /// Poll: fold any newly-appended lines (or rebuild on a truncation/rewrite) and return
    /// the current blocks + per-turn times + metrics. Returns `None` when nothing changed
    /// since the last poll (the common idle tick). O(delta) except on a rewrite.
    #[allow(clippy::type_complexity)]
    pub fn poll(&mut self) -> std::io::Result<Option<(Vec<Block>, Vec<Option<f64>>, Metrics)>> {
        let p = self.tail.poll()?;
        if !p.reset && p.lines.is_empty() {
            return Ok(None); // nothing new this tick
        }
        if p.reset {
            // Truncation / compaction: the kept prefix changed. Rebuild from scratch — the
            // TailReader re-read from 0, so `p.lines` is the whole new file.
            self.replayer = Replayer::new(self.adapter.shaping(), std::collections::HashSet::new());
            self.cwd.clear();
            self.metrics = self.adapter.metrics_acc();
        }
        let mut delta: Vec<Message> = Vec::new();
        for line in &p.lines {
            delta.clear();
            self.adapter.decode_line(line, &mut self.cwd, &mut delta);
            // Pre-scan this delta's tool ids so a result whose tool_use is later in the same
            // delta is held pending, not mis-emitted as an orphan (matches a batch pre-scan).
            self.replayer
                .extend_tool_ids(delta.iter().filter_map(|m| match m {
                    Message::ToolUse { id, .. } if !id.is_empty() => Some(id.clone()),
                    _ => None,
                }));
            self.replayer.apply(&delta);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                self.metrics.push(&v);
            }
        }
        let (mut blocks, user_times) = self.replayer.snapshot();
        self.graph.resolve_relationships(&self.source, &mut blocks);
        Ok(Some((blocks, user_times, self.metrics.finish())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::parse_session_with_graph;
    use crate::SessionGraph;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp() -> std::path::PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "cr-follow-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// The follower's `poll` must return byte-identical blocks + metrics to a full
    /// `parse_session_as` of the current file, at every append — including a Codex
    /// call/output split across polls (back-patch) and a truncation→rebuild (reset).
    fn assert_follow(agent: Agent, chunks: &[&str]) {
        let path = tmp();
        let mut fp = FollowParser::open(agent, &path);
        let mut written = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            written.push_str(chunk);
            std::fs::write(&path, written.as_bytes()).unwrap();
            let (fblocks, ftimes, fmetrics) = fp.poll().unwrap().expect("content advanced");
            let s = crate::engine::parse_session_as(agent, &path).unwrap();
            assert_eq!(
                format!("{:?}", fblocks),
                format!("{:?}", s.blocks),
                "blocks differ after chunk {i} ({agent:?})"
            );
            assert_eq!(ftimes, s.user_times, "user_times differ after chunk {i}");
            assert_eq!(
                fmetrics, s.metrics,
                "metrics differ after chunk {i} ({agent:?})"
            );
        }
        // Truncation / rewrite: shrink the file → the follower resets and rebuilds.
        let rewritten = format!("{}\n", chunks[0].trim_end());
        std::fs::write(&path, rewritten.as_bytes()).unwrap();
        let (fblocks, _, _) = fp.poll().unwrap().expect("reset advances");
        let s = crate::engine::parse_session_as(agent, &path).unwrap();
        assert_eq!(
            format!("{:?}", fblocks),
            format!("{:?}", s.blocks),
            "blocks differ after rewrite ({agent:?})"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn follow_matches_full_reparse_claude() {
        assert_follow(
            Agent::Claude,
            &[
                "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"b1\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}],\"usage\":{\"input_tokens\":10,\"output_tokens\":20}},\"timestamp\":\"2026-07-26T10:00:01Z\"}\n",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"b1\",\"content\":\"out\"}]},\"timestamp\":\"2026-07-26T10:00:02Z\"}\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":8}},\"timestamp\":\"2026-07-26T10:00:03Z\"}\n",
            ],
        );
    }

    #[test]
    fn follow_matches_full_reparse_codex() {
        // Codex splits a call and its output across polls — the persistent Replayer
        // back-patches without a full re-parse.
        assert_follow(
            Agent::Codex,
            &[
                "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/repo\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"fix\"}]}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"call_id\":\"c1\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"c1\",\"output\":\"a.rs\"}}\n",
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n",
            ],
        );
    }

    #[test]
    fn follow_matches_full_reparse_codex_subagents() {
        let root = std::env::temp_dir().join(format!(
            "cr-follow-agents-{}-{}",
            std::process::id(),
            AtomicUsize::new(0).fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&root).ok();
        let sessions = root.join("sessions");
        let day = sessions.join("2026/07/27");
        std::fs::create_dir_all(&day).unwrap();
        let parent = day.join("rollout-parent.jsonl");
        let child = day.join("rollout-child.jsonl");
        let chunks = [
            r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn","arguments":"{\"task_name\":\"review\",\"message\":\"encrypted\"}"}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn","output":"{\"task_name\":\"/root/review\"}"}}"#,
            r#"{"type":"response_item","payload":{"type":"agent_message","author":"/root/review","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nPASS"}]}}"#,
        ];
        std::fs::write(&parent, "").unwrap();
        let graph = SessionGraph::from_backend(Box::new(
            crate::codex_discover::CodexSessionGraph::open_in(&sessions, &parent),
        ));
        let mut follower = FollowParser::open_with_graph(Agent::Codex, &parent, graph.clone());

        for (index, chunk) in chunks.iter().enumerate() {
            writeln!(
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&parent)
                    .unwrap(),
                "{chunk}"
            )
            .unwrap();
            if index == 1 {
                let child_meta = serde_json::json!({
                    "type": "session_meta",
                    "payload": {
                        "id": "child",
                        "cwd": "/repo",
                        "source": {
                            "subagent": {
                                "thread_spawn": {
                                    "parent_thread_id": "parent",
                                    "agent_path": "/root/review",
                                    "agent_nickname": "Hume"
                                }
                            }
                        },
                        "agent_path": "/root/review",
                        "agent_nickname": "Hume"
                    }
                });
                std::fs::write(&child, format!("{child_meta}\n")).unwrap();
            }

            let (follow_blocks, follow_times, follow_metrics) =
                follower.poll().unwrap().expect("parent advanced");
            let batch = parse_session_with_graph(Agent::Codex, &parent, graph.clone()).unwrap();
            assert_eq!(
                format!("{follow_blocks:?}"),
                format!("{:?}", batch.blocks),
                "blocks differ after chunk {index}"
            );
            assert_eq!(follow_times, batch.user_times);
            assert_eq!(follow_metrics, batch.metrics);
        }

        let final_blocks = parse_session_with_graph(Agent::Codex, &parent, graph.clone())
            .unwrap()
            .blocks;
        assert!(
            matches!(
                &final_blocks[0],
                crate::Block::SubAgent(agent)
                    if agent.agent_id == "child"
                        && agent.agent_type == "Hume"
                        && agent.status == crate::AgentStatus::Completed
            ),
            "{final_blocks:#?}"
        );
        assert_eq!(graph.subagent_source(&parent, "child"), Some(child));
        std::fs::remove_dir_all(root).ok();
    }
}
