//! Session metrics parsed from the transcript: token totals, wall-clock
//! duration, model, and a best-effort USD cost estimate.

use crate::model::UsdCost;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

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
    /// This model context's exact estimated price, or `None` when it isn't priced.
    pub fn price(&self, model: &ModelContext) -> Option<PriceEstimate> {
        self.price_with(&PriceTable::default(), model)
    }

    /// [`price`](Self::price) against host-supplied rates — see [`PriceTable`].
    pub fn price_with(&self, prices: &PriceTable, model: &ModelContext) -> Option<PriceEstimate> {
        estimate_price_with(
            prices,
            model,
            self.input,
            self.cache_creation,
            self.cache_read,
            self.output,
        )
    }

    /// Backward-compatible USD projection for callers that still carry only a model name.
    pub fn cost(&self, model: &str) -> Option<UsdCost> {
        self.cost_with(&PriceTable::default(), model)
    }

    /// [`cost`](Self::cost) against host-supplied rates. A rate without an explicit USD currency
    /// is intentionally not projected into this legacy USD-only return type.
    pub fn cost_with(&self, prices: &PriceTable, model: &str) -> Option<UsdCost> {
        self.price_with(prices, &ModelContext::new(model))
            .and_then(|price| price.amount_in("USD"))
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
/// The flag is not decoration. Pricing is exact-name matched, so a model the catalog does not
/// know contributes **nothing** — and per-model attribution makes that visible where a single
/// flat counter hid it. Reporting a partially covered sum as "the cost" would be worse than the
/// omission; reporting it as a LOWER BOUND is honest.
pub fn total_cost(per_model: &BTreeMap<String, TokenCounts>) -> (Option<UsdCost>, bool) {
    total_cost_with(&PriceTable::default(), per_model)
}

/// [`total_cost`] against host-supplied rates — see [`PriceTable`]. An override narrows the gap
/// the flag reports: a model the built-in table misses but the host names is now covered, and
/// counts towards the sum rather than towards the `≥`.
pub fn total_cost_with(
    prices: &PriceTable,
    per_model: &BTreeMap<String, TokenCounts>,
) -> (Option<UsdCost>, bool) {
    let (mut total, mut partial): (Option<PriceEstimate>, bool) = (None, false);
    for (name, counts) in per_model {
        // Adapters may open a model bucket before its first nonzero usage delta. It is not usage
        // and must not turn the absent cost state into a misleading priced zero.
        if *counts == TokenCounts::default() {
            continue;
        }
        let context = ModelContext::new(name);
        match counts.price_with(prices, &context) {
            Some(price) if price.is_currency("USD") => match total.as_ref() {
                Some(current) => match current.checked_add(&price) {
                    Some(sum) => total = Some(sum),
                    // Preserve the known subtotal. Dropping it would make a later priced model
                    // silently restart the sum rather than extend an honest lower bound.
                    None => partial = true,
                },
                None => total = Some(price),
            },
            Some(_) | None => partial = true,
        }
    }
    (total.and_then(|price| price.amount_in("USD")), partial)
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

const AMOUNT_MICROS_PER_UNIT: u64 = 1_000_000;

/// Context used to resolve a model's price.
///
/// The field is deliberately private: callers construct a context from a model name today, while
/// future billing dimensions can be added without changing every pricing API signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelContext {
    name: String,
}

impl ModelContext {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl From<&str> for ModelContext {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

/// Replaceable policy for turning a recorded model context into a catalog key.
pub trait ModelNormalizer: Send + Sync {
    fn normalize(&self, context: &ModelContext) -> String;
}

/// The built-in conservative normalizer: case/separator normalization plus a valid terminal
/// snapshot date. It does not perform family or substring matching.
#[derive(Debug, Default)]
pub struct DefaultModelNormalizer;

impl ModelNormalizer for DefaultModelNormalizer {
    fn normalize(&self, context: &ModelContext) -> String {
        normalize_model_name(context.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateError {
    ZeroTokenUnit,
    EmptyCurrency,
    InvalidDecimal,
    TooManyDecimalPlaces,
    Overflow,
    MismatchedUnits,
}

impl fmt::Display for RateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroTokenUnit => "a token-rate unit must cover at least one token",
            Self::EmptyCurrency => "currency must be omitted or non-empty",
            Self::InvalidDecimal => "rate must be a non-negative plain decimal",
            Self::TooManyDecimalPlaces => "rate supports at most six decimal places",
            Self::Overflow => "rate is too large",
            Self::MismatchedUnits => "all four model rates must use the same unit",
        };
        f.write_str(message)
    }
}

impl std::error::Error for RateError {}

/// The denominator and optional currency attached to a token rate.
///
/// `tokens` is mandatory. `currency` is optional so the pricing core can represent non-monetary
/// or not-yet-classified rates without pretending they are dollars. Amounts are stored as integer
/// millionths of the unit named here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TokenRateUnit {
    tokens: u64,
    currency: Option<String>,
}

impl TokenRateUnit {
    pub fn new(tokens: u64, currency: Option<String>) -> Result<Self, RateError> {
        if tokens == 0 {
            return Err(RateError::ZeroTokenUnit);
        }
        let currency = currency
            .map(|value| {
                let value = value.trim().to_ascii_uppercase();
                (!value.is_empty() && !value.chars().any(char::is_whitespace))
                    .then_some(value)
                    .ok_or(RateError::EmptyCurrency)
            })
            .transpose()?;
        Ok(Self { tokens, currency })
    }

    pub fn usd_per_million_tokens() -> Self {
        Self {
            tokens: 1_000_000,
            currency: Some("USD".to_string()),
        }
    }

    pub fn tokens(&self) -> u64 {
        self.tokens
    }

    pub fn currency(&self) -> Option<&str> {
        self.currency.as_deref()
    }
}

/// An exact amount per [`TokenRateUnit`], stored in millionths rather than binary floating point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRate {
    amount_micros: u64,
    unit: TokenRateUnit,
}

impl TokenRate {
    pub fn from_micros(amount_micros: u64, unit: TokenRateUnit) -> Self {
        Self {
            amount_micros,
            unit,
        }
    }

    pub fn from_decimal(value: &str, unit: TokenRateUnit) -> Result<Self, RateError> {
        if value.is_empty() || value.starts_with(['-', '+']) {
            return Err(RateError::InvalidDecimal);
        }
        let mut parts = value.split('.');
        let whole = parts.next().ok_or(RateError::InvalidDecimal)?;
        let fraction = parts.next().unwrap_or("");
        if parts.next().is_some()
            || whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(RateError::InvalidDecimal);
        }
        if fraction.len() > 6 {
            return Err(RateError::TooManyDecimalPlaces);
        }
        let whole = whole.parse::<u64>().map_err(|_| RateError::Overflow)?;
        let fraction = if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<u64>()
                .map_err(|_| RateError::Overflow)?
                .checked_mul(10u64.pow((6 - fraction.len()) as u32))
                .ok_or(RateError::Overflow)?
        };
        let amount_micros = whole
            .checked_mul(AMOUNT_MICROS_PER_UNIT)
            .and_then(|value| value.checked_add(fraction))
            .ok_or(RateError::Overflow)?;
        Ok(Self::from_micros(amount_micros, unit))
    }

    pub fn amount_micros(&self) -> u64 {
        self.amount_micros
    }

    pub fn unit(&self) -> &TokenRateUnit {
        &self.unit
    }
}

