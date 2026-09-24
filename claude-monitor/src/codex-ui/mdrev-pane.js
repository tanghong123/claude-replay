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
// internals the released bundle does not export (the design says why).

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

async function answer(response) {
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}

/** The mount's facts for one tab: which collection, which document, its capability, and how much
 *  of mdrev it gets. */
async function factsFor(item) {
  if (item.text != null) {
    const held = await answer(await fetch(`${CONTRACT}/hold`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: item.name || "document.md", text: item.text }),
    }));
    return { root: held.root, path: held.path, cap: held.cap, isGit: false, review: false, annotate: false, toolbar: "none" };
  }
  const query = `path=${encodeURIComponent(item.path || "")}&sig=${encodeURIComponent(item.fsig || "")}`;
  const open = await answer(await fetch(`${CONTRACT}/open?${query}`, { cache: "no-store" }));
  // The leave is the monitor's to give: history always (from git), notes only where it has a node
  // to run mdrev's CLI — mdrev's own format is written through nothing else.
  return { root: open.root, path: open.path, cap: open.cap, isGit: open.isGit, review: open.review, annotate: open.annotate };
}

/**
 * Mount mdrev on `el` for `item`. Resolves to `{unmount()}`, or null from a server that carries no
 * mdrev. Rejects when this document cannot be mounted — the pane then shows the text exactly as it
 * did before mdrev.
 */
export async function mountMarkdown(el, item) {
  const version = mdrevVersion();
  if (!version) return null;
  const [{ mountMdrev }, facts] = await Promise.all([loadMdrev(version), factsFor(item)]);
  // A key pressed while the reader is engaged with it is mdrev's (shared/keymap.js tracks the same
  // engagement mdrev does: the last click or focus inside). NOT focusable: measured in Chrome, a
  // `tabindex` on mdrev's host turned a single key press into thousands of trusted keydowns on it.
  el.dataset.guestKeys = "";
  // The mount element names what it mounted, as mdrev's guide shapes it (§10) — for a reader of the
  // DOM, and for the case that runs `mdrev-cli conform` against this very document.
  Object.assign(el.dataset, { contract: CONTRACT, root: facts.root, path: facts.path, cap: facts.cap || "" });
  let mounted = null;
  const mount = () => { mounted = mountMdrev(el, { contract: CONTRACT, ...facts, theme: theme() }); };
  mount();
  // The shell owns the theme and mdrev cannot re-theme a live mount, so a toggle remounts it; the
  // routes answer the same document, and the notes and history come back with it.
  const watch = new MutationObserver(() => { mounted?.unmount(); mount(); });
  watch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  return { unmount() { watch.disconnect(); mounted?.unmount(); mounted = null; } };
}
