# Design: a durable, cross-run session cache

> **Status:** design v4 (not built). Task #96.
> v1's core ("resume the fold from a byte offset") was reviewed and **rejected as
> unsound**; the evidence is kept in Appendix A so it is not re-proposed. v4 below
> follows the owner's direction: materialize the frontend-agnostic Block stream, and
> specify the TUI and HTML paths together so the shared/specific seam is explicit.

## 1. Problem

Every run rebuilds its artifacts from zero: `start_server` creates a bundle under
`temp_dir()` and `remove_dir_all`s it at startup (`serve.rs:622-630`). The TUI is worse
off still — it keeps everything in RAM (`ArcStore`) and persists nothing, so every open
re-parses and re-folds the whole transcript.

Transcripts are append-only and committed blocks are put-once, so reuse is possible. v1
got *which* work is reusable wrong; v4 answers it per frontend.

## 2. Goals / non-goals

**Goals.** Reuse parse+fold (and, for HTML, render) across runs; never serve a stale,
torn, or differently-parameterised prefix; never wedge on a dead process's lock; never
block a user from viewing a session; keep the mechanism in `present` so any frontend can
adopt it.

**Non-goals.** Caching interactive view state (wrapped heights depend on width and fold
state). Multi-machine caches. Resuming the fold from a bare byte offset (Appendix A).

## 3. What already exists (and must not be rebuilt)

Three pieces of this design are already implemented and tested. The v4 work is mostly
*connecting* them.

