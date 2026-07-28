# Session storage, streaming, and the presentation cache

Status: **design locked**, execution phased (see §11). This is the consolidated design for
how a parsed session is stored, streamed, reused across restarts, and presented — the layer
*above* the emit-and-drop core described in `streaming-core-memory.md`. That doc covers the
Replayer's durability frontier and O(turn) content; this one covers the **storage record
format**, the **`Session` shape**, and the **presentation cache** that real apps sit on.

It supersedes the ad-hoc `TierBSession::persist/load` added in `c9f4175` (see §11, Phase B).

---

## 1. Goals and non-goals

**Goals**

- Core never *forces* O(N) resident content on a consumer; a consumer that wants O(turn)
  content (paged, or dropped) can have it. The in-memory default stays byte-identical.
- One session's durable form is a **self-describing append-only log** that can be reopened
  across process restarts and caught up to a growing transcript without a full re-fold.
- The HTML live feed becomes **O(delta) render + O(delta) bytes**: render each block once,
  and the render cache *is* the wire record streamed to the client.
- One reusable **presentation layer** with two concrete renderers (TUI, HTML) — no duplicated
  incremental-update logic.

**Non-goals (for now — see §10/§11)**

- Fine-grained structural grouping ops (nest/coalesce with stable ids). We use turn-grain
  `Commit`.
- In-stream metadata checkpoints for mid-stream reconstruction. Dropped — metadata is
  resident by default (§8).
- Persisting the TUI's rendered form (ratatui rows aren't serializable — §10).

---

## 2. The three layers

```
claude-replay-core            pure library — mechanism, no policy, no presentation types
  Replayer                    folds Messages → emits Provisional / Commit / Meta / Reset
  SessionAccumulator<S>       ingests Blocks, put: Block→S::Bv per Commit; the RESIDENT unit:
                              committed: Vec<S::Bv> + provisional: Vec<Block> + metadata
  BlockStore                  InMemoryStore (Bv = L) · DeferredBlockStore (Bv = Deferred, L = Block)
  BvLog                       append-only record file (Bv / Meta / Rewind) + read-by-offset
  model · metrics · follow · SessionIndex::push / push_sub_agent (incremental folders)

present layer  (root crate, top-level, beside present/fold/highlight) — what real apps need
  cache                       catalog of all sessions; residency of loaded SessionAccumulators;
                              evict / re-admit; the two residency policies (§7)
  reconcile                   apply Provisional / Commit → append + turn-commit rewind
  viewport                    block window + per-block line-height index + render cache;
                              L may be a render record / union
  trait Renderer { type Rendered; fn render(&Block,width)->Rendered; fn height(&Rendered)->u16 }

tui/  ·  html_export/          Renderer impls; the html server streams the cache's render records
```

Three axes, each owned by exactly one layer:

- **what to hold** (`L`) — the presentation chooses (`Block`, a render record, a union);
- **where it lives** (in-memory vs deferred-to-file) — the `BlockStore`, generic in core;
- **which sessions stay resident, how they update, how they render** — the cache, in the
  present layer.

Core names **no** presentation types: `DeferredBlockStore` is `L = Block`; a store that holds
a render record is a *presentation* store built on the generic `BvLog`.

---

## 3. The emit model: `Provisional` / `Commit`

The Replayer stays the atomic transform (no store, no BV, no index — so `--dump` can drive it
store-free). It emits:

```rust
enum Emit {
    Provisional(Block),   // a raw block appended to the OPEN window — shown live, not yet grouped
    Commit(Vec<Block>),   // a turn (or pinned run) closed: its FINAL grouped blocks,
                          //   superseding that turn's provisionals
    Meta(TurnBoundary { time: Option<EpochSeconds> }),  // per user turn (feeds user_times)
    Reset,                // whole-file truncation/compaction only — discard all (defensive; never
                          //   fires on real append-only transcripts)
}
```

- **Grouping stays in the Replayer** (`finish_turns`: thinking absorbs the preceding activity
  run; runs coalesce). The emit *conveys the result*: `Commit` carries the already-grouped
  blocks. Consumers never re-derive grouping.
- **The open turn is a transaction.** `Provisional`s accumulate for latency; `Commit` is the
  transaction commit — it replaces that turn's provisionals with the grouped form and moves
  them past the durability frontier. This is the "rewind" made semantic and turn-scoped rather
  than positional (`reset/from N`). The only mutable region is the open window, so a `Commit`
  replaces O(turn).
- **The durability frontier** (from `streaming-core-memory.md`) decides *when* a turn commits:
  a turn stays in the open window (un-committed) while it is pinned by (a) a **pending prompt
  queue** (its `⧗` marker may still be suppressed at dequeue) or (b) an **un-bodied `Skill`**
  (a `SkillBody` may still nest into it). Both pins are already implemented (commit `a49467e`).
  So `provisional` = the open turn **plus any pinned earlier turns**; `Commit` fires as the
  frontier advances.

