# Engine refactor — end-to-end execution plan

The step-by-step plan to finish the three-layer engine refactor (`parser-engine.md`), such
that **production runs entirely on the new engine and the old parsers are deleted**, with
every step independently mergeable to `main`, consumed by the current code, and gated by
tests. Milestones continue the numbering already merged (M1–M7).

## Status (M1–M15)

The **core refactor is complete and merged**: production runs entirely on the three-layer
engine, one streaming pass per session (blocks + folded metrics + index), one line resident.
What's done vs. deferred:

- ✅ **M1–M9** — L1/L2 split; `Session`/`SessionIndex`; Codex on the shared L2; TUI + HTML
  sourced from `Session`; **production migrated onto the streaming engine**, with
  `parse_main`/`parse_lines` **frozen as `#[cfg(test)]` golden references** (the safer chosen
  resolution — kept to pin the engine bit-identical rather than deleted).
- ✅ **M10** — metrics folded into the streaming pass (one file read, not two).
- ✅ **M11 (primitives)** — `Replayer::snapshot` + `extend_tool_ids`, proven byte-identical
  line-by-line == full replay. Wiring the live consumers through them (the CPU win) is the
  deferred **M16**.
- ✅ **M13** — one `BlockKind` classification, coarse (`fold_key`) + fine (`html`) projections.
- ✅ **M14** — message block-model lift: `Message::ToolUse` carries raw `{id,name,input,cwd}`
  and L2 shapes the block via `Shaping::build_tool`; `tokenize` no longer builds blocks and
  `engine::message` no longer references `Block`. Byte-identical (equivalence oracles + a
  real-transcript pre/post render diff over 11 SubAgent spawns).
- ✅ **M16** — live consumers routed through the incremental `Replayer` via `FollowParser`
  (persistent `Replayer<'static>` + byte cursor + reset-on-shrink): the `--dump-html -f`
  follower, the TUI live tail (`view::update`), and the served `--html -f` `Live` tailer all
  fold only the delta. Byte-identical (follow==full-reparse both agents), tmux- + curl-verified.
- ✅ **M12** (SessionStore/tiers) — **done, scoped**. M16's `FollowParser` already delivered the
  live-CPU win; this milestone extracted the server's residency bookkeeping into a generic,
  transport-agnostic `engine::store::SessionStore<Info, Res>` (tier (c) path-only registry +
  tier (a) resident-follower set + lazy `see`/`admit`, TTL `reap`, `resident_ids`,
  `with_resident`). `html_export::Live` now layers HTML rendering, the materialized `<id>.jsonl`
  (tier (b)), and the `/stream` byte cursor over it. Unit-tested in isolation (the tier
  lifecycle — path-only → resident → grow → idle-reaped → re-resident) and curl-verified served
  (append → reset + back-patch + new turn). **Deliberately NOT unified with the TUI `Frame`
  descend stack**: that's a LIFO navigation stack of live `View`s with no byte cursor, keyed
  residency, or TTL — already optimal (each ancestor kept alive, never re-parsed); folding one
  real consumer into a shared abstraction would be over-engineering. No user-visible change.

## Original definition of done (for reference)

1. `parse_main` (Claude) and `parse_lines` (Codex) retired from production — **achieved**
   (frozen as test-only golden references rather than deleted).
2. A session is produced by **one streaming pass** (blocks + metrics + index), one line
   resident — the RSS invariant holds. **Achieved** (M9 + M10).
3. The **live** paths fold **only the delta** (incremental `ingest` + `reset`). **Mechanism
   achieved + proven** (M11 primitives); consumer routing is M16 (deferred).
4. The library surface is real: `Session`/`Parser`/`Replayer`/`SessionIndex` with no
   TUI/HTML/clap leakage. **Achieved** (`Session`/`Replayer`/`SessionIndex` shipped; the
   crate split is optional and deferred).
5. Zero user-facing change throughout — **held** (every merged milestone byte-identical).

## Baseline (after M1–M7)

- L1 `tokenize` + L2 `replay` exist and are **proven bit-identical** for both agents (via
  `replay_tokenize_matches_parse_main`, `codex_replay_matches_parse_lines`) — but are used
  **only by the in-memory batch entries** (`model::parse`, `codex::parse_codex`).
- **Production file parsing still runs on the OLD `parse_main`/`parse_lines`**:
  `parse_session_as → parse_path_timed_for → parse_main`/`parse_lines`. The two engines
  coexist; the duplication is not yet removed.
- `Session` + `SessionIndex` (turns/agents/tools/attachments + block-kind histogram) exist;
  TUI `build_frame` and HTML `snapshot` source from `parse_session_as`.
