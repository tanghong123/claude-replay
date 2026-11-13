// The session view's one layout decision (#s431 stage 2, design/pane-layout.md, "Its own size, and
// touch" and "One decision, read by styles and scripts"). The view is laid out by its own box and the
// screen's pointer, never by the window or by what a host has beside it (the owner's ruling, #s461):
// every rule its sources wrote under a size query reads a class this module sets on the view's element
// (the derivation rewrites `(max-width:1180px)` as `pl-max-w-1180`, scripts/session-view-css.mjs), and
// the view's scripts ask `phone()` here rather than matching `PHONE_QUERY` themselves.
//
// The view is a phone (`pl-phone`) when it is 460px wide or less, or a touch screen's view 500px tall or
// less. A width class reads `B`, the width the bands read: the view's own inside that family, where a
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

/** Whether a view of `width` × `height` on a screen whose pointer is `coarse` is a phone. */
export const isPhone = ({ width, height, coarse }) => width <= 460 || (!!coarse && height <= 500);

/** The width the bands read for that view: its own on a phone, 300px more elsewhere. */
export const bandWidth = view => (isPhone(view) ? view.width : view.width + 300);

/** Whether that view is a wide phone held sideways (`WIDE_PHONE_QUERY` as the view reads it): a short
 *  touch view 900px wide or more. */
export const isWidePhone = view => isPhone(view) && !!view.coarse && view.height <= 500 && view.width >= 900;

/** The classes a view of that size holds, `pl-phone` among them. */
export function classesFor(view) {
  const sample = { ...view, width: bandWidth(view) };
  const on = new Set(LAYOUT_CLASSES.filter(name => holds(name, sample)));
  if (isPhone(view)) on.add("pl-phone");
  return on;
}

/** Decide now and again whenever the view's box, the screen's pointer or the window's size changes:
 *  the classes on `element`, `pl-phone` with them, and the lengths its rules read (`--pl-b`, `--pl-w`,
 *  `--pl-h`, `--pane-x`, `--pane-r`, `--pl-vh`, `--pl-dvh`). `onChange` hears every decision
 *  that changed a class, after it is written. `query(family)` answers as a `MediaQueryList` does
 *  (`matches`, a `change` event), so a script that matched `PHONE_QUERY` asks the view's family instead
 *  by one name. Everything bound here ends with `signal`.
 *
 *  A decision lays the element out; it never glides. While it writes, the element wears `pl-holding`,
 *  under which its stylesheet holds every transition inside it, and one read styles every part at what
 *  was written before the hold lets go, so letting go starts none (#s431 stage 2: the outline's track
 *  glided 36px into place while the first prompts measured, and a prompt counted its lines at the
 *  narrower column). The reader's own toggles, which no decision writes, still glide. */
export function mountPaneLayout(element, { signal, onChange } = {}) {
  const coarse = matchMedia("(pointer:coarse)"), fine = matchMedia("(pointer:fine)");
  const events = new EventTarget();
  const family = { phone: false, wide: false };
  let said = null, wrote = null;
  const decide = () => {
    if (signal?.aborted) return;
    const box = element.getBoundingClientRect();
    const view = { width: box.width, height: box.height, coarse: coarse.matches, fine: fine.matches };
    const on = classesFor(view);
    const right = Math.max(0, viewRoot.viewportWidth() - box.right);
    const tall = box.height >= innerHeight - 2;
    const writing = JSON.stringify([[...on].sort(), box.width, box.height, box.left, right, tall]);
    if (writing === wrote) return;
    wrote = writing;
    const was = { ...family };
    family.phone = on.has("pl-phone");
    family.wide = isWidePhone(view);
    element.classList.add("pl-holding");
    for (const name of [...LAYOUT_CLASSES, "pl-phone"]) element.classList.toggle(name, on.has(name));
    element.style.setProperty("--pl-b", `${bandWidth(view)}px`);
    element.style.setProperty("--pl-w", `${box.width}px`);
    element.style.setProperty("--pl-h", `${box.height}px`);
    element.style.setProperty("--pane-x", `${box.left}px`);
    // Where the view sits in the window, for placing alone (#s461 rules out reading the window for a
    // layout): its gap to the window's right edge, and whether it is narrower than the window — beside
    // the app's list or its rail, beside a wide phone's column, in a host's pane. A phone reader in
    // such a box contains its layout, so its fixed layers are placed in its own box, not across the
    // window, and the screen's insets count only where they fall in it (`pl-boxed`,
    // design/pane-layout.md, "Drawn inside the pane").
    // Measured to the layout viewport, which a classic scrollbar does not count in.
    element.style.setProperty("--pane-r", `${right}px`);
    element.classList.toggle("pl-boxed", box.left > 0.5 || right > 0.5);
    // A share of the height is the window's own unit while the view is as tall as the window (always
    // in the app, whose grid is 100dvh: iOS gives `innerHeight` that height too), so iPhone Safari's
    // `dvh`, which follows its bars, still does; else the view's. The window is read here for that
    // alone, never for a layout decision.
    element.style.setProperty("--pl-vh", tall ? "1vh" : `${box.height / 100}px`);
    element.style.setProperty("--pl-dvh", tall ? "1dvh" : `${box.height / 100}px`);
    element.getBoundingClientRect();
    element.classList.remove("pl-holding");
    const key = [...on].sort().join(" ");
    if (key === said) return;
    said = key;
    onChange?.();
    for (const name of Object.keys(family)) if (family[name] !== was[name]) events.dispatchEvent(new Event(name));
  };
  decide();
  const observer = new ResizeObserver(decide);
  observer.observe(element);
  for (const query of [coarse, fine]) query.addEventListener("change", decide, { signal });
  addEventListener("resize", decide, { signal });
  signal?.addEventListener("abort", () => observer.disconnect(), { once: true });
  const query = name => {
    if (!(name in family)) throw new Error(`pane-layout: no family named ${name}`);
    return {
      get matches() { return family[name]; },
      addEventListener: (type, listener, options) => { if (type === "change") events.addEventListener(name, listener, options); },
      removeEventListener: (type, listener, options) => { if (type === "change") events.removeEventListener(name, listener, options); },
    };
  };
  return { decide, phone: () => family.phone, wide: () => family.wide, query };
}

