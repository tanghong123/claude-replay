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
  re-baseline), the **counter home** for elision gauges (b recommended: per-fold report
  output, no FOLD_VERSION bump), and — added by the §10 review addendum (2026-08-20) —
  **placement**: per-read-site elision as §4 specifies, or one `LineSource` collapsing the
  four hand-rolled whole-file `read_line` loops into one, which makes §4's offset invariant
  structural — and §10.6 (review, 2026-08-20) raises the stakes on that choice: elision over an
  already-buffered `&str` removes the `Value` DOM multiplier but **not** the buffer, so §4's
  per-site placement leaves the raw read unbounded permanently unless the chunked read is later
  written four times. Also measured in review: `LineReader::poll` is `read_to_end` + a
  `Vec<String>`, so a followed session's COLD poll materializes the whole file twice — larger
  than anything else in the inventory and unreachable from `advance_at`.
  **§11 (2026-08-20) is a full boundedness audit and the go/no-go answer: GO, scope corrected.**
  13 unbounded transcript reads found (12 unbounded per line, 1 unbounded per file); 4 sites are
  already bounded and need nothing. The trap: `.lines().take(N)` bounds the line COUNT, not its
  SIZE, so `detect_agent`'s five-line sniff reads an ENTIRE file with no newlines — and discovery
  runs it on every candidate. Mechanism (§3/§5.3/§7/§8) is sound and unchanged; what fails is a
  scope that cannot deliver the property. Cost is far below "rewrite every read": the streaming
  primitive is already implemented and tested in `agent-metrics/src/elide.rs`
  (`read_line_elided`, 475 lines) — adopting it is the stated point of #193. Two pieces are new:
  `load_attachment` (capture sink on the same state machine) and, **under α only**, making that
  scanner path-aware — the seed is explicitly size-driven, "not a list of known paths". So
  **α vs β is re-opened on new terms**: boundedness forces the policy inside the read, which
  makes α the larger half of the change rather than nearly free. Also corrected: `session_card`
  is bounded on its COLD read only — its memoized path reads everything appended since the last
  scan and pre-reserves it. Agent-written sidecars (`qoderwork` `sidecar`, `load_tasks_in`) are
  read whole too: same invariant, separate issue. Constants (64 KB / 256 KB / K=64 / 64 MB) need a nod. Docs updated alongside:
  `docs/architecture.md` §7 (+ the html twin) now names the raw line as the ladder's one
  unbounded rung, marked designed-not-built. Addendum also records a **pre-existing**
  torn-tail divergence (`advance_reader` counts a torn final line malformed;
  `parse_reader` documents why it must not) — out of #193's scope, surfaced not fixed.
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
