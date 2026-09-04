//! **Claude's token/cost metrics adapter** — the Claude half of the shared `metrics`
//! engine (mirrors `codex_metrics`). Folds each Claude transcript line's `/message/usage`
//! into a running [`MetricsAcc`]; the agent-neutral [`Metrics`] value, pricing, and footer
//! formatting live in [`claude_replay_engine::seam`].

use claude_replay_engine::seam::{
    credits_cost, parse_ts, total_cost, Metrics, RuntimeInfo, TimeSpan, TokenCounts,
};
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
    /// The API call the last `usage` belonged to — `message.id`, plus the top-level
    /// `requestId` when present. Claude Code writes ONE assistant message as SEVERAL
    /// lines (one per content item: thinking, text, tool_use) and replicates the SAME
    /// `usage` object on every one of them, so summing per line charges a single call
    /// two or three times over. Measured on a real corpus: 43,519 of 87,211
    /// usage-bearing lines (50%) are such repeats, inflating tokens ~2x and cost ~2.07x.
    ///
    /// The repeats are always CONSECUTIVE usage lines — verified across 26,522 groups
    /// with zero interleaved exceptions — so one key is enough state to collapse them,
    /// which keeps the resumable cursor small.
    last_usage_key: Option<String>,
    /// The model `last_usage_key` was credited under. Both must match before a line is
    /// treated as a repeat: after a restore the single-model-per-message invariant can't
    /// be re-verified, and subtracting against another model's figures would under-credit.
    last_usage_model: Option<String>,
    /// What has already been credited for `last_usage_key`, so a repeat adds only the
    /// growth. Identical repeats add nothing; a streamed/retried group that grows
    /// (`output` 6,6,6,6,6,478) credits 478 in total, never the 508 a sum would give.
    credited: TokenCounts,
    /// The latest runtime facts the transcript recorded (#62): Claude Code writes the reasoning
    /// `effort` on each assistant record, a `permission-mode` record whenever the mode is set,
    /// and its `version` on every record — each changes mid-session (effort per request, the
    /// mode per `/permissions`), so the last value wins, as in the Codex family. `recorded`
    /// is declared by the adapter that builds this fold ([`MetricsAcc::recording`]), since the
    /// same fold serves the Qoder family, whose format carries a different set.
    runtime: RuntimeInfo,
}

impl MetricsAcc {
    /// A fold that declares which runtime facts its FORMAT records, by the wire's key names —
    /// the adapter's statement, not the fold's guess.
    pub(crate) fn recording(keys: &[&str]) -> Self {
        let mut acc = Self::default();
        acc.runtime.recorded = keys.iter().map(|k| k.to_string()).collect();
        acc
    }

    /// Fold an **agent-specific** metric into the accumulating [`Metrics::extra`] bag (sum by
    /// key). The seam for a Claude-only counter: call this from [`push`](Self::push) when the
    /// relevant JSON key is seen. Its first users are the compaction counters below (#108).
    pub(crate) fn bump(&mut self, key: &str, n: u64) {
        *self.extra.entry(key.to_string()).or_default() += n;
    }

