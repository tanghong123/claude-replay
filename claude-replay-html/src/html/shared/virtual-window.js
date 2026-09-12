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
 *  It is a mean over DISTINCT records (#194): `learn(height, previous)` first takes back the share
 *  the same record taught last time, so a record measured at every size as it streams in counts
 *  once, at its latest height, and `forget(share)` withdraws a record that is gone. The class
 *  holds only the arithmetic; which share belongs to which record is the engine's (`shares`, by
 *  identity — framework §4.4), so both pages learn the same way. */
class HeightGuess {
  constructor(floor, minSamples = 8, outlier = 4) {
    this.floor = floor;
    this.minSamples = minSamples;
    this.outlier = outlier;
    this.count = 0;
    this.sum = 0;
    // What the sums READ (#194): the mean as of the last `apply`, never the live one. A mean that
    // moves under a reader mid-gesture re-estimates every never-measured record above them at
    // once — measured on the owner's session: a fraction of a pixel across 8,500 records was a
    // 7.8k px shift of the page above them — so the engine takes it only while they are at rest,
    // holding the anchor as it does.
    this.applied = floor;
  }

  /** One record's measured height, replacing what the SAME record taught before: `previous` is
   *  the share this returned for it last time, 0 for a record seen for the first time, and the
   *  return is the new share (0 when the height taught nothing). A record measured again adds no
   *  weight — the mean is over DISTINCT records, however often the open turn's tail is
   *  re-rendered or a growing record is measured at every size (#194: stacked samples of four
   *  tail records walked the shell's mean 337→362 in ten deltas with the record count flat). At
   *  or under the floor teaches nothing: it is the floor itself, or an element the layout has
   *  not reached. */
  learn(height, previous = 0) {
    this.forget(previous);
    if (!(height > this.floor)) return 0;
    const mean = this.count ? this.sum / this.count : 0;
    const share = this.count >= this.minSamples ? Math.min(height, this.outlier * mean) : height;
    this.sum += share;
    this.count += 1;
    return share;
  }

  /** A record that is gone — a rewrite dropped it, the page cleared it — takes its share back. */
  forget(share) {
    if (!share) return;
    this.sum -= share;
    this.count -= 1;
  }

  /** What the sums read: the mean as of the last `apply` (#194). */
  estimate() {
    return this.applied;
  }

  /** Take the live mean into what the sums read; true when it moved. The engine calls this only
   *  while the reader is at rest, and holds the anchor across the shift it makes (#194). */
  apply() {
    const next = this.value();
    if (next === this.applied) return false;
    this.applied = next;
    return true;
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
    this.applied = Math.max(this.floor, this.applied * ratio);
  }

  /** Nothing learned here applies any more — a new session, a font that changed the metrics. */
  reset() {
    this.count = 0;
    this.sum = 0;
    this.applied = this.floor;
  }
}

/** Is the viewport trace on (#192)? Decided from the page's own URL and storage — `?trace=viewport`
 *  for one load, `localStorage.viewportTrace = "1"` to keep it across reloads — and pure, so the
 *  contract can ask it without a browser. Off, the engine records nothing: `trace()` returns on one
 *  boolean before an entry exists, though every seam still evaluates the fields it passes (an object
 *  literal and a few rounded reads) — negligible, not zero. */
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
 *   kindOf       (default: one kind) the estimator kind of an item, one of the `floors` keys.
 *   floors       — { kind: px }: the floor each kind's running mean starts from (rule 5 as #184
 *                  amends it). The estimator itself — learn, forget, apply — is the engine's
 *                  (framework §4.4); `defaultKind` (default: the first floor) takes unknown kinds.
 *   heightFor / setHeight / clearHeights / scaleHeights — where measured heights live. Persistence
 *                  only: the engine learns from every measure before it hands the height over, and
 *                  `scaleHeights` keeps the page's own floor (#132 step 4).
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

/** `P` when the reader follows: the end, whatever the end is (framework §1.7). */
const TAIL = Object.freeze({ source: "tail" });

