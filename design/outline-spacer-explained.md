# How the outline column works

The app shell's left column, as built for #157. Short on purpose.

## One number per pane

`drawers.open` is a fraction from 0 to 1 — how far that drawer is pulled out. Its body is
painted at `B × open`, where `B` is the list's natural height. `drawers.dir` remembers which
way it last moved. **Both controls write that one number**, which is what stops them
disagreeing.

## The input is the reader's push, not the scroll position

```
┌─ at rest ───────────┐    ┌─ push 200 ──────────┐    ┌─ pull 200 ──────────┐
│  Outline            │    │  Outline            │    │  Outline            │
│  v Turns    B = 145 │    │  v Turns    B =   0 │    │  v Turns    B = 145 │
│  v Tasks    B =  97 │    │  v Tasks    B =  42 │    │  v Tasks    B =  97 │
│  v Agents   B =  97 │    │  v Agents   B =  97 │    │  v Agents   B =  97 │
└─────────────────────┘    └─────────────────────┘    └─────────────────────┘
```

- **Push down** — spend it closing, starting at the topmost pane with anything still open.
  Only what is left over scrolls the column.
- **Pull up** — give the scroll back first, then reopen from the bottom. A push and an equal
  pull cancel exactly.

Which pane a delta reaches is read off the states of *all* of them, so a pane the toggle shut
is simply a pane at 0, found by the walk like any other.

**Why not the scroll position.** Closing a drawer removes content, and a scroll offset only
exists because there is content to scroll — so spending `scrollTop` eats the room it needs to
keep going. The previous version compensated with an invisible spacer that re-added the height
the bodies gave up, which froze the column's scroll extent, which capped the state at what the
extent could hold: three shut panes wanted 584px of offset where only 386px existed, and they
could never be scrolled back open. A push has no such limit. The spacer is gone.

## The pile is the browser's

```css
.session-navigator > .outline-caption { position: sticky; top: 0 }
.session-navigator > .outline-card    { position: sticky; top: var(--slot, 0px) }
```

`--slot` accumulates only the heads and gaps above each card, and each card carries a z-index
above the one before it, so a lower card paints over its neighbour. Scroll and the heads pile
up at the top on their own:

```
┌─ all open ──────────┐    ┌─ all shut ──────────┐
│  Outline            │    │  Outline            │
│  v Turns   [pinned] │    │  v Turns   [pinned] │
│      turn 01        │    │  v Tasks   [pinned] │
│      turn 02        │    │  v Agents  [pinned] │
│  v Tasks            │    │  v Session [pinned] │
│      #157           │    │                     │
└─────────────────────┘    └─────────────────────┘
```

No JavaScript sizes anything for this. It is why a head never moves while the drawer under it
closes — the point of the owner's review note, and it is exactly right.

## The toggle

At either end it flips: 0 → 1, 1 → 0. Part-way it finishes the movement the pane was last
making (`dir`); with no direction to follow it makes the bigger visual change — a pane mostly
open closes, a pane nearly shut opens.

## The two bugs this design cannot have

- *A pane toggled shut cannot be reopened by scrolling.* It is a pane at 0; a pull finds it.
- *A pane shut by scrolling will not open from its head.* The head reads the same number the
  gesture wrote, and at 0 the toggle flips it.

Both are browser cases (`the_app_shell_reopens_a_toggled_shut_pane_by_pulling`,
`the_app_shell_opens_a_pushed_shut_pane_from_its_head`), confirmed red on the old engine.

## Consequences worth knowing

- The column's scroll extent **shrinks by exactly what closed**. That is the honest height of
  the content; nothing stands in for the space a body gave up.
- Dragging the scrollbar scrolls what is left; it does not close drawers. Closing is a push.
- `stackOutlineHeads` is the only thing that measures `B`, on a render and on a resize. The
  gesture path reads no layout at all, so a pane can never appear to move because its `B` was
  re-measured underneath it.

