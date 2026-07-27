# Design: streaming core memory — O(N) indices allowed, O(N) content is a consumer policy

> **Status:** proposed (foundational). This is the memory model the codebase should converge on;
> it reframes task **#11** from "shave the footprint" into "make the core's *content* residency a
> policy the consumer picks." Builds on `SessionBuilder` (#19), `LineReader` reset (#18), the
> `Transcript` locator (#23/#27), and the `SessionCache` tiers (#21 / `session-cache.md`).
> Each stage is independently gated on **byte-identical** output.

## The invariant

- **Core MAY retain O(N) indices/metadata** — per-block offsets, turn boundaries, tool/agent/
  attachment entries, per-agent status, counts, metrics. These are small, bounded per entry, and
  are what make random access possible. Linear-in-N is fine here.
- **Core MUST NOT force O(N) *content* resident** — block text (prose, tool output, diffs,
  thinking), rendered styled lines, attachment bytes. Whether to hold the content is a **downstream
  policy** (in-memory / on-disk / index-into-presentation), never a core mandate.

Everything below follows from separating **index (cheap, O(N), resident)** from **content (paged,
policy-driven)**.

## Why the core's *content* working set is O(turn), not O(N): the durability frontier

A block the fold has produced can still be mutated by exactly four mechanisms. Three are
turn-local or become index updates; only content within the current turn must stay resident.

| Mutation | What it changes | Frozen once… | Handled by |
|---|---|---|---|
| **Back-patch** (`tool_result`→`tool_use`) | fills `output`/`patch` | its result arrives (post-#24, results follow uses, ~adjacent) | held in the **turn window** |
| **Grouping** (`finish_turns`) | thinking absorbs the following activity run; runs coalesce | the turn closes (next user turn) | finalized per-turn, then emitted |
| **Queue-marker suppress** (dequeue → drop a `⧗ queued:` marker) | removes an immediately-picked-up marker | content advances past the marker's enqueue (`content_seq > content_at_enqueue`) or it's dequeued | held while **suppressible** (see below) |
| **Reset** (file shrink) | invalidates everything | — | a **`Reset` event** → consumer discards + re-emit; **defensive only, see below** |

Back-patch + grouping are turn-local ⇒ the fold holds only the **current turn**'s content; when the
turn closes it finalizes (group + resolve joins) and **emits the turn's blocks, then drops them**.
So the Replayer's resident **content** is **O(turn)**, while it maintains the O(N) **index** as it
goes. There is **no async block *mutation*** — but see the queue-marker caveat next.

### The queue-marker suppress pins the frontier (the one cross-turn *drop*)

A prose `⧗ queued:` marker is dropped iff, at its dequeue, `content_seq == content_at_enqueue` (the
prompt was picked up with no agent work in between). `content_seq` advances only on assistant /
thinking / tool content — **not** on a `UserText`/`Command` turn boundary. So in a rapid type-ahead
case (enqueue → an unanswered user turn → immediate dequeue) the suppress can target a marker that
sits **behind** a turn boundary. Today one global `apply_suppress` over the single `out` buffer
handles this for free; a naive per-turn buffer-split would make that suppress index **stale** (the
marker already finalized + dropped) — the one thing that breaks byte-identicality under emit-and-drop.

**Frontier rule (reference-stable):** a closed turn is durable/emittable only once it holds **no
still-suppressible marker** — i.e. every pending queue item whose `marker_idx` lands in that turn has
either been dequeued or seen `content_seq` advance past its `content_at_enqueue`. Since content
advances every assistant turn and type-ahead depth is tiny, markers unpin almost immediately, so the
resident window stays **O(turn + pending type-ahead) ≈ O(turn)**. This keeps every suppress target
resident when its suppress fires, so per-turn finalize stays byte-identical to the global pass.

**Reset does not occur in real transcripts — they are append-only.** `/compact` *appends* a
summary line (`isCompactSummary`) and the session keeps appending (verified: summaries sit
mid-file with content after); resume/fork *append* (the libgen dup-id case). Nothing rewrites the
consumed prefix, so the `LineReader`'s `len < offset` shrink-guard never fires on real data — it's
defensive, like the retired pre-scan. **Consequence:** a block past the turn window is durable
**permanently** (the prefix never changes), so emit-and-drop content is never re-requested. The
`Reset` path is kept only as cheap defense (the follower already rebuilds on a detected shrink),
not as part of the real durability frontier — which is therefore **purely the turn window**.

### Sub-agents are two durable events, not a back-patch

A spawned sub-agent is **two independently-durable events**: the **spawn** (`Block::SubAgent`,
durable at spawn-time with `Running`/`AsyncLaunched`) and the **finish** (`Block::AgentDone`,
durable when its notification arrives, carrying the terminal status/result). **Neither block is
ever mutated.** The agent's *current* status is **derived by the resident index** (`sub_agents`
map, #20) from the two events — the finish supersedes the spawn.

The current back-patch exists only because the spawn block was **reused as a mutable state-slot**.
But its *display* is status-independent — the spawn always renders "launched", the finish always
renders its terminal verb — so removing the back-patch changes no rendered text. The only
status-derived *output* is a state read-out (HTML `terminal:` flag), which moves to the
index. This is **un-conflating state from display**: display blocks are immutable and durable;
state lives in the index.

This replaces the current back-patches in `apply_completions_and_suppress`, which today mutate
held blocks cross-turn:
- `sa.status = st` (spawn ← completion) → **drop**; the index derives terminal from the finish
  event. Two read-sites move to the index: `html_export` `terminal: sa.status.is_terminal()`
  (rendered output — the byte-identical-relevant one) and `view.rs` `active_agent_indices`
  (`!status.is_terminal()`).
- `AgentDone.agent_type`/`agent_id` (finish ← spawn) → **resolve from the index** at render
  (the map already holds `agent_type`).

So the sub-agent lifecycle needs no held-mutable state: two durable blocks + an index-derived
status. (This also completes #20's deferred field-lifting for the status/type fields.)

## The Replayer becomes a streaming emitter

The `Replayer` ingests lines and **emits an iterator** of durable events instead of accumulating a
`Vec<Block>`:

```rust
enum Emit {
    Block(Block),                       // a durable, finalized block (past the turn frontier)
    Meta(Meta),
}
enum Meta {
    TurnBoundary { at: BlockIndex, time: Option<EpochSeconds> },
    MetricsDelta(/* running totals */),
    Reset,                                                  // (defensive; never fires on real data)
}
```

The `Replayer` stays **atomic** — a pure transform, no storage/`BV`/index. A **separate**
`SessionAccumulator` (the stateful layer around it) pulls this iterator and folds it into a
`Session<BV>`: each `Block` → the store's `put(block, at) -> BV`; each `Meta` updates the **BV-free
index/metrics**. The **index is maintained by the accumulator** — one entry per block (kind,
position, turn; and for the on-disk BV, the byte offset returned by `put`). The sub-agent **status
is derived here from the two durable block events** (a `SubAgent` block sets running/async; a later
`AgentDone` block supersedes it to terminal) — no meta-event and no block back-patch. The index is
the resident backbone in every mode; only *content* (the `Vec<BV>`) varies. (Keeping `Replayer` and
`SessionAccumulator` **distinct** is deliberate: `Replayer` is a reusable transform — `--dump`
drives its emits → render → drop with *no* accumulator — and the accumulator is where the storage
policy + index live.)

## `Session<BV>` — the per-block storage is a type parameter

The sink policy is a **generic**: `Session<BV>` (default `BV = Block`, so existing `Session` =
`Session<Block>` with no churn) is parameterized over the **block value** it stores. The **one-shot
batch parse** takes a **mapping closure** `FnMut(Block, BlockIndex) -> BV`; the **persistent
`SessionAccumulator<S: BlockStore>`** uses the store's `put`. Either pulls `Emit` events from the
`Replayer` (the atomic transform), maps each `Block`, and updates the BV-free index/metrics per
meta. Monomorphized, zero-dispatch.

```rust
fn build<BV>(events: impl Iterator<Item = Emit>, map: impl FnMut(Block, BlockIndex) -> BV) -> Session<BV>
struct Session<BV> { index: SessionIndex /* BV-free */, blocks: Vec<BV>, metrics: Metrics, /* sub_agents … */ }
```

| `BV` | `map` closure | `Vec<BV>` holds | For |
|---|---|---|---|
| `Block` | `\|b,_\| b` | content in RAM | small transcripts; today's default |
| `Deferred { offset, size }` | `\|b,at\| store.append(at,&b)` | **the offset table itself** (O(N) tiny — it *is* the tier-b index) | huge transcripts; restart-survival; eviction |
| `Presentation` | `\|b,_\| render(&b)` | the view's rendered rep | eager presentation |
| **`RenderedRef { block: Deferred, rendered: Option<CachedLines> }`** | writes to tier-b, returns `{block, None}` | locator (+ maybe height); styled lines materialized on demand (bounded LRU), re-derivable via `block` | **the windowed TUI** — cheap resident, render-on-focus, block re-readable for fold/descend |
| `()` | `\|_,_\| ()` | zero-sized — nothing | pass-through (`--dump`) |

`BV` spans **eager → lazy** and the closure picks the point; `RenderedRef` *composes* `Deferred`
(the raw block) with a lazy render cache. Rules that keep the generic from metastasizing:
- the **`SessionIndex`** (turns/agents/metrics/`sub_agents`) is **BV-free** — most code works on
  `&SessionIndex` and never names `BV`;
- content access is gated behind `trait BlockAccess { fn block(&self, i: BlockIndex) -> Cow<Block>; }`
  — trivial for `Block`, a disk read for `Deferred`/`RenderedRef`, unavailable for `()` (so "a
  pass-through session has no blocks" holds at the type level);
- a `SessionCache` is typed to **one** BV policy — and that's correct, not a limit: the cache is a
  **client-side convenience** that lives next to the presentation and caches the BV most efficient
  for it (the core stays policy-free; the client picks BV + owns the cache). If a client genuinely
  needs two coexisting representations, use a **union `enum BV { Raw(Block), Rendered(RenderedRef),
  … }`** with `BlockAccess` matching on the variant — one cache, one shared index, a flat `Vec<BV>`
  with `match` dispatch (better than two caches, which duplicate the index, and than `Box<dyn>`,
  which boxes every block + adds a vtable). Often unneeded: a single BV with `Option`s (e.g.
  `RenderedRef { rendered: Option<…> }` for cold/hot) already covers what looks like two policies.

In every instantiation the **session metadata** (index, `sub_agents`, metrics) stays resident and
O(N) — explicitly fine.

**Two forms, two idioms** (Rust-honest): the one-shot batch parse takes a **closure**
`map: FnMut(Block, BlockIndex) -> BV` (concise, idiomatic for a throwaway transform). The
**persistent, cache-held** object — fed over time, `Session<BV>` referenceable at any moment —
should instead use a **trait** `BlockStore { type Bv; fn put(&mut self, Block, BlockIndex) ->
Self::Bv; }`, because a struct that *stores* a closure has an unnameable type (hard to name,
return, or cache without `Box<dyn>`), whereas a trait yields nameable types like
`SessionAccumulator<InMemoryStore>` you can hold in the cache. And it's an **accumulator / fold**
(`advance(events)` + `snapshot()`/`session()`), **not** Rust's one-shot "Builder" idiom
(configure-then-`.build()`-once) — the `advance`/`snapshot` shape today's `SessionBuilder` already
has is the right one; only the *name* leans the wrong way.

## Presentation is its own policy

The presentation layer independently chooses full-in-memory (small sessions), on-disk rendered
cache (the serve `<id>.jsonl` / a TUI render cache), or **windowed** (hold only the visible range of
blocks + rendered lines, page the rest from tier-b via the resident index's offsets; re-render on
scroll). Search may build its own O(N) content index (allowed) or scan content from disk.

## Optimal resident memory per use case

| Use case | Random access? | Optimal resident | vs today |
|---|---|---|---|
| `--dump` / `--dump-html` (pipe/file) | no | **O(turn) content** (emit→render→write→drop); index optional | O(N) → sublinear |
| `--dump-all-html` (tree) | no | **O(largest node's turn)** + lazy child streaming | O(Σ tree) → sublinear |
| `parse_session` as a value | consumer's call | **O(N) index** + {O(N) content in-mem \| paged on-disk} | forced O(N) content → choice |
| **TUI viewer** (scroll/fold/search) | yes | **O(N) index + O(window) content + O(window) render** | O(N) blocks + 2–3× render → windowed |
| **HTML live serve** | no (server) | **O(turn) content** server-side (emit→render→append→drop); browser holds the presentation | server holds full Session → O(turn) |
| `FollowParser` / live tail | no | **O(turn)** fold window; emits deltas | already close |

## Worked example: TUI residency (layered)

For the interactive TUI we deliberately **keep the current session's content in memory** (fast
search/scroll — no windowing; "don't over-optimize" a single opened session). The layers:

1. **First-order — blocks resident, styled lines windowed (matches the ratatui idiom).** Research
   of the ratatui ecosystem settles the layer question: apps are **immediate-mode/viewport-only**
   — the frame `Buffer` is viewport-sized (and capped at `u16::MAX` cells, so the whole scrollback
   *can't* be one buffer), scrollable widgets **window** the content, and the large-log crates
   cache line **heights**, not styled lines, rendering only the visible `k` (~O(k/n)). So:
   - **Resident O(N) = `blocks`** — the compact structured content, and the thing fold/descend/
     attachment/re-render all need. (Not rendered text: for a *log viewer* "lines of text" is the
     model, but here rendered text is a *lossy derived* form that would still need blocks for
     styling/fold/descend — a second O(N) rep, strictly worse than just holding blocks.)
   - **+ a per-block display-height index** (fold-aware, width-keyed, O(#blocks) prefix-sum) for
     scroll math — the ecosystem's "cache heights." A **resize** recomputes it by *measuring* each
     block's wrapped height (string-wrap + count rows — **no markdown/syntect/`Span` build**, so
     O(N) but *cheap*, tens of ms on the few-MB retained text), then re-renders only the visible
     window. Scroll is anchored to `(block, offset)` (content), not an absolute row, so heights can
     change under it without a jump. Scroll/typing don't touch heights; only resize (all) and fold
     (one block) do. (If ever a hitch on an enormous transcript: measure lazily/amortized with an
     estimated scrollbar — a later optimization.)
   - **Render only the visible window each frame**, with a **bounded LRU cache of rendered blocks**
     (visible + margin) so syntect highlighting isn't redone every frame → styled `Line<Span>` is
     **O(window)**, never O(N).
   - **Search over block text** (scan the `String`s — fast in RAM).

   The concrete win: **drop the three O(N) styled caches (`raw`+`wrapped`+`body_cache`) — the
   measured 12× — and render the window on the fly.** Blocks stay resident; no O(N) styled-line
   cache and no separate O(N) text buffer. Paging blocks to tier-b only matters for transcripts
   that exceed RAM (a later concern), not the common TUI case.

   Refs: ratatui [rendering under the hood](https://ratatui.rs/concepts/rendering/under-the-hood/),
   [scrollable-widgets RFC #1924](https://github.com/ratatui/ratatui/discussions/1924),
   [ratatui_widget_scrolling](https://lib.rs/crates/ratatui_widget_scrolling),
   [paragraph-perf #1880](https://github.com/ratatui/ratatui/discussions/1880).
2. **Second-order — attachments (done, #23):** each view holds `Deferred{at}` locators; a download
   loads via that view's `Transcript`, one at a time, dropped after. No resident blobs.
3. **Second-order — sub-agents (on-demand + cached):** the root parses **flat**; a descend goes
   through `SessionCache.poll(agent_id)` — **parse flat on first descend, tail on re-descend** (a
   running child grows), evicted by a **residency budget** (root + X most-recent). `subtree_cost`
   becomes lazy/incremental (summed as children are descended) rather than forcing eager
   enrichment. Today `build_child_frame` uses `parse_enriched()` (eager whole-subtree) and no
   cache — this is the TUI adopting the `SessionCache` (#21) the HTML server already uses.

Net: **O(root + X resident children)** content + **O(N) index**; attachments and non-resident
children are locators / on-disk. Never O(whole tree).

## Staging (each byte-identical-gated)

1. **Sink seam (zero behavior/footprint change).** Give `SessionBuilder` a `Sink`; the default is
   the in-memory sink that reconstructs today's `Vec<Block>`. Pure refactor — the equivalence
   oracle + `verify.sh` prove identical output. Establishes the interface.
2. **Un-conflate state from display — no cross-turn block *mutation* (LANDED, byte-identical).**
   The sub-agent status back-patch is gone: the spawn/finish are two immutable durable events and the
   terminal status is derived by the `sub_agents` index (`build_sub_agents`). `AgentDone` resolves its
   spawn's id/type at emit-time from a running `agent_ids` map (O(#agents)) rather than a finalize
   scan over — soon-dropped — spawn blocks; `apply_completions_and_suppress` shrank to a pure
   index-keyed `apply_suppress`. This removes every cross-turn *mutation*, leaving only the
   queue-marker *drop* (above). Proven via the equivalence oracle (`replay(tokenize) == parse_main`,
   the unchanged reference) + end-to-end `--dump`/`--dump-html`/`--dump-all-html` diffs.
   *Remaining plumbing (emit-and-drop of finalized turns) is folded into stage 3* — see the note there:
   per-turn finalize is proven turn-local (`finish_turns(⊕turns) == ⊕finish_turns(turn_i)`, since a
   `UserText`/`Command` boundary breaks both grouping passes), but the *buffer-split that actually
   drops content* has zero footprint payoff until a **dropping** sink exists (the in-memory sink here
   re-accumulates), and its one hazard — the queue-marker frontier pin — is cleanest to build against
   tier-b's concrete offset model. So it lands with the consumer that exercises it.
3. **Tier-b on-disk sink + emit-and-drop.** Fold in stage-2's remaining plumbing here, against a real
   dropping consumer: the `Replayer` finalizes per-turn behind the queue-marker-pinned frontier and
   **emits** finalized turns; the `SessionAccumulator` pulls them into the store and the Replayer
   keeps only the open window (content → O(turn)). Then: serialize blocks append-only + per-block
   offsets in the index; a paged
   block accessor (`Transcript`/`SessionCache` read-by-offset). `--dump`/serve/`parse_session` can
   opt into O(N)-index + paged content. Underpins `SessionCache` eviction + the resume production
   consumer (restart survival — #18's `tell`/`open_at`, still test-wired, finally used here).
4. **Windowed presentation + lazy enrichment.** TUI holds a block/line window paged from tier-b;
   `--dump*`/serve stop eager-enriching the whole sub-agent tree (stream each child as its subtree
   is emitted). Drop the render-model 2–3× duplication (`raw`/`wrapped`/`body_cache`).

Footprint wins land at 2/3/4; step 1 is a pure seam. The **byte-identical pivot** to prove once is:
*the emit-stream replayed into the in-memory sink reproduces today's `Vec<Block>` exactly* — i.e.
per-turn finalize ≡ global `finish_turns`, and `StatusPatch` ≡ global completions.

## Relationship to prior work

- Generalizes **#23**'s attachment locator (`Deferred { at }`, read on demand) to **all block
  content** (tier-b sink).
- Realizes **`session-cache.md`** tier-b (neutral on-disk parse) as the on-disk sink's format.
- Gives **#18**'s resume (`tell`/`open_at`/`Position`) its production consumer at stage 3.
- Absorbs the **#20** deferred field-lifting (content off the blocks) — moot once content is paged.
