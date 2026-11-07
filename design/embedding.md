# Embedding the session view in another web app

> **DRAFT** (#s406, 2026-11-07). A proposal; nothing is built. The owner asked for agent-monitor
> to be embeddable in another web app "natively (not as an iframe)", a team's collaboration app
> first, design first. The decision it asks for is at the top. Its companion is #s407's design (one
> app over many paired monitors), which owns the shared first stage named below.

## The decision

**What it is for.** A host web app shows an agent's session inside its own page, drawn exactly
as agent-monitor draws it, kept current with agent-monitor's releases, with no iframe.

**Options**, each judged against that goal ("native, and no drift"). Details are under
*The options, judged* below.

1. **A published embedding kit, and the host keeps a same-origin iframe.** Every release ships the
   renderer, the projection, the stylesheets, type declarations and a conformance check. No change
   to how the view treats the page. It fixes drift and the unwritten contract, but keeps the frame.
2. **The kit, plus a session view that mounts in a host element** (stage 1), **then the
   long-session engine inside it** (stage 2). The view lives in a Shadow DOM inside the host's own
   DOM, fed records by the host, with nothing leaking either way.
3. **The whole app shell as a custom element the monitor serves.** It is the tool for #s407's
   hub, which talks to monitors. A host that never talks to a monitor cannot load code from a
   person's loopback, so it does not serve this goal.

**Recommended: 2**, stage 1 first. **Default: 2**: stage 1 starts when this is agreed, stage 2
waits on stage 1 being used by a host. **What waits on it:** any code for #s406, and the shape of
the data source that #s407 shares.

**A sub-decision: which piece first.** The session view only (one session's transcript, its folds
and its controls) fits a host's side pane, about 380px wide. The session list and the outline
panes come later, if a host asks. The first host shows one session at a time, so the session view
comes first.

**What is a contract here** (the owner's to approve, per the repo's rule): the mount API, the
record format a host feeds in, the theme tokens, and the kit's layout and version. None of it
touches auth, pairing or the network: a host that feeds its own records opens no path to any
monitor.

## The problem

A team's collaboration app already draws its agents' work with agent-monitor's renderer. It took
a copy of the renderer and both stylesheets, and feeds the renderer records it builds from its own
agent streams. It never talks to a monitor. It pays four costs for doing it that way:

- **Drift.** The copy is many releases behind. Each refresh is a manual copy, an import walk to
  find new files, and a check of what changed.
- **A frame.** The stylesheets assume they own the page (`:root`, `html`, `body`, `#app`), so
  the host draws the view in an iframe, moving nodes into it, only to keep the styles apart.
- **An unwritten contract.** Which state slots and actions the renderer needs is not written
  anywhere. The host finds them with a test that reads the renderer's source as text. A missing
  one shows up as a button that does nothing.
