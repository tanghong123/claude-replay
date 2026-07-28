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
claude-replay-core            pure library — mechanism ONLY: the generic seam, no storage, no cache
  Replayer                    folds Messages → emits Provisional / Commit / Meta / Reset
  SessionAccumulator<S>       ingests Blocks, put: Block→S::Bv per Commit; the RESIDENT unit:
                              committed: Vec<S::Bv> + provisional: Vec<Block> + metadata
  BlockStore (trait) + get    the store SEAM + InMemoryStore (Bv = Block). NO tier-b type in core.
  BlockAccess (trait)         content access over a Session<BV>
  model · metrics · follow · SessionIndex::push / push_sub_agent (incremental folders)

present layer  (root crate, top-level, beside present/fold/highlight) — the DATA LAYER + rendering
  SessionCache                THE data layer under BOTH frontends: catalog of all sessions,
                              residency of loaded SessionAccumulators, evict / re-admit, and it
                              OWNS tier-b — its own DeferredStore (impl BlockStore) + Deferred BV +
                              the committed-only append-only BvLog file. tier-b is intrinsic to the
                              cache, not a standalone core store.
  reconcile                   apply Provisional / Commit → append + turn-commit rewind (WIRE-only)
  viewport                    block window + per-block line-height index + render cache;
                              L may be a render record / union
  trait Renderer { type Rendered; fn render(&Block,width)->Rendered; fn height(&Rendered)->u16 }

tui/  ·  html_export/          Renderer impls; BOTH obtain sessions from the SessionCache; the html
                              server streams the cache's render records
