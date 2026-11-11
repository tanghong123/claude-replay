// The session list (#s430, design/session-list-component.md, stage 2): the sessions to show, their
// state, project and agent, the filter and the arrangements, the collapsed rail's agent marks and
// their flyout, and the list keys, behind one entry point, `mountSessionList`. The app shell
// (app.js) mounts it the way a host would and answers what it says (`onOpen`); session-list.js keeps
// the helpers the list draws with, which touch no page. The list's code moved here line for line
// from app.js, at the margin, as the session view's did (design/session-view-component.md, stage 3).
import { agentLogo, svg } from "./icons.js";
import { backend } from "./backend.js";
import { PHONE_QUERY } from "./shared/capabilities.js";
import { SessionIndexStore } from "./session-index-store.js";
import { SESSION_LIST_MARKUP } from "./session-list-markup.js";
import { indexState, selectedRow } from "./state.js";
import { families, hideAction, ignoreQuery, visibleTree } from "./shared/session-visibility.js";
import { ARRANGE_SOURCES, arrangeSessions, familyHolds, flatFamilies, isDefaultView, nestSideSessions, paneLookup, rowLookup, sectionView, viewSection } from "./session-list.js";
import { displayState, denoteState, sessionBucket, sessionFilterBuckets, FILTER_BUCKETS, FILTER_LABELS, BLOCKED_SUMMARY } from "./shared/state-labels.js";
import { agentName, escapeText } from "./view-model.js";
import { page } from "./view-root.js";
import { resolveKey } from "./shared/keymap.js";

/** The controls a host may switch off (#s436; design/session-list-component.md, stage 4), each shown
 *  unless the `show` option says `false`. */
export const OPTIONAL_CONTROLS = Object.freeze(["filter", "hiding", "arrangements"]);
/** How the list takes its keys (the `keyboard` option), as the view does: "engaged" binds them itself,
 *  answering only while the reader works in the list; "page" leaves the page's keyboard to the host,
 *  which calls the handle's `keys`. */
export const KEYBOARD_MODES = Object.freeze(["engaged", "page"]);
/** Every option `mountSessionList` takes; any other name is refused, so a misspelt or retired one
 *  (the old `toast` or `compact`) fails loudly rather than being dropped. */
export const MOUNT_OPTIONS = Object.freeze([
  "theme", "show", "keyboard", "persist", "onOpen", "onIndex", "onToast",
  "projectTree", "projectActions", "newSession", "layer", "place",
]);

/** Draw the session list in `element` and return its handle. The element becomes `#sessionList`, the
 *  id the list's rules hang on (stage 1); one with another id is refused, and so is one that holds
 *  anything but white space (stage 4), so the list never takes a host's nodes for its own, nor takes
 *  them away when it is unmounted. The list builds its markup in it from its own (stage 3,
 *  `session-list-markup.js`, cut from the demo's shell): the tree, under the arrangement strip it
 *  adds; the rest of what it draws it builds and keeps a reference to, and hands the parts its host
 *  shows elsewhere to `place`. It looks up nothing outside its element.
 *
 *  The selection, the folds, the filter, the arrangement and the read marks are `indexState`'s
 *  (state.js) for now; the host's `persist` writes them.
 *
 *  Options, each named in `MOUNT_OPTIONS` (any other name is refused), each optional:
 *  - `theme`: "light" (the default) or "dark", which the list's element carries (`data-theme`) and
 *    its stylesheet reads, as the view's does; the handle's `setTheme` changes it.
 *  - `show`: which optional controls are drawn, by name, as `{ filter: false }`. Each is shown unless it
 *    is `false`; a name not in `OPTIONAL_CONTROLS`, or a value that is not a boolean, is refused. A
 *    control switched off is not drawn, and what it would choose is not applied, so a reader is never
 *    left with sessions held back by a choice they cannot see or change; what the reader chose is kept
 *    as it is, for a host that shows it.
 *    - `filter`: the session filter's states (active in the last 6 hours, blocked, idle) and its
 *      Everything. Off, every state is listed.
 *    - `hiding`: hiding and restoring a session, a project or an agent, and the filter's Include
 *      hidden. Off, neither control is drawn and what the reader hid stays out of the list.
 *    - `arrangements`: the filter's Arrange choices (one list by activity, like another app) and an
 *      arranged list's section menus. Off, the list is grouped by agent and project, and reads no
 *      other app's list.
 *    With all three off the filter has nothing to offer, and neither its glyph nor its sheet is drawn.
 *  - `keyboard`: "engaged" (the default) or "page", as `KEYBOARD_MODES` says. Engaged, the list
 *    answers its keys (the next and the previous session, ↓ and ↑) only while a row of its own has the
 *    focus, and leaves every other key to the page. "page" is the app's mode: the list binds none, and
 *    the host calls `keys[action]` from its own keymap (shared/keymap.js's "list" actions).
 *  - `persist()`: remember the list's choices, at each moment the list changes one.
 *  - `onOpen({ id, push })`: the reader chose a session in the list (a row, the rail's flyout, the
 *    list keys), `push` whether it is a new place in the history. The list has not moved its
 *    selection: the host does, with `select(id)`, when it opens the session.
 *  - `onIndex()`: the index has new rows: the host's own work on them (the selection gone, the view
 *    told, a first session chosen), raised before the list draws them; the list is drawn even if it
 *    throws.
 *  - `onToast(message)`: say something briefly (a scan that failed, a hide or a restore), as the
 *    view's `onToast`.
 *  - `projectTree(filtered, all)`: the tree the list draws, from the one the filter leaves and the
 *    whole grouped index (native mode's projects); left out, the filtered one.
 *  - `projectActions()`: whether the Codex groups offer a new session and a new project.
 *  - `newSession(project)`: start one, in `{ id, cwd }` or, for "add", in a project to add.
 *  - `layer(kind)`: the element a floating layer is appended to: "flyout" (the rail's flyout) and
 *    "menu" (a section's menu, its scrim and the safe-area probe); left out, the list's own
 *    element, where the list's stylesheet styles them (a fixed layer still floats free of it).
 *  - `place(part, node)`: put one of the list's parts where the host shows it, called once for each
 *    as the list mounts: "controls" (the list's strip of controls, `div.session-list-controls`:
 *    Collapse every group, Expand every group and the filter's glyph, in that order; it is
 *    `display: contents`, so each is an item of whatever row holds it), "sheet" (the filter's sheet,
 *    which hangs below what holds it, `top: 100%` of a positioned parent), "rail-filter" (the filter's
 *    button for a rail) and "marks" (the agent marks' holder for a rail). A part switched off is not
 *    handed over (the filter's sheet and rail button, with the filter's three controls off). Left out,
 *    the list draws the controls and the sheet in a head of its own at the top of its element, the
 *    sheet's containing block, and no rail parts.
 *
 *  The handle: `redraw()`, `refresh()`, `start()` and `stop()` (the index's polls), `grouped()`,
 *  `rows()`, `row(id)`, `workers(id)`, `selected()`, `select(id)` (a selection the host
 *  made: the list takes it, marks it read and draws it, and says nothing), `keys` (the list's key
 *  actions, by the keymap's names: `keys["list-next"]()` and `keys["list-prev"]()`), `compact(on)`
 *  (the list is drawn small, as the collapsed sidebar's rail of agent marks, whose flyouts open only
 *  then; turned off, an open flyout closes), `setSource({ load, group })` (where the index reads its
 *  rows and how it groups them: native mode's), `setBuckets(buckets)` and `filtered()` (the session
 *  filter, for the host's session search), `setTheme(theme)` (light or dark, on the list's element)
 *  and `unmount()`.
 *
 *  `unmount()` takes the list away (stage 4): it stops all it started (its listeners on the page,
 *  bound with one abort signal, its timers and the index's polls), and work still out when it is called
 *  (the index's read, another app's list, a hide or a restore) does nothing when it comes back. It
 *  removes what it put outside its element (the parts it handed to `place`, wherever the host put
 *  them, and its floating layers), and leaves the element as the host gave it: empty, with its own
 *  attributes. After the unmount every other member of the handle does nothing and answers nothing,
 *  and the list tells its host nothing more; mounting again in the same element draws a new list. */
