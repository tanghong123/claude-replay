# Design: a durable, cross-run session cache

> **Status: v6 — the blocker is resolved (§0). Still gated on the §4 measurement.**
>
> **Framing (owner): everything needed for a durable cache already exists — this task
> generalises it across invocations.** `SharedSession::hibernate`/`restore`
> (`shared.rs:592-666`) already persists committed values, per-turn times, metrics, meta,
> tasks and the store's render continuation, and validates before trusting them. It is
> merely scoped to one process today. The whole of #96 is four deltas:
> **(1)** write to a durable location instead of the per-run temp dir wiped at startup;
> **(2)** accept a **grown** source on restore, not only a byte-identical one
> (`shared.rs:639-641`); **(3)** let a restored session keep **advancing**
> (`shared.rs:248-250`); **(4)** add the lock so one process writes.
> **The only piece that was actually missing is the session metadata and the rest of the
> in-memory agent-neutral session state** — what `hibernate` does not carry, and therefore
> what a restored session needs in order to keep folding rather than being read-only. That
> is precisely what §0 and §5 specify; the review rounds' contribution was pinning *which*
> fields and, decisively, at *which boundary* they must be captured.
> Two successive designs for resuming the *fold* mid-transcript were reviewed and found
> **unsound** (Appendices A and B). What survives is narrower, and its value is unproven —
> which is why §4 comes before the design.

## 0. The correction that unblocks this (owner)

Two rules, and they must name the **same** boundary:

1. **Only durable state for committed blocks is written.** Nothing reflecting the open
   turn is persisted.
2. **On resume, re-read the transcript only from where the open block starts.**

v4/v5 put those at two *different* points — state captured at the commit drain (which had
already absorbed effects of later lines) but re-reading from an earlier line. Everything
the reviews found fatal lived in that gap. With one boundary there is no gap:

- **Nothing is re-read that the original already processed**, so the double-application
  class (Appendix B: duplicate `ToolResult`s, an extra `UserText`, double-stamped turns,
  double-counted metrics) cannot occur.
- **A committed tool call awaiting its result matches by construction**: the commit prunes
  it from the pending map (`replay.rs:499`), so the original orphans a late result too — a
  resumed run starting with an empty map behaves identically.
- **The queue matches by construction**: a commit only happens when the queue is empty
  (`replay.rs:439`), so restoring an empty queue is always right.
- **No `(Position, n)` ordinal is needed** — the resume point is a line boundary.

