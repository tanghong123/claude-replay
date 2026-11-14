# Each component lays itself out by its pane

> **DRAFT 5** (#s431, 2026-11-13). Step 6 of `design/embedding.md` ("Layout decides itself", "The
> order of the work"). Stage 1 shipped in 1.514.0, and stages 2 and 3 in 1.520.0; stage 4 is not
> built (2026-11-14). The owner's ruling (#s459): start it now, and fix in it the
> transcript column that the right pane squeezes to 126px just past 1180px. The owner ruled how a
> component counts its room (#s461): by its own width, height and touch, with nothing from the host;
> and he set the order of what gives way (#s462, #s472 to #s476), its touch screen's rules (#s478,
> #s480) and how a host hears the reader's outline choice (#s479). No question is open.

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

The app is a host like any other, so it must look and behave as it does now at a desktop window and
on a phone. The exceptions are the 126px column, which the owner asked to fix, and the places listed
under "What the app changes", where the view now follows the room it really has.

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

### Its own size, and touch

The owner's ruling (#s461): the view decides its layout from its own width and height, and from
whether the screen is touch. Nothing reads the window or what sits beside the view, and a host
tells it nothing: "why cannot the session view to decide its own layout simply by its own height,
width and whether it is on desktop (no touch) or mobile? No calculation of window width minus side
pane. This would make it even more general, e.g. embedding the view anywhere on the page?"

- **Touch** is the screen's primary pointer, `(pointer:coarse)`: a phone or a tablet.
- **The phone family** is a view no wider than 460px, on any screen, or a touch view no taller than
  500px (a phone held sideways). Every phone held upright is 460px or narrower. A side column on a
  desktop, about 380px, is the phone reader, which is the narrow layout `design/embedding.md` asks
  for. Stage 3 widens the family (below, "The narrowest steps"): a view that cannot hold the session
  at `M` and its margin, 489px or less, on any screen, and on a touch screen any view of 760px or
  less (the owner, #s475).
- **The bands.** Inside the phone family each band keeps today's number, read from the view's
  width: on a phone the view is the screen, so it reads what the window read. Outside the family
  each band is today's number less 300px. Today's numbers were window widths with the default 300px
  sidebar beside the view, so a desktop window at the default sidebar reads exactly what it reads
  today. The 300 is part of each threshold, written once; nothing measures a sidebar.

| Today, the window | The view outside the phone family | Inside it |
|---|---|---|
| `≤1180` | `≤880` | every phone |
| `≤900` | `≤600` | `≤900` |
| `≤760`, or a short touch screen: the phone | the family itself | the family |
| `≤700`, `≤620`, `≤560` | never: `≤400` and under is the family | `≤700`, `≤620`, `≤560` |
| a wide phone held sideways, `≥900` | never | a short touch view of `≥900` |
| the complements, each "not a phone" (`≥761`, `≥901`, `901–1180`, `701–900` …) | any desktop view, `>600`, `601–880`, `401–600` … | never |

So the module works out one number, `B`, the width the bands read: the view's width inside the
family, and its width plus 300 outside. Every condition reads `B` the way it read the window, and
the old phone query, rewritten, reads exactly the family. A height in a condition reads the view's height,
which in the app is the window's.

What each place gets, the app at the default sidebar unless it says otherwise:

| Where | The view | Today | Under the rule |
|---|---|---|---|
| A desktop window over 760px | the window less the sidebar | the bands by the window | the same |
| A desktop window over 760px on the rail | the window less 64px | the bands by the window | the bands by its own width: at 1000px, a 936px view takes the wide layout, where today the pane lay over the session |
| A desktop window of 761–980px with a 520px sidebar | 241–460px | the desktop layout squeezed into it | the phone reader beside the sidebar |
| A desktop window of 461–760px | the window (the list in its drawer) | the phone reader | the compact desktop layout of a 761–1060px window, under the drawer's handle |
| A desktop window of 460px or less | the window | the phone reader | the same |
| A tablet upright with its sidebar, from 761px (an iPad but the mini) | the window less the sidebar | the bands by the window | the same |
| A touch window of 461–760px that is not short: an iPad mini upright, an iPad in split view, a foldable open | the window (the list in its drawer) | the phone reader | the compact desktop layout, under the drawer's handle |
| A phone upright | the screen, 460px or less | the phone reader | the same |
| A phone held sideways | the screen, 667–956px by 375–440px | the phone reader, by its width | the same |
| A wide phone held sideways, its list column hidden | the screen, 932–956px | the wide phone's | the same |
| A wide phone held sideways, its list column shown | 652–676px by 430–440px | the wide phone's | a 676px phone held sideways: the `≤900` and `≤700` tiers apply and the two wide rules do not |

Stage 3 changes the rows for a mouse window under 761px, for a wide sidebar in a narrow window and
for a tablet (below, "The narrowest steps"): with a mouse the list gives way before the session goes
under `M`, and on a touch screen a view of 760px or less is the phone reader, beside its list too. The last row is the one phone state that changes. No rule on the view's own size can keep it: phones
held sideways are 667–956px wide, and the column leaves the view 652–676px, narrower than the
narrowest of them. So the view takes what its width says. Stage 2 lists what that changes, every
element's computed style in that state before and after (the rendering audit's instrument). The
app's own rules for its column keep the view's fixed layers clear of it, as `app-shell.css` already
does for the dock while a search is open, until stage 4 draws them inside the view. The Simulator
walk covers it.

**The app's chrome is unchanged.** Its drawer and handle still follow the window's phone query, the
one query of #s32, so the app's own rules and `reference.css` keep reading the window. Two things
follow:

- At 461–760px the drawer's handle (44px, fixed at the top left) sits over a desktop top bar. The
  room for it is the app's, not the view's. Today the phone top bar keeps 44px at its left for the
  handle. That rule moves to the app, which gives the room while its handle shows, whatever the
  view's family. A phone reader in a host, or beside a 520px sidebar, then wastes no 44px.
- Everything that draws inside the view reads the view's family, never the window's phone query:
  the view's scripts (`layout.phone()`), the reach-in rules in `app-shell.css` that style the view's
  parts, and native mode, whose composer and runtime panel live in the view (`monitor.css` 11
  blocks, `monitor-controls.js` 4 reads). `native-entry.css`'s two phone blocks draw nothing in the
  view: one is the app's sidebar and menu, which keep the window, and the other the list's project
  actions, which follow the list's touch rows. In the app the two disagree only in
  the rows above that change, but there they must agree. Native mode's pickers, placed from
  `innerWidth`, wait for stage 4 with the other fixed layers.

**The pinned mdrev guest** in the right pane reads the window through its own queries, and this
repository does not change it. At a 461–760px desktop window it shows its phone toolbar inside the
pane, as it does today; in a host's narrow pane on a wide desktop it shows its desktop form. Its
embedding kit decides that.

### Units and lengths

A length in a component rule that is a share of the window becomes a share of the view, and which
share depends on what the length is for:

- **A length that keeps something on the screen** (`calc(100vw - 46px)`, the overlaid pane's
  `min(620px,72vw)`, the outline overlay's `min(310px,84vw)`, the popovers' `calc(100vw - 24px)`)
  keeps it inside the view: `calc(var(--pl-w) - 46px)` and the like, with `--pl-w` the view's width.
  In the app this changes one thing: the transcript panel at a 761–815px window narrows to fit its
  461–515px view, where today it is 470px.
- **A length that sizes a control** (the session title's cap, `min(52vw,700px)` and `45vw`; the
  focused search box, `min(460px,36vw)` and its kin) is the same share of `B`. In the app at the
  default sidebar that is the same length as today; as a share of the view it would shrink the
  focused search box at 1440px from 460 to 410px.
- **The three lengths measured from the app's sidebar** become lengths against the view:
  `calc(100vw - var(--sidebar) - 46px)` is `calc(var(--pl-w) - 46px)`, and the turn menu's
  `left: calc(var(--sidebar) + 18px)` is the view's left edge plus 18px: `--pane-x`, the view's
  left edge in the window, less the cut a contained view sets ("Drawn inside the pane"), until
  stage 4 draws the menu inside the view. In the app these are the
  same numbers, except on the rail (below).
- **Heights** (`vh`, `dvh`) become shares of the view's height, `--pl-vh` and `--pl-dvh`. While the
  view is as tall as the window, which is always in the app, those are the literal `1vh` and `1dvh`,
  so iPhone Safari's difference between the two, as its bars show and hide, is kept.
- On a phone, `B` is the view's width, so the two kinds of width agree.

### One decision, read by styles and scripts

A container query lets styles ask the view's box, but a script needs the answer before the first
render (the outline's default), and each band reads its number by family, which a container query
could say only by writing every band twice. So one module decides (`pane-layout.js`), and everything
reads its answer.

- **It measures.** It reads the component's box, its width and its height, at mount, synchronously
  before the first render, and again through a `ResizeObserver`. It hears the screen's pointer
  through a `matchMedia` listener, and the reader's text size (`--code-size`), chosen pane width
  (`--preview`) and outline choice (`navigatorOpen`) from the view.
- **It writes classes and sizes** on the component's element: the family (`pl-phone`), one class per
  condition the stylesheets use (below), the order's results (stage 3), and the lengths
  the rules read: `--pl-b` (`B`), `--pl-w` and `--pl-h` (the view's box), `--pl-vh` and `--pl-dvh`,
  `--pane-x` and `--preview-fit`. It reads the window's height for one thing only, whether the view
  is as tall as it, which keeps the window's own height units.
- **Scripts ask it**: `layout.phone()`, `layout.query(…)` for the wide phone, and its change events.
  No script that draws inside the view reads `PHONE_QUERY` itself, native mode's included; the app's
  chrome keeps its own reads.
- **After every decision it calls `paneLayoutChanged`**, which remeasures the transcript only when
  its width changed. A band moving, the outline folding and the pane narrowing all change it without
  a resize of the transcript's own making.
- **The widths it computes with** are the order's (stage 3, below): the outline's 290px track and
  its glyphs' 68px column, the 10px margin, the resizer and `M`. They are tokens on the view's
  element (`--pl-outline: 290px` and the like), which the grid and the module both read. They are
  never read off what is drawn: a folded outline draws no 290px column. A contract case holds them to
  the functions that use them.

### The rewrite of the styles

`scripts/session-view-css.mjs` rewrites every component rule under a `@media` that has a WIDTH
feature into the same rule gated by classes, feature by feature:

- a width feature reads `B`: `(max-width:1180px)` is `pl-max-w-1180`, `(min-width:761px)` is
  `pl-min-w-761`;
- a height feature reads the view's height: `(max-height:500px)` is `pl-max-h-500`,
  `(min-height:501px)` is `pl-min-h-501`;
- a pointer feature reads the screen, as today, mirrored as a class: `(pointer:coarse)` is
  `pl-coarse`, `(pointer:fine)` is `pl-fine`;
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
- **What stays `@media`**: a query with no size feature (`hover`, `pointer`, `prefers-*`), and
  everything in `reference.css`, the demo, and the app's own rules in `app-shell.css`. A short touch
  screen alone, `(pointer:coarse) and (max-height:500px)`, is rewritten like the rest, since its
  height is the view's.
- **Native mode's sheets** are not derived: their rules that draw inside the view are rewritten by
  hand to read the view's classes, and a contract case holds that none of them asks the window's
  phone query.
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

### The owner's order: what keeps its width, and what gives way

This decides how the session, the right pane, the outline and the session list share a window. The
owner set it on #s462 (2026-11-12, his words logged there), replacing the readable-line rule this
section used to propose. In his order:

1. the session, at a least width `M`;
2. what the right pane shows, at the same `M`;
3. the outline's glyphs, in a column of their own;
4. the session list, at its width;
5. the outline, at its width.

`M` is the least width of the session's column, and of the right pane's: 480px (the owner, #s472),
the session's least today (`minmax(480px,1fr)`). The session's text is 64px narrower, about 58
characters of prose. The page's prose has one size (the reading menu's − and + size code alone), so
`M` is in CSS pixels, and a browser's zoom scales it with the text.

**The rule.** A part shows where it fits beside the parts above it, each at its least, without taking
room from any of them. Whatever is left then goes to the list, up to the reader's width for it, and
the rest to the session and the pane. A right pane that cannot be a column beside the session covers
the whole view, so it uses all of the view's width and nothing below it takes room from it: the list
keeps its rail there, as his sequence has it.

**The parts.** What each takes, as the page draws it (measured on 2026-11-13 at 1600 to 820px
windows; `pane-layout.js` uses these numbers):

| Order | Part | Least width | Where it does not fit |
|---|---|---|---|
| 1 | The session | `M`, and the 10px margin at the view's right edge | the list is the phone's drawer |
| 2 | The right pane, while open | `M` beside the session, with the 1px resizer between (the 10px margin is the pane's then) | it covers the session |
| 3 | The outline's glyphs | 68px | they leave their column for the session's top bar |
| 4 | The session list | 232px, today's least | its rail, 64px |
| 5 | The outline | 290px: its 268px cards and their gutter | its glyphs' column |

**Narrowing the window** (his second follow-up): first the session and the pane give up their slack,
equally, down to `M` each; then the list narrows from the reader's width to 232; then the parts fold
in the reverse of his order: the outline to its glyphs' column (5), the list to its rail (4), the
glyphs out of their column (3), and last the pane over the session (2). **Widening** undoes each step
at the same width, so there is one layout per width whichever way the window moved.

One step may look odd. It follows from his rule that the slack goes to the session and the pane only
once every part shows at its width: when the outline folds, the list goes back from 232 to the
reader's width, and only what is left after that goes to the session. Read the other way (the list
kept at 232 until it folds), the session would take slack while the list was under its width.

**His two sequences** (#s462, 13:05), at `M` 480 with the list at its default 300 and the outline
wanted. They are generated from `pane-layout.js`'s functions, which the page runs; his letters are in
the first column.

| | Window | The pane shut |
|---|---|---|
| | under 490 | the drawer, the phone reader |
| | 490–553 | the drawer, the compact desktop under its handle |
| a | 554–621 | the rail, the glyphs in the top bar |
| b | 622–789 | the rail, the glyphs' column |
| c | 790–857 | the list narrowing, the glyphs' column |
| c | 858–1011 | the list, the glyphs' column |
| d | 1012–1079 | the list narrowing, the outline |
| d | from 1080 | the list, the outline |

| | Window | The pane open |
|---|---|---|
| | under 490 | the drawer, the phone reader |
| | 490–553 | the drawer, the compact desktop under its handle |
| a | 554–1034 | the rail, the pane covering the session |
| b | 1035–1102 | the rail, the glyphs in the top bar, the session beside the pane |
| c | 1103–1270 | the rail, the glyphs' column, the session beside the pane |
| d | 1271–1338 | the list narrowing, the glyphs' column, the session beside the pane |
| d | 1339–1492 | the list, the glyphs' column, the session beside the pane |
| e | 1493–1560 | the list narrowing, the outline, the session beside the pane |
| e | from 1561 | the list, the outline, the session beside the pane |

The narrowest window that keeps the rail is 554px: the rail, the session at `M` and the 10px margin.
#s472 and #s474 said 544, leaving out the margin.

On a touch screen (below, "The narrowest steps"), at the same defaults:

| Window | The pane shut | The pane open |
|---|---|---|
| up to 760 | the drawer, the phone reader | the drawer, the pane over the phone reader |
| 761–789 | the rail, the phone reader | the rail, the pane over the phone reader |
| 790–857 | the list narrowing, the phone reader | the list narrowing, the pane over the reader beside it |
| 858–992 | the list, the phone reader | the list, the pane over the reader beside it |
| 993–1011 | the list narrowing, the glyphs' column | as with a mouse: the rail, the pane covering the session |
| from 1012 | as with a mouse | as with a mouse |

So at the browser suite's own 1400px window, a case that opens the pane sees the outline as its
glyphs and the list at 300, where today the outline stays open beside a short line. The cases that
rely on it are listed with the stage, each with what it asserted before.

**The reader's choices** (his first follow-up):

- An expand control is disabled where the width cannot hold what it would open: the rail's expand
  (the list at 232 must fit beside everything above it) and the outline's toggle. Its tooltip says
  why. The drawer's handle always opens the drawer, which lies over the session. While the pane
  covers the session (554–1034px at the defaults) the rail's expand is disabled too, as his sequence
  has it: the list opens once the window is wide enough for it, from 790px with the pane shut and
  1271px with it open at the defaults. On a touch screen the one exception is a pane over the phone
  reader (790–992px windows, below): there the rail's expand stays enabled wherever expanding would
  open the list beside the pane, so a reader who folded the list for a wider pane can bring it back.
- A collapse the reader makes is remembered (`am-…sidebar`, `am-demo-navigator`, as today), and a
  wider window never undoes it. A fold the width forced is not remembered: it comes back when the
  window has room. Today a list or an outline the reader opened stays open at any width and squeezes
  the session (measured: a 301px session column beside the outline at a 901px window); under the
  order it folds.
- **The list's drag** takes its width from the session and the pane equally, down to `M` each, and
  folds nothing: its edge stops at 232 and 520 (today's limits), and wherever the session and the
  pane reach `M`. On a touch screen it also stops where the view would become the phone reader,
  wherever the list at 232 leaves the view over 760px, which follows from the owner's #s478.
- **The pane's drag** sets how the slack splits between the session and the pane (the owner, #s473):
  remembered, equal until the reader first drags, and each side kept at least at `M`. The layout
  writes the pane's width as `--preview-fit` on the view, from `arrangeView`: the pane's own box,
  whose track is that and its 10px margin. A `--preview` a host or a case sets on the page's root or
  `#app` still reaches the pane where the layout writes none. A pane that covers the session takes
  the view's whole width, and the layout writes no width for it. The drag keeps a share, not a width:
  the width remembered before stage 3 (`am-demo-preview`) stays in storage unread, so the split is
  equal until the reader drags again.

**The narrowest steps** (the owner, #s474: his order with a mouse, the phone's layout kept for a touch
screen up to 760px):

- **With a mouse**, the rail and the session last down to 554px. Below that the list is the phone's
  drawer, and the view, now the whole window, decides by its own width: from 490px (the session at
  `M` and its margin) the compact desktop under the drawer's handle, below 490 the phone reader. That
  moves the phone family's width edge from stage 2's 460px to 489px: a view that cannot hold the
  session at `M` gets the reader made for a narrow box, in the app and in a host alike. Every phone
  upright is 460px or narrower, so no phone changes.
- **The drawer with a mouse** opens from its handle at the top left, as on a phone, and closes on a
  click outside it or Esc. The handle takes 44px at the left of the session's top bar while it shows
  (stage 2 moved that room to the app).
- **On a touch screen** (the owner, #s475: "touch views of 760px or less are the phone reader, the
  iPad beside its list too"), a view of 760px or less is the phone reader wherever it sits: across a
  window with the drawer, or beside the rail or the list. So a finger never meets the desktop's
  controls in a box that narrow. The list follows the mouse's rules, with the two below, and a touch
  window of 760px or less has the drawer, as today. A short touch screen (a phone held sideways) keeps the
  phone's own list, as today: the drawer, or a wide phone's column. At the default list and with the
  outline open: upright, the iPad mini (744px)
  is the phone layout, as today; an 810 or 820px iPad has the list at 252 or 262px with the phone
  reader beside it, and the 834px iPad Pro the list at 276px; the 13-inch iPad Pro (1032px) has the
  list at 252px and the desktop layout with the outline. Held sideways, every iPad has the list at
  300px and the desktop layout. A touch window of 761–789px has the rail and the phone reader.
- **A tablet reader's fold** (the owner, #s478: "on touch the list narrows (to 232px at least)
  before the view becomes the phone reader"): on a
  touch screen the list narrows, down to 232 and never folding, before it would make the view 760px
  or less. Without it a reader who folds the outline on a 1032px iPad Pro gives the list its 300px
  back, the view drops to 732px and becomes the phone reader, which has no control that opens the
  outline column again. With it the list stops at 271px and the view keeps the desktop's layout, its
  glyphs and the way back. At the defaults with the outline open no iPad changes. The rule bends the
  order on touch only: there the desktop's layout ranks above the list's width beyond 232. Where even
  the list at 232 leaves 760px or less (825–992px windows with the list open), the list's own fold
  and open do change the view: folded to its rail the view is 761px or more and the desktop's, and
  opened again it is the phone reader beside the list, as #s475 has it. That is the one place a
  reader's action changes the view between the two, and the contract walks every action to hold it.
- **A pane over the phone reader** (the owner, #s480: "Leave the list beside the pane over the phone
  reader")
  on a touch screen covers the reader's area alone, beside the list or the rail as they were, as a
  wide phone held sideways covers the session beside its list (#s49). So the session keeps its width
  beneath, and opening or closing the pane remeasures nothing under it (#s25). Folding the list for
  it, as his sequence does with a mouse, would remeasure the hidden session twice, at each open and
  close: at another width of the phone reader in 790–824px windows, and in 825–992px ones into the
  desktop's layout and back. The list's own fold and open under that pane do change the session
  beneath, as they do with the pane shut (above): a reader who folds the list there gets the pane
  over the whole view, and the rail's expand brings the list back; only from 825px does the fold make
  the view the desktop's. From 993px, where the view beside the list is the desktop's, his sequence
  applies: the rail, the pane covering the session.
  So a touch window widened past 992px with the pane open folds the list: the phone layout gives way
  to his order there.
- **The glyphs in the top bar.** On a phone they float in the dock. On a desktop they join the
  session's top bar, after the title and before Info and the pane's control, the title giving way
  first. Each opens its list as a panel over the transcript's right edge, 290px wide, one at a time,
  closed by a click elsewhere or Esc, as an 820px window opens the outline today (hd-replay's research
  on #s462: Windows NavigationView's minimal mode, JetBrains' undocked tool windows, VS Code's
  title-bar toggles). A glyph in the column opens the outline at its pane where the outline fits, as
  today, and its list as the same panel where the outline does not.

**Who decides what.** The view decides its own parts from its own box (#s461): whether it is the phone
reader, the pane, the glyphs, the outline, and how the slack splits between the session and the pane
(`arrangeView`). The app decides the list from the window (`arrangeApp`), by what the view needs
beside it, which it works out with the same functions from what the view says of itself on its
element and through its handlers: whether the pane is open (`preview-off`, as today) and whether
the reader wants the outline, which the view says through `onOutline` (below; a fold the width
forced does not change it). A host with no list runs `arrangeView` alone: no drawer, no room, and the
phone reader below 490px.

The two never disagree about the outline. Folding the list to its rail frees at most 168px (232 less
64), less than the 222px the outline needs over its glyphs (290 less 68), so a view the list's fold
widened never fits an outline the list's least did not. A touch screen follows the same rule, and a
view it makes 760px or less is the phone reader, which has no columns to disagree about.

**The view asks its host for room** (the owner, #s476: "Add the optional room() mount option"). The
one thing the view cannot see is room the app could make: the outline's toggle could open where
narrowing the list toward 232 would fit it, a band 68px wide at the default list (288px at a 520px
list). So the view takes one new optional mount option, `room`, and the app passes it:

- **What it is.** A function, called with no arguments, that answers how many more pixels the host
  could give the view on its own, by narrowing what it lays beside it. A `room` that is not a function
  is refused at mount, as a `show` that is not a boolean is.
- **What its answer means.** A finite number above 0 is the room; anything else (0, a negative
  number, `NaN`, `Infinity`, a string, nothing) is none, and `arrangeView` itself reads it so. A
  throw is reported as a handler's throw is (`reportError`), never raised, and is none.
- **When it is asked.** At each layout decision: the view's box, the pane, the reader's choice. A
  room that changes while none of those does is seen at the next decision. It decides only whether
  the outline's toggle is enabled; nothing is laid out by it.
- **The app's answer.** Its list's width less 232 while the list is shown, and 0 on the rail or in the
  drawer: the list ranks above the outline, so it narrows for it but never folds for it.
- **The reader's tap.** The toggle says the outline is wanted at once (its pressed state); the app
  hears it, narrows the list, the view widens, and the outline opens by the view's own width. A host
  that answers room and then gives none leaves the toggle pressed and the glyphs shown, as a window
  too narrow for the outline does today.
- **The toggle pressed** (the outline wanted, open or held to its glyphs by the width) is always
  enabled, and a tap un-wants the outline, so a reader can stop it coming back as the window widens.
  Not pressed, it is enabled where the outline fits, with the room the host could give
  (`outlineCanOpen`): the disabled state is for opening only.
- **A host that passes none** gets the toggle disabled where the view alone cannot fit the outline.
- **How a host hears the tap** (the owner, #s479: "Add an onOutline({ wanted }) handler"): a seventh
  optional handler, `onOutline({ wanted })`, beside `onTitle`, `onTurn`, `onInfo`, `onFile`,
  `onLinkCopied` and `onToast`, through the same emitter (a handler's throw is reported, never
  raised, and what is said during the mount is held until it returns). The view says it once as the
  mount returns, with the reader's remembered choice, and then whenever the reader opens or folds
  the outline; a fold or a return the width forces says nothing, since the choice has not changed.
  A view its host mounts hidden (#s464) has no box, so its first decision says nothing of its family:
  with no choice remembered, its default (open, except on the phone reader) waits for the first
  decision that sees the box, and is said then; a default is never remembered as the reader's choice.
  `wanted` is a boolean. The app passes it and keeps the choice for `arrangeApp`, reading no class
  off the view's element; any host may pass `room` with it.

`room` and `onOutline` join `MOUNT_OPTIONS` and the doc comment over `mountSessionView`, and the
contract's mount interface cases hold them: `room`'s refusal at mount, the answers that read as none
and a throw; `onOutline` said once at the mount with the remembered choice (or, mounted hidden with none, once
the view has a box), again on each tap and not on a fold the width forces.

**Transitions.** A layout decision never glides (stage 2's `pl-holding`), and a reader's own toggle
glides whole. Opening the pane can make the list give way in the same moment, and a decision taken
then would cancel the pane's glide. So while the view's own toggle runs (its transition's time), a
decision writes without the hold, and the case asserts no `transitioncancel` on the toggle.

**Tests.** The contract sweeps `pane-layout.js`'s functions over every window from 320 to 2400px:

- the inputs: the pane shut and open, the outline wanted or not, the list at 232, 300 and 520,
  folded or not, a mouse, a touch screen and a short one, `M` at 480 and 560, the room at 0 and at
  the list's give, and the pane's share at a half, 0, 0.3 and 1;
- the checks, against the rule restated on its own: each part shown where it fits beside the parts
  above it and nowhere else; the list's width; the drawer and the phone reader exactly where they
  belong; all the slack given, the pane's by its share and equal at a half; and each expand control
  enabled exactly where it fits;
- a wider window never shows less of what ranks higher in his order, whatever the reader chose,
  among the desktop's layouts (the phone reader has no columns to rank);
- every reader action at every state (the outline folded or opened, the list folded or opened, the
  pane opened or closed): the control that undoes it is there and enabled afterwards, and the view
  changes between the phone reader and the desktop's layout only as the list folds or opens on a
  touch screen, in 825–992px windows;
- under a covering pane, the session's layout with the pane shut, and no width for the pane;
- the numbers it is built from, written out (`M` 480, the outline 290, the glyphs 68, the margin 10,
  the resizer 1, the rail 64, the list's least 232, the touch screen's 760, the narrowest window
  554), and both of his sequences, the touch screen's with the pane shut and open, and a tablet
  reader's fold, step by step at their windows: the tables above, written out;
- the edges the sweep's heights and inputs do not reach: a view 499, 500 and 501px tall, rooms and
  shares out of range, and the functions' defaults.

Each of thirty mutations of the functions fails it, among them each number above changed, the
touch clause and a short mouse window taken as touch, the height's edge moved either way, the pane
taking none of the slack or the share ignored or unclamped, the outline's control always enabled or
the room ignored or unchecked, the list open under a covering pane, the phone family at 460, the
reader's outline choice ignored by the view or the app, the defaults changed, the tablet's fold
rule left out, off by one or applied to a mouse, the list folded for a pane over the phone reader or
kept beside it on a mouse, the rail's expand read with the pane, and the touch floor of an earlier
draft. The page's widths are held to the functions' numbers (the outline 290, the glyphs 68, the
margin). In the browser: each step of both sequences at its window in the app, the drag limits, a
collapse remembered across a widening, an expand disabled where it cannot fit, the pane's glide
whole when the list gives way, and on a touch screen a pane opened and closed over the phone reader
beside the list with the transcript's turn and its measured heights unchanged.

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
  It takes the same transition, for the pane. Stage 3 sets these tracks from the order's functions
  instead: the session and the pane by their share of the slack, or the pane over the session.
- **Its width is its track's, never its content's**: `contain: inline-size`. With the pane open its
  content's minimum is 901px (480 + 1 + 420), which in an `auto`-minimum track or a flex item would
  hold a 1100px window's view at 901px, and so `B` at 1201, and keep the layout that made it so.
  Containment of the inline size captures no fixed layers and forms no stacking context.
- **Its height**: `height: 100%; min-height: 0; grid-template-rows: minmax(0,1fr)`, which `.app`
  carries today. On a phone, where `.app` is a block, the view takes the whole height.
- **Unpositioned.** The view's box stays static in stage 1, so every absolutely placed part keeps
  its containing block. Stage 4 changes that on purpose.
- **The wide phone.** `#app:not(.phone-column-off)>…>.workspace{grid-column:2}` becomes the view
  in column 2 and the workspace in the view's column 1.
- **The reach-in rules** keep reading `#app`'s copies of the pane's states in stage 1. In stage 2
  the one under a size query, the demo's z-index for the overlaid pane at `≤1180`, moves into the
  view, keyed by its own classes and the pane state its element carries; the rest read no size
  (the rail, a drag) or are the app's (the handle's room, the wide phone's column). The copies go
  once nothing reads them.
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

**The phone reader takes it in stage 2.** Stage 2 is what makes a phone reader that is not the
screen: beside a 520px sidebar, beside the rail, beside a wide phone's column, in a host's pane. Its
first cut placed that reader's dock, sheets, Info, menus, dims, reply box and right pane against the
window, over the sidebar (the shut pane even lay over its title), and kept them beside a wide phone's
column with app rules, one per layer. So a phone reader narrower than the window contains its layout
(`pl-boxed`, which `pane-layout.js` sets from where the view's box sits in the window; it places the
layers and picks no layout), and the fixed layers drawn inside its element land in its box. Measured
before choosing, in Chrome, at 900/520 and on a 932×430 phone with its column and a 59px island:
every layer inside the view; the list, the handle, the title and each opened layer take a press at
their middle; and nothing else under the view moves but the fixed layers (on the wide phone, those
the app's rules had already set beside the column, now set by the view's own).

What follows from it:

- **The layers' origin is the view's edge.** The derivation writes the screen's insets as only the
  part of each that falls in the box, `max(0px, inset - cut)`, and an edge at the sidebar as the
  view's left edge less the cut, where the containment sets the cuts, `--pl-cut-left` and
  `--pl-cut-right`, to how far the view's edges are from the window's (`--pane-x`, `--pane-r`; the
  right edge's measured to the layout viewport, so a classic scrollbar is not counted as page).
  Uncontained, the cuts are 0 and every length is the window's, as before.
- **Its stacking is its own.** In the app one thing of the app's is drawn over a boxed reader: a
  wide phone's handle, by the title. It stays above the view, the right pane included, as designed;
  while a layer that rises over the top bar is open (the dock's sheets, a dialog's card) with the
  pane shut, the handle hides, since none of its z can go under a layer it no longer shares a
  stacking with. Over an open pane it stays.
- **The whole screen is the screen's.** At ⤢, held sideways, the view lets go of its layout, so the
  pane covers the window, a wide phone's column included, as it did, and the pane alone takes the
  screen's whole insets; the session's own fixed layers under it keep theirs. The review sheet (#s12,
  the owner's full-screen sheet) is the screen's too, but the view keeps its layout for it: it opens
  over the open pane, which would move to the window's edge and reflow mdrev under the sheet. So the
  sheet reaches the window's sides from the view's edges (`--pane-x`, `--pane-r`) with the screen's
  whole insets, and while it is open the view's stacking rises to the sheet's level, 95: over a wide
  phone's column, the sidebar beside a narrow view and the handle, under the toasts. Its height is
  the view's, the app's whole height. The rules read the view's own classes, so they hold exactly
  where its layout is contained (a phone reader in a box), whatever the window. At ⤢ the view no
  longer contains its layout, so the sheet is the window's from its own edges, with the screen's
  whole insets. Reaching out from the box is the app's choice (its rules are in the app's
  stylesheet): in a host the sheet is drawn inside the pane like the rest of the view, the insets
  counted where they fall in it, and the view's stacking left alone, since a sheet reaching out over
  a host's page is the host's to ask for (stage 4); at ⤢ it is the screen's there too, as the pane
  is. The app's rules read the pane as open, since the sheet opens only from the open pane's Review
  control; an opener with the pane shut would need them changed. It is the open session's document,
  so it closes with the session.
- **The shut pane hides once its glide out ends.** It parks past the view's right edge, which in a
  host's pane may be over the page beside it. A phone's pane never glides, so there it opens and
  hides at once.
- **Not every layer is inside the element.** The task card and its dim go in the page's layer
  (`viewRoot.layer()`), outside the view's element, and stay the window's until stage 4.

A phone reader as wide as the window is not contained, so a phone is unchanged. The desktop's
layers, placed by scripts from the window, and the app's own box stay stage 4's.

### What stays the host's

The host keeps the drawer and its handle, with the room the handle takes at the top bar's left; the
rail; the sidebar's width and its `sidebar-tight`; the app's grid; and a wide phone's column beside
the session, with the one rule that has the right pane cover the view there and the one that hides
the handle while a layer of the view rises over the top bar. It lays the two components out against
each other, by the window: the app's chrome is the one part of the page
that still reads it. Native mode is the standalone app's alone, but what it draws inside the view
reads the view's family (above).

### The session list

`design/embedding.md` asks for full rows where there is room and compact rows where there is not.
On a desktop a tree's session row is already one line of 32px: the title cut with an ellipsis, a
fork chip, and at its end the state mark with the age; the flat list's row adds a second line, the
agent's mark and the project.
On a phone they are 44px touch rows. The list follows the same rule as the view, its own size and touch:

- **Touch rows on a touch screen**, `(pointer:coarse)`. The list's own size cannot tell a phone's
  drawer from a desktop's sidebar (both are about 300px wide and the window's height), and a finger
  needs 44px wherever it is. In the app this changes two places: an iPad's sidebar gets 44px rows,
  and a desktop window of 760px or less gets 32px rows in its drawer.
- **Compact rows** are a band of its own width: below 232px, the app's minimum sidebar, so the app
  never draws them. There the flat list's project name goes and the agent's mark stays, and a
  session's indent narrows from 32px to 20px. The same module measures the list's box and sets the
  class (`pl-max-w-231`) while the tree is shown; on the app's 64px rail the tree is hidden and the
  class is not set.
- Its stylesheet's other size rules are the phone's and two stray copies of the view's
  `--navigator`, which go. `sidebar-tight` belongs to the app's head, not to the list.
- Its floating layers (the filter sheet, the section menu, the rail flyout) keep reading the window
  until stage 4 draws them in the pane.

### What the app changes

At the default text size, the pane at the reader's width:

- **The 126px column** goes: by the owner's order (above), a window too narrow for the session and
  the pane at `M` each folds the outline, the list and the glyphs first, then lays the pane over the
  session. At the default list with the pane open, the list narrows below a 1561px window and the
  outline folds to its glyphs below 1493px.
- **The glyphs' column keeps one width**, 68px at every width, where today it narrows to 62px at a
  view of `B` ≤1180 and 56px at ≤900. The outline's track is 290px at every desktop width today
  (measured), and stays so, or folds. Its narrower values in the stylesheets (`--navigator` 256px,
  `min(264px, 82%)`) are the card's own and stay as they are; at `B` ≤900 the outline is an overlay
  today, and under the order it is folded there instead.
- **A mouse window of 554–760px**: the rail and the session, the glyphs in the top bar or in their
  column, where today the phone reader; 490–553px the compact desktop under the drawer's handle;
  below 490 the phone reader.
- **A touch view of 760px or less beside the rail or the list** (an 810–834px iPad upright, split
  view, a foldable open): the phone reader (#s475), where today the desktop layout squeezed beside
  the list. A touch window of 760px or less is the drawer and the phone reader, as today.
- **A wide sidebar in a narrow window**: with a 520px sidebar the list narrows toward 232, then folds
  to its rail, before the session goes under `M`. Today at 761–980px the desktop layout is squeezed
  beside it, and at 1181–1340px the grid runs past the window.
- **The rail**: the view reads its own width, 64px less than the window, so at 981–1180px the right
  pane is a column beside the session, where today it lies over it; stage 3's order decides how much
  room it takes, and below a 1035px window lays it over the session again. The overlaid pane starts 10px inside the view, not over the rail. The
  turn menu and the two other lengths measured from `--sidebar`, which on the rail still holds the
  hidden sidebar's width (so today the turn menu opens 254px into the session), move to the rail's
  edge. The empty column under the open pane at `≤1180` goes.
- **A wide phone held sideways with its list column shown**: the layout of a 676px phone held
  sideways, where today the wide phone's. Measured on a 932×430 phone, every computed style under
  the view before and after; 41 of its 604 elements change:
  - the dock spans the session's column, Aa at its left and the search at its right, where the
    wide phone kept both together at the right; the Aa menu opens from Aa's side;
  - the session's menu is the narrow phone's popover under the top bar, not a menu hung from the
    title;
  - the status chip is capped at 105px (the `≤700` tier), and the title's row keeps its place;
  - the shut right pane is the view's width, off the screen as before.

  Three of those layers and the shut pane are fixed to the screen, and at first they landed over the
  list column (the dock's Aa, the Aa menu and the session's menu; the shut pane kept about 250px on
  the screen, over Info). The view now holds its own fixed layers there (below, "Drawn inside the
  pane"), and the app's rules that set them beside the column went, all but the pane's, which says
  the pane covers the view. `a_wide_phone_held_sideways_keeps_its_sheets_beside_the_column_and_its_head_whole`
  checks each. The island's inset is the column's, so the view's controls keep only what of it falls
  in the view's box: none, beside the column.
- **The list's rows**: 44px on any touch screen, 32px with a mouse.
- **The transcript panel** at a 761–815px window narrows to fit the view, where today it is 470px.
- **The drags**: the list's stops where the session and the pane reach `M`; the pane's sets its
  share of the slack, each side kept at `M`.

## Tests

- **The column, red first.** The app with the right pane open at 1181 and 1280px: the session's
  column is at least `M`, and the pane ends inside the window. This is red on 34c7eb50 at both, and
  1600, where everything shows before and after, is the control.
- **Widths in a host.** The view and the list each in a host fixture's pane that is smaller than
  the window and does not start at its edge: 380px at x=500 in a 1400px window, 700px, 1000px,
  1100px, light, and dark from stage 4, where containment is checked. In stage 2 each asserts its family, condition classes and `B` (380 the
  phone reader with `B` 380; 700 `B` 1000; 1100 `B` 1400). From stage 4 each also asserts that
  nothing overflows the pane, and that every control is inside it and is the element
  `elementFromPoint` finds there; a rect alone proves nothing.
- **On an emulated phone.** A 390×844 phone with the host's pane the whole screen (`B` 390), and a
  932×430 phone held sideways with a 450px pane (the phone family by its height, `B` 450). Each
  asserts its family and classes.
- **The app reads what the window read** wherever the table above says "the same": a 1440, 1180,
  1000 and 820px window at the default sidebar, an iPad-sized touch window of 834×1194 with its
  sidebar, a phone upright and held sideways, and a wide phone with its column hidden. In each the
  view's classes match the window's media exactly as the old queries read it.
- **The app where it changes**, each red on the old code: a 700px mouse window (the compact desktop,
  the drawer's handle clear of the title), a 900px window with a 520px sidebar (the phone reader
  beside the sidebar, no empty 44px at its left, and each of its layers opened in turn inside its box
  with the list still taking a press, but a review sheet over the whole window), a 1000px window on the rail (the pane a column), a wide phone
  with its column and its island (its listed changes, the same layers inside the view, the dock 12px
  in from the view's edge, ⤢ covering the column too), the same phone in Safari's 330px (Aa risen
  over the top bar, and a review sheet over the whole screen, column and handle included, its close
  control inside the island's inset, over the open pane and at ⤢), and the list's rows on a touch
  screen. Each layer step first
  checks that its layer opened. The host's 380px phone reader keeps its shut pane out of the page
  beside it. A host's 450px pane on a phone held sideways, at the screen's left edge, its middle and
  its right edge, keeps its dock 12px from each side of its own and the island's inset only where the
  island falls in the pane, and so does a review sheet there, which at ⤢ takes the whole screen with
  the pane; and with a classic scrollbar the view's right gap stops at it. A Back to another session closes the review sheet.
- **Mutations.** Point the layout module back at the window, drop a condition from the rewrite's
  table, give the family the window's height, or let the order read what it drew: a width
  case goes red, or the derivation fails.
- **The contract.** The component stylesheets carry no `@media` with a width or height feature.
  Every class the derivation writes is one the module sets. No component script, and none of native
  mode's rules that draw inside the view, reads `PHONE_QUERY`.
- **The app's suite**, whole, at every stage, with each change named before it lands: the filter
  popover case at a 680px window (`browser_follow.rs`, the phone reader today), the wide phone's
  column cases, the cases that open the right pane at a 1181–1394px window, the case that draws a
  760px pane at 1440 (`files.rs`, which today runs the grid 101px past the window), the rail's turn
  menu, and the drags at their limits. Each is listed with what it asserted before.
- **The Simulator walk.** The app on a phone upright and sideways, a wide phone with its column
  shown and hidden, and the host fixture with a narrow pane on a phone, light and dark.

## The order

Each stage is page code: the whole local browser suite and an iOS Simulator walk before it lands.

1. **A box for each component**, as above. No threshold moves; the suite is unchanged.
2. **One decision** (#s461). `pane-layout.js` (the family from the view's own size and the screen's
   pointer, and `B`), the derive script's rewrite and its table, the units, the tokens, the scripts
   asking the module, native mode's rules inside the view, the reach-in rules moved into the view or
   kept as the app's (the handle's room, the wide phone's column), the list's rows, and the width
   cases in the host fixture and the app. It lands with stage 3: on its own it would give the rail at
   981–1180px windows the short line that 1181–1394px windows have today. A layout decision never
   glides, at the mount or later (`pl-holding`, `sv-mounting`).
3. **The owner's order** (#s462, #s472–#s474). `arrangeView` and `arrangeApp`, the classes they set,
   the folds, the drags, the disabled expands, the narrowest steps, the touch family (#s475) and the
   `room` mount option (#s476), with the red case. `phoneView` replaces stage 2's `isPhone` wherever
   the family is read (`classesFor`'s `pl-phone`, `bandWidth`), and inside the family `B` is still the
   view's width: the views new to the family (461–489px on any screen, and 490–760px touch views
   taller than 500px) read their bands as a phone of that width does.
4. **Drawn inside the pane.** Containment, the layers in the box's coordinates, the width cases'
   containment checks on a desktop and an emulated phone, and the Simulator walk of a narrow pane.
   The phone reader narrower than the window is contained from stage 2 (above); this stage takes the
   desktop's layers, a host's vertical offset and the app's own box.
   The lengths that fit a fixed layer to the window (`calc(100vw - 24px)` and its kin in the task
   card, the sheets and the menus) take a unit of their own there: `1vw` while the box is not
   contained, the pane's width over 100 once it is. Stage 2's shares of `B` would size them to `B`,
   which in a host's 500px desktop pane is 800px.
