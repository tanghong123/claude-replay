# Each component lays itself out by its pane

> **DRAFT 1** (#s431, 2026-11-12). Step 6 of `design/embedding.md` ("Layout decides itself", "The
> order of the work"). Not built. The owner's ruling (#s459): start it now, and fix in it the
> transcript column that the right pane squeezes to 126px just past 1180px. The owner's notes on the
> width-study videos may still change it; every number they can touch is in one table.

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

- **Styles.** `reference.css` (generated from the demo) and `production.css` (ours) carry 85
  `@media` blocks; the three derived stylesheets carry 116 (`session-view.css` 79,
  `app-shell.css` 28, `session-list.css` 9). Eighteen distinct conditions read a size. They fall
  into a few bands:

  | Window band | What changes there | Whose |
  |---|---|---|
  | `≤1180` | The right pane leaves the app's grid and lies over the session (a fixed overlay at the right, `min(620px,72vw)`); the Info panel becomes a row; the outline narrows | the app's grid; the rest the view's |
  | `≤900` | The outline becomes an overlay with a toggle; the title, the search box and the actions shrink | the view's |
  | `≤760`, or a finger on a short side (`PHONE_QUERY`) | The phone: the list a drawer, the one-row top bar, the dock, the sheets, 44px targets | the drawer the app's; the reader the view's; 44px rows the list's |
  | `≤700`, `≤620`, `≤560` | Narrower phones: the search box an icon, denser cards, fewer turn controls | the view's |
  | `≥761`, `≥901`, `≥1181`, `701–900` and the like | The complements: the desktop compose box, the outline's 290px column, the search box's focused width | the view's |
  | a finger on a short side, `≥900` wide | A wide phone held sideways: the list a 280px column beside the session | the app's, laying out the view's parts |

  Rules inside those blocks also size themselves in window units: `72vw`, `78vw`, `45vw`,
  `calc(100vw - 46px)`, `40vh`, the phone sheets' `100dvh`.
- **Scripts.** `matchMedia(PHONE_QUERY)` in `app.js` (2), `session-view.js` (6),
  `session-list-view.js` (1), `view-state.js` (the outline's default, read before the first
  render) and `onPhone()` in `preview.js`; window sizes in the right pane's resize drag
  (`innerWidth - clientX`), the task popover, the section menu, the filter sheet and the rail
  flyout.
- **No box.** `#sessionView` and `#sessionList` are `display: contents` in the app, so neither has
  a size to measure or to query: the view's parts are items of `#app`'s grid
  (`var(--sidebar) minmax(480px,1fr) 1px var(--preview)`: the list, the session, the resizer, the
  right pane), and the app copies the pane's open, full and resizing states onto `#app` to drive it.

## The 126px column

With the right pane open, above 1180px the app's grid gives the session `max(480, W − 721)` px
(sidebar 300, resizer 1, pane 420). The outline takes 290 of it, and the transcript's text
(`.transcript-inner`) is the rest less 64px of margin:

| Window | Session | Transcript | Text |
|---|---|---|---|
| 1181 | 480 (the floor) | 190 | **126** |
| 1280 | 559 | 269 | **205** |
| 1440 | 719 | 429 | **365** |

These are the owner's three measurements, to the pixel. At 1181 the grid's tracks also add up to
1201px in a 1181px window, so the last 20px of the pane are cut off. The text reaches its 820px
measure only from 1905px.

## Decisions

### Two families, chosen once

A pane cannot reuse the window's numbers. In the app the view's pane is the window less the sidebar
(300px by default, 232 to 520 as the reader resizes it, 64 as the rail) while the sidebar is a
column, and the whole window once the sidebar becomes a drawer. A single threshold on the pane
cannot be met by both:

- at a 760px window, today's phone, the pane is 760 (the drawer is over it);
- at a 761px window, today's desktop, it is 461.

So the view keeps today's two layouts as two FAMILIES and decides between them once:

- **The phone family** when the screen is a phone's (`PHONE_QUERY`, unchanged: up to 760px wide, or
  a finger on a side up to 500px) or the pane is narrow (`NARROW_PANE`, 460px, a host's side
  column). In the app this is exactly today's phone, since the drawer makes the pane the screen.
  In a host's 380px column on a desktop, it is the phone reader drawn in the pane, which is the
  narrow layout `design/embedding.md` asks for: the transcript alone, the outline's panes and the
  artifacts one tap away as sheets.
- **The desktop family** otherwise.

Each family keeps its own bands, read from the pane:

