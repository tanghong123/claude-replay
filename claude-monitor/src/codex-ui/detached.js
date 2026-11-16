// A tab of its own (#s433, design/embedding.md, "Detached views"): a session's transcript, a Markdown
// document or an image as the whole of one element, on a page a host serves at a route of its own.
// The kit carries it so a host's tab is drawn by agent-monitor, with its data from the host's
// contract, the same routes the embedded components read. The monitor is a host of it too: its
// `/detached` page (`/markdown` and `/image` are the same page) runs `detached-page.js`, which mounts
// this on the page's own address.
//
// What to show is read from the address's query, told apart in this order, so every address the
// monitor's `/markdown` and `/image` tabs were opened with keeps working:
//  - `session=<id>`: the session's transcript, the session view mounted with no list beside it;
//  - `root=&path=&cap=` (and `from`, `to`, `since`): a Markdown document, in mdrev's viewer
//    (`mountStandalone`, mdrev-pane.js);
//  - `path=&sig=`, `held=`, `name=`: an image, from the file it names or the copy the tab that
//    opened this one left in sessionStorage (`window.open` copies it; never `noopener`).
import { backend, useBackend } from "./backend.js";
import { mountSessionView } from "./session-view.js";
import { mountStandalone, Unavailable } from "./mdrev-pane.js";
import { createImageView } from "./shared/image-view.js";
import { bindKeymap } from "./shared/keymap.js";

/** Every option `mountDetached` takes; any other name is refused. */
export const DETACHED_OPTIONS = Object.freeze(["backend", "theme", "address"]);
const THEMES = Object.freeze(["light", "dark"]);
/** How often the transcript's tab reads the session index, as the app's list does. */
const INDEX_POLL_MS = 5000;

/** What an address shows: "session", "markdown", "image", or "none" for an address that names
 *  nothing. */
export function detachedKind(address) {
  const params = new URLSearchParams(address || "");
  if (params.get("session")) return "session";
  if (params.get("root")) return "markdown";
  if (params.get("path") || params.get("held")) return "image";
  return "none";
}

/** Draw what `address` names in `element` and return its handle. The element becomes
 *  `#detachedView`, the id the kit's `detached.css` hangs its rules on (one with another id is
 *  refused), and must be empty (nothing in it but white space); the host gives it a place and a size (the whole window, in a tab of
 *  its own) and styles nothing else of it.
 *
 *  Options, each named in `DETACHED_OPTIONS`, each optional:
 *  - `backend`: the backend handle to read through (`backendHandle`'s); it becomes the page's
 *    (`useBackend`), since this is the page's one component. Left out, the one `useBackend` last
 *    named, else the server at the directory of the page's address.
 *  - `theme`: "light" (the default) or "dark"; the handle's `setTheme` changes it.
 *  - `address`: the query that says what to show (`?session=…`, `?root=…`, `?path=…`). Left out,
 *    the page's own (`location.search`), which the tab then keeps current as the reader moves (a
 *    session they go to from this one, which Back returns from; another range or document): on the
 *    page's own path, so a host's route keeps its tab, and so a reload or a copied link lands where
 *    they were. Either way the tab keeps the page's title.
 *
 *  The transcript's tab is a reader: it draws the session view with no list (its index read every
 *  5 s, as the app's list reads it), offers no writes and no ↗ of its own, and binds the view's keys
 *  at the document, as the page's one component. The handle: `kind` (as `detachedKind` says),
 *  `setTheme(theme)` and `unmount()`, which stops everything it started and leaves the element as
 *  the host gave it, empty. */
