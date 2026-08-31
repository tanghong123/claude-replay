# Design: Qoder credits → USD — the money figure a credit-billed agent already knows

---
Status: Accepted
Created: 2026-08-27
Author: Claude (design-intent-first)
---

**The gap.** Qoder bills in *credits*, not tokens: its `usage` carries `credits` with
`input_tokens`/`output_tokens` zeroed and an opaque model alias (`cmodel`, `qwork-ultimate`).
The fold already reads that — `agents/claude/metrics.rs` sums `credits` into the reserved
`credits_micro` extra key, and the shared footer renders `~12.16 credits`. But `cost_usd`
stays `None`, because it is derived from `total_cost(per_model)` and Qoder's token counts are
zero under a model no price table knows. Consequence: **a Qoder session is worth $0 to every
money surface** — the TUI/HTML footer shows no `$`, the HTML panel omits `cost`, and the
monitor's machine-wide ledger (`claude-monitor/src/cost.rs`) banks `None`, so the rail's
per-project total (`$X · N`) silently under-reports by exactly the Qoder half of a machine's
work. That is the same class of failure §14 of `design/claude-monitor.md` was written to kill
(cost gated on visits showed $121 of $2,421); the ledger fixed *which sessions are priced*, not
*which agents can be priced at all*.

**The anchor.** <https://docs.qoder.com/account/pricing> (checked 2026-08-27) publishes three
subscription tiers that agree exactly: Pro $20 / 2,000 credits, Pro+ $60 / 6,000, Ultra
$200 / 20,000 — **$0.01 per credit** in all three. That is a published, verifiable rate, in the
same class as the model list prices `metrics::price()` already hardcodes, and it converts a
figure the agent *measured* (credits deducted) rather than one we estimate (tokens × rate).

## Section 1: Invariants

- **INV-1**: `credits_micro` present and > 0 ⟹ `cost_usd == Some(credits × 0.01)`, exactly
  (`credits = credits_micro / 1e6`). Violation makes the reported money a different number from
  what Qoder deducted.
- **INV-2**: `credits_micro` absent ⟹ `cost_usd` and `cost_partial` are bit-for-bit what
  `total_cost(per_model)` returns today. Violation changes every Claude/Codex/QoderWork session's
  cost — the byte gate's whole corpus.
- **INV-3**: The credits-derived cost never *adds* to the token-derived one. A session's money is
  one figure from one currency; summing an agent's own deduction with an estimate of the tokens
  it reported as zero double-counts on the day Qoder starts reporting both.
- **INV-4**: The footer keeps both segments when credits exist — `~$0.02 · ~2.00 credits`, in that
  order, both at shed priority 7. The native figure stays visible beside the derived one, so no
  reader has to trust the conversion blind.
- **INV-5**: The monitor's persisted ledger entry cannot serve a pre-change `cost: null` for a
  credits-bearing transcript after this change lands. Violation is the documented "v8 lesson" in
  `cost::ledger_version()`: the size/mtime fast path never re-folds an idle transcript, so an
  un-versioned rollout leaves every already-scanned Qoder session at $0 **forever**.

## Section 2: Contracts

```
claude_replay_engine::metrics::USD_PER_CREDIT: f64 = 0.01
  the published subscription rate; one constant, pinned by a test that cites the plan table.

claude_replay_engine::metrics::credits_cost(extra: &BTreeMap<String, u64>) -> Option<UsdCost>
  accepts: any metrics extra bag (an agent's, possibly empty).
  returns: Some(extra["credits_micro"] as f64 / 1e6 * USD_PER_CREDIT) when the key is present
           and non-zero; None when the key is absent or zero.
  raises:  never — total function, no panic path (u64 → f64 is lossless at these magnitudes).

claude_metrics::Acc::finish(self) -> Metrics                             [modified]
  accepts: unchanged.
  returns: unchanged EXCEPT cost_usd = credits_cost(&extra).or(token_total). cost_partial is
           untouched: Qoder's token counts are zero, so total_cost reports no omission, and a
           hypothetical mixed session keeps its honest `≥` marker.
  raises:  never (unchanged).

Metrics::credits() / credits_label()                                     [unchanged]
  still the native figure; `cost_label` still renders the USD one.
```

## Section 3: Non-goals

- **NG-1**: The add-on **Credit Pack** rate ($20 / 1,500 ≈ $0.0133/credit). Reason: the transcript
  records no purchase provenance, so which credits a line consumed is unknowable from disk. One
  rate, the one three of four published plans agree on, is the honest approximation.
- **NG-2**: The **tier multiplier** table (Auto ~1.0× / Ultimate ~1.6× / Performance ~1.1× /
  Efficient ~0.3× / Lite free, <https://docs.qoder.com/user-guide/chat/model-tier-selector>).
  Reason: those describe how many *credits* a task consumes, not how many dollars a credit is —
  and the transcript already reports the post-multiplier credits. Applying them would double-count
  the multiplier.
- **NG-3**: **Backfilling historical QoderWork money.** Older transcripts in the measured corpus
  carry no `usage` at all (issue #30 / taskq #2: zero credits lines across 52 sessions), so nothing on disk
  can reconstruct their cost. Current QoderWork transcripts can carry zeroed token counts plus
  `usage.credits`; because QoderWork delegates to the same metrics fold, those sessions are priced
  by this change while historical sessions honestly remain at "no cost figure".
- **NG-4**: **issue #30 / taskq #2** (absent `billable` defaults to billable). Reason: adjacent and still
  evidence-blocked; this change inherits the existing `billable`/`original_credits` rules untouched.
- **NG-5**: Per-plan or user-configured rates. Reason: the same argument `price()`'s doc comment
  makes — user configuration adds a way for the number to be wrong that no test can see.

## Section 4: Assumptions

- **A-1**: `usage.credits` is the **post-adjustment** deduction (not `original_credits`) and
  failed calls carry `billable:false`. Owner: `agents/claude/metrics.rs`'s existing credits fold,
  which already encodes both rules; this change consumes its output and adds no new parsing.
- **A-2**: Only Qoder-family transcripts (Qoder CLI and current QoderWork) carry `credits` in
  `usage`; real Claude usage has no such field. Owner: the shared Claude-shaped metrics adapter,
  with separate Qoder and QoderWork provenance tests pinning both store identities.
- **A-3**: A `credits_micro` bump survives a resumed fold, so `finish()` sees the full session
  total. Owner: `Acc::reseed` / `MetricsTotals` (#96 §7) — the extra bag is part of the resumable
  state.
- **A-4**: Bumping `LEDGER_SHAPE` invalidates every persisted ledger entry and forces a cold
  re-fold. Owner: `claude-monitor/src/cost.rs::ledger_version()`, whose version gate `load_entry`
  already enforces.
- **A-5**: The published $0.01/credit rate is current as of 2026-08-27. Owner: the pinning test's
  dated citation — the same mechanism that caught `price()` shipping the retired Opus rate.
