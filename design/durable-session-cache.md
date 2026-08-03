# Design: a durable, cross-run session cache

> **v19.** Reuse a prior invocation's parse of a transcript. Read §3 first: the rest follows
> from it.

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
meta      a StreamHeader, then one MetaRecord per commit — a flat list of optional fields
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
| `queue` | rebuilt — empty at every commit, by the drain's own gate |
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
starting after it loses provisional ones. One line can do this (`committed_len` 0 → 2 on one
line). Such a commit carries no resume payload and a load falls back to the previous qualifying
record.

The partition is otherwise total: `finalize_completed` drains `out[0..k)` where `k` indexes a
turn-boundary block, so `out` retains `out[k..]` and is never empty after a commit
(`replay.rs:428-449`). A first uncommitted block therefore always exists, and its line's start
is `replay_from`.

## 4. The record

One record per commit: **a flat list of optional fields.** Absent always means "nothing from
this commit". Fields belong to one of three classes, and the classes are the whole format.

| class | read rule | fields |
|---|---|---|
| **accumulate** | value at `n` = fold of every present value in records ≤ `n` — numeric `+`, list append, map upsert | `turns`, `tools`, `children`, `agent_ids`, `user_times` |
| **override** | value at `n` = the value **on record `n`** | `window`, `cwd`, `prev_ts`, `pending_ts`, `metrics`, `tasks`, `present`, `store` |
| **indicator** | presence marks the record as a resume point | `commit: (committed_id, replay_from)` |

```rust
struct MetaRecord {
    // accumulate
    turns:      Option<usize>,
    tools:      Option<usize>,
    children:   Vec<ChildOp>,                    // empty = absent
    agent_ids:  Vec<(String, String, String)>,   // (key, agent_id, agent_type); upsert
    user_times: Vec<Option<EpochSeconds>>,       // appended by THESE committed turns

    // indicator — present iff §3's partition exists at this commit (I5)
    commit:     Option<(usize, ByteOffset)>,

    // override — present iff `commit` is; each is state as of `replay_from`
    window:     Option<Hash>,                    // sha256 of the 64 KiB ending at replay_from
    cwd:        Option<String>,
    prev_ts:    Option<Option<EpochSeconds>>,
    pending_ts: Option<Option<EpochSeconds>>,
    metrics:    Option<Value>,                   // agent-opaque (§7); O(1)
    tasks:      Option<Value>,                   // TaskFold incl. `pending`; bounded by open tasks
    present:    Option<Value>,                   // engine-opaque: epoch, provisional_gen, n_provisional
    store:      Option<Value>,                   // engine-opaque: EmitState O(1) part
}

enum ChildOp {
    Add(ChildMeta),   // positional: SessionMeta keeps duplicate ids (session.rs:297-303)
    Done(AgentId),    // by id, clearing every match: SessionMeta's scan (session.rs:300-302).
                      // An ordinal is not computable — an AgentDone can match a child added
                      // many batches earlier, and a delta cannot see the accumulated list.
}

struct StreamHeader {     // record 0, written once
    anchor:   Hash,       // first line of the transcript
    versions: Versions,   // format, fold-logic, build; HTML adds flavor
}
```

**The indicator gates the override fields.** An override value is the fold's state as of *that
record's* `replay_from`, and every record has a different one — so carrying a value forward from
an earlier record would restore something measured at the wrong offset. Writing them only
alongside `commit` is what lets the read rule be "the value on record `n`" rather than "the last
present value ≤ `n`", and a record without `commit` is never landed on, so it needs none of them.

**Why `commit` is one field and not two.** `committed_id` is used only to align against
`|committed|` and to name the record a load stops at; both apply solely to resume points. Paired
with the offset it is one fact — "resumable here" — rather than an invariant to maintain across
two fields.

**Three lifetimes, three homes.** Conflating them wastes space, and one case is asymptotically
wrong:

| lifetime | fields | home |
|---|---|---|
| constant per session | `anchor`, `versions` | **header**, once |
| accumulating | `turns`, `tools`, `children`, `agent_ids`, `user_times` | **accumulate class** |
| as-of-`replay_from` | `window`, `cwd`, timestamps, metrics, tasks, present, store | **override class** |

`user_times` is the case that matters. It is append-only and grows with turns, so writing it
whole per commit costs O(turns²) in bytes *and* serialization, and makes a load O(turns²). As an
accumulate field it is O(turns): folding through record `n` yields exactly its as-of-`replay_from`
value, because at that offset precisely the committed turns are stamped.

Measured at ~217 commits (the mean over 131 local transcripts): **253 KiB → 55 KiB** per session,
O(turns²) → O(turns). At 2000 turns, 15.9 MiB → 504 KiB.

The record names no `BV`, so restore is one implementation with no type parameter (R5). The two
opaque `Value`s exist because R1 puts the format in the engine, which cannot name a `present` or
frontend type; precedent is `PersistentStore::hibernate_state`/`restore_state`
(`shared.rs:565-572`).

## 5. Invariants

