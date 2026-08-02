# Design: a durable, cross-run session cache

> **Status: v7 — no known blocker. Ready for review, then implementation.**
> Earlier drafts rejected the idea after review; that was wrong. Every objection is
> answered here (§3), and the two dead-end shapes are kept condensed in Appendix A only so
> they are not re-proposed.

## 1. Requirements (owner)

1. **Agent-neutral** implementation — lives in `claude-replay-present`.
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
3. let a restored session **keep advancing** (`shared.rs:248-250`);
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
    session.state       # agent-neutral fold state — CONTAINS NO BV   ← metadata from here
    content             # this presentation's artifact: Block log (TUI) / records (HTML)
    bv.state            # committed Vec<Bv> + the store's render continuation
    LOCK                # owner pid (+ port for a server)
```

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
| resume point | line-exact source offset + prefix hash + identity anchor (§5.1 #5) |
| versioning | cache format version, fold-logic version, and a **build id** (§5.1 #10) |

### 5.1 Corrections review found (each with its fix)

1. **`out` is not `provisional`.** `out` (`replay.rs:92`) is the raw window; hibernate's
   `provisional` comes from `open_snapshot()` → `finalize_open()` (`replay.rs:522-538`),
   which drops suppressed blocks and runs `finish_turns`. Restoring the finalized form into
   `out` corrupts every stored index — `tool_slot`, `last_skill`, `suppress`,
   `queue[].marker_idx`, `stamped` are all `base + rel` offsets into the **raw** vector.
   **Fix:** persist raw `out`; keep `provisional` only as a post-restore cross-check
   (`finalize_open(out) == stored_provisional`), a free assertion that catches this whole
   bug class.
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
   **Fix (preferred):** have `SessionAccumulator` compute the offset itself as
   `offset_K + len(line_K) + 1` and hash the prefix incrementally in `advance_at` — which
   also gives the cold path (`advance_reader`, `builder.rs:171-191`) an anchor without
   owning a `LineReader`.
6. **`RecordStore::reopen` cannot advance** (`cx: None`, and `put` then panics,
   `record_store.rs:128-131, 178-189`). **Fix:** keep `reopen` as the read-only hibernate
   path; add `RecordStore::resume(path, fold, cwd, transcript, len)` opening for append.
   `TierBStore::open` is already append-correct (`tier_b.rs:94-108`).
7. **Serde prerequisites (compile blockers, all one-liners).** `TimeSpan` has private
   fields and no derive (`metrics.rs:63-66`) — derive + re-export via `seam`. `QueueItem`
   has no derive (`replay.rs:629`). **`Agent` cannot be deserialized** — it is
   `&'static str` (`agent.rs:16`); store the id string and resolve through the registry at
   restore, never serde the `Agent`.
8. **The TUI has no content artifact.** `ArcStore` is pure RAM (`session.rs:94-105`), and
   switching it to `TierBStore` breaks `poll_view`'s `Bv = Arc<Block>` bound
   (`shared.rs:289`) and the one-copy-shared-by-`Arc` principle. **Fix:** an
   `ArcTierBStore` — `Bv = Arc<Block>`, `put` write-through (append the same serde-JSON
   record *and* return the `Arc`), resume decodes the log into `Vec<Arc<Block>>`. Then the
   TUI's `bv.state` collapses to `{n_committed, content_len}` instead of an O(N) locator
   table, and serde's `rc` feature (off workspace-wide) is not needed.
9. **Where and how often to checkpoint.** Inside `SharedSession::advance`/`poll_view`
   (`shared.rs:243, 287`) **after** `advance_stream()` returns — under the existing lock,
   the only point where state, artifact and counters cannot tear — plus a clean-exit flush.
   Gate on `committed_grew` (already computed, `shared.rs:257, 300`) with a time/byte
   throttle; checkpointing when only the open turn moved buys nothing.
10. **Crash-safety was backwards in §8.** Content is appended *during* `advance_at`, state
    is written *after*, so a crash always leaves the artifact **longer** than the state
    claims. Treating a length mismatch as a rejection would force a full rebuild after
    every crash. **Fix:** record `content_len` in `session.state` and **truncate the
    artifact back to it on resume**; write `session.state` via temp-file + `rename` so a
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

