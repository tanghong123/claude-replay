# Design: a durable, cross-run session cache

> **Status: v15 — rewritten around the record-stream model. Under review against §1.**
> Two earlier shapes were reviewed and rejected; they are kept condensed in Appendix A so
> they are not re-proposed. Read §1 → §11 in order; the appendix is history.

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
`{t:"meta", …}` record built by `assemble_meta` from the maintained `SessionMeta`,
`Metrics` and `TaskList` (`html_export/mod.rs:1281`). `--dump-all-html` already writes it
to disk as line 1 of each stream file. There is even an existing oracle test,
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

**Commit is the unit.** A block commits when a later turn begins (the fold cuts at the
last user-turn boundary), so a commit is always a line boundary, and the fold only commits
when the queue is empty. Every meta record is emitted at a commit.

**The offset points at the first message *after* the last committed block** — i.e. at the
line that opens the still-open turn.

**Alignment is by construction, not by cross-validation.** On load, take
`n = min(last committed id in content, last committed id in meta)`, use that meta record's
offset, and ignore anything past `n` in either stream. A crash that leaves either stream
ahead is not a special case — it is a smaller `n`. This is why there is no flush ordering,
no length cross-check and no truncate-back rule.

## 4. What the meta record carries

**Only state that spans turns.** Everything scoped to the open turn is rebuilt by
re-reading it from the offset, which costs one turn:

| rebuilt by re-reading the open turn | persisted in the meta stream |
|---|---|
| `out`, `base` — the open window | `agent_ids` — never pruned; a completion can resolve a spawn many turns back |
| `tool_slot`, `suppress`, `last_skill` — pruned to the open window at the drain | `user_times` + `stamped` |
| `queue` — provably empty at a commit | `cwd` — first-non-empty-wins, set at line 1 for Codex |
| | `committed_meta` — turns, tools, children |
| | the task fold **including `pending`** — a create whose id arrives later |
| | the metrics accumulator's opaque state (§5) |

That partition is the whole correctness argument, and it is checkable rather than a
judgement call: *does the drain prune it, or is it provably empty at a commit?* If yes it
is rebuilt; if no it is streamed.

**Requirement 7 — every record carries only what changed.** Scalars appear only when they
change; collections carry added/updated entries (upsert, since `AgentDone` mutates an
existing child); the metrics blob carries changed counters. No absolute re-statement.

*Accepted consequence:* a load replays the stream from the start — O(records), far below
O(lines). If that ever dominates, the fix is a periodic absolute record **within the same
stream**, not a format change.

**Requirement 5** holds because the record contains no `BV` of any kind: metadata restore
is one implementation, identical for every presentation, and the reader is a free function
with no type parameter. If it ever needs a `::<S>` turbofish, the requirement is lost.

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

- **No per-block sidecar.** The `BV` table and the render continuation belong to the
  store, reached through the existing `hibernate_state`/`restore_state` hook — HTML's
  `EmitState` (block anchors, turn numbers, the sidebar index) rides that hook, not
  frontend code.
- **No lock or path handling in the frontend.** It hands the cache a session id and gets a
  ready `SharedSession`. The only frontend-supplied callback is the port probe (§7), and
  only because a frontend is the only thing that knows its own port.
- **No checkpoint scheduling.** The cache decides when to write (§8).
- **The `aux` slot stays view state.** The TUI's `ViewSidecar` is derived and
  width-dependent; it must never enter the meta stream.

Acceptance is the LOC measure in §1.7, taken per frontend before and after.

## 7. Locking

One lock per `<presentation, session>` — the artifact directory. Reclaim is
liveness-based; a server holder is additionally port-probed, since pids are recycled, via
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
| **per line** | the byte offset only — `advance_at` already receives it. No hashing, no allocation, no I/O. **Zero.** |
| **per committed block** | HTML: none, it already appends a record. TUI: one `serde_json` serialize + buffered append. |
| **per commit** | one delta record — sized to the change (§1.7) |
| **per open** | `stat` + first line + 64 KiB window + replay the deltas + decode/scan the content stream — O(records + committed), far below parse+fold |
| **cache off / `--no-cache`** | exactly today's path. **Zero.** |

