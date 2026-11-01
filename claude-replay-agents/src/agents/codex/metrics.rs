use claude_replay_engine::seam::{
    epoch_secs, parse_ts, total_cost, AnswerTiming, Metrics, MetricsTotals, RateLimitWindow,
    RateLimits, RequestPricing, RuntimeInfo, ServiceTier, TimeSpan, TokenCounts,
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
    pub(crate) request_pricing: Option<RequestPricing>,
    /// The counter reading the most recent push credited, as `<timestamp>/<input>/<output>` of
    /// the raw cumulative `total_token_usage` — the seam's `usage_id`. Codex names no request,
    /// so the reading is the unit: a credited reading always stands above the last, so no two
    /// share one, and a copied or rewritten rollout keeps the record verbatim. Cleared on every
    /// push; transient, never in the cursor.
    pub(crate) usage_id: Option<String>,
    /// The answer the most recent push credited (#s341): one per `token_count` reading that
    /// moved the counter, its id that reading's `usage_id`, timed by [`clock`](Self::clock).
    /// Cleared on every push; transient.
    pub(crate) answer: Option<AnswerTiming>,
    /// The service the session reaches its model through, `session_meta`'s `model_provider`.
    provider: Option<String>,
    /// The timing of the answer the next reading credits (#s341). In the state.
    clock: ResponseClock,
    /// Whether any `token_count` was folded yet. A forked or sub-agent thread's cumulative
    /// counter INHERITS the parent's pre-fork history — its first reading can stand at
    /// hundreds of millions of tokens that live (and are priced) in the PARENT's rollout.
    /// The first event therefore banks only its own `last_token_usage`, with the remainder
    /// of the total set aside as the baseline; measured over one machine's lumen rollouts,
    /// banking that first total from zero over-counted 18 forked files by ~$476.
    seen_usage: bool,
    /// The latest context occupancy Codex reported for one model interaction. Unlike
    /// `total_token_usage` (the session-wide billing counter), `last_token_usage.total_tokens`
    /// resets to the compacted continuation size immediately after a compaction, so it is the
    /// value the native TUI uses for its context gauge and the pre/post seam below.
    last_context_tokens: Option<u64>,
    /// A `compacted` record is followed a few records later by a zero-work `token_count` whose
    /// `last_token_usage.total_tokens` is the continuation's new size. Hold the pre-compaction
    /// occupancy until that record arrives so the shared usage panel can report both the count
    /// and the genuinely dropped tokens, just as it does for Claude sessions.
    pending_compaction_pre_tokens: Option<u64>,
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
    /// A fold that declares which runtime facts its FORMAT records (#62), by the wire's key
    /// names; the Claude family has the same constructor.
    pub(crate) fn recording(keys: &[&str]) -> Self {
        let mut acc = Self::default();
        acc.runtime.recorded = keys.iter().map(|k| k.to_string()).collect();
        acc
    }

    /// Fold an **agent-specific** metric into the accumulating [`Metrics::extra`] bag (sum by
    /// key). Parse diagnostics use this for skipped malformed and unsupported records.
    pub(crate) fn bump(&mut self, key: &str, n: u64) {
        *self.extra.entry(key.to_string()).or_default() += n;
    }

    pub(crate) fn malformed_line(&mut self) {
        self.bump("malformed_lines", 1);
        self.clock.unread();
    }

    pub(crate) fn push(&mut self, value: &Value) {
        self.request_pricing = None;
        self.usage_id = None;
        self.answer = None;
        self.clock.line(
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(epoch_secs),
            RolloutLine::of(value),
        );
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            if let Some(p) = value
                .pointer("/payload/model_provider")
                .and_then(Value::as_str)
            {
                self.provider = Some(p.to_string()).filter(|p| !p.is_empty());
            }
            if let Some(version) = value
                .pointer("/payload/cli_version")
                .and_then(Value::as_str)
            {
                self.clock.items_unseen = super::model::transport_ignored(version);
            }
        }
        if let Some(timestamp) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
        {
            self.span.observe(timestamp);
        }
        if value.get("type").and_then(Value::as_str) == Some("compacted") {
            self.bump("compactions", 1);
            self.pending_compaction_pre_tokens = self.last_context_tokens;
        }
        if value.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(next) = value.pointer("/payload/model").and_then(Value::as_str) {
                if self.first_model.is_empty() && !next.is_empty() {
                    self.first_model = next.to_string();
                }
                self.model = next.to_string();
            }
            if let Some(tier) = value.pointer("/payload/service_tier") {
                self.runtime.service_tier =
                    tier.as_str().filter(|s| !s.is_empty()).map(str::to_string);
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
            if let Some(tier) = settings.and_then(|v| v.get("service_tier")) {
                self.runtime.service_tier =
                    tier.as_str().filter(|s| !s.is_empty()).map(str::to_string);
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
            self.runtime.context_window_tokens = value
                .pointer("/payload/info/model_context_window")
                .and_then(Value::as_u64)
                .or(self.runtime.context_window_tokens);
            let context_tokens = value
                .pointer("/payload/info/last_token_usage/total_tokens")
                .and_then(Value::as_u64)
                // Older rollouts did not carry `total_tokens`; their input figure remains the
                // best available context gauge and preserves the pre-v1.96 behavior.
                .or_else(|| {
                    value
                        .pointer("/payload/info/last_token_usage/input_tokens")
                        .and_then(Value::as_u64)
                });
            if let Some(context_tokens) = context_tokens {
                self.runtime.context_used_tokens = Some(context_tokens);
                if let Some(pre_tokens) = self.pending_compaction_pre_tokens.take() {
                    self.bump("compact_dropped", pre_tokens.saturating_sub(context_tokens));
                }
                self.last_context_tokens = Some(context_tokens);
            }
            if let Some(limits) = value.pointer("/payload/rate_limits") {
                // #s14: a reading is dated by the event it came from, so a consumer can tell one
                // taken before a re-login from one after it.
                let observed_at = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_ts);
                self.runtime.rate_limits = Some(RateLimits {
                    primary: rate_limit_window(limits.get("primary"), observed_at),
                    secondary: rate_limit_window(limits.get("secondary"), observed_at),
                    plan_type: limits
                        .get("plan_type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    reached: limits
                        .get("rate_limit_reached_type")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    observed_at,
                });
            }
            let Some(total) = value.pointer("/payload/info/total_token_usage") else {
                return;
            };
            let field = |name: &str| total.get(name).and_then(Value::as_u64).unwrap_or(0);
            let cached = field("cached_input_tokens");
            // #31: Codex DOES have a cache-write tier — `cache_write_input_tokens` rides in
            // the modern payload (1,950 of 1,977 lines on a real store), a sibling of
            // `cached_input_tokens` and by the same naming a SUBSET of `input_tokens`, so
            // it is split out the same way. Every observed value is 0 so far, which is why
            // hardcoding 0 here used to be output-identical; the moment Codex emits a
            // nonzero write, this records it instead of silently dropping the tier. (If
            // nonzero evidence ever contradicts the subset reading, this is the spot.)
            let cache_write = field("cache_write_input_tokens");
            let now = TokenCounts {
                input: field("input_tokens")
                    .saturating_sub(cached)
                    .saturating_sub(cache_write),
                cache_creation: cache_write,
                cache_read: cached,
                output: field("output_tokens"),
                ..Default::default()
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
                    let lcache_write = lfield("cache_write_input_tokens");
                    self.last_total = TokenCounts {
                        input: now.input.saturating_sub(
                            lfield("input_tokens")
                                .saturating_sub(lcached)
                                .saturating_sub(lcache_write),
                        ),
                        cache_creation: now.cache_creation.saturating_sub(lcache_write),
                        cache_read: now.cache_read.saturating_sub(lcached),
                        output: now.output.saturating_sub(lfield("output_tokens")),
                        ..Default::default()
                    };
                }
            }
            // Bank the increment against the CURRENT model. `saturating_sub` because a total
            // that goes backwards (a reset) must contribute nothing, never wrap.
            let delta = TokenCounts {
                input: now.input.saturating_sub(self.last_total.input),
                cache_creation: now
                    .cache_creation
                    .saturating_sub(self.last_total.cache_creation),
                cache_read: now.cache_read.saturating_sub(self.last_total.cache_read),
                output: now.output.saturating_sub(self.last_total.output),
                ..Default::default()
            };
            // A cumulative jump can cover several unseen requests. Only the last request
            // matching the entire increment establishes a context threshold for this delta.
            if let Some(last) = value.pointer("/payload/info/last_token_usage") {
                let n = |key: &str| last.get(key).and_then(Value::as_u64).unwrap_or(0);
                let request = TokenCounts {
                    input: n("input_tokens")
                        .saturating_sub(n("cached_input_tokens"))
                        .saturating_sub(n("cache_write_input_tokens")),
                    cache_creation: n("cache_write_input_tokens"),
                    cache_read: n("cached_input_tokens"),
                    output: n("output_tokens"),
                    ..Default::default()
                };
                if request == delta && delta != TokenCounts::default() {
                    self.request_pricing = Some(RequestPricing {
                        tier: self
                            .runtime
                            .service_tier
                            .as_deref()
                            .map(ServiceTier::from_recorded)
                            .unwrap_or_default(),
                        long_context: last
                            .get("input_tokens")
                            .and_then(Value::as_u64)
                            .map(|n| n > 272_000),
                        tier_confirmed: false,
                    });
                }
            }
            if delta != TokenCounts::default() {
                let id = format!(
                    "{}/{}/{}",
                    value.get("timestamp").and_then(Value::as_str).unwrap_or(""),
                    field("input_tokens"),
                    field("output_tokens")
                );
                // The request's own output is `last_token_usage`'s, reasoning included; the
                // counter's delta may span several requests, so a rollout without it gives none.
                // Its times are the clock's; with no item read, the reading's alone.
                let at = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(epoch_secs);
                let (sent, first, last, lines) = self.clock.counted(at);
                self.answer = Some(AnswerTiming {
                    id: id.clone(),
                    model: self.model.clone(),
                    provider: self.provider.clone(),
                    sent,
                    first,
                    last,
                    lines,
                    output_tokens: value
                        .pointer("/payload/info/last_token_usage/output_tokens")
                        .and_then(Value::as_u64),
                });
                self.usage_id = Some(id);
            }
            let e = self.per_model.entry(self.model.clone()).or_default();
            e.input += now.input.saturating_sub(self.last_total.input);
            e.cache_creation += now
                .cache_creation
                .saturating_sub(self.last_total.cache_creation);
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
            "last_context_tokens": self.last_context_tokens,
            "pending_compaction_pre_tokens": self.pending_compaction_pre_tokens,
            "model": self.model,
            "first_model": self.first_model,
            "runtime": self.runtime,
            "provider": self.provider,
            "clock": self.clock,
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
        self.last_context_tokens = state.get("last_context_tokens").and_then(Value::as_u64);
        self.pending_compaction_pre_tokens = state
            .get("pending_compaction_pre_tokens")
            .and_then(Value::as_u64);
        if let Some(m) = state.get("model").and_then(Value::as_str) {
            self.model = m.to_string();
        }
        if let Some(m) = state.get("first_model").and_then(Value::as_str) {
            self.first_model = m.to_string();
        }
        if let Some(p) = state.get("provider").and_then(Value::as_str) {
            self.provider = Some(p.to_string());
        }
        // A cursor parked before the clock existed may sit inside an answer: the next one's
        // start is not known.
        self.clock = state
            .get("clock")
            .and_then(|v| serde_json::from_value::<ResponseClock>(v.clone()).ok())
            .unwrap_or_else(ResponseClock::resumed_blind);
        if let Some(mut runtime) = state
            .get("runtime")
            .and_then(|v| serde_json::from_value::<RuntimeInfo>(v.clone()).ok())
        {
            runtime.recorded = std::mem::take(&mut self.runtime.recorded);
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

/// What one rollout line is to [`ResponseClock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RolloutLine {
    /// An item the model sent: its reasoning, a message, a call.
    Item,
    /// A message sent to the model that opens a request: the prompt, or the instructions with it.
    Prompt,
    /// Anything else the next request carries: a call's output, a compaction's summary, another
    /// agent's message.
    Sent,
    /// A `token_usage_record`: the response ended.
    Ended,
    Other,
}

impl RolloutLine {
    fn of(value: &Value) -> Self {
        let payload = |key: &str| value.pointer(&format!("/payload/{key}"))?.as_str();
        match value.get("type").and_then(Value::as_str) {
            Some("response_item") => match payload("type") {
                Some("message") => match payload("role") {
                    Some("assistant") => Self::Item,
                    Some(_) => Self::Prompt,
                    None => Self::Other,
                },
                Some(
                    "reasoning"
                    | "function_call"
                    | "custom_tool_call"
                    | "local_shell_call"
                    | "web_search_call"
                    | "tool_search_call"
                    | "image_generation_call",
                ) => Self::Item,
                Some("compaction" | "agent_message") => Self::Sent,
                Some(kind) if kind.ends_with("_output") => Self::Sent,
                _ => Self::Other,
            },
            Some("compacted") => Self::Sent,
            Some("token_usage_record") => Self::Ended,
            _ => Self::Other,
        }
    }
}

/// Times each Codex answer (#s341) from the rollout around the reading that counts it. Codex
/// writes each item the model sends (its reasoning, a message, a call) as a line when the item
/// ends, and each line sent with the next request (the prompt, a call's output) as it records
/// it; from 0.147 (2026-09-03) a `token_usage_record` marks when the response ended; the
/// `token_count` reading comes after the calls the answer made have run, and the next request
/// goes out after it.
///
/// So an answer is the items read since the reading before: `sent` is the latest line sent to
/// the model, or the latest reading, before its first item; `first` that item's time; and
/// `last` the response's end where a `token_usage_record` says it, else its last item's where
/// the fold reads every item. A session Codex 0.147 or later started draws each call from its
/// mirror and leaves the transport's lines out of the fold (an `exec` or `wait` call and its
/// receipt, which the preprocessor ignores): there the reading counts as sent, as the outputs
/// of many calls are not here to say it, and an answer whose end is not recorded has no `last`,
/// as its last item may be a call the fold never saw (0.147's alphas wrote no
/// `token_usage_record` before 2026-09-03). Never the reading as the end: it waits for the
/// answer's calls, 2.5 s after the last item at the median and 60 s at the 99th percentile,
/// against 0.2 s and 5 s for an answer that made none (163,925 readings that moved the counter,
/// in 1,445 rollouts on one box, 2026-11-01).
///
/// A forked thread's rollout copies the parent's items, stamped with the time of the fork, and
/// none of its readings: a prompt that arrives while an answer has not ended drops that answer
/// (274 of those readings lost theirs). A response that ended with no reading before the next
/// request's first item is dropped too (33), and so is one that ended twice with no item between,
/// as which response the reading counts is not told. A rollout written before
/// `token_usage_record` that misses a reading folds the requests it counts into one answer.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct ResponseClock {
    /// The time of the latest line sent to the model (`None` if it carried none): the next
    /// answer's `sent`.
    #[serde(default)]
    prev: Option<f64>,
    /// The answer whose items were read, and its reading not yet.
    #[serde(default)]
    open: Option<OpenAnswer>,
    /// The answer whose item comes next may have begun before what was read (a fold resumed
    /// from a state that held no clock, or a line that could not be read): its `sent` and
    /// `first` are not known.
    #[serde(default)]
    blind: bool,
    /// The session leaves the transport's lines out of the fold
    /// ([`transport_ignored`](super::model::transport_ignored)), so an answer's last item read
    /// may not be its last: with no recorded end, its end is not known.
    #[serde(default)]
    items_unseen: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct OpenAnswer {
    sent: Option<f64>,
    first: Option<f64>,
    /// The time of its last item read.
    last: Option<f64>,
    lines: u32,
    /// A `token_usage_record` said its response ended.
    #[serde(default)]
    ended: bool,
    /// When, where that record carried a time.
    #[serde(default)]
    end: Option<f64>,
}

impl ResponseClock {
    /// A clock for a fold resumed from a state written before it existed: see `blind`. The
    /// session's version was read before the cursor, so whether it hides items is not known.
    fn resumed_blind() -> Self {
        Self {
            blind: true,
            items_unseen: true,
            ..Self::default()
        }
    }

    /// Fold one line: its time, and what it is to the answers.
    fn line(&mut self, ts: Option<f64>, line: RolloutLine) {
        match line {
            RolloutLine::Item => match &mut self.open {
                Some(open) if !open.ended => {
                    open.last = ts.or(open.last);
                    open.lines += 1;
                }
                _ => {
                    let blind = std::mem::take(&mut self.blind);
                    self.open = Some(OpenAnswer {
                        sent: if blind { None } else { self.prev },
                        first: if blind { None } else { ts },
                        last: ts,
                        lines: 1,
                        ended: false,
                        end: None,
                    });
                }
            },
            RolloutLine::Prompt => {
                if self.open.as_ref().is_some_and(|open| !open.ended) {
                    self.open = None;
                }
                self.prev = ts;
            }
            RolloutLine::Sent => self.prev = ts,
            RolloutLine::Ended => match &mut self.open {
                Some(open) if !open.ended => {
                    open.ended = true;
                    open.end = ts;
                }
                Some(_) => self.open = None,
                None => {}
            },
            RolloutLine::Other => {}
        }
    }

    /// A line that could not be read: it may have been an item, the end of a response or a line
    /// sent to the model, so neither the answer being read nor the next one's start is known.
    fn unread(&mut self) {
        self.prev = None;
        self.open = None;
        self.blind = true;
    }

    /// A reading that counted an answer, at `ts`: the answer's `sent`, `first`, `last` and
    /// `lines`, which the clock lets go, or the reading's time alone when no item was read. The
    /// reading also says when the next request went out, as it comes after every output of the
    /// calls the answer made, and some of those never reach this fold (see the type).
    fn counted(&mut self, ts: Option<f64>) -> (Option<f64>, Option<f64>, Option<f64>, u32) {
        self.prev = ts;
        match self.open.take() {
            Some(open) => {
                let last = open.end.or(open.last.filter(|_| !self.items_unseen));
                (open.sent, open.first, last, open.lines)
            }
            None => (None, None, ts, 1),
        }
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

/// One window of a `token_count` reading. `resets_at` is absolute epoch seconds; an older Codex
/// wrote `resets_in_seconds`, relative to the event, which `observed_at` (the event's time) turns
/// absolute (#s14). Without the event's time a relative reset stays unknown rather than wrong.
fn rate_limit_window(value: Option<&Value>, observed_at: Option<i64>) -> Option<RateLimitWindow> {
    let value = value?;
    let resets_in = || {
        let secs = value.get("resets_in_seconds").and_then(Value::as_i64)?;
        Some(observed_at? + secs)
    };
    Some(RateLimitWindow {
        used_percent: value.get("used_percent").and_then(Value::as_f64)?,
        window_minutes: value
            .get("window_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        resets_at: value
            .get("resets_at")
            .and_then(Value::as_i64)
            .or_else(resets_in),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #s14: a session's reading is dated by the LAST `token_count` event that carried one, and a
    /// window's reset is absolute epoch seconds whichever shape the client wrote: `resets_at` as
    /// is, an older client's relative `resets_in_seconds` counted from that event.
    #[test]
    fn a_rate_limit_reading_is_dated_and_its_resets_are_absolute() {
        let reading = |ts: &str, primary: Value, secondary: Value| {
            serde_json::json!({"timestamp": ts, "type": "event_msg", "payload": {
                "type": "token_count", "info": null,
                "rate_limits": {"plan_type": "plus", "primary": primary, "secondary": secondary}}})
        };
        let mut acc = CodexMetricsAcc::default();
        acc.push(&reading(
            "2026-10-04T01:00:00.250Z",
            serde_json::json!({"used_percent": 10.0, "window_minutes": 300, "resets_in_seconds": 600}),
            serde_json::json!({"used_percent": 20.0, "window_minutes": 10080, "resets_in_seconds": 86400}),
        ));
        let first = acc.clone_runtime_limits();
        let one_am = parse_ts("2026-10-04T01:00:00Z").unwrap();
        assert_eq!(first.observed_at, Some(one_am));
        assert_eq!(first.primary.unwrap().resets_at, Some(one_am + 600));
        acc.push(&reading(
            "2026-10-04T02:00:00Z",
            serde_json::json!({"used_percent": 12.0, "window_minutes": 300, "resets_at": 1_790_000_000}),
            serde_json::json!({"used_percent": 21.0, "window_minutes": 10080, "resets_in_seconds": 3600}),
        ));
        let limits = acc.finish().runtime.rate_limits.unwrap();
        let two_am = parse_ts("2026-10-04T02:00:00Z").unwrap();
        assert_eq!(
            limits.observed_at,
            Some(two_am),
            "the last reading's event time"
        );
        assert_eq!(
            limits.primary.unwrap().resets_at,
            Some(1_790_000_000),
            "absolute, as written"
        );
        assert_eq!(
            limits.secondary.unwrap().resets_at,
            Some(two_am + 3600),
            "relative, counted from its event"
        );
    }

    /// #s14: without the event's time a relative reset is unknown, never a guess.
    #[test]
    fn an_undated_relative_reset_stays_unknown() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&serde_json::json!({"type": "event_msg", "payload": {"type": "token_count",
            "rate_limits": {"primary": {"used_percent": 5.0, "window_minutes": 300, "resets_in_seconds": 60}}}}));
        let limits = acc.finish().runtime.rate_limits.unwrap();
        assert_eq!(limits.observed_at, None);
        assert_eq!(limits.primary.unwrap().resets_at, None);
    }

    impl CodexMetricsAcc {
        fn clone_runtime_limits(&self) -> RateLimits {
            self.runtime.rate_limits.clone().expect("a reading")
        }
    }

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

    /// Codex's compaction boundary itself carries no token fields. The immediately preceding
    /// usage snapshot is the full context and the immediately following zero-work snapshot is
    /// the compacted continuation. Pair those two readings across the boundary — including a
    /// durable-cursor round trip — so the shared Claude-shaped usage statistic stays exact.
    #[test]
    fn compactions_count_and_sum_dropped_context_tokens() {
        fn usage(cumulative_input: u64, context_tokens: u64) -> Value {
            serde_json::json!({"type":"event_msg","payload":{
                "type":"token_count",
                "info":{
                    "model_context_window":258400,
                    "last_token_usage":{
                        "input_tokens":context_tokens,
                        "total_tokens":context_tokens
                    },
                    "total_token_usage":{
                        "input_tokens":cumulative_input,
                        "cached_input_tokens":0,
                        "output_tokens":0
                    }
                }
            }})
        }

        let mut acc = CodexMetricsAcc::default();
        acc.push(&usage(240_000, 240_000));
        acc.push(&serde_json::json!({"type":"compacted","payload":{"window_number":1}}));
        acc.push(&usage(240_000, 18_000));
        acc.push(&usage(452_000, 230_000));
        acc.push(&serde_json::json!({"type":"compacted","payload":{"window_number":2}}));

        // A live fold can checkpoint after the boundary but before Codex writes the post-
        // compaction usage record. Both the count and pending pre-token reading must survive.
        let state = acc.state();
        let mut resumed = CodexMetricsAcc::default();
        resumed.restore(&state);
        resumed.push(&usage(452_000, 17_000));
        let metrics = resumed.finish();

        assert_eq!(metrics.compactions(), (2, 435_000));
        assert_eq!(
            metrics.compaction_label().as_deref(),
            Some("2× compacted, 435.0k dropped")
        );
        assert_eq!(metrics.runtime.context_used_tokens, Some(17_000));
        assert_eq!(metrics.context_left_percent(), Some(94));
        assert!(metrics
            .footer_segments()
            .iter()
            .any(|(text, _)| text == "2× compacted, 435.0k dropped"));
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

    /// #31: the modern payload carries `cache_write_input_tokens` (1,950 of 1,977 lines on
    /// a real store — every observed value 0 so far). A nonzero write must be banked into
    /// `cache_creation` and split OUT of input like its sibling `cached_input_tokens`, not
    /// dropped by a hardcoded 0. This fixture is nonzero on purpose: the all-zero corpus
    /// cannot tell the fix from the old hardcode.
    #[test]
    fn a_cache_write_tier_is_banked_not_dropped() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&turn_context("gpt-5.6"));
        acc.push(
            &serde_json::json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {
                    "input_tokens": 10_000, "cached_input_tokens": 4_000,
                    "cache_write_input_tokens": 1_000, "output_tokens": 500,
                }
            }}}),
        );
        let m = acc.finish();
        assert_eq!(m.input_tokens, 5_000, "input minus BOTH cache subsets");
        assert_eq!(m.cache_creation_tokens, 1_000, "the write tier is recorded");
        assert_eq!(m.cache_read_tokens, 4_000);
        assert_eq!(m.output_tokens, 500);
    }

    /// #31 × the fork baseline: an inherited total's cache-write share is the parent's,
    /// priced in the parent's own rollout — the first event banks only its own
    /// `last_token_usage` write share, mirroring the other three counters.
    #[test]
    fn an_inherited_cache_write_total_is_a_baseline_not_a_delta() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&turn_context("gpt-5.6"));
        acc.push(
            &serde_json::json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {
                    "input_tokens": 1_001_000, "cached_input_tokens": 400,
                    "cache_write_input_tokens": 5_100, "output_tokens": 50,
                },
                "last_token_usage": {
                    "input_tokens": 1_000, "cached_input_tokens": 400,
                    "cache_write_input_tokens": 100, "output_tokens": 50,
                }
            }}}),
        );
        let m = acc.finish();
        assert_eq!(
            m.cache_creation_tokens, 100,
            "own write share only — the inherited 5,000 stays with the parent"
        );
        assert_eq!(
            m.input_tokens, 500,
            "own input minus own cached and own write"
        );
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
        // OpenAI documents gpt-5.6 as the gpt-5.6-sol alias: $4 input, $0.40 cached, $20 output.
        let cost = metrics.cost_usd.expect("gpt-5.6 should be priced");
        let expected = (100.0 * 4.0 + 200.0 * 0.4 + 80.0 * 20.0) / 1e6;
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
            turn_context("gpt-5-mini"), // and later switches
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
        // those tokens were produced — not `gpt-5-mini`, the one merely in force at the end.
        //   gpt-5.6-sol: inherited (100 cumulative, of which 40 cached → 60 in / 40 cache / 10 out)
        //          + its own (300→400 in, 200→250 cached, 30→45 out)
        assert_eq!(m.per_model["gpt-5.6-sol"].input, 150, "400 - 250 cached");
        assert_eq!(m.per_model["gpt-5.6-sol"].cache_read, 250);
        assert_eq!(m.per_model["gpt-5.6-sol"].output, 45);
        assert_eq!(m.per_model["gpt-5-mini"].output, 10, "55 - 45");
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
        tail.push(&turn_context("gpt-5-mini")); // the suffix only ever sees the SECOND name
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

