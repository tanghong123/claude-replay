// The virtual window's ARITHMETIC, for both pages (#107 step 1, design/virtual-window.md).
// Two pages render transcripts of thousands of records by keeping only what is near the
// viewport in the DOM, the rest as two pads; each learned the same sums, the same binary
// search, the same window range and the same anchor correction in its own words, and the
// differences between those words are what #98 was. The rules live here now.
//
// Everything in this file is NUMBERS IN, NUMBERS OUT. No element, no layout read, no timer:
// the pages measure, the pages scroll, the pages mount. That is deliberate — a shared module
// that touched the DOM could not be run by the node contract, and this repo has been bitten
// three times by a shell that only a real browser could prove broken.
//
// Where the two pages genuinely differ, the difference is a PARAMETER, not a fork: the classic
// page wants an unclamped index (it addresses a record past the end while a filter is on), the
// app shell a clamped one; the classic page's first-visible test uses no epsilon and the app
// shell's a pixel; their following slacks differ. Each is named at the call site.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.

/** Prefix sums of `count` heights: `sums[i]` is the offset of item i, `sums[count]` the total.
 *  `heightAt(i)` is the page's — a measured height, or its own estimate for an unmeasured item
 *  (the classic page estimates UNDER at 30px so learning heights only grows the page below the
 *  reader; the app shell's 132px is the known-wrong side, and its own to fix). */
function prefixSums(count, heightAt) {
  const sums = [0];
  for (let i = 0; i < count; i++) sums.push(sums[i] + heightAt(i));
  return sums;
}

/** The item whose span contains offset `y`. `clamp` keeps the answer inside `[0, count-1]` (the
 *  app shell, which always addresses a mounted unit); without it the answer may be `count`,
 *  which is what the classic page wants when the offset is past the last record. */
function indexAt(sums, count, y, clamp) {
  if (!count) return 0;
  let lo = 0, hi = count;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (sums[mid + 1] > y) hi = mid;
    else lo = mid + 1;
  }
  return clamp ? Math.min(lo, count - 1) : lo;
}

/** The window the current scroll offset wants, with `overscan` of slack on each side. */
function rangeForScroll(sums, count, scrollTop, clientHeight, overscan) {
  const top = Math.max(0, scrollTop - overscan);
  const bottom = scrollTop + clientHeight + overscan;
  return { lo: indexAt(sums, count, top, true), hi: Math.min(count, indexAt(sums, count, bottom, true) + 1) };
}

/** The window around one item: `overscan` of content above it, a viewport plus `overscan` below
 *  — the shape a jump wants, where the target sits at the top and the reader looks down.
 *
 *  At the LAST item there is nothing below to spend the downward budget on, so the window is
 *  `overscan` alone — which is also the window the tail converge asks for, and it leaves the
 *  classic page opening at its tail with about a screenful less mounted than its own anchored
 *  walk used to give it. Handing the unspent budget to the upward walk was tried for #140 step 4
 *  and REVERTED: it widens every anchored update near the end, which on a page opened at its end
 *  is most of them, and three app-shell cases that had every right to stay put moved. The
 *  estimate error it was chasing had a nearer cause (the classic page never re-measured on a
 *  width change) and fixing that made this unnecessary. Left here so it is not tried a third
 *  time. */
function rangeAround(index, count, heightAt, clientHeight, overscan) {
  let lo = index, hi = index + 1, above = overscan, below = clientHeight + overscan;
  while (lo > 0 && above > 0) { lo--; above -= heightAt(lo); }
  while (hi < count && below > 0) { below -= heightAt(hi); hi++; }
  return { lo, hi };
}

/** A window clamped to what exists. */
function clampRange(lo, hi, count) {
  const start = Math.max(0, Math.min(lo, count));
  return { lo: start, hi: Math.max(start, Math.min(hi, count)) };
}

/** The two pads that stand in for what is not mounted. */
function padHeights(sums, lo, hi, count) {
  return { top: sums[lo] || 0, bottom: Math.max(0, sums[count] - (sums[hi] || 0)) };
}

/** Is a freshly measured height worth remembering? `minHeight` rejects a element that is not
 *  laid out yet (the app shell demands more than a pixel, the classic page more than zero), and
 *  `threshold` the sub-pixel noise that would rebuild the sums for nothing. */
function heightChanged(known, measured, minHeight, threshold) {
  return measured > minHeight && Math.abs(known - measured) > threshold;
}

/** A learned height for the items that have not been measured yet (#184 — and the amendment to
 *  rule 5 that it is).
 *
 *  Rule 5 said "estimate UNDER, never over", and half its reason still holds: guess HIGH and
 *  learning the truth SHRINKS the page. What it missed is that a constant FLOOR is not the neutral
 *  choice — it is the choice that MAXIMISES the distance between the guess and the truth, and that
 *  distance is exactly what displaces a reader when a run mounted ABOVE them turns from estimates
 *  into real heights. Measured under #180, walking up a 120-turn transcript in 900px steps: the
 *  record under the reader moved 2355px and 2854px on the app shell and 3263px on the classic page
 *  for a 900px request, and `scrollHeight` grew by the same amount each time — because every record
 *  above them was being carried at 30px against a real 78 or 184.
 *
 *  #179 then took away the other half of the old fear. The pads now carry the page's height BEFORE
 *  anything forces layout, so a page that shrinks under a reader no longer clamps them: the anchor
 *  correction is the only thing that has to catch it, and it is computed against the position they
 *  are at, in both directions equally.
 *
 *  So the rule becomes: **estimate CLOSE, and let the floor stand only while there is nothing to
 *  learn from.** A mean over one page's own records is right for the one quantity that matters —
 *  the SUM of a mounted run — even where it is wrong about any single record: guessing a prompt
 *  high and its answer low leaves the run they form exact.
 *
 *  Two guards, because a bad mean is worse than a floor. Nothing is used until `minSamples`
 *  records have been seen, so one short record cannot set the page's idea of a height. And a
 *  sample is CLAMPED to `outlier` times the running mean rather than rejected: one enormous record
 *  must not drag the mean, but dropping it altogether biases the mean low, which is the old bug
 *  wearing a hat.
 *
 *  It learns from MEASUREMENTS, not from items, so a record measured repeatedly as it streams in
 *  counts more than once, at the heights it passed through. That skews the mean DOWN — toward the
 *  floor, the side rule 5 always preferred — and only while a record is growing under the reader's
 *  eye. Keeping a per-item sample would cost an array the page already has no use for. */
class HeightGuess {
  constructor(floor, minSamples = 8, outlier = 4) {
    this.floor = floor;
    this.minSamples = minSamples;
    this.outlier = outlier;
    this.count = 0;
    this.sum = 0;
  }

  /** One measured height. At or under the floor teaches nothing: it is the floor itself, or an
   *  element the layout has not reached. */
  learn(height) {
    if (!(height > this.floor)) return;
    const mean = this.count ? this.sum / this.count : 0;
    this.sum += this.count >= this.minSamples ? Math.min(height, this.outlier * mean) : height;
    this.count += 1;
  }

