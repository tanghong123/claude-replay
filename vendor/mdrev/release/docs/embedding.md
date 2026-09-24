# One Origin, No Iframe

*mdrev · design · 2026-09-02 · decided and built*

A third-party application, written in any language, already serving Markdown over HTTP, wants mdrev's rendering inside its own pages — one page, no iframe, and nothing listening that the host does not already own. This is what that takes, what it costs, and the order to build it in.

Three words carry the whole document. The **host** is the application integrating mdrev — the one that already serves the Markdown, owns the page, the session and the routes; the host in view is written in Rust. The **bundle** is mdrev's JavaScript and CSS, which the host serves as static files and which runs entirely in the reader's browser. The **contract** is the handful of HTTP routes the host implements so the bundle can fetch texts, revisions and annotations from it.

> [!NOTE]
> **Status: built.** Everything this document proposes for its first cut — stages 1 to 5 of §8: `mdrev-bundle`, `mdrev-cli`, the contract, sanitisation with the policy, and `mdrev-v2` — is implemented, tested and shipped; this repository reviews its own documents on `mdrev-v2`. What follows is the design as it was decided, kept for its reasons. To integrate mdrev into an application, read the [developer guide](embedding-guide.md); the routes are specified in [the contract](contract.md).

> [!IMPORTANT]
> **Proposal.** Ship mdrev as a *guest*: a self-contained browser bundle the host serves as static files under its own origin, mounted into any element with one call, and a small language-neutral *document contract* — five JSON routes the host implements under a path prefix of its choosing. The rendering engine runs in the browser (it is already isomorphic, and the Obsidian port already runs it outside a server), so nothing of mdrev listens on the host; the one thing of mdrev the host machine runs is its command line, a short-lived process per note, so that notes are written by the same code the mdrev application reads them with. Server-side rendering is an optional sixth route the host can back with a proxied sidecar later, behind the same contract, without touching the page.
>
> All four constraints are met by construction: the backend integration is a JSON contract any language can implement in an afternoon; the frontend is a component in the host's page; every request goes to the host's own endpoint under the host's own session; and the security work reduces to two things — sanitising Markdown that is no longer trusted, and stating the CSP the bundle needs.
>
> **The two things a spike must prove before committing:** the viewer mounted in a page it does not own, with a scroll container it did not create; and one-time first-render cost of the engine in the browser on a large document. Both are one day.

## 1. What the viewer actually needs — measured, not assumed

The whole backend surface the browser viewer touches is one interface, `ReviewClient` in `packages/web/src/client.ts`: sixteen methods, of which ten reach the daemon over HTTP (`doc`, `commits`, `src`, `changed`, `sections`, `move-section`, `watch`, `raw`, `recents`, and the annotation CRUD) and two are optional renderers (`diagram`, `math`). Nothing else in the viewer knows a server exists.

That interface has already been implemented a second time, in-process, by the Obsidian plugin — which mounts the same `<App/>` into a container it does not own, with a stylesheet scoped under `.mdrev-host` (372 rules, by a build-time transform) and colours mapped from the host's variables. Every finding below about "mounting in a foreign page" is a finding the Obsidian port already paid for.

The engine is isomorphic by test: `isomorphic.test.ts` fails the build on any `node:` import. It is 408K on disk, seven pure-JS dependencies, and renders a 25KB document in about 8ms in node. It runs inside Electron's renderer today; a browser is the same thing.

| What the host must supply | Why | Already exists as |
|---|---|---|
| A document's text at a revision | the redline is a merge of two texts | `/api/src`, `/api/doc` |
| The revisions that touched a document | the range picker | `/api/commits` |
| A document's own assets (images) | relative references | `/api/raw` |
| Annotations: list, create, patch, reply, delete | the review loop | `/api/annotations` |
| A way to learn the document changed | live reload while an agent writes | `/api/watch` (SSE) |

Everything else — diff, alignment, rendering, anchors, the layout model, diagrams, math — is the viewer's, and needs no backend.

## 2. The constraints, as tests a design has to pass