Rationale for turn-grain over fine-grained nest/coalesce ops: the mutable region is one small
turn, so re-sending it on `Commit` is O(turn); fine-grained ops save that at the cost of stable
ids + nest/coalesce support in every renderer. Deferred (§10) unless a profile demands it.

---

## 4. The `Session` shape: committed + provisional

```rust
struct Session<BV = Block> {
    committed:   Vec<BV>,            // durable, already grouped   (BV = Block, or Deferred)
    provisional: Vec<Block>,         // the open window: raw, un-grouped — ALWAYS resident, O(turn)
    index:       SessionIndex,       // resident metadata, small
    metrics:     Metrics,            //   ""
    sub_agents:  BTreeMap<..>,       //   ""
    user_times:  Vec<..>,            //   ""
    agent / cwd,
}
```

- `committed.len()` is the commit frontier. A renderer shows `committed` + a live view of
  `provisional`, and swaps a turn from provisional→committed on the next `Commit`.
- **Metadata is small and always resident** (index/metrics/sub_agents/user_times +, in deferred
  mode, the committed offset table + the open-turn fold-state). Only the **committed block
  content** is ever deferred off-heap. This is the key simplification (§8).

The presentable `Vec<Block>` a consumer sees today is `committed ++ finish_turns(provisional)`;
because the frontier sits at a user-turn boundary and `finish_turns` distributes over such
boundaries, this equals a global finalize (already proven byte-identical by C1).

---

## 5. Storage: `BlockStore`, `BV`, and the `BvLog`

### 5.1 The store seam

`BlockStore::put(Block, at) -> S::Bv` is applied by `SessionAccumulator` **as it ingests each
`Commit`** — this is where `Block → BV` happens, incrementally.

- **`InMemoryStore`** — `Bv = L`, held in RAM. `put` is identity for `L = Block` (today's
  default, byte-identical).
- **`DeferredBlockStore`** — `Bv = Deferred { offset, size }`. `put` appends the serialized `L`
  to a `BvLog` and returns the locator. Core ships this for `L = Block` (canonical). A store
  that appends a *render record* (`L = JSON`) is the same shape built in the present layer over
  the same `BvLog` — core stays presentation-agnostic via the `L: Serialize` bound.

`Deferred` is "a deferral **of BV**": the file holds a serialized copy of `L`, and the handle
is its offset. `Vec<Deferred>` (the committed offset table) *is* metadata — resident, tiny.

### 5.2 The `BvLog` record format

One append-only, self-describing file per session:

```
header  { magic, version }
records (framed <len:u32><tag:1B><payload>):
  Bv      <serialized L>                 a block value (committed, or a provisional-tail block)
  Meta    <turn time | metrics>          non-derivable running state (see §8 on metrics)
  Rewind  <to: byte offset>              the provisional tail from here is superseded (grouping/queue)
```

- **Committed prefix is immutable.** Committed `Bv` records are appended once and never change.
- **Provisional tail is the mutable region.** As the open turn folds, its raw blocks are
  appended as `Bv` records after the last commit. On the next `Commit`, a `Rewind` marks the
  provisional tail superseded and the grouped committed `Bv` records are appended (advancing the
  committed watermark). The file stays **strictly append-only** — a live async reader (§7,
  point 5) reads records in order and applies `Rewind` itself; nothing is truncated under it.
- **The committed/provisional watermark** = the byte offset of the first provisional record.
  Held in resident metadata; after compaction it is simply the file end.
- Superseded provisional records are **garbage** (≈ up to 2× committed size in the worst case).
  Reclaimed by **compaction on evict** (rewrite committed `Bv` + `Meta` only) — deferred (§11
  Phase D). Tolerated while a session is live.

`Reset` is **not** a `BvLog` record — a whole-file truncation invalidates the log; the follower
rebuilds. The turn-commit `Rewind` above is the only in-log rewind, and it only ever targets the
provisional tail.

---

## 6. Reading a stored session

- **`BlockAccess for Session<Deferred>`**: read a committed block = seek to `Deferred.offset`,
  read `size`, deserialize `L`. Optional small LRU.
- **Random access for streaming** (§7 point 5): the committed **offset table** (`Vec<Deferred>`,
  resident) gives O(1) `committed[N]` → seek + read. The provisional tail is read sequentially
  from the watermark.
- **Cold load** (fresh process, nothing resident): scan the log once — `Bv` records rebuild the
  offset table + fold `index`/`sub_agents` via the incremental `push` folders; `Meta` records
  give `user_times`/`metrics`; apply `Rewind`s to drop superseded provisionals. Then keep the
  `Session` resident (§8). One O(N) scan per process per session, or O(1) via a sidecar (§8).