| existing | what it gives us | gap |
|---|---|---|
| **`TierBStore`** (`engine/tier_b.rs`) | **already IS the Block-JSON artifact**: append-only, newline-framed `serde_json` of `Block`, `Deferred{offset,size}` locators, a sidecar, and a `reopen` that opens read+write and resumes `len` from `SeekFrom::End(0)` so later puts append (`tier_b.rs:91-108`) | not durable across runs; not used by the TUI |
| **`engine::reader::Position`** | source identity + resume validation: `{offset, consumed_hash, anchor, len_hint}`, `open_at`, `poll_resume` re-reads the prefix, checks `len >= offset`, hashes it, checks a first-line anchor, else full re-read (`reader.rs:44-86, 148-173, 243-278`) | `mod reader` is `pub(crate)`; its header names `SessionAccumulator::checkpoint`/`resume` (#19) as the intended consumer — **unbuilt** |
| **`SharedSession::hibernate`/`restore`** | serialising committed BVs + open turn + meta/metrics/tasks and reloading them (`shared.rs:592-666`) | requires the source to be byte-**identical**, and a restored body can never `advance()` (`shared.rs:248-250`) |

## 4. The two paths, concretely

Both paths are the same shape. Only the **artifact** and the **BV** differ.

### 4.1 TUI path

- **`BV = Block`** — committed blocks live in memory, exactly as today.
- **The sink writes Block JSON on the side.** `put` keeps the `Block` (returns it as the
  BV) *and* appends its `serde_json` encoding to the durable log. That log is the
  `TierBStore` format, unchanged.
- **On first client read**, the committed vector is materialised by decoding the log:
  `Vec<Block>` in memory. (Lazy: opening a session validates and locks; nothing is
  decoded until someone actually reads.)
- **The tailing reader starts at the first line of the provisional zone** (§5.2) and
  rebuilds the open turn; subsequent tailing appends new committed blocks to the log.

The TUI artifact is **parameter-free**: a serialised `Block` bakes in no fold policy, no
width, no cwd. That is what makes it shareable across frontends *and* valid across flag
changes.

### 4.2 HTML path

- **`BV = RecordLocator`** — a pointer into the wire-format records file.
- **The sink renders at put time** (`record_store.rs:122-142`), appends the wire record,
  returns the locator. Unchanged from today.
- **On first client read**, the committed vector is materialised by **scanning the
  records file for entry boundaries** into `Vec<RecordLocator>`. Content stays on disk —
  which is the whole point of the pointer path; nothing is decoded into memory.
- Tailing behaves as in 4.1.

The HTML artifact is **parameter-bound** — but less deeply than v4 first assumed, and
the difference is worth exploiting. `RenderCx` carries `{fold, cwd, transcript}`
(`record_store.rs:30-38`), and of those:

- **`FoldPolicy` affects exactly one integer per foldable record.** The renderer emits
  `"open": 0|1` from `self.fold.collapses(b)` (`mod.rs:392-397`) and then emits the body
  **unconditionally**; the client already treats it as a starting value it may override
  (`export.js:473, 507`). So fold policy changes a flag, not content.
- `cwd` and `transcript` are **session properties, not flags** — stable for a given
  session, so they never change between runs of the same session.

**Decision: hoist the `open` flag out of the cached record and apply it at serve time.**
Then HTML's records become fold-policy-independent, `--html` and `--html --full` share
one artifact, and the only remaining validity inputs are session properties that cannot
drift. This removes most of the TUI/HTML asymmetry rather than validating around it.

### 4.3 The asymmetry that drives everything

| | TUI | HTML |
|---|---|---|
| artifact | Block JSON (canonical) | wire records (projection) |
| BV | `Block` (in memory) | locator (on disk) |
| materialise = | decode every entry | scan entry offsets |
| parameter-bound? | **no** | yes (fold, cwd, transcript) |
| can recover `Block`s from it? | yes | **no** (no `BlockRead`, deliberately) |
| shareable across frontends? | **yes** | no |

The last two rows have a sharp consequence for resume, addressed in §5.3.

## 5. Resume protocol (shared)

### 5.1 Why v1's frontier failed, in one line

Fold state with session-long reach (`agent_ids`) is never pruned at the drain, so
re-folding from any non-zero offset changes output; and the commit cut is not even
nameable as a byte offset. Full evidence in Appendix A.

### 5.2 The composite frontier

The checkpoint records **where the provisional zone begins**, as a pair:

```
frontier = (line_position, blocks_from_that_line_already_committed)
```

The line part is a `reader::Position` (§3) — a **line** boundary, so it always exists.
The ordinal handles the case that killed v1's `u64`: one line can emit several blocks,
some committed and some not (verified: `committed_len` goes 0 → 2 on a single line). On
resume we re-fold **from that line** and **discard the first `n` blocks produced**, since
they are already in the committed table.

Provisional blocks are never persisted — they are rebuilt by that re-fold, which is
bounded by the open zone.

### 5.3 Carried state: persist it, do not re-derive it

At the drain, `finalize_completed` prunes `suppress`, `tool_slot` and `last_skill` to the
open window (`replay.rs:496-502`), and the drain is gated on `queue.is_empty()`
(`replay.rs:439`). So **every one of those concerns only the provisional zone** and is
rebuilt for free by the §5.2 re-fold.

What is *not* pruned must be persisted in the checkpoint sidecar:

`agent_ids`, `cwd`, `prev_ts`, `pending_ts`, `prev_user_text`, `delivered_rendered`,
`user_times` + `stamped`, the folded `Metrics`, the task op-log, `SessionMeta`, and the
committed count.

**Decision: persist `agent_ids` directly rather than re-deriving it from committed
`SubAgent` blocks.** Re-derivation would work for the TUI but is *impossible for HTML*,
whose BV cannot yield a `Block` (§4.3). Persisting the map — small, one entry per spawn —
keeps the resume protocol identical for both frontends and is the difference between one
shared implementation and two.

### 5.3b How meta is reconstructed — snapshot vs. event stream

Session metadata (`SessionMeta`, the task op-log, `agent_ids`) is built incrementally
while parsing the raw transcript. Loading from artifacts must reproduce it without the
transcript. Three ways, debated:

**(i) Checkpoint / snapshot** — serialise the carried state at a point.
*For:* load is O(state), independent of session length; one schema, one write path; **no
replay logic, so no second implementation of fold semantics to drift**.
*Against:* rewrites the whole state per checkpoint; the snapshot's as-of point must line
up with the artifact's `valid_up_to`, so blocks appended after the last checkpoint are
either discarded or re-folded; a schema change invalidates the whole cache.

**(ii) Meta message stream** — append a small meta record as state changes; replay on load.
*For:* append-only, the same discipline as every other stream here; **alignment falls out**
— each record carries its transcript offset, so both streams fast-forward to a common
point with no waste and no skew; a torn tail truncates harmlessly; schema evolves per
record type.
*Against:* load is O(events) not O(state); and the real cost — **it is a second
serialisation surface that must mirror the fold's state transitions.** If replay drifts
from the fold, the cache silently diverges. That is exactly the bug class the byte gate
exists to prevent, and the gate cannot see the cache (`gate.sh:32-33`).

**(iii) Derive meta from the Block log — recommended.** Note what the codebase already
does: `SessionMeta` is built by folding blocks one at a time (`meta.push(b)`), the task
state is *already* an op-log replay (`engine/tasks.rs`), and `agent_ids` is derivable
from committed `Block::SubAgent`s. So "replay the meta stream" can be **"fold the Blocks
through the builders that already exist"** — no new vocabulary, no second implementation,
hence none of (ii)'s divergence risk, while keeping (ii)'s append-only alignment because
the Block log *is* the stream.

This works for HTML too, and is precisely the cross-frontend sharing the owner asked for:
HTML keeps wire records for **content** (its BV) and reads the shared Block log for
**meta**. The Block log earns its keep twice.

What (iii) cannot supply is the handful of values that are neither derivable from blocks
nor stream-shaped — `cwd`, `prev_ts`, `pending_ts`, `prev_user_text`,
`delivered_rendered`, plus the composite frontier and `valid_up_to`. Those are current
values, so they go in a **tiny scalar sidecar**: a snapshot, but of ~6 fields, which
makes every objection to (i) vanish (nothing to amplify, nothing to drift).

**Recommendation:** (iii) + the scalar sidecar. Fall back to a periodic meta snapshot
only if Step 0 shows folding the Block log for meta is too slow on HTML's path — an
optimisation, added later, that changes no interface.

**Alignment rule (all options).** The two artifact streams and the transcript must land
on one consistent point: take `n = min(block_log.valid_up_to, record_log.valid_up_to)`,
use the frontier `Position` recorded for `n`, and treat anything beyond `n` in either log
as absent. Truncate-on-load rather than trusting a torn tail.

### 5.4 The one seam addition

`MetricsAccumulator` is `push`/`finish` with no seed hook (`adapter.rs:24-34`). Resume
needs `fn seed(&mut self, _m: &Metrics) {}` — a defaulted method, implemented by Claude's
family (which sums) and ignorable by Codex's (which overwrites from a cumulative total
and self-corrects on the next `token_count` line). One deliberate addition to an audited
seam; no other `claude-replay-agents` work.