export function mountSessionList(element, options = {}) {
for (const name of Object.keys(options)) if (!MOUNT_OPTIONS.includes(name)) throw new Error(`mountSessionList takes no option named ${name}; its options are ${MOUNT_OPTIONS.join(", ")}`);
for (const [name, shown] of Object.entries(options.show || {})) {
  if (!OPTIONAL_CONTROLS.includes(name)) throw new Error(`mountSessionList has no optional control named ${name}; the controls a host may switch off are ${OPTIONAL_CONTROLS.join(", ")}`);
  if (typeof shown !== "boolean") throw new Error(`mountSessionList shows ${name} or not by true or false, not ${JSON.stringify(shown)}`);
}
const keyboardMode = options.keyboard ?? "engaged";
if (!KEYBOARD_MODES.includes(keyboardMode)) throw new Error(`mountSessionList takes its keyboard "engaged" or "page", not ${JSON.stringify(keyboardMode)}`);
if (element.id && element.id !== "sessionList") throw new Error(`mountSessionList draws into an element whose id is sessionList, or that has none; this one is #${element.id}`);
if ([...element.childNodes].some(node => !(node.nodeType === 3 && !node.data.trim()))) throw new Error("mountSessionList draws into an empty element; this one holds the host's nodes");
/** Whether an optional control is drawn: each is, unless the host switched it off. */
const shows = name => options.show?.[name] !== false;
// What the list starts outside its element, which `unmount` stops (stage 4), as the view's (its stage
// 7): every listener it binds on the page carries `signal`, and the timers its code starts are made
// through the names below, which keep each one until it has run or been cleared. The index's own poll
// stops with the store (`stop`).
const life = new AbortController(), signal = life.signal;
const timers = new Set();
let unmounted = false;
const setTimeout = (run, ms) => {
  const id = globalThis.setTimeout(() => { timers.delete(id); run(); }, ms);
  timers.add(id);
  return id;
};
const clearTimeout = id => { timers.delete(id); globalThis.clearTimeout(id); };
// The element as the host gave it, which `unmount` puts back: its attributes (the id, the class, the
// theme and the arrangement the list writes on it) and no children.
const elementBefore = [...element.attributes].map(attribute => [attribute.name, attribute.value]);
// The session list's own element (#s430, stage 1): where the list keeps the state it sets for
// itself, its arrangement (`sessions-flat`, `sessions-arranged`), which it kept on `#app`. In the app
// it is the sidebar's child where the tree stood (stage 3), `display: contents`, so what it holds stays
// in the sidebar's column. An id, so the rules that read the state weigh what they did on `#app`.
element.id = "sessionList";
// Its class and its theme (stage 3): its stylesheet hangs every rule on #sessionList, the page's root
// stood in for by `.session-list`, and reads the theme there, light or dark, as the host says, as the
// view's does on #sessionView.
element.classList.add("session-list");
const applyTheme = theme => { element.dataset.theme = theme === "dark" ? "dark" : "light"; };
applyTheme(options.theme);
const sessionList = element;
const { onToast = () => {}, persist = () => {} } = options;
// Left out, a floating layer goes in the list's own element, where the list's stylesheet reaches it
// (its rules hang on `#sessionList`); the app passes its own, the page's body and `#app`.
const layer = kind => options.layer?.(kind) || sessionList;
// The list's markup (stage 3), built from its own template, cut from the demo's shell: the tree, which
// goes in its element, and Collapse every group and the agent marks' holder, which the host places
// (`place`, at the end of the mount). The list keeps each, and finds nothing of them in the page.
const markup = document.createElement("template");
markup.innerHTML = SESSION_LIST_MARKUP;
const built = id => markup.content.querySelector(`#${id}`);
const tree = built("tree");
const collapseBtn = built("collapseBtn");
const railMarks = built("sidebarMiniAgents");
element.append(tree);
// Collapse every group's glyph and name (#75, #76): the demo's button has none, an empty 34px button
// invisible until hovered.
collapseBtn.innerHTML = svg("collapseStack");
collapseBtn.setAttribute("aria-label", "Collapse every group");
collapseBtn.title = "Collapse every group";
// Expand-all (#77, parity item 12): the counterpart of collapse-all — every agent and project
// group open again, remembered like the folds are. Runtime chrome beside its counterpart, made
// here with it, before anything that draws the tree reads it (stage 4).
const expandBtn = document.createElement("button");
expandBtn.type = "button";
expandBtn.className = "iconbtn";
expandBtn.id = "expandBtn";
expandBtn.innerHTML = svg("expandStack");
expandBtn.title = "Expand every group";
expandBtn.setAttribute("aria-label", "Expand every group");
// What `renderTree` last wrote into the list, so an unchanged refresh leaves the rows in place (#s72).
let treeWritten = null;
const sessionIndex = new SessionIndexStore({
  update: () => {
    // The host's half first (`onIndex`): in the app, a selection whose row is gone, the view told and
    // the first session chosen while none is. Before #s430 the app drew the tree between the first
    // and the second; none of it reads the drawn tree (a first session chosen there is drawn by its
    // `select`, and again here). A host that throws still gets its list drawn.
    try { options.onIndex?.(); } finally {
      renderTree();
      // #s387: an app's list is followed at the index's own pace.
      refreshLayout();
    }
  },
  error: error => { console.warn("Session index refresh failed", error); onToast("Session scan failed — retrying"); }
});

// The state words come from the shared table (shared/state-labels.js, #44) — the rail's
// tooltip and this shell's chips read the same label for the same verdict. `stateDenote`
// adds the one input that is this viewer's: the read mark.
function stateDenote(row) { return denoteState(row, Number(indexState.read[row.id] || 0)); }

function loadSessions() { return sessionIndex.refresh(); }

function groupedSessions() {
  return sessionIndex.grouped().map(agent => ({
    ...agent,
    name: agentName(agent.id),
    // #s53: a headless worker whose starter this index knows lives in that session's Agents pane,
    // not in the list (the server sends `startedBy` only for a starter that is a row here).
    projects: agent.projects.map(project => ({ ...project, sessions: project.sessions.filter(row => !row.startedBy) })).filter(project => project.sessions.length)
  })).filter(agent => agent.projects.length);
}
/** A session's headless workers (#s53): the rows it started, oldest first. */
function sessionWorkers(id) {
  if (!id) return [];
  return [...indexState.rows.values()].filter(row => row.startedBy === id).sort((a, b) => (a.startTs || 0) - (b.startTs || 0) || String(a.id).localeCompare(String(b.id)));
}
const SIDEBAR_SESSION_LIMIT = 5;
/** The agent's mark on a flat row (#s206): one colour, the row's muted text colour, as the agent
 *  lists that show several agents side by side draw it (Zed's and JetBrains' shared registry
 *  requires a single-colour 16px mark; VS Code's sessions view mutes it). The marks are the
 *  vendors' own registry marks (#s209, the owner); QoderWork shares Qoder's, Qwenwork takes Qwen's.
 *  An agent the shell has no mark for gets its name's first letter in a rounded square rather than
 *  a robot every such agent would share. */
function agentGlyph(id, name) {
  if (["claude", "codex", "qoder", "qoderwork", "qwenwork"].includes(id)) return agentLogo(id);
  const letter = escapeText((String(name || id || "?").trim()[0] || "?").toUpperCase());
  return `<svg class="brand-icon icon agent-monogram" viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="5"/><text x="12" y="16.4" text-anchor="middle">${letter}</text></svg>`;
}
// #s206: the flat list shows this many families before "Show more", the selected one always.
const FLAT_SESSION_LIMIT = 30;

// #s387 (design/layout-sync.md): another app's list. `layoutState` holds the chosen source's last
// answer from `api/layout`, asked at every index refresh while a source is chosen and never
// otherwise; `sourceState` whether each source can be read, asked as the filter sheet opens.
let layoutState = { source: null, data: null, error: null, text: null, loading: null };
let sourceState = {};
// One timer asks again while a source's reader has not read (pending) or answers with a read from
// before it last stopped (stale): every answer re-arms the same one, so retries never pile up.
let layoutRetry = null;
function retryLayout() { clearTimeout(layoutRetry); layoutRetry = setTimeout(() => { layoutRetry = null; refreshLayout(); }, 1000); }
let arrangeStrip = null;
const ARRANGE_LABELS = { "claude-app": "the Claude app", "codex-app": "the Codex app", herdr: "Herdr" };
// An arranged section shows this many sessions before "Show more", the selected one always.
const ARRANGED_LIMIT = 30;
// The section menu (#s387): one floating menu, opened from a section head's
// glyph, with the Claude app's section menu's choices; `sectionMenuKey` is the section it serves.
let sectionMenu = null;
let sectionScrim = null;
let sectionMenuKey = null;
let sectionMenuAnchor = null;
const SECTION_GROUP_LABELS = { none: "None", project: "Project", date: "Date", state: "State" };
const SECTION_SORT_LABELS = { updated: "Last updated", created: "Date created", name: "Name" };
function sectionMenuHtml(key, label) {
  const view = sectionView(indexState.sectionViews[key]);
  const app = ARRANGE_LABELS[arrangeSource()] || "the app";
  const option = (attr, value, text, on, disabled = false) => `<button class="scope-option${on ? " on" : ""}${disabled ? " disabled" : ""}" type="button" role="radio" aria-checked="${on}" aria-disabled="${disabled}" ${attr}="${value}"><span class="scope-check"></span><span>${escapeText(text)}</span></button>`;
  return `<div class="scope-menu-head"><strong>GROUP BY</strong><span class="section-menu-name">${escapeText(label)}</span></div>
<div class="scope-menu-list" role="radiogroup" aria-label="Group by">${option("data-view-group", "app", `As in ${app}`, view.group === "app")}${Object.entries(SECTION_GROUP_LABELS).map(([value, text]) => option("data-view-group", value, text, view.group === value)).join("")}</div>
<div class="scope-menu-divider"></div>
<div class="scope-menu-head"><strong>SORT BY</strong></div>
<div class="scope-menu-list" role="radiogroup" aria-label="Sort by">${option("data-view-sort", "app", `As in ${app}`, view.sort === "app")}${Object.entries(SECTION_SORT_LABELS).map(([value, text]) => option("data-view-sort", value, text, view.sort === value)).join("")}</div>
<div class="scope-menu-divider"></div>
<div class="scope-menu-list" role="radiogroup" aria-label="Order">${option("data-view-dir", "desc", "Descending", view.sort !== "app" && view.descending, view.sort === "app")}${option("data-view-dir", "asc", "Ascending", view.sort !== "app" && !view.descending, view.sort === "app")}</div>
<div class="scope-menu-divider"></div>
<button class="scope-option section-menu-reset${isDefaultView(view) ? " disabled" : ""}" type="button" data-view-reset aria-disabled="${isDefaultView(view)}"><span></span><span>${escapeText(`Reset to ${app}'s order`)}</span></button>`;
}
let insetProbe = null;
/** The safe area's inset on one side, in px. Script cannot read `env()` itself, so a hidden probe
 *  takes it as its height (#s403). */
function safeInset(side) {
  if (!insetProbe) {
    insetProbe = document.createElement("div");
    insetProbe.setAttribute("aria-hidden", "true");
    insetProbe.style.cssText = "position:fixed;top:0;left:0;width:0;visibility:hidden;pointer-events:none";
    layer("menu").append(insetProbe);
  }
  insetProbe.style.height = `env(safe-area-inset-${side}, 0px)`;
  return insetProbe.offsetHeight;
}
function placeSectionMenu() {
  if (!sectionMenu || sectionMenu.hidden || !sectionMenuAnchor) return;
  // A popover is capped at the height the reader can SEE, between the safe areas. Its stylesheet
  // caps (74vh from `#app .navigator-options`, and 640px) count iOS Safari's vh with the bars put
  // away: held sideways on an iPhone 17 Pro that is the screen's 402px, and the menu was 297px tall
  // on the 292px page beside the tab bar, its foot off the screen (the fourth review of #s387). So
  // the page's own height, less the safe areas, may LOWER that cap and never raise it: a desktop
  // keeps its 74vh and 640px, and a Home Screen app held sideways ends above its home indicator
  // (#s403). The sheet keeps its own rule.
  sectionMenu.style.maxHeight = "";
  const top = 8 + safeInset("top"), bottom = innerHeight - 8 - safeInset("bottom");
  if (!sectionMenu.classList.contains("sheet")) {
    const cap = parseFloat(getComputedStyle(sectionMenu).maxHeight), room = Math.max(0, bottom - top);
    if (!(cap <= room)) sectionMenu.style.maxHeight = `${room}px`;
  }
  const r = sectionMenuAnchor.getBoundingClientRect();
  const width = sectionMenu.offsetWidth, height = sectionMenu.offsetHeight;
  sectionMenu.style.left = `${Math.round(Math.max(8, Math.min(r.right - width, innerWidth - width - 8)))}px`;
  const below = r.bottom + 4;
  sectionMenu.style.top = `${Math.round(below + height <= bottom ? below : Math.max(top, r.top - height - 4))}px`;
}
/** Upright on a phone the section menu is a sheet at the foot over a dim; held sideways, as on a
 *  desktop, a popover at the control's side (#s41: a band across a sideways screen hides what it
 *  covers), whose scrim stays clear — it still takes the tap that closes the menu. */
function placeSheet() {
  const sheet = matchMedia(PHONE_QUERY).matches && innerHeight > innerWidth;
  sectionMenu.classList.toggle("sheet", sheet); sectionScrim.classList.toggle("dim", sheet);
}
function openSectionMenu(key, label, anchor) {
  if (!sectionMenu) {
    sectionMenu = document.createElement("div");
    sectionMenu.className = "navigator-options section-view-menu"; sectionMenu.id = "sectionViewMenu"; sectionMenu.hidden = true;
    sectionMenu.setAttribute("role", "dialog");
    // On a phone the menu is a sheet over a dim (the phone block styles both): the dim takes the
    // tap that closes it, so that tap never reaches a row beneath.
    sectionScrim = document.createElement("div");
    sectionScrim.className = "section-view-scrim"; sectionScrim.hidden = true;
    sectionScrim.addEventListener("click", event => { event.preventDefault(); event.stopPropagation(); closeSectionMenu(false); });
    layer("menu").append(sectionScrim, sectionMenu);
    sectionMenu.addEventListener("click", event => {
      const view = sectionView(indexState.sectionViews[sectionMenuKey]);
      const pick = event.target.closest("[data-view-group], [data-view-sort], [data-view-dir], [data-view-reset]");
      if (!pick || pick.getAttribute("aria-disabled") === "true") return;
      if (pick.dataset.viewGroup) view.group = pick.dataset.viewGroup;
      if (pick.dataset.viewSort) view.sort = pick.dataset.viewSort;
      if (pick.dataset.viewDir) view.descending = pick.dataset.viewDir === "desc";
      if (pick.hasAttribute("data-view-reset")) { delete indexState.sectionViews[sectionMenuKey]; }
      else if (isDefaultView(view) && view.descending) delete indexState.sectionViews[sectionMenuKey];
      else indexState.sectionViews[sectionMenuKey] = view;
      persist(); renderTree();
      sectionMenu.innerHTML = sectionMenuHtml(sectionMenuKey, sectionMenu.dataset.label || "");
      sectionMenuAnchor = tree.querySelector(`[data-section-view="${CSS.escape(sectionMenuKey)}"]`) || sectionMenuAnchor;
      placeSectionMenu();
    });
    document.addEventListener("pointerdown", event => { if (sectionMenu && !sectionMenu.hidden && !sectionMenu.contains(event.target) && event.target !== sectionScrim && !event.target.closest?.("[data-section-view]")) closeSectionMenu(false); }, { capture: true, signal });
    document.addEventListener("keydown", event => { if (event.key === "Escape" && sectionMenu && !sectionMenu.hidden) { event.stopPropagation(); closeSectionMenu(true); } }, { capture: true, signal });
    addEventListener("resize", () => { if (sectionMenu.hidden) return; placeSheet(); placeSectionMenu(); }, { signal });
  }
  if (sectionMenuKey === key && !sectionMenu.hidden) { closeSectionMenu(false); return; }
  sectionMenuKey = key; sectionMenuAnchor = anchor; sectionMenu.dataset.label = label;
  sectionMenu.setAttribute("aria-label", `Sort and group ${label}`);
  sectionMenu.innerHTML = sectionMenuHtml(key, label);
  placeSheet();
  sectionMenu.hidden = false; sectionScrim.hidden = false;
  anchor.setAttribute("aria-expanded", "true");
  placeSectionMenu();
}
function closeSectionMenu(refocus) {
  if (!sectionMenu) return;
  const key = sectionMenuKey;
  sectionMenu.hidden = true; sectionScrim.hidden = true; sectionMenuKey = null; sectionMenuAnchor = null;
  const anchor = key && tree.querySelector(`[data-section-view="${CSS.escape(key)}"]`);
  if (anchor) { anchor.setAttribute("aria-expanded", "false"); if (refocus) anchor.focus(); }
}

// Hide / restore (parity #1). The control lives ON the row, where the shell keeps row-level
// actions, and appears on hover or focus — except on a hidden row, where "restore" is the
// whole point of showing it. The key is the server's (`ignoreKey`), passed through verbatim.
function treeAction(target, kind) {
  if (!shows("hiding")) return "";
  const action = hideAction(target, kind);
  return `<button class="tree-action" type="button" data-ignore-op="${action.op}" data-ignore-key="${escapeText(action.key)}" title="${escapeText(action.title)}" aria-label="${escapeText(action.title)}">${svg(action.icon)}</button>`;
}

// A row, or a fork family's representative (`fam`): the family's state and age, not just the
// rep's — a forked conversation is "running" when any fork is — plus a fork count that opens
// the other members, indented, the way the classic rail's ⑂ chip does (#142).
function sessionTreeRow(row, fam = null, member = false, context = null, side = null, name = null) {
  const forked = fam && fam.forks.length ? fam : null;
  const denote = forked && forked.growing && stateDenote(row)?.tone !== "busy" ? { label: "A fork is running", tone: "busy" } : stateDenote(row);
  const newest = forked ? forked.members.reduce((a, b) => ((b.activityTs || 0) > (a.activityTs || 0) ? b : a), row) : row;
  const marker = denote ? `<span class="session-state ${escapeText(denote.tone)}" role="img" aria-label="${escapeText(denote.label)}" title="${escapeText(denote.label)}"></span>` : "";
  const open = forked && (indexState.openFamilies.has(forked.key) || forked.forks.some(r => r.id === indexState.selected));
  const chip = forked ? `<button class="tree-forks" type="button" data-family-toggle="${escapeText(forked.key)}" aria-expanded="${!!open}" title="${forked.forks.length} fork${forked.forks.length === 1 ? "" : "s"} of this session — forking copies the conversation, so they overlap heavily">⑂ ${forked.forks.length}</button>` : "";
  // #s206: a row of the flat list says where it lives, since no group head does — the project's
  // name under the title, after the agent's mark, which is named for a screen reader and a pointer.
  // #s340: a Claude desktop side session wears a mark of its own under its starter (the fork
  // chip means a copied conversation; a side session is a conversation of its own). `side` is
  // `{ depth, starter }`: how deep it nests, and its starter's title.
  // #s387: an arranged row takes the title the app shows (`name`), and its mark says where it sits.
  const sideMark = side ? `<span class="tree-side" role="img" aria-label="Side session" title="${escapeText(side.via || `Started from “${side.starter}” in the Claude desktop app`)}">↳</span>` : "";
  // Over a context line (an arranged row, #s387) the mark rides inside the title's line.
  const title = context && sideMark ? `<span class="tree-title">${sideMark}${escapeText(name || row.name || row.id)}</span>` : `${sideMark}<span class="tree-title">${escapeText(name || row.name || row.id)}</span>`;
  const main = context ? `<span class="tree-main">${title}<span class="tree-context"><span class="tree-agent" role="img" aria-label="${escapeText(context.agent.name)}" title="${escapeText(context.agent.name)}">${agentGlyph(context.agent.id, context.agent.name)}</span><span class="tree-project" title="${escapeText(context.project.path || context.project.name)}">${escapeText(context.project.name)}</span></span></span>` : title;
  return `<div class="tree-row session ${context ? "flat" : ""} ${row.id === indexState.selected ? "selected" : ""} ${row.hidden ? "is-hidden" : ""} ${member ? "is-member" : ""} ${side ? "is-side" : ""}"${side ? ` style="--side-depth:${side.depth}"` : ""} role="treeitem" tabindex="0" data-session="${escapeText(row.id)}" title="${escapeText(displayState(row).label)} · ${escapeText(row.stateDetail || "")}">${main}<span class="session-end">${chip}${treeAction(row, "session")}${marker}<span class="session-age">${escapeText(newest.activity || row.activity || "")}</span></span></div>`;
}

/** A family as the list draws it: its representative, its forks while open, then the Claude
 *  desktop side sessions it started (#s340), each with theirs. `context` names the project and
 *  agent on a flat row; a side session sits under its starter, so it needs none. */
function familyRowsHtml(fam, context = null, side = null) {
  let html = sessionTreeRow(fam.rep, fam, false, context, side);
  const open = fam.forks.length && (indexState.openFamilies.has(fam.key) || fam.forks.some(row => row.id === indexState.selected));
  if (open) for (const fork of fam.forks) html += sessionTreeRow(fork, null, true);
  const starter = fam.rep.name || fam.rep.id;
  for (const sub of fam.sides || []) html += familyRowsHtml(sub, null, { depth: (side ? side.depth : 0) + 1, starter });
  return html;
}

/** The flat list (#s206): every family the tree would show, in one list by activity (the shared
 *  `flatFamilies`, the server's calm order), each row naming its project and agent; forks open
 *  under their family as in the tree. The sidebar and the collapsed rail's flyout both draw it,
 *  as they both draw `agentProjectsHtml` in the tree. `agents` is `visibleTree`'s output. */
function flatSessionsHtml(agents, overflowKey = "flat") {
  const fams = nestSideSessions(flatFamilies(agents, Date.now() / 1000));
  const expanded = indexState.expandedProjects.has(overflowKey);
  let shown = expanded ? fams : fams.slice(0, FLAT_SESSION_LIMIT);
  const selectedFam = fams.find(fam => familyHolds(fam, indexState.selected));
  if (!expanded && selectedFam && !shown.includes(selectedFam)) shown = shown.concat(selectedFam);
  let html = "";
  for (const fam of shown) html += familyRowsHtml(fam, { agent: { id: fam.agent.id, name: agentName(fam.agent.id) }, project: fam.project });
  const hidden = fams.filter(fam => !shown.includes(fam)).length;
  if (fams.length > FLAT_SESSION_LIMIT) html += `<button class="tree-project-more" type="button" data-project-more="${escapeText(overflowKey)}" aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${hidden} more`}</span><span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>`;
  return html;
}

/** Another app's list (#s387, design/layout-sync.md): its sections and sub-heads in its order, each
 *  session the app lists as a row with the app's title — whatever the session filter or a hide
 *  says, since the point is the SAME sessions as the app — and an entry this monitor has no row
 *  for kept, dimmed, so nothing the app lists is missing. Every other session the monitor lists
 *  follows in one collapsed group, "Not in …", under the filter and the hides as ever, drawn as the
 *  flat list draws. `agents` is the whole grouped index (for each row's agent and project);
 *  `shownAgents` is the filtered tree. A section opens and closes like the app's (collapsed as the
 *  app has it) and the reader's flip of it is remembered with the tree's. */
function arrangedSessionsHtml(layout, agents, shownAgents) {
  const source = layout.source;
  const where = new Map();
  for (const agent of agents) for (const project of agent.projects) for (const row of project.sessions) where.set(row.id, { agent: { id: agent.id, name: agentName(agent.id) }, project });
  const { sections, placed } = arrangeSessions(layout, rowLookup(indexState.rows), paneLookup(indexState.rows));
  const node = (item, depth, parent, grouped) => {
    const side = depth ? { depth, starter: parent, via: `Under “${parent}” in ${layout.label}` } : null;
    const html = item.row
      ? sessionTreeRow(item.row, null, false, where.get(item.id) || null, side, item.title).replace('class="tree-row session ', `class="tree-row session arranged ${grouped && !depth ? "in-group " : ""}`)
      : absentRowHtml(item, side, layout.label, grouped);
    const name = item.title || item.row?.name || item.id;
    return html + item.children.map(child => node(child, depth + 1, name, grouped)).join("");
  };
  // A long section shows its first sessions and a "Show more", as a project does in the tree; the
  // selected session is always shown.
  const holds = (item, id) => item.id === id || item.children.some(child => holds(child, id));
  const list = (nodes, overflowKey, grouped) => {
    const expanded = indexState.expandedProjects.has(overflowKey);
    let shown = expanded ? nodes : nodes.slice(0, ARRANGED_LIMIT);
    const selected = nodes.find(item => holds(item, indexState.selected));
    if (!expanded && selected && !shown.includes(selected)) shown = shown.concat(selected);
    let out = shown.map(item => node(item, 0, "", grouped)).join("");
    if (nodes.length > ARRANGED_LIMIT) out += `<button class="tree-project-more" type="button" data-project-more="${escapeText(overflowKey)}" aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${nodes.length - shown.length} more`}</span><span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>`;
    return out;
  };
  // The reader's own order and grouping per section (#s387): the app says which
  // sessions a section holds; the section's menu says how they are sorted and grouped here.
  const facts = {
    updated: item => item.row?.activityTs || 0,
    created: item => item.row?.startTs || 0,
    name: item => item.title || item.row?.name || item.id,
    project: item => where.get(item.id)?.project?.name || "",
    state: item => (item.row ? sessionBucket(item.row) : ""),
    now: Date.now() / 1000,
  };
  const groupIcon = view => ({ project: "folder", date: "clock", state: "list" })[view.group] || (source === "herdr" ? "terminal" : "folder");
  let html = "";
  for (const arranged of sections) {
    const key = `L:${source}:${arranged.id}`;
    const view = sectionView(indexState.sectionViews[key]);
    const section = viewSection(arranged, view, facts);
    const open = section.collapsed ? indexState.collapsed.has(key) : !indexState.collapsed.has(key);
    const custom = !isDefaultView(view);
    html += `<div class="agent-label arranged-head" role="treeitem" tabindex="0" data-toggle="${escapeText(key)}" aria-expanded="${open}"><span class="arranged-label">${escapeText(section.label)}</span><button class="arranged-view ${custom ? "on" : ""}" type="button" data-section-view="${escapeText(key)}" data-section-label="${escapeText(section.label)}" aria-haspopup="true" aria-expanded="${sectionMenuKey === key}" aria-label="${escapeText(`Sort and group ${section.label}`)}" title="${escapeText(custom ? `Sorted or grouped here, not as in ${layout.label}` : `Sort and group ${section.label}`)}">${svg("filterLines")}</button></div>`;
    if (!open) continue;
    html += list(section.nodes, `${key}:more`, false);
    for (const group of section.groups) {
      // A sub-head opens and closes like the app's too: a Codex project folder starts closed unless
      // the person opened it there, and is drawn even when it holds no session, as the app draws it.
      const groupKey = `${key}:${group.id}`, groupOpen = group.collapsed ? indexState.collapsed.has(groupKey) : !indexState.collapsed.has(groupKey);
      html += `<div class="tree-row project arranged-group" role="treeitem" tabindex="0" data-toggle="${escapeText(groupKey)}" aria-expanded="${groupOpen}" title="${escapeText(group.label)}">${svg(groupIcon(view))}<span class="tree-title">${escapeText(group.label)}</span></div>`;
      if (groupOpen) html += group.nodes.length ? list(group.nodes, `${groupKey}:more`, true) : `<div class="arranged-empty">No sessions</div>`;
    }
  }
  // Everything else the monitor lists: the filtered tree less what the app placed, collapsed.
  const restTree = shownAgents.map(agent => ({ ...agent, projects: agent.projects.map(project => ({ ...project, rows: project.rows.filter(row => !placed.has(row.id)) })).filter(project => project.rows.length) })).filter(agent => agent.projects.length);
  const rest = nestSideSessions(flatFamilies(restTree, Date.now() / 1000));
  if (rest.length) {
    const key = `L:${source}:rest`, open = indexState.collapsed.has(key);
    html += `<div class="agent-label arranged-head arranged-rest" role="treeitem" tabindex="0" data-toggle="${escapeText(key)}" aria-expanded="${open}" title="Sessions this monitor lists that ${escapeText(layout.label)} does not, under the session filter and hides"><span class="arranged-label">Not in ${escapeText(layout.label)}</span></div>`;
    if (open) {
      const overflowKey = `L:${source}:rest`, expanded = indexState.expandedProjects.has(overflowKey);
      let shown = expanded ? rest : rest.slice(0, FLAT_SESSION_LIMIT);
      const selectedFam = rest.find(fam => familyHolds(fam, indexState.selected));
      if (!expanded && selectedFam && !shown.includes(selectedFam)) shown = shown.concat(selectedFam);
      for (const fam of shown) html += familyRowsHtml(fam, { agent: { id: fam.agent.id, name: agentName(fam.agent.id) }, project: fam.project });
      const hidden = rest.filter(fam => !shown.includes(fam)).length;
      if (rest.length > FLAT_SESSION_LIMIT) html += `<button class="tree-project-more" type="button" data-project-more="${escapeText(overflowKey)}" aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${hidden} more`}</span><span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>`;
    }
  }
  return html;
}

