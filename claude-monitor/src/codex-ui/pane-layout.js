// The session view's one layout decision (#s431 stage 2, design/pane-layout.md, "Its own size, and
// touch" and "One decision, read by styles and scripts"). The view is laid out by its own box and the
// screen's pointer, never by the window or by what a host has beside it (the owner's ruling, #s461):
// every rule its sources wrote under a size query reads a class this module sets on the view's element
// (the derivation rewrites `(max-width:1180px)` as `pl-max-w-1180`, scripts/session-view-css.mjs), and
// the view's scripts ask `phone()` here rather than matching `PHONE_QUERY` themselves.
//
// The view is a phone (`pl-phone`) where `phoneView` says (#s431 stage 3): too narrow for the session at
// `M` beside its margin, under 490px, or a touch screen's view 760px wide or less or 500px tall or less.
// A width class reads `B`, the width the bands read: the view's own inside that family, where a
// phone's view is its screen, and 300px more outside it, since the stylesheets' numbers were windows
// with the default 300px sidebar beside the view. A height class reads the view's height; a pointer
// class mirrors the screen's, through `matchMedia`.

import { viewRoot } from "./view-root.js";

/** Every class the view's rules read, as the derivation reports them (`report.layoutClasses`): the
 *  contract holds the two to one list. */
export const LAYOUT_CLASSES = Object.freeze([
  "pl-coarse", "pl-fine", "pl-max-h-500", "pl-max-w-1180", "pl-max-w-560", "pl-max-w-620", "pl-max-w-700",
  "pl-max-w-760", "pl-max-w-900", "pl-min-h-501", "pl-min-w-1181", "pl-min-w-701", "pl-min-w-761",
  "pl-min-w-900", "pl-min-w-901",
]);

const SHAPE = /^pl-(max|min)-(w|h)-(\d+)$/;

/** Whether a class holds for a band width `width` (`B`), the view's `height` and the screen's pointer:
 *  the rule the page applies, kept pure so the contract can check it against the media queries it
 *  replaced. */
export function holds(name, { width, height, coarse, fine }) {
  if (name === "pl-coarse") return !!coarse;
  if (name === "pl-fine") return !!fine;
  const m = SHAPE.exec(name);
  if (!m) throw new Error(`pane-layout: no rule for ${name}`);
  const value = m[2] === "w" ? width : height;
  return m[1] === "max" ? value <= Number(m[3]) : value >= Number(m[3]);
}

/** The width the bands read for a view: its own in the phone family (`phoneView`, below), 300px more
 *  elsewhere. */
export const bandWidth = view => (phoneView(view) ? view.width : view.width + 300);

/** Whether that view is a wide phone held sideways (`WIDE_PHONE_QUERY` as the view reads it): a short
 *  touch view 900px wide or more. */
export const isWidePhone = view => phoneView(view) && !!view.coarse && view.height <= 500 && view.width >= 900;

/** The classes a view of that size holds, `pl-phone` among them. */
export function classesFor(view) {
  const sample = { ...view, width: bandWidth(view) };
  const on = new Set(LAYOUT_CLASSES.filter(name => holds(name, sample)));
  if (phoneView(view)) on.add("pl-phone");
  return on;
}

