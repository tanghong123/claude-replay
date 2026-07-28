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
- One session's durable form is a **strictly append-only, never-rewritten** log that can be
  reopened across process restarts and caught up to a growing transcript without a full
  re-fold. Recorded offsets are valid **forever**.
- The HTML live feed becomes **O(delta) render + O(delta) bytes**: render each block once,
  and the render cache *is* the wire record streamed to the client.
- One reusable **presentation layer** with two concrete renderers (TUI, HTML) — no duplicated
  incremental-update logic.

**Non-goals (for now — see §10/§11)**

- Fine-grained structural grouping ops (nest/coalesce with stable ids). We use turn-grain
  `Commit`.
- In-stream metadata checkpoints for mid-stream reconstruction. Dropped — metadata is resident
  by default (§8).
- Persisting the **provisional** (open-turn) blocks, or the TUI's rendered form. Neither is
  written to the log (§5, §10).
- Log compaction. The log is append-only and never rewritten — no garbage collection, no
  compaction, ever (§5).

---

## 2. The three layers

```
claude-replay-core            pure library — mechanism, no policy, no presentation types
  Replayer                    folds Messages → emits Provisional / Commit / Meta / Reset
  SessionAccumulator<S>       ingests Blocks, put: Block→S::Bv per Commit; the RESIDENT unit:
                              committed: Vec<S::Bv> + provisional: Vec<Block> + metadata
  BlockStore                  InMemoryStore (Bv = L) · DeferredBlockStore (Bv = Deferred, L = Block)
  BvLog                       append-only, committed-only record file (Bv / Meta) + read-by-offset
  model · metrics · follow · SessionIndex::push / push_sub_agent (incremental folders)

present layer  (root crate, top-level, beside present/fold/highlight) — what real apps need
  cache                       catalog of all sessions; residency of loaded SessionAccumulators;
                              evict / re-admit; the two residency policies (§7)
  reconcile                   apply Provisional / Commit → append + turn-commit rewind (WIRE-only)
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

Core names **no** presentation types: `DeferredBlockStore` is `L = Block`; a store that holds a
render record is a *presentation* store built on the generic `BvLog`.

---

## 3. The emit model: `Provisional` / `Commit`

The Replayer stays the atomic transform (no store, no BV, no index — so `--dump` can drive it
store-free). It emits:

```rust
enum Emit {
    Provisional(Block),   // a raw block appended to the OPEN window — shown live, not yet grouped
    Commit(Vec<Block>),   // a turn (or pinned run) closed: its FINAL grouped blocks,
                          //   superseding that turn's provisionals
    Meta(MetaRec),        // per commit: user-turn time(s), metrics delta, and the source byte
                          //   offset up to which content is now committed (source_tell)
    Reset,                // whole-file truncation/compaction of the SOURCE — discard all
                          //   (defensive; never fires on real append-only transcripts)
}
```

- **Grouping stays in the Replayer** (`finish_turns`: thinking absorbs the preceding activity
  run; runs coalesce). The emit *conveys the result*: `Commit` carries the already-grouped
  blocks. Consumers never re-derive grouping.
- **The open turn is a transaction.** `Provisional`s accumulate for latency; `Commit` is the
  transaction commit — it supersedes that turn's provisionals with the grouped form and moves
  them past the durability frontier. This is the "rewind" made **semantic and turn-scoped**
  rather than positional (`reset/from N`). The only mutable region is the open window, so a
  `Commit` supersedes O(turn). **This supersession is a wire/present-layer concern — it never
  reaches the `BvLog`** (which stores only committed blocks; §5).
- **The durability frontier** (see `streaming-core-memory.md`) decides *when* a turn commits: a
  turn stays in the open window (un-committed) while pinned by (a) a **pending prompt queue**
  (its `⧗` marker may still be suppressed at dequeue) or (b) an **un-bodied `Skill`** (a
  `SkillBody` may still nest into it). Both pins are implemented (commit `a49467e`). So
  `provisional` = the open turn **plus any pinned earlier turns**; `Commit` fires as the
  frontier advances. The **last turn of a session is always provisional** (no next turn ever
  closes it), which is why cold start always re-parses it (§6).
- **Metrics ride `Meta`.** Per commit the Replayer emits the metrics delta (tokens/cost for the
  just-committed content) as part of `Meta`, so the log is intrinsically self-sufficient for
  metrics — no separate parallel snapshot. (The per-agent metrics *extraction* stays in the L1
  decoder; only its result rides the emit.)

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
    source_tell: u64,                // transcript byte offset where `provisional` begins
    agent / cwd,
}
```