| # | Invariant | Enforced by |
|---|---|---|
| I1 | `n = max { id : r.commit == Some((id, _)) ∧ id ≤ \|committed\| }`. The loaded `BV` vector's length is the sole authority; no committed count is persisted separately. | §6.2 |
| I2 | One record per commit. `commit` ids are strictly increasing, and reach `\|committed\|` unless the tail is torn or the last commits failed I5. | §6.1 |
| I3 | `replay_meta(records, n) == SessionMeta::build(committed[..n])`. | oracle test |
| I4 | Every override field equals its value at the start of that record's `replay_from` line, and is present iff `commit` is. | §6.1; double-apply test |
| I5 | `commit.is_some()` ⟺ the §3 partition exists at this commit: no line authored both a committed and an uncommitted block. Then `replay_from` is that partition's offset. | `line_boundary()` |
| I6 | After a load, `\|committed\| == n`, and content stream, meta stream and store backing are truncated to `n` before any append. | §6.2 |
| I7 | Across a resume, `committed_len()` is unchanged until the first new commit. | `debug_assert!`; violation ⇒ cold rebuild |
| I8 | Reuse only if §6.4 holds. | §6.4 |
| I9 | At most one live process writes a `<presentation, session>`. Where liveness is undecidable (`pid_alive` is unix-only, `jdi/state.rs:150-167`) the cache is disabled. | §8 |
| I10 | A fold reset truncates both streams to 0 and bumps `epoch`. | §6.5 |

## 6. Algorithms

### 6.1 Write

```
on advance_at(offset, line):
    msgs ← decode(line)
    scratch ← { logical: base + |out|, offset, prev_ts, pending_ts }
    if any(m.can_open_turn() for m in msgs):        # before folding — see below
        deque.push(scratch + { metrics: metrics.save_state(), tasks: tasks.save_state() })
    fold(msgs)

on drain():                                        # finalize_completed
    finalized ← the turns behind the frontier
    rec ← MetaRecord::default()
    for b in finalized: rec.accumulate(b)          # accumulate class, incl. user_times
    deque.prune(below: base)
    if line_boundary():                            # I5 — else indicator+override stay absent
        e ← deque.front()                          # the entry for the first uncommitted block
        rec.commit ← Some((committed_emitted + |finalized|, e.offset))
        rec.set_override_fields(e)
    meta.append(rec)
```

`can_open_turn()` is evaluated before the fold because the snapshot must predate the line's
effects. It must over-approximate: a missed snapshot loses a resume point; a spurious one wastes
a clone. Its set is every `Replayer::apply` arm pushing a turn block — `UserText`
(`replay.rs:274`), `AttachmentPrompt` (`:335`), `Command` (`:293`), and `CommandStdout`
(`:308`, which pushes a `Block::Command` when no preceding `Command` exists). `SkillBody` and
`QueueOp` push a `ToolResult` and a `QueueEvent` and are excluded. It is defined beside those
arms, with a `debug_assert!` firing if a drain ever puts the partition inside a rejected line.

`deque.front()` locates the partition: after a drain the first uncommitted block is always a
`UserText` or `Command` (`replay.rs:428-449`), so its line's start is `replay_from`, and the
deque holds one entry per turn in the open window rather than per line.

### 6.2 Load

```
load(dir):
    committed ← bv_loader(dir)                     # frontend-specific: the only such piece
    records   ← meta_loader(dir)                   # shared; discards a torn trailing record
    n ← per I1                                     # max over records whose `commit` is present
    if n is None or !valid(header, records[n]): return None     # cold rebuild
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
    acc.replayer.restore_state(records[n].override_fields())  # read from record n alone
    acc.replayer.base ← 0; acc.replayer.stamped ← 0; out ← []
    return { acc, committed_id: n, replay_from: records[n].commit.1 }

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
valid(hdr, r):  let (_, from) = r.commit
                len(src) ≥ from
              ∧ first_line(src) == hdr.anchor         # checked once, not per record
              ∧ sha256(src[from-64KiB .. from]) == r.window
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
fn save_state(&self)                  -> Value { Value::Null }
fn restore_state(&mut self, _: Value)          {}
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
| per line | four scalars into a scratch slot, one predicate over decoded messages |
| per user turn | one clone each of the metrics accumulator and task fold |
| per committed block | HTML: none. TUI: one serialize + buffered append |
| per commit | one record + one 64 KiB sha256 (23 µs, on bytes just read) |
| per open | `stat` + first line + 64 KiB + replay deltas + scan content — O(records + committed) |
| `--no-cache` | today's path |

## 10. Changes

| file | change |
|---|---|
| `engine/meta_stream.rs` | exists; flatten to the §4 record + `StreamHeader`; remove the unanchored-record path |
| `engine/replay.rs` | scratch slot, boundary deque, `can_open_turn`, `line_boundary`, `save_state`/`restore_state` |
| `engine/builder.rs` | `save_state`, `restore`, `committed_meta()` |
| `engine/tasks.rs`, `engine/adapter.rs` | `save_state`/`restore_state`; `TimeSpan` serde |
| `engine/reader.rs`, `engine/follow.rs` | `open_at_offset`, `FollowParser::resume` |
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

Fixtures must include a pinned drain, a mid-turn typed prompt and a late tool result. A linear
transcript passes while the design is broken.

## Rejected

| shape | why |
|---|---|
| Resume from a bare byte offset | the commit cut is not a byte offset: one line can carry several turns, the drain lags the open window, and `agent_ids` has session-long reach |
| Snapshot at the drain, re-read from the frontier | double-applies everything between; `tool_slot` is pruned, the queue is non-empty, `user_times` is a turn ahead, metrics are a mixed epoch |
| Two offsets plus a suppression list | sound, but the list must be extended by hand for every cumulative fold added to `advance_at`, and an omission is silent |
| Unanchored records restating the open turn | a resume never reads them; `open_read` already derives the same value in-process from provisional blocks, independent of `BV`; provisional blocks are not persisted |