---

## 7. The presentation cache

Owns *policy*; holds a collection of resident `SessionAccumulator`s.

1. **Catalog** — every session ever registered → `{ id → transcript locator }`. Content-free;
   unbounded is fine.
2. **Load** — fold the transcript (or replay the `BvLog` on cold start) → build the session
   object + write/extend the `BvLog`. Two residency policies:
   - **materialized (default):** `committed: Vec<Block>` resident.
   - **deferred (opt-in, cache policy):** `committed: Vec<Deferred>` (offset table) + the
     provisional-start offset; committed content stays on the `BvLog`, read on demand. Metadata
     resident either way.
3. **Evict** (memory pressure / policy) — drop the resident committed *content*; **keep the
   `BvLog` on disk** and (by default) keep the small session object (metadata + offset table +
   O(turn) fold-state) resident.
4. **Re-admit** — re-materialize committed content from the `BvLog`; `open_at(source_tell)` and
   **delta-fold the transcript tail** onto the resident fold-state; append new records. No
   re-parse, no metadata reconstruction (it was never dropped).
5. **Async streaming** — a client requests committed blocks from index `N`: the offset table
   seeks and streams `Bv[N..]` render records, then live records as they append (applying
   `Rewind` for the open turn). This is the deferred policy's raison d'être.

---

## 8. Metadata: resident by default; sidecars are position-tagged and optional

The decisive simplification: **metadata is small and, once a session is loaded, the session
object stays resident.** So nothing ever needs to reconstruct metadata *at a middle index* —
`Ckpt`-in-stream is dropped. The two places it seemed needed both dissolve:

- streaming from `N` needs only the resident **offset table**, not a metadata replay;
- re-admit keeps the resident session object.

The **only** rebuild is a genuine **cold start**, handled by the one-time log scan (§6). If that
scan's cost ever matters, add an **optional, disposable, position-tagged sidecar** — never a
bare snapshot (that is a staleness trap: log grows, sidecar doesn't, two disagreeing truths):

```
sidecar = { state, valid_up_to: <BvLog offset at a commit boundary> }
load: log_len == valid_up_to → use as-is (O(1))
      log_len >  valid_up_to → replay log[valid_up_to..] through the incremental folders (self-correcting)
      log_len <  valid_up_to → discard, full rebuild (truncation; shouldn't happen)
```

Consistent by construction (the offset *is* the contract), disposable (drop → rescan), and it is
the checkpoint benefit **out-of-band** — no log pollution, no two-writer consistency surface.
A `metrics` sidecar and an `index` sidecar are two instances of this shape. **Deferred** (§11
Phase D) — the default is scan-once-then-resident, with **zero** sidecar.

`metrics` is the one value not derivable from `Bv` content (it comes from transcript `usage`).
Decision: keep the metrics fold **parallel** (as today) and carry it as a **trailing cumulative
`Meta`** record, so even a no-sidecar cold start reads it without a full fold. (Routing metrics
through `Emit::Meta` deltas instead — making the log intrinsically self-sufficient — is possible
but a larger fold refactor; deferred, see §10.)

---

## 9. The present layer and the O(delta) live feed

- **`reconcile`** replaces `html_export::serve::stream_delta`. Instead of *diffing snapshots*,
  it **applies the emit records**: `Provisional` → append a live row; `Commit` → swap that
  turn's rows for the grouped ones (the turn-scoped rewind). Written once, consumed by both the
  HTML JSONL feed and a windowed TUI's "which rows changed" update.
- **`viewport`** holds the block window + per-block line-height index + a bounded render cache,
  generic over `Renderer::Rendered`.
- **`Renderer`** turns a `Block` into the frontend's rendered form once; `viewport` caches it.

The efficiency win, precisely: for HTML, `L = the JSON render record`. `put(Block)` renders
once → the cache entry **is** the wire record. The server flips from *pull-and-re-render every
poll* to *push-once*: emit a durable block → render once → append the serialized record → the
client just inserts it. Only the open turn re-renders (O(turn)); committed turns render exactly
once. The TUI gets the identical shape with `Rendered = Vec<Line<'static>>`, cached in the same
`viewport`.

---

## 10. Layering consequences worth naming

These are inherent to the module boundaries; documented so they are choices, not surprises.

1. **The TUI cannot persist its rendered form.** `ratatui::Line<'static>` isn't `Serialize`, so
   a `DeferredStore<L = TUI-rows>` is not possible. The TUI therefore uses `L = Block` (deferred)
   + an **in-memory** render cache; only HTML (`L = JSON record`) persists rendered records. This
   is fine: the TUI is interactive/single-session; the HTML server serves many and benefits from
   persisted, streamable records.
2. **A render-store's `L` is frontend-specific.** A `BvLog` of HTML records can't be re-rendered
   by the TUI. So the **durable, cross-frontend** artifact is the canonical `L = Block` log; a
   render-record log is a **derived cache** (disposable, like a sidecar), keyed by the same
   `Deferred`. An app serving both frontends keeps the Block log as source of truth; a
   single-frontend live server may keep only its render log. The mechanism (generic over `L`)
   supports all; the *policy* is the app's. (A union `enum L { Raw(Block), Html(Record) }` in one
   log is available if an app wants both in one file.)
