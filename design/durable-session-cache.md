# Design: a durable, cross-run session cache

> **v22 — handoff state.** Every type is declared, every code citation verified against the tree,
> every invariant has a named enforcer. Reuse a prior invocation's parse of a transcript. Read §3
> first: the rest follows from it. §12 lists what must be true before step 1 starts.
>
> v22 adds: an **iterative** meta reader (entries are never all resident), **no positional
> correspondence** between the two streams (I2 — the link is `resume.id` alone), and
> **checkpoints** (§6.6) that bound an open's work, validate a fold in production, and make
> compaction asynchronous.

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
            Part I  — session facts (counters + gauges); on every record
            Part II — resumption (offset, checksum, fold clocks); iff resumable
```

A block commits when a later turn begins and the prompt queue is empty. There is no periodic
snapshot, no third file, and nothing schedules a write.

**Part II is in-band deliberately.** Only the record a load lands on reads it, so a separate
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
| turn/tool counts, sub-agent lifecycle, `agent_ids`, per-turn timestamps, tokens, task ops | **Part I counters** — deltas that fold |
| `cwd`, the metrics time span | **Part I gauges** — last value wins |
| `prev_ts`, `pending_ts` | **Part II** — fold clocks, read only by a resume |

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

Either way the record omits Part II and a load falls back to the previous qualifying record —
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

Part I's two classes are the counter/gauge distinction:

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
    // ── Part I — session facts. Every record. A metadata reader needs ONLY this.
    turns:      Option<u32>,                          // counter: user turns that committed here
    tools:      Option<u32>,                          // counter: tool calls that committed here
    agents:     Vec<AgentEvent>,                      // counter: ordered; see the note below
    user_times: Vec<Option<EpochSeconds>>,            // counter: the turns THIS drain stamped
    tokens:     BTreeMap<Model, TokenCounts>,         // counter: per-model increments (§7)
    extra:      BTreeMap<String, u64>,                // counter: a repeated key ADDS
    task_ops:   Vec<TaskOp>,                          // counter: ordered op-log
    cwd:        Option<String>,                       // gauge: first non-empty wins
    span:       Option<(EpochSeconds, EpochSeconds)>, // gauge: session min/max timestamps

    // ── Part II — resumption. Its PRESENCE is the resume-point indicator (I5); there is no
    //    separate flag. A metadata reader skips it entirely.
    resume:     Option<Resume>,
}

struct Resume {
    id:          usize,                  // committed-block count after this drain
    replay_from: ByteOffset,             // §3's partition offset
    window:      u32,                    // CRC32 of the 64 KiB ending at replay_from
    prev_ts:     Option<EpochSeconds>,   // the thinking clock's zero
    pending_ts:  Option<EpochSeconds>,   // stamps turns authored on the resume's first line
}

// ── stream entries ────────────────────────────────────────────────────────────
/// The meta stream is a sequence of these. A `Checkpoint` is an absolute `Part1` — the fold of
/// everything before it — so a reader may START at the last one it sees and ignore all earlier
/// entries (§6.6).
enum MetaEntry {
    Header(StreamHeader),        // first entry, once
    Checkpoint(Checkpoint),      // absolute state; written only by compaction
    Record(MetaRecord),          // one per committing drain
}

struct Checkpoint { id: usize, state: Part1 }   // `id` = committed-block count it represents

// ── the reader half ───────────────────────────────────────────────────────────
/// **Iterative, not slice-at-once.** Unlike `Vec<BV>` — which is resident by definition, being
/// the committed index itself — meta entries are consumed one at a time and never all held.
/// A reader streams the file; a resume stops feeding at its aligned `n`.
///
/// `push` has NO bound check: the CALLER guarantees it feeds nothing past `n` (§6.2). Keeping
/// the fold ignorant of the alignment is what lets a metadata reader (#98) use the same type
/// with no bound at all.
impl Part1 {
    fn seed(&mut self, c: Checkpoint);      // adopt an absolute state; discards what came before
    fn push(&mut self, r: &MetaRecord);     // counters accumulate, gauges replace
}

struct Part1 {
    session_meta: SessionMeta,                  // turns, tools, children — from `agents`
    agent_ids:    HashMap<String, (AgentId, String)>, // spawn identity, BOTH keys per Spawn
    user_times:   Vec<Option<EpochSeconds>>,
    tokens:       BTreeMap<Model, TokenCounts>,
    extra:        BTreeMap<String, u64>,
    tasks:        TaskFold,                     // ops APPLIED as they arrive — list + pending
    cwd:          String,
    span:         Option<(EpochSeconds, EpochSeconds)>,
}

// ── Part I payload types ──────────────────────────────────────────────────────
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
| **resume** (the cache) | Part I **and** Part II | re-seeds the fold, then re-reads from `replay_from` |
| **metadata reader** | Part I only | no fold, no adapter, **no transcript** — the property that makes a machine-wide monitor (#98) cheap |

`prev_ts`/`pending_ts` are fold-continuation state, not session facts: they appear nowhere
outside `replay.rs`, and what they produce is carried elsewhere — `pending_ts` produces
`user_times` (its own field), `prev_ts` produces `Thinking.duration_secs`, which is **block
content** in the content stream. The monitor property holds only as long as every *displayed*
fact stays in Part I rather than being recomputed from blocks.

### 4.3 Why the shape is what it is

**The two-part split earns its keep.** It makes the two consumers a type fact; it removes I5's
indicator field (*"resumable here"* **is** `resume.is_some()`); and it deletes the one way to get
the gauge rule wrong — `prev_ts`/`pending_ts` ride Part II, so a resume reads them from the
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
| I3 | A `Part1` folded over the entries up to `n` == the maintained `SessionMeta` / `TaskList` / metrics over `committed[..n]`. | oracle test |
| I4 | For every record with `resume`, each **gauge**'s value at that record — its last present value ≤ it — and each **counter**'s fold through it equal the fold's value at the start of that record's `replay_from` line. | §6.1; double-apply test |
| I5 | `resume.is_some()` ⟺ the §3 partition exists at this drain. Then `resume.replay_from` is that partition's offset. | `line_boundary()` (§6.1) |
| I6 | After a load, `\|committed\| == n`, and content stream, meta stream and store backing are all truncated to `n` before any append. | §6.2 |
| I7 | Across a resume, `committed_len()` is unchanged until the first genuinely new commit. | `debug_assert!`; violation ⇒ cold rebuild |
| I8 | A record is reused only if §6.4 passes: length, anchor, window, versions. | §6.4 |
| I9 | At most one live process writes a `<presentation, session>`. Where liveness is undecidable (`pid_alive` is unix-only, `jdi/state.rs:150-167`) the cache is **disabled**, never assumed stale. | §8 |
| I10 | A fold reset truncates both streams and the store backing to 0. | §6.5 |
| I11 | Seeding from a `Checkpoint` and folding from the stream's start yield the **same** `Part1`. A reader that passes a checkpoint while folding compares against it; a mismatch ⇒ cold rebuild. | §6.6; equivalence test |

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

**A record with no Part II still carries its Part I counters.** They are not lost — the next
qualifying record's `tokens` delta is measured from `emitted`, which only advances when Part II
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
    # the running Part1 and the best resume so far is retained.
    p1 ← Part1::default(); hdr ← None; best ← None
    for entry in meta_entries(dir):             # drops a torn trailing entry
        match entry:
            Header(h)     → hdr ← h
            Checkpoint(c) → if c.id ≤ |committed| { p1.seed(c); best ← None }   # §6.6
            Record(r)     →
                if r.resume.is_some() and r.resume.id > |committed|: break      # I1's bound
                p1.push(r)                      # the CALLER enforces the bound, not the fold
                if r.resume.is_some(): best ← Some(r.resume)                    # by id, not index
    if hdr is None or best is None or !valid(hdr, best): return None            # cold rebuild
    n ← best.id
    truncate(content, n); truncate(meta, after the entry carrying id n); store.truncate(n)  # I6
    return Loaded { committed: committed[..n], part1: p1, resume: best }
```

