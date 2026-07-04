# Design: streaming transcript parse (cut the load-time memory spike)

Status: **implemented on branch `perf/streaming-parse`.** Scope: `model::parse`
(+ the file reads in `app.rs`, and a streaming `metrics::parse_reader`). Does
**not** touch the render model, live-tail correctness, or the picker.

## Results (measured)

Peak RSS, `--dump -`, release binary, old (0.2.2 two-pass) vs new (streaming):

| file | old peak | new peak | reduction |
|---|---|---|---|
| 12 MB | 173 MB | 131 MB | 1.3× |
| 78 MB | 577 MB | 316 MB | 1.8× |
| **298 MB** | **2144 MB** | **811 MB** | **2.6× (−1.3 GB)** |

Parse+dump wall time on the 298 MB file: 3.7 s → 4.0 s (**+8%**, despite two
passes — the pre-scan is id-only and the second pass no longer builds a whole-file
`Vec<Value>`). The remaining 811 MB is the retained block + render model (full
block text + ratatui `Line`/`Span` overhead), not the parse; shrinking it needs
viewport windowing, which stays out of scope (see Non-goals).

**Output equivalence:** byte-identical `--dump` (folded and `--full`) to 0.2.2 on
29/30 real transcripts sampled (all normal sessions, including the 298 MB one). The
one differing file is a **forked/resumed session with 256 duplicate tool_use ids**;
see "Duplicate ids" below.

**The key assumption below ("tool_use precedes its result") turned out to be FALSE**
on real 78/298 MB transcripts — results can precede their tool_use (compaction /
sidechain reordering). The design was corrected with a tool_use-id **pre-scan** pass
+ a `pending` buffer; the sections below are updated to match what shipped.

## Problem

Opening a transcript peaks at **~7× the file size in RAM**, measured on real
sessions with the 0.2.2 release binary (`--dump -`, peak RSS via `/usr/bin/time -l`):

| file | peak RSS | ×file |
|---|---|---|
| 12.1 MB | 175 MB | 14× |
| 78.4 MB | 550 MB | 7× |
| **298.4 MB** | **2.21 GB** | **7.4×** |

Model: `RSS ≈ ~120 MB fixed (syntect syntax/theme defs) + ~7× file bytes`. The
298 MB session — a real one in this account — costs **2.2 GB** and would OOM a
small machine.

### Where the memory goes (verified)

The peak is **not** the retained render model — it's a transient during load.

- The **rendered** text for the 298 MB file is only **14.8 MB (5% of source)**.
  Images don't survive: 270 base64 blocks in the source → 17 in the rendered
  output (the 17 are base64 sitting inside command/result *text*, not `image`
  content blocks). `parse` already drops `type:"image"` blocks and reduces
  image-only tool_results to empty. So **steady-state resident state is small.**
- The spike is two whole-file allocations at load:
  1. `app.rs:24` `std::fs::read_to_string(path)` — the entire file as one `String`
     (298 MB), shared by `model::parse` and `metrics::parse`.
  2. `model.rs:392` `let parsed: Vec<Value> = jsonl.lines()…collect()` — **every**
     line decoded into `serde_json::Value` and held **simultaneously** before block
     building starts. `Value` is ~5–8× the JSON bytes, so this vector alone is
     ~1.5–2 GB for the 298 MB file. This is the balloon.

`metrics::parse` (`metrics.rs:59`) already streams line-by-line and drops each
`Value` — it is **not** part of the problem. Only `model::parse` collects.

## Goals / non-goals

**Goals**
- Eliminate the ~1.5–2 GB transient `Vec<Value>` balloon — the dominant term in
  the 2.2 GB peak. After that the peak is bounded by whatever whole-file
  allocations remain (see "Expected outcome" for the two paths and their floors).
- No change to the produced `Vec<Block>` — byte-for-byte identical render output.
- Keep parse fast (low-seconds on the 298 MB file; ≤~2× current CPU is acceptable).

> **Note on "retained".** The 14.8 MB "5% of source" figure above is the *folded
> dump output text* — a lower bound, **not** the in-memory retained footprint. The
> resident model holds the **full unfolded** block text (collapsed content is still
> stored) plus the styled lines materialized twice (`raw` + `body_cache`, each a
> `Vec<Line>` of `Span{Cow<str>, Style}` with real per-span overhead). So retained
> RAM is a multiple of 14.8 MB and is **currently unmeasured** — the fix's PR must
> measure it rather than assert it.

**Non-goals (explicitly out of scope)**
- Viewport windowing / true O(1) resident memory. The retained model is already
  ~5% of source; windowing is a large rearchitecture (search, metrics, offset
  math all assume full residency) and is deferred. This doc only kills the load
  spike.