/** A session the app lists that this monitor has no row for (#s387): the app's title, dimmed,
 *  saying so; choosing it still asks the monitor for the session, which may find it. */
function absentRowHtml(item, side, label, grouped) {
  const sideMark = side ? `<span class="tree-side" role="img" aria-label="Side session" title="${escapeText(side.via)}">↳</span>` : "";
  return `<div class="tree-row session flat arranged is-absent ${grouped && !side ? "in-group" : ""} ${side ? "is-side" : ""} ${item.id === indexState.selected ? "selected" : ""}"${side ? ` style="--side-depth:${side.depth}"` : ""} role="treeitem" tabindex="0" data-session="${escapeText(item.id)}" title="${escapeText(`Listed in ${label}; not in this monitor's list`)}"><span class="tree-main"><span class="tree-title">${sideMark}${escapeText(item.title || item.id)}</span><span class="tree-context"><span class="tree-project">Not in this monitor's list</span></span></span></div>`;
}

/** One agent's projects and their session rows, as the tree draws them — the sidebar and the
 *  collapsed rail's flyout (#300) both render from here, so the two cannot drift. `agent` is an
 *  entry of `visibleTree` (its projects carry the filtered `rows`). */
function agentProjectsHtml(agent) {
  let html = "";
  for (const project of agent.projects) {
    const rows = project.rows;
    const projectKey = `p:${project.id}`, openProject = !indexState.collapsed.has(projectKey);
    html += `<div class="tree-row project ${project.hidden ? "is-hidden" : ""}" role="treeitem" tabindex="0" data-toggle="${escapeText(projectKey)}" aria-expanded="${openProject}" title="${escapeText(project.path)}">${svg("folder")}<span class="tree-title">${escapeText(project.name)}</span>${agent.id === "codex" && options.projectActions?.() ? `<button class="native-project-new" type="button" data-native-project="${escapeText(project.nativeProjectId || "")}" data-native-cwd="${escapeText(project.path)}" aria-label="New session in ${escapeText(project.name)}" title="New session in ${escapeText(project.name)}"><svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14"/></svg></button>` : ""}${project.ignoreKey ? treeAction(project, project.kind) : ""}</div>`;
    if (!openProject) continue;
    const overflowKey = `${agent.id}:${project.id}`;
    const expanded = indexState.expandedProjects.has(overflowKey);
    const fams = nestSideSessions(families(rows));
    let shown = expanded ? fams : fams.slice(0, SIDEBAR_SESSION_LIMIT);
    const selectedFam = fams.find(fam => familyHolds(fam, indexState.selected));
    if (!expanded && selectedFam && !shown.includes(selectedFam)) shown = shown.concat(selectedFam);
    for (const fam of shown) html += familyRowsHtml(fam);
    const hidden = fams.filter(fam => !shown.includes(fam)).length;
    if (fams.length > SIDEBAR_SESSION_LIMIT) html += `<button class="tree-project-more" type="button" data-project-more="${escapeText(overflowKey)}" aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${hidden} more`}</span><span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>`;
  }
  return html;
}
function renderTree() {
  const agents = groupedSessions(); let html = "";
  // The filter's buckets over the rows in view (hidden rows count only under Include hidden;
  // a row can be recent AND blocked), and the tree filtered by the chosen set — every bucket
  // chosen is no filter (#202).
  const now = Date.now() / 1000;
  const bucketsOf = row => sessionFilterBuckets(row, now);
  const counts = { recent: 0, blocked: 0, idle: 0 }; let total = 0;
  indexState.rows.forEach(row => { if (row.startedBy || (row.hidden && !includeHidden())) return; total++; for (const bucket of bucketsOf(row)) counts[bucket]++; });
  const filtered = bucketFilterActive();
  const filteredAgents = visibleTree(agents, { showHidden: includeHidden(), buckets: filtered ? indexState.buckets : null, bucketsOf });
  const shownAgents = options.projectTree?.(filteredAgents, agents) || filteredAgents;
  const shown = shownAgents.reduce((n, agent) => n + agent.projects.reduce((m, project) => m + project.rows.length, 0), 0);
  lastShownAgents = shownAgents;
  // #s206: one list by activity, or the tree. Flat, no group is collapsed and nothing is left to
  // expand, so Collapse every group and Expand every group go with the heads (`sessions-flat`).
  // #s387: or another app's list, while it reads; until it does, or when it cannot be read, the
  // arrangement it replaced, under a line that says so.
  const source = arrangeSource();
  const layout = source && layoutState.source === source ? layoutState.data : null;
  const arranged = !!layout?.available;
  const flat = arrangement() === "flat" || (source && !arranged && indexState.arrangeBefore === "flat");
  paintArrangeStrip(source, layout);
  sessionList.classList.toggle("sessions-flat", flat || arranged);
  sessionList.classList.toggle("sessions-arranged", arranged);
  // The two group controls go with the heads; they are placed outside the list's element (stage 3), so
  // they carry the mark themselves.
  for (const control of [collapseBtn, expandBtn]) control.toggleAttribute("data-flat", flat || arranged);
  if (arranged) html = arrangedSessionsHtml(layout, agents, shownAgents);
  else if (flat) html = flatSessionsHtml(shownAgents);
  else for (const agent of shownAgents) {
    const agentKey = `a:${agent.id}`, openAgent = !indexState.collapsed.has(agentKey);
    html += `<div class="agent-label" role="treeitem" tabindex="0" data-toggle="${escapeText(agentKey)}" aria-expanded="${openAgent}">${escapeText(agent.name)}${agent.id === "codex" && options.projectActions?.() ? `<button class="native-project-add" type="button" data-native-add-project aria-label="Add Codex project" title="Add project"><svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M9 19H3V5h6l2 2h10v5M17 14v8m-4-4h8"/></svg></button>` : ""}</div>`;
    if (!openAgent) continue;
    html += agentProjectsHtml(agent);
  }
  // Written only when it reads differently (#s72). The index refreshes every 5 s and the list
  // usually reads the same; rewriting it anyway replaced every row under the reader, and a press
  // that straddled the write, down on a row and up on its replacement, made no click at all: the
  // session tapped just then was not chosen.
  const treeHtml = html || `<div class="no-results">${filtered ? "No sessions in this filter — Everything, in the filter, shows them all" : "No sessions"}</div>`;
  // A session row with the focus is carried across a rewrite, as a mark is below (stage 4): choosing
  // a session redraws the tree with its selection, so a list key moved the focus to the next row and
  // the redraw dropped it to <body>, and the next key was no longer the list's.
  const active = page.active();
  const focusedRow = tree.contains(active) && active.matches?.(".tree-row.session") ? active.dataset.session : "";
  if (treeHtml !== treeWritten) {
    tree.innerHTML = treeHtml; treeWritten = treeHtml;
    if (focusedRow) tree.querySelector(`.tree-row.session[data-session="${CSS.escape(focusedRow)}"]`)?.focus({ preventScroll: true });
  }
  renderFilterControl(counts, shown, total);
  refreshRailFlyout();

  // Redrawn on every index refresh; a focused mark would be replaced under the reader and the focus
  // dropped to <body> (#300 caught it handing focus back after Escape), so it is carried across.
  const focusedMark = page.active()?.closest?.("#sidebarMiniAgents [data-mini-agent]")?.dataset.miniAgent;
  railMarks.innerHTML = agents.map(agent => { const first = agent.projects.flatMap(project => project.sessions).find(row => !row.hidden); return first ? `<button class="sidebar-mini-agent ${agent.projects.some(project => project.sessions.some(row => row.id === indexState.selected)) ? "selected" : ""}" data-mini-agent="${escapeText(agent.id)}" title="${escapeText(agent.name)}" aria-haspopup="true" aria-expanded="${railFlyoutFor === agent.id}">${agentLogo(agent.id)}</button>` : ""; }).join("");
  if (focusedMark) railMarks.querySelector(`[data-mini-agent="${CSS.escape(focusedMark)}"]`)?.focus();
}

