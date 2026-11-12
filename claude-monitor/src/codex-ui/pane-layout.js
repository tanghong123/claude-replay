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
 *  `--pl-h`, `--pane-x`, `--pl-vh`, `--pl-dvh`). `onChange` hears every decision
 *  that changed a class, after it is written. `query(family)` answers as a `MediaQueryList` does
 *  (`matches`, a `change` event), so a script that matched `PHONE_QUERY` asks the view's family instead
 *  by one name. Everything bound here ends with `signal`. */
export function mountPaneLayout(element, { signal, onChange } = {}) {
  const coarse = matchMedia("(pointer:coarse)"), fine = matchMedia("(pointer:fine)");
  const events = new EventTarget();
  const family = { phone: false, wide: false };
  let said = null;
  const decide = () => {
    if (signal?.aborted) return;
    const box = element.getBoundingClientRect();
    const view = { width: box.width, height: box.height, coarse: coarse.matches, fine: fine.matches };
    const on = classesFor(view);
    const was = { ...family };
    family.phone = on.has("pl-phone");
    family.wide = isWidePhone(view);
    for (const name of [...LAYOUT_CLASSES, "pl-phone"]) element.classList.toggle(name, on.has(name));
    element.style.setProperty("--pl-b", `${bandWidth(view)}px`);
    element.style.setProperty("--pl-w", `${box.width}px`);
    element.style.setProperty("--pl-h", `${box.height}px`);
    element.style.setProperty("--pane-x", `${box.left}px`);
    // Whether the view is narrower than the window: beside the app's list or its rail, beside a wide
    // phone's column, in a host's pane. It picks no layout (#s461): a phone reader in such a box
    // contains its layout, so its fixed layers are placed in its own box, not across the window
    // (`pl-boxed`, design/pane-layout.md, "Drawn inside the pane").
    element.classList.toggle("pl-boxed", box.left > 0.5 || box.right < innerWidth - 0.5);
    // A share of the height is the window's own unit while the view is as tall as the window (always
    // in the app, whose grid is 100dvh: iOS gives `innerHeight` that height too), so iPhone Safari's
    // `dvh`, which follows its bars, still does; else the view's. The window is read here for that
    // alone, never for a layout decision.
    const tall = box.height >= innerHeight - 2;
    element.style.setProperty("--pl-vh", tall ? "1vh" : `${box.height / 100}px`);
    element.style.setProperty("--pl-dvh", tall ? "1dvh" : `${box.height / 100}px`);
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