| Band | Phone family | Desktop family | The app, default sidebar |
|---|---|---|---|
| The outline gives way (was `≤900`) | pane `≤900` | pane `≤600` | window `≤900` |
| The right pane lies over the session (was `≤1180`) | always | below a readable line (next decision) | window `≤1212` |
| Narrower phones (`≤700`, `≤620`, `≤560`) | pane, as written | never | as today |
| The complements (`≥761`, `≥901`, `≥1181`, `701–900`) | their phone-family reading | their desktop reading | as today |

The desktop family's numbers are today's less the default sidebar, so the app at its default
sidebar keeps every band it has. A resized sidebar or the rail now moves the bands with the pane,
which is the point of the step. Any existing case whose expected layout that changes, outside the
right pane's band below, is brought to the owner before it is re-pinned.

The phone family's narrower bands (`≤700` and below) apply only inside it. Today a window that
narrow is always a phone, so they never applied to a desktop layout, and in a desktop pane of 600px
they still do not.

One place where the phone family's pane is not the screen: a wide phone held sideways, whose list
is a 280px column beside the session. There the pane is the window less 280 (676px on a 956px
phone), so the outline's band (`≤900`), which the window (956) does not meet today, is met by the
pane. Most of that band's rules are overridden by the phone's own, but not all of them, so stage 2
measures that phone with the band read from the pane and from the window. If the two differ on
screen, the app's wide-phone cases decide, and the owner hears of any that would change.

The list chooses its family from the screen alone, with no narrow-pane term. Its phone form is
touch-sized rows, which a narrow column on a desktop does not want. Its answer to a narrow pane is
the compact rows it already has (`sidebar-tight`, measured from the sidebar's own width), now
measured from its own box.

### The right pane needs a readable line

The right pane is a column only while the transcript keeps a readable line beside it: `READABLE`,
360px of text, about 50 characters at the transcript's size. Short of that:

1. **First the outline folds** to its 68px rail, as a fold of the layout only. The reader's own
   choice (`navigatorOpen`, remembered) is untouched, and the outline comes back when the pane
   closes or the room returns.
2. **Then the pane lies over the session**, as it does below 1180px today.

The outline goes first because the reader opened the pane to read it beside the transcript, and
the outline folded is one click away. At the default sidebar, with the pane at its default 420px:

| Window | Today | Now |
|---|---|---|
| 1181–1212 | column, text 126–137 | over the session |
| 1213–1434 | column, text 138–359 | column, the outline folded: text 360–581 |
| from 1435 | column, text 360 and up | the same |

A wider pane (the reader can drag it to 680px) raises each line by the difference. The rule reads
the pane's width as it is, the outline's as drawn, and `READABLE`. `READABLE`, `NARROW_PANE`
and the desktop thresholds live together in one table in the layout module, so the owner's notes
change a number, not the design.

Left alone: the outline's own column. From 601px of pane (a 901px window) it leaves the
transcript 311px, text 247. That is today's layout, and no one asked for it to change.

### One decision, read by styles and scripts

A container query would let the styles ask the pane, but scripts cannot read its result, and a
dozen do (the outline's default before the first render, the search box, the Tasks pane's groups,
the dock). Two decisions made separately can disagree for a frame. Container queries also cannot
say "the screen is a phone's, or the pane is narrow".

So one module decides (`pane-layout.js`), and everything reads its answer:

- It measures the component's box, at mount and synchronously before the first render, and again
  through a `ResizeObserver`. It reads the device (`PHONE_QUERY`, a coarse pointer, the short side)
  through `matchMedia` listeners.
- It writes the answer as classes on the component's element: the family (`pl-phone`) and each
  band (`pl-outline-over`, `pl-pane-over`, `pl-700`, `pl-620`, `pl-560`). It also writes the pane's
  size as `--pane-w` and `--pane-h`.
- Scripts ask it (`layout.phone()`, `layout.paneOver()`), never `matchMedia(PHONE_QUERY)`, and hear
  its changes.
- The derive script (`scripts/session-view-css.mjs`) rewrites each component rule that sits in a
  size `@media` into the same rule gated by the band's class, through `:where()`. So specificity
  and source order are what they were, and the cascade does not move: `@media(max-width:1180px){
  :where(#sessionView) .preview{…} }` becomes `:where(#sessionView.pl-pane-over) .preview{…}`.
  The conditions it knows form a table, and a size condition missing from it fails the derivation.
  Window units in a component rule become pane units: `72vw` becomes
  `calc(var(--pane-w) * .72)`.
- `reference.css` and the demo stay window pages, untouched. The app's own rules (`app-shell.css`:
  its grid, the drawer, the rail, the wide phone's column) keep their `@media`: the host owns the
  screen.
- Rules that only ask the device (`hover`, `pointer`, `prefers-*`) stay `@media`.

`every_media_query_agrees_on_what_a_phone_is` keeps its job in the new form: `PHONE_QUERY` is still
written once, in `shared/capabilities.js`; the app's `@media` still spell it; and the layout module
is the only script that reads it.

### A box for each component

`#sessionView` becomes a real box. In the app it takes the session's place in `#app`'s grid and
holds its own grid: the session, the resizer and the right pane. `#app`'s grid shrinks to the list
and the view. The pane's states then drive the view's own grid from the view's own element, and the
copies on `#app` go wherever the app no longer reads them. `#sessionList` becomes a box inside the
sidebar.

This lands first and alone, with no threshold changed, so the whole suite, unchanged, proves the
move kept the layout.

### Drawn inside the pane

In a host's pane, all of a component is drawn inside the pane, never across the host's screen
(`design/embedding.md`). That covers the right pane lying over the session, the outline over the
transcript, the phone reader's dock and sheets, and every popover, which today are fixed to the
window and placed from `innerWidth`. In the app, in the phone family, the pane is the screen, so
nothing moves.

How it is done rests on a measurement: whether a box captures its fixed descendants. In Chrome
153, the suite's engine:

- `container-type: inline-size` does not: a fixed child at `left:0; top:0` stays at the window's
  corner.
- `contain: layout` does: the same child sits at the box's corner.
- `contain: layout` also traps the child's stacking. A `z-index: 100` child paints under a
  `z-index: 50` element outside the box.

WebKit (iOS Safari) is being measured on the Simulator. If it agrees, the view's box can take
`contain: layout` while it is not the screen, and every fixed layer then lands in the pane. What is
placed from window coordinates moves to the box's coordinates: the task popover, the section menu,
the filter sheet, the rail flyout, and the right pane's drag (`innerWidth - clientX`). If WebKit
does not agree, each layer is placed absolute in the view's box instead.

In the app the box never takes it: in the phone family the pane is the screen, and on a desktop
the window's coordinates are what those layers are written in.

### What stays the host's

The drawer and its handle, the rail, the sidebar's width and its `sidebar-tight` (already measured
from the sidebar), the app's grid, and a wide phone's column beside the session all stay the
host's, laid out by the window. The host still lays the two components out against each other.

