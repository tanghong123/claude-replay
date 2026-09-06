# The virtual window: one set of scroll rules for both shells (#98)

Two pages render a transcript that can run to thousands of records: the classic page
(`claude-replay-html/src/html/export.js`, the html server's page, also the classic rail's iframe)
and the app shell (`claude-monitor/src/codex-ui/viewport.js` + `app.js`). Each keeps a *virtual
window*: only the records near the viewport are in the DOM, the rest are two pads whose heights
come from measured or estimated record heights. The classic page has had a year of scroll bugs
fixed against real use (#50, #89, #103, #171 among them); the app shell reimplemented the window
and inherited some of those rules (#70, #72, #73) and not others. #98 — the owner reading up
through a long turn and landing in the middle of another one, the turns pane losing its focus,
a scrollbar thumb that skips a zone — is the price of the ones it did not inherit.

This note puts the two engines side by side, names the rules, records what #98 borrowed, and
recommends where the code should live. `design/dom-virtualization.md` is the classic page's
own account of its pillars; this one is the comparison.

## What a scroll engine has to answer

Every event below moves the content, the viewport, or both. The engine's job is that the reader
never notices any of them unless they asked for it.

| event | who asked | the reader must see |
|---|---|---|
| the reader scrolls | the reader | exactly the scroll they made |
| a poll appends or rewrites records | nobody | the same record at the same offset (unpinned); the tail (pinned) |
| a record is materialized with an estimated height and then measured | nobody | nothing |
| a mounted record grows or shrinks — an image decodes, a fold opens, a font arrives | nobody, or the reader (a click) | nothing move under the point they are reading |
| a search hit, a turn click, a deep link | the reader | the target, plus the context that makes it readable |
| the reader drags the scrollbar thumb | the reader | the thumb where the pointer holds it |

## The rules, and who has them

**1. Data is truth; the DOM is a cache.** Both pages hold the record stream in memory and
render a window of it. The window is contiguous; heights are learned by measuring and remembered;
prefix sums turn heights into pad sizes and an index-at-offset lookup. Both pages have this
(classic: `records`/`recHeights`/`idxAt`; app shell: `units`/`heights`/`prefix`/`indexAt`).

**2. Disable the browser's own anchoring.** Chrome's scroll anchoring fights a page that
re-anchors itself: the two corrections compound into a walk. Both pages set
`overflow-anchor: none` on the scroller (classic on `html`, app shell on `.transcript`, #70) and
`scroll-behavior: auto` so a programmatic correction lands in the same frame.

**3. The anchor is the first visible RECORD.** Before anything the page does to the DOM, it
records which element holds the viewport top and at what offset; afterwards it puts that element
back at that offset. The classic page anchors on a record element by id (`captureAnchor` /
`restoreAnchor`). The app shell anchored on a *unit* — a whole process, which can hold a hundred
rows. That is the first #98 defect: anything that grows inside the unit above the visible region
(a thumbnail decoding, a fold opening, a late reflow) moves what the reader is looking at while
the unit's own top does not move, so the restore has nothing to correct. #98 descends to the
first visible row (`[data-block-index]`, a record index that also survives a tail rewrite that
re-emits the same positions with new block ids) and restores that row.

**4. Displacement is a HEIGHT signal, not a scroll signal (#89).** A late reflow grows the content
without firing a scroll event. Pinned, both pages observe the body (classic) or the content (app
shell, #70) and heal to the bottom. Unpinned, neither page did anything — and this is the second
#98 defect, shared by both: a growth above the viewport shifts the reader by its height and no
scroll event says so. The fix is the same on both: keep the anchor the reader last settled on
(refreshed after every scroll frame and every apply, cleared the instant a scroll begins so a
resize heard between a scroll and its frame cannot undo the scroll), and let the observer restore
it when unpinned. The app shell's per-unit observer already re-measured on growth but captured its
anchor *after* the growth had moved the view, which describes the moved view and corrects nothing;
it now reads the kept anchor (`readerAnchor`), refreshed once per batch of scroll events by the
deferred window update and after every settle — not per scroll event, which on a 160-row process
would be a rect per row per event.

**5. Estimate under, not over.** The classic page estimates an unmeasured record at 30px (below
almost any real record) so learning heights only ever grows the page below the reader; the app
shell estimates a unit at 132px, which is above many real units, so learning heights shrinks the
page — and a shrink above the viewport is a jump unless rule 3 catches it. Rule 3 catches it now;
the over-estimate is still the wrong side and remains a follow-up (the estimate could be per unit
type: a short prompt, a process with N rows).

**6. The thumb owns the position while it is held.** Dragging the thumb into unvisited territory
mounts units whose real heights differ from the estimates; a page that then re-anchors on its old
first-visible row snaps the thumb away from the pointer — the "zone the slider cannot rest in".
Neither page had this rule; the app shell has it now (`beginDrag`/`endDrag`: a pointer that lands
on the scroller ITSELF is on its scrollbar — content lands on a descendant, and an overlay
scrollbar sits inside the client box, so this is not a coordinate test; while it is held the
window is placed by the scroll offset and nothing corrects it; the anchor rule resumes on
release). The harness proves the code path with a synthetic pointer; a native scrollbar grab is
the owner's machine to confirm. The classic page
should take the same rule when it moves onto the shared engine.

**7. Intent decides following (#103 hysteresis).** Both pages: a scroll within a short window of
user input decides pinned/unpinned (acquiring the pin needs the true end; leaving it needs only
the old slack); a scroll with no input behind it is displacement and, when pinned, is healed.

**8. Reveal before you jump.** A search hit or a turn click inside a fold must open the fold
(classic `revealMark`; app shell `revealNavigationContext`) and then place the target; both pages
do this, with different notions of what to open (#100 is the app-shell gap in what a *next hit*
reveals).

**9. The scroll-spy follows the settled view, never the moving one.** The turns pane's focus is
recomputed after the view has settled (classic `spy()` after `settleAfterApply`; app shell
`afterScroll` → `updateOutlineFocus`). The owner's "the turns pane loses the current turn" was
rule 3 failing — the view moved, the spy followed it faithfully — not a spy bug.

**10. Position is remembered per session, following is the default.** Both pages: the position
the reader left is restored on return; a session never visited opens at the tail (#84).

## What #70/#72/#73 borrowed, and what #98 adds

- #70 brought rule 2 and rule 4's pinned half (the content observer).
- #72 brought rule 7's acquire/hold slack.
- #73 established `view_anchor` as the harness metric and proved the unpinned apply held —
  for a short open turn, where the first visible element is a stable prompt.
- #98 adds rule 3 at row granularity, rule 4's unpinned half on BOTH pages, and rule 6 on the
  app shell; and it adds `view_anchor_index`, the position-based metric (a record index and an
  offset) that survives a rewrite's new block ids, plus three scenarios: reading inside a long
  open turn through six live rewrites, a 300px growth above the reader inside the same turn, and
  the held thumb.

## Recommendation: one engine under `html/shared/`

Yes to the owner's question — the scroll behaviour should be one module both pages drive, and
it should be the classic page's rules, held by the scenarios that run on both surfaces. The
shape:

- **`shared/virtual-window.js`** owns: heights + prefix sums + index-at-offset; the contiguous
  window and its two pads; the anchor (capture before, restore after, at row granularity, kept
  fresh for changes nobody asked for); the height observer with both halves of rule 4; the
  following state with the #103 hysteresis; the thumb-drag mode; the tail pin and converge;
  position memory. It knows nothing about records: the page hands it `count`, `render(index)`
  → element, `identity(index)` → a string stable across rewrites (a record index or id), an
  optional `estimate(index)`, and it calls back `onSettle(firstVisibleIndex)` for the spy.
- **The pages keep** what is theirs: which records form a unit (the app shell's projection),
  what a fold is and how a reveal opens it, the search stepping, the outline column.
**Steps 1, 2 and 3 landed (v1.195.0, v1.196.0, v1.197.0).** `shared/virtual-window.js` holds the arithmetic — `prefixSums`,
`indexAt` (clamped for the app shell, raw for the classic page), `rangeForScroll`, `rangeAround`,
`clampRange`, `padHeights`, `heightChanged`, `correction`, `firstVisible`, `classifyScroll` — and
`viewport.js` runs it. It takes NUMBERS and returns numbers: every layout read and every DOM write
stayed on the page, which is what lets the node contract test the rules differentially against the
bodies they replaced. Two divergences the extraction surfaced are parameters, not forks: the index
clamp, and the follow slacks (the app shell decides on the true end in both directions, filed as
its own task; the classic page holds at 80px). The class the note describes below is not built:
the app shell's scroller is an element and the classic page's is the document, so a frame adapter
has to come first. Step 2 put the CLASSIC page on the same arithmetic — its lazy prefix sums, its
unclamped search, its pads, its anchor (captured and corrected through the shared rules) and its
follow decision — which is what validates the module against the reference. What remains is the
class: the state machine (the window range, the observers, the measure schedule, the tail
converge, position memory) still lives twice, and moving it needs that frame adapter.

**Step 4a landed (v1.198.0).** The state machine is the module's now — `VirtualWindow`, with the
frame adapter the classic page will need: the engine reaches the scroll host only through
`{scrollTop, scrollTo, scrollBy, clientHeight, scrollHeight, viewportTop, on, isScrollbarTarget}`,
and `elementFrame(el)` is the element implementation the app shell passes. `viewport.js` is now
that class's consumer (232 lines from 513): it answers what a unit IS (`count`, `identityAt`,
`estimateAt`, `heightFor`/`setHeight`/`clearHeights`, `renderItem`), where the follow flag lives
(a get/set pair over the page's own state, so `state.following` keeps every reader it had), and
what a reveal opens. The rules half of the module stays pure and node-tested; the engine half
drives the DOM by definition, and the contract guards the split.

What remains for the acceptance: the classic page driving the same class through a DOCUMENT
frame. Its window is built around `matEls()` and its own materialization, so that is a step of
its own — and the reference page is the one that must not move.

Step 3 fixed rule 5's wrong side on the app shell: the estimate is a FLOOR per unit type (a
prompt 44px, an assistant note 40, a process 34) instead of 132px for everything, so learning a
height only grows the page below the reader. Measured on a forty-turn session: reading down and
back used to SHRINK the page by 505px, and now it does not shrink at all
(`scenario_learning_heights_only_grows_the_page`, both surfaces). The search-hit reveal the note
lists beside it stays with each page on purpose — `revealNavigationContext` walks the app shell's
projection (prompt expansions, process folds, cap state) and `revealMark` the classic page's
folds; they are the pages' vocabulary, not the engine's.

- **Migration** in three steps, each releasable: (1) the app shell's `viewport.js` becomes the
  module's first consumer (it is already class-shaped and record-agnostic); (2) the classic page
  adopts it for the window/anchor/observer core, keeping `updateView`'s materialization as the
  `render` callback — the byte-identical gate covers the rendered HTML, the scenarios cover the
  behaviour, and this step is what validates the module against the reference; (3) the
  per-unit-type estimate and any remaining classic-only rule (search-hit reveal) move in.
- **The harness must hold**, on both surfaces, before and after each step: the scenarios in
  `tests/scenarios.rs` (pill, queued text, unpinned open turn, live rewrites, growth above the
  reader, images, and the app shell's held thumb) and the structural cases in
  `tests/browser_follow.rs` (follow, anchor, jump). A case that fails on the classic page is a classic bug and gets its
  own task, per #71 — the classic page is the reference, not an oracle.

The extraction is more than the #98 fix and is filed on its own; #98 ships the rules above in
place, so that the extraction moves code that is already right.

## Step 4b: the two rules that differ, and the decision they need (#128)

Steps 1–4a made the *plumbing* shared: both pages run the same arithmetic, and the app shell
runs the same engine class. What is left — the classic page driving that class through a
document frame — is not a mechanical move, because two rules genuinely differ between the
pages. Both are rules the reference page holds deliberately, and both were written to close a
bug that actually shipped. This section states them so the choice can be made on the facts.

### The model both pages already share

A session is N records. Only a slice `[lo, hi)` is in the DOM; two pad elements stand in for
everything above and below, sized from prefix sums over per-record heights. A record's height is
an **estimate** until it is mounted and measured, then it is remembered. So the index↔pixel
mapping is exact behind the reader and approximate ahead of them. Everything below is about the
two places that approximation bites.

### Rule 1 — where the window's range comes from

**The engine (app shell): pixel-anchored.** `rangeForScroll` (`shared/virtual-window.js:44`)
takes the scroll offset and maps pixels → indices through the prefix sums:
`lo = indexAt(scrollTop − OVERSCAN)`, `hi = indexAt(scrollTop + clientHeight + OVERSCAN) + 1`,
with `OVERSCAN = 1500` (`viewport.js:49`). After a jump, `rangeAround(index)`
(`shared/virtual-window.js:52`) walks heights outward from the target instead. Both are pure
functions of numbers, which is why they are node-tested.

**The classic page: index-anchored (#66, `export.js:401` `updateView`).** It does not ask the
scroll offset where it is; it asks the DOM. It scans materialized elements for the first one
actually on screen (`r.bottom > 0 && r.top < innerHeight`, `export.js:414`), takes **that
element's record index** as the anchor, and walks effective heights outward from it:
`anchorTop + MARGIN_PX` of content above the viewport top, `innerHeight − anchorTop + MARGIN_PX`
below (`export.js:421–433`). It falls back to the pixel mapping only when nothing materialized is
visible. Then it re-reads the anchor's rect and `scrollBy`s the delta (`export.js:435`), so
measurement drift never visibly moves the page.

The difference matters in one specific way. The engine's estimates decide **which indices** the
window contains; the classic page's estimates decide only **how far** the window extends. So a
prefix full of drift can make the engine compute a window that EXCLUDES the block on screen —
the #66 failure, written in that comment: you navigate to record N, the next update recomputes
from `scrollTop`, stale estimates above map that offset to different indices than the real rects
on screen, and the very block just navigated to is unmounted. The index-anchored rule cannot
produce that outcome, because it starts from an element that is provably on screen.

What the classic rule costs: it needs materialized DOM to read (hence the fallback, and a
chicken-and-egg on first paint), it does a DOM scan per update, and it makes the range a
function of DOM state rather than of numbers — so it cannot be unit-tested in isolation the way
`rangeForScroll` is, and a stale anchor is a way to be wrong that the pure rule does not have.

### Rule 2 — what actually gets mounted

**The engine: dense.** One child per index in `[lo, hi)`, reconciled by `dataset.unitIndex` with
a cursor walking in lockstep with the range (`shared/virtual-window.js:329` `reconcile`).
Children are a bijection with the range: index → child is direct, reuse is an identity compare,
and the pads come straight from the sums. Filtering is then a *paint* concern — `applyFilters`
(`app.js:770`) mounts everything in range and adds `.filter-hidden{display:none!important}`.

**The classic page: sparse.** `setWindow` skips `isHiddenRec(i)` entirely (`export.js:344`) — a
filtered-out record is never built — and `effH(i)` returns **0** for it (`export.js:215–216`), so
the prefix sums are a *filtered* coordinate space. DOM children are therefore a subset of
`[lo, hi)`, and every loop has to walk by `data-idx` and check real indices. That is exactly what
#94 fixed: trimming one node per index step deleted visible elements that belonged inside the new
window.

Three consequences follow from the code (derived from reading it, not measured in a browser):

- With a filter on, the classic page's document height is right immediately — it knows a record
  is hidden without mounting it. The app shell only learns a record is hidden after mounting and
  measuring it at zero, so records outside the window still contribute their unfiltered heights
  and the page shortens as the reader scrolls through.
- The app shell builds DOM it immediately hides: cheap when a filter keeps most rows, wasteful
  when it hides almost all of them.
- Serving the classic page on the engine means teaching the hot reconcile loop a skip predicate
  and index-sparse children — the loop where #94 and the #98 class of bug live.

### The two options

**(a) One engine, two strategies.** `rangeAround` / `rangeForScroll` and the mount step become
page-supplied. Nothing on the reference page changes; the engine keeps the pads, sums,
measurement, anchoring, correction, follow state and position memory. The cost is honest: the
engine no longer owns the two rules that matter most, so "both pages drive it" becomes true of
the plumbing but not of the policy, and a future #98-class bug can still be fixed on one page and
not the other.

**(b) The app shell's rules win and the classic page moves.** One rule in one place, real
convergence. The cost is a behaviour change on the REFERENCE page in the two spots with the worst
bug history (#66, #94, #98) — which needs scenarios written first and confirmed to fail, the
byte-identical gate re-verified line by line, and it re-opens the drift class #66 was written to
close.

### Recommendation

**(a), with one addition: make the index-anchored range the engine's DEFAULT strategy and the
pixel-anchored one the fallback**, rather than the other way round. Where the two rules differ,
the classic page's is strictly more robust — it cannot exclude the block on screen — so the app
shell would inherit that protection instead of the two pages keeping different exposure to the
same bug class. Whether that changes the app shell's post-jump behaviour at all is a measurable
question: the existing jump and anchor scenarios would show it, on both surfaces, before and
after.

The sparse-mount rule is the one I would leave as a strategy for real. Dense-and-hide fits a
shell that filters interactively (the filter is a paint pass, not a re-window); sparse fits an
export page whose filter is usually a big cut. That difference is a property of the two products,
not an accident of history.

### Either way, the harness comes first

The scenarios in `tests/scenarios.rs` and the structural cases in `tests/browser_follow.rs` run
on both surfaces before and after every step, and the byte-identical gate is verified line by
line — a rendered-HTML change is not what this work is for. A case that fails on the classic page
is a classic bug and gets its own task (#71), because the classic page is the reference, not an
oracle.

### The owner's model, and where the code already agrees (2026-09-06)

Stated as a model rather than as a patch to the current one: the page is three consecutive
groups — an invisible TOP, a rendered MIDDLE, an invisible BOTTOM, either end possibly empty;
only the middle is concrete; the viewport is **an offset within the rendered middle** (or, in
the pinned case, "at the bottom"); a height change in an invisible group needs no change to
that offset, and only a change inside the middle can move it; a scroll first maps the old
offset into the new middle and then applies the delta; a jump checks whether the target is
already in the middle and, if not, does the full calculation; a thumb drag inside the pads is
an ordinary scroll, and beyond them translates the offset to a record, renders that record
first and resets.

**Most of this is what the code does, and the parts that differ are the interesting ones.**

- *Three groups, only the middle concrete.* Exactly the implementation on both pages: `[lo, hi)`
  mounted between two pads sized from prefix sums (`padHeights`).
- *The pinned case.* That is `following` + `convergeBottom` — the tail wins over any offset.
- *A jump checks the middle first.* `jumpToRecord` always re-ranges around the target and then
  lands it at a fixed offset; skipping the re-range when the target is already mounted would be
  an optimisation, not a behaviour change.
- *A thumb drag has its own mode.* `dragging` exists and the held thumb has a scenario. What the
  model adds is the RESET at the end of a long drag — make the translated record the first
  visible thing and re-zero the offset — instead of scrolling to a computed pixel and then
  correcting.
- *The offset within the middle.* This representation is already in the code — `captureDomAnchor`
  returns `{key, top: elementTop − viewportTop, block, blockTop}`, an identity and a delta. But it
  is a **capture/restore patch around each mutation**, not the stored position: between mutations
  the position IS the browser's `scrollTop`, measured over a document that includes both pads.

That last difference is the whole of Rule 1 above, and the model states the consequence exactly:
a height change inside an invisible group leaves the OFFSET alone and changes the height of the
invisible top, so the absolute position is recomputed — `scrollTop = padTop(lo) + offset` — and
written back. The invariant is on the offset; the absolute number is derived from it, every time.

That is a different operation from what the code does today, not a different spelling of it. The
code keeps `scrollTop` as the stored position and repairs it after the fact: capture an anchor,
mutate, restore it, correct (`correction`, `heightChanged`, the classic page's `scrollBy(d)` after
`setWindow`). A repair is an increment — it measures where the anchor ended up and nudges — so it
needs the DOM already laid out, it can be skipped on a path nobody thought to instrument, and its
error accumulates. The recomputation is absolute: it needs only the prefix sums and the offset, and
it produces the same answer no matter how the window got there. Every #98-class bug we have had
lives in a gap between the repairs; there is no equivalent gap in a recomputation, because there is
nothing to skip — the write-back IS the position.

Two details make it practical here. The pages already own anchoring completely — `overflow-anchor:
none` on the app shell's `.transcript` and on the classic page's `html`/`body` (#50/#66/#89) — so
nothing else writes the scroll offset and there is no second mechanism to fight. And the write-back
must land in the same frame as the mutation, before paint, and be marked as the page's own scroll
(`classifyScroll` already draws that line) so the follow logic does not read it as the reader
moving. Promoting the anchor from patch to stored position is the same direction as the
recommendation above (make the index-anchored range the default), taken to its end.

**What it costs, honestly.** The browser owns the scrollbar. Keeping a native scroller means the
authoritative number is `scrollTop` whether we like it or not, so the model can be the source of
truth only if every window change ends by writing `scrollTop` back from it. Going further — a
transform-based virtual scroller with a synthetic scrollbar — makes the model exact but gives up
native scroll anchoring, trackpad momentum, keyboard paging, find-in-page and the accessibility
tree. That trade is not worth it for a transcript reader; the reconcile-after-every-change version
gets most of the benefit and keeps the platform.

**One idea in the model that is in NEITHER page: the pads under a width change.** Today a resize
makes the app shell throw every measured height away and relearn (`remeasure` → `clearHeights`),
while the classic page keeps heights measured at the old width and simply re-runs `updateView`.
Both are wrong in opposite directions for the invisible groups. Scaling the remembered heights
when the width changes — a text block's height moves roughly with the inverse of its measure — is
a better first guess than either, costs one multiply per remembered record, and is independent of
the two rules above. Worth its own task whichever way #128 goes.
