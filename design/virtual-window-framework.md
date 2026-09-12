# The virtual window as a framework: the model, its invariants, and what holds them (#195)

*Written 2026-09-12 for the owner, after #194 and #199. The engine's arithmetic was extracted and
shared in #107; the class and the classic page's port followed (`design/virtual-window.md`,
`design/one-engine-two-pages.md`). The directive it answers: "look at the
virtual dom design, and scrutinize simply on the design and make sure it is air-tight (make it a
framework with clean abstractions that the rest of the viewer logic would build on); then look at
the implementation, and make sure the implementation is clean and invariances are strictly
enforced. … This should not be a patch work or guess work." This document is the design; #196 is
the implementation; #197 the diagnostics. Every claim about today's engine was read in
`claude-replay-html/src/html/shared/virtual-window.js` (1,153 lines at c2cbd9c), its two consumers
(`claude-monitor/src/codex-ui/viewport.js`, `claude-replay-html/src/html/export.js` from line 1829)
and the sixteen viewport bugs on record.*

---

## 0. The one sentence

**The reader's position is a value in the model, and `scrollTop` is derived from it — never the
other way round.** Every bug on the list below is a place where the engine treated the browser's
number as the truth and repaired it after the fact, and the repair was skipped, deferred, dropped,
or computed from sums that had just moved. The framework removes the repairs by removing the thing
they repair: there is one position, one function that writes the scroll offset from it, and every
change to the sums is a transaction that ends by calling that function.

---

## 1. The model

The engine knows numbers and elements; it knows nothing about records. A page tells it what a
record is through the seams in §3. Everything below is engine state.

### 1.1 Records

