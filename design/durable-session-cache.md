# Design: a durable, cross-run session cache

> **Status:** design v2 (not built). Task #96.
> **v1 was reviewed and its core was rejected as unsound** — see §A. That is recorded
> here in full so the approach is not proposed again. §5 is the replacement.
> One question remains for the repo owner: §10.

## 1. Problem

Every run rebuilds its on-disk artifacts from zero. `html_export::start_server` creates a
bundle under `std::env::temp_dir()` and `remove_dir_all`s it at startup
(`serve.rs:622-630`), so a session parsed, folded and rendered yesterday is parsed,
folded and rendered again today.

Transcripts are append-only in the normal case and committed block content is put-once at
the durability frontier, so *some* reuse must be possible. The question v1 got wrong was
**which** work can be skipped.

## 2. Goals / non-goals

**Goals.** Reuse expensive work across runs; never serve a stale, torn, or
differently-rendered prefix; never wedge on a lock left by a dead process; never block a
user from viewing a session because another process holds a lock; keep the capability
frontend-agnostic.

**Non-goals.** Sharing artifacts *between* frontends (§6). Caching anything derived from
interactive view state. Multi-machine caches. **Resuming the fold mid-transcript** (§A).

## 3. Layering

Lives in **`claude-replay-present`**, which already owns `cache/`.

**`SessionCache` learns nothing** — it is already agnostic via the store type parameter.
But v1's "just wrap the `shared_session` closure" is **not free**, and the doc must say
so: `shared_session` invokes that closure inside `or_insert_with` **while holding the
cache-wide `pull_residents` mutex** (`cache/mod.rs:167-173`), and that module's header
states the invariant — *"the only work under a cache lock is the brief O(delta) follower
advance"* (`cache/mod.rs:25-27`).

**Decision.** Admission is **two-phase, outside the lock**: resolve paths, take the file
lock, validate, and build the store *before* calling `shared_session`, then hand the
already-constructed `SharedSession` in. Nothing slow ever runs under the cache mutex. A
port probe, a prefix hash, and a lock acquire that sleeps in its race-grace window
(`jdi/lock.rs:49`) must never block every other session's `/pull`.

## 4. Location and layout

Root: **`$CLAUDE_REPLAY_CACHE`**, default `$XDG_CACHE_HOME/claude-replay`.

v1 keyed artifacts at `sessions/<session>/<frontend>/`. **That contradicts how the HTML
server works** and is corrected here: `start_server` is multi-root (several roots share
one bundle dir, `serve.rs:622-628`) and **lazily registers child sessions into the same
dir** as parents' streams are generated (`serve.rs:632-635`). One bundle dir cannot live
inside N session dirs, and a per-session lock would have to be acquired mid-run for every
newly discovered child.

**Decision.** Two separate things, because they have different lifetimes:

```
~/.cache/claude-replay/
  render/<frontend>/<agent>-<session-id>/    # per-SESSION, the reusable artifact (§5)
      records                                #   rendered wire records
      meta.json                              #   validity key (§5.2)
  run/<frontend>/<lock-id>/                  # per-RUN, ephemeral: bundle shell, streams
      LOCK                                   #   rendezvous record (§7)
```

The **reusable** unit is per `<session, frontend>` — that is what a later run wants. The
**run** unit (bundle dir, shell, the multi-root set) stays per-run and is disposable,
exactly as today. The lock guards *writes to a session's render dir*, so a multi-root run
takes one lock per session it actually renders, acquired lazily when that session is
first pulled — the same moment the child is registered. A session it fails to lock is
served un-cached (§7.4); no partial-failure policy is needed because each session is
independent.

## 5. What is actually reused: renders, not the fold

**Decision. The fold always runs from zero. Only *rendering* is cached.**

This is the v2 core. Folding is re-executed exactly as today, so every piece of fold
state — `agent_ids`, `cwd`, metrics, `user_times`, the task op-log, `prev_ts` — is
correct **by construction**, with no seeding, no frontier, and no new engine surgery.
What gets skipped is the expensive part: rendering each committed block to its wire
record (markdown + syntect highlighting) inside `RecordStore::put`
(`record_store.rs:122-142`).

### 5.1 Mechanism