  /** The floor until there is enough to say otherwise, then the running mean — never under the
   *  floor, which is a real lower bound on what a record can occupy. */
  value() {
    return this.count < this.minSamples ? this.floor : Math.max(this.floor, this.sum / this.count);
  }

  /** A width change re-guesses what has been learned, exactly as #132 step 4 re-guesses the
   *  measured heights: a block of text is about as tall as its measure is narrow. */
  scale(ratio) {
    this.sum *= ratio;
  }

  /** Nothing learned here applies any more — a new session, a font that changed the metrics. */
  reset() {
    this.count = 0;
    this.sum = 0;
  }
}

/** Is the viewport trace on (#192)? Decided from the page's own URL and storage — `?trace=viewport`
 *  for one load, `localStorage.viewportTrace = "1"` to keep it across reloads — and pure, so the
 *  contract can ask it without a browser. Off, the engine records nothing and pays one boolean. */
function traceWanted(search, stored) {
  if (/(^|[?&])trace=viewport(&|$)/.test(search || "")) return true;
  return stored === "1";
}

/** The scroll correction that puts an anchored element back where it was: the page measures
 *  `currentTop` and remembers `wantTop`, both relative to the viewport. Below `epsilon` the
 *  correction is noise and scrolling by it would fight the reader. */
function correction(currentTop, wantTop, epsilon) {
  const delta = currentTop - wantTop;
  return Math.abs(delta) > epsilon ? delta : 0;
}

/** The first item the reader can see, from rects the page has measured: `items` is
 *  `[{ index, top, bottom, height }]` in document order, `viewportTop`/`viewportBottom` bound
 *  the view. `epsilon` is how far above the fold still counts as gone (the app shell allows a
 *  pixel, the classic page none), and `requireHeight` skips items that are laid out to nothing.
 *  Returns the item, or null when the reader is past the last of them — which is not "the last
 *  item" but "no anchor at all": a dragged thumb leaves the window entirely, and then the
 *  scroll offset, not an anchor, says where the reader is. */
function firstVisible(items, viewportTop, viewportBottom, epsilon, requireHeight) {
  for (const item of items) {
    if (requireHeight && !(item.height > 0)) continue;
    if (item.bottom <= viewportTop + epsilon) continue;
    if (item.top >= viewportBottom) return null;
    return item;
  }
  return null;
}

/** Rule 7, the #103 hysteresis: what a scroll means. A scroll with the reader's input behind it
 *  DECIDES following — acquiring the pin needs the true end (`acquire`), keeping it only the old
 *  slack (`hold`); a scroll with no input behind it is displacement, and a followed view that
 *  has drifted more than `heal` from the end is put back. Returns `"follow"`, `"unfollow"`,
 *  `"heal"` or `"none"`. The two pages pass different slacks on purpose — the app shell decides
 *  on the true end in both directions today, the classic page holds at 80px. */
function classifyScroll(following, userIntent, gap, acquire, hold, heal) {
  if (userIntent) {
    const next = following ? gap <= hold : gap <= acquire;
    if (next === following) return "none";
    return next ? "follow" : "unfollow";
  }
  return following && gap > heal ? "heal" : "none";
}

/* ── the engine ───────────────────────────────────────────────────────────
 * The state machine the arithmetic above serves: the contiguous window and its pads, the
 * anchor, the two height observers, the measure schedule, the follow state, the thumb-drag
 * mode, the tail converge and the position-memory debounce. Both pages had all of it, twice.
 *
 * It knows nothing about records. A page supplies:
 *   frame        — where the scrolling happens: scrollTop()/scrollTo(y)/clientHeight()/
 *                  scrollHeight()/viewportTop()/on(type, fn, opts)/isScrollbarTarget(event).
 *                  An element scroller and the document are the same engine through it.
 *   mount        — { top, window, bottom }: the two pads and the element items mount into.
 *   count        — how many items there are (a getter on the page).
 *   identityAt   — a string stable across a rewrite that re-emits the same positions.
 *   estimateAt   — an item's height before it has been measured. UNDER, never over (rule 5).
 *   heightFor / setHeight / clearHeights — where measured heights live.
 *   clampIndex   (default true) whether an offset past the end reads as the last item.
 *   skipAt       (default none) an item the page is hiding: no height, and never mounted.
 *   renderAll    (default never) mount every item, whatever the offset says.
 *   renderItem   — index → an element, already stamped with its identity.
 *   following    — the page's own flag (both pages render from it), through a get/set pair.
 *   afterMount / afterRender / afterScroll / followChanged / remember — the page's hooks.
 *
 * Every layout read and DOM write in here is the engine's own; the arithmetic above stays pure
 * so the node contract can test the rules directly. */

/** Rule 8, #140 step 2: ONE measure. An item's height is its border box plus its margins — a
 *  margin the reader cannot see still takes the space that decides where everything below it
 *  sits. This is the only place either page turns an element into a height, so the two cannot
 *  drift apart again. (Top-to-next-top, which the classic page measured until #140, charges the
 *  gap between two items to the upper one, so which item owns a margin changes as the window
 *  slides; it was tried in the engine for #132 and reverted for moving the reader on a width
 *  reflow. #128, step 1, removed the reason the classic page needed it: `#stream` is a flex column
 *  with `#stream > * { margin-top: 0 }`, so on that page the two bases agree to the integer.)
 *  It reads layout, so it lives below the marker with the rest of the engine's DOM work and not
 *  among the rules, which stay numbers in, numbers out. */
function itemHeight(element) {
  const style = getComputedStyle(element);
  return element.getBoundingClientRect().height + (parseFloat(style.marginTop) || 0) + (parseFloat(style.marginBottom) || 0);
}