- Metrics are still a **separate file read** (`parse_reader_for`) inside `parse_session_as`.
- The `Message` vocabulary still carries a built `Block` (the documented waypoint).
- Not started: `SessionStore`/tiers, classification-unify (`BlockKind`), incremental, crate
  split, lazy sub-agents.

## Guiding rules (every milestone)

- **Mergeable:** compiles + `cargo fmt`/`clippy`(0)/`test` green on its own; merges to `main`
  with a `Milestone N:` marker.
- **Consumed:** wired into a real caller (not a dangling type) by the end of the milestone,
  or explicitly additive with a test that exercises it.
- **Tested:** the acceptance gate below, plus a bit-identical proof where output could move.
- **Byte-identical:** the standing prime directive. The proofs are the golden parse tests,
  the equivalence tests, the dump sweep (over the sibling `claude-replay-eval` corpus), the
  HTML stream tests, and the RSS probe.

## The critical path (why this order)

The blocker to deleting the old parsers is **memory shape**. Today `tokenize` returns
`Vec<Message>` for the whole file, then `replay` builds `Vec<Block>` — both resident at once,
~2× the peak of `parse_main` (which streams straight to `Vec<Block>`). So we cannot just
swap production onto `replay(tokenize(file))` without regressing RSS on a 298 MB session.

The fix is to **drive L2 incrementally, per line**: a stateful `Replayer` fed one line's
messages at a time, so no whole-file `Vec<Message>` ever exists — peak = one line + the
`Vec<Block>`, matching `parse_main`. That same stateful `Replayer` is exactly what the live
incremental path needs. So **M8 (the `Replayer`) is the keystone that unlocks both the
streaming migration (M9) and incremental (M11).**

```
M8 Replayer ──▶ M9 streaming driver + delete old parsers ──▶ M10 metrics fold-in
      │                        │
      └────────────────────────┴──▶ M11 incremental ingest + reset ──▶ M12 SessionStore
M13 classification-unify · M14 message block-model lift · M15 crate split  (slot in anywhere)
```

---

## M8 — the stateful `Replayer` (keystone, additive)

**Goal.** Turn the `replay` fold-loop into a struct that can be fed messages incrementally.

**Changes.** `model::Replayer { blocks, tool_slot, pending, queue, trigger_ts, …, shaping,
tool_ids }` with:
- `new(shaping, tool_ids) -> Self`
- `apply(&mut self, &[Message])` — the current loop body, folding a batch into the state.
- `snapshot(&self) -> Vec<Block>` / `into_blocks(self) -> Vec<Block>` — run `finish`
  (grouping) over the accumulated blocks (owned; `into_` moves instead of clones).
- `replay(messages, user_times, shaping)` becomes `Replayer::new(shaping, ids); apply(all);
  into_blocks()` — a thin wrapper, so the existing bit-identical tests keep proving it.

**Consumed by.** Nothing changes in production yet (the batch `replay` wrapper is unchanged
externally). Additive.

**Test / gate.** Existing `replay_tokenize_matches_parse_main` + `codex_replay_matches_parse_lines`
(unchanged output). **New:** a split-apply test — `apply(all)` vs `apply(a); apply(b)` for
every split point produces identical blocks (proves incremental folding is sound for the
append case).

**Risk.** LOW — pure structural refactor guarded by the equivalence tests.

**Done when.** Merged; both equivalence tests + the split-apply test green.

---

## M9 — streaming engine driver + delete `parse_main`/`parse_lines` (the migration)

**Goal.** Make the new engine the **production streaming parser**; retire the old ones.

**Changes.**
- Give L1 a per-line entry: `Adapter::decode(line) -> smallvec/Vec<Message>` (Claude + Codex
  tokenizers refactored from "iterator of lines" to "one line → messages"; the existing
  whole-iterator `tokenize` becomes `lines.flat_map(decode)`).
- A streaming driver `engine::parse_stream(agent, open) -> (Vec<Block>, user_times)`:
  **pass 1** streams the file collecting `tool_use` ids (today's `scan_tool_ids`/
  `scan_call_ids`); **pass 2** streams the file, `decode`-ing each line and
  `Replayer::apply`-ing its messages — one line resident, no whole-file `Vec<Message>`.
- Repoint `model::parse_path_timed_for` (both agents) onto `parse_stream`.
- **Delete** `parse_main`, `parse_lines`, and their duplicated slot/pending loops.

**Consumed by.** Everything — `parse_session_as`, `build_frame`, HTML `snapshot`, `--dump`
all already funnel through `parse_path_timed_for`, so this flips the whole app onto the new
engine at once.

**Test / gate.** Golden parse tests; the **dump sweep** (byte-identical over the corpus);
the **RSS probe** on a large session (must not regress vs. the recorded `parse_main` peak);
tmux e2e + HTML stream tests. Keep the deleted parsers' behavior pinned by re-running the
old golden fixtures against the new path before deleting.