Truncation is not optional: HTML serves committed bytes as one range to EOF
(`serve.rs:325-329`), so bytes past `n` are read as garbage, and a later append would sit behind
records already replayed.

### 6.3 Restore

```
SessionAccumulator::restore(adapter, store, ld: Loaded) -> Restored:
    n   ← ld.resume.id                          # NOT an index into any vector (I2)
    p1  ← ld.part1                              # already folded by the streaming load
    acc ← with_store(adapter, store)
    acc.committed      ← ld.committed
    acc.committed_meta ← p1.session_meta        # turns, tools, children (from `agents`)
    acc.cwd            ← p1.cwd
    acc.task_fold ← p1.tasks                    # list AND pending both already folded (§4.3)
    acc.metrics.reseed(p1.tokens, p1.extra, p1.span)
    acc.replayer.reseed(p1.agent_ids,                  # spawn identity, both keys
                        p1.user_times,                 # len == p1.session_meta.turns
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

`user_times` has length `p1.session_meta.turns` — its value at `replay_from`, since by §3's
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

A `Checkpoint` is an absolute `Part1` — the fold of everything before it. It does three jobs.

**Written periodically by the writer**, every `CHECKPOINT_EVERY` committing drains, alongside the
ordinary records. Cost is O(turns + tasks + spawns) *once per interval*, not per commit — the
same snapshot §4.3 rejects at per-commit frequency, made affordable by amortisation. This is why
`Part1` holds a **reduced** `TaskFold` rather than the raw op-log: a checkpoint carrying every
`TaskOp` would be as large as the log it is meant to replace.

**A reader may start at one.** `seed` replaces whatever came before, so a stream beginning with a
checkpoint and one replayed from the start produce the same `Part1` — **I11**, and what the
equivalence test asserts. That bounds an open's work: without checkpoints it is O(records) and
grows without limit for a long-lived session.

**A reader that *passes* one validates against it.** A fold that reaches a checkpoint compares
its running state; a mismatch means the stream is corrupt or the writer and reader have drifted,
and the answer is a cold rebuild. This turns I3 from a property tests assert into one production
checks on every load — the class §6.4 calls the one to guard hardest (a false accept yields
wrong output, not a no-op) now has a second, independent detector.

**Compaction is then trivial and asynchronous.** Because checkpoints already exist in the stream,
discarding a prefix requires no fold:

```
compact(dir):                                   # any time, under the §8 lock
    c ← the newest Checkpoint with c.id ≤ |committed|
    if c is None or entries_before(c) < COMPACT_AFTER: return
    rewrite meta as [ Header(hdr), Checkpoint(c), ...entries after c ]   # temp + rename
