// mdrev's embedded viewer, as a guest in the preview pane (#270) — design/mdrev-in-the-preview-pane.md.
//
// The monitor serves the mdrev release PINNED into it (#274, `vendor/mdrev`): the page learns the
// version from `data-mdrev` and imports the bundle from `/mdrev/<version>/` only when a Markdown
// document is first shown, so a shell that never previews Markdown never loads it. Where the
// document came from decides how much of mdrev it gets:
//
//  * Markdown the transcript CARRIES (`item.text`) — the owner: "only show a cleanly rendered
//    viewer (as a reader)". The monitor holds the text for mdrev's contract and the mount declares
//    no history, no notes and no toolbar — mdrev's own word for a document inside a page that has
//    chrome of its own. The keys stay.
//  * a Markdown FILE the page holds a `/file` stamp for — the whole viewer: redlines against the
//    file's own git history and review notes in its checkout, on mdrev's toolbar. Notes run on
//    mdrev's CLI under node; a monitor with no node refuses them and keeps the history.
//
// Both go through mdrev's HTTP contract under `/api/mdrev`; its in-process `client` seam needs
// internals the released bundle does not export (the design says why). Either goes to a tab of its
// own (#271): `/markdown` serves the viewer as the whole page, at the reader's document and range.

const CONTRACT = "/api/mdrev";
const MARKDOWN = /\.(md|markdown|mdown|mkd)$/i;

/** Whether the pane should hand this tab to mdrev: a Markdown name, by extension. */
export const isMarkdownName = name => MARKDOWN.test(String(name || ""));

/** The pinned mdrev release's version, or "" from a server that carries none. */
export const mdrevVersion = () => document.body?.dataset.mdrev || "";

let loading = null;
/** The bundle, once per page: its stylesheet (scoped under `.mdrev-host` by mdrev's build, so
 *  nothing in it reaches the shell) and its entry module. A failed load is forgotten, so the next
 *  document tries again instead of inheriting the failure. */
function loadMdrev(version) {
  if (!loading) {
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = `/mdrev/${version}/mdrev.css`;
    document.head.appendChild(link);
    loading = import(`/mdrev/${version}/mdrev.js`).catch(error => { loading = null; link.remove(); throw error; });
  }
  return loading;
}

const theme = () => (document.documentElement.dataset.theme === "dark" ? "dark" : "light");

/** The collection name for Markdown the transcript carries (the monitor's `held` store). */
const HELD = "held";

async function answer(response) {
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

/** Hand the monitor text to hold for mdrev's contract: content-addressed, so the same text always
 *  comes back under the same path and capability. */
async function hold(name, text) {
  return answer(await fetch(`${CONTRACT}/hold`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: name || "document.md", text }),
  }));
}

/** A reader of held text: no history, no notes and no toolbar — the owner's "only show a cleanly
 *  rendered viewer (as a reader)". */
const reader = held => ({ root: held.root, path: held.path, cap: held.cap, isGit: false, review: false, annotate: false, toolbar: "none" });

/** The mount's facts for a file on this machine, from the file's own `/file` stamp. */
async function opened(path, sig) {
  const open = await answer(await fetch(`${CONTRACT}/open?path=${encodeURIComponent(path)}&sig=${encodeURIComponent(sig || "")}`, { cache: "no-store" }));
  // The leave is the monitor's to give: history always (from git), notes only where it has a node
  // to run mdrev's CLI — mdrev's own format is written through nothing else.
  return { root: open.root, path: open.path, cap: open.cap, isGit: open.isGit, review: open.review, annotate: open.annotate, name: open.name };
}

/** The mount's facts for one tab: which collection, which document, its capability, and how much
 *  of mdrev it gets. */
async function factsFor(item) {
  if (item.text != null) return reader(await hold(item.name, item.text));
  return opened(item.path || "", item.fsig);
}

/** The capability for another document of the collection the reader moved to — the one `resolve`
 *  mints for a link, asked in the name of the document the mount was given. "" when refused. */
