# Design: a durable, cross-run session cache

> **v20** — v19 re-reviewed against the code, every citation checked. Reuse a prior
> invocation's parse of a transcript. Read §3 first: the rest follows from it.

## 1. Requirements

| # | Requirement |
|---|---|
| R1 | Record format and state accessors live in `claude-replay-engine`; cache, lock and restore live in `claude-replay-present`. |
| R2 | Both presentations, TUI and HTML. |
| R3 | The transcript is parsed once per presentation, amortised across invocations. |
| R4 | Existing `SessionCache` behaviour preserved: keyed residency, TTL reaping, delta reads, tier-b spill, `Arc` sharing, the pull protocol. |
| R5 | Metadata reconstruction does not depend on the choice of `BV`. |
| R6 | No per-block frontend sidecars. Acceptance: neither frontend gains LOC. |
| R7 | A record carries what changed, not a snapshot. |

## 2. Model

Two append-only streams per `<presentation, session>`, written only at a commit:

```
content   TUI:  JSON-encoded Block per committed block   (Bv = Arc<Block>)
          HTML: rendered wire record per committed block (Bv = RecordLocator)
meta      a StreamHeader, then one MetaRecord per committing line — a flat list of optional fields
            · accumulate — what those committed blocks changed  (every record composes)
            · override   — fold state as of replay_from          (read only where you land)
            · indicator  — (committed_id, replay_from): resumable here
```

A block commits when a later turn begins and the prompt queue is empty. There is no periodic
snapshot, no third file, and nothing schedules a write.

**The override fields are in-band deliberately.** Only the record a load lands on reads them, so
a separate latest-only file would save the rest — ~270 bytes × commits, ~58 KiB per session. It
would also need its own alignment against the other two streams, and a crash between writing them
would leave the pair disagreeing. In-band, each commit is **one append per stream** and the last
complete record is by construction a consistent resume point — a torn tail costs one commit,
never correctness. The 58 KiB buys that.

## 3. The resume principle

> **A resume point is a pair `(offset, state)` such that folding the transcript from `offset`
> with the fold seeded to `state` yields exactly the live state a cold parse yields.**

Three questions follow, and their answers are the whole design.

**Which offset?** `replay_from` partitions the transcript: **bytes below it authored only blocks
that have committed; bytes at or above it authored only blocks that have not.**

It is defined by the bytes, not by the current shape of `out`. Blocks in the open window still
merge and mutate before they commit — a `ToolResult` joins its `ToolUse`, a coalesced run
collapses into one `Thinking{tools}` — so "the line that produced `out[0]`" names a block that
may not survive to commit in that form. The partition is stable under all of it: what a line
authored either committed or did not.

Nor is it "the start of the open turn". The open window spans several turns whenever the
`last_skill` pin caps the drain (`replay.rs:439-446`), so `replay_from` can precede the commit
line by more than one turn.

**Which state?** Everything the re-read does *not* rebuild. Fold state partitions exactly:

| fold state | disposition |
|---|---|
| `out`, `base` | rebuilt — the re-read reproduces it |
| `tool_slot`, `suppress`, `last_skill` | rebuilt — pruned to the open window at each drain |
| `queue` | rebuilt — provably empty at `replay_from`: an item resident there would have gated off every commit since its enqueue (`finalize_completed` returns while the queue is non-empty), so its marker could not sit below the partition |
| committed blocks | **content stream** |
| `committed_meta` | **meta stream**, as per-commit deltas |
| `agent_ids` | **meta stream** — never pruned; a completion resolves a spawn many turns back |
| `user_times` | **accumulate** — append-only |
| `cwd`, `prev_ts`, `pending_ts` | **override** — neither pruned nor empty at a commit |
| metrics accumulator, task fold | **override** — folded per line, so not rebuilt |
| `epoch`, `provisional_gen` | **override** (inside `present`) — a held client cursor must resync across a restart |

