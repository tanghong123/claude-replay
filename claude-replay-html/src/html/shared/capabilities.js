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

/** Whether the file manager can be asked to show this file: a path, and the REVEAL stamp the
 *  server offered for it. The file stamp is a different capability and never stands in for this
 *  one. Every file view offers reveal beside showing or downloading (#272, the owner: "offering
 *  both for now") — until a web file browser replaces reveal, no view offers only one half. */
const canReveal = ({ path, sig } = {}) => Boolean(path && sig);

/** The `/__reveal` query for a path and its reveal stamp — encoded once, verbatim. */
const revealQuery = ({ path, sig }) => `/__reveal?path=${encodeURIComponent(path || "")}&sig=${encodeURIComponent(sig || "")}`;

/** The `path=…&sig=…` query for a path and one of its stamps — encoded once, verbatim — the
 *  form both `/__reveal` and `/file` read (the classic page prefixes the route itself). */
const stampQuery = ({ path, sig }) => `path=${encodeURIComponent(path || "")}${sig ? `&sig=${encodeURIComponent(sig)}` : ""}`;

/** The attachment kinds that are POINTERS rather than content (#254).
 *
 *  `ref` and `file` are what a compaction leaves behind — the files that were in context when
 *  it happened — and `edited` is an in-editor file whose inline snippet is truncated. None of
 *  them carries anything to read, so the page has only a path to show.
 *
 *  They are worth naming because of how they LOOK, not how they behave: drawn with the same
 *  furniture as a real record — a bold filename, a chevron, a card, two buttons — four of them
 *  in a row after a compaction read as four updates that have been stripped of their content.
 *  The owner reported exactly that. A pointer should look like a pointer. */
const POINTER_KINDS = new Set(["ref", "file", "edited"]);

/** Is this attachment head a bare pointer — a known pointer kind with nothing to render?
 *
 *  Content is what disqualifies it: a `file` that DID keep its bytes or text is a real record
 *  and keeps its card. So the test is the kind AND the absence of anything to show. */
function isPointerAttachment(head = {}) {
  if (!POINTER_KINDS.has(String(head.att_kind || ""))) return false;
  return head.att_text == null && head.att_datauri == null;
}

/** Fold consecutive POINTER attachments into runs, leaving everything else alone (#254).
 *
 *  They arrive as a group and mean one thing — "these files were in context when the
 *  compaction happened" — so a reader wants one line saying that, not five cards to scroll
 *  past. Measured on the session that prompted this: 28 pointers in runs of 5, 2, 3, 2, 2, 5.
 *
 *  `headOf` reads an item's attachment head, because the two pages hold an item differently.
 *  Returns `[{ run: true, items } | { run: false, item }]` in the original order; a LONE
 *  pointer comes back as `run: false`, because one light line is already clear and wrapping it
 *  in a summary would be more furniture rather than less — the opposite of the point.
 *
 *  Order is never changed and nothing is dropped: every input item appears exactly once. */
function groupPointerRuns(items, headOf) {
  const out = [];
  let run = null;
  for (const item of items || []) {
    if (isPointerAttachment(headOf(item) || {})) {
      if (!run) {
        run = [];
        out.push({ run: true, items: run });
      }
      run.push(item);
      continue;
    }
    run = null;
    out.push({ run: false, item });
  }
  // A run of one is not a run.
  return out.map(g => (g.run && g.items.length === 1 ? { run: false, item: g.items[0] } : g));
}

export { attachmentCapability, canReveal, groupPointerRuns, isPointerAttachment, POINTER_KINDS, referenceAction, revealQuery, stampQuery };
