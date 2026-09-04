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