**When is the state captured?** As of the start of the `replay_from` line — never at the commit
instant. `advance_at` folds metrics, the task op-log and `user_times` for every line
(`builder.rs:116-164`), including every line the re-read covers. Capturing at the commit makes
those double-apply, since the commit line is at or after `replay_from`. Capturing at
`replay_from` makes the re-read apply each line exactly once and suppress nothing.

**Which commits qualify?** Those at which the partition **exists**. This is not a separate rule
— it is the definition's well-definedness condition. A line that authored blocks on both sides
of the frontier admits no offset: re-reading from its start re-produces committed blocks,
starting after it loses provisional ones. Two shapes do this:

- a **multi-turn line** — one line carrying several user texts commits the earlier turns while
  the last stays open (`committed_len` 0 → 2 on one line);
- an **attachment-first prompt** — a user line ordered `[image, text]` authors the `Attachment`
  block *below* its `UserText` (decode preserves item order, `claude/model.rs:655-668`), so the
  attachment commits while the turn stays open.

Either way the record carries no `commit` and a load falls back to the previous qualifying
record — the cost is re-reading one extra turn, never a lost cache. The next drain re-qualifies:
by then the whole straddling line has committed.

The partition is otherwise total: `finalize_completed` drains `out[0..k)` where `k` indexes a
turn-boundary block, so `out` retains `out[k..]` and is never empty after a commit
(`replay.rs:428-449`). A first uncommitted block therefore always exists, and its line's start
is `replay_from`.

## 4. The record

One record per commit: **a flat list of optional fields.** Absent always means "nothing from
this commit". Fields belong to one of three classes, and the classes are the whole format.

**Every field is optional, including the indicator. Absent means "no update".** The class fixes
what an update *is*:

| class | absent means | value at `n` | fields |
|---|---|---|---|
| **accumulate** | added nothing | fold of every present value in records ≤ `n` — numeric `+`, list append | `turns`, `tools`, `agents`, `user_times`, `store_grow` |
| **override** | unchanged | last present value in records ≤ `n` | `cwd`, `prev_ts`, `pending_ts`, `metrics`, `tasks`, `present`, `store` |
| **indicator** | not a resume point | — | `commit: Commit` |

```rust
struct MetaRecord {
    // accumulate — each field holds ONLY THIS COMMIT'S contribution, never the running
    // total. A drain normally commits one turn, so the two Vecs below are normally ONE
    // element (occasionally more: a `last_skill`-pinned drain, or a multi-turn line).
    turns:      Option<usize>,
    tools:      Option<usize>,
    agents:     Vec<AgentEvent>,               // sub-agents arriving/departing here, IN ORDER
    user_times: Vec<Option<EpochSeconds>>,     // timestamps of the turns THIS commit stamped
    store_grow: Vec<Json>,                     // frontend-opaque: the sidebar entries
                                               // (EmitState.turns) THIS commit added

    // indicator — present iff §3's partition exists at this drain (I5)
    commit:     Option<Commit>,

    // override — written when the value differs from the last one written (R7)
    cwd:        Option<String>,
    prev_ts:    Option<Option<EpochSeconds>>,
    pending_ts: Option<Option<EpochSeconds>>,
    metrics:    Option<Json>,                    // agent-opaque (§7); O(1)
    tasks:      Option<Json>,                    // TaskFold incl. `pending`; bounded by open tasks
    present:    Option<Json>,                    // engine-opaque: epoch, provisional_gen, n_provisional
    store:      Option<Json>,                    // frontend-opaque: EmitState's O(1) part
}

struct Commit {
    id:          usize,        // committed-block count after this drain
    replay_from: ByteOffset,   // §3's partition offset
    window:      u32,          // CRC32 of the 64 KiB ending at replay_from — see below
}

enum AgentEvent {
    Spawned(Spawn),
    Finished(AgentId),   // marks every spawn with that id finished — see the ordering note
}

struct Spawn {                  // everything known about one sub-agent at spawn time
    tool_use_id: String,        // the key a completion may arrive under, before agent_id exists
    agent_id:    String,        // empty until the spawn's result lands
    agent_type:  String,
    description: String,
    status:      AgentStatus,   // a spawn can be born terminal
}

/// `Json` is `serde_json::Value` throughout — an arbitrary JSON subtree the engine stores and
/// returns without inspecting. Spelled out because it is the only untyped thing in the format.
type Json = serde_json::Value;

struct StreamHeader {     // record 0, written once
    anchor:   u32,        // CRC32 of the transcript's first line (identity, not trust)
    versions: Versions,   // format, fold-logic, build; HTML adds flavor
}
```