/** Decide now and again whenever the view's box, the screen's pointer, the window's size or what the
 *  view says of itself changes: the classes on `element`, `pl-phone` with them, and the lengths its
 *  rules read (`--pl-b`, `--pl-w`, `--pl-h`, `--pane-x`, `--pane-r`, `--pl-vh`, `--pl-dvh`). `onChange`
 *  hears every decision that changed a class, the view's width or the open pane's, after it is
 *  written: whatever changed the view's box (a host's list, a window), the view hears it here. `query(family)` answers as a
 *  `MediaQueryList` does (`matches`, a `change` event), so a script that matched `PHONE_QUERY` asks the
 *  view's family instead by one name. Everything bound here ends with `signal`.
 *
 *  #s431 stage 3: each decision also arranges the view's parts by the owner's order (`arrangeView`,
 *  below), from what `inputs()` answers at that moment (whether the pane is open, whether the reader
 *  wants the outline, the pane's share of the slack, and the room its host could give). It sets the
 *  arrangement's classes (`ARRANGE_CLASSES`) and the open column's width (`--preview-fit`), and hands
 *  the arrangement to `onArrange` while it writes, so what the view draws from it is laid out in the
 *  same decision. The view calls `decide()` again whenever one of those inputs changes.
 *
 *  A decision lays the element out; it never glides. While it writes, the element wears `pl-holding`,
 *  under which its stylesheet holds every transition inside it, and one read styles every part at what
 *  was written before the hold lets go, so letting go starts none (#s431 stage 2: the outline's track
 *  glided 36px into place while the first prompts measured, and a prompt counted its lines at the
 *  narrower column). The reader's own toggles still glide: `glide()` lets the decisions of the next
 *  `GLIDE_MS` write without the hold, since opening the pane can make the host's list give way in
 *  the same moment, and a decision taken then would cancel the pane's glide. */
