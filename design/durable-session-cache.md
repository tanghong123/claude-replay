# Design: a durable, cross-run session cache

> **Status: v14 — CLEAN. Six review rounds; verified ready to implement.**
> Three review rounds; the core rule (§3) has passed a dedicated soundness pass. Earlier
> drafts rejected the idea after review; that was wrong, and the two dead-end shapes are
> kept condensed in Appendix A only so they are not re-proposed. Read §1 → §12 in order;
> the appendix is history, not guidance.

## 1. Requirements (owner)

1. **Agent-neutral** implementation. The state *format* and `checkpoint`/`resume` live in
   `claude-replay-engine` (the accumulator cannot name a `present` type); the cache, the
   lock and metadata restore live in `claude-replay-present`, which re-exports the format.
2. **Supports both frontends**, TUI and HTML.
3. **The transcript is parsed exactly once per presentation**, amortised across
   invocations. Locking is at `<presentation, session>`, so duplicating the parse and the
   metadata store *per presentation* is fine — two concurrent frontends are two
   invocations.
4. **Preserves every existing `SessionCache` benefit** — keyed residency, TTL reaping,
   delta reads, tier-b spill, `Arc` sharing, the pull protocol.
5. **Metadata construction must not depend on the choice of `BV`.**

## 2. Framing: this already exists; we are generalising it

`SharedSession::hibernate`/`restore` (`shared.rs:592-666`) is already a durable cache. It
persists committed values, the open turn as inline `Block`s, per-turn times, metrics, meta,
tasks, and the store's render continuation — and validates before trusting them. It is
merely scoped to one process's lifetime.

Four deltas make it cross-invocation:

1. write to a durable location instead of the per-run temp dir wiped at startup;
2. accept a **grown** source, not only a byte-identical one (`shared.rs:639-641`);
3. **delete `Body::Hibernated` entirely** — `restore` constructs `Body::Live` from a
   pre-seeded `FollowParser`. This is the spine of the work, not a tweak: the hibernated
   variant duplicates every read arm (`shared.rs:145-199, 433-437, 479-485`) and exists
   only to be discarded when the source grows (`hibernation_stale`, `:520-528`, and
   `serve.rs:280-286`) — exactly what this feature must stop doing. Deleting it makes
   `advance`/`poll_view`/`pull` work unchanged and leaves **one** path for the byte gate;
4. one writer per artifact set (§7).

**The one piece genuinely missing** is the rest of the in-memory agent-neutral session
state — what `hibernate` does not carry today, and therefore what a restored session needs
in order to keep folding rather than being read-only. §5 specifies it.

## 3. The rule that removes every objection: never re-process a line

Earlier drafts persisted state at one point and resumed reading from an *earlier* point.
Every failure found in review lived in that gap: lines re-read after their effects were
already folded in, producing duplicate blocks, double-counted metrics and double-stamped
turns.

**Snapshot the accumulator's full state after line K; resume at the start of line K+1.**
No processed line is ever read twice. Consequently:

- no gap, so no double-application of anything;
- **no composite frontier and no ordinal** — K+1 is a line boundary by construction, so the
  "one line straddles the commit cut" problem cannot arise;