**One ordered list of arrivals and departures, serving both consumers.** `SessionMeta.children`
(the menu view) and the `agent_ids` resolution table were separate fields carrying the same
`(agent_id, agent_type)` twice. They are one thing:

- **Menu view:** the `Spawned` events with a non-empty `agent_id`; `running` = `!status.is_terminal()`
  and not since `Finished`. Derived, not stored — a spawn can be born terminal, which is why
  `status` is in the record.
- **Resolution table:** every `Spawned` under **both** keys, `tool_use_id` and `agent_id`. Two
  keys because the id arrives late: a completion names whichever the agent emitted, and a miss
  degrades to `AgentDone { agent_type: "" }` (`replay.rs:359-361`). A spawn whose `agent_id`
  never arrives is not a child but must still resolve — which is why the consumers filter the one
  list differently rather than sharing a pre-filtered one.

**The order is load-bearing, so this is one list and not two.** `Finished(id)` clears every spawn
carrying that id, reproducing `SessionMeta::push`'s linear scan (`session.rs:300-302`), which
keeps duplicate ids deliberately (`session.rs:297-303`). Splitting into parallel `spawns` and
`spawns_done` lists would discard the interleaving and is **wrong**: for `Spawned(X)`,
`Finished(X)`, `Spawned(X)` in one record, block order yields `[finished, running]`, while
applying all spawns then all dones yields `[finished, finished]`. Duplicate ids are exactly the
case `SessionMeta` goes out of its way to preserve, so this is not hypothetical.

That is the whole reason for an event vocabulary: not to be general, but to keep order. Two
variants, no ordinals — a `Finished` matches by id, because it can refer to a spawn appended many
records earlier and a record cannot see the accumulated list.

Snapshotting the whole list as an override field would need no ordering at all, and at observed
sizes is free: 16 of 873 local transcripts have sub-agents, at most 5 each — 2.9 KiB against
1.0 KiB. Rejected because it is O(agents²): a fan-out workflow spawning 100+ pays ~1 MB, 1000
pays ~100 MB. The same question applies to `tasks`, which **is** an override snapshot; it is
bounded by open tasks rather than session history, so it stays — but if that ceases to hold it
belongs here for the same reason.

**`user_times` semantics.** *Folded across records*, one entry per **user turn**, in turn
order: `user_times[i]` is the timestamp of the *i*-th `UserText`/`Command` block in the display
stream — not per line, not per block. **In any single record it is the delta only** — the turns
that commit stamped, normally one entry. A 217-turn session emits ~217 one-element deltas
(≈2 KiB in total), never 217 copies of a growing array. `stamp_user_turns` (`replay.rs:679-691`) pushes one
entry as each turn-opening block is stamped, so `user_times.len()` tracks `SessionMeta.turns`
exactly, and HTML indexes it with `EmitState.seen_turns` while rendering.

`Option` because a turn can be **unstamped**: the value pushed is the fold's `pending_ts`, the
timestamp of the line currently being read, which is `None` when that line carried none. It is
never a "no turn here" hole — every user turn contributes an entry, some without a clock.

As a `+=` field the delta carries only the turns *this* commit stamped, appended in order, so
folding through record `n` reproduces the prefix exactly. That is also why restore truncates
it to `committed_meta.turns` (§6.3): at `replay_from` precisely the committed turns are
stamped, and the re-read re-stamps the open ones.