- **Loose backend.** The host in view is Rust; the next one may be Java, Go or Python. No in-process call into a JS engine; the integration is data over HTTP, specified well enough to implement without reading mdrev's source.
- **Single page.** No iframe. The viewer is DOM inside the host's document, subject to the host's CSS cascade, its scroll container, its keyboard, its theme.
- **Same endpoint.** Nothing new listening. Every byte the browser fetches comes from the host's origin, through the host's routes, under the host's authentication.
- **Security.** Follows from the above, and adds one thing the daemon never had to face: the Markdown is no longer the reader's own files.

## 3. Three ways to do it

### A · A render sidecar behind the host's reverse proxy

Run the existing daemon core as a process on the host's machine, listening on a Unix socket, and have the host proxy `/mdrev/api/*` to it after authenticating. The host supplies blobs by calling back, or the sidecar reads the host's git checkout directly.

Loose: yes, it is HTTP. Single page: yes, with the same bundle work as any option. Same endpoint: from the browser's view, yes; on the machine, no — a second process with its own lifecycle, restart story and security boundary, which is exactly what "not starting a new endpoint" was guarding against. It also couples the sidecar to the host's storage (a git checkout it can read, or a callback API the host must also build). Best when the host wants rendering off the client for very large documents; wrong as the default.

### B · The engine in the browser, the host implements a small contract

The host serves mdrev's bundle as static files and implements five JSON routes over data it already has. The bundle, running in the reader's browser, fetches texts and revisions from those routes and does everything else — diff, render, anchors, diagrams, math — right there in the browser. No JavaScript runs on the host.

Loose: yes, maximally — the contract is five routes and a JSON schema for an annotation. Single page: yes. Same endpoint: yes, strictly — no mdrev process exists. Security: the host's session covers every request; the new work is sanitisation and a CSP statement. Cost: first render moves to the client (measured below) and range blame needs one text fetch per revision in the range.

### C · The engine inside the host process

Embed a JS runtime (QuickJS, a WASM build) in the host and call the engine in-process. Tight, not loose; a native dependency in a language the host team may not want one in; and it buys nothing B does not, since the rendering B does in the browser is the same code. Rejected.

| | A · sidecar | B · engine in browser | C · embedded runtime |
|---|---|---|---|
| Loose backend | yes | yes — five routes | no |
| Single page, no iframe | yes | yes | yes |
| Same endpoint, nothing new listening | proxied; a second process | strictly | strictly |
| Host effort | proxy + callbacks or a checkout | five routes over existing data | native dependency + bindings |
| Render cost lands on | the host machine | the reader's browser | the host process |
| Host language | any | any | whatever can host the runtime |
| Agent loop (`mdrev --notes`) | works against the sidecar | works on any clone carrying the sidecar | — |

## 4. The proposal: B, with A's one strength kept as an option

```html
<!-- host page -->
<link rel="stylesheet" href="/static/mdrev/mdrev.css">
<div id="review"></div>
<script type="module">
  import {mountMdrev} from '/static/mdrev/mdrev.js';
  mountMdrev(document.getElementById('review'), {
    contract: '/docs/api/mdrev',       // the host's prefix; every fetch goes here
    path: 'guides/setup.md',
    range: {last: 1},                  // or {from, to}; resolved against /revisions
    scrollParent: document.getElementById('main'),   // or 'page'; omit to scroll inside the element
    onNavigate: (path, range) => history.replaceState(null, '', `?path=${path}`),
  });
  // theming: set --mdrev-bg, --mdrev-ink, --mdrev-accent … anywhere above the element
</script>
```

```text
the document contract  (host implements; all under {contract})
  GET  /text?path=&rev=            → text/plain              rev: an id the host issued, or "current"
  GET  /revisions?path=            → [{rev, date, author, subject, body?}]   newest first
  GET  /asset?path=                → the file, with its media type
  GET  /annotations?path=          → [Annotation]           backed by .mdrev/annotations/<path>.jsonl
  POST /annotations?path=   PATCH /annotations/{id}   DELETE /annotations/{id}      in the checkout,
                                                            the same sidecar the mdrev app reads
  GET  /events?path=               → text/event-stream of `change` events, or 204 to say "poll" (optional)
                                     polling is GET /text?rev=current with If-None-Match; send an ETag
  POST /render                     → {html, changes, stats}                     (optional; A's job, behind B's contract)

the bundle  (host serves as static files)
  mdrev.js  mdrev.css  chunks/ (mermaid, katex, prism, lazily)  fonts/
```