3. **Rendering happens in the store's `put`, which is presentation.** So the render store lives
   in the present layer and drives the *core* accumulator with itself; the fold logic isn't
   duplicated (only the store differs). Consequence: a render store renders **eagerly** (per
   `Commit`) — right for the streaming server; the TUI wants **lazy** (render the visible window)
   and so uses a Block store + `viewport` render cache. Both are first-class.
4. **Provisional garbage.** Persisting the provisional tail (§5.2) costs up to ~2× committed
   size until compaction. Accepted; compaction-on-evict deferred (§11 Phase D).

---

## 11. Execution plan (locked)

Already landed (this redesign, byte-identical, gated): the durability frontier + O(turn) Replayer
window (`a49467e`, C1); the tier-b *mechanism* `Deferred`/`TierBStore`/`BlockAccess` (`7c0202c`)
— to be folded into `BvLog`; incremental `SessionIndex::push` / `push_sub_agent` (`43246aa`,
`9caebc6`) — the replay/rebuild primitive; serde on the `Block` model + `Agent`/`Metrics`/
`SubAgentMeta`/`Deferred`.

**Phase A — C2: accumulator owns `committed` (build now, core, byte-identical).** Move `durable`
ownership from the Replayer to `SessionAccumulator`: `committed: Vec<S::Bv>` filled by `put` per
`Commit` drained from the Replayer at each frontier advance; `index`/`sub_agents` maintained via
the incremental `push` folders; `provisional` = the Replayer's resident open window; the Replayer
keeps only the open window (content → O(turn)). `snapshot` = `committed ++ finish_turns(provisional)`.
`InMemoryStore` reproduces today's `Vec<Block>` exactly. Gate: the equivalence oracles, the follow
tests (blocks + user_times + metrics), the `--dump*` byte-identical gate, a new `Session`-equality
test. **This is the floor everything else stands on.**

**Phase B — the `BvLog` + `DeferredBlockStore` (build now/next, core).** Append-only `Bv`/`Meta`
record log + offset table + read-by-offset; `DeferredBlockStore` (`L = Block`); `BlockAccess for
Session<Deferred>`; cold-load = scan → rebuild (via §6). **Refactor `c9f4175`:** remove the
core `TierBSession::persist/load` + the standalone sidecar json — the `BvLog` *is* the
persistence; keep the serde derives. Round-trip test: reload-from-log == parse. In-memory path
untouched (byte-identical gate).

**Phase C — live emit + present layer (defer; this is most of Stage 4 / #31).** `Provisional`
emit vocabulary + `Rewind` records; extract `reconcile` out of `html_export` (delete the
snapshot-diff); `Renderer` trait + `viewport`; **move `SessionCache` core→present**; the
render-record store (`L = JSON`) + stream cached records to the HTML client; windowed TUI as a
`Renderer`.

**Phase D — sidecars + compaction (defer, optional).** Position-tagged `metrics`/`index`
sidecars for O(1) cold-load; provisional-garbage compaction on evict.

Build order rationale: A gives the memory win and unblocks B; B gives durable off-heap content +
restart reuse in the canonical `Block` form; C/D are the presentation efficiency + polish and are
where the app-facing cache and rendering live. A and B touch **only** core and are byte-identical;
C is where new user-visible behavior (live streaming, windowed TUI) lands.

---

## 12. Open decisions to confirm

None block Phase A/B. Flagged for when we reach C/D (my leaning in parens):

1. **Metrics seam** — keep the parallel fold + trailing cumulative `Meta` *(recommended, less
   churn)*, or route metrics through `Emit::Meta::MetricsDelta` so the log is intrinsically
   self-sufficient *(cleaner log, bigger refactor)*.
2. **Durable `L` for an app serving both frontends** — canonical `Block` log + per-frontend
   render caches *(recommended)*, vs a union `enum L` in one log, vs per-frontend logs.
3. **Compaction trigger** — on evict *(recommended)*, periodic, or never (tolerate ~2×).

These are policy/tuning choices the generic mechanism already supports; they don't gate the
core work.
