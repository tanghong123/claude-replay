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
      // `tool` (#292) is the record whose own text this is — the name a `tool:` facet asks for,
      // so "Bash calls whose text matches" is answered per PART: a word in the thinking that
      // absorbed a Bash call is the thinking's, exactly as the scope mask beside it already is.
      parts.push({ start, end: length, mask: directMask(record.kind), tool: record.tool || "" });
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

/* ── one search, two pages (#118) ─────────────────────────────────────────
 * Everything above is the vocabulary; what follows is the SEARCH ITSELF, as far as it can go
 * without a DOM. Both pages ran the same arithmetic in their own words — the query split, the
 * per-scope counts, the count label, the prefix the scope menu writes back into the box — and a
 * rule that lives twice drifts. The pages keep what is theirs: their caches, their marks, their
 * controls. The classic page is the reference; where the two disagreed, its rule is the one
 * written here. */

/** The scope classes in canonical order (no `w`) — the order every count row is read in. */
const CLASS_ORDER = ["u", "a", "t", "o", "b", "r", "e"];

/** A needle shorter than this searches nothing: two characters of noise would mark a page. */
const MIN_NEEDLE = 2;

/** An empty per-class accumulator. */
function zeroCounts() {
  return { u: 0, a: 0, t: 0, o: 0, b: 0, r: 0, e: 0 };
}

/** A scope letter run (`ub`, `u+b`, `wt`) → its set, or null: each letter once, at least one. */
function scopeSetOf(run) {
  const set = { u: false, a: false, t: false, o: false, b: false, r: false, e: false, w: false };
  for (const ch of String(run).toLowerCase()) {
    if (ch === "+") continue;
    if (!(ch in set) || set[ch]) return null;
    set[ch] = true;
  }
  return activeLetters(set).length ? set : null;
}

/** A `tool:` value → `[{name, prefix}]`: comma-separated names, a trailing `*` for a family. */
function toolsOf(value) {
  const out = [];
  for (const piece of String(value).split(",")) {
    const prefix = piece.endsWith("*");
    const name = prefix ? piece.slice(0, -1) : piece;
    if (name.length) out.push({ name, prefix });
  }
  return out;
}

const FACET_KEY = /^(tools?|scope):(.*)$/i;
const BARE_SCOPE = /^([uatobrew+]{1,15}):/i;

/** The box as SPANS (design/in-session-search.md §8): each whitespace-separated token is TEXT, a
 *  SCOPE facet or a TOOLS facet, with its offsets in `raw`, so the readers (`splitQuery`) and the
 *  writers (`writePrefix`, `writeTools`) share one reading and a writer can replace facets without
 *  touching a character of the reader's own text.
 *  - `tool:A,B` (`tools:` too) and `scope:ub` are facets ANYWHERE — at the start or after a space,
 *    so `about:blank` is text: `about` is not a key. An empty or invalid value is text (a reader
 *    mid-type is not suddenly searching nothing; `scope:xyz` is not a scope).
 *  - A leading `:` escapes ONE token, up to the next space (owner, 2026-09-27): `:tools:` is the
 *    literal `tools:`, and the rest of the box parses as usual.
 *  - The bare letter run at the very start (`ub:x`, `ub: x`) is still a scope — the TUI's `/` and
 *    the reader's hands know it — unless nothing follows it (`auto:` searches itself). What follows
 *    its colon in the same token is read as a token of its own, so `ub:tool:Read` is two facets. */
function querySpans(raw) {
  const s = String(raw ?? "");
  const tokens = [];
  const rx = /\S+/g;
  let m;
  while ((m = rx.exec(s))) tokens.push({ t: m[0], start: m.index });
  const spans = [];
  tokens.forEach(({ t, start }, i) => {
    if (i === 0) {
      const bare = BARE_SCOPE.exec(t);
      const set = bare && scopeSetOf(bare[1]);
      if (set && (t.length > bare[0].length || tokens.length > 1)) {
        spans.push({ kind: "scope", start, end: start + bare[0].length, set, bare: true });
        start += bare[0].length;
        t = t.slice(bare[0].length);
        if (!t) return;
      }
    }
    if (t.length > 1 && t.charAt(0) === ":") {
      spans.push({ kind: "text", start, end: start + t.length, escaped: true });
      return;
    }
    const key = FACET_KEY.exec(t);
    if (key) {
      if (key[1].toLowerCase() === "scope") {
        const set = scopeSetOf(key[2]);
        if (set) return void spans.push({ kind: "scope", start, end: start + t.length, set });
      } else {
        const tools = toolsOf(key[2]);
        if (tools.length) return void spans.push({ kind: "tools", start, end: start + t.length, tools });
      }
    }
    spans.push({ kind: "text", start, end: start + t.length });
  });
  return spans;
}