### What the engine-in-browser changes, and what it does not

Nothing in the engine changes. The `HostClient` implementing `ReviewClient` calls `renderDiff(from, to)` locally where the daemon's client calls `/api/doc`; this is the same substitution the Obsidian client made, in the other direction. `doc`, `src`, `changed` and `sections` compose from `/text` and `/revisions`. Range blame — which commit introduced each change — walks adjacent pairs of revisions and needs each one's text; that is one `/text` per revision in the range, cached in the page.

### The three things a foreign page breaks, and the fix for each

*The scroll container.* The viewer owns a `.scrollpane` today; the layout model watches it and every content-pixel conversion is relative to it. In a host page the scroller is the host's, so `mountMdrev` takes `scrollParent` and the viewer measures against it. The Obsidian pane already does this; the change is to make it the parameter it should have been.

*The stylesheet.* Scoped under `.mdrev-host` with the transform the Obsidian build already has, and every colour a token with a fallback — `var(--mdrev-ink, var(--ink, #0b0b0b))` — so a host can theme it by setting variables and an unthemed host gets mdrev's own look. Fonts are served with the bundle.

*The keyboard.* Several shortcuts listen on `window` (Escape for panels and popups, `a` to annotate, `n`/`p`, `[ ]`). In a host page they must listen on the mount element and ignore events from outside it, or the host's own shortcuts collide. An audit, not a design problem.

## 5. Security, which is where "other issues" live

The daemon renders the reader's own files on the reader's own machine, and it took the liberties that allows. A host serving other people's Markdown cannot.

> [!CAUTION]
> **Raw HTML renders live today.** `<img src=x onerror=alert(1)>` in a document becomes exactly that element; `[click](javascript:alert(2))` becomes a live link. Verified by rendering both through the engine. This is correct for a local viewer and is a stored XSS in any multi-user host. The embed sanitises by default: raw HTML blocks render as the escaped code view the redline already uses for changed HTML, URL schemes are allow-listed (`http`, `https`, `mailto`, relative), and attributes pass through an allow-list. A host that trusts its authors can turn it off, explicitly. The host in view has authors who write no raw HTML, so the default costs it nothing and the opt-out is not needed.

