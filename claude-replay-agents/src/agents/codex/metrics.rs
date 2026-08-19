use claude_replay_engine::seam::{
    parse_ts, total_cost, Metrics, MetricsTotals, RateLimitWindow, RateLimits, RuntimeInfo,
    TimeSpan, TokenCounts,
};
use serde_json::Value;

/// Codex's per-line token/cost accumulator, folded through the shared
/// `MetricsAccumulator` seam. Unlike Claude, Codex
/// reports a *cumulative* `total_token_usage`, so each `token_count` event overwrites
/// (keeping the newest total), not sums.
#[derive(Default, Clone)]
pub(crate) struct CodexMetricsAcc {
    /// Tokens attributed to the model in force when they were reported (#104). Readings that
    /// arrive before the session names one land under the empty key and are attributed in
    /// [`finish`](Self::finish).
    per_model: std::collections::BTreeMap<String, TokenCounts>,
    /// The last cumulative reading. Codex reports RUNNING TOTALS, so each event's contribution
    /// is its difference from this — the totals→increments conversion the record format wants,
    /// done here in the adapter because only it knows its agent reports totals at all.
    last_total: TokenCounts,
    /// Whether any `token_count` was folded yet. A forked or sub-agent thread's cumulative
    /// counter INHERITS the parent's pre-fork history — its first reading can stand at
    /// hundreds of millions of tokens that live (and are priced) in the PARENT's rollout.
    /// The first event therefore banks only its own `last_token_usage`, with the remainder
    /// of the total set aside as the baseline; measured over one machine's lumen rollouts,
    /// banking that first total from zero over-counted 18 forked files by ~$476.
    seen_usage: bool,
    model: String,
    /// The FIRST model the session named — not necessarily [`model`](Self::model), the one in
    /// force at the end. Usage reported before any name was seen is attributed to this one,
    /// being the model in force closest to when those tokens were produced.
    first_model: String,
    span: TimeSpan,
    extra: std::collections::BTreeMap<String, u64>,
    runtime: RuntimeInfo,
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
                if self.first_model.is_empty() && !next.is_empty() {
                    self.first_model = next.to_string();
                }
                self.model = next.to_string();
            }
            remember_string(
                &mut self.runtime.reasoning_effort,
                value.pointer("/payload/effort"),
            );
            remember_string(
                &mut self.runtime.approval_policy,
                value.pointer("/payload/approval_policy"),
            );
            remember_string(
                &mut self.runtime.sandbox,
                value.pointer("/payload/sandbox_policy/type"),
            );
            remember_string(
                &mut self.runtime.permission_profile,
                value.pointer("/payload/permission_profile/type"),
            );
            remember_string(
                &mut self.runtime.collaboration_mode,
                value.pointer("/payload/collaboration_mode/mode"),
            );
        }
        if value.get("type").and_then(Value::as_str) == Some("event_msg")
            && value.pointer("/payload/type").and_then(Value::as_str)
                == Some("thread_settings_applied")
        {
            let settings = value.pointer("/payload/thread_settings");
            remember_string(
                &mut self.runtime.reasoning_effort,
                settings.and_then(|v| v.get("reasoning_effort")),
            );
            remember_string(
                &mut self.runtime.approval_policy,
                settings.and_then(|v| v.get("approval_policy")),
            );
            remember_string(
                &mut self.runtime.permission_profile,
                settings.and_then(|v| v.pointer("/permission_profile/type")),
            );
            remember_string(
                &mut self.runtime.collaboration_mode,
                settings.and_then(|v| v.pointer("/collaboration_mode/mode")),
            );
            remember_string(
                &mut self.runtime.service_tier,
                settings.and_then(|v| v.get("service_tier")),
            );
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
            self.runtime.context_window_tokens = value
                .pointer("/payload/info/model_context_window")
                .and_then(Value::as_u64)
                .or(self.runtime.context_window_tokens);
            self.runtime.context_used_tokens = value
                .pointer("/payload/info/last_token_usage/input_tokens")
                .and_then(Value::as_u64)
                .or(self.runtime.context_used_tokens);
            if let Some(limits) = value.pointer("/payload/rate_limits") {
                self.runtime.rate_limits = Some(RateLimits {
                    primary: rate_limit_window(limits.get("primary")),
                    secondary: rate_limit_window(limits.get("secondary")),
                    plan_type: limits
                        .get("plan_type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    reached: limits
                        .get("rate_limit_reached_type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
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
            if !self.seen_usage {
                self.seen_usage = true;
                // The FIRST reading of a forked/sub-agent thread inherits the parent's
                // cumulative counter: banking it from zero would price the parent's whole
                // pre-fork history AGAIN (it is priced in the parent's own rollout, which the
                // monitor rolls up). The event's `last_token_usage` is this thread's own share,
                // so the baseline is `total − last` and the first banked delta is exactly that
                // share. A rollout old enough to lack `last` keeps the from-zero behavior.
                if let Some(last) = value.pointer("/payload/info/last_token_usage") {
                    let lfield = |name: &str| last.get(name).and_then(Value::as_u64).unwrap_or(0);
                    let lcached = lfield("cached_input_tokens");
                    self.last_total = TokenCounts {
                        input: now
                            .input
                            .saturating_sub(lfield("input_tokens").saturating_sub(lcached)),
                        cache_creation: 0,
                        cache_read: now.cache_read.saturating_sub(lcached),
                        output: now.output.saturating_sub(lfield("output_tokens")),
                    };
                }
            }
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

    /// This accumulator's cursor state (#14): the shared totals PLUS the private fields
    /// `push` reads — `last_total` (Codex reports cumulative usage, so each event banks its
    /// difference from this) and the model in force. A resume that restored only the shared
    /// half double-counted the first usage record after the checkpoint and misattributed
    /// until the next `turn_context`; carrying these is the whole point of the override.
    /// `first_model` rides along because [`finish`](Self::finish) attributes pre-naming usage
    /// to it, and which name came FIRST is not recoverable from a resumed suffix.
    pub(crate) fn state(&self) -> Value {
        serde_json::json!({
            "totals": self.totals(),
            "last_total": self.last_total,
            "seen_usage": self.seen_usage,
            "model": self.model,
            "first_model": self.first_model,
            "runtime": self.runtime,
        })
    }

    /// Restore [`state`](Self::state). Unrecognized input is ignored — a cursor is a cache,
    /// and an unreadable one is a cold start, never an error.
    pub(crate) fn restore(&mut self, state: &Value) {
        let Some(totals) = state.get("totals") else {
            return;
        };
        let Ok((tokens, extra, span)) = serde_json::from_value::<MetricsTotals>(totals.clone())
        else {
            return;
        };
        self.reseed(tokens, extra, span);
        if let Some(t) = state
            .get("last_total")
            .and_then(|v| serde_json::from_value::<TokenCounts>(v.clone()).ok())
        {
            self.last_total = t;
        }
        // A cursor from before `seen_usage` existed carries no flag; a fold that banked
        // anything has a non-default `last_total`, which is the same evidence.
        self.seen_usage = state
            .get("seen_usage")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| self.last_total != TokenCounts::default());
        if let Some(m) = state.get("model").and_then(Value::as_str) {
            self.model = m.to_string();
        }
        if let Some(m) = state.get("first_model").and_then(Value::as_str) {
            self.first_model = m.to_string();
        }
        if let Some(runtime) = state
            .get("runtime")
            .and_then(|v| serde_json::from_value::<RuntimeInfo>(v.clone()).ok())
        {
            self.runtime = runtime;
        }
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

    pub(crate) fn finish(mut self) -> Metrics {
        // Usage the transcript reported before it named a model: `turn_context` is written when
        // a turn STARTS, so a resumed, forked or sub-agent transcript replays the usage it
        // inherited BEFORE its first turn, and those readings bank under the empty key — which
        // no price table matches, degrading the session's cost to a `≥$` lower bound that
        // silently omits them. Measured over one day of real rollouts (2026-08-11, 262 files on
        // two machines): 29 transcripts, 5.86B tokens, ~$892 invisible on one host alone.
        //
        // They are this session's own history, so the model it named FIRST claims them, that
        // being the one in force closest to when they were produced. Resolving it HERE and not
        // when the name arrives is deliberate: `per_model` is also the resumable fold's
        // checkpoint and the basis of its per-event deltas (#14, diffed key by key), so moving
        // tokens between keys mid-fold would emit the same tokens a second time — a
        // double-counted cost, which is the very failure this fixes.
        let claimant = if self.first_model.is_empty() {
            &self.model
        } else {
            &self.first_model
        };
        if !claimant.is_empty() {
            if let Some(inherited) = self.per_model.remove("") {
                let claimant = claimant.clone();
                *self.per_model.entry(claimant).or_default() += inherited;
            }
        }
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
        m.runtime = self.runtime;
        m
    }
}

fn remember_string(slot: &mut Option<String>, value: Option<&Value>) {
    if let Some(value) = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        *slot = Some(value.to_string());
    }
}

fn rate_limit_window(value: Option<&Value>) -> Option<RateLimitWindow> {
    let value = value?;
    Some(RateLimitWindow {
        used_percent: value.get("used_percent").and_then(Value::as_f64)?,
        window_minutes: value
            .get("window_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        resets_at: value.get("resets_at").and_then(Value::as_i64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_runtime_context_settings_and_limits_survive_the_adapter() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&serde_json::json!({"type":"turn_context","payload":{
            "model":"gpt-5.6-sol","effort":"xhigh","approval_policy":"never",
            "sandbox_policy":{"type":"danger-full-access"},
            "permission_profile":{"type":"disabled"},
            "collaboration_mode":{"mode":"default"}
        }}));
        acc.push(&serde_json::json!({"type":"event_msg","payload":{
            "type":"thread_settings_applied","thread_settings":{"service_tier":"priority"}
        }}));
        acc.push(&serde_json::json!({"type":"event_msg","payload":{
            "type":"token_count",
            "info":{
                "model_context_window":200000,
                "last_token_usage":{"input_tokens":50000},
                "total_token_usage":{"input_tokens":50000,"cached_input_tokens":40000,"output_tokens":1000}
            },
            "rate_limits":{"plan_type":"plus","rate_limit_reached_type":null,
                "primary":{"used_percent":12.5,"window_minutes":10080,"resets_at":1234},
                "secondary":null}
        }}));

        let state = acc.state();
        let mut resumed = CodexMetricsAcc::default();
        resumed.restore(&state);
        let metrics = resumed.finish();
        assert_eq!(metrics.context_left_percent(), Some(75));
        assert_eq!(metrics.runtime.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(
            metrics.runtime.sandbox.as_deref(),
            Some("danger-full-access")
        );
        assert_eq!(metrics.runtime.service_tier.as_deref(), Some("priority"));
        let limits = metrics.runtime.rate_limits.as_ref().unwrap();
        assert_eq!(limits.plan_type.as_deref(), Some("plus"));
        assert_eq!(limits.primary.as_ref().unwrap().used_percent, 12.5);
        assert!(metrics
            .footer_segments()
            .iter()
            .any(|(text, _)| text == "75% context left"));
    }

    /// A `token_count` event carrying Codex's CUMULATIVE usage — the totals so far, not a
    /// per-turn delta. Tests push a SEQUENCE of these and assert on the banked increments.
    /// Note `input` is the total that already CONTAINS `cached`.
    fn token_count(input: u64, cached: u64, out: u64) -> Value {
        serde_json::json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {
                "input_tokens": input,
                "cached_input_tokens": cached,
                "output_tokens": out,
            }
        }}})
    }

    /// The record that puts a model in force for every reading that follows it.
    fn turn_context(model: &str) -> Value {
        serde_json::json!({"type": "turn_context", "payload": {"model": model}})
    }

    /// A `token_count` event as MODERN Codex writes it: the cumulative total AND the call's
    /// own `last_token_usage` side by side. On a forked thread the total INHERITS the
    /// parent's history, so the two diverge wildly on the first event.
    fn token_count_with_last(
        total: (u64, u64, u64), // (input, cached, output) — cumulative
        last: (u64, u64, u64),  // this call's own usage
    ) -> Value {
        serde_json::json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {
                "input_tokens": total.0, "cached_input_tokens": total.1, "output_tokens": total.2,
            },
            "last_token_usage": {
                "input_tokens": last.0, "cached_input_tokens": last.1, "output_tokens": last.2,
            }
        }}})
    }

    /// A forked/sub-agent thread's first cumulative reading carries the PARENT's entire
    /// pre-fork history — priced already in the parent's own rollout. The first event must
    /// bank only its own `last_token_usage`; from-zero banking over-counted 18 real forked
    /// rollouts by ~$476 on one audited project.
    #[test]
    fn a_forked_threads_inherited_total_is_a_baseline_not_a_delta() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&turn_context("gpt-5.6"));
        // First reading: total stands at 1,000,000 inherited + this call's own 1,000/400/50.
        acc.push(&token_count_with_last(
            (1_001_000, 400, 50),
            (1_000, 400, 50),
        ));
        // Second reading advances the total by its own last — ordinary delta banking.
        acc.push(&token_count_with_last(
            (1_003_000, 1_200, 80),
            (2_000, 800, 30),
        ));
        let m = acc.finish();
        assert_eq!(
            m.input_tokens, 1_800,
            "own uncached input only: (1000-400)+(2000-800)"
        );
        assert_eq!(m.cache_read_tokens, 1_200);
        assert_eq!(m.output_tokens, 80);
    }

    /// The baseline is established ONCE: a resumed fold must not re-baseline against the
    /// first post-checkpoint event (that would drop the delta since the checkpoint), so
    /// `seen_usage` rides the cursor.
    #[test]
    fn the_fork_baseline_is_not_reapplied_after_a_cursor_round_trip() {
        let mut head = CodexMetricsAcc::default();
        head.push(&turn_context("gpt-5.6"));
        head.push(&token_count_with_last(
            (1_001_000, 400, 50),
            (1_000, 400, 50),
        ));
        let cursor = head.state();

        let mut tail = CodexMetricsAcc::default();
        tail.restore(&cursor);
        tail.push(&token_count_with_last(
            (1_003_000, 1_200, 80),
            (2_000, 800, 30),
        ));
        let m = tail.finish();
        // Same figures as the unbroken fold above — the resume neither re-banks the
        // inherited total nor re-baselines away the checkpointed delta.
        assert_eq!(m.input_tokens, 1_800);
        assert_eq!(m.cache_read_tokens, 1_200);
        assert_eq!(m.output_tokens, 80);
    }

    /// #104, Codex side. Codex reports RUNNING TOTALS, so per-model attribution means banking
    /// each event's INCREMENT against the model in force — not overwriting a flat counter.
    #[test]
    fn running_totals_bank_increments_against_the_model_in_force() {
        let mut acc = CodexMetricsAcc::default();
        for l in [
            turn_context("gpt-a"),
            token_count(100, 0, 10),
            token_count(300, 0, 30),
            turn_context("gpt-b"),
            token_count(400, 0, 45),
        ] {
            acc.push(&l);
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
        let mut acc = CodexMetricsAcc::default();
        for l in [token_count(0, 0, 500), token_count(0, 0, 100)] {
            acc.push(&l);
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

    /// Codex's cumulative `input_tokens` INCLUDES tokens served from cache. The adapter must
    /// report uncached input and cache-read input as separate buckets so cost/footers are not
    /// double-counted.
    #[test]
    fn input_tokens_total_includes_cached_and_cache_read_is_reported_separately() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&turn_context("gpt-5.6"));
        acc.push(&token_count(1000, 600, 200));
        let m = acc.finish();
        assert_eq!(m.input_tokens, 400, "uncached input = total input - cached");
        assert_eq!(m.cache_read_tokens, 600);
        assert_eq!(m.output_tokens, 200);
        // Footer must show BOTH buckets, not fold cache back into input.
        let footer = m.footer();
        assert!(footer.contains("400 in"), "footer: {footer}");
        assert!(footer.contains("600 cached"), "footer: {footer}");
    }

    /// A cache-hit-only increase between two cumulative readings must advance `cache_read`
    /// without changing the uncached input bucket.
    #[test]
    fn cache_read_increments_when_only_cached_input_grows() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&token_count(100, 50, 10));
        acc.push(&token_count(300, 200, 80));
        let m = acc.finish();
        assert_eq!(m.input_tokens, 100); // 300 - 200
        assert_eq!(m.cache_read_tokens, 200);
        assert_eq!(m.output_tokens, 80);
    }

    /// Cache increments follow the same per-model rule as ordinary input/output: the delta
    /// from the previous cumulative reading is banked against the model currently in force.
    #[test]
    fn cache_increments_are_attributed_to_the_model_in_force() {
        let mut acc = CodexMetricsAcc::default();
        for l in [
            turn_context("gpt-a"),
            token_count(100, 80, 10),
            turn_context("gpt-b"),
            token_count(400, 300, 20),
        ] {
            acc.push(&l);
        }
        let m = acc.finish();
        // gpt-a saw one reading: uncached 100-80=20, cache 80, output 10.
        assert_eq!(m.per_model["gpt-a"].input, 20);
        assert_eq!(m.per_model["gpt-a"].cache_read, 80);
        assert_eq!(m.per_model["gpt-a"].output, 10);
        // gpt-b saw only the DELTA between the two cumulative readings:
        //   uncached (400-300) - (100-80) = 100 - 20 = 80
        //   cache     300 - 80           = 220
        //   output    20 - 10            = 10
        assert_eq!(m.per_model["gpt-b"].input, 80);
        assert_eq!(m.per_model["gpt-b"].cache_read, 220);
        assert_eq!(m.per_model["gpt-b"].output, 10);
        // Flat totals equal the last cumulative reading's net/cache/output.
        assert_eq!(m.input_tokens, 100);
        assert_eq!(m.cache_read_tokens, 300);
        assert_eq!(m.output_tokens, 20);
    }

    /// A backwards cache total must saturate, never wrap or erase earlier work.
    #[test]
    fn a_backwards_cached_total_is_saturated_to_zero() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&token_count(500, 300, 100));
        acc.push(&token_count(400, 250, 80));
        let m = acc.finish();
        assert_eq!(m.input_tokens, 200, "no wrap on input");
        assert_eq!(m.cache_read_tokens, 300, "no wrap on cache");
        assert_eq!(m.output_tokens, 100, "no wrap on output");
    }

    /// A resumed/forked/sub-agent transcript replays its inherited usage BEFORE its first
    /// `turn_context`. Those readings must not be stranded under the empty model key, where no
    /// price matches and the session's cost silently degrades to a `≥$` lower bound.
    #[test]
    fn tokens_reported_before_the_first_turn_context_go_to_the_first_named_model() {
        let mut acc = CodexMetricsAcc::default();
        for l in [
            token_count(100, 40, 10), // inherited: no model named yet
            token_count(300, 200, 30),
            turn_context("gpt-5.6-sol"), // the session finally names one
            token_count(400, 250, 45),
            turn_context("gpt-5.6-mini"), // and later switches
            token_count(500, 300, 55),
        ] {
            acc.push(&l);
        }
        let m = acc.finish();
        assert!(
            !m.per_model.contains_key(""),
            "nothing left unattributed: {:?}",
            m.per_model.keys().collect::<Vec<_>>()
        );
        // The FIRST name claims the inherited readings — the model in force closest to when
        // those tokens were produced — not `gpt-5.6-mini`, the one merely in force at the end.
        //   gpt-5.6-sol: inherited (100 cumulative, of which 40 cached → 60 in / 40 cache / 10 out)
        //          + its own (300→400 in, 200→250 cached, 30→45 out)
        assert_eq!(m.per_model["gpt-5.6-sol"].input, 150, "400 - 250 cached");
        assert_eq!(m.per_model["gpt-5.6-sol"].cache_read, 250);
        assert_eq!(m.per_model["gpt-5.6-sol"].output, 45);
        assert_eq!(m.per_model["gpt-5.6-mini"].output, 10, "55 - 45");
        assert!(
            !m.cost_partial,
            "every token now belongs to a priced model, so the cost is not a lower bound"
        );
    }

    /// The complement: with no model ever named there is nothing to attribute to. The tokens
    /// stay in the unnamed bucket and the cost stays a lower bound — claiming them for a
    /// guessed model would price a session at a rate the transcript never evidences.
    #[test]
    fn tokens_stay_unattributed_when_no_model_is_ever_named() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&token_count(100, 40, 10));
        let m = acc.finish();
        assert_eq!(m.per_model[""].output, 10);
        assert!(
            m.cost_partial,
            "an unpriced model makes the total a lower bound"
        );
        assert_eq!(m.cost_usd, None, "nothing priced, so no figure at all");
    }

    /// A periodic fold checkpoints through `state` → `restore` (#14) and may well split
    /// between the inherited usage and the first `turn_context`. Which name came FIRST is not
    /// recoverable from the resumed suffix, so the cursor has to carry it — otherwise a
    /// resumed fold attributes less than a cold fold of the same bytes.
    #[test]
    fn the_first_model_survives_a_cursor_round_trip() {
        let mut head = CodexMetricsAcc::default();
        for l in [
            token_count(300, 200, 30), // read before any turn_context
            turn_context("gpt-5.6-sol"),
        ] {
            head.push(&l);
        }
        let cursor = head.state();

        let mut tail = CodexMetricsAcc::default();
        tail.restore(&cursor);
        tail.push(&turn_context("gpt-5.6-mini")); // the suffix only ever sees the SECOND name
        let m = tail.finish();
        assert!(
            !m.per_model.contains_key(""),
            "claimed after the resume too"
        );
        assert_eq!(
            m.per_model["gpt-5.6-sol"].output, 30,
            "the first name, carried by the cursor"
        );
        assert!(!m.cost_partial);
    }

    /// Attribution happens in `finish`, never mid-fold, because `per_model` is also the
    /// resumable fold's per-event delta basis (#14): moving tokens between keys while folding
    /// would surface as a fresh delta and double-count the cost. The running totals must
    /// therefore stay put as the model name arrives.
    #[test]
    fn claiming_the_unnamed_bucket_never_moves_a_running_total() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&token_count(300, 200, 30));
        let (before, _, _) = acc.totals();
        acc.push(&turn_context("gpt-5.6"));
        let (after, _, _) = acc.totals();
        assert_eq!(before, after, "naming a model moves no counter");
        // Only the finished metrics attribute — once, at the end.
        assert_eq!(acc.finish().per_model["gpt-5.6"].output, 30);
    }
}
