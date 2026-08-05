//! **Claude's token/cost metrics adapter** — the Claude half of the shared `metrics`
//! engine (mirrors `codex_metrics`). Folds each Claude transcript line's `/message/usage`
//! into a running [`MetricsAcc`]; the agent-neutral [`Metrics`] value, pricing, and footer
//! formatting live in [`claude_replay_engine::seam`].

use claude_replay_engine::seam::{parse_ts, total_cost, Metrics, TimeSpan, TokenCounts};
use serde_json::Value;

/// Claude's per-line token/cost accumulator, folded through the shared
/// `MetricsAccumulator` seam — so the streaming engine
/// (`model::parse_stream`, M10) folds metrics in the same pass that builds blocks, and the
/// metrics-only reader path (`metrics::parse_reader_for`) reuses it. `push` sums each line's
/// `/message/usage`; `finish` prices it.
#[derive(Default, Clone)]
pub(crate) struct MetricsAcc {
    /// Tokens keyed by the model that produced them (#104). Claude reports `usage` per
    /// assistant message alongside `/message/model`, so attribution is free — it used to be
    /// discarded, which priced a whole multi-model session at the last model's rate.
    per_model: std::collections::BTreeMap<String, TokenCounts>,
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
        // The model THIS message ran on — not `self.model`, which is only the last seen.
        let m = v
            .pointer("/message/model")
            .and_then(|x| x.as_str())
            .unwrap_or(&self.model)
            .to_string();
        if let Some(u) = v.pointer("/message/usage") {
            let e = self.per_model.entry(m).or_default();
            // Three distinct buckets so the footer can tell them apart: new input,
            // cache writes (new content, cached on first sight), and cache reads
            // (the whole context re-read every turn — the dominant number, kept
            // separate so it doesn't drown out genuinely-new input).
            e.input += field(u, "input_tokens");
            e.cache_creation += field(u, "cache_creation_input_tokens");
            e.cache_read += field(u, "cache_read_input_tokens");
            e.output += field(u, "output_tokens");
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

    /// Running per-model totals, the counter bag and the observed span (#96 §7).
    pub(crate) fn totals(&self) -> claude_replay_engine::seam::MetricsTotals {
        (
            self.per_model.clone(),
            self.extra.clone(),
            self.span.endpoints().map(|(a, b)| (a as f64, b as f64)),
        )
    }

    /// Re-seed a resumed accumulator (#96 §7).
    pub(crate) fn reseed(
        &mut self,
        tokens: std::collections::BTreeMap<String, TokenCounts>,
        extra: std::collections::BTreeMap<String, u64>,
        span: Option<(f64, f64)>,
    ) {
        self.per_model = tokens;
        self.extra = extra;
        self.span
            .set_endpoints(span.map(|(a, b)| (a as i64, b as i64)));
    }

    pub(crate) fn finish(self) -> Metrics {
        let duration_secs = self.span.duration_secs();
        // Cost is the SUM over models (#104) — pricing the whole session at one model's rate
        // is wrong whenever it switched, and 4.7% of measured sessions did.
        let (cost_usd, cost_partial) = total_cost(&self.per_model);
        // The flat totals keep their meaning: the sum across models. Every existing consumer
        // reads these unchanged.
        let mut tot = TokenCounts::default();
        for c in self.per_model.values() {
            tot += *c;
        }
        let mut m = Metrics::default();
        m.input_tokens = tot.input;
        m.cache_creation_tokens = tot.cache_creation;
        m.cache_read_tokens = tot.cache_read;
        m.output_tokens = tot.output;
        m.model = self.model;
        m.duration_secs = duration_secs;
        m.cost_usd = cost_usd;
        m.cost_partial = cost_partial;
        m.extra = self.extra;
        m.per_model = self.per_model;
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_engine::seam::estimate_cost;

    /// #104: a session that SWITCHES models must price each model's tokens at its own rate.
    /// Before this, `finish` applied the LAST model's rate to every token in the session —
    /// measured at 4.7% of local sessions, and wrong in whichever direction the last model
    /// happened to differ.
    #[test]
    fn cost_is_summed_per_model_not_priced_at_the_last_one() {
        let line = |model: &str, out: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","model":"{model}",
                   "usage":{{"input_tokens":0,"output_tokens":{out}}}}},
                   "timestamp":"2026-08-05T10:00:00Z"}}"#
            )
            .replace('\n', "")
        };
        let mut acc = MetricsAcc::default();
        for l in [
            line("claude-opus-4-8", 1_000_000),
            line("claude-haiku-4-5-20251001", 10),
        ] {
            acc.push(&serde_json::from_str::<Value>(&l).unwrap());
        }
        let m = acc.finish();

        // Attribution survives: two models, tokens with the one that produced them.
        assert_eq!(m.per_model.len(), 2, "both models attributed");
        assert_eq!(m.per_model["claude-opus-4-8"].output, 1_000_000);
        assert_eq!(m.per_model["claude-haiku-4-5-20251001"].output, 10);
        // The flat totals keep their old meaning — the sum across models.
        assert_eq!(m.output_tokens, 1_000_010);

        // The bug: 1M Opus tokens priced at Haiku's rate. Cost must be dominated by the Opus
        // share, i.e. far above what the last model alone would give.
        let all_at_last = estimate_cost("claude-haiku-4-5-20251001", 0, 0, 0, 1_000_010);
        let (got, wrong) = (m.cost_usd.unwrap(), all_at_last.unwrap());
        assert!(
            got > wrong * 4.0,
            "the Opus share must dominate: got ${got:.4}, last-model pricing gave ${wrong:.4}"
        );
        // And it equals the sum of the two priced separately.
        let want = estimate_cost("claude-opus-4-8", 0, 0, 0, 1_000_000).unwrap()
            + estimate_cost("claude-haiku-4-5-20251001", 0, 0, 0, 10).unwrap();
        assert!((got - want).abs() < 1e-9, "cost must be the per-model sum");
        // One line, still: the label signals the extra model without a breakdown.
        assert!(m.model_label().ends_with("+1"), "got {}", m.model_label());
        assert!(!m.cost_partial, "both models are priced");
    }

    /// Per-model attribution EXPOSES a gap flat counters hid: a model the price table does not
    /// know contributes nothing, so a sum over models can silently cover a fraction of the
    /// session. The byte-gate fixture is exactly this — 97% of its tokens are `claude-fable-5`,
    /// unpriced — and reporting 3% as "the cost" would be worse than the bug being fixed.
    #[test]
    fn an_unpriced_model_makes_the_cost_a_lower_bound() {
        let line = |model: &str, out: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","model":"{model}",
                   "usage":{{"input_tokens":0,"output_tokens":{out}}}}},
                   "timestamp":"2026-08-05T10:00:00Z"}}"#
            )
        };
        let mut acc = MetricsAcc::default();
        for l in [
            line("claude-opus-4-8", 100),
            line("some-unpriced-model", 9_000_000),
        ] {
            acc.push(&serde_json::from_str::<Value>(&l).unwrap());
        }
        let m = acc.finish();
        assert!(
            m.cost_partial,
            "an unpriced model with tokens must flag the total"
        );
        let c = m.cost_usd.unwrap();
        assert_eq!(
            m.cost_label(c),
            format!("≥${c:.2}"),
            "rendered as a lower bound"
        );
        assert_eq!(m.output_tokens, 9_000_100, "totals still count every token");
    }
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
        // claude-opus-4-8 is $5/$25 per MTok — NOT the retired Opus 4/4.1 $15/$75 this test
        // previously assumed, which is the stale-table bug the published rates exposed.
        let expected =
            (1500.0 + 50000.0 * 1.25 + 5_000_000.0 * 0.10) / 1e6 * 5.0 + 10000.0 / 1e6 * 25.0;
        assert!((c - expected).abs() < 1e-9, "cost {c} vs {expected}");
    }
}
