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

- **#167 — the durable cache refactor ("one cache, three providers")** — waiting on the
  owner's **final design review** (requested 2026-08-20) of
  [design/session-cache-redesign.md](design/session-cache-redesign.md);
  [design/cache-persistence-seam.md](design/cache-persistence-seam.md) preserves the
  exploration. The rule being implemented: the session cache has no knowledge of
  persistence — durability comes from `BlockStore` and the other provider interfaces,
  so the durable directory leaves the main cache API. Build starts on sign-off.
  Review in progress — the spec sharpened four times on 2026-08-20/21 (`ours` named as the
  entry-backing witness; quiesce's UNLOCK leveled; redirect placement per provider; the
  admitting gate's why) and **amended once**: §4.2a (owner, 2026-08-21) replaces the
  three-maps + global-gate internals with one slot per session, the first drive being the
  admission, `admit()` kept as the compatibility veneer (migration step 2b). **One flagged
  question for the owner before build**: post-quiesce peer takeover — serve the retained
  blocks read-only, or redirect (§4.2a's OPEN box).

## Evidence-blocked

- **#29 — Codex `update_goal` collapses goal states** (blocked→completed,
  unknown→pending). Scanned 2026-08-20: the local 9-session Codex corpus never calls
  `update_goal`/`update_plan` (only exec/wait/rowt/parity fired), so the real vocabulary
  is still unobserved — and the fix wants a rendering call (should `blocked` stay
  distinct from `completed`?) along with it.
- **#30 — Qoder credits: absent `billable` defaults to billable.** Scanned 2026-08-20:
  zero `credits` lines across all 52 local QoderWork sessions — the credits path belongs
  to Qoder-the-IDE, which the owner does not use. Stays open awaiting a Qoder corpus.
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