    pub(crate) fn push(&mut self, v: &Value) {
        let field = |u: &Value, k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        // The runtime snapshot (#62): last value wins. `permission-mode` is its own record type
        // (`{"type":"permission-mode","permissionMode":"bypassPermissions"}`); `effort` rides on
        // the assistant record (top level, beside `message`), `version` on every record. The
        // qwork family's `runtime-config` head carries `reasoningEffort`/`contextWindow` under
        // the same fold — present-and-null in real stores, so they usually stay unknown.
        if v.get("type").and_then(Value::as_str) == Some("permission-mode") {
            remember_string(
                &mut self.runtime.permission_profile,
                v.get("permissionMode"),
            );
        }
        remember_string(&mut self.runtime.reasoning_effort, v.get("effort"));
        remember_string(&mut self.runtime.client_version, v.get("version"));
        if v.get("type").and_then(Value::as_str) == Some("runtime-config") {
            remember_string(&mut self.runtime.reasoning_effort, v.get("reasoningEffort"));
            if let Some(w) = v.get("contextWindow").and_then(Value::as_u64) {
                self.runtime.context_window_tokens = Some(w);
            }
        }
        // Compactions (#108). Both are true counters, so they fold by addition exactly like
        // tokens do — which is what lets a resumed session keep counting from the meta record
        // instead of re-reading the transcript.
        //
        // `compact_dropped` SUMS each boundary's own `pre - post` rather than reading the
        // record's `cumulativeDroppedTokens`: that field is missing from 11 of the 65
        // compactions on this machine, and the summed form reproduces it exactly where both
        // exist (594718-8617 then 725463-7015 = 1304549, the second record's own figure).
        if v.get("subtype").and_then(|x| x.as_str()) == Some("compact_boundary") {
            if let Some(m) = v.get("compactMetadata") {
                self.bump("compactions", 1);
                self.bump(
                    "compact_dropped",
                    field(m, "preTokens").saturating_sub(field(m, "postTokens")),
                );
            }
        }
        // The model THIS message ran on — not `self.model`, which is only the last seen.
        let m = v
            .pointer("/message/model")
            .and_then(|x| x.as_str())
            .unwrap_or(&self.model)
            .to_string();
        if let Some(u) = v.pointer("/message/usage") {
            // Three distinct buckets so the footer can tell them apart: new input,
            // cache writes (new content, cached on first sight), and cache reads
            // (the whole context re-read every turn — the dominant number, kept
            // separate so it doesn't drown out genuinely-new input).
            let seen = TokenCounts {
                input: field(u, "input_tokens"),
                cache_creation: field(u, "cache_creation_input_tokens"),
                cache_read: field(u, "cache_read_input_tokens"),
                output: field(u, "output_tokens"),
            };
            // One API call may arrive as several lines carrying the same `usage`; credit
            // only what grew since this call was last seen (see `last_usage_key`). A line
            // that names no call at all falls back to the old add-everything behaviour, so
            // this is never worse than before it — measured at 0 of 88,027 lines locally,
            // since `message.id` is always present.
            let key = v.pointer("/message/id").and_then(|x| x.as_str()).map(|id| {
                match v.get("requestId").and_then(|x| x.as_str()) {
                    Some(r) => format!("{id}\u{0}{r}"),
                    None => id.to_string(),
                }
            });
            let repeat = key.is_some()
                && self.last_usage_key == key
                && self.last_usage_model.as_deref() == Some(m.as_str());
            let credit = if repeat {
                // Per-field growth. `saturating_sub` because a repeat is never expected to
                // report LESS, and if one ever does, crediting zero beats underflowing.
                TokenCounts {
                    input: seen.input.saturating_sub(self.credited.input),
                    cache_creation: seen
                        .cache_creation
                        .saturating_sub(self.credited.cache_creation),
                    cache_read: seen.cache_read.saturating_sub(self.credited.cache_read),
                    output: seen.output.saturating_sub(self.credited.output),
                }
            } else {
                seen
            };
            // Per-field max, so a repeat that grew in one field and shrank in another
            // can never be re-credited for the shrunk one later in the same group.
            self.credited = if repeat {
                TokenCounts {
                    input: self.credited.input.max(seen.input),
                    cache_creation: self.credited.cache_creation.max(seen.cache_creation),
                    cache_read: self.credited.cache_read.max(seen.cache_read),
                    output: self.credited.output.max(seen.output),
                }
            } else {
                seen
            };
            if !repeat {
                self.last_usage_key = key;
                self.last_usage_model = Some(m.clone());
            }
            *self.per_model.entry(m).or_default() += credit;
            // Qoder bills in `credits` with zeroed token counts and an opaque model alias,
            // so credits are the only honest cost figure. Folded as micro-credits into the
            // reserved `credits_micro` extra key the shared footer reads. Real Claude usage
            // has no such field; current QoderWork usage does, and intentionally shares this
            // path with Qoder CLI.
            // `billable:false` lines are skipped — the published rule is that failed calls
            // deduct nothing — and `credits` (the actual deduction) is read over
            // `original_credits` (the pre-adjustment figure).
            let billable = u.get("billable").and_then(|x| x.as_bool()).unwrap_or(true);
            if let Some(c) = u.get("credits").and_then(|x| x.as_f64()) {
                if billable && c > 0.0 {
                    self.bump("credits_micro", (c * 1e6).round() as u64);
                }
            }
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

    /// The resumable snapshot. Overrides the seam default (which stores bare
    /// [`totals`](Self::totals)) because the repeat-collapsing guard must cross a resume
    /// boundary: park a cursor between lines 2 and 3 of one message's group and a guard
    /// that reset to `None` would treat line 3 as a fresh call and credit it again —
    /// turning the 2x over-count into a smaller, harder-to-spot one.
    pub(crate) fn state(&self) -> Value {
        serde_json::json!({
            "totals": self.totals(),
            "last_usage_key": self.last_usage_key,
            "last_usage_model": self.last_usage_model,
            "credited": self.credited,
            "runtime": self.runtime,
        })
    }

    /// Restore [`state`](Self::state). Unrecognized input is ignored — a cursor is a cache,
    /// and an unreadable one is a cold start, never an error.
    pub(crate) fn restore(&mut self, state: &Value) {
        // Cursors parked by <= v1.69.0 hold a bare `MetricsTotals`, not this object. They
        // must still reseed: silently restoring NOTHING would leave a resumed fold counting
        // only the tail of its transcript, which is a worse (and invisible) error than the
        // over-count this guard exists to fix.
        let totals = match state.get("totals") {
            Some(t) => t.clone(),
            None => state.clone(),
        };
        let Ok((tokens, extra, span)) =
            serde_json::from_value::<claude_replay_engine::seam::MetricsTotals>(totals)
        else {
            return;
        };
        self.reseed(tokens, extra, span);
        self.last_usage_key = state
            .get("last_usage_key")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        self.last_usage_model = state
            .get("last_usage_model")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        self.credited = state
            .get("credited")
            .and_then(|x| serde_json::from_value::<TokenCounts>(x.clone()).ok())
            .unwrap_or_default();
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

    pub(crate) fn finish(self) -> Metrics {
        let duration_secs = self.span.duration_secs();
        // Cost is the SUM over models (#104) — pricing the whole session at one model's rate
        // is wrong whenever it switched, and 4.7% of measured sessions did.
        let (cost_usd, cost_partial) = total_cost(&self.per_model);
        // A credit-billed agent (Qoder) already MEASURED what it charged, so its own figure
        // beats an estimate priced off the token counts it zeroed — converted at the published
        // plan rate (design/qoder-credits-usd.md). `.or(…)` and not a sum: two currencies for
        // one session's money would double-count (INV-3). Claude never writes the key, so it
        // and historical QoderWork sessions without credits stay bit-for-bit what
        // they were (INV-2); current QoderWork sessions with credits intentionally take this
        // path too. `cost_partial` is untouched — zero tokens report no omission, and a mixed
        // session keeps its honest `≥` marker.
        let cost_usd = credits_cost(&self.extra).or(cost_usd);
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
        m.runtime = self.runtime;
        m
    }
}

/// Last value wins; an empty or non-string value changes nothing. (The Codex family keeps its
/// own copy — the two folds stay symmetric rather than sharing a three-line helper.)
fn remember_string(slot: &mut Option<String>, value: Option<&Value>) {
    if let Some(value) = value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        *slot = Some(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_replay_engine::seam::estimate_cost;

    /// A line of ONE assistant message: Claude Code writes each content item (thinking,
    /// text, tool_use) as its own transcript line and replicates the same `usage` on all
    /// of them. Synthetic — never built from a real transcript.
    fn msg_line(id: &str, req: &str, out: u64, cache_read: u64) -> Value {
        serde_json::from_str(&format!(
            r#"{{"type":"assistant","requestId":"{req}","message":{{"role":"assistant",
               "id":"{id}","model":"claude-opus-4-8","usage":{{"input_tokens":2,
               "output_tokens":{out},"cache_creation_input_tokens":945,
               "cache_read_input_tokens":{cache_read}}}}},
               "timestamp":"2026-08-05T10:00:00Z"}}"#
        ))
        .unwrap()
    }