export function mountPaneLayout(element, { signal, onChange, inputs, onArrange } = {}) {
  const coarse = matchMedia("(pointer:coarse)"), fine = matchMedia("(pointer:fine)");
  const events = new EventTarget();
  const family = { phone: false, wide: false };
  // `sized`: whether a decision has seen the view's box. A view mounted hidden decides once at 0×0,
  // which says nothing of its family, and `phone()` then answers for no box.
  let said = null, wrote = null, arranged = null, glideUntil = 0, sized = false;
  // Everything a decision reads of the page, read at once: the view's box, the window's width and
  // height, and the screen's pointer.
  const measure = () => ({ box: element.getBoundingClientRect(), viewport: viewRoot.viewportWidth(), height: innerHeight, coarse: coarse.matches, fine: fine.matches });
  const decideAt = ({ box, viewport, height, coarse: isCoarse, fine: isFine }) => {
    if (signal?.aborted) return;
    // A box with no size is not laid out: the host hid the view (its `hidden` attribute, #s464, or what
    // holds it). Nothing about it is the view's to decide, so the last decision stands until the box
    // comes back; decided at 0×0, the view would wear the phone's layout while hidden and again as shown.
    if (wrote !== null && !box.width && !box.height) return;
    const view = { width: box.width, height: box.height, coarse: isCoarse, fine: isFine };
    const asked = inputs?.() || {};
    const arrangement = arrangeView({ ...asked, width: box.width, height: box.height, coarse: isCoarse });
    const on = classesFor(view);
    for (const name of arrangeClasses(view, arrangement)) on.add(name);
    const right = Math.max(0, viewport - box.right);
    const tall = box.height >= height - 2;
    // Its gaps to the window's top and bottom, only where it is shorter than the window (#s431 stage
    // 4): a view as tall as the window has the window's top and bottom, and in the app it always has.
    // Read from its box there, a phone's keyboard ride (which moves the app up over iOS's scroll) would
    // move them while the reader types.
    const top = tall ? 0 : Math.max(0, box.top), bottom = tall ? 0 : Math.max(0, height - box.bottom);
    const writing = JSON.stringify([[...on].sort(), box.width, box.height, box.left, right, top, bottom, tall, arrangement]);
    if (writing === wrote) return;
    wrote = writing;
    watchMoves(box);
    arranged = arrangement;
    sized = !!(box.width || box.height);
    const was = { ...family };
    family.phone = on.has("pl-phone");
    family.wide = isWidePhone(view);
    const hold = performance.now() >= glideUntil;
    if (hold) element.classList.add("pl-holding");
    for (const name of [...LAYOUT_CLASSES, ...ARRANGE_CLASSES, "pl-phone"]) element.classList.toggle(name, on.has(name));
    element.style.setProperty("--pl-b", `${bandWidth(view)}px`);
    element.style.setProperty("--pl-w", `${box.width}px`);
    element.style.setProperty("--pl-h", `${box.height}px`);
    element.style.setProperty("--pane-x", `${box.left}px`);
    // The open column's width, the pane's own box, from the arrangement (its track is that and the
    // view's 10px margin); none where the pane is shut or covers the session, so a `--preview` a host
    // or a case sets reaches the pane there.
    if (arrangement.pane === "column") element.style.setProperty("--preview-fit", `${arrangement.paneWidth}px`);
    else element.style.removeProperty("--preview-fit");
    // Where the view sits in the window, for placing alone (#s461 rules out reading the window for a
    // layout): its gaps to the window's right edge, top and bottom, and whether it is not the whole
    // window — beside the app's list or its rail, beside a wide phone's column, in a host's pane. A
    // view in such a box contains its layout, so its fixed layers are placed in its own box, not
    // across the window, and the screen's insets count only where they fall in it (`pl-boxed`,
    // design/pane-layout.md, "Drawn inside the pane"; the phone reader from stage 2, every family
    // from stage 4). The right edge is measured to the layout viewport, which a classic scrollbar
    // does not count in.
    element.style.setProperty("--pane-r", `${right}px`);
    element.style.setProperty("--pane-y", `${top}px`);
    element.style.setProperty("--pane-b", `${bottom}px`);
    element.classList.toggle("pl-boxed", box.left > 0.5 || right > 0.5 || top > 0.5 || bottom > 0.5);
    // A share of the height is the window's own unit while the view is as tall as the window (always
    // in the app, whose grid is 100dvh: iOS gives `innerHeight` that height too), so iPhone Safari's
    // `dvh`, which follows its bars, still does; else the view's. The window is read here for that
    // alone, never for a layout decision.
    element.style.setProperty("--pl-vh", tall ? "1vh" : `${box.height / 100}px`);
    element.style.setProperty("--pl-dvh", tall ? "1dvh" : `${box.height / 100}px`);
    onArrange?.(arrangement);
    element.getBoundingClientRect();
    if (hold) element.classList.remove("pl-holding");
    const key = JSON.stringify([[...on].sort(), Math.round(box.width), arrangement.paneWidth]);
    if (key === said) return;
    said = key;
    onChange?.();
    for (const name of Object.keys(family)) if (family[name] !== was[name]) events.dispatchEvent(new Event(name));
  };
  const decide = () => decideAt(measure());
  // #s467: a host may MOVE the view's box without resizing it (a pane slid in by a transform, placed
  // by script, scrolled with its page), which no ResizeObserver hears, and the gaps a decision writes
  // (`--pane-x`, `--pane-r`, `--pane-y`, `--pane-b`) and `pl-boxed` would keep the old place. So the
  // box is watched where it was measured, as floating-ui watches a reference move: an
  // IntersectionObserver whose root is the window cut down to that box (its root margins are the box's
  // gaps to the window's edges) hears the box leave that place, because what it sees of the box
  // changes. Each call says where the box is now; one that has moved decides again and is watched at
  // its new place, and one that has not (a host's clip changed what shows of it) is watched from what
  // shows now. No timer: a box with no size is not watched (the ResizeObserver hears it come back).
  let moves = null;
  const watchMoves = (box, threshold = 1) => {
    moves?.disconnect();
    moves = null;
    const root = viewRoot.styleRoot();
    if (signal?.aborted || !root || !box.width || !box.height || typeof IntersectionObserver !== "function") return;
    const gaps = [box.top, root.clientWidth - box.right, root.clientHeight - box.bottom, box.left];
    const heard = entries => {
      if (watcher !== moves) return;
      const { boundingClientRect: now, intersectionRatio: ratio } = entries[entries.length - 1];
      if (["left", "top", "width", "height"].some(side => Math.abs(now[side] - box[side]) > 0.5)) {
        decide();
        if (watcher === moves) watchMoves(element.getBoundingClientRect());
        return;
      }
      // Still where it was: all of the box shows, or the part a host's clip leaves, which is then the
      // threshold (none of it: any of it showing is a move).
      const seen = Math.min(1, Math.max(1e-7, ratio));
      if (Math.abs(seen - threshold) > 1e-6) watchMoves(box, seen);
    };
    const options = { rootMargin: gaps.map(gap => `${-Math.floor(gap)}px`).join(" "), threshold };
    // The view's own page is the root, so the margins are its window's even inside a frame; a browser
    // that takes no document for a root takes the window.
    let watcher;
    try {
      watcher = new IntersectionObserver(heard, { ...options, root: viewRoot.root() });
    } catch {
      watcher = new IntersectionObserver(heard, options);
    }
    moves = watcher;
    watcher.observe(element);
  };
  decide();
  const observer = new ResizeObserver(decide);
  observer.observe(element);
  for (const query of [coarse, fine]) query.addEventListener("change", decide, { signal });
  addEventListener("resize", decide, { signal });
  signal?.addEventListener("abort", () => { observer.disconnect(); moves?.disconnect(); moves = null; }, { once: true });
  const query = name => {
    if (!(name in family)) throw new Error(`pane-layout: no family named ${name}`);
    return {
      get matches() { return family[name]; },
      addEventListener: (type, listener, options) => { if (type === "change") events.addEventListener(name, listener, options); },
      removeEventListener: (type, listener, options) => { if (type === "change") events.removeEventListener(name, listener, options); },
    };
  };
  /** The reader's own toggle is starting: the decisions of its transition's time write without the
   *  hold, so the toggle glides whole. */
  const glide = () => { glideUntil = performance.now() + GLIDE_MS; };
  /** The reader's own toggle of a state the view wears (the pane's `preview-off`): `change` makes it,
   *  after the page is read and before the decision writes, so no layout runs between the change and
   *  what the decision writes, and the decision glides. A layout there would style the view in its old
   *  arrangement with the new state (a pane that covered the view, open over it, where its host's list
   *  just gave way for the pane as a column), and start that state's transitions, which the decision
   *  then cancels (#s431 stage 3). */
  const toggled = change => { const read = measure(); change(); glide(); decideAt(read); };
  return { decide, glide, toggled, arrangement: () => arranged, phone: () => family.phone, wide: () => family.wide, sized: () => sized, query };
}