- **Authentication.** None in mdrev. Every request is a same-origin fetch to the host's routes; the host's session applies as it does to any of its pages. The annotation author is whoever the host says it is. The host in view authenticates with a key carried in the URL, which it rewrites into a cookie — the easy case for the embed: `fetch` with `credentials: 'same-origin'` and `EventSource` both send that cookie without being told. Two consequences follow. The rewrite must happen before the page that mounts mdrev is served, so that the bundle's first fetch already carries the cookie; and mdrev puts nothing in a URL beyond `path` and `rev`, so the key can never leak into an mdrev request, a referrer, or a log line the bundle causes.
- **Path scoping.** The viewer never sees a filesystem. What `path` may name — and whether this session may read it — is decided entirely by the host's `/text` and `/asset` handlers. The daemon's own root-escape guard is not part of the embed and should not be mistaken for one.
- **CSP.** A Content-Security-Policy is a response header a site sends with its pages to tell the browser what the page may load and run — which scripts, styles, fonts, images and connections — and the browser blocks everything else. It is the standard defence against the injected script that sanitisation is meant to prevent: even if something slips through, a policy that names only the host's own files refuses to run it. The host sends none today, so nothing stands in the bundle's way and mdrev needs no host change to run. Adding one is a hardening step the host can take when it likes, and it is one header. What the bundle needs from it: `script-src 'self'` (external modules only, nothing inline), `connect-src 'self'`, `font-src 'self'`, `img-src 'self' data:`. Styles are the one demand: mermaid writes `<style>` blocks and `style=` attributes into its SVG, KaTeX writes `style=` attributes, and shiki colours tokens with inline styles — so `style-src 'self' 'unsafe-inline'`, or a nonce the host injects and mdrev applies to the styles it creates. The guest keeps shiki — in the browser, with its JavaScript regex engine (no WASM to allow) and one lazy chunk per grammar a document fences — because the viewer's stylesheet is written for shiki's output and the daemon's code blocks and the guest's are then identical; Prism, which colours by class and needs no inline styles, stays the option for a host under a strict nonce policy, which can also mount mdrev with diagrams and math off; the engine leaves both as their source text, readable. The recommended header ships with the embed as a copyable line, with a test page that mounts under it.
- **Diagrams and math.** mermaid runs at `securityLevel: 'strict'` already; KaTeX with `trust: false`. Both render in the reader's browser from text the host served — the same trust boundary as the Markdown itself.
- **Annotations.** One mechanism, shared with the mdrev application: the host stores notes in mdrev's own sidecar — `.mdrev/annotations/<path-to-doc>.md.jsonl` in the checkout it serves from, one JSON record per line, the same file the daemon and `mdrev --notes` read and write. A note written through the embed is then a note in `mdrev` on any clone that carries the file, and a note filed in `mdrev` shows in the host's page. Who writes the file: mdrev does, not the host. The host's five annotation routes do not reimplement the format — they invoke the mdrev command line, which is the same `@mdv/core` store the daemon and `mdrev --notes` use, in a headless JSON mode: `mdrev-cli notes list | add | reply | resolve | wontfix | reopen | delete --json` (§7). One implementation of the record, of the snapshot `blob`, of `rev` resolution and of the self-ignore marker; nothing for a second implementation to drift away from, which is the point. The cost is a runtime dependency on the host machine — node and mdrev installed, by `brew` or as the release tarball's `mdrev.js` — and not a listening process: each call is a short-lived process that reads or rewrites one small file and exits, tens of milliseconds, fine at the rate people file notes. The CLI already lists, replies, closes, reopens and deletes; *creating* a note is the one verb it lacks, since creation has only ever come through the daemon, so `add` is a small addition in stage 3, where the command line becomes `mdrev-cli`. A host that cannot have node on its machine ports the format instead — *list* parses the lines, *create* appends one, the rest rewrite the file under a per-file lock, an afternoon in Rust — and the conformance runner is what keeps that port honest. Three fields carry the interoperability: `rev` must be the git commit id the note was taken on — the host's revision ids therefore are git commits, or the host maps them when it writes; `anchor` is mdrev's — source offsets into the Markdown, the quote, its prefix and suffix, the heading trail, `space: 'source'`, `side`; and `blob` is a content snapshot of the document as the reviewer saw it, which lets a note be mapped *exactly* through later edits rather than searched for. The CLI writes it with the same git call the daemon uses; a ported host writes it with `git hash-object -w` if it can and omits it if it cannot — every reader falls back to quote search, which is what the exact/moved/changed/orphaned ladder does anyway. Whether the sidecar travels in git is the host's decision: mdrev's default writes a self-ignoring `.mdrev/.gitignore` because a local review is private; a host whose notes are meant to be shared deletes that marker and commits the sidecars, which is the switch the marker itself documents. Permissions — who may file, close, or delete whose note — and audit stay the host's, applied at the routes.
- **Live reload.** An SSE route if the host has push; otherwise the embed polls `/text` with `If-None-Match` every few seconds while the tab is visible. A 204 from `/events` selects polling, so a host implements push only if it wants to.

## 6. Evaluation of the proposal

### Strengths

It is the smallest thing that meets all four constraints, and most of it exists: the client interface, the in-process client shape, the scoped stylesheet, the isomorphic engine, the diagram and math seams. The host's work is five routes over data it already serves. Nothing runs on the host that did not before. And because rendering happens where the reader is, the host's cost of adding mdrev is bytes served, not CPU spent.

### Costs and risks, stated plainly