/** `raw` with every span of `kinds` removed — each with the whitespace before it, so what is left
 *  of `a tools:Bash b` is `a b` — and every other character kept as typed (escapes included). */
function withoutSpans(raw, spans, kinds) {
  const s = String(raw ?? "");
  let out = "";
  let at = 0;
  for (const span of spans) {
    if (!kinds.includes(span.kind)) continue;
    let from = span.start;
    while (from > at && /\s/.test(s.charAt(from - 1))) from--;
    out += s.slice(at, from);
    at = span.end;
  }
  return (out + s.slice(at)).trim();
}

/** A raw box value → what to search (§8): the text (lowercased in `lc`), the scope set (the union
 *  of every scope facet; null for everything), the tools (the union of every tools facet) and
 *  whether the text is too short to run. The text is what is left once the facets are out, with
 *  each escaped token's colon dropped. */
function splitQuery(raw, minLen = MIN_NEEDLE) {
  const s = String(raw ?? "").trim();
  const spans = querySpans(s);
  let set = null;
  const tools = [];
  const seen = new Set();
  let text = "";
  let prevEnd = 0;
  for (const span of spans) {
    if (span.kind === "scope") {
      set = set || { u: false, a: false, t: false, o: false, b: false, r: false, e: false, w: false };
      for (const k of activeLetters(span.set)) set[k] = true;
    } else if (span.kind === "tools") {
      for (const tool of span.tools) {
        const key = (tool.prefix ? "^" : "=") + tool.name.toLowerCase();
        if (!seen.has(key)) {
          seen.add(key);
          tools.push(tool);
        }
      }
    } else {
      // The reader's own spacing between their own words is kept; a facet's space went with it.
      const piece = s.slice(span.start + (span.escaped ? 1 : 0), span.end);
      text += (text ? s.slice(prevEnd, span.start) : "") + piece;
    }
    prevEnd = span.end;
  }
  text = text.trim();
  return {
    needle: text,
    lc: text.toLowerCase(),
    set,
    scoped: set ? { set } : null,
    tools,
    // A facet with no text is a whole query; text too short to run is too short whatever rides
    // beside it — a one-character needle silently dropped would answer a question nobody asked.
    tooShort: text.length > 0 && text.length < minLen,
  };
}

/** A `tool:` VALUE as the reader may write it: a bare name (`tool:Bash`), or a name ending in `*`
 *  for a family (`tool:mcp__github__*`, `tool:mcp__*`) — the prefix match the classic page's
 *  `[data-tool^=]` filter already does. Quoting is not part of the grammar: a tool name has no
 *  spaces (the value ends at whitespace), and a name that did could not be typed into the box
 *  today either. */
const TOOL_TOKEN = /(?:^|\s)tool:([^\s]+)/gi;

/** Pull every `tool:` token out of `raw`, newest rules in `splitQuery`. Returns an array of
 *  `{name, prefix}` — `prefix` true for the `*` form, with the star removed — carrying `rest`:
 *  what is left of the box for the text search. Duplicates collapse; an empty value (`tool:`)
 *  is not a token and stays in the text, so a reader mid-type is not searching nothing. */
function takeTools(raw) {
  const out = [];
  const seen = new Set();
  const rest = String(raw ?? "").replace(TOOL_TOKEN, (whole, value) => {
    const prefix = value.endsWith("*");
    const name = prefix ? value.slice(0, -1) : value;
    if (!name.length) return whole;
    const key = (prefix ? "^" : "=") + name.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      out.push({ name, prefix });
    }
    // The match ATE the space before the token, so it is removed with it: what is left of
    // `a tool:Bash b` is `a b`, not `a  b`, which would search two literal spaces. The reader's
    // own spacing between their own words is untouched.
    return "";
  });
  out.rest = rest;
  return out;
}

