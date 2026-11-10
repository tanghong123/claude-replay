// The session view's stylesheet and the app's, derived from reference.css and production.css
// (#s429 stage 5b, design/session-view-component.md). `extract-agent-monitor-demo.mjs` writes both,
// and the frontend contract derives them again and compares them byte for byte, so neither is ever
// edited by hand. The two sources stay as they are: reference.css is the demo's own extraction, and
// production.css is where production rules are written.
//
// What it makes:
// - `session-view.css`: every rule that styles what the view draws, with each selector hung under
//   the view's element. A plain selector gets the prefix `:where(#sessionView) `, which weighs
//   nothing. A rule that already hangs on `#sessionView` (stage 5a) keeps its own. A rule on the
//   page's root (`:root`, `html`, `body`) becomes a rule on the view's element, carrying only what
//   the view inherits from the page: the tokens and the inherited properties (font, colour…).
//   Nothing in it styles the page around the view.
// - `app-shell.css`: everything else, as it was written: the app's own chrome, the rules that read
//   an app state on `#app`, and the page's own rules. A rule that styles both the app's elements and
//   the view's (a glyph button, a generic state class, a bare `button`) is in both files.
//
// The app shell loads `app-shell.css` first and `session-view.css` after it. A rule in both files is
// then the same rule twice, its later copy in the view's file, so for an element of the view the
// view's file alone decides, in the order and at the weight the sources gave every rule.
//
// The weight is kept EXACTLY, so no rule's standing against another of ours moves:
// - the prefix is `:where(…)`, which weighs nothing;
// - where a selector named the page's root, its stand-in on the view's element weighs the same:
//   `.session-view` (the view's own class) for `:root`, `:is(*,div)` for `html` or `body` (it
//   matches any element and weighs one type), and the conditions they carried (`[data-theme="dark"]`,
//   native mode's `[data-shared-codex]`) move onto the view's element, which carries both;
// - `:is(#sessionView,#app)` (stage 5a's shared hook) becomes `#sessionView`, an id as before.
// `deriveStylesheets` checks every selector it writes against the one it read.

// ── CSS, read just well enough ────────────────────────────────────────────────────────────────────

/** Skip a string starting at `i` (its quote), returning the index after its closing quote. */
function skipString(text, i) {
  const quote = text[i];
  let k = i + 1;
  while (k < text.length && text[k] !== quote) k += text[k] === "\\" ? 2 : 1;
  return k + 1;
}

/** The CSS without its comments; strings are left as they are. */
export function stripComments(text) {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === '"' || c === "'") {
      const end = skipString(text, i);
      out += text.slice(i, end);
      i = end;
    } else if (c === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      i = end < 0 ? text.length : end + 2;
    } else {
      out += c;
      i += 1;
    }
  }
  return out;
}

/** The index of the `}` closing the block whose `{` is at `open`. */
function blockEnd(text, open) {
  let depth = 0;
  for (let i = open; i < text.length; i += 1) {
    const c = text[i];
    if (c === '"' || c === "'") i = skipString(text, i) - 1;
    else if (c === "{") depth += 1;
    else if (c === "}" && --depth === 0) return i;
  }
  throw new Error(`unclosed block at ${open}`);
}

// At-rules whose block holds rules. Any other at-rule's block is kept whole (`@keyframes`).
const GROUPING = /^@(media|supports|container|layer|document)\b/;

/** Comment-free CSS as nodes: `{kind: "rule", prelude, body}`, `{kind: "group", prelude, children}`
 *  and `{kind: "at", prelude, body}` (a block kept whole; `body: null` for a statement). */
export function parseCss(text) {
  const nodes = [];
  let i = 0;
  while (i < text.length) {
    let k = i;
    while (k < text.length && text[k] !== "{" && text[k] !== ";" && text[k] !== "}") {
      k = text[k] === '"' || text[k] === "'" ? skipString(text, k) : k + 1;
    }
    const prelude = text.slice(i, k).trim();
    if (k >= text.length) {
      if (prelude) throw new Error(`stray text at the end: ${prelude.slice(0, 60)}`);
      break;
    }
    if (text[k] === "}") throw new Error(`stray } at ${k}`);
    if (text[k] === ";") {
      if (prelude) nodes.push({ kind: "at", prelude, body: null });
      i = k + 1;
      continue;
    }
    const end = blockEnd(text, k);
    const body = text.slice(k + 1, end);
    if (GROUPING.test(prelude)) nodes.push({ kind: "group", prelude, children: parseCss(body) });
    else if (prelude.startsWith("@")) nodes.push({ kind: "at", prelude, body: body.trim() });
    else nodes.push({ kind: "rule", prelude, body: body.trim() });
    i = end + 1;
  }
  return nodes;
}