class VirtualWindow {
  constructor(options) {
    const { frame, mount, overscan, slacks, userIntentMs, rememberMs, clampIndex, skipAt, renderAll } = options;
    this.frame = frame;
    this.mount = mount.window;
    this.topPad = mount.top;
    this.bottomPad = mount.bottom;
    this.overscan = overscan;
    this.slacks = slacks;
    this.userIntentMs = userIntentMs;
    // #140 step 4, the three things the classic page needs and the app shell does not. Each is a
    // PARAMETER with the shell's behaviour as its default, not a fork:
    //   clampIndex  the shell asks "which item is at this offset" and wants the last one when the
    //               offset runs past the end; the classic page wants PAST THE END to read as past
    //               the end, because that is what places its bottom pad under a filter.
    //   skipAt      an item the page is hiding. It cannot be expressed as a zero height, because
    //               `heightOf` falls through to `estimateAt` whenever a height is falsy.
    //   renderAll   mount everything, whatever the offset. The classic page renders a SMALL
    //               filtered set in full so its sums are exact and a one-hit jump cannot land in
    //               a pad; hundreds of force-opened folds is why that is conditional, not always.
    this.clampIndex = clampIndex !== false;
    this.skipAt = skipAt || (() => false);
    this.renderAll = renderAll || (() => false);
    this.rememberMs = rememberMs;
    this.prefix = [0];
    this.lo = 0;
    this.hi = 0;
    // The width the remembered heights were measured at (#132 step 4). Zero until the first
    // `remeasure`, so the first one has no ratio and clears instead of scaling; a page for which
    // the FIRST width change is the one that matters seeds it itself, from the mount, which is
    // attached and empty here and therefore already at the width its items will be laid out at.
    this.lastWidth = 0;
    this.pendingScroll = false;
    // Sentinel far in the past: performance.now() is small right after load, so a 0 init would
    // read the load sequence's own scrolls (and the browser's async scroll restoration) as the
    // reader moving, and unpin a fresh page (#89).
    this.lastUserInput = -1e9;
    this.lastInputStamp = -1e9;
    this.anchor = null;
    this.dragging = false;
    this.bottomTimer = 0;
    this.rememberTimer = 0;
    // A correction the reader's own motion postponed (#132 step 3), and the timer that pays it.
    this.owed = null;
    this.settleTimer = 0;
    // The viewport trace (#192): every decision the engine makes, with the geometry it saw, in a
    // ring buffer the reader can copy out of a bug report — `copy(window.__viewportTrace)` — and
    // on the console under one prefix. On by the page's URL or storage; `options.trace` overrides.
    let stored = null;
    try { stored = typeof localStorage === "undefined" ? null : localStorage.getItem("viewportTrace"); } catch (e) { stored = null; }
    this.tracing = options.trace != null ? !!options.trace : traceWanted(typeof location === "undefined" ? "" : location.search, stored);
    this.traceSeq = 0;
    if (this.tracing && typeof window !== "undefined") window.__viewportTrace = window.__viewportTrace || [];

    // A height that changes on its own — a row growing, a font arriving, an estimate replaced —
    // is measured INSIDE the observer delivery (#132): after layout and before the frame paints,
    // so the pads move and the reader's position is written back in the same frame and nobody
    // sees the growth. Deferring this by a task painted the shifted content once first — a
    // one-frame jolt the classic page never had, because it restores inside its observer too.
    this.observer = new ResizeObserver(() => this.measureNow());
    // Displacement is a HEIGHT signal, not a scroll signal (#89): a late reflow — fonts
    // arriving, an estimate replaced by a real height, chrome around the window — grows the
    // content below the viewport WITHOUT firing a scroll event, and a pinned view is silently
    // parked a few pixels above the tail until the next apply happens to converge. The per-item
    // observer only hears an item's own height change; observing the content as a whole hears
    // every displacement, and any size change while following that leaves the bottom is healed
    // on the spot. Converging moves scroll, not size — no feedback loop.
    this.contentObserver = new ResizeObserver(() => {
      if (this.following && this.count && this.gapToBottom() > 1) this.convergeBottom();
    });
    // The MOUNTED window, not the content that also holds the pads: measuring writes the pads,
    // and a pad write inside a delivery would re-fire an observer that watched them — the loop
    // the browser cuts short with "undelivered notifications", deferring the very correction
    // this delivery exists to make.
    this.contentObserver.observe(mount.window);
    // …and the same signal from AROUND the window (#140 step 4). The observer above hears the
    // mounted run's own height; nothing else. A header wrapping to a second line, a panel
    // opening, a font arriving in the chrome above the run — each moves everything below it,
    // fires no scroll event, and leaves a pinned view parked above the tail and a reader pushed
    // silently down the page. The classic page has heard this since #89/#98 by observing
    // `document.body`; the engine could not simply do the same, because that element also holds
    // the pads and measuring WRITES them, so a delivery would re-fire itself (the loop the
    // browser cuts short with "undelivered notifications", dropping the very correction it
    // exists to make). The guard is what makes it safe: only a change in where the content
    // BEGINS counts, and a pad write never moves that.
    this.lastContentTop = null;
    this.outerObserver = new ResizeObserver(() => this.displaced());
    // BORDER-box, not the default content-box: what moves the run is often the chrome's own
    // padding — a header gaining a line of chips, a pane's inset — and a padding change leaves
    // the content box exactly as it was, so the default box hears nothing at all.
    if (mount.content) this.outerObserver.observe(mount.content, { box: "border-box" });
    const noteIntent = event => {
      if (event.type === "keydown" && event.target && /^(INPUT|TEXTAREA)$/.test(event.target.tagName)) return;
      if (event.type === "pointermove" && !event.buttons) return;
      this.markIntent(event.timeStamp);
    };
    for (const type of ["pointerdown", "pointermove", "wheel", "touchstart", "touchmove", "keydown"]) {
      frame.on(type, noteIntent, { passive: true, capture: true });
    }
    frame.on("scroll", event => this.onScroll(event), { passive: true });
    // The scrollbar thumb owns the position while the pointer holds it (#98).
    frame.on("pointerdown", event => { if (frame.isScrollbarTarget(event)) this.beginDrag(); }, { passive: true, capture: true });
    for (const type of ["pointerup", "pointercancel", "mouseup"]) addEventListener(type, () => this.endDrag(), { passive: true, capture: true });
    addEventListener("blur", () => this.endDrag());
  }

  /** An item's height as the sums see it: what was measured, or the page's estimate. */
  heightOf(index) {
    // A skipped item takes NO space — the pads absorb it. Asked BEFORE the estimate, because a
    // falsy height falls through to `estimateAt` and a skip would come back as an estimate.
    if (this.skipAt(index)) return 0;
    return this.heightFor(index) || this.estimateAt(index);
  }

  rebuildPrefix() {
    this.prefix = prefixSums(this.count, index => this.heightOf(index));
  }

  indexAt(y) {
    return indexAt(this.prefix, this.count, y, this.clampIndex);
  }

  /** Where the content the sums describe BEGINS, in the scroller's own coordinate (#140 step 4).
   *  Never zero on either page — the app shell's pads sit under the transcript's own top padding
   *  and the classic page's under a whole topbar and session header. Scroll-invariant by
   *  construction: a scroll of `d` moves the pad's rect by `-d` and `scrollTop` by `+d`, and the
   *  engine's own pad writes never move it either, which is what makes it safe for `displaced`
   *  to watch. Both readers want the same thing: an offset handed straight to the sums names an
   *  item about 250px late on the classic page and 24px late on the shell. */
  contentTop() {
    return this.topPad.getBoundingClientRect().top - this.frame.viewportTop() + this.frame.scrollTop();
  }

  rangeForScroll() {
    return rangeForScroll(this.prefix, this.count, this.frame.scrollTop() - this.contentTop(), this.frame.clientHeight(), this.overscan);
  }

  rangeAround(index) {
    return rangeAround(index, this.count, i => this.heightOf(i), this.frame.clientHeight(), this.overscan);
  }

  /** Which index carries this identity, or -1 — the scan a rewrite makes necessary. */
  indexOfIdentity(key) {
    for (let i = 0; i < this.count; i++) if (this.identityAt(i) === key) return i;
    return -1;
  }

