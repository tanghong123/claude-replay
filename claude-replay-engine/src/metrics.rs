//! Session metrics parsed from the transcript: token totals, wall-clock
//! duration, model, and a best-effort USD cost estimate.

use crate::model::UsdCost;
use std::collections::BTreeMap;

/// A session's token/cost tally.
///
/// **Two-way compatible by design.** The parse side is liberal: each accumulator pulls only
/// the JSON keys it knows, defaulting a *missing* field to `0` (an **older** transcript) and
/// *ignoring* unknown ones (a **newer** transcript) — so neither direction fails, at worst a
/// brand-new token category isn't yet counted. The struct is `#[non_exhaustive]`, so new
/// fields can be **added** without breaking downstream crates (they read fields + use
/// [`Default`], never a struct literal). Evolving a *specific* agent's usage format is a
/// change in that agent's accumulator (`claude_metrics` / `codex_metrics`), not here — the
/// shared value stays stable.
///
/// For a metric a *single* agent reports that the shared struct shouldn't grow a field for,
/// the mechanism is the **accumulating extension bag** [`extra`](Self::extra) below — an
/// agent's accumulator folds keys into it (via its `bump` helper) exactly like the typed
/// counters. Data-only (the standard `footer` shows the typed fields), and it makes a brand-new
/// category need *no* struct change — the complement to `#[non_exhaustive]`. The seam is wired
/// through the `MetricsAccumulator` interface and ready to use; no agent
/// currently populates it, so `extra` is empty in practice.
/// (If `Metrics` is ever persisted — e.g. a `SessionAccumulator` checkpoint — add `serde(default)`
/// per field and don't `deny_unknown_fields`; the bag then carries unknown keys for free.)
#[derive(Debug, Default, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Metrics {
    /// Genuinely-new input tokens (excludes cached content — see the two cache
    /// fields below). Small on cache-heavy sessions.
    pub input_tokens: u64,
    /// Tokens written to the prompt cache the first time content is seen.
    pub cache_creation_tokens: u64,
    /// Cached tokens re-read on later turns. This dominates a long session (the
    /// whole context is re-read every turn), so it's tallied separately from
    /// `input_tokens` rather than lumped in.
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub duration_secs: i64,
    /// Best-effort estimated cost in US dollars; see [`UsdCost`]. `None` when the model isn't
    /// priced.
    pub cost_usd: Option<UsdCost>,
    /// **Agent-specific metrics** an accumulator folded in — namespaced snake_case keys (e.g.
    /// `reasoning_tokens`, `web_searches`), summed across the session. The accumulating
    /// extension bag: data-only (not shown by the standard [`footer`](Self::footer)), so a
    /// brand-new category needs no struct change. Empty for an agent that reports none.
    pub extra: BTreeMap<String, u64>,
    /// Tokens **attributed to the model that produced them** (#104). A session can switch
    /// models — measured at 4.7% of local sessions — and pricing every token at one model's
    /// rate is simply wrong, so [`cost_usd`](Self::cost_usd) is the SUM over this map.
    /// `model` above remains the last one seen, which is what a live session is running now.
    pub per_model: BTreeMap<String, TokenCounts>,
    /// Set when [`cost_usd`](Self::cost_usd) omits a model that produced tokens but is absent
    /// from the price table — the figure is then a LOWER BOUND, rendered `≥$x` not `~$x`.
    /// Phrased as the *exception* so `Default` (false = nothing omitted) is correct.
    pub cost_partial: bool,
}

/// One model's share of a session's tokens. The four typed counters of [`Metrics`], split out
/// so they can be keyed by model.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct TokenCounts {
    pub input: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub output: u64,
}

impl std::ops::AddAssign for TokenCounts {
    fn add_assign(&mut self, o: Self) {
        self.input += o.input;
        self.cache_creation += o.cache_creation;
        self.cache_read += o.cache_read;
        self.output += o.output;
    }
}

impl TokenCounts {
    /// This model's cost, or `None` when the model isn't priced.
    pub fn cost(&self, model: &str) -> Option<UsdCost> {
        estimate_cost(
            model,
            self.input,
            self.cache_creation,
            self.cache_read,
            self.output,
        )
    }
}

/// The resumable form of any agent's metrics accumulator (#96 §7): per-model token totals, the
/// agent-specific counter bag, and the observed time span. Named because it crosses the seam in
/// both directions and an anonymous tuple there reads as noise.
pub type MetricsTotals = (
    BTreeMap<String, TokenCounts>,
    BTreeMap<String, u64>,
    Option<(crate::model::EpochSeconds, crate::model::EpochSeconds)>,
);