    /// One API call written as three lines must be charged ONCE. Summing per line
    /// inflated a real corpus ~2x (43,519 of 87,211 usage lines were such repeats).
    #[test]
    fn one_api_call_split_across_lines_is_counted_once() {
        let mut acc = MetricsAcc::default();
        for _ in 0..3 {
            acc.push(&msg_line("msg_01A", "req_01A", 1400, 45461));
        }
        let m = acc.finish();
        assert_eq!(m.output_tokens, 1400, "3 lines, one call: not 4200");
        assert_eq!(m.cache_read_tokens, 45461, "not 136383");
        assert_eq!(m.input_tokens, 2, "not 6");
        assert_eq!(m.cache_creation_tokens, 945, "not 2835");
    }

    /// The other observed shape: a group whose usage GROWS line to line (streamed or
    /// retried). The final figure is the truth, so per-field growth is credited and the
    /// total lands on the last value — never the sum of the increments.
    #[test]
    fn a_growing_usage_group_credits_the_final_figure() {
        let mut acc = MetricsAcc::default();
        for out in [6, 6, 6, 6, 6, 478] {
            acc.push(&msg_line("msg_01B", "req_01B", out, 9509));
        }
        assert_eq!(acc.finish().output_tokens, 478, "not 508, not 2868");
    }

