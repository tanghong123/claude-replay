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
 *   renderItem   — index → an element, already stamped with its identity.
 *   following    — the page's own flag (both pages render from it), through a get/set pair.
 *   afterRender / afterScroll / followChanged / remember — the page's hooks.
 *
 * Every layout read and DOM write in here is the engine's own; the arithmetic above stays pure
 * so the node contract can test the rules directly. */
class VirtualWindow {
  constructor(options) {
    const { frame, mount, overscan, slacks, userIntentMs, rememberMs } = options;
    this.frame = frame;
    this.mount = mount.window;
    this.topPad = mount.top;
    this.bottomPad = mount.bottom;
    this.overscan = overscan;
    this.slacks = slacks;
    this.userIntentMs = userIntentMs;
    this.rememberMs = rememberMs;
    this.prefix = [0];
    this.lo = 0;
    this.hi = 0;
    this.pendingScroll = false;
    // Sentinel far in the past: performance.now() is small right after load, so a 0 init would
    // read the load sequence's own scrolls (and the browser's async scroll restoration) as the
    // reader moving, and unpin a fresh page (#89).
    this.lastUserInput = -1e9;
    this.anchor = null;
    this.dragging = false;
    this.bottomTimer = 0;
    this.rememberTimer = 0;

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
    const noteIntent = event => {
      if (event.type === "keydown" && event.target && /^(INPUT|TEXTAREA)$/.test(event.target.tagName)) return;
      if (event.type === "pointermove" && !event.buttons) return;
      this.lastUserInput = performance.now();
    };
    for (const type of ["pointerdown", "pointermove", "wheel", "touchstart", "touchmove", "keydown"]) {
      frame.on(type, noteIntent, { passive: true, capture: true });
    }
    frame.on("scroll", () => this.onScroll(), { passive: true });
    // The scrollbar thumb owns the position while the pointer holds it (#98).
    frame.on("pointerdown", event => { if (frame.isScrollbarTarget(event)) this.beginDrag(); }, { passive: true, capture: true });
    for (const type of ["pointerup", "pointercancel", "mouseup"]) addEventListener(type, () => this.endDrag(), { passive: true, capture: true });
    addEventListener("blur", () => this.endDrag());
  }

  /** An item's height as the sums see it: what was measured, or the page's estimate. */
  heightOf(index) {
    return this.heightFor(index) || this.estimateAt(index);
  }

  rebuildPrefix() {
    this.prefix = prefixSums(this.count, index => this.heightOf(index));
  }

  indexAt(y) {
    return indexAt(this.prefix, this.count, y, true);
  }

