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

---

## 10. The parsing workflow, in code (review addendum, 2026-08-20)

Requested during review: *what does the main parsing workflow actually look like with this
change?* Two answers, because they are different sizes. §10.1 is the design exactly as §4–§6
specify it — the minimal diff. §10.2–§10.4 is an **amendment the code inventory argues for**,
put here for the same review rather than assumed.

Everything below is illustration. **Nothing in `claude-replay-engine` is modified** — #193 is
still study-only.

### 10.1 The design as reviewed: three lines at one chokepoint

§6 established `advance_at` as the single per-line unit for *all* durable block building — the
batch parse (A1) and the live follower (C2) both funnel through it, with the offset already
computed from raw bytes upstream. So the whole of the block path's change is at its top:

```rust
// engine/builder.rs — SessionAccumulator::advance_at, today's signature, unchanged.
pub fn advance_at(&mut self, offset: ByteOffset, line: &str) -> Option<usize> {
    // #193: shorten oversized attachment bodies before ANYTHING parses the line.
    // `offset` was counted from raw bytes upstream (advance_reader / LineReader::poll)
    // and is not touched here — that ordering is the §4 invariant.
    let line = elide(line, self.adapter.elision());        // Cow<'_, str>

    // ── unchanged from here down ──────────────────────────────────────────────
    let pre = self.preprocessor.state();
    let metrics_state = self.metrics.state();
    let mut delta: Vec<Message> = match self.preprocessor.process(&line) {
        PreprocessedLine::Include => {
            let mut messages = Vec::new();
            self.adapter.decode_line(&line, &mut self.cwd, &mut messages);
            messages
        }
        PreprocessedLine::Ignore => return None,
        PreprocessedLine::Messages(messages) => messages,
    };
    // … locator stamping, boundary capture, fold, drain, checkpoint — untouched …
}
```

`elide` returns a `Cow`, which is what makes the fast path free:

```rust
// engine/reader.rs — agent-neutral machinery. `policy` is the §4.1 α seam hook.
pub fn elide<'a>(line: &'a str, policy: Elision) -> Cow<'a, str> {
    if line.len() < SCAN_THRESHOLD {          // ~99.9 % of lines — no scan, no copy
        return Cow::Borrowed(line);
    }
    Cow::Owned(scan(line, policy))            // escape-state walk, prefix-preserving placeholder
}
```