    /// Two DIFFERENT calls must both count — the guard must not collapse distinct work.
    #[test]
    fn distinct_calls_still_accumulate() {
        let mut acc = MetricsAcc::default();
        acc.push(&msg_line("msg_01C", "req_01C", 100, 10));
        acc.push(&msg_line("msg_01C", "req_01C", 100, 10)); // repeat of the first
        acc.push(&msg_line("msg_01D", "req_01D", 250, 20));
        let m = acc.finish();
        assert_eq!(m.output_tokens, 350, "two calls: 100 + 250");
        assert_eq!(m.cache_read_tokens, 30);
    }

    /// The reason `state`/`restore` are overridden: a cursor parked INSIDE a group must
    /// not let the resumed fold re-credit the rest of that group.
    #[test]
    fn a_group_split_across_a_resume_boundary_is_not_recredited() {
        let mut before = MetricsAcc::default();
        before.push(&msg_line("msg_01E", "req_01E", 1400, 45461));
        before.push(&msg_line("msg_01E", "req_01E", 1400, 45461));
        let parked = before.state();

        let mut after = MetricsAcc::default();
        after.restore(&parked);
        after.push(&msg_line("msg_01E", "req_01E", 1400, 45461)); // line 3 of the group
        let m = after.finish();
        assert_eq!(m.output_tokens, 1400, "resumed fold must not re-credit");
        assert_eq!(m.cache_read_tokens, 45461);
    }

    /// A cursor parked by an older build holds a bare `MetricsTotals`. Restoring it must
    /// still reseed: dropping it would leave a resumed fold counting only the tail, which
    /// is a worse and much less visible error than the over-count this guard fixes.
    #[test]
    fn a_legacy_bare_totals_cursor_still_reseeds() {
        let mut before = MetricsAcc::default();
        before.push(&msg_line("msg_01F", "req_01F", 900, 100));
        let legacy = serde_json::to_value(before.totals()).unwrap();

        let mut after = MetricsAcc::default();
        after.restore(&legacy);
        assert_eq!(
            after.finish().output_tokens,
            900,
            "legacy cursor must not silently restore nothing"
        );
    }

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

