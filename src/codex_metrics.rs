use crate::metrics::{parse_ts, Metrics};
use serde_json::Value;
use std::io::BufRead;

/// Codex's per-line token/cost accumulator — the folding half of `parse_codex_reader`,
/// split out so the streaming engine (`model::parse_stream`, M10) folds metrics in the same
/// pass. Unlike Claude, Codex reports a *cumulative* `total_token_usage`, so each
/// `token_count` event overwrites (keeping the newest total), not sums.
#[derive(Default)]
pub(crate) struct CodexMetricsAcc {
    input: u64,
    cached: u64,
    output: u64,
    model: String,
    first: Option<i64>,
    last: Option<i64>,
}

impl CodexMetricsAcc {
    pub(crate) fn push(&mut self, value: &Value) {
        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
        {
            self.first = Some(
                self.first
                    .map_or(timestamp, |seen: i64| seen.min(timestamp)),
            );
            self.last = Some(self.last.map_or(timestamp, |seen: i64| seen.max(timestamp)));
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
        let cost_usd =
            crate::metrics::estimate_cost(&self.model, self.input, 0, self.cached, self.output);
        Metrics {
            input_tokens: self.input,
            cache_creation_tokens: 0,
            cache_read_tokens: self.cached,
            output_tokens: self.output,
            model: self.model,
            duration_secs: match (self.first, self.last) {
                (Some(start), Some(end)) => (end - start).max(0),
                _ => 0,
            },
            cost_usd,
        }
    }
}

pub(crate) fn parse_codex_reader<R: BufRead>(reader: R) -> Metrics {
    let mut acc = CodexMetricsAcc::default();
    for line in reader.lines().map_while(|line| line.ok()) {
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            acc.push(&value);
        }
    }
    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_newest_cumulative_usage_and_keeps_cached_input_separate() {
        let jsonl = r#"
{"timestamp":"2026-07-18T01:00:00Z","type":"turn_context","payload":{"model":"gpt-5.6"}}
{"timestamp":"2026-07-18T01:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":20}}}}
{"timestamp":"2026-07-18T01:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"cached_input_tokens":200,"output_tokens":80}}}}
"#;
        let metrics = parse_codex_reader(std::io::Cursor::new(jsonl));
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
