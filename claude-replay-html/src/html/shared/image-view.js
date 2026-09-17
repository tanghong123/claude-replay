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
  const state = { scale: 1, fit: 1, x: 0, y: 0, dragging: false };

  /** The scale at which the whole image is visible, never magnifying past 1:1. */
  function fitScale() {
    const box = stage.getBoundingClientRect();
    const w = img.naturalWidth, h = img.naturalHeight;
    if (!w || !h || !box.width || !box.height) return 1;
    return Math.min(1, Math.min(box.width / w, box.height / h));
  }

  /** How far the image may travel on each axis: half its overflow, or nothing when it fits. */
  function bounds() {
    const box = stage.getBoundingClientRect();
    const w = img.naturalWidth * state.scale, h = img.naturalHeight * state.scale;
    return { x: Math.max(0, (w - box.width) / 2), y: Math.max(0, (h - box.height) / 2) };
  }

  function apply() {
    const limit = bounds();
    state.x = clamp(state.x, -limit.x, limit.x);
    state.y = clamp(state.y, -limit.y, limit.y);
    img.style.transform = `translate(${state.x}px, ${state.y}px) scale(${state.scale})`;
    const movable = limit.x > 0.5 || limit.y > 0.5;
    stage.dataset.pannable = movable ? "yes" : "no";
    stage.dataset.zoom = String(Math.round(state.scale * 100));
    if (!movable) stage.classList.remove("dragging");
    onChange({ scale: state.scale, fit: state.fit, percent: Math.round(state.scale * 100), pannable: movable });
  }

  /** Zoom about a point in stage coordinates, so what is under the cursor stays under it. */
  function zoomAt(next, clientX, clientY) {
    const box = stage.getBoundingClientRect();
    const scale = clamp(next, state.fit * MIN_FIT_FRACTION, MAX_SCALE);
    if (scale === state.scale) return;
    const cx = (clientX == null ? box.left + box.width / 2 : clientX) - box.left - box.width / 2;
    const cy = (clientY == null ? box.top + box.height / 2 : clientY) - box.top - box.height / 2;
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

  let pointer = null;
  function onPointerDown(event) {
    if (stage.dataset.pannable !== "yes" || event.button !== 0) return;
    pointer = { id: event.pointerId, x: event.clientX, y: event.clientY, moved: false };
    stage.classList.add("dragging");
    try { stage.setPointerCapture(event.pointerId); } catch (_) {}
    event.preventDefault();
  }
  function onPointerMove(event) {
    if (!pointer || event.pointerId !== pointer.id) return;
    state.x += event.clientX - pointer.x;
    state.y += event.clientY - pointer.y;
    pointer.x = event.clientX;
    pointer.y = event.clientY;
    pointer.moved = true;
    apply();
  }
  function onPointerUp(event) {
    if (!pointer || event.pointerId !== pointer.id) return;
    // A drag that moved is not a click: swallow the click that follows it, or a viewer whose
    // backdrop closes on click would close the moment the reader releases the image.
    const moved = pointer.moved;
    pointer = null;
    stage.classList.remove("dragging");
    try { stage.releasePointerCapture(event.pointerId); } catch (_) {}
    if (moved) stage.addEventListener("click", swallow, { capture: true, once: true });
  }
  function swallow(event) { event.stopPropagation(); event.preventDefault(); }

  function onDoubleClick(event) {
    event.preventDefault();
    event.stopPropagation();
    if (state.scale > state.fit + 0.001) toFit();
    else zoomAt(1, event.clientX, event.clientY);
  }

  function onKey(event) {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
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
      img.style.transform = "";
      delete stage.dataset.pannable;
      delete stage.dataset.zoom;
      stage.classList.remove("dragging");
    },
  };
}

export { createImageView };