/// One model's complete four-tier price estimate.
///
/// `cache_write` is the 5-minute prompt-cache write rate. [`TokenCounts`] retains one aggregate
/// cache-creation count, even when a source transcript exposes separate 5-minute and 1-hour writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPrice {
    input: TokenRate,
    cache_write: TokenRate,
    cache_read: TokenRate,
    output: TokenRate,
}

impl ModelPrice {
    pub fn new(
        input: TokenRate,
        cache_write: TokenRate,
        cache_read: TokenRate,
        output: TokenRate,
    ) -> Result<Self, RateError> {
        if [cache_write.unit(), cache_read.unit(), output.unit()]
            .into_iter()
            .any(|unit| unit != input.unit())
        {
            return Err(RateError::MismatchedUnits);
        }
        Ok(Self {
            input,
            cache_write,
            cache_read,
            output,
        })
    }

    pub fn from_micros(unit: TokenRateUnit, amounts: [u64; 4]) -> Self {
        Self {
            input: TokenRate::from_micros(amounts[0], unit.clone()),
            cache_write: TokenRate::from_micros(amounts[1], unit.clone()),
            cache_read: TokenRate::from_micros(amounts[2], unit.clone()),
            output: TokenRate::from_micros(amounts[3], unit),
        }
    }

    pub fn input(&self) -> &TokenRate {
        &self.input
    }