- Live-tail parse *correctness* (the `trigger_ts` / cross-poll tool_result-join
  discussion). Orthogonal; complementary. Noted under "Interactions" below.

## Current shape of `model::parse`

Two passes over a fully-materialized `Vec<Value>`:

- **Pass 1** builds `results: HashMap<id, (String, Value)>` (text + the whole
  `toolUseResult`) and `tool_ids: HashSet<id>`.
- Finds `cwd` by scanning for the first event that has one.
- **Pass 2** walks events in order; for each `tool_use`, looks up its result in
  `results` to fill `output`/`patch`/`read_lines`; tracks `trigger_ts` forward for
  thinking durations.
- `group_turns(out)` at the end folds each `Thinking` with its preceding activity
  tools.

The two-pass design exists because a `tool_result` (a `user` event) is written
**after** the `tool_use` (an `assistant` event) it answers — a forward pass hits
the `tool_use` before its result exists.

## Implemented design: id pre-scan + streaming back-patch

> Revised from the original "single forward pass" proposal: real transcripts break
> the tool_use-precedes-result assumption, so a lightweight **pre-scan** pass and a
> `pending` buffer were added. Two passes, each a fresh streaming read.

**Pass 1 — `scan_tool_ids`:** stream the file once, collect the `HashSet<String>`
of every `tool_use` id (ids only — ~40 bytes each, <1 MB even for 17 k tools). This
lets pass 2 distinguish a genuine orphan `tool_result` (id in no tool_use) from one
whose tool_use simply appears later.

**Pass 2 — `parse_main`:** parse **one line at a time**, drop each `Value`
immediately, back-patching results into already-emitted blocks:

State carried across lines (all O(1) or O(retained), never O(all-Values)):
- `out: Vec<Block>` — the blocks (retained anyway).
- `tool_slot: HashMap<String, usize>` — tool_use id → index in `out`, so a later
  result can fill in that block. Holds only ids + indices.
- `pending: HashMap<String, (String, Value)>` — results seen **before** their
  tool_use (id is in the pass-1 set, so the tool_use is coming). Applied when that
  tool_use arrives; normally near-empty (reordered pairs are local).
- `trigger_ts: Option<f64>`, `cwd: String`.

Per line:
- **assistant / tool_use** → push `Block::ToolUse{ output: None, patch: None, … }`,
  record `tool_slot[id] = out.len()-1`. If `pending` has the id, apply it now.
- **user / tool_result** → if `tool_slot` has the id, patch that block's
  `output`/`patch`/`read_lines` from this line's `toolUseResult` (then the `Value`
  drops). Else if the id is in the pass-1 set, stash `(text, tur)` in `pending`
  (its tool_use appears later). Else it's a genuine orphan → push `ToolResult`
  inline (matching the old `tool_ids` skip semantics exactly).
- everything else (text, thinking, user prose, commands, notifications) → as today.
- **cwd**: capture from the first line that carries `cwd` (session start always
  does). Fallback `""`.

`group_turns(out)` runs unchanged at the end (operates on `Block`s, cheap).

Net effect: at most **one line's `Value`** is live at a time. Retained state is the
id set (<1 MB), an id→index map, and a near-empty pending buffer. Tool outputs live
in the blocks we keep regardless.

### Duplicate ids (forked/resumed sessions)

A resumed/compacted session can replay history, so the **same** tool_use id appears
in two use/result pairs. There is no canonical join here — the transcript is
ambiguous. The old two-pass did **global last-wins** (the last result for an id is
smeared onto *every* same-id tool_use). The new streaming parse pairs each tool_use
with the result that matches it **chronologically** (via `tool_slot`/`pending`),
which is arguably more correct and is always a superset (it never shows *less* tool
output than old). Exactly reproducing old's last-wins would require holding every
result's `Value` to the end — reintroducing the memory cost this change removes — so
we accept the (more-correct) divergence. This is the sole source of the 1/30
non-identical file in the equivalence sweep.

### Also stream the file read

Replace `read_to_string` + `&str` with a line reader so the 298 MB `String` is
never fully resident either:
- `model::parse` gains a variant that takes `impl BufRead` (or an
  `Iterator<Item = io::Result<String>>`); the existing `&str` entry point wraps it
  over `Cursor` so tests stay unchanged.
- `app.rs` opens a `BufReader<File>` for `model::parse`. `metrics::parse` needs a
  second pass over the file — either re-open the file (cheap I/O, no big string) or
  fold metrics into the same streaming pass later (separate change).

## Expected outcome (superseded by "Results (measured)" above)

