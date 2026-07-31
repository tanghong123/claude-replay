//! **Claude's token/cost metrics adapter** — the Claude half of the shared `metrics`
//! engine (mirrors `codex_metrics`). Folds each Claude transcript line's `/message/usage`
//! into a running [`MetricsAcc`]; the agent-neutral [`Metrics`] value, pricing, and footer
//! formatting live in [`claude_replay_engine::seam`].

use claude_replay_engine::seam::{estimate_cost, parse_ts, Metrics, TimeSpan};
use serde_json::Value;

/// Claude's per-line token/cost accumulator, folded through the shared
/// `MetricsAccumulator` seam — so the streaming engine
/// (`model::parse_stream`, M10) folds metrics in the same pass that builds blocks, and the
/// metrics-only reader path (`metrics::parse_reader_for`) reuses it. `push` sums each line's
/// `/message/usage`; `finish` prices it.
#[derive(Default, Clone)]
pub(crate) struct MetricsAcc {
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
    model: String,
    span: TimeSpan,
    extra: std::collections::BTreeMap<String, u64>,
}

impl MetricsAcc {
    /// Fold an **agent-specific** metric into the accumulating [`Metrics::extra`] bag (sum by
    /// key). The seam for a Claude-only counter (e.g. `reasoning_tokens`): call this from
    /// [`push`](Self::push) when the relevant JSON key is seen. Nothing calls it yet — the
    /// interface is ready for the first such metric (task #22).
    #[allow(dead_code)]
    pub(crate) fn bump(&mut self, key: &str, n: u64) {
        *self.extra.entry(key.to_string()).or_default() += n;
    }

    pub(crate) fn push(&mut self, v: &Value) {
        let field = |u: &Value, k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        if let Some(u) = v.pointer("/message/usage") {
            // Three distinct buckets so the footer can tell them apart: new input,
            // cache writes (new content, cached on first sight), and cache reads
            // (the whole context re-read every turn — the dominant number, kept
            // separate so it doesn't drown out genuinely-new input).
            self.input += field(u, "input_tokens");
            self.cache_creation += field(u, "cache_creation_input_tokens");
            self.cache_read += field(u, "cache_read_input_tokens");
            self.output += field(u, "output_tokens");
        }
        if let Some(m) = v.pointer("/message/model").and_then(|x| x.as_str()) {
            self.model = m.to_string();
        }
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Some(secs) = parse_ts(ts) {
                self.span.observe(secs);
            }
        }
    }

    pub(crate) fn finish(self) -> Metrics {
        let duration_secs = self.span.duration_secs();
        let cost_usd = estimate_cost(
            &self.model,
            self.input,
            self.cache_creation,
            self.cache_read,
            self.output,
        );
        let mut m = Metrics::default();
        m.input_tokens = self.input;
        m.cache_creation_tokens = self.cache_creation;
        m.cache_read_tokens = self.cache_read;
        m.output_tokens = self.output;
        m.model = self.model;
        m.duration_secs = duration_secs;
        m.cost_usd = cost_usd;
        m.extra = self.extra;
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_engine::seam::{human_tokens, parse_reader_with};

    /// Metrics via the public reader dispatch — exercises the shared `MetricsAccumulator`
    /// default path with Claude's accumulator.
    fn parse_reader(jsonl: &str) -> Metrics {
        parse_reader_with(&crate::adapters::ClaudeAdapter, std::io::Cursor::new(jsonl))
    }

    /// The agent-specific extension seam: `bump` accumulates by key and `finish` emits the bag.
    /// (No production agent populates `extra` yet; this exercises the interface end-to-end.)
    #[test]
    fn bump_accumulates_agent_specific_metrics_into_extra() {
        let mut acc = MetricsAcc::default();
        acc.bump("reasoning_tokens", 30);
        acc.bump("web_searches", 1);
        acc.bump("reasoning_tokens", 12);
        let m = acc.finish();
        assert_eq!(m.extra.get("reasoning_tokens"), Some(&42)); // summed
        assert_eq!(m.extra.get("web_searches"), Some(&1));
        // Untouched by the default parse — `extra` stays empty when no agent bumps it.
        assert!(parse_reader("").extra.is_empty());
    }

    #[test]
    fn parses_tokens_model_duration_cost() {
        let jsonl = r#"
{"type":"assistant","timestamp":"2026-06-28T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":500}}}
{"type":"assistant","timestamp":"2026-06-28T10:02:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":2000,"output_tokens":1500}}}
"#;
        let m = parse_reader(jsonl);
        assert_eq!(m.input_tokens, 3000);
        assert_eq!(m.output_tokens, 2000);
        assert_eq!(m.duration_secs, 120);
        assert!(m.cost_usd.unwrap() > 0.0);
        let f = m.footer();
        assert!(f.contains("opus4.8"), "footer: {f}");
        assert!(f.contains("2m"), "footer: {f}");
        // No cache tokens → no "cached" tier in the footer.
        assert!(!f.contains("cached"), "footer: {f}");
    }

    #[test]
    fn sums_cache_tiers_and_shows_them() {
        let jsonl = r#"
{"type":"assistant","timestamp":"2026-06-28T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":1000,"cache_creation_input_tokens":40000,"cache_read_input_tokens":2000000,"output_tokens":5000}}}
{"type":"assistant","timestamp":"2026-06-28T10:01:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":500,"cache_creation_input_tokens":10000,"cache_read_input_tokens":3000000,"output_tokens":5000}}}
"#;
        let m = parse_reader(jsonl);
        assert_eq!(m.input_tokens, 1500);
        assert_eq!(m.cache_creation_tokens, 50000);
        assert_eq!(m.cache_read_tokens, 5000000);
        assert_eq!(m.output_tokens, 10000);
        let f = m.footer();
        // All three token tiers are present, cache reads dominating.
        assert!(f.contains("1.5k in"), "footer: {f}");
        assert!(f.contains("5.0M cached"), "footer: {f}");
        assert!(f.contains("10.0k out"), "footer: {f}");
        // Cached tokens can reach billions on long sessions.
        assert_eq!(human_tokens(2_728_200_000), "2.7B");
        // Cost prices reads (0.1×) and writes (1.25×) on top of new input.
        let c = m.cost_usd.unwrap();
        let expected =
            (1500.0 + 50000.0 * 1.25 + 5_000_000.0 * 0.10) / 1e6 * 15.0 + 10000.0 / 1e6 * 75.0;
        assert!((c - expected).abs() < 1e-9, "cost {c} vs {expected}");
    }
}
