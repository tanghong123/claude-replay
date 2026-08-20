# Design: incremental consumption — `LineReader` + `SessionBuilder` + `SessionCache`

> **Status:** SUPERSEDED — both layers exist in the engine (`engine/reader.rs`'s `LineReader`,
> `builder.rs`'s `SessionAccumulator`), delivered by the parser-engine refactor
> ([parser-engine.md](parser-engine.md), M1–M16 built). Kept for the original motivation and
> API exploration; the engine docs are the record of what actually shipped.

## Motivation

Today the engine offers two entry points at opposite extremes:

- **`parse_session(path)`** — eager. It *streams* internally (one line resident, no whole-file
  `Vec<Value>`), but at the API level it runs to EOF and hands back a finished [`Session`]. A
  consumer cannot stop early, feed it from a source it controls, hold a partial, or resume.
- **`FollowParser`** — incremental, but purpose-built for live tailing: `poll()` returns a raw
  `(blocks, user_times, metrics)` triple (not a `Session`), and on restart it re-reads and
  re-folds the whole file — there is no way to resume from a saved position.

That restart gap matters once transcripts get large (10s–100s of MB) **and** the consumer
builds expensive derived state per block (indexes, embeddings, summaries): a crash forces a
full re-fold *and* a full client-state rebuild, the latter often costing more than the file
itself. The fix is to make incremental consumption a first-class, resumable capability.

The key realization: the fold is **already** message-iterative internally (the `Replayer`
folds line-by-line; `FollowParser` folds deltas). We are not building a new engine — we are
exposing the incremental construction that already happens, and giving it a resume story.

Two core layers, plus a helper that already exists:

1. **`LineReader`** — a resumable line source (rename + extension of today's `TailReader`).
2. **`SessionBuilder`** — folds a line source into an evolving `Session`, checkpointable.

Both are **core** (new capability). A lazy, keyed, self-tailing *store* of `Session`s (for
materializing sub-agents on demand) is **convenience, not a new type** — the existing generic
`engine::store::SessionStore` + a `FollowParser` already is it; see
[Layer 3](#layer-3--lazy-session-store-reuse-sessionstore-dont-add-a-type-task-21). This keeps
the core-vs-helper discipline the rest of the engine follows.

```text
source ──lines──▶ LineReader ──lines──▶ SessionBuilder ──▶ Session (snapshot)
                  tell()/open_at(pos)    advance() / snapshot() / checkpoint()
```

`parse_session` and `FollowParser` both become thin wrappers, collapsing three code paths
(batch, live, resumable-restart) into one primitive.

---

## Layer 1 — `LineReader` (task #18)

### Rename

Today's `TailReader` (`claude-replay-core/src/tail.rs`) is misnamed once it can start from an
arbitrary saved position — it's no longer only "tailing the end." And its `poll()` already
returns **complete lines** (`Poll { lines: Vec<String>, reset: bool }`, holding back a
trailing partial until its `\n`), so it is fundamentally a *line reader*.

- `TailReader` → **`LineReader`**; `tail.rs` → `reader.rs`.
- `open_at_start` / `open_at_end` keep their meaning (where to begin); `poll()` is unchanged.
- It is `pub(crate)` (only `FollowParser` + tests use it), so the rename is mechanical.

### New: a resumable cursor

Add a serializable cursor so a consumer can persist "where I stopped" and resume there,
instead of re-reading from 0.

```rust
impl LineReader {
    /// An opaque, serializable position at the current read point. O(1).
    pub fn tell(&self) -> Position;

    /// Resume reading at `pos`. Optimistic: the next `poll()` validates `pos` against the
    /// current file and either resumes (returns only the delta) or, if the position is
    /// stale/foreign, resets to 0 and returns the whole file with `reset: true`.
    pub fn open_at(path: impl Into<PathBuf>, pos: Position) -> Self;
}

/// Opaque resume token. Encodes where reading stopped plus enough identity to reject a
/// position from a different session/file and to detect that the consumed region was
/// rewritten (compaction). Serialize via `encode()` / `decode()`.
pub struct Position {
    offset: u64,          // resume here
    consumed_hash: u64,   // hash of bytes [0, offset) as seen at tell()  (rolling, O(1))
    anchor: u64,          // hash of the first line (session id / session_meta) — fast identity
    len_hint: u64,        // file length at tell() (quick sanity / truncation check)
}
```

### Two guards (why a bare `u64` offset is wrong)

- **Wrong-session / misuse** → `anchor`. The first transcript line carries identity (Claude's
  `sessionId`, Codex's `session_meta.payload.id`). `open_at`'s first poll reads just that line
  and rejects immediately if its hash ≠ `pos.anchor`. Also: `encode`/`decode` use a version
  tag (`crtail1:…`) so a garbage or mismatched-version token is rejected, not misread.
- **File changed / compaction** → `consumed_hash`. A head-anchor alone is insufficient: a
  compaction can keep the `sessionId` line, rewrite turns *inside* `[0, offset)`, and regrow
  past `offset` — so `len ≥ offset` and the anchor still matches, yet resuming at `offset`
  would fold onto rewritten bytes. So `open_at` re-reads the current `[0, offset)` and
  re-hashes it, comparing to `pos.consumed_hash`.

### Why the validation cost is acceptable

`open_at` re-reads `offset` bytes to validate, but only to **hash** them — never to parse or
fold. Hashing is memory-bandwidth-bound (sub-second even at 100 MB); the expensive work
(re-parse **and** client-state rebuild) is skipped entirely on a valid resume. So the trade is
"pay an O(offset) hash to save an O(offset) parse plus a potentially far larger client
rebuild" — worth it exactly when derived state is non-trivial, which is the premise.

`tell()` stays O(1): `LineReader` maintains `consumed_hash` as a **rolling hash**, updated on
each `poll()` over the bytes it already reads; `tell()` just snapshots the fields. Hashing
uses `std::hash::{Hasher, DefaultHasher}` — **no new dependency**. `Position` fields are
private; `encode() -> String` / `decode(&str) -> Option<Position>` are the only way in/out, so
it is genuinely opaque.

### Semantics — reuse the `reset` contract

`open_at` never errors and never corrupts: on the first `poll()` it validates and either
returns the delta (valid) or resets to 0 and returns the whole file with `reset: true`
(stale/foreign/rewritten). This is the **same signal** the reader already emits on a
truncation/compaction, so the consumer's loop is uniform:

```rust
if poll.reset { client.discard_state(); client.rebuild_from(poll.lines) }
else          { client.apply(poll.lines) }
```

A stale position is therefore never worse than today's behavior — it degrades to a full
re-read — and is faster whenever the position is valid.

### Residual limitation

None beyond the physical: `open_at` validation is as correct as the hash (any change in the
consumed region is caught). The only "cost" is the O(offset) validation read, which is the
deliberate trade above.

---

## Layer 2 — `SessionBuilder` (task #19)

### Shape

Expose the incremental fold as a pull-based primitive that produces an evolving `Session`:

```rust
pub struct SessionBuilder { /* agent, Replayer, cwd, metrics acc, … */ }

impl SessionBuilder {
    pub fn new(agent: Agent) -> Self;

    /// Fold more input (lines from a LineReader, an in-memory source, or a live tail delta).
    /// A `reset` from the source discards and rebuilds, matching FollowParser's behavior.
    pub fn advance(&mut self, lines: &[String]);

    /// A consistent `Session` for everything folded so far — blocks (results joined, turns
    /// grouped), the derived index, metrics, and cwd. Cheap to call repeatedly.
    pub fn snapshot(&self) -> Session;

    /// Bundle the source position with the fold state, to resume after a restart.
    pub fn checkpoint(&self, reader: &LineReader) -> Checkpoint;   // = (Position, fold state)
    pub fn resume(chk: Checkpoint) -> (Self, LineReader);
}
```

- `parse_session(path)` = `let mut b = SessionBuilder::new(agent); feed the whole file; b.snapshot()`.
- `FollowParser` = a `SessionBuilder` fed each poll's delta over a `LineReader` — so batch and
  live share the **one** line source, and `FollowParser` gains `checkpoint`/`resume` too. It
  can now return a full `Session`, not just the triple.
- The huge-transcript / expensive-state consumer = advance in chunks; `snapshot()` (or a
  lower-level per-advance block view) to process-and-discard; `checkpoint()` to survive a
  restart.

### Cold start from the middle — why a checkpoint is `Position` **+** fold state

`FollowParser` reads through the `LineReader`, so it *can* `open_at(Position)` and pick up from
the middle of a file — but resuming the **fold** is not the same as resuming the **reader**.
The fold is stateful across lines: a tool result back-patches onto a `tool_use` that appeared
earlier, thinking-turns group preceding tools, the queue lifecycle and the user-turn / metrics
counters all carry. So:

- A **bare `Position`** resumes only the raw line stream — a *tail-view* (blocks folded from
  the offset against an empty base): correct for a raw-line consumer, **wrong** for folded
  blocks (a result whose `tool_use` was before the offset becomes an orphan; grouping/counts
  restart).
- A correct mid-file resume therefore restores a **`Checkpoint` = `Position` + the `Replayer`
  state** (blocks-so-far, `tool_slot`/`pending`/`queue`/`completions`, counters, `cwd`, metrics
  accumulator). `resume(checkpoint)` = `LineReader::open_at(pos)` + rehydrate the `Replayer`,
  then continue — **byte-identical to never having stopped**.

The win is skipping the **re-read + re-decode** of a 100 MB prefix (and letting the client keep
its expensive derived state), not skipping the fold state — that state is exactly what makes
the continuation correct. (Serializing the `Replayer` couples the checkpoint to internal fold
fields; version the checkpoint format, and fall back to a full re-parse if it doesn't load —
the same `reset`-means-rebuild contract the `LineReader` already uses.)

### Three constraints that shape the API

These are not incidental — they determine that the unit is **advance → snapshot**, not a
`next() -> Block` iterator.

1. **Blocks aren't final until finalize, and results can precede their call.** The fold
   back-patches a tool result onto a `ToolUse` that may appear *later* in the stream, and
   `finish_turns` groups/coalesces at the end. So the builder cannot emit "final block N" the
   instant it reads line N. Forward-referencing results are held pending — resolved via the
   `scan_join_ids` pre-scan, or (across `advance` calls) the "a result physically before its
   `ToolUse` = a rewrite = `reset`" rule the follower already relies on. Hence `snapshot()`
   (a consistent projection), not a per-block pull.

2. **Expose `Block`s, keep `Message` internal.** Internally the builder consumes the L1
   `Message` stream, but `Message`/`QueueOpKind` are `pub(crate)` on purpose (the
   abstraction-level fix — consumers pin `Block`, never the L1↔L2 vocabulary). The builder's
   surface is `Session`/`Block`; it must **not** re-expose the raw `Message` stream, or we'd
   freeze the L1 contract we just hid.

3. **`index` + grouping are a finalize step → `snapshot()` is a cheap recomputed projection.**
   `SessionIndex::build` and `finish_turns` run over the block list; incrementally they are
   re-derived per `snapshot()` (cheap) rather than mutated in place. `Replayer::snapshot`
   already does exactly this for blocks/times — the builder extends it with the index + cwd.
   And the memory reality: streaming avoids holding all raw lines/`Value`s, but a *full*
   `Session` still holds every `Block`. The real memory win for a 100 MB transcript
   materializes **only** if the consumer processes-and-discards as it advances — which this
   API enables but does not force.

### What it unifies

`Replayer` (fold) already exists; `FollowParser` (incremental over a tail) already exists;
`LineReader` (resumable source) is Layer 1. `SessionBuilder` is the missing public seam that
turns "resume the bytes" into "resume the *parse*", and lets batch, live, and
resumable-restart share one implementation instead of three.

---

## Layer 3 — lazy session store (task #21)

> **SUPERSEDED (2026-07).** This section argued for "reuse `SessionStore`, no new type." That
> was reversed after auditing how the store is actually used: it has exactly one consumer, which
> bundles *presentation* state (`prev`) and *session* state (`follower`) into the generic `Res`
> slot — a coupling the genericity exists only to serve. The decision is now to **build a
> concrete `SessionCache` that owns parsed `Session`s and have the HTML server depend on it**,
> with `SessionStore` folded in as private machinery. See
> [`session-cache.md`](session-cache.md) for the current design. The text below is kept for the
> reasoning it records, but its conclusion no longer holds.

The earlier drafts of this design proposed a new `SessionCache`. On reflection **we don't need
one** — it would duplicate the generic `engine::store::SessionStore` that already ships and is
already the convenience helper for exactly this. `SessionCache` never added a capability; it
was only an opinionated wrapper, and the lazy-parse-then-tail behaviour isn't even in the
store — it's a few lines of caller composition over the *core* APIs:

```rust
// the whole "lazy open + tail" behaviour, from the core APIs (this is what serve.rs does):
let mut open: HashMap<AgentId, FollowParser> = HashMap::new();
let follower = open.entry(id).or_insert_with(|| FollowParser::open(agent, &meta.transcript));
let session = follower.poll()?;   // parses on first call, tails on later calls
```

So the plan is:

- **Keep `engine::store::SessionStore`** as the one (generic, `Info`/`Res`) residency helper.
  It handles the map + admit/evict + TTL reaping that the ad-hoc `HashMap` above lacks.
- **Provide lazy session loading as a documented composition**, with `html_export::serve.rs`
  as the reference usage (`SessionStore<AgentInfo, Tailer>` + `FollowParser`). A client that
  wants different policy (eviction, keying, eager vs lazy) writes the ~5-line composition
  itself against the core.
- **Only if `serve.rs`'s hand-rolling proves worth de-duplicating** — e.g. the sub-agent
  descend feature (#20) grows a second copy of it — factor the shared "open-or-tail a resident
  `FollowParser` keyed by id" step into a *small helper on/beside `SessionStore`* (a
  constructor or one method), **not** a parallel abstraction.

A flat `Session` (one transcript) references its sub-agents by id and carries only their
*paths* (see [`sub-agent-normalization.md`](sub-agent-normalization.md)); turning a path into a
live `Session` — lazily, and tailing on re-open — is the composition above, so no new type is
warranted.

### Shape

```rust
pub struct SessionCache { /* SessionStore<Meta, Resident> */ }

impl SessionCache {
    /// Cheap: record an id and where to load it from (a transcript path + artifact paths).
    /// No I/O — nothing is parsed yet. A whole `Session.sub_agents` map registers in one loop.
    pub fn register(&self, id: &str, meta: SessionSource);

    /// The current `Session` for `id`, parsing it on first request and **tailing new events**
    /// on every request after (via a resident `FollowParser`/`SessionBuilder`). `None` for an
    /// unregistered id; `Err` if the transcript can't be read.
    pub fn get<R>(&self, id: &str, use_session: impl FnOnce(&Session) -> R) -> Option<io::Result<R>>;

    /// Drop residents idle past a TTL (their `Session` is freed; a later `get` re-parses).
    pub fn reap(&self, ttl: Duration);
}
```

- `register` is the bridge from the metadata map: `for (id, sa) in &session.sub_agents {
  cache.register(id, SessionSource::from(sa)) }` — O(map), no parsing.
- `get` is where I/O happens, and only for ids the client actually opens. First call:
  `FollowParser::open(agent, meta.transcript).poll()` → a `Session`, admitted resident.
  Later calls: `poll()` folds only appended bytes and updates the resident — so repeat reads
  are O(delta) and reflect a still-running child.
- `reap` bounds memory: an unopened or long-idle child costs only its metadata.

### Access shape (a real decision)

Returning a borrow while the entry sits behind the store's lock and may be mutated by a tail
is awkward, so the API takes a **`use_session` closure** (like `SessionStore::with_resident`
today) rather than handing out `&Session` or forcing a clone. A client that truly needs to own
the value can clone inside the closure — cheap now that `Session` is flat (no nested child
`Session`s to deep-copy).

### Keying, and the root

Keyed by a session identity — the root registers under its own session id; children under
their `AgentId`. So the cache is general (it caches *any* session, not only sub-agents), and
the sub-agent map is just the most common source of registrations. This is the exact
generalization of `serve.rs`, whose `SessionStore<AgentInfo, Tailer>` already keys per agent
id under a root.

## Sequencing

1. **`LineReader`** (task #18): rename + `tell`/`open_at`/`Position`. Bounded, no new deps,
   immediately useful to a raw-line consumer. Foundation for everything else.
2. **`SessionBuilder`** (task #19): the incremental `Session` seam; refactor `parse_session`
   and `FollowParser` onto it; add `checkpoint`/`resume`.
3. **Lazy session loading** (task #21): **no new type** — reuse `SessionStore` + `FollowParser`
   (the composition `serve.rs` already uses) and pair it with the sub-agent metadata map
   (task #20) so children materialize on demand. Only factor a *small* shared helper if the
   descend feature would otherwise copy `serve.rs`'s logic.

All **design-only** until prioritized. They are output-preserving refactors of existing
machinery, so implementation is gated on the usual byte-identical checks (`--dump` /
`--dump-html` on frozen Claude + Codex transcripts) plus the follower's
`follow_matches_full_reparse` equivalence test extended to cover checkpoint/resume, and (for
the cache) a lazy-load + tail-update test mirroring `serve.rs`'s current behaviour.

See [`docs/architecture.md`](../docs/architecture.md) for the engine these layers extend
(§6 streaming parse & the live follower).

[`Session`]: ../claude-replay-core/src/engine/session.rs
