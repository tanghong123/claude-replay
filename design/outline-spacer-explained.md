# The outline spacer, and why the column stops scrolling

Written for #157, which needs a decision. The drawer model in
[`outline-drawers.md`](outline-drawers.md) assumes one property — the column's
`scrollHeight` never changes — and that property is what blocks the per-pane state the
owner asked for. This explains the property from the ground up.

The code is `claude-monitor/src/codex-ui/app.js:522-534`.

---

## 1. What the column is made of

`#sessionNavigator` is a scrolling box. Inside it: a caption, then a stack of cards, each
a **head** (always visible) and a **body** (the list).

```
┌─ #sessionNavigator ─────────────────────┐
│  Outline                          [x]   │   caption   ·  31px
├─────────────────────────────────────────┤
│  v Turns                                │   head      ·  35px
│      turn 01   parse the fold           │
│      turn 02   re-run the gate          │   body B    · 145px
│      turn 03   ship it                  │
│                                         │
│  v Tasks                                │   head      ·  35px
│      #157  one state per pane           │   body B    ·  97px
│                                         │
└─────────────────────────────────────────┘
```

A drawer closes by its **body height** going 145 → 100 → 0. Not by disappearing. The head
always stays, which is what lets the reader open it again.

Write `B` for a body's natural height and `p` for how open it is, 0 to 1. The body's
drawn height is `B·p`.

---

## 2. Scrolling spends; it doesn't move

The column's scroll offset is a **budget**. Scroll down 60px and the top drawer's body
gives up 60px:

```
┌─ scroll = 0 ────────┐      ┌─ scroll = 60 ───────┐
│  v Turns            │      │  v Turns            │
│      turn 01        │      │      turn 01        │
│      turn 02        │      │                     │
│      turn 03        │      │  v Tasks            │
│  v Tasks            │      │      #157           │
│      #157           │      │                     │
└─────────────────────┘      └─────────────────────┘

                               Turns' body: 145 -> 85
                               Tasks rose by 60
```

Call the spent amount `s`. In the model as built, `s` is the scroll offset and openness is
computed from it — spend it on the drawers in order, top first.

---

## 3. The problem that creates

The browser is *also* scrolling. So the reader gets two movements at once: the body
shrinks by 60, **and** the browser shifts everything up by 60. Every head would lurch by
120px while they scrolled.

---

## 4. The spacer

So the column keeps an empty `<div>` — no text, no border, invisible — as the **first
child**, above the caption. Its only job is to be exactly as tall as the pixels already
spent.

```
the scrollable content          what you actually see
(taller than the window)

┌─ scroll = 0 ────────┐         ┌─ the window ────────┐
│  [ spacer:  0 ]     │         │  Outline            │
│  Outline            │         │  v Turns            │
│  v Turns    B = 145 │         │      turn 01        │
│  v Tasks    B =  97 │         │      turn 02        │
└─────────────────────┘         └─────────────────────┘

┌─ scroll = 60 ───────┐         ┌─ the window ────────┐
│  [ spacer: 60 ]     │         │  Outline            │
│  Outline            │         │  v Turns            │
│  v Turns    B =  85 │         │      turn 01        │
│  v Tasks    B =  97 │         │  v Tasks            │
└─────────────────────┘         └─────────────────────┘
```

Scrolling down 60 hides the top 60px of content — which is *precisely* the spacer.
Everything below it lands where it already was. The scroll and the spacer cancel, and the
heads hold still.

In the source this is `drawerSlide`, and the comment puts it as: *"the scroller lifts the
stack by `s`, the slide pushes it back down by `s`, and the two cancel, so the heads stay
where they are."*

---

## 5. Why the total never changes

Bodies lost 60. Spacer gained 60. Add up everything in the scroll box:

```
total content = caption + spacer + Σ heads + Σ bodies
              = caption + (ΣB − ΣB·p) + Σ heads + ΣB·p
                           ^^^^^^^^^^              ^^^^^   these cancel
              = caption + Σ heads + ΣB
```

`p` drops out entirely. **The scrollable total is identical whether every drawer is open,
shut, or halfway.**

That is on purpose, for one concrete reason: if `scrollHeight` shrank as you scrolled, the
scrollbar thumb would grow and re-seat itself mid-drag — you would be chasing the control
you are holding.

(It is constant *with respect to how open the drawers are*. It still changes when `B`
itself changes: a list grows, a window resize reflows.)

---

## 6. Where it runs out

How far the column can scroll is `total − window`. The total is fixed, so that distance is
fixed too — and nothing makes it big enough to hold every drawer's body.

Measured on a real column (turns 145 + tasks 97 + agents 97 + session 390):

```
to shut every drawer      ████████████████████████████████████████████████████  729px
furthest it will scroll   ████████████████████████████  386px
                                                      ^─────────────────────── 343px it can never reach
```

The intuition: **you can only scroll as far as there is content below the fold.** Once
every drawer is shut, all that is left is the caption and four heads — about 200px. If the
window is 546px tall, there is nothing left to scroll against, so "everything shut" is
simply not a reachable scroll position.

Algebraically, reaching it needs `caption + Σ heads ≥ window`, which is false whenever the
column is taller than its own heads. Which is the normal case.

---

## 7. Why this blocks #157

The old model never noticed, because openness was **computed from** the scroll offset:
clamped input, clamped output, always self-consistent. You could only close as much as the
range allowed, and that was fine because nothing else had an opinion.

#157 makes openness the **truth** and has the offset follow it. Now the state can say
things the scrollbar has no room to say back. Start with three drawers shut — 584px owed —
scroll all the way to the top, and only 386px come back. The last drawer stops partway
open.

That is `the_app_shell_outline_panes_are_drawers` failing on *"scrolling back opens them
again, bottom-first, to exactly where they were."*

### The three ways out

| | what it does | what it costs |
|---|---|---|
| **Rescale** | map the scroll range `[0, max]` onto `[0, ΣB]` | the wheel stops tracking the drawers 1:1 — the property this design was built around |
| **Taller spacer** | pad the spacer so the range can hold the offset | the excess pushes every head down the column |
| **Accept the clamp** | states are the truth, the offset is best-effort | both reported bugs fixed; "scroll to the top and everything is open" stops being exactly true |

The third is the smallest change and still delivers what was asked. The first is what most
people mean by a drawer chain.

Both of the reported bugs are fixed by any of them, because they come from something else
entirely: a toggle-shut pane was read as having *no body to close*, so it took no budget,
so no amount of scrolling could ever give it any back.