`put` is called exactly once per committed block, in commit order, and the cached
`records` file holds those renders in the same order. So:

```
put(block, at, times):
    if cache_valid and at < cached_record_count:
        return cached_locator[at]        # reuse the bytes already on disk
    else:
        render, append, return the new locator
```

The first divergence disables reuse for the remainder of the run (a monotone switch — no
interleaving). Correctness is trivially preserved: on any doubt, render.

### 5.2 Validity — what makes cached renders reusable

v1 proposed inline FNV-1a. **That reinvents machinery that already exists**:
`claude-replay-engine/src/engine/reader.rs` implements `Position { offset,
consumed_hash, anchor, len_hint }` with `encode`/`decode`, `tell()`, `open_at()`, and
`poll_resume` — which re-reads the prefix, checks `len >= offset`, hashes it, and checks
a first-line **identity anchor** before accepting (`reader.rs:44-86, 148-173, 243-278`).
It is strictly stronger than what v1 described, and its header names its intended
consumers as `SessionAccumulator::checkpoint`/`resume` (#19) and restart persistence
(#11) (`reader.rs:52-54`).

**Decision.** Consume `reader::Position`; do not write a second hasher. Two follow-ups
the implementation must settle first: `mod reader` is `pub(crate)`
(`engine/mod.rs:13`) so it needs promoting, and whether #96 supersedes or merely
consumes #19/#11 must be answered before code lands.

Beyond source identity, cached **renders** are only reusable if the parameters they were
rendered with still hold. `RecordStore::put` renders through `cx.fold`, `cx.cwd`,
`cx.transcript` (`record_store.rs:122-142`), supplied per run from `args.fold_policy()`
(`serve.rs:601`). v1 validated **none** of these — so `--html` followed by
`--html --full` would have appended `--full` records onto a folded prefix in one file,
silently. The validity key is therefore:

| key component | why |
|---|---|
| source `Position` (offset + prefix hash + identity anchor) | the transcript is the same file and has only grown |
| `FoldPolicy` fingerprint | renders bake in fold state |
| `cwd` | renders bake in relativized paths |
| record count + `records` byte length | the artifact is not torn |
| cache format version | forward/backward compatibility |

Any mismatch ⇒ ignore the cache and render fresh. Never partially trust it.

### 5.3 STEP 0 — measure before building

The entire value of this design is the assumption that **rendering dominates folding**.
That is plausible (markdown + highlighting per block vs. `serde_json` per line) but it is
**not measured**, and if folding dominates, this design's ceiling is low and #96 should
be reconsidered rather than built.

**The first commit of #96 is an instrumented measurement** of cold-open time split into
read / fold / render on a large real session. Everything below is contingent on it.

## 6. Why artifacts are per-frontend

`RecordStore`'s `Bv` is a locator into **rendered wire records**; it deliberately does not
implement `BlockRead` (`record_store.rs:9-11`). v1 argued a shared artifact set is "not
representable" — that is circular, since one-wayness is a *choice*. The real reasons are
concrete: the store carries a render continuation (`EmitState`, `record_store.rs:87-91`)
and run-scoped `RenderCx { fold, cwd, transcript }` (`record_store.rs:30-38`). Those are
frontend-specific by nature, so per-frontend directories are the honest layout — and the
per-`<session, frontend>` lock follows.

## 7. Locking, stale recovery, hand-off

### 7.1 The lock: a move *and* a new state machine

`src/jdi/lock.rs` is the right **mechanism** (atomic mkdir + owner pidfile + pid-liveness
reclaim) and must not be forked. But two v1 claims were wrong:

- It is a **short-held setup mutex**, not a long-held writer lock: *"We hold the lock. A
  live supervisor runs lock-free"* (`lock.rs:65-66`); long-lived exclusion is done by an
  injected `session_alive` callback, and `Acquire` has three outcomes where §7.3 needs
  four. The state machine on top is genuinely new work — budget it.
- The lock is a **directory** `.lock/` containing an `owner` file, removed with
  `remove_dir_all` on drop (`lock.rs:26-28, 38-39, 71`), not a leaf file as v1's tree
  drew it.

Dependency direction checks out (root → present → core), so moving it down is graph-legal
and jdi's other `pid_alive` call sites just re-import.

### 7.2 Portability — a real correctness hole

`pid_alive` shells out to `kill -0` and **returns `false` unconditionally on non-unix**
(`jdi/state.rs:150-167`). In `present` — which is cross-platform and ships
`#[cfg(windows)]` paths — that means every lock reads as stale on Windows, every process
reclaims, and the single-writer invariant fails into **concurrent writers**, the one
outcome §12 promises cannot happen.

**Decision.** Gate durable caching on a real liveness implementation. On a platform
without one, the cache is **disabled** (run un-cached), never "assume dead". Also: one
subprocess per liveness check makes §8's eviction scan spawn a process per cached
session — the liveness check must be syscall-based, or eviction must not consult it
per-entry.

### 7.3 The behavior matrix (HTML, asking for session `S`)

| holder of `S` | behavior |
|---|---|
| none, or pid not alive | take the lock, serve, read+write the cache |
| alive, port answers, **hosts `S`** | hand off — open the holder's `…?session=S`, start no server |
| alive, port answers, does **not** host `S` | serve ourselves, **read-only**: private run dir, no cache writes |
| alive, port does **not** answer | treat as dead → reclaim (row 1) |

Row 2 supersedes v1's private-dir fallback only when the holder can already serve the
request; row 3 keeps it for when it cannot.

**Unresolved in v1, decided here:** the hand-off row has nothing to wait on (the browser
owns the session), so the handing-off process **prints the URL and exits**.

The port probe is HTTP-shaped knowledge and `present` has zero `std::net` usage.
**Decision.** The probe is an **injected callback** supplied by the frontend
(`claude-replay-html`), not a new capability in `present`.

### 7.4 Who may write

**Only the lock holder writes that `<session, frontend>` render dir.** A non-holder runs
with a private, non-cached store — fully correct, just without the benefit. This is the
whole single-writer story; no cross-process protocol beyond the lock.

## 8. Eviction

Size cap, default 2 GiB (`CLAUDE_REPLAY_CACHE_MAX`), enforced by a `render/` scan at
startup, evicting whole session dirs LRU by mtime; plus an age cap of 30 days. A **locked
(live) entry is never evicted** — subject to §7.2's constraint that the liveness check
used here must be cheap.

## 9. Work breakdown

**Step 0** — the §5.3 measurement. Everything else is contingent.

**Engine** — promote `engine::reader` (or re-export `Position`); no fold changes, no
frontier, no seeding. This is the big win of v2 over v1: **the engine is barely touched.**

**Present** — `cache/durable.rs` (location, discovery, validity, two-phase admission,
the §7.3 matrix, eviction); `cache/lock.rs` moved down + the four-state machine;
a liveness abstraction (§7.2).

**HTML** — render-reuse in `RecordStore::put` + the validity key it needs
(`RenderCx` identity); durable render dir; hand-off in the serve/picker path
(`LiveServer` already exposes `{dir, port, root_ids}` + `url_for(sid)`).

**Agents** — none. (v1 needed metrics seeding across both agent families; v2 does not.)

**TUI** — see §10.

## 10. `DECISION NEEDED` — TUI scope

The TUI uses `ArcStore`: pure RAM, no on-disk artifacts (`cache/mod.rs:225-233`). Under
v2 the cached thing is *renders*, and the TUI's renders are ratatui lines whose wrapping
depends on width and fold state — i.e. exactly the view state §2 excludes from caching.

So under v2 the honest options are narrower than under v1:

- **(a)** `present` owns location/lock/eviction/validity generically; HTML is the only
  day-one consumer; the TUI opts in if and when it grows a cacheable artifact.
- **(b)** The TUI additionally gets a durable *block* backing (`TierBStore`), caching
  parse+fold rather than render — which only pays off if Step 0 shows folding is
  expensive, and which changes the TUI's memory profile away from `ArcStore`'s
  refcount-bump resync.

**Recommendation: (a)**, revisited after Step 0.

## 11. Testing

- **Lock:** two writers one wins; dead-pid lock reclaimed; live-pid lock respected;
  live pid + dead port treated as dead.
- **Render reuse:** second run reuses records (assert renders-performed == 0 on an
  unchanged session); appended transcript reuses the prefix and renders only the tail.
- **Rejection:** changed `FoldPolicy` ⇒ no reuse; changed cwd ⇒ no reuse; rewritten
  prefix ⇒ no reuse; torn records file ⇒ no reuse.
- **Equivalence (the one that matters):** a cached run and a cold run produce
  **byte-identical** output. Note the existing byte gate does **not** cover this — it
  drives `--dump`/`--dump-html`, which never construct a `SessionCache`
  (`gate.sh:32-33`). This is new, unbudgeted test infrastructure.
- **Concurrency:** two processes, one render dir, no interleaved writes.

## 12. Rollout

Additive: any validation failure falls back to exactly today's behavior. The worst
realistic bug is "the cache didn't help", never "the viewer showed something wrong".
Release: minor.

---

## Appendix A — the rejected v1 core (resume the fold at a byte frontier)

v1 proposed: persist committed state, and on a later run re-fold **from the byte offset
where the open turn began**, skipping the prefix entirely. Two independent reviews plus a
direct probe show this is unsound. Recorded so it is not re-proposed.

**A1. The commit cut is not a byte offset.** `finalize_completed` runs once per *line*
after all of that line's messages (`replay.rs:428`), and one line can carry several user
turns — the documented #56 shape. Verified directly:

```
after line0 (offset 0):   committed_len=0
after line1 (offset 200): committed_len=2     ← line1 produced a COMMITTED block AND the open turn
```

`frontier = 200` re-emits an already-committed block; `frontier = 201` drops a block. No
`u64` names the cut.

**A2. The drain line is not the frontier line.** The cut is `rposition(UserText|Command)`
capped back by the `last_skill` pin and gated on `queue.is_empty()`
(`replay.rs:439, 443-462`). The drain routinely fires on a much later line than the one
the open window starts at, so "update it at the drain site" silently drops whole turns.
The same pins refute v1's cost claim: the open window holds every turn since the most
recent `Skill` call, not "bounded by one turn".

**A3. Fold state crosses the frontier with unbounded reach — the fatal one.**
`agent_ids` (`replay.rs:120-124`) is populated when a spawn is emitted and is **never
pruned at the drain** (`finalize_completed` retains only `suppress`/`tool_slot`/
`last_skill`, `replay.rs:496-502`). A `Completion` in the open turn resolves its spawn's
real id and type from that map (`replay.rs:361-369`) — from a spawn possibly dozens of
turns earlier. Re-folding from any non-zero frontier starts with an empty map and emits
`AgentDone { agent_type: "" }`, which is **directly visible output**
(`html_export/mod.rs:451-454`) and breaks the spawn↔done join (`session.rs:234-246`).

Because that reach is session-long, v1 §5.3's escape hatch — "move the frontier back; the
constant just gets larger" — has **no finite answer**. The only sound frontier is 0,
which is v1's own stated fallback. Also unseedable without new seams: `cwd` is
first-non-empty-wins from line 1 (`codex/model.rs:141-143`); `MetricsAccumulator` has no
seed hook (`adapter.rs:24-34`) and Codex *overwrites* from cumulative usage
(`codex/metrics.rs:41-51`); the task op-log holds unjoined pending creates
(`tasks.rs:104-107`); `user_times` in the sidecar is tail-aligned and would double-count.

**A4. The proposed assertion was untestable as written.** `patch_floor` is a raw logical
index; `committed_len()` counts post-`finish_turns` blocks — different index spaces. The
invariant that *does* hold, and is worth pinning, is `patch_floor >= base` in raw space:
every patch source is at or above `base`, so no drained block is ever mutated.

**What survived review.** The durable location, the lock and its stale-recovery, the
rendezvous/hand-off, eviction, and the observation that committed content is put-once —
all intact. Only the "skip the fold" ambition was wrong, and §5 replaces it with "skip
the render", which needs none of the machinery A1–A3 would have required.

---

## Appendix B — v3: materialize the Block stream (owner's fork, 2026-08-02)

The owner's proposal, and why it **rescues the "skip the fold" ambition that v1 lost**:

> The conversion from transcripts to Block-json is frontend agnostic, so materializing
> the Block json lets the HTML frontend benefit from what the TUI left behind. For the
> TUI the cache `BV` should still be `Block`, but the sink generates the Block json on
> the side. `SessionAccumulator` may need to support building the BVs from on-disk
> artifacts and then start tailing.

### B1. Why this works where the byte frontier did not

v1 died on §A3: fold state with session-long reach (`agent_ids`) could not be
reconstructed, so re-folding from a mid-file offset produced different bytes.

Materializing **Blocks** dissolves that, because *the blocks carry the state*.
Classifying `Replayer`'s carried fields (`replay.rs:96-124`):

| field | at resume |
|---|---|
| `agent_ids` | **derivable by scanning committed `Block::SubAgent`s** — each carries `agent_id`/`agent_type`/`tool_use_id`. This is the field that killed v1. |
| `user_times`, `stamped`, `prev_ts`, `pending_ts`, `prev_user_text`, `delivered_rendered` | small scalars/vectors — persist in the sidecar |
| `out`, `durable`, `base`, `tool_slot`, `queue`, `suppress`, `last_skill` | open-window / in-flight — **empty at a quiescent boundary** (below) |

So resume needs **no transcript replay at all**: load the committed BVs from the Block
artifact, restore a small sidecar, position the reader, tail.

### B2. Checkpoint at quiescence

The remaining fields are only safe if they are empty, so **checkpoint only at a quiescent
line boundary**:

```
quiescent := queue.is_empty() && tool_slot.is_empty() && last_skill.is_none()
             && out.len() == base          // window fully drained
```

No in-flight tool call, no queued prompt, no pending skill, nothing uncommitted. Such
points recur constantly (typically just before each new user turn). The checkpoint stores
the reader `Position` (`engine/reader.rs`, §5.2) taken at that boundary — a *line*
boundary, so §A1's "the cut falls inside a line" cannot arise: we are not naming a
semantic cut, we are naming a line at which the semantic state happens to be empty.

This is the sound form of `SessionAccumulator::checkpoint`/`resume` that
`reader.rs:52-54` already anticipates (#19).

### B3. The one seam addition

`MetricsAccumulator` still has no seed hook (`adapter.rs:24-34`). v3 needs **one** default
method — `fn seed(&mut self, _m: &Metrics) {}` — implemented by Claude's family (which
sums) and ignorable by Codex's (which overwrites from a cumulative total, so it
self-corrects on the next `token_count` line). One deliberate seam addition, versus v1's
redesign of both agent metrics families.

### B4. Lock granularity — keeping the owner's rule *and* the sharing benefit

The owner keeps locks at `<session, frontend>` so a TUI and an HTML server never block
each other. But the Block artifact is deliberately **shared** between frontends, so under
per-frontend locks alone, two concurrent frontends would both write it.

**Resolution: separate the artifact scope from the frontend scope.** Three lock scopes,
each guarding what it owns:

```
<session, blocks>     ← the shared, frontend-agnostic Block stream (one writer)
<session, html>       ← rendered wire records          (one writer)
<session, tui>        ← whatever the TUI materializes  (one writer)
```

A frontend that loses `<session, blocks>` still runs: it **reads** the Block artifact
(safe without a reader lock, since the file is append-only and a durable
`valid_up_to = N blocks` marker bounds what a reader may trust), and folds privately past
that point. So concurrency is preserved exactly as the owner requires, and the shared
artifact still gets shared. This also settles B's "hard to support TUI and HTML
simultaneously" — it isn't, once the shared artifact has its own scope.

### B5. What this changes upstream in this document

- §5's "fold always, cache only renders" becomes the **fallback**, not the core: with a
  valid Block artifact the fold is skipped too. Render caching (§5.1) still applies
  independently for HTML, and the §5.2 validity key still governs *renders*.
- §9's "the engine is barely touched" no longer holds: v3 adds
  `SessionAccumulator::checkpoint`/`resume` + the quiescence predicate. That is real
  engine work — but bounded, and it is work `reader.rs` was already designed for.
- §10's TUI question is answered by the owner: the TUI's `BV` stays `Block`, with the
  sink writing Block json on the side. The TUI therefore *does* become a day-one producer
  (and consumer) of the shared artifact.
- §5.3's Step-0 measurement matters **more**, and its question sharpens: v3 targets
  fold+parse, §5 targets render. The split decides which to build first — or whether both
  are worth it.