  /** The reader's anchor is the first visible ROW, not the first visible item (#98): an item
   *  can hold a hundred rows, and anchored on the item a growth inside it above the visible
   *  region moves everything the reader is looking at while the item's own top never moves. An
   *  item that starts BELOW the viewport is no anchor at all — the reader has left the window
   *  entirely (a dragged thumb does that) and the scroll offset places it. */
  captureDomAnchor() {
    const viewportTop = this.frame.viewportTop();
    const viewportBottom = viewportTop + this.frame.clientHeight();
    const rects = element => {
      const rect = element.getBoundingClientRect();
      return { element, top: rect.top, bottom: rect.bottom, height: rect.height };
    };
    const child = firstVisible([...this.mount.children].map(rects), viewportTop, viewportBottom, 1, false);
    if (!child) return null;
    const anchor = { key: child.element.dataset.unitKey, top: child.top - viewportTop, block: null, blockTop: 0 };
    const rowIn = element => firstVisible([...element.querySelectorAll("[data-block-index]")].map(rects), viewportTop, Infinity, 1, true);
    // ONE predicate, applied from the item down: refine only while the thing you are holding
    // STRADDLES the viewport edge. Whatever straddles is the only thing whose top the reader
    // cannot see, so it is the only thing whose top is a lie about where they are reading.
    //
    // Descending matters because a parent qualifies whenever a child does and document order
    // offers the parent first, so an unrefined pick is always the OUTERMOST row — on the app
    // shell the wrapper around the record the reader is inside (#177), whose top sits far above
    // them and does not move when the record grows.
    //
    // Stopping matters just as much, and #178 measured why. When the picked item's own top is
    // VISIBLE, that top is already the better anchor: every row inside it sits at a fixed offset
    // below a top the reader can see. Refining anyway hands the anchor to a child BELOW the head
    // they are reading, and the next growth in that head drives it off the top of the screen —
    // 420px on the classic page, where the mounted item IS the record and `matBlock` indexes only
    // its nested `.blk` children, so the head is not an anchor candidate at all.
    //
    // Residual: a straddling parent whose own body fills the edge and whose first indexed child
    // begins well below it anchors lower than the reader is reading. Uncorrected before this too,
    // and it needs a tall body AHEAD of the children.
    let row = null;
    for (let scope = child; scope.top < viewportTop; scope = row) {
      const inner = rowIn(scope.element);
      if (!inner) break;
      row = inner;
    }
    if (row) { anchor.block = row.element.dataset.blockIndex; anchor.blockTop = row.top - viewportTop; }
    return anchor;
  }

  /** Put the reader back: the anchor IS the position (#132) — an item, a row inside it, and
   *  where on screen that row sat — and the scroll offset is DERIVED from it and WRITTEN, not
   *  nudged. `scrollTo(where it goes)` rather than `scrollBy(how far it has drifted)`: an
   *  increment carries whatever the last one missed and needs the anchor already laid out under
   *  the offset it is correcting, so a path that skips it leaves the error behind. Where the
   *  item goes is read from its own rect — the same quantity the sums stand for, known exactly
   *  where it is mounted, and not summed through heights a rewrite has just turned back into
   *  estimates. */
  restoreDomAnchor(anchor, immediate = false) {
    if (!anchor) return;
    const item = [...this.mount.children].find(child => child.dataset.unitKey === anchor.key);
    // Not mounted: nothing to hold it by. Placing it from the sums was tried and reverted —
    // the paths that lose the anchor are the ones that just cleared the heights (a width
    // change), so the sums there are estimates and the "restore" lands the reader somewhere
    // else entirely. Staying put is right.
    if (!item) { this.trace("restore:unmounted", { anchor: anchor.key }); return; }
    let within = 0, sat = anchor.top;
    if (anchor.block != null) {
      const row = item.querySelector(`[data-block-index="${anchor.block}"]`);
      if (row && row.getBoundingClientRect().height > 0) {
        // The row's place inside its item is relative geometry — the pads do not move it.
        within = row.getBoundingClientRect().top - item.getBoundingClientRect().top;
        sat = anchor.blockTop;
      }
    }
    // Where the item IS, in the scroller's coordinate. Measured, not summed: the sums are the
    // model's, and above a mounted item they are exact only when every height in between has
    // been measured — during a tail rewrite they are estimates again, and a position summed
    // through them lands on a different record (measured: twenty records off). The rect is the
    // same quantity the sums stand for, read where it is known exactly.
    const itemTop = item.getBoundingClientRect().top - this.frame.viewportTop() + this.frame.scrollTop();
    const want = itemTop + within - sat;
    const delta = correction(this.frame.scrollTop(), want, 1);
    if (!delta) return;
    // The reader is moving: do not write under them (#132 step 3). What they get instead is a
    // fresh anchor once they stop (#138) — never this position, replayed late.
    //
    // `immediate` is the one case where that rule INVERTS, and #180 measured why. On the SCROLL
    // path the engine has just mounted items ABOVE the reader whose remembered height was a floor
    // estimate (30px classic, 34 on the shell) against a real height five to twenty times that.
    // The pads absorbed the estimate, the mount adds the real height, and the content under the
    // reader moves down by the whole difference. Not writing does not leave the reader alone — it
    // displaces them by exactly the correction being withheld. Measured on the app shell, walking
    // up in 900px steps: +1954px of drift per step once the walk reaches unmeasured ground, more
    // than twice the distance asked for, with scrollHeight growing by the same amount.
    //
    // This does not reopen #138. That dropped the debt because the correction restored a position
    // captured BEFORE the reader moved — stale by the time it was paid, which is what dragged them
    // back. The scroll-path correction is computed AT the position they are at now; it replays
    // nothing. It only undoes the engine's OWN mount displacement, and over already-measured
    // ground the correction is zero and returns above, so this fires only where it is the lesser
    // harm.
    if (!immediate && this.readerOwnsPosition()) { this.trace("restore:deferred", { anchor: anchor.key, delta: Math.round(delta) }); this.owed = anchor; this.scheduleSettle(); return; }
    this.trace("restore:wrote", { anchor: anchor.key, delta: Math.round(delta), want: Math.round(want) });
    this.frame.scrollTo(want);
  }

  /** The position the SUMS name, for a reader no mounted item can hold (#191): the item whose span
   *  the scroll offset falls in, and how far into it. Read BEFORE a mount changes the sums, so it
   *  describes the layout the reader actually scrolled through. */
  modelAnchor() {
    const y = this.frame.scrollTop() - this.contentTop();
    const index = Math.max(0, Math.min(this.indexAt(y), this.count - 1));
    // The offset is SIGNED: above the first record (the page header, a jump to the very top) it is
    // how far above, and clamping it to zero would pull the reader down onto record 0 — measured
    // as the turn bar lighting up at the top of the page. Past the last record it is the bottom
    // padding. Either way the restore reproduces the offset the reader had.
    return { index, offset: y - (this.prefix[index] || 0) };
  }

