//! **The single incremental fold orchestrator.** One `SessionBuilder` threads everything the
//! parse paths used to thread by hand — the agent's L1 `decode_line` (with its `cwd`), the
//! shared L2 [`Replayer`] fold, and the per-agent metrics accumulator — behind one `advance`.
//!
//! Incremental parsing is first-class; one-shot is derived: the whole-file batch parse
//! ([`parse_session_as`](crate::parse_session_as) /
//! [`parse_path_timed_for`](crate::engine::replay::parse_path_timed_for)) feeds this builder
//! line-by-line (one line resident, no whole-file `Vec<String>`), and the live
//! [`FollowParser`](crate::FollowParser) feeds it the appended lines each poll — so batch and
//! live share exactly one line loop. The output is proven byte-identical to the retired
//! whole-file oracles (the equivalence gates in `claude_model`/`codex_model`) and to a full
//! re-parse (the `follow_*` tests).

use crate::adapter::{adapter, MetricsAccumulator, TranscriptAdapter};
use crate::engine::message::Message;
use crate::engine::replay::Replayer;
use crate::engine::session::Session;
use crate::engine::SessionIndex;
use crate::metrics::Metrics;
use crate::model::{AttachmentContent, Block, ByteOffset, EpochSeconds};
use crate::Agent;
use serde_json::Value;
use std::io;

/// Folds a transcript incrementally: `advance` a batch of lines, `fold`/`snapshot` the current
/// state, `reset` on a truncation/rewrite. Everything agent-specific — the L1 decoder, the L2
/// `Shaping`, the metrics accumulator — comes from the agent's `TranscriptAdapter`, so the
/// builder itself is agent-agnostic.
pub struct SessionBuilder {
    agent: Agent,
    adapter: &'static dyn TranscriptAdapter,
    replayer: Replayer<'static>,
    /// The FOLD cwd — threaded across lines by `decode_line` for path relativization.
    cwd: String,
    metrics: Box<dyn MetricsAccumulator>,
}

impl SessionBuilder {
    /// A fresh builder for `agent`: empty replayer/cwd/metrics, ready to `advance`.
    pub fn new(agent: Agent) -> Self {
        let adapter = adapter(agent);
        Self {
            agent,
            adapter,
            replayer: Replayer::new(adapter.shaping()),
            cwd: String::new(),
            metrics: adapter.metrics_acc(),
        }
    }

    /// Fold ONE line into the running state, knowing its **start byte offset** in the transcript:
    /// decode it to canonical messages, stamp that offset into any content-bearing attachment's
    /// [`Deferred`](AttachmentContent::Deferred) locator (so the bytes can be re-loaded on
    /// demand), `apply` the messages to the replayer, and push the raw line's usage into metrics.
    /// The single per-line unit shared by the batch parse ([`advance_reader`](Self::advance_reader))
    /// and the live follower.
    pub fn advance_at(&mut self, offset: ByteOffset, line: &str) {
        let mut delta: Vec<Message> = Vec::new();
        self.adapter.decode_line(line, &mut self.cwd, &mut delta);
        // Stamp `offset` (and a per-line ordinal) onto every content-bearing attachment this
        // line produced — the builder is the level that knows the byte offset; the L1 decoder
        // left the locators as placeholders. Path-only (`None`) attachments are untouched.
        let mut index = 0usize;
        for m in &mut delta {
            if let Message::Attachment(a) = m {
                if let AttachmentContent::Deferred { at, index: ix } = &mut a.content {
                    *at = offset;
                    *ix = index;
                    index += 1;
                }
            }
        }
        self.replayer.apply(&delta);
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            self.metrics.push(&v);
        }
    }

    /// Fold a whole transcript `reader` line-by-line, tracking each line's start byte offset so
    /// attachment locators are stamped correctly. One line resident at a time (no whole-file
    /// `Vec<String>`), so a multi-gigabyte transcript never balloons into memory. The batch
    /// parse entry points feed the builder through this; the live follower uses
    /// [`advance_at`](Self::advance_at) directly with the reader's per-line offsets.
    pub fn advance_reader(&mut self, reader: &mut dyn io::BufRead) -> io::Result<()> {
        let mut offset: ByteOffset = 0;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 {
                break;
            }
            let start = offset;
            offset += n as ByteOffset;
            // Match `BufRead::lines()`: strip a trailing `\n` (and a paired `\r`). `decode_line`
            // trims anyway, but this keeps the fed line byte-for-byte what the old loop produced.
            let line = buf
                .strip_suffix('\n')
                .map(|s| s.strip_suffix('\r').unwrap_or(s))
                .unwrap_or(&buf);
            self.advance_at(start, line);
        }
        Ok(())
    }

    /// Rebuild from scratch — recreate the replayer, clear the cwd, and take a fresh metrics
    /// accumulator. The live follower calls this on a truncation/compaction (the reader re-read
    /// from 0, so the next `advance` folds the whole new file).
    pub(crate) fn reset(&mut self) {
        self.replayer = Replayer::new(self.adapter.shaping());
        self.cwd.clear();
        self.metrics = self.adapter.metrics_acc();
    }

    /// The current presentable blocks + per-turn times + folded metrics, WITHOUT consuming the
    /// builder (so the follower can `advance` a delta, `fold` to render, then keep folding).
    /// Same output as a full whole-file parse.
    pub(crate) fn fold(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>, Metrics) {
        let (blocks, times) = self.replayer.snapshot();
        (blocks, times, self.metrics.finish())
    }

    /// The current state as a [`Session`] (blocks + per-turn times + metrics + derived index),
    /// with `cwd` left `None` — the batch entry fills it from the transcript path.
    pub fn snapshot(&self) -> Session {
        let (blocks, user_times, metrics) = self.fold();
        let index = SessionIndex::build(&blocks, &user_times);
        // Post-pass over the finished blocks (the fold is untouched): the sub-agent entity map.
        // `transcript` stays None here — the path-aware parse fills it (it alone knows the path).
        let sub_agents = crate::engine::session::build_sub_agents(&blocks);
        Session {
            agent: self.agent,
            cwd: None,
            blocks,
            user_times,
            metrics,
            index,
            sub_agents,
        }
    }
}
