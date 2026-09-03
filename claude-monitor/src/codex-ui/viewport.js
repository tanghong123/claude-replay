import { renderUnit } from "./components.js";

export function revealNavigationContext(units, index, state, recordIndex, reveal = "record") {
  const unit = units[index];
  if (!unit) return;
  if (unit.type === "user" && reveal !== "turn") state.promptExpanded.add(unit.key);
  if (unit.type === "process") {
    state.processFolds.set(unit.key, false);
    state.processExpanded.add(unit.key);
    const target = unit.views.find(item => item.index === recordIndex)?.view;
    if (target?.id && target.t !== "assistant") state.folds.set(target.id, false);
  } else if (reveal === "turn") {
    const process = units[index + 1];
    if (process?.type === "process" && process.turn === unit.turn) {
      state.processFolds.set(process.key, false);
      state.processExpanded.add(process.key);
    }
  }
}

// This adapter carries the original viewer's three scroll invariants into the demo's
// `.transcript` scroller: incremental materialization, DOM-identity anchoring, and an
// explicit follow mode changed only by user input.
import { parseViewMemory, serializeViewMemory, viewMemoryKey } from "./view-memory.js";

const ESTIMATE = 132;
const REMEMBER_MS = 250;
const OVERSCAN = 1500;
const HOLD_SLACK = 80;
const ACQUIRE_SLACK = 2;
const USER_INTENT_MS = 320;

export class Viewport {
  constructor(scroller, inner, state, actions) {
    this.scroller = scroller;
    this.inner = inner;
    this.state = state;
    this.actions = actions;
    this.units = [];
    this.prefix = [0];
    this.lo = 0;
    this.hi = 0;
    this.pendingScroll = false;
    this.pendingMeasure = false;
    this.lastUserInput = -1e9;
    this.bottomTimer = 0;
    // Per-session position memory (parity #6): the session this viewport is showing, the
    // remembered anchor still to be applied once its unit has streamed in, and a debounce.
    this.session = "";
    this.pending = null;
    this.pendingTries = 0;
    this.rememberTimer = 0;
    addEventListener("pagehide", () => this.remember());

    this.top = document.createElement("div");
    this.top.className = "virtual-pad";
    this.window = document.createElement("div");
    this.window.className = "virtual-window";
    this.bottom = document.createElement("div");
    this.bottom.className = "virtual-pad";
    this.empty = document.createElement("div");
    this.empty.className = "monitor-empty";
    this.empty.hidden = true;
    inner.replaceChildren(this.top, this.window, this.bottom, this.empty);

    this.observer = new ResizeObserver(() => this.scheduleMeasure());
    const noteIntent = event => {
      if (event.type === "keydown" && event.target && /^(INPUT|TEXTAREA)$/.test(event.target.tagName)) return;
      if (event.type === "pointermove" && !event.buttons) return;
      this.lastUserInput = performance.now();
    };
    for (const type of ["pointerdown", "pointermove", "wheel", "touchstart", "touchmove", "keydown"]) {
      scroller.addEventListener(type, noteIntent, { passive: true, capture: true });
    }
    scroller.addEventListener("scroll", () => this.onScroll(), { passive: true });
  }

  /** A session is opening: read what was remembered for it. A remembered anchor is applied by
   *  `setUnits` once its unit has streamed in (the stream arrives in batches); until then the
   *  viewport does not follow, so the tail never flashes past before the restore. Nothing
   *  remembered — or "following" remembered — means the tail, as it always did. */
  beginSession(session) {
    this.session = session || "";
    this.pendingTries = 0;
    let memory = null;
    try { memory = parseViewMemory(sessionStorage.getItem(viewMemoryKey(this.session))); } catch (_) {}
    this.pending = memory && !memory.following ? memory : null;
    this.state.following = !this.pending;
    this.state.newRecords = 0;
  }

  /** Remember the current position for this session: following, or the anchor. */
  remember() {
    clearTimeout(this.rememberTimer);
    if (!this.session || this.pending) return;
    const value = this.state.following ? { following: true } : this.captureDomAnchor();
    if (!value) return;
    try { sessionStorage.setItem(viewMemoryKey(this.session), serializeViewMemory(value.following ? value : { following: false, key: value.key, top: value.top })); } catch (_) {}
  }

