//! **The cost ledger**: every session's equivalent-API cost, fold-free and machine-wide.
//!
//! The rail used to read cost from the durable entry's meta stream — which exists only for
//! sessions someone VISITED (§3: the entry is written by serving, never by a sweep). Cost
//! gated on visits under-reported brutally: on one audited project the rail showed $121 of
//! an actual $2,421 (5%), because 551 of 556 sessions had never been opened.
//!
//! This ledger prices transcripts DIRECTLY, through the engine's resumable metrics fold
//! (#14, `MetricsFold`): a bounded line scan that folds only `token_count`-class records —
//! no blocks, no shaping, no durable entry — and stops at a serializable cursor. That is
//! why this does not violate R7 ("no fold on the index path"): R7 bans the BLOCK fold,
//! whose cost is proportional to a transcript's full content; the metrics fold reads each
//! byte once ever, and a cursor makes every later cycle's cost proportional to what was
//! APPENDED. Measured cold: 837 files / 1.45 GB in ~3 s — and the per-cycle byte budget
//! below spreads even that over a few scan cycles rather than stalling the first paint.
//!
//! Pricing goes through the adapter's accumulator `finish()`, so the attribution rules
//! live in ONE place: usage a rollout reports before naming a model is claimed by the
//! first model named (#16) — the monitor no longer re-derives cost from raw per-model
//! maps with its own, subtly different rules (the blank-model bucket priced as $0).
//!
//! Cursors persist at the monitor's OWN root (`<cache_root>/costs/<stem>.json` — R5), so
//! a restart resumes instead of re-reading the store.

use claude_replay_core::{adapter, Agent, MetricsCursor, MetricsFold};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Fresh bytes folded per SCAN CYCLE, across all sessions. A cold start on a year of
/// rollouts must not stall the first `/api/sessions` for seconds; under this budget the
/// rail paints immediately and costs stream in over the next few polls. Steady-state
/// appends are a few KiB and never feel the cap.
pub(crate) const COST_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// The persisted entry's format version — bumped when the JSON shape changes. A mismatch
/// is a cold re-fold, never an error (a ledger entry is a cache).
const LEDGER_VERSION: u32 = 1;

/// One transcript's priced state: what the fold said last time, and where it stopped.
#[derive(Clone)]
struct Entry {
    /// Transcript size and mtime at the last fold — the no-op fast path: a file that
    /// did not move costs one `stat`.
    len: u64,
    mtime: Option<u64>,
    cost: Option<f64>,
    /// Some models in the mix were unpriced — the cost is a `≥` lower bound.
    partial: bool,
    cursor: Option<MetricsCursor>,
}

pub(crate) struct CostLedger {
    /// `<cache_root>/costs` — the monitor's own root (R5), beside `html/` and
    /// `ignored.json`.
    dir: PathBuf,
    entries: HashMap<String, Entry>,
    /// Stems whose disk entry was already looked for — a missing file must not be
    /// re-stat'ed every cycle.
    probed: std::collections::HashSet<String>,
}

impl CostLedger {
    pub(crate) fn new(cache_root: &Path) -> Self {
        Self {
            dir: cache_root.join("costs"),
            entries: HashMap::new(),
            probed: std::collections::HashSet::new(),
        }
    }

    /// The equivalent-API cost of the transcript at `path`, `(cost, partial)`, folded
    /// incrementally. `budget` is the cycle's remaining fresh-byte allowance: a fold that
    /// would read more than is left is deferred (the cached value — possibly stale,
    /// possibly `None` — is returned, and a later cycle picks it up). One fold may
    /// overshoot the remainder rather than starve forever behind one huge file.
    pub(crate) fn cost(
        &mut self,
        agent: Agent,
        path: &Path,
        budget: &mut u64,
    ) -> Option<(f64, bool)> {
        let stem = stem_of(path);
        if !self.entries.contains_key(&stem) && self.probed.insert(stem.clone()) {
            if let Some(e) = load_entry(&self.dir.join(format!("{stem}.json"))) {
                self.entries.insert(stem.clone(), e);
            }
        }
        let meta = std::fs::metadata(path).ok()?;
        let (len, mtime) = (meta.len(), epoch(meta.modified().ok()));
        if let Some(e) = self.entries.get(&stem) {
            if e.len == len && e.mtime == mtime {
                return e.cost.map(|c| (c, e.partial));
            }
        }
        let cached = self.entries.get(&stem);
        let fresh = len.saturating_sub(
            cached
                .and_then(|e| e.cursor.as_ref())
                .map_or(0, |c| c.offset.min(len)),
        );
        if *budget == 0 || (fresh > *budget && fresh > COST_BUDGET_BYTES / 8) {
            // Out of allowance this cycle — answer from the cache, fold later. (A small
            // overshoot is allowed so one giant file cannot monopolize every cycle's
            // budget without ever finishing.)
            return cached.and_then(|e| e.cost.map(|c| (c, e.partial)));
        }
        *budget = budget.saturating_sub(fresh);

        let cursor = cached.and_then(|e| e.cursor.clone());
        let Ok(mut fold) = MetricsFold::open(adapter(agent), path, cursor.as_ref()) else {
            return cached.and_then(|e| e.cost.map(|c| (c, e.partial)));
        };
        while let Ok(Some(_)) = fold.next_event() {}
        let m = fold.metrics();
        let entry = Entry {
            len,
            mtime,
            cost: m.cost_usd,
            partial: m.cost_partial,
            cursor: fold.cursor().ok(),
        };
        save_entry(&self.dir.join(format!("{stem}.json")), &entry);
        let out = entry.cost.map(|c| (c, entry.partial));
        self.entries.insert(stem, entry);
        out
    }
}