  /** Put the reader back on the record the sums named (#191).
   *
   *  A jump into a pad — a long wheel fling, a thumb released over unmeasured ground — leaves no
   *  item on screen, so `captureDomAnchor` has nothing and the mount goes uncorrected: its measure
   *  moves the `HeightGuess` mean, every unmeasured record above re-estimates, the top pad grows by
   *  thousands of pixels, and `scrollTop` stays where it was — inside the pad the shift just made.
   *  Measured from the tail: −40,000px left the mounted window 1,944px BELOW the viewport on the
   *  classic page and 6,709px on the shell (−6,000 was enough on the classic page), at rest, every
   *  probe in a pad, until the reader scrolled again. `#180` made the scroll path's correction
   *  immediate; this is the case where there was no correction at all.
   *
   *  The model position is computed from where the reader IS, so writing it replays nothing
   *  (`#138`) and undoes only the engine's own shift — the argument `#180` made — which is why it
   *  lands regardless of intent. Then the window is settled around the corrected offset, once: the
   *  range was chosen from the sums before the measure, and the viewport may now run past it. */
  restoreModelAnchor(held, immediate = false) {
    const want = this.documentTopOf(held.index) + held.offset;
    const delta = correction(this.frame.scrollTop(), want, 1);
    this.trace(delta ? "model:wrote" : "model:held", { index: held.index, offset: Math.round(held.offset), delta: Math.round(delta), want: Math.round(want) });
    if (delta) this.frame.scrollTo(want);
    const range = this.rangeForScroll();
    if (range.lo !== this.lo || range.hi !== this.hi) this.reconcile(range.lo, range.hi, Infinity, false, this.captureDomAnchor(), immediate);
  }

  /** One entry in the viewport trace (#192): what the engine decided, and the geometry it decided
   *  it against. Nothing here is computed unless the trace is on. */
  trace(event, fields) {
    if (!this.tracing) return;
    const now = performance.now();
    const entry = Object.assign({
      seq: ++this.traceSeq,
      t: Math.round(now),
      event,
      following: this.following,
      dragging: this.dragging,
      lo: this.lo,
      hi: this.hi,
      count: this.count,
      top: Math.round(this.frame.scrollTop()),
      height: Math.round(this.frame.scrollHeight()),
      pads: [Math.round(parseFloat(this.topPad.style.height) || 0), Math.round(parseFloat(this.bottomPad.style.height) || 0)],
      sinceInput: Math.round(now - this.lastUserInput),
      owed: !!this.owed,
    }, fields || {});
    if (typeof window !== "undefined" && window.__viewportTrace) {
      window.__viewportTrace.push(entry);
      if (window.__viewportTrace.length > 500) window.__viewportTrace.shift();
    }
    if (typeof console !== "undefined" && console.debug) console.debug("[viewport]", JSON.stringify(entry));
  }

  /** The anchor a SPONTANEOUS change is measured against (#98). A change the engine makes
   *  itself captures the anchor before it touches the DOM; a change that arrives on its own —
   *  a row resizing under the observer — has already moved the view by the time it is heard,
   *  and an anchor captured then describes the moved view and corrects nothing. So it is kept:
   *  refreshed on every settle, cleared the instant a scroll begins. */
  readerAnchor() {
    if (this.following || this.dragging) return null;
    return this.anchor || this.captureDomAnchor();
  }

  syncAnchor() {
    this.anchor = this.following || this.dragging ? null : this.captureDomAnchor();
  }

  /** Is the position the READER's right now? While a thumb is held, and for `userIntentMs`
   *  after a wheel or a touch — which is a fling still travelling. Writing `scrollTop` under
   *  either fights whoever owns the motion: the drag jumps under the pointer, the fling
   *  stutters or dies. The correction is not dropped, it is OWED (#132 step 3). */
  readerOwnsPosition() {
    return this.dragging || performance.now() - this.lastUserInput < this.userIntentMs;
  }

  /** The reader just did something. Two clocks, because they answer different questions: handler
   *  time is "how long since they touched anything", the EVENT's own stamp is what a later scroll
   *  event is compared against (#156). Public, because a page can move the view on the reader's
   *  behalf where no input event of its own fires — the classic page's drag-select auto-scroll
   *  runs on a 16ms timer while the pointer rests in the band, and without this stamp the engine
   *  reads each of those scrolls as displacement and, while pinned, heals it straight back. */
  markIntent(stamp) {
    this.lastUserInput = performance.now();
    this.lastInputStamp = stamp || performance.now();
  }

  /** Something around the mounted window changed size. Only a change in where the content BEGINS
   *  is a displacement — the pads move the content's END, and those are the engine's own writes —
   *  so this is a cheap no-op on everything except the case it is for. Following: the tail moved
   *  away and is converged back on. Reading: what they are looking at moved down by the growth
   *  above it, and the anchor puts it back. */
  displaced() {
    const top = this.contentTop();
    if (this.lastContentTop !== null && Math.abs(top - this.lastContentTop) < 0.5) return;
    const first = this.lastContentTop === null;
    this.lastContentTop = top;
    if (first || !this.count) return;
    if (this.following) { if (this.gapToBottom() > 1) this.convergeBottom(); }
    else this.restoreDomAnchor(this.readerAnchor());
  }

  /** What happens once the reader comes to rest (#138). NOT "pay the correction that was owed":
   *  the position that correction was protecting is from BEFORE they moved, and writing it back
   *  afterwards moves the page under someone who has stopped — a small jump every time they stop
   *  scrolling, which is exactly how it was reported. Where the reader is NOW is the reference,
   *  so the debt is dropped and the anchor is re-read from the view at rest. A displacement that
   *  happened mid-motion is not worth a visible jump to undo; it is indistinguishable from the
   *  reader's own movement anyway. Re-armed while they are still moving. */
  scheduleSettle() {
    clearTimeout(this.settleTimer);
    this.settleTimer = setTimeout(() => {
      if (this.following) { this.owed = null; return; }
      if (this.readerOwnsPosition()) { this.scheduleSettle(); return; }
      this.trace("settle", { dropped: !!this.owed });
      this.owed = null;
      this.syncAnchor();
    }, this.userIntentMs);
  }

  beginDrag() {
    if (this.dragging) return;
    this.dragging = true;
    this.anchor = null;
    this.lastUserInput = performance.now();
  }

  endDrag() {
    if (!this.dragging) return;
    this.dragging = false;
    // The model's own reset (#132 step 3): the offset the drag left names a record, that record
    // is mounted first, and the anchor is re-read there — rather than correcting toward an
    // anchor from before the drag, which is a place the reader deliberately left.
    this.owed = null;
    this.updateWindow();
    this.syncAnchor();
    this.scheduleRemember();
  }

  /** Heights from `itemHeight` (rule 8) — the same function the classic page measures with.
   *  The sums do not have to be exact for the restore: that reads the item's own rect. */
  measureMounted(anchor = this.readerAnchor(), immediate = false) {
    let changed = false;
    for (const child of this.mount.children) {
      const index = Number(child.dataset.unitIndex);
      const height = itemHeight(child);
      if (index >= 0 && index < this.count && heightChanged(this.heightOf(index), height, 1, 0.5)) {
        this.setHeight(index, height);
        changed = true;
      }
    }
    if (!changed) return false;
    this.rebuildPrefix();
    this.updatePads();
    this.trace("measured", { anchor: anchor ? anchor.key : null, immediate, estimate: this.count ? Math.round(this.estimateAt(0)) : null });
    if (!this.following) this.restoreDomAnchor(anchor, immediate);
    this.syncAnchor();
    return true;
  }

