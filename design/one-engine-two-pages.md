# One engine, two pages: what still differs, and what each option costs

*For the owner to read and comment. Written 2026-09-06, after steps 1–4 of the virtual-window
work shipped (v1.212.0, v1.213.0) and step 5 was surveyed and NOT started. Every claim here was
read in the source; the file:line references are checkable.*

The goal has been one scroll engine both pages drive. Most of it is done. This document is about
the part that is not, why it resists, and what the ways forward actually cost — with the
arithmetic drawn out, because the disagreements are arithmetic disagreements.

---

## Where we already are

| | Classic page | App shell | Shared? |
|---|---|---|---|
| **Arithmetic** — `prefixSums`, `indexAt`, `rangeForScroll`, `rangeAround`, `padHeights`, `correction`, `firstVisible`, `classifyScroll`, `heightChanged` | uses it | uses it | ✅ **yes**, and node-tested |
| **State machine** — the window, the pads, the anchor, the observers, the follow state, the tail converge | its own | `VirtualWindow` | ⚠️ shared *class*, one consumer |
| **Drives the class** | ❌ **no** — this is step 5 | ✅ yes | — |
| **Held to the same behaviour** | ~95 two-surface scenarios | same ~95 | ✅ **yes**, zero `known_red` |


- **Steps 1–3** put both pages on the same *arithmetic*. Node-tested, no DOM.
- **Step 4a** made the state machine a class (`VirtualWindow`) with a frame adapter, so a
  document-scrolling page is expressible.
- **Step 4b/5** — the classic page driving that class — is what remains.

**What the migration was a means to, we already have.** Both pages are held to the same
behaviour by ~95 scenarios that run on *both* surfaces, and the suite currently has zero
`known_red` cases. What step 5 would add is that one *file* holds the loop.

---

## The model both pages share

Only a slice of the records is in the DOM. Two pads stand in for the rest, sized from prefix
sums over per-record heights.

```
   index:  0 ............ lo .................................. hi ............ N
           |             |                                     |               |
           |  TOP PAD    |   MOUNTED (real DOM)                |  BOTTOM PAD   |
           |  height =   |   ┌───────────────┐                 |  height =     |
           |  sums[lo]   |   │  overscan     │                 |  sums[N]      |
           |             |   ├───────────────┤ ← viewport top  |   − sums[hi]  |
           |             |   │  what you see │                 |               |
           |             |   ├───────────────┤ ← viewport btm  |               |
           |             |   │  overscan     │                 |               |
           |             |   └───────────────┘                 |               |
```

A record's height is an **estimate** until it is mounted and measured, then it is remembered.
So the index↔pixel map is exact behind the reader and approximate ahead of them. Every
disagreement below is about that map.

---

## Difference 1 — how a record's height is measured  ⚠️ **the blocker**

The two pages measure the *same* thing two different ways, and each is right for its own CSS.

### The app shell: border box + margins

```js
// shared/virtual-window.js:353
const height = child.getBoundingClientRect().height + marginTop + marginBottom;
```

Its units are spaced with **padding** (`.turn{padding:23px 0}`, reference.css), so there are no
margins to collapse and this is exact.

### The classic page: top of one to top of the next

```js
// export.js:322-324
var next = k + 1 < els.length ? els[k + 1] : botPad;
var h = next.offsetTop - els[k].offsetTop;
```

Its blocks are spaced with **margins on both sides** — `.uturn{margin:16px 0 10px}`,
`.fold{margin:2px 0}`, `.fold[data-open="1"]:not(.uturn){margin:8px 0}` — and adjacent margins
**collapse**.

### Why the engine's measure is wrong on the classic page

```
  CSS:  block A  margin-bottom: 10px
        block B  margin-top:    16px
  What the browser actually renders (margins collapse to the larger):

        ┌──────────────┐  A's border box  (say 100px)
        │      A       │
        └──────────────┘  ┐
                          │  collapsed gap = max(10, 16) = 16px   ← ONE gap, not two
        ┌──────────────┐  ┘
        │      B       │  B's border box  (say 200px)
        └──────────────┘

  ENGINE measure:   A = 100 + 0 + 10 = 110      ⎫  110 + 216 = 326
                    B = 200 + 16 + 0 = 216      ⎭  but A-top → B-bottom is only 316
                                                   OVER-COUNTED by min(10,16) = 10px per pair

  CLASSIC measure:  A = B.offsetTop − A.offsetTop = 116   ⎫  116 + 200 = 316  ✅ exact
                    B = botPad.offsetTop − B.offsetTop = 200 ⎭
```

