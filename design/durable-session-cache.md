# Design: a durable, cross-run session cache

> **v27 — BUILT.** Every step in §11 has landed and every gate passes. The design below stands as
> written; §14 records the four places the implementation chose differently and why. Read §3
> first: the rest follows from it.
>
> v22 adds: an **iterative** meta reader (entries are never all resident), **no positional
> correspondence** between the two streams (I2 — the link is `resume.id` alone), and
> **checkpoints** (§6.6) that bound an open's work, validate a fold in production, and make
> compaction asynchronous. v23 carries the checkpoint **on a resumable record** rather than as a
> standalone entry — otherwise compaction can leave state with no resume point. v24 names the
> folded form `MaterializedMeta` and retires the opaque "Part I/II" labels for `delta` /
> `resume` / `checkpoint`.

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

Two append-only streams per `<presentation, session>`, written only at a **committing drain**:

```
content   TUI:  JSON-encoded Block per committed block   (Bv = Arc<Block>)
          HTML: rendered wire record per committed block (Bv = RecordLocator)
meta      a StreamHeader, then one MetaRecord per committing drain
            delta      — session facts (counters + gauges); on every record
            resume     — offset, checksum, fold clocks; iff resumable
            checkpoint — a materialized MaterializedMeta; every CHECKPOINT_EVERY drains (§6.6)
```

A block commits when a later turn begins and the prompt queue is empty. There is no periodic
snapshot, no third file, and nothing schedules a write.

**The resume payload is in-band deliberately.** Only the record a load lands on reads it, so a separate
latest-only file would save the rest — ~50 bytes × commits, ~11 KiB per session. It would also
need its own alignment against the other two streams, and a crash between writing them would
leave the pair disagreeing. In-band, each commit is **one append per stream** and the last
complete record is by construction a consistent resume point — a torn tail costs one commit,
never correctness.

## 3. The resume principle

> **A resume point is a pair `(offset, state)` such that folding the transcript from `offset`
> with the fold seeded to `state` yields exactly the live state a cold parse yields.**

Three questions follow, and their answers are the whole design.

### Which offset?

`replay_from` partitions the transcript: **bytes below it authored only blocks that have
committed; bytes at or above it authored only blocks that have not.**

It is defined by the bytes, not by the current shape of `out`. Blocks in the open window still
merge and mutate before they commit — a `ToolResult` joins its `ToolUse`, a coalesced run
collapses into one `Thinking{tools}` — so "the line that produced `out[0]`" names a block that
may not survive to commit in that form. The partition is stable under all of it: what a line
authored either committed or did not.

Nor is it "the start of the open turn". The open window spans several turns whenever the
`last_skill` pin caps the drain (`replay.rs:439-446`), so `replay_from` can precede the commit
line by more than one turn.

### Which state?

Everything the re-read does *not* rebuild. Fold state partitions exactly — this table is the
authority; §4 gives each persisted item its field.

| fold state | disposition |
|---|---|
| `out`, `base`, `stamped` | rebuilt — the re-read reproduces it |
| `tool_slot`, `suppress`, `last_skill` | rebuilt — pruned to the open window at each drain |
| `queue` | rebuilt — provably empty at `replay_from`: an item resident there would have gated off every commit since its enqueue (`finalize_completed` returns while the queue is non-empty), so its marker cannot sit below the partition |
| committed blocks | **content stream** |
| turn/tool counts, sub-agent lifecycle, `agent_ids`, per-turn timestamps, tokens, task ops | **delta counters** — they fold |
| `cwd`, the metrics time span | **delta gauges** — last value wins |
| `prev_ts`, `pending_ts` | **resume payload** — fold clocks, read only by a resume |

`agent_ids` (the spawn-identity map) has no field of its own: it is rebuilt from the `Spawned`
events in `agents`, which carry both keys (§4).

### When is the state captured?

**As of the start of the `replay_from` line — never at the commit instant.** `advance_at` folds
metrics, the task op-log and `user_times` for **every** line (`builder.rs:116-164`), including
every line the re-read covers. Capturing at the commit double-applies them, since the commit
line is at or after `replay_from`. Capturing at `replay_from` makes the re-read apply each line
exactly once and suppress nothing.

For a counter this means the emitted delta is `value(replay_from_k) − value(replay_from_{k−1})`,
not "since the last commit" — §6.1 shows the mechanism.

### Which commits qualify?

Those at which the partition **exists**. Not a separate rule — it is the definition's
well-definedness condition. A line that authored blocks on both sides of the frontier admits no
offset: re-reading from its start re-produces committed blocks, starting after it loses
provisional ones. Two shapes do this:

- a **multi-turn line** — one line carrying several user texts commits the earlier turns while
  the last stays open (probe: `committed_len` 0 → 2 on one line);
- an **attachment-first prompt** — a user line ordered `[image, text]` authors the `Attachment`
  block *below* its `UserText` (decode preserves item order, `claude/model.rs:655-668`), so the
  attachment commits while the turn stays open.

Either way the record omits its resume payload and a load falls back to the previous qualifying record —
the cost is re-reading one extra turn, never a lost cache. The next drain re-qualifies: by then
the whole straddling line has committed.

The partition is otherwise total: `finalize_completed` drains `out[0..k)` where `k` indexes a
turn-boundary block, so `out` retains `out[k..]` and is never empty after a commit
(`replay.rs:428-449`). A first uncommitted block therefore always exists, and its line's start
is `replay_from`.

## 4. The record

**Two parts, split by who reads it.**

| part | present | read by | rule |
|---|---|---|---|
| **I — session facts** | every record | *both* consumers | flat optional fields; **absent = no update**; the field's class fixes what an update *is* |
| **II — resumption** | **iff** this drain is a resume point (I5) | the resume only | one block; all fields current when present |

The delta's two classes are the counter/gauge distinction:

| class | absent means | value at `n` | fields |
|---|---|---|---|
| **counter** | added nothing | fold of every present value in records ≤ `n` — scalars `+`, lists append, **maps sum per key** | `turns`, `tools`, `agents`, `user_times`, `tokens`, `extra`, `task_ops` |
| **gauge** | unchanged | last present value in records ≤ `n` | `cwd`, `span` |

### 4.1 Declarations

Every type the format **introduces** is declared here; nothing is `serde_json::Value`. Types it
merely *references* already exist in the tree and are unchanged: `AgentStatus` (`model.rs:250`),
`SessionMeta`/`TaskList`/`TaskItem` (`engine/session.rs`, `engine/tasks.rs`), `RecordLocator` and
`FoldPolicy` (HTML).