- **Bundle.** 4.9MB across 156 files today, almost all lazy — mermaid's per-diagram chunks and KaTeX's fonts. The entry is ~264K. A document with no diagram or formula never fetches them. Still, the host serves them; a CDN in front is the host's call.
- **First render on the client.** The host's documents run to hundreds of KB, so this was measured rather than extrapolated, in node on synthetic documents built from real ones: plain rendering is linear at ~0.45ms per KB (66KB in 33ms, 396KB in 180ms); the redline is not — 103ms at 66KB, 205ms at 110KB, 424ms at 198KB, 1.23s at 396KB, and the same whether or not anything changed, because the alignment step costs the same either way. The assumption that a browser would be one and a half to two times slower than node was wrong, and the spike measured it: in headless Chrome, with the engine in the guest bundle, this document (28KB) renders plain in 10ms and as a redline in 65ms; 113KB in 35/103ms; 198KB in 60/206ms; 397KB in 128/462ms — the browser's JIT does better than node's here. So a 100KB document opens as a redline in a tenth of a second and a 400KB one in under half a second, once per range change, with the page usable while it works. Acceptable at review scale — and since improved: the alignment now anchors on runs of identical blocks and searches exhaustively only between them, so an unchanged 400KB document aligns in 69ms instead of 501ms, in node, and a redline costs two parses plus its edits, in the daemon and the guest alike. The optional `/render` route is no longer needed for cost and stays only for hosts that want rendering off the client for other reasons.
- **Blame fetches.** N texts for a range of N revisions. Cached per page; a host can add an ETag. For the common range — the last change — it is two.
- **node on the host machine.** Notes are written by the mdrev CLI, so the host's machine needs node and mdrev installed and on the path of the host process. It is a runtime dependency and a deployment step, not a service: nothing listens, nothing stays resident, and if it is missing the annotation routes fail loudly while reading and rendering carry on. The port in §5 is the escape if the dependency is unacceptable.
- **No server cache.** Two readers of the same redline each render it. Acceptable at review scale; not a CDN-shaped workload.
- **The agent half.** `mdrev --notes` and `--resolve` read and write the sidecar on disk, and the host keeps its notes in that same sidecar — so an agent working in a clone that carries the file already sees the host's notes and closes them in place, with nothing new to build. The one thing it needs is the clone to have the sidecar, which is the host's commit-or-not decision in §5. A `--host URL` mode that speaks the contract instead of reading the disk stays possible for an agent with no clone; it is no longer on the path.
- **Highlighting.** shiki in the browser: the core and its JavaScript regex engine (~90K, no WASM), the two themes, then one chunk per grammar fetched the first time a document fences that language. The same colours as the daemon's, from the same themes. Prism would be smaller and class-based; it is the fallback for strict-CSP hosts, not the default.

### What it does not solve, on purpose

Permissions and who may resolve whose note — the host's rules, applied at its routes. Two writers of one sidecar at the same moment — the per-file lock in the host's route handlers, and git's ordinary merge for two clones, since the file is one record per line. Search, listing, and navigation between documents — the host has those. mdrev is the reading, reviewing and hand-off surface for one document; the page around it is the host's.

## 7. Three things, named so nothing breaks

What ships is three deliverables, not one, and each has its own name so that today's `mdrev` — the daemon, the Finder app, the Obsidian plugin, the tap — is disturbed by none of them until the last one has earned its place.

| | What it is | Who runs it | Built from |
|---|---|---|---|
| **`mdrev-cli`** | The standalone command line: notes (list, add, reply, resolve, wontfix, reopen, delete, as JSON), texts and revisions over git, no daemon and no browser | The host's machine, one short-lived process per call — and any agent in a clone | `packages/cli` minus the daemon and viewer; `@mdv/core` and `@mdv/engine` unchanged |
| **`mdrev-bundle`** | The guest: `mdrev.js`, `mdrev.css`, lazy chunks and fonts, mounted with one call into any element, engine in the browser | The reader's browser, served by the host as static files | `packages/web`, with the mount entry and the scoped stylesheet |
| **`mdrev-v2`** | The sample host: serves a git checkout's Markdown, implements the contract by calling `mdrev-cli` and `git`, serves the bundle under its own origin, mounts it in its own page | Whoever runs a host — first of all, this repository, for its own reviews | New, small, TypeScript; grows into the next mdrev |