- `committed.len()` is the commit frontier. A renderer shows `committed` + a live view of
  `provisional`, and swaps a turn from provisional→committed on the next `Commit`.
- **Metadata is small and always resident** (index/metrics/sub_agents/user_times +, in deferred
  mode, the committed offset table + `source_tell`). Only the **committed block content** is ever
  deferred off-heap. This is the key simplification (§8).
- **`provisional` is never persisted.** It lives only in memory (O(turn)); on cold start it is
  reconstructed by re-parsing the transcript from `source_tell` (§6).

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
  to a `BvLog` and returns the locator. Core ships this for `L = Block` (canonical). A store that
  appends a *render record* (`L = JSON`) is the same shape built in the present layer over the
  same `BvLog` — core stays presentation-agnostic via the `L: Serialize` bound.

`Deferred` is "a deferral **of BV**": the file holds a serialized copy of `L`, and the handle is
its offset. `Vec<Deferred>` (the committed offset table) *is* metadata — resident, tiny.

### 5.2 The `BvLog` — committed-only, append-only, immutable

One append-only file per session, holding **only committed content**:

```
header  { magic, version }
records (framed <len:u32><tag:1B><payload>):
  Bv    <serialized L>              a COMMITTED block value (never a provisional block)
  Meta  <turn time | metrics Δ |    per-commit metadata, incl. source_tell (the transcript
         source_tell>                 offset up to which the log is committed)
```

Invariants — these are the whole point:

- **Committed-only.** Provisional (open-turn) blocks are **never written**. They are transient,
  re-derivable from the transcript, and would only add churn.
- **Strictly append-only, never rewritten.** No `Rewind` records, no truncation, no compaction —
  ever. A committed `Bv`'s offset is stable for the life of the file, so any `Deferred` handed
  out (to a resident session, a client, a sidecar) is valid forever.
- **No garbage.** Because only committed blocks are written and they never change, the file is
  ~1× the committed content — nothing to reclaim.
- The turn-commit **rewind of the open turn is a wire/present-layer concept** (§9), not a log
  record. `Reset` denotes a *source* truncation (the transcript shrank) → the follower discards
  and rebuilds; it never rewrites the log in place.

---

## 6. Reading a stored session

- **`BlockAccess for Session<Deferred>`**: read a committed block = seek to `Deferred.offset`,
  read `size`, deserialize `L`. Optional small LRU.
- **Random access for streaming** (§7 point 5): the committed **offset table** (`Vec<Deferred>`,
  resident) gives O(1) `committed[N]` → seek + read.
- **Cold load** (fresh process, nothing resident): read the `BvLog` — `Bv` records rebuild the
  committed offset table + fold `index`/`sub_agents` via the incremental `push` folders; `Meta`
  records give `user_times`/`metrics` + the last `source_tell`. Then **re-parse the open turn**:
  `open_at(source_tell)` on the transcript and fold the tail (the provisional turn + any new
  appends). Cost = one committed-log pass (or O(1) via a sidecar, §8) **plus** re-folding the
  last turn's few lines — the only overhead of not persisting provisionals, and a deliberate
  trade for a never-rewritten log.

---

## 7. The presentation cache

Owns *policy*; holds a collection of resident `SessionAccumulator`s.

1. **Catalog** — every session ever registered → `{ id → transcript locator }`. Content-free;
   unbounded is fine.
