//! Session metrics parsed from the transcript: token totals, wall-clock
//! duration, model, and a best-effort USD cost estimate.

use crate::model::UsdCost;
use std::collections::BTreeMap;

/// One persisted rate-limit window, normalized from an agent's usage snapshots.
#[derive(Debug, Default, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
pub struct RateLimitWindow {
    pub used_percent: f64,
    pub window_minutes: u64,
    pub resets_at: Option<i64>,
}

/// The latest persisted rate-limit state for a session.
#[derive(Debug, Default, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
pub struct RateLimits {
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub plan_type: Option<String>,
    pub reached: Option<String>,
}

/// Latest persisted execution context/settings. This is session metadata, not a synthetic
/// historical block: adapters fill only fields their transcripts actually record, and shared
/// presenters can expose the snapshot in their status/header surfaces.
#[derive(Debug, Default, PartialEq, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuntimeInfo {
    pub context_window_tokens: Option<u64>,
    pub context_used_tokens: Option<u64>,
    pub reasoning_effort: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
    pub permission_profile: Option<String>,
    pub collaboration_mode: Option<String>,
    pub service_tier: Option<String>,
    pub rate_limits: Option<RateLimits>,
    /// The agent CLIENT's version, when the transcript names it (Claude Code writes
    /// `version` on every record); last value wins, like every field here.
    #[serde(default)]
    pub client_version: Option<String>,
    /// Which of this snapshot's facts the agent's transcript FORMAT records at all, by the
    /// wire's own key names (`context`, `effort`, `mode`, `sandbox`, `approvals`, `permission`,
    /// `tier`, `plan`, `client`) — declared by the family's accumulator, so a presenter can
    /// tell "unknown" (recorded by this agent, not seen yet) from "not recorded by this agent"
    /// (#62) without a per-agent table of its own. Empty for a fold that declares nothing.
    #[serde(default)]
    pub recorded: Vec<String>,
}

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
/// counters. It makes a brand-new category need *no* struct change — the complement to
/// `#[non_exhaustive]`. The seam is wired through the `MetricsAccumulator` interface; Codex
/// currently uses it for skipped-record diagnostics rendered by the standard footer.
///
/// Two keys are **reserved, cross-agent diagnostic names** rather than agent-private ones —
/// `"malformed_lines"` and `"unsupported_items"` — because the shared footer sums exactly
/// those into its `⚠ N skipped` label. Any agent may bump them; everything else in the bag
/// stays opaque to the engine.
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
    /// Latest context, settings, and limits snapshot carried by the transcript.
    pub runtime: RuntimeInfo,
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

/// USD per Qoder **credit** — the published subscription rate, checked 2026-08-27 against
/// <https://docs.qoder.com/account/pricing>: Pro $20/2,000 · Pro+ $60/6,000 · Ultra
/// $200/20,000. Three plans, one rate, so this is an anchor rather than a guess.
///
/// Prepaid Credit Packs ($20/1,500 ≈ $0.0133) are deliberately NOT modelled: a transcript
/// records no purchase provenance, so which credits a line drew on is unknowable from disk
/// (design/qoder-credits-usd.md NG-1). Same argument as `price()` for why the number lives
/// in code with a dated citation and a pinning test, not in configuration.
pub const USD_PER_CREDIT: f64 = 0.01;