- the open turn is not rebuilt by re-reading; it is *restored* inline — but note it is the
  replayer's **raw** window, not hibernate's `provisional` (§5.1 #1);
- in-flight state (`tool_slot`, `queue`, `suppress`, `last_skill`) is restored rather than
  re-derived, so the pinned-drain cases are just data.

Committed **content** is still written only for committed blocks (put-once, append-only) —
the open turn lives only in the small state file, rewritten per checkpoint.

## 4. Layout — and the BV boundary (requirement 5)

Everything is per `<presentation, session>`; nothing is shared between presentations.

```
$CLAUDE_REPLAY_CACHE/<presentation>/<agent>-<session-id>/
    # root default: $XDG_CACHE_HOME/claude-replay (else ~/.cache/claude-replay)
    # <presentation> ∈ { tui, html }
    # FLAVOR (html only) := the record-render fingerprint — FoldPolicy + render cwd +
    #   record-schema/build id. It distinguishes the served, --dump-html and
    #   --dump-all-html renderings, which are mutually unusable (`record_store.rs:136-137`
    #   hardcodes the served variant). It is a `BvTable` field, presentation-owned and
    #   checked at load; the TUI has no flavor (raw Blocks bake in no render parameters).
    session.state       # agent-neutral fold state — CONTAINS NO BV   ← metadata from here
    content             # this presentation's artifact: Block log (TUI) / records (HTML)
    bv.state            # committed Vec<Bv> + the store's render continuation
    LOCK                # owner pid (+ port for a server)
```

**Requirement 5 is not achievable against today's types and needs a deliberate split.**
`HibernatedSidecar<Bv>` (`shared.rs:535-554`) carries metadata *and* `committed: Vec<Bv>`
in one struct, deserialized inside an `S`-bounded impl (`:584-587, 637-638`) — so reading
metadata today *requires* naming `BV`. (`TierBSession::Sidecar`, `tier_b.rs:330-342`,
repeats the same mistake.) Three types instead:

- `SessionState` — **non-generic**: versions, `committed_meta`, metrics blob, raw
  `user_times`, task fold, raw `out`, replayer state, `cwd`, the agent id, and the
  validity quintuple `{offset, anchor, window_hash, n_committed, content_len}`.
- `BvTable<Bv>` — `epoch`, `provisional_gen`, `committed: Vec<Bv>`, `store_state`, and
  (HTML only) the **flavor fingerprint** §8 validates.
  **`content_len` lives only in `SessionState` and is authoritative** (it is what §5.1 #10
  truncates back to); `HibernatedSidecar`'s `backing_len` (`shared.rs:539-542`) is dropped
  rather than duplicated.
- **`pub fn read_session_state(path) -> Option<SessionState>` as a free function outside
  any `S`-bounded impl.** That free function *is* requirement 5's acceptance test: if it
  ever needs a `::<S>` turbofish, the requirement has been lost. §10's metadata test must
  call it with no type parameter.

Cross-validate on load: `n_committed` == the decoded record count (TUI, which has no
`bv.state`) / `BvTable.committed.len()` (HTML), else rebuild — the torn-write guard, since the files are written
separately.

**`session.state` contains no `BV` of any kind** — requirement 5. Metadata is reconstructed
from it alone, so its schema and its reconstruction code are identical for every
presentation regardless of what that presentation chose for `BV`; the `BV` table is a
separate, presentation-owned file. One implementation of metadata restore, in `present`,
forever.

**Requirement 3** is then per-presentation: the second and every later invocation of the
*same* presentation parses nothing below the checkpoint. Two presentations each keep their
own copy — accepted, and the eventual fix is not shared storage but a single process that
is both TUI app and HTML server, at which point the duplication disappears without
any cross-process sharing protocol ever having been built.

Consequently HTML never needs to read `Block`s from disk: it restores fold state from
`session.state`, serves committed content from its own rendered records, and continues
forward. The old worry that its `BV` cannot yield a `Block` never arises.

## 5. What `session.state` carries (the missing piece)

Everything the fold needs to continue, none of it `BV`-shaped. Field choice matters more
than it looks: hibernate stores *presentation-facing* values, and several of them are the
wrong side of a transformation. The correct captures are:

| group | fields |
|---|---|
| fold window | **raw `out`** (pre-`finish_turns`), `base`, `durable` (assert empty) |
| turn stamping | **raw `user_times`**, `stamped`, `pending_ts` — as one triple |
| in-flight | `tool_slot`, `queue`, `suppress`, `last_skill` |
| carried | `prev_ts`, `prev_user_text`, `delivered_rendered`, `agent_ids` |
| accumulator | `cwd`, **`committed_meta`**, the whole `TaskFold`, the metrics accumulator's opaque state (§6) |
| resume point | line-exact source offset + **first-line anchor** + **trailing-window hash** (§8) |
| versioning | cache format version, fold-logic version, and a **build id** (§5.1 #10) |

### 5.1 Corrections review found (each with its fix)

1. **`out` is not `provisional`.** `out` (`replay.rs:92`) is the raw window; hibernate's
   `provisional` comes from `open_snapshot()` → `finalize_open()` (`replay.rs:522-538`),
   which drops suppressed blocks and runs `finish_turns`. Restoring the finalized form into
   `out` corrupts every stored index — `tool_slot`, `last_skill`, `suppress`,
   `queue[].marker_idx`, `stamped` are all `base + rel` offsets into the **raw** vector.
   **Fix:** persist raw `out`; keep `provisional` only as a post-restore cross-check
   (`finalize_open(out) == stored_provisional`), a free assertion that catches this whole
   bug class — which means `provisional` is stored too, as a debug/verification field.
2. **`user_times` must be the raw vector.** `open_snapshot` clones it and *then* runs
   `stamp_user_turns` (`replay.rs:570-576`); restoring that flushed copy alongside the
   pre-flush `stamped` makes the next `LineStart` re-stamp the same turns. **Fix:** capture
   `replayer.user_times()` (raw, `replay.rs:566`) + `stamped` + `pending_ts` together.
3. **`SessionMeta` must be `committed_meta`.** `session_meta()` folds the open turn on top
   per call (`builder.rs:272-278`); storing the merged value double-counts on restore. This
   is the requirement-5 field, so getting it right *is* getting metadata right.
4. **`TaskFold.pending` is lost** if only `TaskList` is stored (`tasks.rs:103-108`): a
   `TaskCreate` before the checkpoint whose id arrives after it never becomes a task.
   **Fix:** derive serde on `TaskFold` and persist the whole fold.
5. **`LineReader::tell()` is not a line boundary** — it returns all bytes read, including a
   partial line still in `pending` (`reader.rs:166-173, 194-214`), so resuming there
   delivers a truncated line. A live-tailing server checkpoints mid-write constantly.
   **Fix:** the **driver reports the resume offset** — the last complete line boundary —
   and the accumulator only stores it. Both drivers already know it exactly
   (`advance_reader` post-increments, `builder.rs:180-181`; `LineReader` has
   `offset - pending.len()`, `reader.rs:190`). Do **not** recompute it as
   `offset + len(line) + 1`: `advance_at` receives the line already stripped of its newline
   (`builder.rs:182-187`), so CRLF undercounts, a final line with no newline overshoots
   past EOF, and `LineReader::consume` skips empty lines (`reader.rs:205`). And do **not**
   hash incrementally while folding — §8 validates from a trailing window at load time, so
   there is nothing to maintain per line.
6. **`RecordStore::reopen` cannot advance** (`cx: None`, and `put` then panics,
   `record_store.rs:128-131, 178-189`). **Fix:** keep `reopen` as the read-only hibernate
   path; add `RecordStore::open_append(path, fold, cwd, transcript, len)` — one name, used in §12
   step 4 too.
   `TierBStore::open` is already append-correct (`tier_b.rs:94-108`).
7. **Serde prerequisites (compile blockers, all one-liners).** `TimeSpan` has private
   fields and no derive (`metrics.rs:63-66`) — it is **already** re-exported (`seam.rs:45`),
   so only the derive is missing. `QueueItem`
   has no derive (`replay.rs:629`).
   **Split the `Bv` bound off the checkpoint path.** `hibernate`/`restore` live today in
   `impl<S: PersistentStore> SharedSession<S> where S::Bv: Serialize + DeserializeOwned`
   (`shared.rs:584-587`). Writing/reading `SessionState` must carry **no** `Bv` bound —
   only the `BvTable<Bv>` path is bounded — or step 4's `SharedSession<ArcTierBStore>`
   (`Bv = Arc<Block>`) cannot resolve them without serde's `rc`, which §5.1 #8 exists to
   avoid. Splitting that single bounded impl is a **step-4 prerequisite**.
   **Store the agent as a label `String`, not an `Agent`.** `Agent` *is* serde-able
   (hand-written impls, `agent.rs:53-64`) — an earlier claim here
   that it was not is withdrawn. The real limit: `from_label` resolves only the three
   built-ins (`agent.rs:39-46`), so a third-party agent's checkpoint cannot be resolved
   *inside the engine*, where the format lives. Keep the label opaque there and resolve it
   above the engine, through the registry; rebuild on failure. Related: §6's defaulted
   `checkpoint` returning `Null` means a future adapter that skips it silently restores
   zeroed metrics — call that out in the adapter docs.
8. **The TUI has no content artifact.** (Note the counters live in **one** place:
   `n_committed` and `content_len` are `SessionState` fields, so the TUI writes **no**
   `bv.state` at all — its committed values *are* the log and its `store_state` is empty.
   Cross-validation is `n_committed` == decoded record count (TUI) / `BvTable.committed.len()`
   (HTML).) `ArcStore` is pure RAM (`session.rs:94-105`), and
   switching it to `TierBStore` breaks `poll_view`'s `Bv = Arc<Block>` bound
   (`shared.rs:289`) and the one-copy-shared-by-`Arc` principle. **Fix:** an
   `ArcTierBStore` — `Bv = Arc<Block>`, `put` write-through (append the same serde-JSON
   record *and* return the `Arc`), resume decodes the log into `Vec<Arc<Block>>`. Then the
   TUI needs no `bv.state` at all — instead of an O(N) locator table — and serde's `rc`
   feature (off workspace-wide) is not needed.
9. **Where and how often to checkpoint.** Inside `SharedSession::advance`/`poll_view`
   (`shared.rs:243, 287`) **after** `advance_stream()` returns — under the existing lock,
   the only point where state, artifact and counters cannot tear — plus a clean-exit flush.
   Gate on **`committed_grew || reset`** (`reset` is returned by `advance_stream`; `committed_grew` is derived at the call site,
   `shared.rs:257, 300`) with a
   time/byte throttle. The `reset` half is mandatory, not defensive: a truncation clears
   `committed` and `BlockStore::reset` **truncates the artifact** (`tier_b.rs:198-211`,
   `record_store.rs:160-167`), leaving `committed_grew` false — without it `session.state`
   would keep claiming the old counts against a truncated file.
10. **Crash-safety was backwards in §8.** Content is appended *during* `advance_at`, state
    is written *after*, so a crash always leaves the artifact **longer** than the state
    claims. Treating a length mismatch as a rejection would force a full rebuild after
    every crash. **Two rules make that safe, and they are not optional.** (a) The checkpoint **flushes the
    content writer before** writing `session.state`, so the "artifact ≥ state" invariant
    actually holds — §8.1's append is buffered, and without the flush the artifact can be
    *shorter* than the recorded length. (b) On resume, `artifact_len > content_len`
    **truncates back** to it; `artifact_len < content_len` is a **validity failure ⇒
    rebuild**, never a `set_len` (which would zero-extend a short file into a corrupt log).
    **Fix:** record `content_len` in `session.state` and apply those two rules; write `session.state` via temp-file + `rename` so a
    torn write degrades to "no checkpoint", never "corrupt checkpoint". Also fold a **build
    id** into the version, since `reader.rs` hashes with `DefaultHasher`, whose algorithm is
    not stable across Rust versions (fails safe into a rebuild, but only if versioned).
11. **`prev_provisional` must not restore empty.** `shared.rs:650` sets it empty because a
    hibernated body never advanced; once restored sessions *do* advance, the
    `prefix_intact` check is vacuously true on the first tick and a finalization reshape
    (#54) skips its `provisional_gen` bump, so a client holding a persisted cursor appends
    wrongly. **Fix:** set it to `finalize_open(restored_out)` — which is also #1's
    cross-check value.
12. **`base` ≠ `committed.len()`.** `base` advances by *raw* blocks (`replay.rs:477, 497`)
    while the accumulator pushes *finalized* values (`builder.rs:150-158`). `n_committed`
    validates the BV table and content record count; `base` is fold state only. Do not
    conflate them in the validity check.
13. `FollowParser::prev_committed`/`prev_provisional` (`follow.rs:55-56`) need not be
    persisted — restoring them empty costs one conservative full re-render and fails safe.

14. **`std::process::exit(0)` skips every destructor** (`tui/app.rs:55-57, 91-93`, kept
    deliberately to avoid slow drops). A `Drop`-based checkpoint or lock release would
    silently never run, making a *normal quit* indistinguishable from a crash — every later
    launch takes the dead-pid reclaim path. **Fix:** explicit `checkpoint + release` before
    both exits and on `Outcome::Switch` (`app.rs:373-376`), which replaces the whole cache;
    keep `Drop` as the crash-only backstop.
15. **The TUI's dominant invocation never touches the cache.** `SessionCache` is registered
    and polled only when `args.follow` (`app.rs:469-495`); plain `claude-replay <path>`
    calls `transcript.parse()` directly, so as specified the cache would help `-f` only.
    **Fix:** route the non-follow root through the cache too — one `poll_view` whose first
    poll folds the whole file — and simply do not enter the tail loop. `tui::app::dump`
    keeps using `parse_session_enriched_as` directly, so `--dump`/`--dump-html` stay
    cache-free and the byte gate is unaffected.
16. **`reap_over_budget` gives no checkpoint hook** — it returns `()` and drops residents
    (`cache/mod.rs:142-156`), unlike `reap`, which hands them back for exactly this reason.
    **Fix:** return the evictions, like `reap`; checkpoint each before its `Arc` drops.
17. **Two-phase admission needs a cache API.** `shared_session` only accepts a factory that
    runs *under* the `pull_residents` mutex (`cache/mod.rs:162-173`). **Fix:** add
    `shared_insert_or_get(id, Arc<SharedSession<P>>)`; callers `shared_peek` → build outside
    the lock → insert. The loser of a race drops its `Arc` **and must release its lock** —
    cover that path in the lock tests.
18. **Add `LineReader::open_at_offset(path, offset)`; do NOT use `open_at`.** `open_at`
    stores `resume: Some(pos)` and the next `poll()` routes into `poll_resume`
    (`reader.rs:219-220, 245-285`), which `read_exact`s and hashes `[0, offset)` — the
    whole-prefix re-read §8 exists to avoid. `Position`'s fields are also private with no
    offset-only constructor, so the argument cannot even be built. The new entry point
    seeks, sets `anchor: None`, and never routes through `poll_resume`. **Invariant this
    creates:** a reader seeded at K *does* set `anchor` (`consume` does so,
    `reader.rs:206-208`) — but from the first line **after K**, not the file's first line.
    So its `anchor` and `tell()` are both meaningless to the cache, which owns the offset
    and the anchor itself and never calls `tell()`. Do **not** convert `advance_reader` to `LineReader`: its
    `poll` does `read_to_end` (`reader.rs:236-239`), destroying the one-line residency that
    makes multi-GB transcripts viable, and `consume` skips empty lines (`:206`) while
    `advance_reader` feeds every line — a byte-gate risk.
19. *(withdrawn — no seam change needed: the audit flags only `claude_replay_engine::`
    paths, and `serde_json` is already a direct dependency of `claude-replay-agents`.)*

### 5.2 Reuse, do not rebuild — and delete the third format

- **`LineReader::open_at_offset`** (new — §5.1 #18) for resuming at an offset; `open_at`
  and `Position` are **not** reused; **`src/jdi/lock.rs`** for the lock
  (move the primitive to `present::lockdir` + `present::sys::pid_alive`; keep jdi's
  three-way `Acquire` mapping in jdi, and let the owner file carry `pid[:port]` with each
  caller parsing, so jdi's `read_owner` keeps working); **`PersistentStore::hibernate_state`
  /`restore_state`** as §6's model.
- **`TierBSession::persist`/`load` is a third, production-unused on-disk session format**
  (`tier_b.rs:245-342`) that already claims "reload without re-folding" and repeats the
  §4 metadata/BV mixing; only an integration test uses it. Delete **`persist`/`load`/`Sidecar`, the two file constants and `to_io`** (whose only
  callers are those two, `tier_b.rs:267, 280, 302, 345` — leaving it would be a dead-code
  warning, which the gate forbids) as part of this work
  (retargeting that test at `SessionState`/`BvTable`) — not `TierBSession` itself, which is
  re-exported (`cache/mod.rs:31`) and used elsewhere — or promote it to be the writer.
  Leaving three formats is the worst outcome.
- **The `aux` slot stays out.** The TUI's `ViewSidecar` is derived, width-dependent state
  with consumer-owned validity — it must never enter `SessionState`.

## 6. The metrics seam — an existing pattern

`MetricsAccumulator` is `push`/`finish` only (`adapter.rs:23-34`), and the collapsed
`Metrics` cannot restore an accumulator: both agents hold private span endpoints, and
Codex's `model` comes from a `turn_context` line near the session start
(`codex/metrics.rs:35-39`), so without it a resumed run reports a wrong duration and no
cost.

**Do not widen `Metrics`.** Mirror `PersistentStore::hibernate_state`/`restore_state`
(`shared.rs:565-572`) — opaque `serde_json::Value`:

```rust
fn checkpoint(&self) -> serde_json::Value { Value::Null }   // defaulted
fn restore(&mut self, _v: serde_json::Value) {}
```

Verified round-trippable for both agents once `TimeSpan` derives serde (§5.1 #7). Note
`claude-replay-agents` has `serde_json` but **not** `serde` as a direct dependency, so
either add that one line (the seam audit permits it — it flags only
`claude_replay_engine::` paths) or hand-roll with `json!` + `serde_json::to_value`, which
needs no new dependency.
everything else in both accumulators is `u64`/`String`/`BTreeMap`. Note QoderWork shares
Claude's accumulator (`adapters.rs:162`), so the blob is keyed by presentation **and agent
id** — which §4's directory naming already provides.

## 7. Locking — one lock per `<presentation, session>`

One lock, keyed by the artifact directory of §4. Reclaim is liveness-based (dead pid ⇒ take
it; a server holder is additionally port-probed, since pids are recycled, via a callback injected by
the frontend — *not* because `present` would gain a dependency, which was wrong reasoning
on my part since `std::net` is std, but because only the frontend knows its port and a
callback keeps the lock testable).

- **Free, or holder dead:** take it; read + write.
- **Held, TUI:** quit, naming pid, dir and `tmux attach`. Refusing surfaces the duplicate
  fold-and-hold in RAM that happens silently today, and `tmux attach` is the real sharing
  primitive.
- **Held, HTML at pick time:** open the holder's `…?session=S`.
- **Multi-root HTML start:** acquire **per session, independently**. Partial success is
  the normal outcome, not a refusal: sessions won are cached, sessions held elsewhere are
  served uncached. The "open the holder's URL" rule applies only to the single-root
  paths (`src/lib.rs:31-35`, and the `cands.len() == 1` picker branch ~`src/lib.rs:44`); the picker path just starts with a mixed
  lock set. The TUI is the only refusing presentation.
- **Held, HTML child discovered mid-run:** serve it **uncached** — correct output, no cache
  writes. The lock governs *writing*, never *viewing*, which is what fits the multi-root
  server: it discovers children lazily and cannot know its lock set at startup.

`--no-cache` is a hidden flag (`hide = true`, precedent `jdi/mod.rs:164,167`) that skips
**the durable cache (on-disk artifacts + checkpointing) and the lock** — the in-process
`SessionCache` is unaffected — so it does permit a second TUI, running exactly today's
path. Coherent rather than a loophole: it is insurance for the cache path itself (a
liveness bug, an unwritable dir, a filesystem without locks), and hiding it keeps it from
becoming the routine way to re-enable silent duplicate folds. Where there is no real liveness
check (`pid_alive` returns `false` on non-unix, `jdi/state.rs:150-167`) the cache is
disabled rather than assuming staleness, which would fail *into* concurrent writers.

## 8. Validity — bounded cost, no per-line work

Reuse iff **all** hold, else rebuild from the transcript:

| check | cost |
|---|---|
| source length ≥ recorded `offset` | `stat` |
| **first-line identity anchor** matches | one line |
| **hash of the K bytes immediately before `offset`** matches (K = 64 KiB) | one bounded read |
| artifact lengths/counts agree with `SessionState.n_committed` | `stat` + the state file |
| format version, fold-logic version and build id match | free |
| HTML only: record **flavor** matches (§4 — the render fingerprint, which subsumes `FoldPolicy`) | free |

**Deliberately NOT a whole-prefix hash.** `reader::poll_resume` validates by re-reading and
hashing `[0, offset)` (`reader.rs:243-278`) — sound, but on a 40 MB transcript that is a
full re-read on every open, which partly defeats the point of not re-reading. A trailing
window is the right trade: transcripts are append-only, and the realistic invalidation is
compaction, which rewrites content — producing a byte-identical 64 KiB immediately before
the checkpoint offset is not a case that occurs. The first-line anchor separately catches
"different file, same shape". Retain the full-prefix hash as an opt-in paranoid mode only.

**No rolling hash is maintained while folding.** The window is read at *load* time for
validation and computed by a single bounded `pread` at *checkpoint* time. A maintained
64 KiB ring buffer would be per-line copying and is explicitly rejected.

**Who computes it.** `SessionAccumulator` has neither the path nor a seek
(`builder.rs:31-52`; `advance_reader` takes a `&mut dyn BufRead`), so it cannot. Therefore
`SessionAccumulator::checkpoint` returns **fold state + offset only**; the caller — which
owns the path (`FollowParser::path()`, `follow.rs:246`) — computes the anchor and window
hash and writes the file.

**Caveat:** a single line larger than the window (a big tool result or base64 attachment)
collapses it to a sub-line check; the opt-in full-prefix mode is the answer where that
matters.

## 8.1 Overhead budget (owner requirement)

The cache must add no meaningful cost to the steady-state path.

| when | added work | verdict |
|---|---|---|
| **per line** | the byte offset only — and `advance_at` already receives it (`builder.rs:118-131`). **No hashing, no allocation, no I/O.** | **zero** |
| **per block (committed)** | HTML: none — `RecordStore` already appends a record per commit today. TUI: one `serde_json` serialize + a **buffered** append of the block. | real but bounded; it is the price of durability, and it buys skipping the entire parse+fold on every later invocation |
| **per commit** | write `SessionState` (small: at a commit the open window is the turn just started) **+ one bounded 64 KiB `pread` + hash for the validity window**, throttled together | **O(turns + agents + tasks)** — the throttle is mandatory, not an optimisation |
| **per open** | `stat` + one line + 64 KiB + parse the state file + take the lock, **plus O(committed)** to decode the log into `Vec<Arc<Block>>` (TUI) or parse the locator table (HTML) | small, once — far below parse+fold; measured at §12 step 4 |
| **cache disabled / `--no-cache`** | nothing is tracked, written or hashed — the path is exactly today's | **zero** |

Two rules follow, and they are requirements rather than optimisations:

1. **Nothing is maintained during folding that is only needed at checkpoint time.** Offsets
   are already threaded; everything else (`out`, `tool_slot`, `agent_ids`, …) is *read* at
   checkpoint from state the fold already holds. No shadow bookkeeping.
2. **Checkpoint on commit, never per advance, and throttle it.** A poll-driven checkpoint
   would rewrite the state every `POLL_MS` (2 s, `mod.rs:46`) per session; commits are line
   boundaries by construction, so this also satisfies §3 for free. Note the state file is
   **not** small in the way "the open window is one turn" suggests — `user_times` is
   O(turns), `agent_ids` is never pruned (`replay.rs:496-502`), `committed_meta.children`
   is O(sub-agents), and the whole `TaskFold` rides along — so the **throttle is mandatory,
   not an optimisation**. The first-line anchor is read **once at open and cached**, never
   per checkpoint.

The TUI's per-block serialize is the only genuinely new steady-state cost in the design. It
is opt-out, buffered, and amortised against skipping a full parse next time — but it should
be **measured at §12 step 4**, not assumed.

## 9. Preserving `SessionCache` (requirement 4)

Nothing here changes `SessionCache`: it stays agnostic about where content lives, via the
store type parameter. Admission is **two-phase and outside the cache mutex** — resolve,
lock, validate and construct the `SharedSession` *before* calling `shared_session`, because
its factory runs under the cache-wide `pull_residents` lock (`cache/mod.rs:167-173`) whose
header states that only an O(delta) advance may happen there (`cache/mod.rs:25-27`).

Eviction: size cap (default 2 GiB, `CLAUDE_REPLAY_CACHE_MAX`) + 30-day age cap, LRU by
mtime. **Skip any entry with a `LOCK` present — no liveness probe**, since `pid_alive` is a
fork/exec (`jdi/state.rs:150-167`) and eviction must not pay it per candidate; a crashed
holder's lock is reclaimed at the next open of that session. **The age cap overrides the
skip** — a `LOCK` older than the age cap is presumed dead — so an entry whose session is
never reopened cannot hold its bytes forever.

## 10. Testing

- **Equivalence:** cached vs cold, byte-identical, per frontend and flavor. The byte gate
  cannot see this (`gate.sh:32-33` drives only `--dump`/`--dump-html`, which never construct
  a `SessionCache`), so this is a new harness.
- **Re-invocation:** a second run of the *same* presentation parses **zero** transcript
  lines below the checkpoint (assert parse count). Also assert metadata restored from
  `session.state` equals a cold fold's — the check that pins requirement 5, since the same
  assertion must pass for both presentations against the same schema.
- **Rejection:** rewritten prefix, changed fold/format version, torn artifact, changed
  flavor (records only) ⇒ full rebuild, never a partial serve.
- **Lock:** two writers; dead-pid reclaim; live-pid respected; live pid + dead port; TUI
  refusal text; HTML pick-time hand-off; mid-run child served uncached.
- **Fixture shapes (required):** a linear transcript passes while badly broken. Include a
  **pinned drain** (queued prompt / skill), a **mid-turn typed prompt**, and a **late tool
  result** — the three shapes that break a re-processing design.

## 11. Rollout

Additive **except** the deliberate TUI single-writer refusal (§7) — the one intended
behavior change, since two `-f` sessions on one transcript work today. Any *validation*
failure falls back to today's behavior, so the realistic bug is "the cache didn't help".
The exception to guard hardest is a **false accept** in §8, which yields wrong output
rather than a no-op — hence anchor + window rather than length alone, and the opt-in
full-prefix mode. Release: minor.

---

## 12. Implementation order

Nothing writes a file until the format is final; no store gains append mode until a body
exists that can use it.

1. **Engine: format + accessors + checkpoint/resume.** In `claude-replay-engine`: the
   serde derives (§5.1 #7); `MetricsAccumulator::{checkpoint, restore}` (§6); the
   non-generic `SessionState`, `BvTable<Bv>` and the free `read_session_state`;
   `Replayer::{checkpoint, restore}` (its fields are private to `replay.rs`, so this is a
   prerequisite, not a detail); `SessionAccumulator::{checkpoint, resume}` — returning fold
   state **+ offset only** (§8); `PersistentStore::flush(&mut self)` (defaulted no-op — fix (a) of §5.1 #10 has nothing to
   flush through otherwise; today's `put`s are unbuffered, so it is a superset requirement)
   with a `FollowParser::store_mut()` passthrough (`SessionAccumulator::store_mut` already
   exists, `builder.rs:236`); the artifact dir + throttle state on `SharedSession`'s
   `Inner`/`with_store`, since `advance`/`poll_view` take no path today;
   `FollowParser::resume`, `LineReader::open_at_offset`
   (§5.1 #18) and `LineReader::line_boundary() -> u64` (`self.offset - self.pending.len()`,
   the value §5.1 #5 needs — the field is private and `tell()` is ruled out) with a
   `FollowParser` passthrough; a `committed_meta()` accessor (`session_meta()` is the merged value §5.1 #3
   forbids). No cache, no frontend, no durable location. **The requirement-5 test lands
   here and gates everything after.** Byte gate unchanged.
2. **Present: rewrite `hibernate`/`restore` onto the two files**, still temp-scoped. Not a
   pure re-plumb: §5.1 #2 persists the **raw** `user_times` while the old hibernated body
   served the flushed vector, so restore must already route through the **public** path —
   `FollowParser::resume` then `open_finalized()` (`follow.rs:221`, `builder.rs:265`) — for
   `hibernate_then_restore_serves_identical_pulls_without_refold` (`shared.rs:917`) to keep
   passing. Do **not** reach `Replayer::open_snapshot`: it is `pub(crate)` in the engine
   (`replay.rs:570`) and step 2's code lives in present, so a re-export cannot widen it.
3. **Delete `Body::Hibernated`; `restore` yields `Body::Live`** (§2 delta 3). Existing
   hibernate tests are rewritten to assert *continued advance*. Must precede 4, or the
   append-mode store is built against a body that cannot `put`. Also remove the now-dead
   `hibernation_stale()` branch in `serve.rs:277-286`: with no hibernated body it is always
   `false`. **Narrow, do not delete:** that condition is
   `hibernation_stale() || poisoned()`, and the poison half is #56's drop-and-refold
   recovery — keep `if shared.poisoned()` with its `remove_pull` + `open_fresh` body; only
   the "source changed" duty moves to §8's open-time gate.
   **`RecordStore::open_append` lands in this step, not step 4.** Once `restore` yields a
   live body, the store still comes from `S::reopen`, whose `cx: None` makes the first
   commit panic (`record_store.rs:128-131`) — and `serve.rs:277` is production code, so
   the tree would be broken between steps 3 and 4, breaking step 5's bisectability promise.
4. **The TUI's durable `Arc<Block>` store** — `ArcTierBStore` beside `ArcStore` in the
   engine (`session.rs:94-105`), with its `PersistentStore` impl in present (the trait
   lives at `shared.rs:561`, as `TierBStore`'s impl does at `:575`).
   **Measure here** (§8.1): the TUI's per-block serialize and the O(committed) resume decode
   are the only new steady-state costs, and both should be numbers before they are defaults.
5. **Cache API** — `shared_insert_or_get`, `reap_over_budget` returning evictions. No
   behaviour change; both frontends keep passing. `--no-cache` (hidden) lands here so every
   later step is bisectable against it.
6. **Move the lock primitive to `present`**, retarget jdi. Independent of 1–5; must precede 7.
7. **Wire HTML** — cache dir split from the ephemeral bundle dir, per-session locks incl.
   the multi-root rule, two-phase admission, checkpoint-on-commit.
8. **Wire TUI** — non-follow path through the cache, explicit checkpoint+release before both
   `process::exit(0)`s and on `Outcome::Switch`.
9. **Eviction/GC** — last, because it must know the final layout.

**Measure at step 4** (§8.1): the TUI's per-block serialize is the only new steady-state
cost, and it should be a number before it is a default.

---

## Appendix A — two shapes that were tried and must not be re-proposed

Both failed for the *same* reason, now fixed by §3 (never re-process a line):

**A1. Resume from a bare byte offset.** The commit cut is not a byte offset —
`finalize_completed` runs once per *line* (`replay.rs:428`) and one line can carry several
turns (probe: `committed_len` 0 → 2 on one line). The drain also fires later than the open
window starts, being gated on `queue.is_empty()` (`replay.rs:439`) and capped by the
`last_skill` pin (`replay.rs:455-462`). And `agent_ids` is never pruned
(`replay.rs:496-502`), so re-folding from any non-zero offset emits
`AgentDone { agent_type: "" }` (`mod.rs:443-449`).

**A2. Composite frontier + a meta message stream.** State captured at the drain (line D)
but re-read from the frontier (line L ≤ D) double-applies everything in `(L, D]`:
`tool_slot` entries pruned at D orphan late results; the queue is non-empty at L; the
per-turn times vector is one turn ahead at every drain (`replay.rs:473-475`); metrics are a
mixed epoch (`builder.rs:150-162`); `SessionMeta.children` is mutated by `AgentDone`
(`session.rs:297-303`) so a suffix delta cannot express it.

§3 avoids all of it by restoring the open zone instead of recomputing it.
