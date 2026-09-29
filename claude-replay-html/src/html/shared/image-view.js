// shared: image-view — zoom and pan for an image shown over the page, one implementation for
// every viewer that shows one (#228).
//
// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
//
// The owner, on the enlarged view: "allow me to have zoom control and move control (when the
// image is too big to fit). By default, zoom to fit on screen."
//
// FIT IS A FLOOR, NOT A SCALE. `fit` is the scale at which the whole image is visible, capped
// at 1 — a 40×40 icon opens at 40×40, not blown up to fill the stage, because upscaling an
// image nobody asked to magnify is a worse default than whitespace. Zooming BELOW fit is
// allowed down to a fraction of it, since a reader who has zoomed in wants the way back out to
// be continuous rather than snapping.
//
// PAN ONLY WHEN THERE IS SOMEWHERE TO GO. The grab cursor, the drag handler and the arrow keys
// are live only while the scaled image exceeds the stage on that axis; an image that fits is
// not draggable, so a click on it still reaches whatever the viewer put underneath. Each axis
// is decided on its own — a tall screenshot zoomed to the stage's width pans vertically and is
// pinned horizontally.
//
// The viewer owns opening, closing and the Escape key. This module owns only what happens to
// the image between those, and `destroy()` puts every listener back.

const MAX_SCALE = 8;
/** How far below fit a reader may zoom out. Not 1:1 with fit, so leaving a zoomed state feels
 *  continuous instead of hitting a wall the moment the whole image is visible again. */
const MIN_FIT_FRACTION = 0.2;

function clamp(value, low, high) {
  return value < low ? low : value > high ? high : value;
}

/**
 * Attach zoom and pan to one `<img>` inside a stage element.
 *
 * `stage` is the box the image is shown in and the surface gestures are read from; `img` is the
 * image itself, which this module positions with a transform and never re-lays-out. The image
 * may have no natural size yet — `sync()` is safe to call before load and is wired to the
 * image's own `load` event, so a viewer can attach first and set `src` after.
 *
 * Returns the handle the viewer drives: `zoomBy`, `zoomTo`, `fit` (back to the default),
 * `actual` (1:1), `sync` (re-fit after the stage resizes) and `destroy`.
 * `onChange(state)` fires whenever the scale changes, for a viewer that shows a percentage.
 */
