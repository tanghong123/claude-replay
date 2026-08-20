# Design: the bounded eliding line reader

> **Status:** study only (#193) — a design to review. **Build nothing until it is reviewed.**
> The exit condition for #193 is *this document read and accepted*, not a merge.
>
> **v2, 2026-08-20.** v1 proposed elision at each read site; review found that placement cannot
> deliver the property the design is for, and a full boundedness audit (§8) re-scoped it around
> a streaming primitive the seed repo had already implemented. v1 and the review addenda are
> preserved as written in git history (`fd3765d` and before); §12 is the compressed trail of
> what changed and why. The open decisions are in §11.
>
> **v2.1, 2026-08-20 (owner review).** The placeholder now carries the elided value's **span**
> — `<elided:{off},{len}>`, offset from the line start — captured by the one scan that already
> passes over every byte, so `load_attachment` seeks straight to the value instead of
> re-scanning the line (§9.2). The ordinal walk remains the base mechanism for sub-threshold
> values, which have no span. Ripples into §11's decisions ① and ③; trail in §12.5.

Inspired by, and generalizing, agent-metrics' adopted proposal
`~/code/agent-metrics/docs/proposals/bounded-line-reads.md` — which did not stop at proposing:
its scanner is implemented and tested as `agent-metrics/src/elide.rs` (475 lines), and that code
is what this design adopts. Its measurements over ~400 real Claude-format transcripts (212,236
lines) anchor the design and are taken as given: largest single line **8.08 MB**, 83 lines
> 1 MB, and **~95 % of the bulk is base64 attachment bodies** — none of it metric-bearing. (A
sibling repo of this author's, credited by reference here rather than in `ATTRIBUTION.md`, which
records external MIT sources.)

The question this study answers: **can ONE bounded, eliding line reader live in the engine,
shared by every line consumer (including agent-metrics through the seam), instead of each
downstream repo growing its own scanner that drifts?** Yes — and the scanner is already written
in the sibling. The engine's job is to adopt it, add the one thing the engine needs that the
seed does not (a prefix-preserving placeholder for the Codex asymmetry, §7), and expose it at
the seam so agent-metrics deletes its copy.

---

## 1. The interface at a glance

The property being bought (§2 states it precisely): **no read allocates without a cap.**

One component touches bytes; everything else drives it:

| component | role | new? |
| --- | --- | --- |
| `read_line_elided` | **the only byte-toucher** — a streaming state machine over `fill_buf`/`consume`: reads one line, shortens oversized string values in flight, never holds a whole raw line | adopted from `agent-metrics/src/elide.rs`, three deltas (§4) |
| `LineSource` | the **batch driver** — wraps the primitive for the whole-file loops: raw-offset accounting, torn-tail policy, blank skipping, elision counters, each written once | new, small (§5) |
| `LineReader` (exists) | the **tail batcher** — the follower's poll path; keeps its name and its resume contract, swaps its `read_to_end` for chunked reads through the same primitive, returns a bounded batch per poll | changed inside (§9.1) |
| `load_attachment` | the one path that *wants* the bytes — an elided value's locator carries its **span** (recorded by the scan that elided it), so load seeks straight to the value; sub-threshold values use the same state machine with a **capture sink** to the *n*-th content-bearing value | span hint + new mode (§9.2) |

Policy enters once, through the seam (§6): the adapter says *what may be elided*; the machinery
never knows an agent's field names.

What a whole-file consumer writes:

```rust
let mut src = LineSource::new(reader, 0, TornTail::Stop, policy);
while let Some((at, line)) = src.next()? {          // (raw byte offset, elided body)
    …                                               // parse `line`; trust `at` absolutely
}
// src.offset() is the durable cursor; src.elided has the gauges.
```

Two guarantees the shape enforces, so no caller can hold them wrong:

- **Offsets are raw.** `at` and `offset()` count bytes as consumed from disk, inside the source,
  before anything is shortened — the caller never counts, so it cannot count elided text.
- **The line handed out is already bounded.** Peak per-line allocation is the elided size,
  backstopped by a hard ceiling (§4); the raw 8 MB never exists in memory.

---

## 2. Problem

`DESIGN.md` commits the fold to never loading a whole session into memory. The engine honours
that at the *session* level — it streams line by line, a resident `Session` holds only
attachment **locators** (`AttachmentContent::Deferred { at, index }`), and the
`resident_window.rs` test pins the open frontier to ≤ 200 blocks. It honours it **not at the
line level, and not at the follower's cold open**:

- Every whole-file and tail-delta path buffers each raw line in full, then hands it to
  `serde_json::from_str::<Value>` (a DOM several times the text size) and to `decode_line`. An
  8 MB line is materialized several times over — *specifically to build a `Deferred` locator
  that deliberately holds none of those bytes*. The engine buffers 8 MB to record an offset and
  an ordinal.
- Worse than one line: these loops reuse their buffer via `String::clear`, which keeps capacity
  — one 8 MB line raises the loop's resident buffer to 8 MB **for every remaining line of the
  parse**.
- Worst: `LineReader::poll` on a cold open does `read_to_end` (cursor → EOF, no cap) and then
  splits into a `Vec<String>` — the transcript materialized **twice** before the fold sees byte
  one. Measured on this machine: transcripts of 214, 157, 150, 128 and 102 MB, so the largest
  cold open is ≈ 430 MB of allocation. (A *resumed* session reads only the suffix above
  `replay_from` — durability paying for itself again. A cold session has no such mercy, and the
  transient provider is always cold.)

The invariant this design installs, stated so it is checkable (it is a **robustness** property,
not a memory-savings target — an unbounded read means a pathological or truncated transcript
can take the process down at deployment scale):

> **No read's allocation is a function of transcript content without a ceiling.** Reads are
> chunked; per-line allocation is bounded by the elision policy, backstopped by `ELIDE_CEILING`;
> a line that exceeds the ceiling is consumed, skipped, and **counted** — never buffered, never
> silent.

This is not "always small": a 2.7 MB line of many small strings stays 2.7 MB, because nothing in
it is elidable. It is *never unlimited*, which is the property being bought. There is no
line-size or attachment-size cap anywhere in the engine or adapters today; this design
introduces the first one.

---

## 3. What is actually large (the finding that shapes the policy)

From the seed's measurements, across the 141 lines ≥ 500 KB (~163 MB of text):

| json path | share |
| --- | --- |
| `$.toolUseResult.file.base64` (+ its `message.content[].content[]` `tool_result` twin) | ~99 % |
| `$.toolUseResult[].source.data`, one giant `$.message.content[].text` | the rest |

**~95 % of the bulk is base64 attachment bodies, and none of it is metric-bearing.** Everything
the metrics fold reads is a small scalar — `usage.*`, `model`, `id`, `requestId`, `timestamp`,
`type`, `subtype`, `compactMetadata.*` — none can exceed 64 KB. A line is huge *because of* the
one part of it the fold has no use for.

Two facts make this more than a port of the seed:

1. **The engine surfaces `decode_line` output.** Blocks carry assistant text and tool output; a
   > 64 KB string is not automatically inert here the way it is for a pure metrics fold. The
   generic "elide any big string" rule is content-neutral for metrics but *not* for blocks —
   that is the α/β decision (§6).
2. **The engine is agent-free (#87).** The scanner lives in `claude-replay-engine`, behind the
   `agents_import_only_the_seam` audit. It **cannot know** that `toolUseResult.file.base64` is a
   Claude path or that Codex wraps images as `data:<mime>;base64,…`. Any per-agent knowledge
   enters through `engine/seam.rs`, exactly as `LinePreprocessor` and `Shaping` do.

---

## 4. The primitive: `read_line_elided`

Adopted from the seed under its own name — real, tested code, not a sketch:

```rust
// agent-metrics/src/elide.rs → engine/reader.rs
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

Driven over `fill_buf`/`consume`, no whole raw line is ever resident: the scanner is a JSON
escape-state machine that copies bytes through, except that a string value longer than
`ELIDE_STRING_BYTES` (64 KB) is **consumed without buffering** and replaced by a placeholder.
The output stays syntactically valid JSON with the same shape, so every downstream consumer —
`serde_json::from_str::<Value>`, `LinePreprocessor::process`, `decode_line`, the metrics `push`
— works unmodified and sees a small line.

```
{"type":"user","toolUseResult":{"file":{"base64":"data:image/png;base64,iVBORw0KGgo…<elided:10432,8117324>"}}, …}
                                        └── kept K=64-byte prefix ──┘         └ off ┘ └ len ┘
```

`off` is the raw byte offset of the value's first content byte from the **line start**, `len`
its full raw (escaped) byte length — the value's span, measured by the scanner as it consumes,
in the same pass that elides. `line offset (at) + off` is therefore an absolute file position:
the one scan that already touches every byte captures where the bytes live, so nothing ever
has to find them again (§9.2).

The contract answers, once, three questions the engine's four hand-rolled loops answer
inconsistently today (§12.2): `raw_len` is always the true byte count for offset accounting;
`Torn` is a first-class outcome rather than a per-site `ends_with('\n')` convention; `skipped`
is the ceiling overflow, counted rather than silent. And the properties fall out of the shape:

- **The offset invariant is structural, not a rule.** v1 had to say "elide the line handed
  onward, never the byte counting" because the two were separable. Here they are not: `raw_len`
  counts bytes *as they are consumed*, by the same pass that drops them.
- **The peak is the elided size**, not the raw size; `BufReader`'s fixed buffer (8 KB, never
  grown by `fill_buf`) is the only per-read constant.
- **Fast path:** lines under `SCAN_THRESHOLD` (256 KB) are copied verbatim with no scanning
  (~99.9 % of lines). The risky code stays off the hot path — an argument for fixture coverage
  (§10 step 0), not against the design.
- **Hard ceiling** (`ELIDE_CEILING`, 64 MB): a line large without any single large string (a
  million short keys) is consumed to its newline, `out` holds nothing, `skipped: true` — a
  safety valve, unreachable once elision is in place, never a routine data-loss policy.
- **Scanner uncertainty** (an escape state it cannot resolve) falls back to **verbatim copy**
  under the ceiling — never a corrupt line.

The engine adopts the seed with **three deltas**, each argued elsewhere in this document:

1. **The prefix-preserving, span-carrying placeholder.** The seed emits a bare `"<elided:N>"`;
   the engine keeps the first **K = 64 bytes** of the value, then `…<elided:{off},{len}>` —
   the prefix forced by the Codex asymmetry (§7), the span by the owner-review principle that
   *the scan that already passes over every byte records where the value lives* (§12.5), so the
   loader seeks instead of re-scanning. Both are changes inside the scanner's emit path, not to
   the signature. Disambiguation is by visible length: an elided value's visible text is
   ~K + marker (~100 bytes), while any genuine value that merely *ends* with marker-shaped text
   is, by construction, over the 64 KB threshold — decode treats a value as elided only when it
   is smaller than the threshold AND ends with the marker, so a false match cannot occur.
2. **A policy parameter — only under α** (§6): `read_line_elided(reader, out, policy)`. Under β
   the seed's size-driven signature stands unchanged. This is α's cost line in §11.
3. **A torn-tail mode above the primitive** (§5): the seed's `Torn` matches `TornTail::Stop`;
   the engine also needs `Yield` (the batch block fold feeds a torn final line onward today).
   A policy choice in the driver, not inside the primitive.

---

## 5. The batch driver: `LineSource`

The whole-file read paths are four hand-rolled `BufRead` loops (block fold, metrics fold's
whole-file and tail paths, cwd extraction) that each re-implement offset arithmetic, blank
skipping, and a torn-tail rule — three different torn-tail rules, in fact (§12.2). `LineSource`
is the one loop, written once, wrapping the primitive:

```rust
// engine/reader.rs — beside LineReader (the tail batcher, §9.1), not replacing it.

/// What to do with a final line that has no newline yet.
pub enum TornTail {
    /// A live file may be mid-append: stop before the incomplete line, cursor on the
    /// last complete one. (The metrics fold's durable cursor requires this.)
    Stop,
    /// The last line is all there is: yield it, offset advanced past it.
    Yield,
}

pub struct LineSource<R> {
    /* reader, raw offset, TornTail, Elision policy, out buffer */
    pub elided: ElisionCounts,      // the §11.3 gauges: elided_lines/bytes, skipped_lines
}

impl<R: io::BufRead> LineSource<R> {
    pub fn new(reader: R, at: ByteOffset, tail: TornTail, policy: Elision) -> Self;

    /// The next non-blank line as `(start offset, elided body)`, or `None` at EOF — or at
    /// a torn tail under `Stop`. A lending iterator: the `&str` borrows until the next call.
    pub fn next(&mut self) -> io::Result<Option<(ByteOffset, &str)>>;

    /// The offset of the next unread line — the durable cursor.
    pub fn offset(&self) -> ByteOffset;

    /// Reposition the underlying reader to `offset()`. Needed by exactly one consumer:
    /// the live metrics tail, which keeps its reader open across polls (see below).
    pub fn rewind_to_cursor(&mut self) -> io::Result<()> where R: io::Seek;
}
```

**Torn tails, stated once.** The primitive's contract is that a `Torn` line leaves the caller's
offset unadvanced so the next run re-reads it whole. `LineSource` expresses that per driver
mode: under `Stop` the torn line is never yielded and `offset()` stays on the last complete
line; under `Yield` it is delivered and counted. The bytes, however, have been consumed from the
reader either way — so a consumer that *keeps its reader open across polls* (only the live
metrics tail does; every other site opens, reads to EOF, drops) must call `rewind_to_cursor()`
before its next poll. That is the entire `Seek` story: one method, one bound, one consumer —
today's fold does the same seek inline, for the same reason.

**The four sites, after** (policy per §6; the α/β choice changes only the `policy` argument):

```rust
// A1 — builder.rs, the whole-file block fold. Policy inside the read (α: adapter policy;
// β: Aggressive) — NOT deferred to advance_at, which would elide after the buffer exists.
let mut src = LineSource::new(reader, 0, TornTail::Yield, self.adapter_policy());
while let Some((at, line)) = src.next()? { self.advance_at(at, line); }

// A2 — adapter.rs parse_reader, the whole-file metrics fold. Aggressive (§6.1).
// Loses a branch: under Stop a torn line is never yielded, so `malformed` needs no
// `if complete` excuse — the rule moved from a per-site guard to a named argument.
let mut src = LineSource::new(reader, 0, TornTail::Stop, Elision::Aggressive);

// C1 — metrics_fold.rs next_event, the live tail. Aggressive; the one Seek consumer.
let Some((_, line)) = self.src.next()? else { self.src.rewind_to_cursor()?; return Ok(None) };

// A3 — discover.rs latest_cwd. Aggressive; .lines() replaced, same result.
let mut src = LineSource::new(reader, 0, TornTail::Yield, Elision::Aggressive);
```

`advance_at` itself — the single per-line unit for **all** durable block building, batch (A1)
and live follower (C2) alike — does not change signature: it still takes `(offset, &str)` and
the offsets it stamps into `Deferred` locators arrived raw from the source. The elided value
never escapes the line's own fold: blocks hold `{ at, index }`, no bytes, and `load_attachment`
re-reads the raw line from disk (§9.2), so the shortened string dies at the end of `advance_at`.

> **A finding, reported not fixed (pre-existing).** The block fold and the metrics fold disagree
> about torn tails today: `parse_reader` documents why a torn final line must not count as
> malformed (*"a write IN PROGRESS … not schema drift"*) while `advance_reader` feeds that same
> line to `advance_at`, which counts it. Both run over the same live transcripts. Under
> `LineSource` the divergence becomes a visible argument — `Yield` vs `Stop` at two call sites —
> instead of a discrepancy between two loops; *unifying* them would pick a winner and is out of
> #193's scope.

---

## 6. Policy through the seam: α vs β

The scanner (escape-state walk, placeholder, threshold, ceiling, counters) is agent-neutral
engine machinery. The one thing it must not contain is *which nodes may be elided* when that is
not purely a size question — that is agent knowledge and enters through `engine/seam.rs`,
precisely as `LinePreprocessor`/`Shaping` do. Two designs follow from where the policy sits.
**Review re-priced this choice** (§12.3): boundedness forces the policy *inside the streaming
read* — there is no post-hoc `&str` to filter — so α is no longer nearly free.

**Design α — adapter-supplied elision policy.** The adapter answers, per oversized string,
"may this value be elided?" — true for attachment-body nodes (Claude:
`toolUseResult.file.base64` and its `tool_result` twin / `source.data`; Codex: the
`input_image` url, `image_generation_call.result`), false for everything else, so a giant
assistant *text* stays intact. **Viewer-lossless and rendered-output-neutral ⇒ no byte-gate
re-baseline.** Cost, at the new price: the scanner must become **path-tracking while
streaming** — the seed's rule is deliberately "a property of JSON, *not* a list of known paths"
— plus the policy plumbed through `LineSource` *and* `LineReader`. Real new logic on the one
component whose stated hazard is mis-tracking JSON string escapes. **α is the larger half of
the whole change.**

**Design β — the seed's generic rule, as-is.** Any string > 64 KB is elided, no seam hook, the
seed's scanner ships unchanged. Attachment classification still works — the K-byte prefix
carries the `data:` header and magic bytes (§7) — with zero agent knowledge. Cost: the one
giant *non-attachment* string in the corpus (a 0.8 MB assistant text) renders as
`iVBOR…<elided:1042,800000>` — a rendered-output change ⇒ **FOLD_VERSION bump + one byte-gate
re-baseline** (routine), and a viewer regression on that class of line. **The v2.1 span
softens this materially:** the placeholder now names exactly where the full value lives, so a
frontend can offer "load full text" through the same span read the attachment loader uses
(§9.2) — the elided text is one click away, not lost. What remains of the regression is the
default rendering, no longer the reachability.

The α-favoring argument stands — the engine *renders* content; a 0.8 MB assistant text is real
reading, unlike an inert base64 blob that is deferred and never shown — but v1's "recommend α"
was priced before boundedness moved the policy inside the read. **The choice goes to the owner
at the new price** (§11.1), and β-first-then-α is a valid order (the seam hook is additive).

### 6.1 Per-consumer rules, whichever is chosen

- **Metrics folds** (A2, C1, the monitor's ledger, agent-metrics through the seam) take the
  **aggressive** generic rule outright: every field they read is a small scalar (§3), so
  eliding is provably metric-neutral. **Oracle:** eliding produces byte-identical token/cost/
  kind metrics to not eliding — the elision gauges held out of the comparison, since by
  construction they differ (this is §11.3 surfacing in the oracle).
- **Block building** (A1 + C2 through `advance_at`) takes the adapter policy under α — only
  attachment-body nodes, which the block model already defers and never renders inline, so no
  rendered byte changes — or the aggressive rule under β, with the bump and re-baseline. The
  same elided line also feeds the metrics `Value` parse inside `advance_at`; conservative
  elision is a subset of the metric-neutral rule, so one elision serves both readers of the
  line.
- **The head sniffs** (§8) are switched to the primitive **for boundedness, not for elision** —
  they read five lines; whether those are also elided does not matter.

---

## 7. The Codex asymmetry — why the placeholder keeps a prefix

`load_attachment` re-reads the **raw** line from disk and selects the `index`-th
*content-bearing* node, where `index` is a pure **ordinal in document order** — never an
intra-line byte position. A placeholder that keeps the JSON shape and the count of
content-bearing nodes keeps `index` valid. The catch: **decode walks the elided line; the
loader walks the raw line; they must agree on which nodes are content-bearing.**

- **Claude is safe under whole-string elision.** Its shape checks read only structural
  discriminants — `type == "image"`, `source.type == "base64"`, "is a JSON string" — never the
  payload value. A bare `<elided:N>` would pass.
- **Codex is NOT.** `data_image` does `strip_prefix("data:")` / `split_once(',')`, and
  `image_generation_call` sniffs base64 magic bytes (`iVBOR` → PNG, `/9j/` → JPEG,
  `R0lGOD` → GIF, `UklGR` → WEBP). A bare `<elided:N>` makes the parse-side node classify as
  *not* content-bearing while the raw-line loader still counts it → the ordinal desyncs or the
  attachment silently vanishes.

**K = 64 bytes** of kept prefix comfortably covers both discriminators — Codex's
`data:<mime>;base64,` header (~22 bytes) and the magic signatures — so decode and load classify
identically, for both agents, **with no agent knowledge in the scanner**: the engine keeps a
fixed prefix of *any* oversized string and never parses it. `LinePreprocessor::process` is
unaffected throughout — it classifies on structural fields, never the payload.

With the v2.1 span, the load side of this agreement is needed only on the **walk path** (§9.2):
a spanned load seeks straight to the value and never re-classifies nodes, so the ordinal cannot
desync there. The prefix stays required regardless — it is what lets *decode* classify the
elided node as an attachment in the first place.

---

## 8. The audit: every transcript read, bounded or not

Review vetted the whole engine against §2's invariant. The criterion is binary per site and
stricter than "does this site need elision?" — it is **"can any input make this allocation grow
without limit?"**

**Bounded today — no work needed.** A fixed byte window, taken before the read: `tail_pulse`
(64 KB), `inflight_tools_in_tail` (256 KB), the monitor's `last_event_ts` (32 KB),
`first_event_within`, and `read_from` (`take(to − from)`).

**Bounded on one path only.** `session_card`'s *cold* read windows to `TAIL_BYTES` — but its
**memoized** path reads everything appended since the last scan (`from = memo.at`, `to = len`)
and pre-reserves it (`Vec::with_capacity(to − from)`). The monitor takes the memoized path on
every rescan; a session not visited for a long time — or one that appended a single huge line —
is read whole. Fix in §9.3.

**Unbounded in one line's length** — `read_line` and `.lines()` both grow until a newline
arrives; neither has a cap:

| # | site | read | note |
| --- | --- | --- | --- |
| 1 | `parse_reader` (A2) | `read_line` | whole file |
| 2 | `next_event` (C1) | `read_line` | tail delta / whole cold |
| 3 | `advance_reader` (A1) | `read_line` | whole file |
| 4 | `first_cwd` | `.lines().take(50)` | runs on **every** `poll_shared` |
| 5 | `latest_cwd` (A3) | `.lines()` | whole file |
| 6 | `session_id` | `.lines().take(50)` | |
| 7 | Claude snippet sniff | `.lines().take(80)` | |
| 8 | Codex sniffs | `.lines().take(100\|300)` | four sites |
| 9 | Codex first-line probe | `read_line` | |
| 10 | `detect_agent` | `.lines().take(5)` | runs on **every candidate** discovery sees |
| 11 | `load_attachment` (D1) | `read_line` | the one path that *wants* the bytes — §9.2 |
| 12 | `anchor_of` | `read_line` | first line, for the resume-CRC identity check |

> **`take(N)` bounds the line COUNT, not the line SIZE.** This is the trap that made six of
> these read as safe. On a transcript with no newline at all — a truncated write, a binary file
> that reached the store, a single 200 MB line — `detect_agent`'s five-line sniff reads the
> entire file, and discovery runs it against every candidate on the machine. The
> cheapest-looking site in the inventory is the one that fails hardest on malformed input.

**Unbounded in the whole file.** `LineReader::poll` (§2's cold-open measurement, ≈ 430 MB on
the largest local transcript): even with every line capped, `read_to_end` + `Vec<String>` is
O(file). It needs a chunked read **and** a bounded batch per poll — §9.1.

**Scope, stated so omission is not mistaken for coverage.** This audit covers *transcript*
reads. Agents also write **sidecar** files into the same stores, and those are read whole
(`read_to_string`): QoderWork's `<sid>-session.json` sidecar and Claude's `load_tasks_in` task
files — same trust domain as a transcript, but single JSON documents, not JSONL, so there is no
line to elide. They want a size cap before the read: a two-line fix and a **separate issue**,
named here rather than left silent. (The monitor's cost-ledger `read_to_string` is its own
file — a different trust domain.)

---

## 9. The three reads the loop does not cover

### 9.1 `LineReader` — the tail batcher gets a chunked poll

`LineReader` keeps its name, its resume contract, and its role as the follower's line source;
what changes is inside: the `read_to_end` becomes chunked reads through `read_line_elided`, and
`poll` returns a **bounded batch** of lines per call instead of "everything to EOF". The
follower's loop already re-polls, so a bounded batch is a latency choice, not a semantics
change; the partial-line pending buffer it keeps today maps onto the primitive's `Torn`
outcome. This is the step that bounds the cold open — the single largest allocation in the
inventory — and it is a named migration step with its own oracle (§10 step 4), not a deferred
aside.

### 9.2 `load_attachment` — the span fast path, the capture-sink base

The one path whose *purpose* is the bytes, so elision is the wrong operation. Today:

```rust
// core/transcript.rs — seeks to `at`, re-reads that ONE line raw, re-runs the adapter's
// extraction to the `index`-th content-bearing node.
pub fn load_attachment(&self, at: ByteOffset, index: usize)
    -> io::Result<Option<LoadedAttachment>>
```

The transcript on disk is never rewritten — elision is a read-time, in-memory transformation —
so the raw file is the only durable artifact and the placeholder itself never survives to load
time. What survives is the **locator**, and v2.1 widens it: when decode classifies an elided
node as a content-bearing attachment, it parses `{off, len}` out of the placeholder and stores
it, so `Deferred` gains an optional hint — `{ at, index, span: Option<Span> }`, where `Span`
carries the value's raw span and what the walk would otherwise re-derive from *sibling* fields
(Claude's `media_type` lives beside the value, not inside it). Two load paths follow:

- **Span path (the fast one — every elided value has it).** `seek(at + off)`, read `len` bytes,
  unescape (a no-op for base64 — its alphabet has no `"` or `\` — and a real pass for a general
  string), decode. O(the value), no scan, no re-classification — the one scan that ever touched
  the line already recorded where the bytes live.
- **Walk path (the base mechanism).** The same scanner in **capture-sink** mode: stream the raw
  line to the *n*-th content-bearing string and feed that one value into the decoder, dropping
  everything else as it passes — one extra mode on a state machine that has to exist anyway.
  This path serves what *cannot* have a span: sub-threshold values (fast-path lines are copied
  verbatim, never scanned; serde destroys positions, so decode cannot compute spans for them)
  and locators folded before v2.1. Sub-threshold lines are ≤ `SCAN_THRESHOLD` by construction,
  so once the FOLD_VERSION refold has replaced old locators, the walk only ever runs on small
  lines — the two paths partition exactly along cost: spans exist precisely where a rescan
  would be expensive. (A pre-v2.1 locator surviving in an old export still walks its big line;
  the sink keeps that bounded.)

### 9.3 `session_card` — cap the memoized read

The memoized path either caps its catch-up read or re-windows to `TAIL_BYTES` when the gap
since `memo.at` exceeds it (the memo protocol already tolerates a re-derived card). One
function; no new machinery.

---

## 10. Migration order, each step gated on its oracle

0. **Port the primitive + fixtures (+ the seam hook under α).** `elide.rs` → `engine/reader.rs`
   with the K-byte prefix; `LineSource` beside it; under α, the path-tracking scanner and the
   seam policy hook. **Oracle:** escape-state torture fixtures — escaped quotes, backslashes,
   unicode, nested arrays, a 10 MB base64 field — asserting the elided line parses to a `Value`
   shape-identical to the un-elided line (only oversized leaves differ), plus the seed's own
   test suite carried over.
1. **Metrics sites** (A2, C1) → `LineSource`, aggressive. **Oracle:** the existing metrics
   equivalence tests plus §6.1's elided ≡ un-elided metrics fixture over a real > 64 KB line.
2. **`latest_cwd`** (A3) and the head sniffs (4, 6–10, 12) → the primitive. **Oracle:**
   identical results on the same fixtures; for the sniffs, a no-newline 1 MB fixture asserting
   bounded allocation and unchanged detection.
3. **Block building** (A1 + C2 via `advance_at`'s callers) → `LineSource` with the §6 policy.
   **Oracle, made non-vacuous:** a fixture with a **real embedded image** asserting (a) elided
   ≡ un-elided **blocks** — equal modulo the span hint, which only an elided fold can carry
   (held out of the comparison exactly as §6.1 holds out the gauges), and (b) `load_attachment`
   returns **byte-identical bytes** three ways: through the span path, through the walk path
   with the hint stripped, and through an un-elided parse — the first pins §9.2's seek
   arithmetic, the second pins the §7 ordinal. The `--dump`/`--dump-html` byte gate must stay
   **PASS unchanged** under α (β instead re-baselines once, with the diff reviewed
   line-by-line). The vacuity trap is measured: **Claude is covered** — `frozen_self` holds 49
   lines > 64 KB (48 of them base64 bodies, largest ~570 KB) and `frozen_claude_sa` adds 10 —
   but **`frozen_codex`'s largest line is 43 KB, zero over threshold**, and Codex is exactly
   where §7 bites. **Step 3 therefore adds a Codex fixture carrying a real
   `data:<mime>;base64,…` image over 64 KB, or its "PASS unchanged" proves nothing.**
   - Named verification: grep the decode paths to confirm nothing derives a rendered or stored
     value from the payload *string itself* (length, decoded dimensions, a stringified copy).
     The classification walks are verified structural; this closes the derivation question. Any
     node that does is excluded from α's policy (or preserved).
4. **`LineReader`'s chunked poll** (§9.1). **Oracle:** `follow_matches_full_reparse` stays
   green, and a peak-RSS assertion over a fixture containing one 8 MB line, against the
   un-elided baseline.
5. **`session_card` cap + `load_attachment` capture sink** (§9.2–9.3). **Oracle:** card
   equality on a re-windowed read; attachment bytes identical through the capture sink.

Then agent-metrics deletes its local `elide.rs` and calls the seam — the motivation the study
exists to serve.

---

## 11. Decisions for review

Two things are settled, not open:

- **Placement — settled by the invariant.** A streaming read has no expression under v1's
  per-site placement (there is no already-read `&str` to filter — the elision must happen
  inside the read), so per-site placement forecloses boundedness permanently. `LineSource` +
  the primitive is the placement. (Reopens only if the owner rejects boundedness as the goal.)
- **The span — settled at owner review (v2.1, §12.5).** The placeholder carries
  `<elided:{off},{len}>` and `Deferred` gains the optional span hint; the loader seeks instead
  of re-scanning. This carries the FOLD_VERSION bump for the locator field — which re-weighs
  decisions ① and ③ below.

1. **α vs β** (§6) — *the headline, re-priced twice.* α: adapter policy through the seam,
   viewer-lossless, no re-baseline — but requires a path-tracking streaming scanner and is the
   larger half of the change, with the new logic sitting on the escape-tracking hazard. β: the
   seed's scanner as-is, one byte-gate re-baseline, and the 0.8 MB-assistant-text regression —
   **now softened by the span**: the elided text is one click away through the same span read,
   so β's cost is a default rendering, not lost content. β-first-then-α remains a valid order,
   and the span shifts the balance toward β being *enough*. The owner picks the price.
2. **Constants** — `ELIDE_STRING_BYTES = 64 KB`, `SCAN_THRESHOLD = 256 KB`, prefix `K = 64 B`,
   `ELIDE_CEILING = 64 MB`. All inherited from the seed except `K`, the engine's addition for
   §7.
3. **Counter home** — where `elided_lines` / `elided_bytes` / `skipped_lines` live. v1 framed
   this as "sets whether the design carries a FOLD_VERSION bump at all"; **the span hint (v2.1)
   bumps FOLD_VERSION regardless**, so (a)'s headline cost vanished. Silent loss is the failure
   to avoid, and `skipped_lines > 0` (the ceiling — genuine data loss) should read louder than
   routine elision wherever they land:
   - **(a) `Metrics::extra`** — gauges flow to footer + monitor like `compact_dropped`, riding
     the bump v2.1 already pays; needs the gauges-held-out clause in the step-1 oracle (the
     same clause the span hint needs anyway). Fullest visibility.
   - **(b) Per-fold report output** — returned by the fold (`LineSource.elided`), not
     persisted. No oracle clause; the monitor never sees them (the live footer and a `--json`
     report do).
   - **(c) Compute-but-don't-persist** — in the live accumulator for the footer, dropped from
     the checkpoint. A *resumed* view loses them.
   Recommendation updated: **(a)** — it is now the free option, and the durable elision signal
   is exactly what the monitor should see when a store grows a pathological transcript.

Build gating is the usual: `cargo fmt`/`clippy`/`test`, the byte gate on the frozen fixtures
(plus step 3's new Codex fixture), `follow_matches_full_reparse`, and the per-step oracles.
All **design-only** until reviewed.

---

## 12. Review trail (2026-08-20) — what v2 changed, and the carried invariants

### 12.1 The scope correction

v1's mechanism (§4's placeholder, §7's ordinal analysis, the CRC proof below) survived review
intact. Its *placement* did not: eliding a `&str` that already exists removes everything
downstream of the buffer — the `Value` DOM multiplier, `decode_line`'s walk — but the raw
`read_line` allocation itself survives, and the loop's reused buffer keeps that high-water
capacity for the rest of the parse. v1's inventory was a *relevance* inventory (which sites
materialize a DOM), not a *boundedness* one: it missed that `take(N)` bounds count not size,
missed `LineReader::poll` entirely, missed `anchor_of`, and dismissed `load_attachment` as
"O(one line)" when one line is precisely the unbounded quantity. The audit (§8) re-ran the
question as "can any input make this grow without limit?", found 13 unbounded transcript reads,
and moved the chunked read from a deferred aside to the deliverable. What made GO cheap: the
streaming primitive was already implemented and tested in the seed — the only genuinely new
logic in the whole plan is α's path-tracking (if chosen) and the capture sink.

### 12.2 Findings surfaced, not fixed

- **The torn-tail divergence** (§5): the block fold counts a torn final line malformed; the
  metrics fold documents why it must not. Pre-existing, pre-dates #193, now visible as a
  `Yield`/`Stop` argument.
- **The sidecar reads** (§8): whole-file `read_to_string` of agent-written JSON — inside the
  invariant, outside this mechanism; separate issue.

### 12.3 What re-priced α

v1 recommended α when the policy hook was a predicate consulted after parse-side scanning.
Boundedness moves the policy *inside* the streaming read at every site, which demands
path-tracking while streaming — the seed is explicit that its rule is "a property of JSON, not
a list of known paths" — so α went from nearly-free to the larger half of the change. §11.1
hands the re-priced choice to the owner rather than inheriting v1's recommendation.

### 12.4 Invariants carried unchanged from v1

- **Offsets advance by the full raw byte count, always** — now structural (§4).
- **The resume-window CRC never sees an elided byte — proven.** `window_at` opens its *own*
  file handle and CRC32s raw disk bytes of the 64 KiB window; both the cache admit and the
  metrics cursor validate by recomputing from disk. Elision touches only the in-memory line fed
  to the parser, so it is *structurally impossible* for it to reach the CRC. Resume cannot
  break.
- **Residency stays bounded — strictly improved.** A resident `Session` holds locators, not
  bytes; the 8 MB base64 was only ever transient during folding, and elision makes the
  transient smaller-or-equal, never larger.
- **Elision happens once**, at the read, so every downstream consumer sees one body.
- **Torn tails stay unconsumed** where a durable cursor watches (`Stop`); the ceiling-skip
  consumes to the newline and counts.

### 12.5 The span amendment (owner review, 2026-08-20)

v2 loaded attachments by re-deriving: `Deferred { at, index }`, re-read the raw line, walk to
the *n*-th node — "never store what a rescan can recompute." The owner rejected the trade:
*the scan that already passes over every byte of the value should record where it lives*, so
the placeholder became `<elided:{off},{len}>` and the locator gained the span hint (§9.2).

What the exchange established, kept because the next reviewer will re-ask it:

- **The disk is raw.** The transcript is never rewritten; the elided line is transient. A span
  in the placeholder is therefore only a *transport* from scanner to decode — to survive to
  click time it must be persisted in the locator. The in-band transport won over a side-channel
  span list because it is self-synchronizing: the span rides inside the very string decode is
  classifying, so there is no span-to-node matching to get wrong.
- **The walk cannot be deleted, and doesn't need to be.** Spans exist only for scanned lines
  (> `SCAN_THRESHOLD`) and elided values (> `ELIDE_STRING_BYTES`); serde destroys positions for
  everything on the fast path, so sub-threshold attachments keep the ordinal walk. The split
  partitions exactly along cost: spans precisely where a rescan would be expensive, the walk
  precisely where it is cheap.
- **Two v2 objections did not survive scrutiny.** "It breaks the equivalence oracle" — the
  doc already holds elision-only artifacts out of comparison (the §6.1 gauges); the hint rides
  the same clause. "The rescan is cheap and cold" — true, but it defended a recompute-forever
  choice where one integer pair captured at scan time ends the question; and the span turned
  out to buy more than load speed (β's regression softened, §6; counter-home (a) freed, §11.3).