export function mountDetached(element, options = {}) {
  if (!element || element.nodeType !== 1) throw new Error("mountDetached takes the element to draw in");
  if (element.id && element.id !== "detachedView") throw new Error(`mountDetached draws in an element with no id or the id detachedView (its stylesheet's), not #${element.id}`);
  if ([...element.childNodes].some(node => node.nodeType !== 3 || node.textContent.trim())) throw new Error("mountDetached draws in an empty element");
  for (const name of Object.keys(options)) if (!DETACHED_OPTIONS.includes(name)) throw new Error(`mountDetached takes no option named ${name}; its options are ${DETACHED_OPTIONS.join(", ")}`);
  if (options.theme !== undefined && !THEMES.includes(options.theme)) throw new Error(`mountDetached takes its theme as ${THEMES.join(" or ")}`);
  if (options.address !== undefined && typeof options.address !== "string") throw new Error("mountDetached takes its address as a query string");
  if (options.backend) useBackend(options.backend);
  const hadId = !!element.id;
  element.id = "detachedView";

  const ownsAddress = options.address === undefined;
  const address = options.address ?? location.search;
  const kind = detachedKind(address);
  const params = new URLSearchParams(address);
  const life = new AbortController(), signal = life.signal;
  let theme = options.theme || "light";
  let live = true;
  const stops = [];
  element.dataset.theme = theme;
  element.dataset.kind = kind;

  /** Keep the page's title, and its address where the tab owns it, on what the tab shows now. */
  function moved({ title, query, push = false }) {
    if (!live) return;
    if (title) document.title = title;
    if (!ownsAddress || query == null) return;
    const url = new URL(location.href);
    url.search = query;
    if (push) url.hash = "";
    if (url.href !== location.href || push) history[push ? "pushState" : "replaceState"](history.state, "", url);
  }
  function unavailable(title, words) {
    if (!live) return;
    element.replaceChildren();
    const box = Object.assign(document.createElement("div"), { className: "detached-unavailable" });
    const head = Object.assign(document.createElement("strong"), { textContent: title });
    const body = Object.assign(document.createElement("span"), { textContent: words });
    box.append(head, body);
    element.append(box);
  }

  let view = null;
  if (kind === "session") view = mountTranscript(params.get("session"));
  else if (kind === "markdown") mountDocument();
  else if (kind === "image") mountImage();
  else unavailable("Nothing to show here", "This address names no session, document or image. Open one again from its session.");

  // ------------------------------------------------------------------ the transcript
  function mountTranscript(first) {
    const box = Object.assign(document.createElement("div"), { id: "sessionView" });
    element.append(box);
    const rows = new Map();
    let selected = first, timer = 0, polling = false, loading = null, title = "";
    // Whether the session was a row of the index when it was chosen, and has since gone from it (the
    // app's rule: a sub-agent's transcript is never a row, and is never "gone" for it).
    let wasRow = false, gone = false;
    /** The session index, as the app's list reads it: each row keeps its object from read to read,
     *  carrying its group as `_group`. */
    async function load() {
      const response = await backend().fetch("/api/sessions", { cache: "no-store" });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await response.json();
      if (!live) return;
      const seen = new Set();
      for (const group of data.groups || []) for (const incoming of group.rows || []) {
        const stable = rows.get(incoming.id);
        if (stable) for (const key of Object.keys(stable)) if (!(key in incoming)) delete stable[key];
        const row = stable ? Object.assign(stable, incoming) : incoming;
        row._group = group;
        rows.set(row.id, row);
        seen.add(row.id);
      }
      for (const id of [...rows.keys()]) if (!seen.has(id)) rows.delete(id);
      if (wasRow && !gone && !rows.has(selected)) {
        gone = true;
        handle.close("Session is gone", "It may have been deleted or moved. This tab keeps reading the index.");
      }
      handle.indexChanged();
    }
    function refresh() {
      clearTimeout(timer); timer = 0;
      if (!live) return Promise.resolve();
      if (!loading) {
        loading = load().catch(() => {}).finally(() => {
          loading = null;
          if (live && polling) timer = setTimeout(refresh, INDEX_POLL_MS);
        });
      }
      return loading;
    }
    const start = () => { polling = true; return refresh(); };
    const stop = () => { polling = false; clearTimeout(timer); timer = 0; };
    stops.push(stop);
    const query = id => `session=${encodeURIComponent(id)}`;
    function choose(id, push) {
      if (!id || !live || (id === selected && handle.session() === id)) return;
      selected = id;
      wasRow = rows.has(id); gone = false;
      handle.select(id);
      moved({ title, query: query(id), push });
      handle.open(id);
    }
    const handle = mountSessionView(box, {
      theme,
      backend: backend(),
      grid: element,
      keyboard: "page",
      show: { detach: false },
      selectedId: () => selected,
      sessionRow: id => rows.get(id) || null,
      sessionWorkers: id => [...rows.values()].filter(row => row.startedBy === id).sort((a, b) => (a.startTs || 0) - (b.startTs || 0) || String(a.id).localeCompare(String(b.id))),
      selectSession: (id, push) => choose(id, push),
      renderList: () => {},
      refreshIndex: () => refresh(),
      pauseIndex: stop,
      resumeIndex: () => start(),
      // A child opened from its own address offers its parent: Back goes there, once (the app's own
      // rule, #82), where the tab keeps its address.
      parentShown: parent => {
        if (!ownsAddress || !parent || history.length > 1) return;
        try { const child = location.href; const url = new URL(child); url.search = query(parent); history.replaceState(history.state, "", url); history.pushState(history.state, "", child); } catch (_) {}
      },
      onTitle: ({ session, title: said }) => { title = said; if (session) moved({ title, query: query(session) }); else moved({ title }); },
    });
    // The view's keys, as the app binds them: the tab is the page's one component. Bound at the
    // document through a root that hands each listener the tab's `signal`, so `unmount` takes them.
    const keysRoot = { addEventListener: (type, listener, capture) => document.addEventListener(type, listener, { capture: capture === true, signal }) };
    bindKeymap(keysRoot, () => "view", (action, event) => (live && handle.keys[action] ? handle.keys[action](event) : false));
    if (ownsAddress) addEventListener("popstate", () => { const id = new URLSearchParams(location.search).get("session"); if (id) choose(id, false); }, { signal });
    // The server answers a session by id once its index has listed it, so the first read comes first.
    start().then(() => { if (!live) return; wasRow = rows.has(selected); handle.select(selected); handle.open(selected); });
    return handle;
  }

  // ------------------------------------------------------------------ a Markdown document
  function mountDocument() {
    const doc = Object.assign(document.createElement("div"), { className: "detached-doc" });
    element.append(doc);
    moved({ title: (params.get("path") || "").split("/").pop() || "Markdown" });
    mountStandalone(doc, address, place => moved(place)).then(mounted => {
      if (live) stops.push(() => mounted.unmount());
      else mounted.unmount();
    }, error => unavailable("This document cannot be shown", error instanceof Unavailable ? error.message : error.message));
  }

  // ------------------------------------------------------------------ an image
  function mountImage() {
    const name = params.get("name") || "image";
    moved({ title: name });
    const stage = Object.assign(document.createElement("div"), { className: "detached-stage" });
    const img = Object.assign(document.createElement("img"), { alt: name });
    stage.append(img);
    element.append(stage);
    let objectUrl = "";
    source().then(src => {
      if (!live) { if (src.startsWith("blob:")) URL.revokeObjectURL(src); return; }
      if (src.startsWith("blob:")) objectUrl = src;
      const shown = createImageView(stage, img, {});
      stops.push(() => shown.destroy());
      if (typeof ResizeObserver !== "undefined") {
        const watch = new ResizeObserver(() => shown.sync());
        watch.observe(stage);
        stops.push(() => watch.disconnect());
      }
      img.src = src;
    }, error => unavailable(error.title || "This image cannot be shown", error.message));
    stops.push(() => { if (objectUrl) URL.revokeObjectURL(objectUrl); });

    /** Where the bytes come from: the file the pane was offered, at full size; the copy the
     *  transcript embeds, handed over by the opener, where there is no file or the file has gone. */
    async function source() {
      let held = null;
      try { held = params.get("held") ? sessionStorage.getItem(params.get("held")) : null; } catch (_) {}
      if (!params.get("path")) {
        if (held) return held;
        throw Object.assign(new Error(params.get("held") ? "This image was handed to the tab that opened it, and that copy is gone. Open it again from its session." : "This address names no image."), { title: "This image is no longer here" });
      }
      try { return await fromFile(); } catch (error) { if (held) return held; throw error; }
    }
    async function fromFile() {
      const path = params.get("path"), sig = params.get("sig");
      const response = await backend().fetch(`/file?path=${encodeURIComponent(path)}&sig=${encodeURIComponent(sig || "")}`, { cache: "no-store", signal });
      // #s7: the route says which — gone, refused, unpaired — and the words are the server's.
      if (response.status === 410) throw Object.assign(new Error(`Nothing is at ${path} any more: it was moved or deleted after the session named it.`), { title: "This image is gone" });
      if (response.status === 401) throw Object.assign(new Error("Reading local files requires pairing — run `agent-monitor --pair`."), { title: "Not paired" });
      if (!response.ok) throw Object.assign(new Error(await response.text() || `HTTP ${response.status}`), { title: "This monitor may not show this image" });
      if (!(response.headers.get("content-type") || "").startsWith("image/")) throw Object.assign(new Error(`${path} is not an image this page can show.`), { title: "Not an image" });
      return URL.createObjectURL(await response.blob());
    }
  }

  return {
    kind,
    /** "light" or "dark": the tab's theme, which its document and the transcript follow. */
    setTheme(next) {
      if (!live || !THEMES.includes(next) || next === theme) return;
      theme = next;
      element.dataset.theme = theme;
      view?.setTheme(theme);
    },
    unmount() {
      if (!live) return;
      view?.unmount();
      live = false;
      life.abort();
      for (const stop of stops.splice(0)) { try { stop(); } catch (error) { (globalThis.reportError || console.error)(error); } }
      element.replaceChildren();
      delete element.dataset.theme;
      delete element.dataset.kind;
      if (!hadId) element.removeAttribute("id");
    },
  };
}
