# DOM virtualization: the memory optimization in the HTML export (#50)

How `src/html/export.js` renders arbitrarily long transcripts with a small, bounded
DOM — written up as a transferable technique (per request), with the pitfalls we hit
and the exact fixes. The concrete symbols named here live in `export.js`; everything
else is framework-free vanilla JS and applies to any long scrolling document.

## The problem

A long session renders to tens of thousands of DOM nodes (code blocks, diffs,
syntax-highlighted spans). Browsers handle big DOMs, but every layout/style pass
touches them, live-tail appends get slower over time, and memory grows without
bound. The classic fix is **windowed rendering** (a.k.a. virtualization): keep only
the elements near the viewport in the DOM, replace everything above and below with
two spacer `<div>`s whose heights sum to the missing content.

Ready-made virtual-list libraries assume fixed-height rows and total ownership of
scrolling. A transcript has neither: block heights vary by 100×, users toggle folds
(heights change under you), and content streams in live. What follows is the shape
that works under those constraints.

## Architecture: data is the source of truth, DOM is a cache

The pillars, each one load-bearing:

1. **`records[]` — the append-only model.** Every renderable block is a JSON record
   held in a plain array. The DOM never holds state the records can't regenerate:
   materializing block *i* is a pure function `render(records[i])`. This is the
   invariant that makes eviction safe — if re-rendering could lose anything, you
   could never throw an element away.

2. **A contiguous window between two pads.** The DOM contains `.vpad-top`,
   elements for records `[lo, hi)`, `.vpad-bot`. The pads' heights are the summed
   (estimated or measured) heights of the evicted prefix/suffix, so the scrollbar
   geometry stays truthful. One contiguous window — never islands — keeps the
   bookkeeping trivial.

3. **Estimated heights, corrected by measurement.** `recHeights[i]` starts as a
   constant guess (`EST_H = 30px`) and is replaced by the real `offsetHeight` when
   block *i* is materialized. Measured values persist across eviction (heights are
   part of the model, not the DOM). Two consequences:
   - the scrollbar is approximately right immediately and exactly right for
     visited regions;
   - estimate→measurement corrections change pad heights *above* the viewport,
     which the browser experiences as content shifting — see pitfall 1.

4. **Prefix sums + binary search for addressing.** `prefix[i]` = sum of
   `recHeights[0..i]`. `idxAt(y)` binary-searches the prefix array to map a scroll
   offset to a record index. Rebuilt lazily (O(N) on a dirty flag), searched in
   O(log N) per scroll event. This is what makes "which blocks belong on screen at
   scrollY?" cheap at any N.

5. **The window follows scroll with margins.** On each scroll/resize (coalesced
   through `requestAnimationFrame`), compute the target window as
   `idxAt(scrollY - MARGIN) .. idxAt(scrollY + viewport + MARGIN)` (margin ~1500px
   each side), then reconcile the DOM: materialize entering indices, evict leaving
   ones, update both pads. The margin buys smooth wheel scrolling and keyboard
   navigation without churn on every tick.

## The hard-won details

### 1. Disable the browser's scroll anchoring

Chrome's native scroll anchoring watches DOM mutations and "helpfully" adjusts
`scrollTop` when content above the viewport changes size. Virtualization does that
constantly (pad-height swings on estimate corrections), so the browser and your
windowing logic fight — symptoms are jitter, runaway scroll, or the viewport
sliding off the element you just navigated to. One line ends the war:

```css
body { overflow-anchor: none; }
```

Do your own anchoring instead (next point).

### 2. Anchor the window on a *materialized element's index*, not on pixels

Deriving the window purely from `scrollY` and the prefix sums is subtly wrong while
estimates are still being corrected: a programmatic jump (search hit, permalink)
into unvisited territory lands on estimated coordinates; materialization then
re-measures, the prefix sums shift, and the next `updateView` computes a window
that *excludes the element you just jumped to* — it flickers in and out.

The fix (`updateView`'s index-anchored mode): after a navigation, record the target
index as the anchor; when reconciling, first find an anchor element that is
actually in the DOM (`dataset.idx`), keep it pinned to its on-screen position, and
grow the window from there by walking real heights. Pixel-derived windowing is the
fallback for plain wheel scrolling where no anchor is active.

### 3. Batch DOM reads and writes into phases

Materializing N blocks naively (`append → measure → append → measure`) forces N
layout passes (layout thrashing). The reconcile loop is phased: append all entering
elements (writes), then measure them all (reads), then set pad heights (writes).
Measurement itself uses an **offsetTop delta** trick: reading consecutive siblings'
`offsetTop` gives every height in the batch from a single layout pass, instead of
one forced layout per `offsetHeight` read.

### 4. Interaction state must be keyed by record identity, not held in the DOM

Anything the user does to a block — expanding a fold, being the current search hit,
an "N more lines" expansion — dies with the element on eviction unless it lives in
the model. The pattern, used four times over (`userFolds` #61, `filterCurId` #49,
`curHit` #66, `smallMore` #67):

- keep a small map `{recordId → state}` beside `records[]`;
- the click handler *records* intent in the map, then applies it to the DOM;
- a `postMat` hook re-applies the map whenever a block (re)materializes.

Corollary discovered in #67: if state addresses a *sub-element* (the k-th expand
button inside a block), compute the ordinal at **materialization time** and stamp
it on the element (`dataset.ord`). Deriving it at click time from current siblings
is unstable — earlier expansions remove earlier buttons and shift the indices.

Deliberate exception, also #67: unbounded state is *allowed* to be ephemeral.
Recording a "⋯ 2,000 more lines" expansion would defeat the memory bound the
virtualizer exists for, so only expansions whose hidden content is ≤
`MAX_BUFFER_LINES` (200) are recorded; giant ones reset on eviction, by design.
When you cap something, cap it by *content size*, not by visual distance — a
30-line file that happens to exceed the 10-line display cap is cheap to remember;
pixels are the wrong unit.

### 5. Live appends compose trivially — because the model is append-only

Tailing just pushes records and extends the bottom pad; the window logic is
untouched. This is a dividend of pillar 1: streaming, re-rendering a rewritten
tail (the 4-tuple cursor protocol), and virtualization all meet at "records
changed; reconcile" with no special cases against each other.

## Testing gotcha (browser automation)

Programmatic `scrollTo`/`scrollBy` issued from a Chrome extension's isolated world
does **not** fire scroll events in the page world — the window silently never
updates, which looks exactly like a virtualizer bug. Real wheel/keyboard scrolls
(or synthesized input events, e.g. a driver's `computer` scroll) work. When a
"frozen window" reproduces only under automation, check how the scroll is being
injected before debugging the virtualizer.

## When to reach for this shape

Rule of thumb: virtualize when the document is unbounded (live logs, transcripts,
feeds) or when profiling shows layout/memory pain past ~10k nodes. Below that, the
complexity isn't free — the pitfalls above are real bugs we had to find. But if
you do need it, the five pillars + id-keyed interaction state give you eviction
that users cannot observe, which is the entire game.