#[cfg(test)]
mod request_pricing_tests {
    use super::*;
    use serde_json::json;
    fn usage(total: u64, input: u64, cached: u64) -> Value {
        json!({"type":"event_msg","payload":{"type":"token_count","info":{
            "total_token_usage":{"input_tokens":total,"cached_input_tokens":cached,"output_tokens":10},
            "last_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":10}
        }}})
    }
    #[test]
    fn threshold_uses_full_request_input_and_duplicate_usage_has_no_context() {
        for (input, long) in [(272_000, false), (272_001, true)] {
            let mut acc = CodexMetricsAcc::default();
            acc.push(&json!({"type":"turn_context","payload":{"model":"gpt-6-astra","service_tier":"priority"}}));
            let event = usage(input + 1_000_000, input, 270_000);
            acc.push(&event);
            assert_eq!(
                acc.request_pricing,
                Some(RequestPricing {
                    tier: ServiceTier::Fast,
                    long_context: Some(long),
                    tier_confirmed: false
                })
            );
            let mut resumed = CodexMetricsAcc::default();
            resumed.restore(&acc.state());
            resumed.push(&event);
            assert_eq!(resumed.request_pricing, None);
            assert_eq!(resumed.totals(), acc.totals());
            resumed.push(&json!({"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":null}}}));
            assert_eq!(resumed.runtime.service_tier, None);
        }
    }
    /// Ultrafast (the claude-replay queue's #5): a turn recorded with `service_tier` "ultrafast"
    /// on gpt-6-astra carries the tier — an estimate, as every client-set tier is until the
    /// provider confirms it. The engine's `ultrafast_is_six_times_standard_where_the_page_lists_it`
    /// pins what that tier prices at (6x Standard); before it was known, this request priced at
    /// Standard with only the estimate flag.
    #[test]
    fn an_ultrafast_turn_carries_its_tier() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&json!({"type":"turn_context","payload":{"model":"gpt-6-astra","service_tier":"ultrafast"}}));
        acc.push(&usage(200_000, 200_000, 0));
        let pricing = acc.request_pricing.expect("one request's billing context");
        assert_eq!(
            pricing,
            RequestPricing {
                tier: ServiceTier::Ultrafast,
                long_context: Some(false),
                tier_confirmed: false
            }
        );
    }
    #[test]
    fn missing_last_usage_and_multi_request_jumps_are_unknown() {
        let mut acc = CodexMetricsAcc::default();
        acc.push(&usage(100, 100, 0));
        acc.push(&usage(900_000, 300_000, 0));
        assert_eq!(acc.request_pricing, None);
        acc.push(&json!({"type":"turn_context","payload":{"model":"gpt-6-astra"}}));
        assert_eq!(acc.request_pricing, None);
    }
}

