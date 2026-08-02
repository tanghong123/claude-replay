# Design: a durable, cross-run session cache

> **Status: v7 — no known blocker. Ready for review, then implementation.**
> Earlier drafts rejected the idea after review; that was wrong. Every objection is
> answered here (§3), and the two dead-end shapes are kept condensed in Appendix A only so
> they are not re-proposed.

## 1. Requirements (owner)

1. **Agent-neutral** implementation — lives in `claude-replay-present`.
2. **Supports both frontends**, TUI and HTML.
3. **The transcript is parsed exactly once**, ever — across invocations *and* across
   frontends.
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
- the open turn is not rebuilt by re-reading; it is *restored*, exactly as `hibernate`
  already restores `provisional: Vec<Block>` inline;
- in-flight state (`tool_slot`, `queue`, `suppress`, `last_skill`) is restored rather than
  re-derived, so the pinned-drain cases are just data.

Committed **content** is still written only for committed blocks (put-once, append-only) —
the open turn lives only in the small state file, rewritten per checkpoint.

## 4. Layout — and the BV boundary (requirement 5)

```
$CLAUDE_REPLAY_CACHE/<agent>-<session-id>/
    blocks              # committed Blocks, append-only, agent-neutral CONTENT  (shared)
    session.state       # agent-neutral fold state — CONTAINS NO BV             (shared)
    html/records        # HTML's rendered projection, derived from `blocks`
    html/state          # committed Vec<RecordLocator> + EmitState continuation
    tui/state           # committed Vec<Bv> (+ any TUI continuation)
    LOCK.<frontend>
```

**`session.state` contains no `BV` of any kind**, which is exactly requirement 5: metadata
is reconstructed from it alone, so it is identical no matter which frontend wrote it and no
matter what that frontend chose for `BV`. The `BV` table is per-frontend and lives beside
that frontend's own artifact.

**Requirement 3 (parse once) follows from `blocks` + `session.state` being shared.** The
transcript → `Block` conversion happens once, ever. HTML no longer re-parses to obtain
blocks: it reads them from `blocks` and renders its records from there. That also disposes
of the old worry that HTML's `BV` cannot yield a `Block` — it never needs to, because the
canonical blocks are in a shared file.

## 5. What `session.state` carries (the missing piece)

Everything the fold needs to continue, none of it `BV`-shaped:

| group | fields |
|---|---|
| already in `hibernate` | `SessionMeta`, `TaskList`, `user_times`, epoch/gen |
| **new: replayer working state** | the open turn `out` (inline `Block`s), `base`, `stamped`, `tool_slot`, `queue`, `suppress`, `last_skill`, `prev_ts`, `pending_ts`, `prev_user_text`, `delivered_rendered`, `agent_ids` |
| **new: accumulator state** | `cwd`, the metrics accumulator's opaque state (§6) |
| **new: resume point** | the reader position after line K + source identity (§8) |
| versioning | cache format version **and** a fold-logic version |

All are plain `String`/`usize`/`HashMap`/`Vec` of simple types; `QueueItem` is
`{content: String, marker_idx: Option<usize>, rendered: bool}` — serde-able as-is. Indices
stay coherent because `out` and `base` are restored together.

The fold-logic version answers the one real coupling concern: this is a private format, so
if the fold or the `Block` layout changes, bump the version and rebuild from the transcript.

## 6. The metrics seam — solved by an existing pattern

`MetricsAccumulator` is `push`/`finish` only (`adapter.rs:23-34`), and the collapsed
`Metrics` value cannot restore an accumulator: both agents hold private span endpoints, and
Codex's `model` comes from a `turn_context` line near the session start
(`codex/metrics.rs:35-39`), so without it a resumed run reports a wrong duration and no
cost.

**Do not widen `Metrics`.** Mirror the pattern the workspace already uses for stores —
`PersistentStore::hibernate_state`/`restore_state` (`shared.rs:565-572`), opaque
`serde_json::Value`:

```rust
fn checkpoint(&self) -> serde_json::Value { Value::Null }   // defaulted
fn restore(&mut self, _v: serde_json::Value) {}
```

Agent-neutral at the seam, agent-specific in each impl. One defaulted method pair, no
change to `Metrics`, no change to the neutral vocabulary.

## 7. Locking — one writer, never a blocked reader

One lock per artifact set. `blocks` + `session.state` are shared, so they get one writer;
each frontend's own directory gets its own.

- **Lock free / holder dead** (liveness-based reclaim, port-probed for a server holder via a
  frontend-injected callback so `present` grows no `std::net` dep): take it, read + write.
- **Held, TUI:** quit, naming pid, dir and `tmux attach` — refusing surfaces the silent
  duplicate fold-and-hold in RAM that happens today, and `tmux attach` is the real sharing
  primitive.
- **Held, HTML at pick time:** open the holder's `…?session=S`.
- **Held, HTML child discovered mid-run:** serve it **uncached** — correct output, no
  writes. The lock governs *writing*, never *viewing*; this is what fits the multi-root
  server, which discovers children lazily and cannot know its lock set at startup.

A non-holder may always **read** the shared artifacts (append-only + a durable
`valid_up_to`), so requirement 3 still holds when two frontends run at once.

`--no-cache` is a hidden flag (`hide = true`, precedent `jdi/mod.rs:164,167`): insurance
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

Note the `blocks` + `session.state` pair is **flavor- and policy-independent** — only the
rendered projection is parameterised. So a `--full` run still reuses the parse.

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
- **Cross-frontend:** a TUI run populates `blocks` + `session.state`; a subsequent HTML run
  parses **zero** transcript lines below the checkpoint (assert parse count).
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
