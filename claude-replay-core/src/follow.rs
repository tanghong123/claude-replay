//! Incremental live follower (M16): fold only the newly-appended lines through a persistent
//! `Replayer`, instead of re-parsing the whole file each poll. The `snapshot` is proven
//! byte-identical to a full re-parse (`follow_matches_full_reparse`), at O(delta) work per
//! poll — no whole-file re-read/re-decode. On a truncation/rewrite (compaction) the tail
//! resets and the Replayer rebuilds from scratch (a full replay of the new content).

use std::path::{Path, PathBuf};

use crate::engine::builder::SessionAccumulator;
use crate::engine::session::{InMemoryStore, Session};
use crate::metrics::Metrics;
use crate::model::{Block, EpochSeconds};
use crate::reader::LineReader;
use crate::{Agent, SessionGraph};

/// Follows a transcript file, folding only newly-appended lines each poll through a shared
/// [`SessionAccumulator`]. Everything agent-specific — the L1 decoder, the L2 `Shaping`, the
/// metrics accumulator — lives in the accumulator, so the follower itself is agent-agnostic and
/// is just the byte-offset reader plus the same incremental fold the batch parse uses.
pub struct FollowParser {
    path: PathBuf,
    builder: SessionAccumulator<InMemoryStore>,
    reader: LineReader,
    graph: SessionGraph,
}

impl FollowParser {
    /// Follow `path` from the beginning: the first `poll` folds the whole current file, then
    /// subsequent polls fold only appends.
    pub fn open(agent: Agent, path: &Path) -> Self {
        Self::open_with_graph(agent, path, SessionGraph::open(agent, path))
    }

    /// Follow a related transcript with the operation-scoped relationship resolver shared by
    /// its owning [`Transcript`](crate::Transcript).
    pub(crate) fn open_with_graph(agent: Agent, path: &Path, graph: SessionGraph) -> Self {
        Self {
            path: path.to_path_buf(),
            builder: SessionAccumulator::new(agent),
            reader: LineReader::open_at_start(path),
            graph,
        }
    }

    /// Fold any newly-appended lines (or rebuild on a truncation/rewrite) into the running
    /// builder. Returns `Ok(true)` when content advanced this tick, `Ok(false)` when nothing
    /// changed since the last poll (the common idle tick). O(delta) except on a rewrite.
    fn advance_from_source(&mut self) -> std::io::Result<bool> {
        let p = self.reader.poll()?;
        if !p.reset && p.lines.is_empty() {
            return Ok(false); // nothing new this tick
        }
        if p.reset {
            // Truncation / compaction: the kept prefix changed. Rebuild from scratch — the
            // LineReader re-read from 0, so `p.lines` is the whole new file.
            self.builder.reset();
        }
        // Fold each appended line with its file start offset, so attachment locators in a LIVE
        // session get correct byte offsets (same as a batch parse).
        for (offset, line) in p.offsets.iter().zip(&p.lines) {
            self.builder.advance_at(*offset, line);
        }
        Ok(true)
    }

    /// Poll: fold any newly-appended lines and return the current blocks + per-turn times +
    /// metrics. Returns `None` when nothing changed since the last poll (the common idle tick).
    #[allow(clippy::type_complexity)]
    pub fn poll(
        &mut self,
    ) -> std::io::Result<Option<(Vec<Block>, Vec<Option<EpochSeconds>>, Metrics)>> {
        if !self.advance_from_source()? {
            return Ok(None);
        }
        let session = self.snapshot_session();
        Ok(Some((
            session.blocks(),
            session.user_times,
            session.metrics,
        )))
    }

    /// Poll and return the current state as a **fully-assembled** owned [`Session`] — blocks +
    /// per-turn times + metrics + derived index + sub-agent map, with `cwd` and each sub-agent's
    /// `transcript` filled from the source path (exactly as
    /// [`parse_session_as`](crate::parse_session_as) does). Returns `None` when the source hasn't
    /// grown since the last poll (idle). This is the residency cache's single assembly point — it
    /// needs no core internals.
    pub fn poll_session(&mut self) -> std::io::Result<Option<Session>> {
        if !self.advance_from_source()? {
            return Ok(None);
        }
        Ok(Some(self.snapshot_session()))
    }

    fn snapshot_session(&mut self) -> Session {
        let mut session = self.builder.snapshot();
        session.cwd = crate::discover::session_cwd(&self.path);
        crate::engine::session::resolve_session_relationships(
            &mut session,
            &self.graph,
            &self.path,
        );
        session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
                format!("{:?}", s.blocks()),
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
            format!("{:?}", s.blocks()),
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
    fn follow_resolves_a_codex_child_created_after_the_spawn_poll() {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "cr-follow-codex-tree-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let sessions = root.join("sessions/2026/07/28");
        std::fs::create_dir_all(&sessions).unwrap();
        let parent = sessions.join("rollout-parent.jsonl");
        let child = sessions.join("rollout-child.jsonl");
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"late\",\"message\":\"hidden\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/late\"}"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let transcript = crate::Transcript::open(Agent::Codex, &parent);
        let mut follower = transcript.follow();
        let first = follower.poll_session().unwrap().expect("initial bytes");
        assert!(first.sub_agents.is_empty(), "child does not exist yet");

        std::fs::write(
            &child,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"child","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/late","agent_nickname":"Late"}}}}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"late child"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&parent)
            .unwrap()
            .write_all(
                concat!(
                    r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"still working"}]}}"#,
                    "\n"
                )
                .as_bytes(),
            )
            .unwrap();

        let second = follower.poll_session().unwrap().expect("parent advanced");
        let meta = second.sub_agents.get("child").expect("late child resolved");
        assert_eq!(meta.transcript.as_deref(), Some(child.as_path()));
        assert!(transcript.subagent("child").is_some());

        std::fs::remove_dir_all(root).unwrap();
    }
}
