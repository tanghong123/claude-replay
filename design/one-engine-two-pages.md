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

**2. Do you want one *file* to hold the loop, or one *behaviour*?** — still open. What the two
mean in practice:

### "One behaviour" — where we are now

Two implementations of the scroll loop: `export.js` has ~300 lines of window / anchor / follow,
`shared/virtual-window.js` has the class. They are kept identical **by the scenarios**, which run
the same assertions against both pages.

- A new scroll rule is **written twice**, and tested once.
- A rule nobody wrote a scenario for can differ silently — the scenarios *catch* divergence, they
  do not *prevent* it.
- The reference page is never at risk, because nothing moves.

### "One file" — step 5

One loop. The classic page becomes a consumer of the class, the way `viewport.js` is: it says
what an item is, how one renders, where the reader's choices live, and the engine owns the
window, the pads, the anchor, the observers and the follow state.

- A new scroll rule is **written once** and both pages have it.
- Divergence becomes *impossible* for anything the engine owns — a stronger guarantee than "no
  scenario has caught one".
- The cost is the migration: the spacing change, then the measure, then the sparse-filter
  scenario, then the port — with the reference page moving underneath it.

### The evidence from building this

Both this session:

| For "one file" | For "one behaviour" |
|---|---|
| **#138 was fixed twice.** The scroll-jump fix — "at rest, adopt where the reader is; never replay an old position" — was written into `virtual-window.js` AND `export.js`, and needed its own contract pin on each. One rule, two edits, two chances to get it wrong. | **Three classic-page bugs were found by app-shell rules**: #71, #98 and #134. The scenarios are not a formality — they are what catches the page that nobody is currently working on. |
| **#134 exists because of duplication.** The classic page fought the reader's fling for a whole release *because* the fix had gone into the engine only. | **Nothing regressed on the reference page all session**, precisely because it was not being refactored. |

Note that the second column survives either choice: the two-surface scenarios stay, and stay
valuable, whichever way this goes. The first column is what only "one file" buys.

### What stays split either way

