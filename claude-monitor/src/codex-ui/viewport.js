import { renderUnit } from "./components.js";
// The window's arithmetic — the sums, the search, the ranges, the pads, the anchor correction,
// the follow rule — is the shared module's (#107, html/shared/virtual-window.js): one set of
// scroll rules for both pages. What reads layout and what writes the DOM stays here.
import { VirtualWindow, elementFrame } from "./shared/virtual-window.js";

export function revealNavigationContext(units, index, state, recordIndex, reveal = "record") {
  const unit = units[index];
  if (!unit) return;
  if (unit.type === "user" && reveal !== "turn") state.promptExpanded.add(unit.key);
  if (unit.type === "process") {
    state.processFolds.set(unit.key, false);
    state.processExpanded.add(unit.key);
    const target = unit.views.find(item => item.index === recordIndex)?.view;
    if (target?.id && target.t !== "assistant") state.folds.set(target.id, false);
    // A navigated-to record shows whole: every cap in it opens (#108), so a search hit or a
    // deep link behind "⋯ N more lines" is on screen, as the classic page's revealMark does —
    // and for a search or a deep link the records NESTED in it open too, folds and caps (#100):
    // a hit on line 55 of a Read inside an activity is inside a closed row with a closed cap.
    if (target?.id && state.capOpen) state.capOpen.add(`${target.id}:*`);
    if (reveal === "search" || reveal === "hash") {
      const openAll = view => { for (const child of view?.children || []) { if (child?.id) { state.folds.set(child.id, false); state.capOpen?.add(`${child.id}:*`); } openAll(child); } };
      openAll(target);
    }
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
import { applyViewChoices, parseViewMemory, serializeViewMemory, viewChoices, viewMemoryKey } from "./view-memory.js";

// Rule 5 (#107 step 3): estimate UNDER, never over. An unmeasured unit's height is a guess, and
// the guess is wrong in one of two directions. Guess LOW and learning the real height only ever
// grows the page BELOW the reader, which nobody feels; guess HIGH and learning it SHRINKS the
// page, and a shrink above the viewport is a jump unless the anchor catches it. This shell
// guessed 132px for everything — above most prompts and every one-line assistant note — which is
// the wrong side. The guesses below are floors: a prompt is at least one line in its card, an
// assistant note the same, a process at least its head row. The classic page's own floor is 30.
const ESTIMATES = { user: 44, assistant: 40, process: 34 };
const ESTIMATE = 34;
const REMEMBER_MS = 250;
const OVERSCAN = 1500;
const HOLD_SLACK = 80;
const ACQUIRE_SLACK = 2;
const USER_INTENT_MS = 320;

export class Viewport extends VirtualWindow {
  constructor(scroller, inner, state, actions) {
    const top = document.createElement("div");
    top.className = "virtual-pad";
    const mounted = document.createElement("div");
    mounted.className = "virtual-window";
    const bottom = document.createElement("div");
    bottom.className = "virtual-pad";
    const empty = document.createElement("div");
    empty.className = "monitor-empty";
    empty.hidden = true;
    inner.replaceChildren(top, mounted, bottom, empty);
    // The engine is the shared one (#107, html/shared/virtual-window.js): the window and its
    // pads, the anchor, the observers, the follow state, the thumb, the converge, the remember
    // debounce. What this shell brings is what a unit IS, how one renders, and where the
    // reader's choices are kept.
    super({
      frame: elementFrame(scroller),
      mount: { top, window: mounted, bottom, content: inner },
      overscan: OVERSCAN,
      // Rule 7's hysteresis (#127): acquiring the pin needs the true end, KEEPING it only the
      // old slack. Held at the true end too, a nudge of three pixels dropped the tail — which
      // is what this shell did, where the classic page has held at 80 since #103.
      slacks: { acquire: ACQUIRE_SLACK, hold: HOLD_SLACK, heal: HOLD_SLACK },
      userIntentMs: USER_INTENT_MS,
      rememberMs: REMEMBER_MS,
    });
    this.scroller = scroller;
    this.inner = inner;
    this.state = state;
    this.actions = actions;
    this.units = [];
    this.top = top;
    this.window = mounted;
    this.bottom = bottom;
    this.empty = empty;
    // Per-session position memory (parity #6): the session this viewport is showing, the
    // remembered anchor still to be applied once its unit has streamed in, and a debounce.
    this.session = "";
    this.pending = null;
    this.pendingTries = 0;
    addEventListener("pagehide", () => this.remember());
  }

  // ── what a unit is, for the engine ──────────────────────────────────────
  get count() { return this.units.length; }
  get following() { return this.state.following; }
  set following(value) {
    this.state.following = value;
    if (value) this.state.newRecords = 0;
  }
  identityAt(index) { return this.units[index]?.key; }
  estimateAt(index) { return ESTIMATES[this.units[index]?.type] || ESTIMATE; }
  heightFor(index) { const unit = this.units[index]; return unit ? this.state.heights.get(unit.key) : 0; }
  setHeight(index, height) { this.state.heights.set(this.units[index].key, height); }
  clearHeights() { this.state.heights.clear(); }
  renderItem(index) { return renderUnit(this.units[index], this.state); }
  afterRender() { this.actions.afterRender?.(); }
  afterScroll() { this.actions.afterScroll?.(); }
  followChanged() { this.actions.followChanged?.(); }

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
    // The reader's choices come back with the first batch (#114): a fold they opened, a turn
    // they read raw, a cap they expanded, an image they showed. Applied in `setUnits`, after the
    // record store's reset has cleared the state for the new session — not here, before it.
    this.pendingView = memory?.view || null;
  }

  /** Remember the current position for this session: following, or the anchor. */
  remember() {
    clearTimeout(this.rememberTimer);
    if (!this.session || this.pending) return;
    const value = this.state.following ? { following: true } : this.captureDomAnchor();
    if (!value) return;
    const view = viewChoices(this.state);
    try { sessionStorage.setItem(viewMemoryKey(this.session), serializeViewMemory(value.following ? { following: true, view } : { following: false, key: value.key, top: value.top, view })); } catch (_) {}
  }

  setUnits(units, changedUnit = 0) {
    this.empty.hidden = true;
    if (this.pendingView) { applyViewChoices(this.state, this.pendingView); this.pendingView = null; }
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
    this.syncAnchor();
    this.scheduleRemember();
    return true;
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