  scheduleRemember() {
    clearTimeout(this.rememberTimer);
    this.rememberTimer = setTimeout(() => this.remember(), REMEMBER_MS);
  }

  setUnits(units, changedUnit = 0) {
    this.empty.hidden = true;
    const following = this.state.following;
    const anchor = following ? null : this.captureDomAnchor();
    const oldKeys = new Set(units.slice(0, changedUnit).map(unit => unit.key));
    const nextKeys = new Set(units.map(unit => unit.key));
    for (const key of this.state.heights.keys()) if (!oldKeys.has(key) && !nextKeys.has(key)) this.state.heights.delete(key);
    for (let i = changedUnit; i < units.length; i++) this.state.heights.delete(units[i].key);
    this.units = units;
    this.rebuildPrefix();

    if (!units.length) {
      this.clearWindow();
      return;
    }
    if (this.pending) {
      const index = units.findIndex(unit => unit.key === this.pending.key);
      if (index >= 0) {
        const memory = this.pending;
        this.pending = null;
        this.state.following = false;
        const range = this.rangeAround(index);
        this.reconcile(range.lo, range.hi, changedUnit, false, null);
        this.restoreDomAnchor({ key: memory.key, top: memory.top });
        this.updateWindow(index);
        this.actions.followChanged?.();
        return;
      }
      // Not streamed in yet — keep waiting a few batches, then give the tail up as lost.
      if (++this.pendingTries > 12) { this.pending = null; this.state.following = true; }
    }
    if (following) {
      const range = this.rangeAround(units.length - 1);
      this.reconcile(range.lo, range.hi, changedUnit, false, null);
      this.convergeBottom();
    } else {
      const anchorIndex = anchor ? units.findIndex(unit => unit.key === anchor.key) : -1;
      const range = anchorIndex >= 0 ? this.rangeAround(anchorIndex) : this.rangeForScroll();
      this.reconcile(range.lo, range.hi, changedUnit, false, anchor);
    }
  }

  rebuildPrefix() {
    this.prefix = [0];
    for (const unit of this.units) this.prefix.push(this.prefix.at(-1) + (this.state.heights.get(unit.key) || ESTIMATE));
  }

