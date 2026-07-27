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
use crate::model::{Block, EpochSeconds};
use crate::Agent;
use serde_json::Value;

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

    /// Fold `lines` into the running state: decode each line to canonical messages, `apply` them
    /// to the replayer, and push the raw line's usage into the metrics accumulator. The single
    /// per-line loop shared by the batch parse and the live follower. Reuses one `delta` buffer,
    /// cleared per line.
    pub fn advance(&mut self, lines: &[String]) {
        let mut delta: Vec<Message> = Vec::new();
        for line in lines {
            delta.clear();
            self.adapter.decode_line(line, &mut self.cwd, &mut delta);
            self.replayer.apply(&delta);
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                self.metrics.push(&v);
            }
        }
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
        Session {
            agent: self.agent,
            cwd: None,
            blocks,
            user_times,
            metrics,
            index,
        }
    }
}