/// Sum the per-model costs, and say whether the sum covers **every** model that produced
/// tokens (the returned flag is `true` when some model was OMITTED).
///
/// The flag is not decoration. Pricing is name-matched (`price`), so a model the table does not
/// know contributes **nothing** — and per-model attribution makes that visible where a single
/// flat counter hid it. A real case from the byte-gate fixture: 97% of its tokens are
/// `claude-fable-5`, unpriced, so the sum covers 3% of the session. Reporting that as "the
/// cost" would be worse than the bug this fixes; reporting it as a LOWER BOUND is honest.
pub fn total_cost(per_model: &BTreeMap<String, TokenCounts>) -> (Option<UsdCost>, bool) {
    let (mut total, mut partial) = (None, false);
    for (m, c) in per_model {
        match c.cost(m) {
            Some(v) => *total.get_or_insert(0.0) += v,
            // Only tokens make a gap: a model that produced none costs nothing either way.
            None if *c != TokenCounts::default() => partial = true,
            None => {}
        }
    }
    (total, partial)
}

/// Parse an RFC3339-ish timestamp ("2026-06-28T13:54:10.106Z") to unix seconds
/// (integer — sub-second precision is dropped, matching the old byte-offset parser).
/// Shares the one epoch-seconds converter with the parse layer (`engine::time`).
pub fn parse_ts(s: &str) -> Option<i64> {
    crate::engine::time::epoch_secs(s).map(|secs| secs as i64)
}

/// Running min/max of observed epoch-second timestamps → a session duration. Both agents'
/// metrics accumulators fold their per-line timestamps through this (each parses the raw
/// timestamp its own way, then `observe`s the seconds).
#[derive(Default, Clone)]
pub struct TimeSpan {
    min: Option<i64>,
    max: Option<i64>,
}
impl TimeSpan {
    pub fn observe(&mut self, secs: i64) {
        self.min = Some(self.min.map_or(secs, |a| a.min(secs)));
        self.max = Some(self.max.map_or(secs, |a| a.max(secs)));
    }
    /// Elapsed wall-clock seconds (clamped to ≥ 0); `0` if fewer than two timestamps seen.
    /// The observed endpoints (#96 §7): a resumed accumulator needs these, where the collapsed
    /// `duration_secs` has already thrown them away.
    pub fn endpoints(&self) -> Option<(i64, i64)> {
        self.min.zip(self.max)
    }
    /// Re-seed from endpoints a resume restored.
    pub fn set_endpoints(&mut self, e: Option<(i64, i64)>) {
        (self.min, self.max) = e.map_or((None, None), |(a, b)| (Some(a), Some(b)));
    }

    pub fn duration_secs(&self) -> i64 {
        match (self.min, self.max) {
            (Some(a), Some(b)) => (b - a).max(0),
            _ => 0,
        }
    }
}

/// Rough USD/1M-token (input, output) list prices for cost estimation.
/// Best-effort — rates are approximate and drift over time.
/// `(input, output)` USD per million tokens, from the official table
/// (<https://platform.claude.com/docs/en/about-claude/pricing>, checked 2026-08-05).
///
/// **Why this is code and not configuration.** Rates change, and a table baked into a binary
/// goes stale — which is exactly what happened here: every `opus` was priced at the retired
/// $15/$75 long after Opus 4.5+ moved to $5/$25, inflating real estimates ~3×. But moving the
/// numbers to a config file would not have caught that, because nothing would have told anyone
/// the file was wrong. Two things actually help, and both are here: the estimate is marked
/// `≥` when any model is unpriced (so a NEW model is visibly missing rather than silently
/// free), and `price_tests` pins every rate against the published table, so a stale entry is a
/// failing test at the next touch rather than a wrong number in a footer.
///
/// A user-editable file would add a way for the number to be wrong that no test can see. If
/// rates ever move faster than releases, the shape to reach for is a *shipped* data file
/// (updated by upgrade, still covered by those tests) — not user configuration.
///
/// **Order matters**: the deprecated Opus 4/4.1 cost 3× what Opus 4.5+ do, so the specific
/// matches must precede the family fallback. Getting this wrong is not cosmetic — a table that
/// priced every `opus` at the retired $15/$75 rate inflated a real session's estimate ~3×.
fn price(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    // ── Anthropic ──
    if m.contains("fable") || m.contains("mythos") {
        return Some((10.0, 50.0));
    }
    if m.contains("opus") {
        // Opus 4 and 4.1 are retired/deprecated and were priced 3× the current family.
        let legacy = m.contains("opus-4-1") || m.contains("opus-4.1") || is_bare_opus_4(&m);
        return Some(if legacy { (15.0, 75.0) } else { (5.0, 25.0) });
    }
    if m.contains("sonnet") {
        // Sonnet 5 runs introductory pricing through 2026-08-31, then $3/$15 like Sonnet 4.x.
        // Not date-aware: a transcript read after the change prices its Sonnet 5 turns at the
        // introductory rate. Revisit when that matters more than the added plumbing.
        return Some(if m.contains("sonnet-5") {
            (2.0, 10.0)
        } else {
            (3.0, 15.0)
        });
    }
    if m.contains("haiku") {
        return Some(if m.contains("haiku-3") {
            (0.80, 4.0)
        } else {
            (1.0, 5.0)
        });
    }
    // ── OpenAI (Codex) — best-effort list price ──
    if m.contains("codex") || m.contains("gpt-5") || m.contains("gpt5") {
        return Some((1.25, 10.0));
    }
    None
}