The point of the third is that we eat what we cook. Every claim in this document — that the contract is enough, that a foreign page and scroller work, that notes written by a host are notes in `mdrev`, that the CLI is the only mdrev code a host machine needs — is a claim `mdrev-v2` either demonstrates or fails to, on this repository's documents, before any other host is asked to believe it. When `mdrev-v2` does everything the daemon does, it becomes `mdrev`; until then the names keep the two apart.

## 8. Work plan, and what landed

1. **Spike, one day, throwaway — done 2026-09-02.** A std-only Rust host (180 lines, no crates) serving its own page — header, sidebar, a main column — plus the contract over this repository's checkout by shelling out to `git`, and the guest built from `packages/web` mounted into that page. What it proved, in headless Chrome: the viewer mounts and renders inside a page it does not own (redline, change bars, rail, note chips from the sidecar the Rust host serves); **no stylesheet leaks in either direction** — the host's Georgia, colours and margins survive untouched beside the guest's own type and palette, with every rule scoped under `.mdrev-host`; the **review loop works through the host** — select, invite, compose, save, `POST /annotations` to Rust, chip appears, record in the sidecar with `rev` and a `blob` from `git hash-object -w`; and the browser render numbers in §6. What it found for stage 2: (a) library-mode vite leaves `process.env.NODE_ENV` in place and React throws before the first render — one `define` fixes it; (b) `.app` sizes itself to `100vh`, so inside a host column it overflowed and its topbar sat under the host's header — `height: 100%` of the mount puts everything where it belongs, verified; (c) five things are `position: fixed` to the viewport — the enlarge dialog (correct), and the composer, blame popup, toast and off-page notes panel (should be mount-relative); (d) library mode inlines every asset, so `mdrev.css` is 1.5MB of KaTeX fonts — they must ship as files; (e) the viewer's own pane scrolling inside the host's column works as Obsidian's does, and rail positions are content-relative so they survive the pane not being the scroller, but the rail, the sticky topbar, viewport-first diagram drawing and reveal-on-jump all assume the pane scrolls — `scrollParent` is real work, not plumbing. The client (`hostClient.ts`), the mount entry (`embed.tsx`) and the guest build (`vite.embed.config.ts`, `scripts/build-embed.mjs`) were kept; the Rust host and the page were thrown away.
2. **`mdrev-bundle`.** The guest, as a product with a name: the mount entry (`mountMdrev`, `unmount`), `HostClient` implementing `ReviewClient` over the contract with the engine in the browser, `scrollParent` plumbed through the layout model, keyboard listeners scoped to the mount, the scoped stylesheet with tokenised colours, Prism highlighting through the existing hooks. Built from the same `packages/web` source as today's viewer, shipped as static files. The daemon and Obsidian shells are untouched; both keep passing their suites.
3. **`mdrev-cli`, and the contract written down.** The standalone command line, as a product with a name: `mdrev-cli notes list | add | reply | resolve | wontfix | reopen | delete --json` over the existing store, adding the one missing verb, `add`, plus `text` and `revisions` over git so a host can delegate those too. No daemon, no viewer, no browser; it is what a host's routes call. One page of contract: routes, the annotation record and the sidecar file it lives in (path, one record per line, append on create, rewrite on change, `rev` a git commit, `blob` optional), revision ids, error shapes, the SSE-or-poll rule. Plus a conformance runner — `mdrev-conform URL` — that any host team can point at their implementation and get a pass/fail list, including a round trip: file a note through the host's routes, then read it back with `mdrev --notes` in the checkout. The reference host is stage 5.
4. **Sanitisation and CSP.** Raw HTML as escaped code by default, URL allow-list, attribute allow-list, the CSP statement with a test page that mounts under it. Tests that the XSS probes above render inert. This is the stage that makes the embed safe to ship, and it gates release.
5. **`mdrev-v2`, the sample host — and our own dogfood.** A small application that does what any integrating host does, and nothing a host would not: serves Markdown from a git checkout, implements the contract by calling `mdrev-cli` for notes and `git` for texts and revisions, serves `mdrev-bundle` as static files under its own origin, and mounts it into a page of its own. (Its page HAD a header, a navigation and a scroller of its own, which is what a host has and what the guide showed; that went when `mdrev` began opening this host by default — chrome announcing itself as a sample is a lie about what the reader is looking at, so the mount now fills the page and the guide says to copy the mount rather than the layout.) In TypeScript, so that it can grow into the next mdrev: the day it does everything today's daemon does — recents, live reload, the Finder app, `--notes` on the command line — it replaces it, and the integration story is no longer a document but the product itself. Until then it lives beside the daemon under its own name, and this repository's own reviews move onto it first, because a host that cannot host mdrev's own design reviews is not ready to host anyone else's. The Rust host reads it as the worked example; the conformance runner passes against both.
6. **Optional server render.** `POST /render` on the contract; a sidecar mode of `mdrev-cli` (`mdrev-cli serve --socket`) as its reference; the bundle prefers it when the host answers. Benchmark the crossover document size so the recommendation to hosts is a number.
7. **An agent with no clone.** `mdrev-cli notes --host URL`, speaking the contract instead of reading the sidecar, only if an agent ever has to act on a host's notes without a checkout of them. With the sidecar shared, an agent in a clone needs nothing.

