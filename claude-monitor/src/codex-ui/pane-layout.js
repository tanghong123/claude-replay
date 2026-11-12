// The session view's one layout decision (#s431 stage 2, design/pane-layout.md, "One decision, read
// by styles and scripts"). The view is laid out by the pane a host gives it, not by the window: every
// rule its sources wrote under a width query now reads a class this module sets on the view's
// element (the derivation rewrites `(max-width:1180px)` as `pl-max-w-1180`, scripts/session-view-css.mjs),
// and the view's scripts ask `phone()` here rather than matching `PHONE_QUERY` themselves.
//
// A width class reads the view's EFFECTIVE width, `effectiveWidth`. Every other feature a width query
// carried (a short screen, a coarse pointer) still reads the screen, as it did: its class mirrors the
// browser's own answer to that feature, through `matchMedia`, so it is exact by construction.

/** Every class the view's rules read, as the derivation reports them (`report.layoutClasses`): the
 *  contract holds the two to one list. */
export const LAYOUT_CLASSES = Object.freeze([
  "pl-coarse", "pl-fine", "pl-max-h-500", "pl-max-w-1180", "pl-max-w-560", "pl-max-w-620", "pl-max-w-700",
  "pl-max-w-760", "pl-max-w-900", "pl-min-h-501", "pl-min-w-1181", "pl-min-w-701", "pl-min-w-761",
  "pl-min-w-900", "pl-min-w-901",
]);

const SHAPE = /^pl-(max|min)-(w|h)-(\d+)$/;

/** The media feature a class stands for: `pl-max-w-760` is `(max-width:760px)`. */
export function featureOf(name) {
  if (name === "pl-coarse") return "(pointer:coarse)";
  if (name === "pl-fine") return "(pointer:fine)";
  const m = SHAPE.exec(name);
  if (!m) throw new Error(`pane-layout: no feature for ${name}`);
  return `(${m[1]}-${m[2] === "w" ? "width" : "height"}:${m[3]}px)`;
}

/** Whether a class reads the effective width (and not the screen). */
export const readsWidth = name => SHAPE.test(name) && SHAPE.exec(name)[2] === "w";

/** Whether a class holds for a width, a screen height and a pointer: the rule the page applies, kept
 *  pure so the contract can check it against the media queries it replaced. */
export function holds(name, { width, height, coarse, fine }) {
  if (name === "pl-coarse") return !!coarse;
  if (name === "pl-fine") return !!fine;
  const m = SHAPE.exec(name);
  if (!m) throw new Error(`pane-layout: no rule for ${name}`);
  const value = m[2] === "w" ? width : height;
  return m[1] === "max" ? value <= Number(m[3]) : value >= Number(m[3]);
}

/** The families the view's scripts ask about, each a query the stylesheets used, read from the
 *  classes. `phone` is `PHONE_QUERY`: an effective width of 760px or less, or a coarse pointer on a
 *  screen 500px tall or less (a phone held sideways). `wide` is `WIDE_PHONE_QUERY`: a phone held
 *  sideways whose effective width is 900px or more. */
const FAMILIES = {
  phone: has => has("pl-max-w-760") || (has("pl-coarse") && has("pl-max-h-500")),
  wide: has => has("pl-coarse") && has("pl-max-h-500") && has("pl-min-w-900"),
};
export const isPhone = sample => FAMILIES.phone(name => holds(name, sample));
export const isWidePhone = sample => FAMILIES.wide(name => holds(name, sample));

/** The width the view's rules read. Waits on the owner's decision (#s431's #s461): what a host has
 *  beside the view. Until then it is the view's own width and the 300px the app's sidebar has by
 *  default, which is the window at the default sidebar on a desktop, and nowhere else. */
export function effectiveWidth(element) {
  return element.getBoundingClientRect().width + 300;
}

/** Decide now and again whenever the view's box or the screen changes: the classes on `element`,
 *  `pl-phone` with them, and `--pl-e`. `onChange` hears every decision that changed a class, after it
 *  is written. `query(family)` answers as a `MediaQueryList` does (`matches`, a `change` event), so a
 *  script that matched `PHONE_QUERY` asks the view's family instead by one name. Everything bound
 *  here ends with `signal`. */
export function mountPaneLayout(element, { signal, onChange } = {}) {
  const screens = new Map(LAYOUT_CLASSES.filter(name => !readsWidth(name)).map(name => [name, matchMedia(featureOf(name))]));
  const events = new EventTarget();
  const family = Object.fromEntries(Object.keys(FAMILIES).map(name => [name, false]));
  let said = null;
  const decide = () => {
    if (signal?.aborted) return;
    const width = effectiveWidth(element);
    const on = new Set(LAYOUT_CLASSES.filter(name => (readsWidth(name) ? holds(name, { width }) : screens.get(name).matches)));
    const was = { ...family };
    for (const [name, rule] of Object.entries(FAMILIES)) family[name] = rule(c => on.has(c));
    if (family.phone) on.add("pl-phone");
    for (const name of [...LAYOUT_CLASSES, "pl-phone"]) element.classList.toggle(name, on.has(name));
    element.style.setProperty("--pl-e", `${width}px`);
    const key = [...on].sort().join(" ");
    if (key === said) return;
    said = key;
    onChange?.();
    for (const name of Object.keys(FAMILIES)) if (family[name] !== was[name]) events.dispatchEvent(new Event(name));
  };
  decide();
  const observer = new ResizeObserver(decide);
  observer.observe(element);
  for (const query of screens.values()) query.addEventListener("change", decide, { signal });
  signal?.addEventListener("abort", () => observer.disconnect(), { once: true });
  const query = name => {
    if (!(name in FAMILIES)) throw new Error(`pane-layout: no family named ${name}`);
    return {
      get matches() { return family[name]; },
      addEventListener: (type, listener, options) => { if (type === "change") events.addEventListener(name, listener, options); },
      removeEventListener: (type, listener, options) => { if (type === "change") events.removeEventListener(name, listener, options); },
    };
  };
  return { decide, phone: () => family.phone, wide: () => family.wide, query };
}