Two rules, not optimisations: nothing is maintained during folding that is only needed at
checkpoint time — everything is *read* at commit from state the fold already holds; and
checkpoints happen at commits, never per advance (a poll-driven write would fire every
`POLL_MS`, 2 s, per session).

The TUI's per-block serialize is the only genuinely new steady-state cost. **Measure it**
(§11 step 4) rather than assume it.

## 9. Validity

Reuse iff **all** hold, else rebuild: source length ≥ the offset; the **first-line anchor**
matches; a hash of the **64 KiB immediately before the offset** matches; format version,
**fold-logic version** and build id match; and, for HTML only, the **flavor** — the render
fingerprint (`FoldPolicy` + render cwd + record schema) distinguishing the served,
`--dump-html` and `--dump-all-html` renderings, which `record_store.rs:136-137` hardcodes
apart.

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
  gate cannot see this (`gate.sh:32-33` drives only `--dump`/`--dump-html`, which never
  construct a `SessionCache`) — new harness.
- **Re-invocation:** a second run of the same presentation parses **zero** lines below the
  resume point (assert a parse count). Assert metadata restored from the stream equals a
  cold fold's — the requirement-5 check, run for both presentations against one reader.
- **Alignment:** truncate either stream independently; load takes the min and resumes
  correctly. Kill mid-write; recovery is a smaller `n`, never a rebuild.
- **Rejection:** rewritten prefix, changed fold/format version, changed flavor ⇒ full
  rebuild, never a partial serve.
- **Lock:** two writers; dead-pid reclaim; live-pid respected; live pid + dead port; the
  TUI refusal text; HTML pick-time hand-off; mid-run child uncached.
- **Requirement 6:** LOC delta per frontend, recorded in the PR.
- **Fixture shapes (required):** a linear transcript passes while badly broken. Include a
  **pinned drain** (queued prompt / skill), a **mid-turn typed prompt**, and a **late tool
  result**.

## 11. Implementation order

1. **Engine: format + accessors.** The meta record type and its delta vocabulary;
   `Replayer::{checkpoint, restore}` (its fields are private to `replay.rs`);
   `SessionAccumulator::{checkpoint, resume}` returning fold state **+ offset only**;
   a `committed_meta()` accessor (`session_meta()` is the merged value);
   `LineReader::open_at_offset` (**not** `open_at`, which routes into `poll_resume` and
   re-reads the whole prefix) and `line_boundary()`; `MetricsAccumulator::{checkpoint,
   restore}` + the `TimeSpan` derive. **The requirement-5 test lands here and gates the
   rest.** Byte gate unchanged.
2. **Present: the stream writer/reader**, replacing `hibernate`/`restore`, still
   temp-scoped. Not a pure re-plumb: the raw `user_times` differs from the flushed vector
   the old hibernated body served, so restore routes through the public
   `FollowParser::resume` + `open_finalized()` path (`Replayer::open_snapshot` is
   `pub(crate)` and present cannot widen it).
3. **Delete `Body::Hibernated`;** `restore` yields `Body::Live`. Include
   `RecordStore::open_append` here — `reopen` leaves `cx: None` and its `put` panics, and
   `serve.rs:277` is production code, so deferring it breaks the tree. Narrow the
   `hibernation_stale() || poisoned()` branch to `poisoned()`, keeping #56's recovery.
4. **The TUI's durable `Arc<Block>` store** — `Bv = Arc<Block>` with a write-through
   `put`, so `poll_view`'s one-copy-shared-by-`Arc` property survives. **Measure here** (§8).
5. **Cache API:** `shared_insert_or_get` (two-phase admission — `shared_session` runs its
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

## Appendix A — two rejected shapes

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

**Why v15 avoids both:** it resumes at a *commit* boundary — a line boundary by
construction — re-reads only the open turn, and persists only state that provably spans
turns.

## Appendix B — removed upstream

`prev_user_text` and `delivered_rendered` were on the persisted list until #97 removed them
from the codebase entirely: measurement showed the dedup they implemented fired on genuine
resubmissions (3 of 1859 enqueues, 3s–6m30s apart) and hid them. Shipped in v1.30.0, so the
cache never has to carry them.
