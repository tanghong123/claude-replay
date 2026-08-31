# Monitor v2 — a single app shell, no iframe

> **Partly built.** Slices 1–3 (the shell, browser-served artifacts, one search box) and
> compose/send + pairing shipped between 2026-08-26 and 2026-08-27. The new UX beyond the
> parity rail is HELD at the owner's request, and whether the reload-per-switch is too coarse
> is still unmeasured. Live work is tracked in the queue (`tasks/`), not here — this doc
> carries the argument and the record of what landed and why.

`agent-monitor` today is a session-list rail beside the html crate's session view in an
`<iframe>`. v2's goals, as set by the owner on 2026-08-27:

1. one shell instead of rail + `<iframe>`;
2. one search box instead of two;
3. every clickable artifact served through the browser rather than the split it had
   (embedded bytes → Blob/`data:`, path-only → `/__reveal` into Finder);
4. feature parity with the claude-replay one-page view, sharing its backend.

## What the feasibility scout found (2026-08-27)

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
  forced: `html,body{height:100%}` keeps the monitor document unscrolled and the iframe
  scrolls internally, so a shell that lets the DOCUMENT scroll and makes the rail
  `position: fixed` removes the iframe while leaving every one of those sites untouched.
- **Browser-served artifacts have a containment rule to reuse.** `/__reveal`'s `remap_reveal`
  refuses any path no hosted root explains; a `/file?path=` endpoint can reuse that guard plus
  the existing token gate. It becomes a local-file read endpoint — the security review belongs
  in the design, and "reveal in Finder" probably stays as a secondary action for directories
  and binaries. (This turned out to overstate the existing guard — see Goal 3 below.)
- **One search box is federation, not a merge.** The rail searches the session INDEX; the view
  searches the record stream (`searchNeedle`/`searchScope`). One input over two result kinds;
  mostly UX design, little architecture. `rail.html` already notes the two boxes were aligned
  to read as one.

**Decided (2026-08-27): reload first.** Native teardown for `export.js` is deferred until the
reload proves too coarse.

## What landed

### Slice 1 — the shell

`claude-monitor-v2/` (`agent-monitor-v2`, port 2828, its own cache root). A separate app on
purpose: v1 is untouched and both run at once. It composes by fetching the session page from
the SAME public backend (`SessionService::page`) and splicing its own shell in before
`</body>`; every other route delegates to `service_routes` verbatim, and the rail's session
list is built on the discovery facade (`store_all` + `session_card` + `session_id`), so
nothing reaches into v1's private index. A browser test pins the property the whole shape was
chosen for: the DOCUMENT still scrolls, the rail stays fixed while it does, the transcript
clears it, and there is no iframe — so `export.js`'s ~27 `window.scrollY` sites are untouched.
Not in the release matrix, so it ships to nobody until deliberately added.

### Goal 3 — artifacts served to the page

`/file?path=` serves a clicked path's BYTES to the page (an overlay: the text, or the image)
instead of opening Finder on the server; refusals (uncontained, a directory, a binary, over
8 MB) fall back to `/__reveal`, which is what those cases want.

The scout's "reuse `remap_reveal`'s hosted-roots guard" described a guard that does not exist
— `/__reveal`'s hit branch is `exists()` alone, which is fine for opening a Finder window and
not for moving bytes. The route got POSITIVE containment instead: canonicalize both sides (a
symlink out of a contained tree is what a textual prefix test loses), then require a hosted
session's `cwd`, `project_path`, or transcript dir to explain the path.

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
local user. Unpaired pages keep the reveal-in-Finder behaviour; `service_routes` takes the
whole `Request` so a route can see that verdict.

### Goal 2 — one visible search box

The rail's box filters the session list AND drives the transcript search by writing into the
page's own `#q`, so scoping, highlighting and hit stepping run unforked; ⏎/⇧⏎ and the rail's
▲▼ click the page's own stepper. The page renders its own field hidden via
`PageChrome::host_search` — the renderer hiding its own control, the way `embed` already hides
the theme toggle, rather than the host reaching in with a selector.

### Compose/send + pairing — by SHARING, not porting

`claude-monitor` grew a library half (`[lib]` + `src/lib.rs`), and v2 depends on it: the same
`Index` (so a row's liveness, counters, family and `injectable`/`consented` facts are one
derivation), the same `ConsentStore`/`Passcode`, the same `send_route`/`consent_route`, the
same transports. "May this prompt be injected into that pane" is answered once. v2's
`sessions_json`, `live_by_argv`, `counters` and hide list are gone — ~200 lines of second
implementation deleted, and `main.rs` in v1 fell from 1056 to 489 lines with nothing but moves.

**One token for the machine, not one per app.** The `cmauth` cookie is scoped to `127.0.0.1`
and NOT to a port, so two tokens would mean whichever page loaded last clobbers the other's
cookie and the other's writes start 401ing. `agent-monitor-v2 --pair` mints and reads the same
0600 secret as v1, and one consent store covers the machine. Cache roots stay separate
(durable entries, hide list, scan state).

Known and correct: v2's growth-proof bank (#146) starts EMPTY, since a proof is earned by
watching a session grow and a freshly started monitor has watched nothing. So v2 briefly marks
fewer rows injectable than a long-running v1 — unproven means no compose, which is the rule
working. v1 does the same after a restart.
