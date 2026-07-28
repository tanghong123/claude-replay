# Design: a concrete `SessionCache` the HTML server depends on

> **Status:** proposed (not built). Tracked as task #21 — **this supersedes that task's
> earlier "reuse `SessionStore`, no new type" stance** (see
> [`line-reader-and-session-builder.md` §Layer 3](line-reader-and-session-builder.md#layer-3--lazy-session-store-reuse-sessionstore-dont-add-a-type-task-21),
> now superseded). Output-preserving; gated on the live `--html` byte-identical diff plus the
> cache's own lifecycle tests. Composes with `SessionBuilder` (#19) and the sub-agent metadata
> map (#20).

## The reversal, stated plainly

The earlier design concluded we did **not** need a `SessionCache` — that the generic
`engine::store::SessionStore<Info, Res>` plus a `FollowParser` already *was* the lazy,
self-tailing store, and a concrete type would only be an opinionated wrapper. Looking at how the
store is actually used, that conclusion was wrong in an instructive way. We should build the
concrete `SessionCache` — one that **owns parsed `Session`s** — and make the HTML server depend
on it. `SessionStore`'s genericity is not carrying its weight; a concrete cache is the honest
abstraction.

## Why the generic store isn't pulling its weight

A `SessionStore<Info, Res>` earns its two type parameters only if multiple callers instantiate
it differently. There is **exactly one** instantiation in the whole codebase —
`html_export::serve.rs`:

```rust
store: SessionStore<AgentInfo, Tailer>
struct Tailer { prev: Vec<String>, follower: FollowParser }
//              └─ presentation state ┘  └─ session/liveness state ┘
```

and it uses the generic `Res` slot to bundle **two unrelated domains** into one resident entry:

- `follower: FollowParser` — session domain: turn a growing file into current parsed state.
- `prev: Vec<String>` — pure HTML-presentation domain: the rendered-line baseline the server
  diffs against to append only byte-deltas to `<id>.jsonl`.

The store's genericity exists **only** to let those two get jammed into one slot under one TTL
clock. That is the smell: a generic abstraction paying for a coupling that shouldn't exist.
Nothing else consumes the store; its flexibility has no second customer to justify it.

## The clean cut

Split the two domains along the crate boundary they already belong to:

| Concern | Owner | Why |
|---|---|---|
| Parsed `Session` per id, lazily materialized | **`SessionCache`** (core) | session domain; presentation-agnostic |
| The incremental `FollowParser` keeping it current | **`SessionCache`** (core) | liveness of the parse |
| Residency map + TTL admit/evict/reap | **`SessionCache`** (core, private) | the ex-`SessionStore` mechanism, now internal |
| Rendering blocks → JSONL | HTML server | presentation |
| The on-disk `<id>.jsonl` + byte serving | HTML server | transport |
| The `prev` diff baseline | HTML server | presentation state, kept in a small local map |

`SessionCache` stays in `claude-replay-core` (it never names HTML, HTTP, or a rendered line).
The server *depends on* it for "give me the current `Session` for this id," and keeps only what
is genuinely its own.

```text
                       ┌───────────────── claude-replay-core ─────────────────┐
serve.rs  ── get id ──▶│ SessionCache   id → { Session, FollowParser }  + TTL  │
   │                   └───────────────────────────────────────────────────────┘
   │ owns: rendering, <id>.jsonl, prev-baseline map (id → Vec<String>), byte serving
```

## Three storage tiers (a / b / c)

The cache is not one level of residency but a **storage hierarchy**, each tier a more-processed,
more-expensive-to-hold form of the same session. Both viewers walk this hierarchy; they differ
only in policy (what they keep in the top tier).

| Tier | Form | Where | Cost to produce | Cost to hold |
|---|---|---|---|---|
| **(a)** | raw transcript (`<id>.jsonl` as the agent wrote it) | on disk, **agent-owned** | — (it's the source) | free (not ours) |
| **(b)** | **parsed** session serialized as an append-only record stream | on disk, **ours** | full parse of (a): L1 tokenize + L2 fold | disk only |
| **(c)** | parsed `Session` (blocks + index + metrics) | **in memory** | deserialize (b) — *no re-fold* | RAM per resident |

The point of tier (b) is that **loading (b) is much cheaper than producing it from (a)**:
deserializing already-folded records skips L1 tokenization *and* the L2 back-patch/grouping fold
(and, for Claude, the id join). It is also **durable across process restarts** and **incremental**
— as (a) grows, the follower folds only the delta and *appends* to (b), keeping all three tiers
consistent through the one append-only mechanism the engine already uses.

### Tier (b) must be presentation-neutral — which today's `<id>.jsonl` is *not*

The obvious tier-(b) candidate is the on-disk stream the HTML server already writes. **It can't
serve as (b)**: it is HTML-baked — each record is `{"p":"md","h": md_html(text)}` (markdown
pre-rendered to HTML in Rust, plus turn labels / chips / badges). It is a *presentation
projection*, not a serialized parse; the TUI can't reconstruct a `Session` from it. So:

> **Tier (b) is a NEW, presentation-neutral serialization of the parsed `Session`** (serde over
> the `Block` model + index + metrics + `user_times`), append-only and incremental. The HTML
> record stream is a **projection of (b)** (or of the resident (c)), not (b) itself.

This forks into a decision the build must make:

- **Option A — neutral (b) + a rendered projection (recommended).** Core owns tier (b) as neutral
  serialized blocks. The HTML server renders its browser-facing stream *from* (c) or (b), exactly
  as it renders today (`render_agent_stream` stays; its input becomes a cache `Session` instead of
  a `FollowParser` tuple). Two on-disk shapes for a *served* session — the neutral parse cache and
  the HTML projection — but the projection is HTML-server-local and regenerable, and the neutral
  (b) is the shared, TUI-loadable one. Smallest change; keeps the byte-identical HTML gate intact
  (rendering still happens in Rust).
- **Option B — one neutral (b), render client-side.** The browser fetches neutral (b) and renders
  markdown/diffs/highlight in JS. Then (b) is literally the single format both viewers consume.
  Cleaner long-term, but it ports the Rust render pipeline (`markdown`/`render`/`highlight`) into
  the browser and moves the byte-identical gate to JS-rendered output — a large, separate effort.

Recommendation: **Option A.** Tier (b) neutral + serde; HTML stays a Rust-rendered projection. Do
not couple this task to a client-side-render rewrite.

### The eviction gradient, and what each viewer keeps in (c)

Promotion/eviction is along the tier ladder, cheapest reload first:

```text
need session ─▶ (c) hit? serve.        miss ─▶ (b) on disk? deserialize → admit (c).
                                        miss ─▶ parse (a) → write (b) → admit (c).
idle past TTL / over budget ─▶ evict (c) → Session freed; (b) remains (cheap re-admit).
disk pressure (optional) ─▶ drop (b) → regenerate from (a) on next need.
```

- **HTML server.** A connected client's session stays resident in (c) (live-tailed); the
  browser is served from the always-current on-disk projection of (b). A session with no live
  interest needs no (c) — its (b) persists so a late/reconnecting client still fetches without a
  re-parse, and (c) is rebuilt cheaply from (b) if it grows again. This is today's behavior with
  (b) promoted from an HTML detail to a real, shared cache layer.
- **TUI.** Same hierarchy, a residency *budget* instead of pure TTL: keep the **root session
  always resident** plus at most **X most-recent** descended/switched sessions in (c); beyond X,
  evict to (b) and re-admit on demand (an ascend/descend or `s`-switch back into an evicted
  session deserializes (b) rather than re-folding (a)). This bounds TUI memory by the working set,
  not the total sub-agent count — the same goal the `Frame` stack already pursues for *live*
  ancestors, now extended to *evicted* ones. (Ties into #11 memory-footprint and #23 lazy
  attachments: tier (b) can hold attachment *references*, materializing blobs only on (c) demand.)

The public API below is the same across tiers — `poll`/`get` walk (c)→(b)→(a) internally; callers
express policy via `reap` (TTL) or a resident budget, not by naming tiers.

### Tier (d): materialized views — the rendered projection as its own layer

Tiers (a)/(b)/(c) are all the **same neutral session** in more-processed forms. A *presenter* needs
one more step: an **output-specific rendered projection** — and that projection is itself
worth materializing and maintaining incrementally. Call it **tier (d)**.

Crucially this is **not new machinery**: it *names* what `serve.rs` already does. Its on-disk
`<id>.jsonl` (HTML records) plus `stream_delta` — append the render-delta as the source grows,
emit `{t:"reset",from:N}` when an already-rendered block changed — **is** an incrementally
maintained, on-disk materialized view. Generalizing it unifies three ad-hoc paths:

| Instance | Renderer | Stored | Persist value |
|---|---|---|---|
| HTML live `<id>.jsonl` | `render_agent_stream` | on disk (browser fetches) | **high** — browser needs pre-rendered HTML; delta-append avoids re-send |
| `--dump` / `--dump-html` / `--dump-all-html` | dump writers | on disk / stdout | **high** — the projection *is* the deliverable |
| TUI `raw` / `body_cache` styled lines | `render.rs` | **in memory** | **low** — width/theme invalidate; disk persistence rarely pays (see fold-completeness below) |

Two kinds of materialization, on the same incremental spine, at **different crate layers**:

- **Materialized *parse* = tier (b)** — format-**neutral** `Session`, **core**, one per session,
  feeds every renderer.
- **Materialized *view* = tier (d)** — format-**specific** projection, **root crate** (rendering
  is presentation; core stays presentation-agnostic), keyed by `(session id, output format,
  render params: fold / width / theme / reveal)`, potentially many per session.

The seam is a **`Projection`** — a renderer plus an incremental-maintenance contract that
generalizes `stream_delta`:

```rust
// root crate — depends on SessionCache (core), never the reverse.
trait Projection {
    type Record;                                   // an HTML block-JSON line, a dump line, a styled TUI Line
    fn render(&self, session: &Session) -> Vec<Self::Record>;
    /// The append chunk (or a reset) to bring a stored view from `prev` to the current render —
    /// exactly today's `stream_delta`, lifted to a trait.
    fn delta(&self, prev: &[Self::Record], session: &Session) -> ViewDelta<Self::Record>;
}
enum ViewDelta<R> { Unchanged, Append(Vec<R>), Reset { from: BlockIndex, tail: Vec<R> } }
```

**A materialized view is fold-complete.** A record carries everything needed to render *every*
interactive display state of its block — both the **folded** and the **expanded** form — so a
fold/expand toggle is a **view-time selection**, not a re-render or a cache invalidation. This is
already how HTML works (it emits the full content and the JS shows/hides client-side), and it is
the property that lets the browser be stateless. The consequence for keying: **fold state is NOT
a cache key** — only the things that actually change the rendered bytes are (see below). The cost
is storing both variants; for a stateless consumer (the browser) that is mandatory, for the TUI
it is the trade that makes the cache resilient to the most frequent interaction.

**The caches are per-presenter, packaged with each presenter — not one shared thing in core.**
Core owns only the neutral tiers (a)/(b)/(c). Each output format keeps its **own** tier-(d) cache
next to its rendering code (the HTML view cache in `html_export`, the TUI view cache in the
viewer), because the record type, the persistence medium, and the invalidation triggers all
differ. Each does for tier (d) what `SessionCache` does for (c) — hold rendered records, maintain
them incrementally via the presenter's `delta` as the underlying `Session` (pulled from
`SessionCache`) grows, evict on its own policy:

- **HTML view cache** (`html_export`) — records on **disk** (`<id>.jsonl`; the browser fetches),
  keyed by `(id, reveal-mode)`, delta-appended via `stream_delta`. This is what `serve.rs`
  already is; its `prev` baseline is just this cache's per-id `prev`.
- **TUI view cache** (viewer) — records **in memory** (its `body_cache` *is* this), keyed by
  `(id, width, theme)` and **invalidated on screen-size change** — a resize rebuilds it (wrapping
  is width-dependent); a fold toggle does not (fold-complete records). No disk persistence.
- **`--dump*`** — a one-shot render to disk/stdout, no residency, no cache.

`Projection`/`ViewDelta` above are the *shape these already share*, not a type either presenter is
required to adopt; if the two view caches ever converge enough to share code, that trait is where
it lands — driven by the duplication, not ahead of it.

This makes **Option A principled**: "HTML stays rendered in Rust" just means "HTML has a tier-(d)
materialized view"; **Option B** was "drop HTML's tier (d), render client-side from tier (b)."
The parse cache (b) and the render cache (d) are cleanly separated — one neutral and shared, one
per-format and derived.

> **Scope note — tier (d) on disk is an HTML-serve concern, not a TUI one.** The value is
> lopsided: the browser is a separate process that must *fetch* pre-rendered records, so an
> on-disk, incrementally-appended HTML view is genuinely useful (it already exists). The TUI
> renders in-process; its materialized view stays **in memory** (`body_cache`, keyed by
> `(width, theme)` and invalidated on resize — fold is a view-time selection, not a key) — there
> is **no plan to persist a TUI view to disk**; the round-trip would buy nothing over
> re-rendering from the resident `Session`.
>
> So this task does **not** build a unified `Projection`/view-cache abstraction. Task #21 delivers
> tiers (a)/(b)/(c) and rewrites `serve.rs` onto them, keeping its existing HTML rendering +
> `prev`/`stream_delta` exactly where it is (now fed a `Session` from the cache). The `Projection`
> trait above is recorded only as the shape the HTML path *already* has — should a genuine second
> on-disk consumer ever appear, factor it out then, driven by that consumer, not speculatively
> (the discipline that motivated collapsing `SessionStore` in the first place).

## API

```rust
/// A lazy, self-tailing, TTL-evicted cache of parsed `Session`s keyed by session identity.
/// Materializes a transcript on first request (via a resident `FollowParser`/`SessionBuilder`),
/// tails appended events on every request after, and frees idle residents on `reap`. Owns the
/// session domain only — it renders nothing and knows no transport.
pub struct SessionCache { /* registry: id → SessionSource ;  residents: id → (Instant, Live) */ }

/// Where a not-yet-parsed session lives (the bridge from a sub-agent metadata entry, #20).
pub struct SessionSource { pub agent: Agent, pub transcript: PathBuf /* + artifact paths */ }

impl SessionCache {
    pub fn new() -> Self;

    /// Cheap, no I/O: record an id and where to load it from. A whole `Session.sub_agents`
    /// map registers in one loop. The common case for a large sub-agent tree whose children
    /// were discovered but never opened — costs only the metadata.
    pub fn register(&self, id: &str, source: SessionSource);
    pub fn is_registered(&self, id: &str) -> bool;

    /// The hot path. Admit to tier (c) on first call — deserialize tier (b) if present, else
    /// parse (a) and write (b) — **tail** on every call after (fold only appended bytes,
    /// appending the delta to (b)), and hand back an **owned snapshot** of the current
    /// `Session`. `None` when the source hasn't grown since the last poll (the skip that makes
    /// repeat reads O(delta)); `Some(Err)` if the transcript can't be read; `None` for an
    /// unregistered id. Bumps the resident's idle clock.
    pub fn poll(&self, id: &str) -> Option<io::Result<Session>>;

    /// A snapshot without forcing fresh tail I/O — the resident (c) if materialized, else a
    /// load from (b), else a first parse of (a). For batch / one-shot callers that don't tail.
    pub fn get(&self, id: &str) -> Option<io::Result<Session>>;

    /// Drop residents idle past `ttl` — the in-memory (c) `Session` + follower are freed, but
    /// **tier (b) on disk remains**, so a later `poll`/`get` re-admits by deserializing (b)
    /// (cheap) rather than re-folding (a). Bounds RAM to the working set. (A TUI passes a
    /// resident *budget* — root + X most-recent — instead; same eviction, different trigger.)
    pub fn reap(&self, ttl: Duration);
    pub fn resident_ids(&self) -> Vec<String>;
}
```

### Why `poll` returns an owned `Session`, not a borrow or a closure

The old Layer 3 left the "access shape" open (hand out `&Session` under the lock, force a
clone, or take a `use_session` closure). The live path settles it: **return an owned `Session`.**

`FollowParser::poll()` already builds a fresh snapshot (`replayer.snapshot()` allocates a new
`Vec<Block>`) and moves it out — returning a `Session` that owns those blocks is the *same*
ownership transfer at the *same* cost as today's `(blocks, times, metrics)` tuple. So there is
no borrow-lifetime problem and no extra clone: the cache does the brief incremental fold under
its internal lock, then hands the caller an owned value to render **outside** any lock. This
reproduces exactly the discipline `serve.rs` hand-codes today (poll under `with_resident` →
render in the clear → update `prev` under a second `with_resident`) — but now the "under the
lock" part lives inside the cache, and the caller simply gets a `Session` back. A closure API
(`with_resident`-style) is unnecessary here and would tempt callers to render under the lock.

## What the HTML server becomes

`Tailer` disappears. The server keeps a small presentation-only map beside the cache:

```rust
// was: SessionStore<AgentInfo, Tailer>   (Tailer = { prev, follower })
cache: SessionCache,                    // owns Session + follower + TTL   (core)
prev:  Mutex<HashMap<String, Vec<String>>>,  // rendered-line baseline per id  (presentation)
titles: HashMap<String, AgentInfo>,     // id → title/agent_type/ancestors for the breadcrumb
```

The tailer loop reads almost identically, but the session domain is behind the cache:

```rust
self.cache.reap(TAIL_TTL);
for id in self.cache.resident_ids() {
    let Some(Ok(session)) = self.cache.poll(&id) else { continue };   // fold delta, owned snapshot
    let (jsonl, children) = render_agent_stream(&session, /* fold, cwd, … */);  // outside any lock
    self.register_children(children);        // → cache.register(child_id, source) + titles
    let fresh = block_lines(&jsonl);
    // presentation diff, server-local:
    let mut prev = self.prev.lock().unwrap();
    if let Some(delta) = stream_delta(prev.get(&id).map_or(&[], |v| v), &fresh, meta) {
        append_line(&self.dir.join(format!("{id}.jsonl")), delta);
        prev.insert(id.clone(), fresh);
    }
}
```

`AgentInfo`'s *session-locating* half (`source`) moves into `SessionSource` in the cache; its
*presentation* half (`title`, `agent_type`, `ancestors` for the breadcrumb) stays with the
server as `titles`. That separation is the whole point — the earlier `AgentInfo` mixed both
because the generic store forced one `Info` type.

## What happens to `SessionStore`

The tier-(c)/tier-(a) + TTL-reap mechanism is genuinely useful; it just shouldn't be *public
and generic*. Two options, decide at build time:

1. **Absorb** — inline the two maps + `see`/`admit`/`reap` logic as private fields/methods of
   `SessionCache`. Simplest; `store.rs` is deleted. Preferred, since there's no second consumer.
2. **Keep as private guts** — `SessionCache { store: SessionStore<SessionSource, Live> }`,
   with `SessionStore` demoted to `pub(crate)`. Choose this only if a test wants the mechanism
   in isolation.

Either way the generic type leaves the public API. This is the concrete answer to the earlier
open question "do we need both `SessionCache` and `SessionStore`?" — **no; one concrete cache.**

## How it composes with the rest of the backlog

- **`SessionBuilder` (#19)** is the natural engine *inside* a resident: a `SessionBuilder` fed by
  a `LineReader` (#18) *is* the follower, and its `snapshot()` already returns a `Session` — so
  post-#19 `poll` is literally `builder.advance(delta); Some(Ok(builder.snapshot()))`. Before
  #19 lands, the cache bridges from `FollowParser::poll()`'s tuple by assembling a `Session`
  (`SessionIndex::build(&blocks)` + the known `agent`/`cwd`) — a few lines, no new capability.
- **Sub-agent map (#20)** is the primary source of `register` calls: `for (id, sa) in
  &session.sub_agents { cache.register(id, SessionSource::from(sa)) }` — O(map), no parsing.
  The map holds *edges + paths*; the cache turns a path into a live `Session` on demand. Clean
  split of structure (the map) vs content (the cache).
- **Root sessions too.** The cache keys by *any* session identity — the root registers under its
  own session id, children under their `AgentId`. So it caches any session, and sub-agents are
  just the most common registration source. This generalizes `serve.rs` exactly.

## Migration

1. Define the neutral tier-(b) format: serde over `Session` (blocks + index + metrics +
   `user_times`), append-only, with a version tag and a `reset`-means-rebuild fallback on a
   version/identity mismatch (the same contract `LineReader` uses). Round-trip test: parse (a) →
   write (b) → load (b) == direct `parse_session(a)`, byte-identical.
2. Add `SessionCache` (tiers (b)+(c), absorbing `SessionStore`'s residency mechanism) to
   `claude-replay-core`; port the `tier_lifecycle_*` test onto it and add the (b)-persistence leg.
3. Rewrite `serve.rs` onto `cache` + a local `prev`/`titles`: `Tailer` and the
   `SessionStore<AgentInfo, Tailer>` field go away; `ensure_stream`/`run_tailer` call `cache.poll`
   and feed the returned `Session` to `render_agent_stream` (the HTML projection). No change to the
   served output.
4. Delete `engine/store.rs` (option 1) or demote it to `pub(crate)` (option 2).
5. (Later, separate) TUI adoption: replace the always-resident `Frame` stack's implicit residency
   with a cache-backed budget (root + X); out of scope for the server migration.

## Verification (byte-identical gate)

- **Tier-(b) round-trip:** load-from-(b) == parse-(a) for the frozen Claude + Codex transcripts
  (the serialized parse is lossless — every `Block` field a presenter reads survives).
- **Live output unchanged:** the `--html` served stream for the frozen transcripts is
  byte-identical to `fr-mh.html` (and the multi-agent descend/deep-link paths), whether the served
  `Session` came from (c), (b), or a fresh (a) parse — Option A keeps rendering in Rust, so the
  gate holds unchanged.
- **Cache lifecycle test:** register → first `poll` parses (a), writes (b), admits (c) → second
  `poll` on an unchanged file returns `None` → append to (a) → `poll` returns the grown `Session`
  and appends to (b) → `reap` frees (c) but keeps (b) → `poll` re-admits by loading (b) (assert no
  re-fold). Extends `tier_lifecycle_see_admit_reap_readmit` with the (b)-persistence leg.
- **Lazy-load correctness:** a `poll` of a registered-but-never-opened child yields the same
  `Session` a direct `parse_session` of its transcript would (mirrors `serve.rs`'s current
  deep-link resolution).
- Gate every step on `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test`.

## Settled vs open

- **Settled:** build the concrete cache; it is a three-tier hierarchy (raw (a) → neutral parsed
  on-disk (b) → in-memory `Session` (c)); server depends on it; `SessionStore` leaves the public
  API; `poll` returns an owned `Session`; presentation `prev`/`titles` stay server-side; tier (b)
  is a **presentation-neutral** serialized parse (the HTML `<id>.jsonl` is a *projection*, not (b),
  because it is HTML-baked); **Option A** — keep rendering in Rust, HTML stream is a projection of
  (b)/(c) — so the byte-identical gate is preserved.
- **Open (decide at build):** the on-disk (b) encoding (serde_json lines vs a compact binary — start
  with JSON lines for debuggability, matching the existing stream shape); absorb `store.rs` vs keep
  it `pub(crate)`; and whether to build on `FollowParser` now or wait for `SessionBuilder` (#19) so
  `poll` is native `advance`+`snapshot`. Recommendation: JSON-lines (b), absorb `store.rs`, bridge
  from `FollowParser` now (don't block #21 on #19), refactor to `SessionBuilder` when #19 lands.
- **Explicitly out of scope here:** Option B (client-side rendering so the browser consumes neutral
  (b) directly) — a separate, larger effort that moves the render pipeline into JS; and the TUI's
  cache-backed residency budget (a follow-on to this task).

See [`line-reader-and-session-builder.md`](line-reader-and-session-builder.md) for the
`LineReader`/`SessionBuilder` layers this sits on, [`sub-agent-normalization.md`](sub-agent-normalization.md)
for the metadata map that feeds `register`, and [`docs/architecture.md`](../docs/architecture.md)
§6 for the streaming parse + live follower this extends.
