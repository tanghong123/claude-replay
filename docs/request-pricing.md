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

`RequestPricing::cost_with` applies GPT-6 Astra's context and service-tier factors
to standard short-context catalog rates (or an explicit host override with the
same meaning). It returns a separate incomplete-evidence flag. Such estimates
are not necessarily lower bounds: a missing tier might be Flex or Fast. Other
models continue to use their catalog rates. Prices are recomputed from the
original token counts, with integer arithmetic until the USD projection.

Consumers should retain `PricedTokens` classes inside each time/model bucket,
merge identical classes, and separately retain tokens without evidence. Never
use a session's final runtime settings to reprice its earlier requests.
`FOLD_VERSION` invalidates prior cursors so available sources can be refolded.

Rates were verified against the [official pricing page](https://developers.openai.com/api/docs/pricing)
and [Astra model page](https://developers.openai.com/api/docs/models/gpt-6-astra)
on 2026-09-28. This is a token-price estimate, not a provider invoice.
