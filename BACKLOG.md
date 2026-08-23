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

## Parked — explicit go-ahead needed

- **Sunset the `claude-*` compat symlinks + tap rename mapping** (v1.101.0 shipped the
  rename): drop `bin.install_symlink` old names from the three formulas and retire
  `formula_renames.json` at the next major boundary (or ~2 months of releases). Will be
  forgotten otherwise — this entry is the reminder.

- **agent-metrics: monitor state lookup must check both dir names** — fresh machines now
  write `~/.local/state/agent-monitor` while agent-metrics looks up `claude-monitor`
  (existing machines are unaffected: old dirs keep winning). Patch agent-metrics to try
  both. Standing rules there: never push its Aone remote; no AI-attribution trailers.

- **#35 — cursor'd resume for `--dump --json`** (owner, 2026-08-22): kill the daily
  sweep's active-file read amplification (measured 30–70× on >130 MB live sessions;
  full sweep 12.3 s / windowed daily 6.3 s at v1.100.0). Opt-in cursor
  (`MetricsCursor` pattern; likely a `Json` presentation in the #167 cache — the
  committed-only record contract already solves the reshaping tail); the default dump
  stays read-only/lock-free. **Watch trigger**: active sessions ~2–3× bigger, or the
  collector's sweep showing up in real profiles. Until then whid passes `--since 24h`
  at discovery (shipped v1.99.0). Numbers + design in the issue.

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