class VirtualWindow {
  constructor(options) {
    const { frame, mount, overscan, slacks, userIntentMs, rememberMs, clampIndex, skipAt, renderAll, floors, defaultKind } = options;
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
    // The estimator (framework §4.4): one `HeightGuess` per kind the page names in `floors`, seeded
    // by that kind's floor, and the share each identity taught it. Built here, before anything can
    // ask for a height; `defaultKind` is where a record of an unknown kind goes.
    if (!floors || !Object.keys(floors).length) throw new Error("VirtualWindow: `floors` (kind → px) is required");
    this.guesses = new Map(Object.entries(floors).map(([kind, floor]) => [kind, new HeightGuess(floor)]));
    this.defaultKind = defaultKind != null && this.guesses.has(defaultKind) ? defaultKind : Object.keys(floors)[0];
    this.shares = new Map();
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
    // `P`, the reader's position (framework §1.7), stored: an anchor — a mounted record, the row
    // in it, where on screen that row sat — or the model form when nothing mounted is on screen,
    // each carrying the scroll offset it was read at (`at`). Between transactions the only thing
    // that moves the offset is the reader, so `P` plus the offset's drift since it was read IS
    // where they are, exactly, without reading the DOM — which is what a change that has already
    // moved the DOM needs (`transact`, below). Re-read from the DOM at the end of every transaction
    // and at the start of one the engine is about to make when the offset has moved since. Null
    // while following (the tail is the position) and while a thumb is held (#98).
    this.position = null;
    this.dragging = false;
    this.rememberTimer = 0;
    // The one timer (framework §4.2): the reader is resting once `userIntentMs` has passed since
    // their last input, and that is the only thing it decides. Two things wait for it, because
    // both would move the view under a hand that is moving it: a placement on the tail (#165) and
    // the sums taking a moved estimate (#194). Never a correction — a change that has moved the
    // DOM is placed at once, whatever the reader is doing (framework I7; #180, #196).
    this.restTimer = 0;
    this.pendingTail = false;
    this.estimatesPending = false;
    // One transaction at a time (framework §4.3): a page callback that reaches the engine while
    // one is running — `afterRender`, `followChanged` — queues its own and runs it after.
    this.transacting = false;
    this.queued = [];
    // The offset the engine's last placement wrote, read back after the write so a clamped write
    // records what the browser kept. Its scroll event is the engine's own, not the reader's, and
    // `onScroll` recognises it by this value (framework §4.1). Kept until an event that does not
    // match or the reader's next input, never waited for: Chrome for Testing 151 fires none for an
    // assignment to `scrollTop`, stable 152 fires two.
    this.wrote = null;
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
      if (this.following && this.count && this.gapToBottom() > 1) this.transact("grown", { spontaneous: true });
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
    // Alongside the anchor, the model form of the same position (#191), read from the same sums,
    // now — before any rewrite moves them. It is the fallback if the anchor's identity is gone or
    // names another record by the time it is placed (framework I12; #165).
    const anchor = { source: "anchor", key: child.element.dataset.unitKey, index: Number(child.element.dataset.unitIndex), top: child.top - viewportTop, block: null, blockTop: 0, at: this.frame.scrollTop(), fallback: this.modelAnchor() };
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

  /** The scroll offset `P` names (framework §1.7). The anchor IS the position (#132) — an item, a
   *  row inside it, and where on screen that row sat — and the offset is DERIVED from it, from the
   *  item's own rect: the same quantity the sums stand for, known exactly where it is mounted, and
   *  not summed through heights a rewrite has just turned back into estimates (measured: twenty
   *  records off). The model form is the sums' own position, for a reader no mounted item can hold
   *  (#191). The tail is the end, whatever the end is. `null` for an anchor whose record is not
   *  mounted: nothing to hold it by. Placing it from the sums was tried and reverted — the paths
   *  that lose the anchor are the ones that just cleared the heights (a width change), so the sums
   *  there are estimates and the "restore" lands the reader somewhere else entirely. */
  offsetOf(position) {
    // The tail is the furthest the offset can go, not `scrollHeight`: written as `scrollHeight`
    // the browser clamps it and every placement reads as a full-viewport correction that was
    // never made (the trace showed twelve in a row at the bottom of a quiet page).
    if (position.source === "tail") return Math.max(0, this.frame.scrollHeight() - this.frame.clientHeight());
    if (position.source === "model") return this.documentTopOf(position.index) + position.offset;
    const item = [...this.mount.children].find(child => child.dataset.unitKey === position.key);
    if (!item || (position.index != null && Number(item.dataset.unitIndex) !== position.index)) {
      // The identity is gone, or names another record: ids are positional (#165), so after a
      // queued prompt's pickup the same id is the record one slot up. The model form captured with
      // the anchor, before the rewrite, is the position (framework I12). Merely unmounted — the
      // same record still at its index, outside the window — stays put, as above.
      const moved = position.index != null && this.indexOfIdentity(position.key) !== position.index;
      return moved && position.fallback ? this.offsetOf(position.fallback) : null;
    }
    let within = 0, sat = position.top;
    if (position.block != null) {
      const row = item.querySelector(`[data-block-index="${position.block}"]`);
      if (row && row.getBoundingClientRect().height > 0) {
        // The row's place inside its item is relative geometry — the pads do not move it.
        within = row.getBoundingClientRect().top - item.getBoundingClientRect().top;
        sat = position.blockTop;
      }
    }
    // Where the item IS, in the scroller's coordinate. Measured, not summed: the sums are the
    // model's, and above a mounted item they are exact only when every height in between has
    // been measured — during a tail rewrite they are estimates again.
    const itemTop = item.getBoundingClientRect().top - this.frame.viewportTop() + this.frame.scrollTop();
    return itemTop + within - sat;
  }

  /** The ONE write (framework I2): `P` in, `scrollTop` out, through the frame's `scrollTo`. Written,
   *  not nudged — `scrollTo(where it goes)` rather than `scrollBy(how far it has drifted)`: an
   *  increment carries whatever the last one missed and needs the anchor already laid out under
   *  the offset it is correcting, so a path that skips it leaves the error behind.
   *
   *  `P` was read at offset `at`; the reader may have scrolled since, and that scroll is theirs to
   *  keep (framework I1): what is written is where `P` sits now plus that `drift`, so over a DOM
   *  that has not moved the write is a no-op and over one that has it undoes exactly the engine's
   *  own displacement. The drift is the TRANSACTION's to compute, once, from the offset it started
   *  at: nothing the reader does can land inside a synchronous transaction, so an offset change
   *  after its start is never theirs — it is a clamp (the page shrank above a reader near its end
   *  while the sums took a smaller estimate; measured: 1,882px read as the reader's and placed
   *  twice) or the engine's own write. No policy lives here — whether to write at all is the transaction's decision
   *  (`transact`, framework §4.3), and for an anchor or a model position the answer is always yes:
   *  not writing does not leave the reader alone, it displaces them by exactly the correction
   *  withheld (#180 measured +1954px per 900px step on the scroll path; #196 measured 503px of
   *  motion for 780px of wheel with a 300px growth above a fling, and 51px lost per wheel under a
   *  live tail on the owner's session, both with the correction deferred and then dropped).
   *
   *  Returns whether the reader is where `P` says — placed, or already there. */
  place(position, drift = 0) {
    if (!position) return false;
    const base = this.offsetOf(position);
    if (base == null) { this.trace("place:unmounted", { anchor: position.key }); return false; }
    const want = base + drift;
    const delta = correction(this.frame.scrollTop(), want, 1);
    if (!delta) return true;
    this.frame.scrollTo(want);
    this.wrote = this.frame.scrollTop();
    this.trace("place", { source: position.source, anchor: position.key || null, index: position.index == null ? null : position.index, want: Math.round(want), delta: Math.round(delta), drift: Math.round(drift) });
    return true;
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
    return { source: "model", index, offset: y - (this.prefix[index] || 0), at: this.frame.scrollTop() };
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
      position: this.position ? `${this.position.source}:${this.position.key != null ? this.position.key : this.position.index}` : null,
      pending: (this.pendingTail ? "tail " : "") + (this.estimatesPending ? "estimates" : "") || null,
    }, fields || {});
    if (typeof window !== "undefined" && window.__viewportTrace) {
      window.__viewportTrace.push(entry);
      if (window.__viewportTrace.length > 500) window.__viewportTrace.shift();
    }
    if (typeof console !== "undefined" && console.debug) console.debug("[viewport]", JSON.stringify(entry));
  }

  /** `P` for a transaction about to run (framework §4.3). The tail while following; nothing while
   *  a thumb is held (framework I14) or the caller placed the reader itself (a jump); the anchor a
   *  page captured before its own mutation, when it hands one over. Otherwise the stored `P` —
   *  re-read from the DOM first if the offset has moved since it was read, because the engine is
   *  about to move the DOM and the view it reads now is still the reader's own.
   *
   *  A SPONTANEOUS change is the one case that must NOT re-read (#98): a row that grew under the
   *  observer has already moved the view by the time it is heard, and a position captured then
   *  describes the moved view and corrects nothing. That case takes the stored `P` as it is, and
   *  `place` adds the reader's scroll since it was read. This is what turns a growth above a fling
   *  into a no-op for the reader (#196): every scroll event used to clear the kept anchor, so the
   *  growth's observer found nothing to hold and re-read the displaced view. */
  positionFor(options) {
    if (options.place === false || options.position === null || !this.count) return null;
    if (this.following) return options.tail === false ? null : TAIL;
    if (this.dragging) return null;
    if (options.position) return options.position;
    if (options.spontaneous) return this.position;
    if (!this.position || this.position.at !== this.frame.scrollTop()) this.position = this.captureDomAnchor() || this.modelAnchor();
    return this.position;
  }

  /** Re-read `P` where the transaction left the reader: the same position, at the new offset. */
  syncPosition() {
    this.position = this.following || this.dragging || !this.count ? null : this.captureDomAnchor() || this.modelAnchor();
  }

  /** Is the reader moving the view right now? While a thumb is held, and for `userIntentMs` after
   *  a wheel or a touch — which is a fling still travelling. Two things wait for them to rest: a
   *  placement on the tail (#165) and the sums taking a moved estimate (#194). A correction does
   *  not — the engine has written under the wheel on every scroll batch since #180, and trackpad
   *  momentum is delivered as wheel events, so the walk and the growing-tail scenarios (#194) are
   *  the evidence that a placement mid-fling neither stutters nor dies. What #132 step 3's deferral
   *  protected was a STALE position, and `P` carries its own offset now (see `place`). */
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
    // The reader touched something: whatever the engine last wrote, the next scroll event is theirs.
    this.wrote = null;
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
    if (this.following && this.gapToBottom() <= 1) return;
    this.transact("displaced", { spontaneous: true });
  }

  /** Something is waiting for the reader to rest (framework §4.2, I8): arm the one timer for the
   *  rest of the intent window. Wheel and trackpad gestures end without an event — a fling is a
   *  sequence of scroll events with no input behind them — so time since the last input is the
   *  only signal there is, stated once, here. */
  defer(what) {
    if (what === "tail") this.pendingTail = true;
    else this.estimatesPending = true;
    this.armRest();
  }

  armRest() {
    clearTimeout(this.restTimer);
    const wait = Math.max(0, this.userIntentMs - (performance.now() - this.lastUserInput)) + 1;
    this.restTimer = setTimeout(() => this.rest(), wait);
  }

  /** The reader has come to rest: what waited runs, in order — the sums take the moved estimate
   *  first (#194, with `P` held across the shift and the window settled around it), then the tail
   *  is converged on if they are still following (#165). Nothing here is a correction from before
   *  they moved (#138): each is a transaction that reads where they are NOW. */
  rest() {
    if (this.dragging) return; // `endDrag` re-arms
    if (this.readerOwnsPosition()) { this.armRest(); return; }
    if (!this.pendingTail && !this.estimatesPending) return;
    this.trace("rest", { tail: this.pendingTail, estimates: this.estimatesPending });
    if (this.estimatesPending) {
      this.estimatesPending = false;
      this.transact("estimates", { mutate: () => this.applyLate(), range: p0 => this.rangeFor(p0) });
    }
    if (this.pendingTail) {
      this.pendingTail = false;
      this.convergeBottom();
    }
  }

  applyLate() {
    if (!this.applyEstimates()) return;
    this.trace("estimates:applied", { estimate: this.count ? Math.round(this.estimateAt(0)) : null, late: true });
  }

  beginDrag() {
    if (this.dragging) return;
    this.dragging = true;
    this.position = null;
    this.wrote = null;
    this.lastUserInput = performance.now();
  }

  endDrag() {
    if (!this.dragging) return;
    this.dragging = false;
    // The model's own reset (#132 step 3, framework I14): the offset the drag left names a record,
    // that record is mounted first, and `P` is re-read there — rather than correcting toward a
    // position from before the drag, which is a place the reader deliberately left.
    this.updateWindow();
    this.scheduleRemember();
    if (this.pendingTail || this.estimatesPending) this.armRest();
  }

  /** Heights from `itemHeight` (rule 8) — the same function the classic page measures with. A
   *  mutation step of the transaction that runs it (the observer's own, or a mount): it moves the
   *  sums and the pads, and the transaction places `P` after it. The sums do not have to be exact
   *  for that: the placement reads the item's own rect. */
  measureMounted() {
    let changed = false;
    // What changed, for the trace (#192): the first eight [index, from, to] — which record moved
    // the sums is the whole question when a reader is displaced under growth.
    const changes = this.tracing ? [] : null;
    for (const child of this.mount.children) {
      const index = Number(child.dataset.unitIndex);
      const height = itemHeight(child);
      if (index >= 0 && index < this.count && heightChanged(this.heightOf(index), height, 1, 0.5)) {
        if (changes && changes.length < 8) changes.push([index, Math.round(this.heightOf(index)), Math.round(height), this.heightFor(index) ? "measured" : "estimate"]);
        this.learn(index, height);
        this.setHeight(index, height);
        changed = true;
      }
    }
    if (!changed) return false;
    // The mean may have moved. It reaches the sums only while the reader is at rest (#194).
    this.settleEstimates();
    this.rebuildPrefix();
    this.updatePads();
    this.trace("measured", { estimate: this.count ? Math.round(this.estimateAt(0)) : null, live: this.count ? Math.round(this.liveEstimateAt(0)) : null, changes });
    return true;
  }

  /* ── the estimator (framework §1.3, §4.4) ──
   * One running mean per KIND of record, seeded by the page's floor for that kind, and one share
   * per identity so a record measured again replaces what it taught rather than adding to it
   * (I5). The page says what kind a record is and what a kind's floor is; everything the mean
   * does — learn, forget, apply, scale, reset — happens here, once, for both pages (#196 stage 3;
   * before it each page kept its own share bookkeeping around the shared `HeightGuess`). */

  /** The estimator kind of record `index`. A page with more than one kind overrides it (the app
   *  shell: a prompt card, an assistant note and a process group are three populations with three
   *  shapes, and one mean over all of them would be wrong about each); an unknown kind falls back
   *  to the default. */
  kindOf() { return this.defaultKind; }
  kindFor(index) {
    const kind = this.kindOf(index);
    return this.guesses.has(kind) ? kind : this.defaultKind;
  }
  guessFor(index) { return this.guesses.get(this.kindFor(index)); }
  /** An item's height before it has been measured: the APPLIED estimate, which moves only through
   *  `applyEstimates` (#194). UNDER, never over (rule 5): guess high and the page SHRINKS when the
   *  truth arrives, and a shrink above the viewport is a jump unless the position catches it. */
  estimateAt(index) { return this.guessFor(index).estimate(); }
  /** The live mean, for the trace only. */
  liveEstimateAt(index) { return this.guessFor(index).value(); }
  /** Take the live means into what the sums read; true when any moved. Called only from
   *  `settleEstimates` — from the measure when the reader is at rest, else as the estimates
   *  transaction once they rest (#194). */
  applyEstimates() {
    let moved = false;
    for (const guess of this.guesses.values()) if (guess.apply()) moved = true;
    return moved;
  }
  /** One measured height teaches the record's kind, replacing the share the SAME record taught
   *  before (a record measured at every size as it streams in counts once, at its latest height);
   *  a record whose kind changed takes its old share back from the old kind first. */
  learn(index, height) {
    const key = this.identityAt(index);
    const kind = this.kindFor(index);
    const prior = this.shares.get(key);
    let previous = 0;
    if (prior && prior.kind !== kind) this.guesses.get(prior.kind).forget(prior.share);
    else if (prior) previous = prior.share;
    this.shares.set(key, { kind, share: this.guesses.get(kind).learn(height, previous) });
  }
  /** A record that is gone takes back what it taught (#194). */
  forget(key) {
    const prior = this.shares.get(key);
    if (!prior) return;
    this.guesses.get(prior.kind).forget(prior.share);
    this.shares.delete(key);
  }
  /** The records from `index` on are about to be dropped or rewritten (a rewritten tail, #165):
   *  called BEFORE the page truncates them, while their identities can still be read. */
  forgetFrom(index) {
    for (let i = index; i < this.count; i++) this.forget(this.identityAt(i));
  }
  /** Nothing learned applies any more: a layout change with no ratio to apply (`remeasure`). */
  resetGuesses() {
    this.shares.clear();
    for (const guess of this.guesses.values()) guess.reset();
  }
  /** A width change re-guesses what has been LEARNED by the ratio the measured heights are scaled
   *  by (#132 step 4, #184): left alone, every unmeasured record would carry a height from the old
   *  measure, the staleness the scaling fixes for the measured ones. The shares scale with the
   *  means so a later re-measure withdraws the right amount. */
  scaleGuesses(ratio) {
    for (const guess of this.guesses.values()) guess.scale(ratio);
    for (const [key, prior] of this.shares) this.shares.set(key, { kind: prior.kind, share: prior.share * ratio });
  }

  /** Take a moved mean into the sums now, or hold it until the reader rests (#194).
   *
   *  The sums are built from every record's height, and for a never-measured record that is the
   *  estimate — so a change to the estimate is a change to every one of them at once. Above a
   *  reader who opened at the tail that is thousands of records, and the page above them moves
   *  by thousands of pixels: measured on the owner's session, a mean that moved by a fraction of
   *  a pixel across 8,500 records shifted the page 7.8k px. At rest the placement hides it (the
   *  scrollbar jumps, the content does not). Under the wheel the shift itself waits (framework I8)
   *  — not the write, which is never deferred, but the change to the sums: from the measure that
   *  moved the mean when no hand is on the wheel, else as a transaction of its own once the intent
   *  window has passed, with `P` held across the shift and the window settled around it. */
  settleEstimates() {
    if (this.readerOwnsPosition()) {
      if (!this.estimatesPending) this.trace("estimates:pending", {});
      this.defer("estimates");
      return false;
    }
    this.estimatesPending = false;
    const applied = this.applyEstimates();
    if (applied) this.trace("estimates:applied", { estimate: this.count ? Math.round(this.estimateAt(0)) : null, late: false });
    return applied;
  }

  /** The observer's own measure: synchronous, so it lands before the frame paints, as a
   *  transaction on a change that has ALREADY moved the DOM — `P` is not re-read (see
   *  `positionFor`), and the placement lands whatever the reader is doing (framework I7). */
  measureNow() {
    this.transact("measure", { spontaneous: true, measure: true });
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

  /** A page's own mount (framework §3.3): a records change with the anchor the page captured
   *  before it mutated its list, or a jump with none. One transaction. */
  reconcile(lo, hi, dirtyFrom = Infinity, refresh = false, anchor) {
    return this.transact("reconcile", { range: { lo, hi }, dirtyFrom, refresh, position: anchor });
  }

  /** Mount exactly `[lo, hi)`, reusing what is already right. `dirtyFrom` is the first index
   *  whose content changed; `refresh` rebuilds everything mounted. The mutation step of a mount
   *  transaction: it never places — `transact` does, after it. */
  mountRange(lo, hi, dirtyFrom = Infinity, refresh = false, p0 = null) {
    // The page can ask for the whole thing (#140 step 4): a small filtered set rendered in FULL
    // has every height real, so the sums are exact and a jump cannot land in a pad.
    if (this.renderAll()) { lo = 0; hi = this.count; }
    ({ lo, hi } = clampRange(lo, hi, this.count));
    if (!refresh && dirtyFrom === Infinity && lo === this.lo && hi === this.hi) return null;

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
    this.measureMounted();
    this.updatePads();
    for (const child of this.mount.children) this.observer.observe(child, { box: "border-box" });
    this.afterRender();
    this.trace("reconciled", { dirtyFrom: dirtyFrom === Infinity ? null : dirtyFrom, refresh, anchor: p0 && p0.key ? p0.key : null, fresh: fresh.length, estimate: this.count ? Math.round(this.estimateAt(0)) : null });
    return { lo, hi, fresh: fresh.length };
  }

  /** Does the mounted window cover what the viewport shows? The offset's first and last records,
   *  against `[lo, hi)`; the end of the page counts as covered when the last record is mounted. */
  viewportMounted() {
    const y = this.frame.scrollTop() - this.contentTop();
    const first = this.indexAt(Math.max(0, y));
    const last = Math.min(this.indexAt(y + this.frame.clientHeight()), this.count - 1);
    return first >= this.lo && last < this.hi;
  }

  /** The window `P` asks for (framework I11): around the record it names — the anchor's, by
   *  identity, the model form's by index, the last while following — and around the raw offset
   *  only when there is no `P` at all. */
  rangeFor(p0) {
    if (!p0) return this.rangeForScroll();
    if (p0.source === "tail") return this.rangeAround(this.count - 1);
    if (p0.source === "model") return this.rangeAround(Math.min(p0.index, this.count - 1));
    const at = this.indexOfIdentity(p0.key);
    if (this.tracing) this.lastRangeChoice = { at, index: p0.index == null ? null : p0.index, fallback: p0.fallback ? p0.fallback.index : null };
    if (at >= 0 && (p0.index == null || at === p0.index)) return this.rangeAround(at);
    return p0.fallback ? this.rangeAround(Math.min(p0.fallback.index, this.count - 1)) : this.rangeForScroll();
  }

  /** One change to the model or the DOM, as one unit (framework §4.3):
   *
   *      P0 = positionFor(…)          the reader's position, read BEFORE the sums move (I1)
   *      mutate()                     heights, estimates, records, skips
   *      rebuildPrefix()              the sums are a pure function of them (I3)
   *      mount(range around P0)       pads first (I6), then the DOM, then the measure
   *      place(P0)                    the one write (I2), at once (I7) — or, for the tail under
   *                                   a gesture, once the reader rests (#165)
   *      syncPosition()               `P` re-read where it left the reader
   *
   *  `options`: `mutate` — the model change; `range` — a range, or a function of `P0`, for a
   *  mount, else no mount; `measure` — measure what is mounted (the observer's own); `dirtyFrom`
   *  / `refresh` — what the mount rebuilds; `position` — a `P0` the caller read itself (a page's
   *  own capture, `null` for a jump); `spontaneous` — the DOM already moved, take `P` as stored;
   *  `tail: false` — while following, leave the tail alone (the reader's own scroll batch);
   *  `commanded` — the reader asked for the tail, so it does not wait. A mount that leaves the
   *  viewport past the window — the placement moved the offset by the sums' shift (#191) — mounts
   *  once more around the offset, and places again. */
  transact(cause, options = {}) {
    if (this.transacting) { this.queued.push([cause, options]); return false; }
    this.transacting = true;
    let summary = null;
    try {
      const startTop = this.frame.scrollTop();
      const p0 = this.positionFor(options);
      // The reader's scroll since `P` was read — everything the offset moved between the last
      // transaction and this one's start (see `place`). Zero after a re-read, and for a page's own
      // capture; the reader's motion for a spontaneous change.
      const drift = p0 && p0.at != null ? startTop - p0.at : 0;
      if (options.mutate) options.mutate();
      this.rebuildPrefix();
      // The pads follow the sums at once (framework I6): a mutation that moved them — an estimate
      // applied, heights scaled — has moved where every record IS, and a mount that finds its
      // range unchanged writes nothing. Left stale, the DOM and the sums disagree, the placement
      // sees nothing to correct, and the coverage test below reads the new sums against the old
      // page (measured: the estimate transaction after a jump mounted a window 400 records above
      // the reader on the classic page).
      if (options.mutate) this.updatePads();
      let mounted = null;
      let quiet = false;
      if (options.range) {
        const range = typeof options.range === "function" ? options.range(p0) : options.range;
        mounted = this.mountRange(range.lo, range.hi, options.dirtyFrom, options.refresh, p0);
      } else if (options.measure) {
        // An observer that heard nothing new — the notification every freshly observed element
        // sends — changes nothing, and a tail that is placed anyway snaps back a following reader
        // who has nudged up inside the hold slack (#127): the converge follows a CHANGE, as it did
        // when it ran on `changed` alone.
        quiet = !this.measureMounted();
      }
      let placed = quiet && p0 && p0.source === "tail" ? "quiet" : this.placeAfter(p0, options, drift);
      if (options.range && p0 && p0.source !== "tail" && placed === "placed" && !this.viewportMounted()) {
        // The window was chosen from the sums before the mutation; the placement moved the offset
        // by their shift, and the viewport now shows territory the window does not cover (#191).
        // Only then: the offset-based range and the one around `P` disagree by construction at
        // their edges, and re-mounting on that alone mounted two windows per transaction, each
        // measuring the edge unit differently (11px on the app shell, per transaction).
        const again = this.rangeForScroll();
        mounted = this.mountRange(again.lo, again.hi, Infinity, false, p0) || mounted;
        placed = this.placeAfter(p0, options, drift);
      }
      this.syncPosition();
      summary = Object.assign({ p0: p0 ? p0.source : null, anchor: p0 && p0.key ? p0.key : null, placed }, mounted ? { range: [mounted.lo, mounted.hi], fresh: mounted.fresh } : {}, options.fields || {});
      // Trace only: is the viewport inside the window it left, and how the window was chosen.
      if (this.tracing) { summary.covered = this.viewportMounted(); summary.choice = this.lastRangeChoice || null; this.lastRangeChoice = null; }
    } finally {
      this.transacting = false;
    }
    this.trace(cause, summary);
    if (this.queued.length) { const [next, opts] = this.queued.shift(); this.transact(next, opts); }
    return true;
  }

  /** The placement a transaction ends with. An anchor or a model position is written now; the
   *  tail waits for rest unless the reader asked for it (#165: the pin does not outrank a hand on
   *  the wheel — a tail rewrite once a second slammed a reader crawling up inside the hold slack
   *  back to the end, seven times). */
  placeAfter(p0, options, drift) {
    if (!p0) return null;
    if (p0.source === "tail") {
      if (!options.commanded && this.readerOwnsPosition()) { this.defer("tail"); this.trace("tail:deferred", {}); return "deferred"; }
      this.pendingTail = false;
      return this.place(p0) ? "tail" : "none";
    }
    return this.place(p0, drift) ? "placed" : "unmounted";
  }

  /** The window for where the reader is: the scroll batch's own update (#180 — the correction it
   *  needs is this engine's own mount replacing floor estimates above them, and it lands now), the
   *  end of a drag, a page after its own scroll write. A `forceIndex` is a jump's window. While
   *  following, the reader's scroll batch leaves the tail alone: their scroll is theirs, and
   *  `classifyScroll` has already decided whether it dropped the pin. */
  updateWindow(forceIndex = null) {
    if (!this.count) return;
    this.transact("update", { range: p0 => forceIndex != null ? this.rangeAround(forceIndex) : this.rangeFor(p0), tail: false, fields: { force: forceIndex } });
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
    // A reshape is not a scroll, and the click that caused it started the intent window (#190).
    // That used to matter: inside the window the correction was deferred as owed and then dropped,
    // and the reader was left in a pad thirteen thousand pixels below the content on the owner's
    // 1210-turn session — even "Show 2 more" did it. Nothing waits for the window now except the
    // tail and the estimates, and the pin is dropped right here, so there is no stamp to release.
    this.trace("reshaped", { wasFollowing: this.following });
    if (!this.following) return;
    this.following = false;
    this.followChanged();
    // A tail placement waiting for rest is void; `P` is re-read because from here the reader's own
    // position is the reference.
    this.pendingTail = false;
    this.syncPosition();
  }

  /** Rebuild what is mounted — a fold opened, a filter changed — holding the reader's place. */
  render(forceIndex = null) {
    if (!this.count) return;
    if (forceIndex != null) {
      this.transact("render", { range: this.rangeAround(forceIndex), dirtyFrom: 0 });
      return;
    }
    this.transact("render", { range: p0 => p0 && p0.source === "tail" ? this.rangeAround(this.count - 1) : { lo: this.lo, hi: this.hi }, refresh: true });
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
    // The engine's own write (framework §4.1): `place` recorded what it wrote, and an event at that
    // offset is neither the reader's nor displacement — nothing to classify, nothing to re-read,
    // no window to update (the transaction that placed already mounted around `P`).
    if (this.wrote != null && Math.abs(this.frame.scrollTop() - this.wrote) <= 1) {
      this.trace("scroll:own", { top: Math.round(this.frame.scrollTop()) });
      this.afterScroll();
      return;
    }
    this.wrote = null;
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
    // This scroll moved the reader, and that is all it does to `P` (framework I1): the position
    // keeps the offset it was read at, so what they scrolled since is known to the pixel without a
    // read, and the deferred window update below re-reads it once per batch of scroll events
    // rather than per event. Nothing is owed and nothing is dropped (#138): a correction that
    // reads where they are now replays nothing.
    this.afterScroll();
    if (this.pendingScroll) return;
    this.pendingScroll = true;
    setTimeout(() => {
      this.pendingScroll = false;
      this.updateWindow();
    }, 0);
  }

  /** Sit on the tail (framework §4.5): mount around the last record and place the end. What the
   *  heights under it do AFTER this — an image arriving, a font, an estimate replaced — the
   *  observers hear, and each is a transaction that places the tail again; the seven timed passes
   *  this used to run are that, driven by what actually changed. `commanded` is the reader asking
   *  for the end — the jump-to-end pill, a keyboard End, a session opening at its tail. That click
   *  stamps input like any other, so without it the one converge the reader ASKED for would be the
   *  one that waited (#165). */
  convergeBottom(commanded) {
    if (!this.following || !this.count) return;
    this.transact("converge", { range: () => this.rangeAround(this.count - 1), commanded: !!commanded, fields: { commanded: !!commanded, gap: Math.round(this.gapToBottom()) } });
  }

  /** The layout changed under everything: a pane opened, the font arrived, the window resized.
   *  What that does to a height is not "unknown" — a block of text is about as tall as its
   *  measure is narrow — so a width change SCALES the remembered heights (#132 step 4) rather
   *  than throwing them away and falling back to per-kind floors, which shrinks the page under
   *  the reader until they have scrolled through it again. The mounted ones are corrected
   *  exactly by the observer a frame later; only the invisible groups keep the guess. A change
   *  that is not a width change (a font) has no ratio to apply, and there the old rule stands. */
  remeasure() {
    const width = this.mount.getBoundingClientRect().width;
    const ratio = this.lastWidth && width ? this.lastWidth / width : 0;
    this.lastWidth = width || this.lastWidth;
    this.transact("remeasure", {
      mutate: () => {
        if (ratio && Math.abs(ratio - 1) > 0.01) { this.scaleHeights(ratio); this.scaleGuesses(ratio); } else { this.clearHeights(); this.resetGuesses(); }
      },
      range: p0 => this.rangeFor(p0),
      dirtyFrom: 0,
      fields: { ratio: Math.round(ratio * 1000) / 1000 },
    });
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