```rust
type Model        = String;          // e.g. "claude-opus-5"
type EpochSeconds = i64;             // engine::time
type ByteOffset   = u64;             // engine::model
type AgentId      = String;

// ── the stream ────────────────────────────────────────────────────────────────
struct StreamHeader {                // record 0 of the meta stream, written once
    anchor:   u32,                   // CRC32 of the transcript's FIRST LINE — identity
    versions: Versions,
}

struct Versions {
    format: u16,                     // this record schema
    fold:   u16,                     // fold-logic version: bump when block output changes
    flavor: Option<u64>,             // HTML only: FoldPolicy + render cwd + record schema
}

struct MetaRecord {
    // ── The DELTA — session facts. Every record. A metadata reader needs ONLY this.
    turns:      Option<u32>,                          // counter: user turns that committed here
    tools:      Option<u32>,                          // counter: tool calls that committed here
    agents:     Vec<AgentEvent>,                      // counter: ordered; see the note below
    user_times: Vec<Option<EpochSeconds>>,            // counter: the turns THIS drain stamped
    tokens:     BTreeMap<Model, TokenCounts>,         // counter: per-model increments (§7)
    extra:      BTreeMap<String, u64>,                // counter: a repeated key ADDS
    task_ops:   Vec<TaskOp>,                          // counter: ordered op-log
    cwd:        Option<String>,                       // gauge: first non-empty wins
    span:       Option<(EpochSeconds, EpochSeconds)>, // gauge: session min/max timestamps

    // ── The RESUME payload. Its PRESENCE is the resume-point indicator (I5); there is no
    //    separate flag. A metadata reader skips it entirely.
    resume:     Option<Resume>,

    // ── Checkpoint — the MATERIALIZED meta as of this record, replacing every delta before it.
    //    Written every CHECKPOINT_EVERY drains (§6.6). `Some` ⇒ `resume` is also `Some`: a
    //    checkpoint a reader cannot resume from would let compaction strand the cache.
    checkpoint: Option<MaterializedMeta>,
}

struct Resume {
    id:          usize,                  // committed-block count after this drain
    replay_from: ByteOffset,             // §3's partition offset
    window:      u32,                    // CRC32 of the 64 KiB ending at replay_from
    prev_ts:     Option<EpochSeconds>,   // the thinking clock's zero
    pending_ts:  Option<EpochSeconds>,   // stamps turns authored on the resume's first line
}

// ── the reader half ───────────────────────────────────────────────────────────
// `MaterializedMeta` is the log's MATERIALIZED VIEW: what folding the delta records yields.
// The stream is the deltas; this is their sum. A `checkpoint` is simply one of these written
// down, which is why adopting a checkpoint and folding from the start must agree (I11).
/// **Iterative, not slice-at-once.** Unlike `Vec<BV>` — which is resident by definition, being
/// the committed index itself — records are consumed one at a time and never all held. A reader
/// streams the file; a resume stops feeding at its aligned `n`.
///
/// `push` has NO bound check: the CALLER guarantees it feeds nothing past `n` (§6.2). Keeping
/// the fold ignorant of the alignment is what lets a metadata reader (#98) use the same type
/// with no bound at all.
impl MaterializedMeta {
    /// Feed one record. If `r.checkpoint` is `Some`, ADOPT it and discard everything folded so
    /// far — the checkpoint is the state *as of* this record, so its own delta fields are
    /// already included and must not be applied again. Otherwise fold: counters accumulate,
    /// gauges replace.
    fn push(&mut self, r: &MetaRecord);
}

struct MaterializedMeta {
    session_meta: SessionMeta,                  // turns, tools, children — from `agents`
    agent_ids:    HashMap<String, (AgentId, String)>, // spawn identity, BOTH keys per Spawn
    user_times:   Vec<Option<EpochSeconds>>,
    tokens:       BTreeMap<Model, TokenCounts>,
    extra:        BTreeMap<String, u64>,
    tasks:        TaskFold,                     // ops APPLIED as they arrive — list + pending
    cwd:          String,
    span:         Option<(EpochSeconds, EpochSeconds)>,
}

// ── delta payload types ───────────────────────────────────────────────────────
#[derive(Default)]                   // + AddAssign: `maps sum per key` needs it
struct TokenCounts { input: u64, cache_creation: u64, cache_read: u64, output: u64 }

/// Sub-agent lifecycle. ONE ordered list, because order is load-bearing: for
/// `Spawned(X)`, `Finished(X)`, `Spawned(X)` in one record, block order yields
/// `[finished, running]` while "all spawns then all dones" yields `[finished, finished]`.
/// `SessionMeta` deliberately keeps duplicate ids (`session.rs:297-303`), so this is reachable.
enum AgentEvent {
    Spawned(Spawn),
    Finished(AgentId),               // clears EVERY spawn with that id — SessionMeta's linear
                                     // scan (`session.rs:300-302`). By id, not ordinal: it can
                                     // refer to a spawn appended many records earlier.
}

struct Spawn {
    tool_use_id: String,             // the key a completion may arrive under, before agent_id
    agent_id:    AgentId,            // empty until the spawn's tool result lands
    agent_type:  String,
    description: String,
    status:      AgentStatus,        // a spawn can be born terminal (a sync Task returns done)
}

/// `Create`/`Update` alone are NOT a complete log: a create's id arrives in the *tool result*
/// ("Created task #12: …"), which `on_tool_result` parses (`tasks.rs:186-205`) — transcript
/// data, not an op. Replaying only those two strands every create in `pending` and rebuilds an
/// EMPTY list, since `Update{task_id}` targets items that never landed.
enum TaskOp {
    Create  { tool_use_id: String, subject: String, description: String,
              active_form: String, blocked_by: Vec<String> },
    Update  { task_id: String, status: Option<String>, subject: Option<String>,
              description: Option<String>, active_form: Option<String>,
              add_blocks: Vec<String>, add_blocked_by: Vec<String> },
    Resolve { tool_use_id: String, id: Option<String> },  // Some ⇒ joined; None ⇒ create failed
}
```

### 4.2 Two consumers, and what each reads