// The session filter (#202, the owner's shape): a glyph in the head-actions row — between
// Expand every group and the sidebar collapse — opening a sheet under the sidebar's head with
// three checkboxes: Active in the last 6 hours (#s253; any activity then, and every session busy
// now), Blocked (what "needs attention" means: a wait, or an idle reason that cut the work
// short) and Idle (neither), then Include hidden. "Everything" in the sheet's head checks the
// three and leaves Include hidden alone. Recent and blocked overlap by design (the shared
// `sessionFilterBuckets`); the set is remembered (`indexState.buckets`) and starts as Active
// recently and Blocked, the owner's default; every bucket is no filter; the set is never empty
// (unchecking the last puts every bucket back). The demo's
// Needs attention button and the Show Hidden control are gone — the sheet's Blocked and
// Include hidden checkboxes are those two by name — and the collapsed rail's bell with them;
// the rail gets a filter glyph that opens the sidebar and the sheet (the extraction cuts the demo's
// two from the app's shell, #s430 stage 3). Built at runtime so the extracted demo markup stays exact,
// and placed by the host with the list's other parts.
const filterBtn = document.createElement("button");
filterBtn.type = "button"; filterBtn.className = "iconbtn session-filter"; filterBtn.id = "filterBtn";
filterBtn.innerHTML = svg("filterLines");
filterBtn.setAttribute("aria-label", "Filter the sessions");
filterBtn.setAttribute("aria-expanded", "false"); filterBtn.setAttribute("aria-haspopup", "true"); filterBtn.setAttribute("aria-controls", "sessionFilter");
const filterSheet = document.createElement("div");
filterSheet.className = "navigator-options session-filter-options"; filterSheet.id = "sessionFilter"; filterSheet.hidden = true;
filterSheet.setAttribute("role", "group"); filterSheet.setAttribute("aria-label", "Show sessions");
// The sheet's three parts, each drawn while its control is shown (stage 4's `show`): the states, under
// the head with its Everything; Include hidden; and the arrangements. Shown together, as the app shows
// them, they are the sheet as it always was; Include hidden alone keeps the head, without Everything.
const sheetHead = everything => `<div class="scope-menu-head"><strong>SHOW</strong>${everything ? `<button class="scope-menu-action" type="button" data-filter-reset title="Every state — active in the last 6 hours, blocked and idle; Include hidden stays as it is">Everything</button>` : ""}</div>`;
const sheetParts = [];
if (shows("filter")) sheetParts.push(`${sheetHead(true)}
<div class="scope-menu-list">
<button class="scope-option" type="button" role="checkbox" aria-checked="false" data-bucket="recent" title="Any activity in the last 6 hours, and every session that is busy now"><span class="scope-check"></span><span>${FILTER_LABELS.recent}</span><span class="scope-count" data-bucket-count="recent"></span></button>
<button class="scope-option" type="button" role="checkbox" aria-checked="false" data-bucket="blocked" title="${BLOCKED_SUMMARY}"><span class="scope-check"></span><span>${FILTER_LABELS.blocked}</span><span class="scope-count" data-bucket-count="blocked"></span></button>
<button class="scope-option" type="button" role="checkbox" aria-checked="false" data-bucket="idle" title="Neither of the two — no activity in the last 6 hours and nothing owed"><span class="scope-check"></span><span>${FILTER_LABELS.idle}</span><span class="scope-count" data-bucket-count="idle"></span></button>
</div>`);
if (shows("hiding")) sheetParts.push(`${shows("filter") ? "" : `${sheetHead(false)}\n`}<div class="scope-menu-list">
<button class="scope-option" type="button" role="checkbox" aria-checked="false" data-include-hidden title="Show the sessions, projects and agents you have hidden, dimmed"><span class="scope-check"></span><span>Include hidden</span><span class="scope-count" data-bucket-count="hidden"></span></button>
</div>`);
if (shows("arrangements")) sheetParts.push(`<div class="scope-menu-head"><strong>ARRANGE</strong></div>
<div class="scope-menu-list" role="radiogroup" aria-label="Arrange the sessions">
<button class="scope-option" type="button" role="radio" aria-checked="false" data-arrange="tree" title="Grouped by agent, then by project"><span class="scope-check"></span><span>By agent and project</span></button>
<button class="scope-option" type="button" role="radio" aria-checked="false" data-arrange="flat" data-flat title="One list, most recent activity first, without the agent and project groups: each session names its project and agent"><span class="scope-check"></span><span>One list by activity</span></button>
${ARRANGE_SOURCES.map(key => `<button class="scope-option arrange-app" type="button" role="radio" aria-checked="false" data-arrange="${key}" title="${escapeText(`The sessions ${ARRANGE_LABELS[key]} shows, in its sections and order, following it as it changes; everything else under “Not in ${ARRANGE_LABELS[key]}”`)}"><span class="scope-check"></span><span class="arrange-text"><span>Like ${escapeText(ARRANGE_LABELS[key])}</span><small class="arrange-note" data-arrange-note="${key}"></small></span></button>`).join("")}
</div>`);
filterSheet.innerHTML = sheetParts.join(`\n<div class="scope-menu-divider"></div>\n`);
/** Whether the filter offers anything: with its three controls off, neither its glyph nor its sheet is
 *  drawn or handed to the host. */
