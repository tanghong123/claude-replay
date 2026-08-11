//! **The resumable metrics fold** (#14): fold a transcript's token/cost metrics from a byte
//! offset, not from zero — for a PERIODIC collector (no resident daemon) that wants each run to
//! fold only what changed since the last one and exit.
//!
//! The shape mirrors the durable cache's resume, deliberately: an opaque serializable
//! [`MetricsCursor`] pairs a byte offset with the same CRC window
//! [`Resume`](crate::engine::meta_stream::Resume) uses — proving the prefix being skipped is
//! the prefix already folded —
//! plus the adapter's own fold state, captured through
//! [`MetricsAccumulator::state`] and the
//! preprocessor's. Private state stays private: the consumer stores the cursor and hands it
//! back, never interprets it.
//!
//! [`MetricsFold::next_event`] also owns the raw-line reading recipe — blank lines are nothing,
//! a torn final line is a write in progress (left UNCONSUMED, so the next run re-reads it),
//! [`PreprocessedLine::Ignore`] gates embedded
//! records — so a consumer gets per-event deltas without copying `parse_reader`'s rules.

use crate::adapter::{LinePreprocessor, MetricsAccumulator, PreprocessedLine, TranscriptAdapter};
use crate::engine::meta_stream::window_at;
use crate::metrics::{Metrics, TokenCounts};
use crate::model::EpochSeconds;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Where a metrics fold stopped, serializable — store it anywhere, hand it back to
/// [`MetricsFold::open`] to continue from there. Opaque on purpose: `acc`/`pre` are the
/// adapter's own state (`Null` for adapters with none), and interpreting them is the
/// adapter's business alone.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetricsCursor {
    /// The byte offset folding stopped at — always a line boundary.
    pub offset: u64,
    /// CRC32 of the window below `offset` (see [`window_at`]) — the proof that the skipped
    /// prefix is the prefix this cursor's state was folded from.
    pub window: u32,
    acc: Value,
    pre: Value,
}

/// How [`MetricsFold::open`] started — diagnosable, like the durable cache's `Origin`,
/// because "the cursor did not help, and here is why" is a support answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldStart {
    /// The cursor validated; folding continues from its offset.
    Resumed,
    /// Folding restarted from byte 0.
    Cold(CursorReject),
}

/// Why a cursor was not honored. Never an error: a cursor is a cache, and an unusable one
/// means a cold fold, not a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorReject {
    /// No cursor was offered.
    NoCursor,
    /// The bytes below the cursor's offset are not the bytes it was folded from — the
    /// transcript was truncated or rewritten. (A shorter file is also this: the offset
    /// lying past EOF is the plainest form of "that prefix is gone".)
    SourceRewritten,
}

/// One metrics-bearing line, as a **delta**: what this event added, per model, plus any
/// change to the agent's extension-bag counters. Lines that move no counter produce no
/// event. Downstream time-bucketing consumes these directly instead of diffing
/// [`totals`](crate::adapter::MetricsAccumulator::totals) snapshots itself.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsEvent {
    /// The line's own timestamp, when it carries one.
    pub ts: Option<EpochSeconds>,
    /// Tokens this event added, keyed by model — only models whose counts moved.
    pub tokens: BTreeMap<String, TokenCounts>,
    /// Extension-bag counters this event moved (e.g. a skipped-record diagnostic), as deltas.
    pub extra: BTreeMap<String, u64>,
}

/// A metrics fold over one transcript, resumable at a [`MetricsCursor`].
pub struct MetricsFold {
    path: PathBuf,
    reader: BufReader<std::fs::File>,
    offset: u64,
    pre: Box<dyn LinePreprocessor>,
    acc: Box<dyn MetricsAccumulator>,
    start: FoldStart,
}