fn epoch(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn load_entry(path: &Path) -> Option<Entry> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if v.get("v").and_then(Value::as_u64) != Some(u64::from(LEDGER_VERSION)) {
        return None;
    }
    Some(Entry {
        len: v.get("len").and_then(Value::as_u64)?,
        mtime: v.get("mtime").and_then(Value::as_u64),
        cost: v.get("cost").and_then(Value::as_f64),
        partial: v.get("partial").and_then(Value::as_bool).unwrap_or(false),
        cursor: v
            .get("cursor")
            .and_then(|c| serde_json::from_value(c.clone()).ok()),
    })
}

/// Best-effort persistence: a write failure leaves the in-memory entry authoritative for
/// this run — the next cold start re-folds, which is correct, just slower.
fn save_entry(path: &Path, e: &Entry) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let v = serde_json::json!({
        "v": LEDGER_VERSION,
        "len": e.len,
        "mtime": e.mtime,
        "cost": e.cost,
        "partial": e.partial,
        "cursor": e.cursor.as_ref().and_then(|c| serde_json::to_value(c).ok()),
    });
    let _ = std::fs::write(path, v.to_string());
}

/// A transcript's ledger key — its file stem, the same key the index rows use.
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cm-cost-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A Codex `token_count` line carrying CUMULATIVE usage.
    fn token_count(ts: &str, input: u64, cached: u64, out: u64) -> String {
        format!(
            "{{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"total_token_usage\":{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},\"output_tokens\":{out}}}}}}}}}\n"
        )
    }

    fn turn_context(model: &str) -> String {
        format!(
            "{{\"timestamp\":\"2026-08-12T01:00:00Z\",\"type\":\"turn_context\",\"payload\":{{\"model\":\"{model}\"}}}}\n"
        )
    }

    /// Priced without any visit (no durable entry anywhere near this test), resumed
    /// incrementally on append, and served from the mtime/len fast path when quiet.
    #[test]
    fn prices_grows_and_resumes_without_a_visit() {
        let d = scratch("fold");
        let t = d.join("rollout-2026-08-12T01-00-00-fold-test.jsonl");
        let mut f = std::fs::File::create(&t).unwrap();
        write!(f, "{}", turn_context("gpt-5.6")).unwrap();
        write!(
            f,
            "{}",
            token_count("2026-08-12T01:00:01Z", 1_000_000, 0, 0)
        )
        .unwrap();
        drop(f);

        let mut ledger = CostLedger::new(&d.join("cache"));
        let mut budget = COST_BUDGET_BYTES;
        let (c1, partial) = ledger.cost(Agent::CODEX, &t, &mut budget).expect("priced");
        assert!((c1 - 1.25).abs() < 1e-9, "1M input on gpt-5: {c1}");
        assert!(!partial);
        assert!(budget < COST_BUDGET_BYTES, "fresh bytes were charged");

        // Quiet file: the fast path answers, spending no budget.
        let mut b2 = COST_BUDGET_BYTES;
        assert_eq!(ledger.cost(Agent::CODEX, &t, &mut b2), Some((c1, false)));
        assert_eq!(
            b2, COST_BUDGET_BYTES,
            "a quiet file costs a stat, not bytes"
        );

        // Growth: cumulative usage advances; a FRESH ledger (as after a restart) must
        // resume from the persisted cursor and still price the WHOLE file.
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        write!(
            f,
            "{}",
            token_count("2026-08-12T01:01:00Z", 2_000_000, 0, 0)
        )
        .unwrap();
        drop(f);
        let mut restarted = CostLedger::new(&d.join("cache"));
        let mut b3 = COST_BUDGET_BYTES;
        let (c2, _) = restarted
            .cost(Agent::CODEX, &t, &mut b3)
            .expect("still priced");
        assert!((c2 - 2.50).abs() < 1e-9, "2M cumulative input: {c2}");
        // The resumed fold read only the appended line, not the whole file.
        let spent = COST_BUDGET_BYTES - b3;
        assert!(
            spent < std::fs::metadata(&t).unwrap().len(),
            "resume folds the delta, not the file: spent {spent}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An exhausted budget defers the fold and answers from the cache — `None` before any
    /// fold ever ran, the stale value after one did.
    #[test]
    fn an_exhausted_budget_defers_and_serves_the_cache() {
        let d = scratch("budget");
        let t = d.join("rollout-2026-08-12T01-00-00-budget-test.jsonl");
        std::fs::write(
            &t,
            format!(
                "{}{}",
                turn_context("gpt-5.6"),
                token_count("2026-08-12T01:00:01Z", 1_000_000, 0, 0)
            ),
        )
        .unwrap();
        let mut ledger = CostLedger::new(&d.join("cache"));
        let mut none = 0u64;
        assert_eq!(
            ledger.cost(Agent::CODEX, &t, &mut none),
            None,
            "no budget, never folded: honestly unknown"
        );
        let mut full = COST_BUDGET_BYTES;
        let priced = ledger.cost(Agent::CODEX, &t, &mut full);
        assert!(priced.is_some());
        // Grown file + zero budget: the STALE price, not a stall and not None.
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        write!(
            f,
            "{}",
            token_count("2026-08-12T01:02:00Z", 9_000_000, 0, 0)
        )
        .unwrap();
        drop(f);
        let mut none2 = 0u64;
        assert_eq!(
            ledger.cost(Agent::CODEX, &t, &mut none2),
            priced,
            "deferred fold serves the cached value"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
