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

Since #196 stage 3 (2026-09-12) the engine owns it: the page supplies `kindOf(i)` and a floor per
kind (`floors`, with `defaultKind` for a kind it has no floor for), and the engine keeps one
`HeightGuess` per kind and one share per identity — `learn(i, h)` around the page's `setHeight`,
`forget(key)` / `forgetFrom(i)` for identities that vanish, `applyEstimates()` from the estimates
transaction only, `scaleGuesses(ratio)` / `resetGuesses()` beside the page's own `scaleHeights` /
`clearHeights`. One implementation, not two; the pages' `recShares[]` and `this.shares` are gone,
and the `HeightGuess` docblock now describes the class below it (§4.9).

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
| **offset** | `{y}` — a raw scroll offset the reader asked for | a page key (`pageBy`), a restore by offset when the remembered record is gone, the drag-select band's nudge; it lives only until the next transaction re-reads an anchor |

The anchor form is exact (it is read from a rect); the model form is exact only through measured
heights. `P` is captured from the DOM (`captureDomAnchor`, or `modelAnchor` when that returns
null) and written back through `place()` (§2, I2). **Before #196 `P` was not stored: it was captured
and restored around each mutation, and between mutations the browser's `scrollTop` was the
position.** That was the structural fact every bug below traced to. Since stage 2 (§4.8) `P` is the
stored state, and it carries the offset it was read at (`at`): between transactions only the reader
moves the offset, so `P` plus the drift since it was read IS where they are, exactly, without a DOM
read — which is what a change that has already moved the DOM needs. A transaction the engine is
about to make re-reads `P` from the DOM when the offset moved since (the view it reads is still the
reader's own); one that reports a change that already moved the DOM never does (the view it would
read has already moved). Reading it per scroll EVENT would be a rect per mounted child per event,
the cost #98 avoided by reading once per scroll batch; leaving it un-read until a transaction needs
it is what keeps it both cheap and never stale when it is used.

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