    /// #108: compactions fold into `extra` as two counters. `compact_dropped` SUMS each
    /// boundary's own `pre - post`, and the figures below are the first two real
    /// compactions from this project's own transcript — whose SECOND record states
    /// `cumulativeDroppedTokens: 1304549`. Reproducing that number from the summed deltas
    /// is what proves the derivation right, and it is the reason the record's own field
    /// (absent from 11 of 65 local compactions) is never read.
    #[test]
    fn compaction_counters_reproduce_the_transcripts_cumulative_dropped() {
        let jsonl = r#"
{"type":"system","subtype":"compact_boundary","timestamp":"2026-07-01T01:32:57.757Z","compactMetadata":{"trigger":"manual","preTokens":594718,"postTokens":8617,"cumulativeDroppedTokens":586101}}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-07-03T16:16:27.142Z","compactMetadata":{"trigger":"manual","preTokens":725463,"postTokens":7015}}
"#;
        let m = parse_reader(jsonl);
        assert_eq!(m.compactions(), (2, 1_304_549));
        assert_eq!(
            m.compaction_label().unwrap(),
            "2× compacted, 1.3M dropped",
            "the footer segment"
        );
        assert!(
            m.footer().contains("2× compacted"),
            "footer: {}",
            m.footer()
        );
        assert!(
            m.footer_segments().first().unwrap().0.contains("compacted"),
            "compactions shed first, so they sit ahead of `cached`"
        );
    }

    /// A session that never compacted keeps its footer exactly as it was — no empty
    /// segment, no stray separator.
    #[test]
    fn a_session_without_compactions_shows_no_compaction_segment() {
        let m = parse_reader(
            r#"{"type":"assistant","timestamp":"2026-06-28T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"output_tokens":5}}}"#,
        );
        assert_eq!(m.compactions(), (0, 0));
        assert_eq!(m.compaction_label(), None);
        assert!(!m.footer().contains("compacted"), "footer: {}", m.footer());
    }

