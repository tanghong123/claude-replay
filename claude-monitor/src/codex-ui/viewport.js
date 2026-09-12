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

// Rule 5 (#107 step 3), as #184 amends it: estimate CLOSE, and let the floor stand only while
// there is nothing to learn from. The numbers below are still floors — a prompt is at least one
// line in its card, an assistant note the same, a process at least its head row — but a floor is
// now the SEED of a running mean (`HeightGuess`, one per kind, kept by the engine since #196
// stage 3), not the answer. The original rule said
// estimate UNDER because learning a real height then only grows the page BELOW the reader; what
// it missed is that a floor MAXIMISES the gap between guess and truth, and that gap is exactly
// what displaces a reader when a run mounted above them is measured (#180 measured 2355px and
// 2854px of drift here for a 900px request). The shell before #107 guessed 132px for everything,
// which was the wrong side of the old rule and the wrong distance from this one.
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
      // #184: the floors seed one running mean per unit type; a unit of a type with no floor
      // learns as a process. The means, and what each unit taught them, are the engine's (#196
      // stage 3) — on its instance, never in `state`: a persisted share against a fresh mean would
      // withdraw what was never learned.
      floors: ESTIMATES,
      defaultKind: "process",
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
  /** One mean per unit TYPE (#184): a prompt card, an assistant note and a process group are
   *  three populations with three different shapes, and one mean over all of them would be wrong
   *  about each. The shell names the kind; the engine keeps the means (#196 stage 3). */
  kindOf(index) { return this.units[index]?.type; }
  // Where the measured heights live — per session, in `state` — and nothing else: the engine
  // learns from every measure before it hands the height over.
  heightFor(index) { const unit = this.units[index]; return unit ? this.state.heights.get(unit.key) : 0; }
  setHeight(index, height) { this.state.heights.set(this.units[index].key, height); }
  clearHeights() { this.state.heights.clear(); }
  /** #132 step 4: the same heights, re-guessed for a new measure. A text block's height moves
   *  roughly with the inverse of its width, and rule 5 still holds — an estimate is a FLOOR, so
   *  a widen that scales a height down may not take it under this shell's own floor. The engine
   *  re-guesses what it has LEARNED by the same ratio (`scaleGuesses`, #184). */
  scaleHeights(ratio) {
    for (const [key, height] of this.state.heights) this.state.heights.set(key, Math.max(ESTIMATE, height * ratio));
  }
  renderItem(index) { return this.stampNeighbours(renderUnit(this.units[index], this.state), index); }

  /** What the cascade may read of a unit's NEIGHBOURS comes from the model, not from what happens
   *  to be mounted (#201). A rule keyed on a mounted sibling — the demo's
   *  `.process-surface + .turn.assistant{padding-top:4px}`, production's own
   *  `*:has(+ .process-surface){margin-bottom:8px}` — made a unit at the window's edge measure
   *  11px differently from the same unit with its neighbour mounted, and the engine's sums assume a
   *  height is a property of the unit. `production.css` carries each such rule again, keyed on
   *  these stamps, so the edge unit is padded as it will be once its neighbour arrives. */
  stampNeighbours(root, index) {
    const prev = this.units[index - 1], next = this.units[index + 1];
    if (prev) root.dataset.prevType = prev.type; else delete root.dataset.prevType;
    if (next) root.dataset.nextType = next.type; else delete root.dataset.nextType;
    return root;
  }
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

  /** The store's update (framework §4.11): one records transaction whose mutation is this
   *  shell's — the units swapped in, and the estimator told which measured keys are gone (the
   *  heights are this shell's, so it says). A remembered position, once its unit has streamed in,
   *  is the command it already was: the swap first — `jumpTo` checks the index against the count
   *  — and no mount before it, so the estimator learns nothing a restore never taught it. */
  setUnits(units, changedUnit = 0) {
    this.empty.hidden = true;
    if (this.pendingView) { applyViewChoices(this.state, this.pendingView); this.pendingView = null; }
    const swap = () => {
      const nextKeys = new Set(units.map(unit => unit.key));
      for (const key of this.state.heights.keys()) if (!nextKeys.has(key)) { this.state.heights.delete(key); this.forget(key); }
      // A rewritten unit KEEPS its last height as the provisional value until the measure in
      // this same task replaces it (#194). Dropping it to the estimate made the sums short by the
      // whole open turn for the length of one render; `afterMount`/`measureMounted` force layout
      // inside that gap, the browser clamps `scrollTop` to the shorter page (#179), and the
      // reader is pulled up by the difference — 31px on a 300px wheel with the tail growing,
      // measured. A stale real height is a far better guess than the mean, and `heightChanged`
      // corrects it on the measure; a unit outside the window keeps it until it is mounted.
      this.units = units;
      // A kept element's neighbours may have changed under it (the unit that was last has a
      // successor now; a rewrite renamed the one after it): the stamps follow the model.
      for (const child of this.window.children) this.stampNeighbours(child, Number(child.dataset.unitIndex));
      return changedUnit;
    };
    if (!units.length) {
      this.recordsChanged(swap);
      return;
    }
    if (this.pending) {
      const index = units.findIndex(unit => unit.key === this.pending.key);
      if (index >= 0) {
        const memory = this.pending;
        this.pending = null;
        swap();
        this.rebuildPrefix();
        // A session reopening where it was: a landing the engine holds through whatever settles
        // under it (framework §4.10). No stamp — a restore is not a gesture.
        this.jumpTo({ key: memory.key, index, top: memory.top }, { dirtyFrom: changedUnit });
        this.actions.followChanged?.();
        return;
      }
      // Not streamed in yet — keep waiting a few batches, then give the tail up as lost: the
      // transaction below reads `following` at its start and begins from the tail.
      if (++this.pendingTries > 12) { this.pending = null; this.state.following = true; }
    }
    this.recordsChanged(swap);
  }

  /** Where a jump LANDS its target: 18px under the scroller's top, unless something sticky sits
   *  there — the turn bar (#123) declares its own height as the scroller's `scroll-padding-top`,
   *  so a jumped-to record does not arrive behind it. The spy's line sits just below this (#199),
   *  as the classic page's does, so a turn the reader jumped to is the turn the spy names. */
  get landing() {
    return parseFloat(getComputedStyle(this.scroller).scrollPaddingTop) || 18;
  }

  jumpToRecord(recordIndex, reveal = "record") {
    const index = this.units.findIndex(unit => recordIndex >= unit.from && recordIndex <= unit.to);
    if (index < 0) return false;
    // Outline navigation reveals one deliberate level of context. A turn opens its following
    // process list (including progressive items), while a task/agent/search hit additionally
    // opens the exact execution block. These are monotonic opens: an existing Expand all state
    // is never toggled back closed by navigation.
    revealNavigationContext(this.units, index, this.state, recordIndex, reveal);
    // The jump is one transaction (framework §4.10): `P` is the record's row at the landing, the
    // window is mounted around it, one write lands it, and the engine holds the row there through
    // whatever settles under it until the reader moves — the three-pass loop this replaced saw
    // only the heights that existed when it ran. Stamped, as every jump here always was.
    this.jumpTo({ index, block: recordIndex, top: this.landing }, { dirtyFrom: index, intent: true });
    this.window.querySelector(`[data-block-index="${recordIndex}"]`)?.classList.add("source-flash");
    return true;
  }

  showEmpty(title, detail, error = false) {
    // An empty model is a records change like any other (framework §4.11): the transaction
    // mounts nothing and the pads go to zero.
    this.recordsChanged(() => { this.units = []; });
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