const hasSheet = sheetParts.length > 0;
const filterMini = document.createElement("button");
filterMini.className = "sidebar-mini-button filter-mini"; filterMini.id = "sidebarMiniFilter"; filterMini.type = "button"; filterMini.title = "Filter the sessions";
filterMini.setAttribute("aria-label", "Filter the sessions");
filterMini.setAttribute("aria-haspopup", "true"); filterMini.setAttribute("aria-expanded", "false");
filterMini.innerHTML = svg("filterLines");
/** Whether the bucket set leaves anything out: never while the filter is switched off (stage 4). */
const bucketFilterActive = () => shows("filter") && FILTER_BUCKETS.some(bucket => !indexState.buckets.has(bucket));
/** Whether the reader's hidden sessions are listed: never while hiding is switched off (stage 4). */
const includeHidden = () => shows("hiding") && indexState.showHidden;
/** Set the buckets shown — never an empty set — and repaint everything that reads them. */
function setBuckets(next) {
  const set = new Set([...next].filter(bucket => FILTER_BUCKETS.includes(bucket)));
  indexState.buckets = set.size ? set : new Set(FILTER_BUCKETS);
  persist(); renderTree();
}
function toggleFilterSheet(open) {
  if (!open && railFlyoutFor === "filter") { closeRailFlyout(false); return; }
  if (open && shows("arrangements")) refreshSources();
  filterSheet.hidden = !open; filterBtn.setAttribute("aria-expanded", String(open));
  fitFilterSheet();
}
/** The sheet ends on the screen (#s206): its stylesheet caps it at 74% of the viewport, which
 *  forgets where it STARTS — under the drawer's head on a phone held sideways (a 292px page in
 *  Safari) it ran 16px past the screen's foot, half of Include hidden and all of the row below out
 *  of reach. Capped at the room below its top instead, it scrolls within what is on screen. */