    pub fn cache_write(&self) -> &TokenRate {
        &self.cache_write
    }

    pub fn cache_read(&self) -> &TokenRate {
        &self.cache_read
    }

    pub fn output(&self) -> &TokenRate {
        &self.output
    }

    pub fn unit(&self) -> &TokenRateUnit {
        self.input.unit()
    }
}

/// An exact rational price accumulated from token counts. Floating point is used only when a
/// presentation-facing compatibility API requests a decimal value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceEstimate {
    numerator: u128,
    denominator: u128,
    currency: Option<String>,
}

impl PriceEstimate {
    fn new(numerator: u128, denominator: u128, currency: Option<String>) -> Self {
        Self {
            numerator,
            denominator,
            currency,
        }
    }

    pub fn currency(&self) -> Option<&str> {
        self.currency.as_deref()
    }

    pub fn is_currency(&self, currency: &str) -> bool {
        self.currency()
            .is_some_and(|value| value.eq_ignore_ascii_case(currency))
    }

    pub fn amount(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }

    pub fn amount_in(&self, currency: &str) -> Option<f64> {
        self.is_currency(currency).then(|| self.amount())
    }

    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        if self.currency != other.currency {
            return None;
        }
        let divisor = gcd(self.denominator, other.denominator);
        let left_factor = other.denominator / divisor;
        let right_factor = self.denominator / divisor;
        let numerator = self
            .numerator
            .checked_mul(left_factor)?
            .checked_add(other.numerator.checked_mul(right_factor)?)?;
        let denominator = self.denominator.checked_mul(left_factor)?;
        Some(Self::new(numerator, denominator, self.currency.clone()))
    }
}

fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        (left, right) = (right, left % right);
    }
    left
}

/// Per-model overrides plus the replaceable normalization policy used for built-in lookup.
///
/// Overrides match the complete raw recorded name before normalization, folding ASCII case only.
/// This lets a host override one snapshot without accidentally repricing every alias that
/// normalizes to the same catalog key.
#[derive(Clone)]
pub struct PriceTable {
    rates: BTreeMap<String, ModelPrice>,
    normalizer: Arc<dyn ModelNormalizer>,
}

impl fmt::Debug for PriceTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PriceTable")
            .field("rates", &self.rates)
            .finish_non_exhaustive()
    }
}

impl Default for PriceTable {
    fn default() -> Self {
        Self {
            rates: BTreeMap::new(),
            normalizer: Arc::new(DefaultModelNormalizer),
        }
    }
}

