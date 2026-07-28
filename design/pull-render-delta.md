# `/pull` — maintained meta + delta-sized reply (no per-poll clone, no scan)

**Status:** planned → implementing. Increment of Phase C (see `streaming-core-memory.md` §9a).
**Baseline commit:** `50fb1a7`.
**Gates (every increment):** `cargo fmt --check`, `cargo clippy --all-targets` (no new
warnings), `cargo test`, `/tmp/sc-gate/gate.sh` → `BYTE-IDENTICAL: PASS`, **plus** a new
oracle test — the gate does **not** drive `/pull` (§6), so gate-PASS alone is not evidence
the `/pull` meta is unchanged.

---

## 1. Problem (measured)

Each `/pull` poll pays **three** O(session) costs, for a reply whose payload is only the
delta since the client's cursor:

1. `SharedSession::render_snapshot()` clones **every** block — `committed.clone()` ++
   `provisional` (`src/cache/shared.rs:129‑154`). **22 MB** for the biggest real session
   (12 664 blocks × 216 B spine + content); ~112 MB/s at 5 Hz × clients.
2. The `Session` behind it was built by `snapshot()` **rebuilding** `index` + `sub_agents`
   from a full block scan every advance (`builder.rs:191` `SessionIndex::build`, `:194`
   `build_sub_agents`) — even though `/pull` never reads them.
3. `pull_response` then re-derives the meta with `agent_meta` scanning **all** blocks
   again (`serve.rs:225` → `count_turns`/`count_tools`/`collect_child_refs`).