#[cfg(test)]
mod answer_tests {
    use super::*;

    /// #s341: each reading that moves the counter is one answer: the turn's model, the session's
    /// provider, the reading's own output (`last_token_usage`, reasoning included) and its time.
    /// With no item of the model's read before it, `sent` and `first` are `None`; a reading that
    /// moves nothing is no answer; and the provider crosses a resume.
    #[test]
    fn each_reading_that_moves_the_counter_is_a_timed_answer() {
        let reading = |ts: &str, total_out: u64, last_out: u64| {
            serde_json::json!({"timestamp": ts, "type": "event_msg", "payload": {
                "type": "token_count", "info": {
                    "last_token_usage": {"input_tokens": 100, "output_tokens": last_out,
                        "reasoning_output_tokens": 40},
                    "total_token_usage": {"input_tokens": 100, "output_tokens": total_out}}}})
        };
        let mut acc = CodexMetricsAcc::default();
        acc.push(&serde_json::json!({"type": "session_meta", "payload": {
            "id": "s", "model_provider": "openai"}}));
        acc.push(&serde_json::json!({"type": "turn_context", "payload": {"model": "gpt-5.6-sol"}}));
        acc.push(&reading("2026-10-30T10:00:03.500Z", 250, 250));
        let a = acc.answer.clone().expect("an answer");
        assert_eq!(a.id, acc.usage_id.clone().unwrap(), "the reading's id");
        assert_eq!(a.model, "gpt-5.6-sol");
        assert_eq!(a.provider.as_deref(), Some("openai"));
        assert_eq!((a.sent, a.first), (None, None));
        assert_eq!(a.last, epoch_secs("2026-10-30T10:00:03.500Z"));
        assert_eq!((a.lines, a.output_tokens), (1, Some(250)));
        acc.push(&reading("2026-10-30T10:00:04Z", 250, 250));
        assert_eq!(acc.answer, None, "a reading that moves nothing");
        let mut after = CodexMetricsAcc::default();
        after.restore(&acc.state());
        // The counter moved 150 over two requests the rollout did not each record; the last one
        // wrote 120.
        after.push(&reading("2026-10-30T10:00:09Z", 400, 120));
        let b = after.answer.clone().expect("an answer");
        assert_eq!(b.provider.as_deref(), Some("openai"), "across a resume");
        assert_eq!(
            b.output_tokens,
            Some(120),
            "the request's own, not the counter's"
        );
        after.push(&serde_json::json!({"type": "turn_context", "payload": {"model": "x"}}));
        assert_eq!(after.answer, None, "cleared on every push");
        let mut unnamed = CodexMetricsAcc::default();
        unnamed.push(&serde_json::json!({"type": "session_meta", "payload": {
            "id": "s", "model_provider": ""}}));
        unnamed.push(&reading("2026-10-30T10:00:03Z", 10, 10));
        assert_eq!(
            unnamed.answer.unwrap().provider,
            None,
            "an empty name is none"
        );
    }

