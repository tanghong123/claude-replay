# Each component lays itself out by its pane

> **DRAFT 3.1** (#s431, 2026-11-12). Step 6 of `design/embedding.md` ("Layout decides itself", "The
> order of the work"). Not built. The owner's ruling (#s459): start it now, and fix in it the
> transcript column that the right pane squeezes to 126px just past 1180px. The owner's notes on the
> width-study videos may still change it; every number they can touch is in one table.
> Two decisions are the owner's, and only stages 2 and 3 wait on them: how a component counts its
> pane's width (the host says what it has beside it, or not), and which gives way first when the
> right pane leaves a short line (the pane or the outline). Stage 1 waits on neither.

## What it is for

A host mounts the session view, and the session list, in a pane of its own: a column of the
collaboration app, half an iPad's screen, a window of the Mac app beside another. Today both
components choose their layout from the size of the WINDOW, so in a host's 380px column on a wide
desktop the view draws its widest layout and overflows. This step makes each component choose from
the size of the PANE it is mounted in, and from the device, as `design/embedding.md` sets out:

- in a wide pane on a desktop, the transcript, the outline and the artifact pane side by side;
- in a narrower pane, the artifact pane over the transcript, and the outline giving way;
- in a narrow pane (a side column, about 380px), the transcript alone, the rest one tap away;
- on a phone, the phone reader the app has today.

The app is a host like any other, so it must look and behave as it does now. The exceptions are the
126px column, which the owner asked to fix, and the few listed under "What the app changes".

## What asks the window today

Measured on main at 34c7eb50.

- **Styles.** `reference.css` (generated from the demo) and `production.css` (ours) carry 90
  `@media` blocks, 45 each; the three derived stylesheets carry 116 (`session-view.css` 79,
  `app-shell.css` 28, `session-list.css` 9). Sixteen distinct conditions read a size. They fall
  into a few bands:

  | Window band | What changes there | Whose |
  |---|---|---|
  | `≤1180` | The right pane leaves the grid and lies over the whole session area (the app's rule at 761–1180 sets its left edge at the sidebar); the folded outline's rail narrows from 68 to 62px; the search box, the session's frame and the turn dock tighten | the grid and the overlay's placement the app's; the rest the view's |
  | `≤900` | The outline becomes an overlay with a toggle; the title, the search box and the actions shrink | the view's |
  | `≤760`, or a finger on a short side (`PHONE_QUERY`) | The phone: the list a drawer, the one-row top bar, the dock, the sheets, 44px targets | the drawer the app's; the reader the view's; 44px rows the list's |
  | `≤700`, `≤620`, `≤560` | Narrower phones: the search box an icon, denser cards, fewer turn controls | the view's |
  | `≥761`, `≥901`, `≥1181`, `701–900` and the like | The complements: the desktop compose box, the outline's 290px column, the search box's focused width | the view's |
  | a finger on a short side (`(pointer:coarse) and (max-height:500px)`), and that at `≥900` wide (`WIDE_PHONE_QUERY`) | A phone held sideways: a reading column, popovers beside their controls; a wide one shows the list as a 280px column | the view's; the column the app's, which also lays out the view's parts beside it |

  About 30 rules in `app-shell.css` reach into the view: they style the view's parts under the
  app's state and the window's media (`.app:not(.preview-off)>…>.preview`, `.preview-full`, the wide
  phone's column, which also splits the session at `50%` and measures from `var(--sidebar)`).
  Fifty-one rules in `session-view.css` size themselves in window units (`36vw`, `45vw`,
  `calc(100vw - 46px)`, `74vh`, and seven phone rules' `100dvh` or `70dvh`), seventeen of them
  outside any size `@media`. Three more measure from the app's sidebar (`var(--sidebar)`): the turn
  menu's left edge, the turn dock's menu and the transcript panel.
- **The grid.** `#sessionView` and `#sessionList` are `display: contents` in the app, so neither has
  a size to measure or to query. The view's parts are items of `#app`'s grid: the list, the session
  (`minmax(480px,1fr)`), the resizer and the right pane. At `≤1180` the grid drops to the list and
  the session (`minmax(0,1fr)`, no floor), except on the rail: its `64px` rule comes later and
  keeps four tracks, so an empty 421px column lies under the open pane there. The app copies the
  pane's open, full and resizing states onto `#app` to drive the grid.
- **Scripts.** `matchMedia(PHONE_QUERY)` in `app.js` (2, and `WIDE_PHONE_QUERY` once),
  `session-view.js` (5, and the wide query once), `session-list-view.js` (1) and `view-state.js` (the
  outline's default, read before the first render); `onPhone()` in `preview.js`. Window sizes in the
  right pane's resize drag (`innerWidth - clientX`), the task popover, the section menu, the filter
  sheet, the rail flyout and `native-menu.js`.
- **Native mode** (`codex-workspace/monitor.css`, `native-entry.css`, `monitor-controls.js`,
  `project-picker.js`) reads the same queries and `innerWidth` for its composer and pickers. It is
  the standalone app's, never a host's.

## The 126px column

With the right pane open, above 1180px the app's grid gives the session `max(480, W − 721)` px
(sidebar 300, resizer 1, pane 420). The outline takes 290 of it. The transcript's text
(`.transcript-inner`) is the rest, less 64px of margin:

| Window | Session | Transcript | Text |
|---|---|---|---|
| 1181 | 480 (the floor) | 190 | **126** |
| 1280 | 559 | 269 | **205** |
| 1440 | 719 | 429 | **365** |

These are the owner's three measurements, to the pixel. At 1181 the grid's tracks add up to 1201px
in a 1181px window, so the last 20px of the pane are cut off. By the same arithmetic, the text
reaches its 820px measure only from 1895px.

## Decisions

### One effective width

Today's layout is a set of conditions on the window's width. Each component keeps those conditions
exactly and reads them from one number instead: its EFFECTIVE WIDTH, `E`, the width of the window
the app would need to give the component this pane.

**Recommended: the host says what it has beside the pane.** `E` is the pane's width plus
`--pane-beside`, a length the host may set on the component's element: 300px (the app's default
sidebar) unless it does. The app sets it, in its own stylesheet, to the first track of its grid in
every state: the sidebar's width, 64px on the rail, 0 under a phone's drawer, and the column beside
the session on a wide phone held sideways. So in the app `E` is always the window's width, and
every band reads the number it reads today, at every sidebar width, on the rail and on a phone.

- **The phone family** is `E ≤ 760`, or a screen that is a phone held sideways (a coarse pointer and
  a short side of at most 500px). In the app that is `PHONE_QUERY` exactly, so native mode, which
  keeps its own queries, always agrees with the view.
- **In a host:** a 380px column on a desktop is `E` 680, the phone reader drawn in the pane, which
  is the narrow layout `design/embedding.md` asks for; a 700px pane is a desktop window of 1000 (the
  outline gives way and the right pane lies over the session); a 1100px pane is a window of 1400; a
  450px pane on a phone held sideways is `E` 750, laid out by its pane rather than by the screen.

This adds to what a host may rely on: one CSS property, optional, with a default. The view
registers it (`@property --pane-beside { syntax: '<length>'; inherits: false; initial-value: 300px
}`) so that it computes to a length the module can read: the wide phone's
`calc(var(--sidebar) + env(safe-area-inset-left,0px))` would otherwise come back as that text.
A registration holds for the whole page, which is part of the contract too. So it waits for the
owner's yes (#s461). **The alternative with no contract change** (draft 2's): on a phone screen
`E` is the window; elsewhere a pane of 460px or less is the phone family with `E` the pane; else
`E` is the pane plus 300. It differs in three ways:

- a host's pane on a phone screen is laid out as the whole screen, against `design/embedding.md`
  ("Every rule that today asks the window's size asks the pane's instead");
- the app's bands move with a resized sidebar and with the rail (on the rail at a 1000px window the
  view is laid out as a 1236px one);
- the app must cap its sidebar at the window less 461px, or a 761–980px window with a wide sidebar
  would turn the view into the phone reader beside a desktop sidebar. That shrinks a wide sidebar
  in a narrow window, a change to the app.

### Units and lengths

- **Width units** in a component rule become `E` units: `36vw` is `calc(var(--pl-vw) * 36)`, where
  `--pl-vw` is `E / 100`. In the app that is `1vw`.
- **Height units** (`vh`, `dvh`) become `--pl-vh` and `--pl-dvh`. Where the pane is as tall as the
  window, which is always in the app, those are the literal `1vh` and `1dvh`, so iPhone Safari's
  difference between the two, as its bars show and hide, is kept. Elsewhere both are the pane's
  height over 100.
- **The three lengths measured from the app's sidebar** are written against the pane:
  `calc(100vw - var(--sidebar) - 46px)` is `calc(var(--pl-w) - 46px)`, with `--pl-w` the pane's
  width, and the turn menu's `left: calc(var(--sidebar) + 18px)` is `calc(var(--pane-x) + 18px)`,
  with `--pane-x` the pane's left edge in the window (until stage 4 draws the menu inside the
  pane). In the app these are the same numbers, except on the rail (below).

### One decision, read by styles and scripts

A container query lets styles ask the pane, but a script needs the answer before the first render
(the outline's default), and "the pane plus what the host has beside it" is no container condition.
So one module decides (`pane-layout.js`), and everything reads its answer.

- **It measures.** It reads the component's box at mount, synchronously before the first render,
  and again through a `ResizeObserver`, with `--pane-beside` from the element's computed style. It
  hears the device through `matchMedia` listeners, and the reader's text size (`--code-size`),
  chosen pane width (`--preview`) and outline choice (`navigatorOpen`) from the view.
- **It writes classes and sizes** on the component's element: the family (`pl-phone`), one class per
  condition the stylesheets use (below), the readable line's results (stage 3), and `--pl-e`,
  `--pl-w`, `--pl-h`, `--pl-vw`, `--pl-vh`, `--pl-dvh`, `--pane-x`, `--pane-y` and `--preview-fit`.
- **Scripts ask it**: `layout.phone()`, and its change events. No component script reads
  `PHONE_QUERY` itself; native mode, the app's, keeps its own reads.
- **After every decision it calls `paneLayoutChanged`**, which remeasures the transcript only when
  its width changed. A band moving, the outline folding and the pane narrowing all change it without
  a resize of the transcript's own making.
- **The widths it computes with** (the outline's 290px and its 68px rail, the transcript's 64px of
  margin, the session's 480px floor, the pane's 340px minimum) are tokens on the view's element
  (`--pl-outline: 290px` and the like), which the grid and the module both read. They are never read
  off what is drawn: a folded outline draws no 290px column. A contract case holds them to the
  rules that use them.

### The rewrite of the styles

`scripts/session-view-css.mjs` rewrites every component rule under a `@media` that has a WIDTH
feature into the same rule gated by classes, feature by feature:

- a width feature reads `E`: `(max-width:1180px)` is `pl-max-w-1180`, `(min-width:761px)` is
  `pl-min-w-761`;
- the other features in the same query read the screen, as today, and the module mirrors each as a
  class: `(max-height:500px)` is `pl-max-h-500`, `(min-height:501px)` is `pl-min-h-501`,
  `(pointer:coarse)` is `pl-coarse`, `(pointer:fine)` is `pl-fine`;
- a query list is a `:where()` list on the component's element, so weight and source order are
  what they were.

| Rule today | Rewritten |
|---|---|
| `@media(max-width:1180px){:where(#sessionView) .x{…}}` | `:where(#sessionView.pl-max-w-1180) .x{…}` |
| `@media(max-width:1180px){#sessionView .x{…}}` | `#sessionView:where(.pl-max-w-1180) .x{…}` |
| `@media(max-width:1180px){#sessionView:is(*,div):not([data-shared-codex]) .x{…}}` | `#sessionView:where(.pl-max-w-1180):is(*,div):not([data-shared-codex]) .x{…}` |
| `@media(min-width:761px) and (min-height:501px),(min-width:761px) and (pointer:fine)` | `:where(.pl-min-w-761.pl-min-h-501, .pl-min-w-761.pl-fine)` |
| `@media(min-width:701px) and (max-width:900px) and (min-height:501px),(…) and (pointer:fine)` | `:where(.pl-min-w-701.pl-max-w-900.pl-min-h-501, .pl-min-w-701.pl-max-w-900.pl-fine)` |
| `@media(max-width:760px),(pointer:coarse) and (max-height:500px)` | `:where(.pl-max-w-760, .pl-coarse.pl-max-h-500)` |
| `:root{--navigator:…}` inside a band | `:where(#sessionView.pl-max-w-1180).session-view{…}` |

- **Exact by construction.** The 701–900 rules still apply on a tall 701–760 window, as they do
  today, because each feature is carried over and none is replaced by "not a phone".
- **What stays `@media`**: a query with no width feature (`hover`, `pointer`, `prefers-*`, and
  `(pointer:coarse) and (max-height:500px)` alone), and everything in `reference.css`, the demo,
  `app-shell.css` and native mode's sheets.
- **Checks.** A feature missing from the table, a nested group or an `@supports` fails the
  derivation (none exist today). Every rewritten selector weighs what it did, as the script already
  asserts for the selectors it moves. The contract case holds the classes the derivation writes to
  the set the module sets.
- **`ui.rs`.** `every_media_query_agrees_on_what_a_phone_is` reads the class form too.
  Its count of wide-window complements (`wides >= 8`, 11 today) drops to the four left in the app's
  and native mode's sheets, and its floor with it.
- **Tokens.** The view declares the tokens it reads that only `#app` declared: `--phone-dock-h` and
  `--phone-glass`, under `pl-phone`. In a host's narrow pane `calc(var(--phone-dock-h) + 16px)`
  would otherwise be invalid. `--preview` is NOT declared on the view: `preview.js` writes the
  reader's width on the root and a case writes one on `#app`, and a declaration on the view would
  shadow both. Rules read `var(--preview-fit, var(--preview, 420px))`.

### The right pane needs a readable line

The right pane is a column only while the transcript beside it keeps a readable line: `READABLE`,
320px of text at the default text size, scaled with `--code-size`. That is about 45 characters,
the low end of a readable line. It is decided from the reader's inputs, never from what was drawn,
so it cannot feed back on itself: the component's width, the chosen pane width, whether the reader
wants the outline, and the text size.

**Recommended: the pane gives way first** (the owner's decision, #s462). Above `E` 1180:

1. The pane narrows from the reader's width toward its 340px minimum, until the text beside the
   outline has `READABLE` and the session its 480px floor.
2. If 340px is not enough, the outline folds to its rail, and the pane takes back as much of the
   reader's width as the line and the floor allow.

The alternative folds the outline first, then narrows the pane. Pane first keeps everything the
reader opened on screen 80px further down (to 1315px at the defaults) and gives the drag a clear
limit; outline first keeps the pane at the reader's width. At the default sidebar, the pane at its
default 420px:

| Window | Today | Pane first | Outline first |
|---|---|---|---|
| 1181–1200 | text 126, the pane cut off by up to 20px | outline folded, pane 400–419: text 348 | the same |
| 1201–1314 | text 126–239 | outline folded, pane 420: text 348–461 | the same |
| 1315–1394 | text 240–319 | outline shown, pane 340–419: text 320 | outline folded, pane 420: text 462–541 |
| from 1395 | text 320 and up | the same | the same |

In general the rule acts below a window of 655 + the pane's width + `READABLE` at the default
sidebar: 1395 at the defaults, 1655 with a 680px pane, wider with a larger text size.

**The pane is a column only while the component is at least 821px wide** (the session's 480px
floor, the resizer, the pane's 340px minimum); below that, or at `E` 1180 and under, it lies over
the session, as today below the 1180 line, its left edge at `--pane-x` plus 10px. At the default
sidebar the two lines meet: `E` over 1180 is a component over 880px. A wider sidebar (or a host's
`--pane-beside` over 300px) parts them. At a 1200px window with a 520px sidebar, `E` is 1200 but
the component is 680px, so the pane lies over the session, where today the grid runs 221px past
the window. Where the pane is a column, the outline folded and the pane at 340px leave at least
348px of text; a text size whose `READABLE` is more than that runs the line short rather than
laying the pane over the session.

- **The fold** is the existing `navigator-off`, set when the reader does not want the outline OR
  the layout folds it, so every rule that reads it already does the right thing. The reader's choice
  (`navigatorOpen`, remembered) is untouched.
- **Shown anyway.** While the layout holds the outline folded, the rail's reopen and the outline's
  toggle key show it anyway, and the line runs short, since the reader asked. That lapses when the
  layout no longer folds it or the pane closes. The toggle key on a shown outline hides it, setting
  the reader's choice as today.
- **The pane's width** is drawn at `--preview-fit`, written only while the rule narrows the pane and
  removed otherwise, so a `--preview` set on `#app` or the root by a case or a host still reaches the
  pane. The reader's chosen width stays as remembered and comes back when there is room.
- **The drag.** With the pane first, its edge stops at the widest pane the rule allows beside what
  is shown: at 1400px with the outline, 425px, where today it goes to 680 and leaves 65px of text.
  With the outline first, a drag past that folds the outline and goes on to the session's floor.
- **The suite's own window** is 1400px wide, 5px above the line at the defaults (text 325): stage 3
  measures the harness's real `innerWidth` before relying on that.

### A box for each component

Stage 1 makes `#sessionView` and `#sessionList` real boxes, with no threshold changed and the whole
suite unchanged as the proof. It replaces "The grid stays the app's" in
`design/session-view-component.md`: the view lays out its own parts in the app as it already says
it does in a host, and `reference.css` still does not change. What moves:

- **`#app`'s grid** shrinks to the list and the view, overridden at id weight in `production.css`:
  `var(--sidebar) minmax(0,1fr)`, `64px minmax(0,1fr)` on the rail, `0 minmax(0,1fr)` on the rail at
  `≤700`. Its `grid-template-columns` transition stays, for the sidebar.
- **The view's grid** takes the rest: `minmax(480px,1fr) 1px var(--preview)`, `minmax(480px,1fr) 0
  0` with the pane shut, `minmax(0,1fr)` at `≤1180`, and on the rail at `≤1180` the four tracks
  today's rule order gives, reading `#app.sidebar-off` like the other reach-in rules until stage 2.
  It takes the same transition, for the pane.
- **Its width is its track's, never its content's**: `contain: inline-size`. With the pane open its
  content's minimum is 901px (480 + 1 + 420), which in an `auto`-minimum track or a flex item would
  hold a 1100px window's view at 901px, and so `E` at 1201, and keep the layout that made it so.
  Containment of the inline size captures no fixed layers and forms no stacking context.
- **Its height**: `height: 100%; min-height: 0; grid-template-rows: minmax(0,1fr)`, which `.app`
  carries today. On a phone, where `.app` is a block, the view takes the whole height.
- **Unpositioned.** The view's box stays static in stage 1, so every absolutely placed part keeps
  its containing block. Stage 4 changes that on purpose.
- **The wide phone.** `#app:not(.phone-column-off)>…>.workspace{grid-column:2}` becomes the view
  in column 2 and the workspace in the view's column 1.
- **The reach-in rules** keep reading `#app`'s copies of the pane's states in stage 1. They move
  into the view, keyed by its own classes and `--pane-beside`, in stage 2, and the copies go once
  nothing reads them.
- **The list's box** takes `flex:1; min-height:0; display:flex; flex-direction:column`, or the
  tree's scroll breaks, and the tree's `width: var(--sidebar)` is read against it.
- **The host fixture** drops the three-track grid it gives the view's element: the view lays out
  its own parts.

### Drawn inside the pane

In a host's pane, all of a component is drawn inside the pane, never across the host's screen
(`design/embedding.md`). That covers the right pane lying over the session, the outline over the
transcript, the phone reader's dock and sheets, and every popover, which today are fixed to the
window and placed from `innerWidth`.

How it is done rests on a measurement: whether a box captures its fixed descendants. It was made
in Chrome 153, the suite's engine, and in Safari 26.4 on the iOS 26.4 Simulator, and the two agree:

- `container-type: inline-size` captures nothing: a fixed child at `left:0; top:0` stays at the
  window's corner. In Safari it also forms no stacking context, since a `z-index: 100` child still
  paints over a `z-index: 50` element outside the box.
- `contain: layout` captures: the same child sits at the box's corner. It also traps the child's
  stacking: the `z-index: 100` child paints under the `z-index: 50` element outside.
- On a `display: contents` element, `container-type` computes but no box carries it.

So the view's box can take `contain: layout` while it is not the screen, and every fixed layer then
lands in the pane. What is placed from window coordinates moves to the box's coordinates: the task
popover, the section menu, the filter sheet, the rail flyout, the turn menu and the right pane's
drag (`innerWidth - clientX`).

Whether the app's box takes it too is stage 4's question. One path for every host is simpler: the
layers written in the box's coordinates, and the box contained. But containment traps stacking, and
in the app the view's layers interleave with the app's own today: the drawer and its handle, the
toasts, the review sheet above the drawer. Stage 4 lists each one before choosing.

### What stays the host's

The host keeps the drawer and its handle, the rail, the sidebar's width and its `sidebar-tight`, the
app's grid, a wide phone's column beside the session, and `--pane-beside`. It lays the two
components out against each other, by the window. Native mode is the standalone app's alone and
keeps its queries, which agree with the view's family in the app because `E` is the window there; a
case holds that.

### The session list

`design/embedding.md` asks for full rows where there is room and compact rows where there is not.
On a desktop the list's rows are already one line of 32px: a state mark, the title cut with an
ellipsis, a fork chip, and in the flat list the agent's mark and the project. On a phone they are
44px touch rows. So:

- **Its family is the screen's** (`PHONE_QUERY`): its phone form is touch rows, which a narrow
  column on a desktop does not want.
- **Compact rows** are a band of its own width: below 232px, the app's minimum sidebar, so the app
  never draws them. There the flat list's project name goes and the agent's mark stays, and a
  session's indent narrows from 32px to 20px. The same module measures the list's box and sets the
  class (`pl-max-w-231`) while the tree is shown; on the app's 64px rail the tree is hidden and the
  class is not set.
- Its stylesheet's other size rules are the phone's and two stray copies of the view's
  `--navigator`, which go. `sidebar-tight` belongs to the app's head, not to the list.
- In stage 4 its floating layers (the filter sheet, the section menu, the rail flyout) are drawn in
  the pane.

### What the app changes

At the default text size, the pane at the reader's width:

- **The 126px column**, by the readable line, at windows below 655 + the pane's width + 320px.
- **The rail's three lengths.** On the rail `--sidebar` still holds the hidden sidebar's width, so
  today the turn menu opens 254px into the session at the default width. It moves to the rail's
  edge.
- **The drag** of the right pane, which stops at the readable line (pane first).
- **The empty column on the rail** at `≤1180`, under the open pane, which stage 2 removes.
- **A wide sidebar in a narrow window**: with the sidebar wider than the window less 821px (at
  1181–1340px with a 520px sidebar), the open pane lies over the session, where today the grid runs
  past the window.
- Under the alternative width model: the sidebar's cap, and bands that move with the sidebar,
  instead of the line above.

## Tests

- **The column, red first.** The app with the right pane open at 1181 and 1280px: the transcript's
  text is at least `READABLE`, and the pane ends inside the window. This is red on 34c7eb50 at
  both, and 1440 is the control, unchanged.
- **Widths in a host.** The view and the list each in a host fixture's pane that is smaller than
  the window and does not start at its edge: 380px at x=500 in a 1400px window, 700px, 1000px,
  1100px, light and dark. In stage 2 each asserts its family, condition classes and `E`. From
  stage 4 each also asserts that nothing overflows the pane, and that every control is inside it
  and is the element `elementFromPoint` finds there; a rect alone proves nothing.
- **On an emulated phone.** A 390×844 phone with the host's pane the whole screen (`E` 390), and a
  932×430 phone held sideways with a 450px pane (`E` 750 under the recommended model; the screen's
  932 under the alternative). Each asserts its family and classes, and from stage 4, the dock and
  sheets inside the pane.
- **The app's `E` is the window.** At a 1000px window with the sidebar at 232 and at 520, on the
  rail, on a phone with its drawer, and on a wide phone held sideways with its column: `E` equals
  `innerWidth`, and the classes match the window's media.
- **Mutations.** Point the layout module back at the window, drop a condition from the rewrite's
  table, or let the readable rule read what it drew: a width case goes red, or the derivation fails.
- **The contract.** The component stylesheets carry no `@media` with a width feature. Every class
  the derivation writes is one the module sets. No component script reads `PHONE_QUERY`.
- **The app's suite**, whole and unchanged, at every stage. The expected changes: the cases that
  open the right pane at a 1181–1394px window, the case that draws a 760px pane at 1440
  (`files.rs`, which today runs the grid 101px past the window), the rail's turn menu, and a drag
  past the readable line. Stage 3 lists each one, with what it asserted before.
- **The Simulator walk.** The app as today, and the host fixture with a narrow pane on a phone,
  upright and sideways, light and dark.

## The order

Each stage is page code: the whole local browser suite and an iOS Simulator walk before it lands.

1. **A box for each component**, as above. No threshold moves; the suite is unchanged.
2. **One decision** (after #s461). `pane-layout.js`, `--pane-beside` and the app's settings of it,
   the derive script's rewrite and its table, the units, the tokens, the scripts asking the module,
   and the reach-in rules moved into the view. The host-fixture width cases at the classes and `E`.
3. **A readable line** (after #s462). The fold, the narrowed pane and the drag, with the red case.
4. **Drawn inside the pane.** Containment, the layers in the box's coordinates, the width cases'
   containment checks on a desktop and an emulated phone, and the Simulator walk of a narrow pane.
   The lengths that fit a fixed layer to the window (`calc(100vw - 24px)` and its kin in the task
   card, the sheets and the menus) take a unit of their own there: `1vw` while the box is not
   contained, the pane's width over 100 once it is. Stage 2's `E` units would size them to `E`, which
   in a host's 380px pane is 680px.