```

**tier-b lives in the cache, not core.** Core exposes only the `BlockStore` *trait* (+ `InMemoryStore`)
so a session can be produced over any storage policy; the concrete on-disk store (`DeferredStore`,
the `Deferred` locator, the `BvLog` record file) is an implementation detail of the `SessionCache`
in the present layer. The `TierBStore`/persist-load currently sitting in `claude-replay-core`
(`7c0202c`, `c9f4175`) are a scaffolding step — they **move into the cache** when the present layer
is built (§11 Phase C). The cache is the single data layer both the TUI and the HTML server sit on
(today only the HTML server uses it; the TUI parses directly — Phase C unifies them).

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

## 9a. Concurrency: one shared session, lock-free committed reads, a serializable pull cursor

The cache **owns no threads**. Multiple clients share **one session per id** (an `Arc`, never a
copy); each keeps its **own read cursor**; a client that reads past the end **borrows its own
thread** to advance. This is the two-level model — the cache holds the tailer state, the client
drives the tail on demand.

### SharedSession

```
SharedSession {
  committed:  append-only log (BvLog / immutable prefix), byte-offset addressable, length M
              published release-atomic — reads are LOCK-FREE (content at [..M] never moves)
  tail: Mutex<{ provisional{ after, blocks }, follower, source_tell }>   // advance + open turn
  epoch: u64  // bumped on a truncation/Reset; invalidates outstanding cursors
}
```

- The **committed volume needs no lock**: append-only ⇒ existing offsets never move, so N clients
  read `committed[..M]` concurrently; only `M` is an atomic (release on append, acquire on read).
- The **only** locked region is the `tail`: a client whose cursor hits the end takes it, advances
  the transcript + folds on **its own thread** (append committed + update `provisional`), releases,
  reads on. No background thread; readers of committed are never blocked by a tail.

### The commit-transition race → `provisional.after` (lock-free versioning)

A commit mints the new committed block(s) while the provisional set that *became* them may not be
cleared yet — a reader could momentarily see a block twice. Fix: tag the provisional with
**`after` = the committed length it follows** (a seqlock over the append-only log):

- Writer: while the open turn is live, `provisional.after == M`; on commit, append the grouped
  block(s) (`M → M'`), then set `provisional.after = M'`.
- Reader: load `M` (acquire), read `committed[..M]`, load `provisional`; the snapshot is
  `committed[..M] ++ provisional` **iff `provisional.after == M`**; else a commit raced → discard
  the provisional, serve `committed[..M]` only, retry. Every inconsistent interleaving is detectable
  by `after != M` (both directions), so the transition needs **no lock**.

### The serializable pull cursor

```
Cursor( committed_id, provisional_gen )    // + epoch (session validity) — see below
```

- **`committed_id`** — monotonic, **rewind-free, durable**: committed is append-only, so this only
  advances and its target never changes. The serializable anchor — safe to hand a remote process or
  keep across a restart. Representation: a committed **`BvLog` byte offset** (a remote seeks the log
  directly — no shared state, no offset table needed).
- **`provisional_gen`** — a **version of the open turn**, not a block index. *Correction from the
  first sketch* (`provisional_index`): building it surfaced that the open turn is **not append-only**
  — a tool block in the current turn **back-patches** (its output fills in when the `tool_result`
  arrives) **without adding a block**. A raw index (a count) can't see that same-length content
  change, so a client would show a stale, output-less tool call. `provisional_gen` bumps on **any**
  open-turn change (append, back-patch, or regroup); a change ⇒ the server replaces the provisional.
  Still two positions + an epoch — only the second position's *meaning* changed. (Committed stays a
  true index precisely because it's append-only.)
- **`epoch`** — a session validity token the client carries and the server checks each pull; a
  mismatch (truncation/Reset) ⇒ re-sync from 0. Conceptually the "third field" beside the two
  positions; it rides in the serialized cursor so a remote is self-validating.

### The pull protocol

Given `Cursor(committed_id, provisional_gen)` + its `epoch`, against live `(M, provisional, gen,
epoch')`:

- **`epoch != epoch'`** → **`Resync`**: serve `committed[0..M]` + `provisional`, render fresh.
- **`M > committed_id`** (a commit happened — possibly several turns/blocks) → **`Update`**: serve
  `committed[committed_id..M]` **and replace** the provisional with the current one; the client
  discards its old provisional (it *became* that committed range). Committed progress takes priority.
- **`M == committed_id` and `gen != provisional_gen`** (open turn changed, no commit — append,
  back-patch, or regroup) → **`Update`** with an empty committed range + the replaced provisional.
- else → **`Idle`**.
- Return `Cursor(M, gen)`.

The provisional is **replaced whole** on any change (it's O(turn) and mutable via back-patch), not
served incrementally — so the client's render is "append committed rows, replace the provisional
tail." (An incremental-provisional-append optimization would need a proven append-only sub-window of
the open turn; deferred.)

The client's render maps 1:1 to the two cases — **append committed rows** (permanent) or **replace
the provisional tail** (transient). That is exactly the `append` / `reset-from` the HTML feed does
today by server-side snapshot-diff (§9), now a principled **pull** with a serializable, remote-
friendly cursor and no server-held per-client diff baseline.

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

**Core is essentially DONE.** The memory-model floor is landed, byte-identical and gated:
the durability frontier + O(turn) Replayer window (`a49467e`, C1); **C2 — the
`SessionAccumulator` owns `committed`, Replayer → O(turn)** (`b282abe`), with `BlockStore::get`
the read-back seam; incremental `SessionIndex::push` / `push_sub_agent` (`43246aa`, `9caebc6`);
serde on the `Block` model + `Agent`/`Metrics`/`SubAgentMeta`. What remains in core is small
(the `Provisional` emit split, `Session { committed, provisional }`) and folds into Phase C.

The `TierBStore`/`Deferred`/persist-load in `claude-replay-core` (`7c0202c`, `c9f4175`) are
**scaffolding to relocate**: tier-b is intrinsic to the cache (§2), so it **moves into the
present-layer `SessionCache` in Phase C**, not polished as a standalone core store. There is no
separate "core `BvLog`" phase — the `BvLog` is built as the cache's storage.

**Phase C — the `SessionCache` as the universal data layer + present layer (the bulk of the
remaining work).** In dependency order:
1. **`Session { committed, provisional }` split** (core) — **DONE** (`b8d5f6f`), byte-identical, with
   a `blocks()` compatibility view. The accumulator's `snapshot` no longer puts the open tail through
   the store, so any store is now committed-only. + `FollowParser::poll_session` returns a fully-
   assembled session (`ead0560`), so the cache needs no core internals.
2. **`Provisional`/`Commit` emit vocabulary** (core) — the Replayer emits the open turn for live
   latency; `Commit` supersedes it. Durable path = `Commit` only (== today's committed drain). Add
   `provisional.after` (the committed length it follows — §9a) at this step.
3. **Move `SessionCache` core→present and give it tier-b** — (3a) **DONE** (`17d6737`): the cache is
   in the present layer, uses only public core API. (3b remaining) fold `TierBStore`/`Deferred`/
   persist-load into it as its `DeferredStore` + committed-only append-only `BvLog` (`Bv`/`Meta`
   records, `source_tell`, cold-load = read committed + re-parse the open turn); route metrics
   through `Emit::Meta`; remove the standalone core sidecar json.
4. **`SharedSession` + `reconcile` + `viewport` + `Renderer`** (present) — the `Arc<SharedSession>`
   (lock-free committed reads + tail `Mutex` + `epoch`, §9a); the serializable `Cursor(committed_id,
   provisional_gen)` pull protocol; extract `reconcile` out of `html_export` (delete the snapshot-
   diff; the open-turn rewind becomes the wire form of the cursor's discard); the windowed block/line
   model + render cache.
5. **Both frontends on the cache** — the HTML server holds an `Arc<SharedSession>` + a `Cursor` per
   client and streams via the pull protocol (`L = JSON` render records); the TUI obtains sessions from
   the cache (today it parses directly) and renders lazily (`L = Block` + in-memory render cache).
   Windowed TUI as a `Renderer`.

**Phase D — sidecars (defer, optional).** Position-tagged `metrics`/`index` sidecars for O(1)
cold-load. (No compaction — the log is never rewritten.)

Build order rationale: core's floor (C1+C2) is done and byte-identical. Phase C is where the
value lands — one `SessionCache` data layer, owning tier-b, under both frontends — and where the
new user-visible behavior (live streaming, windowed TUI) appears. It is a mostly-presentation
refactor with real byte-identical risk, so it proceeds as small gated increments in the order
above (each keeping the `--dump*`/oracle/follow gates green).

---

## 12. Decisions — resolved

1. **Metrics seam** — **route through `Emit::Meta::MetricsDelta`** so the `BvLog` is intrinsically
   self-sufficient. (Extraction stays per-agent in L1; only the delta rides the emit.)
2. **Durable `L` when one binary serves both frontends** — **non-issue today** (each mode caches
   its own `L`). Parked; if it ever arises, pick a union `BV` from the candidates in §10.2.
3. **Provisional persistence & compaction** — **do not persist provisional blocks; never compact.**
   The `BvLog` is committed-only, append-only, and immutable; cold start re-parses the open turn
   from `source_tell`. Offsets are stable forever.