Over a mounted run of twenty blocks that is 40–160px of phantom height; over a whole session,
the document's total height inflates as the reader scrolls through it.

### Why we cannot simply adopt the classic measure

**We tried. It is in the tree as a reverted experiment (#132).** Measuring top-to-next-top
attributes the *gap between two items to the upper one* — so **which item owns a margin changes
as the window slides**:

```
   window [lo=5 .. hi=9]          window slides down → [lo=6 .. hi=10]

   item 5  ← owns the 5/6 gap     item 5  not mounted; its height in the sums
   item 6                          item 6  ← now the FIRST mounted item, and the
   item 7                                    5/6 gap belongs to nobody
   item 8                          item 7
                                   item 8
                                   item 9
```

The app shell's own case caught it: `the_app_shell_gives_the_transcript_the_whole_width` moved
the reader from `process:b91` to `user:t51` on a width reflow. So each page's measure is right
for that page and wrong for the other.

---

## Difference 2 — what gets mounted under a filter

**Classic: sparse.** A filtered-out record is never built, and counts 0 in the sums *by
predicate*, before it is ever mounted:

```js
// export.js:215-216
function isHiddenRec(i) { return !!filter && !isTurnKind(records[i]) && !recHit[i]; }
function effH(i)        { return isHiddenRec(i) ? 0 : recHeights[i]; }
```

**App shell: dense, and since #133 there is nothing to hide.** Its filter is a *search by kind*
— it marks and steps, it does not cut — so every record stays mounted at its own height.

This one is **already settled**: you chose sparse-vs-dense as a per-page policy, and the app
shell needs no predicate at all. It is listed here because the *implementation* of that policy
has a cost nobody could have foreseen at the time — the next section.

---

## Difference 3 — the cost of the agreed placeholder model

The agreed shape: the engine takes `skip(index)`, a skipped record counts 0 and mounts as an
**empty placeholder**, so the reconcile loop stays 1:1 index↔child.

### Cost A — the range never stops growing

```js
// shared/virtual-window.js:52-55
while (hi < count && below > 0) { below -= heightAt(hi); hi++; }
                                  ^^^^^^^^^^^^^^^^^^^^
                                  a skipped record is 0 → spends none of the budget
```

```
  A filter that keeps 1 record in 20, over a 4,000-record session:

  budget: clientHeight + overscan  ≈  700 + 1500 = 2200px

  DENSE + placeholders          SPARSE (classic today)
  ─────────────────────         ──────────────────────
  walks 0,0,0,...,0,340,        walks the same indices, but
  0,0,...,0,290, ...            builds NOTHING for the zeros
  → [lo,hi) spans ~1,300        → [lo,hi) spans ~1,300 too
    indices                       indices
  → mounts ~1,300 children      → mounts ~65 elements
    (65 real + ~1,235
     placeholders)
  → observes all ~1,300         → observes 65
    (reconcile:441 observes
     every child it mounts)
```

### Cost B — the placeholder reuse trap

```js
// shared/virtual-window.js:415
const reusable = … && cursor.dataset.unitKey === this.identityAt(index) && …
```

- Stamp the placeholder with its record's key → when the filter clears, the placeholder
  **passes the reuse test** and the real record never renders.
- Stamp it with anything else → it is torn down and rebuilt on **every** reconcile.

Both directions need explicit handling the model does not have yet.

---

## Difference 4 — smaller, but each needs a home

| | Classic | Engine | Consequence |
|---|---|---|---|
| **What is observed** | `document.body` — growth *anywhere*, including chrome outside the mounted run | `mount.window` + each mounted child (`:186`, `:441`) | Growth outside the mounted run stops being corrected |
| **Positions from sums alone** | six sites: filter exit, `landOn`, `goToId`, `matRecord`, search ordering, `turnTo` | none | Six call sites need an engine answer or keep their own |
| **`indexAt` clamping** | unclamped (an offset past the last visible record must read as past the end, so the bottom pad lands) | clamped | A parameter, but a real one |
| **Node contract** | pins seven literal `export.js` lines | — | Mandatory CI step, red until rewritten |

And the DOM half, which turned out **easy**: `#stream` is empty in the exported HTML (the JS
builds every child), `export.css` never mentions it, and the whole repo has exactly one
`#stream >` child selector. Wrapping the mounted run in a div — which the engine needs, since six
of its methods assume `mount.window.children` *are* the items — changes no rendered byte.

---

## The options

### Option A — stop here, and close the acceptance differently

Leave the classic page on its own loop. Keep what we have: shared arithmetic, a shared class the
app shell runs, and ~95 scenarios holding both pages to the same behaviour.

- **Cost:** two files still hold a scroll loop. A future scroll rule must be written twice — but
  it must also be *tested* twice today, and the two-surface scenarios are what actually catch the
  divergence (three classic-page bugs this session were found exactly that way: #71, #98, #134).
- **Buys:** no risk to the reference page. Zero work.

### Option B — parameterize the height measure

Give the engine both measures and let each page choose.

- **Cost:** this is the design #132 tried and reverted, re-introduced deliberately. It also
  concedes the point of step 5: the engine would no longer own the rule that decides where
  everything sits. Two measures means two behaviours to keep true, in the one place that was
  supposed to make them one.
- **Buys:** the classic page can move without touching its CSS.

### Option C — take the collapsing margins out of the classic page's CSS

Convert `.uturn` / `.fold` spacing from margins to padding (or to a flex `gap`), so
border-box-plus-margins becomes exact there too.

- **Cost:** changes how the **reference page looks** — spacing is what these rules are. Every
  gate fixture re-baselines on appearance, not just on bytes. Backgrounds and borders land
  differently: a margin is transparent, padding is inside the box.
- **Buys:** one measure, honestly shared. This is the only option that leaves the engine owning
  the rule.

### Option D — port anyway, with the classic measure overridden in a subclass

- **Cost:** a fork wearing the shape of a shared engine. The worst of both: the migration's risk,
  without its benefit.
- **Buys:** the file count goes down.

---

## What I would do

**Option A, unless you want the spacing change** — and you do; see the answer under
*Questions* below, which supersedes this.

**Option A was the recommendation when the reference page's look was fixed.** The property we were after — one behaviour,
held on both surfaces — is already ours, and it is held by tests rather than by a shared file,
which is the stronger of the two. Step 5's remaining benefit is one loop instead of two; its
price is either a fork of the measure or a change to how the reference page looks.

**If you want it properly unified, Option C is the only honest one**, and the first commit is not
the port. It is:

1. a decision on the spacing (margins → padding on `.uturn` / `.fold`), with the gate
   re-baselined on appearance and reviewed by eye;
2. a scenario for the sparse-filter window that **fails on the placeholder model** — Cost A above
   is unmeasured today, and both existing classic filter cases run 12–14-record fixtures where
   the explosion cannot show;
3. only then the port.

---

## Questions for you

**1. Is the classic page's spacing something you would let change (Option C)?**
→ **Answered (owner, 2026-09-06):** *"I have no opinion on the classic page's spacing. To me it
was an implementation detail and I was not informed or weighed on that decision. I feel the app
shell's way is cleaner, self-contained. So yes, I'd let it change."*

**That decides it: Option C is on the table, and it is the only option that leaves the engine
owning the rule.** The recommendation above (Option A) was written on the assumption that the
reference page's look was fixed. It is not, so the plan becomes:

1. **Spacing first, on its own.** Convert `.uturn` / `.fold` spacing from collapsing margins to
   padding (or a flex `gap`), so border-box-plus-margins is exact on both pages. This is a
   visual change to the reference page: the gate re-baselines on *appearance*, and it wants a
   look at the rendered result, not just a byte diff. Backgrounds and borders move — a margin is
   transparent, padding is inside the box — so a card's fill grows by the space it used to have
   outside it. Ships on its own, with nothing else in the commit.
2. **Then one measure.** With no collapsing margins, `measureMounted` is exact for both, and the
   engine keeps the rule it reverted top-to-next-top to protect (#132).
3. **Then the sparse-filter scenario**, which must fail on the placeholder model before the port
   is written — Cost A above is unmeasured, and both existing classic filter cases use 12–14
   record fixtures where the explosion cannot show.
4. **Then the port.**

**2. Do you want one *file* to hold the loop, or one *behaviour*?** — still open, but the answer
to (1) suggests you want the real thing, so the plan above assumes it.

**3. What happens to #128?** — it stays open as the port, no longer blocked on a decision. The
spacing change becomes its first step.