/// The USD a credit-billed agent's own figure implies, or `None` when it reported none.
///
/// The complement to [`total_cost`]: that one *estimates* from tokens at list price, this one
/// *converts* what the agent says it actually deducted. A credit-billed agent zeroes its token
/// counts and names an opaque model alias, so the token estimate is `None` for exactly the
/// sessions this covers — the two never both answer for real data, and where they could, the
/// measured figure wins (design/qoder-credits-usd.md INV-3).
pub fn credits_cost(extra: &BTreeMap<String, u64>) -> Option<UsdCost> {
    match extra.get("credits_micro") {
        Some(&micro) if micro > 0 => Some(micro as f64 / 1e6 * USD_PER_CREDIT),
        _ => None,
    }
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
    // ── OpenAI (Codex) — best-effort list price (tiers checked 2026-08-13) ──
    // Tier matches must precede the family fallback, same reason as Opus above: the flat
    // $1.25/$10 rate priced a real sol-heavy session tree at ~$118 when the tier rates say
    // ~$436 — a 3.7× understatement.
    if m.contains("gpt-5.6") || m.contains("gpt-5-6") {
        if m.contains("sol") {
            return Some((5.0, 30.0));
        }
        // Terra/luna are the STANDARD rates effective 2026-07-30 — #19 initially carried
        // exactly half for both, which is the Batch-API discount; Codex traffic is
        // interactive and bills at standard rates.
        if m.contains("terra") {
            return Some((2.0, 12.0));
        }
        if m.contains("luna") {
            return Some((0.20, 1.20));
        }
    }
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

fn rate_limit_label(window: &RateLimitWindow) -> String {
    let span = match window.window_minutes {
        minutes if minutes % 10_080 == 0 => format!("{}w", minutes / 10_080),
        minutes if minutes % 1_440 == 0 => format!("{}d", minutes / 1_440),
        minutes if minutes % 60 == 0 => format!("{}h", minutes / 60),
        minutes => format!("{minutes}m"),
    };
    format!("{span} {:.0}% used", window.used_percent)
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
        if let Some(left) = self.context_left_percent() {
            segs.push((format!("{left}% context left"), 2));
        }
        let cached = self.cache_creation_tokens + self.cache_read_tokens;
        if cached > 0 {
            segs.push((format!("{} cached", human_tokens(cached)), 1));
        }
        if !self.model.is_empty() {
            segs.push((self.model_label(), 3));
        }
        if let Some(effort) = &self.runtime.reasoning_effort {
            segs.push((format!("{effort} effort"), 3));
        }
        segs.push((format!("{} in", human_tokens(self.input_tokens)), 4));
        segs.push((format!("{} out", human_tokens(self.output_tokens)), 5));
        if self.duration_secs > 0 {
            segs.push((human_dur(self.duration_secs), 6));
        }
        if let Some(c) = self.cost_usd {
            segs.push((self.cost_label(c), 7));
        }
        if let Some(label) = self.credits_label() {
            segs.push((label, 7));
        }
        if let Some(primary) = self
            .runtime
            .rate_limits
            .as_ref()
            .and_then(|limits| limits.primary.as_ref())
        {
            segs.push((rate_limit_label(primary), 8));
        }
        segs
    }

    /// Percentage of the recorded model context still available at the latest usage snapshot.
    pub fn context_left_percent(&self) -> Option<u64> {
        let window = self.runtime.context_window_tokens?;
        let used = self.runtime.context_used_tokens?;
        if window == 0 {
            return None;
        }
        Some(
            100_u64
                .saturating_sub(used.saturating_mul(100) / window)
                .min(100),
        )
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

    /// Credits this session consumed, from the reserved `credits_micro` extra key
    /// (micro-credits, so the u64 bag can carry a fractional figure exactly enough).
    /// `None` for an agent that bills in tokens/USD instead — the footer then keeps
    /// its existing shape. Qoder's accumulator is the first writer: its usage carries
    /// `credits` while its token counts are zero, so credits are the NATIVE cost figure
    /// — [`cost_usd`](Self::cost_usd) is derived from them at [`USD_PER_CREDIT`], and the
    /// footer shows both (`~$0.02 · ~2.00 credits`) rather than asking a reader to trust
    /// the conversion blind.
    pub fn credits(&self) -> Option<f64> {
        self.extra.get("credits_micro").map(|&c| c as f64 / 1e6)
    }

    /// The footer's credits segment (`~12.16 credits`), or `None` when the agent
    /// reports none. `~` because per-line rounding to micro-credits makes it an
    /// estimate, exactly like the USD figure it stands in for.
    pub fn credits_label(&self) -> Option<String> {
        self.credits().map(|c| format!("~{c:.2} credits"))
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
        let credits = self
            .credits_label()
            .map(|label| format!(" · {label}"))
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
        let diagnostics = self
            .diagnostics_label()
            .map(|label| format!(" · {label}"))
            .unwrap_or_default();
        format!(
            "{model}{} in · {cached}{} out · {}{cost}{credits}{compacted}{diagnostics}",
            human_tokens(self.input_tokens),
            human_tokens(self.output_tokens),
            human_dur(self.duration_secs),
        )
    }

    fn diagnostics_label(&self) -> Option<String> {
        let skipped = self.extra.get("malformed_lines").copied().unwrap_or(0)
            + self.extra.get("unsupported_items").copied().unwrap_or(0);
        (skipped > 0).then(|| format!("⚠ {skipped} skipped"))
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
            ("gpt-5.6-sol", (5.0, 30.0)),
            ("gpt-5.6-terra", (2.0, 12.0)),
            ("gpt-5.6-luna", (0.20, 1.20)),
            ("gpt-5.6", (1.25, 10.0)),
            ("gpt-5.1-codex", (1.25, 10.0)),
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
mod diagnostic_tests {
    use super::*;

    #[test]
    fn footer_surfaces_skipped_schema_records() {
        let mut metrics = Metrics::default();
        metrics.extra.insert("malformed_lines".into(), 2);
        metrics.extra.insert("unsupported_items".into(), 3);
        assert_eq!(metrics.diagnostics_label().as_deref(), Some("⚠ 5 skipped"));
        assert!(metrics.footer().contains("⚠ 5 skipped"));
    }
}

#[cfg(test)]
mod credits_tests {
    use super::*;

    /// The reserved `credits_micro` key surfaces as a footer segment; agents that never
    /// write it keep their footer byte-identical.
    #[test]
    fn credits_come_from_the_extra_bag() {
        let mut m = Metrics::default();
        assert_eq!(m.credits(), None);
        assert!(!m.footer().contains("credits"), "footer: {}", m.footer());
        assert!(m
            .footer_segments()
            .iter()
            .all(|(s, _)| !s.contains("credits")));

        m.extra.insert("credits_micro".into(), 12_164_261);
        assert_eq!(m.credits_label().as_deref(), Some("~12.16 credits"));
        assert!(
            m.footer().ends_with("~12.16 credits"),
            "footer: {}",
            m.footer()
        );
        assert!(m
            .footer_segments()
            .iter()
            .any(|(s, p)| s == "~12.16 credits" && *p == 7));
    }

    /// The credit rate against the published plan table (checked 2026-08-27,
    /// <https://docs.qoder.com/account/pricing>). Every subscription tier divides to the same
    /// $0.01, which is why one constant is honest; the Credit Pack does NOT (NG-1), and is
    /// pinned here too so a future edit that "corrects" the rate to the pack's has to argue
    /// with the table rather than with a bare number.
    #[test]
    fn the_credit_rate_matches_the_published_plans() {
        for (usd, credits) in [(20.0, 2_000.0), (60.0, 6_000.0), (200.0, 20_000.0)] {
            assert!(
                (usd / credits - USD_PER_CREDIT).abs() < 1e-12,
                "${usd}/{credits} credits"
            );
        }
        assert!(
            (20.0 / 1_500.0_f64 - USD_PER_CREDIT).abs() > 1e-3,
            "the prepaid pack rate is deliberately not the modelled one"
        );
    }

    /// Credits convert to the USD figure every money surface reads, and an agent that reports
    /// none is untouched — the property that keeps Claude/Codex footers byte-identical.
    #[test]
    fn credits_convert_to_usd() {
        let mut extra = BTreeMap::new();
        assert_eq!(credits_cost(&extra), None, "no key ⇒ no cost");
        extra.insert("credits_micro".into(), 0);
        assert_eq!(credits_cost(&extra), None, "zero credits ⇒ no cost");

        extra.insert("credits_micro".into(), 12_164_261);
        let usd = credits_cost(&extra).expect("priced");
        assert!((usd - 0.12164261).abs() < 1e-12, "usd: {usd}");

        let mut m = Metrics::default();
        m.extra = extra;
        m.cost_usd = credits_cost(&m.extra);
        assert!(
            m.footer().contains("~$0.12") && m.footer().contains("~12.16 credits"),
            "both figures, native beside derived: {}",
            m.footer()
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
