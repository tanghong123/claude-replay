# Design: a common eliding line reader — no consumer buffers a whole raw line

> **Status:** study only (#193) — a design to review. **Build nothing until it is reviewed.**
> The exit condition for #193 is *this document read and accepted*, not a merge.

Inspired by, and generalizing, agent-metrics' adopted proposal
`~/code/agent-metrics/docs/proposals/bounded-line-reads.md`, which fixes this locally in its own
`timeline.rs` and notes the approach "ports cleanly" to the engine. Its measurements over ~400
real Claude-format transcripts (212,236 lines) anchor the whole design and are taken as given
here: largest single line **8.08 MB**, 83 lines > 1 MB, and **~95 % of the bulk is base64
attachment bodies** (`toolUseResult.file.base64` and its `tool_result` twin) — none of it
metric-bearing. (It is a sibling repo of this author's, not a third-party borrow, so it is
credited by reference here, not in `ATTRIBUTION.md`, which records external MIT sources.)

The question this study answers: **can ONE eliding reader live in the engine, shared by every
line consumer (including agent-metrics through the seam), instead of each downstream repo growing
its own scanner that drifts?** The answer is yes, with one architectural constraint the seed
never faced (the engine is agent-free; agent-metrics is not) and one correctness asymmetry it
never faced (the engine *renders* what it decodes, and Codex re-derives attachments from the
payload string).

---

## 1. Problem

`DESIGN.md` commits the fold to never loading a whole session into memory. The engine honours
that at the *session* level — it streams line by line, a resident `Session` holds only attachment
**locators** (`AttachmentContent::Deferred { at, index }`, `model.rs:266`), and the
`resident_window.rs` test pins the open frontier to ≤ 200 blocks. It honours it **not at all at
the line level**: every whole-file and tail-delta read path buffers each raw line in full, then
hands it to `serde_json::from_str::<Value>` (a DOM several times the text size) and to
`decode_line`. A single 8 MB line is materialized several times over — and the engine does this
*specifically to build a `Deferred` locator that deliberately holds none of those bytes*. That is
the prize: today the engine buffers 8 MB to record an offset and an ordinal.

There is **no line-size or attachment-size cap anywhere in the engine or adapters today** — this
design introduces the first one.

---

## 2. What is actually large (the finding that determines the design)

From the seed's measurements, across the 141 lines ≥ 500 KB (~163 MB of text):

| json path | share |
| --- | --- |
| `$.toolUseResult.file.base64` (+ its `message.content[].content[]` `tool_result` twin) | ~99 % |
| `$.toolUseResult[].source.data`, one giant `$.message.content[].text` | the rest |

**~95 % of the bulk is base64 attachment bodies, and none of it is metric-bearing.** Everything
the metrics fold reads is a small scalar — `usage.*` (five ints), `model`, `id`, `requestId`,
`timestamp`, `type`, `subtype`, `compactMetadata.*` — none can exceed 64 KB. A line is huge
*because of* the one part of it the fold has no use for.

**Where the engine differs from agent-metrics** — the two facts that make this more than a port:

1. **The engine surfaces `decode_line` output.** Blocks carry assistant text and tool output;
   a >64 KB string is not automatically inert here the way it is for a pure metrics fold. So the
   generic "elide any big string" rule is content-neutral for metrics but *not* for blocks.
2. **The engine is agent-free (#87).** The scanner would live in `claude-replay-engine`, behind
   the `agents_import_only_the_seam` audit. It **cannot know** that `toolUseResult.file.base64`
   is a Claude path or that Codex wraps images as `data:<mime>;base64,…`. Any per-agent knowledge
   must enter through `engine/seam.rs`, exactly as `LinePreprocessor` and `Shaping` do.

---

## 3. The mechanism (adopted from the seed, with one deliberate divergence)

Read each line through a JSON-aware streaming filter that copies bytes through, except that a
string value longer than `ELIDE_STRING_BYTES` (64 KB) is **consumed without copying** and
replaced by a short placeholder. The output stays syntactically valid JSON with the same shape,
so every downstream consumer — `serde_json::from_str::<Value>`, `LinePreprocessor::process`,
`decode_line`, the metrics `push` — works unmodified and sees a small line.

```
{"type":"user","toolUseResult":{"file":{"base64":"data:image/png;base64,iVBORw0KGgo…<elided:8117324>"}}, …}
```

**The divergence: a prefix-preserving placeholder, not the seed's bare `"<elided:N>"`.** This is
forced by the Codex asymmetry (§5.3) and, happily, resolves the #87 constraint agent-neutrally.
Keep the first **K = 64** bytes of the elided value, then `…<elided:N>`. Sixty-four bytes
comfortably covers both discriminators the engine must not break but must not understand:

- Codex's `data:<mime>;base64,` header (`data_image`, `codex/model.rs:583`) — ~22 bytes.
- Raw base64 magic-byte signatures (`encoded_image`, `codex/model.rs:600`: `iVBOR`→PNG,
  `/9j/`→JPEG, `R0lGOD`→GIF, `UklGR`→WEBP).

The engine keeps a fixed-length prefix of *any* oversized string; it never parses or recognizes
the prefix. That is agent-neutral (no seam knowledge in the scanner) yet keeps every adapter's
payload-shape recognizer working. `N` is the original byte count, carried so a consumer can tell
an elided value from a literal and know what it weighed (and, later, seek to it — see §7 fast
path forward-compat in the seed).

- **Fast path:** lines under `SCAN_THRESHOLD` (256 KB) never touch the scanner — fill a buffer to
  the threshold; a newline first ⇒ the existing path, unchanged (~99.9 % of lines); the threshold
  crossed without a newline ⇒ run the scanner over the buffered prefix and stream on. The risky
  code stays off the hot path, which is an argument for fixture coverage, not against the design.
- **Hard ceiling** (`ELIDE_CEILING`, 64 MB): a pathological line large without any single large
  string (a million short keys) is skipped, consumed to its newline, and **counted** — a safety
  valve, unreachable in practice once elision is in place, never a routine data-loss policy.
- **Scanner uncertainty** (an escape-state it cannot resolve) falls back to **verbatim copy**
  under that ceiling — never a corrupt line.

---

## 4. Placement: one scanner in the engine, policy through the seam

The natural home is `engine/reader.rs` beside `LineReader`, but the scanner is applied at each
read site, not only inside `LineReader` — because the read paths do **not** all funnel through
`LineReader` today (§6). The invariant that fixes where elision may run:

> **Elision runs strictly downstream of raw-offset accounting.** Offsets (`Deferred.at`, the
> resume cursor) are computed from raw bytes read off disk *before* any string is shortened
> (`reader.rs:100-107` splits and stamps offsets from the raw buffer; `builder.rs:481-486`
> advances `offset += n` on the raw `read_line` count). Elide the `line` string handed onward,
> never the byte counting. Get this wrong and every locator and every resumed fold's CRC breaks.

### 4.1 The #87 split — scanner is engine machinery, *policy* is an adapter hook

The scanner (escape-state scanning, the prefix-preserving placeholder, the size threshold, the
ceiling, the counters) is agent-neutral and lives in the engine. The one thing it must not
contain is *which nodes to elide* when that decision is not purely size-based — that is agent
knowledge and must arrive through `engine/seam.rs`, precisely as `LinePreprocessor` /`Shaping`
do. Two designs follow from where the policy sits; **the doc's recommendation is α**, with β
named as the cheaper fallback.

**Design α — adapter-supplied elision policy (recommended).** The generic size rule is used only
where it is provably safe (metrics, §5.1). For block-building, the adapter supplies the policy
through a new seam hook — the smallest shape that works is a per-value predicate the scanner
consults when a string crosses the threshold:

```rust
// engine/seam.rs — a new per-agent hook, doc-hidden like Shaping.
// Given the JSON *pointer path* to an oversized string, may this value be elided
// (i.e. is it an attachment body the block model will defer, not rendered content)?
pub struct ElisionPolicy { pub elide_here: fn(path: &JsonPath, len: usize) -> bool }
```

The Claude policy answers true for `toolUseResult.file.base64` and its `tool_result` twin /
`source.data`; the Codex policy for the `input_image` url and `image_generation_call.result`.
Anything else — a giant assistant *text* — is left intact. Result: **viewer-lossless and
rendered-output-neutral** (§5.2), so **no byte-gate re-baseline**. (Whether α carries a
FOLD_VERSION bump at all is a separate question, settled by the counter home in §9.3 — the
rendered output does not change either way.) Cost: a real seam hook plus a path-tracking scanner.

**Design β — pure generic rule + prefix placeholder, accept one output change (fallback).** No
seam hook: the engine elides *any* string > 64 KB with the prefix-preserving placeholder. Agent
classification still works (the prefix carries the `data:`/magic bytes), so attachments stay
correct with zero agent knowledge. The one cost: a giant *non-attachment* string (the lone 0.8 MB
assistant text) renders as `iVBOR…<elided:800000>` — a **rendered-output change ⇒ FOLD_VERSION
bump + one byte-gate re-baseline** (cheap; we do these routinely). Simpler, fewer moving parts,
strictly weaker on the viewer.

**Recommendation:** α. The engine *renders* content; a 0.8 MB assistant text is real, readable
content a reader may want, unlike an inert base64 blob that is deferred and never shown. Eliding
it is a genuine viewer regression β accepts and α avoids. α also keeps the byte gate honest (no
re-baseline that could mask an unrelated regression). β is the right answer only if the seam hook
proves not worth its weight — record it, don't build it first.

### 4.2 Why not a bare per-repo copy

agent-metrics' seed is explicitly scoped to its own `timeline.rs`, "no engine change." If the
engine grows its own copy independently, the two scanners — the exact code whose one hazard is
mis-tracking JSON string escapes — drift, and a corruption fixed in one silently survives in the
other. Exposing **one audited scanner at the seam** (§8) is the whole point of doing this in the
engine: agent-metrics then deletes its planned local copy and calls the shared one.

---

## 5. Per-consumer elision rules

### 5.1 Metrics folds — the aggressive generic rule (metric-neutral)

`parse_reader` (A2), `MetricsFold::next_event` (C1), the monitor's cost ledger, and agent-metrics
through the seam fold only token metrics and classify message kinds. Every field they read is a
small scalar (§2), so the **generic >64 KB rule with no policy hook** is provably metric-neutral.
No placeholder-shape care is even needed here (metrics never classify attachments), but sharing
the one prefix-preserving scanner is harmless. **Oracle:** a fixture asserting eliding produces
**byte-identical token/cost/kind metrics** to not eliding — comparing the folded metric *values*,
with the elision gauges (§7) held out of the comparison, since by construction they differ between
an elided and an un-elided fold (this is the §9.3 counter-home question surfacing in the oracle).

### 5.2 Block building — the conservative rule (viewer-lossless, output-neutral)

`advance_at` (the sole per-line unit for **both** the batch parse A1 and the live follower C2 —
see §6) elides under Design α's policy: only attachment-body nodes, which the block model already
turns into `Deferred` locators that are **never rendered inline** (the viewer shows the
attachment name and loads bytes on demand). Eliding them therefore changes **no rendered byte**,
so α needs **no byte-gate re-baseline** — distinct from "no FOLD_VERSION bump", which turns on the
counter-home decision (§9.3), not on rendered output. The same elided `line` also feeds the
metrics `Value` parse inside `advance_at` (`builder.rs:402`); conservative elision is a subset of
the metric-neutral rule, so that fold is unaffected too — one elision serves both readers of the
line.

### 5.3 The Codex asymmetry — the load-bearing constraint

`load_attachment` re-reads the **raw** line from disk (`transcript.rs:136-140`) and selects the
`index`-th *content-bearing* node (`nth_loaded_attachment`), where `index` is a pure **ordinal in
document order** (`claude/model.rs:1581`, `codex/model.rs:617`) — never an intra-line byte
position. So a placeholder that keeps the JSON shape and the count of content-bearing nodes keeps
`index` valid. The catch: **decode walks the elided line; the loader walks the raw line, and they
must agree on which nodes are content-bearing.**

- **Claude is safe under whole-string elision.** Its shape checks read only structural
  discriminants — `type=="image"`, `source.type=="base64"`, "is a JSON string"
  (`claude/model.rs:1548`, `:1511`) — never the payload value. A bare `<elided:N>` would pass.
- **Codex is NOT.** `data_image` does `strip_prefix("data:")` / `split_once(',')` and
  `image_generation_call` sniffs base64 magic bytes (`codex/model.rs:583`, `:600`). A bare
  `<elided:N>` makes the **parse-side** node classify as `None` (not content-bearing) while the
  **raw-line loader** still counts it → the ordinal desyncs or the attachment silently vanishes.

The prefix-preserving placeholder (§3) is exactly what closes this: `data:image/png;base64,` and
the magic bytes survive in the kept prefix, so decode and load classify identically, **for both
agents, with no agent knowledge in the scanner.** `LinePreprocessor::process` is unaffected — it
classifies on structural fields only (`codex/model.rs:44`), never the payload.

---

## 6. The read-site inventory, and what actually needs elision

The inventory reframes the scope: elision matters **only** where an *unbounded* whole-file or
tail-delta path materializes a per-line `Value` DOM or runs `decode_line`. The bounded tail
scanners are already memory-safe by construction and are **out of scope**.

### In scope (unbounded + per-line Value/decode)

| # | Site | Reads | Rule |
| --- | --- | --- | --- |
| A1 | `builder.rs:476` `advance_reader` → `advance_at` | whole file | via `advance_at` |
| C2 | `follow.rs` `FollowParser` → `LineReader::poll` → `advance_at` | tail delta / whole on reset | via `advance_at` |
| A2 | `adapter.rs:164` `parse_reader` | whole file | aggressive |
| C1 | `metrics_fold.rs:161` `next_event` | tail delta / whole cold | aggressive |
| A3 | `discover.rs:234` `latest_cwd` | whole file, extracts cwd only | aggressive |

**`advance_at` is the single chokepoint for all *durable* block-building** — both A1 and C2 funnel
`(offset, line)` through it, with the offset already computed raw upstream. Eliding at the top of
`advance_at` covers batch and live at one point. (Precise: the *durable* block paths funnel here;
`tail_pulse` C4 also calls `decode_line`, but on a bounded 64 KB window — out of scope, below.)

### Out of scope — already bounded, with two residuals named honestly

- **Bounded-window field scanners** read a fixed byte window and mostly skip JSON parsing:
  `inflight_tools_in_tail` (256 KB, `liveness.rs:110`), `last_event_ts` (32 KB), `tail_pulse`
  (64 KB, `state.rs:327`), `first_event_within`, `session_card` (bounded `TAIL_BYTES`). A single
  8 MB line inside these is truncated by the window itself; no unbounded materialization exists to
  fix.
- **Residual 1 — head sniffs are line-count-bounded, not byte-bounded.** `first_cwd`/`session_id`
  (`take(50)`), `detect_agent` (`take(5)`), the Codex/Claude snippet sniffs (`take(80..300)`)
  read few lines, but a giant *first* line would still materialize in full — and `first_cwd` runs
  on **every** `poll_shared` (`follow.rs:311`), so a huge head line re-materializes per poll. Rare
  (base64 blobs are mid-session, not line 1); worth eliding cheaply if the shared scanner is
  already at hand, but not a step-1 obligation.
- **Residual 2 — `load_attachment` (D1) must keep reading the raw line.** It is the one path that
  *wants* the bytes; it re-reads from disk at `Deferred.at` and is O(one line, one attachment).
  It is never elided, and §5.3 is what keeps its ordinal valid against the elided decode.

---

## 7. Invariants carried over unchanged

- **Offsets advance by the full raw byte count, always** (§4). The cursor indexes the file on
  disk, not the elided text.
- **The resume window CRC never sees an elided byte — proven.** `window_at` opens its *own* file
  handle and CRC32s raw disk bytes of the 64 KiB window (`meta_stream.rs:686-701`); both the
  cache (`cache/admit.rs:256`) and the metrics cursor (`metrics_fold.rs:129`) validate by
  recomputing from disk. Elision touches only the in-memory line fed to the parser, so it is
  *structurally impossible* for it to reach the CRC. Resume cannot break.
- **Residency stays bounded — strictly improved.** A resident `Session` holds locators, not bytes
  (`resident_window.rs`, `MAX_RESIDENT = 200`); the 8 MB base64 is never resident today, only
  transient during folding. Elision makes the transient footprint smaller-or-equal, never larger,
  so it cannot threaten the invariant — it is the *transient* per-line buffer this design shrinks.
- **Elision happens once**, immediately after read, so every downstream consumer sees one body.
- **Torn tails stay unconsumed**; the ceiling-skip path (§3) consumes to the newline and counts.
- **Visibility.** Silent loss is the failure to avoid. Surface `elided_lines` / `elided_bytes` /
  `skipped_lines` (the ceiling). The natural slot is `Metrics::extra` (the existing gauge map,
  beside `compact_dropped`, which flows to the footer and the monitor) — **but that is not a free
  choice**: `extra` is persisted in the durable meta stream, so new keys there are a FOLD_VERSION
  bump by this repo's own doctrine (the v7 `credits_micro` precedent), and they also make an
  elided fold's `extra` differ from an un-elided one, which collides with the step-1 metric oracle.
  This is a real decision, deferred to §9.3; whichever home is chosen, `skipped_lines > 0` (the
  ceiling — genuine data loss) should read louder than routine elision.

---

## 8. Migration order, with the equivalence oracle per step

Each step is output-preserving and gated on its named oracle before the next.

0. **Scanner + fixtures + seam.** Build the agent-neutral scanner (prefix placeholder, threshold,
   ceiling, fallback, counters) and the `ElisionPolicy` seam hook. **Oracle:** escape-state
   torture fixtures — escaped quotes, backslashes, unicode, nested arrays, a 10 MB base64 field —
   asserting the elided line parses to a `Value` shape-identical to the un-elided line (only the
   oversized leaves differ).
1. **Metrics-only sites** (A2, C1) → aggressive generic rule. **Oracle:** the existing metrics
   equivalence tests plus a fixture asserting **byte-identical token/cost/kind metrics** (elision
   gauges held out, per §5.1) for an elided vs un-elided fold of a transcript with a real >64 KB
   line.
2. **`latest_cwd`** (A3) → aggressive. **Oracle:** identical cwd result on the same fixture.
3. **Block building** (`advance_at`, covering A1 + C2) → Design α policy. **Oracle, made
   non-vacuous:** a fixture with a **real embedded image** asserting (a) elided ≡ un-elided
   **blocks** (not merely metrics), and (b) `load_attachment` returns **byte-identical bytes**
   after an elided parse — this is what pins the §5.3 ordinal. The `--dump`/`--dump-html` byte gate
   must stay **PASS unchanged** (α is output-neutral). The vacuity trap is already measured against
   the current gate fixtures: **Claude is covered** — `frozen_self` holds 49 lines > 64 KB, 48 of
   them base64 attachment bodies (largest ~570 KB), and `frozen_claude_sa` adds 10 more — so an
   unchanged Claude PASS is real evidence. **Codex is NOT** — `frozen_codex`'s largest line is
   43 KB, zero over threshold, so it never exercises elision at all. Since Codex is exactly where
   the §5.3 asymmetry bites, **step 3 must add a Codex fixture carrying a real
   `data:<mime>;base64,…` image over 64 KB**, or its "PASS unchanged" proves nothing.
   - **Named verification for this step:** grep the decode path to confirm nothing derives a
     *rendered or stored* value from the payload **string itself** (its length, decoded
     dimensions, or a generic output path that stringifies it). Agent inventory verified the
     *classification* walks are structural; this closes the *derivation* question. If anything
     does, that node is excluded from α's policy (or the value is preserved).
4. **Deferred — aggressive rule in the block path** (elide giant non-attachment strings too). A
   block-output change ⇒ FOLD_VERSION bump + byte-gate re-baseline. Ship only if giant
   non-attachment strings ever become a real memory problem; today one 0.8 MB line in 212 k does
   not justify the viewer regression.

Head sniffs (Residual 1) fold in opportunistically once the scanner exists; the bounded scanners
never do.

---

## 9. The seam question, and the open decisions for review

**Expose the scanner through `engine/seam.rs`** so third-party consumers — agent-metrics first —
share the one audited scanner rather than each maintaining a copy that drifts. This is the
motivation the study exists to serve, and the seam is where `LinePreprocessor`/`Shaping` already
establish the precedent for a per-agent hook.

Decisions this document puts to review:

1. **α vs β** (§4.1) — *the headline decision, the one that changes what gets built.* The
   seam-hook, viewer-lossless design (no rendered-output change) versus the simpler generic-rule
   design that also elides giant non-attachment text and takes one byte-gate re-baseline. Recommend
   **α**, because the engine renders content and a 0.8 MB assistant text is real reading a reader
   may want, not an inert blob.
2. **Constants** — `ELIDE_STRING_BYTES = 64 KB`, `SCAN_THRESHOLD = 256 KB`, prefix `K = 64 B`,
   `ELIDE_CEILING = 64 MB`. All inherited from the seed except `K`, which the engine adds for the
   Codex asymmetry.
3. **Counter home** (§7) — the elision gauges have to live somewhere, and the choice sets whether
   this design carries a FOLD_VERSION bump *at all* (independent of α's rendered-output neutrality):
   - **(a) `Metrics::extra`, and accept the bump.** Gauges flow to footer + monitor like
     `compact_dropped`, at the cost of one FOLD_VERSION bump (the `credits_micro` precedent) and an
     explicit "exclude the gauges" clause in the step-1 metric oracle. Fullest visibility.
   - **(b) Per-fold report output** — returned by the fold, not persisted in the durable stream.
     No bump, no oracle collision; the trade-off is the monitor never sees them (only the live
     footer / a `--json` report does).
   - **(c) Compute-but-don't-persist** — carry the gauges in the live accumulator for the footer,
     drop them from the checkpoint. No bump; the monitor's *resumed* view loses them.
   Recommend **(b)** unless the monitor is judged to need a durable elision signal, in which case
   **(a)**. Whichever is chosen, the step-1 oracle states exactly what it compares.
4. **Whether to bound the *read buffer* too, later.** Line-level elision kills the dominant cost
   (the `Value` DOM multiplier and the persistent line `String`), but the raw `read_line` /
   `read_to_end` still slurps one whole raw line before elision. Fully bounding that needs a
   chunked streaming read with inline elision (the seed's "bounded chunked read"). Named here as a
   follow-up, not part of this design.

Gating for the eventual build is the usual: `cargo fmt`/`clippy`/`test`, the `--dump`/`--dump-html`
byte gate on frozen Claude + Codex, and the `follow_matches_full_reparse` equivalence test — plus
the per-step oracles above. All **design-only** until reviewed.