// #s431 stage 3 (design/pane-layout.md, "The owner's order"): what keeps its width and what gives way
// (the owner, #s462; `M` #s472, the pane's drag #s473, the narrowest step #s474). In his order: the
// session at `M`, the right pane's content at `M`, the outline's glyphs in a column of their own, the
// session list at its width, and last the outline at its width. Each is a pure function of the
// reader's inputs and the width given, never of what was drawn, so neither can feed back on itself;
// the app and the view run the same ones, so what the app leaves the view is what the view needs.

/** The least width of the session's text and of the right pane's content, at the default text size
 *  (the owner, #s472: today's least, about 58 characters of prose). It scales with the text size. */
export const M = 480;
/** The outline at its width, and its glyphs in their column. */
export const OUTLINE = 268, GLYPHS = 68;
/** The resizer between the session and the pane, and the card's margin at the view's right edge. */
export const RESIZER = 1, EDGE = 10;
/** The app's list: its rail, and its least width open. */
export const RAIL = 64, LIST_MIN = 232;

/** How much the outline takes beside the session, as it is shown. */
const outlinePart = outline => (outline === "open" ? OUTLINE : outline === "glyphs" ? GLYPHS : 0);

/** The width the view needs for its parts as given: the session at `m`, the pane (a column when open)
 *  at `m`, and the outline open, as glyphs in their column, or in the top bar ("bar"). */
export const viewNeeds = ({ m = M, pane = false, outline = "bar" }) => outlinePart(outline) + m + EDGE + (pane ? RESIZER + m : 0);

/** How the view lays out its parts at `width`. `paneOpen` is whether the right pane is open,
 *  `outlineWanted` the reader's choice for the outline, `share` the pane's share of the slack (a half
 *  until the reader drags its edge, #s473), and `room` how much wider the host could make the view by
 *  giving up what ranks below the outline (the app's list, from its width to its least): only whether
 *  the outline could open reads it. The pane is a column while the session and it each keep `m`; the
 *  glyphs keep their column while that fits beside them; the outline opens while it fits too. What is
 *  left over goes to the session and the pane by `share`, or to the session alone. */
export function arrangeView({ width, m = M, paneOpen = false, outlineWanted = true, share = 0.5, room = 0 }) {
  const pane = !paneOpen ? "shut" : width >= viewNeeds({ m, pane: true }) ? "column" : "over";
  const column = pane === "column";
  const fits = outline => width >= viewNeeds({ m, pane: column, outline });
  const outline = outlineWanted && fits("open") ? "open" : fits("glyphs") ? "glyphs" : "bar";
  const slack = Math.max(0, width - viewNeeds({ m, pane: column, outline }));
  const toPane = column ? Math.round(slack * Math.min(1, Math.max(0, share))) : 0;
  return {
    pane,
    outline,
    // Whether the reader may open the outline here: it fits, or would once the host gave its room.
    outlineCanOpen: width + Math.max(0, room) >= viewNeeds({ m, pane: column, outline: "open" }),
    session: m + slack - toPane,
    paneWidth: column ? m + toPane : null,
  };
}

/** The narrowest window that keeps the list's rail beside the view: the rail and the session at `m`
 *  with its card's margin (the owner's "rail + M", #s462). */
export const narrowest = (m = M) => RAIL + viewNeeds({ m });

/** The app's list at a window `width`, beside the view the app has (`view`: its `m`, whether its pane
 *  is open and whether the reader wants its outline). `list` is the reader's width for it and
 *  `collapsed` whether the reader folded it to its rail (remembered; a wider window never undoes it).
 *  A touch screen 760px wide or less, or a short one, keeps the phone's drawer (#s474), and so does a
 *  window narrower than `narrowest`; otherwise the list keeps the reader's width while the view
 *  beside it has all it wants, narrows to its least before the outline folds, comes back once the
 *  outline has folded, and folds to its rail only when even at its least it would push the glyphs out
 *  of their column, or the pane out of being one where the rail leaves it room to be. `canOpen` is
 *  whether the reader may open it from the rail. */
export function arrangeApp({ width, coarse = false, short = false, list = 300, collapsed = false, view = {} }) {
  const m = view.m ?? M;
  if ((coarse && (width <= 760 || short)) || width < narrowest(m)) return { list: "drawer", canOpen: true };
  // The list gives way to the pane only where the pane can be a column at all, beside the rail.
  const pane = !!view.paneOpen && width - RAIL >= viewNeeds({ m, pane: true });
  const levels = view.outlineWanted === false ? ["glyphs"] : ["open", "glyphs"];
  const open = levels.map(outline => viewNeeds({ m, pane, outline })).find(need => width - LIST_MIN >= need);
  const canOpen = open !== undefined;
  if (collapsed || !canOpen) return { list: "rail", canOpen };
  return { list: Math.max(LIST_MIN, Math.min(list, width - open)), canOpen };
}
