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
use crate::engine::elide::Elision;
use crate::engine::meta_stream::{window_at, FOLD_VERSION};
use crate::engine::reader::{LineSource, TornTail};
use crate::metrics::{Metrics, TokenCounts};
use crate::model::EpochSeconds;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufReader, Seek, SeekFrom};
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
    /// The [`FOLD_VERSION`] the parked state was folded under (#22) — the version guard
    /// lives HERE, beside the CRC guard, so no consumer re-implements it. `serde(default)`
    /// makes a legacy cursor (written before the field existed) read as version 0, which
    /// rejects as [`CursorReject::StaleFoldVersion`] — the correct verdict for it.
    #[serde(default)]
    fold: u16,
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
/// means a cold fold, not a failure. The variants are DISTINCT FACTS a consumer may key
/// durable decisions on — in particular, only [`SourceRewritten`](Self::SourceRewritten)
/// says anything about the file (#22: a consumer inferring "rewritten" from "did not
/// resume" advanced a per-transcript epoch counter on transcripts nothing had happened
/// to, values that could not be re-derived after the fact).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorReject {
    /// No cursor was offered.
    NoCursor,
    /// The cursor's state was folded under a different [`FOLD_VERSION`]: the bytes are
    /// intact, but the rules that produced the parked totals are not the current ones.
    /// A cold fold, but NOT a statement about the file. (A cursor from a NEWER version —
    /// a downgraded binary — lands here too: its state is equally not this fold's.)
    StaleFoldVersion,
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
    /// The one bounded line source (#193): `Stop` — the durable cursor must never pass a
    /// torn line — and aggressive elision, provably metric-neutral.
    src: LineSource<BufReader<std::fs::File>>,
    /// Gauges already banked into `acc.extra` — the next [`Self::next_event`] banks the delta.
    banked: crate::engine::reader::ElisionCounts,
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
            // Version before CRC: it is free (no IO), and the verdicts are different
            // facts — a stale-version cursor says nothing about the file, so it must
            // never be reported as a rewrite (#22).
            Some(c) if c.fold != FOLD_VERSION => {
                (0, FoldStart::Cold(CursorReject::StaleFoldVersion))
            }
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
            src: LineSource::new(reader, offset, TornTail::Stop, Elision::Aggressive),
            banked: Default::default(),
            pre,
            acc,
            start,
        })
    }

    /// Bank the source's elision gauges into the accumulator's `extra` (#193 ③a), as deltas
    /// since the last bank — outside any event's before/after capture, so events report only
    /// their own line's movement while the cursor state still carries the totals.
    fn bank_elision(&mut self) {
        let c = self.src.elided;
        for (key, n) in [
            ("elided_lines", c.elided_lines - self.banked.elided_lines),
            ("elided_bytes", c.elided_bytes - self.banked.elided_bytes),
            ("skipped_lines", c.skipped_lines - self.banked.skipped_lines),
        ] {
            if n > 0 {
                self.acc.bump_extra(key, n);
            }
        }
        self.banked = c;
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
        self.bank_elision();
        loop {
            let Some((_, body)) = self.src.next()? else {
                // Torn tail or EOF: bank whatever the tail's reads counted, then reposition
                // the reader on the cursor (discovering a torn line consumed its bytes), so
                // the next poll re-reads it whole — the same seek this loop did inline.
                self.bank_elision();
                self.src.rewind_to_cursor()?;
                return Ok(None);
            };
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
            offset: self.src.offset(),
            window: window_at(&self.path, self.src.offset())?,
            fold: FOLD_VERSION,
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
