use claude_replay_engine::seam::{parse_ts, total_cost, Metrics, TimeSpan, TokenCounts};
use serde_json::Value;

/// Codex's per-line token/cost accumulator, folded through the shared
/// `MetricsAccumulator` seam. Unlike Claude, Codex
/// reports a *cumulative* `total_token_usage`, so each `token_count` event overwrites
/// (keeping the newest total), not sums.
#[derive(Default, Clone)]
pub(crate) struct CodexMetricsAcc {
    /// Tokens attributed to the model in force when they were reported (#104).
    per_model: std::collections::BTreeMap<String, TokenCounts>,
    /// The last cumulative reading. Codex reports RUNNING TOTALS, so each event's contribution
    /// is its difference from this — the totals→increments conversion the record format wants,
    /// done here in the adapter because only it knows its agent reports totals at all.
    last_total: TokenCounts,
    model: String,
    span: TimeSpan,
    extra: std::collections::BTreeMap<String, u64>,
}

impl CodexMetricsAcc {
    /// Fold an **agent-specific** metric into the accumulating [`Metrics::extra`] bag (sum by
    /// key). Parse diagnostics use this for skipped malformed and unsupported records.
    pub(crate) fn bump(&mut self, key: &str, n: u64) {
        *self.extra.entry(key.to_string()).or_default() += n;
    }

    pub(crate) fn malformed_line(&mut self) {
        self.bump("malformed_lines", 1);
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
        if value.get("type").and_then(Value::as_str) == Some("response_item") {
            let supported = matches!(
                value.pointer("/payload/type").and_then(Value::as_str),
                Some(
                    "message"
                        | "reasoning"
                        | "function_call"
                        | "custom_tool_call"
                        | "function_call_output"
                        | "custom_tool_call_output"
                        | "tool_search_call"
                        | "tool_search_output"
                        | "web_search_call"
                        | "image_generation_call"
                        // A persistent sub-agent's reply is intentionally not a parent block.
                        | "agent_message"
                )
            );
            if !supported {
                self.bump("unsupported_items", 1);
            }
        }
        if value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
        {
            let Some(total) = value.pointer("/payload/info/total_token_usage") else {
                return;
            };
            let field = |name: &str| total.get(name).and_then(Value::as_u64).unwrap_or(0);
            let cached = field("cached_input_tokens");
            let now = TokenCounts {
                input: field("input_tokens").saturating_sub(cached),
                cache_creation: 0, // Codex has no cache-write tier
                cache_read: cached,
                output: field("output_tokens"),
            };
            // Bank the increment against the CURRENT model. `saturating_sub` because a total
            // that goes backwards (a reset) must contribute nothing, never wrap.
            let e = self.per_model.entry(self.model.clone()).or_default();
            e.input += now.input.saturating_sub(self.last_total.input);
            e.cache_read += now.cache_read.saturating_sub(self.last_total.cache_read);
            e.output += now.output.saturating_sub(self.last_total.output);
            self.last_total = now;
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
        // Cost is the sum over models; cached input bills at the read discount (no write tier).
        let (cost_usd, cost_partial) = total_cost(&self.per_model);
        let mut tot = TokenCounts::default();
        for c in self.per_model.values() {
            tot += *c;
        }
        let mut m = Metrics::default();
        m.input_tokens = tot.input;
        m.cache_creation_tokens = tot.cache_creation;
        m.cache_read_tokens = tot.cache_read;
        m.output_tokens = tot.output;
        m.per_model = self.per_model;
        m.model = self.model;
        m.duration_secs = self.span.duration_secs();
        m.cost_usd = cost_usd;
        m.cost_partial = cost_partial;
        m.extra = self.extra;
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #104, Codex side. Codex reports RUNNING TOTALS, so per-model attribution means banking
    /// each event's INCREMENT against the model in force — not overwriting a flat counter.
    #[test]
    fn running_totals_bank_increments_against_the_model_in_force() {
        let ctx = |m: &str| format!(r#"{{"type":"turn_context","payload":{{"model":"{m}"}}}}"#);
        let count = |input: u64, out: u64| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{
                   "total_token_usage":{{"input_tokens":{input},"cached_input_tokens":0,
                   "output_tokens":{out}}}}}}}}}"#
            )
        };
        let mut acc = CodexMetricsAcc::default();
        for l in [
            ctx("gpt-a"),
            count(100, 10),
            count(300, 30),
            ctx("gpt-b"),
            count(400, 45),
        ] {
            acc.push(&serde_json::from_str::<Value>(&l).unwrap());
        }
        let m = acc.finish();
        // gpt-a got the first two readings (cumulative 300/30); gpt-b only the INCREMENT.
        assert_eq!(
            m.per_model["gpt-a"].output, 30,
            "cumulative, not double-counted"
        );
        assert_eq!(
            m.per_model["gpt-b"].output, 15,
            "the delta 45-30, not the total 45"
        );
        assert_eq!(m.per_model["gpt-a"].input, 300);
        assert_eq!(m.per_model["gpt-b"].input, 100);
        // The flat totals still equal the last cumulative reading.
        assert_eq!(m.output_tokens, 45);
        assert_eq!(m.input_tokens, 400);
    }

    /// A total that goes BACKWARDS (a reset) must contribute nothing, never wrap.
    #[test]
    fn a_backwards_total_contributes_zero() {
        let count = |out: u64| {
            format!(
                r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{
                   "total_token_usage":{{"input_tokens":0,"cached_input_tokens":0,
                   "output_tokens":{out}}}}}}}}}"#
            )
        };
        let mut acc = CodexMetricsAcc::default();
        for l in [count(500), count(100)] {
            acc.push(&serde_json::from_str::<Value>(&l).unwrap());
        }
        assert_eq!(acc.finish().output_tokens, 500, "no wrap, no loss");
    }
    use claude_replay_engine::seam::parse_reader_with;

    /// Metrics via the public reader dispatch — exercises the shared `MetricsAccumulator`
    /// default path with Codex's accumulator.
    fn parse_codex_reader(jsonl: &str) -> Metrics {
        parse_reader_with(&crate::adapters::CodexAdapter, std::io::Cursor::new(jsonl))
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

    /// The complement of the observability test below: what must NOT count. A final line
    /// without its newline is a write in progress — the agent is appending at that moment —
    /// and a blank line is nothing. Counting either would flash "⚠ skipped" on a one-shot
    /// parse of a LIVE transcript, reading as data corruption to the user.
    #[test]
    fn a_torn_tail_and_blank_lines_are_not_schema_drift() {
        let jsonl = concat!(
            r#"{"timestamp":"2026-08-09T22:48:04.513Z","type":"turn_context","payload":{"model":"gpt-5.6"}}"#,
            "\n",
            "\n",                                        // a blank line: nothing
            r#"{"type":"response_item","payload":{"ty"#, // torn mid-write, no newline
        );
        let metrics = parse_codex_reader(jsonl);
        assert_eq!(metrics.extra.get("malformed_lines"), None);
        assert_eq!(metrics.extra.get("unsupported_items"), None);
    }

    #[test]
    fn malformed_and_unsupported_content_records_are_observable() {
        let jsonl = concat!(
            "not json\n",
            r#"{"type":"response_item","payload":{"type":"future_content_item","value":1}}"#,
            "\n",
        );
        let metrics = parse_codex_reader(jsonl);
        assert_eq!(metrics.extra.get("malformed_lines"), Some(&1));
        assert_eq!(metrics.extra.get("unsupported_items"), Some(&1));
    }
}
