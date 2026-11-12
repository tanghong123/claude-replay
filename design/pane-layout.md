# Each component lays itself out by its pane

> **DRAFT 2** (#s431, 2026-11-12). Step 6 of `design/embedding.md` ("Layout decides itself", "The
> order of the work"). Not built. The owner's ruling (#s459): start it now, and fix in it the
> transcript column that the right pane squeezes to 126px just past 1180px. The owner's notes on the
> width-study videos may still change it; every number they can touch is in one table. Draft 2
> answers the first review: the bands read one effective width, the right pane narrows rather than
> lying over the session, and stage 1 names the rules it has to move.

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

The app is a host like any other, so it must look and behave as it does now, with one exception the
owner asked for: the 126px column.

## What asks the window today

Measured on main at 34c7eb50.

- **Styles.** `reference.css` (generated from the demo) and `production.css` (ours) carry 90
  `@media` blocks, 45 each; the three derived stylesheets carry 116 (`session-view.css` 79,
  `app-shell.css` 28, `session-list.css` 9). Sixteen distinct conditions read a size. They fall
  into a few bands:

  | Window band | What changes there | Whose |
  |---|---|---|
  | `≤1180` | The right pane leaves the app's grid and lies over the whole session area (the app's rule at 761–1180 sets its left edge at the sidebar); the folded outline's rail narrows from 68 to 62px; the search box, the session's frame and the turn dock tighten | the grid and the overlay's placement the app's; the rest the view's |
  | `≤900` | The outline becomes an overlay with a toggle; the title, the search box and the actions shrink | the view's |
  | `≤760`, or a finger on a short side (`PHONE_QUERY`) | The phone: the list a drawer, the one-row top bar, the dock, the sheets, 44px targets | the drawer the app's; the reader the view's; 44px rows the list's |
  | `≤700`, `≤620`, `≤560` | Narrower phones: the search box an icon, denser cards, fewer turn controls | the view's |
  | `≥761`, `≥901`, `≥1181`, `701–900` and the like | The complements: the desktop compose box, the outline's 290px column, the search box's focused width | the view's |
  | a finger on a short side (`(pointer:coarse) and (max-height:500px)`), and that at `≥900` wide (`WIDE_PHONE_QUERY`) | A phone held sideways: a reading column, popovers beside their controls; a wide one shows the list as a 280px column | the view's; the column the app's, which also lays out the view's parts beside it |

  About 30 rules in `app-shell.css` reach into the view: they style the view's parts under the
  app's state and the window's media (`.app:not(.preview-off)>…>.preview`, `.preview-full`, the wide
  phone's column). Fifty-one rules in `session-view.css` size themselves in window units (`36vw`,
  `45vw`, `calc(100vw - 46px)`, `74vh`, the phone sheets' `100dvh`), seventeen of them outside any
  size `@media`.
- **Scripts.** `matchMedia(PHONE_QUERY)` in `app.js` (2, and `WIDE_PHONE_QUERY` once),
  `session-view.js` (5, and the wide query once), `session-list-view.js` (1) and `view-state.js` (the
  outline's default, read before the first render); `onPhone()` in `preview.js`. Window sizes in the
  right pane's resize drag (`innerWidth - clientX`), the task popover, the section menu, the filter
  sheet, the rail flyout and `native-menu.js`.
- **Native mode** (`codex-workspace/monitor.css`, `native-entry.css`, `monitor-controls.js`,
  `project-picker.js`) reads the same queries and `innerWidth` for its composer and pickers. It is
  the standalone app's, never a host's.
- **No box.** `#sessionView` and `#sessionList` are `display: contents` in the app, so neither has
  a size to measure or to query. The view's parts are items of `#app`'s grid
  (`var(--sidebar) minmax(480px,1fr) 1px var(--preview)`: the list, the session, the resizer, the
  right pane). The app copies the pane's open, full and resizing states onto `#app` to drive it.

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
exactly and reads them from one number instead: its EFFECTIVE WIDTH, `E`. A pane is laid out the way
the app lays out a window whose session area is that pane, with the default sidebar beside it:

| Where the component is | `E` | Family |
|---|---|---|
| The screen is a phone's (`PHONE_QUERY` on the window) | the window's width, as today | phone |
| Otherwise, its pane is narrow: at most `NARROW_PANE` (460px) | the pane's width | phone |
| Otherwise | the pane's width + `REFERENCE_SIDEBAR` (300px) | desktop |

So the app keeps every band it has at its default sidebar, by construction:

- **In the phone family on a phone**, every band reads the window, as today. The wide phone held
  sideways, whose session area is the window less its 280px list column, keeps today's layout.
  So does a tall window of 701–760px, a phone whose `701–900` rules apply today.
- **On a desktop**, the view's pane is the window less the sidebar, so `E` is the window's width
  whenever the sidebar is at 300px.
- **A resized sidebar or the rail** moves the bands with the pane, which is the point of the step.

In a host:

- a 380px column on a desktop is a narrow pane: the phone reader, drawn in the pane, which is the
  narrow layout `design/embedding.md` asks for (the transcript alone; the outline's panes and the
  artifacts one tap away as sheets);
- a 700px pane is a desktop window of 1000: the outline gives way and the right pane lies over the
  session;
- a 1100px pane is a desktop window of 1400.

The device's own facts stay the device's: a coarse pointer, hover, a short side, and the sideways
and wide-phone queries. In the phone family on a phone they read the screen, as today. On a
desktop they are false, and in a narrow pane on a desktop a coarse pointer is not there.

**The app keeps its sidebar out of the narrow line.** At a 761–980px window, a sidebar dragged to
301–520px would leave the view 460px or less, and the view would turn into the phone reader beside
a desktop sidebar. Laying the two out against each other is the host's job, so the app caps its
sidebar at the window less 461px, in its own rule (`--sidebar: min(var(--sidebar-user), 100vw -
461px)`). Today such a sidebar keeps the session at its 480px floor and runs the grid past the
window's edge; no case holds that.

### The right pane needs a readable line

The right pane is a column only while the transcript beside it keeps a readable line: `READABLE`,
320px of text at the default text size, scaled with the reader's text size. That is about 45
characters, the low end of a readable line. Short of it, on a desktop (`E > 1180`):

1. **The outline folds** to its rail. This is a fold of the layout only, `pl-outline-fold`. The
   reader's own choice (`navigatorOpen`, remembered, which `.workspace.navigator-off` draws) is
   untouched, and the outline comes back when the pane closes or the room returns.
2. **Then the pane narrows**, to no less than its 340px minimum, until the text has `READABLE`
   and the session its 480px floor. It is drawn at an effective width (`--preview-fit`). The
   reader's chosen width (`--preview`, remembered) is untouched and comes back when there is room.
   The floor is today's too, which the grid keeps by running past the window: at 1181–1200 the
   pane's last 1–20px are cut off. Narrowing the pane ends that.

Below the 1180 line the pane lies over the session, as today. Above it, at the default text size,
the two steps always suffice, so no new overlay band exists: the steps apply only to a component
over 880px wide (`E` over 1180, less the 300px sidebar), and there the outline folded and the pane
at 340px leave at least 408px of text. At a larger text size `READABLE` may be more than that; the
pane then stops at 340px and the line runs short, rather than the pane lying over the session.

The rule reads its inputs, never what it drew, so it cannot feed back on itself: the pane's width
(the component's), the reader's chosen pane width, whether the reader wants the outline, and the
text size. While the layout holds the outline folded, a click on the rail's reopen opens it, and
the pane then narrows further. If it is already at 340px and the line is still short, the click
wins: the outline shows and the text runs under `READABLE`, since the reader asked for it. A drag
of the pane's edge is clamped at the readable line, so the reader cannot drag it into an overlay.

At the default sidebar, the pane at its default 420px:

| Window | Today | Now |
|---|---|---|
| 1181–1200 | column, text 126, the pane cut off by up to 20px | column, the outline folded, the pane 400–419px: text 348 |
| 1201–1394 | column, text 126–319 | column, the outline folded: text 348–541 |
| from 1395 | column, text 320 and up | the same |

`READABLE`, `NARROW_PANE` and `REFERENCE_SIDEBAR` live in one table in the layout module, so the
owner's notes change a number, not the design. At 360px of text (50 characters) the line would
reach 1435, and the default window of the browser suite (1400) would fold the outline whenever a
case opens the pane.

Left alone: the outline's own column. From a 901px window it leaves the transcript 311px, text 247.
That is today's layout, and no one asked for it to change.

### One decision, read by styles and scripts

A container query lets styles ask the pane. But a script needs the answer before the first render
(the outline's default), and "the screen is a phone's, or the pane is narrow, or the pane plus a
sidebar" is no container condition. So one module decides (`pane-layout.js`), and everything reads
its answer:

- **It measures.** It reads the component's box at mount, synchronously before the first render,
  and again through a `ResizeObserver`. It hears the device (`PHONE_QUERY`, a coarse pointer, the
  short side) through `matchMedia` listeners, and the reader's text size, pane width and outline
  choice from the view.
- **It writes classes and sizes** on the component's element. The classes are the family
  (`pl-phone`), one per condition the stylesheets use (`pl-max-1180`, `pl-max-900`, `pl-max-700`,
  …), and `pl-outline-fold`. The sizes are `--pl-e` (`E`), `--pl-h` (the pane's height), and
  `--pane-x` and `--pane-y` (its box's corner in the window).
- **Scripts ask it**: `layout.phone()`, and its change events. No component script reads
  `PHONE_QUERY` itself.
- **A change of `E` that moves the transcript's width calls `paneLayoutChanged`**, which remeasures
  the transcript. While a host's pane is being dragged, that is throttled to the drag's end.
- **The derive script rewrites the styles.** `scripts/session-view-css.mjs` rewrites each component
  rule under a size `@media` into the same rule gated by the conditions' classes, through
  `:where()`, so weight and source order are what they were:

  | Rule today | Rewritten |
  |---|---|
  | `:where(#sessionView) .x` | `:where(#sessionView.pl-max-1180) .x` |
  | `#sessionView .x` | `#sessionView:where(.pl-max-1180) .x` |
  | `:where(#sessionView).preview-off …` | `:where(#sessionView.pl-max-1180).preview-off …` |
  | `#sessionView:is(*,div):not([data-shared-codex]) …` | `#sessionView:where(.pl-max-1180):is(*,div):not([data-shared-codex]) …` |
  | a complement (`min-width:761px … pointer:fine`) | `…:where(:not(.pl-phone))…` |
  | `:root{--navigator:…}` inside a band | `:where(#sessionView.pl-max-1180).session-view{…}` |

  - The conditions it knows form a table. A size condition missing from it, a nested group or an
    `@supports` fails the derivation.
  - It asserts that every rewritten selector weighs what it did, as it already does for the
    selectors it moves.
  - Window units in a component rule become `E` units: `36vw` is `calc(var(--pl-e) * .36)`, and
    `74vh` is `calc(var(--pl-h) * .74)`. At the default sidebar they are the window's, so the app
    is unchanged. This covers the seventeen rules outside any size band too.
- **What stays `@media`.** Rules that ask only the device (`hover`, `pointer`, `prefers-*`, the
  sideways and wide-phone queries) stay as they are. `reference.css` and the demo stay window pages,
  untouched.
- **Tokens.** The view declares the tokens it reads and only `#app` declared: `--phone-dock-h`
  and `--phone-glass` under `pl-phone`, and `--preview` with its own rules. In a host's narrow
  pane, `calc(var(--phone-dock-h) + 16px)` must not be invalid.
- **One phone query.** `every_media_query_agrees_on_what_a_phone_is` is rewritten, not kept as it
  is. `PHONE_QUERY` is still written once, in `shared/capabilities.js`, and the app's `@media` still
  spell it. The component stylesheets carry no size `@media`. The layout module and native mode
  are the only scripts that read the query. Its count of wide-phone rules falls from 8 to the
  app's 4.

### A box for each component

`#sessionView` becomes a real box. In the app it takes the session's place in `#app`'s grid and
holds its own grid: the session, the resizer and the right pane. `#app`'s grid shrinks to the list
and the view. `#sessionList` becomes a box in the sidebar's column. This lands first and alone,
with no threshold changed, and the whole suite unchanged proves the move kept the layout. It is not
free of CSS. What it has to move:

- **The grid.** About ten generated rules write `.app`'s four tracks: the base, `preview-off`,
  `sidebar-off` at 0 and at 64px, and their `≤1180` and `≤700` forms. They are overridden at id
  weight in `production.css` (`#app`: two tracks; `#sessionView`: three). The `minmax(480px,1fr)`
  floor and the `grid-template-columns` transition move with them. `.session-view{display:contents}`
  becomes a grid. On a phone, where `.app` is a block, the view takes the whole height.
- **The wide phone.** `#app:not(.phone-column-off)>…>.workspace{grid-column:2}` would put the
  session in the view's own second track. It becomes the view's column 1.
- **The reach-in rules** keep reading `#app`'s copies of the pane's states in stage 1. They move
  into the view, keyed by its own classes, in stage 2, and the copies go once nothing reads them.
- **The list.** Its box takes `flex:1; min-height:0` and a column of its own, or the tree's scroll
  breaks. `.sidebar>…>.tree{width:var(--sidebar)}` is read against the list's box.

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
popover, the section menu, the filter sheet, the rail flyout, and the right pane's drag
(`innerWidth - clientX`).

Whether the app's box takes it too is stage 4's question. One path for every host is simpler: the
layers written in the box's coordinates, and the box contained. But containment traps stacking, and
in the app the view's layers interleave with the app's own today: the drawer and its handle, the
toasts, the review sheet above the drawer. Stage 4 lists each one before choosing. Until then, the
right pane's overlay takes its left edge from `--pane-x` (in the app, the sidebar's width), not from
the app's `--sidebar`.

### What stays the host's

The host keeps:

- the drawer and its handle, and the rail;
- the sidebar's width, its cap, and its `sidebar-tight`;
- the app's grid, and a wide phone's column beside the session.

All are laid out by the window. The host still lays the two components out against each other.
Native mode is the standalone app's alone and keeps its queries.

### The session list

The list has no width bands of its own today:

- its stylesheet's size rules are the phone's (44px rows, the tree's padding) and two stray copies of
  the view's `--navigator`, which go;
- `sidebar-tight` belongs to the app's head, not to the list.

So the list takes its family from the screen alone. Its phone form is touch-sized rows, which a
narrow column on a desktop does not want, so it gets no narrow-pane term. What laying out by its
pane means for the list is what stage 4 gives it: its rows fit the pane at every width, and its
floating layers (the filter sheet, the section menu, the rail flyout) are drawn in the pane.
Compact rows for a very narrow pane are not designed here; if the owner's notes ask for them, they
are a band of the list's own.

## Tests

- **The column, red first.** The app with the right pane open at 1181 and 1280px: the transcript's
  text is at least `READABLE`, the outline is folded, and the pane ends inside the window. This is
  red on 34c7eb50 at both, and 1440 is the control, unchanged.
- **Widths in a host.** The view and the list each in a host fixture's pane that is smaller than
  the window and does not start at its edge: 380px at x=500 in a 1400px window, 700px, 1000px,
  1100px. Each runs with a mouse and on an emulated phone, light and dark. At each size:
  - the expected family and condition classes are on the element;
  - nothing overflows the pane;
  - every control is inside it and is the element `elementFromPoint` finds there. A rect alone
    proves nothing.
- **The app's sidebar and the bands.** The view's `E` and classes at a 1000px window with the
  sidebar at 232 and at 520, and on the rail. The cap holds at 800px.
- **Mutations.** Point the layout module back at the window, drop a condition from the rewrite's
  table, or let the readable rule read what it drew: a width case goes red, or the derivation fails.
- **The contract.** The component stylesheets carry no size `@media`. Every class the derivation
  writes is one the module sets. No component script reads `PHONE_QUERY`.
- **The app's suite**, whole and unchanged, at every stage. The only expected changes are the cases
  that open the right pane at a 1181–1394px window. Stage 3 lists each one, with what it asserted
  before.
- **The Simulator walk.** The app as today, and the host fixture with a narrow pane on a phone,
  upright and sideways, light and dark.

## The order

Each stage is page code: the whole local browser suite and an iOS Simulator walk before it lands.

1. **A box for each component.** The view's own grid, with the overrides above; `#app`'s grid
   shrinks to the list and the view; the list's box. No threshold moves. The suite is unchanged.
2. **One decision.** `pane-layout.js`, the derive script's rewrite and its table, `E` units, the
   tokens, and the scripts asking the module. The reach-in rules move into the view. The app's
   sidebar cap. The host-fixture width cases with a mouse. The cases that collapse or resize the
   sidebar are rechecked, and any whose layout moved goes to the owner.
3. **A readable line.** The fold, the narrowed pane and the clamped drag, with the red case.
4. **Drawn inside the pane.** Containment, the layers in the box's coordinates, the width cases on
   an emulated phone, and the Simulator walk of a narrow pane.
