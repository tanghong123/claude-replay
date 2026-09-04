// The search haystack and the hit rules both pages share (#111, design/rendering-parity-audit.md
// rows 5.3–5.4; the module #118 grows into). A record's searchable TEXT is what a reader can see
// of it: its head's summary, badge, preview, name, target and attachment name, then its body
// parts — markdown with the tags stripped, pre/note text, numbered source lines, diff lines —
// and the same for every record nested in it. Not its JSON: a query that is only a field name
// finds nothing. The scope classes (u/a/t/o/b/r/e) and the whole-word rule live here too.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.
// `strip` is the page's HTML-to-text function (the classic page uses a scratch element; the app
// shell a regex) so the module stays DOM-free.

const CLASS_BIT = { u: 1, a: 2, t: 4, o: 8, b: 16, r: 32, e: 64 };

/** The scope classes a record kind belongs to directly, as a bitmask. */
function directMask(k) {
  if (k === "user" || k === "command") return CLASS_BIT.u;
  if (k === "assistant") return CLASS_BIT.a;
  if (k === "think" || k === "act") return CLASS_BIT.t;
  if (!/^(bash|edit|write|read|skill|tool)$/.test(k)) return 0;
  let mask = CLASS_BIT.o;
  if (k === "bash") mask |= CLASS_BIT.b;
  if (k === "read") mask |= CLASS_BIT.r;
  if (k === "edit" || k === "write") mask |= CLASS_BIT.e;
  return mask;
}

/** A record's OWN text parts (nested records excluded), in reading order. */
function ownTextParts(b, strip) {
  const parts = [], h = b.head || {};
  for (const k of ["summary", "badge", "preview", "name", "target", "att_name"]) if (h[k]) parts.push(String(h[k]));
  for (const p of b.body || []) {
    if (p.p === "md" || p.p === "think") parts.push(strip(p.h));
    else if (p.p === "pre" || p.p === "note") parts.push(String(p.x));
    else if (p.p === "num") for (const r of p.rows || []) parts.push(strip(String(r[1])));
    else if (p.p === "diff") for (const r of p.rows || []) parts.push(String(r[2]));
  }
  return parts;
}

/** A record's whole searchable text: its own parts, then each nested record's, newline-joined. */
function recordText(b, strip) {
  const out = [];
  (function walk(record) {
    const own = ownTextParts(record, strip).join("\n");
    if (own) out.push(own);
    for (const p of record.body || []) if (p.p === "blocks") for (const item of p.items || []) walk(item);
  })(b);
  return out.join("\n");
}

/** A regex HTML-to-text: tags out, the five entities the renderer emits decoded. */
function stripTags(h) {
  return String(h ?? "").replace(/<[^>]*>/g, " ").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#39;/g, "'");
}

const WORD_LEFT = /[\p{L}\p{N}\p{M}_]$/u;
const WORD_RIGHT = /^[\p{L}\p{N}\p{M}_]/u;

/** Whether the match at `start` of `len` chars in `t` is a whole word. */
function wholeAt(t, start, len) {
  return !WORD_LEFT.test(t.slice(0, start)) && !WORD_RIGHT.test(t.slice(start + len));
}

/** Occurrences of the lowercase needle `lc` in `t` (whole words only when `whole`). */
function countOcc(t, lc, whole) {
  let n = 0, i = 0;
  while ((i = t.indexOf(lc, i)) !== -1) {
    if (!whole || wholeAt(t, i, lc.length)) n++;
    i += lc.length;
  }
  return n;
}

export { CLASS_BIT, directMask, ownTextParts, recordText, stripTags, WORD_LEFT, WORD_RIGHT, wholeAt, countOcc };