### 5.5 Validity

Reuse the cache iff **all** hold, else discard and rebuild from zero:

| check | scope |
|---|---|
| `reader::Position` accepts (grown, prefix hash + anchor match) | both |
| artifact entry count and byte length match the sidecar | both |
| **params fingerprint** matches | HTML only (fold + cwd + transcript); empty for TUI |
| sidecar format version | both |

## 6. The shared / frontend-specific split

This is the deliverable the owner asked for. Everything above the line is written once.

**Shared — `claude-replay-present` (+ the engine bits noted):**

1. Cache root resolution, per-session directory naming, discovery.
2. Lock, rendezvous record, liveness, stale reclaim, the §7 matrix, eviction.
3. Validity (§5.5) via `reader::Position`.
4. The **checkpoint sidecar** — its schema *is* frontend-agnostic, because §5.3 persists
   carried state rather than deriving it from an artifact.
5. `SessionAccumulator::checkpoint()` / `resume()` (engine): seed carried state, position
   the reader at the composite frontier, drop the first `n` re-produced blocks, continue.
6. The **append-only log primitive** — extracted from `TierBStore`, which already
   implements exactly it: append bytes, count entries, track a durable `valid_up_to`.
   Both artifacts are this log; only the payload differs.

**Frontend-specific — one small trait implemented twice:**