function fitFilterSheet() {
  filterSheet.style.maxHeight = "";
  if (filterSheet.hidden || railFlyoutFor === "filter") return;
  const room = Math.floor(innerHeight - filterSheet.getBoundingClientRect().top - 8);
  if (room > 0 && filterSheet.scrollHeight > room) filterSheet.style.maxHeight = `${Math.max(120, room)}px`;
}
addEventListener("resize", fitFilterSheet, { signal });
filterBtn.onclick = () => toggleFilterSheet(filterSheet.hidden);
filterMini.onclick = () => { if (railFlyoutFor === "filter" && railFlyoutPinned) closeRailFlyout(false); else openRailFlyout("filter", filterMini, true); };
filterSheet.addEventListener("click", event => {
  if (event.target.closest("[data-filter-reset]")) { setBuckets(FILTER_BUCKETS); return; }
  if (event.target.closest("[data-include-hidden]")) { indexState.showHidden = !indexState.showHidden; renderTree(); return; }
  // #s387: an arrangement. One list by activity keeps its checkbox's toggle (#s206): chosen again,
  // it goes back to the tree. A source that cannot be read is not chosen.
  const arrange = event.target.closest("[data-arrange]");
  if (arrange) {
    if (arrange.getAttribute("aria-disabled") === "true") return;
    const next = arrange.dataset.arrange;
    setArrange(next === "flat" && indexState.arrange === "flat" ? "tree" : next);
    return;
  }
  const option = event.target.closest("[data-bucket]"); if (!option) return;
  const bucket = option.dataset.bucket;
  const next = new Set(indexState.buckets); if (next.has(bucket)) next.delete(bucket); else next.add(bucket);
  setBuckets(next);
});
document.addEventListener("click", event => { if (!filterSheet.hidden && !filterSheet.contains(event.target) && !filterBtn.contains(event.target) && !filterMini.contains(event.target)) toggleFilterSheet(false); }, { signal });
document.addEventListener("keydown", event => { if (event.key === "Escape" && !filterSheet.hidden) toggleFilterSheet(false); }, { signal });

// The collapsed rail's flyouts (#300, the owner: "only showing the agent icon is point-less, please
// show the projects and sessions for the agent in a float window", and the same for the filter).
// ONE floating panel beside the rail. For an agent it draws `agentProjectsHtml` — the rows the
// expanded tree draws, under the same filter — and answers clicks with the tree's own handler; for
// the filter it HOSTS the sidebar's filter sheet (the element itself, moved in and back), so there
// is one menu, one set of handlers and one remembered choice. Pointing opens it after a beat and it
// survives the trip from the mark into it (a grace period on leaving either); a click pins it;
// Escape, a click outside, or expanding the sidebar closes it. `var`, not `const`, so a renderTree
// that reached `refreshRailFlyout` before this block ran would find it declared; nothing draws the
// tree until the mount has returned (the index's first rows, or a host's `redraw` or `select`).
var lastShownAgents = [];
var railFlyout = null, railFlyoutFor = null, railFlyoutAnchor = null, railFlyoutPinned = false, railOpenTimer = 0, railCloseTimer = 0;
// Whether the list is drawn small, as the collapsed sidebar's rail (`compact(on)`, which the host
// calls): the rail's flyouts open only then.
var compactOn = false;
// Where the filter's sheet was placed (the host's head, or the list's own) while the flyout hosts it:
// it goes back there, before the node it stood before when that is still there.
var sheetHome = null;
function returnFilterSheet() {
  if (!sheetHome) return;
  const { parent, next } = sheetHome;
  // A host that left the sheet unplaced gets it back unattached, as it was.
  if (parent) parent.insertBefore(filterSheet, next && next.parentNode === parent ? next : null);
  else filterSheet.remove();
  sheetHome = null;
}
const RAIL_OPEN_MS = 140, RAIL_GRACE_MS = 280;
railFlyout = document.createElement("div");
railFlyout.className = "rail-flyout"; railFlyout.id = "railFlyout"; railFlyout.hidden = true;
railFlyout.setAttribute("role", "group");
layer("flyout").appendChild(railFlyout);
function railFlyoutBody() {
  if (railFlyoutFor === "filter") return null;
  const agent = lastShownAgents.find(item => item.id === railFlyoutFor);
  const name = agentName(railFlyoutFor);
  const rows = !agent ? "" : arrangement() !== "tree" ? flatSessionsHtml([agent], `flat:${agent.id}`) : agentProjectsHtml(agent);
  return `<div class="rail-flyout-head">${escapeText(name)}</div><div class="rail-flyout-tree" role="tree" aria-label="${escapeText(name)} sessions">${rows || `<div class="no-results">${bucketFilterActive() ? "No sessions in this filter" : "No sessions"}</div>`}</div>`;
}
function placeRailFlyout() {
  if (!railFlyoutAnchor) return;
  const r = railFlyoutAnchor.getBoundingClientRect();
  railFlyout.style.left = `${Math.round(r.right + 8)}px`;
  const h = railFlyout.offsetHeight;
  railFlyout.style.top = `${Math.round(Math.max(8, Math.min(r.top - 6, innerHeight - h - 8)))}px`;
}
function refreshRailFlyout() {
  if (!railFlyout || railFlyout.hidden || railFlyoutFor === "filter") return;
  const keep = railFlyout.querySelector(".rail-flyout-tree")?.scrollTop || 0;
  railFlyout.innerHTML = railFlyoutBody();
  const list = railFlyout.querySelector(".rail-flyout-tree"); if (list) list.scrollTop = keep;
  placeRailFlyout();
}
function openRailFlyout(kind, anchor, pinned) {
  clearTimeout(railOpenTimer); clearTimeout(railCloseTimer);
  if (!compactOn) return;
  if (railFlyoutFor !== kind) closeRailFlyout(false);
  railFlyoutFor = kind; railFlyoutAnchor = anchor; railFlyoutPinned = railFlyoutPinned || !!pinned;
  railFlyout.setAttribute("aria-label", kind === "filter" ? "Show sessions" : `${agentName(kind)} sessions`);
  if (kind === "filter") {
    if (filterSheet.parentElement !== railFlyout) { railFlyout.innerHTML = ""; sheetHome = { parent: filterSheet.parentNode, next: filterSheet.nextSibling }; railFlyout.appendChild(filterSheet); }
    filterSheet.hidden = false; filterMini.setAttribute("aria-expanded", "true");
  } else {
    railFlyout.innerHTML = railFlyoutBody();
  }
  railFlyout.classList.toggle("rail-flyout-filter", kind === "filter");
  railFlyout.hidden = false; anchor.setAttribute("aria-expanded", "true");
  placeRailFlyout();
}
function closeRailFlyout(refocus) {
  clearTimeout(railOpenTimer); clearTimeout(railCloseTimer);
  if (railFlyoutFor === "filter" && filterSheet.parentElement === railFlyout) {
    filterSheet.hidden = true; returnFilterSheet();
    filterMini.setAttribute("aria-expanded", "false");
  }
  // The marks are redrawn whenever the index refreshes, so the element that opened the flyout may
  // be gone: hand the focus to the CURRENT mark for what it showed.
  const kind = railFlyoutFor;
  const anchor = kind === "filter" ? filterMini : (railMarks.querySelector(`[data-mini-agent="${CSS.escape(kind || "")}"]`) || railFlyoutAnchor);
  if (anchor) anchor.setAttribute("aria-expanded", "false");
  railFlyout.hidden = true; railFlyout.innerHTML = ""; railFlyoutFor = null; railFlyoutAnchor = null; railFlyoutPinned = false;
  if (refocus && anchor && anchor.isConnected) anchor.focus();
}
function railFlyoutHover(kind, anchor) {
  clearTimeout(railCloseTimer);
  if (!railFlyout.hidden && railFlyoutFor === kind) return;
  clearTimeout(railOpenTimer);
  railOpenTimer = setTimeout(() => openRailFlyout(kind, anchor, false), railFlyout.hidden ? RAIL_OPEN_MS : 0);
}
function railFlyoutLeave() {
  clearTimeout(railOpenTimer);
  if (railFlyout.hidden || railFlyoutPinned) return;
  clearTimeout(railCloseTimer);
  railCloseTimer = setTimeout(() => closeRailFlyout(false), RAIL_GRACE_MS);
}
railMarks.addEventListener("pointerover", event => { const mark = event.target.closest("[data-mini-agent]"); if (mark) railFlyoutHover(mark.dataset.miniAgent, mark); });
railMarks.addEventListener("pointerenter", event => { const mark = event.target.closest?.("[data-mini-agent]"); if (mark) railFlyoutHover(mark.dataset.miniAgent, mark); }, true);
railMarks.addEventListener("pointerleave", railFlyoutLeave, true);
filterMini.addEventListener("pointerenter", () => railFlyoutHover("filter", filterMini));
filterMini.addEventListener("pointerleave", railFlyoutLeave);
railFlyout.addEventListener("pointerenter", () => clearTimeout(railCloseTimer));
railFlyout.addEventListener("pointerleave", railFlyoutLeave);
// Rows answer as the tree's rows do; choosing a session is the end of the errand.
railFlyout.addEventListener("click", event => {
  if (railFlyoutFor === "filter") return;
  railFlyoutPinned = true;
  const chose = event.target.closest("[data-session]") && !event.target.closest("[data-ignore-op], [data-family-toggle]");
  tree.onclick(event);
  if (chose) closeRailFlyout(false);
});
railFlyout.addEventListener("keydown", event => {
  if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); closeRailFlyout(true); return; }
  if (railFlyoutFor !== "filter" && ["Enter", " "].includes(event.key) && event.target.closest("[role=treeitem]")) { event.preventDefault(); event.target.click(); }
});
document.addEventListener("keydown", event => { if (event.key === "Escape" && !railFlyout.hidden) { event.stopPropagation(); closeRailFlyout(true); } }, { capture: true, signal });
document.addEventListener("pointerdown", event => { if (!railFlyout.hidden && !railFlyout.contains(event.target) && !railMarks.contains(event.target) && !filterMini.contains(event.target)) closeRailFlyout(false); }, { capture: true, signal });
addEventListener("resize", () => { if (!railFlyout.hidden) placeRailFlyout(); }, { signal });
/** The arrangement the list draws: the reader's, or by agent and project while the arrangements are
 *  switched off (stage 4). */