**The one condition:** the committed/open split must fall *between* lines. Normally it
does (a turn commits when the next user line arrives). One line can occasionally produce
both a committed block and the open turn (the #56 shape) — **skip checkpointing on such a
line**; the next clean boundary is moments away.

**Still to settle:** the metrics seam must carry more than `Metrics` — the accumulators
hold private span endpoints and Codex's `model` comes from a line near the session start
(`codex/metrics.rs:35-39`), so a resumed run needs both or it reports a wrong duration and
no cost.

**Note this is a small delta on existing code, not new machinery.**
`SharedSession::hibernate` already persists committed values, per-turn times, metrics,
meta, tasks and the store's render continuation (`shared.rs:592-628`). What is missing is
only: allow `restore` when the source has **grown** rather than requiring byte-identity
(`shared.rs:639-641`), let a restored session **continue advancing** (`shared.rs:248-250`),
and carry the handful of fold values above.

## 1. Problem

Every run rebuilds its artifacts from zero. `start_server` creates a bundle under
`temp_dir()` and `remove_dir_all`s it at startup (`serve.rs:622-630`); the TUI persists
nothing at all (`ArcStore`, RAM only). A session parsed, folded and rendered yesterday is
parsed, folded and rendered again today.

## 2. Goals / non-goals

**Goals.** Reuse expensive per-session work across runs; never serve a stale, torn or
mis-parameterised prefix; never wedge on a dead process's lock; keep the mechanism in
`present` so any frontend can adopt it.

**Non-goals.** Resuming the fold mid-transcript (Appendices A, B). Sharing artifacts
between two concurrently running frontends — each writes its own (§6). Multi-client TUI
(`tmux attach` is the answer; §8). Caching view state (wrapped heights depend on width and
fold state).

## 3. What already exists

| existing | gives us | gap |
|---|---|---|
| `TierBStore` (`tier_b.rs`) | append-only newline-framed `serde_json` log of `Block` with `{offset,size}` locators; `open` resumes `len` from `SeekFrom::End(0)` and appends (`tier_b.rs:91-107`) | not durable across runs; unused by the TUI |
| `PersistentStore::hibernate_state`/`restore_state` (`shared.rs:565-572`) | the per-store **render continuation** hook, already implemented by `RecordStore` (`record_store.rs:191-199`) | not wired to a cross-run cache |
| `engine::reader::Position` (`reader.rs:44-86`) | prefix hash + first-line identity anchor + `len >= offset` validation, with full-re-read fallback | `pub(crate)`; **no constructor** except `LineReader::tell()`/`decode()`; and the cold-build path never uses `LineReader` (`advance_reader` reads a bare `dyn BufRead` and computes its own offsets, `builder.rs:171-191`) — so the path that would *write* a cache has no anchor and no hash today |

## 4. STEP 0 — the gate. Measure before designing further.

Everything below is contingent on one unmeasured number: **how cold-open time splits
between read, fold and render.**

- If **render dominates** (plausible — folding is `serde_json` per line, rendering is
  markdown + syntect per block), §5 is worth building.
- If **fold dominates**, §5's ceiling is low, and the only way to beat it is fold-resume,
  which two reviews have now found unsound. Then the right answer for #96 is probably
  *don't build it*, revisiting only with Appendix B's prerequisites scoped as their own
  project.

**Deliverable:** an instrumented cold open on a large real session, split read/fold/render,
for both frontends. Nothing else in #96 starts first.

## 5. The design, if render dominates: cache renders, always fold

**The fold always runs from zero.** Every piece of fold state — `agent_ids`, `cwd`,
metrics, `user_times`, the task op-log, the queue, `tool_slot` — is then correct *by
construction*: no seeding, no frontier, no engine surgery. That is precisely why this shape
survives review while Appendices A and B did not.

What is skipped is the expensive part: re-rendering each committed block inside
`RecordStore::put` (`record_store.rs:122-142`).

```
put(block, at, times):
    if cache_valid && at < cached_record_count:  return cached_locator[at]
    else:                                        render, append, return new locator
```

The first divergence disables reuse for the rest of the run — a monotone switch, never
interleaved. On any doubt: render.

### 5.1 What the artifact must carry beyond the records

Review found three things a naive "just reuse the records file" misses:

1. **The render continuation.** `RecordStore` carries `EmitState { next_block, turn,
   seen_turns, turns }` across every `put` (`record_store.rs:52,141`) — block anchors
   `#bN`, turn numbers `#tN`, the `user_times` index, the accumulated sidebar. Resuming a
   log without it restarts at `b1`/`t1`: duplicate anchors, truncated sidebar, wrong turn
   timestamps. **Use the existing `hibernate_state`/`restore_state` hook** (§3); do not
   invent a second one.
2. **`open_append` is a new constructor, not `reopen`.** `RecordStore::reopen` sets
   `cx: None`, and its `put` then `expect("a reopened record store never puts")`
   (`record_store.rs:130-131,178-189`).
3. **The record *flavor* is part of the key.** `put` hardcodes `reveal=true, linked=true`
   (`record_store.rs:136-137`) — the *served* flavor. `--dump-html` renders
   `reveal=false, linked=false`; `--dump-all-html` adds an `AssetSink`. Three incompatible
   flavors; one `html/` directory would conflate them.

### 5.2 Validity key

| component | note |
|---|---|
| source identity: length ≥ cached, prefix hash, first-line anchor | reuse `reader::Position`'s scheme — but see §3: the cold path has no `LineReader`, so a write-side anchor must be added |
| record count + log byte length | artifact not torn |
| **record flavor** (served / dump / dump-all) | §5.1(3) |
| **`FoldPolicy`** | §5.3 — stays in the key |
| `cwd`, transcript path | session properties; cannot drift between runs of one session |
| cache format version, incl. a **fold-logic version** | if the fold or `Block` layout changes, rebuild |

Any mismatch ⇒ ignore the cache and render fresh.

### 5.3 `FoldPolicy` stays in the key (correcting v4)

v4 claimed the `open` flag could be hoisted out of records, making them
fold-policy-independent. **Half right.** Confirmed: `self.fold` reaches exactly one emitted
value in the whole HTML crate — `o.insert("open", !fold.collapses(b))` (`mod.rs:396`); no
suppression, no block-set change, no summary depends on it. So it is a flag, not content.

But the hoist is **not implementable** as stated: the record's `kind` is
`BlockKind::html()`, deliberately non-injective against `fold_key` (`model.rs:375-415`,
pinned at `model.rs:633`) — `ToolResult` and a generic MCP `ToolUse` both emit `kind:"tool"`
while their fold keys differ. Neither a serve-time layer nor the client can recompute
`open` from a record that dropped it. Doing so would mean adding `fold_key` to the wire
format (the JS reads `b.open` at `export.js:645,654,715`) and abandoning the zero-copy
pointer path, since `records_bytes` serves raw byte ranges (`serve.rs:393-410`).

**Decision: keep `FoldPolicy` in the validity key.** A different fold policy simply misses
the cache. Revisit only if flag-switching proves common.

## 6. Layout, and per-frontend duplication

```
$CLAUDE_REPLAY_CACHE (default $XDG_CACHE_HOME/claude-replay)/
  <frontend>/<flavor>/<agent>-<session-id>/
      records | blocks     # the frontend's content artifact
      state.json           # validity key + render continuation (hibernate_state)
      LOCK                 # owner pid (+ port for a server)
```

Each frontend writes its own artifacts; nothing is shared between concurrently running
frontends. The transcript is therefore parsed twice when one session is opened in both —
**accepted deliberately**, because the intended fix is not shared storage but a single
process that is both TUI app and HTML server (§8), at which point the duplication
disappears with no cross-process protocol ever having been built.

**Reset / torn tail.** `SessionAccumulator::reset` (`builder.rs:211-221`) fires on any
source truncation or rewrite and calls `store.reset()`. Under a durable cache a reset must
discard the content log **and** `state.json` together; on load, a torn or inconsistent pair
is a **full rebuild**, never a partial trust.

## 7. Locking

One lock per `<frontend, flavor, session>` — the artifact directory. Reclaim is
liveness-based (dead pid ⇒ take it; a server holder is additionally port-probed, since pids
are recycled, via a callback injected by the frontend so `present` grows no `std::net`
dependency).

**The lock governs *writing the cache*, not *serving*.** This correction is what makes it
fit the multi-root HTML server, which serves N sessions from one run with one shell and one
port and discovers children *lazily* mid-run (`serve.rs:598-676, 632-635`) — so the lock
set is not knowable at startup and "refuse the run" is not available.

| situation | behavior |
|---|---|
| lock free, or holder dead | take it; read + write the cache |
| **TUI**, held by a live holder | **quit**, naming pid, dir, and `tmux attach` |
| **HTML**, held at pick time | open the holder's `…?session=S`; serve nothing here |
| **HTML**, child discovered mid-run, lock held | serve it **uncached** — correct output, no cache writes; the page is already open, so no mid-run hand-off exists |

`--no-cache` is a **hidden** flag (`#[arg(long, hide = true)]`; precedent at
`jdi/mod.rs:164,167`) — operational insurance for the cache path itself, not a way to force
a second TUI. Refusing a second TUI is an *improvement*: today two instances each fold and
hold the whole session in RAM, silently, and `tmux attach` is the real sharing primitive.

**Portability is a correctness gate.** `pid_alive` shells out to `kill -0` and returns
`false` on non-unix (`jdi/state.rs:150-167`). Where there is no real liveness check the
cache is **disabled**, never "assume stale" — which would fail *into* concurrent writers.
That shell-out costs a fork/exec per call, so eviction must not consult it per candidate.

## 8. Future direction

Real multi-client TUI means splitting the TUI into backend + frontend, as HTML already is —
one backend per session, N clients. The pull protocol exists (`pull.rs`), but it carries
only `epoch`/indices/`Vec<Block>`; `user_times`, `metrics`, `meta` and `tasks` ride
`PullDelta`/`ViewDelta` and the server's `assemble_meta` (`shared.rs:47-90`,
`serve.rs:355-388`). A decoupled TUI client needs all four, so the neutral protocol is less
complete than v4 implied.