```rust
/// A frontend's durable artifact for one session. The log, the lock, the validity and
/// the resume protocol are shared; this is the entire per-frontend surface.
pub trait DurableArtifact: BlockStore {
    /// Parameters baked into appended entries — "" when parameter-free (and therefore
    /// shareable across frontends). TUI: "". HTML: fold + cwd + transcript.
    fn params_key(&self) -> String;

    /// Build the committed BV table from the artifact — called LAZILY, on the session's
    /// first client read. TUI: decode each entry into a `Block`. HTML: record each
    /// entry's offset/len as a locator, decoding nothing.
    fn materialize(log: &AppendLog, count: usize) -> std::io::Result<Vec<Self::Bv>>;

    /// Open the artifact for append after an existing prefix.
    fn open_append(log: AppendLog, params: RenderParams) -> std::io::Result<Self>;
}
```

So the per-frontend cost of adopting the cache is: choose a payload encoding, say whether
it is parameter-bound, and say how a BV is made from a log entry. Nothing about location,
locking, validity, or resume is duplicated.

Note the TUI store is a **composite** — `BV = Block` in memory *and* a side log — which
is the same put-does-two-things shape `RecordStore` already uses.

## 7. Locking

Three scopes, because the shared artifact and the per-frontend artifacts have different
owners:

```
<session, blocks>   ← the parameter-free Block log (§4.1) — shareable, one writer
<session, html>     ← wire records
<session, tui>      ← the TUI's own artifact set
```