## Tests

- **The column, red first.** The app with the right pane open at 1181, 1280 and 1440px: the
  transcript's text is at least `READABLE`, or the pane lies over the session. This is red on
  34c7eb50 at all three.
- **Widths in a host.** The view and the list each in a host fixture's pane that is smaller than
  the window and does not start at its edge: 380px at x=500 in a 1400px window, 700px, 1000px,
  1400px. Each runs with a mouse and on an emulated phone, light and dark. At each size: the
  expected family and bands are on the element; nothing overflows the pane; every control is
  inside it and is the element `elementFromPoint` finds there. A rect alone proves nothing.
- **Mutations.** Point the layout module back at the window, or take a band's class off a rule,
  and a width case goes red.
- **The contract.** No size `@media` is left in the component stylesheets. Every class the
  derivation writes is one the module sets. No script but the module reads `PHONE_QUERY`.
- **The app's suite**, whole and unchanged, at every stage. The cases in the right pane's band
  (above) are the only expected changes. Each is named in the stage that changes it, with what it
  asserted before.
- **The Simulator walk.** The app as today, and the host fixture with a narrow pane on a phone,
  upright and sideways, light and dark.

## The order

Each stage is page code: the whole local browser suite and an iOS Simulator walk before it lands.

1. **A box for each component.** The view's own grid; `#app`'s grid shrinks to the list and the
   view; the pane's state copies on `#app` are dropped where unread; the list's box. No threshold
   moves. The suite is unchanged.
2. **One decision.** `pane-layout.js`, with the derive script's rewrite and the scripts asking it.
   Thresholds as in the first table. The host-fixture width cases with a mouse. The cases that
   collapse or resize the sidebar are rechecked, and any whose layout moved goes to the owner.
3. **A readable line.** The right pane's rule, with the red case.
4. **Drawn inside the pane.** Floating layers in the pane, window units to pane units, the scripts'
   window reads, the width cases on an emulated phone, and the Simulator walk of a narrow pane.