    /// Rollout lines for the clock, at `HH:MM:SS.mmm` on one day. Synthetic — never built from a
    /// real rollout.
    fn at(hms: &str) -> String {
        format!("2026-10-30T{hms}Z")
    }
    fn secs(hms: &str) -> Option<f64> {
        epoch_secs(&at(hms))
    }
    fn prompt(hms: &str, role: &str) -> Value {
        serde_json::json!({"timestamp": at(hms), "type": "response_item", "payload": {
            "type": "message", "role": role, "content": [{"type": "input_text", "text": "go"}]}})
    }
    fn item(hms: &str, kind: &str) -> Value {
        let mut payload = serde_json::json!({"type": kind});
        if kind == "message" {
            payload["role"] = "assistant".into();
        }
        serde_json::json!({"timestamp": at(hms), "type": "response_item", "payload": payload})
    }
    fn output(hms: &str) -> Value {
        serde_json::json!({"timestamp": at(hms), "type": "response_item", "payload": {
            "type": "custom_tool_call_output", "call_id": "c", "output": "ok"}})
    }
    fn ended(hms: &str) -> Value {
        serde_json::json!({"timestamp": at(hms), "type": "token_usage_record", "payload": {
            "response_id": "resp_x", "usage": {"output_tokens": 1}}})
    }
    fn reading(hms: &str, total_out: u64, last_out: u64) -> Value {
        serde_json::json!({"timestamp": at(hms), "type": "event_msg", "payload": {
            "type": "token_count", "info": {
                "last_token_usage": {"input_tokens": 100, "output_tokens": last_out},
                "total_token_usage": {"input_tokens": total_out, "output_tokens": total_out}}}})
    }

