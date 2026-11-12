// Where the session view finds what it draws in (#s429, design/session-view-component.md, stage 1).
//
// The session view is a component a host mounts, and every lookup it makes (an element by id, the
// focus, the layer its popovers float in, where its stylesheets and measured properties go, its
// theme) goes through `viewRoot`, here. It was to draw inside a shadow root of its own, where the
// document's own lookups find nothing it drew; the owner chose no shadow root (#s438), so the view
// draws into the page in every host, its styles hung on its own element, and its root IS the page.
// One view per page: the native app gives each window a page of its own.
//
// What belongs to the app around the view (the session list, the theme toggle, the brand, the
// session search, the toast) reaches the page through `page` instead (and the view through `viewRoot`), so the two are told apart
// before the app shell's code is split from the view's (stage 3). A test holds every module of the
// shell to these two: no `document` lookup anywhere else.

// `null` stands for no page: node has no `document` when the contract test imports a module.
const doc = () => (typeof document === "undefined" ? null : document);

/** The page's `<html>`, where the view's measured properties hang. */
const rootElement = () => doc()?.documentElement;
/** The view's own element (`#sessionView`), once the page holds one. */
const viewElement = () => doc()?.getElementById("sessionView") ?? null;

/** The session view's root and its lookups. */
export const viewRoot = Object.freeze({
  /** The root itself: the page. */
  root: () => doc(),
  /** The view's own element, which carries its state (`#sessionView`, stage 2) and its theme. */
  element: viewElement,
  /** One of the view's elements by id. */
  byId: id => doc().getElementById(id),
  /** The first of the view's elements a selector matches. */
  $: selector => doc().querySelector(selector),
  /** Every one of the view's elements a selector matches. */
  $$: selector => doc().querySelectorAll(selector),
  /** The element that has focus. */
  active: () => doc()?.activeElement ?? null,
  /** Where the view appends what floats over it: its sheets, popovers, menus and veils. */
  layer: () => doc().body,
  /** Where a stylesheet the view loads for itself goes (mdrev's): the page's head. */
  sheets: () => doc().head,
  /** Where the view sets the custom properties it measures (`--preview`, `--phone-top`, …). */
  styleRoot: () => rootElement(),
  /** The layout viewport's width: the window's, less a classic scrollbar (`innerWidth` counts one). */
  viewportWidth: () => rootElement()?.clientWidth ?? (typeof innerWidth === "number" ? innerWidth : 0),
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