function arrangement() { return shows("arrangements") ? indexState.arrange : "tree"; }
function arrangeSource() { return ARRANGE_SOURCES.includes(arrangement()) ? indexState.arrange : null; }
function setArrange(next) {
  if (!["tree", "flat", ...ARRANGE_SOURCES].includes(next) || next === indexState.arrange) return;
  if (!arrangeSource()) indexState.arrangeBefore = indexState.arrange;
  indexState.arrange = next;
  if (layoutState.source !== arrangeSource()) layoutState = { source: null, data: null, error: null, text: null, loading: null };
  persist(); renderTree(); refreshLayout();
}
function refreshLayout() {
  const source = arrangeSource();
  if (!source || layoutState.loading) return layoutState.loading || Promise.resolve();
  const loading = backend().fetch(`/api/layout?source=${encodeURIComponent(source)}`, { cache: "no-store" })
    .then(response => { if (!response.ok) throw new Error(`HTTP ${response.status}`); return response.text(); })
    .then(text => {
      if (unmounted || arrangeSource() !== source) return;
      const data = JSON.parse(text);
      // The source's reader has not read yet (the server never reads in a request): ask again soon.
      if (data.pending) { retryLayout(); return; }
      // A read from before the reader last stopped is drawn, said to be updating, and asked again.
      if (data.stale) retryLayout();
      const changed = text !== layoutState.text || layoutState.error;
      layoutState = { ...layoutState, source, text, data, error: null };
      // A read that failed is said beside the choice; it never disables it (only missing files do).
      const known = sourceState[source] || { available: true, reason: "" };
      sourceState[source] = { ...known, lastError: data.available ? "" : data.reason || "" };
      if (changed) renderTree();
    })
    .catch(error => {
      if (unmounted || arrangeSource() !== source) return;
      const changed = layoutState.error !== error.message;
      layoutState = { ...layoutState, source, data: null, text: null, error: error.message };
      if (changed) renderTree();
    })
    .finally(() => { if (layoutState.loading === loading) layoutState.loading = null; });
  layoutState.loading = loading;
  return loading;
}
function refreshSources() {
  return backend().fetch("/api/layout", { cache: "no-store" })
    .then(response => response.ok ? response.json() : null)
    .then(data => {
      if (!data || unmounted) return;
      sourceState = Object.fromEntries((data.sources || []).map(item => [item.source, { available: !!item.available, reason: item.reason || "", lastError: item.lastError || "" }]));
      renderTree();
    })
    .catch(() => {});
}
/** The line over an arranged list: which app it follows (and anything the app's list leaves out),
 *  that it is being read, or that it cannot be — with a ✕ back to the arrangement it replaced. */
function paintArrangeStrip(source, layout) {
  if (!arrangeStrip) {
    arrangeStrip = document.createElement("div");
    arrangeStrip.className = "arrange-strip"; arrangeStrip.id = "arrangeStrip"; arrangeStrip.hidden = true;
    arrangeStrip.setAttribute("role", "status");
    tree.insertAdjacentElement("beforebegin", arrangeStrip);
    arrangeStrip.addEventListener("click", event => { if (event.target.closest("[data-arrange-off]")) setArrange(indexState.arrangeBefore); });
  }
  if (!source) { arrangeStrip.hidden = true; return; }
  const label = ARRANGE_LABELS[source];
  const back = indexState.arrangeBefore === "flat" ? "one list by activity" : "by agent and project";
  let text, detail = "", warn = false;
  if (layout?.available) {
    text = layout.stale ? `Arranged like ${label} · updating…` : `Arranged like ${label} · follows it`;
    const empty = (layout.sections || []).every(section => !(section.entries || []).length && (section.groups || []).every(group => !(group.entries || []).length));
    detail = layout.note || (empty ? `${label[0].toUpperCase()}${label.slice(1)} lists no sessions right now` : "");
  }
  // A failed read from before the reader last stopped is old news: the source is being read again.
  else if (layout?.stale) text = `Reading ${label}'s list…`;
  else if (layout || layoutState.error) { warn = true; text = `Showing ${back}: ${label}'s list could not be read`; detail = layout?.reason || layoutState.error || ""; }
  else text = `Reading ${label}'s list…`;
  const html = `<div class="arrange-strip-box"><span class="arrange-strip-text"><span>${escapeText(text)}</span>${detail ? `<small>${escapeText(detail)}</small>` : ""}</span><button class="arrange-strip-close" type="button" data-arrange-off aria-label="${escapeText(`Stop arranging like ${label}`)}" title="${escapeText(`Stop arranging like ${label} — back to ${back}`)}">${svg("x")}</button></div>`;
  if (arrangeStrip.dataset.html !== html) { arrangeStrip.innerHTML = html; arrangeStrip.dataset.html = html; }
  arrangeStrip.classList.toggle("warn", warn);
  arrangeStrip.hidden = false;
}

/** Paint the glyphs and the sheet from the one state. */
function renderFilterControl(counts, shown, total) {
  if (!indexState.ignoredCount) indexState.showHidden = false;
  const filtered = bucketFilterActive();
  const chosen = FILTER_BUCKETS.filter(bucket => indexState.buckets.has(bucket)).map(bucket => FILTER_LABELS[bucket]);
  filterBtn.classList.toggle("on", filtered); filterMini.classList.toggle("on", filtered);
  filterBtn.title = filtered ? `Showing ${chosen.join(" and ")} — ${shown} of ${total}` : "Filter the sessions — active in the last 6 hours, blocked, idle — and include hidden ones";
  for (const option of filterSheet.querySelectorAll("[data-bucket]")) {
    const on = indexState.buckets.has(option.dataset.bucket);
    option.classList.toggle("on", on); option.setAttribute("aria-checked", String(on));
  }
  const include = filterSheet.querySelector("[data-include-hidden]");
  if (include) { include.classList.toggle("on", indexState.showHidden); include.setAttribute("aria-checked", String(indexState.showHidden)); }
  for (const option of filterSheet.querySelectorAll("[data-arrange]")) {
    const key = option.dataset.arrange, on = indexState.arrange === key;
    option.classList.toggle("on", on); option.setAttribute("aria-checked", String(on));
    if (!ARRANGE_SOURCES.includes(key)) continue;
    // A source whose files are not there is offered with the reason, and not chosen — unless it is
    // the one chosen, which stays checked and says why it is not drawn. A source whose last read
    // failed stays choosable (choosing it is what reads it again) and says what the read met.
    const known = sourceState[key];
    const off = !!known && !known.available && !on;
    option.classList.toggle("disabled", off); option.setAttribute("aria-disabled", String(off));
    const note = filterSheet.querySelector(`[data-arrange-note="${key}"]`);
    if (note) note.textContent = !known ? "" : !known.available ? known.reason || "Cannot be read" : known.lastError ? `Last read: ${known.lastError}` : "";
  }
  for (const el of filterSheet.querySelectorAll("[data-bucket-count]")) {
    const key = el.dataset.bucketCount;
    el.textContent = String(key === "hidden" ? indexState.ignoredCount : counts[key] || 0);
  }
}
async function applyIgnore(op, key) {
  try {
    const response = await backend().fetch(ignoreQuery({ op, key }), { cache: "no-store" });
    if (unmounted) return;
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    onToast(op === "add" ? "Hidden — find it under Hidden" : "Restored");
    await sessionIndex.refresh();
  } catch (error) { if (!unmounted) onToast(`Could not ${op === "add" ? "hide" : "restore"}: ${error.message}`); }
}