async function capFor(root, origin, target) {
  try {
    const caps = await answer(await fetch(`${CONTRACT}/resolve?root=${encodeURIComponent(root)}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ from: origin, targets: [target] }),
    }));
    return caps.caps?.[0] || "";
  } catch { return ""; }
}

/**
 * Mount mdrev on `el` and keep up with the reader: `place` is where they are — the document, the
 * capability that opens it, the range they chose — and the mount element names it (the dataset
 * mdrev's guide shapes, §10). A theme toggle remounts at that place, since mdrev cannot re-theme a
 * live mount; `moved` hears every move.
 */
function mountAt(mountMdrev, el, facts, moved) {
  const origin = { path: facts.path, cap: facts.cap || "" };
  const place = { root: facts.root, path: facts.path, cap: origin.cap, range: facts.range || null };
  // A key pressed while the reader is engaged with it is mdrev's (shared/keymap.js tracks the same
  // engagement mdrev does: the last click or focus inside). NOT focusable: measured in Chrome, a
  // `tabindex` on mdrev's host turned a single key press into thousands of trusted keydowns on it.
  el.dataset.guestKeys = "";
  const show = () => {
    Object.assign(el.dataset, { contract: CONTRACT, root: place.root, path: place.path, cap: place.cap });
    for (const key of ["from", "to", "since"]) {
      const value = place.range?.[key];
      if (value) el.dataset[key] = value; else delete el.dataset[key];
    }
    moved?.(place);
  };
  const onNavigate = (path, range) => {
    place.range = range || null;
    if (path && path !== place.path) {
      place.path = path;
      place.cap = path === origin.path ? origin.cap : "";
      if (!place.cap) capFor(place.root, origin, path).then(cap => { if (place.path === path) { place.cap = cap; show(); } });
    }
    show();
  };
  let mounted = null;
  const mount = () => {
    const at = place.cap ? place : { ...place, ...origin };
    mounted = mountMdrev(el, { contract: CONTRACT, ...facts, path: at.path, cap: at.cap || undefined, range: at.range || undefined, theme: theme(), onNavigate });
  };
  show();
  mount();
  const watch = new MutationObserver(() => { mounted?.unmount(); mount(); });
  watch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  return { place, unmount() { watch.disconnect(); mounted?.unmount(); mounted = null; } };
}

/**
 * Mount mdrev on `el` for `item`. Resolves to a handle — `unmount()`, the reader's `place`, and
 * `href()`, the address of a tab of its own for it (#271) — or null from a server that carries no
 * mdrev. Rejects when this document cannot be mounted — the pane then shows the text exactly as it
 * did before mdrev.
 */
export async function mountMarkdown(el, item) {
  const version = mdrevVersion();
  if (!version) return null;
  const [{ mountMdrev }, facts] = await Promise.all([loadMdrev(version), factsFor(item)]);
  const { place, unmount } = mountAt(mountMdrev, el, facts);
  const carried = item.text != null ? { name: item.name || "", text: item.text } : null;
  return {
    place,
    unmount,
    /** Where a tab of its own finds this document; for held text, first leave the tab its own copy
     *  (`window.open` copies this page's sessionStorage into the tab it makes), so a monitor restart
     *  that empties the store cannot strand a tab the reader keeps open. */
    href() {
      if (carried) carry(place.path, carried);
      return standaloneHref(place);
    },
  };
}

// ------------------------------------------------------------------------ a tab of its own (#271)

/** The address of a tab of its own for the document the reader is on: the same collection,
 *  document and capability — nothing the tab could not already ask the routes for — and the range
 *  they chose, so it opens where they were. */
export function standaloneHref({ root, path, cap, range }) {
  const query = new URLSearchParams({ root: root || "", path: path || "", cap: cap || "" });
  if (range?.from) query.set("from", range.from);
  if (range?.to && range.to !== "WORKTREE") query.set("to", range.to);
  if (range?.since) query.set("since", range.since);
  return `/markdown?${query}`;
}

/** What a tab's address carries, as `standaloneHref` wrote it. */
export function standaloneFacts(search) {
  const query = new URLSearchParams(search);
  const from = query.get("from"), to = query.get("to"), since = query.get("since");
  // Any one of the three is a range: `to` alone is a plain read of the document at that revision.
  const range = from || to || since ? { from: from || null, ...(to ? { to } : {}), ...(since ? { since } : {}) } : null;
  return { root: query.get("root") || "", path: query.get("path") || "", cap: query.get("cap") || "", range };
}

const carryKey = path => `mdrev-held:${path}`;

function carry(path, held) {
  try { sessionStorage.setItem(carryKey(path), JSON.stringify(held)); } catch { /* too large for the store: the monitor's copy serves */ }
}

function carried(path) {
  try { return JSON.parse(sessionStorage.getItem(carryKey(path)) || "null"); } catch { return null; }
}

/** A document a tab cannot show, and what the reader can do about it. */
export class Unavailable extends Error {}

/** The facts for the document a tab's address names. Held text is held again from the tab's own
 *  copy when it has one — content-addressed, so it returns under the same path and capability — and
 *  otherwise must still be in the monitor's store; a file goes through `open`, the pane's own
 *  guards, with the stamp the address carries. */
async function standaloneMount(want) {
  if (want.root === HELD) {
    const copy = carried(want.path);
    if (copy?.text != null) {
      const held = await hold(copy.name, copy.text);
      return { ...reader(held), name: copy.name || held.name };
    }
    const probe = await fetch(`${CONTRACT}/text?root=${HELD}&path=${encodeURIComponent(want.path)}&cap=${encodeURIComponent(want.cap)}`, { cache: "no-store" });
    if (probe.status === 404) throw new Unavailable("This text is no longer held — the monitor has restarted or let it go. Open it again from its session.");
    if (!probe.ok) throw new Unavailable(`This document cannot be opened here (HTTP ${probe.status}).`);
    return { ...reader(want), name: want.path.split("/").pop() };
  }
  const abs = want.root === "/" ? `/${want.path}` : `${want.root.replace(/\/+$/, "")}/${want.path}`;
  try {
    return await opened(abs, want.cap);
  } catch (error) {
    if (/HTTP 401/.test(error.message)) throw new Unavailable("Reading local files requires pairing — run `agent-monitor --pair`.");
    throw new Unavailable(`This document cannot be opened here (${error.message}).`);
  }
}

/**
 * The page `/markdown` serves: the document a tab's address names, as the whole page. The address
 * follows the reader — a move to another range or document rewrites it — so a reload or a copied
 * link lands where they were. The theme is the shell's own, read from the setting the shell keeps,
 * and follows a toggle made in any tab.
 */
export async function mountStandalone(el, search = location.search) {
  const want = standaloneFacts(search);
  const version = mdrevVersion();
  if (!version) throw new Unavailable("This server carries no Markdown viewer.");
  if (!want.root || !want.path) throw new Unavailable("This address names no document.");
  const follow = () => { document.documentElement.dataset.theme = localStorage.getItem("am-demo-theme") === "dark" ? "dark" : ""; };
  follow();
  addEventListener("storage", event => { if (event.key === "am-demo-theme") follow(); });
  const [{ mountMdrev }, facts] = await Promise.all([loadMdrev(version), standaloneMount(want)]);
  const name = facts.name || want.path;
  return mountAt(mountMdrev, el, { ...facts, range: want.range }, place => {
    document.title = place.path.split("/").pop() || name;
    history.replaceState(null, "", standaloneHref(place));
  });
}
