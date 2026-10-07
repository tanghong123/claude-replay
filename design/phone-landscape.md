# A phone held sideways (#s35)

**Status: the owner chose two columns on a wide phone and a top bar that never hides (2026-10-07,
§7); the find bar at the keyboard is shown step by step in the mockups and awaits the owner's
answer. Nothing is built yet.**
Since #s32 a phone held sideways gets the phone layout, and since #s33 that layout stays on the
screen. The owner's verdict on 1.357.0: "kind of works, but not ideal", with five issues and a
request for "a proper design session to think through them holistically", leaning on how other
apps do it. The mockups are `design/phone-landscape-mockups.html` (synthetic content, drawn at the
phone's true size with its island and corners): open it on the phone and turn the phone sideways.

## 1. What is wrong, measured

The owner's phone is the largest iPhone, sideways: **956 × 440 points**. Everything below was
measured on 1.357.0 from the owner's screenshots and in WebKit at that size.

1. **The Dynamic Island covers content.** The page asks for the whole screen
   (`viewport-fit=cover`) but its CSS reads only the TOP and BOTTOM safe-area insets. Sideways, the
   insets that matter are LEFT and RIGHT, about 60pt each, and nothing respects them: the drawer
   handle, the transcript's fold chevrons and the start of every line sit where the island is.
2. **The rounded corners clip controls.** The right pane's count badge sits 12pt from the right
   edge and 6pt from the top, inside the top-right corner's curve. The drawer handle's 44pt box
   starts 4pt from the left and 2pt from the top, so its outer corner is cut by the top-left one.
3. **The lines are far too long to read.** Prose is 15.5px across 926px: **about 119 characters a
   line**. Comfortable reading is 45–75 (typography's long-standing range, and Apple's guidance).
   The extra width buys nothing but eye travel.
4. **Navigation moved to the other side.** Portrait puts Turns, Tasks and Agents at the LEFT of the
   bar's second row. Sideways, #s32 folded the bar to one row, and they landed on the RIGHT, after
   the title. (The owner's "quick navigation control" is those three icons: confirmed by the owner,
   2026-10-07.)
5. **Search fights for the top.** The open search is an overlay across the whole bar, and it lies
   over the drawer handle. With the keyboard up, the transcript has **136pt**: the space between
   the turn bar, which ends at 81pt, and the keyboard's ↑ ↓ ✓ bar, which starts at 217pt. That is
   under a third of the screen. The search's own ↑ ↓ sit at the top while the keyboard's ↑ ↓,
   which move between form fields rather than between results, sit right above the keys.

The list is not exhaustive (the owner said as much), and the screenshots show two more:

6. **Two bars stand at the top.** The top bar (51pt) and the transcript's sticky turn bar take
   **81pt together, 18% of a screen whose height is the scarce side**.
7. **Jump-to-latest sits on the text.** The ↓ button is centred at the bottom, over the middle of
   the line being read.

## 2. What a phone held sideways is

Two facts decide most of this.

**Its height is compact, and on the largest phones its width is REGULAR.** iOS describes every
screen by two size classes. A Plus or Max iPhone held sideways is *regular width, compact height*.
That is the iPad's width class, and the only phone layout with it. Any other iPhone held sideways is
*compact width, compact height*. Apple's own two-level apps switch layout on exactly this line.

**The screen's edges are not all usable.** On these phones the island sits at one side and the
corners are rounded, so iOS reports a safe area. Apple's rule is that content and controls stay
inside it, while backgrounds run to the edges so the screen still looks full.

### What other apps do, and what to take from each

| App, held sideways | What it does | What this design takes |
|---|---|---|
| Mail, Messages, Notes, Settings, Files on a Plus/Max phone | Two columns: the list on the left, the selected item on the right. On a smaller phone, the item alone | The wide phone shows the session list and the transcript side by side; a narrower one keeps the drawer |
| The same apps' iPad sidebars | One sidebar switches between sections (Mail's mailboxes, Files' locations) | One left column that switches between Sessions, Turns, Tasks and Agents |
| Books, Kindle, Safari Reader | The text column is capped at a reading width; the extra width becomes margin | The transcript is capped at about 70 characters |
| Every Apple app in landscape | A shorter navigation bar; no large titles | One compact bar, about 44pt, carrying the turn as its subtitle |
| Safari, Books | The bars slide away while reading and come back on a scroll up or a tap at the top | Phase 2 (§5): the bar hides while reading |
| Safari's Find on Page | The find bar sits right above the keyboard, with the field, "3 of 12", ↑ ↓ and Done; the page stays where it is | Search becomes a find bar at the keyboard; nothing overlays the top bar |
| Messages, Slack | "Jump to latest" floats at the bottom right, clear of the text | The ↓ moves to the bottom right, inside the safe area |

## 3. The recommendation: two columns on a wide phone, one reading column on a narrower one

### 3.1 The safe area, everywhere sideways

Every control and every line of text sits inside `env(safe-area-inset-left/right/bottom)`; the
bar's, the column's and the transcript's backgrounds still run to the screen's edges. The insets
come from `env()`, never a fixed number, because each phone model has its own (and the island
side changes with the direction the phone is turned). This alone fixes issues 1 and 2.

### 3.2 A wide phone (sideways at 900pt or more: the Plus and Max models)

- **A left column, always there.** It is about 300pt of content plus the left inset: the drawer,
  docked instead of floating, with no scrim.
- **A switcher at its top: Sessions · Turns · Tasks · Agents,** each with its count as today. The
  column shows the same live lists the phone's drop-downs borrow from the outline today, so one
  renderer and one click handler still serve every layout. Navigation is now on the LEFT, where
  portrait has it and where the desktop keeps its outline (issue 4). Choosing a turn, a task or an
  agent moves the transcript and leaves the column where it is, as Mail does.
- **The transcript gets what is left:** about 530pt, 65–70 characters a line. The width that made
  lines too long now holds the navigation (issue 3).
- **The handle hides and shows the column.** With it hidden, the transcript is centred at its
  reading width; it does not stretch back to 119 characters.

### 3.3 A narrower phone sideways (under 900pt: every other iPhone)

There is no room for two columns, so the session list stays a drawer over a scrim. The transcript
is a centred column, capped at about 70 characters; the leftover width becomes margin. Turns, Tasks
and Agents sit right after the handle, at the LEFT of the bar, as in portrait. Their drop-downs are
today's.

### 3.4 One compact bar

In landscape the top bar and the turn bar merge into one bar, about 44pt tall (issue 6):

> ☰ │ ⌸ ✓ ⌘ (narrow phone only) │ **session name** ▸ Turn 77 — the turn's first words … │ 🔍 Aa ⓘ ▣

The session's name is the title and the current turn is its subtitle, the way iOS bars carry a
subtitle. A tap on the subtitle opens the turn menu the turn bar opens today. The chrome goes from
81pt to 44pt.

### 3.5 Find on Page, at the keyboard

The search icon opens a **find bar docked right above the keyboard**: the field, "3 of 12", the
result arrows and Done. The top bar stays where it is (the owner: "keep the top bar"), and nothing
lies over the drawer handle any more, because the search no longer lives at the top (issue 5).
The arrows sit next to the count, so they read as result steppers and not as the keyboard's
field-to-field arrows. The page cannot remove those (§6). When the keyboard closes, the find bar
stays at the bottom, above the home indicator, with the count and the arrows, until Done, so the
reader reads and steps with one thumb. That is Safari's behaviour too. The query language (#367) and the chips (#353) are unchanged; only
where the box lives changes.

With the keyboard up and the top bar kept, the transcript has about what it has today (129pt
against 136): a keyboard held sideways takes most of the height whatever the page does. The gain is
WHERE the controls are (the field, the count and the arrows together, right above the thumbs) and
what is left once the keyboard closes (the whole height, with the arrows still under a thumb). The
mockups' "Find on Page at the keyboard, step by step" shows it in portrait and held sideways.

### 3.5a The owner's idea: the toolbar at the bottom, in a pill

The owner (2026-10-07): "move the toolbar to the bottom and put them in a pill instead of docked at
the bottom which would collide with the home bar". It is iOS 26's own pattern, and it makes §3.5
simpler rather than adding to it:

- **Precedent.** In iOS 26, Safari's bottom bar is a back circle and a pill (share, reload, compass);
  Mail, Notes, Music and Photos float their tab bar as a capsule with search as a separate circle
  beside it, which grows into a field above the keyboard. Controls float in a glass layer inset from
  the edges and lifted above the home indicator, and the content scrolls under them.
- **What moves.** Portrait's second top row (Turns, Tasks, Agents, search, Aa) goes into a floating
  pill above the home indicator, with search as its own circle at the right and ↓ floating above
  it. The top keeps ONE row: ☰, the session with the current turn as its subtitle (§3.4), ⓘ and ▣.
  It never hides (the owner's decision 3). That gives back about 70pt at the top in portrait.
- **Panes become sheets.** Turns, Tasks and Agents open as sheets rising from the pill, by the thumb
  that tapped them, instead of drop-downs from the top. They are the same live lists (one renderer,
  one click handler).
- **Search is the pill's other state.** The search circle grows into the field above the keyboard;
  with the keyboard closed, a find pill takes the toolbar pill's place. It holds the funnel, the
  chips, the count and the arrows, and ✕ brings the toolbar back. Filtering keeps everything it has
  today. The query language and its chips are unchanged, and the funnel opens the same filter
  (User messages, Agent replies, Thinking, Tools and each tool, Whole words) as a sheet above the
  pill, so the count is in view while the reader ticks. Nothing sits on the home indicator any
  more, which is the collision the owner saw in the flat bar.
- **Sideways.** On a narrower phone, the same pill as portrait. On a wide phone, the left column
  already holds Sessions, Turns, Tasks and Agents, so the pill carries what is left (Aa) beside the
  search circle, at the bottom right of the transcript.

What it costs, named in advance:

- **The pill floats over the transcript.** The transcript's end gets bottom padding the pill's
  height, so its last line, and a reader following the tail, rest above the pill, never under it.
  That is a static change to the scroller's padding, outside the touch-glide rules of #372.
- **The compose box shares the bottom.** When write mode offers the compose box, the bottom is the
  composer's, as in Messages. The toolbar tucks into one circle beside the composer, and a tap opens
  the pill above it. This needs its own mockup before it is built.
- **Portrait changes, not only sideways.** This reworks the phone toolbar that #313, #352 and #353
  shaped. Their cases move with it: a control's place changes, its behaviour does not.

### 3.6 Jump to latest

The ↓ (and its "N new" pill) floats at the bottom RIGHT, inside the safe area: clear of the text
column on both kinds of phone (issue 7).

### 3.7 The landscape layout's state is its own

#s33's lesson: a choice made in one layout must never silently govern another. The wide phone's
column being shown or hidden is its own remembered key, not the desktop's collapsed rail
(`am-demo-sidebar`) and not the drawer's `mobile-detail`. Turning the phone to portrait and back
restores each layout as it was left.

## 4. Alternatives considered

- **A. One reading column at every width sideways.** That is §3.3 on the Max phones too: a smaller
  change, and it fixes issues 1–7. But on the phone that has the width, the width becomes empty
  margin, and the session list and the outline stay one or two taps away behind a scrim. Every
  Apple two-level app on that phone does the opposite.
- **C. An immersive reader.** All chrome hides until a tap, as in Books or Kindle. It is the best
  for pure reading and the worst for a monitor: a live session's state, its new records and the
  compose box disappear. It does not fit an app whose job is also watching.
- **Hiding the bar while reading,** as Safari does, is not in the first step (§5).

## 5. Phases

1. **The layout:** the safe area (3.1), the two kinds of phone (3.2, 3.3), the compact bar (3.4) and
   jump-to-latest (3.6). Pure layout and the switcher, with no change to the viewport engine.
2. **Find on Page at the keyboard** (3.5). The keyboard is tracked through `window.visualViewport`
   (iOS does not move a `position: fixed; bottom: 0` element above the keyboard on its own). That
   is iOS-specific behaviour and needs the real device or the Simulator to confirm.
3. ~~The bar hides while reading~~: dropped, the owner keeps the top bar (§7). Had it been built,
   hiding the bar changes the transcript's height in the middle of a touch glide, exactly the kind of
   write #372 keeps out of one.

## 6. Constraints and risks

- **The keyboard's ↑ ↓ ✓ bar cannot be removed from a page.** iOS Safari shows it for every text
  field. The find bar is designed around it, not against it.
- **Each phone has its own insets,** and the island's side flips with the direction the phone is
  turned. Only `env()` gets that right.
- **900pt is the line between the two kinds of phone,** and it is Apple's own: today's Plus and Max
  models measure 926–956pt sideways, all the others 844–874.
- **Portrait is unchanged.** Whether portrait's search should become a find bar too, for
  consistency across a rotation, is a question for after the owner has used it sideways.

## 7. Decisions for the owner

1. **Two columns on a Max phone, or one reading column everywhere (A)?** Decided (2026-10-07): two
   columns.
2. **The find bar at the keyboard, sideways only, or also in portrait?** The owner asked to see it
   first (the mockups show it step by step), asked whether filtering survives (it does, §3.5a), and
   proposed the toolbar at the bottom in a pill. The question is now: **the bottom pill (§3.5a), in
   portrait and sideways (recommended)?** If yes, search docks at the keyboard everywhere as the
   pill's other state, and the flat find bar of §3.5 is not built.
3. **The bar that hides while reading?** Decided (2026-10-07): no, the top bar stays. Phase 3 is
   dropped.

## 8. How it will be held

- **The safe area is emulated headless.** Chrome's `Emulation.setSafeAreaInsetsOverride` feeds
  `env()` (checked on Chrome 154: 62 / 62 / 20 read back under `viewport-fit=cover`). The cases run
  a 956×440 phone (wide) and an 852×393 one (narrow) with landscape insets.
- **Predicates, in the audit's style, not selector lists:**
  - every visible interactive element's rectangle lies inside the safe rectangle;
  - the transcript's prose runs at most about 75 characters a line;
  - on the wide phone, Turns, Tasks and Agents are reachable in the left column, and choosing a
    turn moves the transcript without closing the column;
  - on the narrow phone, those three icons sit in the bar's left half;
  - the jump control and the find bar cover no text line's middle.
- **Each layout's state is separate:** a case shows the column, turns the phone to portrait, and
  finds portrait's drawer shut; it collapses the desktop rail, and finds the wide phone's column
  untouched.
- **On a real phone,** the owner confirms phase 2 (the keyboard), which no emulation reproduces.