Verified round-trippable for both agents once `TimeSpan` derives serde (§5.1 #7);
everything else in both accumulators is `u64`/`String`/`BTreeMap`. Note QoderWork shares
Claude's accumulator (`adapters.rs:162`), so the blob is keyed by presentation **and agent
id** — which §4's directory naming already provides.

## 7. Locking — one lock per `<presentation, session>`

One lock, keyed by the artifact directory of §4. Reclaim is liveness-based (dead pid ⇒ take
it; a server holder is additionally port-probed, since pids are recycled, via a callback
injected by the frontend so `present` grows no `std::net` dependency).

- **Free, or holder dead:** take it; read + write.
- **Held, TUI:** quit, naming pid, dir and `tmux attach`. Refusing surfaces the duplicate
  fold-and-hold in RAM that happens silently today, and `tmux attach` is the real sharing
  primitive.
- **Held, HTML at pick time:** open the holder's `…?session=S`.
- **Held, HTML child discovered mid-run:** serve it **uncached** — correct output, no cache
  writes. The lock governs *writing*, never *viewing*, which is what fits the multi-root
  server: it discovers children lazily and cannot know its lock set at startup.

`--no-cache` is a hidden flag (`hide = true`, precedent `jdi/mod.rs:164,167`) — insurance
for the cache path itself, not a way to force a second TUI. Where there is no real liveness
check (`pid_alive` returns `false` on non-unix, `jdi/state.rs:150-167`) the cache is
disabled rather than assuming staleness, which would fail *into* concurrent writers.

## 8. Validity

Reuse iff **all** hold, else rebuild from the transcript: source length ≥ recorded and its
prefix hash + first-line identity anchor match (`reader::Position`'s scheme,
`reader.rs:44-86` — note it is `pub(crate)` and the cold-build path
(`advance_reader`, `builder.rs:171-191`) currently has no `LineReader`, so a write-side
anchor must be added); artifact lengths and counts match; the cache format **and fold-logic
versions** match. For HTML's records additionally: the record *flavor*
(served / `--dump-html` / `--dump-all-html` — `put` hardcodes the served variant,
`record_store.rs:136-137`) and the `FoldPolicy` (which bakes into one emitted flag,
`mod.rs:396`, and cannot be recomputed downstream because `BlockKind::html` is
non-injective against `fold_key`, `model.rs:375-415`).

Note `session.state` is **flavor- and policy-independent** — only the rendered projection
is parameterised. So within the HTML presentation a `--full` run still reuses the parse,
re-rendering only its records.

## 9. Preserving `SessionCache` (requirement 4)

Nothing here changes `SessionCache`: it stays agnostic about where content lives, via the
store type parameter. Admission is **two-phase and outside the cache mutex** — resolve,
lock, validate and construct the `SharedSession` *before* calling `shared_session`, because
its factory runs under the cache-wide `pull_residents` lock (`cache/mod.rs:167-173`) whose
header states that only an O(delta) advance may happen there (`cache/mod.rs:25-27`).

Eviction: size cap (default 2 GiB, `CLAUDE_REPLAY_CACHE_MAX`) + 30-day age cap, LRU by
mtime, never evicting a live-locked entry — and not calling the fork/exec liveness check per
candidate.

## 10. Testing

- **Equivalence:** cached vs cold, byte-identical, per frontend and flavor. The byte gate
  cannot see this (`gate.sh:32-33` drives only `--dump`/`--dump-html`, which never construct
  a `SessionCache`), so this is a new harness.
- **Re-invocation:** a second run of the *same* presentation parses **zero** transcript
  lines below the checkpoint (assert parse count). Also assert metadata restored from
  `session.state` equals a cold fold's — the check that pins requirement 5, since the same
  assertion must pass for both presentations against the same schema.
- **Rejection:** rewritten prefix, changed fold/format version, torn artifact, changed
  flavor or fold policy (records only) ⇒ full rebuild, never a partial serve.
- **Lock:** two writers; dead-pid reclaim; live-pid respected; live pid + dead port; TUI
  refusal text; HTML pick-time hand-off; mid-run child served uncached.
- **Fixture shapes (required):** a linear transcript passes while badly broken. Include a
  **pinned drain** (queued prompt / skill), a **mid-turn typed prompt**, and a **late tool
  result** — the three shapes that break a re-processing design.

## 11. Rollout

Additive: any validation failure falls back to today's behavior. Worst realistic bug is
"the cache didn't help". Release: minor.

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
