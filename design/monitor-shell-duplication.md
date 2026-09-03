# The monitor's two shells: where the logic forks, and the seams that would stop it

Status: **accepted, executing** (task #40; reviewed and corrected 2026-09-03 — see "Review
notes" at the end). Measured against `main` at v1.139.0. Steps are queue tasks #42–#49.

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
each carry a route table with the same five API arms — `api/ui`, `api/sessions`,
`api/ignore`, `api/send`, `api/consent` — plus the `monitor-ui/*` assets and the
`service_routes` fallthrough; only `ui.rs` is shared between them. The genuine differences
are two: v1 has a `session` arm that defaults `chrome=embed` for its iframe, and v2's index arm
splices its classic rail into the session page.

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
| HTML preview sandbox | — (the classic view never renders HTML inline: `fileview` shows an image or text, and an HTML file is a download or a reveal) | `sandbox.js` (28): policy placed by rule, node-tested | Not duplicated. The app shell is the only place HTML is rendered in-page, and its policy is tested. |
| image lightbox | `export.js` `lightbox` | `attachment-viewer.js` (105) | Two lightboxes. |
| session list: grouping, hide/restore, fork families, state labels | `rail.html` `render`, `families`, `clusterKey`, `ignore`, `gStatusTip`, `rowTitle`; `shell.html` `render` | `session-visibility.js` (92, pure), `app.js` `renderTree`, `displayState` | Three lists. The app shell's grouping/families/visibility are pure functions; the rails' are inline. State labels are a hand-kept table in each. |
| compose / consent / passcode | `rail.html` `openCompose` + `updateButton`…`doSend` (~60 lines of protocol), `shell.html` the same (~50) | `control-store.js` (124) | **Three copies** of the same `/api/send` + `/api/consent` protocol, including the passcode lockout dance. |
| theme | `rail.html` `applyTheme` (+ a reframe into the iframe), `export.js` `applyTheme` | `app.js` `themeBtn` | Three toggles of one `data-theme` (`shell.html` has none — it rides the session page's). |
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
  in the app shell's copy of a rule export.js already had right; the scroll-memory and keymap
  fixes went into the app shell's copies while export.js kept its own.
- **Two route tables drift.** `api/ui` had to be added to both `main.rs` files by hand, and
  the two genuine differences (v1's embed arm, v2's splice) are implicit in two 400-line files
  rather than named anywhere.

## The seams

The principle: **logic becomes shared modules that every consumer loads from one source;
layout stays per shell.** What must stay per shell is what makes each a shell: the rail's rows
and its iframe host, the splice's insertion into the session page, the app shell's
header/sidebar/preview chrome.

### 0. The packaging seam — one source, two loaders

The constraint the first draft missed: **`export.js` is embedded, not served.** The html crate
`include_str!`s it into the self-contained pages that `--dump-html` and `--dump-all-html`
write — pages opened from `file://` with no server behind them (the gate's `self.html` has
zero external scripts and zero modules). So a shared module cannot simply be `import`ed by
export.js; it has to reach the export INLINED, and reach the monitor SERVED.

The seam: shared modules live in the html crate — `claude-replay-html/src/html/shared/<name>.js`
— because the monitor depends on the html crate, never the reverse. Each is a plain ES module
with **no imports** and **one trailing `export { … };` line**. The html crate exposes them as
constants (`include_str!`), and:

- the monitor's `ui::asset()` serves them at `/monitor-ui/shared/<name>.js` unchanged, so
  the app shell imports them as it imports its own modules;
- the html crate's page assembly inlines them ahead of `export.js` through one deterministic
  transform — the trailing `export { a, b };` becomes
  `window.__shared = window.__shared || {}; Object.assign(window.__shared, { a, b });` and the body is wrapped in an IIFE —
  so export.js reads `window.__shared.<name>`. A unit test asserts the assembled page carries
  no `export` or `import` token; the byte gate proves the output.
- the classic pages (`rail.html`, `shell.html`) are classic `<script>`s, not modules; they get
 The rails take the same inlined block at serve time through a `{{SHARED}}` placeholder (`shared_inline_all()`), so `window.__shared` is there synchronously — a module script would race the first render (shipped this way in #43).__shared`. Module scripts run after the inline classic script, so the rail must call
  shared functions lazily — from `poll()` and handlers, never at top level — which is how it
  already runs.

Which modules qualify: the pure ones — `session-visibility`, `capabilities` (extracted from
`components.js`), `keymap`, `reading`, `view-memory`, `state-labels`, `sandbox` (app shell
only, but it belongs beside its peers), and later `record-store`. `components.js`,
`view-model.js`, `viewport.js`, `app.js` stay the app shell's: they render its DOM.

### 1. Shared modules — in this order

| step | module | replaces | risk |
|---|---|---|---|
| a | `session-visibility.js` (exists) | `rail.html` `families`/`clusterKey`/hidden filtering; `shell.html`'s | Low: pure, tested; the rails keep their row markup and call the functions. |
| b | `capabilities.js` — extract `attachmentCapability`, `referenceAction`, `revealQuery` from `components.js` | `export.js`'s inline `fsig ? openArtifact : reveal` at the tool header and in `fileview` | Medium: the first module export.js consumes through seam 0, so it proves the inliner; the byte gate diffs on every HTML output (the embedded source changes) and must be verified as "source lines only", the method used for v1.129.x. |
| c | `control-store.js` | the three compose/consent copies | Medium: three UIs, one protocol; the store already separates state from DOM. |
| d | `keymap.js` (exists) + `reading.js` (exists) | `rail.html` keydown, `export.js` `onKey`/`setMono`/`setWrap`/`setWide` | Low: tables and setters; export.js keeps its actions. |
| e | `record-stream.js` — the two-zone REDUCER (`reducePull(cursor, length, reply)` → a plan: resync / truncate / append committed / provisional truncate + append / the next cursor / `changedFrom` / `idle`, plus the `pull` and `records` query builders and `parseRecords`) | `export.js` `consumePull` and `record-store.js` `apply` become "apply the plan to my store" | **Decided (#49, 2026-09-03): one reducer, two transports.** The protocol semantics — epoch bump ⇒ resync, `committed_from` truncate + append, provisional truncate to the committed prefix + `provisional_from` then append, the cursor arithmetic, and what changed from where — live once. Each page keeps its fetch loop (export.js: interval + inflight guard + self-heal resync on a torn apply; the store: a 1 s timer), its DOM (`resetFrom`/`putBlock` vs array ops + `handlers.update`), its meta rule (both ignore an idle reply's meta, which the server sends as null), and export.js keeps its OFFLINE modes (`#session-data`, the static bundle) untouched — they never touch the reducer. Pinned by the harness on both surfaces: tail pin / unpinned hold through growth, search through growth, and a server restart with resume (#53). |
| f | `state-labels.js` — the busy/wait/idle label table | `rail.html` `gStatusTip`, `app.js` `displayState` | Trivial once (a) is in. |

Theme: one `theme.js` with `apply(dark)` that also posts into an iframe when there is one —
the rail's reframe is the only special case; `shell.html` needs nothing.

### 2. One route table in the `claude-monitor` lib

`claude_monitor::routes(cfg) -> impl Fn(&Request) -> HttpResponse` holding
`api/sessions`, `api/ignore`, `api/ui`, `api/send`, `api/consent`, the `monitor-ui/*` assets,
and the `service_routes` fallthrough; each `main.rs` keeps only its shell selection (`/` →
rail vs splice vs app), v1 its embed-defaulting `session` arm, and its port/cache defaults.
`api/ui` never gets added by hand twice again, and the two genuine differences become named
parameters of one table instead of drift between two files.

### 3. What the classic shell keeps as its own

After 1(a)–(d): `rail.html` keeps its row markup, the iframe host, the strip/collapse
behaviour and its CSS; `shell.html` keeps the splice and its rail; `export.js` keeps the
transcript rendering it has always owned (which the app shell does NOT share — it renders
records through `view-model.js`/`components.js`, and that is the one large fork this note
does not propose to close: two renderers over one record stream, each with its own tests).
Dropping the classic shell then deletes `rail.html`, `shell.html`, the splice code in v2's
`main.rs`, and nothing else.

### Migration order that keeps both shells shipping

0 → (a) → (f) → (d) → (b) → 2 → (c) → (e). The packaging seam first, proved on a module the
app shell already owns; then the pure tables and predicates (no behaviour change, the byte
gate stays green because export.js does not yet consume them); (b) is the first export.js
consumer and the first byte-gate re-baseline; the route table before compose so the shared
store has one server; the record client last because it is the only step where the classic
view's durable-cache resume and offline (`#session-data`) modes are in play. Every step keeps
`reference-*.{css,html}` generated, production chrome layered at runtime, and nothing under
`design/` carrying real session content.

### What pays off regardless of how long the classic shell lives

If the classic shell were dropped next month, most of the sharing would be deleted with it —
so the order also sorts by horizon. **Worth doing regardless:** seam 0 (it is what lets
export.js and the app shell ever share logic, classic or not), the route table (2), and
compose (c) — that protocol moves with the control plane and is written three times today.
**Worth doing only while classic lives:** (a), (d), (f), and (e) — for (e), the reducer half is worth keeping even after: it is the protocol, and the app shell's store consumes it. The owner accepted the full
list on 2026-09-03; the horizon-independent steps are the ones to keep if that changes.

### Gates per step

`ui_contract.mjs` for every shared module (they are node-loadable by construction — no
imports); a Rust test on the inliner (no `export`/`import` tokens in an assembled page, every
shared name reachable on `window.__shared`); `browser_follow.rs` for anything that scrolls,
folds or navigates (six app-shell cases exist; the classic v2 case exists; the rail has none —
add one before (c)); the byte gate for anything touching `export.js` or the inlined set,
verified line by line as "embedded source only" before re-baselining.

## Review notes (2026-09-03)

A second pass against the code before execution corrected the first draft:
- export.js is embedded into self-contained pages, so "export.js gains an import" was wrong;
  seam 0 is the mechanism.
- There is no `think` route in v2 (that grep matched a test's renderer list); both binaries
  carry the same five API arms.
- The classic view never renders HTML in a frame, so there was no "unaudited sandbox" in
  export.js; the sandbox row is now "not duplicated".
- Theme toggles: three, not four — `shell.html` has none.
