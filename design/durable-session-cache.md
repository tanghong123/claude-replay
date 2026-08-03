# Design: a durable, cross-run session cache

> **Status: v18 — all seven requirements MET; §3.1's emission protocol is BUILT (see the note
> there).** v18 replaced the two-offset resume with one offset, which deleted §4.1's
> suppression rule and shrank §9's validity window; each decision is now stated once, in final
> form, with superseded reasoning condensed into Appendix A rather than left inline as a
> correction. Read §1 → §11 in order; the appendix is history and exists so rejected shapes are
> not re-proposed.

## 1. Requirements

1. **Agent-neutral.** The record format and `checkpoint`/`resume` live in
   `claude-replay-engine` (the accumulator cannot name a `present` type); the cache, the
   lock and metadata restore live in `claude-replay-present`, which re-exports the format.
2. **Both presentations**, TUI and HTML.
3. **The transcript is parsed once per presentation**, amortised across invocations.
   Locking is `<presentation, session>`, so duplicating the parse and the metadata store
   per presentation is fine — two concurrent frontends are two invocations.
4. **Every existing `SessionCache` benefit preserved** — keyed residency, TTL reaping,
   delta reads, tier-b spill, `Arc` sharing, the pull protocol.
5. **Metadata construction must not depend on the choice of `BV`.**
6. **The cache makes frontend state management easier, not harder.** No per-block
   sidecars, no frontend bookkeeping to work around a restrictive API. **Acceptance
   measure:** after both frontends move onto the cache, neither shows a significant LOC
   increase. A frontend growing bookkeeping code means the *cache API* is wrong.
7. **Records are sized to the change.** One changed number is one number on the wire —
   never a whole snapshot. Governs the record *format*, not just its frequency.

## 2. This mechanism already exists; we are extending it

The HTML frontend already emits exactly the stream this design needs: block records plus a
`{t:"meta", …}` record: the served `/pull` path builds it with `assemble_meta` from the
maintained `SessionMeta`, `Metrics` and `TaskList` (`html_export/mod.rs:1281`), and
`--dump-all-html` writes the same wire shape to disk as line 1 of each stream file from the
block-scan assembler `agent_meta` (`bundle.rs:132-159`). Two assemblers, one record shape —
which is what the existing oracle test pins. There is even an existing oracle test,
`assemble_meta_equals_agent_meta_oracle` (`mod.rs:1511`), asserting that meta assembled
from maintained state equals meta derived by folding blocks.

Three deltas turn that into a durable cache:

1. **Emit the meta record from the fold, not per pull** — stamped with the committed block
   it corresponds to and the transcript offset to resume at.
2. **Carry the fold's session-spanning state in it** (§4) — today's record carries what a
   page needs to render, not what a fold needs to continue.
3. **Persist both streams to a durable location**, one writer per
   `<presentation, session>` (§7).

`SharedSession::hibernate`/`restore` is the in-process ancestor of this and is replaced by
it: `Body::Hibernated` is **deleted**, and `restore` yields a `Body::Live` whose
accumulator is pre-seeded — so `advance`, `poll_view` and `pull` work unchanged and the
byte gate covers one path instead of two.

## 3. The model

Two append-only streams per `<presentation, session>`:

```
content   # TUI: JSON-encoded Block per committed block   (Bv = Arc<Block>, write-through)
          # HTML: rendered wire record per committed block (Bv = locator)
meta      # delta records, each stamped {committed_id, transcript_offset}
```