Numbered, each with the bug that motivated it, how the engine held it before #196 ("today" in the
table), and how the framework holds it. "Construction" means the code cannot express a violation;
"policy" means a timer or a guard decides; "tests" means only the browser suite would notice. Stage
2 (§4.8, 2026-09-12) moved I1, I4, I7, I8, I11 and I12 to construction and I2's engine half (one
write site); the pages' own writes are stage 4's.

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
- I14 — `scenario_a_press_in_the_gutter_is_not_a_thumb` (#140 step 4), and `app_shell_lets_the_thumb_own_the_position_while_dragged` (app shell only — the case that proved the code path with a synthetic pointer, #98).

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
| `kindOf(i)` (§4.4, stage 3) | the estimator kind for record `i` (the app shell: `user`/`assistant`/`process`; the classic page: one kind, `record`); with a floor per kind passed at construction (`floors`, `defaultKind`) |
| `heightFor(i)` / `setHeight(i, h)` / `clearHeights()` / `scaleHeights(ratio)` | where measured heights live — persistence only (the app shell keeps them per session, the classic page in an array): the engine learns from every measure before it hands the height over; `scaleHeights` keeps rule 5's floor |
| `skipAt(i)` | default none; a skipped record has no height and is never mounted |
| `renderAll()` | default never; mount everything (the classic page's small filtered set) |
| `renderItem(i)` | index → a detached element stamped with its identity (`data-unit-key`) and, for the anchor's row form, `[data-block-index]` rows |
| `afterMount(fresh)` | the elements just mounted, attached, before measurement (the classic page clamps long turns here; a no-op elsewhere) |
| `afterRender()` / `afterScroll()` / `followChanged()` / `remember()` | the page's hooks; `afterScroll` is where a spy runs |
| `following` | a get/set pair over the page's own flag |
| `frame` | `{scrollTop, scrollTo, clientHeight, scrollHeight, viewportTop, on, isScrollbarTarget}` — the element scroller or the document |
| `mount` | `{top, window, bottom, content}` — the pads, the run, and what to watch for chrome growth |
| parameters | `overscan`, `slacks {acquire, hold, heal}`, `userIntentMs`, `rememberMs`, `clampIndex`, `landing` |

Removed from the contract by stage 3: `estimateAt`, `liveEstimateAt`, `applyEstimates` (the
engine owns the estimator), and the pages' share bookkeeping.

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

- A `Position` value `{source: "anchor"|"model"|"tail", key, index, top, block, blockTop, offset,
  at, fallback}` held in `this.position` — `at` the offset it was read at, `fallback` the model form
  read alongside an anchor from the same sums (I12). Assigned at the START of a transaction the
  engine is about to make when the offset moved since it was read (`P := captureDomAnchor() ??
  modelAnchor()`), at the END of every transaction (the same position, where the transaction left the
  reader), by `endDrag` (nulled; the next update re-reads it where the thumb left it) and by the
  follow transitions. `onScroll` does nothing to it at all: a reader's scroll does not compute
  anything, and it does not erase anything either — the scroll is theirs, and `P` plus what they
  scrolled since it was read is where they are (stage 2 changed this from "marks it stale": see
  §4.8, the fling case).
- `place(P, drift)`: computes `want` from `P` — the anchor's rect when its record is mounted
  (exact), the model sums when not (`documentTopOf(index) + offset`), `scrollHeight` for the tail —
  plus the reader's `drift` since `P` was read, and writes it through the one `frame.scrollTo` call,
  traced as `place` with `{source, key/index, want, delta, drift}`. The drift is the transaction's
  to compute, once, at its start: nothing the reader does can land inside a synchronous transaction,
  so an offset change after its start is a clamp or the engine's own write, never theirs. Replaces
  `restoreDomAnchor`, `restoreModelAnchor` and `convergeBottom`'s write (stage 1–2); the jump
  landing loop's write is stage 4's. **I2 by construction in the engine.**
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
- **The engine's own writes fire scroll events**, and they must not read as the reader's: a
  placement's scroll event lands inside the intent window, so `classifyScroll` would take it for
  the reader (a placement that changes the gap could flip follow) and it would mark `P` stale on
  every transaction. `place()` records the offset it wrote; `onScroll` treats an event whose
  offset is within a pixel of that record as the engine's own — no staleness, no classification,
  no window update — and clears it. A smooth placement (the classic page's local `goTo` and
  `stepHead` keep their ease: it is reference behaviour) records its TARGET instead and owns
  every event until the offset arrives within a pixel of it or the reader's next input, whichever
  comes first; the frame's `scrollTo` takes the behaviour flag, so it is still the one write path.
- `transact()` is guarded against re-entry: an observer delivery or a page callback from
  `renderItem`/`afterMount` that reaches it while one is running is queued and run after, never
  nested.
- `owed`, `scheduleSettle`, `settleTimer`: deleted. There is no debt because nothing is dropped:
  a deferred transaction re-projects the CURRENT `P`, which the reader's own scrolls keep fresh.
  This is the #138 fix stated positively — the position that was "replayed late" was a stale
  capture; `P` is never stale because I1 gives the reader's scroll the write to it.

### 4.2 One reader state, one timer

The state is DERIVED from the clocks, never stored beside them: `readerOwnsPosition()` is
"dragging, or an input in the last `userIntentMs`" (as it always was), and the one timer
(`armRest`/`rest`) exists only to run what waited once that stops being true — re-armed by
`rest()` itself while it is still true, and by `endDrag`. Two things wait: a placement on the tail
(`pendingTail`, #165) and the sums taking a moved estimate (`estimatesPending`, #194). At rest the
estimates transaction runs first (it moves the sums, with `P` held across the shift), then the
converge. The three timers (`settleTimer`, `estimatesTimer`, `bottomTimer`) became this one;
`owed` and the settle's drop are gone with them (§4.8). A page that stamps `lastUserInput` itself
(the shell's keys, until stage 4) is covered: the timer re-checks the clock when it fires.

**Why a timer remains (I8):** wheel and trackpad gestures end without an event. A fling is a
sequence of scroll events with no input behind them; the only way to know the reader has stopped
is that no input has arrived for a while. That is one policy, stated once, and it decides only
WHEN a model-only transaction runs — never whether a position is written.

### 4.3 Transactions

```
transact(cause, { mutate, range, measure, position, spontaneous, tail, commanded })
  startTop = scrollTop
  P0 = positionFor(…)          // the tail while following (unless the reader's own scroll batch);
                               // nothing while dragging (I14) or for a jump; a page's own capture;
                               // the stored P as is for a spontaneous change; else re-read when the
                               // offset moved since P was read (I1)
  drift = P0.at != null ? startTop - P0.at : 0                        // the reader's scroll since
  mutate()                     // heights, estimates, skips, records
  rebuildPrefix()              // I3
  if mutate: updatePads()      // the pads follow the sums BEFORE a mount decides it has nothing to do
  mount(range(P0)) | measure() // pads first (I6), then the DOM, then the measure
  placed = P0.tail ? (commanded || resting ? place(tail) : defer("tail"))   // #165
                   : place(P0, drift)                                        // I2, I7
  if placed && viewport shows unmounted territory: mount(rangeForScroll()); place again   // #191
  syncPosition()               // P re-read where the transaction left the reader
  trace(cause, …)
```

Deferral is per KIND, not per transaction: the DOM and the sums always change now; only a tail
placement and an estimate application wait for rest (I8), on the one timer (§4.2). The
"deferrable reconcile when a delta mounts above a moving reader" branch and the lo-hold patch it
stood for are gone: with the placement synchronous, a mount above a moving reader is placed back in
the same task (the fling case measures exactly that), and there is nothing to hold off.

Every path that changes the sums goes through it: the observers' measure (not deferrable — the
DOM already moved; the write is deferrable only when `P` is the tail, above), reconcile (not deferrable when it mounts on the reader's own scroll path;
deferrable when a delta wants to mount above a moving reader — the lo-hold patch goes, the
transaction simply waits), estimate application (deferrable), `recordsChanged` (not deferrable —
the tail must render — but its estimate application inside is), `remeasure` (not deferrable),
`jumpTo` and `follow` (never deferred: they are the reader's).

**I1, I4, I7, I11 by construction.** The "no anchor and not following" gap in the old
`scheduleEstimates` cannot exist: `P` always has a source — `syncPosition` stores the model form
when nothing mounted is on screen.

**Why a write under a gesture is safe to rely on.** The engine has written `scrollTop` under the
wheel on every scroll batch since #180 (`updateWindow(null, true)`), and trackpad momentum is
delivered as wheel events, so the walk and the growing-tail scenarios (#194) are already evidence
that a placement during a fling neither stutters nor dies. What #132 step 3's deferral protected was
a STALE position — a capture from before the reader moved — not the act of writing; with `P`
re-read at the start of every transaction there is no stale position to replay. #196's step 0 is
the one case that pins this before anything else moves: growth above the reader during a momentum
fling, both surfaces, holding to the pixel. Written and measured on 2026-09-12
(`scenario_growth_above_the_reader_during_a_fling_holds`, `known_red_196`): a 300px growth above
the reader at the fourth of twelve decaying wheels moved them 503px for 780px of wheel on the app
shell and 484px on the classic page — the growth, never placed back.

### 4.4 The estimator moves into the engine

`this.guesses = new Map(kind → HeightGuess(floor))`, `this.shares = new Map(key → {kind, share})`.
`setHeight` in the engine learns/forgets; the page's `setHeight` only persists. `recordsChanged`
forgets the shares of identities that vanished. **I5 in one place.** `apply()` is called only from
the estimate transaction. The classic page's `recShares[]` and the app shell's `this.shares` go.
Landed as stage 3 (§4.9); until stage 4's `recordsChanged`, the shell's `setUnits` calls
`forget(key)` and the classic `resetFrom` calls `forgetFrom(from)` before it truncates.

### 4.5 The tail converge is observer-driven

`follow()` sets `P := tail` and places. Every later transaction, while following, ends with
`place()` to the tail — including the ones the per-item and run observers raise when heights
settle after the mount. The seven `setTimeout(0)` passes go; `bottomTimer` goes. Under a gesture
while following, tail transactions are deferrable (the growth is below the reader; #165) and the
reader's scroll past the hold slack unfollows first (I13).

### 4.6 The trace becomes the transaction log

Each entry is one transaction: `{cause, P0, deferred, mutated: [index, from, to, kind], lo, hi,
pads, want, delta}` — the shape #197's action-and-state history records and replays.

### 4.8 Stage 2 as landed (2026-09-12)

What landed: `transact()` with the shape above; one reader timer; `place(P, drift)` as the engine's
one write with no policy of its own; the model form captured with every anchor and used when the
anchor's identity is gone or names another record by index (I12); the engine's own scroll events
recognised by the offset they wrote (`scroll:own`); the observer-driven converge (one pass per
transaction, the seven timed passes gone); `owed`, `scheduleSettle`, `settleTimer`,
`estimatesTimer`, `bottomTimer` and the #194 lo-hold deleted; `readerReshaped` no longer clears the
input stamp. The trace became the transaction log (§4.6; CLAUDE.md has the vocabulary).

Three rules the design did not state, each found by a real-session probe on the day and each
worth stating because the suite was green through all of them:

1. **A reader's scroll moves `P`; it does not erase it.** The design said `onScroll` marks `P`
   stale and the next transaction re-reads it. For a change that has ALREADY moved the DOM — the
   fling case: a growth above the reader mid-fling — the observer fires in the same rendering
   update as the scroll event, and a re-read then describes the displaced view and corrects
   nothing: 503px of motion for 780px of wheel, unchanged by removing every deferral. `P` carries
   the offset it was read at, and the reader's scroll since is added to the placement (§1.7).
2. **The drift is computed once, at the transaction's start.** Measured from the current offset
   inside a transaction it counted the engine's own write as the reader's (a second mount in the
   same transaction placed the reader 4,589px past the anchor it had just put back) and a
   browser clamp too (the page shrank above a reader near its end when the sums took a smaller
   estimate: 1,882px of clamp, placed twice).
3. **The pads follow the sums before a mount decides it has nothing to do**, and a second mount
   in one transaction happens only when the viewport actually shows unmounted territory. An
   estimate transaction whose range came out unchanged wrote no pads, so the DOM and the sums
   disagreed and the coverage test read the new sums against the old page: a window 400 records
   above the reader on the classic page, and the owner's #194 walk cycling backward again on the
   shell. And the offset-based range disagrees with the one around `P` at its edges by
   construction; re-mounting on that alone mounted two windows per transaction, each measuring the
   shell's edge unit 11px differently (the demo's `.turn:first-child{padding-top:8px}` applies to
   the first MOUNTED turn, so a unit's height depends on whether it is the window's edge — a
   trait the sums do not know about, left for a follow-up).
4. **A converge follows a change.** The observer's initial notification for every freshly observed
   element is a measure that changes nothing; a tail placed on it anyway snapped a following reader
   who had nudged up inside the hold slack straight back (#127's case, and the classic page's
   "leave the tail first" cases). An observer-driven measure that measured nothing new places
   nothing, as the old converge ran only on `changed`. And the tail's target is the furthest the
   offset can go, not `scrollHeight`: written as `scrollHeight` the browser clamps it and every
   placement reads as a full-viewport correction that was never made (twelve in a row on a quiet
   page, in the trace).
5. **A write under the wheel is not what fights the reader.** `scenario_the_readers_motion_is_never_fought`
   asserted the old policy (no write while the intent window is open). The case now asserts what
   the reader can measure — the same record at the same screen offset through the storm, to the
   pixel — and prints the count rather than asserting it; measured on the committed engine, both
   pages wrote none during the storm (one intermediate build wrote once on the classic page, and
   the case is written so that a write which undoes displacement passes).
6. **The rest timer fires exactly at the end of the intent window.** The old engine re-checked
   ownership `userIntentMs` after each deferral, at whatever phase that fell; the one timer arms
   for the remainder of the window after the last input. `scenario_a_converge_yields_to_the_reader`
   drives a 7px notch every 200ms from the harness, and a CDP round trip under load pushed that
   past the classic page's 300ms window (two of three runs alone: the converge landed, as the rule
   says it may once a hand pauses); the old engine caught the same lapse only when its check fell
   inside it. The case now drives the crawl from inside the page, at the cadence it describes.

Measured, before → after, on the owner's sessions (hermetic copies, both pages):
`scenario_growth_above_the_reader_during_a_fling_holds` 503px / 484px lost → 0 (the `known_red_196`
marker came off); the unfold probe near a live tail on the classic page, worst wheel 51px → 1px
(the same 51px on the stage-0 and stage-1 engines — the deferred-then-dropped correction, entries
312–321 of its trace); the walk and the runaway probes unchanged (no backward turn, no motion after
the hands come off); the full browser suite green on both pages.

### 4.9 Stage 3 as landed (2026-09-12)

A move with no behaviour change, held to the same acceptance as stage 1: the probes must read the
same numbers before and after. What moved, and what had to survive it:

- **The engine constructs the estimator** from `floors` (required: a page must say what a record
  costs at least) and `defaultKind` (the first floor unless named). `kindOf(i)` is the one seam a
  page overrides; the app shell's fallback — a unit of a type with no floor learns as a `process` —
  is `kindFor(i)` in the engine now, and the classic page's single kind is `record`.
- **Learning is in `measureMounted`**, `learn(i, h)` immediately before the page's `setHeight` —
  so the classic page's `replaceMounted`, which reaches `measureMounted` directly, still learns.
  The page's `setHeight` is persistence and nothing else.
- **`clearHeights` / `scaleHeights` split**: the page clears or scales the heights it stores
  (each floors them at its own value, which stays page-side), the engine resets or scales the means
  and the shares beside it, in the remeasure transaction's mutation step.
- **Vanished identities**: the shell's `setUnits` calls the engine's `forget(key)` (was its own
  `forgetShare`); the classic `resetFrom` calls `forgetFrom(from)` BEFORE truncating, while the
  dropped records' ids can still be read — the same shares it forgot by index before, now by
  identity.
- **The `HeightGuess` docblock** describes the class below it (the mean over distinct records) and
  says where the shares live.
- **The contract** pins the engine's `learn`/`forget`/`forgetFrom`/`applyEstimates` shapes, the
  `floors` requirement, both pages' persistence-only `setHeight`, and that neither page names
  `HeightGuess(`, `this.guesses`, `this.shares`, `recShares` or `estimator.` any more.

### 4.7 Out of scope

- Sparse vs dense mount under a filter (settled per page: `skipAt`/`renderAll`).
- The height measure (settled by #140 step 4: the classic page's CSS makes the two bases agree).
- The spy, the pane, the search reveal, the fold vocabulary — page-level, held by their own cases.
- A synthetic scrollbar. The browser owns `scrollTop`; the framework keeps the native scroller
  and writes it from `P` after every change, which is the whole of the trade discussed in
  `design/virtual-window.md` ("what it costs, honestly").

---

## 5. Held by what

- **The node contract** pins the shapes: one `frame.scrollTo` call site; the spontaneous
  transaction taking `P` as stored and the mount transaction re-reading it when the offset moved;
  the drift computed at the transaction's start; the pads written after a mutation before the mount;
  no `owed`, no settle, no lo-hold; the reconcile order; the trace vocabulary.
- **The scenarios on both surfaces** hold I9–I14 as behaviour: the pixel-hold cases, the growth
  cases, the walk, the growing tail, the deep jump, the held thumb, the end rule.
- **The real-session probes** (#194's `tmp_walk`, `tmp_runaway`, `tmp_unfold`, kept for #197)
  are the acceptance: before/after numbers on the owner's sessions, both pages.

The decision rule throughout: an invariant held by construction beats one held by a timer; the
one timer that stays (§4.2) decides only when a model-only change runs, and nothing about where
the reader is.