/** Does a record's `tool` answer one of `tools` (as `splitQuery` returns them)? An empty list
 *  asks nothing and admits every record — the caller decides whether that means "no filter".
 *  Case-insensitive, because the box is typed by hand. */
function toolMatches(tool, tools) {
  if (!tools || !tools.length) return true;
  if (!tool) return false;
  const lc = String(tool).toLowerCase();
  return tools.some(t => (t.prefix ? lc.startsWith(t.name.toLowerCase()) : lc === t.name.toLowerCase()));
}

/** The `tool:` token for a set of names, as the box would hold them — what a menu writes when
 *  the reader ticks rows (#292: the box is the truth, so a click and a typed token are one
 *  thing). Every tools facet already in the box, wherever it sat, is replaced by ONE comma-joined
 *  `tool:A,B` token at the front (§8; owner: `tool:`, the singular, is the written form). A name ending in `*` is passed through as the family form. */
function writeTools(raw, names) {
  const rest = withoutSpans(raw, querySpans(raw), ["tools"]);
  const token = (names || []).length ? "tool:" + names.join(",") : "";
  return [token, rest].filter(Boolean).join(" ");
}

/** One record's hits: the total IN SCOPE, and — always, whatever the scope — every part's hits
 *  added into `counts` by the classes that part carries. The scope decides what is COUNTED for
 *  the reader, never what the per-class row shows, which is what makes an unscoped search still
 *  fill the scope rows a reader is about to choose from. */
function countRecord(text, parts, lc, whole, wanted, counts, tools) {
  let inScope = 0;
  const byTool = !!(tools && tools.length);
  for (const part of parts || []) {
    const n = countOcc(text.slice(part.start, part.end), lc, whole);
    if (!n) continue;
    if (counts) for (const k of CLASS_ORDER) if (part.mask & CLASS_BIT[k]) counts[k] += n;
    if (byTool && !toolMatches(part.tool, tools)) continue;
    if (!wanted || part.mask & wanted) inScope += n;
  }
  // Unscoped, the record's own text is the truth — a part-sum would drop the bytes no part
  // claims (a head's summary, an agent or attachment record) that the classic page counts.
  // A TOOL facet (#292) is a claim about which call the words sit in, so there the parts ARE the
  // answer: text no part claims belongs to no tool.
  return wanted || byTool ? inScope : countOcc(text, lc, whole);
}

/** Does a record, or anything nested in it, hold a call one of `tools` names (#292)? The facet's
 *  own question, for a query with no text: the chain is `shared/filter.js`'s, and this is the
 *  predicate a page hands it. */
function recordHasTool(b, tools) {
  if (!tools || !tools.length) return false;
  if (toolMatches(b.tool, tools)) return true;
  for (const p of b.body || []) {
    if (p.p !== "blocks") continue;
    for (const item of p.items || []) if (recordHasTool(item, tools)) return true;
  }
  return false;
}

/** The count a reader sees: "12 hits", "3 hits in ua", "1 hit · whole words". */
function countLabel(total, set, whole) {
  const letters = scopeLetters(set);
  return total + " hit" + (total === 1 ? "" : "s")
    + (letters.length ? " in " + letters.join("") : "")
    + (whole ? " · whole words" : "");
}

/** The box value the scope menu writes back (§8): every scope facet — the bare prefix or a
 *  `scope:` token, wherever it sat — replaced by ONE `scope:<letters>` token at the front, the
 *  reader's own words kept as typed. No letters means no token: the box says "everything" by
 *  saying nothing. */
function writePrefix(raw, letters) {
  const rest = withoutSpans(raw, querySpans(raw), ["scope"]);
  const token = letters.length ? "scope:" + letters.join("") : "";
  return [token, rest].filter(Boolean).join(" ");
}

export { CLASS_BIT, CLASS_ORDER, MIN_NEEDLE, directMask, ownTextParts, recordText, recordTextParts, recordTextSize, LIVE_SEARCH_LIMIT, parseScope, scopeLetters, activeLetters, scopeMask, splitQuery, querySpans, takeTools, toolMatches, writeTools, recordHasTool, zeroCounts, countRecord, countLabel, writePrefix, stripTags, WORD_LEFT, WORD_RIGHT, wholeAt, countOcc };