**The writer's obligation, and the only way to get this wrong.** An override value is the fold's
state as of *that record's* `replay_from`, and every record has a different `replay_from`. So the
writer must emit an override field on any record where its value differs from the last value it
wrote — otherwise "last present ≤ `n`" restores something measured at an earlier offset. This is
R7 applied to the override class: `cwd` is written once, `metrics` and the timestamps change at
every commit and so appear at every commit. The rule is uniform; the frequencies differ.

**Why `commit` is one field and not three.** `committed_id` is used only to align against
`|committed|` and to name the record a load stops at; both apply solely to resume points, so a
record without an offset never needs it. `window` is a checksum **of** `replay_from`'s bytes —
meaningful only alongside that exact offset, and "absent = unchanged" would be nonsense for it
(`replay_from` strictly increases, so it always changes). All three are one fact — "resumable
here" — not an invariant to maintain across fields.

**Why CRC32 and not a cryptographic hash.** This detects *accidental* divergence — a
compaction, a truncation, a different file under the same name — never tampering, and it
cannot be a trust boundary in any case: anything able to rewrite the transcript can rewrite the
cache beside it. A cryptographic digest would advertise a guarantee this design does not have,
and would pull a crypto dependency into a crate that has three. CRC32 is also 13× cheaper
(measured on 64 KiB: 1.6 µs vs 21.7 µs for sha256 — 0.35 ms vs 4.7 ms across a 217-commit
session, 3 ms vs 43 ms at 2000), though speed is the lesser reason: neither figure was a
budget problem. A ~2⁻³² false-accept chance is acceptable because the window is one of three
independent checks (§6.4) — length, first-line anchor, window — and the anchor alone catches a
different file.

**`store` vs `store_grow`.** Each opaque layer gets a slot in whichever class it needs. HTML's
`EmitState` splits: its O(1) part (`next_block`, `turn`, `seen_turns`) rides `store` (override);
its growing sidebar index (`turns: Vec<(anchor, label)>`, `html_export/mod.rs:716-722`) rides
`store_grow` (accumulate), one entry per user turn — written whole it would be a second
O(turns²) `user_times`.

**Three lifetimes, three homes.** Conflating them wastes space, and one case is asymptotically
wrong:

| lifetime | fields | home |
|---|---|---|
| constant per session | `anchor`, `versions` | **header**, once |
| accumulating | `turns`, `tools`, `agents`, `user_times`, `store_grow` | **accumulate class** |
| as-of-`replay_from` | `cwd`, timestamps, metrics, tasks | **override class** |
| as-of-the-write | `present`, `store` | **override class** — see I4's scope |

`user_times` is the case that matters. It is append-only and grows with turns, so writing it
whole per commit costs O(turns²) in bytes *and* serialization, and makes a load O(turns²). As an
accumulate field it is O(turns): folding through record `n` yields exactly its as-of-`replay_from`
value, because at that offset precisely the committed turns are stamped.

Measured at ~217 commits (the mean over 131 local transcripts): **253 KiB → 55 KiB** per session,
O(turns²) → O(turns). At 2000 turns, 15.9 MiB → 504 KiB.

**The meta stream is deliberately self-sufficient, not derived from the content stream.** The
TUI's content stream (JSON `Block`s) could yield `turns`/`tools`/`agents` by scanning at load —
but HTML's (rendered wire records) cannot, and R5 forbids metadata reconstruction that depends
on the `BV` choice. `user_times` is in no content stream at all.

The record names no `BV`, so restore is one implementation with no type parameter (R5).

**Why four fields are opaque JSON.** R1 puts the format in `claude-replay-engine`, which cannot
name a `present` type, a frontend type, or an agent's metrics state — and the metrics
accumulator is genuinely per-agent (`MetricsAccumulator`, `adapter.rs:24-34`: Claude and Codex
hold different span endpoints, and Codex discovers its `model` from a `turn_context` line). So
each layer hands the engine a blob it stores and returns without inspecting. Precedent already
in the tree: `PersistentStore::hibernate_state`/`restore_state` (`shared.rs:565-572`).

