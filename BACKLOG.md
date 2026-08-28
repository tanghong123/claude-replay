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

- **Monitor v2 — single app-shell, no iframe** (owner, 2026-08-27; feasibility scouted, not
  designed). Goals: one shell instead of rail + `<iframe>`; one search box instead of two;
  every clickable artifact served through the browser rather than today's split (embedded
  bytes → Blob/`data:`, path-only → `/__reveal` into Finder); feature parity with the
  claude-replay one-page view, sharing its backend.

  What the scout found:
  - **Backend sharing is already done.** `service_routes(live, static_dir, name, query)` is the
    shared surface the monitor already calls, and `/session?id=…&chrome=embed&theme=…` already
    renders the view for embedding. v2 keeps this; nothing to build.
  - **The iframe is a LIFECYCLE boundary, not just layout.** `export.js` is one IIFE with ~78
    module-level state vars and listeners on `document`/`window`, and no teardown. Session
    switching is `view.src = …` — a full reload, which is what currently disposes of all that
    state for free. Removing the iframe means an explicit `init(container)`/`destroy()`
    contract, or a shell reload per switch (which forfeits the app-shell feel). This is the
    real cost of goal 1 and the thing that decides v2's shape.
  - **Viewport coupling is avoidable.** 27 sites in `export.js` use `window.scrollY` /
    `innerHeight` / `document.body.scrollHeight` / `scrollTo` — the follow/pin, jump-to-bottom,
    turn-landing and search-stepping contract that `claude-replay-browser-tests` exists to
    protect. Re-deriving it against an overflow container is the expensive path. It is NOT
    forced: today `html,body{height:100%}` keeps the monitor document unscrolled and the iframe
    scrolls internally, so a shell that lets the DOCUMENT scroll and makes the rail
    `position: fixed` removes the iframe while leaving every one of those sites untouched.
  - **Browser-served artifacts have a containment rule to reuse.** `/__reveal`'s `remap_reveal`
    already refuses any path no hosted root explains; a `/file?path=` endpoint can reuse that
    guard plus the existing token gate. Note it becomes a local-file read endpoint — the
    security review belongs in the design, and "reveal in Finder" probably stays as a secondary
    action for directories and binaries.
  - **One search box is federation, not a merge.** The rail searches the session INDEX; the view
    searches the record stream (`searchNeedle`/`searchScope`). One input over two result kinds;
    mostly UX design, little architecture. `rail.html` already notes the two boxes were aligned
    to read as one.

  **Decided (2026-08-27): reload first.** Native teardown for `export.js` is deferred until the
  reload proves too coarse.

  **Slice 1 landed** — `claude-monitor-v2/` (`agent-monitor-v2`, port 2828, its own cache root).
  A separate app on purpose: v1 is untouched and both run at once. It composes by fetching the
  session page from the SAME public backend (`SessionService::page`) and splicing its own shell
  in before `</body>`; every other route delegates to `service_routes` verbatim, and the rail's
  session list is built on the discovery facade (`store_all` + `session_card` + `session_id`),
  so nothing reaches into v1's private index. A browser test pins the property the whole shape
  was chosen for: the DOCUMENT still scrolls, the rail stays fixed while it does, the transcript
  clears it, and there is no iframe — so `export.js`'s ~27 `window.scrollY` sites are untouched.
  Not in the release matrix, so it ships to nobody until deliberately added.

  **Goal 3 landed** — `/file?path=` serves a clicked path's BYTES to the page (an overlay: the
  text, or the image) instead of opening Finder on the server; refusals (uncontained, a
  directory, a binary, over 8 MB) fall back to `/__reveal`, which is what those cases want.
  The scout's "reuse `remap_reveal`'s hosted-roots guard" described a guard that does not
  exist — `/__reveal`'s hit branch is `exists()` alone, which is fine for opening a Finder
  window and not for moving bytes. The route got POSITIVE containment instead: canonicalize
  both sides (a symlink out of a contained tree is what a textual prefix test loses), then
  require a hosted session's `cwd`, `project_path`, or transcript dir to explain the path.
  Nothing is ever served as ACTIVE content: anything that decodes as UTF-8 comes back as
  `text/plain` (markup included — you get to READ the file), raster images as themselves, and
  everything else as an `attachment` the browser SAVES rather than renders (owner, 2026-08-27:
  "for anything that cannot be safely rendered, offer them as downloads"). Serving a repo's
  `.html` or `.svg` as itself would be stored XSS with the agent's whole file history as the
  payload surface, since the page holds the monitor's cookie on the monitor's origin.
  `nosniff` + `Content-Security-Policy: sandbox` on every reply.

  **Reading local files requires PAIRING** (owner, 2026-08-27). The route needs a valid token
  and a same-origin request, so unpaired it is absent rather than narrowed — which is exactly
  where the connection gate is weakest: an unpaired loopback listener on macOS admits every
  local user. Unpaired pages keep today's reveal-in-Finder behaviour; `service_routes` now
  takes the whole `Request` so a route can see that verdict.

  **Goal 2 landed** — one visible search box. The rail's box filters the session list AND
  drives the transcript search by writing into the page's own `#q`, so scoping, highlighting
  and hit stepping run unforked; ⏎/⇧⏎ and the rail's ▲▼ click the page's own stepper. The
  page renders its own field hidden via `PageChrome::host_search` — the renderer hiding its
  own control, the way `embed` already hides the theme toggle, rather than the host reaching
  in with a selector.

  **Compose/send + pairing landed (2026-08-27)** — by SHARING, not porting. `claude-monitor`
  grew a library half (`[lib]` + `src/lib.rs`), and v2 depends on it: the same `Index` (so a
  row's liveness, counters, family and `injectable`/`consented` facts are one derivation), the
  same `ConsentStore`/`Passcode`, the same `send_route`/`consent_route`, the same transports.
  "May this prompt be injected into that pane" is answered once. v2's `sessions_json`,
  `live_by_argv`, `counters` and hide list are gone — ~200 lines of second implementation
  deleted, and `main.rs` in v1 fell from 1056 to 489 lines with nothing but moves.

  **One token for the machine, not one per app.** The `cmauth` cookie is scoped to
  `127.0.0.1` and NOT to a port, so two tokens would mean whichever page loaded last clobbers
  the other's cookie and the other's writes start 401ing. `agent-monitor-v2 --pair` mints and
  reads the same 0600 secret as v1, and one consent store covers the machine. Cache roots stay
  separate (durable entries, hide list, scan state).

  Known and correct: v2's growth-proof bank (#146) starts EMPTY, since a proof is earned by
  watching a session grow and a freshly started monitor has watched nothing. So v2 briefly
  marks fewer rows injectable than a long-running v1 — unproven means no compose, which is the
  rule working. v1 does the same after a restart.

  Still to do: the new UX itself beyond the parity rail (HELD at the owner's request), and
  whether the reload on session switch is in fact too coarse.

- **QoderWork spawn chips read `Agent(agent: …)`.** Its spawn input names no `subagent_type`, so
  the fold falls back to the tool name and every QoderWork child renders with the type `agent` —
  while the source knows better in two places (`toolUseResult.agentType` on the result, `agentType`
  in the `subagents/` sidecar, both usually `general-purpose`). #37 deliberately left the label
  alone: adopting the sidecar's type for RUNNING spawns only would have flipped the label back the
  moment the result folded. Fixing it properly means a QoderWork-local `join_result` that lifts the
  type in-band as well — a rendering change, so it rides a byte-gate re-baseline.

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

## Queued — approved, not started

- **Presentation-layer host slot (`PageChrome.host`)** — approved 2026-08-27. v2 positions
  itself with the RENDERER's internal selectors: it pads `body`, offsets `#topbar`, and nudges
  `#newbadge`, having learned from `export.css` that `.layout` centres itself. That knowledge
  lives outside the crate that owns the layout, so an `export.css` restructure misaligns v2
  silently — the one place a fork of the presentation layer could start by accident. Fix:
  an opaque host slot on `PageChrome` (an inset the host asks for, plus a `data-shell`
  attribute), with the offsets living in `export.css` beside the layout they offset. Purely
  additive and inert for v1 (existing callers pass no chrome ⇒ byte-identical dumps), and it
  moves the coupling inside the crate that owns it.

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