Per-frontend scopes keep a TUI and an HTML server from ever blocking each other (the
owner's requirement). Giving the *shared* Block log its own scope is what lets it stay
shared: a frontend that loses that lock still **reads** it — safe without a reader lock,
because the log is append-only and the sidecar's `valid_up_to` bounds what a reader may
trust — and folds privately past that point.

**Stale recovery is liveness-based**, reclaiming a lock whose pid is dead; a holder that
also advertises a port is probed, since pids are recycled. The probe is HTTP-shaped
knowledge and `present` has zero `std::net` usage, so it arrives as an **injected
callback** from the frontend, not a new capability in `present`.

**Portability is a correctness issue, not a detail.** `pid_alive` shells to `kill -0` and
returns `false` on non-unix (`jdi/state.rs:150-167`); dropped into `present` as-is, every
lock would read stale on Windows and the design would fail *into* concurrent writers.
Durable caching is therefore **disabled** on a platform without a real liveness check.

The mechanism moves down from `src/jdi/lock.rs` (dependency direction checks out), but
note it is a short-held *setup* mutex with three outcomes, whereas this needs a long-held
writer lock with four (§7.3 of v2, retained). The state machine on top is new work.

### 7.1 Hand-off matrix (HTML, session `S`)

| holder | behavior |
|---|---|
| none / pid dead / port dead | take the lock, serve, read+write cache |
| alive, hosts `S` | hand off: print the holder's `…?session=S`, open it, exit |
| alive, does not host `S` | serve read-only: private run dir, no cache writes |

## 8. Admission must not run under the cache mutex

`shared_session` invokes its factory closure **while holding the cache-wide
`pull_residents` mutex** (`cache/mod.rs:167-173`), and that module's header states *"the
only work under a cache lock is the brief O(delta) follower advance"* (`cache/mod.rs:25-27`).
Locking, hashing a large prefix, and probing a port must therefore happen **before**
`shared_session` is called, with the constructed `SharedSession` handed in ready-made.

## 9. Eviction

Size cap (default 2 GiB, `CLAUDE_REPLAY_CACHE_MAX`) + a 30-day age cap, evicting whole
session dirs LRU by mtime; a live-locked entry is never evicted, and the liveness check
used here must be cheap enough to run per entry (§7).

## 10. Step 0 — measure first

v4 targets parse+fold (both frontends) and render (HTML). Which dominates is **not
measured**. The first commit of #96 is an instrumented cold-open split into read / fold /
render on a large real session; it decides whether the HTML render cache is worth its
parameter-validation complexity, and how much the TUI actually gains.

## 11. Work breakdown

- **Engine:** promote `reader`; `SessionAccumulator::checkpoint`/`resume` (§5.2–5.3);
  extract the append-log primitive from `TierBStore`.
- **Agents:** `MetricsAccumulator::seed` only (§5.4).
- **Present:** `cache/durable.rs` (location, validity, two-phase admission, matrix,
  eviction); `cache/lock.rs` moved down + the four-state machine; the `DurableArtifact`
  trait; a liveness abstraction.
- **HTML:** implement `DurableArtifact` for `RecordStore` (+ its params key); durable
  dir; hand-off.
- **TUI:** implement `DurableArtifact` for a composite `Block`-BV store that side-writes
  the Block log.

## 12. Testing

Lock (two writers; dead-pid reclaim; live-pid respected; live pid + dead port);
resume (append then reopen — assert only the delta is folded); rejection (rewritten
prefix; changed fold policy for HTML; torn artifact); **cached-vs-cold byte equality for
both frontends** — note the existing byte gate cannot evidence this, since
`--dump`/`--dump-html` never construct a `SessionCache` (`gate.sh:32-33`), so this is new
test infrastructure; concurrency (two processes, one artifact, no interleaved writes).

## 13. Rollout

Additive: any validation failure falls back to exactly today's behavior, so the worst
realistic bug is "the cache didn't help", never "the viewer showed something wrong".
Release: minor.

---

## Appendix A — the rejected v1 core (resume from a bare byte offset)

v1 proposed re-folding from the byte offset where the open turn began, with no committed
artifact. Rejected on three independent grounds; kept so it is not re-proposed.

**A1. The commit cut is not a byte offset.** `finalize_completed` runs once per *line*
after all that line's messages (`replay.rs:428`), and one line can carry several user
turns (the #56 shape). Verified directly:

```
after line0 (offset 0):   committed_len=0
after line1 (offset 200): committed_len=2   ← one line produced a COMMITTED block AND the open turn
```

`frontier = 200` re-emits a committed block; `frontier = 201` drops one. **v4 fixes this
with the composite (line, ordinal) frontier of §5.2.**

**A2. The drain line is not the frontier line.** The cut is `rposition(UserText|Command)`
capped back by the `last_skill` pin and gated on `queue.is_empty()`
(`replay.rs:439, 443-462`), so it routinely fires on a much later line than the open
window starts at. "Stamp it at the drain site" drops whole turns. The same pins refute
v1's cost claim: the open window holds every turn since the most recent `Skill` call.

**A3. Session-long fold state — the fatal one.** `agent_ids` (`replay.rs:120-124`) is
populated at spawn-emit and **never pruned at the drain** (`replay.rs:496-502`); an
`AgentDone` resolves its spawn's real id and type from it (`replay.rs:361-369`), possibly
dozens of turns later. Re-folding from any non-zero offset starts empty and emits
`AgentDone { agent_type: "" }` — directly visible (`html_export/mod.rs:451-454`) and it
breaks the spawn↔done join (`session.rs:234-246`). Because the reach is session-long,
v1's escape hatch ("move the frontier back") has no finite answer. **v4 fixes this by
persisting the map (§5.3) instead of re-deriving it.**

**A4. The proposed assertion was untestable.** `patch_floor` is a raw logical index;
`committed_len()` counts post-`finish_turns` blocks — different index spaces. The
invariant that *does* hold and is worth pinning is `patch_floor >= base` in raw space: no
drained block is ever mutated.
