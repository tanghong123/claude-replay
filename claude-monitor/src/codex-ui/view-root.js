// Where the session view finds what it draws in (#s429, design/session-view-component.md, stage 1).
//
// The session view is a component a host mounts. It was to draw inside a shadow root of its own,
// where `document.getElementById` finds nothing the view drew, a popover appended to
// `document.body` takes none of its styles, and `document.activeElement` is the host's element
// rather than the field that has focus. So every one of those the view makes goes through its ROOT,
// here. The owner chose no shadow root (#s438): the view draws into the page in every host, its
// styles hung on its own element, so the root is the page itself. One view per page: the native app
// gives each window a page of its own.
//
// What belongs to the app around the view (the session list, the theme toggle, the brand, the
// session search, the toast) reaches the page through `page` instead (and the view through `viewRoot`), so the two are told apart
// before the app shell's code is split from the view's (stage 3). A test holds every module of the
// shell to these two: no `document` lookup anywhere else.

// `null` stands for the page: node has no `document` when the contract test imports a module.
let root = null;
const doc = () => (typeof document === "undefined" ? null : document);
const at = () => root || doc();

/** The element a root's styles and state hang on: the shadow root's host, or the page's `<html>`. */
const rootElement = () => root?.host || doc()?.documentElement;
/** The view's own element (`#sessionView`), once the page holds one. */
const viewElement = () => at()?.getElementById("sessionView") ?? null;

/** The session view's root and its lookups. */
export const viewRoot = Object.freeze({
  /** The root itself: the page, or the shadow root the view draws in. */
  root: () => at(),
  /** The view's own element, which carries its state (`#sessionView`, stage 2) and its theme. */
  element: viewElement,
  /** One of the view's elements by id. */
  byId: id => at().getElementById(id),
  /** The first of the view's elements a selector matches. */
  $: selector => at().querySelector(selector),
  /** Every one of the view's elements a selector matches. */
  $$: selector => at().querySelectorAll(selector),
  /** The element that has focus inside the view (a shadow root names its own; the page's is the
   *  host's element then). */
  active: () => at()?.activeElement ?? null,
  /** Where the view appends what floats over it: its sheets, popovers, menus and veils. */
  layer: () => (root?.host ? root : doc().body),
  /** Where a stylesheet the view loads for itself goes (mdrev's): the page's head, or the shadow
   *  root, where a stylesheet linked in the head would not reach. */
  sheets: () => (root?.host ? root : doc().head),
  /** Where the view sets the custom properties it measures (`--preview`, `--phone-top`, …). */
  styleRoot: () => rootElement(),
  /** "dark" or "light": the theme the view draws in, carried by the view's own element, whose
   *  stylesheet reads it there (stage 5b); a page with no view (mdrev's tab) carries it on its root. */
  theme: () => ((viewElement() || rootElement())?.dataset.theme === "dark" ? "dark" : "light"),
  /** The element whose `data-theme` the view follows: what to watch for a theme change. */
  themeElement: () => viewElement() || rootElement(),
});

/** The app around the view: its own chrome, which stays in the page. */
export const page = Object.freeze({
  byId: id => doc().getElementById(id),
  $: selector => doc().querySelector(selector),
  $$: selector => doc().querySelectorAll(selector),
  active: () => doc()?.activeElement ?? null,
  body: () => doc()?.body ?? null,
  html: () => doc()?.documentElement ?? null,
});

/** Draw the view inside another root (a shadow root). Called once, before the view mounts. Nothing
 *  calls it since the shadow root was dropped (#s438); the lookups above still go through the root. */
export function useViewRoot(next) {
  root = next || null;
}