  /** The observer's own measure: synchronous, so it lands before the frame paints. */
  measureNow() {
    const changed = this.measureMounted();
    if (changed && this.following) this.convergeBottom();
  }

  /** Where item `index` starts, in the scroller's own coordinate, from the MODEL: the content's
   *  top edge plus the sums before it. Exact for a mounted item only when the heights between
   *  are measured, so the restore prefers the item's own rect and this serves the case that has
   *  no rect — an anchor that is not mounted at all. */
  documentTopOf(index) {
    return this.contentTop() + (this.prefix[index] || 0);
  }

  updatePads() {
    const pads = padHeights(this.prefix, this.lo, this.hi, this.count);
    this.topPad.style.height = `${pads.top}px`;
    this.bottomPad.style.height = `${pads.bottom}px`;
  }

  clearWindow() {
    this.observer.disconnect();
    this.mount.replaceChildren();
    this.lo = this.hi = 0;
    this.topPad.style.height = "0px";
    this.bottomPad.style.height = "0px";
  }

  /** The elements just mounted, ATTACHED, before anything has measured them (#140 step 4). A page
   *  that must write to a fresh element before its height counts does it here — the classic page
   *  clamps a long user turn there. Clamping after the measure would remember the UNCLAMPED
   *  height and then shrink it, which is the shrink above the reader that rule 5 forbids;
   *  `afterRender` is too late (it fires past `measureMounted`) and `renderItem` too early (the
   *  element is still detached, so `scrollHeight` reads 0). A no-op here so a subclass that has
   *  nothing to do — the app shell has nothing — needs no change. */
  afterMount(_fresh) {}

  /** Mount exactly `[lo, hi)`, reusing what is already right. `dirtyFrom` is the first index
   *  whose content changed; `refresh` rebuilds everything mounted. */
  reconcile(lo, hi, dirtyFrom = Infinity, refresh = false, anchor = this.following ? null : this.captureDomAnchor(), immediate = false) {
    // The page can ask for the whole thing (#140 step 4): a small filtered set rendered in FULL
    // has every height real, so the sums are exact and a jump cannot land in a pad.
    if (this.renderAll()) { lo = 0; hi = this.count; }
    ({ lo, hi } = clampRange(lo, hi, this.count));
    if (!refresh && dirtyFrom === Infinity && lo === this.lo && hi === this.hi) return false;

    this.observer.disconnect();
    for (const child of [...this.mount.children]) {
      const index = Number(child.dataset.unitIndex);
      if (index < lo || index >= hi || this.skipAt(index)) child.remove();
    }

    const fresh = [];
    let cursor = this.mount.firstElementChild;
    for (let index = lo; index < hi; index++) {
      // A skipped item is not mounted at all — the range walks OVER it and the pads, which count
      // it at zero height, absorb the space. That is what makes a sparse window sparse.
      if (this.skipAt(index)) continue;
      while (cursor && Number(cursor.dataset.unitIndex) < index) {
        const stale = cursor;
        cursor = cursor.nextElementSibling;
        stale.remove();
      }
      const reusable = cursor && Number(cursor.dataset.unitIndex) === index && cursor.dataset.unitKey === this.identityAt(index) && index < dirtyFrom && !refresh;
      if (reusable) {
        cursor = cursor.nextElementSibling;
        continue;
      }
      const item = this.renderItem(index);
      item.dataset.unitIndex = index;
      fresh.push(item);
      if (cursor && Number(cursor.dataset.unitIndex) === index) {
        const next = cursor.nextElementSibling;
        cursor.replaceWith(item);
        cursor = next;
      } else {
        this.mount.insertBefore(item, cursor);
      }
    }
    while (cursor) {
      const stale = cursor;
      cursor = cursor.nextElementSibling;
      stale.remove();
    }

    this.lo = lo;
    this.hi = hi;
    // PAD, then measure (#179). The pads stand in for everything the window does not mount, and
    // until they are written the page is short by exactly what the mutation above just dropped
    // off the top of the window — one whole turn, measured: 262px on the app shell, 228 on the
    // classic page. Anything that forces layout in that gap hands the browser a page that has
    // SHRUNK, and a browser clamps `scrollTop` to it. A reader sitting on the TAIL is pulled up
    // by the entire difference; the pads land a moment later and leave them that far above the
    // bottom of a page that is its old height again. Worse, the clamp's own scroll event arrives
    // inside the intent window, so the engine reads it as the reader's own — and past the hold
    // slack, so it unfollows on it too. That is the whole of #179: a wheel down at the tail
    // bounced the reader 262px back up, then 200 down, between exactly two positions for ever,
    // "letting me keep scrolling" while only the last few records ever re-rendered.
    //
    // Two things below force layout: `afterMount` — the classic page clamps a long user turn
    // there, one batched READ pass over the fresh elements — and `measureMounted`, which reads
    // every mounted child's box on both pages. So the pads go above both.
    //
    // This is NOT the order #140 step 4 rejected. That one wrote the pads for the new window
    // while the OLD elements were still mounted, which is short whenever the window grows. These
    // describe exactly what is mounted right now. And a measure cannot change them: they are
    // `prefix[lo]` and `prefix[count] - prefix[hi]`, sums over the items OUTSIDE the window,
    // while a measure only ever corrects the ones inside it. The trailing call stays because
    // `measureMounted` can still rebuild the sums under them.
    this.updatePads();
    this.afterMount(fresh);
    this.measureMounted(anchor, immediate);
    this.updatePads();
    this.restoreDomAnchor(anchor, immediate);
    for (const child of this.mount.children) this.observer.observe(child, { box: "border-box" });
    this.afterRender();
    this.syncAnchor();
    this.trace("reconciled", { dirtyFrom: dirtyFrom === Infinity ? null : dirtyFrom, refresh, anchor: anchor ? anchor.key : null, immediate, fresh: fresh.length, estimate: this.count ? Math.round(this.estimateAt(0)) : null });
    return true;
  }

  updateWindow(forceIndex = null, immediate = false) {
    if (!this.count) return;
    const anchor = this.following || this.dragging ? null : this.captureDomAnchor();
    // Nothing on screen to hold — the reader is in a pad (#191). The position the sums name is
    // the only one there is, read now, before the mount changes them.
    const held = anchor || forceIndex != null || this.following || this.dragging ? null : this.modelAnchor();
    const anchorIndex = forceIndex == null && anchor ? this.indexOfIdentity(anchor.key) : -1;
    const range = forceIndex != null ? this.rangeAround(forceIndex) : anchorIndex >= 0 ? this.rangeAround(anchorIndex) : this.rangeForScroll();
    this.trace("update", { anchor: anchor ? anchor.key : null, held: held ? held.index : null, force: forceIndex, immediate, range: [range.lo, range.hi] });
    this.reconcile(range.lo, range.hi, Infinity, false, anchor, immediate);
    if (held) this.restoreModelAnchor(held, immediate);
    this.syncAnchor(); // an unchanged window returns early above; the anchor is re-read either way
  }