// #s431 stage 3 (design/pane-layout.md, "The owner's order"): what keeps its width and what gives way
// (the owner, #s462; `M` #s472, the pane's drag #s473, the narrowest step #s474). In his order: the
// session at `M`, the right pane's content at `M`, the outline's glyphs in a column of their own, the
// session list at its width, and last the outline at its width. A part shows where it fits without
// taking room from a part above it, each at its least; a pane that cannot be a column covers the whole
// view and so uses all of it, and the list keeps its rail there, as his sequence has it. Each is
// a pure function of the reader's inputs and the width given, never of what was drawn, so neither can
// feed back on itself; the app and the view run the same ones, so the app leaves the view what it needs.

/** The least width of the session's column, and of the right pane's (the owner, #s472: today's least,
 *  `minmax(480px,1fr)`; its text is 64px narrower, about 58 characters of prose). The page's prose has
 *  one size, so `M` is in CSS pixels and a browser's zoom scales it with the text. The functions take
 *  it as `m` so the contract can hold the rules at other values. */
export const M = 480;
/** The outline's track at its width (its 268px cards and their gutter, as the page draws it), and its
 *  glyphs' column. */
export const OUTLINE = 290, GLYPHS = 68;
/** The resizer between the session and the pane, and the 10px margin at the view's right edge: the
 *  session card's with the pane shut, the pane's with it open (so `paneWidth` is the pane's own box
 *  and its track is that and the margin). */
export const RESIZER = 1, EDGE = 10;
/** The app's list: its rail, and its least width open. */
export const RAIL = 64, LIST_MIN = 232;
/** A touch screen's view this wide or less is the phone reader, wherever it sits: the owner's #s474
 *  (a touch screen keeps the phone layout up to 760px) and #s475 ("touch views of 760px or less are
 *  the phone reader, the iPad beside its list too"). */
