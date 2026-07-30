use crate::engine::seam::{parse_ts, Metrics, TimeSpan};
use serde_json::Value;

/// Codex's per-line token/cost accumulator, folded through the shared
/// `MetricsAccumulator` seam. Unlike Claude, Codex
/// reports a *cumulative* `total_token_usage`, so each `token_count` event overwrites
/// (keeping the newest total), not sums.
#[derive(Default, Clone)]
pub(crate) struct CodexMetricsAcc {
    input: u64,
    cached: u64,
    output: u64,
    model: String,
    span: TimeSpan,
    extra: std::collections::BTreeMap<String, u64>,
}

impl CodexMetricsAcc {
    /// Fold an **agent-specific** metric into the accumulating [`Metrics::extra`] bag (sum by
    /// key) — the seam for a Codex-only counter. Nothing calls it yet; the interface is ready
    /// for the first such metric (task #22).
    #[allow(dead_code)]
    pub(crate) fn bump(&mut self, key: &str, n: u64) {
        *self.extra.entry(key.to_string()).or_default() += n;
    }

    pub(crate) fn push(&mut self, value: &Value) {
        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
        {
            self.span.observe(timestamp);
        }
        if value.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(next) = value.pointer("/payload/model").and_then(Value::as_str) {
                self.model = next.to_string();
            }
        }
        if value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
        {
            let Some(total) = value.pointer("/payload/info/total_token_usage") else {
                return;
            };
            let field = |name: &str| total.get(name).and_then(Value::as_u64).unwrap_or(0);
            let total_input = field("input_tokens");
            self.cached = field("cached_input_tokens");
            self.input = total_input.saturating_sub(self.cached);
            self.output = field("output_tokens");
        }
    }

    pub(crate) fn finish(self) -> Metrics {
        // Codex has no cache-write tier; cached input bills at the read discount.
        let cost_usd = crate::engine::seam::estimate_cost(
            &self.model,
            self.input,
            0,
            self.cached,
            self.output,
        );
        Metrics {
            input_tokens: self.input,
            cache_creation_tokens: 0,
            cache_read_tokens: self.cached,
            output_tokens: self.output,
            model: self.model,
            duration_secs: self.span.duration_secs(),
            cost_usd,
            extra: self.extra,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::seam::parse_reader_for;

    /// Metrics via the public reader dispatch — exercises the shared `MetricsAccumulator`
    /// default path with Codex's accumulator.
    fn parse_codex_reader(jsonl: &str) -> Metrics {
        parse_reader_for(
            crate::engine::seam::Agent::CODEX,
            std::io::Cursor::new(jsonl),
        )
    }

    #[test]
    fn uses_newest_cumulative_usage_and_keeps_cached_input_separate() {
        let jsonl = r#"
{"timestamp":"2026-07-18T01:00:00Z","type":"turn_context","payload":{"model":"gpt-5.6"}}
{"timestamp":"2026-07-18T01:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":20}}}}
{"timestamp":"2026-07-18T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"cached_input_tokens":200,"output_tokens":80}}}}
"#;
        let metrics = parse_codex_reader(jsonl);
        assert_eq!(metrics.input_tokens, 100);
        assert_eq!(metrics.cache_read_tokens, 200);
        assert_eq!(metrics.output_tokens, 80);
        assert_eq!(metrics.model, "gpt-5.6");
        assert_eq!(metrics.duration_secs, 60);
        // gpt-5 family is priced: 100 in + 200 cached·0.10 + 80 out → non-zero.
        let cost = metrics.cost_usd.expect("gpt-5 should be priced");
        let expected = (100.0 + 200.0 * 0.10) / 1e6 * 1.25 + 80.0 / 1e6 * 10.0;
        assert!((cost - expected).abs() < 1e-9, "cost: {cost}");
        let footer = metrics.footer();
        assert!(footer.contains("gpt-5.6"), "footer: {footer}");
        assert!(footer.contains("100 in"), "footer: {footer}");
        assert!(footer.contains("200 cached"), "footer: {footer}");
        assert!(footer.contains('$'), "footer: {footer}");
    }
}