Stages 1–5 are the deliverable — three named things, `mdrev-cli`, `mdrev-bundle` and `mdrev-v2`, none of which disturbs the `mdrev` people use today; 6 and 7 follow demand. **Status, 2026-09-02: stages 1–5 have landed** — the spike (`65db024`), the bundle (`5dfb1a6`…`e74f9c7`), `mdrev-cli` with the contract and the conformance runner (`17a1ba5`, [contract.md](contract.md)), sanitisation and the policy (`06ff555`), and `mdrev-v2` (`bdd5dca`…), which conforms 14 of 14 and serves this repository's own reviews. Two things the dogfood found and fixed on the way: a modified paragraph's runs carried no source stamps, so a note inside one landed beside the heading above (`30295d3`); and a diagram added or removed whole rendered as source rather than drawn. Each stage ships behind its own tests, and stage 4 is not skippable: a bundle without it is a local-files viewer wearing a host's session.

## 9. What the host team answered, and what each answer decides

These were the questions put before the spike; the answers are in, and each one has been folded into the sections above. They are kept here so the decisions stay traceable.

- **Does the host have revision history for its documents?** Yes. So the redline is real from the first cut, and `/revisions` is a route over data the host has, not a stub. (Without history, mdrev would have been a reader with annotations.)
- **How are readers authenticated?** A key carried in the URL, which the host rewrites into a cookie. Decided: nothing to build — same-origin fetches and `EventSource` carry the cookie. Two conditions on the host, in §5: the rewrite happens before the page that mounts mdrev is served, and mdrev never places the key in any URL of its own.
- **What is the host's CSP today?** None; the host can carry a change if it is not intrusive. Decided: ship without one — mdrev needs no host change to run — and hand the host the one-line recommended header as an optional hardening step. Diagrams and math ship in the first cut. (What a CSP is: §5.)
- **Do authors write raw HTML that must render?** No. Decided: the sanitised default stands and the opt-out is not built.
- **Largest document?** Hundreds of KB. Decided: measured in §6 — a 100KB redline well under half a second in the browser, 400KB two to three seconds; acceptable, and the alignment cost that makes it superlinear is filed against the engine. Concurrent reader count was not asked back; with rendering on the client it only affects bytes served.
- **Annotations?** The host needs them, and they must use the same mechanism as mdrev itself, so that a note written through the host is visible in the mdrev application and the other way round. That rules out a store of the host's own choosing — the first draft of this document had the host keeping the records in any shape it liked — and decides the storage outright: notes live in mdrev's sidecar, in the checkout the host serves the documents from, and mdrev's own command line writes them, invoked by the host's annotation routes, so there is one implementation of the format rather than a port that can drift. §5 has the format, the rules and the cost (node on the host machine); the consequence for the agent loop is in §6 and §8 — `mdrev --notes` and `--resolve` work on any clone that carries the sidecar, so no host-aware CLI mode is needed.
