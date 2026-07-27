//! Incremental live follower (M16): fold only the newly-appended lines through a persistent
//! `Replayer`, instead of re-parsing the whole file each poll. The `snapshot` is proven
//! byte-identical to a full re-parse (`follow_matches_full_reparse`), at O(delta) work per
//! poll — no whole-file re-read/re-decode. On a truncation/rewrite (compaction) the tail
//! resets and the Replayer rebuilds from scratch (a full replay of the new content).

use std::path::Path;

use crate::engine::builder::SessionAccumulator;
use crate::engine::session::{InMemoryStore, Session};
use crate::metrics::Metrics;
use crate::model::{Block, EpochSeconds};
use crate::reader::LineReader;
use crate::Agent;

/// Follows a transcript file, folding only newly-appended lines each poll through a shared
/// [`SessionAccumulator`]. Everything agent-specific — the L1 decoder, the L2 `Shaping`, the
/// metrics accumulator — lives in the accumulator, so the follower itself is agent-agnostic and
/// is just the byte-offset reader plus the same incremental fold the batch parse uses.
pub struct FollowParser {
    builder: SessionAccumulator<InMemoryStore>,
    reader: LineReader,
}

impl FollowParser {
    /// Follow `path` from the beginning: the first `poll` folds the whole current file, then
    /// subsequent polls fold only appends.
    pub fn open(agent: Agent, path: &Path) -> Self {
        Self {
            builder: SessionAccumulator::new(agent),
            reader: LineReader::open_at_start(path),
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
        Ok(self.advance_from_source()?.then(|| self.builder.fold()))
    }

    /// Poll and return the current state as an owned [`Session`] (blocks + per-turn times +
    /// metrics + derived index + sub-agent map), `cwd` left `None` — the caller fills it from
    /// the source path, like [`parse_session_as`](crate::parse_session_as) does. Returns `None`
    /// when the source hasn't grown since the last poll (idle). Used by the residency cache.
    pub fn poll_session(&mut self) -> std::io::Result<Option<Session>> {
        Ok(self.advance_from_source()?.then(|| self.builder.snapshot()))
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
}