- **No long-session engine.** Only the renderer was taken, not the engine that keeps a long
  session readable (the virtual window, #192–#197), so a long stream is drawn whole.

The owner's ask removes the frame. The design below also removes the other three costs, since a
native view without them would only move the problem.

## A precedent in this repo: mdrev

agent-monitor already embeds another product natively: mdrev's viewer is a guest in the preview
pane (#270, `design/mdrev-in-the-preview-pane.md`). The same approach applies in reverse here:

- mdrev publishes an **embedding kit** with each release: the bundle, its CLI and its docs, with
  a published digest. agent-monitor pins one release by a script (`scripts/vendor-mdrev.sh`), checks
  the digest, and refuses versions below a floor. Upgrades are explicit.
- The guest's styles are scoped under its own host class (`.mdrev-host`), and nothing of the
  monitor's reaches it.
- **Keys follow engagement.** The guest answers keys only while the reader last clicked or focused
  inside it (`[data-guest-keys]`); otherwise the host's keymap has them.
- **A conformance check is the definition of done.** `mdrev-cli conform` against the host proves
  the contract, so neither side reads the other's source to know it.

This design gives agent-monitor's session view the same four things.

## The shape: a data source, and (for monitors only) a backend handle

Agreed with #s407 so both designs build on one seam:

- **The data source** is what a view consumes: the session index rows, one session's records (a
  snapshot, then growth), the turn index, search, files, and writes as optional capabilities
  (send a prompt, answer a dialog, grant a pane). It has three implementations:
  - **MonitorSource**: today's behaviour, over HTTP to one monitor.
  - **MergedSource**: #s407's: rows from many monitors, each tagged with its device, each
    per-session call routed to the row's own monitor.
  - **HostSource**: a host that pushes records itself and declares which capabilities it has.
    For the first host: none of the writes, and no files.
- **The backend handle** is how MonitorSource reaches one monitor: its base address, how
  credentials ride (the ambient cookie, or a bearer token), whether the reader is local to it
  (reveal or download, windowed reading), and where to load mdrev's bundle from. Routing every
  request through it is **stage 0**, written up in #s407's design. This design only cites it:
  HostSource never uses a handle.

### What a host feeds in

A view is driven by one call today: the projection is rebuilt from the session's records from the
first index that changed (`Projection.rebuild(records, changedFrom)` in `view-model.js`). The
records are the ones the monitor serves at `/records` and `/pull`, the html crate's JSON records.
So HostSource is:

- `records`: the session's records in order, in that format.
- An update that names `changedFrom`, the first index rewritten. Usually it is the old length
  (an append). It is lower when earlier records were rewritten: a queued prompt picked up shifts
  the markers behind it (#165), and an attachment arriving after its prompt rewinds to the prompt.
  A host that only ever appends always passes the old length.
- Optionally, metadata the views read (the session's title and agent) and the capabilities.

That record format is today a private agreement between the monitor's server and its own page.
**Embedding makes it a contract**, which is why this goes to the owner:

- It gets a format version, carried in the kit. A change that only adds a field keeps the
  version; a change that removes or reinterprets one moves it, and the kit says which versions it
  reads.
- The kit documents every field the views read, record kind by record kind.
- The conformance check runs a host's adapter output through the real projection and renderer,
  and fails on a record kind or field the views do not know. It also lists the state slots and
  actions the renderer uses, from the code, so no host reads our source to find them.

`agent-replay --dump - --json` is a different stream (the block stream for programs, #34), and is
not this format. A host that starts from a transcript file still feeds the page format.

## Stage 1: the session view in a host element

The host imports the kit's module and mounts the view into an element it owns:

```js
import { mountSession } from "./agent-monitor-embed/embed.js";

const view = mountSession(element, {
  source,                 // a HostSource (or a MonitorSource, for a host that has a monitor)
  theme: "dark",          // or "light"; the host can also override tokens with CSS variables
  layout: "auto",         // "auto" (by the element's width), "compact" or "wide"
  pointer: "auto",        // "coarse" on a touch device; "auto" asks the real device
  prefs,                  // optional { get(key), set(key, value) } for reading preferences
  linkFor,                // optional: the host's link for a record, for "copy link"
  on: { openFile, openSession, toast },   // optional handlers; absent means "not offered"
});
view.update({ theme: "light" });
view.unmount();
```

`mountSession` returns a handle whose `unmount()` removes every node, listener, observer and timer
it made. Two views can be mounted at once.

What changes inside the view, so it stops assuming it owns the page:

- **Styles live in a Shadow DOM** attached to the host element. The host's CSS cannot reach a
  record, and ours cannot reach the host. The rules that target the whole page (`:root`, `html`,
  `body`, the global `*` reset) are rewritten for the shadow root: `:root` becomes `:host`, and
  the `html` and `body` rules move to `:host` or are dropped. The `#app` rules keep working,
  because ids are scoped to their shadow root.
- **The generated stylesheet is not hand-edited.** `reference.css` is extracted byte for byte from
  the demo by `scripts/extract-agent-monitor-demo.mjs`. The extractor gains a second output, the
  scoped variant, made by a fixed rewrite of those selectors. Its two byte-identity tests stay,
  and a third checks the rewrite. `production.css` is ours, so it is written once with selectors
  that serve both the page and a shadow root, or built into the two variants the same way.
- **The theme** is an attribute on the host element, not on `document.documentElement`. The kit
  documents the colour tokens (`--bg`, `--ink`, `--faint`, `--green` and the rest) as part of the
  contract. A host may set them on its element, and they inherit into the shadow root.
- **Layout follows the element, not the window.** The phone rules today answer a viewport query
  (`PHONE_QUERY`: width 760px or less, or a touch screen held sideways). Inside a host, a 380px
  side pane on a desktop is not a phone, and a touch screen cannot be asked by a container query.
  So:
  - The reading layout (the margins, the wrapped outputs, the compact heads) follows the
    element's width through container queries, and `layout` can force it.
  - Behaviour that is about touch (no hover reveals, 44px targets) follows `pointer`, which the
    host sets or the kit reads from the device.
  - **The view never draws phone chrome inside a host:** no dock, no drawer, no handle, no
    safe-area padding. Those belong to whoever owns the screen, and in a host that is the host.
  - The monitor's own page keeps its phone layout exactly as it is.
- **No page-level listeners.** The view listens on its own element and its shadow root. A scroll
  that needs the window today scrolls the view's own element.
- **Overlays stay inside.** Menus, popovers, toasts, the copy field and download links are
  appended to a layer inside the shadow root, never to `document.body`. The host element sets
  `position: relative` and contains them. Nothing uses `position: fixed`, which a transformed
  ancestor in the host would break.
- **Focus** is read from the shadow root (`shadowRoot.activeElement`), since `document.activeElement`
  is the host element from outside.
- **Keys follow engagement**, as mdrev's do in our page: the view answers its keys (`/`, `j`, `k`,
  `[`, `]` and the rest) only while the reader's last click or focus was inside it, and never
  claims ⌘K. Outside that, the host's keys are the host's.
- **The URL is the host's.** The view never reads or writes `location` or `history`. The session
  and the record to open come in through the API; a link to copy comes from `linkFor`, and opening
  another session or a file goes out through the handlers.
- **Storage is the host's.** Reading preferences (the text size, folds, filters) go through
  `prefs`, or default to keys under one embed prefix. The view never reads the monitor page's own
  keys (`am-prod-*`, `am-demo-*`).
- **What a HostSource does not offer is not drawn.** No send capability: no compose box. No
  files: a path is shown as text, as the first host has it today. No `openFile` handler: no file
  click.

The monitor's own page becomes **one host of the same view**: it mounts the session view in its
transcript column with a MonitorSource. That is what keeps the embed from drifting: there is one
view, and the page's 534 browser cases keep exercising it. The page keeps everything around the
view (the session list, the outline panes, the preview pane, the dock) as it is today.

## Stage 2: long sessions

Stage 1 gives a host the renderer it has today, framed natively. Stage 2 brings in the virtual
window engine (the shell's `viewport.js` and the shared `virtual-window.js`, which the classic page
uses too), so a long session stays readable. That engine is where most of the work is: about 2,560
lines that assume the window scrolls, and a change there reaches the classic page as well.

- The engine scrolls the view's own element, and its observers watch the shadow root.
- A HostSource that holds a long stream can serve records in pieces, through the same calls
  the monitor's record store uses (a tail first, the head behind it). That is optional: a host that
  hands over the whole array still works.
- The engine's invariants and its trace are unchanged, and the sandbox replays (#197) run against
  the mounted view as they do against the page.

## Delivery: the kit

Every release publishes `agent-monitor-embed-<version>.tar.gz` beside its binaries, with its
SHA-256 digest, as mdrev publishes its own kit. It holds:

- `embed.js`, one ES module with no imports outside itself: the mount, the renderer, the
  projection and (from stage 2) the engine;
- `embed.css`, the scoped stylesheets, loaded into the shadow root by the mount;
- `embed.d.ts`, types for the mount API, the data source, the record format and the
  handlers;
- `docs/`: the contract (mount API, record format and version, tokens, capabilities) and the
  changes since the previous kit;
- `conform`, the conformance check, a Node script a host runs against its adapter.

A host pins a version and refreshes by a script like ours for mdrev: download the release's kit,
check the digest, refuse a version below the floor it knows. It takes no copy of our source tree
and walks no imports.

A host that talks to monitors (#s407's hub) does not need the kit: it serves the shell itself,
and the shell mounts the same view.

## Security

HostSource opens nothing: no monitor is contacted, no token held, no origin trusted. The host's
records are drawn the way the page draws them, escaped by the same renderer, so a record cannot
inject script.

A host that wants a real monitor's sessions uses MonitorSource through a backend handle. That
path, and every question it raises (CORS, preflight, a relay, trusted origins), is #s407's. This
design adds none. One fact from the research belongs here so nobody widens it by accident:
`/api/ignore` is a GET that changes state and does not check the origin, so it must never become
reachable cross-site.

## Testing

- **The existing suite keeps running unchanged**, since the page becomes a host of the same view.
- **One new browser case per stage, on a fixture host page** that is hostile on purpose:
  - global CSS that restyles `*`, `pre`, `button` and `:root` variables;
  - bare-letter keys and ⌘K of its own;
  - a transformed ancestor;
  - a narrow container on a wide window, and a wide one on a narrow window.

  It checks isolation both ways: no host rule reaches a record, no view rule reaches the host,
  overlays stay inside, keys go to whoever is engaged, and `unmount()` leaves no listener behind.
- **The rendering audit (#174)** runs against the mounted view in the fixture host, which measures
  "did any host rule reach a record" over every element and property.
- **`conform`** runs in CI against a fixture adapter, so the kit's contract and the views cannot
  drift apart.
- **A walk on the iOS Simulator** of the fixture host on a phone: the view in a narrow pane, touch
  behaviour, no phone chrome.

## What a host would do to adopt

This is the host lead's to decide and schedule in the host's own repository. In outline:

- replace its copy and its frame with the kit and `mountSession`;
- keep its adapter that turns its streams into records, now checked by `conform` rather than by
  reading our source;
- pass its theme and, if it wants, its token values;
- route "copy link", "open session" and toasts through the handlers.

Its own content rules (what it shows and hides) stay its own. For example, a host that never
emits thinking records shows none.

## The options, judged

| | Removes the frame | Removes drift | Writes the contract down | Long sessions | Cost |
|---|---|---|---|---|---|
| 1. Kit, host keeps a same-origin iframe | no | yes | yes | no | small: a release artifact, docs, `conform` |
| 2. Kit + mountable view (stage 1, then 2) | yes | yes | yes | at stage 2 | stage 1: medium; stage 2: large |
| 3. Whole shell as a custom element | yes | yes, for hosts with a monitor | partly | yes | largest; needs a monitor, so not for this host |

- **Option 1 is the honest baseline.** It changes nothing in how the view treats the page, and
  still removes two of the four costs. A same-origin iframe the host fills itself works today.
  The cookie problem the research found applies only to an iframe that points at a monitor's
  origin, which this host does not use. It is not what the owner asked for.
- **Option 2's stage 1** means pulling the transcript view out of `app.js`, which today runs
  everything at load with no mount function. That is the main cost. Stage 2 is a second, larger
  piece on its own.
- **Option 3** turns all of `app.js` into a mounted element, with its session list, panes and dock:
  re-scoping every page listener, the URL, storage, the safe areas and the fixed elements. That
  is the right work for #s407 if its hub needs it, not for a host that feeds its own records.

## Open questions

- **Selectors that serve both a page and a shadow root.** Whether `production.css` can be written
  once, with `:host()` beside `:root`, or must be built into two files, needs measuring in the
  browsers we support.
- **Where the kit is published for an internal host:** the public GitHub release, the corp mirror,
  or both. The kit holds only what the public repo holds, so publishing it publicly is safe.
- **The record format's version** may be best tied to `FOLD_VERSION`, which already moves when what
  a block carries changes, or kept as a number of its own.

## Appendix: what the page assumes today

From the research for this design, on main at v1.487.0. These are the things stage 1 must re-scope
in the part of the code the session view uses:

- **Boot.** `app.js` (4,877 lines) runs at load with no mount function or teardown, and
  `state.js` reads storage at import.
- **Ids.** 282 `getElementById` calls, through five copies of a `byId` helper, and 29 `document.querySelector`
  calls.
- **Listeners.** About 16 on `document` and about 27 on `window` (resize, popstate, scroll,
  pagehide, storage).
- **Portals.** About 8 elements appended to `document.body`.
- **Focus.** 15 reads of `document.activeElement`.
- **The URL.** About 25 uses of `location` and `history`, and debug parameters read from the
  URL.
- **Storage.** About 20 keys in `localStorage`, under `am-demo-*` and `am-prod-*`.
- **Page-wide CSS.**
  - 82 `:root` selectors in `reference.css`, plus `html, body` and a global `*` rule;
  - 377 `#app` selectors in `production.css`;
  - 59 safe-area rules, 30 `position: fixed` rules and 33 uses of `100vh` or `100dvh`;
  - 90 `@media` rules (20 of them the phone query) and 9 `matchMedia` calls;
  - no container queries.
- **The theme.** Set on `document.documentElement`, with CSS variables written on the root
  element from script.
- **Server paths.** 74 written from the root across 17 files (stage 0's concern, not this
  design's).
