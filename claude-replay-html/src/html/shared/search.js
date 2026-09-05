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

/** A record's searchable text with per-part OWNERSHIP (#101): each own text — the record's,
 *  then each nested record's — as a `[start, end)` span carrying its kind's scope mask, so a
 *  scoped search counts hits inside a thinking block's absorbed tool call as the tool's, not
 *  the thinking's. `lower` transforms each own text before it is measured (both pages search
 *  lowercase), so the spans index the transformed text. */
function recordTextParts(b, strip, lower = s => s) {
  const all = [], parts = [];
  let length = 0;
  (function walk(record) {
    const own = lower(ownTextParts(record, strip).join("\n"));
    if (own) {
      if (all.length) { all.push("\n"); length++; }
      const start = length;
      all.push(own);
      length += own.length;
      parts.push({ start, end: length, mask: directMask(record.kind) });
    }
    for (const p of record.body || []) if (p.p === "blocks") for (const item of p.items || []) walk(item);
  })(b);
  return { text: all.join(""), parts };
}

/** The `uatobrew:` scope grammar (the same syntax as the TUI's `/` search, case-insensitive):
 *  a run of DISTINCT letters — u (your turns), a (agent replies), t (thinking), o (all tools),
 *  b (bash output), r (reads), e (edits/writes), w (whole words) — then a colon. Order-free, so
 *  `aut:` ≡ `uat:`; `+` (the old separator) still parses; a repeated letter is a word, not a
 *  scope; a leading `:` escapes a scope-shaped literal. Returns `{ set, len }` or null. */
function parseScope(needle) {
  if (needle.charAt(0) === ":") return { set: null, len: 1 };
  const m = /^([uatobrew+]{1,15}):/i.exec(needle);
  if (!m) return null;
  const set = { u: false, a: false, t: false, o: false, b: false, r: false, e: false, w: false };
  const run = m[1].toLowerCase();
  for (let i = 0; i < run.length; i++) {
    const p = run.charAt(i);
    if (p === "+") continue;
    if (set[p]) return null;
    set[p] = true;
  }
  if (!activeLetters(set).length) return null;
  return { set, len: m[0].length };
}

/** The scope classes of a set, in canonical order (without `w`). */
function scopeLetters(set) {
  return ["u", "a", "t", "o", "b", "r", "e"].filter(k => set && set[k]);
}

/** Every active letter of a set, `w` included. */
function activeLetters(set) {
  return ["u", "a", "t", "o", "b", "r", "e", "w"].filter(k => set && set[k]);
}

/** The bitmask of a scope set's classes; 0 means no scope (everything). */
function scopeMask(set) {
  let mask = 0;
  for (const k of scopeLetters(set)) mask |= CLASS_BIT[k];
  return mask;
}

/** Above this many characters of haystack a page searches on Enter, not on every keystroke
 *  (#104): the owner's threshold, "don't try to do progressive search above ~10 MB". */
const LIVE_SEARCH_LIMIT = 10 * 1024 * 1024;

/** A cheap size of a record's searchable text — the raw part and head strings, nested records
 *  included — for deciding whether live search is affordable, without building the text. */
function recordTextSize(b) {
  let n = 0;
  (function walk(record) {
    const h = record.head || {};
    for (const k of ["summary", "badge", "preview", "name", "target", "att_name"]) if (h[k]) n += String(h[k]).length;
    for (const p of record.body || []) {
      if (p.p === "md" || p.p === "think") n += (p.h || "").length;
      else if (p.p === "pre" || p.p === "note") n += String(p.x ?? "").length;
      else if (p.p === "num") for (const r of p.rows || []) n += String(r[1] ?? "").length;
      else if (p.p === "diff") for (const r of p.rows || []) n += String(r[2] ?? "").length;
      else if (p.p === "blocks") for (const item of p.items || []) walk(item);
    }
  })(b);
  return n;
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

export { CLASS_BIT, directMask, ownTextParts, recordText, recordTextParts, recordTextSize, LIVE_SEARCH_LIMIT, parseScope, scopeLetters, activeLetters, scopeMask, stripTags, WORD_LEFT, WORD_RIGHT, wholeAt, countOcc };
