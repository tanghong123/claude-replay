// SHARED between the app shell (served as an ES module at /monitor-ui/shared/…), the classic
// rail and the v2 splice (inlined at serve time through {{SHARED}}) and the html crate's pages
// (inlined by html_export/shared.rs). Conventions the inliner relies on: no imports, exactly
// one trailing `export { … };` line.
//
// The two-stamp file rule (#46, design/monitor-shell-duplication.md §1(b)). A served page's
// file links carry up to two capability stamps the renderer signed for THAT path: `att_sig`
// / `sig` (reveal — `/__reveal` opens the folder) and `att_fsig` / `fsig` (file — `/file`
// renders the bytes, present only when the render policy allows the path). The routes act
// only on their own stamp, so a link cannot be edited from one capability into the other;
// this module decides what a click DOES from the stamps offered, once, for every consumer.

const IMAGE_FILE = /\.(png|jpe?g|gif|webp|avif|svg)$/i;
const TEXT_FILE = /\.(txt|md|mdx|rs|js|mjs|cjs|ts|tsx|jsx|json|jsonl|toml|ya?ml|html?|css|scss|py|rb|go|java|kt|swift|sh|zsh|fish|sql|csv|tsv|log|diff|patch|xml|ini|conf)$/i;

function attachmentCapability(head = {}) {
  const name = head.att_name || head.att_path || "";
  const image = head.att_kind === "image" || IMAGE_FILE.test(name);
  const hasSource = head.att_datauri != null || (head.att_path && head.att_fsig);
  if (image && hasSource) return { action: "image", label: "Enlarge", hint: head.att_datauri != null ? "image · saved with the session" : "image · temporary file" };
  if (head.att_text != null || (TEXT_FILE.test(name) && head.att_path && head.att_fsig)) return { action: "preview", label: "Open preview", hint: "opens in the preview pane" };
  if (head.att_datauri != null || (head.att_path && head.att_fsig)) return { action: "download", label: "Download", hint: "no inline preview · click to download" };
  // The render policy withheld the file stamp (or the bytes are not the kind the page shows),
  // but the server offered the REVEAL stamp: the file manager can still show the file. This is
  // the classic view's fallback (export.js: `fsig ? openArtifact : reveal`), and it is what
  // keeps every path actionable under `render-policy.json` mode "never".
  if (head.att_path && head.att_sig) return { action: "reveal", label: "Reveal in file manager", hint: "not readable here · opens its folder" };
  return { action: "copy", label: "Copy path", hint: head.att_path ? "path only · click to copy" : "attachment record only" };
}

/** What a clicked path reference does, from the stamps the server offered for it. The two
 *  stamps are different capabilities — a reveal stamp never authorizes `/file` — so the
 *  precedence is by what the page may DO, not by which stamp happens to be present. */
function referenceAction({ fileSig, revealSig } = {}) {
  if (fileSig) return "preview";
  if (revealSig) return "reveal";
  return "copy";
}

/** The `/__reveal` query for a path and its reveal stamp — encoded once, verbatim. */
const revealQuery = ({ path, sig }) => `/__reveal?path=${encodeURIComponent(path || "")}&sig=${encodeURIComponent(sig || "")}`;

/** The `path=…&sig=…` query for a path and one of its stamps — encoded once, verbatim — the
 *  form both `/__reveal` and `/file` read (the classic page prefixes the route itself). */
const stampQuery = ({ path, sig }) => `path=${encodeURIComponent(path || "")}${sig ? `&sig=${encodeURIComponent(sig)}` : ""}`;

export { attachmentCapability, referenceAction, revealQuery, stampQuery };