impl MetricsFold {
    /// Open a fold at `cursor`, or cold at byte 0 when the cursor is absent or no longer
    /// describes this file. Validation before trust, exactly as the durable resume does it:
    /// the offset must still be reachable AND the window below it must hash to what the
    /// cursor recorded — otherwise the state the cursor carries was folded from bytes that
    /// no longer exist, and continuing would silently double- or mis-count.
    pub fn open(
        adapter: &dyn TranscriptAdapter,
        path: &Path,
        cursor: Option<&MetricsCursor>,
    ) -> std::io::Result<Self> {
        let mut pre = adapter.line_preprocessor();
        let mut acc = adapter.metrics_acc();
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        let mut reader = BufReader::new(file);

        let (offset, start) = match cursor {
            None => (0, FoldStart::Cold(CursorReject::NoCursor)),
            Some(c) if c.offset <= len && window_at(path, c.offset)? == c.window => {
                pre.restore(&c.pre);
                acc.restore(&c.acc);
                (c.offset, FoldStart::Resumed)
            }
            Some(_) => (0, FoldStart::Cold(CursorReject::SourceRewritten)),
        };
        reader.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            path: path.to_path_buf(),
            reader,
            offset,
            pre,
            acc,
            start,
        })
    }

    /// How this fold started — [`Resumed`](FoldStart::Resumed), or cold and why.
    pub fn start(&self) -> FoldStart {
        self.start
    }

    /// The next metrics-bearing event, or `None` at the end of the *complete* lines.
    ///
    /// A final line without its newline is a write in progress — it is left **unconsumed**
    /// (the reader rewinds to the line's start), so [`cursor`](Self::cursor) never points
    /// past it and the next run re-reads it whole. Blank lines and
    /// [`Ignore`](PreprocessedLine::Ignore)d records are consumed silently; a malformed
    /// complete line is consumed and counted through
    /// [`malformed_line`](MetricsAccumulator::malformed_line), which surfaces here as an
    /// `extra` delta when the adapter records such diagnostics.
    pub fn next_event(&mut self) -> std::io::Result<Option<MetricsEvent>> {
        let mut line = String::new();
        loop {
            let at = self.offset;
            line.clear();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                return Ok(None);
            }
            if !line.ends_with('\n') {
                // Torn tail: rewind so the cursor stays at the last complete line.
                self.reader.seek(SeekFrom::Start(at))?;
                return Ok(None);
            }
            self.offset += n as u64;
            let body = line.trim_end();
            if body.is_empty() {
                continue;
            }
            if matches!(self.pre.process(body), PreprocessedLine::Ignore) {
                continue;
            }
            let (tokens_before, extra_before, _) = self.acc.totals();
            let ts = match serde_json::from_str::<Value>(body) {
                Ok(v) => {
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(crate::metrics::parse_ts)
                        .map(|secs| secs as EpochSeconds);
                    self.acc.push(&v);
                    ts
                }
                Err(_) => {
                    self.acc.malformed_line();
                    None
                }
            };
            let (tokens_after, extra_after, _) = self.acc.totals();
            let tokens = diff_tokens(&tokens_before, &tokens_after);
            let extra = diff_extra(&extra_before, &extra_after);
            if !tokens.is_empty() || !extra.is_empty() {
                return Ok(Some(MetricsEvent { ts, tokens, extra }));
            }
        }
    }

    /// The metrics folded so far — the same [`Metrics`] a whole-file parse produces once
    /// every event has been drained.
    pub fn metrics(&self) -> Metrics {
        self.acc.finish()
    }

    /// Where this fold stands, as a value the next run hands back to [`open`](Self::open).
    /// Always at a line boundary (see [`next_event`](Self::next_event) on torn tails).
    pub fn cursor(&self) -> std::io::Result<MetricsCursor> {
        Ok(MetricsCursor {
            offset: self.offset,
            window: window_at(&self.path, self.offset)?,
            acc: self.acc.state(),
            pre: self.pre.state(),
        })
    }
}

/// Per-model token deltas — only models whose counts moved. Counts are monotone within a
/// fold (accumulators only add), so plain subtraction is exact.
fn diff_tokens(
    before: &BTreeMap<String, TokenCounts>,
    after: &BTreeMap<String, TokenCounts>,
) -> BTreeMap<String, TokenCounts> {
    let mut out = BTreeMap::new();
    for (model, a) in after {
        let b = before.get(model).copied().unwrap_or_default();
        let d = TokenCounts {
            input: a.input.saturating_sub(b.input),
            cache_creation: a.cache_creation.saturating_sub(b.cache_creation),
            cache_read: a.cache_read.saturating_sub(b.cache_read),
            output: a.output.saturating_sub(b.output),
        };
        if d != TokenCounts::default() {
            out.insert(model.clone(), d);
        }
    }
    out
}

fn diff_extra(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for (key, a) in after {
        let b = before.get(key).copied().unwrap_or_default();
        if *a > b {
            out.insert(key.clone(), a - b);
        }
    }
    out
}