**What that costs, and the one open decision it forces.** `Json` is untyped: nothing
compile-checks that a layer's `restore_state` reads what its `save_state` wrote, and a blob
whose shape changed between versions would be silently misread rather than rejected. Today's
answer is the header's `versions`, which includes the **build id** — so *any* new binary
invalidates every cached session, which makes stale-shape misreads impossible.

That is safe and blunt. It also means the durable cache is **discarded on every release**, and
this project ships often (v1.31 and v1.32 landed a day apart) — so in practice a user would
re-parse from cold after most upgrades, which is much of the value gone. The alternative is to
drop the build id from `versions`, keep only format + fold-logic, and make each blob
**self-versioning**: `save_state` stamps a small integer, `restore_state` rejects anything it
does not recognise (⇒ cold rebuild for that session only). That keeps the cache across
upgrades that do not change the fold, at the cost of one integer and one match arm per layer.
**Decide before step 1** — it changes the seam's signature, and retrofitting a version into a
blob already on disk is exactly the migration this design exists to avoid.

## 5. Invariants

| # | Invariant | Enforced by |
|---|---|---|
| I1 | `n = max { c.id : r.commit == Some(c) ∧ c.id ≤ \|committed\| }`. The loaded `BV` vector's length is the sole authority; no committed count is persisted separately. | §6.2 |
| I2 | One record per committing drain (`finalize_completed` runs once per line, `replay.rs:412`, so a multi-turn line commits several blocks under ONE record). `commit` ids are strictly increasing and reach `\|committed\|` unless the tail is torn or the last drains failed I5. | §6.1 |
| I3 | `replay_meta(records, n) == SessionMeta::build(committed[..n])`. | oracle test |
| I4 | For every record with `commit`: each **engine** override field (`cwd`, `prev_ts`, `pending_ts`, `metrics`, `tasks`) reads — as its last present value ≤ that record — the fold's value at the start of that record's `replay_from` line. `present`/`store` are **as-of-the-write**: they advance only as committed blocks are `put` or clients are served, the replay commits nothing (I7), and a client cursor ahead of a restored `provisional_gen` resyncs rather than stale-serves. | §6.1; double-apply test |
| I5 | `commit.is_some()` ⟺ the §3 partition exists at this commit: no line authored both a committed and an uncommitted block. Then `replay_from` is that partition's offset. | `line_boundary()` |
| I6 | After a load, `\|committed\| == n`, and content stream, meta stream and store backing are truncated to `n` before any append. | §6.2 |
| I7 | Across a resume, `committed_len()` is unchanged until the first new commit. | `debug_assert!`; violation ⇒ cold rebuild |
| I8 | Reuse only if §6.4 holds. | §6.4 |
| I9 | At most one live process writes a `<presentation, session>`. Where liveness is undecidable (`pid_alive` is unix-only, `jdi/state.rs:150-167`) the cache is disabled. | §8 |
| I10 | A fold reset truncates both streams to 0 and bumps `epoch`. | §6.5 |

## 6. Algorithms

### 6.1 Write

The record is authored by the **builder**, not the replayer. The builder already holds every
input: the decoded messages before `apply` (the predicate), the metrics accumulator and task
fold (the override sources), the byte offset, and the drained blocks — which it already walks
for `committed_meta.push` and `store.put` (`builder.rs:150-158`); the same walk accumulates the
record. The replayer contributes four `pub(crate)` getters (`raw_len` = base+|out|, `base`,
`prev_ts`, `pending_ts`) and stays purely the block fold.