/// Is this the retired original `claude-opus-4`, as opposed to `claude-opus-4-5` and later?
///
/// The two are only distinguishable by what follows: a MINOR VERSION is one digit (`-4-8`)
/// while a RELEASE DATE is eight (`-4-20250514`). A naive `contains("opus-4")` prices every
/// 4.x at the retired rate — 3× too high — and a naive "next char is `-`" cannot tell the dated
/// original from a minor version at all.
fn is_bare_opus_4(m: &str) -> bool {
    let Some(rest) = m.split("opus-4").nth(1) else {
        return false;
    };
    let tail = rest.trim_start_matches(['-', '.']);
    // Nothing after it, or a date-length digit run ⇒ the original.
    rest.is_empty() || tail.chars().take_while(char::is_ascii_digit).count() >= 4
}

/// Best-effort USD cost from a model name and its token tiers. Cache writes bill
/// at ~1.25× base input, cache reads at ~0.1× (prompt-caching discount). Returns
/// `None` when the model isn't in the price table.
pub fn estimate_cost(
    model: &str,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
) -> Option<UsdCost> {
    price(model).map(|(pi, po)| {
        (input as f64 + cache_creation as f64 * 1.25 + cache_read as f64 * 0.10) / 1e6 * pi
            + output as f64 / 1e6 * po
    })
}

/// Metrics via the reader through `adapter` — the engine-side form (the facade's
/// `parse_reader_for(agent, …)` resolves the adapter and calls this); the returned
/// [`Metrics`] shape is shared across agents.
pub fn parse_reader_with<R: std::io::BufRead>(
    adapter: &dyn crate::adapter::TranscriptAdapter,
    mut reader: R,
) -> Metrics {
    adapter.parse_reader(&mut reader)
}