```

**Rewrite, not truncate-in-place**: a checkpoint replaces a *prefix* of an append-only file, so
compaction writes a new file and renames over the old one — the rename is the commit point, and a
crash mid-rewrite leaves the original intact. It runs under the same `<presentation, session>`
lock as every other write (§8), which is what makes "asynchronous" safe rather than a race.

**Never checkpoint past `n`.** Only state corroborated by the content stream may become absolute.
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

## 8. Locking

One lock per `<presentation, session>`, keyed by `discover::session_id(path)` with the file stem
as fallback — both frontends currently use the bare stem, which collides across projects in a
shared durable directory. The lock is held for the process lifetime, independent of residency,
because `pull_response` reaps per request; GC therefore skips entries holding a lock. A server
holder is port-probed as well as pid-checked, through a callback the frontend injects.

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
| per user turn | one deque `Entry`: four scalars + the metrics totals (two small maps) |
| per committed block | HTML: none. TUI: one serialize + buffered append |
| per committing drain | one record + one 64 KiB CRC32 (1.6 µs, on bytes just read) |
| per `CHECKPOINT_EVERY` drains | one absolute `Part1` — O(turns + tasks + spawns), amortised over the interval |
| per open | `stat` + first line + 64 KiB + fold entries since the newest checkpoint + scan content — O(entries-since-checkpoint + committed), bounded by `COMPACT_AFTER` rather than by session length |
| `--no-cache` | today's path |

## 10. Changes

| file | change |
|---|---|
| `engine/meta_stream.rs` | reshape to §4's `MetaEntry`/`MetaRecord`/`Checkpoint` + `StreamHeader`; the iterative `Part1::{seed, push}` reader; **DELETE** `emit_batch`, `MetaDelta`, `MetaRecord::{anchored,unanchored}` and the unanchored/supersede semantics (a0acaf4, 8b4f4cf) |
| `engine/replay.rs` | **DELETE** the current meta wiring (`meta_out`, `committed_emitted`, `last_provisional`, `drain_meta`); add the four `pub(crate)` getters + `reseed`; the replayer returns to being purely the block fold |
| `engine/builder.rs` | the turn-boundary deque + record authorship at the drain (§6.1); `restore` (§6.3); `committed_meta()`; `drain_meta` passthrough removed |
| `engine/message.rs` | `Message::can_open_turn()`, defined beside the arms it enumerates |
| `engine/tasks.rs` | `TaskOp::Resolve` emitted where `on_tool_result` joins (`tasks.rs:186-205`); `TaskFold::replay(ops)`; `TaskOp` derives serde |
| `engine/adapter.rs` | the typed metrics seam (§7); `TimeSpan` exposes its endpoints |
| `engine/reader.rs`, `engine/follow.rs` | `LineReader::open_at_offset`; `FollowParser::resume`; **RETIRE** `Position`/`open_at`/`tell` — dead code naming this feature as its consumer (`reader.rs:52-54`) whose model is superseded: it re-hashes `[0, offset)` on resume and its `DefaultHasher` is not stable across builds, unusable in a persisted format |
| `present/cache/` | stream writer/reader (entry-at-a-time, both directions), periodic checkpoints, async compaction (§6.6), `Admission`, `shared_insert_or_get`, `--no-cache` |
| `present/cache/shared.rs` | delete `Body::Hibernated`; `restore` yields `Body::Live`; **the cursor guard**: `cursor.committed_id > n_committed ⇒ resync` (§4.3) |
| `present/lock.rs` | moved from `jdi/`, retargeted |
| `html/serve.rs` | durable dir split from the ephemeral bundle dir; per-session locks incl. the multi-root rule; `RecordStore::open_append`; derive the numbering cursor at load |
| `tui/app.rs` | cache on the non-follow path; flush + lock release at both `process::exit(0)`s and on `Outcome::Switch` |

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

## 11. Tests

| test | asserts |
|---|---|
| equivalence | cached vs cold, byte-identical, per presentation |
| re-invocation | zero lines parsed below `replay_from` (assert a parse count) |
| oracle (R5) | I3, run for **both** presentations against **one** reader |
| alignment | truncate either stream ⇒ I1 and I6 hold and the session resumes; a killed write yields a smaller `n`, never a rebuild |
| cursor guard | a client cursor ahead of `n_committed` resyncs — with a *matching* epoch, which is the case §4.3 identifies |
| double-apply | I4: pinned-drain fixture yields an **identical block list** (not merely matching totals — `prev_ts` drift shows only in rendered thinking durations). The open window **must span several turns**, or the two capture points coincide and the test is vacuous |
| rejection | rewritten prefix, changed format/fold version, changed flavor ⇒ full rebuild, never a partial serve |
| lock | two writers; dead-pid reclaim; live pid + dead port; TUI refusal text; HTML pick-time hand-off; mid-run child uncached |

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

## 12. Before step 1

1. **#104 lands first** (per-model tokens) — §7's seam is written against its shape.
2. **Decide nothing else.** Every open question in earlier drafts is resolved in this version:
   blob typing (§4.1), versioning (§4.3), presentation state (§4.3), `epoch` (§4.3 → step 2's
   cursor guard).
3. **The code currently disagrees with this design in one place**: `engine/meta_stream.rs` and
   its `replay.rs` wiring implement the *superseded* emission protocol (anchored/unanchored
   records with accumulate-vs-supersede). It is unreleased and has no consumer. Step 1 deletes
   it rather than building on it — do not try to reconcile the two.

## Rejected

| shape | why |
|---|---|
| Resume from a bare byte offset | the commit cut is not a byte offset: one line can carry several turns, the drain lags the open window, and spawn identity has session-long reach |
| Snapshot at the drain, re-read from the frontier | double-applies everything between; `tool_slot` is pruned, the queue is non-empty, `user_times` is a turn ahead, metrics are a mixed epoch |
| Two offsets plus a suppression list | sound, but the list must be extended by hand for every cumulative fold added to `advance_at`, and an omission is silent |
| Unanchored records restating the open turn | a resume never reads them; `open_read` already derives the same value in-process from provisional blocks, independent of `BV`; provisional blocks are not persisted |
| Opaque `serde_json::Value` payloads | inherited from a *trait* seam, where the trait cannot name every impl's state; a file format is not that situation, and every claimed blocker dissolved (§4.3) |
| Persisting presentation state | it is derived (numbering cursor), never read (sidebar index), or live-protocol (`epoch`) — §4.3 |