```
on advance_at(offset, line):                       # builder
    msgs ← decode(line)
    if any(m.can_open_turn() for m in msgs):       # BEFORE the task-op loop and apply
        cand ← Entry { logical: replayer.raw_len(), offset,
                       prev_ts: replayer.prev_ts(), pending_ts: replayer.pending_ts(),
                       metrics: metrics.save_state(), tasks: task_fold.save_state() }
    fold task ops; replayer.apply(msgs)            # the drain may fire inside apply
    if cand exists and replayer.raw_len() grew:    # the line actually authored a block
        deque.push(cand)
    drained ← replayer.drain_committed()
    if drained ≠ ∅:
        rec ← MetaRecord::default()
        for b in drained: rec.accumulate(b); committed_meta.push(b); store.put(b)
        deque.prune(entries with logical < replayer.base())
        if deque.front()?.logical == replayer.base():          # line_boundary — I5
            e ← deque.front()
            rec.commit ← Some(Commit { id: |committed|, replay_from: e.offset,
                                       window: crc32(src @ e.offset) })
            rec.set_changed_override_fields(e)     # only those differing from the last written
        writer.enrich_and_append(rec)              # present/store filled by their own layers
    metrics.push(line)                             # last — metrics stays as-of-line-start
```

**`line_boundary` is the deque-front check, NOT a current-line check.** The current line at a
pinned drain is the one opening the *newest* turn, while `replay_from` is the line of the
*oldest* uncommitted block — they differ whenever the open window spans several turns. The front
entry's `logical` equals the post-drain `base` exactly when that line's **first** authored block
is the first uncommitted block — which is §3's partition. A straddling line's entry has
`logical < base` and is pruned; the check then fails on whatever entry follows. (A current-line
check would wrongly disqualify every pinned commit.)

**An entry is captured before `apply` but committed to the deque only if the line authored a
block.** Capture must predate the line's effects; but a flagged line can author *nothing* — a
`CommandStdout` that patches into a prior `Command` — and its entry would then carry the raw
index of the NEXT line's block, matching `base` falsely. A resume from that offset re-reads the
`CommandStdout` against an empty window and fabricates an orphan `Command` block a cold fold
never had. So over-approximation in the predicate is safe **only** because unproductive entries
are discarded post-`apply`.

`can_open_turn()`'s set is every `Replayer::apply` arm pushing a turn block — `UserText`
(`replay.rs:274`), `AttachmentPrompt` (`:335`), `Command` (`:293`), and `CommandStdout`
(`:308`, which pushes a `Block::Command` when no preceding `Command` exists). `SkillBody` and
`QueueOp` push a `ToolResult` and a `QueueEvent` and are excluded. Defined beside those arms,
with a `debug_assert!` firing if a drain ever puts the partition inside a rejected line.

One capture per turn-opening line, one entry per *authored* turn in the open window; the drain
prunes both.

### 6.2 Load

```
load(dir):
    committed ← bv_loader(dir)                     # frontend-specific: the only such piece
    records   ← meta_loader(dir)                   # shared; discards a torn trailing record
    n ← per I1                                     # max over records whose `commit` is present
    if n is None or !valid(header, records[n].commit): return None   # cold rebuild
    truncate(content, n); truncate(meta, n); store.truncate(n)  # I6
    return (committed[..n], records[..=n])
```

Truncation is not optional: HTML serves committed bytes as one range to EOF
(`serve.rs:325-329`), so bytes past `n` are read as garbage; and a later append would sit behind
records already replayed.

### 6.3 Restore

```
SessionAccumulator::restore(adapter, store, committed, records) -> Restored:
    n   ← per I1
    acc ← with_store(adapter, store)
    acc.committed      ← committed[..n]
    acc.committed_meta ← replay_meta(records, n)             # plain accumulate
    ov ← override_at(records, n)               # last present value of each override field
    acc.cwd ← ov.cwd; acc.metrics.restore_state(ov.metrics); acc.task_fold.restore_state(ov.tasks)
    acc.replayer.restore_state(ov.prev_ts, ov.pending_ts)     # base = stamped = 0, out = []
    return { acc, committed_id: n, replay_from: rec_n.commit.replay_from,
             present: ov.present, store: ov.store }           # opaque; callers route them

caller: reader ← LineReader::open_at_offset(src, replay_from)   # not open_at, which re-reads [0,offset)
        loop { acc.advance_at(off, line) }                      # normal folding
```