> **This does not bound the buffer — see §10.6.** `elide` takes a `&str`, which means the whole
> raw line is already in memory when it runs. What it removes is everything *downstream* of the
> buffer (the `Value` DOM, `decode_line`'s walk, any retention); the 8 MB `read_line` allocation
> itself survives. That is faithful to §9's decision 4, which scopes the chunked read out — but
> it does not deliver §1's headline, and the gap is bigger than §9 states.

Two properties worth naming, because they are what makes the diff this small:

- **The signature does not change.** `advance_at` still takes `&str` and still returns
  `Option<usize>`, so C2 (`FollowParser` → `LineReader::poll` → `advance_at`) inherits elision
  with **no change at the follower at all**.
- **The elided value never escapes the line's own fold.** Blocks hold
  `AttachmentContent::Deferred { at, index }` — an offset and an ordinal, no bytes — and
  `load_attachment` re-reads the raw line from disk (§5.3). So the shortened string dies at the
  end of `advance_at`.

### 10.2 What the read-site inventory actually shows

§6 lists the read sites. Reading them side by side turns up something the inventory records but
does not draw out: **four hand-rolled `read_line` loops, and three different torn-tail rules.**

```rust
// A1  builder.rs::advance_reader      — offset tracking; a torn final line is FED to advance_at
let n = reader.read_line(&mut buf)?;  if n == 0 { break }
let start = offset;  offset += n as ByteOffset;
let line = buf.strip_suffix('\n').map(|s| s.strip_suffix('\r').unwrap_or(s)).unwrap_or(&buf);
self.advance_at(start, line);

// A2  adapter.rs::parse_reader        — no offsets; a torn final line is EXCUSED
let complete = line.ends_with('\n');   let body = line.trim_end();
if body.is_empty() { continue }
match serde_json::from_str::<Value>(body) {
    Ok(v) => acc.push(&v),
    Err(_) if complete => acc.malformed_line(),   // "a write IN PROGRESS is not schema drift"
    Err(_) => {}
}

// C1  metrics_fold.rs::next_event     — offsets; a torn final line REWINDS the reader
if !line.ends_with('\n') { self.reader.seek(SeekFrom::Start(at))?; return Ok(None) }
self.offset += n as u64;  let body = line.trim_end();
if body.is_empty() { continue }

// A3  discover.rs::latest_cwd         — .lines(), no offsets, no torn-tail notion at all
```

Blank-skipping appears three times, offset arithmetic twice, and the torn-tail question is
answered three different ways. Two of those are deliberate (C1 must not advance its durable
cursor past an incomplete line; A2 must not flash a spurious diagnostic on a live file).

> **A finding, reported not fixed.** A1 and A2 disagree. `parse_reader` documents why a torn
> final line must not count as malformed — *"a write IN PROGRESS … not schema drift"* — while
> `advance_reader` hands that same line to `advance_at`, whose `Err(_) if !line.trim().is_empty()`
> arm counts it. Both run over the same live transcripts. This predates #193 and is out of its
> scope; it is named here because §10.3 is the point where the two rules would have to be stated
> in one place, and a silent unification would pick a winner without saying so.

### 10.3 The amendment: one `LineSource`, and §4's invariant becomes structural

§4 places elision "at each read site, not only inside `LineReader`, because the read paths do not
all funnel through `LineReader` today." That is accurate. It also means **the same three lines get
pasted into four loops** — and the one rule that must never be broken (§4: *elision runs strictly
downstream of raw-offset accounting*) is left as a rule each of the four must remember.

The alternative is to give the four whole-file loops one source. Note the scope: this does **not**
subsume `LineReader`, which owns the tail/resume path and its own partial-line buffering. It
unifies the four `BufRead` loops beside it.

```rust
// engine/reader.rs — beside LineReader, not replacing it.

/// What to do with a final line that has no newline yet.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TornTail {
    /// A live file may be mid-append: stop before the incomplete line and leave the
    /// cursor on the last complete one. (`MetricsFold`'s durable cursor requires this.)
    Stop,
    /// The last line is all there is: yield it.
    Yield,
}

/// One whole-file line source: raw-offset accounting, torn-tail policy, blank skipping and
/// elision — each written once.
pub struct LineSource<R> {
    reader: R,
    offset: ByteOffset,
    tail: TornTail,
    policy: Elision,
    raw: String,
    out: String,
    pub elided: ElisionCounts,      // §9.3 (b): reported per fold, not persisted
}

impl<R: io::BufRead> LineSource<R> {
    pub fn new(reader: R, at: ByteOffset, tail: TornTail, policy: Elision) -> Self { /* … */ }

    /// The next non-blank line as `(start offset, body)`, or `None` at EOF — or at a torn
    /// tail under `TornTail::Stop`. A lending iterator: the `&str` borrows until the next call.
    pub fn next(&mut self) -> io::Result<Option<(ByteOffset, &str)>> {
        let start = loop {
            self.raw.clear();
            let n = self.reader.read_line(&mut self.raw)?;
            if n == 0 {
                return Ok(None);
            }
            if !self.raw.ends_with('\n') && self.tail == TornTail::Stop {
                return Ok(None);              // cursor stays on the last complete line
            }
            let start = self.offset;
            self.offset += n as ByteOffset;   // RAW bytes. Before anything is shortened.
            if !self.raw.trim_end().is_empty() {
                break start;
            }
        };
        // Disjoint field borrows: `body` reads `self.raw`, the scanner writes `self.out`.
        let body = &self.raw[..self.raw.trim_end().len()];
        let hit = scan_into(body, self.policy, &mut self.out, &mut self.elided);
        Ok(Some((start, if hit { &self.out } else { body })))
    }

    /// The offset of the next unread line — the durable cursor.
    pub fn offset(&self) -> ByteOffset {
        self.offset
    }
}
```

**The invariant stops being a rule and becomes a shape.** `self.offset += n` runs on the raw
`read_line` count, inside the source, before `scan_into` can see the line — and the offset is
returned by value while the body is returned by reference. A caller cannot elide first and count
second, because a caller never counts at all.

### 10.4 The four sites, after

```rust
// A1 — builder.rs, the whole-file block fold.
// NOTE: superseded by §11. `Elision::None` here defers the block path's policy to advance_at,
// which is elision AFTER the read and therefore leaves A1 unbounded. Under the boundedness
// invariant the policy must be applied inside the source for every site, block path included.
pub fn advance_reader(&mut self, reader: &mut dyn io::BufRead) -> io::Result<()> {
    let mut src = LineSource::new(reader, 0, TornTail::Yield, Elision::None);
    while let Some((at, line)) = src.next()? {
        self.advance_at(at, line);
    }
    Ok(())
}

// A2 — adapter.rs, the whole-file metrics fold. Aggressive: provably metric-neutral (§5.1).
fn parse_reader(&self, reader: &mut dyn io::BufRead) -> Metrics {
    let mut acc = self.metrics_acc();
    let mut pre = self.line_preprocessor();
    let mut src = LineSource::new(reader, 0, TornTail::Stop, Elision::Aggressive);
    while let Ok(Some((_, line))) = src.next() {
        if matches!(pre.process(line), PreprocessedLine::Ignore) {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => acc.push(&v),
            Err(_) => acc.malformed_line(),   // no `if complete` — Stop means we never see one
        }
    }
    acc.finish()
}

// C1 — metrics_fold.rs. The fold holds the source instead of a bare reader + offset.
pub fn next_event(&mut self) -> io::Result<Option<MetricsEvent>> {
    let Some((_, line)) = self.src.next()? else {
        self.src.rewind_to_cursor()?;         // R: BufRead + Seek; the one site that needs it
        return Ok(None);
    };
    if matches!(self.pre.process(line), PreprocessedLine::Ignore) {
        return self.next_event();
    }
    // … unchanged: totals before, push, totals after, diff into a MetricsEvent …
}

// A3 — discover.rs, cwd only.
pub fn latest_cwd(path: &Path) -> Option<PathBuf> {
    let f = io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut src = LineSource::new(f, 0, TornTail::Yield, Elision::Aggressive);
    let mut last = None;
    while let Ok(Some((_, line))) = src.next() {
        if let Some(cwd) = serde_json::from_str::<Value>(line).ok().as_ref().and_then(cwd_in_record) {
            last = Some(cwd);
        }
    }
    last
}
```

What this buys beyond eliding, and what it costs:

- **`parse_reader` loses a branch.** `let complete = line.ends_with('\n')` and the
  `Err(_) if complete` guard both disappear: under `TornTail::Stop` a torn line is never
  yielded, so "malformed" is unconditional. The rule moves from a per-site guard to a named
  argument, and §10.2's A1/A2 divergence becomes a visible choice — `TornTail::Yield` vs
  `Stop` at one call site — instead of a discrepancy between two loops.
- **`next_event` keeps its rewind** — honestly. `read_line` has already consumed the bytes by
  the time the source knows the line was torn, so somebody must seek back. It moves into
  `LineSource::rewind_to_cursor` under an `R: Seek` bound, which is one site rather than
  everywhere, but it does not vanish.
- **`Elision` becomes one argument at one call site per consumer**, which is §5's per-consumer
  table expressed in the type system rather than in prose.
- **It is the seam where a chunked read could later land** (§9 decision 4). The raw `read_line`
  still slurps one whole line before elision; bounding *that* means replacing one loop body,
  not four.
- **Cost:** a new engine type on the critical path of every parse, and four call sites rewritten
  in a repo whose gate is byte-identical output. The oracles in §8 cover it — the migration
  order does not change — but this is strictly more diff than §10.1.

### 10.5 What this adds to the review

A third decision, alongside §9's α-vs-β and the counter home:

**Placement:** §4's *per-read-site* elision (§10.1 — minimal diff, the rule stays a rule), or
**one `LineSource`** (§10.3 — larger diff, the rule becomes unrepresentable-otherwise, four loops
collapse to one, and the A1/A2 torn-tail divergence surfaces as a decision).

They are not exclusive: §10.1 is a strict subset, so shipping it first and unifying later is a
valid order. The argument for doing it in one pass is that migrating four loops twice is the
expensive half, and step 0's fixtures are written either way.


### 10.6 Correction: the prototype above still buffers the whole line

Raised in review, and correct. §10.1 elides a `&str` that already exists, so the peak allocation
is unchanged. Worth separating what it does and does not buy:

| cost | `elide(&str)` (§10.1) | streaming read (§10.6) |
| --- | --- | --- |
| `serde_json::Value` DOM over 8 MB | **gone** | gone |
| `decode_line` walking 8 MB | **gone** | gone |
| the raw `String` the line was read into | **survives** | gone |
| the reader's high-water capacity | **survives, for the rest of the parse** | gone |

That fourth row is the one nobody counts. Every one of these loops reuses its buffer —
`advance_reader`'s `buf`, `parse_reader`'s `line`, and (my own sketch's) `LineSource::raw` — via
`String::clear`, which keeps capacity. So a single 8 MB line does not cost 8 MB once; it raises
the loop's resident buffer to 8 MB **for every remaining line of the parse**. §10.3 as written
institutionalizes that.

**And the follower is worse than "one line".** §1 says the read paths "buffer each raw line in
full". `LineReader::poll` does not:

```rust
f.seek(SeekFrom::Start(self.offset))?;
let mut buf = Vec::new();
f.read_to_end(&mut buf)?;          // everything from the cursor to EOF
self.consume(&buf, &mut out);      // → out.lines: Vec<String>, one owned String per line
```

On the cold first poll of a followed session (`FollowParser::open` → `LineReader::open_at_start`)
that is **the whole file materialized twice** — once as `Vec<u8>`, once as a `Vec<String>` of
every line — before `advance_at` sees byte one. Elision at `advance_at` cannot touch either,
because both already exist by the time it runs. (A *resumed* session escapes this: `open_at_offset`
reads only the suffix above `replay_from`, which is durability paying for itself again. A cold
session does not.)

#### What a bounded read actually looks like: the seed's `read_line_elided`, adopted

The scanner §3 specifies is **already a streaming state machine** — its own fast-path description
says "fill a buffer to the threshold; the threshold crossed without a newline ⇒ run the scanner
over the buffered prefix and stream on." §10.1's `elide(&str)` is the weaker form that throws that
capability away.

There is no primitive to invent here. The seed **implemented** its proposal —
`agent-metrics/src/elide.rs` — and its contract is the one this design adopts, under its name:

```rust
// agent-metrics/src/elide.rs — real, tested code, not a sketch.
pub fn read_line_elided<R: BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,               // receives the (possibly elided) line bytes
) -> std::io::Result<LineOutcome>;

pub enum LineOutcome {
    Eof,
    /// A final line with no newline: a write in progress. The caller must leave
    /// its offset unadvanced so the next run re-reads the line whole.
    Torn,
    Complete { raw_len: u64, elided: u64, skipped: bool },
}
```

Driven over `fill_buf`/`consume`, no whole line is ever resident, and the contract already
answers three questions the engine's loops answer inconsistently (§10.2): `raw_len` is always the
true byte count for offset accounting; `Torn` is a first-class outcome rather than a per-site
`ends_with('\n')` convention; `skipped` is the `HARD_CEILING` overflow, counted rather than
silent.

The engine adopts it with **three deltas**, each already argued elsewhere in this document:

1. **The prefix-preserving placeholder** (§3): the seed emits a bare `"<elided:N>"`; the engine
   keeps the first K = 64 bytes so Codex's payload-shape recognizers survive (§5.3). A change
   inside the scanner's emit path, not to the signature.
2. **A policy parameter** (§4.1) — only if α is chosen: `read_line_elided(reader, out, policy)`.
   Under β the seed's signature stands unchanged. This is the α cost line in §11.3.
3. **A torn-tail mode** (§10.3): the seed's `Torn` semantics match `TornTail::Stop`; the engine
   also needs `Yield` (A1 feeds a torn final line to the fold today). One flag, or the caller
   simply uses the buffer on `Torn` — a policy choice above the primitive, not inside it.

Three things fall out, and they are the reason this is not merely a nicer §10.1:

- **The offset invariant gets *stronger*, not weaker.** §4 has to say "elide the line handed
  onward, never the byte counting" because the two are separable. Here they are not: `raw_len`
  counts bytes *as they are consumed*, so an elided byte is counted by the same pass that drops it.
- **The peak becomes the elided size**, not the raw size, with the ceiling as the backstop for
  the pathological line that is large without any one large string. `skipped` makes the ceiling
  the actual bound rather than a curiosity.
- **`BufReader`'s own buffer is the only fixed cost** — 8 KB, never grown by `fill_buf`.

#### What this does to the §10.5 decision

It changes its weight. §10.5 framed placement as minimal-diff versus tidiness; it is not.

- A streaming read **cannot** be expressed as "elide at each read site" (§4), because there is no
  site to put it — the elision has to happen *inside* the read, and there are four reads.
- So §4's placement does not merely leave the buffer unbounded today; it leaves it unbounded
  **permanently**, unless the chunked read is later written four times.
- One `LineSource` is where `read_line_elided` lands **once**. That makes §10.3 the prerequisite
  for §9's decision 4 rather than an independent tidy-up — and if decision 4 is genuinely wanted
  later, deferring §10.3 now means migrating four loops twice.

`LineReader` is the fifth read and the one this still does not reach: it is `File` + `read_to_end`
+ `Vec<String>`, not a `BufRead` loop, and bounding it means giving the tail/resume path a chunked
read with its own pending-partial handling. That is a separate step, and it is the one that bounds
the **follower's cold poll** — measurably the largest allocation in the whole inventory. It should
be named in the migration order (§8) rather than left inside decision 4's one-line deferral.

**Revised recommendation.** Ship §10.1 first if the goal is the DOM multiplier — it is real,
cheap, and its oracles are written. But say plainly in §1 and §9 that it does **not** bound the
buffer, adopt §10.3 as the placement so the bounded read has one home, and promote the chunked
read (both `LineSource` and `LineReader`) from a deferred aside to a named step with its own
oracle: peak RSS over a fixture containing one 8 MB line, asserted against the un-elided baseline.

---

## 11. Full audit: every transcript read, bounded or not (2026-08-20)

Requested at review: vet the whole design against one invariant — **no read allocates without a
cap.** Not a memory-savings target; a robustness property. An unbounded read is one whose
allocation is a function of transcript content with no ceiling, which at deployment scale means a
pathological or malformed transcript can take the process down.

The audit criterion is therefore binary per site, and it is stricter than §6's. §6 asked "does
this site need elision?". This asks **"can any input make this allocation grow without limit?"**
Three things pass the first test and fail the second, which is why §6 missed them.

### 11.1 The audit

**Bounded today — no work needed.** A fixed byte window, taken before the read:

| site | bound |
| --- | --- |
| `state.rs` `tail_pulse` | seeks to `len − PULSE_TAIL_BYTES` |
| `core/liveness.rs` `inflight_tools_in_tail` | seeks to `len − INFLIGHT_TAIL_BYTES` |
| `claude/discover.rs:381` `read_from` | `f.take(to − from).read_to_end` |

**Bounded on one path, unbounded on the other.** One site, and §6 records only its good half:

| site | cold | resumed |
| --- | --- | --- |
| `claude/discover.rs:294` `session_card` | `from = len − TAIL_BYTES` — **bounded** | `from = memo.at`, `to = len` — **O(everything appended since the last scan)**, and `read_from` pre-reserves it with `Vec::with_capacity(to − from)` |

§6 calls this one "bounded `TAIL_BYTES`", which is true only of the cold read. The memoized path
is the one the monitor takes on every rescan, and a session not visited for a long time — or one
that appended a single huge line — is read whole. This row was verified against the code rather
than inherited from §6, which §11.2 indicts.

**Unbounded in one line's length.** `read_line` and `.lines()` both grow until a newline
arrives; neither has a cap:

| # | site | read | note |
| --- | --- | --- | --- |
| 1 | `engine/adapter.rs:170` `parse_reader` | `read_line` | A2 |
| 2 | `engine/metrics_fold.rs:166` `next_event` | `read_line` | C1 |
| 3 | `engine/builder.rs:502` `advance_reader` | `read_line` | A1 |
| 4 | `engine/discover.rs:223` `first_cwd` | `.lines().take(50)` | runs on **every** `poll_shared` |
| 5 | `engine/discover.rs:237` `latest_cwd` | `.lines()`, whole file | A3 |
| 6 | `engine/discover.rs:347` `session_id` | `.lines().take(50)` | |
| 7 | `claude/discover.rs:148` | `.lines().take(80)` | snippet sniff |
| 8 | `codex/discover.rs:79,130,269,577` | `.lines().take(100\|300)` | |
| 9 | `codex/discover.rs:384` | `read_line` | first line |
| 10 | `core/discover.rs:75` `detect_agent` | `.lines().take(5)` | runs on every candidate |
| 11 | `core/transcript.rs:140` `load_attachment` | `read_line` | D1 — §6 called this "O(one line)" |
| 12 | `present/cache/stream.rs:30` `anchor_of` | `read_line` | first line, for the CRC identity check |

> **`take(N)` bounds the line COUNT, not the line SIZE.** This is the trap, and it is why six of
> these read as safe. On a transcript with no newline at all — a truncated write, a binary file
> that reached the store, a single 200 MB line — `detect_agent`'s five-line sniff reads the entire
> file, and **discovery runs it against every candidate on the machine.** The cheapest-looking
> site in the inventory is the one that fails hardest on malformed input.

**Unbounded in the whole FILE.** One site, and the design never named it:

```rust
// engine/reader.rs:127 — LineReader::poll
f.seek(SeekFrom::Start(self.offset))?;
let mut buf = Vec::new();
f.read_to_end(&mut buf)?;          // cursor → EOF, no cap
self.consume(&buf, &mut out);      // → out.lines: Vec<String>, one owned String per line
```

On the cold first poll of a followed session (`FollowParser::open` → `open_at_start`) this
materializes the transcript **twice** — once as `Vec<u8>`, once as a `Vec<String>` — before
`advance_at` sees byte one. Measured on this machine: transcripts of 214 MB, 157 MB, 150 MB, 128 MB
and 102 MB, so a cold open of the largest is ≈430 MB of allocation before folding begins. A
*resumed* session escapes it (`open_at_offset` reads only above `replay_from`); a cold one does
not, and the transient provider is always cold.

This one is not fixed by a bounded line read alone: even with every line capped, `Vec<String>`
over a whole file is O(file). It needs a chunked read **and** a bounded batch per poll.

**Scope, stated so omission is not mistaken for coverage.** The audit above covers *transcript*
reads. Agents also write **sidecar** files into the same stores, and those are read whole:

| site | read | verdict |
| --- | --- | --- |
| `qoderwork/discover.rs:259` `sidecar` | `read_to_string(<stem>-session.json)` | **unbounded** — agent-written, same trust domain as a transcript |
| `claude/discover.rs:411` `load_tasks_in` | `read_to_string` per task file, in a loop | **unbounded**, same |
| `claude-monitor/src/index.rs:1602` `last_event_ts` | seeks to `len − 32 KB` | bounded — verified |
| `claude-monitor/src/cost.rs:148` `load_entry` | `read_to_string` of the ledger | unbounded, but the monitor's **own** file — a different trust domain |

The two sidecar reads are outside #193's mechanism (they are single JSON documents, not JSONL, so
there is no line to elide) but they are inside the *invariant* Hong stated. They want a size cap
before the read, which is a two-line fix and a separate issue — named here rather than left silent.

### 11.2 The verdict on the design as written

**Mechanism: sound.** §3's prefix-preserving placeholder, §5.3's ordinal analysis, and §7's proof
that the resume CRC cannot see an elided byte are all correct, and the Codex asymmetry work is a
genuine improvement on the seed rather than a port of it.

**Scope: not sound, against this invariant.** Three specific failures:

1. **§4's placement cannot deliver boundedness at all** (§10.6). Eliding a `&str` that already
   exists shortens what goes downstream; the allocation being audited has already happened.
2. **§6's inventory is a relevance inventory, not a boundedness one.** It correctly excluded the
   fixed-window scanners, and then missed that `take(N)` is count-bounded, missed
   `LineReader::poll` entirely, missed `anchor_of`, and dismissed `load_attachment` as "O(one
   line, one attachment)" when *one line* is precisely the unbounded quantity.