2. **Load** — fold the transcript (or replay the `BvLog` + re-fold the tail on cold start) →
   build the session object + append committed records to the `BvLog`. Two residency policies:
   - **materialized (default):** `committed: Vec<Block>` resident.
   - **deferred (opt-in, cache policy):** `committed: Vec<Deferred>` (offset table into the
     `BvLog`) + `source_tell`; committed content stays on the `BvLog`, read on demand. Metadata
     and the O(turn) provisional stay resident either way.
3. **Evict** (memory pressure / policy) — drop the resident committed *content*; keep the
   `BvLog` on disk and (by default) keep the small session object (metadata + offset table +
   O(turn) provisional/fold-state) resident.
4. **Re-admit** — re-materialize committed content from the `BvLog`; if the transcript grew,
   `open_at(source_tell)` and delta-fold the tail (appending new committed records as turns
   close). No re-parse of committed content, no metadata reconstruction (it was never dropped).
5. **Async streaming** — a client requests committed blocks from index `N`: the offset table
   seeks and streams `Bv[N..]` render records, then live records as turns commit; the **open
   turn is streamed ephemerally** (rendered from the resident `provisional`, with wire-level
   `reset/from` for its rewind — §9), never from the log. This is the deferred policy's raison
   d'être.

**Each mode caches its own `L`** (per decision #2): a TUI-mode `claude-replay` caches `L = Block`;
an HTML-mode one caches `L = JSON render record`. A single binary never serves both today, so
there is no shared-`L` question to answer yet (see §10 for the future union options).

---

## 8. Metadata: resident by default; sidecars are position-tagged and optional

The decisive simplification: **metadata is small and, once a session is loaded, the session
object stays resident.** So nothing ever needs to reconstruct metadata *at a middle index* —
`Ckpt`-in-stream is dropped. The two places it seemed needed both dissolve: streaming from `N`
needs only the resident **offset table**; re-admit keeps the resident session object.

The **only** rebuild is a genuine **cold start**, handled by the one-time committed-log read +
open-turn re-parse (§6). If that read's cost ever matters, add an **optional, disposable,
position-tagged sidecar** — never a bare snapshot (a staleness trap: log grows, sidecar doesn't,
two disagreeing truths):

```
sidecar = { state, valid_up_to: <BvLog offset at a commit boundary> }
load: log_len == valid_up_to → use as-is (O(1))
      log_len >  valid_up_to → replay log[valid_up_to..] through the incremental folders (self-correcting)
      log_len <  valid_up_to → impossible (the log is never rewritten) → treat as corrupt, full rebuild
```

Consistent by construction (the offset *is* the contract; and since the log is append-only the
`<` case cannot arise from our own writes), disposable (drop → re-read), and it is the checkpoint
benefit **out-of-band** — no log pollution, no two-writer consistency surface. `metrics` and
`index` sidecars are two instances of this shape. **Deferred** (§11 Phase D) — the default is
read-once-then-resident, with **zero** sidecar.

Metrics need no special case here: they ride `Meta` deltas in the log (decision #1), so a
no-sidecar cold read reconstructs them by summing while it scans.

---

## 9. The present layer and the O(delta) live feed

- **`reconcile`** replaces `html_export::serve::stream_delta`. Instead of *diffing snapshots*, it
  **applies the emit records**: `Provisional` → append a live row; `Commit` → swap that turn's
  rows for the grouped ones (the turn-scoped rewind). This rewind is **wire-only** — the client
  gets `reset/from N` for the open turn; the durable `BvLog` never sees it. Written once,
  consumed by both the HTML JSONL feed and a windowed TUI's "which rows changed" update.
- **`viewport`** holds the block window + per-block line-height index + a bounded render cache,
  generic over `Renderer::Rendered`.
- **`Renderer`** turns a `Block` into the frontend's rendered form once; `viewport` caches it.

The efficiency win, precisely: for HTML, `L = the JSON render record`. `put(Block)` renders once
→ the cache entry **is** the wire record. The server flips from *pull-and-re-render every poll*
to *push-once*: a durable block commits → render once → append the serialized record → the
client just inserts it. Only the open turn re-renders (O(turn)); committed turns render exactly
once. The TUI gets the identical shape with `Rendered = Vec<Line<'static>>`, cached in the same
`viewport`.

---

## 10. Layering consequences worth naming

Inherent to the module boundaries; documented so they are choices, not surprises.

1. **The TUI cannot persist its rendered form.** `ratatui::Line<'static>` isn't `Serialize`, so a
   `DeferredStore<L = TUI-rows>` is impossible. The TUI uses `L = Block` (deferred) + an
   **in-memory** render cache; only HTML (`L = JSON record`) persists rendered records. Fine: the
   TUI is interactive/single-session; the HTML server serves many and benefits from persisted,
   streamable records.
2. **A render-store's `L` is frontend-specific — but that's a non-issue today.** A `claude-replay`
   process serves TUI *or* HTML, so each mode owns its own cache/`L` (TUI→`Block`, HTML→JSON) and
   there is nothing to reconcile. **Parked** for a hypothetical single binary serving both, where
   avoiding 2× memory needs a union `BV`; candidate shapes (undecided): `{Deferred<Block>, Json}`,
   `{Block, Deferred<Json>}`, or `{Deferred<Block>, Deferred<Json>}` (viable if TUI search — the
   only reader of raw `Block` — is rare enough to page). Decide only if/when that binary exists.
3. **Rendering happens in the store's `put`, which is presentation.** So a render store lives in
   the present layer and drives the *core* accumulator with itself; the fold logic isn't
   duplicated (only the store differs). Consequence: a render store renders **eagerly** (per
   `Commit`) — right for the streaming server; the TUI wants **lazy** (render the visible window)
   and uses a Block store + `viewport` render cache. Both first-class.
4. **Cold start re-parses the open turn.** The price of never persisting provisionals (§6): a
   fresh process must re-fold the last turn's few transcript lines. Cheap, bounded (O(turn)), and
   the deliberate trade for a never-rewritten, offset-stable log.

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

**Phase B — the `BvLog` + `DeferredBlockStore` (build now/next, core).** Append-only, committed-only
`Bv`/`Meta` log + offset table + read-by-offset; `Meta` carries turn time + metrics delta +
`source_tell`; `DeferredBlockStore` (`L = Block`); `BlockAccess for Session<Deferred>`; cold-load =
read committed + re-parse the open turn from `source_tell` (§6). Route **metrics through
`Emit::Meta`** (decision #1) so the log is self-sufficient. **Refactor `c9f4175`:** remove the core
`TierBSession::persist/load` + the standalone sidecar json — the `BvLog` *is* the persistence; keep
the serde derives. Round-trip test: reload-from-log + tail-reparse == parse. In-memory path
untouched (byte-identical gate).

**Phase C — live emit + present layer (defer; this is most of Stage 4 / #31).** `Provisional` emit
vocabulary; extract `reconcile` out of `html_export` (delete the snapshot-diff; the open-turn
rewind becomes wire-only); `Renderer` trait + `viewport`; **move `SessionCache` core→present**; the
render-record store (`L = JSON`) + stream cached records to the HTML client; windowed TUI as a
`Renderer`.

**Phase D — sidecars (defer, optional).** Position-tagged `metrics`/`index` sidecars for O(1)
cold-load. (No compaction phase — the log is never rewritten.)

Build order rationale: A gives the memory win and unblocks B; B gives durable off-heap content +
restart reuse in the canonical `Block` form; C/D are the presentation efficiency + polish and are
where the app-facing cache and rendering live. A and B touch **only** core and are byte-identical;
C is where new user-visible behavior (live streaming, windowed TUI) lands.

---

## 12. Decisions — resolved

1. **Metrics seam** — **route through `Emit::Meta::MetricsDelta`** so the `BvLog` is intrinsically
   self-sufficient. (Extraction stays per-agent in L1; only the delta rides the emit.)
2. **Durable `L` when one binary serves both frontends** — **non-issue today** (each mode caches
   its own `L`). Parked; if it ever arises, pick a union `BV` from the candidates in §10.2.
3. **Provisional persistence & compaction** — **do not persist provisional blocks; never compact.**
   The `BvLog` is committed-only, append-only, and immutable; cold start re-parses the open turn
   from `source_tell`. Offsets are stable forever.