| consumer | reads | needs |
|---|---|---|
| **resume** (the cache) | the delta **and** the resume payload | re-seeds the fold, then re-reads from `replay_from` |
| **metadata reader** | the delta only | no fold, no adapter, **no transcript** — the property that makes a machine-wide monitor (#98) cheap |

`prev_ts`/`pending_ts` are fold-continuation state, not session facts: they appear nowhere
outside `replay.rs`, and what they produce is carried elsewhere — `pending_ts` produces
`user_times` (its own field), `prev_ts` produces `Thinking.duration_secs`, which is **block
content** in the content stream. The monitor property holds only as long as every *displayed*
fact stays in the delta rather than being recomputed from blocks.

### 4.3 Why the shape is what it is

**The two-part split earns its keep.** It makes the two consumers a type fact; it removes I5's
indicator field (*"resumable here"* **is** `resume.is_some()`); and it deletes the one way to get
the gauge rule wrong — `prev_ts`/`pending_ts` ride the resume payload, so a resume reads them from the
record it lands on and can never pick up a value measured at a different `replay_from`. It costs
~50 bytes on records that were already writing an offset.

**Counters vs gauges is the counter/snapshot distinction, and a repeated key ADDS.** If two
records carry the same `extra` key the values **sum**. That is enforced today, not merely
documented: `bump(key, n)` is the only writer and it does `+= n` (`claude/metrics.rs:31-33`,
`codex/metrics.rs:23-25`), with no setter, so a gauge is unrepresentable there. A gauge must
never become an `extra` key — summing it would be wrong **live as well as cached**, since the
accumulator itself would report nonsense. If an agent ever needs one it takes its own gauge
field.

**Everything that grows is a counter.** Three fields were override in earlier drafts, which was
the same O(n²) mistake three times:

| field | as a snapshot | as a counter |
|---|---|---|
| `user_times` | 184.8 KiB at 217 turns; 15.9 MiB at 2000 | ~217 one-element deltas, ≈2 KiB |
| `task_ops` | a `TaskList` is a **full** snapshot — this repo's queue is **96 tasks / 227 KiB** — written on every change | 0–2 ops per commit |
| `tokens` | flat counters + one last-write-wins model | per-model increments |

**`pending` is derived, not persisted.** With `Resolve` in the log, `pending` is exactly the
creates with no matching resolution, so it falls out of the replay.

**Tokens are per-model, and that fixes a shipped bug** (queued as **#104**). The accumulator
keeps one `model: String` last-write-wins and `finish()` prices *every* token at that rate.
Measured across 128 local sessions with real usage, **6 (4.7%) used more than one real model** —
including this session (`claude-fable-5` 2178.8M + `claude-opus-5` 591.5M). Attribution is free:
`/message/model` is already on every assistant message, so it was being discarded, not missing.

**Increment vs total is an adapter concern.** Claude's `/message/usage` is a per-message
**increment** the accumulator sums (`claude/metrics.rs:42-45`); Codex's `total_token_usage` is a
running **total** it assigns (`codex/metrics.rs:43-49`). The record carries increments; the
adapter converts — agent specificity ends at the decoder, as everywhere else in the pipeline.

**`span` is a gauge** because min/max is a *merge*, not a sum, and the last written pair is the
correct seed for an accumulator that keeps observing.

**No presentation state is persisted at all.** Three candidates were dropped on one test — *does
this have to survive a process restart?*

- **HTML's sidebar index** (`EmitState.turns`) — no, and it is never even read: the client
  builds its sidebar from the block records (`addTurn(b)` reads `b.id`/`b.turn`/`b.label`,
  `export.js:967-975`), and its only reader is the whole-session render (`mod.rs:790`) behind the
  offline dump writers, which §6.4 excludes from the cache.
- **HTML's numbering cursor** (`next_block`, `turn`, `seen_turns`) — no: **derive it** from the
  committed records, whose ids are already on disk. Persisting creates a second source of truth
  that disagrees exactly where it hurts: a truncate-back to `n` cuts the content stream while a
  persisted counter stays ahead, leaving an id gap. Derive as a **max over each record's nested
  ids**, not a record count — `block_id()` has 8 mint sites, so one record can consume several.
- **The live counters** (`epoch`, `provisional_gen`, `n_provisional`) — no: live-protocol state.
  A restarted server has no provisional tail until it re-reads. `epoch` looks like the
  exception, and the narrow case is real: a client holding `epoch == 1` with a cursor at
  `committed_id 500`, against a server resumed at `n = 480`, sees a *matching* epoch and silently
  keeps 20 blocks the server no longer has. Persisting `epoch` papers over that; the **protocol
  guard is the fix** — a cursor whose `committed_id` exceeds `n_committed` must resync whatever
  the epoch says. Local, self-evident, and it holds however the mismatch arose. (`epoch` already
  starts at 1 so a default `epoch == 0` cursor mismatches on its first pull, `shared.rs:97-98`;
  this extends the same instinct to the case it misses.) **Ships in step 2.**

**Typed fields settle versioning.** A new field arrives with `#[serde(default)]` and an older
record still loads, so `Versions` carries only format + fold — **not** a build id — and the cache
**survives an upgrade** that does not change the fold. An opaque format would have forced
build-id invalidation, discarding every cache on every release.

## 5. Invariants

| # | Invariant | Enforced by |
|---|---|---|
| I1 | `n = max { r.resume.id : r.resume.is_some() ∧ r.resume.id ≤ \|committed\| }`, and the record used is the one **carrying that id** — never `records[n]`. The loaded `BV` vector's length is the sole authority; no committed count is persisted separately. | §6.2 |
| I2 | **The two streams have no positional correspondence.** One record per committing *drain*, but `finalize_completed` runs once per *line* (`replay.rs:412`), so a multi-turn line commits several blocks under one record and `resume.id` jumps. The only link between the streams is `resume.id`; indexing one by the other's position is always wrong. Ids strictly increase and reach `\|committed\|` unless the tail is torn or the last drains failed I5. | §6.2's lookup-by-id |
| I3 | A `MaterializedMeta` folded over the records up to `n` == the maintained `SessionMeta` / `TaskList` / metrics over `committed[..n]`. | oracle test |
| I4 | For every record with `resume`, each **gauge**'s value at that record — its last present value ≤ it — and each **counter**'s fold through it equal the fold's value at the start of that record's `replay_from` line. | §6.1; double-apply test |
| I5 | `resume.is_some()` ⟺ the §3 partition exists at this drain. Then `resume.replay_from` is that partition's offset. | `line_boundary()` (§6.1) |
| I6 | After a load, `\|committed\| == n`, and content stream, meta stream and store backing are all truncated to `n` before any append. | §6.2 |
| I7 | Across a resume, `committed_len()` is unchanged until the first genuinely new commit. | `debug_assert!`; violation ⇒ cold rebuild |
| I8 | A record is reused only if §6.4 passes: length, anchor, window, versions. | §6.4 |
| I9 | At most one live process writes a `<presentation, session>`. Where liveness is undecidable (`pid_alive` is unix-only, `jdi/state.rs:150-167`) the cache is **disabled**, never assumed stale. | §9 |
| I10 | A fold reset truncates both streams and the store backing to 0. | §6.5 |
| I11 | Adopting a `checkpoint` and folding from the stream's start yield the **same** `MaterializedMeta`. A checkpoint is present only where `resume` is, so compaction can never leave a stream with state but no resume point. A reader that passes a checkpoint compares before adopting; a mismatch ⇒ cold rebuild. | §6.6; equivalence test |

## 6. Algorithms

### 6.1 Write

The record is authored by the **builder**, not the replayer. The builder already holds every
input: the decoded messages before `apply`, the metrics accumulator and task fold, the byte
offset, and the drained blocks — which it already walks for `committed_meta.push` and
`store.put` (`builder.rs:150-158`); the same walk accumulates the record. The replayer
contributes four `pub(crate)` getters (`raw_len()` = `base + out.len()`, `base()`, `prev_ts()`,
`pending_ts()`) and stays purely the block fold.

```
struct Entry {                     # one per authored turn in the open window
    logical:  usize,               # raw-logical index of this line's FIRST authored block
    offset:   ByteOffset,          # this line's start
    prev_ts, pending_ts: Option<EpochSeconds>,
    tokens:   BTreeMap<Model, TokenCounts>,   # metrics TOTALS as of this line's start
    extra:    BTreeMap<String, u64>,
    span:     Option<(EpochSeconds, EpochSeconds)>,
    cwd:      String,
}

state: deque: VecDeque<Entry>, emitted: Entry   # `emitted` = totals at the last emitted resume

on advance_at(offset, line):                    # builder
    msgs ← decode(line)
    if any(m.can_open_turn() for m in msgs):    # BEFORE the task-op loop and apply
        (tk, ex, sp) ← metrics.totals()          # §7 — as-of-line-start (see the last line)
        cand ← Entry { logical: replayer.raw_len(), offset,
                       prev_ts: replayer.prev_ts(), pending_ts: replayer.pending_ts(),
                       tokens: tk, extra: ex, span: sp,
                       cwd: self.cwd }                  # the ACCUMULATOR's cwd (builder.rs:36)
    fold task ops (recording them in pending_ops); replayer.apply(msgs)
    if cand exists and replayer.raw_len() > cand.logical:   # the line AUTHORED a block
        deque.push_back(cand)
    drained ← replayer.drain_committed()
    if drained ≠ ∅:
        rec ← MetaRecord::default()
        turns0 ← committed_meta.turns                   # BEFORE folding this drain
        for b in drained: rec.count(b); committed_meta.push(b); store.put(b)
        # `count` derives turns/tools/agents from the blocks; user_times cannot come from
        # them — the stamps live in the replayer, indexed by TURN — so slice by turn count:
        rec.user_times ← replayer.user_times()[turns0 .. committed_meta.turns]
        rec.task_ops ← take(pending_ops)
        deque.prune_front(entries with logical < replayer.base())
        if deque.front()?.logical == replayer.base():       # line_boundary — I5
            e ← deque.front()
            rec.tokens ← e.tokens − emitted.tokens          # per-key subtraction
            rec.extra  ← e.extra  − emitted.extra
            if e.span ≠ emitted.span: rec.span ← Some(e.span)
            if e.cwd  ≠ emitted.cwd:  rec.cwd  ← Some(e.cwd)
            rec.resume ← Some(Resume { id: |committed|, replay_from: e.offset,
                                       window: crc32(src[e.offset-64KiB .. e.offset]),
                                       prev_ts: e.prev_ts, pending_ts: e.pending_ts })
            emitted ← e
        writer.append(rec)
    metrics.push(line)                          # LAST — so `metrics.totals()` above is
                                                # as-of-line-start, per §3
```

**Counter deltas are differences of two `replay_from` captures**, not "since the last commit" —
that is what makes I4 hold for `tokens`/`extra`. `turns`/`tools`/`agents`/`user_times` need no
subtraction: they are derived from the drained blocks, which are exactly the blocks below the
new partition.

**A record with no resume payload still carries its delta counters.** They are not lost — the next
qualifying record's `tokens` delta is measured from `emitted`, which only advances when a resume payload
is written, so nothing is double-counted or dropped.

**`line_boundary` is the deque-front check, NOT a current-line check.** At a pinned drain the
current line opens the *newest* turn while `replay_from` is the line of the *oldest* uncommitted
block. The front entry's `logical` equals the post-drain `base` exactly when that line's **first**
authored block is the first uncommitted block — §3's partition. A straddling line's entry has
`logical < base` and is pruned; the check then fails on whatever follows. A current-line check
would wrongly disqualify every pinned commit.

**An entry is captured before `apply` but pushed only if the line authored a block.** Capture
must predate the line's effects, but a flagged line can author *nothing* — a `CommandStdout`
that patches into a prior `Command` — and its entry would then carry the *next* line's raw index,
matching `base` falsely. A resume from that offset re-reads the `CommandStdout` against an empty
window and fabricates an orphan `Command` a cold fold never had. Over-approximating the
predicate is safe **only** because unproductive entries are discarded.

`can_open_turn()`'s set is every `Replayer::apply` arm that pushes a turn block — `UserText`
(`replay.rs:274`), `AttachmentPrompt` (`:335`), `Command` (`:293`), and `CommandStdout` (`:308`,
which pushes a `Block::Command` when no preceding `Command` exists). `SkillBody` and `QueueOp`
push a `ToolResult` and a `QueueEvent` and are excluded. Define it beside those arms, with a
`debug_assert!` firing if a drain ever puts the partition inside a rejected line.

### 6.2 Load

```
load(dir) -> Option<Loaded>:
    committed ← bv_loader(dir)                  # frontend-specific: the ONLY such piece
    # ONE streaming pass over the meta file. Entries are folded as they arrive; nothing but
    # the running MaterializedMeta and the best resume so far is retained.
    mm ← MaterializedMeta::default(); hdr ← meta_header(dir); best ← None
    for r in meta_records(dir):                 # streaming; drops a torn trailing record
        if r.resume.is_some() and r.resume.id > |committed|: break   # I1's bound — CALLER's job
        mm.push(r)                              # adopts r.checkpoint if present (§4.1)
        if r.resume.is_some(): best ← Some(r.resume)                 # by id, never by index
    if hdr is None or best is None or !valid(hdr, best): return None            # cold rebuild
    n ← best.id
    truncate(content, n); truncate(meta, after the entry carrying id n); store.truncate(n)  # I6
    return Loaded { committed: committed[..n], meta: mm, resume: best }
```

Truncation is not optional: HTML serves committed bytes as one range to EOF
(`serve.rs:325-329`), so bytes past `n` are read as garbage, and a later append would sit behind
records already replayed.

### 6.3 Restore

```
SessionAccumulator::restore(adapter, store, ld: Loaded) -> Restored:
    n   ← ld.resume.id                          # NOT an index into any vector (I2)
    mm  ← ld.meta                               # already folded by the streaming load
    acc ← with_store(adapter, store)
    acc.committed      ← ld.committed
    acc.committed_meta ← mm.session_meta        # turns, tools, children (from `agents`)
    acc.cwd            ← mm.cwd
    acc.task_fold ← mm.tasks                    # list AND pending both already folded (§4.3)
    acc.metrics.reseed(mm.tokens, mm.extra, mm.span)
    acc.replayer.reseed(mm.agent_ids,                  # spawn identity, both keys
                        mm.user_times,                 # len == mm.session_meta.turns
                        ld.resume.prev_ts,
                        ld.resume.pending_ts)          # base = stamped = 0, out = []
    return { acc, committed_id: n, replay_from: ld.resume.replay_from }
    # No presentation state to route: the frontend derives its numbering cursor from the
    # committed records it just loaded, and the live counters start fresh (§4.3).

caller: reader ← LineReader::open_at_offset(src, replay_from)   # NOT open_at (re-reads [0,off))
        loop { acc.advance_at(off, line) }                      # normal folding, nothing suppressed
```

`base = stamped = 0`, not `committed_len`: `stamped` is raw-logical (like `base`,
`replay.rs:104`) while `committed_len()` counts *finalized* blocks (`builder.rs:204-206`), and
`coalesce_spans` collapses runs, so the two differ. Setting `stamped = committed_len` makes
`window_stamped()` exceed `out.len()` and the first `LineStart` slice out of range. `base`'s
absolute value never escapes the replayer, so rebasing is sound.

`user_times` has length `mm.session_meta.turns` — its value at `replay_from`, since by §3's
partition every uncommitted `UserText` lies at or above that offset and none has been stamped.
(`suppress` holds only `QueueEvent` markers, so no turn is ever suppressed and the count is
exact.)

Alignment lives in the accumulator because the accumulator owns `committed`. It opens no file
and decodes no `BV`, so alignment is a **pure function of two vectors** — testable with
hand-built inputs, including torn tails otherwise reachable only by killing a writer mid-write.
Loaders return two vectors; the persistence layer performs the `set_len`s and is the only writer.

### 6.4 Validate

```
valid(hdr: StreamHeader, r: Resume) -> bool:
    len(src) ≥ r.replay_from
  ∧ crc32(first_line(src)) == hdr.anchor            # checked once, not per record
  ∧ crc32(src[r.replay_from-64KiB .. r.replay_from]) == r.window
  ∧ hdr.versions == current                         # format, fold, and (HTML) flavor
```

The window is fixed-size and ends at `replay_from` because everything restored derives from
bytes **below** it; bytes at or after it are re-read and folded fresh, so a rewrite there is
self-correcting. A whole-prefix hash would re-read `[0, offset)` on every open. A single line
exceeding 64 KiB reduces this to a sub-line check.

CRC32, not a cryptographic digest: this detects accidental divergence (compaction, truncation, a
different file), never tampering, and it cannot be a trust boundary anyway — anything able to
rewrite the transcript can rewrite the cache beside it. Measured 1.6 µs per 64 KiB vs 21.7 µs for
sha256; the ~2⁻³² false-accept chance is acceptable because the window is one of three
independent checks and the anchor alone catches a different file.

Only the served rendering is cached; the offline dump writers are excluded, so the flavor space
is one and `BYTE-IDENTICAL: PASS` never depends on machine state.

### 6.5 Reset

A truncation or compaction drives `builder.reset()`. Both streams **and the store backing**
truncate to 0. Without this, content regrows from 0 while meta holds records stamped against the
old bytes, and the next open accepts a stale resume against different content — the
false-accept class §6.4 guards hardest, reached with no detection failure at all. `open_fresh`
discards the durable directory, not only the in-process store.

### 6.6 Checkpoints and compaction

A checkpoint is an **absolute `MaterializedMeta`** carried by a record that already has a resume payload. It does
three jobs.

**Written periodically**, every `CHECKPOINT_EVERY` committing drains. Cost is
O(turns + tasks + spawns) *once per interval* — the snapshot §4.3 rejects at per-commit
frequency, made affordable by amortisation. This is why `MaterializedMeta` holds a **reduced** `TaskFold`
rather than the raw op-log: a checkpoint carrying every `TaskOp` would be as large as the log it
replaces.

**It must sit on a resumable record, and that is not a convenience.** A checkpoint whose record
has no `resume` would let compaction strand the cache: truncate to a checkpoint that is also the
last record, and the stream holds complete materialized state with **no `replay_from` anywhere** — a
cache that exists and cannot be resumed from, until the next commit happens to add one. So the
writer emits a checkpoint only where I5 holds; if the scheduled drain is a straddling line
(§3), it waits for the next qualifying one.

**A reader may start at one.** `push` adopts a checkpoint and discards what it folded before, so
a stream beginning at a checkpointed record and one replayed from the start produce the same
`MaterializedMeta` — **I11**, and what the equivalence test asserts. That bounds an open's work, which is
otherwise O(records) and grows without limit for a long-lived session.

**A reader that *passes* one validates against it.** A fold that reaches a checkpoint can compare
its running state before adopting; a mismatch means the stream is corrupt or writer and reader
have drifted, and the answer is a cold rebuild. This turns I3 from a property tests assert into
one production checks on every load — so the class §6.4 guards hardest (a false accept yields
wrong output, not a no-op) gains a second, independent detector.

**Compaction is then trivial and asynchronous**, because it needs no fold:

```
compact(dir):                                   # any time, under the §9 lock
    r ← the newest record with r.checkpoint.is_some() and r.resume.id ≤ |committed|
    if r is None or records_before(r) < COMPACT_AFTER: return
    rewrite meta as [ Header(hdr), r, ...records after r ]      # temp file + rename
```

**Rewrite, not truncate-in-place**: a checkpoint replaces a *prefix* of an append-only file, so
compaction writes a new file and renames over the old one — the rename is the commit point, and a
crash mid-rewrite leaves the original intact. It runs under the same `<presentation, session>`
lock as every other write (§9), which is what makes "asynchronous" safe rather than a race.

**Never checkpoint past `n`.** Only state the content stream corroborates may become absolute.
Records above `n` are exactly those alignment rejected — a torn tail, or drains the content
stream does not support — and folding them into a checkpoint would launder unverified data into a
form nothing can later question.

## 7. The one seam addition

`MetricsAccumulator` is `push`/`finish` with no seed (`adapter.rs:24-34`), and the collapsed
`Metrics` cannot re-seed one: it exposes the derived `duration_secs` where a resumed accumulator
needs the span **endpoints**, and it has already collapsed per-model attribution.

```rust
/// Running totals as of the last `push` — per model, plus the agent-specific counter bag and
/// the observed span. The builder captures these at a turn-opening line and emits DIFFERENCES
/// (§6.1); the adapter converts whatever its agent reports (Claude increments, Codex running
/// totals) into these totals.
fn totals(&self) -> (BTreeMap<Model, TokenCounts>, BTreeMap<String, u64>,
                     Option<(EpochSeconds, EpochSeconds)>);
/// Re-seed a resumed accumulator from folded totals and the observed span.
fn reseed(&mut self, tokens: BTreeMap<Model, TokenCounts>, extra: BTreeMap<String, u64>,
          span: Option<(EpochSeconds, EpochSeconds)>);
```

Typed, agent-agnostic, and extensible where it needs to be: a counter no shared struct should
grow a field for goes in `extra`, exactly as `Metrics::extra` already works. `TimeSpan` must
expose its endpoints (`metrics.rs:63-66`). QoderWork shares Claude's accumulator, so state is
keyed by presentation **and agent id** — a Codex state handed to Claude's accumulator is a
deserialization error, not a silent misread.

**Depends on #104** (per-model token attribution). Land #104 first: it changes
`MetricsAccumulator` and `Metrics` in the same place, and doing it second would mean writing the
seam twice.

## 8. The frontend API

Everything above is invisible to a frontend. What it sees is one call.

### 8.1 Admission — exclusive, or denied

There is **exactly one writer per `<presentation, session>`, always** (I9). Admission therefore
has two outcomes, not three: you own it, or you do not and **nothing was opened**. Falling back
to a cache-less session is a separate, explicit choice the frontend makes — never something
`admit` does quietly, because a three-way outcome would suggest a session might be handed out
while another process owns it.

```rust
enum Admission<P: DurableStore> {
    /// Exclusive owner. Durable, and resumed when the cache was valid.
    Owned { session: Arc<SharedSession<P>>, origin: Origin },
    /// Not the owner. NOTHING was opened, nothing is shared.
    Denied(Denial<P::Note>),
}

enum Denial<N> {
    /// Another LIVE process holds it. `Holder` carries what a MESSAGE needs — not a lock.
    Held(Holder<N>),
    /// No durable slot exists to compete for: `--no-cache`, an unwritable root, or a platform
    /// with no liveness check (§9, where the cache is disabled rather than assumed stale).
    Unavailable(Unavailable),
}

struct Holder<N> {
    pid:  u32,
    dir:  PathBuf,
    /// What the holder published about itself — `None` in the window between taking the lock
    /// and knowing what to say (an HTML server does not have its port until it binds).
    note: Option<N>,
}

enum Unavailable { NoCacheFlag, UnwritableRoot, NoLivenessCheck }

/// Namespaces the durable directory AND the lock, so a TUI and an HTML server on the same
/// session never contend — R3's "locking is <presentation, session>".
enum Presentation { Tui, Html }

enum Origin {
    Resumed { committed: usize, replay_from: ByteOffset },
    Cold(ColdReason),
}

/// Why a cold fold happened. Diagnosable on purpose: "the cache did not help" is a support
/// question, and §12's rejection test asserts on these rather than on "it rebuilt".
enum ColdReason { NoPriorCache, SourceRewritten, VersionChanged, FlavorChanged, TornStream }
```

### 8.2 Construction

```rust
impl<P: DurableStore, A> SessionCache<P, A> {
    /// Durable under `root/<presentation>/<session>/`. `make_store` is the ONE per-frontend
    /// piece: only HTML knows its FoldPolicy/cwd/flavor, only the TUI knows it wants
    /// `Arc<Block>`. It captures that config, exactly as `serve.rs:254-261` does today.
    fn durable(p: Presentation, root: PathBuf,
               make_store: impl Fn(&Path, &Transcript) -> io::Result<P> + Send + Sync + 'static)
               -> Self;

    /// `--no-cache`: every `admit` denies with `Unavailable(NoCacheFlag)`.
    fn ephemeral() -> Self;

    /// Take exclusive ownership, or say why not. Never blocks on another holder.
    fn admit(&self, id: &str) -> Admission<P>;

    /// Publish this process's note for whoever finds the lock held. Separate from `admit`
    /// because the useful facts arrive later — a server has no port until it binds.
    fn publish(&self, id: &str, note: P::Note);

    /// The cache-less path, chosen explicitly after a denial: today's behaviour exactly —
    /// no lock, no durable directory, nothing written.
    fn open_uncached(&self, id: &str) -> Arc<SharedSession<P>>;

    fn release(&self, id: &str);   // flush + unlock one session (TUI's `Outcome::Switch`)
    fn release_all(&self);         // both `process::exit(0)` sites, which skip destructors
}
```

`SessionCache::new()` becomes `ephemeral()`; nothing else about the type changes, so `poll_view`,
`pull` and the aux slot keep working (R4).

### 8.3 The store seam

`admit` needs three things from the frontend's store that a construction closure cannot express,
because they are called *during* a load. They replace today's `PersistentStore`:

```rust
trait DurableStore: BlockStore + Sized {
    /// What this frontend leaves in its lock for a peer that finds it held: a port for the
    /// server, a tmux pane for the TUI. **Typed, not opaque** — locks are per-presentation, so
    /// the only reader is the same frontend that wrote it, and both ends know the shape.
    /// Keeping it here rather than on `Holder` is what stops a `port` field — meaningless to
    /// the TUI — from leaking into a shared type.
    type Note: Serialize + DeserializeOwned + Clone;

    /// Reload the committed `Bv`s from the backing — the only frontend-specific step in §6.2's
    /// load. HTML scans its record log for locators; the TUI decodes JSON blocks.
    fn load(&mut self) -> io::Result<Vec<Self::Bv>>;
    /// Truncate the backing to `n` committed blocks (I6). Not optional: HTML serves committed
    /// bytes as one range to EOF (`serve.rs:325-329`), so orphaned bytes are read as garbage.
    fn truncate(&mut self, n: usize) -> io::Result<()>;
    /// Discard everything — a fold reset (§6.5) or a rejected cache.
    fn reset(&mut self) -> io::Result<()>;
}
```

`hibernate_state`/`restore_state` are **deleted**, not ported: they existed to park a render
continuation across hibernation, and §4.3 establishes that no presentation state is persisted
at all.

### 8.4 What each frontend writes

A denial resolves to **fail** or **run cache-less** — the frontend picks, and the choice is
visible at the call site.

```rust
// TUI — app.rs.  Refuses a second instance; falls back only when no slot existed to compete for.
let cache = if args.no_cache { TuiCache::ephemeral() }
            else { TuiCache::durable(Presentation::Tui, cache_root()?, |dir, _| ArcStore::open(dir)) };

match cache.admit(&id) {
    Owned { session, .. }      => run(session),
    // TuiNote { pane: Option<String> } — so the refusal can name the exact pane.
    Denied(Held(h))            => bail!("session open in another claude-replay (pid {}){}; \
                                         or pass --no-cache for a second read-only view",
                                        h.pid,
                                        h.note.and_then(|n| n.pane).map_or(String::new(),
                                            |p| format!("; attach with `tmux attach -t {p}`"))),
    Denied(Unavailable(_))     => run(cache.open_uncached(&id)),
}
// …and `cache.release_all()` immediately before each `process::exit(0)`.
```

```rust
// HTML — serve.rs.  Multi-root: per session, independently. Partial success is normal.
for id in sessions {
    match cache.admit(id) {
        Owned { session, .. }              => serve(id, session),
        // HtmlNote { port: u16 }. No note yet ⇒ the holder has not bound; serve cache-less.
        Denied(Held(h)) if at_pick_time && h.note.is_some()
                                           => redirect(id, h.note.unwrap().port),
        Denied(_)                          => serve(id, cache.open_uncached(id)),
    }
}
```

**The asymmetry on `Held` is deliberate, and is why the note is frontend-typed.** The TUI *refuses*:
a second instance would fold and hold the same session in RAM twice, invisibly, and `tmux attach`
is the real sharing primitive. HTML *redirects at pick time* because the holder is already
serving that session over HTTP — the user gets what they wanted, from the process that owns it.
A child discovered mid-run cannot be handed off (the page is already open), so it serves
cache-less.

### 8.5 Why this shape

- **Two outcomes, not three.** `Owned` is the only variant carrying a durable session, so I9 —
  one writer per `<presentation, session>` — is legible in the type rather than promised in
  prose. A frontend cannot accidentally serve a shared entry, because none is ever produced.
- **The fallback is explicit.** `open_uncached` is a call the frontend makes *after* seeing a
  denial, so "we gave up on caching" is visible at the call site instead of hidden inside
  `admit`. It is also the same call for every reason — `--no-cache`, an unwritable root, a
  non-unix host, or a live holder — so those need no separate handling.
- **One call for the hard part.** Lock acquisition, the port probe, validity checking,
  alignment, truncation and the cold-rebuild decision all happen *inside* `admit`. R6 asked that
  the cache absorb state-keeping; a frontend sequencing those steps would be absorbing it.
- **`Held` carries what a message needs**, not a lock handle. A frontend cannot mishandle a lock
  it never receives, which makes "the frontend's entire lock-related code is the message"
  literally true. The note is the frontend's **own** type (§8.3), so the shared `Holder` never
  grows a `port` the TUI has no use for — the same rule that kept `serde_json::Value` out of
  the record (§4.3). `Option` on it is not defensive: a server holds the lock before it binds,
  so the window where a holder exists with nothing useful to say is real.
- **`Origin` is diagnosable.** `Cold(SourceRewritten)` and `Cold(VersionChanged)` are different
  answers to "why was it slow", and tests assert on them rather than on a rebuild happening.

## 9. Locking

One lock per `<presentation, session>`, keyed by `discover::session_id(path)` with the file stem
as fallback — both frontends currently use the bare stem, which collides across projects in a
shared durable directory. The lock is held for the process lifetime, independent of residency,
because `pull_response` reaps per request; GC therefore skips entries holding a lock. A server
holder is port-probed as well as pid-checked, through a callback the frontend injects.

| situation | behaviour |
|---|---|
| free, or holder dead | `Owned` — take it, resume if valid |
| TUI, live holder | `Denied(Held)` ⇒ quit, naming pid and (from the note) the tmux pane |
| HTML, held at pick time | `Denied(Held)` ⇒ redirect to the holder's `…?session=S`, using its published note |
| HTML, multi-root start | `admit` per session; a mix of `Owned` and `Denied` is normal, never a refusal |
| HTML, child found mid-run, held | `Denied(Held)` ⇒ `open_uncached` — the page is already open, so there is no hand-off |

The lock governs writing, not viewing. `--no-cache` is a hidden flag skipping both.

## 10. Cost

| when | work |
|---|---|
| per line | one predicate over the already-decoded messages |
| per user turn | one deque `Entry`: four scalars + the metrics totals (two small maps) |
| per committed block | HTML: none. TUI: one serialize + buffered append |
| per committing drain | one record + one 64 KiB CRC32 (1.6 µs, on bytes just read) |
| per `CHECKPOINT_EVERY` drains | one absolute `MaterializedMeta` — O(turns + tasks + spawns), amortised over the interval |
| per open | `stat` + first line + 64 KiB + fold entries since the newest checkpoint + scan content — O(entries-since-checkpoint + committed), bounded by `COMPACT_AFTER` rather than by session length |
| `--no-cache` | today's path |

## 11. Changes

| file | change |
|---|---|
| `engine/meta_stream.rs` | reshape to §4's `MetaRecord` + `StreamHeader`; the iterative `MaterializedMeta::push` reader; **DELETE** `emit_batch`, `MetaDelta`, `MetaRecord::{anchored,unanchored}` and the unanchored/supersede semantics (a0acaf4, 8b4f4cf) |
| `engine/replay.rs` | **DELETE** the current meta wiring (`meta_out`, `committed_emitted`, `last_provisional`, `drain_meta`); add the four `pub(crate)` getters + `reseed`; the replayer returns to being purely the block fold |
| `engine/builder.rs` | the turn-boundary deque + record authorship at the drain (§6.1); `restore` (§6.3); `committed_meta()`; `drain_meta` passthrough removed |
| `engine/message.rs` | `Message::can_open_turn()`, defined beside the arms it enumerates |
| `engine/tasks.rs` | `TaskOp::Resolve` emitted where `on_tool_result` joins (`tasks.rs:186-205`); `TaskFold::replay(ops)`; `TaskOp` derives serde |
| `engine/adapter.rs` | the typed metrics seam (§7); `TimeSpan` exposes its endpoints |
| `engine/reader.rs`, `engine/follow.rs` | `LineReader::open_at_offset`; `FollowParser::resume`; **RETIRE** `Position`/`open_at`/`tell` — dead code naming this feature as its consumer (`reader.rs:52-54`) whose model is superseded: it re-hashes `[0, offset)` on resume and its `DefaultHasher` is not stable across builds, unusable in a persisted format |
| `present/cache/` | §8's API — `Admission`/`Origin`/`Holder`, `durable`/`ephemeral`/`admit`/`release{,_all}`; stream writer/reader (entry-at-a-time), periodic checkpoints, async compaction (§6.6); `shared_insert_or_get` |
| `present/cache/shared.rs` | delete `Body::Hibernated`; `restore` yields `Body::Live`; replace `PersistentStore` with §8.3's `DurableStore` (`hibernate_state`/`restore_state` are deleted, not ported); **the cursor guard**: `cursor.committed_id > n_committed ⇒ resync` (§4.3) |
| `present/lock.rs` | moved from `jdi/`, retargeted |
| `html/serve.rs` | durable dir split from the ephemeral bundle dir; `admit` per session with the `Held` → redirect rule (§8.4); `RecordStore` implements `DurableStore`; derive the numbering cursor at load |
| `tui/app.rs` | cache on the non-follow path; `admit` + the `Held` refusal message (§8.4); `release_all()` at both `process::exit(0)`s and `release(id)` on `Outcome::Switch` |

**Order.** #104 → engine (1) → present streams + cursor guard (2) → delete `Body::Hibernated`
(3) → TUI durable store (4) → cache API and the `poll_view` generalisation (5) → lock move (6) →
HTML (7) → TUI (8) → GC (9).

- Step 1 carries the R5 test and **gates the rest**.
- Step 2 must restore the derived per-tick baselines — `prev_provisional` (`shared.rs:650`, set
  empty on the comment *"a hibernated body never advances"*, which step 3 invalidates) and the
  follower's `prev_committed` (`follow.rs:55-56`) — or the first poll reports `changed_from = 0`
  and re-renders everything, defeating the resume.
- Step 3 must include `RecordStore::open_append`: `reopen` leaves `cx: None` and its `put`
  panics, and `serve.rs:277` is production code.
- Step 5 must generalise `poll_view`, today concrete on `SessionCache<ArcStore, A>`
  (`cache/mod.rs:225-233`), **together with** the store factory it takes.
- Step 9's GC skips entries holding a `LOCK` without probing, with an age cap overriding the skip
  so a crashed holder cannot pin bytes forever.

Additive except the deliberate TUI single-writer refusal. Any validation failure falls back to
today's path. Release: minor.

## 12. Tests

| test | asserts |
|---|---|
| equivalence | cached vs cold, byte-identical, per presentation |
| re-invocation | zero lines parsed below `replay_from` (assert a parse count) |
| oracle (R5) | I3, run for **both** presentations against **one** reader |
| alignment | truncate either stream ⇒ I1 and I6 hold and the session resumes; a killed write yields a smaller `n`, never a rebuild |
| cursor guard | a client cursor ahead of `n_committed` resyncs — with a *matching* epoch, which is the case §4.3 identifies |
| double-apply | I4: pinned-drain fixture yields an **identical block list** (not merely matching totals — `prev_ts` drift shows only in rendered thinking durations). The open window **must span several turns**, or the two capture points coincide and the test is vacuous |
| rejection | rewritten prefix, changed format/fold version, changed flavor ⇒ full rebuild, never a partial serve — asserted on `ColdReason`, not merely on "it rebuilt" |
| crash consistency | §12.1's differential harness — every truncation of both streams resumes to a block-identical session |
| lock | two writers; dead-pid reclaim; live pid + dead port; TUI refusal text; HTML pick-time hand-off; mid-run child uncached |
| admission | each `Admission` arm reachable: `Owned{Resumed}`, `Owned{Cold(_)}` per `ColdReason`, `Held` with a live holder, `Uncached` under `--no-cache` and under an unwritable root |

### 12.1 Crash consistency — the differential harness

The interesting failures are not "does a clean resume work" but "what happens when the process
dies mid-write". Both streams are append-only and written under one lock, so every crash leaves
a **prefix** of what a clean run would have written — which makes the whole space enumerable.

**The oracle is a cold parse.** For a transcript `T`, let `cold(T)` be the session a
from-scratch fold produces. Then for *every* truncation pair:

> load the truncated streams → resume → fold the rest of `T` → the result must equal `cold(T)`,
> **block for block**, not merely in totals.

```
harness(T):
    run once to completion, keeping the full content and meta streams   # the "clean" pair
    for (c, m) in truncations(content, meta):
        write the truncated pair into a temp dir
        got = admit(...) then advance to EOF of T
        assert got.blocks == cold(T).blocks           # identical, not "matching counts"
        assert got.meta   == cold(T).meta             # turns/tools/agents/tokens/tasks/times
        assert lines_parsed <= T.lines - resumed_prefix_lines   # it actually resumed
```

**Truncations to enumerate**, surgical before random — a random-only harness reports "some seed
failed" instead of naming the shape:

| shape | why it is the interesting one |
|---|---|
| both at a record boundary, every `n` | the ordinary case; asserts alignment picks the right resume point at every depth |
| meta ahead of content | the writer died between the two appends — content is authoritative (I1), so the extra meta record must be ignored |
| content ahead of meta | the other order — resume falls back to the last record with a resume payload, and re-reads more of `T` |
| **mid-record** (a torn tail: half a JSON line) | the last entry is unparsable and must be dropped, not misread |
| **mid-record with valid-looking JSON** | truncation landing on a `}` boundary — parses, but describes a commit the content stream does not corroborate |
| truncate to zero / header only | a cache with no resume point at all ⇒ cold rebuild, not a panic |
| checkpoint present, everything before it cut | §6.6's compaction output must load like an uncompacted stream (I11) |
| checkpoint is the LAST entry | the case §8's review found: a checkpoint with no resume payload after it must still be resumable |
| random byte truncation, seeded, ×N | the residue — with the seed printed so a failure is reproducible |

**Kill a real process too.** The truncation harness covers what a crash *produces*; it does not
prove the writer only ever produces prefixes. One test spawns a real `claude-replay` against a
growing transcript, `SIGKILL`s it at a random moment (not `SIGTERM` — no destructors, no flush),
then runs the same equality assertion. `SIGKILL` specifically: the design's I6/§6.6 rename
argument and the "flush before `process::exit`" rule (§11) are both claims about surviving the
*ungraceful* path.

**Two invariants this is really testing**, and they are why equality-with-cold is the assertion
rather than "it loaded":

- **No false accept.** A resume that succeeds must be *right*. §6.4 calls this the class to
  guard hardest — wrong output rather than a no-op — and a differential test is the only thing
  that detects it, because a corrupt-but-plausible resume passes every self-consistency check.
- **No lost work beyond one commit.** A torn tail costs at most the last commit (§2). Assert
  the resumed prefix is within one commit of the clean run's, or the cache silently degrades to
  "cold rebuild every time" and nothing fails.

**Required fixture shapes** — a linear transcript passes while the design is badly broken:

| fixture | catches |
|---|---|
| pinned drain (queued prompt / skill) | `line_boundary` as a current-line check; the vacuous double-apply test |
| multi-turn line | I5's first straddle shape |
| **attachment-first prompt** | I5's second straddle shape; the fallback must still resume byte-identically |
| **orphan `CommandStdout` before a turn boundary** | the flagged-line-authors-no-block false positive (§6.1's discard rule) |
| duplicate agent id, spawned → finished → spawned in one record | `AgentEvent` ordering |
| task create whose result lands after a commit | `TaskOp::Resolve`; `pending` derivation |
| mid-session model switch | per-model `tokens` folding (#104) |
| late tool result | back-patch across the frontier |

## 13. Before step 1

*(Historical — all three held. #104 landed first; nothing else needed deciding; step 1 deleted the
superseded emission protocol rather than reconciling with it.)*

1. **#104 lands first** (per-model tokens) — §7's seam is written against its shape.
2. **Decide nothing else.** Every open question in earlier drafts is resolved in this version:
   blob typing (§4.1), versioning (§4.3), presentation state (§4.3), `epoch` (§4.3 → step 2's
   cursor guard).
3. **The code currently disagrees with this design in one place**: `engine/meta_stream.rs` and
   its `replay.rs` wiring implement the *superseded* emission protocol (anchored/unanchored
   records with accumulate-vs-supersede). It is unreleased and has no consumer. Step 1 deletes
   it rather than building on it — do not try to reconcile the two.

## 14. What the build changed

Four places where writing it settled a question the design had answered differently. Each is a
narrowing, not a reversal — the model in §§2–7 is untouched.

**`make_store` is a per-call argument, not a constructor field** (§8.2 had it on `durable`). The
context a store needs is per-*session*: a server hosting several roots renders each against its
own cwd, and a closure captured at construction cannot see it. Passing it to `admit` also drops
the `Send + Sync + 'static` boxing the field required.

**The store opens INSIDE the claim, after the lock.** §8.2's signature implied the caller opens
the backing and hands `committed_len` in, but that ordering opens a backing for a session another
process may own — and "on a denial nothing was opened" would stop being true. `claim` therefore
takes `committed_len` as a callback it invokes only once the lock is ours, and hands the lock
back if the callback cannot open the backing.

**`DurableStore` has two methods, not four.** `reset` was redundant with `BlockStore::reset`,
which already discards everything on the path that matters, and two `reset`s on one type would
force disambiguation at every call site. `truncate` merged with seeding the render continuation
into one `adopt(n, meta)`: they are the same event, and splitting them would let a continuation
count blocks the truncation just removed. §8.3's `Note` stayed exactly as designed — and is what
decides which crate a store lives in, since the impl must sit with the note's type.

**Releasing needs no `DurableStore` bound, so `Drop` can do it.** §8's `release_all` covers the
two `process::exit(0)` sites, but not the `?` paths that skip it — and a lock outliving its
process denies the session until the pid dies, which for a recycled pid is never. Reading the
holder with its note left as raw JSON makes release a pid comparison, which `Drop` can carry.

**The TUI refuses only when FOLLOWING.** §8.4's snippet refuses a held session outright, but its
own argument is about following — two live instances each folding and holding the same growing
session in RAM. Reading a transcript in a second window is neither, and refusing it would break
something that worked before there was a cache. So a held session is served cache-less on the
one-shot path: no lock, no writes, no resume, exactly today's behaviour. Relatedly, the tick loop
now gates on `--follow` rather than on "is this id registered" — every session is registered now,
for the resume, so registration no longer answers "should this view move".

**The jdi lock does not move.** §11 step 6 called for hoisting `src/jdi/lock.rs` into the
present layer and retargeting it. Writing both showed they are not the same lock: jdi's is
`mkdir`-atomic with three outcomes (`Acquired`/`AlreadyRunning`/`SetupInFlight`), is released
after *setup* rather than held for the run, and frees on `Drop`; the cache's is a JSON pidfile
with two outcomes, held for the session's life, carrying a frontend-typed note. Merging them
would force one shape onto both problems. They stay separate.

**Checkpoints ride the writer's own materialized view.** §6.6 left open how the writer builds
the absolute state a checkpoint carries. Rebuilding it from the accumulator's internals would
have needed a committed-only copy of `agent_ids` and a turn-sliced `user_times` — two places to
get subtly wrong, invisibly. Instead the accumulator keeps a running `MaterializedMeta` by
pushing each authored record through **the same fold the reader uses**, and a checkpoint is a
clone of it. It therefore cannot disagree with the stream it summarizes, and `restore` seeds it
so a resumed writer's next checkpoint still describes the whole session.

**When to checkpoint and when to compact is still policy.** `CHECKPOINT_EVERY` and
`COMPACT_AFTER` are named constants, every value of which produces a valid stream, and
`compact()` is deliberately **not** called automatically — the natural site is under the lock
right after a load, but which sessions deserve a rewrite is a question the measurements have not
yet answered. Today they do not need to: a 107 MB transcript produces **711 records / 0.43 MB**,
and its resume costs 49 ms against a 764 ms cold fold. Compaction earns its keep somewhere around
10⁵ records — roughly a 15 GB transcript.

**Two names, since §8's `Admission` needed the room.** The low-level primitive is `claim`
returning `Claim` (`Ours`/`Denied`); `Admission` is what §8 describes and what a frontend
matches on. The shared `Denial`/`Unavailable`/`Origin`/`ColdReason` types are the same in both.
`Unavailable` gained `UnknownSession` — an unregistered id is not the same answer as `--no-cache`.

## Rejected

| shape | why |
|---|---|
| Resume from a bare byte offset | the commit cut is not a byte offset: one line can carry several turns, the drain lags the open window, and spawn identity has session-long reach |
| Snapshot at the drain, re-read from the frontier | double-applies everything between; `tool_slot` is pruned, the queue is non-empty, `user_times` is a turn ahead, metrics are a mixed epoch |
| Two offsets plus a suppression list | sound, but the list must be extended by hand for every cumulative fold added to `advance_at`, and an omission is silent |
| Unanchored records restating the open turn | a resume never reads them; `open_read` already derives the same value in-process from provisional blocks, independent of `BV`; provisional blocks are not persisted |
| Opaque `serde_json::Value` payloads | inherited from a *trait* seam, where the trait cannot name every impl's state; a file format is not that situation, and every claimed blocker dissolved (§4.3) |
| Persisting presentation state | it is derived (numbering cursor), never read (sidebar index), or live-protocol (`epoch`) — §4.3 |