/** Split at the top-level separators (outside parentheses, brackets and strings). */
export function splitTop(text, separator = ",") {
  const parts = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (c === '"' || c === "'") i = skipString(text, i) - 1;
    else if (c === "\\") i += 1;
    else if (c === "(" || c === "[") depth += 1;
    else if (c === ")" || c === "]") depth -= 1;
    else if (c === separator && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(text.slice(start));
  return parts.map(part => part.trim()).filter(Boolean);
}

/** A complex selector as its compounds, each with the combinator before it: `""` for the first,
 *  `" "` for a descendant, `">"`, `"+"` or `"~"`. */
export function compounds(selector) {
  const out = [];
  let depth = 0;
  let current = "";
  let pending = null;
  const flush = () => {
    if (!current) return;
    out.push({ comb: out.length ? (pending ?? " ") : "", text: current });
    current = "";
    pending = null;
  };
  for (let i = 0; i < selector.length; i += 1) {
    const c = selector[i];
    if (depth === 0 && /\s/.test(c)) {
      flush();
    } else if (depth === 0 && (c === ">" || c === "+" || c === "~")) {
      flush();
      pending = c;
    } else if (c === '"' || c === "'") {
      const end = skipString(selector, i);
      current += selector.slice(i, end);
      i = end - 1;
    } else if (c === "\\") {
      current += selector.slice(i, i + 2);
      i += 1;
    } else {
      if (c === "(" || c === "[") depth += 1;
      if (c === ")" || c === "]") depth -= 1;
      current += c;
    }
  }
  flush();
  return out;
}

/** Compounds back into a selector, written tight as the sources write them. */
export function joinCompounds(parts) {
  return parts.map(({ comb, text }, k) => (k === 0 ? text : comb === " " ? ` ${text}` : `${comb}${text}`)).join("");
}

// Pseudo-classes whose argument is a selector list weighing what its heaviest member weighs, and the
// old one-colon pseudo-elements, which weigh a type.
const BY_ARGUMENT = new Set(["is", "not", "has", "matches", "-webkit-any", "-moz-any"]);
const LEGACY_ELEMENTS = new Set(["before", "after", "first-line", "first-letter"]);

/** A selector's specificity, `[ids, classes, types]` (Selectors 4). */
export function specificity(selector) {
  const sum = [0, 0, 0];
  const add = s => { sum[0] += s[0]; sum[1] += s[1]; sum[2] += s[2]; };
  const most = list => splitTop(list).map(specificity).reduce((m, s) => (compare(s, m) > 0 ? s : m), [0, 0, 0]);
  let i = 0;
  const ident = () => {
    const m = /^-?(?:[A-Za-z_]|\\.)(?:[A-Za-z0-9_-]|\\.)*/.exec(selector.slice(i));
    i += m ? m[0].length : 0;
    return m ? m[0] : "";
  };
  const argument = () => {
    const open = i;
    let depth = 0;
    for (; i < selector.length; i += 1) {
      if (selector[i] === '"' || selector[i] === "'") i = skipString(selector, i) - 1;
      else if (selector[i] === "(") depth += 1;
      else if (selector[i] === ")" && --depth === 0) break;
    }
    i += 1;
    return selector.slice(open + 1, i - 1);
  };
  while (i < selector.length) {
    const c = selector[i];
    if (c === "#") { i += 1; ident(); add([1, 0, 0]); }
    else if (c === ".") { i += 1; ident(); add([0, 1, 0]); }
    else if (c === "[") { const end = selector.indexOf("]", i); i = end + 1; add([0, 1, 0]); }
    else if (c === ":" && selector[i + 1] === ":") {
      i += 2; ident();
      if (selector[i] === "(") argument();
      add([0, 0, 1]);
    } else if (c === ":") {
      i += 1;
      const name = ident().toLowerCase();
      const arg = selector[i] === "(" ? argument() : null;
      if (name === "where") continue;
      if (BY_ARGUMENT.has(name)) add(most(arg));
      else if (/^nth-(last-)?child$/.test(name) && arg && / of /.test(arg)) { add([0, 1, 0]); add(most(arg.slice(arg.indexOf(" of ") + 4))); }
      else if (LEGACY_ELEMENTS.has(name)) add([0, 0, 1]);
      else add([0, 1, 0]);
    } else if (c === "*") { i += 1; }
    else if (/[A-Za-z_-]/.test(c)) { ident(); add([0, 0, 1]); }
    else if (c === '"' || c === "'") { i = skipString(selector, i); }
    else { i += 1; }
  }
  return sum;
}

function compare(a, b) {
  return a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
}

/** A rule's declarations as `[property, text]`, the text as written (`--x:1`, `color:red!important`). */
export function declarations(body) {
  return splitTop(body, ";").map(text => [text.slice(0, text.indexOf(":")).trim().toLowerCase(), text]);
}

// ── whose a rule is ──────────────────────────────────────────────────────────────────────────────
// What `app-shell.html` and `app.js` (with `session-list.js`) name is the app's, and every other
// class or id is the view's: its cards, rows, code lines and tables are drawn by components.js and
// session-view.js, never written in the shell. The lists below are the app's names that a selector
// uses, sorted by what they are; the contract checks they still cover what the app names.

/** The app's own chrome, which the view never draws: the session list and its tree, the rail and its
 *  flyout, the arrangement strip and the section menu, the session filter, the brand, the drawer and
 *  a wide phone's column, the session search layer (⌘K), the confirm layer and the toast. `.primary`
 *  is the confirm layer's button: the view's own primary buttons are `.sv-primary`, a name the
 *  collaboration app does not style (#s438), so the view never draws `.primary`. */
export const APP_CHROME = Object.freeze([
  "#collapseBtn", "#expandBtn", "#filterBtn", "#sidebarCollapse",
  ".agent-label", ".agent-monogram",
  ".arrange-app", ".arrange-note", ".arrange-strip", ".arrange-strip-box", ".arrange-strip-close", ".arrange-strip-text", ".arrange-text",
  ".arranged-empty", ".arranged-group", ".arranged-head", ".arranged-label", ".arranged-rest", ".arranged-view",
  ".attention-count", ".attention-filter", ".attention-mini",
  ".brand", ".brand-icon",
  ".column-pane", ".column-switch", ".column-switch-count",
  ".confirm-layer", ".dialog-actions", ".primary",
  ".drawer-handle", ".drawer-scrim", ".filter-mini", ".head-actions",
  ".nav", ".nav-tooltip", ".navbtn", ".no-results",
  ".rail-flyout", ".rail-flyout-filter", ".rail-flyout-head", ".rail-flyout-tree",
  ".result-copy", ".result-kind", ".result-why",
  ".search-input", ".search-layer", ".search-result", ".search-result-stale", ".search-results", ".searchbox",
  ".section-menu-name", ".section-view-menu", ".section-view-scrim",
  ".session-age", ".session-end", ".session-filter", ".session-filter-options", ".session-state",
  ".side-head", ".sidebar", ".sidebar-collapse", ".sidebar-mini", ".sidebar-mini-agent", ".sidebar-mini-agents", ".sidebar-mini-badge", ".sidebar-mini-button", ".sidebar-mini-separator", ".sidebar-resizer",
  ".toast", ".tree", ".tree-action", ".tree-agent", ".tree-context", ".tree-forks", ".tree-main", ".tree-project", ".tree-project-more", ".tree-row", ".tree-side", ".tree-title",
  ".write-control", ".write-help", ".write-info",
]);

/** The app's states, kept on `#app` (stage 5a's groups): its rail, the drawer, a wide phone's column
 *  and its pane, and the list's arrangement. A rule that reads one is the app placing or styling the
 *  view from outside, and stays the app's (stage 6 lays the view out by its own pane's size). */
export const APP_STATES = Object.freeze([
  ".column-pane-on", ".mobile-detail", ".phone-column-off", ".sessions-arranged", ".sessions-flat", ".sidebar-off", ".sidebar-tight",
]);

/** Names the app's chrome and the view both draw: the glyph button, generic states and labels, and
 *  the popover rows the session filter borrows from the outline's menu. A rule that names only these
 *  (or no name at all: `button`, `*`) styles both, and is in both files. */
export const SHARED = Object.freeze([
  ".active", ".count", ".current", ".disabled", ".dragging", ".icon", ".iconbtn", ".label", ".mode-switch", ".navigator-options", ".on", ".phone-pane-body",
  ".project", ".scope-check", ".scope-count", ".scope-menu-action", ".scope-menu-divider", ".scope-menu-head", ".scope-menu-list", ".scope-option",
  ".selected", ".session", ".show", ".smallbtn",
]);

/** Common words the app draws as modifiers (on its rows, layers and hints) that the view does not
 *  draw today but may: like `SHARED`, they decide nothing alone. Beside a chrome name the rule is the
 *  app's (`.navbtn .hint`); beside a name of the view's it is the view's (`.process-row.warn`,
 *  `.transcript .hint`); alone it is in both files (`.dim`). */
export const COMMON = Object.freeze([
  ".arranged", ".dialog", ".dim", ".flat", ".hint", ".in-group", ".is-absent", ".is-hidden", ".is-member", ".is-side", ".kbd",
  ".production-open", ".sheet", ".warn",
]);

/** The classes the view's own element carries: its class and its state (stages 2 and 5a). The pane's
 *  `preview-off`, `preview-full` and `resizing` are copied onto `#app` for its grid as well. A rule
 *  led by one may style the view's element itself. */
export const ROOT_CLASSES = Object.freeze([
  ".session-view", ".composing", ".keyboard-up", ".pane-open", ".preview-full", ".preview-off", ".resizing", ".wide", ".wrap-code",
]);

/** What the view draws that the page shows OUTSIDE `#sessionView`, so a rule for it is in both files
 *  and the app's copy styles it where it is:
 *  - what the view appends to `viewRoot.layer()`, which is the page's `body` until the view has a
 *    layer of its own (stage 6): the task card and its scrim (the copy field and a download's link
 *    carry no class);
 *  - the outline's Turns, Tasks and Agents lists, which a wide phone's column borrows into the app's
 *    `#columnPane` (`PHONE_PANES`), with their rows. Read off the page in each of those states, plus
 *    the rows the hermetic page cannot draw (stage 5a's shared list).
 *  A name that is one of these, or starts with one and a hyphen. */
export const OUTSIDE = Object.freeze([
  ".task-popover", ".task-scrim", ".sv-task-card", ".task-chip",
  "#navigatorTurns", "#navigatorWork", "#navigatorAgents", ".navigator-list",
  ".activity-empty", ".agent-state", ".agents-filter-row", ".outline-agent", ".outline-epoch", ".outline-label", ".outline-number", ".outline-peer",
  ".outline-quote-marker", ".outline-quoted", ".outline-reply-count", ".outline-turn-pending", ".outline-turn-row",
  ".task-state", ".work-copy", ".work-group", ".work-tail", ".work-task",
]);

/** Custom properties a script writes on the page's root (`viewRoot.styleRoot()` or `page.html()`), or
 *  on the app's grid. The view's element must not declare them, or it would hide what was written
 *  above it. Tokens an `#app` rule declares are added to these by `deriveStylesheets`. */
export const SCRIPT_TOKENS = Object.freeze(["--preview", "--sidebar-user", "--phone-top", "--phone-bottom", "--phone-kb"]);

/** The attributes the page's `body` carries (page-head.html's, and native mode's mark), which no
 *  element of the view does: a selector led by one alone never matches under the view's element. */
export const PAGE_ATTRIBUTES = Object.freeze(["data-paired", "data-version", "data-mdrev", "data-contract", "data-shared-codex", "data-shared-reader-open"]);

/** What the view's element takes from a rule on the page's root: the properties that inherit. */
const INHERITED = new Set([
  "color", "color-scheme", "font", "font-family", "font-size", "font-style", "font-variant", "font-weight", "font-stretch",
  "font-feature-settings", "font-variation-settings", "line-height", "letter-spacing", "word-spacing", "text-size-adjust",
  "-webkit-text-size-adjust", "-webkit-font-smoothing", "-moz-osx-font-smoothing", "text-rendering", "caret-color", "accent-color",
]);
/** What stays the page's: the box of `html` and `body`. */
const PAGE_ONLY = new Set(["height", "min-height", "width", "margin", "padding", "overflow", "overflow-x", "overflow-y", "background", "background-color", "overscroll-behavior", "border", "position"]);

const VIEW_ROOT = ":where(#sessionView)";
const TYPE_WEIGHT = ":is(*,div)";
const CLASS_WEIGHT = ".session-view";
const NAME = /[.#]-?[A-Za-z_][A-Za-z0-9_-]*/g;

/** A compound naming the page around the view: its weight's stand-in on the view's element, and the
 *  conditions it carried. `null` for any other compound. */
function pageCompound(text) {
  let m;
  if ((m = /^:root(?![\w-])/.exec(text))) return { weight: CLASS_WEIGHT, conditions: text.slice(m[0].length) };
  if ((m = /^(html|body)(?![\w-])/.exec(text))) return { weight: TYPE_WEIGHT, conditions: text.slice(m[0].length) };
  if ((m = /^:where\(:root(\[[^\]]*\])\)$/.exec(text))) return { weight: "", conditions: `:where(${m[1]})` };
  if (/^\[data-theme[^\]]*\]$/.test(text)) return { weight: "", conditions: text };
  return null;
}

