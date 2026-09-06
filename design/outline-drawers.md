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

### The chain, and the friction (owner's elaboration)

> "These drawers are chained (invisibly). And when the whole chain is sliding, it is as if
> someone is pulling or pushing the drawers from the very bottom. The 'friction' of these drawers
> grow from the top drawer to the bottom drawer. So when push, the top drawer starts collapsing.
> The top drawer's head is fixed."

This is the *why* under rule 2, and it is worth keeping because it settles cases the rule alone
does not:

- The drawers are **linked**, and the force is applied **at the bottom** — the reader's scroll
  pushes the whole chain.
- **Friction increases downward**, so the top drawer offers the least resistance and yields
  first. "Top drawer closes first" is not a policy; it is what the least-resistance drawer does.
- **The top drawer's head is fixed** — it never moves, whatever the chain does.

**And the friction runs both ways** (owner): *"On closing, the top drawer has the least friction.
On opening, the bottom drawer has the least friction."*

That completes the metaphor, and it turns out to be exactly the arithmetic below:

```
  closing (push):  least friction at the TOP     → drawers close  0, 1, 2, 3 …
  opening (pull):  least friction at the BOTTOM  → drawers open   … 3, 2, 1, 0
```

Opening in the reverse of the closing order is what "spend the budget top-first, release it
bottom-first" means, so **openness stays a pure function of the scroll offset**: scroll down and
back up and the column retraces exactly, and one offset always means one state. The owner's
reading and the budget loop are the same model.

| # | Rule | What it replaces |
|---|---|---|
| 1 | Openness is **continuous** — a drawer can sit anywhere between open and shut | a boolean per pane (`uiState.navCards`) |
| 2 | **Scrolling closes** drawers smoothly, **top one first**, then the second, then the third | scrolling moves a viewport over fixed-height cards |
| 3 | The **toggle completes the last movement** — was it closing, close it; was it opening, open it | the toggle flips the boolean |
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

Total column height is then `Σ(H + G) + Σ B(i)·p(i)`.

**How far it can be pushed** — *"we don't need to fully close the bottom drawer when its bottom
is visible"*. So the chain stops when there is nothing left to gain:

```
  s_max = max(0, Σ(H + G) + Σ B(i) − columnHeight)
```

which is the ordinary scroll extent — content height minus the viewport. Drawers close only as
far as it takes to bring the rest into view, and the bottom one keeps whatever openness the
remaining room allows. On the way up, `s = 0` is everything open: *"for scroll up, no"*, there is
nothing beyond it.

**The toggle** — *"if we know whether the drawer was opening or closing in the last movement,
then the toggle just completes the action if the drawer is partially open"*:

```
  p = 1                        →  close it        (a fully open drawer shuts, as today)
  p = 0                        →  open it         (a shut one opens, as today)
  0 < p < 1, last was closing  →  close it        finish the movement
  0 < p < 1, last was opening  →  open it         finish the movement
```

So each drawer remembers the **direction** of its last movement, and the toggle finishes it
rather than reversing it. The endpoints behave exactly as the control does today; only the middle
is new. (Where a drawer is partly open with no movement behind it — the first paint — there is no
direction to complete; see question C.)

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

## Answered (owner, 2026-09-06)

| Question | Answer |
|---|---|
| Does a shut drawer's head stay in the stack? | **Yes** — all four heads are always there. |
| Wheel over an open body: scroll the body, or close the drawer? | **The list first, then the chain** — *"when the mouse is over the list, we will first apply the scroll to the list, and only when the list cannot be scrolled, then the scroll will be applied to the chain of drawers."* (Revises the owner's first answer, "close the drawer", after the long-list consequence below.) |
| Anything left to scroll when everything is shut? | **No** on the way up; and the bottom drawer need not fully close while its bottom is visible (the `s_max` clamp above). |
| Is partial openness remembered across a reload? | **No need.** |
| Does the drawer animate on the toggle? | **Yes** — *"the drawer toggle is the only way to explicitly open/close one drawer."* |
| A window too short for the head stack itself? | **The outline column's own scrollbar takes it**, as it does today. |

Two of the original questions dissolve with those answers: nothing partial is stored, so there is
nothing to restore on load; and the short-window case is the existing scroll behaviour.

**The consequence that produced that revision.** The Turns list has its own scrollbar
(`max-height: min(48vh, 560px)`) and a long session gives it forty-plus rows. Had the wheel always
closed the drawer, the wheel could no longer scroll that list. The rule above is the standard
nested-scroll one — the inner scroller consumes the wheel until it is at its end, and only then
does the gesture reach the chain — which is what `overscroll-behavior: contain` expresses, and
what the options popover already does since #139.

## The rest, answered

| Question | Answer |
|---|---|
| On the way back up, which drawer opens first? | **The bottom-most one that is not fully open** — two-way friction, which is the budget loop (see above). The column retraces exactly. |
| A drawer part-way with no movement behind it | Cannot arise from a reload: **all drawers are fully open when first drawn.** If it arises another way, fall back to the static rule — *"if <50% open, do open, otherwise, do close."* |
| 1:1 or damped? | **1:1 first**, and see how it feels. |

**Every question is now answered, and this document is the specification.** What it asks for, in
one place:

1. Openness is continuous; `p(i)` for each drawer.
2. The scroll offset drives it through the budget loop — closing top-first, opening bottom-first,
   with `s_max` the ordinary scroll extent so the bottom drawer need not shut while its bottom is
   visible.
3. The gaps `G` are outside the budget and never change.
4. All four heads are always in the stack; the top head is fixed.
5. The toggle completes the drawer's last movement, animated; with no movement behind it, the
   static 50% rule decides. The toggle is the only explicit open/close.
6. A wheel over a scrollable body scrolls that body first and reaches the chain only at its end.
7. Sliding tracks the wheel 1:1; the animation belongs to the toggle.
8. Nothing partial is remembered — every drawer opens fully on first paint.
9. A window too short for the head stack falls back to the column's own scrollbar.

## Where this sits in the record

- `#88` (v1.216.0) — sticky cards, opaque, rising z-index: no body above its own head, and an
  opened pane lands at its slot. **Shipped, and its guards stay green** under whatever replaces
  it: those two rules are still true of drawers.
- `#74` — the head stack, and each pane keeping its own height and its own scrollbar.
- This task — the drawer model above, which replaces #88's *mechanism* while keeping its rules.