**Commit is the unit.** A block commits when a later turn begins; the fold only commits
when the queue is empty. A checkpoint is taken at a commit — but **not every commit is a
valid resume point**: one line can produce both a committed block and the block that opens
the still-open turn (Appendix A1's probe: `committed_len` 0 → 2 on one line). So:

> **Checkpoint guard.** Take a checkpoint only when **every block the current line authored
> is still in the open window** — precisely, when the logical index at the line's start
> equals the post-drain `base`. One O(1) comparison off the deque; this is what
> `line_boundary()` returns.
>
> Read it as "authored blocks that committed", **not** "caused a commit": the second reading
> would never checkpoint at all, since the user line opening turn *n* is also the line that
> commits turn *n−1*. Stated affirmatively: **the guard holds at every ordinary commit** and
> skips only the multi-turn-on-one-line case of Appendix A1. §10's zero-parsed-lines
> assertion catches the wrong reading.
>
> **What the skip costs: nothing observed.** Re-probed at v18 — a line carrying user texts
> A, B, C commits A and B and leaves C open, so the frontier really does cut inside a line and
> the guard really is needed. But measured across 860 local transcripts (131 Claude/Codex, 729
> QoderWork), **no line carries two genuine prose user-text items**: all 225 multi-text lines
> are `<system-reminder>` + one prompt, which yields one turn block. So the guard skips a
> checkpoint on no observed line, and the alternative — stamping each anchor with a
> `skip_n` "discard the first *n* blocks this line authored" — would add a field to every
> anchor for a case that has never occurred. Keep the guard; revisit only if an agent starts
> emitting the shape.

### 3.1 When the replayer emits meta records

> **Built.** This section is implemented and pinned by tests ahead of the rest of the cache —
> `claude-replay-engine/src/engine/meta_stream.rs` (the protocol, `emit_batch`, and the reader
> half `replay_meta`/`replay_agent_ids`), wired at the replayer's drain, with the oracle in
> `claude-replay-agents/tests/engine_integration.rs`.

For each batch of regular records emitted, the replayer emits **0–2** meta records such
that:

- **(A)** every meta delta caused by those regular records is captured; and
- **(B)** if the batch contained a committed block, a meta record lands **immediately after
  the last committed one**, capturing all deltas since the previous meta record.

| batch | meta records |
|---|---|
| no commit, no meta change | **0** |
| no commit, meta change | **1** — at the end, *unanchored* |
| commit is the last record | **1** — right after it, **anchored** |
| commit mid-batch, no change after it | **1** — right after the last commit, **anchored** |
| commit mid-batch, changes after it | **2** — anchored after the last commit, then unanchored at the end |

**The two kinds combine DIFFERENTLY** — this is load-bearing, and it is forced by how the
accumulator already maintains the same value. `SessionAccumulator::session_meta` accumulates
each committed block into `committed_meta` exactly once, then folds the open turn **freshly on
top** every time it is asked. The stream must mirror that, or the two derivations disagree:

- **Anchored** records carry an *incremental* delta of blocks that **committed**. They
  **accumulate** — apply every one, in order, exactly once.
- **Unanchored** records carry a *full restatement* of the still-open turn's contribution
  relative to the most recent anchor. They **supersede** — only the latest one after the last
  anchor counts, and an anchor voids every restatement before it.

```
committed_meta = Σ (anchored deltas, in order)      // accumulate
live_meta      = committed_meta + latest unanchored  // supersede
```

Reading an unanchored record as incremental **double-counts**: a block restated while
provisional is counted again by the anchored record that later commits it. Note the corollary
for the writer — a commit must **reset** the "already restated" baseline, or when the new open
turn's contribution happens to equal the one just committed (two consecutive bare prompts do
exactly this) the writer sees "no change" and emits nothing, and the reader is left showing
committed-only state while a turn is open.

**Only anchored records are resume points**, and that is the whole point of rule (B): a
record sitting immediately after a committed block describes state *as of that committed
block*, with nothing from the still-open turn leaked into it. So the
`{committed_id, replay_from}` stamp lives on anchored records only.

**Unanchored records are never resumed from.** They describe provisional-turn
contributions, which the re-read rebuilds — applying them would double-count (§4.1). They
exist so a live reader's meta stays current between commits and so the stream can be
replayed to "now" without the transcript.

**Consequently, on load: replay meta records up to and including the anchored record for
`n`, then truncate the stream there.** Leaving a trailing unanchored record in the file
would let a later load apply it out of order, ahead of records computed relative to `n` —
the same hazard as untruncated content (§3, below).

**One offset — `replay_from`, the line that produced `out[0]`, i.e. where the open turn
begins.** Reading resumes there and the fold runs normally from there to EOF.

An earlier revision carried a second offset, `resume_at` (just past the commit line). It was
never a second *read-start* — reading began at `replay_from` either way. It meant "the offset
the persisted cumulative state is current to", and it existed only because the checkpoint was
written at the commit while the open turn began earlier, which forced a rule suppressing the
cumulative folds across `[replay_from, resume_at)`. **Capturing the cumulative state as-of
`replay_from` instead of as-of the commit collapses that span to empty**, so one offset
suffices and the suppression rule is deleted outright (§4.1). This is not a new mechanism:
§4.1 already did exactly this for `prev_ts`/`pending_ts` ("as of the line's start") for exactly
this reason — v18 stops special-casing timestamps and treats every cumulative fold the same
way. See Appendix A3 for why the two-offset shape is not re-proposed.

The capture is cheap because `out[0]` after a drain is **always** a `UserText`/`Command`
(`replay.rs:428-449`: the frontier is the last turn boundary, and the `last_skill` cap
`rposition`s back to a boundary too). So the snapshot is needed only at turn-boundary lines,
not at every line — §4.1 gives the mechanism and §8 the cost.

**Alignment.** On load, `n` = the largest meta-stamped `committed_id` **≤ the number of
committed blocks actually loaded**. The loaded `BV` vector's length **is** the authority:
there is no separately persisted committed count that could disagree with the data. Because
anchors are **dense** (one per commit, per the protocol above — not one per periodic checkpoint),
`n == committed.len()` exactly, unless a partial tail write left the two streams uneven.
No look-ahead buffering is needed: anchored `committed_id`s are monotonic, so the scan is just
`max { committed_id ≤ committed.len() }`, and the reader is already implemented as
`replay_meta(&records, Some(n))`.

