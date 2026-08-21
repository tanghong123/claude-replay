# BACKLOG — the work tracker

> The single in-repo view of pending and ongoing work. Design docs carry the *arguments*
> (each under `design/`, with a status header); GitHub issues carry the *discussion*;
> **this file carries the state** — what is waiting, on whom, and why. It exists because
> status scattered across 30+ design docs let an agreed refactor (#167) slip out of view.
>
> **Rules.** One entry per item: status · links · the one thing that unblocks it.
> Update this file **in the same commit** that changes an item's state (started, decided,
> shipped, parked) — the same discipline as the release rule. When an item ships, delete
> its entry (the commit and the design doc's status header are the record; this file lists
> only live work). Any agent picking up work in this repo reads this file first.

## Bugs / regressions

- **HTML view: near-bottom viewport creeps on tail growth** (owner, 2026-08-20): with the
  "jump to the bottom" affordance showing (follow OFF, scrolled near but not at the
  bottom), arriving blocks keep scrolling the view down; expected a stable viewport, and
  it used to be. **Investigated 2026-08-21** with the new browser harness
  (`claude-replay-html/tests/browser_follow.rs`, headless Chrome, `--ignored`): the
  claude-shaped legs all HOLD — pinned-follow, unpin-on-scroll, appends, open-tool +
  result reshapes, growing open turn, viewport inside a tall open turn (ids observed
  append-only; zero churn). The report's tell — the pill stuck on "Jump to bottom" while
  blocks arrive — means `added ≤ 0` applies. Un-exercised legs, in suspicion order:
  (1) a **Codex** live session — 57f4090 maps exploration into coalescing activity
  spans, the one shape that rewrites rendered ids; (2) an uninterrupted
  thinking+activity span (no prose between calls) growing live; (3) the monitor's
  iframe context. **Concrete next step — real-codex tail-replay**: copy a local Codex
  transcript (the #28–31 corpus), truncate to ~80%, serve the copy, append the
  original's remaining lines chunk-by-chunk while the harness browser watches
  `__ids` churn and `scrollY` — real coalescing shapes, no synthetic-fixture risk of
  authoring another span-breaking artifact. Mechanism to check when it reproduces:
  provisional `b{n}` anchors are positional, so a coalesce makes `restoreAnchor`
  hold the WRONG element (id names shifted content) — fix sketch: capture a content
  signature beside the id and hold absolute `scrollY` when it mismatches. Ask the
  owner which session they were watching (claude / codex / via the monitor rail).

## Evidence-blocked

- **#29 — Codex `update_goal` collapses goal states** (blocked→completed,
  unknown→pending). A Codex 0.147 transcript supplied on 2026-08-20 contains 17 real
  `update_plan` calls, but `update_goal` remains unobserved — and the fix still wants a
  rendering call (should `blocked` stay distinct from `completed`?) along with it.
- **#30 — Qoder credits: absent `billable` defaults to billable.** Scanned 2026-08-20:
  zero `credits` lines across all 52 local QoderWork sessions — the credits path belongs
  to Qoder-the-IDE, which the owner does not use. Stays open awaiting a Qoder corpus.
- **#32 — `fork_origin` is sidecar-only**, so fork families degrade for sidecar-less
  (post-July-2026) QoderWork sessions. Needs a post-July fork in the store to see where
  QoderWork now records the relation (candidate: `sub_chats.ext`). Softened by the rail's
  title-clustering (#153).

## Needs an owner decision (design sketched, unscheduled)

- **Block provenance anchors** — surfaced in the #167 review (2026-08-21): the fold has no
  stable block identity, so per-block presentation state is position-keyed (the TUI's
  `user_folds` overlay — a gesture can land on the wrong block across an equal-count tail
  reshape), which is tolerable for gestures and insufficient for any presentation layer
  needing strong consistency with `Vec<Bv>` changes. Sketch: `anchor = (first contributing
  line's byte offset, emit-ordinal within the line)` — the `(at, index)` vocabulary
  attachments already use; same anchor across re-emission, first-constituent's anchor on
  coalesce, consumers join old-by-anchor vs new-by-anchor per delta. Cost: ~10 B/block
  persisted ⇒ one FOLD_VERSION bump — ride it with #167's build. Decide: adopt into #167's
  scope, or file as its own issue.

## Queued

- **#34 — `--dump --json`: the structured block stream** (2026-08-21; the second half of
  the shell-out vocabulary `--paths --all` opened): emit `Session::blocks()` as JSON —
  `{i, turn, ts, kind, text | name/target/exit/ms/output}` — with `kind` from the existing
  `fold_key` classification so the JSON never invents a vocabulary the renderers don't
  already use. Timestamps are **per turn** (`Session.user_times` / `index.turns[*].time`)
  and the shape must say so rather than implying per-block precision the model doesn't
  hold. Why: the engine already folds all four agents into one `Block` vocabulary — a
  consumer that can't link the crate (whid, the cross-repo progress collector) otherwise
  writes one transcript slicer per agent, and only ever gets around to writing Claude's.
  The full contract — field shapes, the per-TURN timestamp constraint, and the ownership
  split behind it — is in issue #34. Gate: the text `--dump` output must stay byte-identical
  (the JSON is a second emission, not a re-render).

- **Architecture doc refresh** (owner, 2026-08-21; unblocked — #167 shipped v1.98.0):
  bring `docs/architecture.md` (+ the `.html` twin) up to date on the two structural
  changes — the #193 bounded eliding reader (one byte-toucher, `LineSource`, span-hinted
  locators; §7's rung note exists but the read-path narrative predates it) and the #167
  durable-cache interface (providers behind `Entries`, the three providers incl.
  `Transient`, the slot topology, `admit` as veneer, the aux contract).

## Parked — explicit go-ahead needed

- **Fleet relay (Phase 2)** — [design/fleet-pairing.md](design/fleet-pairing.md) §6:
  reach the fleet page away from the LAN via a small relay (shaped; Cloudflare Worker is
  the evidenced candidate). Phase 1 (token pairing, same-user gate) shipped v1.78–v1.79.
- **HTML message-type filter** — DESIGN.md backlog: generalize the export page's
  "Tools ▾" dropdown to filter by block type (Agent spawns, thinking, attachments),
  keeping tool-name granularity as a sub-case.

## Small / opportunistic

- **QoderWork deleted-session filtering** —
  [design/qoderwork-rail-noise.md](design/qoderwork-rail-noise.md) item 4: UI-deleted
  sessions have no file-level tell; "transcript present, `sub_chats` row absent" is the
  test. Cheap now that the DB reader is compiled into every macOS build (v1.96.3).