## 9. Eviction

Size cap (default 2 GiB, `CLAUDE_REPLAY_CACHE_MAX`) plus a 30-day age cap; evict whole
artifact directories, LRU by mtime; never evict a live-locked entry (subject to §7's cost
note). Per-frontend duplication roughly doubles a dual-opened session's footprint.

## 10. Testing

- **Equivalence:** a cached run and a cold run produce byte-identical output, per frontend
  and per flavor. The existing byte gate **cannot** see this — `gate.sh:32-33` drives only
  `--dump`/`--dump-html`, which never construct a `SessionCache`. New harness required.
- **Rejection:** rewritten prefix, changed fold policy, changed flavor, torn log, changed
  fold version ⇒ full rebuild, never a partial serve.
- **Lock:** two writers; dead-pid reclaim; live-pid respected; live pid + dead port; the TUI
  refusal names pid/dir/`tmux attach`; HTML pick-time hand-off; mid-run child served
  uncached.
- **Fixture shapes (required).** A linear transcript passes while badly broken. The set must
  include a **pinned drain** (queued prompt / `last_skill`), a **mid-turn typed prompt**,
  and a **late tool result**.

## 11. Rollout

Additive: any validation failure falls back to today's behavior, so the worst realistic bug
is "the cache didn't help". The one genuinely new failure mode is refusal-to-open on TUI
lock contention (§7), mitigated by the hidden `--no-cache`. Release: minor.

