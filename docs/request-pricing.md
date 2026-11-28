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
GPT-5.2, GPT-5.1 and GPT-5 Fast is 2x, and GPT-5 mini Fast is 1.8x. GPT-5 nano
has no Fast rate. None of these five has a long-context surcharge. The retired
gpt-5-codex, gpt-5.1-codex, gpt-5.1-codex-max, gpt-5.1-codex-mini and gpt-5.2-codex
have no request rules, so every request on them is an estimate.
Only explicitly listed aliases share rules; unknown snapshots never borrow prices.

Qwen4-Max (Alibaba Model Studio) has one bracket to 1M input tokens, so no long-context
surcharge; Batch is half, per the label on both pricing pages. No Fast, Flex or Ultrafast rate
is published for it, so those tiers price at standard as an estimate. Its rates are the
Beijing / global deployment tier, which the international pricing page publishes in USD;
the international (Singapore) and US deployments carry different rates, and the catalog
has no per-region rows, so every request on the model prices at the Beijing / global
tier. Its cache write and hit rates are the values the international page printed on
2026-11-23; the page removed those sub-tables afterwards, so they are held provisionally
at the read-time values as agreed in review, corroborated by the console's live CNY
cache table (source bailian_console_qwen4; 12.5 / 20 / 0.5 yuan per million tokens for the 5m creation, the 1h
creation and a hit, explicit or implicit) at the published 10-yuan-to-$1.413 input
ratio, and by the explicit-cache (Qwen4) page's creation ratios (about 125% of input
for TTL 5m, about 200% for TTL 1h).

The returned incomplete-evidence flag also covers unsupported tiers and GPT
models without verified request rules. These estimates are not necessarily lower
bounds: a missing tier might be Flex or Fast. Prices use exact integer arithmetic
until projection. Rules live in `pricing.json`, so changes invalidate priced caches
through `PRICING_FINGERPRINT`. This is current API-equivalent token pricing, not
historical invoice reconstruction or Codex subscription-credit consumption.
A request in a discounted background completion window (2 or 8 hours, on
GPT-6.2 Astra and Sol, GPT-6.1 Terra and GPT-6 Luna) is priced at Standard: no
transcript records a background request or its window, so the discount, 30% or
40%, is not applied, and such a request reads high with no estimate flag.

Consumers should retain `PricedTokens` classes inside each time/model bucket,
merge identical classes, and separately retain tokens without evidence. Never
use a session's final runtime settings to reprice its earlier requests.
`FOLD_VERSION` invalidates prior cursors so available sources can be refolded.

Rates were verified against the [official pricing page](https://developers.openai.com/api/docs/pricing)
and [Astra model page](https://developers.openai.com/api/docs/models/gpt-6-astra)
on 2026-09-28, and for GPT-5.2, 5.1, 5, 5 mini and 5 nano on 2026-11-14. Qwen4-Max was
verified against Model Studio's [domestic](https://help.aliyun.com/zh/model-studio/model-pricing)
and [international](https://www.alibabacloud.com/help/zh/model-studio/model-pricing) pricing
pages on 2026-11-23 and again on 2026-11-28, and against its
[explicit-cache (Qwen4)](https://help.aliyun.com/zh/model-studio/explicit-cache-qwen4) and
[context-cache](https://help.aliyun.com/zh/model-studio/context-cache) pages on 2026-11-28;
the console's [qwen4-max model page](https://bailian.console.aliyun.com/cn-beijing/model/market/detail/qwen4-max)
(readable without login) was read on 2026-11-28T12:06Z and prints the CNY cache table;
the international page's qwen4-max cache sub-tables, read on 2026-11-23, were removed by
the re-read, so the stored cache rates are read-time provisional values.
This is a token-price estimate, not a provider invoice.