    /// Qoder's per-line `usage.credits` sums into the reserved `credits_micro` key, surfaces
    /// in the footer, and — since design/qoder-credits-usd.md — converts to the `cost_usd`
    /// every money surface reads (the monitor's ledger banked `None` for these before, so a
    /// machine's Qoder half was worth $0). A line without the field changes nothing.
    #[test]
    fn credits_sum_into_the_extra_bag() {
        let jsonl = r#"
{"type":"assistant","timestamp":"2026-08-13T10:00:00.000Z","message":{"model":"cmodel","usage":{"input_tokens":0,"output_tokens":0,"credits":12.164261516,"billable":true}}}
{"type":"assistant","timestamp":"2026-08-13T10:01:00.000Z","message":{"model":"cmodel","usage":{"input_tokens":0,"output_tokens":0,"credits":0.5}}}
{"type":"assistant","timestamp":"2026-08-13T10:02:00.000Z","message":{"model":"cmodel","usage":{"input_tokens":0,"output_tokens":0}}}
{"type":"assistant","timestamp":"2026-08-13T10:03:00.000Z","message":{"model":"cmodel","usage":{"input_tokens":0,"output_tokens":0,"credits":99.0,"billable":false}}}
"#;
        let m = parse_reader(jsonl);
        // 12.164262 + 0.5; the billable:false line deducts nothing (failed calls are free).
        assert_eq!(m.extra.get("credits_micro"), Some(&12_664_262));
        assert!((m.credits().unwrap() - 12.664262).abs() < 1e-9);
        assert!(
            m.footer().contains("~12.66 credits"),
            "footer: {}",
            m.footer()
        );
        // The `cmodel` alias is unpriced and the token counts are zero, so the token estimate
        // has nothing to say — the money comes from the credits, at $0.01 each.
        assert!(
            (m.cost_usd.expect("priced from credits") - 0.12664262).abs() < 1e-9,
            "cost: {:?}",
            m.cost_usd
        );
        assert!(!m.cost_partial, "zero tokens omit no model");
        assert!(
            m.footer().contains("~$0.13") && m.footer().contains("~12.66 credits"),
            "footer: {}",
            m.footer()
        );
        // A Claude session (no credits field) keeps an empty bag and the token-priced cost —
        // the delegation equivalence QoderWork's suite pins stays byte-identical.
        let claude = parse_reader(
            r#"{"type":"assistant","timestamp":"2026-08-13T10:00:00.000Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":10,"output_tokens":5}}}"#,
        );
        assert!(claude.extra.is_empty());
        assert!(!claude.footer().contains("credits"));
        assert_eq!(
            claude.cost_usd,
            estimate_cost("claude-opus-4-8", 10, 0, 0, 5),
            "a token-billed session is untouched by the credits path"
        );
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

#[cfg(test)]
mod runtime_tests {
    use super::*;

    fn fold(lines: &[&str]) -> MetricsAcc {
        let mut acc = MetricsAcc::recording(&["effort", "permission", "client"]);
        for l in lines {
            acc.push(&serde_json::from_str::<Value>(l).unwrap());
        }
        acc
    }

    /// #62: effort and the permission mode change mid-session — the last value wins — and the
    /// client version rides on every record.
    #[test]
    fn runtime_records_effort_permission_and_client_last_value_wins() {
        let acc = fold(&[
            r#"{"type":"permission-mode","permissionMode":"bypassPermissions","timestamp":"2026-09-03T10:00:00.000Z"}"#,
            r#"{"type":"user","version":"2.1.220","message":{"role":"user","content":"hi"},"timestamp":"2026-09-03T10:00:01.000Z"}"#,
            r#"{"type":"assistant","effort":"xhigh","version":"2.1.220","message":{"model":"claude-opus-5","usage":{"output_tokens":5}},"timestamp":"2026-09-03T10:00:02.000Z"}"#,
            r#"{"type":"assistant","effort":"max","version":"2.1.234","message":{"model":"claude-fable-5-1","usage":{"output_tokens":5}},"timestamp":"2026-09-03T10:00:03.000Z"}"#,
            r#"{"type":"permission-mode","permissionMode":"default","timestamp":"2026-09-03T10:00:04.000Z"}"#,
        ]);
        let m = acc.finish();
        assert_eq!(m.runtime.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(m.runtime.permission_profile.as_deref(), Some("default"));
        assert_eq!(m.runtime.client_version.as_deref(), Some("2.1.234"));
        assert_eq!(m.runtime.recorded, vec!["effort", "permission", "client"]);
        // Never Claude Code's: the format has no sandbox and no context window.
        assert!(m.runtime.sandbox.is_none() && m.runtime.context_window_tokens.is_none());
    }

    /// A fold that saw nothing still DECLARES what it would record, so a pane can say
    /// "unknown" rather than "not recorded".
    #[test]
    fn runtime_declares_what_the_format_records_before_seeing_anything() {
        let m = fold(&[]).finish();
        assert_eq!(m.runtime.recorded, vec!["effort", "permission", "client"]);
        assert!(m.runtime.reasoning_effort.is_none());
    }

    /// The resume cursor carries the snapshot; the declaration stays the adapter's.
    #[test]
    fn runtime_survives_state_and_restore() {
        let acc = fold(&[
            r#"{"type":"assistant","effort":"xhigh","version":"2.1.220","message":{"model":"m","usage":{"output_tokens":1}},"timestamp":"2026-09-03T10:00:02.000Z"}"#,
        ]);
        let state = acc.state();
        let mut resumed = MetricsAcc::recording(&["effort", "permission", "client"]);
        resumed.restore(&state);
        let m = resumed.finish();
        assert_eq!(m.runtime.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(m.runtime.client_version.as_deref(), Some("2.1.220"));
        assert_eq!(m.runtime.recorded, vec!["effort", "permission", "client"]);
    }

    /// The qwork family's head, under the same fold: a real head carries the keys as null.
    #[test]
    fn runtime_reads_the_qwork_head_when_it_says_something() {
        let acc = fold(&[
            r#"{"type":"runtime-config","sessionId":"q","model":"qwork","reasoningEffort":"high","contextWindow":128000,"timestamp":1784282861519}"#,
        ]);
        let m = acc.finish();
        assert_eq!(m.runtime.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(m.runtime.context_window_tokens, Some(128_000));
        let m = fold(&[r#"{"type":"runtime-config","sessionId":"q","model":"","reasoningEffort":null,"contextWindow":null,"timestamp":1}"#]).finish();
        assert!(m.runtime.reasoning_effort.is_none() && m.runtime.context_window_tokens.is_none());
    }
}