---

## Appendix A — rejected: resume the fold from a bare byte offset

**A1.** The commit cut is not a byte offset: `finalize_completed` runs once per *line* after
all that line's messages (`replay.rs:428`), and one line can carry several turns. Probe:
`committed_len` goes 0 → 2 on a single line. **A2.** The drain fires on a much later line
than the open window starts at — gated on `queue.is_empty()` (`replay.rs:439`), capped by
the `last_skill` pin (`replay.rs:455-462`). **A3.** `agent_ids` is populated at spawn-emit
and never pruned (`replay.rs:496-502`); re-folding from any non-zero offset emits
`AgentDone { agent_type: "" }`, directly rendered (`mod.rs:443-449`). Its session-long reach
means "move the frontier back" has no finite answer.

## Appendix B — rejected: composite frontier + meta message stream

Two independent reviews, both **UNSOUND**, one root cause: **the checkpoint is captured at
the drain (line D) but the re-fold restarts at the frontier line L ≤ D**, and every prune
`finalize_completed` performs was live throughout `(L, D]`. The criterion "does
`finalize_completed` prune it?" answers about the drain instant; resume needs the answer at
the start of line L. `L < D` is the normal pinned-drain case — already documented in A2, so
the rule contradicted a finding in the same document.

Consequences, each independently fatal:

- **`tool_slot` misclassified** — results arriving in `(L, D]` join a pre-frontier
  `tool_use`; a re-fold with an empty map sends them to the orphan arm
  (`replay.rs:257-259`) ⇒ spurious `ToolResult` blocks past the frontier.
- **`queue` misclassified** — non-empty at L (that is *why* D > L); a re-fold with an empty
  queue takes a different FIFO pop and emits an extra `UserText` — the double-render #88
  exists to prevent.
- **`user_times` is one turn ahead of the frontier at every drain** (probe: at the drain of
  turn 1, `user_times.len() == 2`), because `finalize_completed` stamps the whole window
  *before* draining (`replay.rs:473-475`). Restoring it double-stamps and shifts every later
  turn — and HTML indexes into it (`mod.rs:757-758`).
- **`stamped` restored absolutely panics** — a raw index in a rebased space;
  `&out[*stamped..]` goes out of range (`replay.rs:649`).
- **Metrics are a mixed epoch** — `metrics.push` runs *after* the drain
  (`builder.rs:150-162`), so a drain-time record covers `[0, D)` and over-counts `[L, D)`;
  Claude sums. Independently `seed(&Metrics)` is lossy both ways: `TimeSpan{min,max}` is
  private and unreconstructible from `duration_secs`, and Codex *cannot* ignore seeding,
  because `model` comes from a `turn_context` line below the frontier ⇒ `cost_usd: None`.
- **`SessionMeta.children` is not append-only** — `AgentDone` mutates earlier entries
  (`session.rs:297-303`), so a suffix delta cannot express it.
- **`(Position, n)` is not constructible** — nothing maps a block index to its producing line
  (`advance_at`'s offset reaches only attachment locators, `builder.rs:119-131`), and `n`
  cannot be counted from the end because `coalesce_spans` treats `Attachment` as
  span-transparent (`model.rs:527-532`).
- **The three-way oracle cannot backstop it** — `committed_meta` is only
  `{turns, tools, children}` (`session.rs:258-266`); metrics, `user_times`, the task fold and
  every carried scalar sit outside what folding blocks can observe.

**If ever revisited**, the prerequisites are: pre-line-L snapshots instead of drain-site
capture; `tool_slot` below-frontier entries as expiring sentinels; queue markers encoded as
*resolved* rather than raw indices; `user_times` truncation with `stamped = 0` and a rebased
`base`; a metrics seam carrying span endpoints and `model`; and block→line attribution.
That is its own project, not a step of #96.