"One file" is only the **scroll loop**. Each page keeps its own renderer, folds, search reveal,
filter meaning (deliberately divergent since #133), and chrome — and the agreed `skip()`
predicate is itself a small per-page fork living inside the shared engine.

### The question under the question

Which failure would you rather have?

- **A rule that quietly differs** between the two pages because no scenario covers it → "one
  file" removes this.
- **A regression on the reference page** introduced while unifying → "one behaviour" removes
  this.

My read: with (1) answered, the expensive half of "one file" is already agreed, and today's #138
— the same fix written twice, in two vocabularies — is the concrete cost of not doing it.

→ **Answered (owner, 2026-09-06):** *"That is right. I prefer one-file, since I think the logic
should be reused. We will resolve regressions over time."*

**Both questions are now settled, and the doc is closed as a decision record.** The spacing may
change (1) and the loop becomes one file (2), so Option C is the plan and #128 is execution, in
this order:

1. **The spacing change** — `.uturn` / `.fold` margins → padding, on its own commit, the gate
   re-baselined on appearance with a look at the render.
2. **One measure** — with no collapsing margins, `measureMounted` is exact for both pages.
3. **The sparse-filter scenario** — written to FAIL on the placeholder model, since the range
   walk never spends its budget on a zero-height record and no existing case is long enough to
   show it.
4. **The port** — the document frame, the wrapper inside `#stream`, the skip predicate, and the
   seven `ui_contract` pins rewritten.

"We will resolve regressions over time" is noted, and the two-surface scenarios are what will
surface them: they stay, and they are the reason a regression on the reference page is a bug
report rather than a silent difference.

**3. What happens to #128?** — it stays open as the port, no longer blocked on a decision. The
spacing change becomes its first step.

---

## Step 1, done (2026-09-06)

The spacing change landed as stated — with one correction to the plan and one finding it turned
up on the page that was supposed to be already right.

**The correction.** "Margins → padding" was the sketch; padding puts the space INSIDE a card's
border and background, which changes what a user turn and an open fold look like. What the
arithmetic actually needs is narrower than that:

```
  engine:  size(i) = borderBox(i) + marginTop(i) + marginBottom(i)
           top(i)  = Σ_{j<i} size(j)                    ← where the pads put the next record
  layout:  top(i)  = Σ_{j<i} (marginTop + borderBox + marginBottom)(j) + marginTop(i)
                                                        ← plus collapsing, which takes the max
                                                          of each adjacent pair instead of the sum
```

The two agree exactly when **no record carries a margin on its top** and **no two margins
collapse** — nothing about padding. So `#stream` became a flex column (flex items never collapse)
and every block's spacing moved to its bottom, with the air that belongs to what FOLLOWS handed
to the predecessor by `:has`. Those selectors key on the next block's KIND, never on its fold
state: a record's size has to depend on its own state alone, or opening a fold resizes the record
ABOVE it — a change no per-element observer can see, and a top that moves under the reader on the
click that asked for more.

**Measured, on a real 400-turn transcript scrolled into its middle:** 26 of 27 mounted pairs
disagreed with their own measure before; 0 of 27 after, with folds shut and again with folds
opened under the reader. Every rendered gap is unchanged except one — an open fold no longer
claims 8px above itself, only below (`ablock → open fold` goes 8 → 2). The 16px above the first
block used to arrive by accident, as the leading turn's top margin collapsing OUT of `#stream`;
it is now asked for on the container, where it cannot corrupt a sum.

**The finding.** The rule was written as a scenario and run against BOTH pages, and the app shell
failed it: `.process-surface` opens with `margin: 8px 0 4px`, so every process unit made the
virtual window's sums 8px short of where the layout actually put the next unit — and the pads
standing in for unmounted records were short by 8 for each process they covered. The same fix
(a flex column, the 8px handed to the record above) went into `production.css`, since
`reference.css` is generated and never hand-edited. Whether this was the residual "the page jumps
a small offset when I stop scrolling" is a hypothesis, not a claim: it was not reproduced from
this cause, only measured.

That is the whole argument for the two-surface discipline in one paragraph — the page we thought
was the correct one had the same bug, and only a scenario written once and run twice found it.

## Steps 2 and 3, done (2026-09-07)

Both landed. Step 2 turned up a property of the classic page that nobody had written down, and
it is the reason step 4 does not follow straight on.

### Step 2 — one measure

**The correction to the plan.** The step reads "take heights from the shared `measureMounted`".
`measureMounted` is a METHOD on `VirtualWindow`, and the classic page does not extend that class
— it consumes the module's arithmetic half as `window.__shared`. So "one measure" is one
FUNCTION, not one loop: `itemHeight(element)` (`shared/virtual-window.js:148`) is now the only
place either page turns an element into a height, called by the engine at `:368` and by the
classic page's `measureWindow` at `export.js:346`. It sits BELOW the engine marker on purpose —
the contract pins the rules half as numbers in, numbers out (`ui_contract.mjs`, "the rules are
numbers in, numbers out"), and this one reads layout.

**The finding: the classic page cannot hold fractional heights.** With the shared measure taken
raw, `classic_page_holds_to_the_pixel_when_unpinned_through_growth` went from 8/8 green to 4/8;
rounding it put it back to 8/8. Three blocks of eight serial runs on an idle machine, back to
back. In a failing run both of the reader's scrolls take (gap 700 then 1400) but the page is
still `following` throughout — it never unfollows — and the next settle heals it to the tail.

What that is NOT, each measured rather than argued:

| suspect | measurement | verdict |
|---|---|---|
| the basis | the two agree to 0.61px over every mounted record (108.390625 vs 109) | not it |
| the cost | 0.020ms vs 0.013ms per pass over 23 elements | not it |
| the corrective `window.scrollBy` | wrapped and counted from before the jump: fires ZERO times | not it |
| total height | 10024px fractional vs 10023px integer | not it |

Every attempt to instrument the page also stops the race reproducing — an arming `eval` after
`await_tail`, and a MutationObserver plus a `scrollBy` wrapper armed before the jump, each took
it to 6/6 or 3/3 green. So what shipped is `Math.round` on the measure, and it is a RESTORED
INVARIANT rather than a fix: `offsetTop` is an integer, so this page always had integer heights.
The cause is **#156**, and it is a precondition for step 4, not a footnote — see below.

**What rounding costs, stated so #156 can weigh it.** The old measure was a POSITION delta, so a
sum over any range telescoped: Σ = round(top_last) − round(top_first), exact to 1px. A sum of
rounded SIZES does not telescope. On the fixture every record rounds down by 0.39px, so `P()`
drifts about −0.4px per record — roughly −60px over a 160-record transcript. The fractional
basis is the exact one; this page cannot hold it yet.

### Step 3 — the sparse-filter scenario

`classic_page_sparse_filter_mounts_only_what_matches` (`scenarios.rs`). The premise checked out
and was worse than the step described: **no filter case before this one exercised the sparse
window at all.** The page renders a filter's visible set in FULL while it is small — `filterFull
= nhits <= 50 && visible <= 400` (`export.js:1723`) — and every existing fixture is under that
ceiling, so their filters mount `[0, N)` and the range walk never runs. The new fixture is over
it: 160 turns, a `Read` on every third, 54 hits.

Measured, scrolled into the middle with the filter on: **118 mounted across 263 indices, 0
strays.** With `isHiddenRec` forced to `return false` — the dense model, which is the RED check
the case exists for — the same probe reads **107 across 107 with 60 strays**. So the two models
are 1.0× and 2.2×, and the case's floor sits between them with room to spare.

One thing the doc feared is bounded by the page's own vocabulary: a filter DIMS turn records
rather than hiding them (`isHiddenRec` excludes `isTurnKind`), so the visible set can never fall
below one record per turn. There is no ratio at which the window degenerates into pads.

### Step 4 — preconditions, verified against the code

The owner has settled the design question ("I prefer one-file, since I think the logic should be
reused. We will resolve regressions over time"), so what follows is cost, not a request for a
decision. Every line below was checked, because the step's own description names things that are
not there.

1. **Named in the plan, absent from the code.** `documentFrame`, `skip()`, `skipPredicate` —
   zero occurrences anywhere in the tree. They are work, not hooks waiting to be wired.
2. **A wrapper inside `#stream` is REQUIRED, and it breaks step 1's own CSS.** The engine calls
   `this.mount.replaceChildren()` (`:405`) and walks `this.mount.firstElementChild` (`:423`), so
   `mount.window` cannot be `#stream` itself with the pads as children. But the records are
   direct children of `#stream` today, and step 1's rules key on exactly that:
   `#stream > *  { margin-top: 0 }` and three `#stream > .blk:has(+ …)` selectors
   (`export.css:648-651`) — the very rules that make `scenario_a_record_measures_as_its_own_box`
   pass. They all have to move to the wrapper in the same commit, and that re-baselines the
   byte-identical gate again.
3. **Four browser cases die on the wrapper.** `view_state()` reports `blocks` as
   `#stream.childElementCount` (`browser_follow.rs:65`); with a wrapper it is permanently 3, and
   four cases assert `> 5`. The harness fix is to count `#stream [data-idx]`.
4. **`indexAt`'s clamp is already a parameter** of the pure function (`:32`), but the engine's
   method hardcodes `true` (`:228`) where the classic page passes `false` (`export.js:226`).
   One option on the class — the smallest of these, and not a fork.
5. **A skip cannot be expressed as a zero height.** `heightOf` is
   `this.heightFor(index) || this.estimateAt(index)` (`:220`), so a 0 falls through to the
   estimate. Sparse mounting needs a real predicate.
6. **`filterFull` is a second mount strategy** (`export.js:1723`) that the engine has no concept
   of: it abandons windowing entirely for a small filtered set.
7. **The document frame has no scrollbar test.** `elementFrame` defines
   `isScrollbarTarget: event => event.target === scroller` (`:569`); the document scroller needs
   its own definition before the thumb-drag mode means anything there.
8. **The correction has to come across with the frame.** The classic page nudges its position
   with `window.scrollBy(0, d)` in FOUR places (`export.js:459, 2126, 2669, 2720`), and the
   contract pins the engine as having none: `doesNotMatch(/this\.frame\.scrollBy\(/)`
   (`ui_contract.mjs:1620`), which #132 did deliberately. Porting the window while leaving those
   in place is not a port of the rules.
9. **#156 sits across the path.** The engine keeps fractional heights; step 2 measured that this
   page heals a scrolled-up reader when it is given them. The port hands it exactly those
   heights. Whatever #156 turns out to be has to be understood BEFORE the port, not discovered
   underneath it.

### Step 4, done (2026-09-09) — the port, and the score against the preconditions

Shipped in three commits: the `#vwin` wrapper, `documentFrame`, `clampIndex`, `skipAt` and
`renderAll` (`9c02aed`, riding along with #157/#159); the `afterMount` hook and the corrected
document scrollbar predicate (v1.241.0, `5dae349`); and the swap itself. The classic page is now
a subclass of `VirtualWindow` over `documentFrame()`; **59/59 classic scenarios pass**, and the
app shell's are unchanged.

**The preconditions above scored 5 wrong out of 9.** Recorded here because the pattern is the
point: every one of them was written from reading, and every one that fell fell the same way —
the code turned out to be simpler than the reading of it.

| # | Claim | Outcome |
|---|---|---|
| 1 | `documentFrame`/`skip` are work, not hooks | **held** — all three written |
| 2 | a wrapper breaks step 1's CSS | **wrong** — the rules moved to `#vwin` and all 57 scenarios passed, `a_record_measures_as_its_own_box` included |
| 3 | four browser cases die on the wrapper | **wrong** — one harness probe, `#stream [data-idx]`, and three selectors |
| 4 | the clamp is one option on the class | **held** — `clampIndex: false` |
| 5 | a skip cannot be a zero height | **held** — `heightOf` falls through to the estimate |
| 6 | `filterFull` is a second mount strategy | **held** — `renderAll` |
| 7 | the document frame needs a scrollbar test | **held, and the first draft of it was wrong** — see below |
| 8 | the four `scrollBy` sites must come across | **wrong** — only 2 of 8 are follow corrections and both are already the engine's absolute anchor path; the other six are navigation, a fold hold and drag-select. The engine pin stands untouched |
| 9 | #156 sits across the path | **held, and it was not the heights** — a long main-thread task delayed the scroll handler past the intent window. Both pages classify on the event's clock now |

**Six defects the port found**, each fixed where the rule lives — five here, and a sixth
below that took longer to see than the other five together:

1. **`rangeForScroll` indexes `scrollTop` straight into the sums** — right only when the pads are
   the first thing in the scroller, which they are on neither page (`.transcript-inner` has 24px
   of top padding; `#stream` sits under a topbar and a session header, measured at ~250px). So
   the fallback range, taken exactly where a dragged thumb leaves the reader, names an item that
   far late. `contentTop()` is the correction, scroll-invariant by construction, and `displaced`
   watches it for the same reason.
2. **`elementFrame.isScrollbarTarget` accepted any press on the scroller** — and
   `.transcript-inner` is `margin: 0 auto` inside `min(880px, 100% - 76px)`, so 38px of gutter
   each side IS the scroller. A gutter press entered drag mode for as long as the button was
   held, which is the whole of a drag-selection: there every scroll counts as the reader's, no
   anchor is held, and a converge is deferred indefinitely. This is the live twin of the
   `documentFrame` predicate that precondition 7 got wrong in its first draft (it accepted
   `body`, and `.layout` is centred at 1160px) — the same mistake, found from the other side.
3. **The engine heard a growth only from inside the mounted window.** The classic page had heard
   it from anywhere since #89/#98, through a `ResizeObserver` on `document.body`, and the port
   would have dropped that: the session header's meta chips wrapping to a second line move every
   record down and fire no scroll event. The engine cannot simply watch the content — that
   element holds the pads and measuring writes them, so the delivery re-fires itself — so
   `displaced()` is guarded on where the content BEGINS, which a pad write never moves.
4. **The classic page never re-guessed its heights on a width change.** #132 step 4 gave the
   engine `remeasure()` — scale the remembered heights by the width ratio rather than discard
   them — and this page's resize handler had always just re-windowed, keeping heights measured at
   the old width. That is exactly what `jump_to_bottom_lands_after_a_viewport_resize` was written
   about ("closing the rail widens the frame, so every height the virtualizer measured at the old
   width is suddenly wrong"), and the old page survived it only by luck of arithmetic. The page
   now calls `remeasure()`, with a `scaleHeights` of its own so the fallback is not
   `clearHeights` — which would drop every unseen record to the 30px floor and shrink the page
   under the reader, the one direction rule 5 forbids — and it seeds `lastWidth` from the mount,
   because the engine's is zero until the first remeasure and on this page the FIRST width change
   is the one that matters.
5. **The lazy sums needed marking per push, not per batch.** An observer delivery can reconcile
   between two pushes, and a reconcile reads the sums to place the pads; a prefix shorter than
   the record list reads `undefined` for the total and writes a pad height of `NaN`. The old
   code had the same shape and got away with it because only `postRender` and the scroll handler
   ever reconciled.

Defects 4 and 5 were found together, from `jump_to_bottom_lands_after_a_viewport_resize` — the
one case in the whole suite that the swap turned red. Both pages were instrumented side by side
through the same gesture, which is what separated them: the classifier's verdict was *identical*
on the two pages (`user: false`, gap 0, "none", 307ms since the last wheel), so the pin was never
the difference. The old page simply did not move, because its tail window had been twice as tall
and its heights therefore real. Reading alone would have blamed the classifier.

**Three wider fixes were written for it first, and all three were withdrawn by measurement.**
Widening `rangeAround` so the tail window is not a screenful short: three app-shell cases. Giving
the converge its own tail window instead: one. Holding the end as a POSITION for a reader who is
at it without following: five more — the shell's cases assert that a reader at the bottom keeps
their OFFSET when a pane opens, not that they keep the bottom, and that is a genuine difference
between the two pages rather than a bug in either. Seeding `lastWidth` in the engine rather than
on the page: three, because the shell's first remeasure is a pane opening, where clearing is what
its own cases were written against.

Each of those was a plausible reading of the failure and each reached the other page through the
shared engine, where only the shell's own suite could say so. What shipped is the narrowest thing
that was actually true: this page had never used a rule the engine has had since #132, and once
it did, the two withdrawn fixes were not needed at all — the case passes without them. That is
the argument for one engine restated as a hazard: a shared rule is shared in both directions, and
"it fixes the page I am looking at" is not evidence about the other one.

**One more defect, found last and the hardest to see.** The hit-nav's landing was not held.
`revealMark` expands a capped tool output to get to a hit; the engine measures that growth under
its own observer a frame later and holds the reader by their anchor — which on this page can only
be the RECORD, because `export.js` emits no `[data-block-index]` rows for #98's row-level anchor.
A record that grows INSIDE keeps its own top exactly where it is and pushes everything below it
down, the mark included: measured, the landing was correct and 956px past the viewport a moment
later. `goToId` has called `holdLanding` since #94 for exactly this ("a turn full of images moves
the page by thousands of pixels"); the hit nav never did, and relied on the scroll handler.
It does now — `holdLanding` takes an element as well as an id, and `goTo` reports whether it
actually moved the page so the hold is armed only when there is a landing to hold.

That one cost the most, and the reason is worth recording: the first bisect said the cause was
`contentTop` in `rangeForScroll` (three failures with it, one pass without). It was not — the
case fails without it too, and the single pass was luck read as a clean split. What settled it was
running the case three times on the PRE-SWAP page (3/3 green), which proved a regression without
naming one, and then a trace of `goTo`'s own arithmetic, which showed the landing was correct and
something moved it afterwards. A wrong bisect off one sample cost two rebuild-and-run cycles and
a filed-then-cancelled task (#175).

**Two differences remain, written down rather than left to drift — and they are not the same
kind of thing.** Only the first is a difference #174 should ALLOW:

- **Accepted.** The anchor's above-the-fold epsilon is 1px on both pages now; the classic page
  used 0. One rule, one number, no symptom — an allowlist entry.
- **Deferred, and CORRECTED after this note first shipped** (2026-09-09). The first version of
  this bullet said the classic page lacks #98's ROW-level anchor and that defect 6 was its
  symptom. Both halves were wrong, and they were wrong in the way this whole document warns
  about — read off a comment rather than off `captureDomAnchor`.
  The anchor has two levels: the mounted ITEM, then a refinement to the first `[data-block-index]`
  inside it. The app shell mounts UNITS holding several records and indexes each (nested children
  included, by the dotted path at `components.js:95`), so the refinement addresses RECORDS. The
  classic page mounts ONE RECORD PER ITEM — so item-level already IS record-level, and the
  "degradation" is harmless. Neither page anchors BELOW a record.
  The real difference is narrower: a classic mounted item can still hold NESTED records
  (`export.js:189`, the `blocks` part) which carry an id but no `data-block-index`, so a nested
  child growing above the reader moves them. That is **#176**, rewritten to this scope.
  And defect 6 is NOT its symptom. That growth is inside one record's BODY, below record
  granularity, where neither page anchors — and it was never an anchor failure: the reader's
  anchor held, the MARK moved. A landing problem, which is why `holdLanding` at the navigation
  site is the whole of the fix rather than a paper-over.

**Three scenarios** were written for the risks that had no coverage, each run on both surfaces:
a landing holds through a growth above it (`holdLanding` against the engine's kept anchor); a
growth around the run displaces the reader, pinned and reading; a press in the gutter is not a
thumb.

**The ten `#107` `export.js` pins in `ui_contract.mjs` are rewritten.** They named this page's
own sums, pads, anchor, owed correction and scroll classifier — every one of those rules MOVED
rather than weakened, and the engine pins above them now hold each once for both pages. In their
place is what only this page can say: that it extends the engine, that it keeps none of the
machinery (`doesNotMatch` on the scroll listener, the body observer and the rule calls), and the
three parameters that carry its genuine differences.

## The anchor's two rules, amended (2026-09-10, #178 and #180)

Two changes to `captureDomAnchor`/`restoreDomAnchor` landed within a day of each other, and both
narrow a rule rather than adding one. They are recorded here because in each case the existing
scenarios passed either way, so the suite is not where a later reader will find the reason.

### #178 — one predicate, applied from the mounted item down

`#177` made the anchor DESCEND from the first qualifying `[data-block-index]` into the innermost
one, because a parent qualifies whenever a child does and document order offers the parent first —
so an unrefined pick is always the OUTERMOST row, which on the app shell is the `.process-surface`
wrapper rather than the record the reader is inside.

Writing the case for that guard (`#178`) found a second defect, and the fix for both is the same
predicate applied one level higher: **refine only while the thing being held STRADDLES the viewport
edge.**

```js
let row = null;
for (let scope = child; scope.top < viewportTop; scope = row) {
  const inner = rowIn(scope.element);
  if (!inner) break;
  row = inner;
}
```

Whatever straddles is the only thing whose top the reader cannot see, so it is the only thing whose
top lies about where they are reading. When the item's own top IS visible the anchor now refines
nothing — that top is already the better anchor, since every row inside sits at a fixed offset below
it. On the classic page the mounted item IS the record and `matBlock` indexes only its nested `.blk`
DESCENDANTS, so before this the anchor jumped to the child below a visible head and a growth in that
head drove it **420px off the top of the screen** (measured 9 → -411). `#176` introduced that; before
it, no `.blk` carried the attribute and the item's own top held the head correctly.

**Reach, stated honestly:** because `firstVisible` picks the STRADDLER, "the item's top is visible"
can only happen within one inter-item gap of the edge — about 10px on the classic page, about 52px
on the app shell (the `.process-surface` margin plus its headbar). Worth doing for the correctness
of the rule; not something a reader hits often.

### #180 — "never write under a moving reader", except to undo our own displacement

`#132` step 3 and `#134` added the guard that defers a correction while `readerOwnsPosition()` is
true, and `#138` made the settle DROP the deferred debt instead of paying it. That was right for the
case it was written against: the debt held a position captured BEFORE the reader moved, so paying it
late dragged them back.

It is wrong for one path. On a SCROLL, `rangeAround` mounts a run of items ABOVE the reader whose
remembered height was a floor estimate — 30px classic, 34/40/44 on the shell — against a real height
five to twenty times that. `measureMounted` replaces every estimate with the truth, the pads absorb
only what is outside the window, and the difference lands above the reader with `scrollTop`
unchanged. Not writing does not leave the reader alone: **it displaces them by exactly the correction
being withheld,** and `#138`'s settle then drops it, so the displacement is permanent. `this.owed` is
assigned in that one line and nowhere read back.

Measured, walking up a 120-turn transcript in 900px steps: the record under the reader moved **2355px
and 2854px** on the app shell and **3263px** on the classic page for a 900px request — an overshoot of
up to 2363px per step — with `scrollHeight` growing by the same amount each time. Steps over ground
the tail jump had already measured drifted 0, which is the first-time-only asymmetry the owner
reported.

So `restoreDomAnchor` takes an `immediate` flag, threaded through `measureMounted` → `reconcile` →
`updateWindow`, and **`onScroll` is the only caller that opts in.** This does not reopen `#138`: that
correction was stale, computed before the reader moved; this one is computed AT the position they are
at now and replays nothing. Over already-measured ground the correction is zero and returns before the
guard, so it fires only where it is the lesser harm.

Every other caller keeps the deferral — the drag end (the thumb owns the position, and the anchor is
null there anyway), the jump paths (they run their own landing loops and stamp `lastUserInput`
precisely so the anchor does not fight them), and every apply path.

**What the suite cannot tell you, and a hand-check should.** Headless Chrome has no momentum
scrolling, so the case proves the correction LANDS but not how a trackpad fling feels once it does.
The secondary mitigation is `#184`: the floor estimate is what makes the correction large, and rule 5
("estimate UNDER, never over") is free only for learning BELOW the reader — above them it MAXIMISES
the delta.

## The pads are the page's height, and a page that shrinks moves the reader (2026-09-10, #179)

Reported: *"jump to the bottom, then scroll up a little bit, then scroll down, and it would let me
keep scrolling (not stop at the bottom), but the screen simply refreshes the last few messages."*

It is a **two-cycle**. Measured on a 120-turn session, app shell, 200px steps:

| step | scrollTop | gap to bottom | mounted | top record |
|---|---|---|---|---|
| at tail | 12972 | 0 | 15 | `user:t119` |
| up 200 | 12772 | 200 | 17 | `assistant:b118` |
| down 200 | **12710** | **262** | 18 | `user:t118` |
| down 200 | 12910 | 62 | 17 | `assistant:b118` |
| down 200 | **12710** | **262** | 18 | `user:t118` |

— and so on for ever, between exactly two positions, which is what "the screen simply refreshes the
last few messages" is. The classic page does the same thing at 228/28, its own turn height.

**No script wrote that position.** Shadowing the scroller's own `scrollTop` setter and recording the
stack of every write shows a write on the 62px step (`restoreDomAnchor`, 16px, correct) and **none at
all** on the 262px step: two scroll events 28ms apart, 12972 then 12710, with nothing in between.
`overflow-anchor` is `none`, so it is not native scroll anchoring either. The browser moved the reader
because the page it was scrolling had got **shorter**: 12710 is the bottom of a 13358px page, and the
page is 13620px before and after. 13620 − 13358 = 262 = one turn = 78 (a prompt) + 184 (its answer).

The shrink is inside `reconcile`, and it is an ORDERING bug. `reconcile` mutates the mounted set, then
`afterMount` (the classic page's clamp pass — one batched READ over the fresh elements) and
`measureMounted` (every mounted child's box, both pages) **force layout while the pads still describe
the window being replaced.** For the length of that measure the content is short by exactly what the
new window dropped off its top, a browser clamps `scrollTop` to a page that has just shrunk, and a
reader sitting ON the tail is pulled up by the whole difference. The pads land a moment later and the
page is its old height again — with the reader 262px above the bottom.

Then it compounds: the clamp's own scroll event arrives inside the intent window, so `onScroll`
reads it as **the reader's**, and 262px is past the 80px hold slack, so the page unfollows on a
movement the reader never made. The next wheel re-acquires the tail, and the cycle closes.

The fix is one line moved: **pad, then measure.** `updatePads()` goes above `afterMount`, and the
trailing call stays because `measureMounted` can still rebuild the sums under it.

This is not the order `#140` step 4 rejected. That one wrote the pads for the NEW window while the OLD
elements were still mounted, which is short whenever the window grows. These pads describe exactly
what is mounted at that instant. And its stated worry — that a measure makes the pads wrong — cannot
happen: the pads are `prefix[lo]` and `prefix[count] − prefix[hi]`, sums over the items OUTSIDE the
window, and a measure only ever corrects the ones inside it.

**The rule it adds:** *the scrollable content must never be shorter, at any instant, than it was a
moment ago.* Not "eventually right" — a browser reads the height synchronously, the moment anything
forces layout, and it acts on it. The residual this leaves is a `renderItem` that reads layout mid-
loop; neither page's does today.

## Rule 5 amended: estimate CLOSE, not merely UNDER (2026-09-10, #184)

Rule 5 has said, since `#107` step 3, *estimate UNDER, never over*. Its reason was sound as far as
it went: guess HIGH and learning the real height SHRINKS the page, and a shrink above the viewport
is a jump unless the anchor catches it. What it never said is what guessing LOW costs, and the
answer turns out to be most of `#180`.

A constant floor — 30px on the classic page, 34/40/44 on the shell — is not a neutral choice. It is
the choice that **maximises** the distance between the guess and the truth, and that distance is
exactly what lands above a reader when a run is mounted and measured. Measured on a 120-turn
transcript, walking up from the tail in nine 900px steps against the floor:

| | asked | turns passed | the page's own height moved |
|---|---|---|---|
| app shell | 8100px | 31 | **5539px (68%)** |
| classic page | 8100px | 35 | **5643px (70%)** |

Two-thirds of every pixel the reader climbed was the page growing under the correction that had to
hold them still. `#180` made that correction land; it did nothing about its size.

`#179` then removed the other half of the old fear. The pads now carry the page's height before
anything forces layout, so a page that shrinks under a reader no longer clamps them — the anchor is
the only thing that has to catch it, and it catches in both directions equally.

So the rule becomes **estimate CLOSE, and let the floor stand only while there is nothing to learn
from.** `HeightGuess` (shared) is a running mean over the heights this page has actually measured,
seeded by the old floor. Both pages feed it from the one place they record a height and ask it from
the one place they answer `estimateAt`.

**Why a mean, when it is wrong about every individual record.** What a mounted run costs the sums is
a SUM. A mean that guesses a 78px prompt high and its 184px answer low leaves the pair they form
exact, which is the only quantity the reader can feel. That is also why the classic page keeps ONE
mean for the whole page where the shell keeps one per unit type: the shell's three types are three
populations that do not interleave, and the classic page's records do.

**The two guards, which are the whole difference between a mean that helps and one that is worse
than the floor.** Nothing is used until eight records have been seen, so one short record cannot set
the page's idea of a height. And a sample is CLAMPED to four times the running mean rather than
rejected: one enormous record must not drag the mean, but dropping it altogether biases the mean
low, which is the old bug wearing a hat.

**What it does not fix, stated plainly.** A mean shrinks the error on a page whose records are alike
and does much less for one whose records are not. `#180`'s case now runs on `fixture_varied_prose`
— answers of 1, 40, 5 and 18 lines, cycling — precisely because over uniform prose the guess became
good enough that the case could no longer produce the error it exists to survive: `scrollHeight` did
not move once across the whole walk, and the case failed its own not-vacuous guard. That guard is
now `|Δh| > 1` rather than `Δh > 1`, because with a learned mean a record SHORTER than the mean
shrinks the page, and a one-signed test would call a step over fresh ground vacuous.

**Amended by #194 (2026-09-12).** "Learning only ever grows the page BELOW the reader, which nobody
feels" was this section's premise, and it is false for the common reader — the one who opened the
session at its tail. Every record above them is unmeasured and carries the mean, so a learned height
re-estimates all of them at once: the page moves ABOVE the reader, by the number of such records
times the change. Measured on the owner's session: 8,500 records above, a fraction of a pixel of mean
movement, a 7.8k px shift. And a re-measure taught the mean AGAIN — a live tail re-rendered on every
delta walked it 337→362 in ten deltas. Rule 5's "close, not merely under" stands; what changes is WHEN
the sums may take a new estimate (at rest, anchor held) and HOW a record teaches (once). See below.

## Growth the reader ASKED for is not the tail moving away (2026-09-10, #185)

Reported on v1.248.0, minutes after `#179` shipped:

> "I scroll to the end, then click show more on a block, the block unfolds downward correctly
> (anything above it is not moved), so now the page is no longer at the bottom. However, apparently
> the engine did not think so and immediately snaps the page to the bottom."

The first half of that sentence is the anchor working perfectly, and the second half is the follow
rule working perfectly, on a page where they disagree. Parked at the tail the page is still
`following`. The expansion makes it taller BELOW the reader, so it is no longer at its tail, and
`convergeBottom` puts it back — scrolling away the very lines the click revealed.

**The rule this adds:** *who caused the growth decides whether the pin survives it.* Growth that
arrives on its own is the tail moving away from a follower, and the pin is what they asked for.
Growth the reader produced by clicking is them choosing something to read, and the pin is then in
their way. `readerReshaped()` drops it, and the pill lights up to offer it back.

This does not weaken `#165`, which defers a converge while a GESTURE is in flight and KEEPS the pin.
There the tail really did move; here nothing arrived. Same guard, opposite answer, because the
question is different.

**Unconditional, and that was a decision.** A height test was proposed and withdrawn by the owner
within the hour — converge when the opened block is short, unfollow when it is tall — because the
same click would then do two different things depending on the block, which nobody can predict from
outside. "It is just one scroll away to re-pin the tail, and feels natural."

**What the investigation actually cost, and the lesson in it.** The first fix covered fold heads
only, and the case still failed on the app shell. There are TWO paths by which a reader grows the
page, and they do not meet:

| control | how the growth reaches the engine |
|---|---|
| a fold head | `rerender` → `render()` → `reconcile` — the engine is told |
| **"⋯ N more lines"** | revealed in place; the engine hears it only through the ResizeObserver |

The cap expander — the control the report was actually about — never reaches `rerender` on either
page. So a fix written at the re-render seam covers the control nobody complained about and misses
the one they did. Both seams need arming, and a scenario that clicks a fold head cannot tell you so.

**A second thing the case had to learn the hard way.** A fold head is the wrong instrument for this
measurement twice over: its click is a four-step cycle, so clicking a head the page had already
opened CLOSES it (measured: the classic page shrank 76px and the case reported itself vacuous), and
a capped output can sit several folds deep inside a parent whose body is `display: none`, so opening
the innermost fold leaves the expander with no box at all. The case now opens the whole ancestor
chain, outermost first, re-querying between clicks because a re-render replaces the nodes under it.

## A reshape is not a scroll (2026-09-11, #190)

Reported on v1.252.0, at turn 1204 of a 1210-turn session, pinned at the tail:

> "click on 'show 8 more'; the turn header shows 'Turn 1202' and the rest of the view blank; after
> a few seconds the turn header shows 'Turn 1181' and the rest of the view blank; even the 'Turn
> 1181' line disappears, and the page is still blank; 'Turn 1182' reappears with the page filled
> with contents. From 2) to 5), I did nothing to the screen."

Measured on that session, on that monitor: the click's re-render re-ranged the window and moved
the top pad by 14,243px, `scrollTop` moved 828, and the last mounted record's bottom sat 13,195px
ABOVE the viewport — the reader was in the bottom pad. Even "Show 2 more" did it.

**The cause is a rule meant for scrolling, applied to a click.** `noteIntent` binds `pointerdown`,
so every click starts the `userIntentMs` window in which `readerOwnsPosition()` is true. Inside it
`restoreDomAnchor` DEFERS its correction as owed (`#132` step 3) and `scheduleSettle` DROPS it
(`#138`) — right for a scroll, where paying an old position after the reader moved drags them
back. But a click on a control moves nothing: no scroll event fires, the reader has not moved, and
what is withheld is the engine's own re-measure. `#180` had already named this on the scroll path —
"not writing does not leave the reader alone, it displaces them by exactly the correction being
withheld" — and at a 1200-turn scale that correction is fourteen thousand pixels.

**The rule this adds:** *the click that reshaped the page does not own the position the reshape has
to correct.* `readerReshaped()` — the seam `#185` put at every control that grows the page — now
also clears `lastUserInput`, so the correction lands at once. It is the `#185` rule taken one step:
who caused the growth decides whether the pin survives it, and whether the correction waits. The
EVENT clock (`lastInputStamp`) is left alone: a scroll that follows the click is still the reader's
own, and `onScroll` classifies against that stamp, not this one.

**Why no case had caught it, and what the case needed.** Ten fixture shapes, four real sessions and
the owner's own monitor had all held, because a synthetic `element.click()` fires `click` and nothing
else — no `pointerdown`, no intent window, and the correction always landed. The scenario now
dispatches the `pointerdown` a finger makes, and that one line is the whole difference between a
green case and the bug. Two more traps in writing it: on the classic page every third record at a
process tail is an absorbed `tool_result` with no box, so "the last closed fold" was a HIDDEN one
whose header top of 0 fires `toggleFold`'s ease-to-104 smooth scroll — the case now clicks a fold
whose header is visible and clear of the sticky bars; and "blank" read off `elementFromPoint` is
wrong wherever chrome is — the sticky turn bar at 5% and the "new" badge at 95% are not pads. The
predicate is now: no probe lands in a pad (`.vpad`, `.virtual-pad`), and the TEXT at 30/50/70% of
the viewport is the same text at the same height, within 2px — exact on both pages where "the record
at the middle" is not (a whole turn is one `data-turn` on the shell; a tail row is 32px on classic).

**Why the app-shell mock could not fail at first, and what made it (#193).** The classic mock was
RED on the old engine from the start; the app-shell mock held the same predicate green on the old
engine too, and for a day the shell's only evidence was the owner's session. The trace (#192) showed
the difference in three lines: on the old engine the click's re-measure produced `measured` with the
pads UNCHANGED — no `restore:*` at all — because `#184`'s per-type mean stays on its floor until eight
samples have been measured, and a fresh open pinned straight at the tail has measured three. On the
floor the expanded surface moves no estimate, no pad moves, nothing is ever owed. The owner's session
had measured hundreds. Six screens up and back before the click — the reader who has BEEN in the
session — and the same trace reads `measured` pads +7,398, `restore:deferred` delta −7,398 twice at
5ms since input, `settle … dropped=true` at 328ms, the surface 7,589px below the viewport: the bug,
in the mock, on both surfaces. A case about an estimate shift has to warm the estimator first.

**The one interaction the fix introduced, and what it took to answer it.** The classic page eases
a fold head that sits under the sticky bars to 104px, and a correction that now lands mid-ease
cancels a SMOOTH scroll (measured: a header at 80px stopped 8px into a 24px ease; before the fix it
reached 96). v1.254.0 answered by stamping the ease as intent (`markIntent`, as the drag auto-scroll
does) — and CI showed that to be wrong within the hour: the stamp also fires for SYNTHETIC clicks,
so the rendering audit's opener, which clicks four hundred heads by query with most of them off
screen, had its ease scrolls read as the reader's own and the page moved out from under six classic
cases. The answer that held is two decisions in `toggleFold`: the ease is an INSTANT nudge (a scroll
event like any other, behind which the engine re-reads its anchor), and it fires only for a head
whose sliver below the bars was on screen to be clicked — top under 96, bottom past it. A head above
the viewport or wholly under the bars was toggled by something other than a click on it, and moving
the page to it was never what that asked for; the old smooth ease fired for those too and was only
ever cancelled by luck. A head clicked at 80px now lands at 103.

**The precaution audit found the sibling (`#191`).** A long WHEEL jump from the tail into
unmeasured ground — −40,000px, and on the classic page even −6,000 — leaves the reader in a pad on
BOTH surfaces, at rest, until they scroll again: `updateWindow` captures its anchor before the mount,
no old child is visible after a jump that size, the anchor is null, and when the measure moves the
`HeightGuess` mean the top pad grows by thousands of pixels under a `scrollTop` nobody corrects.
That is the same "blank, then scroll a bit and content appears" the owner described, by a different
door, and it is queued as its own task with the fix shape (hold the MODEL position when there is no
DOM anchor).

## A jump into a pad holds the record the sums named (2026-09-11, #191)

Found by the `#190` precaution audit, not reported — but it is the other half of the owner's
sentence, "scroll a bit, it shows content". A wheel jump from the tail into ground the engine has
never measured — −40,000px, and on the classic page even −6,000 — left every probe in a PAD, at
rest, on both surfaces: the mounted window sat 1,944px below the viewport on the classic page and
6,709px on the shell, and nothing moved until the reader scrolled again.

**The hole is a null anchor.** `updateWindow` captures the DOM anchor BEFORE the mount, and after a
jump that size no old item is on screen, so there is nothing to capture. The mount then does what a
mount does: `measureMounted` learns forty real heights, the `HeightGuess` mean moves (`#184`), every
unmeasured record above re-estimates, and the top pad grows by thousands of pixels — under a
`scrollTop` that `restoreDomAnchor(null)` cannot correct. `#180` made the scroll path's correction
IMMEDIATE; this is the path where there was no correction at all. And once in the pad the reader
stays: `syncAnchor` finds no visible item either, so no later measure has anything to hold.

**The rule this adds:** *when no item can hold the reader, the sums do.* Before the mount,
`modelAnchor()` reads the record the scroll offset names and how far into it; after the mount,
`restoreModelAnchor` writes `documentTopOf(index) + offset` and settles the window around the
corrected offset once (the range was chosen from the sums before the measure, and the viewport may
now run past it). It writes regardless of intent, and that is `#180`'s argument, not a new one: the
position is computed from where the reader IS, so it replays nothing (`#138`) and undoes only the
engine's own shift. `restoreDomAnchor`'s note about "placing from the sums was tried and reverted"
is a different case — a MOUNTED anchor whose heights a width change had just cleared; here there is
no anchor and the sums are the only position there is.

**One thing the first cut got wrong, caught by the suite.** The offset inside the named record is
SIGNED. Clamping it at zero read well — "how far into the record" — and pulled a reader who had
jumped to the very top down onto record 0, 250px past the page header: two turn-bar cases went red
on both surfaces, because the bar lit up where the first turn should have been naming itself. Above
the first record the offset is how far above; past the last it is the bottom padding; the restore
reproduces the offset the reader had, whatever its sign.

**The case** (both surfaces, −6,000 and −40,000 from the tail): no probe at 20/50/80% lands in a
pad, the viewport shows content at rest, nothing moves the reader once they have stopped, and the
first mounted unit is earlier than the tail's and past the start. Red on the old engine on all four
(every probe in a pad), green with the fix. A scroll offset cannot express "moved up": the mount's
measure moves the whole page's height, and a −6,000 jump legitimately lands at a LARGER offset than
the tail's — the mounted range is the coordinate that survives the shift.

## The engine keeps a record of what it did (2026-09-12, #192)

Asked for by the owner after `#190`: "build the diagnostic feature so that it would be easier for
me to report bugs in the future." Reproducing that bug took five screenshots, a live session and a
probe harness, because the page kept no record of what the engine had done; `#191` was found by a
probe that had to be written from scratch. The engine should be able to say it.

**What it is.** `trace(event, fields)` on `VirtualWindow`, called at every seam where the engine
decides something. As first shipped: `reconciled` (range, dirtyFrom, refresh, anchor, fresh mounts,
the estimate), `update` (anchor or model anchor, forced index, range), `restore:wrote` /
`restore:deferred` / `restore:unmounted` (the DOM anchor's verdict and delta), `model:wrote` /
`model:held` (`#191`), `settle` (and whether a debt was dropped), `reshaped`, `scroll` (user or not,
the verdict, the gap, the event-clock lag), `converge` / `converge:deferred` (pass, commanded),
`remeasure` (ratio) and `measured`. Since `#196` stage 2 the engine is a set of transactions and the
trace is their log: one entry per transaction under its cause (`update`, `reconcile`, `converge`,
`measure`, `displaced`, `grown`, `estimates`, `remeasure`, `render` — the position it started from,
whether and how it placed, the range and how it was chosen) and, inside them, `reconciled`, `place`
(source, offset, correction, the reader's drift) / `place:unmounted`, `scroll` / `scroll:own`,
`estimates:pending` / `estimates:applied`, `tail:deferred` / `rest`, `reshaped`, `measured`. Every
entry carries the same base: sequence number, `performance.now()`, following and dragging, the
mounted range and count, scrollTop and scrollHeight, both pad heights, the ms since the reader's
last input, the stored position and what is pending. `CLAUDE.md` keeps the current vocabulary.

**Where it goes.** A ring of 500 entries at `window.__viewportTrace` — `copy(window.__viewportTrace)`
pastes it into a bug — and one `console.debug` line per entry under `[viewport]`, so a console filter
shows the engine's story and nothing else.

**How it is switched.** `?trace=viewport` in the URL for one load, or `localStorage.viewportTrace =
"1"` to keep it across reloads; `traceWanted(search, stored)` is pure and the contract tests it. The
engine decides once, at construction, and off no entry is built: `trace()` returns on one boolean.
A seam still pays for its own argument — JavaScript evaluates the fields object before `trace()`
sees the flag — so an object literal and a few rounded reads happen per seam regardless. That is
negligible beside the reconcile that called it and not worth a thunk at thirteen call sites, but
"computes nothing", which this paragraph said until 2026-09-12, was too strong. One implementation,
both pages, through the shared module — the classic page inlines it and
the shell imports it, so `window.__viewportTrace` reads the same on either.

**The case** opens each surface, checks nothing is recorded, re-opens with the flag, and asserts the
load was recorded with the fields a report needs, that a wheel leaves a user-classified `scroll`
verdict and the `update` it drove, and that sequence numbers climb inside a ring that holds. Red on
the engine without the trace (no buffer at all), green with it.

## A record teaches the mean once, and the sums take a new estimate only at rest (2026-09-12, #194)

**Reported**, on 1.256.0, in one hour: "simply scroll to the bottom, scroll back up, and then scroll
down, and it never stops"; "every time I attempt to unfold and read, then scroll, the page jumps to
somewhere else"; on the classic page, "scroll back to turn 766, then scroll forward gradually, it will
go down to turn 767, 768, 769, and right after scrolling to 769 [it] jumps back to 767 … and it will
forever cycle through 767 to 769 if I just smoothly scroll". Three symptoms, one mechanism — and the
mechanism is upstream of `#190` and `#191`, which were real fixes of its downstream effects.

**What the trace showed** (`#192`, on hermetic copies of the owner's two sessions, both surfaces,
scratchpad probes `tmp_runaway.rs` / `tmp_walk.rs`):

- *The live session, hands off for sixteen seconds.* Every second a delta re-rendered the open turn's
  tail (`dirtyFrom=611, fresh=4`, the count flat at 615); every re-render was measured again and
  taught the mean again: the shell's estimate climbed 337→362; the top pad grew ~1.7k px a cycle;
  `restore:wrote` rewrote `scrollTop` by the same each time. The classic page rewrote it eight times
  in eight seconds. The static copies: no drift at all.
- *The gradual walk on the classic page.* `count=8715`, the reader at record 8526 — 8,500 records
  above them, none measured. The mean moved by a fraction of a pixel per newly measured record and
  the top pad by ±8k px per 400px wheel. At one wheel the observer's late measure (an image decoding
  after its record was mounted) shifted the pad 7.8k px with the restore deferred (`restore:deferred
  delta -7776`, the reader owned the position); the wheel's own update then found no mounted record
  under the viewport (`anchor: null`), fell back to the model anchor computed from the shifted sums
  (`held: 8462`, `model:wrote`), and mounted a window ninety records up: turn 763.
- The app shell did not jump on that walk. Its three per-type means over larger units move less per
  measure than the classic page's one mean over 8,715 records with a floor of 30 — but it drifts the
  same way under a live tail. One engine, one cause, two sensitivities.

**Two causes.** (1) `HeightGuess.learn(height)` was called on EVERY measurement, so a record measured
again — the open turn's tail on every delta, a growing record at every size, a record measured at
mount and again when its image decoded — added a NEW sample: a mean over measurements, not over
records. (2) The sums read the LIVE mean, so any movement of it re-estimated every never-measured
record at once, and above a reader who opened at the tail that is thousands of them. At rest the DOM
anchor restore hid the shift (the scrollbar jumped, the content did not). Under the wheel the restore
was deferred and the next wheel dropped it (`#138`, deliberately), or the model anchor read the
shifted sums — either way the reader was somewhere else.

**Two rules, in the shared module and both pages.**

1. *A record teaches the mean once.* `learn(height, previous)` withdraws the share the same record
   contributed last time (the page keeps it beside the height: the shell's `shares` map on the
   Viewport instance, the classic page's `recShares[]`) and returns the new one; `forget(share)` for a
   record a rewrite dropped. The mean is over DISTINCT records as they are NOW: re-rendering the tail
   ten times adds no weight, a record that grew moves its own share and nothing else.
2. *The sums take a new estimate only at rest.* The live mean keeps learning; what the sums read is
   `HeightGuess.estimate()`, the value as of the last `apply()`. The engine applies from the measure
   that moved the mean when the reader does not own the position, else from its own timer once the
   intent window has passed — capturing the DOM anchor first, rebuilding the sums, restoring the
   anchor immediately, and settling the window around the corrected offset (`estimates:pending` /
   `estimates:applied` in the trace, `measured` now carries `live` beside `estimate`). Never through
   `owed`/`scheduleSettle`: a user scroll dropping the correction it is owed is deliberate (`#98`,
   `#132`), which is exactly why the shift itself has to wait rather than be corrected.

**Two more things the case found once those two were in**, each caught by the trace at the failing
wheel (`CR_VIEWPORT_TRACE=1` re-opens the page with the trace on and prints every decision between a
failing wheel and its measurement):

- *The shell dropped every unit's height from the rewrite point on each delta*, so for the length
  of one render the sums were short by the whole open turn; `afterMount`/`measureMounted` force
  layout inside that gap, the browser clamps `scrollTop` to the shorter page (`#179`), and the reader
  is pulled up by the difference — 31px on a 300px wheel. A rewritten unit now keeps its last
  height as the provisional value until the measure in the same task replaces it.
- *A delta's reconcile mounted one record ABOVE the reader mid-gesture.* Fifteen milliseconds after
  a wheel, the growth path chose its window around the anchor — one record above the reader's own
  window — mounted it (98px estimated, 120px real), computed the 22px correction, deferred it
  because the reader owned the position, and the next wheel dropped it (`#138`). The reader's own
  scroll path is the one that mounts above them, and it corrects at once (`#180`); so a path that is
  not the reader's does not mount above them while they own the position (`reconcile` holds `lo`
  under a gesture when it has an anchor). Ten traced runs green on the classic page after this;
  it had failed one in five before.

**What is held, and by what.** Rule 1 by construction (the node contract: re-learning one record ten
times leaves count and sum unchanged; a grown record still counts once; a forgotten record leaves the
mean to the others; `estimate()` holds until `apply()`). Rule 2 by policy — a timer — which is the
honest description, and the reason `#195`/`#196` exist: the owner's directive after this bug was to
make the virtual window a framework whose invariants hold by construction, and to keep an hour of
reader actions and engine states so a report is a reproduction (`#197`). The browser cases:
`a_reader_above_a_growing_tail_moves_only_by_their_wheel` on both surfaces, red on the old engine on
both (300px asked, −821px moved on the shell, −1,922px on the classic page); the classic mock went red
only once its tail stayed inside the mounted window (a 2,700px climb had unmounted it — the `#193`
lesson again) and its growth records were TALL. `a_gradual_walk_forward_keeps_its_place`, written for
the owner's walk, stayed green on the hermetic fixture even with pasted images every third turn: the
walk that reproduced on the real session is held by the real-session probe until `#197`'s sandbox can
carry its height profile into a short synthetic one.

**Not here.** The delta emitter re-renders the open turn's four tail records on every delta
(`dirtyFrom` every second with nothing but the last record changed); with rule 1 that costs a
measure and nothing else, and it is its own task. The classic page's one-mean sensitivity — 8,715
records at a 30px floor make every fraction of a pixel a page — is a design question for `#195`.

**The third report, replayed after the release (2026-09-12).** "Every time I attempt to unfold and
read, then scroll, the page jumps to somewhere else." On hermetic copies of the knack session, static
and with its real tail appended live, on both surfaces: open a closed fold that is in view with a
pointer gesture, read for a second, wheel six times down and six times up by 400px, close it, four
more each way — at three positions (two screens above the tail, mid-session, three quarters in).
Forty-four wheel phases: the record under the reader moved by what was asked within 1px every time,
was never unmounted, and the turn under the reader never went the wrong way; the click itself moved
the fold head by at most 1px. What a reader can still see is the scrollbar: with about fifty
measured records of 2,756 carrying the mean, one measurement moves the estimate by about a pixel
and the thumb by up to 7.6k px at rest — the restore holds the content, and the deltas are in the
trace — so the estimate's stability is `#195`/`#196`'s question, not a residue of this one.

The same probe reproduced the navigator report the earlier probes had missed. At the tail the app
shell names the turn of the first unit that STARTS in the viewport, so a short last turn leaves the
pane on the turn before it (the live copy: pane 183, last turn 184), and a unit that spans the whole
viewport leaves it on nothing (the static copy: no row current, bar off). The classic page's spy has
both rules the app shell lacks — the last header above the line names the turn, and at the bottom the
last turn wins (#89). That is `#199`, its own change.

## The spy names the turn the reader is inside, and at the bottom the last turn wins (2026-09-12, #199)

**Reported** under #194, on the app shell: "scrolling to the bottom (the active turn), the turns pane
lost focus (should be focused on the last turn)" and "jumping to the bottom, still the last turn was
not selected in the turns view". Twice it read as not reproduced — on copies of the owner's session
whose last turn happened to be tall enough to reach the top line. The unfold probe's live copy ended
on a short turn and showed it at once (pane 183, last turn 184); its static copy showed the other
shape: no row current at all.

**Cause.** The app shell's `unitAtTop()` returned the FIRST mounted unit whose top sat at or below a
line 24px above the viewport's top edge — the first unit that STARTS in the viewport. That names the
unit BELOW the one being read whenever the one being read spans the top edge, and nothing at all
when no unit starts in the viewport (the bottom of an answer taller than the window). And it had no
end rule: at the document bottom no further header can ever cross the line, so a last turn shorter
than the window could never become current — exactly where a pinned live tail sits. The classic
page's `spy()` has had both rules since #89: `cur` is the LAST turn whose header is above the line,
and at the bottom the last turn wins.

**The change, `codex-ui/app.js` and `viewport.js`.** One rule for the turn bar, the navigator pane and
the keys, `currentUserUnitIndex()`: at the bottom (`viewport.following`, or the gap to the bottom
within the engine's `hold` slack — the same 80px under which it keeps following) the last user unit;
everywhere else the turn of the unit at the top, where `unitAtTop()` is now the last mounted unit
whose top is at or above the line, the first child standing in when nothing has scrolled past it.
The line sits just BELOW where a jump lands its target — `viewport.landing`, 18px or the turn bar's
height as `scroll-padding-top`, the value `jumpToRecord` already used — as the classic page's
`STICKY_Y` sits just below its `GOTO_Y`: otherwise a turn the reader clicked in the pane lands under
the line and the spy keeps the previous turn. The count of user units is taken inline rather than
through `userUnits`, because the first render runs while the module is still being evaluated and
that `const` is declared further down (a temporal dead zone blanked the shell on the first try; the
console said so).
Two consumers inherit the new reading without being named: `recordIndexAtTop()` (#118, the nearest
hit for a cold step or a filter) now starts from the unit containing the top line rather than the
first one starting below it, and `viewport.landing` is read on every scroll rather than only on a
jump — one `getComputedStyle` beside the layout reads already in that path; noted, not acted on.

**What changed for the keys.** `]` and `[` step from the same reading. After real paging the next
header can sit a little below the spy's line; `]` then lands THAT header under the bar — a short
forward hop — where it used to skip to the one after it while the bar named the wrong turn. The
classic page's step has a 128px dead zone for the same reason its landing is 120px: the structure
(line just below the landing) is the parity, not the number.

**Held by** two scenarios on both surfaces, red on the app shell before the change and green on the
classic page throughout: `scenario_at_the_bottom_the_last_turn_is_current` (a short last turn: the
pane and the bar name it after a jump and again after wheeling down; away from the bottom they name
the turn at the top) and `scenario_a_turn_taller_than_the_viewport_names_itself` (two answers of ~900px
against a 650px window: at the bottom nothing starts in the viewport and the last turn is named; a
screen up, with the next header in view, the turn being read is named). The deep-jump scenario now
measures a step by the spy (exactly ±1) with the first-header reading kept as the no-leap bound. The
node contract pins the shapes.

**Two things the case found on the way.** A raw newline inside a builder's text (`user_at`,
`assistant_at`, `thinking_at`) splits the JSONL line and the record is silently dropped: the #194
walk fixture's "varied answers" had never existed, and three app-shell cases had lost their
multi-line user turn the same way. The builders now assert on it, and the fixtures pass `\\n`. And
Space pages only on the app shell under a synthetic key — the classic page scrolls natively on
Space — so the classic deep-jump case had never paged.

## The engine is a set of transactions, and the reader's scroll moves the position instead of erasing it (2026-09-12, #196 stage 2)

`design/virtual-window-framework.md` (#195) argued that every scroll bug since #98 traced to one
structural fact: the reader's position was not stored, it was captured and restored around each
mutation, and between mutations the browser's `scrollTop` was the position. Stage 1 stored it and
made `place()` the engine's one write, changing no behaviour (the suite identical, the fling case
red by the same 503px/484px). Stage 2 is the behaviour: every change to the model or the DOM is one
transaction — position read before the sums move, mutation, sums, pads, mount, placement, position
re-read where it left the reader — with one reader timer that decides only when a tail placement or
an estimate application runs, and no correction ever owed, deferred or dropped.

**What the design had wrong, measured.** The fling case did not flip green when the deferral came
out. The trace showed why: the design said a reader's scroll marks the position stale and the next
transaction re-reads it; but a growth heard by an observer arrives in the same rendering update as
the scroll event, and a position re-read then describes the already-displaced view. The reader's
scroll cannot erase the position — it moves it, by an amount the engine knows to the pixel without a
DOM read, because between transactions nothing else moves the offset. So the position carries the
offset it was read at, a spontaneous change is placed against it plus the reader's drift since, and
only a transaction the engine is about to make re-reads it (its view is still the reader's). With
that, the fling case held on both pages.

**What the probes caught next, each with the suite green.** Three regressions in a row, none of
which any of the 229 cases sees, all of which the owner's sessions do:

1. *The unfold probe, app shell: 1,850px, records unmounted under the reader, turns going the wrong
   way.* A second mount in the same transaction measured the reader's drift from the offset the
   engine had just written, counted its own write as theirs and placed the reader 4,589px past the
   anchor it had just put back. The drift is computed once, at the transaction's start — nothing
   the reader does can land inside a synchronous transaction.
2. *The walk probe: the owner's #194 cycle back on the shell (772 → 769, twice), and the classic
   jump landing 21 turns early.* The estimate transaction at rest changed the sums, found its range
   unchanged, wrote no pads, and the coverage check read the new sums against the old page —
   mounting a window 400 records above the reader. The pads follow the sums before a mount decides
   it has nothing to do. The same stale pads were the shell's residual 11px per wheel: the
   offset-based range and the one around the position disagree at their edges by construction, and
   re-mounting on that alone mounted two windows per transaction, each measuring the edge unit
   differently. A second mount happens only when the viewport actually shows unmounted territory.
3. *The unfold probe, classic page near a live tail: 1,881px on one wheel.* The sums took a smaller
   estimate above a reader near the end, the page shrank, the browser clamped the offset inside the
   transaction, and the placement read the clamp as the reader's drift — the same fix as 1, stated
   the other way: an offset change after a transaction's start is a clamp or the engine's own write,
   never the reader's.

**Before → after, on the owner's sessions.** The fling case 503px/484px lost → 0. The unfold probe
near the live tail on the classic page, worst wheel 51px → 1px — the same 51px on the stage-0 and
stage-1 engines, and its trace names the mechanism the framework was written against: a live delta
re-measured records above the reader inside the intent window, the correction was deferred as
owed, and the settle dropped it (entries 312–321). The walk and the runaway probes unchanged. The
suite green on both pages; the byte gate re-baselined on the inlined engine alone.

The lesson for the method is the one `validate-on-real-sessions` already states, sharpened: the
suite held through all three regressions because each needs a live estimate shift, a rest timer
and a window edge to line up, which the hermetic fixtures never make happen. The probes are the
acceptance; the suite is the floor.

## The estimator is the engine's, and a page only says what kind a record is (2026-09-12, #196 stage 3)

The third stage of the #196 refactor, and like the first a move with no behaviour change: the
framework's §4.4, landed as §4.9. Until it, the shared `HeightGuess` was constructed by each page
— three per unit type on the app shell, one on the classic page — and each page kept its own share
bookkeeping around it (`this.shares` keyed by unit, `recShares[]` keyed by index), its own
`applyEstimates`, and its own rule for a unit whose type had no guess. Two implementations of
I5 ("a record contributes at most one share to the mean"), which is one more than a rule held by
construction can have.

Now the page passes `floors` (`{ user: 44, assistant: 40, process: 34 }` with `defaultKind:
"process"` on the shell; `{ record: 30 }` on the classic page) and overrides `kindOf(i)`; the engine
builds one mean per kind in its constructor, learns in `measureMounted` — `learn(i, h)` right before
the page's `setHeight`, so the classic page's `replaceMounted`, which reaches `measureMounted`
directly, learns too — keeps one share per IDENTITY, and applies the means only from the estimates
transaction. The page's `setHeight` is persistence and nothing else; `clearHeights` / `scaleHeights`
handle only the heights the page stores (each floored at the page's own value), with the engine's
`resetGuesses` / `scaleGuesses` beside them in the remeasure transaction. A vanished identity is
forgotten through the engine: the shell's `setUnits` calls `forget(key)`, the classic `resetFrom`
calls `forgetFrom(from)` BEFORE it truncates the arrays, while the dropped ids can still be read.
The `HeightGuess` docblock, which still described the pre-#194 estimator ("a record measured
repeatedly … counts more than once"), now describes the class under it.

Measured on the owner's 21k-line session, stage 2's engine against stage 3's, one Chrome at a time
(the walk: 30 wheels of 400px forward from turn 766; the unfold: open a fold, wheel 6×400 down and
up, close it, 4×400 down and up, three folds per variant):

| probe | stage 2 | stage 3 |
|---|---|---|
| walk, classic — turn series / backward jumps | 766…772, none | identical series, none |
| walk, app shell — turn series / backward jumps | 766…777, none | identical series, none |
| walk, both — per-wheel movement of the reference record | 400 ±1px | 400 ±1px (the same steps differ by the same 1px of rect rounding) |
| unfold, classic static and live — worst over-movement | ≤1px | ≤1px, 0 unmounted, 0 wrong-way |
| unfold, app shell static and live — worst over-movement | ≤1px | ≤1px, 0 unmounted, 0 wrong-way |
| runaway, both static variants — hands-off drift / wheels to reach the tail | 0 changes / 0 | 0 changes / 0 |
| runaway, both live variants (growth lands during the run, so the samples are not comparable one to one) | reaches the tail, following restored | reaches the tail (3 and 2 wheels), following restored, the spy on the last turn |

The suite: 231 cases (the two fling cases now in the sorted list the chunks are cut from), run in
twelve chunks at most two Chromes at a time under memory pressure.
