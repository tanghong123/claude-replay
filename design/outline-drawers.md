# The outline column as drawers

*Owner's model, 2026-09-06, for review and comment. Nothing is built yet — this document is the
thing to correct before code. It follows `one-engine-two-pages.md` in the same series: state the
model, draw the arithmetic, name what it costs, ask the questions that change the build.*

---

## What was asked for

> "The implementation of the sliding and collapsing/expanding of outline panes are still not
> correct. You may imagine that each of those panes are like a drawer, it already has this
> open/close toggle, but with sliding, it is a smooth close operation. And when it is partially
> closed, clicking the toggle would close it when the part still visible is more than 50%, and
> expand it when the part is less than 50%. When closing, always close the top 'drawer' first,
> and then second, and third, etc. Keep the same amount of space between panes — those are
> invisible but rigid space."

Four rules, and they are all one idea: **the column is a stack of drawers, and scrolling it is a
gesture that closes them.**

| # | Rule | What it replaces |
|---|---|---|
| 1 | Openness is **continuous** — a drawer can sit anywhere between open and shut | a boolean per pane (`uiState.navCards`) |
| 2 | **Scrolling closes** drawers smoothly, **top one first**, then the second, then the third | scrolling moves a viewport over fixed-height cards |
| 3 | The **toggle snaps to the nearer end** — >50% showing closes, <50% opens | the toggle flips the boolean |
| 4 | The **gaps between drawers are rigid** — the same at every openness | gaps are margins between cards, which is already true and must stay true |

---

## What is there today (v1.216.0, #88)

Each card is `position: sticky` at its own slot, opaque, with a z-index rising down the column:

```
  scrolled to the top                 scrolled down a little
  ─────────────────────               ─────────────────────
  ┌─ OUTLINE ─────────┐  ← caption    ┌─ OUTLINE ─────────┐
  ├───────────────────┤               ├───────────────────┤
  │ ▾ Turns        40 │  head         │ ▾ Turns        40 │ ← pinned at its slot
  │   01 question 0   │               │   18 question 17  │   (body still under
  │   02 question 1   │  body         │   19 question 18  │    its own head, but
  │   03 question 2   │               ├───────────────────┤    COVERED, not closed)
  │   …               │               │ ▾ Tasks         0 │ ← scrolled OVER it
  ├───────────────────┤               ├───────────────────┤
  │ ▾ Tasks         0 │               │ ▾ Agents        0 │
  └───────────────────┘               └───────────────────┘
```

It satisfies "no pane shows its body above its own head", and the heads stack in order. **But a
body never shrinks** — the next card slides over it. That is a different physical metaphor:
sheets of paper overlapping, not drawers closing.

---

## The drawer model, drawn

Let each drawer *i* have a head of fixed height `H` and a body whose natural (fully open) height
is `B(i)`. Openness `p(i)` runs 1 → 0. The rigid gap between drawers is `G`.

```
  s = 0   (nothing closed)        s = 250  (top drawer half shut)     s = 500 (top shut)
  ┌─────────────────┐             ┌─────────────────┐                 ┌─────────────────┐
  │ ▾ Turns      40 │ H           │ ▾ Turns      40 │ H               │ ▸ Turns      40 │ H
  │ 01 question 0   │             │ 01 question 0   │                 ├─────────────────┤ G
  │ 02 question 1   │ B₀·p₀       │ 02 question 1   │ B₀·p₀           │ ▾ Tasks       0 │ H
  │ 03 question 2   │  (p₀=1)     └─────────────────┘  (p₀=0.5)       │ (its body now   │
  │ …               │             ────────────── G                    │  begins to      │ B₁·p₁
  └─────────────────┘             │ ▾ Tasks       0 │ H               │  close)         │
  ────────────────── G            │ (still fully    │                 └─────────────────┘
  │ ▾ Tasks       0 │ H           │  open)          │ B₁·p₁           ────────────────── G
  └─────────────────┘             └─────────────────┘  (p₁=1)         │ ▾ Agents      0 │ H
```

**The arithmetic.** One number drives everything: how far the column has been scrolled, `s`.
Spend it on the drawers in order, top first:

```
  budget = s
  for i in 0..n:
      closed(i) = min(budget, B(i))      ← how much of drawer i is shut
      p(i)      = 1 − closed(i) / B(i)   ← its openness, 1 → 0
      budget   −= closed(i)              ← what is left closes the next one
```

Two consequences fall straight out, and they are the rules:

- **Top-first** is not a special case; it is what "spend the budget in order" means. Drawer 1
  cannot begin to close until drawer 0 has spent all of `B(0)`.
- **The gaps are outside the arithmetic.** `G` never appears in the budget, so it is constant at
  every openness — "invisible but rigid".

Total column height is then `Σ(H + G) + Σ B(i)·p(i)`, and the maximum scroll is `Σ B(i)` — the
point where every drawer is shut and only the head stack remains.

**The toggle.** For a drawer at openness `p`:

```
  p > 0.5  →  close it   (animate p → 0)      "finish shutting what the sliding started"
  p ≤ 0.5  →  open it    (animate p → 1)
```

So the toggle is never a flip — it is a snap to the nearer end. A fully open drawer (`p = 1`)
closes and a fully shut one (`p = 0`) opens, which is the behaviour that exists today; the new
part is only what happens in between.

---

## What this costs, honestly

**1. Scroll position becomes state, not a viewport.** Today the column is an ordinary scroller
and the browser owns the offset. In the drawer model the scroll offset *is* the openness vector,
and the content height changes as you scroll — the scrollbar shrinks under your thumb as drawers
close. Native scrolling fights that. The usual answers: drive it from `wheel`/`touch` deltas with
the column no longer natively scrollable, or keep a native scroller with a spacer whose height is
`Σ B(i)` so the offset stays honest while the drawers absorb it. **The second is the one to
build** — it keeps trackpad momentum, keyboard paging and the accessibility tree.

**2. Two scrolls have to compose.** The Turns list already has its own scrollbar
(`max-height: min(48vh, 560px)`). If the wheel over an open Turns body closes the drawer, a
reader cannot scroll the list; if it scrolls the list, the drawer never closes while the pointer
is over the biggest target in the column. The conventional resolution is *the inner scroller
first, the drawer when it is at its end* — the same rule `overscroll-behavior` expresses. **This
is question 2 below and it is the one most likely to make or break the feel.**

**3. Openness must survive a reload.** `am-prod-nav-cards` stores a set of open pane keys. A
continuous `p` per pane is a different shape. Storing partial openness restores a reader to a
half-shut drawer, which may read as broken rather than faithful.

**4. Everything that measures the column assumes fixed slots.** `stackOutlineHeads` computes each
card's slot from the heads above it, `landOutlineCard` scrolls so an opened head meets its slot,
and `revealInPane` scrolls the current turn's row into view inside the pane's own scroller. All
three change meaning when the column's height is a function of the scroll offset.

---

## Questions that change what gets built

1. **Does a shut drawer's head stay in the stack**, so all four heads are always visible and
   clickable? (I assume yes — otherwise a shut drawer is unreachable.)
2. **Wheel over an open body: scroll the body, or close the drawer?** And if the body scrolls
   first, does the drawer start closing the moment the list hits its end (continuous), or does
   that take a second gesture?
3. **When every drawer is shut, is there anything left to scroll?** (If not, the column ends as a
   bare head stack; if yes, what is below it?)
4. **Is partial openness remembered across a reload**, or does it snap to the nearer end on load?
5. **Does the drawer animate on the toggle** (a duration, an easing), and should the sliding
   itself feel 1:1 with the wheel or damped?
6. **What happens on a short window** where `Σ(H + G)` alone exceeds the column's height — the
   head stack itself does not fit? (Today the caption plus four heads is ~200px, so this bites
   under about 260px of column height.)

---

## Where this sits in the record

- `#88` (v1.216.0) — sticky cards, opaque, rising z-index: no body above its own head, and an
  opened pane lands at its slot. **Shipped, and its guards stay green** under whatever replaces
  it: those two rules are still true of drawers.
- `#74` — the head stack, and each pane keeping its own height and its own scrollbar.
- This task — the drawer model above, which replaces #88's *mechanism* while keeping its rules.