  /** The reader changed what is ON the page — opened a fold, expanded a cap, asked for the whole
   *  of a capped output. **The pin is dropped** (#185).
   *
   *  Growth the reader ASKED for is not the tail moving away from them; it is them choosing
   *  something to read. Parked at the tail, following, a fold opens BELOW them: nothing above them
   *  moves — which is the anchor doing exactly its job — and the page is simply taller than it
   *  was, so it is no longer at its tail. The follow rule then does its own job on that, converges,
   *  and scrolls away the very thing the click asked to see. Reported as: "the block unfolds
   *  downward correctly (anything above it is not moved), so now the page is no longer at the
   *  bottom. However, apparently the engine did not think so and immediately snaps the page to the
   *  bottom."
   *
   *  Unconditional, on purpose. A height test was considered and withdrawn by the owner: converge
   *  when the opened block is short, unfollow when it is tall, means the same click does two
   *  different things depending on the block, which nobody can predict from the outside. "It is
   *  just one scroll away to re-pin the tail, and feels natural."
   *
   *  This is NOT #165's rule and does not weaken it. #165 defers a converge while a GESTURE is in
   *  flight and keeps the pin, because there the tail really did move — new content arrived. Here
   *  nothing arrived; the reader reshaped the page themselves, and who caused the growth is the
   *  whole distinction. So every path where the growth is not the reader's keeps the converge it
   *  has today, which is why this could not be "delete the converge from `render`".
   *
   *  Safe to arm from any control without asking: unpinned, the only thing it does is release the
   *  click's intent (#190, below) — and every caller IS a click, on both pages. */
  readerReshaped() {
    // A reshape is not a scroll, so the intent that clicked it must not own the position (#190).
    // `noteIntent` binds pointerdown, so the click that opened this fold or expander started
    // the `userIntentMs` window — and inside that window `restoreDomAnchor` DEFERS its
    // correction as owed (#132 step 3) and `scheduleSettle` DROPS it (#138). Right for a scroll:
    // paying an old position after the reader moved drags them back. Wrong here: no scroll
    // event fired, the reader has not moved, and the correction being withheld is this engine's
    // own re-measure — the case #180 already named on the scroll path, "not writing does not
    // leave the reader alone, it displaces them by exactly the correction being withheld".
    // Measured on the owner's 1210-turn session: the re-render re-ranged and the top pad moved
    // by 14,243px, scrollTop by 828, and the reader was left in pad thirteen thousand pixels
    // below the content — blank — until later measures landed them twenty turns away. Even
    // "Show 2 more" did it. Ten synthetic probes had all held, because a synthetic `click()`
    // fires no pointerdown and never started the window.
    //
    // Clearing the stamp here, and only here, is exactly the #185 rule taken one step: who
    // caused the growth decides whether the pin survives it — and whether the correction waits.
    // `lastInputStamp` is left alone; it is the EVENT clock `onScroll` classifies against, and a
    // scroll that follows this click is still the reader's own.
    this.lastUserInput = -1e9;
    this.trace("reshaped", { wasFollowing: this.following });
    if (!this.following) return;
    this.following = false;
    this.followChanged();
    // The converge already queued by an earlier growth checks `following` on each pass and stops
    // itself; the anchor is re-read because from here the reader's own position is the reference.
    this.syncAnchor();
  }

  /** Rebuild what is mounted — a fold opened, a filter changed — holding the reader's place. */
  render(forceIndex = null) {
    if (!this.count) return;
    if (forceIndex != null) {
      const range = this.rangeAround(forceIndex);
      this.reconcile(range.lo, range.hi, 0, false, this.following ? null : this.captureDomAnchor());
      return;
    }
    this.reconcile(this.lo, this.hi, Infinity, true, this.following ? null : this.captureDomAnchor());
    if (this.following) this.convergeBottom();
  }

  gapToBottom() {
    return this.frame.scrollHeight() - this.frame.clientHeight() - this.frame.scrollTop();
  }

  /** Measured on the EVENT'S clock, never on the handler's (#156). A scroll event carries the
   *  time it was CREATED, and the handler can run much later because a long task on the main
   *  thread holds the queue. Measured on the classic page, which had the same bug: a 908ms
   *  handler lag turned the reader's own scroll into "560ms since input" — outside the window —
   *  so the page called it displacement and healed them back to the tail, throwing away 1400px
   *  they had just scrolled. On the event's clock that same scroll is -348ms from the input:
   *  created before the gesture that followed it. `dragging` still answers for a held thumb,
   *  which produces no input event of its own. */
  onScroll(event) {
    const at = event && event.timeStamp ? event.timeStamp : performance.now();
    const user = this.dragging || at - this.lastInputStamp < this.userIntentMs;
    const verdict = classifyScroll(this.following, user, this.gapToBottom(), this.slacks.acquire, this.slacks.hold, this.slacks.heal);
    this.trace("scroll", { user, verdict, gap: Math.round(this.gapToBottom()), lag: Math.round(at - this.lastInputStamp) });
    if (verdict === "follow" || verdict === "unfollow") {
      this.following = verdict === "follow";
      this.followChanged();
    }
    if (user) this.scheduleRemember();
    else if (verdict === "heal") this.convergeBottom();
    // This scroll moved the reader: the kept anchor is stale until the deferred window update
    // re-reads it, once per batch of scroll events rather than per event.
    this.anchor = null;
    // …and a correction owed from BEFORE they moved is void (#138). The reader's own scroll makes
    // their position the authoritative one; paying an old debt afterwards drags them back to
    // where they were, and since every click registers as intent (a fold is a pointerdown), the
    // debt was owed by the fold and paid a third of a second after they scrolled away. Folding a
    // block and then scrolling read as a page that would not scroll — until the next record
    // arrived and reconciled the debt to a no-op.
    if (user && this.owed) { this.owed = null; clearTimeout(this.settleTimer); }
    this.afterScroll();
    if (this.pendingScroll) return;
    this.pendingScroll = true;
    setTimeout(() => {
      this.pendingScroll = false;
      // The reader's OWN scroll is the one window update whose correction must land NOW (#180):
      // what it corrects is this engine's own mount replacing floor estimates above them. Every
      // other caller keeps #132's deferral — the drag end (the thumb owns the position and the
      // anchor is null there anyway), the jump paths (they run their own landing loops and stamp
      // lastUserInput precisely so the anchor does not fight them), and every apply path.
      this.updateWindow(null, true);
    }, 0);
  }