**Risk.** HIGH — this is the load-bearing swap; memory + byte-identical must both hold. The
mitigation is that M8 already proved the fold is correct; M9 only changes *how messages are
delivered* to it (streamed vs. batched), which the split-apply test also covers.

**Done when.** Old parsers gone; dump sweep + RSS probe + tmux e2e green.

---

## M10 — fold metrics into the parse pass (Phase 4)

**Goal.** Kill the separate metrics file read.

**Changes.** Accumulate the token/cost tally during M9's pass 2 (a `MetricsAcc` updated per
line from its `usage`, finalized to `Metrics`). `parse_session` returns metrics from the
fold; delete the `parse_reader_for` open in `parse_session_as` (and the 2nd/3rd opens in the
old `app.rs`/`html_export` paths already removed by M6/M3). Net reads per session: **2 → 1**
(id pre-scan can also fold metrics, or stay id-only).

**Consumed by.** `parse_session` (one fewer read); the footer + HTML meta unchanged.

**Test / gate.** Metrics footer tests + HTML usage assertions (byte-identical `Metrics`); RSS
re-measured.

**Risk.** MEDIUM — must reproduce `Metrics` exactly from the streamed pass.

**Done when.** Merged; metrics tests green; the extra read is gone.

---

## M11 — incremental L1 + L2 (`ingest` + `reset`) — the live-CPU win (Phase 6+7)

**Goal.** Live paths fold only the delta, not the whole file.

**Changes.**
- L1 `Parser` (stateful, §3.2): `advance(reader)` resumes from a byte cursor, keeps a
  small tail digest, emits `Event::Reset { from }` when the kept tail no longer matches
  (edited/compacted transcript).
- `Replayer::apply` handles `Reset` (truncate blocks/index back to the reset point, re-fold
  what follows) — the only rewind.
- Route the **TUI live tail** (`tail.rs` `TailReader` + `view::ingest`) and the **HTML
  follower** (`follow_and_append`, and the `Live` server's tailer/`/stream` cursor) through
  `Parser::advance` + `Replayer::apply`, replacing the whole-file re-parse and retiring the
  §8.3 skip-if-unchanged stop-gap.

**Consumed by.** `-f` (TUI + HTML), `--html -f`.

**Test / gate.** tmux **live-tail e2e** (unchanged behavior); a **CPU probe** on a large live
session (must drop vs. today); **byte-identical stream** vs. a full re-parse at each step
(append case *and* a rewritten-tail `reset` case).

**Risk.** HIGH — the delicate one (cursor discipline, `reset` correctness, the orphan-vs-
pending decision under streaming). Needs the live e2e, not just unit tests.

**Done when.** Merged; live e2e + CPU probe + stream-equivalence green.

---

## M12 — `SessionStore` + residency tiers (Phase 8) — ✅ DONE (scoped)

**Goal.** A named, transport-agnostic store behind the multi-agent live server's residency.

**Landed.** Extracted `engine::store::SessionStore<Info, Res>` — generic, HTTP/HTML-agnostic:
tier (c) `registry` (id → `Info` descriptor) + tier (a) `residents` (id → open follower +
diff baseline, with a store-owned `last_seen`). Surface: `register`/`register_new`/`resolve`/
`is_registered` (tier c), `see`→`admit` lazy promotion, `reap(ttl)` eviction, `resident_ids`,
`with_resident`. `html_export::Live` holds one and layers HTML rendering + the materialized
`<id>.jsonl` (tier (b)) + the `/stream` byte cursor over it — same URLs, same records, same
lazy behavior. The two maps use independent mutexes, never co-locked (no self-deadlock).

**Scope decision.** The plan's original goal also folded the **TUI `Frame` descend stack** into
the same store. Deliberately **not** done: the `Frame` stack is a LIFO navigation stack of live
`View`s with no byte cursor, keyed residency, or TTL, and it's already optimal (each ancestor
`View` stays alive, never re-parsed). Unifying it with the server's keyed/TTL'd residency store
— the only *other* consumer — would be a worse abstraction over two genuinely different access
patterns. The server store stands alone.

**Test / gate.** Store unit test (the tier lifecycle: path-only → resident → grow/fast-forward
→ idle-reap → re-resident) + served curl smoke (append → `reset` + back-patch + new turn) +
the full suite. All green (244 tests).

**Done.** Merged; store test + served smoke green; TUI descend/ascend unchanged (its own path).

---

## M13 — classification unify (Phase 3, byte-identical)

**Goal.** One `BlockKind` behind `fold_key`/`html_kind`/render, and one shared hunk-numberer.