function createImageView(stage, img, options = {}) {
  const onChange = options.onChange || (() => {});
  // Whether the viewer is on screen right now. A page can hold a viewer that is not
  // showing — the app shell builds its lightbox at load and its preview view keeps one
  // between images — and a viewer that cannot be seen must not own the document's keys.
  const isActive = options.isActive || (() => true);
  const state = { scale: 1, fit: 1, x: 0, y: 0, dragging: false };

  /** The scale at which the whole image is visible, never magnifying past 1:1. */
  function fitScale() {
    const box = viewport();
    const w = img.naturalWidth, h = img.naturalHeight;
    if (!w || !h || !box.width || !box.height) return 1;
    return Math.min(1, Math.min(box.width / w, box.height / h));
  }

  /** How far the image may travel on each axis: half its overflow, or nothing when it fits. */
  /** The visible box is what `overflow:hidden` clips at — the PADDING box, which is what
   *  `clientWidth`/`clientHeight` report. The stage carries padding (18px in the app shell), so
   *  measuring the border box would let the image travel that much too far on each side. */
  function viewport() {
    return { width: stage.clientWidth, height: stage.clientHeight };
  }

  function bounds() {
    const box = viewport();
    const w = img.naturalWidth * state.scale, h = img.naturalHeight * state.scale;
    return { x: Math.max(0, (w - box.width) / 2), y: Math.max(0, (h - box.height) / 2) };
  }

  function apply() {
    const limit = bounds();
    state.x = clamp(state.x, -limit.x, limit.x);
    state.y = clamp(state.y, -limit.y, limit.y);
    // The image is anchored at the stage's CENTRE (`position:absolute; left:50%; top:50%`) and
    // pulled back by half its own size, so `translate(0,0)` is exactly centred whatever the image
    // measures. The layout is not asked to centre anything, which is the whole point: a centred
    // grid/flex item LARGER than its box hits the unsafe-alignment fallback and is aligned to
    // START instead, so a big image laid out top-left while this code believed it was centred.
    // Fit looked off-centre because it was, and panning clamped to bounds measured from a centre
    // the image never occupied. Owning the position outright removes the assumption.
    img.style.transform = `translate(-50%, -50%) translate(${state.x}px, ${state.y}px) scale(${state.scale})`;
    const movable = limit.x > 0.5 || limit.y > 0.5;
    stage.dataset.pannable = movable ? "yes" : "no";
    stage.dataset.zoom = String(Math.round(state.scale * 100));
    if (!movable) stage.classList.remove("dragging");
    onChange({ scale: state.scale, fit: state.fit, percent: Math.round(state.scale * 100), pannable: movable });
  }

  /** Zoom about a point in stage coordinates, so what is under the cursor stays under it. */
  function zoomAt(next, clientX, clientY) {
    const rect = stage.getBoundingClientRect();
    const scale = clamp(next, state.fit * MIN_FIT_FRACTION, MAX_SCALE);
    if (scale === state.scale) return;
    // The cursor, in stage coordinates measured from the stage's centre — the same origin the
    // transform uses, so "keep what is under the pointer under the pointer" is exact rather than
    // approximately right. `clientLeft`/`clientTop` step over any border; the padding box is what
    // the image is centred in.
    const originX = rect.left + stage.clientLeft + stage.clientWidth / 2;
    const originY = rect.top + stage.clientTop + stage.clientHeight / 2;
    const cx = (clientX == null ? originX : clientX) - originX;
    const cy = (clientY == null ? originY : clientY) - originY;
    const ratio = scale / state.scale;
    // The point under the cursor is (c - offset)/scale in image space; hold it still.
    state.x = cx - (cx - state.x) * ratio;
    state.y = cy - (cy - state.y) * ratio;
    state.scale = scale;
    apply();
  }

  function toFit() {
    state.fit = fitScale();
    state.scale = state.fit;
    state.x = 0;
    state.y = 0;
    apply();
  }

  function onWheel(event) {
    // Ctrl/⌘+wheel is the pinch gesture a trackpad sends; a plain wheel over an image the
    // reader has magnified is also a zoom, because there is nothing else it could scroll here.
    event.preventDefault();
    const step = Math.exp(-event.deltaY / 320);
    zoomAt(state.scale * step, event.clientX, event.clientY);
  }

  // Every pointer down on the stage, by id — one is a pan (where there is room), two are a PINCH
  // (#313: a phone has no trackpad, so the ctrl+wheel a Mac's pinch sends never comes, and the
  // stage's `touch-action:none` keeps the browser's own pinch off it; a viewer that tracked one
  // pointer could not be zoomed on a phone at all).
  const pointers = new Map();
  let pointer = null;
  let pinch = null;
  let lastTap = null;
  const spread = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);
  function onPointerDown(event) {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    // A new interaction's first pointer: whatever an earlier one left (released off the stage,
    // uncaptured) is gone, or it would pair with this finger into a pinch nobody is making.
    if (event.isPrimary) { pointers.clear(); pinch = null; }
    pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    if (pointers.size === 2) {
      // A second finger makes it a pinch, at any scale — zooming IN from fit is what a pinch is for.
      const [a, b] = [...pointers.values()];
      pinch = { d0: spread(a, b) || 1, s0: state.scale, mx: (a.x + b.x) / 2, my: (a.y + b.y) / 2, moved: false };
      pointer = null;
      for (const id of pointers.keys()) { try { stage.setPointerCapture(id); } catch (_) {} }
      stage.classList.add("dragging");
      event.preventDefault();
      return;
    }
    if (pointers.size > 2 || stage.dataset.pannable !== "yes") return;
    pointer = { id: event.pointerId, x: event.clientX, y: event.clientY, moved: false };
    stage.classList.add("dragging");
    try { stage.setPointerCapture(event.pointerId); } catch (_) {}
    event.preventDefault();
  }
  function onPointerMove(event) {
    const held = pointers.get(event.pointerId);
    if (held) { held.x = event.clientX; held.y = event.clientY; }
    if (pinch && pointers.size >= 2) {
      const [a, b] = [...pointers.values()];
      const mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2;
      // The midpoint moving is a two-finger pan; the spread changing is the zoom about it.
      state.x += mx - pinch.mx;
      state.y += my - pinch.my;
      pinch.mx = mx;
      pinch.my = my;
      pinch.moved = true;
      zoomAt(pinch.s0 * spread(a, b) / pinch.d0, mx, my);
      apply();
      return;
    }
    if (!pointer || event.pointerId !== pointer.id) return;
    state.x += event.clientX - pointer.x;
    state.y += event.clientY - pointer.y;
    pointer.x = event.clientX;
    pointer.y = event.clientY;
    pointer.moved = true;
    apply();
  }
  function onPointerUp(event) {
    const had = pointers.get(event.pointerId);
    pointers.delete(event.pointerId);
    if (pinch) {
      try { stage.releasePointerCapture(event.pointerId); } catch (_) {}
      if (pointers.size < 2) {
        const moved = pinch.moved;
        pinch = null;
        lastTap = null;
        stage.classList.remove("dragging");
        if (moved) stage.addEventListener("click", swallow, { capture: true, once: true });
        // The finger still down carries on as a pan, where there is somewhere to go.
        const rest = [...pointers.entries()][0];
        if (rest && stage.dataset.pannable === "yes") {
          pointer = { id: rest[0], x: rest[1].x, y: rest[1].y, moved: true };
          stage.classList.add("dragging");
        }
      }
      return;
    }
    let moved = false;
    if (pointer && event.pointerId === pointer.id) {
      // A drag that moved is not a click: swallow the click that follows it, or a viewer whose
      // backdrop closes on click would close the moment the reader releases the image.
      moved = pointer.moved;
      pointer = null;
      stage.classList.remove("dragging");
      try { stage.releasePointerCapture(event.pointerId); } catch (_) {}
      if (moved) stage.addEventListener("click", swallow, { capture: true, once: true });
    }
    if (event.pointerType === "touch" && had && !moved) doubleTap(event);
  }
  /** A touch screen's double click: two taps close in time and place. A browser that honours
   *  `touch-action:none` sends no `dblclick` for them, so the gesture is read here. */
  function doubleTap(event) {
    const now = event.timeStamp || performance.now();
    if (lastTap && now - lastTap.t < 320 && Math.hypot(event.clientX - lastTap.x, event.clientY - lastTap.y) < 32) {
      lastTap = null;
      if (state.scale > state.fit + 0.001) toFit();
      else zoomAt(Math.max(1, state.fit * 2.5), event.clientX, event.clientY);
      return;
    }
    lastTap = { t: now, x: event.clientX, y: event.clientY };
  }
  /** Eat the click that ends a drag — but never a click on a CONTROL.
   *
   *  The swallower exists so releasing a dragged image does not read as a click on the backdrop
   *  and close the viewer. It sits on the stage, and the zoom bar is inside the stage, so a
   *  blanket version also ate the first press of Fit / + / − after any drag: the reader dragged,
   *  reached for Fit, and nothing happened. Controls are exactly the clicks that were meant. */
  function swallow(event) {
    if (event.target.closest("button, a, [data-zoom]")) return;
    event.stopPropagation();
    event.preventDefault();
  }

  function onDoubleClick(event) {
    event.preventDefault();
    event.stopPropagation();
    if (state.scale > state.fit + 0.001) toFit();
    else zoomAt(1, event.clientX, event.clientY);
  }

  function onKey(event) {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    // Not on screen → not ours. See `isActive`.
    if (!isActive()) return;
    // Typing is typing. The keys this viewer claims (`0` `1` `-` `_` `+` `=`, the arrows) are
    // also ordinary characters, so it must yield while the focus is in a text control — the same
    // rule keymap.js states, repeated here because the inliner forbids one shared module from
    // importing another. Without it, a page that had ever built a lightbox swallowed those keys
    // from every INPUT/TEXTAREA on it — the compose box, the passcode field, every search box.
    const t = event.target;
    if (t && (/^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName || "") || t.isContentEditable === true)) {
      return;
    }
    const pan = event.shiftKey ? 120 : 48;
    if (event.key === "+" || event.key === "=") { zoomAt(state.scale * 1.25); }
    else if (event.key === "-" || event.key === "_") { zoomAt(state.scale / 1.25); }
    else if (event.key === "0") { toFit(); }
    else if (event.key === "1") { zoomAt(1); }
    else if (event.key === "ArrowLeft" && stage.dataset.pannable === "yes") { state.x += pan; apply(); }
    else if (event.key === "ArrowRight" && stage.dataset.pannable === "yes") { state.x -= pan; apply(); }
    else if (event.key === "ArrowUp" && stage.dataset.pannable === "yes") { state.y += pan; apply(); }
    else if (event.key === "ArrowDown" && stage.dataset.pannable === "yes") { state.y -= pan; apply(); }
    else return;
    event.preventDefault();
  }

  const onLoad = () => toFit();
  stage.addEventListener("wheel", onWheel, { passive: false });
  stage.addEventListener("pointerdown", onPointerDown);
  stage.addEventListener("pointermove", onPointerMove);
  stage.addEventListener("pointerup", onPointerUp);
  stage.addEventListener("pointercancel", onPointerUp);
  stage.addEventListener("dblclick", onDoubleClick);
  img.addEventListener("load", onLoad);
  // The keys belong to the document while a viewer is open: the stage is not focusable, and the
  // reader's hands are not necessarily on it.
  const keyHost = options.keyHost || document;
  keyHost.addEventListener("keydown", onKey);

  if (img.complete && img.naturalWidth) toFit(); else apply();

  return {
    fit: toFit,
    actual: () => zoomAt(1),
    zoomBy: factor => zoomAt(state.scale * factor),
    zoomTo: scale => zoomAt(scale),
    sync: () => {
      // The stage changed size (a window resize, a viewer that opens at a different shape).
      // A reader sitting at the default keeps the default; one who has zoomed keeps their scale
      // and only has their pan re-clamped, because re-fitting under them would undo their work.
      const before = state.fit;
      state.fit = fitScale();
      if (Math.abs(state.scale - before) < 0.001) state.scale = state.fit;
      apply();
    },
    state: () => ({ scale: state.scale, fit: state.fit, percent: Math.round(state.scale * 100) }),
    destroy: () => {
      stage.removeEventListener("wheel", onWheel);
      stage.removeEventListener("pointerdown", onPointerDown);
      stage.removeEventListener("pointermove", onPointerMove);
      stage.removeEventListener("pointerup", onPointerUp);
      stage.removeEventListener("pointercancel", onPointerUp);
      stage.removeEventListener("dblclick", onDoubleClick);
      img.removeEventListener("load", onLoad);
      keyHost.removeEventListener("keydown", onKey);
      stage.removeEventListener("click", swallow, { capture: true });
      img.style.transform = "";
      delete stage.dataset.pannable;
      delete stage.dataset.zoom;
      stage.classList.remove("dragging");
    },
  };
}

export { createImageView };