  /** Sit on the tail and stay there while the heights under it settle. */
  convergeBottom(commanded) {
    clearTimeout(this.bottomTimer);
    const settle = pass => {
      if (!this.following || !this.count) return;
      // #165: the pin does not outrank a hand on the wheel. Converging writes `scrollTop`, and
      // writing it under a reader who is mid-gesture is the same act `restoreDomAnchor` already
      // refuses (#132 step 3) — the guard was simply never on this path. What it cost: measured
      // on a session with queued prompts, whose pickups REWRITE the tail rather than extend it,
      // an apply landed once a second through a five-second gesture and reset the reader to the
      // end every time. They crawled 21px up and were slammed back, seven times, never reaching
      // the 80px the hysteresis needs to unfollow. "It scrolls up and gets pulled down
      // immediately", and "only after a few trials it would eventually allow me to scroll" —
      // a trial only succeeded when no apply happened to land inside it.
      //
      // Deferred, never dropped: the reader is still following, so the tail is still theirs to
      // sit on once they stop. Re-armed while they keep moving, exactly as `scheduleSettle`
      // does for the correction it owes, and ended by the `following` check above the moment
      // their gesture finally clears the slack. `commanded` is the exception: the jump-to-end
      // pill, a keyboard End, a session opening at its tail. That click stamps input like any
      // other, so without it the one converge the reader ASKED for would be the one deferred.
      if (!commanded && this.readerOwnsPosition()) {
        this.trace("converge:deferred", { pass, commanded: !!commanded });
        this.bottomTimer = setTimeout(() => settle(pass), this.userIntentMs);
        return;
      }
      this.trace("converge", { pass, commanded: !!commanded, gap: Math.round(this.gapToBottom()) });
      const range = this.rangeAround(this.count - 1);
      this.reconcile(range.lo, range.hi, Infinity, false, null);
      this.frame.scrollTo(this.frame.scrollHeight());
      this.measureMounted(null);
      if (this.gapToBottom() > 1 && pass < 7) this.bottomTimer = setTimeout(() => settle(pass + 1), 0);
    };
    settle(0);
  }

  /** The layout changed under everything: a pane opened, the font arrived, the window resized.
   *  What that does to a height is not "unknown" — a block of text is about as tall as its
   *  measure is narrow — so a width change SCALES the remembered heights (#132 step 4) rather
   *  than throwing them away and falling back to per-kind floors, which shrinks the page under
   *  the reader until they have scrolled through it again. The mounted ones are corrected
   *  exactly by the observer a frame later; only the invisible groups keep the guess. A change
   *  that is not a width change (a font) has no ratio to apply, and there the old rule stands. */
  remeasure() {
    const anchor = this.following ? null : this.captureDomAnchor();
    const width = this.mount.getBoundingClientRect().width;
    const ratio = this.lastWidth && width ? this.lastWidth / width : 0;
    this.trace("remeasure", { ratio: Math.round(ratio * 1000) / 1000, anchor: anchor ? anchor.key : null });
    this.lastWidth = width || this.lastWidth;
    if (ratio && Math.abs(ratio - 1) > 0.01 && this.scaleHeights) this.scaleHeights(ratio);
    else this.clearHeights();
    this.rebuildPrefix();
    const anchorIndex = anchor ? this.indexOfIdentity(anchor.key) : -1;
    const range = anchorIndex >= 0 ? this.rangeAround(anchorIndex) : this.rangeForScroll();
    this.reconcile(range.lo, range.hi, 0, false, anchor);
    if (this.following) this.convergeBottom();
  }

  scheduleRemember() {
    clearTimeout(this.rememberTimer);
    this.rememberTimer = setTimeout(() => this.remember(), this.rememberMs);
  }
}

/** The element-scroller frame: a div that scrolls its own content. */
/** The same contract over the DOCUMENT scroller (#140 step 4). The classic page scrolls the page
 *  itself, not a box inside it, and the differences are all real ones rather than aliases:
 *  `viewportTop` is 0 because the document's own client box IS the viewport; the events are on
 *  `window`, since `scroll` does not fire on `document.scrollingElement`; and a pointer on the
 *  document's scrollbar lands on `<html>`, which is the scrolling element itself — the same test
 *  `elementFrame` makes, but its target is the one the document reports. */
function documentFrame() {
  const el = () => document.scrollingElement || document.documentElement;
  return {
    scrollTop: () => el().scrollTop,
    scrollTo: y => { el().scrollTop = y; },
    scrollBy: dy => { el().scrollTop += dy; },
    clientHeight: () => el().clientHeight,
    scrollHeight: () => el().scrollHeight,
    viewportTop: () => 0,
    on: (type, fn, options) => window.addEventListener(type, fn, options),
    // A pointer on the DOCUMENT's scrollbar lands on the scrolling element itself. It must NOT
    // also accept `body`: the classic page centres `.layout` at max-width 1160px (export.css:338),
    // so on any wider window the left and right GUTTERS are body — and treating a gutter click as
    // a thumb grab would put the page in drag mode, where `onScroll` calls every scroll the
    // reader's and the drag-select auto-scroll would silently unpin a followed view. Where a
    // classic scrollbar reserves a gutter the coordinate test catches it; where an overlay one
    // does not (macOS), it sits over the scrolling element and the target test does.
    isScrollbarTarget: event => event.target === el()
      || (event.clientX != null && event.clientX > document.documentElement.clientWidth),
  };
}

function elementFrame(scroller) {
  return {
    scrollTop: () => scroller.scrollTop,
    scrollTo: y => { scroller.scrollTop = y; },
    scrollBy: dy => { scroller.scrollTop += dy; },
    clientHeight: () => scroller.clientHeight,
    scrollHeight: () => scroller.scrollHeight,
    viewportTop: () => scroller.getBoundingClientRect().top,
    on: (type, fn, options) => scroller.addEventListener(type, fn, options),
    // A pointer that lands on the scroller ITSELF is on its scrollbar OR in a GUTTER: content
    // lands on a descendant, but a centred child leaves the scroller's own background exposed on
    // both sides, and `.transcript-inner` is `margin: 0 auto` inside `min(880px, 100% - 76px)`.
    // Reading a gutter press as a thumb grab puts the engine in drag mode for as long as the
    // button is held — where every scroll counts as the reader's and no anchor is held at all —
    // which is the whole of a drag-select. So the target test stays and a coordinate band is
    // added: a scrollbar sits against the inline end whichever kind it is. `clientWidth` EXCLUDES
    // a classic scrollbar's gutter and INCLUDES an overlay one (macOS), and the 20px band covers
    // both without a media query. (#140 step 4 — the twin of `documentFrame`'s predicate, which
    // wrongly accepted `body` for the same reason, found on the other page.)
    isScrollbarTarget: event => {
      if (event.target !== scroller) return false;
      if (event.clientX == null) return true;
      return event.clientX > scroller.getBoundingClientRect().left + scroller.clientWidth - 20;
    },
  };
}

export { prefixSums, indexAt, rangeForScroll, rangeAround, clampRange, padHeights, heightChanged, HeightGuess, correction, firstVisible, classifyScroll, itemHeight, VirtualWindow, elementFrame, documentFrame, traceWanted };