> The predictions below were pre-implementation; the measured 298 MB peak came in
> at **811 MB** (both `collect()` removal *and* read-streaming shipped). Kept for
> the reasoning.

Removing `collect()` deletes the ~1.5–2 GB `Value` vector. The remaining peak
depends on whether we also stream the file read — this is the crux of open
question #3:

- **Drop `collect()` only, keep `read_to_string(&str)`:** the 298 MB file `String`
  stays fully resident and becomes the **new floor**. Peak ≈
  `fixed + 298 MB String + retained render` → order-of **~500 MB** for the 298 MB
  file. Big win over 2.2 GB, but String-dominated.
- **Also stream the read (no whole-file `String`):** peak ≈ `fixed + retained
  render`, with only one line's text + one line's `Value` transient at a time. This
  is the path that can approach the retained model's true size.

So the two allocations are ~2 GB (`Value`s) and ~0.3 GB (`String`); dropping
`collect()` gets the larger slice, but the `String` floor means **read-streaming is
required to get near the retained model, not optional** (revises the earlier lean —
see open question #3). Exact post-fix peaks are **to be measured** in the PR (re-run
the RSS table), not asserted here, because retained RAM is unmeasured (see the Goals
note).

CPU: essentially unchanged — same number of `from_str` calls (one per line); we
remove a `Vec<Value>` allocation, add a small hashmap. No double-parse. (The earlier
"~2× CPU" worry only applied to a re-parse variant we are **not** doing.)

## Testing (per CLAUDE.md — deterministic, no TTY) — shipped

- **Golden equivalence:** the existing `model.rs` block-shape tests
  (`thinking_groups_preceding_tools_with_duration`, `joins_tooluseresult_metadata`,
  `nothing_is_dropped_by_default`, …) pass **unchanged**. All 94 unit tests + the
  opt-in tmux e2e pass; `cargo fmt`/`clippy` clean.
- **Added** `result_before_tool_use_still_joins` — reversed order joins (Edit keeps
  its structuredPatch line number); this is the exact real-transcript bug.
- **Added** `orphan_result_with_no_tool_use_shown_inline` — genuine orphan still
  shown inline.
- **Added** `parse_path_matches_parse_str` — the streaming file path equals the
  `&str` path for the same content.
- **Manual equivalence sweep:** `--dump` (folded + `--full`) diffed old vs new over
  30 real transcripts → 29 byte-identical (see Results / Duplicate ids).

## Risks & edge cases

- **Ordering.** Real transcripts DO put results before their tool_use — handled by
  the id pre-scan + `pending` (covered by `result_before_tool_use_still_joins`).
- **Duplicate ids.** Forked/resumed sessions reuse ids; new pairs chronologically
  vs old's global last-wins (see "Duplicate ids"). More correct, always a superset.
- **`&str` callers.** `parse(&str, …)` (live tail + tests) now does two cheap passes
  over the batch string; unchanged output.
- **Live tail.** `View::ingest` still receives a `Vec<Block>`; the reset branch now
  uses `parse_path` too. Cross-poll correctness items remain separate.
- **`pending` worst case.** If many results precede their uses at once, `pending`
  holds those `Value`s transiently. In practice reordered pairs are local (adjacent
  lines), so it stays near-empty; not O(1) worst-case but bounded by reorder span.

## Interactions with other planned work

- **Live-tail parse correctness (C0/C2 discussion):** independent. If we later move
  the live path to a stateful/streaming incremental parser, this single-pass
  back-patch structure is the natural core to build it on (the `tool_slot` /
  `pending_results` / `trigger_ts` state is exactly what an incremental parser must
  carry across polls).
- **Viewport windowing:** if ever pursued, this streaming pass is a prerequisite
  (you can't window if load already balloons to 2× file in `Value`s).

## Open questions for review

1. `metrics::parse`: re-open the file for its pass (simplest), or merge metrics
   into the single streaming pass now (more change, one read)? Leaning **re-open**.
2. Ship the streaming *file read* in the same change, or land the `Vec<Value>`
   removal first (bigger win, smaller diff) and stream the read as a follow-up?
   Leaning **`Value` removal first**, read-streaming second.
3. Worth a `parse` signature that takes `impl BufRead`, or keep `&str` and just
   drop the `collect()`? Dropping `collect()` alone removes the ~2 GB of `Value`s
   but **leaves the 298 MB file `String` as the resident floor** (peak ~500 MB).
   To approach the retained model we must also stream the read. **Lean: do both** —
   land the `collect()` removal first (largest win, smallest diff), then the
   read-streaming as a fast follow, since the `String` floor is the whole reason to
   bother with the second step. (This supersedes the earlier "read-streaming is the
   smaller remaining slice" framing — it's the slice that unlocks the goal.)
