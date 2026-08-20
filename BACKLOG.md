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

- **#193 — the bounded eliding line reader** —
  [design/bounded-line-reads.md](design/bounded-line-reads.md), **v2.2 (2026-08-20)**: the
  review wave is fully integrated — the marker is an exact substitution pointer
  (`<elided:{off},{len}>`, absolute; splice the file bytes back and the original line returns
  byte-for-byte), the fold is named sans-io (elision must commute with folding — the
  architectural α/β statement), the span load validates its untrusted marker before
  allocating, identity reads run elision-off, and α gained a cheap form (α-lite, key-suffix
  policy). Settled: placement, the substitution marker. On the owner, per its §11:
  **① α vs β** — owner leans α on the sans-io principle, α-lite recommended, no longer the
  larger half; **② constants** (64 KB / 256 KB / K=64 / J=64 / 64 MB — now part of the
  persisted-format contract) need a nod; **③ counter home** — (a) `Metrics::extra`,
  free since the hint bumps FOLD_VERSION anyway. The marker is postfix-framed
  (`{prefix}<elided:{off},{len}>{postfix}` — reconstruction + the load-time content check).
  Build starts on sign-off, per §10.
- **#167 — the durable cache refactor ("one cache, three providers")** — waiting on the
  owner's **final design review** (requested 2026-08-20) of
  [design/session-cache-redesign.md](design/session-cache-redesign.md);
  [design/cache-persistence-seam.md](design/cache-persistence-seam.md) preserves the
  exploration. The rule being implemented: the session cache has no knowledge of
  persistence — durability comes from `BlockStore` and the other provider interfaces,
  so the durable directory leaves the main cache API. Build starts on sign-off.
  Review in progress: **§1.1 added 2026-08-20** — the three states (transcript / durable
  entry / resident), the fact that no background thread reconciles them, and why the
  client's call order (resident ← durable first, then one transcript pass advancing both)
  is the cheap one. Design unchanged; this documents an existing property the doc assumed.

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