`base = stamped = 0`, not `committed_len`: `stamped` is raw-logical (like `base`,
`replay.rs:104`) while `committed_len()` counts finalized blocks (`builder.rs:204-206`), and
`coalesce_spans` collapses runs, so they differ. Setting `stamped = committed_len` makes
`window_stamped()` exceed `out.len()` and the first `LineStart` slice out of range. `base`'s
absolute value never escapes the replayer, so rebasing is sound. `user_times` comes from the replayed deltas and has length `committed_meta.turns`, which is its
value at `replay_from`: by §3's partition every uncommitted `UserText` lies at or above that
offset, so none has been stamped. `suppress` holds only `QueueEvent` markers, so no turn is ever suppressed
and that count is exact.

Alignment lives in the accumulator because the accumulator owns `committed`. It opens no file
and decodes no `BV`, so alignment is a pure function of two vectors — testable with hand-built
inputs, including torn tails otherwise reachable only by killing a writer mid-write. Loaders
return two vectors; the persistence layer performs the `set_len`s and is the only writer.

### 6.4 Validate

```
valid(hdr, c: Commit):
    len(src) ≥ c.replay_from
  ∧ first_line(src) == hdr.anchor                  # checked once, not per record
  ∧ crc32(src[c.replay_from-64KiB .. c.replay_from]) == c.window
  ∧ hdr.versions == current
```

The window is fixed-size and ends at `replay_from` because everything restored derives from
bytes below `replay_from`; bytes at or after it are re-read and folded fresh, so a rewrite there
is self-correcting. A whole-prefix hash would re-read `[0, offset)` on every open. A single line
exceeding 64 KiB reduces this to a sub-line check.

Only the served rendering is cached; the offline dump writers are excluded, so the flavor space
is one and `BYTE-IDENTICAL: PASS` never depends on machine state.

### 6.5 Reset

A truncation or compaction drives `builder.reset()`. Both streams and the store truncate to 0
and `epoch` increments. Without this, content regrows from 0 while meta holds payloads stamped
against the old bytes, and the next open accepts a stale payload against different content.
`open_fresh` discards the durable directory, not only the in-process store.

## 7. The one seam addition

`MetricsAccumulator` is `push`/`finish` with no seed (`adapter.rs:24-34`), and the collapsed
`Metrics` cannot rebuild one: both agents hold private span endpoints and Codex's `model` comes
from a `turn_context` line near the session start (`codex/metrics.rs:35-39`).

```rust
fn save_state(&self)                        -> serde_json::Value { Value::Null }
fn restore_state(&mut self, _: serde_json::Value)                  {}
```

`TimeSpan` must derive serde (`metrics.rs:63-66`). QoderWork shares Claude's accumulator, so the
blob is keyed by presentation and agent id.

## 8. Locking

One lock per `<presentation, session>`, keyed by `discover::session_id(path)` with the file stem
as fallback — both frontends currently use the bare stem, which collides across projects in a
shared directory. The lock is held for the process lifetime, independent of residency, because
`pull_response` reaps per request; GC therefore skips entries holding a lock. A server holder is
port-probed as well as pid-checked, through a callback the frontend injects.

| situation | behaviour |
|---|---|
| free, or holder dead | take it |
| TUI, live holder | quit, naming pid, dir and `tmux attach` |
| HTML, held at pick time | open the holder's `…?session=S` |
| HTML, multi-root start | acquire per session; partial success is normal |
| HTML, child found mid-run, held | serve uncached |

The lock governs writing, not viewing. `--no-cache` is a hidden flag skipping both.

## 9. Cost

| when | work |
|---|---|
| per line | one predicate over the already-decoded messages |
| per user turn | one deque entry: four scalars + one clone each of the metrics accumulator and task fold |
| per committed block | HTML: none. TUI: one serialize + buffered append |
| per committing drain | one record + one 64 KiB CRC32 (1.6 µs, on bytes just read) |
| per open | `stat` + first line + 64 KiB + replay deltas + scan content — O(records + committed) |
| `--no-cache` | today's path |

## 10. Changes

