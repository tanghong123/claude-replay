// The search haystack and the hit rules both pages share (#111, design/rendering-parity-audit.md
// rows 5.3–5.4; the module #118 grows into). A record's searchable TEXT is what a reader can see
// of it: its head's summary, badge, preview, name, target and attachment name, then its body
// parts — markdown with the tags stripped, pre/note text, numbered source lines, diff lines —
// and the same for every record nested in it. Not its JSON: a query that is only a field name
// finds nothing. The scope classes (u/a/t/o), the tool letters and the whole-word rule live here too.
//
// Shared-module conventions (html_export/shared.rs): no imports, one trailing `export` line.
// `strip` is the page's HTML-to-text function (the classic page uses a scratch element; the app
// shell a regex) so the module stays DOM-free.

const CLASS_BIT = { u: 1, a: 2, t: 4, o: 8 };

/** The scope classes a record kind belongs to directly, as a bitmask. Every tool call is the one
 *  TOOLS class (#367): which tool it was is the record's `tool`, narrowed by `o(…)`. */
function directMask(k) {
  if (k === "user" || k === "command") return CLASS_BIT.u;
  if (k === "assistant") return CLASS_BIT.a;
  if (k === "think" || k === "act") return CLASS_BIT.t;
  return /^(bash|edit|write|read|skill|tool)$/.test(k) ? CLASS_BIT.o : 0;
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

/** The box's PREFIX (design/in-session-search.md §8, #367): at the very start of the box, a run of
 *  DISTINCT letters — u (your turns), a (agent replies), t (thinking), o (tools), w (whole words)
 *  — then a colon. The letters are case-insensitive and order-free (`au:` ≡ `ua:`); a repeated
 *  letter is a word, not a prefix. `o` may carry, in parentheses, the tools it narrows to —
 *  `o(BR)` by their letters ([`toolLetters`]), `o(Bash,Read)` by name, `o(mcp__github__*)` for a
 *  family — with nothing inside or no parentheses at all meaning every tool. Inside the
 *  parentheses case matters: `a` and `A` are different tools. A leading `:` escapes the prefix
 *  (`:ua:` is the text `ua:`). `scope:`, `tool:` and the letters b, r and e are gone (the owner,
 *  2026-10-02: old forms stop working). Returns `{ set, tools, len }` — `tools` the text between
 *  the parentheses, "" for every tool — or null when `s` does not start with a prefix. */
function parsePrefix(s) {
  s = String(s ?? "");
  const set = { u: false, a: false, t: false, o: false, w: false };
  let i = 0, tools = "";
  while (i < s.length && s.charAt(i) !== ":") {
    const k = s.charAt(i).toLowerCase();
    if (!(k in set) || set[k]) return null;
    set[k] = true;
    i++;
    if (k === "o" && s.charAt(i) === "(") {
      const close = s.indexOf(")", i);
      if (close === -1) return null;
      tools = s.slice(i + 1, close);
      if (/[\s():]/.test(tools)) return null;
      i = close + 1;
    }
  }
  if (i === 0 || s.charAt(i) !== ":") return null;
  return { set, tools, len: i + 1 };
}

/** The scope classes of a set, in canonical order (without `w`). */
function scopeLetters(set) {
  return ["u", "a", "t", "o"].filter(k => set && set[k]);
}

/** Every active letter of a set, `w` included. */
function activeLetters(set) {
  return ["u", "a", "t", "o", "w"].filter(k => set && set[k]);
}

/** Tools whose letter never changes (#367, the owner: "reserve letters for the most common tools
 *  so they never change"), chosen by use: on 2026-10-02 the newest 400 Claude Code sessions here
 *  called Bash in 394, Read in 265, Write in 246, Edit in 192, Agent in 56, Skill in 27,
 *  AskUserQuestion in 17 and WebFetch in 16. A reserved letter is never handed to another tool,
 *  even in a session that never called its own — so `o(B)` is Bash in every session, and a query
 *  saved by these letters means the same everywhere. Every other tool's letter depends on the
 *  session; a saved query names it in full. Names are the ones a record carries, which are the
 *  ones the page shows: Claude Code shows an Edit (or MultiEdit) as `Update`, so `E` is Update,
 *  and the names `Edit` and `MultiEdit` are read as it ([`TOOL_ALIASES`]). */
const RESERVED_TOOLS = [["B", "Bash"], ["R", "Read"], ["W", "Write"], ["E", "Update"], ["A", "Agent"], ["S", "Skill"], ["Q", "AskUserQuestion"], ["F", "WebFetch"]];

/** Tool names a reader may type for the name a record carries (present.rs `display_name`). */
const TOOL_ALIASES = { edit: "Update", multiedit: "Update" };

/** A session's tools with their LETTERS (§8.2), from `counts` (`{name: calls}`): most-used first.
 *  A reserved tool takes its reserved letter; any other takes its initial when free, else the other
 *  case, else another letter of its name, else any free letter, then a digit — never a reserved
 *  one. MCP tools take ONE letter per server, as the family `mcp__<server>__*`. Returns
 *  `[{key, label, count, family, letter}]`. */
function toolLetters(counts) {
  const entries = new Map();
  for (const [name, n] of Object.entries(counts || {})) {
    const mcp = /^mcp__([^_]+(?:_[^_]+)*)__/.exec(name);
    const key = mcp ? `mcp__${mcp[1]}__*` : name;
    const label = mcp ? mcp[1] : name;
    const e = entries.get(key) || { key, label, count: 0, family: !!mcp };
    e.count += n;
    entries.set(key, e);
  }
  const ordered = [...entries.values()].sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
  const reserved = new Map(RESERVED_TOOLS.map(([letter, name]) => [name, letter]));
  const used = new Set(RESERVED_TOOLS.map(([letter]) => letter)), out = [];
  const pool = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
  const swap = c => (c === c.toLowerCase() ? c.toUpperCase() : c.toLowerCase());
  for (const e of ordered) {
    if (reserved.has(e.key)) { out.push({ ...e, letter: reserved.get(e.key) }); continue; }
    const own = [...e.label].filter(c => /[A-Za-z0-9]/.test(c));
    const tries = [own[0], own[0] && swap(own[0]), ...own.slice(1), ...own.slice(1).map(swap), ...pool];
    const letter = tries.find(c => c && !used.has(c));
    if (!letter) continue;
    used.add(letter);
    out.push({ ...e, letter });
  }
  return out;
}

/** The tools an `o(…)` names, as `[{name, prefix}]` for [`toolMatches`]: the NAMES when every
 *  comma-separated part names a tool of the session or a reserved one (or ends in `*`, a family),
 *  else the LETTERS when the text is one run of known letters, else the parts as names (which match
 *  nothing they do not name). `letters` is [`toolLetters`]'s answer for the session; the reserved
 *  letters resolve whether or not the session called their tool. */
function resolveTools(spec, letters) {
  const s = String(spec ?? "");
  const parts = s.split(",").filter(Boolean);
  if (!parts.length) return [];
  const asTool = key => (key.endsWith("*") ? { name: key.slice(0, -1), prefix: true } : { name: key, prefix: false });
  const known = new Map(Object.entries(TOOL_ALIASES));
  for (const [, name] of RESERVED_TOOLS) known.set(name.toLowerCase(), name);
  for (const e of letters || []) known.set(e.key.toLowerCase(), e.key);
  const named = parts.map(p => known.get(p.toLowerCase()) || p);
  if (parts.every(p => known.has(p.toLowerCase()) || p.endsWith("*"))) return dedupeTools(named.map(asTool));
  const byLetter = new Map(RESERVED_TOOLS);
  for (const e of letters || []) byLetter.set(e.letter, e.key);
  if (parts.length === 1 && [...s].every(c => byLetter.has(c))) return dedupeTools([...s].map(c => asTool(byLetter.get(c))));
  return dedupeTools(named.map(asTool));
}

function dedupeTools(tools) {
  const seen = new Set();
  return tools.filter(t => {
    const key = (t.prefix ? "^" : "=") + t.name.toLowerCase();
    return seen.has(key) ? false : (seen.add(key), true);
  });
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
const CLASS_ORDER = ["u", "a", "t", "o"];

/** A needle shorter than this searches nothing: two characters of noise would mark a page. */
const MIN_NEEDLE = 2;

/** An empty per-class accumulator. */
function zeroCounts() {
  return { u: 0, a: 0, t: 0, o: 0 };
}

/** The box as SPANS (design/in-session-search.md §8): the PREFIX, when the box starts with one,
 *  and the TEXT after it, with offsets in `raw` — so the reader (`splitQuery`) and the writer
 *  (`writePrefix`) share one reading and the writer replaces the prefix without touching a
 *  character of the reader's own words. The prefix is read only at the very start (the owner,
 *  2026-10-02); anywhere else `ua:` is text. What follows its colon in the same token is text
 *  (`ua:deploy`). A leading `:` escapes it: `:ua: x` searches `ua: x`. */
function querySpans(raw) {
  const s = String(raw ?? "");
  const lead = s.length - s.trimStart().length;
  const spans = [];
  let at = lead;
  if (s.charAt(lead) === ":" && s.length > lead + 1) {
    spans.push({ kind: "text", start: lead, end: s.trimEnd().length, escaped: true });
    return spans;
  }
  const prefix = parsePrefix(s.slice(lead));
  if (prefix) {
    spans.push({ kind: "scope", start: lead, end: lead + prefix.len, set: prefix.set, tools: prefix.tools });
    at = lead + prefix.len;
  }
  const rest = s.slice(at);
  const from = at + (rest.length - rest.trimStart().length), to = s.trimEnd().length;
  if (to > from) spans.push({ kind: "text", start: from, end: to });
  return spans;
}

/** A raw box value → what to search (§8): the text (lowercased in `lc`), the scope set (null for
 *  everything), the tools `o(…)` names and whether the text is too short to run. `resolve` turns the
 *  parenthesized text into `[{name, prefix}]` — a page passes its session's letters through
 *  [`resolveTools`]; without one, the text is read as names. The tools narrow the TOOLS class and
 *  nothing else: `uo(B): x` is `x` in your turns or in Bash calls. */
function splitQuery(raw, minLen = MIN_NEEDLE, resolve) {
  const s = String(raw ?? "").trim();
  let set = null, spec = "", text = "";
  for (const span of querySpans(s)) {
    if (span.kind === "scope") {
      set = span.set;
      spec = span.tools;
    } else {
      text = s.slice(span.start + (span.escaped ? 1 : 0), span.end);
    }
  }
  text = text.trim();
  const tools = spec ? (resolve ? resolve(spec) : resolveTools(spec, [])) : [];
  return {
    needle: text,
    lc: text.toLowerCase(),
    set,
    scoped: set ? { set } : null,
    tools,
    toolSpec: spec,
    // A prefix with no text is a whole query; text too short to run is too short whatever rides
    // beside it — a one-character needle silently dropped would answer a question nobody asked.
    tooShort: text.length > 0 && text.length < minLen,
  };
}

/** Does a record's `tool` answer one of `tools` (as `splitQuery` returns them)? An empty list
 *  asks nothing and admits every record — the caller decides whether that means "no filter".
 *  Case-insensitive for names, because the box is typed by hand. */
function toolMatches(tool, tools) {
  if (!tools || !tools.length) return true;
  if (!tool) return false;
  const lc = String(tool).toLowerCase();
  return tools.some(t => (t.prefix ? lc.startsWith(t.name.toLowerCase()) : lc === t.name.toLowerCase()));
}

/** Whether one text part is inside the search: in a wanted class, and — for a tool call, when
 *  `o(…)` names tools — one of them. No scope at all is everything. */
function partWanted(part, wanted, tools) {
  if (!wanted) return true;
  if (!(part.mask & wanted)) return false;
  return !(part.mask & CLASS_BIT.o) || toolMatches(part.tool, tools);
}

/** One record's hits: the total IN SCOPE, and — always, whatever the scope — every part's hits
 *  added into `counts` by the classes that part carries. The scope decides what is COUNTED for
 *  the reader, never what the per-class row shows, which is what makes an unscoped search still
 *  fill the scope rows a reader is about to choose from. */
function countRecord(text, parts, lc, whole, wanted, counts, tools) {
  let inScope = 0;
  for (const part of parts || []) {
    const n = countOcc(text.slice(part.start, part.end), lc, whole);
    if (!n) continue;
    if (counts) for (const k of CLASS_ORDER) if (part.mask & CLASS_BIT[k]) counts[k] += n;
    if (partWanted(part, wanted, tools)) inScope += n;
  }
  // Unscoped, the record's own text is the truth — a part-sum would drop the bytes no part
  // claims (a head's summary, an agent or attachment record) that the classic page counts.
  return wanted ? inScope : countOcc(text, lc, whole);
}

/** Does a record, or anything nested in it, hold a call one of `tools` names (#292)? The prefix's
 *  own question for a query with no text: the chain is `shared/filter.js`'s, and this is the
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

/** The prefix for a set of scope letters and an `o(…)` text, as the box holds it: canonical order,
 *  `o(BR)` when tools are named — which implies `o` — and "" for no scope at all. */
function prefixOf(letters, tools) {
  const on = new Set(letters || []);
  if (tools) on.add("o");
  const run = ["u", "a", "t", "o", "w"].filter(k => on.has(k)).map(k => (k === "o" && tools ? `o(${tools})` : k)).join("");
  return run ? run + ":" : "";
}

/** The count a reader sees: "12 hits", "3 hits in ua", "4 hits in uo(B)", "1 hit · whole words". */
function countLabel(total, set, whole, tools) {
  const scope = prefixOf(scopeLetters(set), scopeLetters(set).includes("o") ? tools || "" : "").slice(0, -1);
  return total + " hit" + (total === 1 ? "" : "s")
    + (scope ? " in " + scope : "")
    + (whole ? " · whole words" : "");
}

/** The box value a menu writes back (§8): the prefix replaced by the one for `letters` and `tools`
 *  (the text inside `o(…)`, "" for none), the reader's own words kept as typed. No letters and no
 *  tools means no prefix: the box says "everything" by saying nothing. */
function writePrefix(raw, letters, tools = "") {
  const s = String(raw ?? "");
  const scope = querySpans(s).find(span => span.kind === "scope");
  const rest = (scope ? s.slice(scope.end) : s).trim();
  return [prefixOf(letters, tools), rest].filter(Boolean).join(" ");
}

export { CLASS_BIT, CLASS_ORDER, MIN_NEEDLE, directMask, ownTextParts, recordText, recordTextParts, recordTextSize, LIVE_SEARCH_LIMIT, parsePrefix, scopeLetters, activeLetters, scopeMask, RESERVED_TOOLS, toolLetters, resolveTools, splitQuery, querySpans, toolMatches, recordHasTool, zeroCounts, countRecord, countLabel, prefixOf, writePrefix, stripTags, WORD_LEFT, WORD_RIGHT, wholeAt, countOcc };