export const TOUCH_PHONE = 760;

/** How much the outline takes beside the session, as it is shown. */
const outlinePart = outline => (outline === "open" ? OUTLINE : outline === "glyphs" ? GLYPHS : 0);

/** The width the view needs for its parts as given: the session at `m`, the pane (a column when open)
 *  at `m`, and the outline open, as glyphs in their column, or in the top bar ("bar"). */
export const viewNeeds = ({ m = M, pane = false, outline = "bar" }) => outlinePart(outline) + m + EDGE + (pane ? RESIZER + m : 0);

/** Whether the view at its own box is the phone reader: too narrow for the session at `m` beside its
 *  card's margin, or a touch screen's view `TOUCH_PHONE` wide or less or 500px tall or less. */
export const phoneView = ({ width, height = Infinity, coarse = false, m = M }) =>
  width < viewNeeds({ m }) || (!!coarse && (width <= TOUCH_PHONE || height <= 500));

/** How the view lays out its parts at its own box. `paneOpen` is whether the right pane is open,
 *  `outlineWanted` the reader's choice for the outline, `share` the pane's share of the slack (a half
 *  until the reader drags its edge, #s473), and `room` how much wider the host could make the view by
 *  narrowing what it lays beside it toward its least (the app's list, from its width to 232), which
 *  the host says through the optional `room` mount option (the owner, #s476), 0 without it, and
 *  hears the reader's choice through `onOutline` (#s479): only
 *  whether the reader may open the outline reads it, and anything but a finite positive number reads as
 *  0. The phone reader has a layout of its own. Otherwise the pane is a column while the session and it
 *  each keep `m`, and covers the whole view where they cannot; the session under it keeps the layout it
 *  has with the pane shut, so in a box that keeps its width (a host with no list) closing the pane
 *  moves nothing, while in the app the list may come back beside it. The glyphs keep their column while
 *  that fits beside them, and
 *  the outline opens while it fits too. What is left over goes to the session and the pane by
 *  `share`, or to the session alone. */
export function arrangeView({ width, height, coarse, m = M, paneOpen = false, outlineWanted = true, share = 0.5, room = 0 }) {
  if (phoneView({ width, height, coarse, m })) {
    return { phone: true, pane: paneOpen ? "over" : "shut", outline: "dock", outlineCanOpen: false, session: width, paneWidth: null };
  }
  const pane = !paneOpen ? "shut" : width >= viewNeeds({ m, pane: true }) ? "column" : "over";
  if (pane === "over") {
    const under = arrangeView({ width, height, coarse, m, outlineWanted });
    return { ...under, pane, outlineCanOpen: false };
  }
  const column = pane === "column";
  const fits = outline => width >= viewNeeds({ m, pane: column, outline });
  const outline = outlineWanted && fits("open") ? "open" : fits("glyphs") ? "glyphs" : "bar";
  const slack = width - viewNeeds({ m, pane: column, outline });
  const toPane = column ? Math.round(slack * Math.min(1, Math.max(0, share))) : 0;
  return {
    phone: false,
    pane,
    outline,
    // Whether the reader may open the outline here: it fits, or would once the host gave its room.
    outlineCanOpen: width + (Number.isFinite(room) && room > 0 ? room : 0) >= viewNeeds({ m, pane: column, outline: "open" }),
    session: m + slack - toPane,
    paneWidth: column ? m + toPane : null,
  };
}

/** The classes an arrangement sets on the view's element (#s431 stage 3), which its rules read in place
 *  of the bands that decided the same things before: the pane's mode, a column where the session and
 *  the pane each keep `m` beside each other (`pl-pane-column`) and over the view elsewhere
 *  (`pl-pane-over`), whether or not the pane is open, as the bands it replaces read it; and the
 *  outline as it is drawn (`pl-outline-open`, `-glyphs` in their column, `-bar` in the top bar, `-dock`
 *  on the phone reader), which a fold the width forced changes and the reader's choice does not. */