pub fn human_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn human_dur(secs: i64) -> String {
    if secs <= 0 {
        return "—".into();
    }
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

/// "claude-opus-4-8" -> "opus4.8". Non-Claude models (e.g. Codex "gpt-5.6") are
/// shown verbatim.
fn short_model(model: &str) -> String {
    if !model.starts_with("claude-") {
        return model.to_string();
    }
    let m = model.strip_prefix("claude-").unwrap_or(model);
    let mut parts = m.split('-');
    let name = parts.next().unwrap_or(m);
    let ver: Vec<&str> = parts
        .filter(|p| p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if ver.is_empty() {
        name.to_string()
    } else {
        format!("{name}{}", ver.join("."))
    }
}

impl Metrics {
    /// Compact one-line footer text.
    /// Footer metric parts as `(text, shed_priority)` — the viewer's fit-and-shed drops
    /// the highest priority first when the footer can't fit its width. Order matches the
    /// spec: cached(1) → model(3) → in(4) → out(5) → duration(6) → cost(7). (`%`, at
    /// priority 2, is scroll-derived and added by the view.)
    pub fn footer_segments(&self) -> Vec<(String, u8)> {
        let mut segs = Vec::new();
        // Compactions shed FIRST — the newest and most niche segment, so a narrow terminal
        // loses it before anything that was already there. (`min_by_key` picks the first
        // segment at the lowest priority, so sharing `1` with `cached` and sitting ahead of
        // it in the vec is what puts it first in line.)
        if let Some(seg) = self.compaction_label() {
            segs.push((seg, 1));
        }
        let cached = self.cache_creation_tokens + self.cache_read_tokens;
        if cached > 0 {
            segs.push((format!("{} cached", human_tokens(cached)), 1));
        }
        if !self.model.is_empty() {
            segs.push((self.model_label(), 3));
        }
        segs.push((format!("{} in", human_tokens(self.input_tokens)), 4));
        segs.push((format!("{} out", human_tokens(self.output_tokens)), 5));
        if self.duration_secs > 0 {
            segs.push((human_dur(self.duration_secs), 6));
        }
        if let Some(c) = self.cost_usd {
            segs.push((self.cost_label(c), 7));
        }
        segs
    }

    /// The model for a ONE-LINE footer. With several models in play a single name beside a
    /// summed cost would misread as "this cost, at this rate", so a `+N` says how many others
    /// contributed (#104). Neither surface has room for a per-model breakdown; this is the
    /// smallest honest signal.
    /// `~$x` when every model that produced tokens is priced; `≥$x` when some is not, so an
    /// estimate covering part of a session never reads as the whole of it. One character.
    pub fn cost_label(&self, c: UsdCost) -> String {
        if self.cost_partial {
            format!("≥${c:.2}")
        } else {
            format!("~${c:.2}")
        }
    }

    /// How many times this session's context was compacted, and how many tokens that
    /// dropped in total — read from the [`extra`](Self::extra) bag an adapter folded (#108).
    /// `(0, 0)` for an agent that reports none, or a session that never compacted.
    pub fn compactions(&self) -> (u64, u64) {
        let get = |k: &str| self.extra.get(k).copied().unwrap_or(0);
        (get("compactions"), get("compact_dropped"))
    }

    /// The footer's compaction segment (`3× compacted, 1.3M dropped`), or `None` when the
    /// session never compacted. Long sessions are *defined* by their compactions — without
    /// this the footer's token totals look inexplicably large beside a short-looking replay.
    pub fn compaction_label(&self) -> Option<String> {
        let (n, dropped) = self.compactions();
        if n == 0 {
            return None;
        }
        Some(if dropped > 0 {
            format!("{n}× compacted, {} dropped", human_tokens(dropped))
        } else {
            format!("{n}× compacted")
        })
    }

    pub fn model_label(&self) -> String {
        let short = short_model(&self.model).to_string();
        match self.per_model.len() {
            0 | 1 => short,
            n => format!("{short}+{}", n - 1),
        }
    }

    pub fn footer(&self) -> String {
        let model = if self.model.is_empty() {
            String::new()
        } else {
            format!("{} · ", self.model_label())
        };
        let cost = self
            .cost_usd
            .map(|c| format!(" · {}", self.cost_label(c)))
            .unwrap_or_default();
        // Show the cache tier only when there is one — cache-less transcripts keep
        // the plain "in / out" shape.
        let cached = self.cache_creation_tokens + self.cache_read_tokens;
        let cached = if cached > 0 {
            format!("{} cached · ", human_tokens(cached))
        } else {
            String::new()
        };
        // Only sessions that actually compacted carry the segment, so an ordinary footer
        // keeps its existing shape exactly.
        let compacted = self
            .compaction_label()
            .map(|s| format!(" · {s}"))
            .unwrap_or_default();
        format!(
            "{model}{} in · {cached}{} out · {}{cost}{compacted}",
            human_tokens(self.input_tokens),
            human_tokens(self.output_tokens),
            human_dur(self.duration_secs),
        )
    }
}

#[cfg(test)]
mod price_tests {
    use super::*;

    /// Every rate against the official table (checked 2026-08-05). Pins the version-specific
    /// splits, which are where a family-only match goes wrong: Opus 4/4.1 cost 3× Opus 4.5+,
    /// and matching `opus-4` naively would catch every 4.x.
    #[test]
    fn prices_match_the_published_table() {
        for (model, want) in [
            ("claude-fable-5", (10.0, 50.0)),
            ("claude-mythos-5", (10.0, 50.0)),
            ("claude-opus-5", (5.0, 25.0)),
            ("claude-opus-4-8", (5.0, 25.0)),
            ("claude-opus-4-5", (5.0, 25.0)),
            ("claude-opus-4-1-20250805", (15.0, 75.0)),
            ("claude-opus-4-20250514", (15.0, 75.0)),
            ("claude-sonnet-5", (2.0, 10.0)),
            ("claude-sonnet-4-6", (3.0, 15.0)),
            ("claude-haiku-4-5-20251001", (1.0, 5.0)),
            ("claude-haiku-3-5", (0.80, 4.0)),
        ] {
            assert_eq!(price(model), Some(want), "{model}");
        }
        assert_eq!(price("some-unknown-model"), None, "unknown stays unpriced");
    }

    /// `claude-opus-4-8` must NOT be read as the retired `claude-opus-4` — a substring match
    /// would triple its price.
    #[test]
    fn versioned_opus_is_not_the_retired_one() {
        assert!(!is_bare_opus_4("claude-opus-4-8"));
        assert!(!is_bare_opus_4("claude-opus-4-5"));
        assert!(is_bare_opus_4("claude-opus-4"));
        assert!(
            is_bare_opus_4("claude-opus-4-20250514"),
            "a DATE, not a minor version"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_model_formats() {
        assert_eq!(short_model("claude-opus-4-8"), "opus4.8");
        assert_eq!(short_model("claude-sonnet-4-6"), "sonnet4.6");
    }
}