**Changes.** Introduce `engine::BlockKind`; derive `fold_key`, `html_kind`, and render's
inline arms from it (kill the drift). Fold `render::render_patch` and `html_export::diff_part`
into one numberer producing neutral rows each surface formats. (`SessionIndex.counts` already
keys off `fold_key` — repoint it to `BlockKind`.)

**Consumed by.** TUI render, HTML emitter, the index counts.

**Test / gate.** render + HTML diff tests; dump sweep (byte-identical — the diff-fix is a
no-op today, verified in M7).

**Risk.** LOW-MEDIUM — mechanical, well-covered.

---

## M14 — message block-model lift (converge the waypoint) — ✅ DONE

**Goal.** The `Message` log stops carrying built `Block`s — the clean agent-neutral `Event`
set (§3.1). L1 emits raw-ish tool fields; L2 builds the `Block` in the fold.

**Landed.** `Message::ToolUse { id, name, input, cwd }` (raw); the block is shaped in L2 via
`Shaping::build_tool` — `claude_build_tool` (`Agent`/`Task`→`SubAgent`, else `ToolUse` with
`tool_target`/`extract_diffs`) and `codex_build_tool` (`call_details`). `engine::message` no
longer imports `Block`. The `Attachment` leaf value stays (not a shaped block). Proven
byte-identical by the equivalence oracles and a real-transcript pre/post render diff.

**Changes.** Move block-shaping (`tool_target`, `extract_diffs`, `call_details`, the two-event
spawn/completion) from the tokenizers into `Replayer`. Both tokenizers shrink to line-shape →
`Event`; `Shaping` folds into the `Adapter` proper.

**Consumed by.** Both engines (Claude + Codex) — one shared L2 with no dialect in the log.

**Test / gate.** The equivalence tests (still bit-identical); Codex + Claude golden.

**Risk.** MEDIUM — touches both tokenizers + the fold; the equivalence tests are the net.

---

## M15 — library polish + optional crate split (Phase 9)

**Goal.** Ship `replay-core` as a real, documented surface.

**Changes.** Document `parse_session`/`Session`/`Parser`/`Replayer`/`SessionIndex`;
`examples/` (parse.rs exists — add an incremental-follow example). Optionally lift `engine/`
into a `claude-replay-core` crate (no ratatui/syntect/clap deps → mechanical) once an
external consumer wants it; defer the split otherwise.

**Risk.** LOW.

---

## Cross-cutting test strategy (build once, run every milestone)

1. **Golden parse tests** (exist) — join/order, coalesce/group, skill, sub-agent two-event,
   attachments, images, queue, Codex response-items, apply_patch.
2. **Equivalence tests** — `replay_tokenize_matches_parse_main`, `codex_replay_matches_parse_lines`,
   and (M8) the split-apply test; these are the bit-identical net through M9/M14.
3. **Dump sweep** — `--dump`/`--dump-html`/`--dump-all-html` over the `claude-replay-eval`
   corpus, diffed against checked-in hashes. The primary "no user impact" proof for M9/M13.
4. **HTML stream tests** + served/live browser smoke.
5. **tmux e2e** — TUI, descend/ascend, live tail (M9, M11, M12). *(Note: the opt-in
   `s_opens_session_switcher_on_latest` currently fails on `main` — a pre-existing, unrelated
   flake to fix or quarantine before relying on the switcher e2e.)*
6. **RSS probe** — re-run at M9 (streaming) and M10 (metrics fold) on a large session.

## Sequencing summary

| # | Milestone | Nature | Risk | Unlocks |
|---|---|---|---|---|
| M8 | stateful `Replayer` | additive | LOW | ✅ **done** |
| M9 | streaming driver + freeze old parsers | migration, byte-identical | HIGH | ✅ **done** |
| M10 | metrics fold-in | byte-identical | MED | ✅ **done** |
| M11 | incremental primitives (snapshot/extend_ids) | additive, proven | MED | ✅ **done** (routing→M16) |
| M12 | `SessionStore` + tiers | internal | MED-HIGH | ✅ **done** (scoped; TUI stack left separate) |
| M13 | classification unify (BlockKind) | byte-identical | LOW-MED | ✅ **done** |
| M14 | message block-model lift | byte-identical | MED | ✅ **done** |
| M15 | docs finalization (crate split deferred) | docs | LOW | ✅ **done** |
| M16 | incremental live followers (`FollowParser`) | migration, byte-identical | HIGH | ✅ **done** |

**Recommended order:** M8 → M9 → M10 → M11 → M12, then M13/M14/M15 as cleanups (M13 and M14
can land any time after M8; M15 last). M8 is the safe keystone; M9 is the load-bearing swap
that finally removes the duplication; M11 is the payoff.