Then **physically truncate to `n`** before appending anything, and discard a trailing record
that is unterminated or unparsable. Truncating the in-memory `Vec` is **not enough** — the
store's backing must be truncated too, since `RecordStore` serves committed bytes as one
contiguous range to EOF (`serve.rs:325-329`), so orphaned bytes past `n` are range-read as
garbage. The split is clean: the accumulator computes and returns `n` (§4.2); the persistence
layer does the `set_len` on both streams.

**A mid-run reset invalidates both streams.** A truncation/compaction drives
`advance_from_source` → `builder.reset()` → `BlockStore::reset()`, and `RecordStore::reset`
does `set_len(0)` on the content log (`record_store.rs:160-167`) — the meta stream has no
such hook. Left alone, content regrows from 0 while meta still holds checkpoints stamped
against the old bytes, and the next open restores a stale checkpoint against different
content: the "false accept ⇒ wrong output" class §9 calls the one to guard hardest, reached
without any rewrite-detection failure. **The writer subscribes to the same reset signal:
truncate both streams to 0, bump `epoch`, drop the pending checkpoint.** The poison path
does the same — step 3 narrows the branch to `poisoned()`, and `open_fresh` must discard the
durable directory, not just the in-process store.

Truncation is not optional. HTML serves committed bytes as one contiguous range from a
locator to EOF (`serve.rs:325-329`), so abandoned records left in place would be range-read
as garbage; and a sequential meta replay would apply abandoned deltas *before* the new ones
that were computed relative to state at `n`, double-applying every increment.

## 4. What the meta record carries

**Only state that spans turns.** Everything scoped to the open turn is rebuilt by
re-reading it from the offset, which costs one turn:

| rebuilt by re-reading | persisted in the meta stream |
|---|---|
| `out`, `base` — the open window | `agent_ids` — never pruned; a completion resolves a spawn many turns back |
| `tool_slot`, `suppress`, `last_skill` — pruned to the open window at the drain | `user_times` + `stamped` |
| `queue` — see the invariant below | `cwd`; `prev_ts`; `committed_meta`; the task fold **including `pending`**; the metrics blob (§5); **`epoch` and `provisional_gen`** |

**The queue is not persisted, and the justification must be stated at `replay_from`, not at
the commit.** "Empty at a commit" is true at the commit line but *not* necessarily at
`replay_from`, which is where the replay actually starts. It is still sound for a stronger
reason: `out[0]` is by construction the last
user-turn boundary in the window (or the `last_skill`-capped one, and the cap re-derives the
same boundary during the replay, `replay.rs:429-436`), so no drain and no marker suppression
can differ between the replay and the original fold.

### 4.1 The third class — state that is persisted AND re-advanced

A two-outcome rule ("pruned ⇒ rebuild, spans turns ⇒ persist") is **not sufficient**, and
missing this is what sank the shape in Appendix A2. `advance_at` folds metrics, the task
op-log and `user_times` for **every** line (`builder.rs:116-164`) — including every line in
the re-read span. So state that is persisted *and* re-advanced by the replay is
double-applied: with the `last_skill` pin the open window can span several turns of
assistant lines carrying `usage`, and Claude's accumulator **sums** them
(`agents/claude/metrics.rs:41-45`).

**The rule.** **Capture every cumulative fold's state as of the start of the `replay_from`
line**, not as of the commit. Then the replay re-reads from `replay_from` and folds
**normally, suppressing nothing** — each line in the re-read span is applied exactly once,
because the restored state predates all of them.

The rejected alternative was to keep capturing at the commit and suppress the cumulative folds
across `[replay_from, resume_at)`. It is not merely clumsier, it re-opens this very section's
bug class: a suppression list must be extended by hand for **every** cumulative fold ever added
to `advance_at`, and forgetting one is silent — double-counted metrics, not a crash. Moving the
capture point removes the class instead of enumerating it. (Appendix A3.)

Two folds need no special handling either way, because they are idempotent: `cwd` is
first-non-empty-wins in both agents, and `agent_ids` is an upsert (`replay.rs:127-137`).

There is one cumulative fold in `advance_at` that must be named even though it cannot fire: the
drain itself (`committed_meta.push` + `store.put` + `committed.push`, `builder.rs:150-159`). If
it fired during the replay it would append duplicate content records and double-count the
restored `committed_meta`. It is unreachable **because of the §3 checkpoint guard plus the
`last_skill` cap** — but that dependency is exactly the kind left unstated in Appendix A2, so:
`debug_assert!` that `committed_len` is unchanged across the resume, and treat a violation as a
validation failure ⇒ cold rebuild. Keep that assert regardless of the capture rule; it is cheap
and it is what catches a mis-derived offset.

`user_times` is restored **truncated to `committed_meta.turns`**, with
**`base = stamped = 0`** and `out` empty; the replay re-stamps. That truncation is not a
special case — it *is* `user_times`' as-of-`replay_from` value, since at the start of that line
the open turn's first `UserText` does not yet exist. Note `stamped` lives in
**raw-logical** space (the same space as `base`, `replay.rs:104`) while `committed_len()`
counts **finalized** blocks (`builder.rs:204-206`) — `coalesce_spans` collapses runs, so the
two differ. Setting `stamped = committed_len` would make `window_stamped()` exceed
`out.len()` and the first `LineStart` slice out of range — the #56 panic, on the first
resume of any session with a committed prefix. `base`'s absolute value never escapes the
replayer (the only leak is `patch_floor`, whose consumers test `is_some()` only), so
rebasing to 0 is safe. (No `UserText`/`Command` is ever suppressed —
`suppress` only holds `QueueEvent` markers — so `committed_meta.turns` is exactly the
committed user-turn count.)

