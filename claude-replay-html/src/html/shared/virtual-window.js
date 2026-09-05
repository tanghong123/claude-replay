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
 *  — the shape a jump wants, where the target sits at the top and the reader looks down. */
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

export { prefixSums, indexAt, rangeForScroll, rangeAround, clampRange, padHeights, heightChanged, correction, firstVisible, classifyScroll };