    /// Every answer the pushes hand out, as a consumer reading the whole file collects them.
    fn answers(acc: &mut CodexMetricsAcc, lines: &[Value]) -> Vec<AnswerTiming> {
        let mut out = Vec::new();
        for line in lines {
            acc.push(line);
            out.extend(acc.answer.clone());
        }
        out
    }

    /// An answer runs from the line sent before its first item to its response's end, as the
    /// `token_usage_record` says it: never to the reading, written after the calls it made ran.
    /// The next answer goes out after that reading, which follows the last call's output.
    #[test]
    fn an_answer_runs_from_its_request_to_its_response_s_end() {
        let got = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:04.000", "reasoning"),
                item("10:00:05.000", "message"),
                item("10:00:06.000", "custom_tool_call"),
                ended("10:00:06.200"),
                output("10:00:09.000"),
                reading("10:00:09.001", 500, 500),
                item("10:00:12.000", "reasoning"),
                item("10:00:13.000", "function_call"),
                ended("10:00:13.150"),
                output("10:00:20.000"),
                reading("10:00:20.001", 900, 400),
            ],
        );
        assert_eq!(got.len(), 2, "{got:?}");
        let a = &got[0];
        assert_eq!(a.sent, secs("10:00:00.000"));
        assert_eq!(a.first, secs("10:00:04.000"));
        assert_eq!(
            a.last,
            secs("10:00:06.200"),
            "the response's end, not the reading"
        );
        assert_eq!((a.lines, a.output_tokens), (3, Some(500)));
        let b = &got[1];
        assert_eq!(b.sent, secs("10:00:09.001"), "after the reading");
        assert_eq!(
            (b.first, b.last, b.lines, b.output_tokens),
            (secs("10:00:12.000"), secs("10:00:13.150"), 2, Some(400))
        );
    }

    /// From 0.147 the fold never sees the transport's lines (an `exec` call and its receipt,
    /// which the preprocessor ignores): the reading after the call says when the next request
    /// went out, not the prompt before it.
    #[test]
    fn the_reading_says_when_the_next_request_went_out() {
        let got = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:02.000", "reasoning"),
                ended("10:00:03.000"),
                // The `exec` call ran for 30 s; it and its receipt never reach the fold.
                reading("10:00:33.000", 300, 300),
                item("10:00:36.000", "reasoning"),
                item("10:00:37.000", "message"),
                ended("10:00:37.500"),
                reading("10:00:37.600", 700, 400),
            ],
        );
        assert_eq!(got[0].sent, secs("10:00:00.000"));
        assert_eq!(
            (got[1].sent, got[1].last),
            (secs("10:00:33.000"), secs("10:00:37.500")),
            "the reading, not the prompt"
        );
    }

    /// A session Codex 0.147 or later started leaves the transport's calls out of the fold, so
    /// an answer whose end is not recorded may have ended in a call never read: it has no
    /// `last`. One whose end is recorded, or any in a session before 0.147, is timed.
    #[test]
    fn where_calls_go_unread_an_answer_with_no_recorded_end_has_no_last() {
        let lines = |version: &str| {
            vec![
                serde_json::json!({"type": "session_meta", "payload": {
                    "id": "s", "model_provider": "openai", "cli_version": version}}),
                prompt("10:00:00.000", "user"),
                item("10:00:02.000", "reasoning"),
                // An `exec` call, never pushed, ended this answer.
                reading("10:00:20.000", 300, 300),
                item("10:00:22.000", "reasoning"),
                ended("10:00:25.000"),
                reading("10:00:25.100", 700, 400),
            ]
        };
        let unread = answers(&mut CodexMetricsAcc::default(), &lines("0.147.0-alpha.6.5"));
        assert_eq!(
            (unread[0].sent, unread[0].last),
            (secs("10:00:00.000"), None)
        );
        assert_eq!(unread[1].last, secs("10:00:25.000"), "its end recorded");
        let older = answers(&mut CodexMetricsAcc::default(), &lines("0.146.2"));
        assert_eq!(older[0].last, secs("10:00:02.000"), "every item read");
    }

    /// A rollout written before `token_usage_record` ends an answer at its last item: the reading
    /// came after the call it made ran for 26 seconds.
    #[test]
    fn with_no_end_recorded_an_answer_ends_at_its_last_item() {
        let got = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:03.000", "reasoning"),
                item("10:00:04.000", "function_call"),
                output("10:00:30.000"),
                reading("10:00:30.001", 300, 300),
            ],
        );
        assert_eq!(
            (got[0].sent, got[0].last),
            (secs("10:00:00.000"), secs("10:00:04.000"))
        );
    }

    /// Every line sent with a request sets when the next went out: a compaction's summary and
    /// the instructions Codex sends as a developer message as well as a prompt; a web search is
    /// one of the model's items.
    #[test]
    fn each_line_sent_to_the_model_moves_the_next_request_s_time() {
        let compacted = serde_json::json!({"timestamp": at("10:00:10.000"), "type": "compacted",
            "payload": {"message": ""}});
        let got = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:01.000", "message"),
                reading("10:00:02.000", 300, 300),
                compacted,
                item("10:00:12.000", "message"),
                reading("10:00:13.000", 600, 300),
                prompt("10:00:20.000", "developer"),
                item("10:00:21.000", "web_search_call"),
                item("10:00:22.000", "message"),
                reading("10:00:23.000", 900, 300),
            ],
        );
        let sent: Vec<_> = got.iter().map(|a| a.sent).collect();
        assert_eq!(
            sent,
            [
                secs("10:00:00.000"),
                secs("10:00:10.000"),
                secs("10:00:20.000")
            ]
        );
        assert_eq!(got[2].lines, 2);
    }

    /// A forked thread's rollout opens with the parent's items stamped with the fork's time and
    /// none of its readings: its own prompt drops them, and its first answer is timed from it.
    #[test]
    fn a_fork_s_copied_items_are_no_answer() {
        let got = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.001", "user"),
                item("10:00:00.002", "reasoning"),
                item("10:00:00.003", "custom_tool_call"),
                output("10:00:00.004"),
                item("10:00:00.005", "message"),
                prompt("10:00:30.000", "user"),
                item("10:00:33.000", "reasoning"),
                item("10:00:35.000", "message"),
                ended("10:00:35.200"),
                reading("10:00:35.300", 400, 400),
            ],
        );
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].sent, got[0].first, got[0].last, got[0].lines),
            (
                secs("10:00:30.000"),
                secs("10:00:33.000"),
                secs("10:00:35.200"),
                2
            )
        );
    }

    /// A response that ended with no reading before the next request's first item is dropped,
    /// and the reading times the request it counts; one that ended twice with no item between
    /// is not timed, as which response the reading counts is not told.
    #[test]
    fn a_response_with_no_reading_is_dropped_and_one_ended_twice_is_not_timed() {
        let got = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:01.000", "function_call"),
                ended("10:00:02.000"),
                output("10:00:03.000"),
                item("10:00:05.000", "message"),
                ended("10:00:06.000"),
                reading("10:00:06.100", 700, 300),
            ],
        );
        assert_eq!(
            (got[0].sent, got[0].first, got[0].last, got[0].lines),
            (
                secs("10:00:03.000"),
                secs("10:00:05.000"),
                secs("10:00:06.000"),
                1
            )
        );
        let twice = answers(
            &mut CodexMetricsAcc::default(),
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:01.000", "message"),
                ended("10:00:02.000"),
                ended("10:00:03.000"),
                reading("10:00:03.100", 300, 300),
            ],
        );
        assert_eq!((twice[0].sent, twice[0].first), (None, None));
        assert_eq!(twice[0].last, secs("10:00:03.100"), "the reading's alone");
    }

    /// A fold resumed from its state anywhere in a rollout hands out exactly what a whole-file
    /// fold does: the clock is in the state.
    #[test]
    fn a_resume_anywhere_times_answers_as_a_whole_file_fold() {
        let lines = [
            prompt("10:00:00.100", "user"),
            item("10:00:01.200", "reasoning"),
            item("10:00:02.300", "custom_tool_call"),
            ended("10:00:02.450"),
            output("10:00:04.000"),
            reading("10:00:04.001", 300, 300),
            item("10:00:05.000", "reasoning"),
            item("10:00:06.000", "message"),
            ended("10:00:06.700"),
            reading("10:00:06.800", 700, 400),
        ];
        let whole = answers(&mut CodexMetricsAcc::default(), &lines);
        assert_eq!(whole.len(), 2);
        assert!(whole.iter().all(|a| a.sent.is_some() && a.first.is_some()));
        for cut in 1..lines.len() {
            let mut before = CodexMetricsAcc::default();
            let mut got = answers(&mut before, &lines[..cut]);
            let parked = serde_json::to_string(&before.state()).unwrap();
            let mut after = CodexMetricsAcc::default();
            after.restore(&serde_json::from_str(&parked).unwrap());
            got.extend(answers(&mut after, &lines[cut..]));
            assert_eq!(got, whole, "resumed after line {cut}");
        }
    }

    /// A cursor parked before the clock existed may sit inside an answer, and a line that could
    /// not be read may have been an item or the request: the next answer's `sent` and `first`
    /// are `None`, and the answers after it are timed whole.
    #[test]
    fn a_cursor_from_before_the_clock_or_an_unread_line_leaves_the_next_start_unknown() {
        let tail = [
            item("10:00:01.000", "message"),
            ended("10:00:02.000"),
            reading("10:00:02.100", 300, 300),
            prompt("10:00:03.000", "user"),
            item("10:00:04.000", "message"),
            ended("10:00:05.000"),
            reading("10:00:05.100", 600, 300),
        ];
        let check = |got: Vec<AnswerTiming>| {
            assert_eq!((got[0].sent, got[0].first), (None, None));
            assert_eq!(got[0].last, secs("10:00:02.000"));
            assert_eq!(
                (got[1].sent, got[1].first),
                (secs("10:00:03.000"), secs("10:00:04.000"))
            );
        };
        let mut old = CodexMetricsAcc::default();
        let mut parked = old.state();
        parked.as_object_mut().unwrap().remove("clock");
        old.restore(&parked);
        check(answers(&mut old, &tail));

        let mut unread = CodexMetricsAcc::default();
        answers(
            &mut unread,
            &[
                prompt("10:00:00.000", "user"),
                item("10:00:00.500", "reasoning"),
            ],
        );
        unread.malformed_line();
        check(answers(&mut unread, &tail));
    }
}