  rangeForScroll() {
    return rangeForScroll(this.prefix, this.count, this.frame.scrollTop(), this.frame.clientHeight(), this.overscan);
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
    const row = firstVisible([...child.element.querySelectorAll("[data-block-index]")].map(rects), viewportTop, Infinity, 1, true);
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
  restoreDomAnchor(anchor) {
    if (!anchor) return;
    const item = [...this.mount.children].find(child => child.dataset.unitKey === anchor.key);
    // Not mounted: nothing to hold it by. Placing it from the sums was tried and reverted —
    // the paths that lose the anchor are the ones that just cleared the heights (a width
    // change), so the sums there are estimates and the "restore" lands the reader somewhere
    // else entirely. Staying put is right.
    if (!item) return;
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
    if (correction(this.frame.scrollTop(), want, 1)) this.frame.scrollTo(want);
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

  beginDrag() {
    if (this.dragging) return;
    this.dragging = true;
    this.anchor = null;
    this.lastUserInput = performance.now();
  }

  endDrag() {
    if (!this.dragging) return;
    this.dragging = false;
    this.updateWindow();
    this.syncAnchor();
    this.scheduleRemember();
  }

  /** An item's height is its border box plus margins — a margin the reader cannot see still
   *  takes the space that decides where everything below it sits. (Measuring top-to-next-top
   *  instead, the way the classic page does, was tried for #132 and reverted: it attributes the
   *  gap between two items to the upper one, so which item owns a margin changes as the window
   *  slides, and a width reflow moved the reader off the unit they were reading. The sums do
   *  not have to be exact for the restore — that reads the item's own rect.) */
  measureMounted(anchor = this.readerAnchor()) {
    let changed = false;
    for (const child of this.mount.children) {
      const index = Number(child.dataset.unitIndex);
      const style = getComputedStyle(child);
      const height = child.getBoundingClientRect().height + (parseFloat(style.marginTop) || 0) + (parseFloat(style.marginBottom) || 0);
      if (index >= 0 && index < this.count && heightChanged(this.heightOf(index), height, 1, 0.5)) {
        this.setHeight(index, height);
        changed = true;
      }
    }
    if (!changed) return false;
    this.rebuildPrefix();
    this.updatePads();
    if (!this.following) this.restoreDomAnchor(anchor);
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
    const contentTop = this.topPad.getBoundingClientRect().top - this.frame.viewportTop() + this.frame.scrollTop();
    return contentTop + (this.prefix[index] || 0);
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

  /** Mount exactly `[lo, hi)`, reusing what is already right. `dirtyFrom` is the first index
   *  whose content changed; `refresh` rebuilds everything mounted. */
  reconcile(lo, hi, dirtyFrom = Infinity, refresh = false, anchor = this.following ? null : this.captureDomAnchor()) {
    ({ lo, hi } = clampRange(lo, hi, this.count));
    if (!refresh && dirtyFrom === Infinity && lo === this.lo && hi === this.hi) return false;

    this.observer.disconnect();
    for (const child of [...this.mount.children]) {
      const index = Number(child.dataset.unitIndex);
      if (index < lo || index >= hi) child.remove();
    }

    let cursor = this.mount.firstElementChild;
    for (let index = lo; index < hi; index++) {
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
      const fresh = this.renderItem(index);
      fresh.dataset.unitIndex = index;
      if (cursor && Number(cursor.dataset.unitIndex) === index) {
        const next = cursor.nextElementSibling;
        cursor.replaceWith(fresh);
        cursor = next;
      } else {
        this.mount.insertBefore(fresh, cursor);
      }
    }
    while (cursor) {
      const stale = cursor;
      cursor = cursor.nextElementSibling;
      stale.remove();
    }

    this.lo = lo;
    this.hi = hi;
    this.updatePads();
    this.measureMounted(anchor);
    this.restoreDomAnchor(anchor);
    for (const child of this.mount.children) this.observer.observe(child, { box: "border-box" });
    this.afterRender();
    this.syncAnchor();
    return true;
  }

  updateWindow(forceIndex = null) {
    if (!this.count) return;
    const anchor = this.following || this.dragging ? null : this.captureDomAnchor();
    const anchorIndex = forceIndex == null && anchor ? this.indexOfIdentity(anchor.key) : -1;
    const range = forceIndex != null ? this.rangeAround(forceIndex) : anchorIndex >= 0 ? this.rangeAround(anchorIndex) : this.rangeForScroll();
    this.reconcile(range.lo, range.hi, Infinity, false, anchor);
    this.syncAnchor(); // an unchanged window returns early above; the anchor is re-read either way
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

  onScroll() {
    const user = performance.now() - this.lastUserInput < this.userIntentMs;
    const verdict = classifyScroll(this.following, user, this.gapToBottom(), this.slacks.acquire, this.slacks.hold, this.slacks.heal);
    if (verdict === "follow" || verdict === "unfollow") {
      this.following = verdict === "follow";
      this.followChanged();
    }
    if (user) this.scheduleRemember();
    else if (verdict === "heal") this.convergeBottom();
    // This scroll moved the reader: the kept anchor is stale until the deferred window update
    // re-reads it, once per batch of scroll events rather than per event.
    this.anchor = null;
    this.afterScroll();
    if (this.pendingScroll) return;
    this.pendingScroll = true;
    setTimeout(() => {
      this.pendingScroll = false;
      this.updateWindow();
    }, 0);
  }

  /** Sit on the tail and stay there while the heights under it settle. */
  convergeBottom() {
    clearTimeout(this.bottomTimer);
    const settle = pass => {
      if (!this.following || !this.count) return;
      const range = this.rangeAround(this.count - 1);
      this.reconcile(range.lo, range.hi, Infinity, false, null);
      this.frame.scrollTo(this.frame.scrollHeight());
      this.measureMounted(null);
      if (this.gapToBottom() > 1 && pass < 7) this.bottomTimer = setTimeout(() => settle(pass + 1), 0);
    };
    settle(0);
  }

  /** Every measured height is wrong (the font changed, the window resized): learn them again. */
  remeasure() {
    const anchor = this.following ? null : this.captureDomAnchor();
    this.clearHeights();
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
function elementFrame(scroller) {
  return {
    scrollTop: () => scroller.scrollTop,
    scrollTo: y => { scroller.scrollTop = y; },
    scrollBy: dy => { scroller.scrollTop += dy; },
    clientHeight: () => scroller.clientHeight,
    scrollHeight: () => scroller.scrollHeight,
    viewportTop: () => scroller.getBoundingClientRect().top,
    on: (type, fn, options) => scroller.addEventListener(type, fn, options),
    // A pointer that lands on the scroller ITSELF is on its scrollbar: content lands on a
    // descendant. Not a coordinate test — an overlay scrollbar (macOS) sits inside the client
    // box. A false positive (the border) costs one window update on release.
    isScrollbarTarget: event => event.target === scroller,
  };
}

export { prefixSums, indexAt, rangeForScroll, rangeAround, clampRange, padHeights, heightChanged, correction, firstVisible, classifyScroll, VirtualWindow, elementFrame };
