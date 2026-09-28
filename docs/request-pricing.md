# Request pricing for metric consumers

`MetricsAccumulator::request_pricing()` describes the delta emitted by the most
recent push. It returns no evidence for unrelated lines, duplicate usage, or a
cumulative jump spanning more than the recorded last request. It is additive;
existing per-model totals and model-only pricing APIs keep their semantics.

The Codex adapter compares all four last-request counters with the cumulative
increment, after removing inherited fork history. The long-context threshold
uses the request's full input, including cache reads and writes: exactly 272,000
is short, 272,001 is long. A client service-tier setting is carried through
cursor restore, but is not evidence of the tier actually returned by a provider.
A null setting clears the previous setting. Missing information stays unknown.

`RequestPricing::cost_with` applies model-specific catalog rules to standard
short-context rates (including host overrides). GPT-6 Astra/Sol/Luna,
GPT-5.6 Sol/Terra/Luna and GPT-5.5/5.4 charge 2x input/cache and 1.5x output
above 272K input tokens, for the whole request. Flex and Batch are half standard.
Fast is 2x for GPT-6 and GPT-5.6; GPT-5.5 short-context Fast is 2.5x.
GPT-5.4 long-context Fast falls back to standard. GPT-5.5 has no published
long-context Fast rate, so that combination uses a standard-rate estimate.
GPT-5.4 mini/nano and GPT-5.3-Codex do not inherit the long-context surcharge.
Only explicitly listed aliases share rules; unknown snapshots never borrow prices.

The returned incomplete-evidence flag also covers unsupported tiers and GPT
models without verified request rules. These estimates are not necessarily lower
bounds: a missing tier might be Flex or Fast. Prices use exact integer arithmetic
until projection. Rules live in `pricing.json`, so changes invalidate priced caches
through `PRICING_FINGERPRINT`. This is current API-equivalent token pricing, not
historical invoice reconstruction or Codex subscription-credit consumption.

Consumers should retain `PricedTokens` classes inside each time/model bucket,
merge identical classes, and separately retain tokens without evidence. Never
use a session's final runtime settings to reprice its earlier requests.
`FOLD_VERSION` invalidates prior cursors so available sources can be refolded.

Rates were verified against the [official pricing page](https://developers.openai.com/api/docs/pricing)
and [Astra model page](https://developers.openai.com/api/docs/models/gpt-6-astra)
on 2026-09-28. This is a token-price estimate, not a provider invoice.