impl PriceTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_normalizer(normalizer: impl ModelNormalizer + 'static) -> Self {
        Self {
            rates: BTreeMap::new(),
            normalizer: Arc::new(normalizer),
        }
    }

    /// Record one raw context's complete rates, returning the replaced value when present.
    pub fn set(&mut self, context: &ModelContext, price: ModelPrice) -> Option<ModelPrice> {
        self.rates
            .insert(context.name().to_ascii_lowercase(), price)
    }

    /// Resolve a raw context through complete-name overrides (ASCII case-insensitive), then the
    /// configured normalizer and embedded catalog. No family fallback is performed.
    pub fn resolve(&self, context: &ModelContext) -> Option<ModelPrice> {
        self.rates
            .get(&context.name().to_ascii_lowercase())
            .cloned()
            .or_else(|| {
                let normalized = self.normalizer.normalize(context);
                builtin_prices().get(&normalized).cloned()
            })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PricingCatalog {
    schema_version: u64,
    cache_write_basis: String,
    sources: BTreeMap<String, PricingSource>,
    units: BTreeMap<String, PricingUnit>,
    models: Vec<PricingEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PricingSource {
    url: String,
    queried_at: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PricingUnit {
    tokens: u64,
    #[serde(default)]
    currency: Option<String>,
}

impl PricingUnit {
    fn rate_unit(&self) -> Result<TokenRateUnit, RateError> {
        TokenRateUnit::new(self.tokens, self.currency.clone())
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PricingEvidence {
    id: String,
    source: String,
    note: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PricingEntry {
    ids: Vec<String>,
    source: String,
    unit: String,
    input_micros: u64,
    cache_write_micros: u64,
    cache_read_micros: u64,
    output_micros: u64,
    #[serde(default)]
    evidence: Vec<PricingEvidence>,
}

impl PricingEntry {
    fn price(&self, unit: TokenRateUnit) -> ModelPrice {
        ModelPrice::from_micros(
            unit,
            [
                self.input_micros,
                self.cache_write_micros,
                self.cache_read_micros,
                self.output_micros,
            ],
        )
    }
}

fn parse_pricing_catalog(json: &str) -> Result<BTreeMap<String, ModelPrice>, String> {
    let catalog: PricingCatalog = serde_json::from_str(json)
        .map_err(|error| format!("pricing catalog must parse: {error}"))?;
    if catalog.schema_version != 3 {
        return Err(format!(
            "unsupported pricing schema {}",
            catalog.schema_version
        ));
    }
    if catalog.cache_write_basis != "5m" {
        return Err("aggregate cache writes must use the documented 5m rate".into());
    }
    if catalog.sources.is_empty() {
        return Err("pricing catalog needs sources".into());
    }
    for (name, source) in &catalog.sources {
        if !source.url.starts_with("https://") {
            return Err(format!("pricing source {name} needs an HTTPS URL"));
        }
        if parse_ts(&source.queried_at).is_none() {
            return Err(format!("pricing source {name} needs an RFC3339 queried_at"));
        }
    }

    let mut prices = BTreeMap::new();
    for entry in catalog.models {
        if entry.ids.is_empty() {
            return Err("pricing entry must name a model".into());
        }
        if !catalog.sources.contains_key(&entry.source) {
            return Err(format!(
                "pricing entry references unknown source {}",
                entry.source
            ));
        }
        let unit = catalog
            .units
            .get(&entry.unit)
            .ok_or_else(|| format!("pricing entry references unknown unit {}", entry.unit))?
            .rate_unit()
            .map_err(|error| format!("pricing unit {} is invalid: {error}", entry.unit))?;
        let mut evidenced = std::collections::BTreeSet::new();
        for evidence in &entry.evidence {
            if !entry.ids.contains(&evidence.id) {
                return Err(format!(
                    "pricing evidence id {} is not listed in its entry",
                    evidence.id
                ));
            }
            if !catalog.sources.contains_key(&evidence.source) {
                return Err(format!(
                    "pricing evidence for {} references unknown source {}",
                    evidence.id, evidence.source
                ));
            }
            if evidence.note.trim().is_empty() {
                return Err(format!("pricing evidence for {} needs a note", evidence.id));
            }
            if !evidenced.insert(evidence.id.clone()) {
                return Err(format!("duplicate pricing evidence for {}", evidence.id));
            }
        }
        let price = entry.price(unit);
        for id in entry.ids {
            let normalized = normalize_model_name(&id);
            if id != normalized {
                return Err(format!("pricing id {id} must already be normalized"));
            }
            if prices.insert(id.clone(), price.clone()).is_some() {
                return Err(format!("duplicate pricing id {id}"));
            }
        }
    }
    Ok(prices)
}

/// Parse and validate the repository-owned default catalog once. Keeping the data in JSON makes
/// adding a model a table edit; code owns only exact arithmetic, validation, and lookup semantics.
fn builtin_prices() -> &'static BTreeMap<String, ModelPrice> {
    static PRICES: OnceLock<BTreeMap<String, ModelPrice>> = OnceLock::new();
    PRICES.get_or_init(|| {
        parse_pricing_catalog(include_str!("../pricing.json"))
            .expect("embedded pricing.json must be valid")
    })
}

/// Whether three decimal fields form a plausible API snapshot date.
///
/// The year range is deliberately bounded: model snapshots are contemporary API artifacts, not
/// arbitrary numeric suffixes that should be allowed to borrow another model's price.
fn is_snapshot_date(year: &str, month: &str, day: &str) -> bool {
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        year.parse::<u32>(),
        month.parse::<u32>(),
        day.parse::<u32>(),
    ) else {
        return false;
    };
    if !(2000..=2999).contains(&year) || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&day)
}

/// Normalize separators and strip a terminal API snapshot date. This handles
/// `claude-opus-4-1-20250805` and `gpt-5-2025-08-07` without family substring matching; every
/// non-date portion still has to be listed explicitly in `pricing.json`.
fn normalize_model_name(model: &str) -> String {
    let normalized = model.to_lowercase().replace('.', "-");
    let parts: Vec<&str> = normalized.split('-').collect();
    let trim = match parts.as_slice() {
        [prefix @ .., year, month, day] if is_snapshot_date(year, month, day) => Some(prefix.len()),
        [prefix @ .., date]
            if date.len() == 8
                && date.is_ascii()
                && is_snapshot_date(&date[..4], &date[4..6], &date[6..]) =>
        {
            Some(prefix.len())
        }
        _ => None,
    };
    trim.map_or(normalized.clone(), |len| parts[..len].join("-"))
}

/// Exact best-effort price for a model context and its four recorded token tiers.
pub fn estimate_price(
    model: &ModelContext,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
) -> Option<PriceEstimate> {
    estimate_price_with(
        &PriceTable::default(),
        model,
        input,
        cache_creation,
        cache_read,
        output,
    )
}

/// [`estimate_price`] against host-supplied rates. Each tier uses its explicit exact rate;
/// `cache_creation` is aggregate and therefore uses the catalog's documented 5-minute rate.
pub fn estimate_price_with(
    prices: &PriceTable,
    model: &ModelContext,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
) -> Option<PriceEstimate> {
    let price = prices.resolve(model)?;
    let numerator = [
        (input, price.input()),
        (cache_creation, price.cache_write()),
        (cache_read, price.cache_read()),
        (output, price.output()),
    ]
    .into_iter()
    .try_fold(0u128, |sum, (tokens, rate)| {
        let component = u128::from(tokens).checked_mul(u128::from(rate.amount_micros()))?;
        sum.checked_add(component)
    })?;
    let denominator =
        u128::from(price.unit().tokens()).checked_mul(u128::from(AMOUNT_MICROS_PER_UNIT))?;
    Some(PriceEstimate::new(
        numerator,
        denominator,
        price.unit().currency().map(str::to_string),
    ))
}

/// Backward-compatible USD projection from a model name. New code should use [`estimate_price`]
/// with [`ModelContext`] and keep the exact [`PriceEstimate`] until presentation.
pub fn estimate_cost(
    model: &str,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
) -> Option<UsdCost> {
    estimate_cost_with(
        &PriceTable::default(),
        model,
        input,
        cache_creation,
        cache_read,
        output,
    )
}

/// [`estimate_cost`] against host-supplied rates. Rates without explicit USD currency are not
/// projected into this USD-only compatibility API.
pub fn estimate_cost_with(
    prices: &PriceTable,
    model: &str,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
) -> Option<UsdCost> {
    estimate_price_with(
        prices,
        &ModelContext::new(model),
        input,
        cache_creation,
        cache_read,
        output,
    )
    .and_then(|price| price.amount_in("USD"))
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

    fn p(input: &str, cache_write: &str, cache_read: &str, output: &str) -> ModelPrice {
        let unit = TokenRateUnit::usd_per_million_tokens();
        ModelPrice::new(
            TokenRate::from_decimal(input, unit.clone()).unwrap(),
            TokenRate::from_decimal(cache_write, unit.clone()).unwrap(),
            TokenRate::from_decimal(cache_read, unit.clone()).unwrap(),
            TokenRate::from_decimal(output, unit).unwrap(),
        )
        .unwrap()
    }

    fn resolve(table: &PriceTable, model: &str) -> Option<ModelPrice> {
        table.resolve(&ModelContext::new(model))
    }

    fn mutated_gpt_alias_evidence(mutate: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut document: serde_json::Value =
            serde_json::from_str(include_str!("../pricing.json")).unwrap();
        let row = document["models"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| {
                row["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id == "gpt-5-6")
            })
            .unwrap();
        mutate(&mut row["evidence"]);
        serde_json::to_string(&document).unwrap()
    }

    #[test]
    fn catalog_records_exact_alias_evidence() {
        let catalog: PricingCatalog =
            serde_json::from_str(include_str!("../pricing.json")).unwrap();
        let evidence_for = |id: &str| {
            catalog
                .models
                .iter()
                .flat_map(|entry| &entry.evidence)
                .find(|evidence| evidence.id == id)
                .map(|evidence| evidence.source.as_str())
        };
        assert_eq!(evidence_for("gpt-5-6"), Some("openai_gpt_5_6_sol_alias"));
        assert_eq!(
            evidence_for("us-anthropic-claude-opus-4-20250514-v1:0"),
            Some("aws_bedrock_opus_4_snapshot")
        );
    }

    #[test]
    fn evidence_for_an_unlisted_alias_is_rejected() {
        let json = mutated_gpt_alias_evidence(|evidence| {
            evidence[0]["id"] = serde_json::json!("gpt-5-6-codex");
        });
        assert!(parse_pricing_catalog(&json)
            .unwrap_err()
            .contains("is not listed in its entry"));
    }

    #[test]
    fn evidence_with_an_unknown_source_is_rejected() {
        let json = mutated_gpt_alias_evidence(|evidence| {
            evidence[0]["source"] = serde_json::json!("missing-source");
        });
        assert!(parse_pricing_catalog(&json)
            .unwrap_err()
            .contains("references unknown source"));
    }

    #[test]
    fn evidence_without_a_rationale_is_rejected() {
        let json = mutated_gpt_alias_evidence(|evidence| {
            evidence[0]["note"] = serde_json::json!("  ");
        });
        assert!(parse_pricing_catalog(&json)
            .unwrap_err()
            .contains("needs a note"));
    }

    #[test]
    fn duplicate_evidence_for_one_alias_is_rejected() {
        let json = mutated_gpt_alias_evidence(|evidence| {
            let duplicate = evidence[0].clone();
            evidence.as_array_mut().unwrap().push(duplicate);
        });
        assert!(parse_pricing_catalog(&json)
            .unwrap_err()
            .contains("duplicate pricing evidence"));
    }

    /// Pins all four billing tiers where vendor/model families differ. Both vendors' tables were
    /// queried 2026-09-09; `pricing.json` records the exact query timestamp and source URLs.
    #[test]
    fn prices_match_the_published_tables() {
        for (model, want) in [
            ("claude-fable-5-1", p("10", "12.5", "0.25", "50")),
            ("claude-mythos-5", p("10", "12.5", "1", "50")),
            ("claude-opus-5", p("5", "6.25", "0.5", "25")),
            ("claude-opus-4-8", p("5", "6.25", "0.5", "25")),
            ("claude-opus-4-1-20250805", p("15", "18.75", "1.5", "75")),
            (
                "us.anthropic.claude-opus-4-20250514-v1:0",
                p("15", "18.75", "1.5", "75"),
            ),
            ("claude-sonnet-5", p("2", "2.5", "0.2", "10")),
            ("claude-sonnet-4-6", p("3", "3.75", "0.3", "15")),
            ("claude-3-7-sonnet-20250219", p("3", "3.75", "0.3", "15")),
            ("claude-haiku-4-5-20251001", p("1", "1.25", "0.1", "5")),
            ("claude-haiku-3-5", p("0.8", "1", "0.08", "4")),
            ("claude-3-5-haiku-20241022", p("0.8", "1", "0.08", "4")),
            ("claude-3-haiku-20240307", p("0.25", "0.3", "0.03", "1.25")),
            ("gpt-6-astra", p("10", "10", "1", "50")),
            ("gpt-5.6-sol", p("4", "4", "0.4", "20")),
            ("gpt-5.6", p("4", "4", "0.4", "20")),
            ("gpt-5-6-terra", p("2", "2", "0.2", "12")),
            ("gpt-5.6-luna", p("0.2", "0.2", "0.02", "1.2")),
            ("gpt-daybreak-red-latest", p("12.5", "12.5", "1.25", "75")),
            ("gpt-5.5", p("5", "5", "0.5", "30")),
            ("gpt-5.4-mini", p("0.75", "0.75", "0.075", "4.5")),
            ("gpt-5.3-codex", p("1.75", "1.75", "0.175", "14")),
            ("gpt-5.2", p("1.75", "1.75", "0.175", "14")),
            ("gpt-5-2025-08-07", p("1.25", "1.25", "0.125", "10")),
            ("gpt-5-mini", p("0.25", "0.25", "0.025", "2")),
            ("gpt-5.1-codex-mini", p("0.25", "0.25", "0.025", "2")),
            ("gpt-5-nano", p("0.05", "0.05", "0.005", "0.4")),
        ] {
            assert_eq!(resolve(&PriceTable::new(), model), Some(want), "{model}");
        }
    }

    /// Lookup normalizes only documented spelling variations and terminal snapshot dates. It
    /// never lets an unlisted family member borrow a plausible sibling's price.
    #[test]
    fn lookup_is_normalized_but_exact() {
        for model in ["GPT-5.6-SOL-20260908", "gpt-5.6-sol-2026-09-08"] {
            assert_eq!(
                resolve(&PriceTable::new(), model),
                Some(p("4", "4", "0.4", "20")),
                "{model}"
            );
        }
        for model in [
            "some-unknown-model",
            "gpt-5.4-cyber",
            "gpt-5.6-mini",
            "gpt-5.6-codex",
            "gpt-5.5-pro",
            "gpt-5.4-pro",
            "gpt-5.2-pro",
            "gpt-5-pro",
            "codex",
            "gpt-7-astra",
            "gpt-5.4-sol",
            "gpt-5.6-sol-preview",
            "gpt-6-some-unannounced-tier",
            "gpt-5.6-sol-99999999",
            "gpt-5.6-sol-20260230",
            "gpt-5.6-sol-2026-13-01",
            "us.anthropic.claude-opus-6-20260901-v1:0",
        ] {
            assert_eq!(resolve(&PriceTable::new(), model), None, "{model}");
        }
    }

    #[test]
    fn zero_token_model_buckets_have_no_cost_state() {
        let per_model = BTreeMap::from([
            ("gpt-5-6".to_string(), TokenCounts::default()),
            ("unknown-model".to_string(), TokenCounts::default()),
        ]);
        assert_eq!(total_cost(&per_model), (None, false));
    }

    /// The four independent columns must all reach the formula; otherwise Claude's exceptional
    /// Fable/Mythos 5.1 cache-read discount is lost behind a universal multiplier.
    #[test]
    fn cost_uses_each_catalog_rate() {
        assert_eq!(
            estimate_cost(
                "claude-fable-5-1",
                1_000_000,
                1_000_000,
                1_000_000,
                1_000_000
            ),
            Some(72.75)
        );
    }

    /// The override must be inert until used. This keeps every caller that passes an empty table
    /// on the built-in catalog path.
    #[test]
    fn an_empty_table_is_the_builtin_table() {
        let empty = PriceTable::new();
        assert_eq!(resolve(&empty, "anything"), None);
        for model in ["claude-fable-5", "gpt-6-astra", "gpt-5.4-cyber", "unknown"] {
            assert_eq!(
                estimate_cost_with(&empty, model, 1_000, 2_000, 3_000, 4_000),
                estimate_cost(model, 1_000, 2_000, 3_000, 4_000),
                "{model}"
            );
        }
    }

    #[test]
    fn a_named_model_takes_the_hosts_complete_rate() {
        let custom = p("3", "4", "0.5", "6");
        let mut table = PriceTable::new();
        assert_eq!(
            table.set(&ModelContext::new("gpt-6-astra"), custom.clone()),
            None
        );
        assert_eq!(resolve(&table, "gpt-6-astra"), Some(custom));
        assert_eq!(
            estimate_cost_with(
                &table,
                "gpt-6-astra",
                1_000_000,
                1_000_000,
                1_000_000,
                1_000_000
            ),
            Some(13.5)
        );
        assert_eq!(
            resolve(&table, "gpt-5-mini"),
            Some(p("0.25", "0.25", "0.025", "2"))
        );
    }

    #[test]
    fn override_matching_is_exact_but_case_insensitive() {
        let custom = p("99", "98", "97", "96");
        let mut table = PriceTable::new();
        assert_eq!(table.set(&ModelContext::new("GPT-5"), custom.clone()), None);
        assert_eq!(resolve(&table, "gpt-5"), Some(custom.clone()));
        assert_eq!(resolve(&table, "GpT-5"), Some(custom));
        assert_eq!(
            resolve(&table, "gpt-5-mini"),
            Some(p("0.25", "0.25", "0.025", "2"))
        );
    }

    /// Pricing a model the built-in catalog misses both adds it to the sum and clears the `≥`.
    #[test]
    fn an_override_can_close_the_lower_bound_gap() {
        let tokens = TokenCounts {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        let per_model = BTreeMap::from([("gpt-5.4-cyber".to_string(), tokens)]);
        assert_eq!(total_cost(&per_model), (None, true));

        let mut table = PriceTable::new();
        assert_eq!(
            table.set(
                &ModelContext::new("gpt-5.4-cyber"),
                p("12.5", "12.5", "1.25", "75")
            ),
            None
        );
        assert_eq!(total_cost_with(&table, &per_model), (Some(87.5), false));
        assert_eq!(tokens.cost_with(&table, "gpt-5.4-cyber"), Some(87.5));
    }

    #[test]
    fn sum_overflow_preserves_the_known_lower_bound() {
        let unit = TokenRateUnit::new(1, Some("USD".into())).unwrap();
        let price = ModelPrice::from_micros(unit, [u64::MAX, 0, 0, 0]);
        let mut table = PriceTable::new();
        for model in ["overflow-a", "overflow-b"] {
            table.set(&ModelContext::new(model), price.clone());
        }
        let tokens = TokenCounts {
            input: u64::MAX,
            ..Default::default()
        };
        let per_model = BTreeMap::from([
            ("overflow-a".to_string(), tokens),
            ("overflow-b".to_string(), tokens),
        ]);

        let one_known = (u128::from(u64::MAX) * u128::from(u64::MAX)) as f64 / 1_000_000.0;
        assert_eq!(total_cost_with(&table, &per_model), (Some(one_known), true));
    }

    #[test]
    fn decimals_are_exact_and_invalid_values_are_rejected() {
        let unit = TokenRateUnit::usd_per_million_tokens();
        for (value, micros) in [("0.075", 75_000), ("0.025", 25_000), ("0.005", 5_000)] {
            assert_eq!(
                TokenRate::from_decimal(value, unit.clone())
                    .unwrap()
                    .amount_micros(),
                micros,
                "{value}"
            );
        }
        for value in ["-1", "+1", "1e-3", "", ".5", "1.2.3"] {
            assert_eq!(
                TokenRate::from_decimal(value, unit.clone()),
                Err(RateError::InvalidDecimal),
                "{value}"
            );
        }
        assert_eq!(
            TokenRate::from_decimal("0.0000001", unit),
            Err(RateError::TooManyDecimalPlaces)
        );
    }

    #[test]
    fn units_are_required_and_four_tiers_must_agree() {
        assert_eq!(
            TokenRateUnit::new(0, Some("USD".into())),
            Err(RateError::ZeroTokenUnit)
        );
        for currency in ["", "   ", "US D"] {
            assert_eq!(
                TokenRateUnit::new(1_000_000, Some(currency.into())),
                Err(RateError::EmptyCurrency),
                "{currency:?}"
            );
        }

        let usd = TokenRateUnit::usd_per_million_tokens();
        let credits = TokenRateUnit::new(1_000_000, None).unwrap();
        assert_eq!(
            ModelPrice::new(
                TokenRate::from_micros(1, usd.clone()),
                TokenRate::from_micros(1, usd.clone()),
                TokenRate::from_micros(1, credits),
                TokenRate::from_micros(1, usd),
            ),
            Err(RateError::MismatchedUnits)
        );
    }

    #[test]
    fn currency_is_optional_but_legacy_cost_is_usd_only() {
        let unit = TokenRateUnit::new(1_000, None).unwrap();
        let price = ModelPrice::from_micros(unit, [250_000, 250_000, 25_000, 2_000_000]);
        let context = ModelContext::new("local-model");
        let mut table = PriceTable::new();
        table.set(&context, price);

        let estimate = estimate_price_with(&table, &context, 1_000, 0, 0, 0).unwrap();
        assert_eq!(estimate.currency(), None);
        assert_eq!(estimate.amount(), 0.25);
        assert_eq!(
            estimate_cost_with(&table, "local-model", 1_000, 0, 0, 0),
            None
        );
        assert_eq!(
            total_cost_with(
                &table,
                &BTreeMap::from([(
                    "local-model".to_string(),
                    TokenCounts {
                        input: 1_000,
                        ..Default::default()
                    }
                )])
            ),
            (None, true)
        );
    }

    #[test]
    fn normalizer_is_replaceable_but_raw_override_wins() {
        struct AliasNormalizer;
        impl ModelNormalizer for AliasNormalizer {
            fn normalize(&self, context: &ModelContext) -> String {
                match context.name() {
                    "internal-astra" => "gpt-6-astra".to_string(),
                    other => other.to_lowercase(),
                }
            }
        }

        let mut table = PriceTable::with_normalizer(AliasNormalizer);
        assert_eq!(
            resolve(&table, "internal-astra"),
            Some(p("10", "10", "1", "50"))
        );

        let custom = p("1", "2", "0.1", "3");
        table.set(&ModelContext::new("internal-astra"), custom.clone());
        assert_eq!(resolve(&table, "internal-astra"), Some(custom));
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