- **`count`** — how many there are. Read from the page (a getter), never cached.
- **Index** `i ∈ [0, count)` — position in the stream. Indices are NOT stable: a live rewrite
  re-emits records at the same indices with different content (#165, `changed_from`).
- **Identity** `identityAt(i)` — a string stable across a rewrite that re-emits the same positions
  (the classic page's `b{n}` id; the app shell's unit key). Identity is what an anchor holds and
  what reconcile reuses a mounted element by. Identity is not stable across a rewrite that DROPS a
  record (a queued prompt picked up shifts every marker behind it): the same id then names a
  different record, and the model must survive that (§2, I12).
- **Skip** `skipAt(i)` — a record the page hides (the classic page under a filter). A skipped
  record has height 0 in the sums and is never mounted. Asked before the height, because a falsy
  height falls through to the estimate.

### 1.2 Heights

A record's height is one of two things, and the model always knows which:

- **measured** — `heightFor(i)` returned a number: the border box plus margins of the element
  that rendered it (`itemHeight`, rule 8: the ONE measure, #140), remembered by the page across
  eviction. A width change scales it (#132 step 4); a session change clears it.
- **estimated** — `estimateAt(i)`: the APPLIED estimate for that record's kind (§1.3). Never the
  live one.

`heightOf(i) = skipAt(i) ? 0 : heightFor(i) || estimateAt(i)`. That is the whole of it; the sums
read nothing else.

### 1.3 The estimator

One `HeightGuess` per kind (the app shell: prompt, note, process; the classic page: one). Each is a
running mean over DISTINCT records: `learn(height, previousShare)` returns the record's share, and
a record measured again replaces its share rather than adding one; `forget(share)` withdraws a
record that is gone (#194). A sample under the floor teaches nothing; above `minSamples` a sample
is clamped to `outlier × mean` rather than dropped (#184).

The estimator has TWO values, and the distinction is the #194 rule: **`value()` is the live mean,
`estimate()` is the APPLIED one, and the sums read only the applied one.** `apply()` moves the
applied value to the live one and reports whether it moved. Why two: a never-measured record's
height IS the estimate, so a change to the estimate is a change to every never-measured record at
once — thousands of records above a reader who opened at the tail, thousands of pixels of page
above them (measured: a fraction of a pixel across 8,500 records was 7.8k px).

Today the pages own the share bookkeeping (`recShares[]`, `this.shares`) and the engine asks
`applyEstimates()`. (The `HeightGuess` docblock, engine lines 113–118, still describes the pre-#194
estimator — "a record measured repeatedly … counts more than once" — and contradicts the class
below it; a comment-only fix for #196, with the byte-gate re-baseline an inlined comment costs.) §4 moves the estimator INTO the engine; the page supplies `kindOf(i)` and a
floor per kind, and the engine owns learn/forget/apply. One implementation, not two.

### 1.4 The sums

`prefix[i] = Σ_{j<i} heightOf(j)`, `prefix[count]` the total. **A pure function of the heights,
the applied estimates and the skips** — rebuilt from them, never patched. The classic page keeps
them lazy (marked dirty, paid at the next read: a live apply pushes records one at a time); the
app shell rebuilds eagerly. Both are the same function.

`indexAt(y)` is the binary search over them (clamped on the app shell, unclamped on the classic
page where "past the end" is a real answer under a filter). `documentTopOf(i) = contentTop() +
prefix[i]` is where the MODEL says record `i` begins in the scroller's coordinate; it is exact
only when every height before `i` is measured.

### 1.5 The pads and the window

Only `[lo, hi)` is in the DOM, contiguous, one element per unskipped index, each stamped with its
index and identity. Two pads stand in for the rest: `top = prefix[lo]`, `bottom = prefix[count] −
prefix[hi]`. **The pads are the page's height** (#179): they are written BEFORE anything forces
layout in a reconcile, or the page is shorter than it was for the length of one task and the
browser clamps `scrollTop` to it — the bounce between two positions that was #179.

`contentTop()` is where the sums' coordinate begins in the scroller's: the top pad's rect against
the viewport plus `scrollTop`. Scroll-invariant by construction, and the engine's own pad writes
never move it — which is what makes it safe for `displaced()` to watch (#140 step 4).

### 1.6 The reader

Three things are the reader's, and only the reader's:

- **Intent** — has the reader touched anything in the last `userIntentMs`? Two clocks: handler
  time answers "how long since", the event's own stamp is what a scroll event is classified against
  (#156: a 908ms handler lag turned the reader's own scroll into displacement).
- **Drag** — the scrollbar thumb is held (#98): the pointer owns the position outright; no anchor,
  no write, until release.
- **Follow** — pinned to the tail (#103 hysteresis: acquiring needs the true end, keeping it only
  the hold slack; a scroll with no input behind it that leaves the tail is healed, not honoured).
  Follow is DROPPED only by the reader: their scroll past the hold slack, or their reshape of the
  page (#185: growth they asked for is not the tail moving away).

### 1.7 The position

**`P`** — where the reader is. One value, three sources, one meaning:

| source | value | when |
|---|---|---|
| **anchor** | `{key, top, block, blockTop}` — a mounted record's identity, its offset from the viewport top, and the first visible ROW inside it when the record straddles the top edge (#98, #177, #178) | the ordinary case: something mounted is on screen |
| **model** | `{index, offset}` — the record whose span the scroll offset falls in, and how far into it, signed (#191) | nothing mounted is on screen — the reader is in a pad after a fling or a released thumb |
| **tail** | — | following: the position is the end, whatever the end is |

The anchor form is exact (it is read from a rect); the model form is exact only through measured
heights. `P` is captured from the DOM (`captureDomAnchor`, or `modelAnchor` when that returns
null) and written back through `place()` (§2, I2). **Today `P` is not stored: it is captured and
restored around each mutation, and between mutations the browser's `scrollTop` is the position.**
That is the structural fact every bug below traces to. §4 makes `P` the stored state — authoritative
between transactions, and re-read from the DOM at the start of any transaction that follows a
reader's scroll (a scroll marks it stale; nothing else does). Reading it per scroll EVENT would be a
rect per mounted child per event, the cost #98 avoided by reading once per scroll batch; leaving it
un-read until a transaction needs it is what keeps it both cheap and never stale when it is used.

### 1.8 The tail

- `count` grows; the open turn's last records are REWRITTEN on every delta (`changed_from`, the
  four tail records re-rendered once a second on the app shell); a queued-prompt pickup rewrites
  the tail and SHIFTS identities (#165).
- Following means `P = tail`; the write is `scrollTop = scrollHeight`, made again after every
  height under it settles (today: up to seven `setTimeout(0)` passes; §4: observer-driven).
- Growth below a non-following reader changes `prefix[count]` and the bottom pad only; nothing
  above `P` moves (I9). Growth INSIDE the record under the reader is the anchor's ROW that holds
  (#98). A rewrite that keeps `P`'s identity keeps `P`; one that drops it falls back to the model
  form captured before the rewrite (I12).

---

## 2. The invariants

Numbered, each with the bug that motivated it, how today's engine holds it, and how the framework
holds it. "Construction" means the code cannot express a violation; "policy" means a timer or a
guard decides; "tests" means only the browser suite would notice.

| # | invariant | motivated by | today | framework |
|---|---|---|---|---|
| **I1** | **Only the reader sets `P`.** A scroll attributed to them invalidates it (it is re-read from the DOM before the next transaction); a drag end, a jump they asked for, a follow acquired or released by their scroll assign it. Nothing else — not a measure, not an estimate, not a mount, not a rewrite. | #98, #132, #138, #194 | **policy** — `syncAnchor()` re-reads `P` after every path; `scheduleSettle` DROPS an owed correction and re-reads; a model anchor was written from shifted sums (#194) | **construction** — `P` is a field; the only assignments are in the reader's handlers (§4.1) |
| **I2** | **Every `scrollTop` write is `place()`, derived from `P`.** One call site in the engine, traced with the `P` it wrote from; a page that wants to move the reader asks the engine (`jumpTo`, `pageBy`, `reveal`), which sets `P` first. | #180 ("not writing displaces them by the correction withheld"), #191, #194 | **not held** — three engine sites (`restoreDomAnchor`, `restoreModelAnchor`, `convergeBottom`), and the pages write the same scroller behind the engine's back: the app shell in four places (the jump's landing loop, `viewport.js:279`; a restore, a reveal and the page keys in `app.js`, plus `scrollIntoView` on a head), the classic page in about twelve (`window.scrollTo`/`scrollBy` on its jump, landing, restore, search, spy-click and drag-select paths). A page write carries no input stamp, so `onScroll` reads it as displacement — and heals it if following. | **construction** — `frame.scrollTo` is private to `place()`; the pages' writes become engine calls that set `P` |
| **I3** | **The sums are a pure function of (heights, applied estimates, skips).** Rebuilt, never patched. | #94, #165 | **construction** (`prefixSums`) | same |
| **I4** | **The applied estimate changes only inside a transaction that holds `P`.** The sums never read the live mean. | #194 | **policy** — `settleEstimates` applies at rest or arms a timer; the timer captures an anchor, applies, restores; with no anchor and not following it restores nothing (known gap) | **construction** — `apply()` is reachable only from `transact()` (§4.3) |
| **I5** | **A record contributes at most one share to the mean.** Measured again → the share is replaced; gone → withdrawn. | #194 | **construction** (`learn(height, previous)` / `forget`) — in two page implementations | construction, in the engine (§4.4) |
| **I6** | **The pads are written before any layout read in a reconcile.** The page's height never shrinks between a DOM mutation and its pad write. | #179 | **construction** (the order in `reconcile`; a comment holds it) | construction, and the contract pins the order |
| **I7** | **A change that has already moved the DOM is placed synchronously, before paint — when `P` is an anchor.** A grown row, a mounted run, a re-rendered record above the reader: the write that puts `P` back lands in the same task, gesture or no gesture — not writing IS the displacement. When `P` is the tail the write is a convergence, not a correction: the growth is below the reader, and under a gesture it waits (#165 — a tail rewrite once a second slammed a reader crawling up inside the hold slack back to the end, seven times). | #132 (before paint), #180 (scroll path) | **policy** — immediate on the scroll path and inside the observer; DEFERRED as `owed` elsewhere and dropped by the next user scroll (#138) | **construction** — a transaction that touched the DOM cannot end without `place()` |
| **I8** | **A change that has NOT touched the DOM may wait for rest, as a whole.** An estimate application, a provisional height. Deferred as a transaction, never as a write: the sums do not move until the write can land with them. | #194 (the wheel dropped the correction, the sums had already moved) | **policy** — the estimates timer | policy, one timer (§4.2) — the browser gives no end-of-gesture event for a wheel or a trackpad; time is the only signal |
| **I9** | **Growth below `P` never moves a non-following reader.** | #98, #103 | **tests** (`scenario_unpinned_holds_to_the_pixel`, the growth scenarios) — true because I1/I3 hold on those paths | construction from I1 + I3 + I7: `P` is above the growth and the write is derived from `P` |
| **I10** | **A jump lands on content.** `P := (target, landing)`; the window is chosen around the target; `place()` puts it there. | #66, #191 | **construction** (`rangeAround`, the landing loop) — but a fling INTO a pad had no anchor at all until #191's model form | construction — the model form of `P` is the fallback source, always |
| **I11** | **The record under `P` is mounted after every transaction.** The window is chosen around `P`, never around the raw offset, whenever `P` has an anchor source. | #194 (the walk unmounted the record under the reader), #66 | **policy** — `updateWindow` ranges around the anchor; a delta's reconcile mounted above mid-gesture until the lo-hold patch | construction — `range = rangeAround(P)` in `transact()`; the lo-hold patch goes |
| **I12** | **A rewrite that drops `P`'s identity falls back to the model form captured before the rewrite.** The same id naming a different record is detected by index, not trusted. | #165 | **partly** — `indexOfIdentity` scans; a dropped key logs `restore:unmounted` and stays put | construction — `transact()` captures both forms before a rewrite; the anchor is validated by index+key |
| **I13** | **Follow is dropped only by the reader.** | #103, #165, #185 | **construction** (`classifyScroll` on the reader's scroll; `readerReshaped`) | same |
| **I14** | **Under a drag nothing is written; at release `P` is re-read from where the thumb left it.** | #98, #132 step 3 | construction (`beginDrag`/`endDrag`) | same |

**What today's engine holds by construction:** I3, I5, I6, I10 (mostly), I13, I14.
**By policy (timers/guards):** I1, I4, I7, I8, I11.
**Only by tests:** I9, I12 (half).
**Not held:** I2 — and this is the count to keep in view: 3 write sites in the engine, ~16 more in the two pages, every one of them a place where `scrollTop` moves without `P` knowing.

**The case that holds each, today** (all in `claude-replay-browser-tests/tests/scenarios.rs`, run on
both surfaces unless named otherwise):

- I1 — `scenario_unpinned_holds_to_the_pixel`, `scenario_scrolling_back_during_growth_holds_between_scrolls`,
  `scenario_a_reader_above_a_growing_tail_moves_only_by_their_wheel` (#194).
- I2 — nothing holds it today: `scenario_the_trace_records_what_the_engine_did` records each write
  but asserts no single path; the node contract will pin the one call site once it exists.
- I3, I5 — the node contract (`prefixSums`; the #194 `HeightGuess` block).
- I4, I8, I11 — `scenario_a_reader_above_a_growing_tail_moves_only_by_their_wheel` (red before #194),
  `scenario_a_gradual_walk_forward_keeps_its_place` (forward only), `scenario_the_page_stops_growing_once_it_knows_what_a_record_costs`.
- I6 — `scenario_the_tail_is_a_wall` (#179), `scenario_a_clicked_control_at_the_tail_does_not_strand_the_reader`.
- I7 — `scenario_growth_above_is_corrected_before_paint`, `scenario_a_growth_in_a_nested_record_holds_the_reader`
  (#176), `scenario_a_growth_in_a_visible_records_head_holds_it` (#178), `scenario_a_growth_around_the_run_displaces_the_reader`
  (#140 step 4), `scenario_a_landing_holds_through_a_growth_above_it`.
- I9 — `scenario_holds_when_unpinned`, `scenario_unpinned_inside_an_open_turn_holds_to_the_pixel`,
  `scenario_growth_above_the_reader_in_the_same_turn_holds`, `scenario_learning_heights_only_grows_the_page` (#184).
- I10 — `scenario_a_long_jump_lands_on_content` (#191), `scenario_deep_jump_then_page_and_step`, `scenario_step_and_page`.
- I12 — `scenario_reading_inside_a_long_open_turn_holds_through_rewrites`, `scenario_queued_prompt_shows_its_text` (#165).
- I13 — `scenario_follows_the_tail_when_pinned`, `scenario_a_nudge_keeps_the_tail` (#127), `scenario_a_fold_opened_at_the_tail_does_not_snap_back`
  (#185), `scenario_folding_near_the_tail_keeps_the_scroll` (#190), `scenario_resize_while_pinned`.
- I14 — `scenario_a_press_in_the_gutter_is_not_a_thumb` (#140 step 4), and the held-thumb structural case in `tests/browser_follow.rs` (#98).

The three policies are three timers — `settleTimer`, `estimatesTimer`, `bottomTimer` — plus the
`owed` debt that connects them. Each was right for the bug it closed and each has a documented
interaction with the others (#138 drops what #132 owes; #194 had to route around both). §4 replaces
them with one state and one timer.

---

## 3. The seams

What a page implements (the contract), and what the engine owns outright.

### 3.1 The page implements

| seam | contract |
|---|---|
| `count` | getter; the stream's length now |
| `identityAt(i)` | a string stable across a rewrite that re-emits the same positions; never empty (the classic page prefixes `@i` as a defensive fallback) |
| `kindOf(i)` *(new, §4.4)* | the estimator kind for record `i` (the app shell: `user`/`assistant`/`process`; the classic page: one kind); with a floor per kind passed at construction |
| `heightFor(i)` / `setHeight(i, h)` / `clearHeights()` / `scaleHeights(ratio)` | where measured heights live (the page owns persistence — the app shell keeps them per session, the classic page in an array); `scaleHeights` keeps rule 5's floor |
| `skipAt(i)` | default none; a skipped record has no height and is never mounted |
| `renderAll()` | default never; mount everything (the classic page's small filtered set) |
| `renderItem(i)` | index → a detached element stamped with its identity (`data-unit-key`) and, for the anchor's row form, `[data-block-index]` rows |
| `afterMount(fresh)` | the elements just mounted, attached, before measurement (the classic page clamps long turns here; a no-op elsewhere) |
| `afterRender()` / `afterScroll()` / `followChanged()` / `remember()` | the page's hooks; `afterScroll` is where a spy runs |
| `following` | a get/set pair over the page's own flag |
| `frame` | `{scrollTop, scrollTo, clientHeight, scrollHeight, viewportTop, on, isScrollbarTarget}` — the element scroller or the document |
| `mount` | `{top, window, bottom, content}` — the pads, the run, and what to watch for chrome growth |
| parameters | `overscan`, `slacks {acquire, hold, heal}`, `userIntentMs`, `rememberMs`, `clampIndex`, `landing` |

Removed from the contract by §4: `estimateAt`, `liveEstimateAt`, `applyEstimates` (the engine
owns the estimator), and the pages' share bookkeeping.

### 3.2 The engine owns

The window and its pads; the sums; the estimator; `P` and `place()`; the reader state (intent,
drag, follow) and its one timer; the observers (per item, the run, the chrome) and the measure
they drive; reconcile (mount/unmount/reuse by identity and index); the jump; the tail converge;
the trace. The page never touches `scrollTop`, the pads, or the sums.

### 3.3 What the page calls

| call | today | framework |
|---|---|---|
| records changed | `applyWindow(dirty)` (classic), `setUnits(units, changedUnit)` (shell) — two implementations of the same thing | `recordsChanged(changedFrom)` — one transaction: capture `P` (both forms), forget the shares of dropped identities, keep provisional heights for rewritten ones, rebuild, range around `P`, reconcile, place |
| jump | `jumpToRecord(index, reveal)` (shell), `goTo`/`landOn` (classic) | `jumpTo(index, landing)` — `P := (index, landing)`; transaction |
| to the tail | `toBottom()` / `convergeBottom(commanded)` | `follow()` — `P := tail`; transaction |
| the reader reshaped the page | `readerReshaped()` | same (drops follow, clears intent — a click is not a scroll, #190) |
| the layout changed | `remeasure()` | same, as a transaction |
| re-render in place | `render()`, `replaceMounted(i)` | `rerender(from)` — a transaction with `dirtyFrom` |

---

## 4. What changes (the #196 refactors)

Each item names the code it replaces and the invariant it converts from policy to construction.

### 4.1 `P` becomes stored state, and `place()` the only write

- A `Position` value `{source: "anchor"|"model"|"tail", key, top, block, blockTop, index, offset,
  stale}` held in `this.position`. Assigned in exactly four places: the start of a transaction when
  `stale` is set (`P := captureDomAnchor() ?? modelAnchor()` — the only DOM read of it), `endDrag`
  (the same, at once), `jumpTo` (the target and the landing), and the follow transitions (`P := tail`
  on follow, `P := capture` on unfollow). `onScroll` sets `stale` and nothing else: a reader's scroll
  does not compute anything, it says the last reading is no longer theirs.
- `place()`: computes `want` from `P` — the anchor's rect when its record is mounted (exact), the
  model sums when not (`documentTopOf(index) + offset`), `scrollHeight` for the tail — and writes
  it through the one `frame.scrollTo` call, traced as `place` with `{source, key/index, want,
  delta}`. Replaces `restoreDomAnchor`, `restoreModelAnchor`, the jump landing loop's write and
  `convergeBottom`'s write. **I2 by construction.**
- The pages' own writes become engine calls: `jumpTo(index, landing)` (the shell's landing loop
  and the classic page's `goTo`/`landOn`), `pageBy(fraction)` (the page keys), `reveal(element)`
  (a search hit, a head brought into view — today `scrollIntoView` on the shell and a `scrollBy`
  on the classic page) and `restore(memory)`. Each sets `P` and places; a page never touches
  `scrollTop` again, and the contract greps for it.
- `indexOfIdentity` is an O(n) scan per call today; `recordsChanged` rebuilds a key → index map
  once per delta and every anchor lookup reads it.
- `readerReshaped` no longer has to clear the input stamp (#190): a DOM change is placed whatever the
  reader's state, so a click's intent window cannot withhold the correction. It still drops follow
  (#185).
- `owed`, `scheduleSettle`, `settleTimer`: deleted. There is no debt because nothing is dropped:
  a deferred transaction re-projects the CURRENT `P`, which the reader's own scrolls keep fresh.
  This is the #138 fix stated positively — the position that was "replayed late" was a stale
  capture; `P` is never stale because I1 gives the reader's scroll the write to it.

### 4.2 One reader state, one timer

`this.reader = { state: "resting"|"moving"|"dragging", lastInput, lastInputStamp, timer }`.
`moving` is entered by any input event (the `noteIntent` list) and by `beginDrag` → `dragging`;
`resting` is entered by ONE timer, `userIntentMs` after the last input (re-armed by each input,
not by polling), and by `endDrag`. On the transition to `resting`: run the pending transactions
(§4.3) and re-read `P` from the DOM (today's `settle`, without the drop). `readerOwnsPosition()`
becomes `reader.state !== "resting"`. The three timers become this one; `convergeBottom`'s
deferral (#165) and `scheduleEstimates` are both "wait for resting".

**Why a timer remains (I8):** wheel and trackpad gestures end without an event. A fling is a
sequence of scroll events with no input behind them; the only way to know the reader has stopped
is that no input has arrived for a while. That is one policy, stated once, and it decides only
WHEN a model-only transaction runs — never whether a position is written.

### 4.3 Transactions

```
transact(cause, mutate, { deferrable })
  if deferrable && reader.state !== "resting": queue it; return      // I8
  if position.stale: position = captureDomAnchor() ?? modelAnchor()   // I1: re-read BEFORE the sums move
  P0 = position
  mutate()                     // heights, estimates, skips, records, DOM
  rebuildPrefix()              // I3
  range = P0.source === "tail" ? rangeAround(count-1) : rangeAround(P0)   // I11
  reconcile(range)             // pads first (I6), then mount, then measure
  if reader.state === "dragging": trace; return                       // I14: the thumb owns the offset
  if P0.source === "tail" && reader.state !== "resting": queue place(); return   // #165: a convergence waits
  place()                      // I2, I7
  trace(cause, …)
```

Every path that changes the sums goes through it: the observers' measure (not deferrable — the
DOM already moved; the write is deferrable only when `P` is the tail, above), reconcile (not deferrable when it mounts on the reader's own scroll path;
deferrable when a delta wants to mount above a moving reader — the lo-hold patch goes, the
transaction simply waits), estimate application (deferrable), `recordsChanged` (not deferrable —
the tail must render — but its estimate application inside is), `remeasure` (not deferrable),
`jumpTo` and `follow` (never deferred: they are the reader's).

**I1, I4, I7, I11 by construction.** The "no anchor and not following" gap in today's
`scheduleEstimates` cannot exist: `P` always has a source.

**Why a write under a gesture is safe to rely on.** The engine has written `scrollTop` under the
wheel on every scroll batch since #180 (`updateWindow(null, true)`), and trackpad momentum is
delivered as wheel events, so the walk and the growing-tail scenarios (#194) are already evidence
that a placement during a fling neither stutters nor dies. What #132 step 3's deferral protected was
a STALE position — a capture from before the reader moved — not the act of writing; with `P`
re-read at the start of every transaction there is no stale position to replay. #196's step 0 is
the one case that pins this before anything else moves: growth above the reader during a momentum
fling, both surfaces, holding to the pixel.

### 4.4 The estimator moves into the engine

`this.guesses = new Map(kind → HeightGuess(floor))`, `this.shares = new Map(key → {kind, share})`.
`setHeight` in the engine learns/forgets; the page's `setHeight` only persists. `recordsChanged`
forgets the shares of identities that vanished. **I5 in one place.** `apply()` is called only from
the estimate transaction. The classic page's `recShares[]` and the app shell's `this.shares` go.

### 4.5 The tail converge is observer-driven

`follow()` sets `P := tail` and places. Every later transaction, while following, ends with
`place()` to the tail — including the ones the per-item and run observers raise when heights
settle after the mount. The seven `setTimeout(0)` passes go; `bottomTimer` goes. Under a gesture
while following, tail transactions are deferrable (the growth is below the reader; #165) and the
reader's scroll past the hold slack unfollows first (I13).

### 4.6 The trace becomes the transaction log

Each entry is one transaction: `{cause, P0, deferred, mutated: [index, from, to, kind], lo, hi,
pads, want, delta}` — the shape #197's action-and-state history records and replays.

### 4.7 Out of scope

- Sparse vs dense mount under a filter (settled per page: `skipAt`/`renderAll`).
- The height measure (settled by #140 step 4: the classic page's CSS makes the two bases agree).
- The spy, the pane, the search reveal, the fold vocabulary — page-level, held by their own cases.
- A synthetic scrollbar. The browser owns `scrollTop`; the framework keeps the native scroller
  and writes it from `P` after every change, which is the whole of the trade discussed in
  `design/virtual-window.md` ("what it costs, honestly").

---

## 5. Held by what

- **The node contract** pins the shapes: one `frame.scrollTo` call site; `apply()` reachable only
  from `transact`; the four assignments to `position`; the reconcile order.
- **The scenarios on both surfaces** hold I9–I14 as behaviour: the pixel-hold cases, the growth
  cases, the walk, the growing tail, the deep jump, the held thumb, the end rule.
- **The real-session probes** (#194's `tmp_walk`, `tmp_runaway`, `tmp_unfold`, kept for #197)
  are the acceptance: before/after numbers on the owner's sessions, both pages.

The decision rule throughout: an invariant held by construction beats one held by a timer; the
one timer that stays (§4.2) decides only when a model-only change runs, and nothing about where
the reader is.