/** The conditions a page compound carried, as the view's element carries them: its theme and native
 *  mode's mark. Anything else cannot move, and stops the build. */
function movable(conditions, selector) {
  for (const piece of conditions.match(/\[[^\]]*\]|:not\(\[[^\]]*\]\)|:where\(\[[^\]]*\]\)|[^[:]+|:[^:]*/g) || []) {
    if (!/^(?::not\(|:where\()?\[(data-theme|data-shared-codex)(=[^\]]*)?\]\)?$/.test(piece)) {
      throw new Error(`session-view.css: \`${selector}\` reads the page around the view through \`${piece}\`, which the view's element does not carry`);
    }
  }
  return conditions;
}

const CHROME_NAMES = new Set(APP_CHROME);
const STATE_NAMES = new Set(APP_STATES);
const SHARED_NAMES = new Set([...SHARED, ...COMMON]);
const ROOT_NAMES = new Set(ROOT_CLASSES);
const isOutside = name => OUTSIDE.some(prefix => name === prefix || name.startsWith(`${prefix}-`));
const namesIn = text => text.match(NAME) || [];
/** A compound without its `:not(…)` parts (two levels of parentheses inside, as the sources write). */
const withoutNot = text => text.replace(/:not\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\)/g, "");
// A pseudo-class whose argument is a list of alternatives.
const ALTERNATIVES = /:(?:is|where|matches|-webkit-any)\(((?:[^()]|\([^()]*\))*)\)/g;