| file | change |
|---|---|
| `engine/meta_stream.rs` | reshape to §4's flat record + `StreamHeader`; DELETE `emit_batch`, `MetaDelta` and the unanchored/supersede semantics (a0acaf4/8b4f4cf) |
| `engine/replay.rs` | DELETE the current meta wiring (`meta_out`, `committed_emitted`, `last_provisional`, `drain_meta`); add four `pub(crate)` getters + `restore_state` seeding — the replayer returns to being purely the block fold |
| `engine/builder.rs` | the turn-boundary deque + record authorship at the drain (§6.1), `Message::can_open_turn`, `restore` (§6.3), `committed_meta()` |
| `engine/tasks.rs`, `engine/adapter.rs` | `save_state`/`restore_state`; `TimeSpan` serde |
| `engine/reader.rs`, `engine/follow.rs` | `open_at_offset`, `FollowParser::resume`; RETIRE `Position`/`open_at`/`tell` — dead code (`reader.rs:52-54` names this feature as its consumer) whose model is superseded: it re-hashes `[0, offset)` on resume, and its `DefaultHasher` is not stable across builds, unusable in a persisted format |
| `present/cache/` | stream writer/reader, `Admission`, `shared_insert_or_get`, `--no-cache` |
| `present/cache/shared.rs` | delete `Body::Hibernated`; `restore` yields `Body::Live` |
| `present/lock.rs` | moved from `jdi/` |
| `html/serve.rs` | durable dir, per-session locks, `RecordStore::open_append` |
| `tui/app.rs` | cache on the non-follow path; flush + release at both `process::exit(0)`s and `Outcome::Switch` |

Order: engine (1) → present streams (2) → delete `Body::Hibernated` (3) → TUI durable store (4)
→ cache API and the `poll_view` generalisation (5) → lock move (6) → HTML (7) → TUI (8) → GC
(9). Step 1 carries the R5 test and gates the rest. Step 5 must generalise `poll_view`, today
concrete on `SessionCache<ArcStore, A>` (`cache/mod.rs:225-233`), together with the store
factory it takes. Step 2 must restore the derived per-tick baselines — `prev_provisional`
(`shared.rs:650`) and the follower's `prev_committed` (`follow.rs:55-56`) — or the first poll
reports `changed_from = 0` and re-renders everything.

Additive except the TUI single-writer refusal. Any validation failure falls back to today's
path. Release: minor.

## 11. Tests

| test | asserts |
|---|---|
| equivalence | cached vs cold, byte-identical, per presentation |
| re-invocation | zero lines parsed below `replay_from` |
| oracle | I3, both presentations, one reader |
| alignment | truncate either stream ⇒ I1 and I6 hold and the session resumes; a killed write yields a smaller `n`, never a rebuild; a held cursor resyncs |
| double-apply | I4: pinned-drain fixture yields an identical block list. The open window must span several turns, or the two capture points coincide and the test is vacuous |
| rejection | rewritten prefix, changed version, changed flavor ⇒ full rebuild |
| lock | two writers; dead-pid reclaim; live pid + dead port; TUI refusal; HTML hand-off |

Fixtures must include a pinned drain, a mid-turn typed prompt, a late tool result, an
**orphan `CommandStdout` directly before a turn boundary** (the flagged-line-authors-no-block
false positive — §6.1's discard rule is what it catches), and an **attachment-first prompt**
(straddles: that drain carries no `commit`, and the fallback to the previous record must
produce a byte-identical resume). A linear transcript passes while the design is broken.

## Rejected

| shape | why |
|---|---|
| Resume from a bare byte offset | the commit cut is not a byte offset: one line can carry several turns, the drain lags the open window, and `agent_ids` has session-long reach |
| Snapshot at the drain, re-read from the frontier | double-applies everything between; `tool_slot` is pruned, the queue is non-empty, `user_times` is a turn ahead, metrics are a mixed epoch |
| Two offsets plus a suppression list | sound, but the list must be extended by hand for every cumulative fold added to `advance_at`, and an omission is silent |
| Unanchored records restating the open turn | a resume never reads them; `open_read` already derives the same value in-process from provisional blocks, independent of `BV`; provisional blocks are not persisted |
