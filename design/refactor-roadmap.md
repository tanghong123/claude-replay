# Refactor roadmap (viewer-side)

> **Status:** active plan. Sequenced, reviewed, approved. Execution is autonomous through Batch 1.
> Batch 2 follows Batch 1; the parked items need explicit go-ahead.

## Governing principle — incremental & resumable is first-class; one-shot is derived

Parsing and presentation are **incremental and resumable by default**. The foundational APIs are:

1. **`LineReader`** — a resumable line source (`tell` / `open_at` / `Position`).
2. **`SessionBuilder`** — an incremental session builder (`advance` / `snapshot` / `checkpoint` /
   `resume`).
3. **Incremental presentation building** — materialized views appended as the session grows (the
   HTML server's `<id>.jsonl` + `stream_delta` is the prototype; see
   [`session-cache.md`](session-cache.md) tier (d)).

The one-shot APIs — `parse_session`, the metrics parse, `--dump`/`--dump-html`, and every current
whole-file path — are **thin wrappers that drive the incremental API to completion**. No path may
assume a single-shot whole-file parse.

This is also what satisfies the **wiring contract** below: because every existing caller reaches
the incremental core *through* its one-shot wrapper, the whole test suite exercises the
incremental engine. So the resumable cursor and `checkpoint`/`resume` are **not deferred** — they
are the backbone, verified by resume-equivalence tests. Their heavy *production* consumer
(persistence-backed restart / eviction-reload) arrives with on-disk persistence in Batch 2
(decision **A**): resume is built + test-wired in Batch 1, production-consumed in Batch 2.

## Wiring contract (every step)

- **Fully wired, no dead code** — new code is reached by a real caller; one-shot paths are
  reimplemented on the new primitive rather than left beside it.
- **Green gate** — `cargo fmt --check`, `cargo clippy --all-targets` (no new warnings),
  `cargo test`.
- **Byte-identical** — `--dump - --full --width 100` and `--dump-html -` on the frozen Claude +
  Codex transcripts equal the pre-refactor baseline (these are output-preserving refactors).
- **Commit per step.**

## Batch 1 — viewer-side refactor (autonomous, in order)

1. **#25 Separate TUI/HTML modules.** `src/tui/` (app, view, picker, wrap, theme, markdown,
   highlight, render, clipboard); shared `fold.rs` + new `present.rs` (text formatters) top-level;
   `claude-replay-core::diff` (diff-row model + `base64_decode`). Fix CLAUDE.md Layout.
2. **#24 One-pass parse.** Drop the `scan_join_ids` pre-scan + the extra read; single forward
   fold, orphans resolved at finish. Enables clean incremental building.
3. **#18 `LineReader`.** Rename `TailReader`→`LineReader` (`tail.rs`→`reader.rs`) **and** add the
   resumable cursor `tell`/`open_at`/`Position` (first-class). Resume-equivalence test.
4. **#19 `SessionBuilder`.** Incremental `Session` build + `checkpoint`/`resume`; **reimplement
   `parse_session` and `FollowParser` on top of it.** One-shot ≡ run-to-EOF, exercised by the
   whole suite.
5. **#20 Sub-agent normalization.** Flat `Session` + sub-agent metadata map; supersede
   `SessionIndex.agents`; rewire renderers. Byte-identical.
6. **#21 `SessionCache` (tiers a+c).** Concrete cache owning the in-memory `Session` via
   `SessionBuilder`/`FollowParser`; `serve.rs` depends on it; absorb `SessionStore`. Tier-(b)
   neutral-on-disk + the materialized-view abstraction deferred to Batch 2.

## Batch 2 — after Batch 1 (order: #23, #22, #11)

- **#23 Lazy attachment content** — don't hold blobs resident.
- **#22 Metrics extension bag** — build the accumulating `extra` bag **and wire it into the
  `TranscriptAdapter` / `MetricsAccumulator` interface** so any agent *can* emit agent-specific
  metrics; the seam is the deliverable even with no current producer.
- **#11 Memory-footprint** — includes **tier-(b) neutral-on-disk persistence**, the production
  consumer of #18/#19's resume (restart survival + eviction-reload).

## Parked — no work until explicit go-ahead

- **#15** read-only task/todo panel (feature)
- **#16** surface plan documents (feature)
- **#17** JDI supervisor agent-agnostic spine (supervisor, not viewer)

See [`session-cache.md`](session-cache.md) and
[`line-reader-and-session-builder.md`](line-reader-and-session-builder.md) for the designs these
tasks implement.