3. **§9's decision 4 defers the chunked read**, which is not a refinement of the design — it *is*
   the property.

### 11.3 Why the cost is far lower than "rewrite every read path"

The primitive already exists, implemented and tested, in the repo this design was ported from.
`agent-metrics/src/elide.rs` (475 lines) — **the seed did not stop at proposing it**:

```rust
pub fn read_line_elided<R: BufRead>(reader: &mut R, out: &mut Vec<u8>)
    -> std::io::Result<LineOutcome>
```

with `LineOutcome::{Eof, Torn, Complete { raw_len, elided, skipped }}` — `raw_len` always the true
byte length for offset accounting, `Torn` meaning "leave the offset unadvanced", `skipped` meaning
the line blew past `HARD_CEILING`. Its caller says so in a comment: *"Never `read_line`, which has
no cap — see `elide`."* This is the streaming reader §10.6 sketched, already written.

That reframes the work, and it is the whole reason #193 exists — *"can ONE eliding reader live in
the engine, shared by every line consumer, instead of each downstream repo growing its own scanner
that drifts?"* The answer is that the scanner is already written in the sibling; the engine's job
is to adopt it, add the K = 64 prefix the Codex asymmetry needs (§3 — the engine's own
contribution), and expose it at the seam so agent-metrics deletes its copy.