export const ARRANGE_CLASSES = Object.freeze(["pl-pane-column", "pl-pane-over", "pl-outline-open", "pl-outline-glyphs", "pl-outline-bar", "pl-outline-dock"]);

/** The classes `arrangement` (as `arrangeView` gave it for `view`) sets. */
export function arrangeClasses(view, arrangement, m = M) {
  const column = !arrangement.phone && view.width >= viewNeeds({ m, pane: true });
  return new Set([column ? "pl-pane-column" : "pl-pane-over", `pl-outline-${arrangement.outline}`]);
}

/** How long a reader's own toggle glides (the pane's `.22s`, and a frame either side). */
export const GLIDE_MS = 260;

/** The narrowest window that keeps the list's rail beside the view: the rail, the session at `m` and
 *  its card's margin, 554px (the owner's "rail + M", #s462; #s472 and #s474 said 544, leaving out the
 *  margin). */
export const narrowest = (m = M) => RAIL + viewNeeds({ m });

/** The app's list at a window `width`, beside the view the app has (`view`: its `m`, whether its pane
 *  is open and whether the reader wants its outline). `list` is the reader's width for it and
 *  `collapsed` whether the reader folded it to its rail (remembered; a wider window never undoes it).
 *  `"drawer"` is the phone's own list: the drawer, or a wide phone's column held sideways, as today. A
 *  touch screen `TOUCH_PHONE` wide or less or a short one has it (#s474), and so does a window
 *  narrower than `narrowest`. Otherwise the list opens only where everything above it fits beside it
 *  at its least: the session, the pane as a column while it is open, and the glyphs' column. Open, it
 *  keeps the reader's width while the view beside it has all it wants, narrows to its least before the
 *  outline folds, and is back at the reader's width once the outline has folded. The rules are the same
 *  on a touch screen, where the view beside the list is the phone reader wherever it is `TOUCH_PHONE`
 *  or less, with one more: the list narrows, down to its least and never folding, before it makes the
 *  view the phone reader, so a reader who folds the outline on a tablet keeps the desktop's layout and
 *  the way back to it (the owner, #s478); and a pane opened over the
 *  phone reader leaves the list as it was. `canOpen` is whether the reader may open the list from the
 *  rail. */
export function arrangeApp({ width, coarse = false, short = false, list = 300, collapsed = false, view = {} }) {
  const m = view.m ?? M;
  if ((coarse && (width <= TOUCH_PHONE || short)) || width < narrowest(m)) return { list: "drawer", canOpen: true };
  // The list's width were it open beside the view, with the pane as given; "rail" where it does not fit.
  const beside = pane => {
    const levels = view.outlineWanted === false ? ["glyphs"] : ["open", "glyphs"];
    const open = levels.map(outline => viewNeeds({ m, pane, outline })).find(need => width - LIST_MIN >= need);
    if (open === undefined) return "rail";
    const given = Math.max(LIST_MIN, Math.min(list, width - open));
    const keepsDesktop = coarse && width - given <= TOUCH_PHONE && width - LIST_MIN > TOUCH_PHONE;
    return keepsDesktop ? width - TOUCH_PHONE - 1 : given;
  };
  const shut = beside(false);
  // On a touch screen a pane opened over the phone reader covers the reader's area alone, beside the
  // list or the rail as they were, as a wide phone held sideways covers the session beside its list
  // (#s49): the session keeps its width beneath, so opening or closing the pane remeasures nothing
  // under it (#s25). The owner's ruling, #s480. The rail's expand stays enabled there,
  // so a list folded under that pane opens again beside it.
  const overReader = coarse && width - (shut === "rail" ? RAIL : shut) <= TOUCH_PHONE;
  const listed = !view.paneOpen || overReader ? shut : beside(true);
  return { list: collapsed ? "rail" : listed, canOpen: listed !== "rail" };
}