`prev_ts` is persisted because it is neither pruned nor empty at a commit: without it a
`Thinking` on the first re-read line renders `duration_secs: None` where a cold fold gives
`Some`.

`epoch`/`provisional_gen` are persisted so a browser holding a cursor across a restart
resyncs. Without them a resumed session starts at `(1, 0)`; a cursor at `committed_id 500`
against `n_committed = 480` yields no payload and no resync, and `PullClient` silently
keeps 20 blocks the server no longer has. **On a truncate-back, bump `epoch`** so every
outstanding cursor resyncs rather than stalling.

**The tracking mechanism.** Everything above is captured *as of a line's start*, and which line
is `replay_from` is only known after the drain, so the value must be recorded before it is
needed. Two tiers, because the costs differ:

- **Scalars** — `(logical_index, line_offset, prev_ts, pending_ts)` — are copied into a
  single **scratch slot at every line's start**. Four words, no allocation.
- **Cumulative blobs** — the metrics accumulator and the task fold — are cloned into that slot
  **only when the previous line actually mutated them**, tracked by a dirty flag. Most lines
  touch neither: metrics moves only on a line carrying `usage`, the task fold only on a
  `TaskOp`.

When a line appends a turn-boundary block (`UserText`/`Command` — by §3.1 the only thing
`out[0]` can be), the scratch slot is promoted into a small deque; the drain prunes entries
below `base`. So the deque holds one entry per *turn* in the open window, not per line, and
`line_boundary()` (§3's guard) is a comparison against the scratch slot.

Why as-of-line-start rather than as-of-checkpoint, in the concrete: persisting `prev_ts` at the
checkpoint instant is right only when `replay_from` **is** the commit line. In the pinned-drain
case it restores `ts(commit_line − 1)` where a cold fold has `ts(replay_from − 1)`, and that
propagates whenever a later line carries `LineStart(None)`. The same argument generalises to
every cumulative fold, which is exactly why v18 captures them all at the same point.

**Requirement 7 — every record carries only what changed.** Scalars appear only when they
change. Collections need **ordinal**, not id-keyed, deltas: `SessionMeta.children` keeps
duplicate ids deliberately (`session.rs:297-303` — "a map lookup would collapse
duplicates") and `TaskFold.pending` pushes without dedup and removes the first positional
match (`tasks.rs:120-131, 185-207`), so an id-keyed upsert is lossy.

**But that is right for *add* and wrong for *done*, and an ordinal `done` is not even
computable** — an `AgentDone` in one batch can match a child appended many batches earlier, and
a delta has no view of the accumulated list. The duplicate-collapse worry is answered instead
by what the op *does*: `ChildOp::Done(AgentId)` clears **every** child carrying that id, which
is exactly `SessionMeta::push`'s linear scan (`session.rs:300-302`), duplicates included. So:
**`Add` is positional, `Done` is id-keyed and clears all matches.** As built in
`engine/meta_stream.rs`; apply the same split to `TaskFold.pending` when it is written.

`agent_ids` is an idempotent upsert and `user_times` is append-only, so both are simple.
The metrics blob is the one **stated exemption**: §5's seam returns an opaque snapshot, and
it is O(1) — five counters, a model string and two span endpoints — so it is written whole,
or shallow-key-diffed by the writer if that proves worthwhile.

*Accepted consequence:* a load replays the stream from the start — O(records), far below
O(lines). If that ever dominates, the fix is a periodic absolute record **within the same
stream**, not a format change.

**The record has three layers, because its contents do.** It must carry engine fold state,
an **agent**-specific metrics blob (§5), **present**-layer counters (`epoch`,
`provisional_gen`) and an HTML-specific `EmitState.turns` delta (§6) — while §1.1 puts the
format in the engine, which may name none of those. So:
`{stamp, engine: {…}, present: {…}, store: Value}`, with the last two opaque to the engine
and filled by present and the frontend. Precedent is already in the tree:
`PersistentStore::hibernate_state`/`restore_state` (`shared.rs:568-572`).

**Requirement 5** holds because the record contains no `BV` of any kind: metadata restore
is one implementation, identical for every presentation, and the reader is a free function
with no type parameter. If it ever needs a `::<S>` turbofish, the requirement is lost.

### 4.2 The accumulator seam — two input streams, no persistence knowledge

The accumulator should not learn about files, locks or directories. It should learn one new
thing: that it can be **started from a prior state** instead of from empty. So the restore is a
constructor taking the two loaded streams as plain values, exactly the shape a cold start already
has (`with_store`):

```rust
pub struct Restored<S: BlockStore> {
    pub acc: SessionAccumulator<S>,
    pub committed_id: usize,     // where the two streams agreed
    pub replay_from: ByteOffset, // the ONE offset (§3.1) — feed advance_at from here
}

pub fn restore(
    adapter: &'static dyn TranscriptAdapter,
    store: S,                  // already holding its loaded backing
    committed: Vec<S::Bv>,     // from the BV loader
    records: &[MetaRecord],    // from the meta record loader
) -> Restored<S>
```

The division of labour that makes this work:

- **The loaders** know formats and paths. They produce a `Vec<BV>` and a `Vec<MetaRecord>` and
  nothing else. Each frontend has its own `BV` loader (that is the only per-frontend piece);
  the meta loader is shared.
- **The accumulator** owns alignment, because it owns `committed`: it takes
  `n = max { committed_id ≤ committed.len() }`, truncates its vector to `n`, seeds itself with
  `replay_meta(records, Some(n))`, and returns `n` plus the offset to resume from. It touches no
  file and decodes no `BV` — so this is ONE implementation for every frontend, which is what
  requirement 5 asks for.
- **The persistence layer** takes the returned `n` and does the `set_len` on both streams and the
  store backing. It is the only thing that writes.

Note what this buys beyond tidiness: alignment becomes a **pure function of two vectors**, so it
is testable with hand-built inputs and no filesystem — including the disagreement cases (meta
ahead of content, content ahead of meta, a torn tail record) that are otherwise reachable only by
crashing a real writer at the right instant.

## 5. The one seam addition

`MetricsAccumulator` is `push`/`finish` with no seed (`adapter.rs:24-34`), and the
collapsed `Metrics` cannot restore an accumulator: both agents hold private span
endpoints, and Codex's `model` comes from a `turn_context` line near the session start
(`codex/metrics.rs:35-39`) — without it a resumed run reports a wrong duration and no cost.

Mirror the pattern already used for stores (`PersistentStore::hibernate_state`/
`restore_state`, `shared.rs:565-572`) — opaque, defaulted, agent-specific in each impl:

```rust
fn checkpoint(&self) -> serde_json::Value { Value::Null }
fn restore(&mut self, _v: serde_json::Value) {}
```

Needs `TimeSpan` to derive serde (private fields, `metrics.rs:63-66`; already re-exported
via `seam.rs:45`). QoderWork shares Claude's accumulator, so the blob is keyed by
presentation **and agent id**. Note `claude-replay-agents` has `serde_json` but not
`serde`; either add the dep (the seam audit only flags `claude_replay_engine::` paths) or
hand-roll with `json!`.

## 6. Requirement 6 — what the frontends must NOT have to do

The cache absorbs state-keeping; frontends stay thin. Concretely:

- **No per-block sidecar.** The `BV` table and the render continuation belong to the store,
  reached through the existing `hibernate_state`/`restore_state` hook — not frontend code.
  **`EmitState` is split**, because writing it whole would violate §1.7: its growing part
  (`turns`, the sidebar index — one entry per user turn) rides the **meta stream** as a
  per-turn delta, which is exactly the record cadence; its O(1) part (`next_block`, `turn`,
  `seen_turns`) stays in the store blob and is written at each checkpoint. Persisting the
  whole struct per commit is O(turns) bytes per commit; persisting it only at eviction means
  a crash restores `EmitState::default()` and the next `put` renders restarted anchors and
  turn numbers into a file clients range-read — corrupt output, not a no-op.
- **One admission call, not a lock protocol.** The frontend calls the cache and gets back
  `Admission { session, cached, holder: Option<Holder{pid, dir, port}> }`. Lock
  acquisition, the port probe, validity checking and the uncached fallback all happen
  **inside** the cache. The frontend's entire lock-related code is then the *message*: the
  TUI's refusal text, or HTML's redirect to `holder`'s `…?session=S`. That is irreducible —
  the cache cannot print a TUI message or issue an HTTP redirect — and `cached` is what a
  multi-root run uses per session, so no frontend bookkeeping is needed.
- **The frontend still supplies a store factory.** It must: only HTML knows its
  `FoldPolicy`/cwd/flavor and only the TUI knows it wants `Arc<Block>`. That is one closure,
  as today (`serve.rs:254-271`), not per-block state.
- **Checkpoint scheduling is the cache's**, with one exception the frontend cannot delegate:
  the TUI skips destructors via `process::exit(0)` (`app.rs:57, 93`) and replaces its cache
  on `Outcome::Switch` (`app.rs:373-376`), so it calls an explicit checkpoint + release at
  those three points (§11 step 8).
- **Tier-b spill (§1.4) is preserved as a *seam*, not as a user.** `TierBStore` is already
  production-vestigial — constructed only in tests, surviving as `SessionCache`'s default
  type parameter, while HTML uses `RecordStore` and the TUI `ArcStore`. What R4 preserves is
  `PersistentStore`, whose successors are `RecordStore` and step 4's durable store.
- **The `aux` slot stays view state.** The TUI's `ViewSidecar` is derived and
  width-dependent; it must never enter the meta stream.

Acceptance is the LOC measure in §1.7, taken per frontend before and after.

## 7. Locking

One lock per `<presentation, session>` — the artifact directory. **The session key is
`discover::session_id(path)` falling back to the file stem**; naming it matters because both
frontends currently use the bare stem (`html_export/mod.rs:1016-1021`, `app.rs:459-461`)
while the engine has a content-derived id (`engine/discover.rs:113-130`), and a durable
cross-project directory keyed on a stem can collide — the visible symptom would be a TUI
refusing to open while naming an unrelated pid. §9's first-line anchor is the backstop that
degrades a collision to a rebuild.

**The lock is held for the winning frontend's whole process lifetime**, independent of
residency: `pull_response` reaps and hibernates per request (`serve.rs:245-247`), so a
session can be evicted while its page is still open. Eviction drops residency only; the
lock is released at exit (§11 step 8). §9's GC rule depends on this.

Reclaim is liveness-based; a server holder is additionally port-probed, since pids are recycled, via
a callback injected by the frontend (only it knows its port, and a callback keeps the lock
testable).

| situation | behavior |
|---|---|
| free, or holder dead | take it; read + write |
| **TUI**, live holder | **quit**, naming pid, dir and `tmux attach` |
| **HTML**, held at pick time (single-root) | open the holder's `…?session=S` |
| **HTML**, multi-root start | acquire per session, independently; partial success is normal, never a refusal — sessions won are cached, the rest served uncached |
| **HTML**, child discovered mid-run, held | serve **uncached**; the page is open, there is no mid-run hand-off |

The lock governs **writing**, never **viewing** — which is what fits the multi-root server,
whose lock set is not knowable at startup because children are discovered lazily.

Refusing a second TUI is an improvement: today two instances each fold and hold the whole
session in RAM, silently, and `tmux attach` is the real sharing primitive. `--no-cache` is
a **hidden** flag (`hide = true`, precedent `jdi/mod.rs:164,167`) skipping the durable
cache and the lock — insurance for the cache path, not the routine way to force a second
TUI.

**Portability is a correctness gate.** `pid_alive` shells to `kill -0` and returns `false`
on non-unix (`jdi/state.rs:150-167`); where there is no real liveness check the cache is
**disabled**, never "assume stale", which would fail *into* concurrent writers. The same
fork/exec cost means eviction must not probe per candidate.

## 8. Overhead budget

| when | added work |
|---|---|
| **per line** | four scalars into a scratch slot (§4.1) — the byte offset `advance_at` already receives, plus the logical index and two timestamps. No hashing, no I/O, no allocation. On the minority of lines that mutate a cumulative fold (a `usage` line, a `TaskOp`), one small clone of that fold. |
| **per turn** | promote the scratch slot into the boundary deque; the drain prunes it. O(turns in the open window), not O(lines). |
| **per committed block** | HTML: none, it already appends a record. TUI: one `serde_json` serialize + buffered append. |
| **per commit** | one delta record — sized to the change (§1.7) — plus one bounded 64 KiB `pread`+hash for the validity window, and the O(1) part of the store blob (§6) |
| **per open** | `stat` + first line + 64 KiB window + replay the deltas + decode/scan the content stream — O(records + committed), far below parse+fold |
| **cache off / `--no-cache`** | exactly today's path. **Zero.** |

One rule, not an optimisation: checkpoints happen at commits, never per advance (a poll-driven
write would fire every `POLL_MS`, 2 s, per session).

**The per-line row is the honest cost of the one-offset resume** (§3.1). The earlier
two-offset shape kept this row at literally zero by capturing cumulative state at the commit
instant — and paid for it with §4.1's suppression rule, a hand-maintained list whose omissions
are silent. Four scalars per line and an occasional small clone is the better trade; it is
still no allocation on the common line and no I/O ever.

The TUI's per-block serialize remains the only genuinely new *steady-state* cost. **Measure
both** (§11 step 4) rather than assume them.

## 9. Validity

Reuse iff **all** hold, else rebuild: source length ≥ **`replay_from`**; the **first-line
anchor** matches; and a hash of the **fixed 64 KiB window ending at `replay_from`** matches.
Also: format version, **fold-logic version** and build id match; and, for HTML only, the
**flavor** — the render fingerprint (`FoldPolicy` + render cwd + record schema). Only the
**served** rendering is cached: the offline dump writers stay off the durable cache (§10), so
the flavor space is one, not three, and `BYTE-IDENTICAL: PASS` never depends on machine state.

**Why the window is fixed-size, and why `replay_from` is the right endpoint.** Everything the
resume restores — the committed blocks and the cumulative state — derives solely from bytes
**below** `replay_from`, so that is the only region a rewrite can silently corrupt. Bytes at or
after `replay_from` are re-read and folded fresh, exactly as a cold fold would, so a rewrite
there is **self-correcting** and needs no coverage. (Under the two-offset shape this was not
true: the folds over `[replay_from, resume_at)` were suppressed, so a rewrite inside that span
was not self-correcting and the window had to stretch to `max(64 KiB, resume_at − replay_from)`
to cover it. Deleting the suppression rule also deleted the variable-size window.)

Deliberately **not** a whole-prefix hash: `poll_resume` re-reads and hashes `[0, offset)`,
which on a 40 MB transcript is a full re-read at every open. A trailing window plus the
anchor catches shrinkage and any rewrite touching it; compaction rewrites content, so a
byte-identical 64 KiB immediately before the offset does not occur. Retain the full-prefix
hash as an opt-in paranoid mode. *Caveat:* a single line larger than the window collapses
it to a sub-line check.

The meta stream is flavor- and policy-independent; only the rendered content stream is
parameterised.

## 10. Testing

- **Equivalence:** cached vs cold, byte-identical, per presentation and flavor. The byte
  gate cannot see this today — `gate.sh:32-33` plus `verify.sh:26-30` drive `--dump`,
  `--dump --full`, `--dump-html` and `--dump-all-html`, none of which construct a
  `SessionCache` — so this needs a new harness. **And it must stay unable to see it:** §9
  gives the dump flavors cache entries, which would make `BYTE-IDENTICAL: PASS` depend on
  machine state. **Decision: the offline dump writers stay off the durable cache**, and
  their flavors are dropped from §9; only the served path caches. (Alternative, if they are
  ever wired: pass `--no-cache` in all five gate invocations and say so in
  `scripts/gate/README.md`.)
- **Re-invocation:** a second run of the same presentation parses **zero** lines below the
  resume point (assert a parse count). Assert metadata restored from the stream equals a
  cold fold's — the requirement-5 check, run for both presentations against one reader.
- **Alignment:** truncate either stream independently; load picks the largest
  meta-stamped id ≤ the content count, truncates both, and resumes correctly. Kill
  mid-write; recovery is a smaller `n`, never a rebuild. **A held browser cursor across a
  truncate-back resyncs, never stalls** (the `epoch` bump, §4).
- **Double-apply:** the pinned-drain fixture must produce a **fully identical block list**
  cached vs cold — not merely matching token totals, task state and turn timestamps, since
  `prev_ts` drift (§4.1) shows up only in rendered thinking durations. This is the check on
  §4.1's capture rule: it fails if any cumulative fold is captured at the commit instant rather
  than as of the `replay_from` line, which is the third-class bug in its current form. Use a
  fixture whose open window spans **several** turns (the `last_skill` pin), since a single-turn
  window makes the two capture points coincide and the test go vacuous.
- **Rejection:** rewritten prefix, changed fold/format version, changed flavor ⇒ full
  rebuild, never a partial serve.
- **Lock:** two writers; dead-pid reclaim; live-pid respected; live pid + dead port; the
  TUI refusal text; HTML pick-time hand-off; mid-run child uncached.
- **Requirement 6:** LOC delta per frontend, recorded in the PR.
- **Fixture shapes (required):** a linear transcript passes while badly broken. Include a
  **pinned drain** (queued prompt / skill), a **mid-turn typed prompt**, and a **late tool
  result**.

## 11. Implementation order

1. **Engine: format + accessors.** ~~The meta record type and its delta vocabulary~~ **DONE**
   — `engine/meta_stream.rs` plus `SessionAccumulator::drain_meta`, the writer half of §3.1,
   landed ahead of the rest with its oracle (see §3.1's note). Remaining here:
   `Replayer::{checkpoint, restore}` (its fields are private to `replay.rs`);
   `SessionAccumulator::checkpoint` returning fold state **+ the one offset**, and its inverse
   `SessionAccumulator::restore(adapter, store, committed, records) -> Restored` (§4.2) —
   naming them as a pair matters, since `restore` is what makes alignment a pure function of
   two vectors and therefore testable without a filesystem;
   a `committed_meta()` accessor (`session_meta()` is the merged value);
   `LineReader::open_at_offset` (**not** `open_at`, which routes into `poll_resume` and
   re-reads the whole prefix) and `line_boundary()` (§3's checkpoint guard);
   **`FollowParser::resume`** (does not exist yet) and **`TaskFold::{checkpoint, restore}`**
   (`pending` is private to `engine::tasks`, so even a sibling module cannot read it); `MetricsAccumulator::{checkpoint,
   restore}` + the `TimeSpan` derive. **The requirement-5 test lands here and gates the
   rest.** Byte gate unchanged.
2. **Present: the stream writer/reader**, replacing `hibernate`/`restore`, still
   temp-scoped. Restore must rehydrate every **derived per-tick baseline**, not just the
   fold: `epoch`, `provisional_gen`, `n_provisional`, `prev_provisional` (from the replayed
   open turn — `shared.rs:650` sets it empty on the comment "a hibernated body never
   advances", which step 3 invalidates; an empty baseline makes `prefix_intact` trivially
   true so a finalization reshape misses its gen bump and clients keep a stale prefix), and
   the follower's `prev_committed`/`prev_provisional` (`follow.rs:55-56`, else the first
   `poll_delta` reports `changed_from = 0` and re-renders everything, defeating the resume). Not a pure re-plumb: the raw `user_times` differs from the flushed vector
   the old hibernated body served, so restore routes through the public
   `FollowParser::resume` + `open_finalized()` path (`Replayer::open_snapshot` is
   `pub(crate)` and present cannot widen it).
3. **Delete `Body::Hibernated`;** `restore` yields `Body::Live`. Include
   `RecordStore::open_append` here — `reopen` leaves `cx: None` and its `put` panics, and
   `serve.rs:277` is production code, so deferring it breaks the tree. Narrow the
   `hibernation_stale() || poisoned()` branch to `poisoned()`, keeping #56's recovery.
4. **The TUI's durable `Arc<Block>` store** — `Bv = Arc<Block>` with a write-through
   `put`, so `poll_view`'s one-copy-shared-by-`Arc` property survives. The store is
   standalone and unit-testable here; step 5 is what routes it to the TUI. **Measure here**
   (§8): the per-block serialize is the only new steady-state cost.
5. **Cache API + the `poll_view` generalisation.** `poll_view` is implemented on the
   concrete `SessionCache<ArcStore, A>` and hardcodes
   `SharedSession::with_store(…, ArcStore)` (`cache/mod.rs:225-233`); it becomes
   `impl<S: BlockStore<Bv = Arc<Block>>, A>` taking the store factory introduced here — so
   the generalisation and the factory land together and step 4 no longer forward-references
   this step. Plus `shared_insert_or_get` (two-phase admission — `shared_session` runs its
   factory under the cache-wide mutex, whose contract is an O(delta) advance only);
   `reap_over_budget` returning its evictions so they can be checkpointed. `--no-cache`
   lands here so later steps are bisectable.
6. **Move the lock primitive to `present`**, retarget jdi. Independent of 1–5, before 7.
7. **Wire HTML** — durable dir split from the ephemeral bundle dir, per-session locks
   including the multi-root rule, checkpoint-on-commit.
8. **Wire TUI** — the non-follow path through the cache (today only `-f` constructs a
   `SessionCache`), and explicit checkpoint + lock release before both `process::exit(0)`s
   and on `Outcome::Switch`, since those skip destructors.
9. **Eviction/GC** — last; size cap (default 2 GiB, `CLAUDE_REPLAY_CACHE_MAX`) + 30-day age
   cap, LRU by mtime, skipping entries with a `LOCK` present without probing, and letting
   the age cap override that skip so a crashed holder cannot pin bytes forever.

**Rollout.** Additive **except** the deliberate TUI single-writer refusal (§7). Any
validation failure falls back to today's behavior; the case to guard hardest is a false
accept in §9, which yields wrong output rather than a no-op. Release: minor.

---

## Appendix A — three rejected shapes

**A1. Resume from a bare byte offset.** The commit cut is not a byte offset:
`finalize_completed` runs once per *line* (`replay.rs:428`) and one line can carry several
turns (probe: `committed_len` 0 → 2 on one line). The drain also fires later than the open
window starts, gated on `queue.is_empty()` and capped by the `last_skill` pin. And
`agent_ids` is never pruned, so re-folding from any non-zero offset emits
`AgentDone { agent_type: "" }` — session-long reach, so "move the frontier back" has no
finite answer.

**A2. Snapshot at the drain + composite frontier.** State captured at the drain (line D)
but re-read from the frontier (line L ≤ D) double-applies everything in `(L, D]`. Both
reviews found it unsound: `tool_slot` entries pruned at D orphan late results; the queue is
non-empty at L; `user_times` is one turn ahead at every drain; metrics are a mixed epoch;
`SessionMeta.children` is mutated by `AgentDone` so a suffix delta cannot express it.

**A3. Two offsets plus a suppression list** (v17, superseded by v18). Capture the cumulative
state at the commit (`resume_at`), re-read from the open turn's start (`replay_from`), and
suppress `metrics.push`, the `TaskOp` fold and `on_tool_result` across `[replay_from,
resume_at)` so the re-read does not double-apply them.

It is sound, and it was reviewed as such — the objection is that it is a **standing invitation
to §4.1's own bug class**. The suppression list must be extended by hand for every cumulative
fold ever added to `advance_at`, and an omission is silent: double-counted metrics, not a
crash. It also cost two things that vanish with one offset: a variable-size validity window
(`max(64 KiB, resume_at − replay_from)`, because the suppressed span is not self-correcting —
§9), and a second offset on every anchor whose name misled readers into seeing two read-starts.

**Why v18 avoids all three:** it resumes at a *commit* boundary — a line boundary by
construction, enforced by §3's guard — re-reads only the open turn, persists only state that
provably spans turns, and captures that state as of the line it resumes from, so the re-read
folds normally and suppresses nothing.

## Appendix B — removed upstream

`prev_user_text` and `delivered_rendered` were on the persisted list until #97 removed them
from the codebase entirely: measurement showed the dedup they implemented fired on genuine
resubmissions (3 of 1859 enqueues, 3s–6m30s apart) and hid them. Shipped in v1.30.0, so the
cache never has to carry them.