| work | size | new logic? |
| --- | --- | --- |
| port `elide.rs` → `engine/reader.rs`, add the K-byte prefix | ~475 lines, existing + a small change | no |
| **under β only** — the seed scanner is size-driven and ships as-is | — | no |
| **under α** — make the scanner *path-aware* while streaming, and plumb the adapter policy into both `LineSource` and `LineReader` | the second genuinely new piece | **yes** — the seed is explicit that its rule is "a property of JSON, **not a list of known paths**" |
| sites 1–10, 12: swap `read_line`/`.lines()` for the primitive | 1–3 lines each | no |
| `LineReader::poll`: chunked read + bounded batch per poll | one function + its one caller | no |
| `session_card`: cap the resumed read, or re-window when the gap is large | one function | no |
| `load_attachment` (11): stream to the *n*th content-bearing string | the first genuinely new piece | yes — same state machine, **capture** sink instead of **drop** |

Site 11 is worth stating plainly since it is the only new design: `load_attachment` is the one path
that *wants* the bytes, so elision is the wrong operation. It needs the same scanner walking to the
*n*th content-bearing node and streaming that value into the base64 decoder — one extra mode on a
state machine that has to exist anyway, not a second scanner.

### 11.4 What "bounded" will actually mean

Stated precisely, so the guarantee is checkable rather than a feeling:

- **No allocation is a function of file size.** Reads are chunked through `BufRead::fill_buf`;
  `LineReader::poll` returns a bounded batch.
- **Per-line allocation is bounded by the elision policy, backstopped by `HARD_CEILING`** (64 MB).
  A line that exceeds it is consumed to its newline, `out` holds nothing, and it is **counted**.

That second clause is the one that answers the deployment concern directly: a malformed transcript
stops being an unbounded allocation and becomes a counted skip. It is not "always small" — a 2.7 MB
line of many small strings stays 2.7 MB, because nothing in it is elidable — it is *never
unlimited*, which is the property being bought.

### 11.5 Recommendation

**Go, with the scope corrected.** The go/no-go turns on whether the design is sound, and the
mechanism is; what fails is a scope that cannot deliver the property. Concretely:

- Adopt §10.3's `LineSource` as the placement — not for tidiness, but because a streaming read has
  no expression under §4's per-site placement, so §4 forecloses the property permanently.
- Promote the chunked read from §9's decision 4 into the migration order as step 0, since it *is*
  the deliverable.
- Add `LineReader::poll`, `anchor_of`, `detect_agent`/`first_cwd`/`session_id` and the adapter
  sniffs to §6's inventory, with the `take(N)`-is-not-a-size-bound note.
