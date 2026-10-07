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

/** An image the page can draw from EMBEDDED bytes (a `data:` URI in an <img>, inert whatever it
 *  holds — an SVG there runs no script). */
const IMAGE_FILE = /\.(png|jpe?g|gif|webp|avif|svg|bmp|ico)$/i;
/** What `/file` serves AS AN IMAGE from a PATH — `raster_type` in html_export/serve.rs, and a test
 *  holds the two to the same list (#275). SVG is deliberately absent: served from this origin it
 *  is a document that can run script, so the server sends its SOURCE as text, and the page
 *  previews it as text. The two lists disagreed once: a path .svg was offered as an image, and the
 *  lightbox asked `/file`, got text and showed its error; a path .avif was served as a download;
 *  a path .bmp the server would show was offered as one. */
const RASTER_FILE = /\.(png|jpe?g|gif|webp|bmp|ico|avif)$/i;
const TEXT_FILE = /\.(txt|md|mdx|rs|js|mjs|cjs|ts|tsx|jsx|json|jsonl|toml|ya?ml|html?|css|scss|py|rb|go|java|kt|swift|sh|zsh|fish|sql|csv|tsv|log|diff|patch|xml|svg|ini|conf)$/i;

function attachmentCapability(head = {}, { reveal = true } = {}) {
  const name = head.att_name || head.att_path || "";
  const embedded = head.att_datauri != null;
  const served = Boolean(head.att_path && head.att_fsig);
  // #275: whether it is an IMAGE depends on where the bytes come from. Embedded bytes are drawn
  // as they are; a path is an image only if `/file` will serve it as one, which it decides from
  // the path's own extension.
  const image = embedded ? head.att_kind === "image" || IMAGE_FILE.test(name) : served && RASTER_FILE.test(head.att_path);
  if (image) return { action: "image", label: "Enlarge", hint: embedded ? "image · saved with the session" : "image · temporary file" };
  if (head.att_text != null || (TEXT_FILE.test(name) && served)) return { action: "preview", label: "Open preview", hint: "opens in the preview pane" };
  if (embedded || served) return { action: "download", label: "Download", hint: "no inline preview · click to download" };
  // The render policy withheld the file stamp (or the bytes are not the kind the page shows),
  // but the server offered the REVEAL stamp: the file manager can still show the file. This is
  // the classic view's fallback (export.js: `fsig ? openArtifact : reveal`), and it is what
  // keeps every path actionable under `render-policy.json` mode "never".
  // #335/#s29: not where the reader cannot see the file manager (a remote client): copied instead.
  if (head.att_path && head.att_sig && reveal) return { action: "reveal", label: "Reveal in file manager", hint: "not readable here · opens its folder" };
  return { action: "copy", label: "Copy path", hint: head.att_path ? "path only · click to copy" : "attachment record only" };
}

/** What a clicked path reference does, from the stamps the server offered for it. The two
 *  stamps are different capabilities — a reveal stamp never authorizes `/file` — so the
 *  precedence is by what the page may DO, not by which stamp happens to be present. */
function referenceAction({ fileSig, revealSig, reveal = true } = {}) {
  if (fileSig) return "preview";
  // #335/#s29: `reveal: false` where the file manager is not the reader's (a remote client).
  if (revealSig && reveal) return "reveal";
  return "copy";
}

/** Whether the file manager can be asked to show this file: a path, and the REVEAL stamp the
 *  server offered for it. The file stamp is a different capability and never stands in for this
 *  one. Every file view offers reveal beside showing or downloading (#272, the owner: "offering
 *  both for now") — until a web file browser replaces reveal, no view offers only one half. */
const canReveal = ({ path, sig } = {}) => Boolean(path && sig);

/** Whether the page can hand the reader the file's bytes: a FILE stamp `/file` honours, or bytes
 *  the page already holds (an embedded data URI, carried text). A reveal stamp is not one. */
const canDownload = ({ path, fsig, data, text } = {}) => Boolean((path && fsig) || data || text != null);

/** The control BESIDE a file's own action (#272's "both halves", made where-aware by #s29): the
 *  file manager for a reader at this machine, the file as a download for one elsewhere — each only
 *  where the server offered what it needs (the reveal stamp; a file stamp or the bytes). `null`
 *  when neither applies. */
const besideAction = item => (revealHere() ? (canReveal(item) ? "reveal" : null) : canDownload(item) ? "download" : null);

/** Whether the file manager is the reader's to see: the page was loaded from THIS machine — a
 *  loopback host, the same test the server applies for gzip and masking (#313, #365). A reveal
 *  opens a Finder window on the machine the monitor runs on, so only a reader at that machine can
 *  use one; a remote reader (a phone over the tailnet, another desktop) is offered the file as a
 *  download instead (#s29, the owner: "only offer reveal in file manager when accessing locally and
 *  offer download the file when accessing remotely"). It was the 760px breakpoint (#335), which
 *  stood in for "remote" and missed a remote desktop and a narrow local window both. Every reveal a
 *  page offers asks this; `referenceAction`/`attachmentCapability` take it as `reveal`. */
const revealHere = () => {
  const host = typeof location === "object" && location ? String(location.hostname || "") : "";
  return host === "" || host === "localhost" || /^127(\.\d{1,3}){3}$/.test(host) || host === "[::1]" || host === "::1";
};

/** The shells' phone breakpoint (#310/#313), as a media query: a screen at most 760px wide, OR a
 *  finger on a screen whose short side is a phone's — a phone held sideways (#s32, the owner's
 *  screenshots of a landscape phone laid out as a narrow desktop). The test mdrev applies to its own
 *  phone layout ("one side of the screen is a phone's, either way up"). The CSS writes the same
 *  query (`@media(max-width:760px),(pointer:coarse) and (max-height:500px)`); every JS check of the
 *  breakpoint reads this one string, so the two cannot disagree. */
const PHONE_QUERY = "(max-width:760px), (pointer:coarse) and (max-height:500px)";

/** Whether the page is laid out for a phone (`PHONE_QUERY`): what a phone's layout withholds for its
 *  own reasons — a tab of its own, which has no way back there (#335) — and offers instead (the
 *  review sheet, #s12). Not a question of where the reader is. */
const onPhone = () => typeof matchMedia === "function" && matchMedia(PHONE_QUERY).matches;

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

export { attachmentCapability, besideAction, canDownload, canReveal, groupPointerRuns, isPointerAttachment, onPhone, PHONE_QUERY, POINTER_KINDS, RASTER_FILE, referenceAction, revealHere, revealQuery, stampQuery };