/** Whether an app rule may still style an element of the view: it reads an app state on `#app` (or
 *  hangs on `#app`) and what it styles is no chrome of the app's. Such a rule stays in app-shell.css
 *  and reaches into the view from outside; `deriveStylesheets` checks it against the view's rules. */
function reachesIn(parts) {
  const subject = namesIn(withoutNot(parts[parts.length - 1].text));
  if (parts.length < 2 || subject.some(n => CHROME_NAMES.has(n) || n === "#app" || n === ".app")) return false;
  return parts.slice(0, -1).every(p => !namesIn(withoutNot(p.text)).some(n => CHROME_NAMES.has(n)));
}

/** Where one selector goes: `{kind, app, view}`, the app's copy as written (or null) and the view's
 *  selectors (none, one, or two where it may also match the view's own element). `root` marks a
 *  selector on the page's root, whose rule the view's element takes only in part. */
export function classify(selector) {
  const parts = compounds(selector);
  let weight = "";
  let conditions = "";
  let k = 0;
  for (; k < parts.length; k += 1) {
    const page = pageCompound(parts[k].text);
    if (!page) break;
    if (k > 0 && parts[k].comb !== " ") throw new Error(`session-view.css: \`${selector}\` is not a descendant of the page's root`);
    weight += page.weight;
    conditions += page.conditions;
  }
  const rest = parts.slice(k);
  if (rest.length && k > 0 && rest[0].comb !== " ") throw new Error(`session-view.css: \`${selector}\` names a child of the page's root`);
  const root = () => `${VIEW_ROOT}${weight}${movable(conditions, selector)}`;
  if (!rest.length) return { kind: "page root", app: selector, view: [root()], root: true };
  const lead = rest[0].text;
  const tail = rest.slice(1);
  // What follows the first compound, with its combinator: `" .x>.y"`.
  const tailText = tail.length ? joinCompounds([{ comb: "", text: "" }, ...tail]) : "";
  // Whether the first compound goes on to its siblings (`+`, `~`) rather than into itself.
  const sibling = tail.length > 0 && (tail[0].comb === "+" || tail[0].comb === "~");
  const notToSiblings = () => {
    if (sibling) throw new Error(`session-view.css: \`${selector}\` goes on from the view's element to its siblings, which are no part of the view`);
  };
  if (/^(#app|\.app)(?![\w-])/.test(lead)) return { kind: reachesIn(parts) ? "an app state on the view" : "the app's", app: selector, view: [] };
  if (lead === ".session-view" && !tail.length && !weight && !conditions) return { kind: "the app placing the view", app: selector, view: [] };
  if (lead.startsWith(":is(#sessionView,#app)")) {
    notToSiblings();
    return { kind: "both, through the shared hook", app: selector, view: [`#sessionView${weight}${movable(conditions, selector)}${lead.slice(":is(#sessionView,#app)".length)}${tailText}`] };
  }
  if (/^#sessionView(?![\w-])/.test(lead)) {
    notToSiblings();
    return { kind: "the view's, on #sessionView", app: null, view: [`#sessionView${weight}${movable(conditions, selector)}${lead.slice("#sessionView".length)}${tailText}`] };
  }
  // A lead that names no element, before a descendant, is a condition on an ancestor, which under
  // the view's element reads only the view's own elements. That is right for an attribute the view's
  // elements carry (`[data-nav-card="tasks"] .x`); for one the page's body carries, or a bare
  // pseudo-class that the page's root met as well (`:focus-within .x`), the rule would quietly match
  // less, or nothing (`[data-shared-codex] .composer`). Say what it means instead.
  if (tail.length && !/^[A-Za-z*]/.test(lead) && !namesIn(withoutNot(lead)).length) {
    const attributes = [...lead.matchAll(/\[([\w-]+)/g)].map(m => m[1]);
    if (!attributes.length || attributes.some(a => PAGE_ATTRIBUTES.includes(a))) {
      throw new Error(`session-view.css: \`${selector}\` leads with a condition the page around the view meets; hang it on a class or on #sessionView`);
    }
  }
  // The names that decide are those outside `:not()`: a rule for a view element that is not `.dim`
  // styles the view's element (`.turn:not(.dim)`).
  const own = rest.map(p => namesIn(withoutNot(p.text)));
  const names = [...new Set(own.flat())];
  const viewOwn = n => !CHROME_NAMES.has(n) && !STATE_NAMES.has(n) && !SHARED_NAMES.has(n) && !ROOT_NAMES.has(n) && n !== "#sessionView";
  for (const p of rest) {
    for (const [, list] of withoutNot(p.text).matchAll(ALTERNATIVES)) {
      const alt = splitTop(list).map(namesIn);
      if (alt.some(ns => ns.some(n => CHROME_NAMES.has(n))) && alt.some(ns => ns.some(viewOwn))) {
        throw new Error(`session-view.css: \`${selector}\` names the app's chrome and the view's elements as alternatives; write one rule for each`);
      }
    }
  }
  if (names.some(n => STATE_NAMES.has(n))) return { kind: reachesIn(rest) ? "an app state on the view" : "the app's", app: selector, view: [] };
  if (names.some(n => CHROME_NAMES.has(n))) return { kind: "the app's", app: selector, view: [] };
  const view = names.filter(viewOwn);
  // The view's copy: under the view's element, and on it too where the first compound may be it.
  const onRoot = lead.startsWith("*") || namesIn(withoutNot(lead)).some(n => ROOT_NAMES.has(n));
  if (onRoot && /^[A-Za-z]/.test(lead)) throw new Error(`session-view.css: \`${selector}\` leads with a type and a class the view's element carries`);
  if (onRoot && sibling) throw new Error(`session-view.css: \`${selector}\` goes on from what may be the view's element to its siblings, which are no part of the view`);
  const under = `${root()} ${joinCompounds(rest)}`;
  const selectors = onRoot ? [`${root()}${lead.replace(/^\*/, "")}${tailText}`, under] : [under];
  if (!view.length) return { kind: "both: no name of the view's own", app: selector, view: selectors };
  if (view.some(isOutside)) return { kind: "both: the view's, shown outside it", app: selector, view: selectors };
  return { kind: "the view's", app: null, view: selectors };
}

// ── the two stylesheets ──────────────────────────────────────────────────────────────────────────

/** The reset (stage 5b): what a host's page may leave at the browser's defaults or set otherwise,
 *  pinned under the view's element as the app's page has it. The page's own rules give most of it,
 *  and they come along by the derivation: `*{box-sizing}` (here on the view's element and all within
 *  it), `button,input,textarea{font:inherit;color:inherit}` and `button{border:0}`. This adds a
 *  `select` beside them, and a `code` as the browser draws it, so a host's own rules for those
 *  reach nothing of the view's. First in the file, so every rule of the view's wins over it; each
 *  value is what the app's page already computes, so nothing in it changes. A button's background
 *  is not pinned: five of the view's buttons keep the browser's own (`#tasksBtn`, `#agentsBtn`, a
 *  phone pane's close, the dock's ✎ and its search's ✕; measured). */
const RESET = [
  ":where(#sessionView) select{font:inherit;color:inherit}",
  ":where(#sessionView) code{font-family:monospace;color:inherit;background:transparent;border:0}",
];

const HEADER = {
  view: `/* Generated by scripts/extract-agent-monitor-demo.mjs from reference.css and production.css (#s429
   stage 5b, design/session-view-component.md). Never hand-edit: change a source and run the script.
   The session view's styles: every rule hangs on the view's element, #sessionView. The prefix
   :where(#sessionView) weighs nothing; where a source named the page's root, .session-view stands for
   :root and :is(*,div) for html or body, each weighing what it replaces, so every rule keeps its
   weight exactly. Its keyframes are named sv-<name>, so a host's of the same name cannot meet them.
   The first two rules are the reset, for what a host's page leaves at the browser's defaults. The
   app shell loads app-shell.css first and this after it. */
`,
  app: `/* Generated by scripts/extract-agent-monitor-demo.mjs from reference.css and production.css (#s429
   stage 5b, design/session-view-component.md). Never hand-edit: change a source and run the script.
   The app shell's own styles: its chrome, the rules that read an app state on #app, and the page's.
   A rule that styles both the app and the view is in session-view.css too, which loads after this. */
`,
};

/** Derive `{view, app}`, the text of `session-view.css` and `app-shell.css`, and what was decided
 *  (`report`), from the texts of reference.css and production.css. */
export function deriveStylesheets({ reference, production }) {
  const sources = [parseCss(stripComments(reference)), parseCss(stripComments(production))];
  const report = { kinds: {}, keyframes: { view: [], app: [] }, reachIn: [], rootDeclarations: [], ties: [] };
  const count = kind => { report.kinds[kind] = (report.kinds[kind] || 0) + 1; };
  // Every selector in source order: where it went, its weight, what it styles and with what.
  const ledger = [];

  // Tokens an `#app` rule declares stay the app's (the grid's `--sidebar`, the reading properties).
  const pageTokens = new Set(SCRIPT_TOKENS);
  const appDeclared = new Set();
  const visit = (nodes, fn) => { for (const node of nodes) { if (node.kind === "group") visit(node.children, fn); else if (node.kind === "rule") fn(node); } };
  for (const nodes of sources) visit(nodes, rule => {
    for (const sel of splitTop(rule.prelude)) {
      const parts = compounds(sel);
      if (/^(#app|\.app)(?![\w-])/.test(parts[parts.length - 1].text)) for (const [prop] of declarations(rule.body)) { appDeclared.add(prop); if (prop.startsWith("--")) pageTokens.add(prop); }
    }
  });

  const rootProps = new Map(); // property → [{spec, element}] on the view's element
  const emit = (nodes, depth) => {
    const out = { view: [], app: [] };
    for (const node of nodes) {
      if (node.kind === "group") {
        const inner = emit(node.children, depth + 1);
        for (const side of ["view", "app"]) if (inner[side].length) out[side].push(`${node.prelude}{`, ...inner[side], "}");
      } else if (node.kind === "at") {
        out.app.push(node.body === null ? `${node.prelude};` : `${node.prelude}{${node.body}}`);
        out.view.push({ keyframes: node });
      } else {
        const viewPlain = [];
        const viewRoot = [];
        const app = [];
        for (const sel of splitTop(node.prelude)) {
          const decision = classify(sel);
          count(decision.kind);
          const parts = compounds(sel);
          ledger.push({ sel, kind: decision.kind, spec: specificity(sel), subject: namesIn(withoutNot(parts[parts.length - 1].text)), props: declarations(node.body).map(([prop]) => prop), view: decision.view.length > 0 });
          if (decision.kind === "an app state on the view") report.reachIn.push(sel);
          if (decision.app) app.push(decision.app);
          for (const written of decision.view) {
            const before = specificity(sel);
            const now = specificity(written);
            if (compare(before, now)) throw new Error(`session-view.css: \`${written}\` weighs ${now} where \`${sel}\` weighed ${before}`);
            (decision.root ? viewRoot : viewPlain).push(written);
            if (decision.root) {
              const element = /(^|\s)body(?![\w-])/.test(sel) ? "body" : "html";
              for (const [prop] of declarations(node.body)) {
                if (!rootProps.has(prop)) rootProps.set(prop, []);
                rootProps.get(prop).push({ spec: before, element, sel });
              }
            }
          }
        }
        if (app.length) out.app.push(`${app.join(",")}{${node.body}}`);
        if (viewPlain.length) out.view.push(`${viewPlain.join(",")}{${node.body}}`);
        if (viewRoot.length) {
          const kept = declarations(node.body).filter(([prop, text]) => {
            if (prop.startsWith("--")) return !pageTokens.has(prop);
            if (INHERITED.has(prop)) return true;
            if (PAGE_ONLY.has(prop)) return false;
            throw new Error(`session-view.css: \`${node.prelude}\` declares \`${text}\` on the page's root; say in INHERITED or PAGE_ONLY whether the view's element takes it`);
          });
          if (kept.length) {
            out.view.push(`${viewRoot.join(",")}{${kept.map(([, text]) => text).join(";")}}`);
            report.rootDeclarations.push(...kept.map(([prop]) => prop));
          }
        }
      }
    }
    return out;
  };
  const halves = sources.map(nodes => emit(nodes, 0));
  let view = halves.flatMap(h => h.view);
  let app = halves.flatMap(h => h.app);

  // A property on the view's element from two rules that stood on different elements (html, body)
  // would now be decided by weight, not by which element is nearer; and one the app's grid declares
  // too would now hide the grid's. Neither happens today; either stops the build.
  for (const [prop, rules] of rootProps) {
    if (prop.startsWith("--") ? pageTokens.has(prop) : !INHERITED.has(prop)) continue;
    if (new Set(rules.map(r => r.element)).size > 1) throw new Error(`session-view.css: \`${prop}\` is set on both html and body (${rules.map(r => r.sel).join(", ")})`);
    if (appDeclared.has(prop)) throw new Error(`session-view.css: \`${prop}\` is set on the app's grid and on the page's root; the view's element would hide the grid's`);
  }

  // An app rule that styles an element of the view from outside used to stand among the view's rules
  // in source order; now it comes before all of them. Where it weighs the same as a view rule written
  // BEFORE it, on the same element and property, that view rule would now win where it lost. Listed
  // for whoever splits: a pair whose subjects could be one element (they share a name outside
  // `:not()`, or one names none: a type or `*`) and whose properties overlap, a shorthand meeting
  // its longhands (`padding` and `padding-left`).
  ledger.forEach((a, at) => {
    if (a.kind !== "an app state on the view" && a.kind !== "the app placing the view") return;
    for (const v of ledger.slice(0, at)) {
      if (!v.view || compare(v.spec, a.spec)) continue;
      if (v.subject.length && a.subject.length && !v.subject.some(n => a.subject.includes(n))) continue;
      const props = v.props.filter(p => a.props.some(q => overlaps(p, q)));
      if (props.length) report.ties.push({ app: a.sel, view: v.sel, props });
    }
  });

  // Keyframes go where an animation names them. A keyframe's name is global, so in the view's file
  // each is renamed `sv-<name>` and every animation there names it so: a host's keyframes of the same
  // name can neither replace the view's nor be replaced by them. The app's file keeps its names. A
  // keyframe no animation in either file names stops the build, rather than going without a word.
  const animated = text => new Set([...text.matchAll(ANIMATION)].flatMap(m => m[2].split(/[\s,]+/)));
  const viewNames = animated(view.filter(line => typeof line === "string").join("\n"));
  const appNames = animated(app.join("\n"));
  const renamed = new Set();
  view = view.flatMap(line => {
    if (typeof line === "string") return [line];
    const m = KEYFRAMES.exec(line.keyframes.prelude);
    if (!m || line.keyframes.body === null) throw new Error(`session-view.css: no rule for \`${line.keyframes.prelude}\``);
    if (!viewNames.has(m[1]) && !appNames.has(m[1])) throw new Error(`session-view.css: no animation names the keyframes \`${m[1]}\`; remove them, or say here who runs them`);
    if (!viewNames.has(m[1])) return [];
    renamed.add(m[1]);
    report.keyframes.view.push(m[1]);
    return [`${line.keyframes.prelude.replace(m[1], `${KEYFRAME_PREFIX}${m[1]}`)}{${line.keyframes.body}}`];
  });
  view = view.map(line => line.replace(ANIMATION, (all, property, value) => property + value.replace(/-?[A-Za-z_][\w-]*/g, word => (renamed.has(word) ? `${KEYFRAME_PREFIX}${word}` : word))));
  app = app.filter(line => {
    const m = KEYFRAMES.exec(line);
    if (!m) return true;
    if (appNames.has(m[1])) { report.keyframes.app.push(m[1]); return true; }
    return false;
  });
  return { view: `${HEADER.view}${[...RESET, ...view].join("\n")}\n`, app: `${HEADER.app}${app.join("\n")}\n`, report };
}

// The longhands a shorthand sets that its name does not lead (`padding` leads `padding-left`).
const LONGHANDS = {
  gap: ["row-gap", "column-gap"], inset: ["top", "right", "bottom", "left"],
  "place-items": ["align-items", "justify-items"], "place-content": ["align-content", "justify-content"], "place-self": ["align-self", "justify-self"],
  "grid-area": ["grid-row", "grid-column"], "flex-flow": ["flex-direction", "flex-wrap"],
  "border-radius": ["border-top-left-radius", "border-top-right-radius", "border-bottom-left-radius", "border-bottom-right-radius"],
};
/** Whether two declared properties set something in common: the same property, or a shorthand and
 *  one of its longhands (`padding`/`padding-left`, `border`/`border-left-color`, `inset`/`top`). */
function overlaps(p, q) {
  if (p === q) return true;
  if (p.startsWith("--") || q.startsWith("--")) return false;
  const covers = (short, long) => long.startsWith(`${short}-`) || (LONGHANDS[short] || []).some(l => long === l || long.startsWith(`${l}-`));
  return covers(p, q) || covers(q, p);
}

/** The view's own keyframes are named with this before their source name. */
export const KEYFRAME_PREFIX = "sv-";
const KEYFRAMES = /^@(?:-webkit-)?keyframes\s+([^{\s]+)/;
// An animation's declaration, its property and its value: what names a keyframe.
const ANIMATION = /((?:-webkit-)?animation(?:-name)?\s*:)([^;}]*)/g;