tree.onclick = event => {
  // #s387: a section head's menu, before the head's own toggle.
  const viewButton = event.target.closest("[data-section-view]");
  if (viewButton) { event.stopPropagation(); openSectionMenu(viewButton.dataset.sectionView, viewButton.dataset.sectionLabel || "", viewButton); return; }
  const addProject = event.target.closest("[data-native-add-project]");
  if (addProject) { event.stopPropagation(); options.newSession?.("add"); return; }
  const newSession = event.target.closest("[data-native-project]");
  if (newSession) { event.stopPropagation(); options.newSession?.({ id: newSession.dataset.nativeProject, cwd: newSession.dataset.nativeCwd }); return; }
  const action = event.target.closest("[data-ignore-op]"); if (action) { event.stopPropagation(); applyIgnore(action.dataset.ignoreOp, action.dataset.ignoreKey); return; }
  const forks = event.target.closest("[data-family-toggle]"); if (forks) { event.stopPropagation(); const key = forks.dataset.familyToggle; indexState.openFamilies.has(key) ? indexState.openFamilies.delete(key) : indexState.openFamilies.add(key); renderTree(); return; }
  const session = event.target.closest("[data-session]"); if (session) { chooseSession(session.dataset.session, true); return; }
  const more = event.target.closest("[data-project-more]"); if (more) { const key = more.dataset.projectMore; indexState.expandedProjects.has(key) ? indexState.expandedProjects.delete(key) : indexState.expandedProjects.add(key); persist(); renderTree(); return; }
  const toggle = event.target.closest("[data-toggle]"); if (toggle) { const key = toggle.dataset.toggle; indexState.collapsed.has(key) ? indexState.collapsed.delete(key) : indexState.collapsed.add(key); persist(); renderTree(); }
};
tree.onkeydown = event => { if (["Enter", " "].includes(event.key)) { event.preventDefault(); event.target.click(); } };
// A mark's click PINS its flyout (#300) — it used to jump to the agent's newest session, which the
// flyout now lists first under its project. Enter and Space are the same click; ArrowRight too.
railMarks.onclick = event => {
  const mark = event.target.closest("[data-mini-agent]"); if (!mark) return;
  if (!railFlyout.hidden && railFlyoutFor === mark.dataset.miniAgent && railFlyoutPinned) { closeRailFlyout(false); return; }
  openRailFlyout(mark.dataset.miniAgent, mark, true);
  if (event.detail === 0) railFlyout.querySelector("[role=treeitem]")?.focus();
};
railMarks.onkeydown = event => { if (event.key === "ArrowRight") { const mark = event.target.closest("[data-mini-agent]"); if (mark) { event.preventDefault(); mark.click(); } } };
collapseBtn.onclick = () => { for (const agent of groupedSessions()) { indexState.collapsed.add(`a:${agent.id}`); for (const project of agent.projects) indexState.collapsed.add(`p:${project.id}`); } persist(); renderTree(); };
expandBtn.onclick = () => { indexState.collapsed.clear(); persist(); renderTree(); };
function stepList(delta) {
  const rows = [...tree.querySelectorAll(".tree-row.session")];
  const at = rows.indexOf(page.active()); if (at < 0) return;
  const next = Math.max(0, Math.min(rows.length - 1, at + delta));
  rows[next].focus(); chooseSession(rows[next].dataset.session, true);
}
// The list's keys (stage 4), by the keymap's names: the next and the previous session row from the
// focused one. Engaged (the default), the list binds them on its own element, so they answer only while
// one of its rows has the focus, and every other key is the page's; with the page's keyboard (the
// app's) the host's keymap calls them through the handle.
const keyActions = { "list-next": () => stepList(1), "list-prev": () => stepList(-1) };
if (keyboardMode === "engaged") element.addEventListener("keydown", event => {
  if (!event.target?.closest?.(".tree-row")) return;
  const run = keyActions[resolveKey(event, "list", event.target)?.action];
  if (run) { event.preventDefault(); run(); }
}, { signal });

// The list's parts its host shows (stage 3): the head's controls, Collapse every group, Expand every
// group and the filter, in that order before the sidebar's collapse; the filter's sheet, hung from the
// head; and the rail's filter and agent marks. A host that places nothing gets the controls and the
// sheet in a head at the top of the list, the sheet's containing block, and no rail. The controls are
// one strip (stage 4), an element the host can hold and place again, `display: contents` so each control
// is an item of the row it is placed in, as each was when the three were placed loose. A filter that
// offers nothing is neither drawn nor handed over.
const controls = document.createElement("div");
controls.className = "session-list-controls";
controls.append(collapseBtn, expandBtn);
if (hasSheet) controls.append(filterBtn);
if (options.place) {
  options.place("controls", controls);
  if (hasSheet) options.place("sheet", filterSheet);
  if (hasSheet) options.place("rail-filter", filterMini);
  options.place("marks", railMarks);
} else {
  const head = document.createElement("div");
  head.className = "session-list-head";
  head.append(controls);
  if (hasSheet) head.append(filterSheet);
  element.prepend(head);
}

tree.innerHTML = '<div class="no-results">Scanning sessions…</div>'; treeWritten = null;
railMarks.innerHTML = "";

/** The list's half of choosing a session: it is the selection, read, remembered and drawn. */
function take(id) {
  indexState.selected = id; indexState.selectedWasRow = indexState.rows.has(id);
  const row = selectedRow(); if (row) { indexState.read[id] = row.activityTs || Date.now() / 1000; persist(); }
  renderTree();
}
/** The reader chose a session in the list: the list tells the host, which takes it as the
 *  selection (`select`) when it opens it. The list's selection is the host's to move, so a host
 *  that holds the reader (a draft to keep, say) leaves it where it is. */
function chooseSession(id, push) {
  if (!id) return;
  options.onOpen?.({ id, push });
}

// Once the list is unmounted every member of its handle does nothing and answers nothing (stage 4), as
// the view's (its stage 7): a host still holding it, or a late callback of its own, reaches nothing of
// a list that is gone. The key actions are fenced one by one.
function fenced(handle) {
  const fence = run => function (...args) { return unmounted ? undefined : run.apply(this, args); };
  for (const [name, value] of Object.entries(handle)) {
    if (name === "unmount") continue;
    if (typeof value === "function") handle[name] = fence(value);
    else if (value && typeof value === "object") handle[name] = Object.fromEntries(Object.entries(value).map(([key, run]) => [key, typeof run === "function" ? fence(run) : run]));
  }
  return handle;
}

return fenced({
  /** Draw the list again, from the index and the choices as they stand. */
  redraw: () => renderTree(),
  /** Read the index now. */
  refresh: () => loadSessions(),
  /** Read the index, and again every 5 s; `stop` pauses it. */
  start: () => sessionIndex.start(),
  stop: () => sessionIndex.stop(),
  /** The index grouped by agent and project, less the headless workers. */
  grouped: () => groupedSessions(),
  /** Every row of the index, a session's by its id, and the headless workers a session started. */
  rows: () => [...indexState.rows.values()],
  row: id => indexState.rows.get(id) || null,
  workers: id => sessionWorkers(id),
  /** The selected session ("" for none). */
  selected: () => indexState.selected,
  /** A selection the host made (the history, the session search, the view's parent control, the
   *  first session): the list's half, without telling the host. */
  select: id => { if (id) take(id); },
  /** The list's key actions, by the keymap's names: the next and the previous session row. */
  keys: keyActions,
  /** The list drawn small, as the collapsed sidebar's rail (the host's to say): the rail's flyouts
   *  open only then, and one open closes when it is no longer. */
  compact: on => { compactOn = !!on; if (!compactOn && !railFlyout.hidden) closeRailFlyout(false); },
  /** Where the index reads its rows and how it groups them (native mode's), or the monitor's. */
  setSource: ({ load = null, group = null } = {}) => { sessionIndex.load = load; sessionIndex.group = group; },
  /** The session filter: set its buckets (never none), and whether it leaves anything out. */
  setBuckets: buckets => setBuckets(buckets),
  filtered: () => bucketFilterActive(),
  /** The list's theme, "light" or "dark", on its element. */
  setTheme: theme => applyTheme(theme),
  /** Take the list away (stage 4). Everything it started stops: its listeners on the page (one abort
   *  signal), its timers and the index's polls, whose read still out does nothing when it comes back,
   *  as another app's list or a hide does. What it put outside its element goes, wherever the host put
   *  it: the strip of controls, the filter's sheet (from the rail's flyout too) and its rail button, the
   *  agent marks, the flyout, a section's menu and its scrim, the safe-area probe. The element is left
   *  as the host gave it: empty, with its own attributes. The list tells its host nothing more, and
   *  every other member of the handle does nothing (`fenced`). Mounting again in the same element draws
   *  a new list. */
  unmount() {
    if (unmounted) return;
    unmounted = true;
    sessionIndex.stop();
    life.abort();
    for (const id of timers) globalThis.clearTimeout(id);
    timers.clear();
    for (const node of [controls, filterSheet, filterMini, railMarks, railFlyout, sectionMenu, sectionScrim, insetProbe]) node?.remove();
    element.replaceChildren();
    for (const { name } of [...element.attributes]) element.removeAttribute(name);
    for (const [name, value] of elementBefore) element.setAttribute(name, value);
  },
});
}