  indexAt(y) {
    if (!this.units.length) return 0;
    let lo = 0, hi = this.units.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this.prefix[mid + 1] > y) hi = mid;
      else lo = mid + 1;
    }
    return Math.min(lo, this.units.length - 1);
  }

  rangeForScroll() {
    const top = Math.max(0, this.scroller.scrollTop - OVERSCAN);
    const bottom = this.scroller.scrollTop + this.scroller.clientHeight + OVERSCAN;
    return { lo: this.indexAt(top), hi: Math.min(this.units.length, this.indexAt(bottom) + 1) };
  }

  rangeAround(index) {
    let lo = index, hi = index + 1, above = OVERSCAN, below = this.scroller.clientHeight + OVERSCAN;
    while (lo > 0 && above > 0) { lo--; above -= this.state.heights.get(this.units[lo].key) || ESTIMATE; }
    while (hi < this.units.length && below > 0) { below -= this.state.heights.get(this.units[hi].key) || ESTIMATE; hi++; }
    return { lo, hi };
  }

  captureDomAnchor() {
    const viewportTop = this.scroller.getBoundingClientRect().top;
    for (const child of this.window.children) {
      const rect = child.getBoundingClientRect();
      if (rect.bottom > viewportTop + 1) return { key: child.dataset.unitKey, top: rect.top - viewportTop };
    }
    return null;
  }

  restoreDomAnchor(anchor) {
    if (!anchor) return;
    const target = [...this.window.children].find(child => child.dataset.unitKey === anchor.key);
    if (!target) return;
    const viewportTop = this.scroller.getBoundingClientRect().top;
    const delta = target.getBoundingClientRect().top - viewportTop - anchor.top;
    if (Math.abs(delta) > 1) this.scroller.scrollTop += delta;
  }

  outerHeight(element) {
    const style = getComputedStyle(element);
    return element.getBoundingClientRect().height + (parseFloat(style.marginTop) || 0) + (parseFloat(style.marginBottom) || 0);
  }

  measureMounted(anchor = this.state.following ? null : this.captureDomAnchor()) {
    let changed = false;
    for (const element of this.window.children) {
      const index = Number(element.dataset.unitIndex);
      const unit = this.units[index];
      const height = this.outerHeight(element);
      if (unit && height > 1 && Math.abs((this.state.heights.get(unit.key) || ESTIMATE) - height) > 0.5) {
        this.state.heights.set(unit.key, height);
        changed = true;
      }
    }
    if (!changed) return false;
    this.rebuildPrefix();
    this.updatePads();
    if (!this.state.following) this.restoreDomAnchor(anchor);
    return true;
  }

  scheduleMeasure() {
    if (this.pendingMeasure) return;
    this.pendingMeasure = true;
    setTimeout(() => {
      this.pendingMeasure = false;
      const changed = this.measureMounted();
      if (changed && this.state.following) this.convergeBottom();
    }, 0);
  }

  updatePads() {
    this.top.style.height = `${this.prefix[this.lo] || 0}px`;
    this.bottom.style.height = `${Math.max(0, this.prefix.at(-1) - (this.prefix[this.hi] || 0))}px`;
  }

  clearWindow() {
    this.observer.disconnect();
    this.window.replaceChildren();
    this.lo = this.hi = 0;
    this.top.style.height = "0px";
    this.bottom.style.height = "0px";
  }

  reconcile(lo, hi, dirtyFrom = Infinity, refresh = false, anchor = this.state.following ? null : this.captureDomAnchor()) {
    lo = Math.max(0, Math.min(lo, this.units.length));
    hi = Math.max(lo, Math.min(hi, this.units.length));
    if (!refresh && dirtyFrom === Infinity && lo === this.lo && hi === this.hi) return false;

    this.observer.disconnect();
    for (const child of [...this.window.children]) {
      const index = Number(child.dataset.unitIndex);
      if (index < lo || index >= hi) child.remove();
    }

    let cursor = this.window.firstElementChild;
    for (let index = lo; index < hi; index++) {
      const unit = this.units[index];
      while (cursor && Number(cursor.dataset.unitIndex) < index) {
        const stale = cursor;
        cursor = cursor.nextElementSibling;
        stale.remove();
      }
      const reusable = cursor && Number(cursor.dataset.unitIndex) === index && cursor.dataset.unitKey === unit.key && index < dirtyFrom && !refresh;
      if (reusable) {
        cursor = cursor.nextElementSibling;
        continue;
      }
      const fresh = renderUnit(unit, this.state);
      fresh.dataset.unitIndex = index;
      if (cursor && Number(cursor.dataset.unitIndex) === index) {
        const next = cursor.nextElementSibling;
        cursor.replaceWith(fresh);
        cursor = next;
      } else {
        this.window.insertBefore(fresh, cursor);
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
    for (const child of this.window.children) this.observer.observe(child);
    this.actions.afterRender?.();
    return true;
  }

  updateWindow(forceIndex = null) {
    if (!this.units.length) return;
    const anchor = this.state.following ? null : this.captureDomAnchor();
    const anchorIndex = forceIndex == null && anchor ? this.units.findIndex(unit => unit.key === anchor.key) : -1;
    const range = forceIndex != null ? this.rangeAround(forceIndex) : anchorIndex >= 0 ? this.rangeAround(anchorIndex) : this.rangeForScroll();
    this.reconcile(range.lo, range.hi, Infinity, false, anchor);
  }

  // Public rerender for fold/filter changes: only the mounted window is rebuilt and its
  // first visible unit is restored to the same pixel offset.
  render(forceIndex = null) {
    if (!this.units.length) return;
    if (forceIndex != null) {
      const range = this.rangeAround(forceIndex);
      this.reconcile(range.lo, range.hi, 0, false, this.state.following ? null : this.captureDomAnchor());
      return;
    }
    this.reconcile(this.lo, this.hi, Infinity, true, this.state.following ? null : this.captureDomAnchor());
    if (this.state.following) this.convergeBottom();
  }

  gapToBottom() {
    return this.scroller.scrollHeight - this.scroller.clientHeight - this.scroller.scrollTop;
  }

  onScroll() {
    const user = performance.now() - this.lastUserInput < USER_INTENT_MS;
    if (user) {
      const following = this.gapToBottom() <= ACQUIRE_SLACK;
      if (following !== this.state.following) {
        this.state.following = following;
        if (following) this.state.newRecords = 0;
        this.actions.followChanged?.();
      }
      this.scheduleRemember();
    } else if (this.state.following && this.gapToBottom() > HOLD_SLACK) {
      this.convergeBottom();
    }
    this.actions.afterScroll?.();
    if (this.pendingScroll) return;
    this.pendingScroll = true;
    setTimeout(() => {
      this.pendingScroll = false;
      this.updateWindow();
    }, 0);
  }

  convergeBottom() {
    clearTimeout(this.bottomTimer);
    const settle = pass => {
      if (!this.state.following || !this.units.length) return;
      const range = this.rangeAround(this.units.length - 1);
      this.reconcile(range.lo, range.hi, Infinity, false, null);
      this.scroller.scrollTop = this.scroller.scrollHeight;
      this.measureMounted(null);
      if (this.gapToBottom() > 1 && pass < 7) this.bottomTimer = setTimeout(() => settle(pass + 1), 0);
    };
    settle(0);
  }

  toBottom() {
    this.state.following = true;
    this.state.newRecords = 0;
    this.actions.followChanged?.();
    this.convergeBottom();
    this.remember();
  }

  jumpToRecord(recordIndex, reveal = "record") {
    const index = this.units.findIndex(unit => recordIndex >= unit.from && recordIndex <= unit.to);
    if (index < 0) return false;
    this.lastUserInput = performance.now();
    this.state.following = false;
    const unit = this.units[index];
    // Outline navigation reveals one deliberate level of context. A turn opens its following
    // process list (including progressive items), while a task/agent/search hit additionally
    // opens the exact execution block. These are monotonic opens: an existing Expand all state
    // is never toggled back closed by navigation.
    revealNavigationContext(this.units, index, this.state, recordIndex, reveal);
    const range = this.rangeAround(index);
    this.reconcile(range.lo, range.hi, index, false, null);
    for (let pass = 0; pass < 3; pass++) {
      const target = this.window.querySelector(`[data-block-index="${recordIndex}"]`);
      if (!target) break;
      const top = target.getBoundingClientRect().top - this.scroller.getBoundingClientRect().top;
      if (Math.abs(top - 18) <= 2) break;
      this.scroller.scrollTop += top - 18;
      this.updateWindow(index);
    }
    this.actions.followChanged?.();
    this.window.querySelector(`[data-block-index="${recordIndex}"]`)?.classList.add("source-flash");
    this.scheduleRemember();
    return true;
  }

  remeasure() {
    const anchor = this.state.following ? null : this.captureDomAnchor();
    this.state.heights.clear();
    this.rebuildPrefix();
    const anchorIndex = anchor ? this.units.findIndex(unit => unit.key === anchor.key) : -1;
    const range = anchorIndex >= 0 ? this.rangeAround(anchorIndex) : this.rangeForScroll();
    this.reconcile(range.lo, range.hi, 0, false, anchor);
    if (this.state.following) this.convergeBottom();
  }

  showEmpty(title, detail, error = false) {
    this.units = [];
    this.rebuildPrefix();
    this.clearWindow();
    this.empty.hidden = false;
    this.empty.classList.toggle("monitor-error", error);
    this.empty.replaceChildren();
    const strong = document.createElement("strong");
    strong.textContent = title;
    const span = document.createElement("span");
    span.textContent = detail;
    this.empty.append(strong, span);
  }
}
