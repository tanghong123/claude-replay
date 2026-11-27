use super::model::field;
use claude_replay_engine::seam::{
    parse_ts, total_cost, Metrics, MetricsTotals, TimeSpan, TokenCounts,
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct MetricsAcc {
    totals: BTreeMap<String, TokenCounts>,
    extra: BTreeMap<String, u64>,
    span: TimeSpan,
    model: String,
    step: Option<u64>,
}

impl MetricsAcc {
    pub(crate) fn push(&mut self, value: &Value) {
        self.step = None;
        if let Some(ts) = field(value, "created_at").and_then(parse_ts) {
            self.span.observe(ts);
        }
        if value["type"] != "PLANNER_RESPONSE" {
            return;
        }
        if let Some(model) = field(value, "model") {
            self.model = model.into();
        }
        let count = |key| value[key].as_u64().unwrap_or(0);
        let counts = TokenCounts {
            input: count("input_tokens"),
            cache_creation: count("cache_write_tokens"),
            cache_read: count("cache_read_tokens"),
            output: count("output_tokens"),
            ..Default::default()
        };
        if counts == TokenCounts::default() {
            return;
        }
        // Some projections omit the model but retain usage. Keep the tokens in the
        // unassigned bucket, without attributing them to a previous response's model.
        let model = field(value, "model").unwrap_or_default();
        *self.totals.entry(model.into()).or_default() += counts;
        self.step = value["step_index"].as_u64();
    }

    pub(crate) fn bump_extra(&mut self, key: &str, n: u64) {
        *self.extra.entry(key.into()).or_default() += n;
    }

    pub(crate) fn usage_id(&self) -> Option<String> {
        self.step.map(|step| step.to_string())
    }

    pub(crate) fn totals(&self) -> MetricsTotals {
        (
            self.totals.clone(),
            self.extra.clone(),
            self.span.endpoints().map(|(a, b)| (a as f64, b as f64)),
        )
    }

    pub(crate) fn finish(&self) -> Metrics {
        let (cost_usd, cost_partial) = total_cost(&self.totals);
        let mut total = TokenCounts::default();
        for counts in self.totals.values() {
            total += *counts;
        }
        let mut metrics = Metrics::default();
        metrics.input_tokens = total.input;
        metrics.cache_creation_tokens = total.cache_creation;
        metrics.cache_read_tokens = total.cache_read;
        metrics.output_tokens = total.output;
        metrics.model.clone_from(&self.model);
        metrics.duration_secs = self.span.duration_secs();
        metrics.cost_usd = cost_usd;
        metrics.cost_partial = cost_partial;
        metrics.extra.clone_from(&self.extra);
        metrics.per_model.clone_from(&self.totals);
        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::MetricsAcc;
    use serde_json::json;

    /// #s713: Antigravity's id for a thinking level (`gemini-3.8-flash-medium`) takes Gemini 3.8
    /// Flash's published rate, each tier priced on its own count. Two readings of the CLI's counts
    /// are assumed, not yet confirmed on a transcript (#s715): input leaves the cache reads out,
    /// and output holds the thinking, which Google's page bills as output.
    #[test]
    fn a_gemini_thinking_level_is_priced_at_its_models_rate() {
        let mut acc = MetricsAcc::default();
        acc.push(&json!({
            "type": "PLANNER_RESPONSE", "created_at": "2026-11-25T00:00:00Z", "step_index": 1,
            "model": "gemini-3.8-flash-medium",
            "input_tokens": 9689, "cache_read_tokens": 400, "output_tokens": 120,
        }));
        let metrics = acc.finish();
        assert!(!metrics.cost_partial, "every token is priced");
        let cost = metrics.cost_usd.expect("gemini-3.8-flash-medium is priced");
        let expected = (9689.0 * 0.75 + 400.0 * 0.075 + 120.0 * 3.75) / 1e6;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost {cost}, expected {expected}"
        );
    }
    #[test]
    fn neon_high_prices_existing_trajectory_counts() {
        let mut acc = MetricsAcc::default();
        acc.push(&json!({
            "type": "PLANNER_RESPONSE", "step_index": 1, "model": "gemini-4-neon-high",
            "input_tokens": 1000000, "cache_read_tokens": 1000000, "output_tokens": 100000,
        }));
        let metrics = acc.finish();
        let cost = metrics
            .cost_usd
            .expect("Neon High has a published base rate");
        assert!((cost - 4.65).abs() < 1e-12);
        assert!(!metrics.cost_partial);
        assert_eq!(metrics.cache_read_tokens, 1000000);

        acc.push(&json!({
            "type": "PLANNER_RESPONSE", "step_index": 2, "model": "gemini-4-neon-unknown",
            "input_tokens": 1000000,
        }));
        let mixed = acc.finish();
        assert_eq!(mixed.cost_usd, metrics.cost_usd);
        assert!(mixed.cost_partial);
    }
}
