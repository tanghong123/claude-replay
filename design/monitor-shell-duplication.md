# The monitor's two shells: where the logic forks, and the seams that would stop it

Status: **proposal** (task #40, 2026-09-03). Measured against `main` at v1.138.0. Nothing here
is built; the owner picks a direction, then the queue tracks it.

## Why now

The parity tasks (#32–#37) re-implemented classic behaviour in the app shell one item at a
time: hide/restore, reveal, fork families, scroll memory, reading controls, the keymap. Each
was done the right way for the app shell — its own surfaces, tested pure logic — and each was
also the fork growing by one more copy. The two shells are both supported until the app shell
is validated (CLAUDE.md); the longer that takes, the more twice-written logic there is to keep
in step. This note measures the duplication and proposes the seams that make the eventual drop
of the classic shell a deletion of *layout*, not of *logic*.

## The four frontend worlds (lines, `wc -l`)

| world | file(s) | lines | what it is |
|---|---|---|---|
| classic v1 rail | `claude-monitor/src/rail.html` | 790 | session list + iframe host, inline JS |
| classic v2 splice | `claude-monitor-v2/src/shell.html` | 493 | session list spliced into the session page |
| the session view | `claude-replay-html/src/html/export.js` (+ `export.css`) | 3,514 (+858) | the transcript itself, embedded by both classic shells |
| the app shell | `claude-monitor/src/codex-ui/*.js` (+ `production.css`, generated `reference.*`) | 2,341 | one document: list + transcript + preview |

Backend: `claude-monitor/src/main.rs` (550 lines) and `claude-monitor-v2/src/main.rs` (390)
each carry a route table of the same shape — `api/sessions`, `api/ignore`, `api/ui`,
`api/send`, `api/consent`, `session`, `service_routes` fallthrough — and only `ui.rs` is
shared between them.

## The duplicated concerns

Where a concern lives, and whether the copies have already diverged. Function anchors are the
names to grep; line numbers drift.

| concern | classic | app shell | diverged? |
|---|---|---|---|
| consume the `/pull` + `/records` stream | `export.js` `consumePull`, `ingest`, `applyRecord`, `resetFrom` | `record-store.js` (84) | Same protocol, two clients. The app shell's is smaller and has the cursor race handled inline; export.js carries the epoch/zone logic. |
| follow the tail / DOM anchoring | `export.js` `setFollowing`, `captureAnchor`, `restoreAnchor`, `settleAfterApply`, `toBottom` | `viewport.js` (409): `captureDomAnchor`, `reconcile`, `convergeBottom`, `USER_INTENT_MS` | Yes: the app shell virtualizes (bounded window), export.js materializes with `.more` groups. Same contract, different machinery — the browser harness holds both. |
| scroll memory per session | `export.js` `saveView` / `loadView` (sessionStorage) | `view-memory.js` (19) + `viewport.beginSession/remember` | Same idea, written twice (#35). |
| search with scope letters | `export.js` `search`, `scopeLetters`, `searchInScope`, `markHits`, `stepHit` | `app.js` `updateSearch`, `stepSearch`, `markSearch`, the `scopes` table | Same seven letters `u a t o b r e`; the app shell adds global search across sessions (`openGlobalSearch`). |
| tool-type filter | `export.js` `setFilter`, `filterNav`, `computeFilterHits` | `app.js` `renderFilterMenu`, `applyFilters` | Classic steps between hits (▲▼); the app shell only hides (audit #14, deferred). |
| capability-aware file actions | `export.js` `openArtifact`, `fileview`, the `fsig ? openArtifact : reveal` rule at the tool header | `components.js` `attachmentCapability`, `referenceAction`, `revealQuery`; `attachment-viewer.js` | Same two-stamp rule since #33; the app shell's is pure and node-tested, export.js's is inline. |
| HTML sandbox | `export.js` `fileview` (an `<iframe sandbox>`) | `sandbox.js` (28): policy placed by rule, node-tested | The app shell's is the safer one (v1.129.1); export.js still injects its own way. |
| image lightbox | `export.js` `lightbox` | `attachment-viewer.js` (105) | Two lightboxes. |
| session list: grouping, hide/restore, fork families, state labels | `rail.html` `render`, `families`, `clusterKey`, `ignore`, `gStatusTip`, `rowTitle`; `shell.html` `render` | `session-visibility.js` (92, pure), `app.js` `renderTree`, `displayState` | Three lists. The app shell's grouping/families/visibility are pure functions; the rails' are inline. State labels are a hand-kept table in each. |
| compose / consent / passcode | `rail.html` (`openCompose`…`doSend`, ~110 lines), `shell.html` (same, ~90) | `control-store.js` (124) | **Three copies** of the same `/api/send` + `/api/consent` protocol, including the passcode lockout dance. |
| theme | `rail.html` `applyTheme` (+ reframe into the iframe), `shell.html`, `export.js` `applyTheme` | `app.js` `themeBtn` | Four toggles of one `data-theme`. |
| keyboard | `rail.html` document keydown (`/`, ↑↓); `export.js` `onKey` (`/ [ ] j k n N w - + = Space`) | `keymap.js` (55, a table) + `app.js` actions | Same keys since #37; the app shell's is a table, export.js's is an if-chain. |
| reading controls / raw toggle | `export.js` `setMono`, `setWrap`, `setWide`, `rawToggle` | `reading.js` (19) + `app.js`; `components.js` `rawTurnHtml` | Same three preferences, persisted under different keys (`WRAP_KEY`… vs `am-prod-reading`). |
| pairing / token bootstrap | the `?token=` → cookie 302 is server-side; each page reads `{{PAIRED}}` | `data-paired` on `<body>` | Fine — one implementation, three consumers. |

Not duplicated, and worth saying so: the render policy and the two capability stamps are
server-side once (`sig.rs`); the session index is once (`index.rs`); the control plane is once
(`control.rs`, `consent.rs`); `ui.rs` is shared by both binaries.

## What the fork has already cost

- **Divergence is real, not hypothetical.** Before #37 the app shell answered three keys and
  the classic view fourteen; before #35 one remembered your place and the other did not;
  before #33 one could reveal a file and the other could only copy its path. Each was a fix in
  one copy that the other copy had had for a year.
- **Fixes land twice or not at all.** The reveal-vs-file stamp confusion (v1.129.1) was a bug
  in the app shell's copy of a rule export.js already had right. The CSP-in-a-comment hole was
  fixed in `sandbox.js`; export.js's `fileview` still injects its own way and has not been
  audited against the same case.
- **Two route tables drift.** `api/ui` had to be added to both `main.rs` files by hand; v2's
  table has a `think` arm v1's does not, and v1 has `session` embed handling v2 does not.

## The seams

The principle: **logic becomes shared ES modules that both shells import; layout stays per
shell.** The classic pages are served by the same binaries as the app shell, so `rail.html`
and `shell.html` can `import` from `/monitor-ui/*` exactly as `app.js` does — nothing about
them being single-file pages prevents it. What must stay per shell is what makes each a
shell: the rail's rows and its iframe host, the splice's insertion into the session page, the
app shell's header/sidebar/preview chrome.

### 1. Shared modules, served by `ui::asset()` — in this order

| step | module | replaces | risk |
|---|---|---|---|
| a | `session-visibility.js` (exists) | `rail.html` `families`/`clusterKey`/hidden filtering; `shell.html`'s | Low: pure, tested; the rails keep their row markup and call the functions. |
| b | `capabilities.js` — extract `attachmentCapability`, `referenceAction`, `revealQuery` from `components.js` | `export.js`'s inline `fsig ? openArtifact : reveal`; `fileview`'s sandbox → `sandbox.js` | Low–medium: export.js gains an `import`, which means `export.js` becomes a module (or the two functions are inlined by the html crate's build). This is the step that also closes the unaudited `fileview` sandbox. |
| c | `control-store.js` | the three compose/consent copies | Medium: three UIs, one protocol; the store already separates state from DOM. |
| d | `keymap.js` (exists) + `reading.js` (exists) | `rail.html` keydown, `export.js` `onKey`/`setMono`/`setWrap`/`setWide` | Low: tables and setters; export.js keeps its actions. |
| e | `record-store.js` | `export.js` `consumePull`/`ingest` | **High** — export.js's client carries the epoch/zone and durable-cache resume logic and is byte-gated; do it last, or accept two clients and pin both with the browser harness. |
| f | `state-labels.js` — the busy/wait/idle label table | `rail.html` `gStatusTip`, `app.js` `displayState` | Trivial once (a) is in. |

Theme: one `theme.js` with `apply(dark)` that also posts into an iframe when there is one —
the rail's `reframe` is the only special case.

### 2. One route table in the `claude-monitor` lib

`claude_monitor::routes(cfg) -> impl Fn(&Request) -> HttpResponse` holding
`api/sessions`, `api/ignore`, `api/ui`, `api/send`, `api/consent`, the `monitor-ui/*` assets,
and the `service_routes` fallthrough; each `main.rs` keeps only its shell selection (`/` →
rail vs splice vs app) and its port/cache defaults. `api/ui` never gets added by hand twice
again; v1's `session` embed handling and v2's `think` arm become explicit, named differences
instead of drift.

### 3. What the classic shell keeps as its own

After 1(a)–(d): `rail.html` keeps its row markup, the iframe host, the strip/collapse
behaviour and its CSS; `shell.html` keeps the splice and its rail; `export.js` keeps the
transcript rendering it has always owned (which the app shell does NOT share — it renders
records through `view-model.js`/`components.js`, and that is the one large fork this note
does not propose to close: two renderers over one record stream, each with its own tests).
Dropping the classic shell then deletes `rail.html`, `shell.html`, the splice code in v2's
`main.rs`, and nothing else.

### Migration order that keeps both shells shipping

(a) → (f) → (d) → (b) → 2 → (c) → (e) — pure tables and predicates first (no behaviour
change, the byte gate stays green), the route table before compose so the shared store has
one server, and the record client last because it is the only step the byte gate can catch
regressing. Every step keeps `reference-*.{css,html}` generated, production chrome layered at
runtime, and nothing under `design/` carrying real session content.

### Gates per step

`ui_contract.mjs` for every pure module; `browser_follow.rs` for anything that scrolls, folds
or navigates (six app-shell cases exist; the classic v2 case exists; the rail has none — add
one before (c)); the byte gate for anything touching `export.js`.