All three are waste: the committed records are already on disk in `<id>.records` (#34) and
the reply is O(delta). The fix: **maintain** the meta as the tail advances, and return only
the delta.

---

## 2. Design (locked with the user)

**One principle:** the `SessionAccumulator` *maintains* the meta as it folds — the committed
prefix is folded **once, as it commits**, and never rescanned; the open turn (which genuinely
changes each poll, and which we already re-render) is folded on top. A `/pull` **reads** the
maintained meta and returns delta-sized blocks. No whole-session clone, no whole-session scan.

The meta **must match the full tail** (`committed ++ provisional`) — a sub-agent spawned in
the live turn appears immediately, exactly as today (`agent_meta` sees `committed ++
provisional` now). So the maintained meta = *maintained committed* + *open-turn fold*.

### 2.1 The neutral meta (core — `claude-replay-core`)

Agent-neutral, presentation-free — the engine surfaces facts; the frontends shape the wire.

```rust
/// Session-derived facts a live header needs, maintained by the accumulator as the tail
/// advances. Counts match count_turns / count_tools over the display stream; `children`
/// is in spawn (block) order.
#[derive(Clone, Default, Debug)]
pub struct SessionMeta {
    pub turns: usize,               // == #(UserText | Command)
    pub tools: usize,               // == #ToolUse + Σ Thinking.tools.len()  (non-spawn tool calls)
    pub children: Vec<ChildMeta>,
}
#[derive(Clone, Debug)]
pub struct ChildMeta {
    pub id: String,
    pub description: String,
    pub agent_type: String,
    pub running: bool,              // !spawn_status.is_terminal(), cleared on AgentDone
}
```

### 2.2 Maintenance rule (per fold-event, in the accumulator)

The accumulator holds a **committed** `SessionMeta` and folds each committed block into it
**once**, on drain (`advance_at`, `builder.rs:103` loop) — reusing the block-level classification
so it is correct by construction (no message-level guesswork):

- `UserText | Command` → `turns += 1`
- `ToolUse` → `tools += 1`
- `Thinking { tools }` → `tools += tools.len()` (activity-grouped calls — the count is
  invariant under coalescing, so counting the finalized block is exact)
- `SubAgent { agent_id ≠ "" }` → push `ChildMeta { running: !status.is_terminal(), … }`
- `AgentDone { agent_id }` → set that child's `running = false`

A poll produces the **full** meta by folding the open-turn blocks (from the replayer's
`open_snapshot`) into a **clone** of the committed meta — `O(open turn)`, the same window we
already re-render; the committed prefix is never touched. On **reset** (truncation) the
committed meta clears and re-accumulates.

**Why block-level, not message-level:** the load-bearing subtlety is that a **spawn's
`tool_use` is a child, not a tool** (it becomes `SubAgent`, which `count_tools` scores 0),
and a `tool_result`-bearing user message is **not** a turn. Counting finalized *blocks* gets
both right for free; a message-level counter would have to re-derive that classification and
risk drift. The committed side is finalized ⇒ exact; the open side is re-folded from the
actual open blocks ⇒ exact.

### 2.3 Light accumulator accessors (core) — never materialize all committed

**Critical:** a snapshot struct that owns the whole `committed: Vec<Block>` would just be
`render_snapshot`'s clone renamed — and `fold()` already clones all committed
(`builder.rs:172` `committed.iter().map(get).collect()`) *before* `render_snapshot` clones it
again. Both O(N) copies must go. So the committed prefix **stays owned by the accumulator**;
the streaming path copies only the tail it needs. The full `snapshot()`/`fold()`/`poll_shared`
(index + sub_agents + whole-committed clone) stays **untouched** for the batch/TUI/`--dump`/
bundle/`/stream` consumers — those gated paths cannot move.

Add granular, non-cloning accessors on `SessionAccumulator` (the streaming path reads these
under the lock; no full-committed copy):

```rust
pub fn committed_tail(&self, from: usize) -> Vec<Block>;          // committed[from..] only — O(delta)
pub fn open_finalized(&self) -> (Vec<Block>, Vec<Option<EpochSeconds>>); // provisional + whole user_times — O(turn)
pub fn metrics(&self) -> Metrics;
pub fn session_meta(&self) -> SessionMeta;                        // committed meta clone + open-turn fold — O(turn)
pub fn provisional_len(&self) -> usize;                           // finalized open len (for counters)
// committed_len() already exists.
```

`FollowParser` adds `advance_stream()` (fold the delta; report `(reset, patch_floor)`, `None`
if idle — no `snapshot()`) and forwards the accessors. `poll_shared`/`poll_session` (the cache
path) are unchanged.

### 2.4 `SharedSession::pull_delta` (cache — kills the clone)

Replace `render_snapshot` (delete it + `RenderSnapshot`). `SharedSession` **stops storing a
full `Session`**; it keeps the `FollowParser` (which owns the accumulator + its committed
prefix) plus the protocol counters and cached `n_committed`/`n_provisional`. `advance()` calls
`follower.advance_stream()` and updates `epoch`/`provisional_gen` from the committed-length
delta + `patch_floor` (same rule as today). `pull_delta` reads the accessors, copying only the
committed tail:

```rust
pub struct PullDelta {
    pub epoch: u64, pub provisional_gen: u64, pub n_committed: usize, pub reset: bool,
    pub committed_delta: Vec<Block>,            // committed[from..]  (from = rendered, or 0 on reset)
    pub provisional: Vec<Block>,                // O(turn)
    pub user_times: Vec<Option<EpochSeconds>>,  // WHOLE session (small)
    pub metrics: Metrics,
    pub meta: SessionMeta,                       // maintained — read, not scanned
}
/// `prev_epoch`/`rendered_committed` = the caller's `PullRender.epoch`/`offsets.len()`.
/// One lock. `committed_delta` is `committed[from..]` — O(delta), never O(session).
pub fn pull_delta(&self, prev_epoch: u64, rendered_committed: usize) -> PullDelta;
```

`from` = `0` on reset (epoch moved ⇒ caller discards `<id>.records`, all re-renders), else
`min(rendered_committed, n_committed)`. `counters()` / `pull_indices` idle fast-path unchanged.

### 2.5 `pull_response` (html — reads the meta, no scan)

Only the non-idle tail changes (`serve.rs:220‑303`):

```rust
let mut rmap = self.render.lock().unwrap();
let pr = rmap.entry(id.to_string()).or_default();
let d = shared.pull_delta(pr.epoch, pr.offsets.len());     // delta-sized; render lock ⊃ shared lock
if d.reset { *pr = PullRender { epoch: d.epoch, ..Default::default() }; let _ = remove_file(&log_path); }
if !d.committed_delta.is_empty() {                          // render-once → <id>.records
    let new = render_blocks(&d.committed_delta, &d.user_times, …, &mut pr.emit);
    append_records(&log_path, &new, &mut pr.offsets, &mut pr.len);
}
let mut open_emit = pr.emit.clone();
let provisional_lines = render_blocks(&d.provisional, &d.user_times, …, &mut open_emit);
let (cf, pf) = pull_indices(d.epoch, pr.offsets.len(), provisional_lines.len(), d.provisional_gen, cursor);
let committed_bytes = read_range(&log_path, pr.offsets.get(cf).copied().unwrap_or(pr.len), pr.len);
drop(rmap);
let meta = assemble_meta(self.agent, &self.cwd, &info, &d.meta, &d.metrics);   // trivial wire transform
// … splice via pull_reply_json exactly as today …
```

`assemble_meta` (new, `html_export/mod.rs`) maps `SessionMeta` + `info` (title/ancestry) +
`metrics` → the same JSON `agent_meta` emits. `agent_meta` stays as the **oracle** and the
`/stream`/bundle assembler (untouched).

### 2.6 Child navigation — inverted, lazy, one-time (html)

Drop the per-pull `register_children` write. A parent's pull touches **only its own state**;
its maintained meta already lists its children (id + description). Instead:

- **child_source** — `discover::subagent_source(agent, root, id)` is a pure path derivation
  (no child-session knowledge); resolve once on first need, cache in the registry.
- **parent pointer** — when a child source is first put in the cache, record its **parent
  session id** (one id).
- **lazy title** — the **first** time the child itself is pulled, follow the parent-id →
  read the parent's maintained meta → take its `description` from `parent.meta.children[id]`
  and its ancestry as `parent.ancestors + parent` → cache as its `TitleInfo`. One cross-session
  read, self-initiated, once. Later pulls read the cache.
- **edge:** a child deep-linked before its parent was ever loaded shows its bare id (identical
  to today's un-registered-deep-link fallback, `serve.rs:119`).

---

## 3. Byte-identity (the meta must equal today's `agent_meta`)

| field | today (`agent_meta` over blocks) | maintained | equal? |
|---|---|---|---|
| `turns` | `count_turns` = #(UserText\|Command) | `+1` per such committed/open block | ✅ same predicate |
| `tools` | `count_tools` = #ToolUse + Σ Thinking.tools | `+1` / `+len` per such block | ✅ same predicate |
| `children[]` order | `collect_child_refs` block walk | push in drain/fold (block) order | ✅ same order |
| `children[].running` | `!terminal && !done` | `!spawn_status.is_terminal()`, cleared on `AgentDone` | ✅ see §2.2 / §3.1 |
| `title`/`agent`/`sid`/`cwd`/`agent_type`/`ancestors`/`usage` | `info`/`m` | `info`/`m` | ✅ untouched |

### 3.1 `running` equivalence
Today: `running = !c.terminal && !done.contains(id)`, `c.terminal =
build_sub_agents(blocks)[id].status.is_terminal()`, `done = ∃ AgentDone(id)`. `build_sub_agents`
supersedes the spawn status with the `AgentDone` status. Cases:
- spawn only → `running = !spawn_status.is_terminal()` ✅ (our push value)
- spawn + `AgentDone` (any status) → today `done ⇒ running=false`; ours clears on `AgentDone` ✅

### 3.2 Duplicate `agent_id` (essentially unreachable)
Today's block walk emits a duplicate spawn id twice. Our push-per-drain also emits per spawn
block ⇒ **duplicates preserved** — no divergence. (`AgentDone` clears `running` on all matching
ids; the dup case is unreachable in practice.)

---

## 4. Cursor reconciliation — already implemented, unchanged

The client is built and correct (`consumePull`, `src/html/export.js:562‑582`); the wire
(`pull_reply_json`) and the 4-number cursor `{epoch, committed, gen, index}` are **locked**
(`src/cache/stream.rs`). The client applies, per zone, "truncate to `from`, then extend":
committed grows append-only (`committed_from ≤ pc.committed`); provisional sends
`provisional_from = pc.index` on a same-gen append, `0` on a gen bump / commit / resync; the
new cursor is the first-returned position. **Invariant we must uphold:** every **non-idle**
reply carries a non-null `meta` (the client calls `renderMeta` past the idle early-return);
the idle fast-path returns empty zones + `meta:null` (client early-returns first) — both safe.

---

## 5. Traps checklist

- [ ] `user_times` returned **whole-session** (not sliced) — `EmitState.seen_turns` indexes it.
- [ ] `(cf, pf)` re-derived **after** rendering from `pr.offsets.len()` + `provisional_lines.len()`
      + the returned epoch/gen (not the earlier `counters()`).
- [ ] Lock order: render lock (`rmap`) ⊃ `pull_delta`'s shared lock; nothing takes shared-then-render.
- [ ] Name collision: keep `super::render_snapshot` (mod.rs:924, `follow_and_append`) + its import;
      delete only `SharedSession::render_snapshot` + `RenderSnapshot`.
- [ ] Reset chicken-and-egg: pass `(pr.epoch, pr.offsets.len())`; method returns `reset` + full
      committed range (`from = 0`).
- [ ] The meta reflects `committed ++ provisional` (open-turn fold), never committed-only.
- [ ] Full `snapshot()` (index/sub_agents) untouched ⇒ `--dump`/bundle/`/stream` stay byte-identical.

---

## 6. Verification

- **Oracle test (new).** The gate never drives `/pull` (`gate.sh` diffs only `--dump -`,
  `--dump-html -`, and `*.bundle`). So: for a fixture block list with user turns, an
  **activity-grouped** `Thinking{tools}` (exercises the nested `tools` count), and two
  `SubAgent` spawns (one completed via `AgentDone`, one running), assert
  `assemble_meta(agent, cwd, info, &session_meta(committed, provisional), m) ==
  agent_meta(agent, cwd, info, &blocks, m).0` as JSON. Proves §3.
- **Accumulator test.** Fold the fixture; assert the maintained `SessionMeta` equals
  `count_turns`/`count_tools`/`collect_child_refs` over `snapshot().blocks()` at each step
  (including mid-open-turn and across a commit).
- **Gate** `BYTE-IDENTICAL: PASS` — proves `--dump`/bundle/`/stream`/full-`snapshot` unmoved.
- **Manual e2e.** `claude-replay <big session> --html -f`, attach a browser: live feed
  identical to the pre-change build; per-poll memory no longer spikes.

---

## 7. Increments (each gated)

1. **Core — maintained `SessionMeta`.** Types + accumulator maintenance (drain fold + open
   fold) + `stream_snapshot` + accumulator test. Full `snapshot()` untouched.
2. **Cache — `pull_delta`.** `SharedSession` stores the light snapshot; `pull_delta` returns
   delta blocks + meta + counters; delete `render_snapshot`/`RenderSnapshot`.
3. **Html — `assemble_meta`.** `pull_response` reads `d.meta`; add `assemble_meta` + the
   oracle test. `agent_meta` stays as oracle + `/stream`/bundle.
4. **Html — child-nav inversion.** Parent-id pointer + lazy one-time child title; drop
   per-pull `register_children`.

## 8. Files

- `claude-replay-core/src/model.rs` (or `engine/session.rs`) — `SessionMeta` / `ChildMeta`.
- `claude-replay-core/src/engine/builder.rs` — maintain committed meta on drain; `stream_snapshot`.
- `claude-replay-core/src/follow.rs` — `poll_shared` returns the light snapshot.
- `src/cache/shared.rs` — store light pieces; `pull_delta`; delete `render_snapshot`/`RenderSnapshot`.
- `src/cache/mod.rs` — re-exports.
- `src/html_export/serve.rs` — `pull_response` via `pull_delta` + `assemble_meta`; child-nav inversion.
- `src/html_export/mod.rs` — `assemble_meta` + oracle test; `agent_meta` kept.
