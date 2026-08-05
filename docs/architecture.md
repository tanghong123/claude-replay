# claude-replay — Architecture

> This Markdown is the maintained source of the architecture narrative (it renders inline on
> GitHub). A **standalone, graphics-rich HTML render** of the same material lives at
> [`docs/architecture.html`](architecture.html) (and the guide at
> [`docs/developer-guide.html`](developer-guide.html)) — open locally or host; regenerate when
> the architecture changes. For the exhaustive per-object API, generate the reference with
> `cargo apidoc` (see the
> [Developer Guide](developer-guide.md#the-api-reference-auto-generated-always-in-sync)).

A developer-facing design document for the `claude-replay` workspace: the reusable transcript
**engine**, the **presentation-support layer**, and the **presenters** built on them (a
terminal viewer, an HTML export/live server, and the `agent-jdi` supervisor). For the
hands-on "how do I build/test/extend this" material — including the
**[add-an-agent walkthrough](developer-guide.md#7-adding-an-agent)** and the
**[build-your-own-frontend guide](developer-guide.md#5-level-2--build-your-own-frontend-on-core--present)**
— see the [Developer Guide](developer-guide.md).

---

## 1. What it is

`claude-replay` reads an AI coding-agent's **session transcript** (a JSONL log — Claude Code,
Codex, or QoderWork) and replays it as a readable, foldable stream of blocks: user turns,
assistant prose, thinking, tool calls with their results and diffs, sub-agent spawns,
attachments, slash commands. It is **read-only** and **fully testable headless** — no TTY
required.

Two design goals shape everything below:

1. **Modularity as compiler-enforced layering.** The engine is agent-agnostic and
   presentation-agnostic; adding an agent is one small adapter, and adding a *presentation*
   is a new crate on top of the shared support layer — in both directions, no shared code is
   touched.
2. **Leanness as a design discipline.** Memory is bounded by keeping each representation no
   longer than it must live (§7), and CPU is bounded by doing only delta work on borrowed
   threads (§9). A live session with a million-line transcript costs O(open turn) RAM on the
   server and O(viewport) rendered state in either frontend.

## 2. The pipeline — and where it lives

Everything in the workspace is a stage on **one data path** (or a cache beside it). Read it
top to bottom: the left rail names the **layer and crate** a stage's machinery lives in — so
this one diagram is also the crate map. Agent-specific code exists only in the decode
stage; the return edge into discover is what makes sub-agent trees work — a parsed
`SubAgent` block names a child transcript, which runs the same pipeline recursively.

```
 layer · crate                        stage
──────────────────────────────────────────────────────────────────────────────────────
 1 FOUNDATION                        │
    claude-replay-core               │ discover     find transcripts by path/id/  ◀──┐
    (the facade: registry-wired      │              cwd; resolve a SubAgent block    │
    entry points)                    │              to its child transcript          │
                                     │      ↓ raw JSONL, one line at a time          │
    claude-replay-agents             │ decode       one raw line → 0+ canonical      │
    (the pluggable half —            │              Messages; the ONLY stage that    │ ↑ SubAgent id
    seam-only imports)               │              knows an agent's field names     │
                                     │      ↓ Message                                │
    claude-replay-engine             │ fold         one Replayer for every agent:    │
    (agent-free machinery            │              join results onto calls, group   │
    + the public seam)               │              turns, coalesce spans — §4       │
                                     │      ↓ Block                                  │
                                     │ accumulate   the durability frontier:         │
                                     │              finished turns drained           │
                                     │              put-once into a BlockStore;      │
                                     │              only the open turn stays in      │
                                     │              the fold — §5  ──────────────────┘
                                     │      ↓ Session — a value, materialized on demand
 2 SUPPORT                           │ cache        ONE live resident per session; the single
    claude-replay-present            │              full presentation copy both frontends
    (frontend-agnostic)              │              share — §7
                                     │      ↓ delta, never the whole session
                                     │ sync         in-process:            cross-process:
                                     │              ONE call, a splice-    stateless replies
                                     │              shaped Arc delta — §8  against a client-held
                                     │                                    4-member cursor — §8
                                     │                ↓ ViewDelta            ↓ PullReply
 3 FRONTENDS                         │              render — TUI:          render — HTML:
    claude-replay-tui                │              blocks → styled        wire records →
    claude-replay-html               │              wrapped lines (the     a virtualized
                                     │              hot window)            DOM window
                                     │                ↓ terminal cells       ↓ browser DOM
 4 APP SHELL                         │              TUI app                HTTP server ·
    claude-replay                    │                                     client-side artifacts
                                     │              (wires a CLI in front of all of the above)
```

### The boundaries are compiler-enforced

The layering above is not a convention — each crate's manifest makes the arrows the only ones
that exist:

| crate | may depend on | must NOT contain |
|---|---|---|
| `claude-replay-engine` | `serde`/`serde_json`, `anyhow` | any adapter implementation, any I/O policy, ratatui, syntect, clap, HTML |
| `claude-replay-agents` | engine — and only through `seam` (audited, §3) | machinery internals, presentation deps |
| `claude-replay-core` | engine + agents | any TUI/HTML/CLI dep — the same ceiling the old single core crate had |
| `claude-replay-present` | core + syntect (the highlighter returns toolkit-neutral `HlSpan`s — no ratatui) | a terminal, an HTTP server, clap (unless the `cli` feature is on) |
| `claude-replay-tui` | core + present + ratatui/crossterm | HTML |
| `claude-replay-html` | core + present | any terminal dep (verified: zero crossterm/nucleo/arboard in its tree) |

The three levels of reuse this buys (each is a real, supported consumer story — worked
examples in the [Developer Guide §4–6](developer-guide.md)):

1. **Core alone** — parse/analyze transcripts in any program (`serde_json` + `anyhow` is the
   whole dependency bill).
2. **Core + present** — build a *new* frontend (native app, web service, notebook widget):
   the session cache, the incremental pull protocol with both halves implemented, fold
   policy, summaries, and highlighting are all reusable without adopting either existing UI.
3. **The finished presenters** — embed the TUI (`app::run`, or drive `View` directly) or the
   HTML exporter/server (`dump_html`/`dump_all_html`/`serve`) from your own binary.

(A fourth, outermost story exists for **agent authors**: build on `claude-replay-engine`
alone — implement `TranscriptAdapter` against its `seam` and bring your own registry slice —
without compiling any built-in adapter. §3 spells it out.)

Internally, each crate re-exports the layers below it at its own root (`crate::model`,
`crate::present`, …) — the same transparency trick at every boundary, so moved code reads as
if the split weren't there. One workspace version, bumped in one place.

## 3. The per-agent seam

Everything that varies by agent is behind **one trait**, `TranscriptAdapter`
(`claude-replay-engine/src/adapter.rs`), and one audited helper surface, the **`seam`**
module (`engine/seam.rs`) — the complete, curated set of engine items adapter code may build
on. Three mechanisms enforce the seam, in escalating strength:

1. **The trait** — the engine calls per-agent behavior only through `TranscriptAdapter`.
2. **The crate boundary** — the built-in families live in `claude-replay-agents`, which
   depends on the engine crate alone; the facade (`claude-replay-core`) is what wires them
   together.
3. **The audit** — the `agents_import_only_the_seam` test
   (`claude-replay-agents/src/agents/mod.rs`) fails any adapter import that isn't
   `claude_replay_engine::seam`; anything an adapter newly needs is added to the seam
   *deliberately*, never reached ad hoc.

The registry is the facade's curry: `adapter(agent)` / `adapters()`
(`claude-replay-core/src/adapter.rs`) resolve over `claude_replay_agents::REGISTRY`, and
every dispatching entry point in core consults them. **Adding an agent** is therefore:
implement `TranscriptAdapter` over the seam — in your own crate with your own registry
slice handed to the engine's entry points, or as one more family + `REGISTRY` row in
`claude-replay-agents`. The shared machinery is never touched. The trait's hooks:

| Hook | Role | Default |
|------|------|---------|
| `agent()` | which `Agent` | — |
| `sniff(head)` | `SniffClaim::{Owns, CanParse, No}` — format *ownership* vs mere compatibility (drives `detect_agent` and the picker's "compatible" badge) | — |
| `store_contains(path)` | provenance: is this path inside my on-disk store? (ownership without a format marker) | `false` |
| `decode_line(line, cwd, out)` | **L1**: raw line → 0+ canonical `Message`s | — |
| `shaping()` | the L2 `Shaping` const (4 fn-pointers) | — |
| `metrics_acc()` | a fresh token/cost accumulator | — |
| `load_attachment(line, index)` | a deferred attachment locator's bytes from ONE raw line | `None` |
| `candidates_scoped(cwd)` | discovery: sessions for a cwd | — |
| `resolve_id(id)` | discovery: id → transcript path | — |
| `load_tasks(path)` | the session's task/todo list from the agent's store | `None` |
| `parse_reader(reader)` | metrics-only fold | **provided** |
| `enrich(path, blocks)` | load the sub-agent tree | **no-op** |
| `subagent_source(root, id)` | a child transcript's path | **None** |
| `subagent_sources(root, ids)` | MANY children's paths in one operation-scoped call (a relationship-store adapter scans once) | **provided**: per-id `subagent_source` |

(The whole-file parse is not a hook: the facade's `Transcript::parse` drives the shared
`SessionAccumulator` with the adapter — one fold, composed from the hooks above.)

`Agent` itself is an **open interned id**, not a closed enum:
`pub struct Agent(&'static str)` with `Agent::CLAUDE`/`CODEX`/`QODERWORK` as associated
constants and `Agent::new("gemini")` for third parties, serialized as its label. The engine
never enumerates agents — the id is only the *name* that keys the registry, so a new agent
mints its own with no variant to add and nothing to fork.

Everything agent-neutral reaches per-agent behavior *only* through the registry — there is no
`match agent` scattered across the engine. Three adapters exist today and demonstrate the
cost floor: Claude (full), Codex (no sub-agent tree ⇒ omits those hooks), QoderWork (delegates
decoding to Claude's family entirely; its adapter is discovery + identity).

> The `agent-jdi` supervisor mirrors this with its own `jdi::agent::AgentAdapter` registry.

### Discovery, precisely

Discovery splits along the same seam. The engine's `discover` module is the agent-free
*vocabulary* — the `Candidate` type, `session_cwd`/`session_id` (transcript-head readers),
and the `ancestors_below`/`home_dir` scoping. The facade's `discover`
(`claude-replay-core/src/discover.rs`, which re-exports the vocabulary so
`core::discover` stays the ONE module) is the registry-driven front door: `detect_agent`
(sniff + store provenance → ownership, so a merely-*compatible* file is labeled, not
mislabeled), `candidates_all` (the cross-agent picker list),
`resolve_any` (id/path/latest → a path), `session_tasks`, and `subagent_source`. Cwd-based
auto-discovery is scoped **strictly inside `$HOME`** (`ancestors_below`): a cwd outside it
probes nothing, and the probe never reaches `$HOME`'s own slug — so stores polluted by
misbehaving agents (sessions recorded against `$HOME` or `/`) can never leak into an
unrelated directory's picker. Explicit paths and ids are never scoped.

## 4. The fold: what the Replayer actually does

The fold stage looks like one arrow; it is where most of the engine's hard-won correctness lives.
A transcript is not a clean event log — results arrive out of order, turns interleave, and
what an agent's own UI *shows* differs from what it *records*. The `Replayer` reconciles all
of it, once, for every agent:

- **Result joining & back-patching.** A tool's result lands as a separate later event — the
  fold attaches it to its call *in place*, even when the call was emitted in an earlier
  poll (the follower back-patches across polls without re-parsing).
- **Turn grouping & span coalescing.** Consecutive thinking bursts and "activity" tool
  calls between two visible outputs fold into ONE work-span block, with summed durations
  and Claude-Code-faithful phrasing — an empirically derived rule set
  ([`design/cc-activity-coalescing.md`](../design/cc-activity-coalescing.md)) that agents
  opt into per adapter (`Shaping::finish_turns`). MCP tool calls coalesce into the same
  spans, phrased per server (`called <server> N times` — `summary.rs`).
- **The queued-prompt lifecycle.** A mid-turn human prompt is recorded when *submitted* but
  displayed when *picked up*; the fold runs that little state machine (suppressing the
  marker when pickup is immediate).
- **The committed/open split.** The fold maintains a **durability frontier**: blocks of
  finished turns are final and never touched again; only the open turn is re-derived as
  events arrive. Every incremental surface in the workspace — `changed_from`, the cursor
  protocol's generations, the put-once stores — leans on this invariant. The few
  back-references that hold a completed turn resident (a skill body that may still nest, a
  queued prompt whose marker may still be suppressed) are each **bounded**, and that is a
  property the tests assert rather than a hope: an unbounded one is invisible — the session
  renders perfectly, it just stops committing — and one such pin once froze 26% of a large
  session in RAM for its remaining 219 turns.
- **Reshape detection.** A raw-level pure append can still REWRITE the finalized open
  turn's prefix (a new tool joins a span and absorbs earlier blocks). The live layer diffs
  the finalized view per tick to catch exactly this — it is why the sync protocol has a
  *generation* member, not just an append cursor.

The proof obligations are pinned in tests: the streaming fold is byte-identical to frozen
whole-file oracles, and the follower to a full re-parse at every append.

## 5. The sans-io accumulator — one fold, every acquisition mode

The engine's heart deliberately does **no I/O**. [`SessionAccumulator`]
(`engine/builder.rs`) is a push-based fold: the *caller* acquires bytes however it likes and
pushes lines in; the accumulator threads them through decode → fold → metrics behind one
method:

```rust
// The REAL signature: the sink is a type parameter, chosen by the consumer.
struct SessionAccumulator<S: BlockStore = InMemoryStore> { /* … */ }

let mut acc = SessionAccumulator::with_store(adapter, store); // ::new(adapter) = identity store
loop {
    acc.advance_at(byte_offset, &line);  // push one line — that's the I/O seam.
                                         // INSIDE this call, any turn that just finished
                                         // crosses the durability frontier and is `put`
                                         // into S — once, right now, not at the end.
    // …read at ANY moment, while the fold keeps going:
    acc.session_meta();                  // the live header — maintained, never rescanned
    acc.open_read();                     // StreamRead: the open turn + times + metrics +
                                         //   header — O(turn), NO committed content
    acc.committed_tail(from);            // Vec<Block>: committed[from..] — any range,
                                         //   materialized back out of S on demand
    acc.stream_read(from);               // the two combined into one StreamRead — what
                                         //   a live consumer polls
}
let session = acc.into_session();        // the one-shot ENDING: consumes the accumulator
```

(`adapter` is a `&'static dyn TranscriptAdapter` — the engine's constructors take the
adapter itself; the facade curries the agent id for you, `claude_replay_core::adapter(agent)`.
`byte_offset` is the line's position in the raw transcript — the fold stamps it into
**locators** for content it deliberately does *not* inline, so presenters can lazy-load
attachments from the raw file later.)

The lifecycle question this answers: a `Session` is a **value materialized at a moment**,
not an object the accumulator manages. The accumulator owns the running state — `committed:
Vec<S::Bv>`, the replayer's open window, the maintained header/metrics/tasks — and there
are three ways to read it:

- **Maintained live reads** — no `Session` built at all; what the live stack actually uses
  per poll. They come as a **pair covering the two zones**: `open_read()` returns a
  `StreamRead` carrying everything O(open turn) — the finalized open turn, per-turn times,
  metrics, the maintained header — and **no committed content**; it is store-agnostic (no
  `BlockRead` bound). `committed_tail(from)` covers the other zone: `committed[from..]` as
  owned `Block`s, any range, materialized back out of the store on demand — it is the call
  that *does* need `S: BlockRead`, so a write-only projection store opts out at the type
  level and serves its committed zone from its own representation instead.
  `stream_read(from)` is literally the two composed: one `StreamRead` whose
  `committed_delta` is that tail. And `session_meta()` is the cheapest cut — just the
  header, folded forward one block at a time as turns commit, never recomputed.
- **`snapshot(&mut self)`** — materialize a whole point-in-time `Session`; *the fold keeps
  going*. It clones the committed values — content for a plain in-memory store, only
  refcount bumps under `ArcStore`. A one-shot inspection tool (a test assertion, a debug
  dump) — **not** a tailing surface: a live consumer reads the deltas above instead of
  re-materializing per poll, which is why it appears nowhere in the loop.
- **`into_session(self)`** — the batch ending, BY MOVE: transfers the committed values out,
  zero block clones for the RAM/`Arc` stores. After this the accumulator is gone; it is
  what `Transcript::parse` calls once at end-of-file, never the tailing surface.

The derived `SessionIndex` (per-turn/tool/attachment rollups) is built only *inside* the
two materializers, as a pass over that snapshot's point-in-time view — the live path
doesn't carry a full index; its incremental complement is the maintained `SessionMeta`.
Everything else the materializers assemble was maintained as the fold went; nothing rescans
the committed prefix.

And this is all `SessionCache` is, one level up — a keyed map of exactly this loop, wrapped
for sharing (§7 covers its residency tiers and store choices):

```rust
// SharedSession ≈ Mutex<FollowParser<S>>, and FollowParser ≈ (LineReader + SessionAccumulator<S>):
// on each poll/pull, ON THE CALLER'S THREAD:
while let Some((offset, line)) = reader.next_appended_line()? {
    acc.advance_at(offset, &line);            // fold ONLY the appended bytes
}
reply_from(acc.stream_read(client_from));     // …or poll_view's Arc-delta splice —
                                              // no whole-Session materialization per tick
```

Because byte acquisition is the caller's job, **every parse path in the workspace is the same
fold**, differing only in who feeds it:

| consumer | who pushes lines | why it matters |
|---|---|---|
| `parse_session_as` (batch) | a file reader, one line resident | whole-file parse never builds a `Vec<String>` |
| [`FollowParser`] (live tail) | the byte-offset line reader (`engine/reader.rs`), only appended bytes | O(delta) per poll; proven byte-identical to a full re-parse |
| `SharedSession` (live server) | the follower, on a *client's request thread* | see §9 — no server-side polling loop |
| your code | a socket, an mmap, a test vector, a decompressor | the library never dictates your I/O |

The accumulator also owns the **durability frontier** that the whole leanness story hangs on:
as each turn completes, its blocks are drained **put-once** into a pluggable [`BlockStore`]
`S` (`committed: Vec<S::Bv>`), while the replayer retains only the **open turn** — so the
fold's resident content is O(turn) regardless of session length. Storage policy is injected,
not baked in:

- `InMemoryStore` (default) — committed blocks resident in RAM (`Bv = Block`).
- `TierBStore` (`engine/tier_b.rs`) — committed *content* serialized append-only to an
  off-heap buffer or an on-disk file; `Bv = Deferred { offset, size }`, a 12-byte locator.
  Reading a block back is a positional read + decode, dropped after use.
- `ArcStore` (#84) — committed blocks live behind `Arc` (`Bv = Arc<Block>`): the accumulator
  retains the authoritative copy, and handing a block to a reader is a refcount bump. The
  store behind the one-copy/source-of-truth principles (§7).
- `ArcLog` (#96, the TUI's) — the same `Arc<Block>` residency **plus** an append-only JSONL
  backing, so a later process can load the committed prefix instead of folding to reach it.
  Not tier-b: tier-b writes to keep content *off* the heap, this writes to save the *next*
  run work.
- …or the presentation's own: the html crate's `RecordStore` renders each block to its wire
  record at `put` time (`Bv = RecordLocator` — the §6 showcase).

Incremental facts are maintained the same way: `session_meta()` folds the live header
(turns/tools/children) once per committed block on drain, and `stream_read(from)` hands a
live consumer `committed[from..]` + the open turn — O(delta), never an O(N) rescan.

## 6. The data model

The presenter-facing vocabulary lives in `model.rs` (`claude-replay-engine`, re-exported by
every layer above) and is the stable public API.

- **[`Block`]** — one render unit. Variants: `UserText`, `AssistantText`, `Thinking { text,
  duration_secs, tools }`, `ToolUse { name, target, diffs, output, patch, read_lines }`,
  `ToolResult`, `Attachment`, `SubAgent`, `AgentDone`, `Command`, `QueueEvent`. Tool results
  are already joined onto their calls; nothing is dropped or truncated (what shows collapsed
  is a *view* decision, not a parse decision).
- **[`Session<BV = Block>`]** — the whole parse: `{ agent, cwd, blocks, user_times, metrics,
  index, tasks }`. Generic over the block representation: `Session<Block>` is fully resident;
  `Session<Deferred>` holds only the locator table (content in a tier-b backing) and reads
  blocks back through the [`BlockAccess`] trait.
- **[`SessionIndex`]** — derived within-session rollups (turns, sub-agents, tool counts,
  attachments) computed in one scan, so presenters don't re-walk the blocks.
- **[`Metrics`]** — token/cost tally + a formatted footer.
- **Classification** — `block_kind(&Block) -> BlockKind`, projected to a coarse `fold_key`
  (fold grouping — and the key of core's `FoldPolicy`) and a fine `BlockKind::html`
  (styling). One classifier, two projections — the TUI and HTML can't disagree on what a
  block *is*.

### Showcase: `Session<BV>` in the live HTML server

**The presentation layer decides what lives in `BV`** — the block itself, auxiliary per-block
state the presentation needs, a pointer into a file the presentation maintains, a deferred
block locator, or any combination. The live server is the full expression of that principle
(#74): its `BlockStore` is the HTML crate's own `RecordStore`, whose `put` renders each
committed block to its **wire-format JSON record** exactly once — as it crosses the
durability frontier — appends it to `<id>.records`, and returns the locator. The `Session`'s
committed table *is* the wire projection:

```
SharedSession<RecordStore>  (present, parameterized by the html crate)
  Session<RecordLocator>
    committed: Vec<RecordLocator{offset,len}> ──▶  <id>.records
                                                   (rendered wire JSON, one record per line —
                                                    the ONLY committed storage)
```

- **One storage, one serialization.** Nothing is stored twice: there is no separate block
  backing and no parallel render cache — the store and the session are one object, so they
  cannot desync. Read-back is deliberately impossible (`RecordStore` implements `BlockStore`
  but not `BlockRead` — a wire record is a one-way projection), and the type system keeps
  every committed consumer on the pointer path. The TUI/batch side instantiates the same
  parameter with `BV = Block` (`InMemoryStore`); a lossless spill store (`TierBStore`,
  `BV = Deferred`) remains available to any consumer that needs `Block`s back.
- **Clients read the projection directly; the serving path burns no CPU on it.** A `/pull`
  reply renders **only the open turn**; the committed zone is answered with a pointer —
  `committed_ext: {offset, len}` — and the client range-reads the wire JSON straight off
  `<id>.records` via `/records`. The main serving function never re-renders, re-serializes,
  or even copies committed content: a session with a thousand committed blocks and a
  three-block open turn costs a poll three blocks of render and the client one `pread`.
- **The projection survives the process.** The record log is also the durable cache's
  content stream (#96): a later run reloads the locator table by walking its framing
  newlines, and **derives** the render continuation (`EmitState`) from the restored prefix
  rather than reloading a stored copy — one record per block makes `next_block` the block
  count, and both turn counters advance once per user turn, which is what the header counts.
  Derived cannot go stale against the prefix; persisted can.

## 7. Lean memory: a ladder of representations, each windowed

The memory design is one idea applied five times: **every representation is kept only as
long, and only as wide, as its consumer needs**. Data climbs this ladder, and each rung holds
a *window*, not the session:

```
  transcript bytes (agent's store, disk)     resident: none — byte-offset tail reads deltas
      │  L1 decode (one line at a time)
  canonical Message                          resident: ~1 line's worth, transient
      │  L2 fold (Replayer)
  Block — open turn                          resident: O(turn) in the accumulator
  Block — committed                          resident: policy! InMemoryStore = RAM ·
      │                                        TierBStore = 12-byte locators, content on disk ·
      │                                        RecordStore = wire-record locators (#74) ·
      │                                        ArcStore = Arc-shared content, ONE copy that the
      │                                        cache owns and every reader references (#84)
      │  presentation shaping (fold policy, summaries, records/lines)
  presentation-friendly form                 TUI: heights+prefix (O(N) small ints);
      │                                        HTML: JSON records (client), record log (server)
      │  render
  rendered result                            TUI: `hot` window ≈ 96 blocks near the viewport;
                                             HTML: DOM window ≈ viewport ± 1500px
```

The windows at the top rung deserve emphasis, because both frontends independently converged
on the same structure — an O(N) *index* of cheap integers plus an O(viewport) cache of
expensive rendered output:

- **TUI** (`view.rs`): `heights: Vec<usize>` (wrapped height per block at the current width
  and fold state) + `prefix` sums make scroll math a binary search — but **rendered styled
  lines are NOT retained per block**. The only styled-line residency is `hot`, a bounded map
  (cap 96 blocks, evicted by distance from the viewport ± 8) filled on demand. A deliberate
  asymmetry: `search_text` keeps plain text for ALL wrapped lines (content-sized, cheap, and
  search must see everything) while styling stays windowed.
- **HTML client** (`html/export.js`): the JSON `records[]` array is the source of truth; the
  DOM holds only records within ~1500px of the viewport between two spacer `<div>`s whose
  heights are estimated then measured (prefix sums + binary search again). Eviction is
  lossless because interaction state (folds, search cursor, expansions) lives in id-keyed
  maps beside the records, re-applied on re-materialization. The full technique write-up:
  [`design/dom-virtualization.md`](../design/dom-virtualization.md).
- **HTML server** (`serve.rs` + `cache/`): a followed session's resident footprint is
  O(open turn) + a locator table — the `Session`'s own `RecordStore` renders each committed
  block to its wire record put-once (#74, §6 showcase), and the committed zone is served as
  a **pointer** (`committed_ext: {offset, len}`) that clients range-read from the append-only
  record log at their own pace (one serialization on disk, zero in RAM, N readers).

### The unified data layer: `SessionCache<P, A>`

Five principles govern this layer (settled in #84, and worth stating because every seam
below follows from them): **(1)** at most ONE full in-memory presentation copy per client
application instance — it exists to make search fast, and search runs over it; **(2)** the
`SessionCache` owns the SOURCE OF TRUTH of that copy — ownership, not storage medium: the
TUI's is the RAM blocks the in-process view references, the HTML server's is the on-disk
wire-record log its browser clients resync from; **(3)** therefore views never own blocks
(else principle 1+2 would force a wasteful disk mirror for the in-process case); **(4)** the
presentation format is each frontend's fastest-final-render form — `Block`s for the TUI,
DOM-loadable wire JSON for HTML — chosen at the `BlockStore` seam; **(5)** only FINAL
rendered results (and viewport neighbors) live under a view: the hot window / DOM window,
plus small derived indexes (geometry, fold state) in the aux sidecar.

Everything above meets in one owner. [`SessionCache`] is **the data layer a presentation
builds on** — it answers "which sessions exist, which are materialized, in what
representation, with what derived state attached" — and its two type parameters are the
two decisions a frontend gets to make without losing generality *or* efficiency:

| seam | decides | the menu |
|---|---|---|
| `P: BlockStore` | the live store — the ONE resident kind's committed representation (#85: one tier serves both consumption styles) | `ArcLog` (cache-owned shared copy + a durable backing — the TUI, #96) · the html crate's `RecordStore` (the wire projection itself, #74) · `ArcStore` (the same sharing, nothing written) · `TierBStore` (lossless spill) |
| `A` | the per-session **presentation sidecar** | any type — park-and-take (`aux_put`/`aux_take`) or in-place (`aux_with`) |

The residency tiers under it (#85: ONE live tier — the same resident serves the in-process
view and the wire protocol):

| tier | holds | cost | transition |
|---|---|---|---|
| (c) registered | agent + path | ~nothing | the default for a discovered-but-unopened session (a large sub-agent tree stays here) |
| (a) live resident | a `SharedSession<P>` | O(turn) + tables (+ disk backing per `P`) | polled recently; reaped to (c) after 30 s idle |
| (d) durable | the committed + meta streams on disk, under `<root>/<presentation>/<session>/` | bytes only — nothing resident | written as the fold commits; survives the process, so the NEXT run resumes instead of re-folding (below). Swept after two weeks idle |

And the consumption protocols, each matched to a representation by the type system
(a capability bound, not a convention):

| protocol | shape | requires | copies of committed blocks | per-tick cost |
|---|---|---|---|---|
| `poll_view(id, make_store)` — in-process | ONE call: advance + splice-shaped `Arc` delta + times/metrics/tasks | `P::Bv = Arc<Block>` | **1 — cache-owned, view-referenced** | **O(delta)** |
| `shared_session(id)` + `pull` — wire | cursor zones / byte-range pointers | any `P` | 1 (or 0 in RAM with `RecordStore`) | O(open turn) |

(Whole-`Session` consumers use the core's own follower — `FollowParser::poll`/`poll_delta`
— or batch `parse_session`; the cache's job is live residency, and its two protocols share
one resident and ONE implementation of "what changed since last tick".)

Both frontends instantiate the same type — that is the "unified without losing generality"
claim made concrete:

```rust
// TUI (app.rs):  the View is the sole block owner; evicted frames park their derived state
type TuiCache = SessionCache<ArcLog, ViewSidecar>;     // every parameter doing chosen work
// HTML server (serve.rs):  committed IS the wire projection; per-id serve state lives in aux
cache: SessionCache<RecordStore, ServeAux>
```

The **sidecar slot** (#75) completes the story: it is the home for derived,
view-parameter-*dependent* state that `put`-once storage can't hold. The TUI parks an
evicted frame's measured heights/prefix, search index, fold toggles, and scroll there
(`ViewSidecar`) and re-adopts them when the frame reloads — same width and shape means no
re-measure, and the user's interaction state survives the eviction. The HTML server keeps
its per-session titles, parent pointers, and stream-diff baselines there (`ServeAux`, #76).
The slot is opaque to the cache; **the consumer owns validity**.

The TUI applies the same thinking one level up: sub-agent frames you drill into are kept
under an LRU cap of 4 (`MAX_RESIDENT_SUBAGENTS`); ancestors evict to registrations and
reload on demand — with the sidecar, an eviction now drops only the blocks.

### Across runs: the durable session cache

Everything above bounds what a *running* process holds. This bounds what a *new* one has to
redo. A durable cache keeps each owned session's committed blocks and meta records under
`$XDG_CACHE_HOME/claude-replay/sessions/<presentation>/<session>/`, so the next invocation
**resumes the fold** instead of re-reading the transcript from byte 0. Measured on real
sessions: 99.99% of a 107 MB transcript skipped, block-identically.

**Two append-only streams.** The content stream is the frontend's own `BlockStore` backing
(the TUI's `ArcLog`, the server's `RecordStore`), so nothing is stored twice — the same
bytes that serve the session persist it. The meta stream is one `MetaRecord` per committing
drain, carrying what a block list cannot: counter deltas (turns, tools, per-model tokens,
task ops), gauges (cwd, span), and the resume payload.

**One principle, from which the rest follows.** A resume point is an `(offset, state)` pair
such that folding from `offset` seeded with `state` yields *exactly* what a cold parse
yields. That is why `replay_from` is a **partition**, not a bookmark: bytes below it
authored only committed blocks, bytes at or above it only uncommitted ones — so the resumed
fold suppresses nothing and re-applies nothing. A drain that admits no such partition (one
line authored blocks on both sides) simply carries no resume payload, and the next one does.

**Every crash leaves a prefix**, both streams being append-only — which makes the recovery
space enumerable rather than hopeful. Loading is therefore an *alignment*: fold the records,
stop at the last one the content stream corroborates, cut the content stream to match. The
content stream is the authority; meta describing commits it cannot corroborate is ignored.

**Validation is a chain of cheap checks, each rejecting a different lie**, and a rejection is
always a full cold rebuild — never a partial serve:

| check | catches | on failure |
|---|---|---|
| fold/format version | a build whose blocks differ — a resume would splice two folds with no visible seam | `Cold(VersionChanged)` |
| anchor (CRC32 of the transcript's first line) | a *different* session at the same path | `Cold(SourceRewritten)` |
| length ≥ `replay_from` | a truncated source | `Cold(SourceRewritten)` |
| window (CRC32 of the 64 KiB below `replay_from`) | the prefix rewritten in place — the only region a resume derives from | `Cold(SourceRewritten)` |
| alignment | a torn tail below the first resume point | `Cold(TornStream)` |
| checkpoint agreement | a checkpoint that disagrees with the state folded up to it — the stream is corrupt, or writer and reader have drifted | `Cold(CheckpointMismatch)` |

CRC32 rather than a cryptographic hash on purpose: this is not a trust boundary, only a
corruption check, and it is ~13× cheaper. The reasons are typed (`ColdReason`) because "the
cache did not help" is a support question, and the rejection tests assert on the reason
rather than on "it rebuilt".

**Checkpoints bound the work and check the fold.** Every `CHECKPOINT_EVERY` resumable drains a
record also carries an absolute `MaterializedMeta`. It does three jobs: a reader may *start*
there, which bounds an open's work (otherwise O(records), growing without limit); compaction
becomes trivial and needs no fold — keep the newest corroborated checkpoint and everything after
it, rewriting to a temp file and renaming, so a crash mid-rewrite leaves the original intact; and
a reader that *passes* one compares it against what it folded, which turns "a resume equals a
cold fold" from a property tests assert into one production verifies on every load. That last
job is why a checkpoint is built from the fold's **maintained** state rather than from the
records it rides with: two identical folds over identical records always agree, so a
self-derived checkpoint would make the comparison tautological — able to catch a corrupted byte
but never a bug in the deltas. A checkpoint
only ever rides a record that already has a resume payload — otherwise compacting onto it could
leave complete state with no `replay_from` anywhere.

**Exactly one writer per `<presentation, session>`, always.** A file lock names its holder;
reclaim is liveness-based, and where liveness cannot be decided (a non-unix host) the cache
is **disabled** rather than assumed stale — guessing wrong fails *into* concurrent writers,
the one outcome the lock exists to prevent. Admission therefore has **two** outcomes, not
three:

```rust
match cache.admit(id, make_store, alive) {
    Admission::Owned { session, origin } => …,   // exclusive; `origin` says resumed or why cold
    Admission::Denied(denial)            => …,   // NOTHING was opened, nothing is shared
}
```

Falling back to a cache-less session is a separate, explicit `open_uncached` call, so "we
gave up on caching" is visible at the call site instead of hidden in a third variant that
would suggest a session might be handed out while another process owns it. The two frontends
resolve a denial differently, and the asymmetry is why the holder's note is *frontend-typed*
(`DurableStore::Note`): the TUI refuses a second **live** view and names the holder's tmux
pane, because two instances would each fold and hold the same growing session in RAM
invisibly; the HTML server serves cache-less, because partial success is normal for a
multi-root server. A one-shot read is never refused — the refusal's argument is about
following.

**Nothing is persisted that can be derived.** No presentation state crosses a run: the HTML
render continuation is recomputed from the restored prefix (§6), fold/scroll state is
per-run by definition, and the pull protocol's `epoch` stays a live-session token. What is
persisted is only what the *fold* cannot recompute without re-reading bytes.

Locks are released on every exit path — both `process::exit(0)` sites explicitly, everything
else by `Drop` — and a GC sweep drops entries idle past two weeks, trusting a lock without
probing it up to an age cap so a crashed holder cannot pin bytes forever.

## 8. The sync protocol: a 4-member cursor

The wire half of the data layer is a client-server sync protocol designed so the **server
keeps no per-client state**: the client's whole position travels with each request as four
numbers — `Cursor { epoch, committed_id, provisional_gen, provisional_index }`:

- **`committed_id`** — how far into the append-only committed log the client has read.
  Committed content is sent once, ever — and as a **byte-range pointer** into the on-disk
  wire-record log, which the client range-reads itself.
- **`provisional_gen` / `provisional_index`** — the open turn's *generation* and the
  client's position within it. Within a generation the served open turn is append-only; a
  back-patch or a finalization reshape bumps the gen, telling the client to replace the
  whole (small) zone. This is the reshape-detection invariant from §4 surfacing on the
  wire.
- **`epoch`** — session validity: a truncation/rewrite bumps it, and any stale cursor
  resyncs from zero — served from the cache's retained copy, never by re-parsing.

The client applies one rule per zone — *truncate to `from`, then extend* — and both halves
ship in Rust: [`pull`] (server) and [`PullClient`] (the executable specification, walked
through every transition by its tests; the embedded JS client mirrors it one for one). N
windows, laggy tabs, and fresh joiners all work against one shared log at their own pace.

## 9. Lean CPU: borrowed threads and delta-only protocols

**No dedicated work happens when nothing changed, and almost no thread exists solely to
wait.** Two mechanisms:

**Borrowed-thread tailing.** Neither frontend runs a follower thread:

- The **TUI** pumps the live tail on its *input event loop*: when `event::poll(250 ms)` times
  out with no keystroke, the idle tick calls `cache.poll_view(id)` — so tailing costs zero
  threads and zero work while the user is interacting flat-out, and at most 4 polls/s idle.
- The **HTML server** (pull mode, the default) has **no background tailer at all**: the fold
  advances on the *client's request thread* (`/pull` → `SharedSession.advance()` → reply).
  An idle page = an idle server; a closed page costs nothing until the 30 s reap tidies the
  resident. The accept loop is one detached thread; connections are thread-per-request on
  loopback.

**Delta-only protocols, at both distances.** The committed/open split (§4) makes "what
changed?" answerable in O(turn), and both consumption protocols exploit it:

- In-process: `SharedSession::poll_view` hands the TUI everything in ONE call — a
  splice-shaped delta of `Arc` clones (the resident RETAINS the authoritative committed
  vector — the cache-owned source of truth), the fresh open turn, `changed_from`, and the
  chrome state (times/metrics/tasks). `View::apply_view` splices pointers in place and
  re-derives only the tail: O(delta) refcount bumps and re-measure, content stored ONCE,
  resync-from-zero served from memory (#84/#85). The change boundary and the wire
  protocol's gen bumps come from the SAME reshape comparison — one implementation.
- Cross-process: the **4-tuple cursor protocol** (`present::pull`) — `Cursor
  { epoch, committed_id, provisional_gen, provisional_index }`. The cursor travels with each
  request, so the server is **per-client stateless** and N tabs/windows each read at their
  own pace; committed data is a pure append (sent once, or range-read from the record log),
  and only the open turn ever re-sends (gen bump). Both halves are implemented in Rust:
  [`pull`] (server) and [`PullClient`] (client — the executable specification the JS client
  mirrors transition-for-transition, and the ready-made consumer for a future decoupled TUI).
  A caught-up client's pull is idle: one `pull_indices` length-check, no clone, no render.

The same discipline shows up smaller: pass-1 id scan is a cheap stream; `put`-once into the
block store (never re-serialize committed content); `session_meta` folded per commit rather
than rescanned; the HTML emitter diffs block lines and re-sends only from the first
divergence; the DOM reconciler batches reads/writes to avoid layout thrash.

## 10. Invariants the codebase holds itself to

- **Crate boundaries** — the §2 table is enforced by the dependency graph, not review.
- **Agent-agnostic everything above L1** — no presenter or shared-engine code matches on
  `Agent` for behavior; it routes through the adapter registry.
- **Byte-identical refactors** — the streaming parse is proven identical to frozen oracles
  (`#[cfg(test)]` equivalence gates in `claude-replay-agents`' `model` families), the follower to a full
  re-parse at every append, and every change runs `scripts/gate/gate.sh` — a frozen-fixture
  `--dump`/`--dump-html`/bundle diff that must print `BYTE-IDENTICAL: PASS`.
- **One classifier / one fold / one phrasing** — block classification, the L2 fold, and the
  summary vocabulary (the engine's `summary.rs`, re-exported as `core::summary`) exist once;
  per-agent difference lives only in `decode_line` + `Shaping` (see
  [`design/fold-coalesce-summarize-extensibility.md`](../design/fold-coalesce-summarize-extensibility.md)).
- **Protocol equivalence** — `SharedSession` pulls are asserted identical across storage
  backings (RAM vs tier-b), and `PullClient` is walked through every protocol transition
  against the server half.
- **A resume equals a cold parse** — the durable cache's oracle is always a from-scratch
  fold, block for block, because a corrupt-but-plausible resume passes every
  self-consistency check there is. Asserted for clean resumes, resumes-from-resumes, and
  every truncation of both streams (the crash-consistency harness).
- **The durability frontier cannot freeze** — every back-reference that holds a completed
  turn resident is bounded, and a test says so on real transcripts. The failure is silent
  (a pinned session renders correctly and simply stops committing), so nothing catches it
  except measuring.

## 11. Where things live

| Path | What |
|------|------|
| `claude-replay-engine/src/model.rs` | `Block` model + classification |
| `claude-replay-engine/src/engine/replay.rs` | L2 `Replayer`/`Shaping` (+ the frozen `replay` reference driver) |
| `claude-replay-engine/src/engine/builder.rs` | `SessionAccumulator` (sans-io fold) + `StreamRead` |
| `claude-replay-engine/src/engine/{message,session,index,tasks,tier_b,reader,path,time}.rs` | canonical log · `Session<BV>`/`BlockStore`/`BlockAccess`/`ArcStore` · rollups · task model · tier-b backing · byte-offset line reader (tail + resume) · helpers |
| `claude-replay-engine/src/adapter.rs` | the `TranscriptAdapter` trait + `SniffClaim` (the contract — the registry lives in the facade) |
| `claude-replay-engine/src/engine/seam.rs` | the audited adapter contract — all adapter code may import (#87) |
| `claude-replay-engine/src/engine/meta_stream.rs` | the durable meta record + `MaterializedMeta` + alignment (#96) |
| `claude-replay-engine/src/{discover,metrics,follow,agent,fold,summary,diff}.rs` | discovery vocabulary · metrics · live follower (tail + resume) · the open `Agent` id · fold policy · span phrasing · diff-row model |
| `claude-replay-agents/src/agents/{claude,codex}/model.rs` | L1 tokenizers + `Shaping` |
| `claude-replay-agents/src/agents/{claude,codex}/metrics.rs` | token/cost folding |
| `claude-replay-agents/src/agents/{claude,codex,qoderwork}/discover.rs` | per-agent transcript stores |
| `claude-replay-agents/src/adapters.rs` | the built-in `TranscriptAdapter` impls + the `REGISTRY` slice |
| `claude-replay-agents/src/agents/mod.rs` | the family tree + the `agents_import_only_the_seam` audit |
| `claude-replay-agents/tests/engine_integration.rs` | machinery-with-real-adapters integration tests (a dev-dep cycle would compile two engines inside engine) |
| `claude-replay-core/src/{adapter,discover,session_entry,transcript}.rs` | the wired `adapter()`/`adapters()` registry · registry-driven discovery · the `parse_session*` dispatchers · the `Transcript` source handle |
| `claude-replay-present/src/cache/{mod,shared}.rs` | `SessionCache` residency + the durable API (`durable`/`admit`/`release`) · `SharedSession` · the `DurableStore` seam |
| `claude-replay-present/src/cache/{stream,lock,admit}.rs` | the meta stream on disk · the single-writer lock · claim/validate/align + GC (#96) |
| `claude-replay-present/src/pull.rs` | the pull protocol (`Cursor`/`PullReply`/`pull`/`PullClient`) (#87) |
| `claude-replay-present/src/{present,highlight,sys,args}.rs` | text formatters · syntect highlighter (`HlSpan`) · OS/path helpers · shared `Args` (`cli` feature) |
| `claude-replay-tui/src/{view,app,render,markdown,wrap,theme,picker,clipboard}.rs` | the terminal viewer |
| `claude-replay-tui/src/store.rs` | `ArcLog` — the TUI's durable `Arc<Block>` store + its lock note (#96) |
| `claude-replay-html/src/html_export/{mod,bundle,serve,record_store}.rs` + `src/html/` | HTML render core · offline writers · live server · the wire-projection `RecordStore` (#74) · embedded CSS/JS |
| `src/jdi/` | the `agent-jdi` supervisor (see `src/jdi/DESIGN.md`) |

[`Block`]: ../claude-replay-engine/src/model.rs
[`Message`]: ../claude-replay-engine/src/engine/message.rs
[`Session<BV = Block>`]: ../claude-replay-engine/src/engine/session.rs
[`BlockAccess`]: ../claude-replay-engine/src/engine/session.rs
[`BlockStore`]: ../claude-replay-engine/src/engine/session.rs
[`SessionIndex`]: ../claude-replay-engine/src/engine/index.rs
[`Metrics`]: ../claude-replay-engine/src/metrics.rs
[`FollowParser`]: ../claude-replay-engine/src/follow.rs
[`SessionAccumulator`]: ../claude-replay-engine/src/engine/builder.rs
[`SessionCache`]: ../claude-replay-present/src/cache/mod.rs
[`DurableStore`]: ../claude-replay-present/src/cache/shared.rs
[`pull`]: ../claude-replay-present/src/pull.rs
[`PullClient`]: ../claude-replay-present/src/pull.rs