- Keep §5.3, §7 and §8 exactly as they are — that analysis is the part of this design worth having,
  and none of it changes.

**And one decision comes back to you, on new terms: α vs β.** §9 framed it as viewer-losslessness
versus a byte-gate re-baseline, and recommended α. Boundedness changes the cost side, because the
policy now has to live *inside* the read at every site rather than at `advance_at`:

- **β** — size-driven, no path awareness. The seed's scanner is already exactly this and ships
  as-is. Cost: one rendered-output change (a giant non-attachment *text* renders as
  `iVBOR…<elided:N>`), so a FOLD_VERSION bump and one byte-gate re-baseline.
- **α** — the adapter says which nodes may be elided. That needs a **path-tracking** streaming
  scanner, which the seed deliberately is not, plus the policy plumbed through `LineSource` *and*
  `LineReader`'s chunked read. Cost: real new logic on the one component whose stated hazard is
  mis-tracking JSON escapes.

α remains better for the viewer. It is no longer nearly free, and it is now the larger half of the
whole change. Worth deciding explicitly rather than inheriting §9's recommendation, which was
priced before boundedness was the goal. (β is also recoverable: the elided text is still shown
with its first 64 bytes and its true length, and the raw line is on disk.)

**The one thing not to buy:** eliding for its own sake at sites where nothing is ever large. Every
site above is switched for boundedness; whether it *also* elides is the per-consumer question §5
already answers, and the answer for the sniffs is "it does not matter, they read five lines."
