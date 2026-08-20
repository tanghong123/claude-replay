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

## Needs an owner decision

- **#193 — the eliding line reader** — [design/bounded-line-reads.md](design/bounded-line-reads.md).
  Study complete; build nothing until reviewed. Decisions: **α vs β** (α recommended:
  adapter-supplied elision policy through a seam hook, viewer-lossless, no byte-gate
  re-baseline) and the **counter home** for elision gauges (b recommended: per-fold report
  output, no FOLD_VERSION bump). Constants (64 KB / 256 KB / K=64 / 64 MB) need a nod.
- **#167 — the durable cache refactor ("one cache, three providers")** — waiting on the
  owner's **final design review** (requested 2026-08-20) of
  [design/session-cache-redesign.md](design/session-cache-redesign.md);
  [design/cache-persistence-seam.md](design/cache-persistence-seam.md) preserves the
  exploration. The rule being implemented: the session cache has no knowledge of
  persistence — durability comes from `BlockStore` and the other provider interfaces,
  so the durable directory leaves the main cache API. Build starts on sign-off.

## Unblocked — revisit (the owner now has real Codex sessions, 2026-08-20)

- **#29 — Codex `update_goal` collapses goal states** (blocked→completed, unknown→pending).
  Verify the real vocabulary against the local Codex store, then fix.
- **#31 — Codex metrics assume no cache-write tier** (`cache_creation` hardcoded 0).
  Verify against real Codex token payloads.
- **#28 — unrecognized spawn-result status leaves a sub-agent Running forever.**
  The fix needs no per-agent vocabulary (absent ≠ present-but-unrecognized; the latter
  should read `AgentStatus::Unknown`); the new sessions let it be verified end to end.
- **#30 — Qoder credits: absent `billable` defaults to billable.** Check whether the
  local QoderWork store carries `billable` evidence before deciding the default.

## Evidence-blocked

- **#32 — `fork_origin` is sidecar-only**, so fork families degrade for sidecar-less
  (post-July-2026) QoderWork sessions. Needs a post-July fork in the store to see where
  QoderWork now records the relation (candidate: `sub_chats.ext`). Softened by the rail's
  title-clustering (#153).

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
