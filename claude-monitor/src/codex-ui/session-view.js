// The session view (#s429, design/session-view-component.md, stage 3): the transcript and its
// viewport, the turn bar and search, the outline panes, the artifact pane, the compose box and its
// dialogs, and the phone's dock and sheets — everything of one session, behind one entry point,
// `mountSessionView`. The app shell (app.js) mounts it as a host would and keeps only what is the
// app's: the session list, the drawer and the rail, the brand, the theme, the session search and the
// toast. What the view needs from the app comes in as options; what the app asks of it is the handle.
//
// The function's body sits at the margin ON PURPOSE. It is the view's code as it stood at app.js's
// top level before the split, moved line for line (`git diff --color-moved` shows it so), and the
// frontend contract's patterns read it unchanged (several pin a column or an indent). A whitespace-
// only commit can indent it once the stages that rewrite it (the template, the styles, the events
// and options) are done.
import { svg } from "./icons.js";
import { SESSION_VIEW_MARKUP } from "./session-view-markup.js";
import { backend, useBackend } from "./backend.js";
import { AttachmentViewer } from "./attachment-viewer.js";
import { bindComponentEvents, fleetHtml, pendingHeadText } from "./components.js";
import { referenceAction } from "./shared/capabilities.js";
import { M, mountPaneLayout } from "./pane-layout.js";
import { costDisplay, reportedCostDisplay } from "./shared/cost-display.js";
import { chainWalk, toolTree } from "./shared/filter.js";
import { taskCardHtml, taskRowMeta, TASK_NO_TITLE } from "./shared/task-card.js";
import { ControlStore } from "./control-store.js";
import { Preview } from "./preview.js";
import { RecordStore } from "./record-store.js";
import { createRecordState, createViewState, persistView } from "./view-state.js";
import { displayState } from "./shared/state-labels.js";
import { DEFAULT_READING, SIZE_STEP, clampSize, readingVars } from "./shared/reading.js";
import { RUNTIME_ALWAYS, runtimeRows, runtimeText } from "./shared/runtime.js";
import { hintFor, inGuest, resolveKey } from "./shared/keymap.js";
import { CLASS_BIT, LIVE_SEARCH_LIMIT, directMask, activeLetters, countOcc, querySpans, recordTextParts, recordTextSize, scopeLetters, scopeMask, stripTags, splitQuery, zeroCounts, countRecord, countLabel, writePrefix, prefixOf, CLASS_ORDER, wholeAt, toolMatches, recordHasTool, toolLetters as sessionToolLetters, resolveTools as resolveToolSpec } from "./shared/search.js";
import { agentName, agentRecordTargets, currentTurnIndex, escapeText, peerHue, peerLabelRest, plainText, recordForSearch, Projection, taskRecordTargets, taskStatus, taskGroups, taskCenterTarget, taskDetails, artifactRoster, compactionTick, replyContext } from "./view-model.js";
import { Viewport } from "./viewport.js";
import { sharedKeyboardFrame } from "./phone-keyboard.js";
import { revealMermaidSource, useMermaidRuntime, useMermaidTheme } from "./shared/mermaid.js";
import { page, viewRoot } from "./view-root.js";

/** The outline's panes a phone opens from its dock as sheets, by key, label and list element. A wide
 *  phone's column shows the same lists under the app's tabs, so the app reads this too. */
export const PHONE_PANES = Object.freeze([["turns", "Turns", "navigatorTurns"], ["tasks", "Tasks", "navigatorWork"], ["agents", "Agents", "navigatorAgents"]].map(pane => Object.freeze(pane)));

/** The controls a host may switch off (#s436; design/session-view-component.md, stage 6), each shown
 *  unless the `show` option says `false`. */
export const OPTIONAL_CONTROLS = Object.freeze(["reading", "history", "detach"]);
/** How the view takes its keys (the `keyboard` option): "engaged" binds them itself, answering only
 *  while the reader works in the view; "page" leaves the page's keyboard to the host, which calls the
 *  handle's `keys`. */
export const KEYBOARD_MODES = Object.freeze(["engaged", "page"]);
/** Every option `mountSessionView` takes; any other name is refused, so a misspelt or retired one
 *  (the old `toast`, `keys` or `controls`) fails loudly rather than being dropped. */
export const MOUNT_OPTIONS = Object.freeze([
  "theme", "grid", "backend", "show", "keyboard", "write", "frame",
  "selectedId", "sessionRow", "sessionWorkers", "selectSession", "renderList", "refreshIndex", "pauseIndex", "resumeIndex",
  "onTitle", "onTurn", "onInfo", "onFile", "onLinkCopied", "onToast", "onOutline",
  "room", "parentShown", "openList", "openSidebar", "paneLayout", "paneCount",
]);

/** Draw the session view in `element` and return its handle. The element becomes `#sessionView`, the
 *  id the view's stylesheet hangs every rule on; one with another id is refused. The view always
 *  draws into the page (#s438): no shadow root.
 *
 *  A host's page carries nothing of the view's: no markup, no element the view binds by id, and no
 *  store (stage 7: the view makes its own as it mounts, view-state.js).
 *
 *  Options, each named in `MOUNT_OPTIONS`; any other name is refused. Required: `grid`, `selectedId`,
 *  `sessionRow`, `sessionWorkers`, `selectSession`, `renderList`, `refreshIndex`, `pauseIndex` and
 *  `resumeIndex`. The rest are optional: a host may leave out `theme`, `backend`, `show` and
 *  `keyboard` (each has a default), `write` (writes are not offered without it), `frame` (the
 *  native app's), and every handler of what the view tells it.
 *  - `theme`: "light" (the default) or "dark", carried by the view's element, where its stylesheet
 *    reads it; the handle's `setTheme` changes it.
 *  - `grid`: the box around the view (the app's `#app`). The phone's sheets are measured against it
 *    (`--phone-top`, `--phone-bottom`), and it is the box moved back over iOS's page scroll while
 *    the keyboard is up, whose height native mode's keyboard frame reads. Embedding's step 6 makes
 *    the view's own element that box.
 *  - `backend`: the backend handle the view reads its session through (`useBackend`'s); the page's
 *    own handle stands without one.
 *  - `show`: which optional controls are drawn, by name, as `{ reading: false }`. Each is shown unless
 *    it is `false`; a name not in `OPTIONAL_CONTROLS`, or a value that is not a boolean, is refused.
 *    A control switched off is not drawn and takes no key.
 *    - `reading`: the Reading menu (Aa) and its keys (`w`, `-`, `+`). Off, the reader's stored
 *      reading preferences (this origin's `am-prod-reading`) still apply as they are.
 *    - `history`: the viewport history's Save, a row of the Reading menu.
 *    - `detach`: the artifact pane's ↗, which opens the document or image in a tab of its own.
 *  - `keyboard`: "engaged" (the default) or "page", as `KEYBOARD_MODES` says. Engaged, the view answers
 *    its keymap only while the reader's last press or focus is inside its element (or the task card
 *    it floats over the page), and yields to a guest inside it (mdrev's pane), as mdrev does; a key
 *    it has no action for is left to the page, and the pane's image viewer keeps to the same rule.
 *    "page" is the app's mode: the view binds none, and the host owns the page's keyboard and calls
 *    `keys[action]`. A host that uses it gets the action names, not the key table (shared/keymap.js,
 *    which the app binds with its list's keys and contexts).
 *  - `write`: what writing to a session needs from the host, which owns write mode; left out, the
 *    view offers no writes (no compose button, no ✎ in a phone's dock, no notice that a session
 *    cannot be written to). An object of two functions:
 *    - `on()`: whether the reader has writing switched on. The view reads it whenever it paints the
 *      compose box; the host calls the handle's `writeChanged()` when it changes.
 *    - `confirm({ title, text, passcode })`: ask the reader before the monitor types into a live
 *      pane (a grant), titled `title` and saying `text`, with a passcode field when `passcode` is
 *      true (the monitor requires one, or refused the one given). It answers (or resolves to) the
 *      passcode the reader gave, "" where none was asked, to go on, and anything else to cancel. An
 *      ask while another is open answers that one as cancelled: only the last goes on.
 *    The app passes its write switch and its confirm layer.
 *  - `frame(html)`: the native app's own arrangement (#s436), not offered to other hosts, and the
 *    kit's guide does not list it. The artifact pane shows an agent's HTML page in a frame sandboxed
 *    without its origin (`sandbox="allow-scripts"`, no referrer), whose document is the page's text
 *    with the preview's policy in front (sandbox.js). Without this option it is the frame's `srcdoc`,
 *    which also takes the policy of the page around it; the native app's page allows no scripts, so
 *    there the page's own would not run. With it, the view hands that document to `frame`, which
 *    answers (or resolves to) the address the frame loads instead: the native app serves the
 *    document from a scheme of its own, under the same policy (design/monitor-hub.md, D3). It is asked
 *    once per tab while the tab's text is the same, and that address is used again for the tab's whole
 *    life, across session switches. No address is ever released and nothing says one is no longer
 *    needed: the native app keeps every address it gave for the life of the page, unless it decides
 *    otherwise itself. An answer that is not an address, or a refusal,
 *    shows the pane's own error (and is asked again the next time the tab is shown).
 *  - `selectedId()`, `sessionRow(id)`, `sessionWorkers(id)`: what the app's session index says —
 *    the session it has selected, a session's row, and the headless workers a session started.
 *  - `selectSession(id, push)`: ask the app to select a session (the parent control, a descent).
 *  - `renderList()`, `refreshIndex()`, `pauseIndex()`, `resumeIndex()`: the session list and
 *    its index, for the compose box's controls (native mode's among them).
 *
 *  What the view tells its host, each with plain data (never a node), each optional. A handler that
 *  throws is reported (`reportError`) and the view goes on; what the view says while it mounts (its
 *  first title) reaches the host a microtask after `mountSessionView` has returned, so a handler may
 *  use the handle.
 *  - `onTitle({ session, title })`: the title its top bar shows changed (a session opened, its
 *    row renamed; `session` "" and the placeholder title for none).
 *  - `onTurn({ session, turn, label })`: the turn the reader is in changed, said once the engine has
 *    placed the reader (after a transaction or a scroll, once per task); `turn` is null for none,
 *    and for a session just opened until its own records arrive.
 *  - `onInfo({ session })`: the reader pressed for the session's Info (the view opens it itself).
 *  - `onFile({ name, path, text })`: a file opened in the artifact pane: the file's name, its path
 *    ("" for none), and the text the transcript carries for it (null for none).
 *  - `onLinkCopied({ url, record })`: the reader copied a link to a record.
 *  - `onToast(message)`: say something briefly. A host that leaves it out hears nothing of what the
 *    view says this way, a send the compose box held or the server refused among it.
 *  - `onOutline({ wanted })`: whether the reader wants the outline open (#s431 stage 3,
 *    design/pane-layout.md, "The view asks its host for room"), said once as the mount returns, with
 *    the reader's remembered choice, and again whenever the reader opens or folds it; never for a fold
 *    or a return the view's width forces, which leaves the choice as it was. `wanted` is a boolean.
 *  - `room()`: how many more pixels the host could give the view on its own, by narrowing what it lays
 *    beside it; asked at each layout decision, and only to decide whether the outline's control may
 *    open the outline, which the host then makes room for when it hears `onOutline`. A finite number
 *    above 0 is the room, and anything else (or a throw, which is reported) is none. One that is not a
 *    function is refused.
 *  - `parentShown(id)`: the top bar offers a way back to this parent (the app writes the browser's
 *    Back to it for a deep-linked child).
 *  - `openList()`, `openSidebar()`: the top bar's back-to-the-list and reopen controls, the app's.
 *  - `paneLayout(name, on)`: the artifact pane opens or shuts, goes full or is being resized; said
 *    before the view wears the state `on` names (#s431 stage 3), so the app lays out its list for the
 *    pane first and the view decides its arrangement at the box that leaves it.
 *  - `paneCount(key, text)`: a phone pane's count changed (a wide phone's column shows it too).
 *
 *  The handle: `select(id)` and `open(id)` (a session chosen: the view takes it, then reads it),
 *  `session()`, `close(title, detail)`, `indexChanged()`, `writeChanged()`, `resized()`,
 *  `setTheme(theme)`, `keys` (the view's key actions), `closePane()`, `paneNode(key)` and
 *  `outlineClick(event)` (the wide phone's column), `persist()` (remember the reader's choices of
 *  the view now), `unmount()`, and, for the monitor's own native mode, `controls` and
 *  `installNativeControls(…)`.
 *
 *  `unmount()` takes the view away: it stops all it started (its listeners on the page, its timers,
 *  frames and observers, the record store's polls, the pane's and the viewport's own), and work
 *  still out when it is called (a send, a grant, the host's confirm, a dialog's read, a download, a
 *  server search) does nothing when it comes back. It removes what it put outside its element; where
 *  it wrote on the host's (the page root's measured properties, the `grid`'s transform) and its
 *  value is still there, it puts back the host's value from the mount (or removes it), and leaves a
 *  property it never wrote, or one the host wrote since, alone; and it leaves the element as the host
 *  gave it: empty, with its own attributes. The element must be empty when the view mounts (nothing
 *  in it but white space): one holding an element, text or a comment is refused. After the
 *  unmount every other member of the handle does nothing and answers nothing (`controls` too), and
 *  the view tells its host nothing more; mounting again in the same element draws a new view. */
export function mountSessionView(element, options) {
for (const name of Object.keys(options)) if (!MOUNT_OPTIONS.includes(name)) throw new Error(`mountSessionView takes no option named ${name}; its options are ${MOUNT_OPTIONS.join(", ")}`);
for (const [name, shown] of Object.entries(options.show || {})) {
  if (!OPTIONAL_CONTROLS.includes(name)) throw new Error(`mountSessionView has no optional control named ${name}; the controls a host may switch off are ${OPTIONAL_CONTROLS.join(", ")}`);
  if (typeof shown !== "boolean") throw new Error(`mountSessionView shows ${name} or not by true or false, not ${JSON.stringify(shown)}`);
}
const keyboardMode = options.keyboard ?? "engaged";
if (!KEYBOARD_MODES.includes(keyboardMode)) throw new Error(`mountSessionView takes its keyboard "engaged" or "page", not ${JSON.stringify(keyboardMode)}`);
if (options.write !== undefined && !(typeof options.write?.on === "function" && typeof options.write?.confirm === "function")) throw new Error("mountSessionView takes its write as { on, confirm }, two functions, or none for a host that offers no writes");
if (options.room !== undefined && typeof options.room !== "function") throw new Error("mountSessionView takes its room as a function answering how many pixels its host could give, or none");
if (options.frame !== undefined && typeof options.frame !== "function") throw new Error("mountSessionView takes its frame as a function from a page's HTML to the address its frame loads");
/** Whether an optional control is drawn: each is, unless the host switched it off. */
const shows = name => options.show?.[name] !== false;
// What the view starts outside its element, which `unmount` stops (stage 7). Every listener it binds
// on the page (the document, the window, the visual viewport, a media query) carries `signal`; the
// timers, frames and observers its code starts are made through the names below, which keep each
// one until it has run or been cleared, so nothing the view's code calls `setTimeout`,
// `requestAnimationFrame`, `ResizeObserver` or `MutationObserver` outlives it. What the modules
// under it start, each stops itself (`destroy`).
const life = new AbortController(), signal = life.signal;
const timers = new Set(), frames = new Set(), observers = new Set();
let unmounted = false;
const setTimeout = (run, ms, ...args) => {
  const id = globalThis.setTimeout(() => { timers.delete(id); run(...args); }, ms);
  timers.add(id);
  return id;
};
const requestAnimationFrame = run => {
  const id = globalThis.requestAnimationFrame(time => { frames.delete(id); run(time); });
  frames.add(id);
  return id;
};
const clearTimeout = id => { timers.delete(id); globalThis.clearTimeout(id); };
const cancelAnimationFrame = id => { frames.delete(id); globalThis.cancelAnimationFrame(id); };
const kept = Observer => class extends Observer { constructor(callback) { super(callback); observers.add(this); } };
const ResizeObserver = kept(globalThis.ResizeObserver), MutationObserver = kept(globalThis.MutationObserver);
// The element as the host gave it, which `unmount` puts back: its attributes (the id, the class, the
// theme and the reading options the view writes on it) and no children.
const elementBefore = [...element.attributes].map(attribute => [attribute.name, attribute.value]);
// …and what the host had set where the view may write too: the properties the view measures onto the
// page's root, the transform it moves the host's box by over a phone's keyboard, and whether either had
// a style of its own. `unmount` puts back the host's value only where the view wrote and its value is
// still there (`viewWrote`, below).
const ROOT_PROPERTIES = ["--preview", "--phone-top", "--phone-bottom", "--phone-kb"];
const hostStyles = [[viewRoot.styleRoot(), ROOT_PROPERTIES], [options.grid, ["transform"]]].filter(([node]) => node).map(([node, names]) => ({
  node, style: node.getAttribute("style"),
  values: names.map(name => [name, node.style.getPropertyValue(name), node.style.getPropertyPriority(name)]),
}));
// What the view itself wrote there, by element and property: the unmount puts back only a property
// whose value is still the one the view last wrote. One the view never wrote, or the host has
// written since, is the host's and stays as it is.
const viewWrote = new Map();
/** Write a style property on the page's root or the host's box, and keep that the view wrote it. */
function writeStyle(node, name, value) {
  if (!node) return;
  if (value) node.style.setProperty(name, value); else node.style.removeProperty(name);
  if (!viewWrote.has(node)) viewWrote.set(node, new Map());
  viewWrote.get(node).set(name, node.style.getPropertyValue(name));
}
// The view's own stores, fresh for this mount (stage 7): the open session's records and how they are
// drawn, and how the view is laid out and read, with the reader's remembered choices.
const recordState = createRecordState();
if (options.backend) useBackend(options.backend);
// The view builds its three parts inside its element from its own markup (stage 4), so a host's
// page carries none of the view's markup, and draws the glyphs that markup names.
if (element.id && element.id !== "sessionView") throw new Error(`mountSessionView draws into an element whose id is sessionView, or that has none; this one is #${element.id}`);
// A host gives it an empty element (stage 7): one that holds anything but white space (an element,
// text, a comment) is refused, so the view never takes a host's nodes for its own, nor takes them
// away when it is unmounted.
if ([...element.childNodes].some(node => !(node.nodeType === 3 && !node.data.trim()))) throw new Error("mountSessionView draws into an empty element; this one holds the host's nodes");
element.innerHTML = SESSION_VIEW_MARKUP;
element.querySelectorAll("[data-icon]").forEach(glyph => { glyph.innerHTML = svg(glyph.dataset.icon); });
// The view's element carries its id, its class and its theme (stage 5b): its stylesheet hangs every
// rule on #sessionView and reads the theme there, light or dark, as the host says (#s436), and
// `viewRoot.element()` finds it by that id. A host's element with an id of its own cannot be both.
element.id = "sessionView";
element.classList.add("session-view");
// The view lays itself out as it is built (the pane shuts itself, the reading options apply), and none
// of it glides (#s431 stage 1): the element has the view's grid from the moment it takes its id, so
// without this the pane's column, laid out open by a read early in the mount, closed over .22s under
// a view that had just appeared. `sv-mounting` holds every transition inside the view off until the
// mount's last layout read, below: the first layout decision adds its classes after a read that styled
// the parts without them, and the outline's track glided into place while the first prompts measured.
element.classList.add("sv-mounting");
// #s431 stage 2 (design/pane-layout.md): the view is laid out by its own box, not the window. Its
// layout is decided now, before the first render, and again whenever the box or the screen changes
// (pane-layout.js); the classes that decision sets on the element are what its width rules read. A
// decision that changes a class or a width remeasures the transcript once the view can (`layoutHeard`,
// set below, since the first decision comes before the code that remeasures exists).
let layoutHeard = null;
// #s431 stage 3: each decision also arranges the view's parts by the owner's order, from what the view
// says of itself (`arrangeInputs`, which answers the defaults until the view's state exists, just
// below), and draws the outline as arranged (`drawOutline`) in the same decision.
let arrangeInputs = () => ({}), outlineWanted = () => true;
// Set by the phone shell below, which holds the glyphs and their panel: none until it has run.
let placeOutlineGlyphs = () => {}, openOutlinePanel = () => false;
const layout = mountPaneLayout(element, { signal, onChange: () => layoutHeard?.(), inputs: () => arrangeInputs(), onArrange: arrangement => drawOutline(arrangement) });
// How the view is laid out and read, made once the layout has decided (the outline's default is the
// view's phone family's).
const uiState = createViewState({ phone: layout.phone() });
outlineWanted = () => uiState.navigatorOpen;
/** The room the host could give, as `room()` answers it: none without one, and none for a throw, which
 *  is reported as a handler's throw is. `arrangeView` reads any answer that is not a finite number
 *  above 0 as none. */
const askRoom = () => {
  if (typeof options.room !== "function") return 0;
  try { return options.room(); } catch (error) { (globalThis.reportError || console.error)(error); return 0; }
};
arrangeInputs = () => ({ paneOpen: !element.classList.contains("preview-off"), outlineWanted: outlineWanted(), share: uiState.paneShare, room: askRoom() });
/** The reader drags the pane's edge to `at` (a pointer's x), splits the slack equally again
 *  (`"equal"`, a double-click), or lets go (`null`, which remembers the share): the pane's part of the
 *  slack is what its box from the pointer to the view's 10px margin takes beyond M, each side kept at
 *  M by the share's own bounds. Only a pane that is a column has an edge to drag. */
function dragPane(at) {
  if (at === null) { persist(); return; }
  if (at === "equal") uiState.paneShare = 0.5;
  else {
    const arrangement = layout.arrangement();
    if (arrangement?.pane !== "column") return;
    const slack = arrangement.session + arrangement.paneWidth - 2 * M;
    const asked = element.getBoundingClientRect().right - 10 - at - 1;
    uiState.paneShare = slack > 0 ? Math.min(1, Math.max(0, (asked - M) / slack)) : 0.5;
  }
  paneLayoutChanged();
  if (at === "equal") persist();
}
layout.decide();
const persist = () => persistView(uiState);
const applyTheme = theme => { element.dataset.theme = theme === "dark" ? "dark" : "light"; };
applyTheme(options.theme);
const { selectSession } = options;
const grid = options.grid;
// The view's own element: its three parts, and the state it keeps for itself (stage 2).
const sessionView = element;
// What the view asks of the app's session index (state.js's `indexState` is the app's).
const selectedId = () => options.selectedId();
const sessionRow = id => options.sessionRow(id) || null;
const selectedRow = () => sessionRow(selectedId());
const sessionWorkers = (id = recordState.session) => options.sessionWorkers(id);
// What the view tells its host (stage 6), as plain data, through ONE emitter. A handler is the
// host's code: one that throws is reported (`reportError`, as an uncaught error would be) and never
// breaks the view in the middle of what it was doing (a redraw, a transaction's placement, the ✓ on a
// copied link). And what the view says while it mounts waits until `mountSessionView` has returned,
// a microtask later, so a handler may already use the handle; anything said in that task waits too,
// so the order holds. The title and the turn are said only when they change: the top bar is redrawn
// on every index poll and the turn bar on every scroll.
let mountingEvents = [];
function say(handler, data) {
  if (typeof handler !== "function" || unmounted) return;
  if (mountingEvents) { mountingEvents.push([handler, data]); return; }
  try { handler(data); } catch (error) { (globalThis.reportError || console.error)(error); }
}
const toast = message => say(options.onToast, String(message));
// The reader's remembered choice of the outline, which the host hears as the mount returns.
say(options.onOutline, { wanted: !!uiState.navigatorOpen });
let saidTitle = null, saidTurn = null;
function sayTitle(title) {
  const session = selectedId() || "", said = `${session}\u0000${title}`;
  if (said === saidTitle) return;
  saidTitle = said;
  say(options.onTitle, { session, title });
}
// The session whose units the view holds: `open` names the new session before its records arrive,
// and until they do the units are the last session's, so its turn is said as none.
let unitsSession = "";
function sayTurn(unit) {
  const session = recordState.session || "", known = !!unit && unitsSession === session;
  const turn = known ? unit.turn : null, label = known ? unit.label || "" : "";
  const said = `${session}\u0000${turn}\u0000${label}`;
  if (said === saidTurn) return;
  saidTurn = said;
  say(options.onTurn, { session, turn, label });
}
// The turn is said from where the engine has placed the reader: after a transaction and after a
// scroll, never from `afterRender`, which runs before the placement and can name a turn the place
// then moves away from (#209). The last of a task's readings is said, once, at its end.
let turnToSay, turnQueued = false;
function queueTurn(unit) {
  turnToSay = unit;
  if (turnQueued) return;
  turnQueued = true;
  queueMicrotask(() => { turnQueued = false; sayTurn(turnToSay); });
}
const sayInfo = () => say(options.onInfo, { session: recordState.session || selectedId() || "" });
/** A file the artifact pane was asked to show: its name (the file's own, not an Open all tab's label),
 *  path and carried text, copied off the tab, which also holds the file's stamps and an image's bytes. */
const sayFile = item => say(options.onFile, { name: String(item.fileName || item.name || ""), path: String(item.path || ""), text: typeof item.text === "string" ? item.text : null });

// The outline row currently marked (#52) — declared ahead of the init code below, which
// renders the navigator before the module's later statements have run.
let outlineCurrent = null;
// Whether the Turns pane follows the session (#s207) — declared here for the same reason.
let turnsFollow = true;

const byId = viewRoot.byId;
const transcript = byId("transcript");
const projection = new Projection();
let landedHash = "";
// #s277: a link waiting for its turn to arrive, and when it began to wait (`landOnHash`).
let hashWait = null;
const filterIcon = viewRoot.$("#filterTranscriptBtn [data-icon]");
if (filterIcon) filterIcon.innerHTML = svg("filterLines");

// Keep the immutable demo shell byte-identical and layer production-only identity actions on
// top of it. The old viewer made the shortened id copy a different value (the transcript path),
// which was useful but surprising; the menu makes both debug identities explicit.
const sessionTitle = byId("sessionTitle");
const sessionCopyMenu = document.createElement("div");
sessionCopyMenu.className = "session-copy-menu";
sessionCopyMenu.id = "sessionCopyMenu";
sessionCopyMenu.setAttribute("role", "menu");
sessionCopyMenu.setAttribute("aria-label", "Copy session details");
sessionCopyMenu.innerHTML = `<div class="session-copy-caption">Copy session details</div><button type="button" role="menuitem" data-copy-session="id">${svg("copy")}<span><strong>Session ID</strong><small data-session-copy-value="id">—</small></span></button><button type="button" role="menuitem" data-copy-session="path">${svg("copy")}<span><strong>Transcript path</strong><small data-session-copy-value="path">Loading…</small></span></button>`;
sessionTitle.insertAdjacentElement("afterend", sessionCopyMenu);
sessionTitle.tabIndex = 0;
sessionTitle.setAttribute("role", "button");
sessionTitle.setAttribute("aria-haspopup", "menu");
sessionTitle.setAttribute("aria-expanded", "false");
sessionTitle.title = "Copy the session id or the transcript path";
const infoCardLabel = viewRoot.$('[data-nav-card="session"] .outline-card-head strong');
const infoRailButton = viewRoot.$('[data-nav-card-open="session"]');
if (infoCardLabel) infoCardLabel.textContent = "Info";
if (infoRailButton) infoRailButton.title = "Info";

let sessionCopyCloseTimer = 0;
function setSessionCopyMenu(open) {
  clearTimeout(sessionCopyCloseTimer);
  sessionCopyMenu.classList.toggle("open", open);
  sessionTitle.setAttribute("aria-expanded", String(open));
}
function scheduleSessionCopyClose() {
  clearTimeout(sessionCopyCloseTimer);
  sessionCopyCloseTimer = setTimeout(() => {
    if (!sessionCopyMenu.contains(viewRoot.active()) && viewRoot.active() !== sessionTitle) setSessionCopyMenu(false);
  }, 120);
}
// #330: a finger or a pen LEAVES when it lifts, so its leave is no reason to close — on iOS the
// tap brings no mouse events after it (the tap revealed content, so WebKit takes it for a hover),
// nothing gives the title focus, and the lift closed the menu for good. A tap elsewhere, a copy or
// Escape closes it; a mouse still closes it by leaving.
const leftByHover = event => event.pointerType === "mouse";
sessionTitle.addEventListener("pointerenter", () => setSessionCopyMenu(true));
sessionTitle.addEventListener("pointerleave", event => { if (leftByHover(event)) scheduleSessionCopyClose(); });
sessionTitle.addEventListener("focus", () => setSessionCopyMenu(true));
sessionTitle.addEventListener("blur", scheduleSessionCopyClose);
sessionTitle.addEventListener("click", () => setSessionCopyMenu(true));
// #s37: on a phone the turn's subtitle under the title is the same target (see `turnSubtitle`).
viewRoot.$(".topbar .session-heading").addEventListener("click", event => { if (event.target.closest("#turnSubtitle")) setSessionCopyMenu(true); });
sessionTitle.addEventListener("keydown", event => {
  if (["Enter", " ", "ArrowDown"].includes(event.key)) {
    event.preventDefault(); setSessionCopyMenu(true);
    sessionCopyMenu.querySelector("button:not(:disabled)")?.focus();
  }
});
sessionCopyMenu.addEventListener("pointerenter", () => clearTimeout(sessionCopyCloseTimer));
sessionCopyMenu.addEventListener("pointerleave", event => { if (leftByHover(event)) scheduleSessionCopyClose(); });
sessionCopyMenu.addEventListener("focusout", scheduleSessionCopyClose);
document.addEventListener("pointerdown", event => {
  if (event.target !== sessionTitle && !event.target.closest?.("#turnSubtitle") && !sessionCopyMenu.contains(event.target)) setSessionCopyMenu(false);
}, { signal });

async function copyText(text) {
  if (!text) return false;
  if (navigator.clipboard?.writeText) {
    try { await navigator.clipboard.writeText(text); return true; } catch (_) { /* use fallback */ }
  }
  try {
    const field = document.createElement("textarea");
    field.value = text; field.style.cssText = "position:fixed;top:-1000px;opacity:0";
    viewRoot.layer().appendChild(field); field.select();
    const copied = document.execCommand("copy"); field.remove(); return copied;
  } catch (_) { return false; }
}
sessionCopyMenu.addEventListener("click", async event => {
  const button = event.target.closest("[data-copy-session]");
  if (!button || button.disabled) return;
  const row = selectedRow(), meta = recordState.session === row?.id ? (recordState.meta || {}) : {};
  const kind = button.dataset.copySession;
  const value = kind === "id" ? (meta.sid || row?.id || "") : (meta.path || "");
  const copied = await copyText(value);
  if (unmounted) return;
  toast(copied ? (kind === "id" ? "Copied session id" : "Copied transcript path") : "Copy failed — check the browser's clipboard permission");
  if (copied) setSessionCopyMenu(false);
});

const viewport = new Viewport(transcript, byId("transcriptInner"), recordState, {
  afterRender: () => { applyFilters(); markSearch(); paintCodeBars(); updateStickyHeaders(); updateOutlineFocus(); updateTurnBar(); },
  // The window is where it will stay (#209): repaint the bar from the engine's OWN belief of where
  // the reader is. `afterRender` fires before the transaction's placement, so a bar painted there
  // can name a window the place has since moved — measured at three turns behind, until a
  // one-pixel nudge put it right. The key costs no layout read; reading the DOM here does, and
  // that forced flush, made while a smooth write travelled, moved the reader (the smooth-step
  // case's record went 190px). The engine no longer calls this mid-travel, and the one position
  // read off the window is a command's placed below the landing (`keyAtSpyLine`, #s454).
  afterTransaction: () => { const p = viewport.position; if (p && p.key) queueTurn(updateTurnBar(p.key)); wantWindow(); },
  afterScroll: () => {
    updateStickyHeaders(); updateOutlineFocus(); queueTurn(updateTurnBar());
    // The reader moved themselves: wherever a jump last landed them is no longer where they
    // are, so the next step enters afresh at the nearest hit rather than stepping past it. Only
    // input LATER than the landing (#s259): the jump stamps intent itself, so its own scroll came
    // inside the window and cleared the landing it had just made.
    if (viewport.lastUserInput > recordState.landedAt && performance.now() - viewport.lastUserInput < viewport.userIntentMs) recordState.landed = null;
  },
  followChanged: paintJump
});
// Delegate from the stable scroll host: the virtual window may replace all of its children
// while a pull reconciliation is in flight, but attachment/fold interactions must survive it.
// #123: the turn bar. A strip at the top of the transcript naming the turn the reader is
// inside — "Turn N — <label>" — and a click on it returns to that turn's card, the classic
// page's `#stickybar` rule (export.js `spy`) in the app shell's own chrome. It names a turn
// only once that turn's card has scrolled past the top: while its head is on screen the turn
// names itself, and the bar would be saying it twice.
const turnStickyBar = document.createElement("button");
turnStickyBar.type = "button";
turnStickyBar.className = "turn-stickybar";
turnStickyBar.id = "turnStickyBar";
const turnStickyCaret = document.createElement("span");
turnStickyCaret.className = "turn-sticky-caret";
turnStickyCaret.textContent = "❯";
const turnStickyText = document.createElement("span");
turnStickyText.className = "turn-sticky-text";
turnStickyText.id = "turnStickyText";
turnStickyBar.append(turnStickyCaret, turnStickyText);
byId("transcript").prepend(turnStickyBar);
let turnStickyAt = null;
turnStickyBar.onclick = () => { if (turnStickyAt != null) { setTurnsFollow(true); viewport.jumpToRecord(turnStickyAt, "turn"); } };
// #s37 (design/phone-landscape.md §3.4): on a phone the turn is the top bar's SUBTITLE, under the
// session's title, where the strip above was a third row of chrome (production.css hides that strip
// there). It always names the turn the reader is in — the strip waited until its card had scrolled
// past. It is part of the title's target, not one of its own: two targets stacked in a 48px row are
// each half a finger, so a tap on either line opens the session's menu (below), and turns are chosen
// in the Turns pane, one tap away in the pill.
const turnSubtitle = document.createElement("span");
turnSubtitle.className = "turn-subtitle";
turnSubtitle.id = "turnSubtitle";
turnSubtitle.hidden = true;
viewRoot.$(".topbar .session-heading").append(turnSubtitle);
bindComponentEvents(transcript, recordState, {
  // #185: `bindComponentEvents` is a CLICK handler, so everything that reaches `rerender` is the
  // reader reshaping the page themselves — a fold, a cap, a prompt expanded, a raw toggle. Parked
  // at the tail that growth would otherwise be read as the tail moving away and converged on,
  // scrolling away the thing the click asked to see. The pin is dropped instead.
  rerender: () => { viewport.readerReshaped(); viewport.rerender(); viewport.scheduleRemember(); },
  // …and for the branches that grow the page IN PLACE without a re-render (the cap expander).
  reshaped: () => viewport.readerReshaped(),
  // What only the page knows reaches the viewport history (#197): a fold, a prompt toggle.
  note: (kind, target) => viewport.noteAction(kind, target),
  codeSize: (key, delta) => setCodeOverride(key, { size: clampSize(effectiveCode(key).size + delta * SIZE_STEP) }),
  codeWrap: key => setCodeOverride(key, { wrap: !effectiveCode(key).wrap }),
  remember: () => viewport.scheduleRemember(),
  copySpot: async (id, button) => {
    const url = new URL(location.href);
    url.hash = id;
    const copied = await copyText(url.href);
    if (unmounted) return;
    if (!copied) { toast("Copy failed — check the browser's clipboard permission"); return; }
    say(options.onLinkCopied, { url: url.href, record: String(id) });
    clearTimeout(button._spotTimer);
    button.classList.add("copied");
    button.querySelector("span").textContent = "✓";
    button.setAttribute("aria-label", "Link copied");
    button.title = "Link copied";
    button._spotTimer = setTimeout(() => {
      if (!button.isConnected) return;
      button.classList.remove("copied");
      button.querySelector("span").textContent = "#";
      button.setAttribute("aria-label", "Copy a link to here");
      button.title = "Copy a link to here";
    }, 1400);
  },
  openChild: id => descendTo(id),
  openAttachment: (id, path, fsig, action, sig, card) => openAttachment(id, path, fsig, action, sig, card),
  // #s222: a group's images share one name ("image.png"), so their tabs say which of the group each is.
  openAll: ids => { const items = ids.map(id => paneImage(id)).filter(Boolean); preview.openAll(items.map((item, at) => ({ ...item, fileName: item.name, name: `${item.name} ${at + 1}/${items.length}` }))); },
  openFiles: offers => preview.openAll(offers.map(offer => ({ id: `reference:${offer.path}`, name: String(offer.path).split("/").pop() || "file", path: offer.path, fsig: offer.fsig, sig: offer.sig || "" }))),
  openReference: path => openReference(path),
  openReferenceOffer: offer => openReferenceOffer(offer),
  toast
});

// Engagement (stage 6): whether the reader is working in the view. It is mdrev's own rule, which
// `bindKeymap` already reads for a guest (#270): the last press or focus landed inside the view's
// element, or inside what the view floats over the page from `viewRoot.layer()` (`floating`: the task
// card and its scrim), and not inside a guest of the view's (mdrev's pane), whose keys stay its own.
// Both `pointerdown` and `mousedown` are heard, since a host that prevents the first stops the
// browser's second. The view's keymap and the artifact pane's image viewer answer only while it holds
// (`keyboard: "engaged"`); with the page's keyboard (`"page"`, the app's) the view is always engaged,
// as before.
const floating = new Set();
let engaged = keyboardMode === "page";
if (keyboardMode === "engaged") {
  const inView = target => !!target && typeof target.closest === "function" && (element.contains(target) || [...floating].some(node => node.contains(target)));
  const engage = event => { engaged = inView(event.target) && !inGuest(event.target); };
  for (const type of ["pointerdown", "mousedown", "focusin"]) document.addEventListener(type, engage, { capture: true, signal });
}

// #s25: the pane changes the TRANSCRIPT's layout only where it is a grid column (a wide desktop).
// Elsewhere — a phone, or below 1180px — it lies over the transcript, whose width and so every
// height are unchanged, and a remeasure only threw every measured height away: on a phone it was the
// step through which a pane a tap had opened moved the reader back several turns.
// Not while the view is mounting (stage 6): the pane shuts itself as it is built, which in a host's
// page whose element did not start `preview-off` (the app's markup does) changes the width, and a
// remeasure then reached the record store before it existed. The reading options' first application,
// later in the mount, measures everything, and the mount's end takes the width it left as the one
// the next change is measured from.
// It remeasures once, from the width it was last laid out at to the width it settles at, and only when
// that width changed (#s431 stage 3): a remeasure at the same width throws every measured height away,
// and one read while a track glides (the pane's, the outline's, or a host's list around the view)
// scales every height to a column the reader never sees settled, from which the next change scales
// again. So it is the ONE way the view remeasures for its width: the pane, the outline, the window's
// resize and a host's `resized()` all come here. It takes the view's own decision first, since the
// pane's width is the layout's (`--preview-fit`) and a host's change to the view's box reaches the
// layout only at its observer's next delivery; and it waits out any glide that moves the transcript's
// edges: a transition of a width or a grid's tracks on a box that holds the transcript, or on one beside
// such a box (a host's list beside the view).
// The width is the engine's own: its mount's, which its `remeasure` scales by, and which the reading
// measure caps (820px, a wide transcript's 1240), so a column that grows past the cap changes nothing.
const laidOutWidth = () => viewport.mount.getBoundingClientRect().width;
let paneLaidOutWidth = laidOutWidth(), viewMounted = false, settling = null;
const WIDTH_PROPERTIES = new Set(["grid-template-columns", "column-gap", "width", "min-width", "max-width", "flex-basis", "flex-grow", "flex-shrink", "inset-inline-start", "inset-inline-end",
  "padding-left", "padding-right", "padding-inline-start", "padding-inline-end", "margin-left", "margin-right",
  "margin-inline-start", "margin-inline-end", "border-left-width", "border-right-width", "left", "right"]);
const movesTheTranscript = target => !!target?.contains && (target.contains(transcript) || !!target.parentElement?.contains(transcript));
const tracksGliding = () => viewRoot.animations().filter(glide => typeof CSSTransition !== "undefined" && glide instanceof CSSTransition
  && WIDTH_PROPERTIES.has(glide.transitionProperty) && movesTheTranscript(glide.effect?.target));
const paneLayoutChanged = ({ decided = false } = {}) => {
  if (!viewMounted || unmounted) return;
  if (!decided) layout.decide();
  const glides = tracksGliding();
  if (glides.length) {
    settling ??= Promise.allSettled(glides.map(glide => glide.finished)).then(() => { settling = null; paneLayoutChanged(); });
    return;
  }
  const width = laidOutWidth();
  if (Math.abs(width - paneLaidOutWidth) < 0.5) return;
  paneLaidOutWidth = width;
  viewport.remeasure();
};
layoutHeard = () => paneLayoutChanged({ decided: true });
// The reader opened or shut the pane: the order arranges the view again, and the pane glides whole
// (`toggled`), even where the host's list gives way in the same moment; the pane shutting itself as
// the view is built glides nothing, but the view is arranged again for it, since what the mount draws
// reads the arrangement. Then `layoutChanged` remeasures once that glide is done.
const preview = new Preview({ phone: () => layout.phone(), layoutChanged: () => paneLayoutChanged(), paneToggled: change => (viewMounted ? layout.toggled(change) : (change(), layout.decide())), dragPane, paneLayout: (name, on) => options.paneLayout?.(name, on), opened: sayFile, detach: shows("detach"), frame: options.frame, rootProperty: (name, value) => writeStyle(viewRoot.styleRoot(), name, value), engaged: () => engaged, keysFirst: keyboardMode === "engaged", toast, reveal: item => attachmentViewer.reveal(item), download: item => attachmentViewer.download(item), jumpToRecord: at => jumpOrFetch(at, "artifact") }, uiState);
const attachmentViewer = new AttachmentViewer({ toast });
// What the compose box asks of the app goes nowhere once the view is unmounted: a dialog's answer
// still settling, a send still out.
const whileMounted = run => (...args) => (unmounted ? undefined : run(...args));
const controlActions = { write: options.write, selectedId, selectedRow, renderTree: whileMounted(() => options.renderList()), recordSnapshot: () => ({ session: recordState.session, records: recordState.records }), toast, refreshIndex: whileMounted(() => options.refreshIndex()), pauseIndex: whileMounted(() => options.pauseIndex()), resumeIndex: whileMounted(() => options.resumeIndex()), selectSession: whileMounted(selectSession), refreshEmpty: () => recordStore.refreshEmpty() };
const ordinaryControls = new ControlStore(controlActions);
let controls = ordinaryControls;
// #s277 (design/slow-links.md §6.7, D3): a reader across a network — the test gzip and masking use —
// reads a tail-first open's head turn by turn, in the order the view needs it; the desktop reads it
// whole (stage 1). `?windowed=1|0` picks either path for a case.
// The Mermaid runtime the page head names loads through the backend handle, as every other
// request the page makes does (#s423).
useMermaidRuntime(path => backend().url(path));
// …and its diagrams are drawn in the view's theme, which the view's element carries (stage 5b).
useMermaidTheme(() => sessionView);
const WINDOWED = (() => { const q = new URLSearchParams(location.search).get("windowed"); return q === "1" ? true : q === "0" ? false : !backend().local; })();
const recordStore = new RecordStore({
  reset: () => { lastRecordCount = -1; heldRecords = null; heldTurnJump = null; fillWaiting = false; recordState.headIndex = null; viewport.cancelStill(); recordState.headProgress = null; projection.units = []; projection.stub = null; projection.upto = Infinity; hashWait = null; remoteSearch = null; remoteFilter = null; stepWhenFound = null; recordState.records = []; if (controls.nativeActive) { recordState.units = []; turnStickyAt = null; turnStickyBar.classList.remove("on"); turnSubtitle.hidden = true; turnSubtitle.textContent = ""; } recordState.meta = null; recordState.heights.clear(); recordState.folds.clear(); recordState.processFolds.clear(); recordState.processBulk.clear(); recordState.processExpanded.clear(); recordState.processChosen.clear(); recordState.promptExpanded.clear(); recordState.taskTargets.clear(); recordState.agentTargets.clear(); recordState.rawTurns.clear(); recordState.codeOverrides.clear(); recordState.capOpen.clear(); recordState.recSizes = []; recordState.pendingSearch = false; recordState.filterHits = null; recordState.filterDirect = null; recordState.filterSnapshot = null; recordState.search = ""; byId("transcriptSearchInput").value = ""; uiState.chips = { scope: "", tools: [] }; renderChips(); viewport.showEmpty("Loading session…", "Reading the normalized record stream."); renderHeader(); renderNavigator(); },
  update: updateRecords,
  // #s276: the map of the records the head still owes — the panes it fills are whole from now on.
  indexed: index => {
    recordState.headIndex = index;
    // #s277 (design/slow-links.md §6.2): the units the head's records will make, from what the index
    // says of each record, take the short card's place — a prepend, held for a still view (#340)
    // like the head's landing; from then on every fill is a rewrite in place.
    if (recordStore.windowed) { projection.stub = stubFor(index); projection.upto = index.upto; updateRecords({ records: recordStore.records, meta: recordStore.meta, changedFrom: 0, prepended: index.upto }); }
    renderNavigator(); renderArtifacts(); renderChips(); if (byId("navigatorOptions").classList.contains("open")) renderFilterMenu();
  },
  // #s277: a piece of the head was read; it goes in when the view lets it (`takeFills`).
  filled: () => takeFills(),
  // #s277: the last piece is in — every record is here, and the index answers for none of them.
  headRead: () => { recordState.headIndex = null; renderNavigator(); renderArtifacts(); renderChips(); },
  empty: () => viewport.showEmpty("Start this session", "Send the first message below. Codex runs in the selected directory."),
  materializing: () => viewport.showEmpty("Loading session history…", "Waiting for the latest messages to become available."),
  // #221: a first open with no cache waits on the server folding the whole transcript. Say so,
  // rather than leaving a blank page a reader cannot tell from a hang, and say that it is a
  // one-off — the same session opens in a couple of seconds afterwards.
  waiting: () => viewport.showEmpty(
    "Reading this session for the first time",
    "The whole transcript is being folded and cached. A long one takes a while; every open after this is immediate.",
  ),
  error: (error, hasRecords) => {
    sessionLoading?.hide();
    const unavailable = controls.sessionReadFailure?.(recordState.session);
    if (unavailable) viewport.showEmpty("Cannot resume this session", `${unavailable.message}. Choose a saved session or create a new session.`, true);
    else if (hasRecords) toast(`${error.message}；retrying`);
    else viewport.showEmpty("Cannot read this session", `${error.message}；The monitor will retry.`, true);
  },
  // #313: how much of a first open has arrived, and that it has — the veil over the session
  // area says the first and goes on the second.
  progress: (got, total) => sessionLoading?.progress(got, total),
  opened: () => sessionLoading?.hide(),
  // #314: how much of a tail-first open's head has arrived — the pending card says it in place,
  // without a render (its height is its own and does not change).
  // #s274: a head piece that failed is read again; the card says so until the next bytes arrive.
  headFailed: () => {
    recordState.headProgress = { ...(recordState.headProgress || { got: 0, total: 0 }), retrying: true };
    for (const text of viewport.window.querySelectorAll("[data-pending-text]")) text.textContent = pendingHeadText(recordState.headProgress);
  },
  headProgress: (got, total) => {
    recordState.headProgress = { got, total };
    for (const text of viewport.window.querySelectorAll("[data-pending-text]")) text.textContent = pendingHeadText(recordState.headProgress);
  },
// #314 (design/tail-first-open.md): a first open asks for the last 512 KB of the committed log and
// draws it, then reads the head behind it; the server sends a log under twice that whole. A case
// sets `?tailBudget=` so a small fixture exercises it, as `?trace=` and `?historyMs=` serve cases;
// `?keepBytes=` does the same for what a reader across a network keeps of the head (#s285).
}, { tailBudget: Number(new URLSearchParams(location.search).get("tailBudget")) || 512 * 1024, headPieceBytes: Number(new URLSearchParams(location.search).get("headPiece")) || 0, keepBytes: Number(new URLSearchParams(location.search).get("keepBytes")) || 0, windowed: WINDOWED, emptySession: null, sessionFailure: null });

const workerRunning = row => displayState(row).state !== "idle";

// The parent control (parity #3): live only while the open session has an ancestor in its
// meta — a sub-agent child, which is never a list row. Back IS the parent: clicking it selects
// the last ancestor, and a deep-linked child gets a synthesized history entry for it, so the
// browser's own Back does the same (the classic view's `synthesizeBack`).
const parentBtn = byId("sessionParent");
parentBtn.onclick = () => { if (parentBtn.dataset.parent) selectSession(parentBtn.dataset.parent, true); };
// The way back (#82): the control was always hidden — the reference's `compat-hidden` rule
// (display:none !important) beat the production rule that showed it — so it is un-hidden here
// and says what it does. The parent comes from the child's meta (its ancestors), or, when the
// shell itself switched from a parent to this child, from what it remembers of that switch.
parentBtn.classList.remove("compat-hidden");
parentBtn.insertAdjacentHTML("beforeend", '<span class="session-parent-label">Parent session</span>');
const parentHints = new Map();
/** Go DOWN into a child session, from wherever we are (#143). Every descent goes through here so
 *  the way back is recorded once: `renderParent` prefers the child transcript's own ancestry and
 *  falls back to this hint, which is the only thing a child whose transcript records no ancestors
 *  has. The hint used to be set at ONE of the two descents — the Agents pane — so a reader who
 *  went down from a workflow's fleet roster or an "Open child transcript" button arrived with no
 *  way back to the parent. Selecting a session from the SIDEBAR is not a descent and must not
 *  record a hint: the session you were reading is not that one's parent. */
function descendTo(childId) { if (!childId) return; parentHints.set(childId, selectedId()); selectSession(childId, true); }
function renderParent(meta) {
  const known = recordState.session === selectedId() ? meta?.ancestors?.at(-1) : null;
  // A headless worker (#s53) names its starter even when reached by a link, not a descent.
  const hint = parentHints.get(selectedId()) || sessionRow(selectedId())?.startedBy;
  const parent = known || (hint ? { id: hint, title: sessionRow(hint)?.name || hint } : null);
  parentBtn.classList.toggle("is-live", !!parent);
  parentBtn.dataset.parent = parent?.id || "";
  parentBtn.title = parent ? `Back to the parent session: ${parent.title || parent.id}  ( ${hintFor("parent")} )` : "Back to the parent session";
  // A deep-linked child gets a history entry for its parent, so the browser's own Back goes there
  // too: the app's to write, since the session's address is its (`parentShown` in app.js).
  if (parent) options.parentShown?.(parent.id);
  return parent;
}
/** The top bar for the selected session, and its title to the host (`onTitle`, when it changed). */
function renderHeader() {
  paintHeader();
  sayTitle(sessionTitle.textContent);
}
function paintHeader() {
  renderArtifacts();
  const row = selectedRow();
  const liveMeta = recordState.session === selectedId() ? recordState.meta : null;
  const parent = renderParent(liveMeta);
  if (!row && liveMeta && parent) {
    // A sub-agent child: not a row, so everything the header shows comes from its own meta.
    byId("sessionTitle").textContent = liveMeta.title || liveMeta.sid || selectedId();
    byId("sessionCrumb").textContent = `${agentName(liveMeta.agent)} · sub-agent of ${parent.title || parent.id}`;
    byId("statusChip").className = "status-chip idle";
    byId("statusChip").textContent = liveMeta.agent_type ? `Sub-agent · ${liveMeta.agent_type}` : "Sub-agent";
    byId("statusChip").title = "";
    sessionTitle.tabIndex = 0;
    sessionCopyMenu.hidden = false;
    sessionCopyMenu.querySelector('[data-session-copy-value="id"]').textContent = liveMeta.sid || selectedId();
    sessionCopyMenu.querySelector('[data-session-copy-value="path"]').textContent = liveMeta.path || "available once the session loads";
    return;
  }
  if (!row) {
    byId("sessionTitle").textContent = "Agent Monitor";
    byId("sessionCrumb").textContent = "Scanning sessions";
    byId("statusChip").className = "status-chip idle";
    byId("statusChip").textContent = "None selected";
    sessionTitle.tabIndex = -1;
    sessionCopyMenu.hidden = true;
    setSessionCopyMenu(false);
    return;
  }
  const state = displayState(row);
  // A worker (#s53) is titled by its brief and says whose worker it is: its own title is its project's.
  const starter = row.startedBy ? sessionRow(row.startedBy) : null;
  byId("sessionTitle").textContent = row.startedBy ? workerName(row) : row.name || row.id;
  byId("sessionCrumb").textContent = starter
    ? `${agentName(row.agent)} · worker of ${starter.name || starter.id}`
    : `${agentName(row.agent)} · ${row._group?.label || ""}`;
  byId("statusChip").className = `status-chip ${state.state} ${state.reason}`;
  byId("statusChip").innerHTML = `<span class="state-dot ${escapeText(state.state)} ${escapeText(state.reason)}" style="margin:0"></span>${escapeText(state.label)}${row.stateConfidence === "inferred" ? " · inferred" : ""}`;
  byId("statusChip").title = row.stateDetail || "";
  const meta = recordState.session === row.id ? (recordState.meta || {}) : {};
  const sid = meta.sid || row.id || "";
  const path = meta.path || "";
  sessionTitle.tabIndex = 0;
  sessionCopyMenu.hidden = false;
  sessionCopyMenu.querySelector('[data-session-copy-value="id"]').textContent = sid;
  sessionCopyMenu.querySelector('[data-session-copy-value="path"]').textContent = path || "available once the session loads";
  sessionCopyMenu.querySelector('[data-copy-session="id"]').disabled = !sid;
  sessionCopyMenu.querySelector('[data-copy-session="path"]').disabled = !path;
}

let lastRecordCount = -1; // -1: nothing applied since the last reset — the next apply is the open, not growth
// #340: a tail-first open's head lands only on a STILL view. Landing moves the reader's offset by
// the whole head (100,000px on the owner's 141-turn session), and on iOS a write into a fling that
// is still gliding does not stick: the glide carries on from its old offset, which in the new page
// is a turn near the start. So while the view moves, the snapshot waits here — every later one too,
// since each carries the head — and the latest is applied once `whenStill` says so. A touch glide
// (#372, from the finger landing until the view is still) holds it too, scroll events or none: the
// scroll clock hears a glide only as fast as its events arrive, and a headless tab under load
// delivers them lazily — the case caught the head landing into a glide that way once #372 stopped
// the engine's own writes, whose echoes had been keeping that clock fresh.
let heldRecords = null;
// #s276: a turn the reader chose in the Turns pane while its records were still on their way: it is
// landed on once they arrive, unless the reader has moved since (`at` against the engine's input).
let heldTurnJump = null;
// #s278: the server's answers while a records-by-turn head is out (`remoteFound`) — the text search's
// and the filter's, apart, so neither throws the other's away — and a step asked for before the
// answer it waits on landed (`{delta, at, slot}`: that answer's own slot).
let remoteSearch = null;
let remoteFilter = null;
let stepWhenFound = null;
function updateRecords(update) {
  if (!heldRecords && !(update.prepended && (viewport.readerMoving() || viewport.touchGlide))) { applyRecords(update); return; }
  const first = !heldRecords;
  heldRecords = first ? update : { records: update.records, meta: update.meta, changedFrom: Math.min(heldRecords.changedFrom, update.changedFrom), prepended: heldRecords.prepended || update.prepended };
  if (first) viewport.whenStill(() => { const held = heldRecords; heldRecords = null; if (held) applyRecords(held); });
}
function applyRecords({ records, meta, changedFrom, prepended }) {
  const before = lastRecordCount; const wasFollowing = recordState.following;
  const metaArrived = meta && meta !== recordState.meta;
  recordState.records = records; recordState.meta = meta;
  controls.observeRecords?.(recordState.session, records);
  // The haystack's size per record (#104), kept in step with the store's truncate/append.
  const sizes = recordState.recSizes;
  sizes.length = Math.min(sizes.length, changedFrom, records.length);
  for (let i = sizes.length; i < records.length; i++) sizes[i] = recordTextSize(records[i]);
  if (metaArrived) { renderHeader(); refreshFleets(); }
  recordState.taskTargets = taskRecordTargets(meta?.tasks || [], records);
  recordState.agentTargets = agentTargetsNow(records, meta);
  // #314: a tail-first open's head landed in front of the tail. The first unit after the pending
  // card keeps its key; how far it moved is how many units now stand where the card stood.
  // Found by its first record, not its key (#s277): when the head's records join the tail's first
  // group of steps, that group is keyed by its first record, in the head — the same content, renamed,
  // which the engine is told so the reader's anchor resolves.
  const after = prepended ? projection.units.find(unit => unit.type !== "pending") : null;
  const was = after ? projection.units.indexOf(after) : -1;
  const changedUnit = projection.rebuild(records, changedFrom);
  recordState.units = projection.units;
  unitsSession = recordState.session || "";
  const now = after ? unitAt(after.from) : -1;
  const shift = now >= 0 ? Math.max(0, now - was) : 0;
  const renamed = now >= 0 && recordState.units[now].key !== after.key ? { [after.key]: recordState.units[now].key } : null;
  viewport.setUnits(recordState.units, changedUnit, shift, renamed);
  // The head landed whole (stage 1), or a pull replaced it: the index answers for nothing now. A
  // stage-2 prepend (the placeholders laid out) leaves the head out and the index with it.
  if (prepended && !recordStore.head) recordState.headIndex = null;
  landOnHash();
  // A held turn goes when the records under it are rewritten (a resync) or the reader moved;
  // otherwise it is landed on as soon as `jumpToRecord` takes it, which it refuses while pending.
  if (heldTurnJump && ((!prepended && changedFrom <= heldTurnJump.record) || viewport.lastUserInput > heldTurnJump.at)) heldTurnJump = null;
  landHeldJump();
  // What arrived since the last apply is "new" only once the session is OPEN (#84): the first
  // apply after a reset is the session's own content — a re-open restored to a remembered
  // position is not following, and counting that batch said "11696 new messages" on a session
  // that had none. The classic page counts only what arrives after the open; so does this.
  const delta = Math.max(0, records.length - before);
  const opening = lastRecordCount < 0;
  lastRecordCount = records.length;
  // #250: a block that GREW while the reader had the page open is one they are watching, and
  // it renders LIST-ALL — every top-level message, no "Show N more". Not a control and not a
  // preference: the reader's intention to watch is expressed by the turn being live in front
  // of them.
  //
  // `opening` is what makes this bounded. The first apply after a reset is the session's own
  // history, not growth, so a session opened cold stamps nothing and every block renders
  // concise — the accumulation is how long the reader watched, never the transcript's length.
  //
  // The stamp is STICKY on purpose. `processExpanded` is never cleared at a turn boundary, so
  // a block keeps list-all once the turn ends: no demotion, no timer, and no "it collapsed
  // while I was reading it".
  if (!opening && delta > 0) {
    for (const unit of recordState.units) {
      // `to >= before` means at least one of the records that just arrived is in this block.
      if (unit.type !== "process" || unit.to < before) continue;
      // The reader's own word on a block is final — otherwise collapsing the live turn would
      // be undone by the very next record to arrive, and it could never be made to stay shut.
      if (recordState.processChosen.has(unit.key)) continue;
      recordState.processExpanded.add(unit.key);
    }
  }
  if (!wasFollowing && delta && !opening) recordState.newRecords += delta;
  paintJump(); renderHeader(); renderNavigator(); refreshFilterHits(); updateSearch(false);
  // #s277: pieces of the head that waited on a held prepend go in now, if the view lets them.
  takeFills();
}

/** #s277 (design/slow-links.md §6.2): what a record not fetched yet stands as — its kind, id and
 *  commentary bit from the index's facts, and for a turn's opener its number, label, sender and
 *  quoted reply from the index's rows — so `buildUnits` makes the units the records will make. */
function stubFor(index) {
  const rows = new Map((index.turns || []).map(row => [row.r, row]));
  return i => {
    const fact = index.facts?.[i];
    if (!fact) return null;
    const row = rows.get(i);
    return { kind: fact[0], id: fact[1] || undefined, ...(fact[2] ? { phase: "commentary" } : {}), ...(row ? { turn: row.turn, label: row.label, head: row.head, reply_context: row.reply_context } : {}) };
  };
}

/** The sub-agents' records: those here, and (#s277) the index's for the ones still on their way, so
 *  an agent's jump reads its turn and lands. */
function agentTargetsNow(records, meta = recordState.meta) {
  const targets = agentRecordTargets(directAgents(meta), records);
  for (const agent of owedIndex()?.agents || []) if (!targets.has(agent.id) && records[agent.r]?.kind === "pending") targets.set(agent.id, agent.r);
  return targets;
}

/** A jump held for its records (a turn, a record, a search hit) lands once they are here; a search
 *  hit then lands on its own mark, as a step does (#s278). */
function landHeldJump() {
  if (!heldTurnJump || !viewport.jumpToRecord(heldTurnJump.record, heldTurnJump.reveal || "turn")) return;
  const { record, reveal } = heldTurnJump;
  heldTurnJump = null;
  if (reveal === "search" || reveal === "filter") {
    recordState.landed = record; recordState.landedAt = performance.now();
    const at = activeMatches().indexOf(record);
    if (at >= 0) recordState.match = at;
  }
  if (reveal === "search") { markSearch(); landOnCurrentMark(); }
}

/** The unit that holds record `r`, by its index, or -1. */
function unitAt(r) {
  const units = recordState.units;
  let lo = 0, hi = units.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (units[mid].to < r) lo = mid + 1;
    else if (units[mid].from > r) hi = mid - 1;
    else return mid;
  }
  return -1;
}

// #s277 (design/slow-links.md §6.3): pieces of the head the store has read wait there until the view
// lets them in. A fill that would redraw a mounted unit at or above the reader waits for a still view
// and no thumb held (D5: a write into an iOS glide does not stick, and absorbing it would pile up in a
// top pad that runs out near the session's top); one below the reader, or outside the window, goes in
// at once — it moves nothing. A held prepend goes first: the fills are written against its units.
let fillWaiting = false;
function takeFills() {
  if (fillWaiting) return;
  for (let next = recordStore.peekFill(); next; next = recordStore.peekFill()) {
    if (heldRecords || fillMustWait(next)) {
      fillWaiting = true;
      viewport.whenStill(() => { fillWaiting = false; takeFills(); });
      return;
    }
    applyFill(recordStore.takeFill());
  }
  dropFar();
}
function fillMustWait({ from, to }) {
  const a = unitAt(from), b = unitAt(to - 1);
  if (a < 0 || b < 0) return true;
  if (b < viewport.lo || a >= viewport.hi) return false;
  // Any fill that would redraw a mounted unit waits while the view moves: the engine's position can
  // be a scroll batch old there, so "below the reader" is not known well enough to let one in.
  return viewport.dragging || viewport.touchGlide || viewport.readerMoving();
}
function applyFill(fill) {
  // A held jump the reader has moved away from goes first, or the fill's own transaction would ask
  // for its turn again (#s285: once the background read is over nothing else reads it).
  if (heldTurnJump && viewport.lastUserInput > heldTurnJump.at) heldTurnJump = null;
  // Units that did not come out as their placeholders were: the index and the log disagree, and
  // nothing may be renamed (framework I12) — the session starts over.
  if (!rewriteInPlace(fill)) { recordStore.recover(); return; }
  recordState.taskTargets = taskRecordTargets(recordState.meta?.tasks || [], recordStore.records);
  recordState.agentTargets = agentTargetsNow(recordStore.records);
  landOnHash();
  landHeldJump();
  renderNavigator(); refreshFilterHits(); updateSearch(false);
}
/** Records `[from, to)` were rewritten where they stand — filled, or let go (#s285): the units over
 *  them are rebuilt under their own keys, and the engine redraws that run alone. False when the units
 *  did not come out as they were. */
function rewriteInPlace({ from, to }) {
  const records = recordStore.records;
  for (let i = from; i < to; i++) recordState.recSizes[i] = recordTextSize(records[i]);
  const run = projection.fill(records, from, to);
  if (!run) return false;
  recordState.units = projection.units;
  viewport.setUnits(recordState.units, run);
  return true;
}
/** The unit the reader is on, for what to read first and what to let go of last. */
function readerUnit() {
  const p = viewport.position;
  return !p ? viewport.lo : p.source === "tail" ? recordState.units.length - 1 : p.index ?? viewport.lo;
}
/** #s277: ask the store for what the window shows — a screen's worth of units beyond it either way,
 *  the nearest to the reader asked last, which the store reads first — and for the jump the page
 *  still waits on. Every earlier ask lapses first (#s285): a turn the reader left, or a jump they gave
 *  up, is not read for them once the background read is over. */
function wantWindow() {
  if (!recordStore.head?.reads) return;
  const units = recordState.units;
  const at = readerUnit();
  const span = Math.max(8, viewport.hi - viewport.lo);
  const lo = Math.max(0, viewport.lo - span), hi = Math.min(units.length, viewport.hi + span);
  const owed = [];
  for (let i = lo; i < hi; i++) if (units[i].owed > 0) owed.push(i);
  owed.sort((x, y) => Math.abs(y - at) - Math.abs(x - at));
  recordStore.unwant();
  for (const i of owed) recordStore.want(units[i].from, units[i].to + 1, 1);
  // A held turn, record or search hit, and a link on its way, keep their asks while they wait.
  for (const r of [heldTurnJump?.record, hashWait?.record]) {
    const unit = r == null ? null : units[unitAt(r)];
    if (unit?.owed > 0) recordStore.want(unit.from, unit.to + 1, 0);
  }
}
/** Units `[lo, hi)` the page never lets go of (#s285): the window and the reader's unit, and twice the
 *  reach `wantWindow` asks for either way, so nothing it asks for is let go. */
function keptAround() {
  const at = readerUnit(), span = 2 * Math.max(8, viewport.hi - viewport.lo);
  return [Math.min(at, viewport.lo) - span, Math.max(at, viewport.hi) + span, at];
}
/** #s285 (design/slow-links.md §6.6; Hong on #s290: "Read up to about 16 MB, then only what I
 *  visit"): once the turns read hold more than the page keeps, the farthest from the reader are let
 *  go. Their units become placeholders again under the same keys, at the heights they were measured
 *  at, so the sums do not move and nothing mounted is redrawn; a visit reads them again. Never a turn
 *  in or near the window (`keptAround`), nor the head's last turn when the tail goes on with it. */
function dropFar() {
  const reads = recordStore.head?.reads;
  if (!reads || recordStore.held <= recordStore.keepBytes) return;
  const units = recordState.units;
  const last = reads.at(-1);
  const goesOn = (units[unitAt(last.last)]?.to ?? -1) > last.last;
  const [lo, hi, at] = keptAround();
  const far = [];
  for (const read of goesOn ? reads.slice(0, -1) : reads) {
    if (read.next === read.r) continue;
    const a = unitAt(read.r), b = unitAt(read.next - 1);
    if (a < 0 || b < 0 || (b >= lo && a < hi)) continue;
    far.push({ read, a, b, distance: b < at ? at - b : a - at });
  }
  far.sort((x, y) => y.distance - x.distance);
  let dropped = false;
  for (const { read, a, b } of far) {
    if (recordStore.held <= recordStore.keepBytes) break;
    // Each drop is a transaction of its own: what the window holds is asked again before the next.
    const [nowLo, nowHi] = keptAround();
    if (b >= nowLo && a < nowHi) continue;
    const run = recordStore.drop(read);
    if (!run) continue;
    if (!rewriteInPlace(run)) { recordStore.recover(); return; }
    dropped = true;
  }
  if (!dropped) return;
  recordState.taskTargets = taskRecordTargets(recordState.meta?.tasks || [], recordStore.records);
  recordState.agentTargets = agentTargetsNow(recordStore.records);
  renderNavigator(); refreshFilterHits(); updateSearch(false);
}
/** A jump to record `r`, or, while its turn is on its way, the turn fetched first (#s277): a TURN
 *  lands at once on its placeholder; a record inside one waits for its unit, unless the reader
 *  moves first. */
function jumpOrFetch(r, reveal) {
  heldTurnJump = null;
  const landed = viewport.jumpToRecord(r, reveal);
  // Whether it must wait is the unit's to say — owed records, read or not yet taken — never the
  // store's alone, which knows nothing of a piece waiting to go in.
  const unit = recordState.units[unitAt(r)];
  if (unit?.owed > 0) recordStore.want(unit.from, unit.to + 1, 0);
  if (!landed && (unit?.owed > 0 || recordState.records[r]?.kind === "pending")) { heldTurnJump = { record: r, reveal, at: performance.now() }; toast("Still loading — the page goes there when it arrives"); }
  return landed;
}

/** The kinds of record a task's status is read from on the server: a task, or a tool call (Claude
 *  Code's TaskCreate and TaskUpdate, a todo or plan update). */
const TASK_RECORD_KINDS = new Set(["task", "tool"]);
/** The ask `findTaskRecord` has out: a later click, or the reader moving on, sets it aside. */
let taskFind = null;
/** #s285: where a task's status was last recorded while a records-by-turn head is out and the page
 *  does not hold that record. Asked of the server's search, which answers for every record of the
 *  head whether read or not, by what `taskRecordTargets` matches: the task's subject (which only its
 *  creation carries) and `#<id>` as a whole word (which every TaskUpdate does, `{taskId, status}`).
 *  The latest of those, or of a held record below the boundary, is jumped to as any target is. */
async function findTaskRecord(task, key) {
  const head = recordStore.head, owed = owedIndex(), session = recordState.session;
  const held = recordState.taskTargets.get(key) ?? -1;
  const subject = String(task.subject || task.title || "").trim().toLowerCase();
  const id = String(task.id ?? "").trim().toLowerCase();
  const asks = [];
  if (subject) asks.push({ lc: subject, whole: false, wanted: 0, tools: [] });
  if (id) asks.push({ lc: `#${id}`, whole: true, wanted: 0, tools: [] });
  if (!head || !owed || !asks.length) return;
  const mine = taskFind = { at: performance.now() };
  let replies = null;
  try {
    replies = await Promise.all(asks.map(q => backend().fetch(`/search?session=${encodeURIComponent(session)}&epoch=${head.epoch}&upto=${owed.upto}&q=${encodeURIComponent(JSON.stringify(q))}`, { cache: "no-store" })
      .then(response => (response.ok ? response.json() : Promise.reject(response.status)))));
  } catch (_) {}
  if (unmounted || taskFind !== mine || recordState.session !== session) return;
  taskFind = null;
  // The reader moved on while it was asked: they are not taken back.
  if (viewport.lastUserInput > mine.at) return;
  // The head landed meanwhile: the page's own target is whole now.
  if (!recordStore.head) { const target = recordState.taskTargets.get(key); if (target != null) jumpOrFetch(target, "task"); return; }
  if (recordStore.head !== head) return;
  if (!replies) { toast("Could not ask the monitor where the task was recorded — try again"); return; }
  let best = held;
  for (const found of replies) for (const [r] of found.records || []) if (r > best && TASK_RECORD_KINDS.has(owed.facts?.[r]?.[0])) best = r;
  if (best < 0) { toast("This record stream did not keep where the task's status was set"); return; }
  jumpOrFetch(best, "task");
}

/** #s277 (design/slow-links.md §6.5): the record a link names while its turn is on its way, from the
 *  index's facts — a turn's opener by its own id, a block by the nearest record id at or below it
 *  (ids number blocks in document order, nested ones after their record's) — or -1. */
function owedRecordFor(id) {
  const facts = recordStore.windowed ? recordState.headIndex?.facts : null;
  if (!facts) return -1;
  const block = /^b(\d+)$/.exec(id);
  let best = -1, bestN = -1;
  for (let r = 0; r < facts.length; r++) {
    const own = facts[r][1];
    if (own === id) return r;
    const n = block && /^b(\d+)$/.exec(own || "");
    if (n && Number(n[1]) <= Number(block[1]) && Number(n[1]) > bestN) { best = r; bestN = Number(n[1]); }
  }
  return best;
}

function landOnHash() {
  let id = location.hash.slice(1);
  try { id = decodeURIComponent(id); } catch (_) { return false; }
  if (!id) { landedHash = ""; return false; }
  const identity = `${recordState.session}:${id}`;
  if (identity === landedHash) return true;
  // Any record lands (#116, the classic page's `#b7`): a tool call nested in an activity too —
  // its chain opens first, the jump lands the record that holds it, then the row itself is
  // brought to the top and flashed.
  const chain = [];
  const index = recordState.records.findIndex(record => recordChain(record, id, chain));
  // #s277: a link into a turn still on its way reads that turn first, and lands once it is here —
  // unless the reader has moved since it was opened: then it stops waiting, as a held tap does.
  if (index < 0) {
    const owed = owedRecordFor(id);
    if (owed < 0) return false;
    if (hashWait?.identity !== identity) hashWait = { identity, at: performance.now(), record: owed };
    else if (viewport.lastUserInput > hashWait.at) { landedHash = identity; hashWait = null; return false; }
    recordStore.want(owed, owed + 1, 0);
    return false;
  }
  for (const rid of chain) recordState.folds.set(rid, false);
  if (!viewport.jumpToRecord(index, "hash")) {
    // Here, but its unit still owes records (#s277): the same wait, which a reader who moves ends.
    if (hashWait?.identity !== identity) hashWait = { identity, at: performance.now(), record: index };
    else if (viewport.lastUserInput > hashWait.at) { landedHash = identity; hashWait = null; return false; }
    const unit = recordState.units[unitAt(index)];
    if (unit?.owed > 0) recordStore.want(unit.from, unit.to + 1, 0);
    return false;
  }
  hashWait = null;
  landedHash = identity;
  const nested = chain.length > 1 ? viewport.window.querySelector(`[data-record-id="${CSS.escape(id)}"]`) : null;
  if (nested) {
    viewport.reveal(nested, { top: 18 });
    nested.closest(".renderer-turn")?.classList.add("source-flash");
  }
  return true;
}
/** The ids from `record` down to the record with `id` (itself included), or false. */
function recordChain(record, id, chain) {
  if (record.id === id) { chain.push(record.id); return true; }
  for (const part of record.body || []) if (part.p === "blocks") for (const item of part.items || []) {
    if (recordChain(item, id, chain)) { if (record.id) chain.unshift(record.id); return true; }
  }
  return false;
}

// #119: a workflow call's fleet roster is rendered WITH the record (`fleetHtml` reads the live
// meta), so a rematerialized block gets it for free — the classic page has to refill by hand.
// What a fresh render cannot cover is a meta that arrives while the block is already mounted:
// the members changed, the record did not. This rewrites just those boxes.
function refreshFleets() {
  for (const host of viewport.window.querySelectorAll("[data-run]")) {
    const shell = host.querySelector(":scope > .renderer");
    if (!shell) continue;
    const html = fleetHtml(host.dataset.run, recordState);
    const box = shell.querySelector(":scope > .fleet");
    if (!html) { box?.remove(); continue; }
    if (box) box.outerHTML = html;
    else shell.insertAdjacentHTML("beforeend", html);
  }
}
// #s53: the open session's headless workers in its Agents pane, after its in-process sub-agents.
// A row opens the worker's own session the way a sub-agent row opens its transcript — a descent,
// so the parent control leads back — and says it is a worker and what state it is in.
let lastWorkers = "";
// A worker is named by its BRIEF, the first line of the prompt it was started with: its own title
// is its project's, the same for every worker of a coordinator.
const workerName = row => row.brief || row.name || row.id;
const workersKey = workers => workers.map(row => `${row.id}:${displayState(row).reason}:${workerName(row)}`).join("|");
function workerRow(row) {
  const state = displayState(row), name = workerName(row);
  return `<div class="outline-agent-row"><button class="outline-agent" type="button" data-worker-session="${escapeText(row.id)}" title="Open the worker's session — ${escapeText(name)}"><span class="agent-state ${workerRunning(row) ? "running" : "completed"}"></span><span class="outline-agent-copy"><strong>${escapeText(name)}</strong><small>worker · ${escapeText(state.label)}</small></span><span class="outline-agent-tail"></span></button></div>`;
}
function directAgents(source = recordState.meta) {
  const meta = source || {}, result = [...(meta.children || [])], ids = new Set(result.map(item => item.id));
  for (const run of meta.runs || []) for (const member of run.members || []) if (!ids.has(member.id)) { ids.add(member.id); result.push(member); }
  return result;
}
// The outline column is a stack of DRAWERS (#139/#157, design/outline-drawers.md). A drawer's
// body has a HEIGHT, not a display, so it can sit anywhere between open and shut, and ONE number
// per pane says where it is: `drawers.open`, 0 to 1, with `drawers.dir` remembering which way it
// was last moving. Both controls write that one number, which is what stops them disagreeing.
//
// THE INPUT IS THE GESTURE, NOT THE SCROLL POSITION (#157). That distinction is the whole design.
// Closing a drawer REMOVES content, and the scroll position only exists because there is content
// to scroll — so spending `scrollTop` on the drawers eats the very room it needs to keep going,
// and the old model had to re-add the height the bodies gave up (an invisible spacer above the
// stack) to stop the column collapsing under itself. That kept `scrollHeight` constant, which
// fixed the scroll range, and the range is smaller than the state needs: measured, three panes
// shut wants 584px of offset where only 386px exist, so scrolling back could never reopen them.
//
// A push has no such limit. The wheel delta is a displacement the reader applies, so:
//
//   push down   spend it CLOSING, topmost pane with anything left open first — the one with the
//               least friction — and only what is left over scrolls the column.
//   pull up     give the scroll back FIRST, then reopen from the bottom, so the column retraces
//               exactly the way it came.
//
// Which pane a delta reaches is therefore read off the states of all of them, never off a fixed
// order, and a pane the toggle shut is simply a pane at 0 that the walk finds like any other.
//
// The bodies are painted at B·open, so the content is honestly shorter when drawers are shut —
// no spacer, no frozen scrollHeight, nothing to reconcile.
//
// #88's sticky SLOTS stay and now have only one job. Each card is `position: sticky` at the sum
// of the heads and gaps above it, stacked back-to-front, so when the OPEN content is taller than
// the window the heads pile up at the top and each card slides over the one above it. That is the
// browser's own mechanism and it no longer competes with anything.
const drawers = { natural: new Map(), open: new Map(), dir: new Map() };
// A pane the reader has turned OFF (#159) is not a shut drawer — it does not exist. Filtering it
// out here is the whole integration: the budget, the slots, the prefix and the toggle all read
// the column through this one function, so none of them can see a pane that is not there.
const drawerCards = () => [...byId("sessionNavigator").querySelectorAll(":scope > .outline-card:not(.pane-off)")];
/** The panes the reader keeps, applied to the column. A pane turned off renders nothing at all —
 *  no body, no head, no gap, no slot — which is what separates it from a drawer that is merely
 *  shut, and is the point the owner made when asking for it: if you do not care about the
 *  information you should not pay for its head either. */
function applyPanes() {
  for (const card of byId("sessionNavigator").querySelectorAll(":scope > .outline-card")) {
    const on = uiState.navPanes.has(card.dataset.navCard);
    card.classList.toggle("pane-off", !on);
  }
  renderNavigator();
  stackOutlineHeads();
}
/** The slots (#88, kept) and B(i), each drawer's body at its NATURAL height. Only a render or a
 *  resize can change either; a scroll never does, so this is not on the scroll path. */
function stackOutlineHeads() {
  const nav = byId("sessionNavigator");
  paintDrawerClasses(); // before any measuring — the list's max-height is written for `.open`
  const caption = nav.querySelector(":scope > .outline-caption");
  let slot = caption ? caption.getBoundingClientRect().height : 0;
  let depth = 0;
  for (const card of drawerCards()) {
    const head = card.querySelector(":scope > .outline-card-head");
    const body = card.querySelector(":scope > .outline-card-body");
    if (!head) continue;
    card.style.setProperty("--slot", `${Math.round(slot)}px`);
    card.style.zIndex = String(++depth);
    // B(i) is read off the LIST, never off the body: the body is the box we clip, so measuring it
    // would mean setting it back to `auto` for a frame — which cannot be animated from, and which
    // reports the stale height rather than the content when a list has SHRUNK. The list is never
    // height-constrained by us, so its own box is the natural height, exactly.
    const list = body && body.querySelector(":scope > .navigator-list");
    if (body) drawers.natural.set(card.dataset.navCard, list ? list.offsetHeight : body.scrollHeight);
    // The gap belongs to the slot: a compacted card rests on `slot`, so sticky catches at once.
    // The step is the card's height when SHUT, measured as the card minus the body it is showing
    // right now — its head plus whatever chrome the box carries, a border and the head's own
    // margin here. The head's height ALONE was 6px short of it, so three stacked cards came to
    // rest 2px apart where the gap is 8, and enough of them would have overlapped outright: the
    // gaps are rigid at every openness AND at every offset (rule 4), which is a promise about the
    // slots as much as about the margins (#214).
    const shut = card.getBoundingClientRect().height - (body ? body.getBoundingClientRect().height : 0);
    slot += shut + (parseFloat(getComputedStyle(card).marginBottom) || 0);
    drawerOpenOf(card.dataset.navCard); // seed it before anything measures the column
  }
  holdTheHeadsInTheColumn(nav, slot);
  paintDrawers();
}

/** The one requirement the model makes of the column (owner, #260): "the column needs to fit all
 *  the header portion of the panes + some gap space between them". Everything else follows from
 *  it — a chain that has spent its whole budget is heads and gaps, so if THOSE fit, the column
 *  never rests scrolled, and a column that never rests scrolled can never slide a sticky head
 *  over the body above it.
 *
 *  The column ends with 90px of breathing room, and at a short window that padding is the whole
 *  difference between the requirement holding and not. It reveals nothing (#214), and the heads
 *  are the thing that must never be unreachable, so at that window the breathing room goes. This
 *  is a CORNER CASE and stays one (owner: "fine with me to make the model also work with very
 *  short window, but that should be a corner case"): the class is off at every ordinary size, and
 *  nothing else in the model consults it.
 *
 *  `shut` is what `stackOutlineHeads` has just summed on its way down: the caption, every card at
 *  its shut height, and the gaps. The natural tail is read with the class OFF so the decision is
 *  made against the padding the stylesheet actually wants at this width — it is 90px at the
 *  default and 30px under 900px, and hard-coding either would mis-decide at the other. */
function holdTheHeadsInTheColumn(nav, shut) {
  nav.classList.remove("heads-tight");
  const style = getComputedStyle(nav);
  const top = parseFloat(style.paddingTop) || 0;
  const tail = parseFloat(style.paddingBottom) || 0;
  nav.classList.toggle("heads-tight", top + shut + tail > nav.clientHeight);
}
/** A pane's openness, seeded on FIRST READ from the boolean the reader left behind. An accessor
 *  rather than an initialiser, because a gesture can land before the column has ever been
 *  measured, and a bare `?? 1` would then read every unseeded pane as fully open. */
function drawerOpenOf(key) {
  if (!drawers.open.has(key)) drawers.open.set(key, uiState.navCards.has(key) ? 1 : 0);
  return drawers.open.get(key);
}
/** The panes a CLOSING push must leave alone: the ones from the first index whose card, and every
 *  card after it, is wholly inside the column's viewport (#206, owner: "I see no point of closing
 *  the bottom drawer once it is all visible (then that means if the second to the last drawer is
 *  also all visible, then no need to further collapse it because there can be no pressure from
 *  below)"). Closing buys room for what is below; a tail that is already wholly visible is asking
 *  for none, so taking from it would shrink the column for nothing. Opening is never exempt — a
 *  pull gives back in the order it took. */
function drawersUnderNoPressure(cards) {
  const nav = byId("sessionNavigator");
  const view = nav.getBoundingClientRect();
  const exempt = new Set();
  for (let i = cards.length - 1; i >= 0; i--) {
    const box = cards[i].getBoundingClientRect();
    // Wholly inside the visible column, within a pixel of rounding.
    if (box.top >= view.top - 1 && box.bottom <= view.bottom + 1) exempt.add(cards[i]);
    else break; // the run must reach the bottom: a card that is cut off puts every card above it under pressure
  }
  return exempt;
}
/** Spend `delta` px on the panes and return what is LEFT OVER. Closing (`delta > 0`) walks from
 *  the top and takes from the first pane with anything still open, skipping a tail that is under
 *  no pressure (above); opening walks from the bottom, so the column gives back in the reverse
 *  order it took, and a push followed by an equal pull lands exactly where it started. Both walks
 *  read the CURRENT states, which is what lets a pane the toggle shut be reopened by pulling: it
 *  is a pane at 0, found like any other. */
function routeDrawerDelta(delta) {
  const cards = drawerCards();
  const exempt = delta > 0 ? drawersUnderNoPressure(cards) : new Set();
  const order = delta > 0 ? cards : [...cards].reverse();
  let left = Math.abs(delta);
  for (const card of order) {
    if (left <= 0.5) break;
    if (exempt.has(card)) continue;
    const key = card.dataset.navCard;
    const natural = drawers.natural.get(key) || 0;
    if (!natural) continue;
    const open = drawerOpenOf(key);
    const room = (delta > 0 ? open : 1 - open) * natural;
    if (room <= 0.5) continue;
    const move = Math.min(left, room);
    left -= move;
    const next = delta > 0 ? open - move / natural : open + move / natural;
    drawers.open.set(key, Math.min(1, Math.max(0, next)));
    drawers.dir.set(key, delta > 0 ? "closing" : "opening");
  }
  return delta > 0 ? left : -left;
}
/** Paint the states. A pane at 100% is measured from its LIST right now: B is otherwise only
 *  refreshed when the column is stacked, and a list that gained rows or its max-height since then
 *  would leave the pane painted at a stale B — empty space under the rows, and a pane that
 *  reopens to a different height than it had at rest. Only the ends are read, never the part-way
 *  panes a gesture is animating. */
/** ONE source for what a pane's state is: the number. `open` and `shut` are its two ends. They
 *  are written before anything MEASURES the column, because the list's own max-height is
 *  addressed to `.outline-card.open` — a pane measured without the class reports the wrong
 *  natural, and the pane is then painted with empty space under its rows. */
function paintDrawerClasses() {
  for (const card of drawerCards()) {
    const open = drawerOpenOf(card.dataset.navCard);
    card.classList.toggle("shut", open === 0);
    card.classList.toggle("open", open > 0);
  }
}
/** Paint the states. Reads NOTHING: every height comes from the B measured at the last stack, so
 *  the gesture path does no layout work, and — the part that matters — a pane cannot appear to
 *  move because its B was re-measured underneath it. B changes only where it legitimately can,
 *  in `stackOutlineHeads`, which runs on a render and on a resize. */
function paintDrawers() {
  paintDrawerClasses();
  for (const card of drawerCards()) {
    const key = card.dataset.navCard;
    const body = card.querySelector(":scope > .outline-card-body");
    if (!body) continue;
    body.style.height = `${Math.round((drawers.natural.get(key) || 0) * drawerOpenOf(key))}px`;
  }
}
/** Where the LAST card's bottom sits relative to the column's: positive while there is still card
 *  below the fold, negative when the column is showing empty space under it.
 *
 *  The column ends with 90px of breathing room (`.session-navigator{padding:20px 60px 90px 18px}`),
 *  which the browser counts as scrollable overflow. Scrolling into it reveals nothing and costs the
 *  reader the layout: the cards are sticky at their slots with a z-index rising down the column, so
 *  an offset the drawers did not pay for drags each card up under the one above it. Measured on the
 *  owner's shape (#214): every card wholly visible, #206's rule exempting all three so the closing
 *  walk spent nothing, the whole delta going to `scrollTop`, and Turns then holding at its slot
 *  while Tasks flowed 45px up into it — 37px INSIDE the Turns body, the 8px gap gone, Tasks
 *  painting over a task row. The owner photographed it: "the last drawer should stop pushing when
 *  it is fully revealed … there should always be a gap between these two panes".
 *
 *  So the offset answers the same question the drawers do — is anything below asking for room? —
 *  in both directions: a push may spend its remainder only while this is positive, and whatever
 *  closing a drawer makes empty is given straight back (`reclaimColumnScroll`). */
function cardTailOffset() {
  const nav = byId("sessionNavigator");
  const cards = drawerCards();
  if (!cards.length) return 0;
  const view = nav.getBoundingClientRect();
  return cards[cards.length - 1].getBoundingClientRect().bottom - view.bottom;
}
/** Give back the offset a closing drawer just made empty. The browser does this itself when
 *  content shrinks — it clamps `scrollTop` to the new maximum — but the column's bottom padding
 *  keeps that maximum artificially high, so the offset survives as empty space below the last card
 *  and every card above stays stuck a little too high. Without it a push in a short window left
 *  9px of offset behind after the drawers had shut, and the last card, which cannot stick past the
 *  end of its own containing block, came to rest 1px inside the card above it. */
function reclaimColumnScroll() {
  const nav = byId("sessionNavigator");
  if (nav.scrollTop <= 0) return;
  const empty = -cardTailOffset();
  if (empty > 0) nav.scrollTop = Math.max(0, nav.scrollTop - empty);
}
/** The largest offset the CHAIN could ever have sold, which is the only offset the column may
 *  hold (#260).
 *
 *  A push is not a scroll: it is a budget spent closing drawers from the top, and the column's
 *  own `scrollTop` moves only with what is left once every drawer is shut — by which point the
 *  content is heads and gaps and there is nothing left to reveal. So the ceiling is the overflow
 *  of the SHUT column, and it is invariant under openness: open a body by δ and the tail moves
 *  down by δ too, so the two cancel.
 *
 *  It has to be enforced rather than assumed, because `scrollTop` has writers the chain never
 *  sees — `scrollIntoView` on a row or a card, the focus ring following a click, a scrollbar
 *  drag. That is where #260 came from: the demo tape framed each pane with `scrollIntoView`, the
 *  column took an offset no drawer had paid for, and the sticky heads (z-index rising downward)
 *  slid out of order — Turns caught its slot and held while Tasks kept coming and came to rest
 *  106px INSIDE the Turns body, cutting a turn row in half. The owner could not reproduce it on
 *  a trackpad because the trackpad goes through the chain, and the chain never sells that offset. */
function chainOffsetLimit(nav) {
  const cards = drawerCards();
  if (!cards.length) return 0;
  const bodies = cards.reduce((n, c) => {
    const body = c.querySelector(":scope > .outline-card-body");
    return n + (body ? body.getBoundingClientRect().height : 0);
  }, 0);
  return Math.max(0, nav.scrollTop + cardTailOffset() - bodies);
}
/** Give back any offset the chain did not sell. Idempotent, so the chain's own write survives it
 *  and a correction cannot start a loop: the second pass finds the offset already at the limit. */
function holdColumnToTheChain() {
  const nav = byId("sessionNavigator");
  if (nav.scrollTop <= 0) return;
  const limit = chainOffsetLimit(nav);
  if (nav.scrollTop > limit) nav.scrollTop = limit;
}
byId("sessionNavigator").addEventListener("scroll", holdColumnToTheChain);
/** A wheel delta in PIXELS, whatever units the device reports it in. */
function wheelPixels(event) {
  const nav = byId("sessionNavigator");
  if (event.deltaMode === 1) return event.deltaY * 16;
  if (event.deltaMode === 2) return event.deltaY * nav.clientHeight;
  return event.deltaY;
}
/* THE INTENTION FLOWS (#157, owner's model), and WHERE A GESTURE BEGINS DECIDES WHAT IT DRIVES
 * (#206, owner 2026-09-13: "if at the beginning of the scroll, the mouse is inside a pane, the
 * behavior should be scroll the content of the pane only (no more transition to drawer motion),
 * if the mouse is outside a pane, then the behavior would be pulling and pushing drawers. Note
 * that if the mouse is outside the drawer but during the scrolling inside, we will continue with
 * the drawer motion unless the user stops.")
 *
 * A run of wheel events with no real pause is ONE gesture, and the gesture owns what it started
 * on for its whole run:
 *
 *   began inside a pane's BODY   the pane's own list scrolls, and nothing else — when the list
 *                                reaches its end the gesture is spent, and the drawers do NOT
 *                                take over. (#157 handed over after a static-friction budget;
 *                                the owner asked for no handover at all, so the friction went
 *                                with it.)
 *   began anywhere else          the chain: closing and opening drawers, the remainder scrolling
 *                                the column — and it keeps the chain even if the pointer lands
 *                                inside a pane while the run continues.
 *
 * Stopping is what ends a gesture (`WHEEL_IDLE_MS` of quiet), and the next push aims afresh from
 * wherever the pointer is then. That is also what lets the reader scroll the clipped list of a
 * PART-WAY drawer: they stopped, so the new gesture is theirs to aim. */
const WHEEL_IDLE_MS = 200;
let wheelOwner = null;   // "pane" | "chain" while a gesture is in flight
let wheelPane = null;    // the pane body a "pane" gesture began in
let wheelIdle = 0;
/** The pane BODY the pointer is inside, or null — where a gesture begins decides what it drives
 *  (#206). The body is the pane's content; its head belongs to the drawer, so a push that starts
 *  on a head works the chain like any other push from outside. */
function paneBodyAt(target) {
  for (let el = target; el && el !== byId("sessionNavigator"); el = el.parentElement) {
    if (el instanceof Element && el.classList.contains("outline-card-body")) return el;
  }
  return null;
}
/** The scroller inside `body` that still has room in this direction, or null. */
function listWithRoom(body, dy) {
  if (!body) return null;
  for (const el of [body, ...body.querySelectorAll("*")]) {
    if (!(el instanceof Element)) continue;
    if (!/(auto|scroll)/.test(getComputedStyle(el).overflowY)) continue;
    const room = el.scrollHeight - el.clientHeight;
    if (room <= 1) continue;
    if (dy > 0 ? el.scrollTop < room - 1 : el.scrollTop > 1) return el;
  }
  return null;
}
/** The reader's push. Spent on whatever the gesture owns: a list scrolls itself and the browser
 *  does that; the chain closes and opens panes, and only the remainder scrolls the column. When
 *  we take the push we drive the offset ourselves, so a delta is never counted twice — once by
 *  the drawers and again by the browser, which is what made the old model need a spacer. */
byId("sessionNavigator").addEventListener("wheel", event => {
  const nav = byId("sessionNavigator");
  let dy = wheelPixels(event);
  if (!dy) return;
  clearTimeout(wheelIdle);
  wheelIdle = setTimeout(() => { wheelOwner = null; wheelPane = null; }, WHEEL_IDLE_MS);
  if (!wheelOwner) {
    // Where the gesture BEGAN, kept for its whole run (#206): a pane's body owns it, or the chain.
    wheelPane = paneBodyAt(event.target);
    wheelOwner = wheelPane ? "pane" : "chain";
  }
  if (wheelOwner === "pane") {
    const list = listWithRoom(wheelPane, dy);
    // The browser scrolls the pane's own list — and the Turns list, scrolled so, stops following
    // the session (#s207).
    if (list) { if (list.contains(byId("navigatorTurns"))) setTurnsFollow(false); return; }
    event.preventDefault();       // …and at its end the gesture is spent: no handover
    return;
  }
  if (dy > 0) {
    dy = routeDrawerDelta(dy);
    // The remainder scrolls the column only as far as there is a card left to reveal (#214).
    if (dy > 0) nav.scrollTop += Math.min(dy, Math.max(0, cardTailOffset()));
  } else {
    const used = Math.min(nav.scrollTop, -dy);
    if (used > 0) nav.scrollTop -= used;
    dy += used;
    if (dy < 0) routeDrawerDelta(dy);
  }
  paintDrawers();
  reclaimColumnScroll();
  event.preventDefault();
}, { passive: false });
/** A toggle's height change is animated; a gesture's is not — that one tracks the wheel 1:1. */
let drawerAnimation = 0;
function animateDrawers() {
  const nav = byId("sessionNavigator");
  nav.classList.add("drawers-animating");
  clearTimeout(drawerAnimation);
  drawerAnimation = setTimeout(() => nav.classList.remove("drawers-animating"), 260);
}
/** Set one pane's state. Re-stacks, because the slots and B move when a body does. */
function setDrawerOpen(key, open) {
  drawers.open.set(key, open);
  drawers.dir.set(key, open >= 1 ? "opening" : "closing");
  if (open >= 1) uiState.navCards.add(key);
  else if (open <= 0) uiState.navCards.delete(key);
  persist();
  animateDrawers();
  stackOutlineHeads();
}
/** The toggle, as the owner specified it (#157). At either END it flips. Part-way it finishes the
 *  movement the pane was last making; with no direction to follow it makes the bigger visual
 *  change, which is to close a pane that is mostly open and open one that is nearly shut. */
function toggleDrawer(key) {
  const card = byId("sessionNavigator").querySelector(`[data-nav-card="${CSS.escape(key)}"]`);
  if (!card || card.classList.contains("pane-off")) return;
  const open = drawerOpenOf(key);
  if (open <= 0) return setDrawerOpen(key, 1);
  if (open >= 1) return setDrawerOpen(key, 0);
  const dir = drawers.dir.get(key);
  const closing = dir === "closing" ? true : dir === "opening" ? false : open >= 0.5;
  setDrawerOpen(key, closing ? 0 : 1);
}
window.addEventListener("resize", stackOutlineHeads, { signal });
/** What a Turns pane row whose records are still coming adds to its title (#s276). */
const STILL_LOADING = " — still loading; the page goes there when it arrives";
/** A turn the turn index lists (#s276), shaped as a Turns pane row reads a unit: its first record,
 *  its number and label, and the sender or the quoted reply it carries. Its records are still on
 *  their way (`pending`). */
function indexedTurn(t) {
  const view = t.kind === "peer" ? { peer: { from: t.head?.from || "another session", mode: t.head?.mode || "" } } : { reply: t.kind === "user" ? replyContext(t) || undefined : undefined };
  return { from: t.r, turn: Number(t.turn) || 0, label: t.label || "", view, pending: true };
}
function renderNavigator() {
  const turns = recordState.units.filter(unit => unit.type === "user");
  const loading = recordState.units.some(unit => unit.type === "pending");
  // #s276: while the head is out, the turn index lists the turns it holds, so the pane is whole
  // from the first moment; a row whose records are still coming is marked, and a tap on it is held.
  // Rows follow the UNITS (a held landing keeps the head pending there); the compaction ticks
  // below follow the records they are read from.
  const index = loading ? recordState.headIndex : null;
  const owed = index ? index.turns.map(t => ({ at: t.r, unit: indexedTurn(t) })) : [];
  // #342: without the index, the turns drawn are the last few — the owner's 141-turn session read
  // "8" — but their NUMBERS are the true ones from the first moment, so the session's count is the
  // highest of them. Once the head lands they are all here.
  const partial = !index && loading && turns.length > 0;
  const turnCount = partial ? Math.max(...turns.map(unit => unit.turn || 0)) : owed.length + turns.length;
  const earlier = partial ? (turns[0].turn || 1) - 1 : 0;
  byId("navigatorTurnCount").textContent = turnCount;
  // A compaction is an epoch tick between turns (#69, restyled in #86): a hairline with a glyph
  // for how it happened and the context size from → to, in the rows' own type, no prose — so a
  // session that compacted fifteen times reads as fifteen chapters. The tick jumps to the
  // compaction record like a turn row jumps to its turn.
  const epochs = (owedIndex()?.compactions || []).filter(c => recordState.records[c.r]?.kind === "pending").map(c => ({ at: c.r, tick: compactionTick(c.head || {}), pending: true }));
  recordState.records.forEach((record, i) => { if (record.kind === "compaction") epochs.push({ at: i, tick: compactionTick(record.head || {}) }); });
  // #s277: a turn whose records are still on their way is a placeholder unit; its row reads what the
  // index said of its opener, and is marked as the index's own rows were.
  const rowUnit = unit => !unit.owed ? unit : unit.stub ? { ...indexedTurn({ ...unit.stub, r: unit.from }), turn: unit.turn, label: unit.label } : { ...unit, pending: true };
  const rows = [...owed, ...turns.map(unit => ({ at: unit.from, unit: rowUnit(unit) })), ...epochs].sort((a, b) => a.at - b.at);
  // #s138, Hong: a turn another session opened is told from the person's own, and one sender from
  // another — its row carries the sender as a chip in the sender's own hue (the card's too).
  const peerRow = unit => {
    const from = unit.view.peer.from;
    return `<button class="outline-turn-row outline-peer${unit.pending ? " outline-turn-pending" : ""}" style="--peer-hue:${peerHue(from)}" data-turn-record="${unit.from}" data-turn-number="${unit.turn}" data-peer-from="${escapeText(from)}" title="${escapeText(unit.label)}${unit.pending ? STILL_LOADING : ""}"><span class="outline-number">${String(unit.turn).padStart(2, "0")} ·</span><span class="outline-peer-from" title="Sent by ${escapeText(from)}, another session"><span class="outline-peer-glyph" aria-hidden="true">⇄</span>${escapeText(from)}</span><span class="outline-label">${escapeText(peerLabelRest(unit.label, from))}</span></button>`;
  };
  // #s292: the number column fits the session's widest turn number, the same width on every row, so
  // "150 ·" keeps to one line and the labels stay in line (a 24px column wrapped every row from 100).
  byId("navigatorTurns").style.setProperty("--outline-digits", String(Math.max(2, String(turnCount).length)));
  byId("navigatorTurns").innerHTML = (earlier > 0 ? `<div class="activity-empty outline-turns-loading" role="status">Turns 1–${earlier} are still loading</div>` : "") + rows.map(r => r.unit?.view?.peer ? peerRow(r.unit) : r.unit
    ? `<button class="outline-turn-row${r.unit.view?.reply ? " outline-quoted" : ""}${r.unit.pending ? " outline-turn-pending" : ""}" data-turn-record="${r.unit.from}" data-turn-number="${r.unit.turn}" title="${escapeText(r.unit.label)}${r.unit.pending ? STILL_LOADING : ""}"><span class="outline-number">${String(r.unit.turn).padStart(2, "0")} ·</span>${r.unit.view?.reply ? `<span class="outline-quote-marker" role="img" aria-label="Quoted reply" title="Reply to selected text">│</span>` : ""}<span class="outline-label">${escapeText(r.unit.label)}</span>${r.unit.view?.reply?.items.length > 1 ? `<span class="outline-reply-count" aria-label="${r.unit.view.reply.items.length - 1} more annotations">+${r.unit.view.reply.items.length - 1}</span>` : ""}</button>`
    : `<button class="outline-epoch${r.pending ? " outline-turn-pending" : ""}" type="button" data-turn-record="${r.at}" title="${escapeText(r.tick.title)}${r.tick.sizes ? ` · ${escapeText(r.tick.sizes)}` : ""} — jump to the compaction" aria-label="${escapeText(r.tick.title)}${r.tick.sizes ? `, ${escapeText(r.tick.sizes)}` : ""}"><span class="outline-epoch-line" aria-hidden="true"></span><span class="outline-epoch-glyph" aria-hidden="true">${r.tick.glyph}</span>${r.tick.sizes ? `<span class="outline-epoch-sizes">${escapeText(r.tick.sizes)}</span>` : ""}<span class="outline-epoch-line" aria-hidden="true"></span></button>`
  ).join("") || '<div class="activity-empty">No turns</div>';
  const tasks = recordState.meta?.tasks || [];
  // #224, the owner: the head counts what the pane SHOWS. A task the session recorded no title for
  // is not listed under any setting (#217), so counting it leaves the reader with "7 active" over a
  // pane showing fewer and no way to reconcile the two. Counted on a COPY: the rows carry their
  // index into `tasks` itself, and `data-task-open` hands that index straight back to the popover.
  // The reader's own state filter (#218) is deliberately NOT applied here — that is a view they
  // chose and can undo, and the head saying what exists behind it is useful.
  const counted = tasks.filter(task => task.subject || task.title);
  const runningTasks = counted.filter(task => taskStatus(task.status) === "in_progress").length;
  const doneTasks = counted.filter(task => taskStatus(task.status) === "completed").length;
  byId("navigatorWorkCount").innerHTML = outlineSummary(runningTasks, doneTasks, counted.length);
  outlineCurrent = null; // the rows were rebuilt: mark and reveal the current one afresh
  updateOutlineFocus();
  byId("outlineTaskDot").hidden = !runningTasks;
  // Grouped and ordered (#56): completed, running, pending — then by id within a group; the
  // stream's index rides along on each row because the record targets are keyed by it.
  const taskRow = ({ task, index }) => {
    // The row opens the task's details (#60); the jump to where its status was recorded is an
    // action inside the popover, so a click never moves the transcript by surprise.
    const status = taskStatus(task.status), statusClass = status === "in_progress" ? "running" : status;
    // #188: a task with no recorded title takes the classic page's words for it, not an invented
    // ordinal. This read `Task ${index + 1}` — the row's POSITION — beside a tail showing the real
    // id, so a stub read "Task 1 … #q119": two numbers for one task, one of them made up.
    // A second line when there is something to say — who holds it, what blocks it, what gates
    // it, whether it is parked (#125, the queue board's two-line card). Silence keeps it to one.
    const meta = taskRowMeta({ ...task, blockedBy: task.blocked_by || task.blockedBy || [] });
    return `<div class="work-task"><button class="work-task-head" type="button" data-task-open="${index}" title="Task details" aria-haspopup="dialog"><span class="task-state ${escapeText(statusClass)}"></span><span class="work-copy"><strong>${escapeText(task.subject || task.title || TASK_NO_TITLE)}</strong>${meta ? `<small class="work-task-meta">${escapeText(meta)}</small>` : ""}</span><span class="work-tail">#${escapeText(task.id || index + 1)}</span></button></div>`;
  };
  // #186: live only, by default. A session with a hundred finished tasks shows a wall nobody
  // reads — "renders it useless" — so the pane opens on what is still moving and the reader asks
  // for the rest. The row objects carry their own stream index, so filtering the LIST is safe:
  // `taskGroups` re-derives the groups from whatever it is given.
  // #218: the pane shows the task states the reader checked. "other" (cancelled, or a status the
  // stream never classified) is always shown — it is neither live nor done, and a box that does not
  // exist would hide it for good.
  // #319: except on a phone, which has no way to reach that filter (it lives in the outline's
  // menu) — its Tasks drop-down lists every state, and its head jumps between them. The reader's
  // remembered choice is left as it is, for the desktop.
  const shownGroups = layout.phone() ? new Set(TASK_GROUP_ROWS.map(g => g.key)) : uiState.taskGroupsShown;
  const liveTasksOnly = !TASK_GROUP_ROWS.every(g => shownGroups.has(g.key));
  // Filter the GROUPS, never the list. Each row carries `index`, the position in `meta.tasks`,
  // and `data-task-open` hands that index straight to `openTaskPopover`, which reads the FULL
  // list back — so filtering the array first would silently open the wrong task's details.
  const afterFilter = taskGroups(tasks).filter(group => group.key === "other" || shownGroups.has(group.key));
  // #217, the owner: "hide tasks on the tasks pane that says no title recorded in this session. It
  // is pointless to show those tasks". A task the session recorded no title for is a row that says
  // it has nothing to say, and a pane is for SCANNING. The detail card still says the absence
  // honestly (#187/#188) — a card is opened deliberately, and there the gap is the answer. Filtered
  // per ROW rather than out of `tasks`, because each row carries its index into that array and
  // `data-task-open` hands the index straight back to the popover.
  const titled = row => !!(row.task.subject || row.task.title);
  const taskShown = afterFilter
    .map(group => ({ ...group, rows: group.rows.filter(titled) }))
    .filter(group => group.rows.length);
  // The two hidings are counted apart, because only one of them is the reader's to undo: the
  // filter's count is what its control reports, and an untitled task is gone whatever they choose.
  const liveRows = afterFilter.reduce((n, group) => n + group.rows.length, 0);
  const hiddenTasks = tasks.length - liveRows;
  const untitledTasks = liveRows - taskShown.reduce((n, group) => n + group.rows.length, 0);
  // An empty pane must never look like missing data: say that the filter is what emptied it, and
  // how much it is holding, because the reader cannot see what is not there.
  const untitledNote = untitledTasks ? ` · ${untitledTasks} with no recorded title` : "";
  // #s356, the owner: "the sub agent pane is empty, it says 149 agents finished and hidden, is
  // there way to show those agents?" The way was the Outline menu's row, which the reader could not
  // find from the pane they were looking at. So the line the filter empties carries the control
  // that undoes it, and says where the setting lives so they can put it back.
  const noTasks = liveTasksOnly && hiddenTasks
    ? `<div class="activity-empty"><span>Nothing live — ${hiddenTasks} finished ${hiddenTasks === 1 ? "task" : "tasks"} hidden${escapeText(untitledNote)}</span>${showFilteredControl("tasks", hiddenTasks)}</div>`
    : untitledTasks
      ? `<div class="activity-empty">No session tasks${escapeText(untitledNote)}</div>`
      : '<div class="activity-empty">No session tasks</div>';
  byId("navigatorWork").innerHTML = taskShown.map(group => `<div class="work-group" data-task-group="${group.key}"><span>${group.label}</span><span class="work-group-count">${group.rows.length}</span></div>${group.rows.map(taskRow).join("")}`).join("") || noTasks;
  mobileShell?.tasksChanged(); // the phone drop-down's jumps name the groups just drawn (#319)
  const agents = directAgents(), workers = sessionWorkers();
  const activeAgents = agents.filter(agent => agent.running).length + workers.filter(workerRunning).length;
  const allAgents = agents.length + workers.length;
  byId("navigatorAgentCount").innerHTML = outlineSummary(activeAgents, allAgents - activeAgents, allAgents);
  lastWorkers = workersKey(workers);
  // A sub-agent row (#61): the click opens the sub-agent's OWN transcript — the whole view
  // switches, the header shows the child with its way back to the parent — and the spawn point
  // in the parent stays reachable as a small secondary control when the record stream kept it.
  const liveAgentsOnly = uiState.liveOnly.has("agents");
  const shownAgents = liveAgentsOnly ? agents.filter(agent => agent.running) : agents;
  const shownWorkers = liveAgentsOnly ? workers.filter(workerRunning) : workers;
  const hiddenAgents = allAgents - shownAgents.length - shownWorkers.length;
  const noAgents = liveAgentsOnly && hiddenAgents
    ? `<div class="activity-empty"><span>None running — ${hiddenAgents} finished ${hiddenAgents === 1 ? "agent" : "agents"} hidden</span>${showFilteredControl("agents", hiddenAgents)}</div>`
    : '<div class="activity-empty">No direct children</div>';
  const agentRows = shownAgents.map(agent => {
    const target = recordState.agentTargets.get(String(agent.id));
    const spawn = target == null ? "" : `<button class="outline-agent-spawn" type="button" data-agent-record="${target}" title="Jump to where the parent launched this agent" aria-label="Jump to where the parent launched ${escapeText(agent.title || agent.id)}"><span aria-hidden="true">↳</span></button>`;
    return `<div class="outline-agent-row"><button class="outline-agent" type="button" data-child-outline="${escapeText(agent.id)}" title="Open the sub-agent's transcript"><span class="agent-state ${agent.running ? "running" : "completed"}"></span><span class="outline-agent-copy"><strong>${escapeText(agent.title || agent.description || agent.id)}</strong><small>${escapeText(agent.type || agent.agent_type || "agent")}</small></span><span class="outline-agent-tail"></span></button>${spawn}</div>`;
  }).concat(shownWorkers.map(workerRow)).join("");
  byId("navigatorAgents").innerHTML = agentRows ? phoneAgentsFilter(liveAgentsOnly, hiddenAgents, allAgents - activeAgents) + agentRows : noAgents;
  // What the filter is holding back, for the menu row that controls it to say so (#215). The head
  // reads "25 active · 253/308 done" either way, so without this the reader has no way to tell a
  // pane that is filtered from a pane that is simply short.
  recordState.hiddenByFilter = { tasks: liveTasksOnly ? hiddenTasks : 0, agents: liveAgentsOnly ? hiddenAgents : 0 };
  // How many tasks each state holds, for the menu's rows to show what a box is worth (#218).
  recordState.taskGroupCounts = Object.fromEntries(taskGroups(tasks).map(g => [g.key, g.rows.length]));
  // The rows the pane ACTUALLY rendered, in order, for anything that wants to aim at one (#225).
  // The centring control used to index `taskOrder(tasks)` — the whole board — into a DOM holding
  // only what survived the live filter (#186), the state filter (#218) and the untitled rule
  // (#217), so on any real session its target was past the end and the click did nothing at all.
  recordState.shownTaskRows = taskShown.flatMap(group => group.rows);
  renderSessionInfo(turnCount, allAgents);
  viewRoot.$$("[data-nav-card]").forEach(card => card.classList.toggle("open", uiState.navCards.has(card.dataset.navCard)));
  stackOutlineHeads();
  drawOutline(layout.arrangement());
  mobileShell?.counts(turnCount, counted.length, allAgents);
}
/** The control an emptied pane carries (#s356): it clears the same remembered filter the Outline
 *  menu's row does, so the menu and the pane cannot disagree. The hint names that row so the reader
 *  can put the filter back — on a desktop only: a phone has no Outline menu (its way back is
 *  `phoneAgentsFilter`), and its Tasks pane shows every state already (#319). */
function showFilteredControl(pane, hidden) {
  const what = pane === "agents" ? `${hidden} finished ${hidden === 1 ? "agent" : "agents"}` : `${hidden} finished ${hidden === 1 ? "task" : "tasks"}`;
  const hint = layout.phone()
    ? ""
    : pane === "agents"
      ? "Active only, in the Outline menu, hides them again"
      : "The Outline menu's task states hide them again";
  return `<button class="activity-empty-show" type="button" data-show-filtered="${pane}" aria-label="Show the ${escapeText(what)}">Show them</button>${hint ? `<small class="activity-empty-hint">${escapeText(hint)}</small>` : ""}`;
}
/** A phone's way to the Agents filter (#s358, Hong's choice on #s357): it has no Outline menu, so
 *  the list carries a row for whichever end applies, on the same remembered filter. Hide finished
 *  while the filter is off; while it is on, Show them, because with an agent still running the list
 *  is not empty and the emptied pane's control is not there. At the TOP of the list, which can run to
 *  149 agents. Every row here is a phone's alone: a desktop keeps the menu. */
function phoneAgentsFilter(liveOnly, hidden, finished) {
  if (!layout.phone()) return "";
  const what = n => `${n} finished ${n === 1 ? "agent" : "agents"}`;
  if (liveOnly) return hidden ? `<button class="agents-filter-row" type="button" data-show-filtered="agents">Show ${what(hidden)}</button>` : "";
  return finished ? `<button class="agents-filter-row" type="button" data-hide-finished="agents" aria-label="Hide the ${what(finished)}">Hide finished agents</button>` : "";
}
const outlineSummary = (active, done, total) => !total ? "0" : `${active ? `<span class="outline-stat-item active"><i class="outline-stat-dot"></i>${active} active</span>` : ""}<span class="outline-stat-item done"><i class="outline-stat-dot"></i>${done}/${total} done</span>`;
// The info pane (#67, #68): only what the shell does not already show — the title, the agent and
// the project are in the header and the tree row, so the Session group carries status and
// counts; the Usage group has the cached-read tokens and, when the session compacted, the
/* #170. INFO LEAVES THE DRAWER STACK. Its content is reference material — model, token columns,
 * cwd, the runtime settings — asked once and not acted on, so a quarter of the column for it is
 * over-weighted. But two of its values are not reference material at all: the COST, which the
 * reader watches climb through a long run, and CONTEXT LEFT, which predicts the next compaction.
 * Those two stay ambient in a one-line footer strip, in the same ~40px the card head used, and
 * the rest goes behind the glyph beside them.
 *
 * Peek on HOVER, pin on CLICK. Auto-hide alone would strand `infoFolds` (#89 persists per-group
 * fold state precisely because readers keep some of this standing), leave keyboard users no
 * target, and make a value impossible to select and copy — a hover popover is fine for an action
 * menu and wrong for a surface you read inside.
 */
const outlineFooter = document.createElement("div");
outlineFooter.className = "outline-footer";
outlineFooter.id = "outlineFooter";
outlineFooter.innerHTML = '<span class="outline-footer-cost" id="outlineFooterCost">—</span><span class="outline-footer-context" id="outlineFooterContext"></span><button class="outline-footer-info" id="outlineFooterInfo" type="button" aria-haspopup="dialog" aria-expanded="false" title="Session details"><svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/></svg></button>';
// Held by REFERENCE, never through `byId`: these are built here, and the contract's rule that
// every `byId` names an element of the generated shell is worth keeping exact.
const footerCost = outlineFooter.querySelector(".outline-footer-cost");
const footerContext = outlineFooter.querySelector(".outline-footer-context");
const footerInfo = outlineFooter.querySelector(".outline-footer-info");
const usageCostLabel = usage => usage && (usage.cost || (usage.cost_partial ? "unpriced" : null));
// #240: Claude Code's OWN recorded cost, as zero or one extra row beside our estimate. The
// window travels with the figure because the client records cost only while its process runs —
// `reportedCostDisplay` is shared with the classic page so both phrase it the same way.
const reportedCostRow = reported => {
  const r = reportedCostDisplay(reported);
  if (!r) return [];
  return [["client cost", r.note ? `${r.label} (${r.note})` : r.label, r.title]];
};
function paintOutlineFooter(row, usage) {
  const display = costDisplay(row);
  const short = costDisplay(row, true);
  const ownCost = usageCostLabel(usage);
  const own = row && row.costOwn != null ? Number(row.costOwn) : null;
  const subs = row && row.costSubs != null ? Number(row.costSubs) : null;
  // Split only a COMPLETE roll-up with an affirmative priced root. A partial total is one lower
  // bound, and total == children is not evidence for an own `$0.00`.
  if (display.kind === "priced" && own != null && Number.isFinite(own) && subs != null && Number.isFinite(subs)) {
    footerCost.textContent = `~$${own.toFixed(2)} + ~$${subs.toFixed(2)} sub`;
    footerCost.title = `total ${display.label} = this session ~$${own.toFixed(2)} + sub-agents ~$${subs.toFixed(2)}`;
  } else {
    // The index row is the whole family total. A direct sub-agent view has no row, so its own
    // usage-level signal is the fallback — including the fully-unpriced state.
    footerCost.textContent = display.label || ownCost || "—";
    footerCost.title = "";
  }
  footerCost.dataset.short = short.label || ownCost || "—";
  const rt = usage.runtime || {};
  const left = rt.context_left != null ? `${rt.context_left}% left` : "";
  footerContext.textContent = left;
  footerContext.hidden = !left;
}
// classic panel's summary ("24× compacted, 4.0M dropped" — a real fact even at zero tokens).
function renderSessionInfo(turns, agents) {
  const row = selectedRow(), meta = recordState.meta || {}, usage = meta.usage || {};
  // #170: BEFORE the early return. The strip is always on screen, so it always has to say
  // something — a session with no cost yet shows an em dash rather than a stale figure or a gap.
  paintOutlineFooter(row, usage);
  if (!row) {
    byId("navigatorSession").innerHTML = '<div class="activity-empty">No session selected</div>';
    return;
  }
  // #170: the card head's cost summary went with the card. Cost is the FOOTER STRIP's now, split
  // the same way — the own share in the text, the total in the hover — because the own share is
  // what the transcript's usage panel reports and one opaque total read as a mismatch.
  // Each subsection folds on its own label and the choice is the READER's, kept across sessions
  // and reloads (#89): a folded group is its label alone, and the rows are not rendered at all.
  const group = (label, rows) => {
    const key = label.toLowerCase();
    const folded = uiState.infoFolds.has(key);
    // A row may carry a THIRD element: the hover that explains it. The classic page's usage
    // panel puts one on the client-cost row, and an explanation the other shell does not carry
    // is an information gap between the two — the thing the parity work exists to catch.
    const body = folded ? "" : rows.map(([name, value, title]) => `<div class="session-info-row"${title ? ` title="${escapeText(title)}"` : ""}><span>${escapeText(name)}</span><strong>${escapeText(value ?? "—")}</strong></div>`).join("");
    return `<div class="session-info-group${folded ? " folded" : ""}" data-info-group="${escapeText(key)}"><button class="session-info-label" type="button" data-info-fold="${escapeText(key)}" aria-expanded="${!folded}"><span class="session-info-chevron" aria-hidden="true">${folded ? "▸" : "▾"}</span>${escapeText(label)}</button>${body}</div>`;
  };
  // #170: the SESSION group is gone rather than relocated. All three rows are already ambient
  // within 40px of here — status is the topbar `#statusChip` (with its dot and "inferred"
  // detail) and the sidebar row chip, turns is `#navigatorTurnCount` on the Turns head,
  // children is `#navigatorAgentCount` on the Agents head. Info's copy was duplication.
  byId("navigatorSession").innerHTML = `<div class="session-info">${group("Usage", [["model", usage.model], ["input", usage.input || usage.input_tokens], ["output", usage.output || usage.output_tokens], ["cache read", usage.cache_read], ...(usage.compacted ? [["compacted", usage.compacted]] : []), ["est. cost", usageCostLabel(usage) || "—"], ...reportedCostRow(usage.reported)])}${group("Runtime", [["cwd", meta.cwd || row._group?.secondary], ...runtimeRows(usage.runtime).filter(r => r.state !== "absent" || RUNTIME_ALWAYS.includes(r.key)).map(r => [r.label, runtimeText(r, agentName(row.agent))])])}</div>`;
}

// #170: the info content MOVED out of this column into the footer popover, so the delegation
// has to reach both hosts — a handler bound to the navigator alone stopped hearing the fold
// clicks the moment `#navigatorSession` left it.
const navigatorClick = event => {
  const turn = event.target.closest("[data-turn-record]");
  if (turn) {
    setTurnsFollow(true, false);
    const record = Number(turn.dataset.turnRecord);
    heldTurnJump = null;
    // #s276: a turn the index listed whose records have not arrived is not a place to land yet
    // (`jumpToRecord` refuses the loading card): the page goes there when they do. Stage 2 (#s277)
    // lands at once on the turn's placeholder and reads that turn first (`jumpOrFetch`).
    if (recordStore.windowed) { jumpOrFetch(record, "turn"); return; }
    if (!viewport.jumpToRecord(record, "turn") && turn.classList.contains("outline-turn-pending")) {
      heldTurnJump = { record, at: performance.now() };
      toast("Still loading — the page goes there when it arrives");
    }
    return;
  }
  const infoFold = event.target.closest("[data-info-fold]");
  if (infoFold) {
    const key = infoFold.dataset.infoFold;
    uiState.infoFolds.has(key) ? uiState.infoFolds.delete(key) : uiState.infoFolds.add(key);
    persist();
    // The whole pane re-renders: the counts are computed there, not held.
    renderNavigator();
    return;
  }
  const open = event.target.closest("[data-task-open]"); if (open) { openTaskPopover(Number(open.dataset.taskOpen), open); return; }
  const task = event.target.closest("[data-task-record]"); if (task) { closeTaskPopover(); jumpOrFetch(Number(task.dataset.taskRecord), "task"); return; }
  const agent = event.target.closest("[data-agent-record]"); if (agent) { jumpOrFetch(Number(agent.dataset.agentRecord), "agent"); return; }
  // #s356: an emptied pane's "Show them" clears the filter the Outline menu's row holds — Agents'
  // Active only, or every task state shown — and the menu, if open, redraws to say so.
  const showFiltered = event.target.closest("[data-show-filtered]");
  if (showFiltered) {
    if (showFiltered.dataset.showFiltered === "agents") uiState.liveOnly.delete("agents");
    else for (const g of TASK_GROUP_ROWS) uiState.taskGroupsShown.add(g.key);
    persist();
    renderNavigator();
    renderPanesMenu();
    return;
  }
  // #s358: a phone's way back, the same remembered filter again (`phoneAgentsFilter`).
  if (event.target.closest("[data-hide-finished]")) {
    uiState.liveOnly.add("agents");
    persist();
    renderNavigator();
    renderPanesMenu();
    return;
  }
  const child = event.target.closest("[data-child-outline]"); if (child) { descendTo(child.dataset.childOutline); return; }
  const worker = event.target.closest("[data-worker-session]"); if (worker) { descendTo(worker.dataset.workerSession); return; }
  // A card's head opens and shuts ITS drawer (#74, #139) — the whole head, and nothing else
  // changes: the other drawers keep their openness. On the one drawer that is part-way, the
  // toggle completes the movement the slide was making instead (design/outline-drawers.md).
  const card = event.target.closest("[data-nav-card-toggle]");
  if (card) {
    toggleDrawer(card.dataset.navCardToggle);
    return;
  }
  // A glyph in the column opens the outline at its pane where the outline fits (#s431 stage 3: with
  // the room its host could give), and its list as the top bar's panel where it does not.
  const rail = event.target.closest("[data-nav-card-open]");
  if (rail) {
    if (!layout.arrangement()?.outlineCanOpen && openOutlinePanel(rail.dataset.navCardOpen)) return;
    toggleNavigator(true);
    setDrawerOpen(rail.dataset.navCardOpen, 1);
  }
};
byId("sessionNavigator").onclick = navigatorClick;
outlineFooter.addEventListener("click", navigatorClick);

// A record's lowercase text with its scope ownership (#101), cached per record object — the
// store keeps committed records and re-creates rewritten ones, so a stale entry never lingers.
const searchTextCache = new WeakMap();
function searchTextOf(record) {
  let entry = searchTextCache.get(record);
  if (!entry) { entry = recordTextParts(recordForSearch(record), stripTags, s => s.toLowerCase()); searchTextCache.set(record, entry); }
  return entry;
}
/** This shell's names for the shared task card (html/shared/task-card.js). */
const APP_TASK = { card: "sv-task-card", head: "sv-task-card-head", glyph: "sv-task-card-glyph", id: "sv-task-card-id", title: "sv-task-card-title", chips: "sv-task-card-chips", chip: "task-chip", dates: "sv-task-card-dates", gap: "sv-task-card-gap", section: "sv-task-card-section", label: "sv-task-card-label", body: "sv-task-card-body", item: "sv-task-card-item", outcome: "sv-task-card-out", log: "sv-task-card-log", logTime: "sv-task-card-log-time", logMsg: "sv-task-card-log-msg", logBy: "sv-task-card-log-by" };
const ALL_SCOPES = ["u", "a", "t", "o"];
/** The active scope as a set for the shared grammar; null when every class is on (no scope). */
function activeScopeSet() {
  const scopes = uiState.searchScopes;
  // Every class on is NO class scope, whole-words or not (#118): the classic page reads `w:` as
  // "whole words, everywhere", and a mask of all four would quietly drop the records no class
  // claims — an agent event, an attachment, a queued prompt. Unless `o(…)` narrows the tools
  // (#367): then every class is on but the tools class is not every tool.
  const all = ALL_SCOPES.every(k => scopes.has(k)) && !queryTools().length;
  if (all && !uiState.searchWhole) return null;
  const set = { u: false, a: false, t: false, o: false, w: !!uiState.searchWhole };
  if (!all) for (const k of ALL_SCOPES) if (scopes.has(k)) set[k] = true;
  return set;
}
/** The box is the truth (#101, the classic page's rule): a typed `uatow:` prefix — `o(…)` naming
 *  tools — sets the scope rows; a leading `:` escapes. Counts per class fill the scope rows;
 *  stepping and marks honour the scope; the total reads "N hits in uo(B)". */
/** #333: whether the box holds a query — typed text or chips. On a phone the closed box then keeps
 *  its count and its arrows (production.css), so the reader still sees how many and can step.
 *  Looked up here, not held: this runs during the module's first render. */
function markQuery() {
  viewRoot.$(".header-searchbox")?.classList.toggle("has-query", !!boxQuery());
}
function updateSearch(reset) {
  // #s278: text typed but not submitted (search on Enter) is not searched by a fill, a pull or an
  // answer arriving. A search started any other way — Enter, a scope row, whole words, a chip's ✕ —
  // submits the box as it stands, ends a step still waiting on the last query's answer, and asks
  // again where an answer gave up, the search's or the filter's.
  let refilter = false;
  if (reset) {
    recordState.pendingSearch = false;
    stepWhenFound = null;
    if (remoteSearch?.found?.failed) remoteSearch = null;
    if (remoteFilter?.found?.failed) { remoteFilter = null; refilter = true; }
  } else if (recordState.pendingSearch) return;
  // The chips and the typed text together are the query (#303): a chip is a facet frozen out of
  // the text, so it is parsed exactly as the token it was.
  const raw = boxQuery();
  markQuery();
  // The split, the per-record count, the label: the shared rules (#118, shared/search.js), so a
  // query means the same thing on both pages — including the two-character floor, which this
  // shell did not have.
  const q = splitQuery(raw, undefined, spec => resolveToolSpec(spec, toolLetters()));
  // The box's `o(…)` tools (#292, #367): read before the scope, which they narrow. The set is
  // independent of the text, so the hits it opens are recomputed only when the tools themselves
  // change — a keystroke must not re-walk every record and re-open folds.
  const tools = q.tools || [];
  const toolKey = tools.map(t => (t.prefix ? "^" : "=") + t.name.toLowerCase()).sort().join(" ");
  const toolsChanged = toolKey !== (recordState.toolKey || "");
  recordState.searchTools = tools;
  recordState.toolKey = toolKey;
  uiState.toolFilters = new Set(tools.map(t => t.name + (t.prefix ? "*" : "")));
  // The rows follow the box (#101): its prefix (§8) ticks them; none (or an escaped `:ua:`) means
  // everything.
  if (q.set) {
    uiState.searchScopes = new Set(scopeLetters(q.set).length ? scopeLetters(q.set) : ALL_SCOPES);
    uiState.searchWhole = !!q.set.w;
  } else {
    uiState.searchScopes = new Set(ALL_SCOPES); uiState.searchWhole = false;
  }
  const query = q.tooShort ? "" : q.lc;
  recordState.search = query;
  const set = activeScopeSet();
  const wanted = scopeMask(set), whole = !!(set && set.w);
  recordState.searchMask = wanted; recordState.searchWhole = whole;
  // Drawn only now that both halves of the state follow the box: drawn above the line before, the
  // menu and its badge showed the PREVIOUS query's tools, so a ticked tool read as unticked (#297).
  renderFilterMenu();
  renderChips();
  recordState.matches = [];
  const classCounts = zeroCounts();
  let total = 0;
  // #s278: the server answers for the records below a records-by-turn head's boundary, and the page
  // counts the rest itself — never both for one record (`scanFrom`, as the panes count).
  const from = scanFrom();
  const remote = query ? remoteFound({ lc: query, whole, wanted, tools }) : { ready: true, found: null };
  if (remote.found) {
    for (const [r, n] of remote.found.records) { recordState.matches.push(r); total += n; }
    for (const k of CLASS_ORDER) classCounts[k] += remote.found.counts[k] || 0;
  }
  if (query) recordState.records.forEach((record, index) => {
    if (index < from) return;
    const { text, parts } = searchTextOf(record);
    // `o(…)` narrows the TOOLS class: "Bash calls whose text matches", counted inside the call's
    // own words (design §4), beside whatever the other letters ask for.
    const inScope = countRecord(text, parts, query, whole, wanted, classCounts, tools);
    if (inScope) { recordState.matches.push(index); total += inScope; }
  });
  if (toolsChanged) applyToolFilter();
  else if (refilter) refreshFilterHits();
  for (const k of CLASS_ORDER) { const cell = byId("scopeRow").querySelector(`[data-scope-count="${k}"]`); if (cell) cell.textContent = query ? String(classCounts[k]) : ""; }
  if (reset) { recordState.match = recordState.matches.length ? 0 : -1; recordState.landed = null; }
  else {
    // #s278: hits the server answered can arrive in front of the ones counted here; the current hit
    // stays the one the reader landed on.
    const at = recordState.landed != null ? recordState.matches.indexOf(recordState.landed) : -1;
    recordState.match = at >= 0 ? at : Math.min(recordState.match, recordState.matches.length - 1);
  }
  paintMatchCount(query ? (remote.ready ? countLabel(total, set, whole, q.toolSpec) + (remote.found?.failed ? " · earlier turns not searched" : "") : "Searching…") : "");
  markSearch();
  applyFilters();
}
/** The count the box shows: the query's own words when there is a query, else what the tool
 *  filter found (#133) — the filter is a search, and a search says how many. */
function paintMatchCount(text) {
  const hits = !text && queryTools().length ? (recordState.filterMatches || []).length : 0;
  // #s278: the filter's answer for the records still on their way is not in yet — or gave up.
  const filtering = !text && queryTools().length && remoteFilter && !!owedIndex();
  const waiting = filtering && !remoteFilter.found, failed = filtering && !!remoteFilter.found?.failed;
  const count = hits ? `${hits} ${hits === 1 ? "match" : "matches"}` : "";
  byId("transcriptSearchCount").textContent = text || (waiting ? "Searching…" : failed ? `${count || "0 matches"} · earlier turns not searched` : count);
}
/** The scope rows rewrite the box's prefix (the classic page's applyScopeFromMenu). */
function applyScopeFromMenu() {
  // #303: the menu sets the scope CHIP (the typed text keeps no prefix of its own). Unticking
  // Tools takes its tools with it (#367: a tool is only ever chosen inside Tools).
  const input = byId("transcriptSearchInput");
  if (!uiState.searchScopes.has("o")) uiState.chips.tools = [];
  const everything = ALL_SCOPES.every(k => uiState.searchScopes.has(k));
  const letters = [...(everything && !uiState.chips.tools.length ? [] : ALL_SCOPES.filter(k => uiState.searchScopes.has(k))), ...(uiState.searchWhole ? ["w"] : [])];
  input.value = writePrefix(input.value, []);
  setScopeChip(letters);
  updateSearch(true);
}
/** The scope chip's letters: `o` is held only as "every tool" — with tools named, the tools chip
 *  is the `o(…)` (#367). */
function setScopeChip(letters) {
  uiState.chips.scope = activeLetters(Object.fromEntries(letters.filter(k => !(k === "o" && uiState.chips.tools.length)).map(k => [k, true]))).join("");
}
/** Whether a marked text node's nearest row is in scope — the RECORD's kind through the shared
 *  class table (#118), the same gate the classic page applies to the block a hit sits in. This
 *  shell used to derive the mask from the row's classes and its DISPLAY name, so an Edit (shown
 *  as "Update") was counted under `e:` and then marked nowhere. */
function kindInScope(row, wanted) {
  if (!wanted) return true;
  const mask = directMask(row?.dataset.recordKind || "");
  if (!(mask & wanted)) return false;
  // `o(…)` narrows the tools class to the tools it names (#367).
  return !(mask & CLASS_BIT.o) || toolMatches(row?.dataset.searchTool || "", queryTools());
}
/** The tools the BOX names (#292, #367) — its `o(…)`, parsed by the shared grammar. The one source
 *  of truth: the menu writes the box, `updateSearch` reads it back, so a tick and a typed letter
 *  are the same thing (#101's rule, now for tools too). */
function queryTools() {
  return recordState.searchTools || [];
}
/** The name `o(…)` asks of a record: the tool it carries. The server writes the display
 *  name into both `tool` and `head.name` (an Edit reads "Update" in both), so either answers —
 *  `tool` first, because that is the field the classic page filters on. */
function toolNameOf(record) {
  return record.tool || record.head?.name || "";
}
/** What ↑/↓ steps (#292, design/in-session-search.md §4): the query's hits — which are ALREADY
 *  narrowed to the facets, since `countRecord` counts inside the chosen calls — else, with no
 *  text, the facets' own hits. One step list per query; a query no longer replaces the facet it
 *  was typed beside, which is what `activeMatches` used to do. */
function activeMatches() {
  if (recordState.search) return recordState.matches;
  return queryTools().length ? recordState.filterMatches || [] : [];
}
function stepSearch(delta) {
  // #s278: while the server's answer for the search or the filter is out, the hits here are only the
  // tail's — a step now would skip every earlier one; it steps once that answer lands (`askRemote`).
  const waitingOn = recordState.search ? remoteSearch : queryTools().length ? remoteFilter : null;
  if (waitingOn && !waitingOn.found && owedIndex()) { stepWhenFound = { delta, at: performance.now(), slot: waitingOn }; return; }
  stepWhenFound = null;
  const matches = activeMatches();
  if (!matches.length) return;
  const textual = !!recordState.search;
  const at = recordState.match >= 0 ? matches[recordState.match] : null;
  // Find the record by the NUMBER its attribute holds, not by string equality: a block index
  // reaches the attribute as the server wrote it, and "32.0" is not the selector "32".
  const current = textual
    ? viewport.window.querySelector("mark.search-mark.current")
    : (at != null ? [...viewport.window.querySelectorAll("[data-block-index]")].find(element => Number(element.dataset.blockIndex) === at) : null);
  const box = current?.getBoundingClientRect(), view = viewport.scroller.getBoundingClientRect();
  // #s257: a match under the box or the dock is not one the reader has seen; the step lands on it.
  // Unless the view is at its end and cannot lift it further (a phone's keyboard, a tall runtime
  // panel): counted unseen there, each step re-entered from the top and bounced between the last
  // two hits without ever wrapping.
  const onScreen = !!box && box.bottom >= view.top && (box.top < searchFloor(view) || box.top <= view.bottom && viewport.gapToBottom() <= 1);
  // Where the last step LANDED the reader, while they have not moved since (#s259): pressing next
  // where you already are must move on from there, whatever the rect says. The cursor alone cannot
  // say this: a fresh query points it at the first hit without taking the reader anywhere, and the
  // first step must land there. Re-entering from the top instead starts at the first hit of the
  // UNIT at the top, so under a cover over the whole column the steps fell back inside a unit
  // holding several hits. Only while that unit still crosses the view: not every move reaches the
  // engine as input (a key the browser scrolls with natively, the browser's own find), and a reader
  // who went elsewhere enters at the hit nearest them.
  const held = recordState.landed == null || !landedInView(view) ? -1 : matches.indexOf(recordState.landed);
  if (recordState.match >= 0 && onScreen) {
    recordState.match = (recordState.match + delta + matches.length) % matches.length;
  } else if (held >= 0) {
    recordState.match = (held + delta + matches.length) % matches.length;
  } else {
    const top = recordIndexAtTop();
    const k = matches.findIndex(index => index >= top);
    recordState.match = delta > 0 ? (k >= 0 ? k : 0) : (k > 0 ? k - 1 : matches.length - 1);
  }
  recordState.landed = matches[recordState.match];
  // A record a tail-first open has not brought yet is not reached (#314): nothing landed there.
  if (viewport.jumpToRecord(recordState.landed, textual ? "search" : "filter")) recordState.landedAt = performance.now();
  else {
    // #s278: a hit whose turn is still on its way reads that turn and lands once its unit is whole
    // (`landHeldJump`), unless the reader moves first.
    const unit = recordState.units[unitAt(recordState.landed)];
    if (unit?.owed > 0 || recordState.records[recordState.landed]?.kind === "pending") {
      if (unit) recordStore.want(unit.from, unit.to + 1, 0);
      heldTurnJump = { record: recordState.landed, reveal: textual ? "search" : "filter", at: performance.now() };
      toast("Still loading — the page goes there when it arrives");
    }
    recordState.landed = null;
  }
  if (!textual) return;
  markSearch();
  landOnCurrentMark();
}
/** Whether the record a step last landed still crosses `view` — covered or not: its own row, or its
 *  unit's where its row is not drawn (#s264: a reader who moved within a tall unit has left it). */
function landedInView(view) {
  const unit = recordState.units.find(u => recordState.landed >= u.from && recordState.landed <= u.to);
  const row = unit && unitRow(unit.key);
  const own = row && [...row.querySelectorAll("[data-block-index]")].find(element => Number(element.dataset.blockIndex) === recordState.landed && element.getBoundingClientRect().height > 0);
  const r = (own || row)?.getBoundingClientRect();
  return !!r && r.bottom > view.top && r.top < view.bottom;
}
/** A hit inside a head's one-line TARGET is unreachable until the target is shown in full
 *  (#262). The span is `nowrap` with `overflow-x:hidden` — measured 572px wide over a 44,782px
 *  command — so a match deep in the command sits thousands of pixels outside the box, and
 *  nothing the reader can do brings it in: there is no scrollbar and nobody writes its
 *  scrollLeft. The owner: "show 4 hits, but one of them I cannot see."
 *
 *  Full is the state the head's own third click step gives the target (#129), so a hit reveals
 *  it the way the reader would rather than inventing a fourth state. `headSteps` is left alone
 *  deliberately: the click handler derives its starting step from the CURRENT open/full pair
 *  when the map has no entry, so it stays coherent by itself.
 *
 *  Returns whether anything changed, because the caller has to re-render and re-mark before it
 *  can trust a rect. */
function revealTargetHolding(mark) {
  // No "is it clipped" test: the same reveal on the classic page measured the target BEFORE
  // its collapsed style had been applied, found it unclipped, and left the first hit of every
  // search 22,895px off screen. A target that needed no opening out looks the same either way.
  const target = mark.closest(".renderer-target");
  if (!target) return false;
  const id = target.closest("[data-record-id]")?.dataset.recordId;
  if (!id || !recordState.fullTargets || recordState.fullTargets.has(id)) return false;
  recordState.fullTargets.add(id);
  return true;
}
/** After a jump, put the current mark on screen if the record's head left it below the fold. */
function landOnCurrentMark() {
  let mark = viewport.window.querySelector("mark.search-mark.current");
  if (!mark) return;
  // Before any rect is read: a clipped target has to be opened out first, or the measurement
  // describes a box the mark is nowhere near.
  if (revealTargetHolding(mark)) {
    viewport.rerender();
    markSearch();
    mark = viewport.window.querySelector("mark.search-mark.current");
    if (!mark) return;
  }
  revealMermaidSource(mark);
  // #354: sideways first. A hit far along an unwrapped line of an output (a phone's default: `pre`
  // that scrolls on its own) sits past its block's right edge, and nothing below moves sideways —
  // the owner's export showed every step landing at the right offset of a 175,539 px agent process
  // with the match out of sight. The block's own scrollLeft, never the transcript's offset.
  revealSideways(mark);
  const box = mark.getBoundingClientRect(), view = viewport.scroller.getBoundingClientRect();
  // A match under whatever floats over the transcript's foot is not on screen (#s37, #s248, #s257),
  // nor one behind the turn bar at its top (#s263).
  const floor = searchFloor(view), ceiling = searchCeiling(view, mark);
  if (box.top >= ceiling && box.bottom <= floor) return;
  // A third of the space between them, so a short one still shows the match: measured from the
  // column's top, a cover within 93px of it put the match behind the bar. Never so far down that the
  // match crosses the cover (#s267: a third of a short space under a group's 40px head did), and a
  // cover over the whole column leaves none: the match lands just below the bar or the head.
  const space = Math.max(0, floor - ceiling);
  viewport.reveal(mark, { top: Math.min(120, ceiling - view.top + Math.min(space / 3, Math.max(0, space - box.height))) });
}
/** Where a landing may put `mark` at the transcript's top: below the turn bar while it is drawn
 *  (#123; hidden on a phone, where the turn is the top bar's subtitle), on or off, since a landing can
 *  turn it on (#s263); and below the head of the process group holding the mark (#s267), which sticks
 *  there whenever the group spans the top, as it does once a match deep inside it is revealed. Its
 *  height, not where it is now: the reveal is what makes it stick. On a phone held sideways with the
 *  keyboard up the space above the find pill is about 75px, and a third of it put the match behind
 *  the 40px head. Only the landing asks: a step's "already on screen" keeps the column's top, since a
 *  hit the reader scrolled up behind the bar is behind them, and counting it unseen sent the next
 *  step back to it. */
function searchCeiling(view, mark) {
  let ceiling = view.top;
  if (getComputedStyle(turnStickyBar).display !== "none") {
    const r = turnStickyBar.getBoundingClientRect();
    if (r.height > 0 && r.bottom > view.top) ceiling = r.bottom;
  }
  const head = mark?.closest("[data-process-surface]")?.querySelector(".process-surface-headbar");
  if (head && !head.contains(mark)) ceiling = Math.max(ceiling, view.top + head.getBoundingClientRect().height);
  return Math.min(view.bottom, ceiling);
}
/** Where a match stops being in sight at the transcript's foot: the bottom of `view`, or the top of
 *  whatever is drawn over it there — a phone's floating toolbar (#s37), an open compose box (#s245,
 *  #s248; native mode's is resident) and native mode's runtime panel above it (#s257), each when it
 *  is drawn, at any width. A step's "already on screen" and its landing both ask this, so a match
 *  under any of them is neither seen nor left there. */
function searchFloor(view) {
  let floor = view.bottom;
  const over = element => {
    if (!element || getComputedStyle(element).display === "none") return;
    const r = element.getBoundingClientRect();
    if (r.height > 0 && r.bottom > view.top) floor = Math.min(floor, Math.max(view.top, r.top));
  };
  over(phoneDock);
  const composer = byId("composer");
  if (composer.classList.contains("production-show")) over(composer);
  over(viewRoot.$(".shared-runtime"));
  return floor;
}
/** Scroll the nearest horizontally scrolling block holding `mark` so the mark sits in it, with a
 *  little of what precedes it — only when it is not already wholly visible there. */
function revealSideways(mark) {
  for (let block = mark.parentElement; block && block !== viewport.window; block = block.parentElement) {
    if (block.scrollWidth <= block.clientWidth + 1) continue;
    if (!/(auto|scroll)/.test(getComputedStyle(block).overflowX)) continue;
    const b = block.getBoundingClientRect(), m = mark.getBoundingClientRect();
    if (m.left >= b.left && m.right <= b.right) return;
    block.scrollLeft += m.left - b.left - Math.min(48, b.width / 4);
    return;
  }
}
function markSearch() {
  viewport.window.querySelectorAll("mark.search-mark").forEach(mark => mark.replaceWith(mark.textContent));
  viewport.window.normalize();
  if (!recordState.search) return;
  // Every hit in the mounted window is marked, the current record's first one stronger (#111,
  // the classic page's rule): a reader sees how many hits a screen holds, not one at a time.
  const needle = recordState.search, matched = new Set(recordState.matches), current = recordState.matches[recordState.match];
  const wanted = recordState.searchMask || 0, whole = !!recordState.searchWhole;
  for (const root of viewport.window.querySelectorAll("[data-block-index]")) {
    const index = Number(root.dataset.blockIndex);
    if (!Number.isInteger(index) || !matched.has(index) || root.parentElement?.closest("[data-block-index]")) continue;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT); const texts = [];
    while (walker.nextNode()) if (walker.currentNode.parentElement && !walker.currentNode.parentElement.closest(".mermaid-diagram") && !["SCRIPT", "STYLE", "MARK"].includes(walker.currentNode.parentElement.tagName) && walker.currentNode.nodeValue.toLowerCase().includes(needle) && (!wanted || kindInScope(walker.currentNode.parentElement.closest("[data-record-kind]"), wanted))) texts.push(walker.currentNode);
    let first = index === current;
    for (const text of texts) {
      const value = text.nodeValue, lower = value.toLowerCase(), frag = document.createDocumentFragment();
      let at = 0, from = 0;
      while ((at = lower.indexOf(needle, from)) !== -1) {
        if (whole && !wholeAt(lower, at, needle.length)) { from = at + needle.length; continue; }
        if (at > from) frag.appendChild(document.createTextNode(value.slice(from, at)));
        const mark = document.createElement("mark"); mark.className = "search-mark" + (first ? " current" : ""); mark.textContent = value.slice(at, at + needle.length); frag.appendChild(mark);
        first = false; from = at + needle.length;
      }
      if (from < value.length) frag.appendChild(document.createTextNode(value.slice(from)));
      text.replaceWith(frag);
    }
  }
}
/** Live search while the haystack is small; above the shared limit the box searches on Enter (#104). */
// `?liveSearchLimit=<chars>` lowers the limit for one page — a case's way onto the large-session
// path (#308) without a ten-megabyte fixture, in the manner of `?historyMs`.
const liveSearchLimit = Number(new URLSearchParams(location.search).get("liveSearchLimit")) || LIVE_SEARCH_LIMIT;
function searchIsLive() {
  // #s278: while a records-by-turn head is out the server answers for it — on Enter, never a round
  // trip per keystroke over a slow link (design/slow-links.md §7.3).
  if (recordStore.windowed && owedIndex()) return false;
  let n = 0; for (const s of recordState.recSizes) n += s; return n <= liveSearchLimit;
}
// #s278 (design/slow-links.md §7): search and the filter for the records below a records-by-turn
// head's boundary, asked of the server while the head is out, one answer per query (its words, class
// mask, whole words and tools, under the head's epoch and boundary). It quotes no text; it lives as
// long as the index does (`remoteSearch`, declared beside the other state a reset clears).
const REMOTE_TRIES = 5;
function remoteFound(q) {
  const head = recordStore.head, owed = owedIndex();
  if (!recordStore.windowed || !head || !owed) return { ready: true, found: null };
  const key = `${JSON.stringify(q)}@${head.epoch}/${owed.upto}`;
  const held = q.lc ? remoteSearch : remoteFilter;
  if (held?.key === key) return { ready: !!held.found, found: held.found };
  const mine = { key, found: null, tries: 0 };
  // A step waiting on the answer this one replaces (the same query read anew: a session letter that
  // moved to another tool) waits on this one instead.
  if (held && stepWhenFound?.slot === held) stepWhenFound.slot = mine;
  if (q.lc) remoteSearch = mine; else remoteFilter = mine;
  askRemote(mine, q, head, owed);
  return { ready: false, found: null };
}
/** One ask of `/search` for `mine`. A failed ask is asked again every few seconds — a 409 too: a
 *  session the server's cache let go answers it until the next pull brings it back, and one whose log
 *  really moved starts over, which drops this ask — and after five the page counts what it holds,
 *  saying the earlier turns were not searched. */
function askRemote(mine, q, head, owed) {
  // Not once the view is unmounted (stage 7): an answer then has no view to settle in.
  const current = () => !unmounted && (q.lc ? remoteSearch : remoteFilter) === mine;
  const settle = found => {
    mine.found = found;
    if (q.lc) updateSearch(false);
    else {
      refreshFilterHits();
      // The hits the answer brings can come in front of the one the reader landed on.
      if (!recordState.search && recordState.landed != null) { const at = activeMatches().indexOf(recordState.landed); if (at >= 0) recordState.match = at; }
    }
    // The step asked for while this answer was out, unless something new is typed or the reader
    // has moved since.
    const asked = stepWhenFound;
    if (!asked || asked.slot !== mine) return;
    stepWhenFound = null;
    if (!recordState.pendingSearch && viewport.lastUserInput <= asked.at) stepSearch(asked.delta);
  };
  mine.tries++;
  backend().fetch(`/search?session=${encodeURIComponent(recordState.session)}&epoch=${head.epoch}&upto=${owed.upto}&q=${encodeURIComponent(JSON.stringify(q))}`, { cache: "no-store" })
    .then(response => (response.ok ? response.json() : Promise.reject(response.status)))
    .then(found => { if (current()) settle(found); })
    .catch(() => {
      if (!current()) return;
      if (mine.tries >= REMOTE_TRIES) settle({ records: [], counts: {}, total: 0, failed: true });
      else setTimeout(() => { if (current()) askRemote(mine, q, head, owed); }, 3000);
    });
}
byId("transcriptSearchInput").oninput = () => {
  if (searchIsLive()) { recordState.pendingSearch = false; updateSearch(true); return; }
  recordState.pendingSearch = true;
  byId("transcriptSearchCount").textContent = byId("transcriptSearchInput").value.trim() ? "⏎ to search" : "";
};
byId("transcriptSearchInput").onkeydown = event => {
  // Escape leaves the box as a click outside would (#298, the owner; the classic page's `q.blur()`):
  // the query, its count and its marks stay, and the next key is the page's keymap's again. An open
  // filter popover closes with it, as the page-wide Escape closes it.
  if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); setPopover(null); event.currentTarget.blur(); setPhoneSearch(false); return; }
  // An Enter that commits an input method's composition (pinyin, kana) is the input method's; Safari
  // sends it with `isComposing` false and keyCode 229.
  if (event.key !== "Enter" || event.isComposing || event.keyCode === 229) return;
  event.preventDefault(); event.stopPropagation();
  // A large session searches on Enter (#104): that Enter also MOVES — to the nearest hit below the
  // reader (above, with Shift), wrapping — rather than only counting (#308, the owner).
  if (recordState.pendingSearch) { recordState.pendingSearch = false; updateSearch(true); }
  stepSearch(event.shiftKey ? -1 : 1);
  // #s287: a phone held sideways in Safari with its tab bar keeps about 16px of the page above the
  // keyboard (iOS 26.4: Safari's two bars take 110pt of 402, the keyboard with its candidate and
  // accessory bars 276), so the find pill, lifted to that strip's foot, sits under Safari's bars and
  // the hit just landed is out of sight; the Home Screen app held sideways keeps 126px, where the pill
  // fits and no line of the transcript does above it. There Return also puts the keyboard down, and
  // the reader sees the hit, the count and the steps; a tap on the field brings it back. Wherever a
  // line shows above the pill, the keyboard stays.
  if (phoneSearch.matches && noLineAboveFindPill()) event.currentTarget.blur();
};
/** Whether the keyboard leaves no room for a line of the transcript between the column's top and the
 *  find pill: a hit landed there cannot be seen. A line below a process group's sticky head (40px,
 *  `searchCeiling`) needs about 60px. Both are inside the app, which is moved back over iOS's keyboard
 *  scroll, so their difference is what the reader has. */
function noLineAboveFindPill() {
  const visible = window.visualViewport;
  if (!visible || !phoneDock || Math.abs(visible.scale - 1) >= 0.01) return false;
  if (visible.height >= page.html().clientHeight) return false;
  return phoneDock.getBoundingClientRect().top - viewport.scroller.getBoundingClientRect().top < 64;
}
// Whole words: production-only chrome, layered beside the box at runtime so the extracted demo
// shell stays exact (#293 — the same way the shell switch is added). `w:` keeps working typed.
// It sits after ↑ ↓ and before the filter button (owner, #296): stepping comes first, then the
// match options, then the facets.
const wholeWords = document.createElement("button");
wholeWords.className = "find-nav find-whole"; wholeWords.id = "transcriptWholeWords"; wholeWords.type = "button";
wholeWords.title = "Whole words only  ·  w:"; wholeWords.setAttribute("aria-label", "Whole words only");
wholeWords.setAttribute("aria-pressed", "false");
wholeWords.textContent = "ab|";
byId("filterTranscriptBtn").insertAdjacentElement("beforebegin", wholeWords);
// #353: the step arrows are drawn, not typed — a text "↑" sat visibly high in its 44px target on a
// phone, where an SVG's box is its ink's (arrowUp spans y 5..19 of 24). Next is the same, turned.
byId("findPrev").innerHTML = svg("arrowUp"); byId("findNext").innerHTML = svg("arrowUp");
// #352: one tap clears the search — the typed text, every chip and the results, the box as fresh
// (the owner: "no easy way to clear a search"; a chip's own × was the only way out, and on a phone
// the chip had been clipped with it). At the end of the field, as a native search field has it,
// whenever the box holds a query (`has-query`, markQuery). Production-only chrome, like whole-words.
const searchClear = document.createElement("button");
searchClear.className = "search-clear"; searchClear.id = "transcriptSearchClear"; searchClear.type = "button";
searchClear.title = "Clear the search"; searchClear.setAttribute("aria-label", "Clear the search");
searchClear.innerHTML = svg("x");
byId("transcriptSearchInput").insertAdjacentElement("afterend", searchClear);
searchClear.addEventListener("click", () => {
  const input = byId("transcriptSearchInput");
  // #s37: the ✕ clears to type again and keeps the field — in an open box. Done ends a phone's search
  // (the dock) and clears through here without raising the keyboard.
  const refocus = !phoneSearch.matches || searchBox.classList.contains("phone-open");
  input.value = "";
  uiState.chips = { scope: "", tools: [] };
  recordState.pendingSearch = false;
  updateSearch(true);
  if (searchSuggest) searchSuggest.hidden = true;
  suggestState = null;
  if (refocus) input.focus();
});

// ---- The search box's prefix (#303, #367, design/in-session-search.md §8) ---------------------------
// The box may start with a prefix — scope letters, `o(…)` naming tools — then a colon. Typing `o(`
// opens a drop-down of the session's tools with their LETTERS (`o(BaA)`); once something follows the
// prefix's colon it freezes into chips at the box's left: deletable (their ×, or Backspace at the
// start of the text), not editable. A chip is the prefix it was, so `updateSearch` parses chips and
// text together (`boxQuery`); the filter popover sets the chips directly, and the chips tick its
// rows. `var`: updateSearch can run during module evaluation, before this block.
var searchChips = null, searchSuggest = null, suggestState = null;
searchChips = document.createElement("span");
searchChips.className = "search-chips"; searchChips.id = "searchChips";
byId("transcriptSearchInput").insertAdjacentElement("beforebegin", searchChips);
// #353: the chips and the field are ONE strip that scrolls as a whole. On a phone the open box is
// narrow, and two chips left the field its 48px floor to scroll its own text in ("hing" for
// "nothing"); now the field is as wide as its text and the strip slides left, chips and all, so the
// end being typed stays in view. Wrapped here, at module load — before anything can focus the input,
// since moving a focused element blurs it — and measured by a hidden twin of the field's font.
var searchField = document.createElement("span");
searchField.className = "search-field"; searchField.id = "searchField";
searchChips.insertAdjacentElement("beforebegin", searchField);
searchField.append(searchChips, byId("transcriptSearchInput"));
var fieldMeasure = document.createElement("span");
fieldMeasure.className = "search-field-measure"; fieldMeasure.setAttribute("aria-hidden", "true");
viewRoot.$(".header-search-cluster").appendChild(fieldMeasure);
searchSuggest = document.createElement("div");
searchSuggest.className = "search-suggest"; searchSuggest.id = "searchSuggest"; searchSuggest.hidden = true;
searchSuggest.setAttribute("role", "listbox");
viewRoot.$(".header-search-cluster").appendChild(searchSuggest);
/** Size the field to its text while the phone's box is open, and keep the end in view (#353). */
function fitSearchField() {
  if (!searchField || !fieldMeasure) return;
  const input = byId("transcriptSearchInput");
  if (!viewRoot.$(".header-searchbox")?.classList.contains("phone-open")) { input.style.width = ""; return; }
  const cs = getComputedStyle(input);
  fieldMeasure.style.font = cs.font; fieldMeasure.style.letterSpacing = cs.letterSpacing;
  // With chips the placeholder is not shown (#355), so an empty field takes no room of its own.
  fieldMeasure.textContent = input.value || (searchChips.classList.contains("has-chips") ? "" : input.placeholder);
  const text = Math.ceil(fieldMeasure.getBoundingClientRect().width) + 8;
  const chips = searchChips.classList.contains("has-chips") ? searchChips.getBoundingClientRect().width + 4 : 0;
  input.style.width = `${Math.max(48, Math.floor(searchField.clientWidth - chips), text)}px`;
  if (input.selectionStart === input.value.length) searchField.scrollLeft = searchField.scrollWidth;
}
addEventListener("resize", () => fitSearchField(), { signal });
// #310, #313: on a phone (the view's phone family, `PHONE_QUERY` read from its own classes, #s431; #s32 a landscape phone too) the search box is an icon with its
// input hidden. A tap on it OPENS it across the bar's second row, and it stays open until a tap lands outside the search's own
// surfaces (the box, the drop-down, the filter popover) or Escape — never on blur, since a touch
// browser does not focus a tapped button and the steps would vanish under the finger. The query,
// its count and its marks outlive the box closing, as they outlive a blur on a wider window.
const phoneSearch = layout.query("phone"), searchBox = viewRoot.$(".header-searchbox");
function setPhoneSearch(open) {
  if (!open && byId("navigatorOptions").classList.contains("open")) setPopover(null);
  searchBox.classList.toggle("phone-open", open && phoneSearch.matches);
  fitSearchField();
  if (open && phoneSearch.matches) byId("transcriptSearchInput").focus();
}
searchBox.addEventListener("click", event => {
  // #333: with a query the closed box keeps its arrows, which step through the matches in place.
  if (!phoneSearch.matches || searchBox.classList.contains("phone-open") || event.target.closest("#filterTranscriptBtn, .find-nav")) return;
  setPhoneSearch(true);
});
// A tap outside dismisses the TOP layer only (#s49, the owner: "after opening and closing the filter
// menu, the layout of the search pill changed"): with the filter open it closes the filter and leaves
// the box as it was; the next one closes the box.
document.addEventListener("pointerdown", event => {
  if (!searchBox.classList.contains("phone-open") || event.target.closest(".header-search-cluster")) return;
  if (phoneSearch.matches && byId("navigatorOptions").classList.contains("open")) { setPopover(null); return; }
  setPhoneSearch(false);
}, { capture: true, signal });
phoneSearch.addEventListener("change", () => { if (!phoneSearch.matches) setPhoneSearch(false); }, { signal });
byId("transcriptSearchInput").addEventListener("focus", () => { if (phoneSearch.matches && byId("navigatorOptions").classList.contains("open")) setPopover(null); });
const SCOPE_ROWS = [["u", "User messages"], ["a", "Agent replies"], ["t", "Thinking"], ["o", "Tools"], ["w", "Whole words"]];
// #367: the session's tools are secondary checkboxes under the Tools scope row — the last row of the
// scope list — not a section of their own. The reference shell draws them as one; production moves
// the list up to follow the scope rows and hides the section's divider and heading (unticking Tools
// clears its tools; All clears them too), so the extracted shell stays exact.
{
  const list = byId("filterOptions"), scopeList = byId("scopeRow");
  list.previousElementSibling?.classList.add("tool-types-head-retired");
  scopeList.nextElementSibling?.classList.contains("scope-menu-divider") && scopeList.nextElementSibling.classList.add("tool-types-head-retired");
  scopeList.insertAdjacentElement("afterend", list);
  list.classList.add("tool-type-nested");
}

/** The chips as the prefix they were, for the parser: the scope letters, and `o(…)` for the tools
 *  — by their letters when every one has one in this session, else by name. */
function chipTokens() {
  const prefix = prefixOf([...uiState.chips.scope], toolChipSpec());
  return prefix ? [prefix] : [];
}
function toolChipSpec() {
  const tools = uiState.chips.tools;
  if (!tools.length) return "";
  const letterOf = new Map(toolLetters().map(e => [e.key, e.letter]));
  return tools.every(k => letterOf.has(k)) ? tools.map(k => letterOf.get(k)).join("") : tools.join(",");
}
/** What `updateSearch` parses: the chips, then the typed text. */
function boxQuery() {
  const input = byId("transcriptSearchInput");
  return [...chipTokens(), input ? input.value : ""].join(" ").trim();
}
/** The session's tools, each with its LETTER — the shared table (#367: the common tools' letters are
 *  reserved and never change; the rest depend on the session). */
function toolLetters() {
  return sessionToolLetters(sessionToolCounts());
}
/** The turn index while the records it answers for are still placeholders in the array the page
 *  scans (#s276), else null — so nothing is counted twice. Not the units: while a landing is held
 *  (#340) the store has already written the head into that array and the units still show it
 *  pending. */
function owedIndex() {
  // Stage 2 (#s277) fills the head in any order, so the index stands while the head is out, and the
  // scans beside it read only the records at or above its boundary (`scanFrom`).
  if (recordStore.windowed) return recordStore.head ? recordState.headIndex : null;
  return recordState.records[0]?.kind === "pending" ? recordState.headIndex : null;
}
/** Where the scans beside the index start: its boundary while it answers for a head filled in any
 *  order (#s277, design/slow-links.md §6.5), else 0 — never both for one record. */
function scanFrom() {
  const owed = owedIndex();
  return owed && recordStore.windowed ? owed.upto : 0;
}
/** How many calls of each tool the session holds: the records here, and the index's count of the
 *  ones still coming (#s276), so the letters and counts are the whole session's before it lands. */
function sessionToolCounts() {
  const from = scanFrom();
  const counts = toolCounts(from ? recordState.records.slice(from) : recordState.records);
  for (const [name, n] of Object.entries(owedIndex()?.tools || {})) counts[name] = (counts[name] || 0) + n;
  return counts;
}
/** An `o(…)` text → the tool KEYS it means (names, `mcp__<server>__*` for a family). */
function resolveTools(value) {
  return resolveToolSpec(value, toolLetters()).map(t => t.name + (t.prefix ? "*" : ""));
}
function renderChips() {
  if (!searchChips) return;
  const scope = uiState.chips.scope, tools = uiState.chips.tools;
  const label = key => key.endsWith("*") ? key.replace(/^mcp__|__\*$/g, "") + "/*" : key;
  // #352: the key is a GLYPH — the filter button's for a scope, a wrench for tools — so a chip fits a
  // phone's box (the word clipped it to "scop" and took its × with it). The word stays in the chip,
  // visually hidden, for a screen reader and for whatever reads the chip as the token it was.
  // #353: a scope is already single-character selectors (u a t o w), so its chip is the letters
  // alone; a tool chip keeps its wrench, which is what tells the two apart — and is the `o(…)`.
  const chip = (kind, text, title) => { const word = kind === "tools" ? "tool" : kind; return `<span class="search-chip" data-chip="${kind}" title="${escapeText(title)}"><span class="search-chip-key">${kind === "tools" ? svg("tool") : ""}<span class="search-chip-word">${word}:</span></span><span class="search-chip-value">${escapeText(text)}</span><button class="search-chip-remove" type="button" data-chip-remove="${kind}" aria-label="Remove the ${word} filter">×</button></span>`; };
  // #355: a tool chip is the wrench and the tools' own single-character selectors — the letters
  // `o(…)` takes and the drop-down lists — with the names in its title; a key with no letter (none
  // in this session) keeps its name.
  const letterOf = new Map(tools.length ? toolLetters().map(e => [e.key, e.letter]) : []);
  const toolText = tools.every(k => letterOf.has(k)) ? tools.map(k => letterOf.get(k)).join("") : tools.map(k => letterOf.get(k) || label(k)).join(", ");
  searchChips.innerHTML = (scope ? chip("scope", scope, SCOPE_ROWS.filter(([k]) => scope.includes(k)).map(([, l]) => l).join(", ")) : "")
    + (tools.length ? chip("tools", toolText, tools.join(", ")) : "");
  searchChips.classList.toggle("has-chips", !!(scope || tools.length));
  fitSearchField();
}
/** Freeze the typed prefix into chips once something follows its colon, and take it out of the
 *  text: its letters join the scope chip, its `o(…)` tools the tools chip. */
function absorbFacets() {
  const input = byId("transcriptSearchInput");
  const value = input.value;
  const span = querySpans(value).find(x => x.kind === "scope");
  if (!span || value.length <= span.end) return false;
  const tools = span.tools ? resolveTools(span.tools) : [];
  uiState.chips.tools = [...new Set([...uiState.chips.tools, ...tools])];
  setScopeChip([...uiState.chips.scope, ...activeLetters(span.set)]);
  const caret = input.selectionStart === value.length;
  input.value = value.slice(span.end).replace(/^\s+/, "");
  if (caret) input.setSelectionRange(input.value.length, input.value.length);
  return true;
}
/** The `o(…)` the caret is in, when the typed text starts with a prefix still being written:
 *  `{start, end, value}` of the text between the parentheses. */
function tokenAtCaret() {
  const input = byId("transcriptSearchInput");
  const value = input.value, at = input.selectionStart ?? value.length;
  const m = /^([uatw]*o\()([^()\s:]*)/i.exec(value);
  if (!m || !/^[uatw]*$/i.test(m[1].slice(0, -2))) return null;
  const start = m[1].length, end = start + m[2].length;
  if (at < start || at > end) return null;
  return { kind: "tools", start, end, value: m[2] };
}
function suggestRows() {
  return toolLetters().map(e => ({ letter: e.letter, label: e.family ? `${e.label}/*` : e.label, count: e.count, key: e.key }));
}
function renderSuggest() {
  if (!searchSuggest) return;
  const tok = viewRoot.active() === byId("transcriptSearchInput") ? tokenAtCaret() : null;
  if (!tok) { searchSuggest.hidden = true; suggestState = null; return; }
  const chosen = new Set(resolveTools(tok.value));
  const rows = suggestRows();
  const active = suggestState ? Math.min(suggestState.active, rows.length - 1) : 0;
  suggestState = { kind: tok.kind, rows, active };
  searchSuggest.innerHTML = '<div class="search-suggest-head">Tools — type letters<span>then ): and your words</span></div>'
    + (rows.map((r, i) => {
      const on = chosen.has(r.key);
      return `<button class="search-suggest-row${on ? " on" : ""}${i === active ? " active" : ""}" type="button" role="option" aria-selected="${on}" data-suggest="${i}"><span class="search-suggest-letter">${escapeText(r.letter)}</span><span class="scope-check"></span><span class="search-suggest-label">${escapeText(r.label)}</span><span class="search-suggest-count">${r.count ?? ""}</span></button>`;
    }).join("") || '<div class="search-suggest-empty">No tool calls in this session</div>');
  searchSuggest.hidden = false;
}
/** Add or remove one row's letter inside the `o(…)` at the caret. */
function toggleSuggest(index) {
  const tok = tokenAtCaret(); if (!tok || !suggestState) return;
  const row = suggestState.rows[index]; if (!row) return;
  const input = byId("transcriptSearchInput");
  let value = tok.value;
  // A value typed by NAME starts over as letters once a row is toggled, so the two never mix.
  const letters = new Set(toolLetters().map(e => e.letter));
  if (![...value].every(c => letters.has(c))) value = "";
  value = value.includes(row.letter) ? value.replace(row.letter, "") : value + row.letter;
  input.value = input.value.slice(0, tok.start) + value + input.value.slice(tok.end);
  const caret = tok.start + value.length;
  input.setSelectionRange(caret, caret);
  suggestState.active = index;
  renderSuggest();
  afterFacetChange();
}
searchChips.addEventListener("click", event => {
  const remove = event.target.closest("[data-chip-remove]"); if (!remove) return;
  if (remove.dataset.chipRemove === "scope") uiState.chips.scope = ""; else uiState.chips.tools = [];
  updateSearch(true);
  byId("transcriptSearchInput").focus();
});
searchSuggest.addEventListener("mousedown", event => event.preventDefault());
searchSuggest.addEventListener("click", event => { const row = event.target.closest("[data-suggest]"); if (row) toggleSuggest(Number(row.dataset.suggest)); });
// A commit re-runs the search only where the box searches live; a large session waits for Enter
// (#104), and the chip alone shows at once.
function afterFacetChange() {
  if (searchIsLive()) updateSearch(true);
  else { recordState.pendingSearch = true; renderChips(); byId("transcriptSearchCount").textContent = "⏎ to search"; }
}
byId("transcriptSearchInput").addEventListener("input", () => { if (absorbFacets()) afterFacetChange(); renderSuggest(); markQuery(); fitSearchField(); });
for (const kind of ["click", "keyup", "focus"]) byId("transcriptSearchInput").addEventListener(kind, event => { if (kind !== "keyup" || !["ArrowDown", "ArrowUp", "Enter", "Tab", "Escape"].includes(event.key)) renderSuggest(); });
byId("transcriptSearchInput").addEventListener("blur", () => { if (searchSuggest) searchSuggest.hidden = true; suggestState = null; });
// The drop-down's keys, ahead of the box's own (Escape closes it before it blurs the box, #298).
byId("transcriptSearchInput").addEventListener("keydown", event => {
  const input = event.currentTarget;
  if (event.key === "Backspace" && input.selectionStart === 0 && input.selectionEnd === 0 && (uiState.chips.tools.length || uiState.chips.scope)) {
    event.preventDefault();
    if (uiState.chips.tools.length) uiState.chips.tools = []; else uiState.chips.scope = "";
    updateSearch(true); return;
  }
  if (!suggestState || searchSuggest.hidden) return;
  const n = suggestState.rows.length;
  if ((event.key === "ArrowDown" || event.key === "ArrowUp") && n) {
    event.preventDefault(); event.stopImmediatePropagation();
    suggestState.active = (suggestState.active + (event.key === "ArrowDown" ? 1 : n - 1)) % n;
    renderSuggest();
  } else if ((event.key === "Enter" || event.key === "Tab") && n) {
    event.preventDefault(); event.stopImmediatePropagation();
    toggleSuggest(suggestState.active);
  } else if (event.key === "Escape") {
    event.preventDefault(); event.stopImmediatePropagation();
    searchSuggest.hidden = true; suggestState = null;
  }
}, true);
wholeWords.onclick = () => { uiState.searchWhole = !uiState.searchWhole; applyScopeFromMenu(); renderFilterMenu(); };
byId("findNext").onclick = () => stepFromBox(1); byId("findPrev").onclick = () => stepFromBox(-1);
/** A step from the box's arrows or keys: text typed and not yet entered is entered first, as Enter
 *  enters it (#s278) — stepping the last query's hits under new text read as stepping the new one,
 *  and over a slow link those hits were the tail's alone. */
function stepFromBox(delta) {
  if (recordState.pendingSearch) updateSearch(true);
  stepSearch(delta);
}

function toolNames(records, into = []) {
  for (const record of records || []) {
    // ONE tool identity with the prefix (#292): what `toolNameOf` reads is what `o(…)` asks for,
    // so a ticked row and a typed letter can never mean different things.
    if (["bash", "read", "write", "edit", "skill", "tool"].includes(record.kind)) into.push(toolNameOf(record) || record.kind);
    for (const part of record.body || []) if (part.p === "blocks") toolNames(part.items, into);
  }
  return into;
}
/** How many calls of each tool the session holds — the menu's counts (#293), from the RECORDS and
 *  their nested items, never the DOM (which holds only the mounted window). */
function toolCounts(records) {
  const counts = Object.create(null);
  for (const name of toolNames(records)) counts[name] = (counts[name] || 0) + 1;
  return counts;
}
/** A row's tool KEY: a name, or `name*` for a family (#292). */
function facetToken(select) {
  return select.toolPre ? `${select.toolPre}*` : select.tool;
}
function renderFilterMenu() {
  // #367: Tools is one scope row, and the session's tools are checkboxes nested under it (the
  // list below, moved there at load) — so a tool is only ever chosen inside Tools and no choice
  // here can contradict another. The hint reads as the prefix a reader would type (`u:`), not as
  // a bare letter that looks like a keyboard shortcut (#293).
  const scopes = [["u", "User messages"], ["a", "Agent replies"], ["t", "Thinking"], ["o", "Tools"]];
  byId("scopeRow").innerHTML = scopes.map(([key, label]) => `<button class="scope-option ${uiState.searchScopes.has(key) ? "on" : ""}" data-scope="${key}"><span class="scope-check"></span><span>${label}</span><span class="scope-count" data-scope-count="${key}"></span><span class="scope-key">${key === "o" && uiState.toolFilters.size ? "o(…):" : `${key}:`}</span></button>`).join("");
  // Whole words is a MATCH OPTION, not a part of the transcript, so it sits beside the box with
  // the other match controls (#293) instead of below the scope classes it is not one of.
  wholeWords.classList.toggle("on", !!uiState.searchWhole);
  wholeWords.setAttribute("aria-pressed", uiState.searchWhole ? "true" : "false");
  // The section's SHAPE is the shared rule (#293, shared/filter.js toolTree): every tool in the
  // session with its count — nested ones included, since a call sits inside an activity record and
  // a top-level scan listed nothing (#110) — and MCP calls collapsed into one expandable family
  // instead of one row per `mcp__server__tool`, which is what a session with dozens of them needs.
  // Most-used first here; the classic page keeps its alphabetical order.
  const groups = toolTree(sessionToolCounts(), uiState.toolTreeOpen, "count");
  const rows = groups.flatMap(g => g.rows);
  const letterOf = new Map(toolLetters().map(e => [e.key, e.letter]));
  byId("filterOptions").innerHTML = rows
    .map(r => {
      const token = facetToken(r.select);
      const on = uiState.toolFilters.has(token) ? " on" : "";
      const sub = r.depth ? ` tool-sub${r.depth}` : "";
      // Two slots on every row (#297): the twisty, which EXPANDS and never selects (the handler
      // checks it first) — empty on a leaf, so the ticks line up — and the tick the Scope rows use,
      // which selects. A branch row carries both. The bullet and its tints went with it: the indent
      // and the twisty already say which rows are servers and which are their tools.
      const twisty = r.twisty
        ? `<span class="tool-tw" data-tw="${escapeText(r.twisty)}">${uiState.toolTreeOpen.has(r.twisty) ? "▾" : "▸"}</span>`
        : '<span class="tool-tw-slot"></span>';
      const bullet = `${twisty}<span class="scope-check"></span>`;
      // The full name as the tooltip: an MCP row reads `server/tool`, and the reader may still
      // need the name a script would use.
      const title = r.select.tool || `${r.select.toolPre}*`;
      // The letter `o(…)` takes for this tool (#367), where the session gives it one.
      const letter = letterOf.get(token);
      return `<button class="tool-type-option${on}${sub}" role="menuitemcheckbox" aria-checked="${on ? "true" : "false"}" data-tool-filter="${escapeText(token)}" data-label="${escapeText(r.label)}" data-depth="${r.depth}" title="${escapeText(title)}">${bullet}<span>${escapeText(r.label)}</span><span class="count">${r.count}</span><span class="scope-key">${letter ? escapeText(letter) : ""}</span></button>`;
    })
    .join("") || '<div class="tool-type-empty">No tool events in this session</div>';
  // The badge counts every active facet, tools AND classes (#293): a scope narrows the results, so
  // a button that read "inactive" while `u:` was on was telling the reader the opposite of the
  // truth. `searchWhole` is a match option, not a facet, and is not counted.
  const classes = ALL_SCOPES.every(k => uiState.searchScopes.has(k)) ? 0 : [...uiState.searchScopes].filter(k => !(k === "o" && uiState.toolFilters.size)).length;
  byId("filterBadge").textContent = uiState.toolFilters.size + classes || "";
  // #s49, the owner: the funnel says when a filter is on, and nothing else (not whether its menu is open).
  byId("filterTranscriptBtn").classList.toggle("on", uiState.toolFilters.size + classes > 0);
}
/** Tick or untick one tool: write the BOX and let `updateSearch` do the rest (#292). Nothing sets
 *  the tool state directly any more — the box is the query. A tool is chosen inside Tools (#367):
 *  ticking one with no scope at all searches the tools, and only them. */
function toggleToolFacet(name) {
  const next = new Set(uiState.toolFilters);
  next.has(name) ? next.delete(name) : next.add(name);
  setToolFacets([...next]);
}
function setToolFacets(names) {
  // #303: the menu sets the tools CHIP (the typed text keeps no prefix of its own).
  const input = byId("transcriptSearchInput");
  input.value = writePrefix(input.value, []);
  uiState.chips.tools = names.slice();
  setScopeChip([...uiState.chips.scope]);
  updateSearch(true);
}
function applyFilters() {
  const hits = recordState.filterHits;
  // Which scope classes each row belongs to, through the SHARED table (#126) — the last
  // hand-written kind map on this shell, and one that read a row's DISPLAY name where the record's
  // own kind is what the classes are defined on. A row no class claims (an agent event, an
  // attachment, a queued prompt) is never dimmed: no scope excludes it.
  const rows = [...viewport.window.querySelectorAll("[data-kind]")];
  const outOfScope = element => {
    const mask = directMask(element.dataset.recordKind || "");
    return !!mask && (![...uiState.searchScopes].some(letter => mask & CLASS_BIT[letter])
      || (mask & CLASS_BIT.o && !toolMatches(element.dataset.searchTool || "", queryTools())));
  };
  const out = new Map(rows.map(element => [element, outOfScope(element)]));
  // #369: a row that HOLDS an in-scope row is never dimmed. An activity span is a thinking row
  // whose children are the tool calls the engine coalesced into it; dimming the span — an opacity
  // no child can undo — dimmed the Bash calls `o(B)` had just matched inside it.
  const holds = new Set();
  for (const element of rows) {
    if (out.get(element)) continue;
    for (let up = element.parentElement?.closest("[data-kind]"); up && !holds.has(up); up = up.parentElement?.closest("[data-kind]")) holds.add(up);
  }
  for (const element of rows) element.classList.toggle("filter-dim", out.get(element) && !holds.has(element));
  // The tool filter is a SEARCH BY KIND on this shell (#133), not the classic page's cut: it
  // finds the calls of a kind and marks them, and the reader steps between them the way they
  // step search hits — with every hit still sitting in its own surroundings. Hiding everything
  // else takes the context away at exactly the moment the reader found what they were looking
  // for, and it is what made a filtered window a different beast for the virtual window (the
  // heights of hidden records, the sparse mount). Nothing is hidden here; nothing changes
  // height; the window stays dense.
  for (const element of viewport.window.querySelectorAll(".filter-hidden, .filter-hit")) element.classList.remove("filter-hidden", "filter-hit");
  if (!hits) return;
  // `.renderer[data-record-id]`, not every record root: a prose turn carries its id too now
  // (#174 — the classic page has stamped every mounted record since `matBlock`, and a record the
  // DOM cannot name is a record no audit can address), and this paints a TOOL head.
  for (const renderer of viewport.window.querySelectorAll(".renderer[data-record-id]")) {
    if (recordState.filterDirect?.has(renderer.dataset.recordId)) renderer.querySelector(":scope > .renderer-head")?.classList.add("filter-hit");
  }
}
// Does this record — or a record nested in it — carry one of the selected tools? Every record
// on such a chain is a hit (its fold opens); the direct ones are accented.
const toolKindsForFilter = new Set(["bash", "read", "write", "edit", "skill", "tool"]);
// The chain — a record matches, or something under it does — is the shared walk (#118,
// shared/filter.js chainWalk); what MATCHES, and what a match opens, stay this shell's own.
function filterChain(record, wanted, hits, direct) {
  return chainWalk(
    record,
    rec => toolKindsForFilter.has(rec.kind) && toolMatches(toolNameOf(rec), wanted),
    (rec, own) => {
      if (!rec.id) return;
      if (own) direct.add(rec.id);
      hits.add(rec.id);
    },
  );
}
/** Which records answer the active filter, and open what it takes to see them. Returns their
 *  indices. Records that are ALREADY on the chain keep whatever the reader has done to them
 *  since — only what is newly on it is opened, so a live session does not re-open a fold the
 *  reader closed a moment ago (#126). */
function computeFilterHits() {
  // The tools `o(…)` names (#367): a tool is only ever chosen inside Tools, so no scope can rule
  // one out — the owner's "1577 matches", none visible, cannot be built any more (#356).
  const wanted = queryTools();
  const before = recordState.filterHits;
  // Every record id this filter has opened, kept beyond the hits of the records here (#s285): a turn
  // let go and read again brings its ids back, which are not new, so a fold the reader closed stays
  // closed and nothing is redrawn for it. A fresh filter starts it again, and other tools start it
  // from the hits before them, as they always did.
  const tools = JSON.stringify(wanted);
  const seen = before && recordState.filterSeen?.tools === tools ? recordState.filterSeen.ids : new Set(before || []);
  recordState.filterSeen = { tools, ids: seen };
  const hits = new Set(), direct = new Set(), indices = [];
  // An EMPTY list means "every tool" to the shared matcher, so no tools must match nothing here.
  if (wanted.length) recordState.records.forEach((record, index) => { if (filterChain(record, wanted, hits, direct)) indices.push(index); });
  // #s278: a record still on its way answers the filter when the server says it holds such a call;
  // its folds open when it arrives (design/slow-links.md §7.3). Asked with text up too, so the
  // filter is whole the moment the text goes.
  const remote = wanted.length ? remoteFound({ lc: "", whole: false, wanted: 0, tools: wanted }) : null;
  if (remote?.found) {
    for (const [r] of remote.found.records) if (recordState.records[r]?.kind === "pending") indices.push(r);
    indices.sort((x, y) => x - y);
  }
  recordState.filterHits = hits;
  recordState.filterDirect = direct;
  recordState.filterMatches = indices;
  // Whether this pass OPENED anything. A record that arrives under a filter must be shown the
  // same way as one that was there when it was applied — and opening a fold is a change to what
  // is rendered, not to what is painted, so the caller has to re-render for it to be seen.
  let opened = false;
  for (const id of hits) if (!seen.has(id)) { seen.add(id); recordState.folds.set(id, false); opened = true; }
  for (const unit of projection.units) {
    if (unit.type !== "process" || recordState.processExpanded.has(unit.key)) continue;
    if (indices.some(index => index >= unit.from && index <= unit.to)) { recordState.processFolds.set(unit.key, false); recordState.processExpanded.add(unit.key); opened = true; }
  }
  recordState.filterOpened = opened;
  return indices;
}

/** Records arrived while a filter is on (#126). Without this they are in no hit set, so
 *  `applyFilters` hides every one of them and the transcript silently stops growing. The
 *  snapshot is NOT retaken — it holds the pre-filter fold state — and the viewport does not
 *  jump: nobody asked to be moved. */
function refreshFilterHits() {
  if (!queryTools().length || !recordState.filterHits) return;
  computeFilterHits();
  // A fold the filter just opened is only in the DOM once the window is rebuilt; `render` holds
  // the reader's place while it does that, so an arrival never moves them.
  if (recordState.filterOpened) viewport.rerender();
  if (!recordState.search) paintMatchCount("");
  applyFilters();
}

function applyToolFilter() {
  const wanted = queryTools();
  if (wanted.length) {
    if (!recordState.filterSnapshot) recordState.filterSnapshot = { folds: new Map(recordState.folds), processFolds: new Map(recordState.processFolds), processExpanded: new Set(recordState.processExpanded) };
    const indices = computeFilterHits();
    viewport.rerender();
    viewport.remeasure();
    // Land on the nearest hit so the filter visibly did something — and that hit becomes where
    // ↑/↓ step from, since a filter is a search by kind here (#133).
    const start = recordIndexAtTop();
    const target = indices.find(index => index >= start) ?? indices[0];
    recordState.match = target == null ? -1 : indices.indexOf(target);
    recordState.landed = target != null && viewport.jumpToRecord(target, "filter") ? target : null;
    recordState.landedAt = performance.now();
  } else if (recordState.filterHits) {
    const snapshot = recordState.filterSnapshot;
    recordState.filterHits = null; recordState.filterDirect = null; recordState.filterSnapshot = null; recordState.filterMatches = null;
    recordState.match = -1; recordState.landed = null;
    if (snapshot) { recordState.folds = snapshot.folds; recordState.processFolds = snapshot.processFolds; recordState.processExpanded = snapshot.processExpanded; }
    viewport.rerender();
    viewport.remeasure();
  }
  if (!recordState.search) paintMatchCount("");
  applyFilters();
}
// Reading controls (parity #7) — behind their OWN header control since #142. They used to be
// the third section of the filter popover, and the owner asked three times where wide mode
// was: a funnel icon means filter, sitting inside the search box means search options, and
// Reading came last, under a tool list that is unsorted and unbounded (five fixture rows
// already needed scrolling; twenty MCP tools push it ~400px below the fold). One control now
// means one thing — funnel: what is shown; this one: how it reads. They apply as custom
// properties and two classes on the view's element (see production.css), persist with the other
// production preferences, and are also what the `w` / `-` / `+` keys drive.
// #173: code size and wrap left this panel. They were the only rows here that governed ONE
// KIND of content rather than the page, and a global control read as "everything, always" —
// the reader who wants one diff bigger got the whole transcript bigger. The preference is
// still the baseline every block starts from and the keys still move it; the per-block bars
// move a single block off it. What is left here is the page: its width, and how a user turn
// is drawn.
// #338: and the wrap BASELINE, which only the `w` key set once #173 moved the per-block bars off it —
// a phone has no `w` key, and the owner found it gone from here. Wide transcript is a phone's too
// since #s55: upright the text keeps a margin by default, and the switch gives it the whole width.
const readingSection = document.createElement("div");
readingSection.className = "reading-section";
readingSection.innerHTML = `<div class="scope-menu-head"><strong>Reading</strong><button class="scope-menu-action" type="button" data-reading-reset>Reset</button></div>
<div class="reading-row reading-wide"><span>Wide transcript</span><button class="mode-switch" type="button" role="switch" data-reading-toggle="wide" aria-label="Wide transcript" aria-checked="false"><span></span></button></div>
<div class="reading-row reading-wrap"><span>Wrap long lines</span><button class="mode-switch" type="button" role="switch" data-reading-toggle="wrap" aria-label="Wrap long lines — the baseline every code block starts from" aria-checked="false"><span></span></button></div>
<div class="reading-row"><span>User turns as raw text</span><button class="mode-switch" type="button" role="switch" data-reading-toggle="rawUser" aria-label="Show user turns as raw text — exactly as typed, whitespace intact" aria-checked="false"><span></span></button></div>
<div class="reading-row"><span>Viewport history</span><button class="scope-menu-action" type="button" data-history-save title="Save the last hour of what you did and what the page did — kinds, heights and timings, no content">Save</button></div>`;
// A host may switch the viewport history's Save off (stage 6): it is this repository's bug report.
if (!shows("history")) readingSection.querySelector("[data-history-save]").closest(".reading-row").remove();
// Production-only chrome, built here so the extracted demo shell stays byte-identical (the
// same reason the retired rail X wrote its own SVG). The cluster is the positioning
// context the popover anchors to; `navigator-options` carries the shared popover styling,
// including the narrow-window rule that turns it into a full-width sheet.
const readingCluster = document.createElement("div");
readingCluster.className = "reading-cluster";
const readingBtn = document.createElement("button");
readingBtn.className = "iconbtn reading-toggle";
readingBtn.id = "readingBtn";
readingBtn.type = "button";
readingBtn.title = "How the transcript reads — width, raw text";
readingBtn.setAttribute("aria-label", "How the transcript reads — width, raw text");
readingBtn.setAttribute("aria-haspopup", "true");
readingBtn.setAttribute("aria-expanded", "false");
// "Aa" — the one glyph every reader already knows for text preferences, and the icon set has
// nothing for it. Drawn in the SET'S OWN wrapper, though (#160): `class="icon"` on a 24 grid with no inline fill, stroke or
// weight, exactly what `svg()` in the generated icons.js emits. It used to carry its own 16
// grid and its own `stroke-width="1.5"`, so it sat outside `.icon`'s 13px/1.6 and read a size
// and a weight apart from every other glyph on the bar.
readingBtn.innerHTML =
  '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M2.1 18.9 6.9 5.9l4.8 13"/><path d="M3.6 15h6.6"/><path d="M21.9 18.9v-6.3a3 3 0 0 0-3-3H18a3 3 0 0 0-2.7 1.7"/><path d="M21.9 15.5h-3.6a2.6 2.6 0 0 0 0 5.1c1.7 0 3.6-1.2 3.6-3.3"/></svg>';
const readingOptions = document.createElement("div");
readingOptions.className = "navigator-options reading-options";
readingOptions.id = "readingOptions";
readingOptions.append(readingSection);
readingCluster.append(readingBtn, readingOptions);
// Anchored on `sessionFoldAll`, not on the outline toggle: #148 has since removed that button,
// and `.before()` on a missing node throws at module load — a blank app shell that every static
// gate would pass. A host that switched the menu off (stage 6) gets none: the cluster is never
// placed, here or in the phone's dock (`placeToolbar`).
if (shows("reading")) byId("sessionFoldAll").after(readingCluster);

/** Only one header popover is open at a time — two overlapping sheets is not a menu. */
function setPopover(which) {
  const reading = which === "reading";
  const filter = which === "filter";
  readingOptions.classList.toggle("open", reading);
  readingBtn.setAttribute("aria-expanded", String(reading));
  byId("navigatorOptions").classList.toggle("open", filter);
  byId("filterTranscriptBtn").setAttribute("aria-expanded", String(filter));
  // #s49, the owner: on a phone the filter and the keyboard never share the screen. The filter opening
  // puts the keyboard down and keeps the box open; the keyboard coming up closes the filter (the
  // field's focus handler), and so does the box closing (`setPhoneSearch`).
  if (filter && phoneSearch.matches) { searchBox.classList.add("phone-open"); fitSearchField(); byId("transcriptSearchInput").blur(); }
}
readingBtn.onclick = () => setPopover(readingOptions.classList.contains("open") ? null : "reading");
addEventListener("pointerdown", event => {
  if (!readingCluster.contains(event.target) && readingOptions.classList.contains("open")) setPopover(null);
}, { capture: true, signal });
/** The size control reads as a RELATIVE step, never as a pixel count (#160). An absolute "11px"
 *  pins the reader to one base size and forecloses the variable sizes we want later — the
 *  adjustment has always been relative, so say so: 0 at the default, +n and −n either side. */
function sizeStepLabel(size) {
  const steps = Math.round((clampSize(size) - DEFAULT_READING.size) / SIZE_STEP);
  return steps > 0 ? `+${steps}` : steps < 0 ? `\u2212${-steps}` : "0";
}
/** The per-block code override (#173). A pane's A− / A+ / wrap move THAT BLOCK: an EPHEMERAL
 *  entry keyed by the record it belongs to, held in memory only — no localStorage on either
 *  page, so a reload is a clean slate — and cleared when a session opens. There is no reset
 *  control because none is needed: A−/A+ walk back and wrap toggles back. What a block SHOWS is
 *  its override when it has one and the reading preference otherwise, so a block never claims a
 *  state it does not have (which is what the bar did while it drove the global preference). */
function codeOverride(key) { return recordState.codeOverrides.get(key) || null; }
function effectiveCode(key) {
  const over = codeOverride(key);
  return { size: over && over.size != null ? over.size : uiState.reading.size, wrap: over && over.wrap != null ? over.wrap : !!uiState.reading.wrap };
}
function setCodeOverride(key, patch) {
  if (!key) return;
  const before = effectiveCode(key);
  const next = { ...codeOverride(key), ...patch };
  // A value that lands back ON the baseline RELEASES the field instead of pinning the block to a
  // number that merely happens to match today (the classic page does the same, export.js:2553).
  // The contract has no reset control because walking back IS the reset — and a block left
  // holding `{size: 12}` would silently stop following the baseline the next time it moved.
  if (next.size === uiState.reading.size) delete next.size;
  if (next.wrap === !!uiState.reading.wrap) delete next.wrap;
  if (next.size == null && next.wrap == null) recordState.codeOverrides.delete(key);
  else recordState.codeOverrides.set(key, next);
  // At the clamp ceiling or floor a press changes nothing. Re-rendering anyway would re-measure
  // and re-anchor the reader for no visible reason — #173's other half, in miniature.
  const after = effectiveCode(key);
  if (after.size === before.size && after.wrap === before.wrap) return;
  // The override rides the MARKUP (view-model's `codeRows`), so re-rendering the mounted window
  // is what applies it: `render` rebuilds every mounted unit from the current state, measures
  // them and holds the reader's anchor — the same call a fold uses, and for the same reason.
  // Not `remeasure`, which throws away the height of every unit in the session: what changed
  // here is one block, and one block is what the mount pass re-measures.
  viewport.rerender();
}
/** The per-pane code bars show what THEIR OWN block reads at (#115, #173); fresh panes are
 *  painted here, and so is every pane when the baseline underneath them moves. */
function paintCodeBars() {
  for (const box of viewport.window.querySelectorAll("[data-codebox]")) {
    const shown = effectiveCode(box.querySelector("[data-code-record]")?.dataset.codeRecord || "");
    // #s8: the step shows only off the default — "0" stays in the DOM, as the classic page's does.
    const value = box.querySelector("[data-code-size-val]"); if (value) { const step = sizeStepLabel(shown.size); value.textContent = step; value.toggleAttribute("data-default", step === "0"); }
    const wrap = box.querySelector("[data-code-wrap]");
    if (wrap) {
      wrap.setAttribute("aria-pressed", String(shown.wrap));
      wrap.setAttribute("aria-label", shown.wrap ? "Long lines in this block wrap" : "Long lines in this block scroll");
      const word = wrap.querySelector("[data-code-wrap-word]"); if (word) word.textContent = shown.wrap ? "long lines wrap" : "long lines scroll";
    }
  }
}
let readingApplied = null;
function applyReading() {
  const prefs = uiState.reading;
  // #173's other half: a preference that changed NOTHING must not re-guess every height. Every
  // one of these four moves layout — size and width through the view element's properties, wrap
  // through its class, raw text through a re-render — and nothing else here does, so this is
  // the whole test. `-` held down at the floor, `Reset` on an already-default panel, a second
  // click on the same switch: each used to clear the height cache and rewrite the reader's
  // position from estimates. The first application has no `readingApplied` and so still
  // measures, as it always did.
  const layoutChanged = !readingApplied || ["size", "wrap", "wide", "rawUser"].some(key => readingApplied[key] !== prefs[key]);
  readingApplied = { size: prefs.size, wrap: !!prefs.wrap, wide: !!prefs.wide, rawUser: !!prefs.rawUser };
  // #109: the raw-text preference is the renderer's business — mirror it and re-render the
  // mounted turns when it changes (the first application, before any session, renders nothing).
  const rawChanged = recordState.rawUser !== !!prefs.rawUser;
  recordState.rawUser = !!prefs.rawUser;
  if (rawChanged) recordState.rawTurns.clear(); // a global change clears per-turn overrides, as the classic page does
  if (rawChanged && recordState.records.length) viewport.rerender();
  // On the view's own element (stage 6), where its rules read them, so a host needs no grid of ours.
  for (const [name, value] of Object.entries(readingVars(prefs))) sessionView.style.setProperty(name, value);
  sessionView.classList.toggle("wrap-code", !!prefs.wrap); sessionView.classList.toggle("wide", !!prefs.wide);
  for (const toggle of readingSection.querySelectorAll("[data-reading-toggle]")) toggle.setAttribute("aria-checked", String(!!prefs[toggle.dataset.readingToggle]));
  // The per-pane bars (#115) mirror the baseline in place — no re-render for a label — but only
  // for the blocks that have no override of their own (#173).
  paintCodeBars();
  // The reading measure moves the engine's width too, so the width laid out is this one from here.
  if (layoutChanged) { viewport.remeasure(); paneLaidOutWidth = laidOutWidth(); }
}
function setReading(patch) { uiState.reading = { ...uiState.reading, ...patch, size: clampSize(patch.size ?? uiState.reading.size) }; uiState.readingChosen = true; persist(); applyReading(); }
readingSection.onclick = event => {
  // The whole row is the switch's target (#s41: the switch alone is 34×20, under a finger's size),
  // as a settings row is on iOS and as the sidebar's Write mode row already was.
  const toggle = event.target.closest("[data-reading-toggle]") || (!event.target.closest("button") && event.target.closest(".reading-row")?.querySelector("[data-reading-toggle]")); if (toggle) { const key = toggle.dataset.readingToggle; setReading({ [key]: !uiState.reading[key] }); return; }
  if (event.target.closest("[data-history-save]")) { saveHistory(); return; }
  if (event.target.closest("[data-reading-reset]")) setReading({ size: 12, wrap: false, wide: false });
};
// The viewport history (#197, design/viewport-history.md): the engine's last hour — the reader's
// actions, its state after every transaction, the shape of every delta — as JSON for a bug
// report: kinds, heights, indices, turns and timings, never content.
// `copy(__viewportHistory.export())` in the console is the same object.
function saveHistory() {
  const out = viewport.exportHistory();
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
  const url = URL.createObjectURL(new Blob([JSON.stringify(out)], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url; a.download = `viewport-history-app-${stamp}.json`;
  viewRoot.layer().append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
// …and every control the reader presses outside the transcript is an action in it, by id.
document.addEventListener("click", event => {
  const button = event.target.closest ? event.target.closest("button[id]") : null;
  if (button && !button.closest("#transcript")) viewport.noteAction("control", button.id);
}, { capture: true, signal });
applyReading();
byId("filterTranscriptBtn").onclick = () => { setPopover(byId("navigatorOptions").classList.contains("open") ? null : "filter"); renderFilterMenu(); };
byId("navigatorOptions").onclick = event => { const scope = event.target.closest("[data-scope]"); if (scope) { const key = scope.dataset.scope; { const everything = ALL_SCOPES.every(k => uiState.searchScopes.has(k)); if (everything) uiState.searchScopes = new Set([key]); else if (uiState.searchScopes.has(key)) { uiState.searchScopes.delete(key); if (!uiState.searchScopes.size) uiState.searchScopes = new Set(ALL_SCOPES); } else uiState.searchScopes.add(key); } applyScopeFromMenu(); } // A twisty EXPANDS its branch and selects nothing (#293): it sits INSIDE the row button, so it
  // must be answered before the row, or opening a family would filter by it. Re-render only the
  // menu — the query has not changed.
  const tw = event.target.closest("[data-tw]"); if (tw) { const key = tw.dataset.tw; uiState.toolTreeOpen.has(key) ? uiState.toolTreeOpen.delete(key) : uiState.toolTreeOpen.add(key); renderFilterMenu(); return; }
  const tool = event.target.closest("[data-tool-filter]"); if (tool) { toggleToolFacet(tool.dataset.toolFilter); } };
byId("selectAllScopes").onclick = () => { uiState.searchScopes = new Set(ALL_SCOPES); uiState.searchWhole = false; uiState.chips.tools = []; applyScopeFromMenu(); };
byId("clearTranscriptFilters").onclick = () => { setToolFacets([]); };


/** `shown` is what the CLICKED card was already displaying (#144). The card rendered from data
 *  the view held, so that src is the truthful source; the record lookup below is an ENRICHMENT —
 *  it supplies the name, the text, the stamps — and must never be the only way to the bytes. When
 *  it misses (a tail rewrite replacing records, a stale id in a DOM that has not re-rendered) an
 *  embedded image has no path or stamp to fall back on, and the reader was told the image "cannot
 *  be opened" while its own thumbnail sat beside the message. */
function openAttachment(id, path, fsig, action = "preview", sig = "", card = {}) {
  const record = findRecord(id); const head = record?.head || {};
  const item = { id: `attachment:${id || path}`, name: head.att_name || card.name || path.split("/").pop() || "attachment", path: head.att_path || path, fsig: head.att_fsig || fsig, sig: head.att_sig || sig, text: head.att_text, data: head.att_datauri, embedded: head.att_datauri != null || head.att_text != null };
  const shown = card.src || "";
  const fileUrl = item.path && item.fsig ? backend().url(`/file?path=${encodeURIComponent(item.path)}&sig=${encodeURIComponent(item.fsig)}`) : "";
  item.source = item.data || fileUrl || shown;
  // #324: an image with its file on disk AND a stamp to show it is opened from the file — for a
  // pasted image that is the saved original, where the copy inside the transcript is a downscaled
  // re-encode (921×2000 against 1320×2868, measured) — and falls back to the inline copy if the
  // file is gone.
  if (action === "image" && fileUrl && item.data) { item.source = fileUrl; item.fallback = item.data; }
  // A card that was showing an inline payload is embedded, whatever the lookup found.
  if (!item.embedded && shown.startsWith("data:")) item.embedded = true;
  if (action === "image") preview.open(paneImageItem(item, shown));
  else if (action === "download") attachmentViewer.download(item);
  else if (action === "copy") attachmentViewer.copyPath(item);
  else if (action === "reveal") attachmentViewer.reveal(item);
  else preview.open(item);
}
/** An image as the right pane opens it (#s223, Hong: "if both are images, why not giving them a
 *  similar experience?" — a delivered file's name already opened there, and an image took three
 *  steps: a thumbnail, the lightbox, then the pane). Two rules of the lightbox carry over. #324: a
 *  pasted image whose saved original is on disk opens FROM the file, the transcript's downscaled
 *  copy kept as the fallback for an original that is gone (the pane prefers inline bytes, so they
 *  move to `fallback`). #144: the pane never shows less than the thumbnail did — a record lookup
 *  that missed still has the bytes the card was drawn from. */
function paneImageItem(item, shown = "") {
  const pane = { ...item };
  const fileUrl = pane.path && pane.fsig;
  if (fileUrl && pane.data) { pane.fallback = pane.data; pane.data = undefined; }
  else if (!pane.data && !fileUrl && String(shown).startsWith("data:")) pane.data = shown;
  return pane;
}
/** The pane item for the image record `id` (#s222's Open all), or null when it carries nothing to show. */
function paneImage(id) {
  const head = findRecord(id)?.head || {};
  if (!head.att_datauri && !(head.att_path && head.att_fsig)) return null;
  return paneImageItem({ id: `attachment:${id}`, name: head.att_name || "image", path: head.att_path, fsig: head.att_fsig, sig: head.att_sig, data: head.att_datauri, embedded: head.att_datauri != null });
}
// A path the server offered with stamps — from a tool header, an attachment, or a link that
// matched one. `referenceAction` decides by capability: file stamp → preview, reveal stamp →
// file manager, neither → the path goes to the clipboard.
function openReferenceOffer({ path, fileSig = "", revealSig = "", record = null }) {
  const head = record?.head || {};
  const item = { id: `reference:${path}`, name: head.att_name || path.split("/").pop() || "file", path, fsig: fileSig, sig: revealSig, text: head.att_text, data: head.att_datauri };
  const action = referenceAction({ fileSig, revealSig, reveal: backend().local });
  if (action === "preview") preview.open(item);
  else if (action === "reveal") attachmentViewer.reveal(item);
  else attachmentViewer.copyPath(item);
}
function openReference(path) {
  let decoded = path;
  try { decoded = decodeURI(path); } catch (_) {}
  const withoutLocation = decoded.replace(/:\d+(?::\d+)?$/, "");
  const candidates = new Set([decoded, withoutLocation]);
  // The best offer any record made for this path: a file stamp beats a reveal stamp.
  let offered = null;
  const visit = record => {
    if (!record || offered?.fileSig) return;
    const head = record.head || {};
    const offeredPath = head.att_path || head.path;
    if (candidates.has(offeredPath)) {
      const fileSig = head.att_fsig || head.fsig || "", revealSig = head.att_sig || head.sig || "";
      if (fileSig || (revealSig && !offered)) offered = { record, path: offeredPath, fileSig, revealSig };
    }
    for (const part of record.body || []) if (part.p === "blocks") for (const child of part.items || []) visit(child);
  };
  recordState.records.forEach(visit);
  // #s276: a path offered in the records still coming, from the index — first in the session's
  // order, so its file stamp stands where the records here offer none.
  const owed = offered?.fileSig ? null : owedIndex()?.files?.find(file => candidates.has(file.path));
  if (owed && (owed.fsig || !offered)) offered = { record: null, path: owed.path, fileSig: owed.fsig || "", revealSig: owed.sig || "" };
  if (offered) { openReferenceOffer(offered); return; }
  const copy = navigator.clipboard?.writeText(decoded);
  if (copy) copy.then(
    () => toast("That path carries no capability signature — copied instead; open it from a terminal or your file manager"),
    () => toast("That path carries no capability signature — the monitor will not read it")
  );
  else toast("That path carries no capability signature — the monitor will not read it");
}
function findRecord(id) { let found = null; const visit = record => { if (!record || found) return; if (record.id === id) { found = record; return; } for (const part of record.body || []) if (part.p === "blocks") for (const child of part.items || []) visit(child); }; recordState.records.forEach(visit); return found; }

// The jump control is the classic page's "↓ N new messages" pill as well (#64): a circle with
// the arrow while nothing new arrived; once records arrive with the reader scrolled up, it
// widens into a pill that SAYS how many, so the count is readable and not a tooltip. The count
// is records, as the classic page counts them; a jump or a scroll to the tail clears it.
function paintJump() {
  const button = byId("jumpToBottom");
  const n = recordState.newRecords;
  // Only where there is somewhere to jump (#s41): a fold opened in a session shorter than the screen
  // leaves the reader not following, and the jump offered a way down to nothing below.
  const below = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight > 4;
  button.classList.toggle("show", !recordState.following && (below || n > 0));
  button.classList.toggle("has-new", n > 0);
  button.setAttribute("aria-hidden", String(recordState.following));
  let count = button.querySelector(".jump-count");
  if (!count) { count = document.createElement("span"); count.className = "jump-count"; button.append(count); }
  count.textContent = n ? `${n} new message${n === 1 ? "" : "s"}` : "";
  const label = n ? `${n} new message${n === 1 ? "" : "s"} — jump to the latest` : "Jump to the latest";
  button.title = label; button.setAttribute("aria-label", label);
}
byId("jumpToBottom").onclick = () => { setTurnsFollow(true); viewport.follow(); };
function updateStickyHeaders() { const top = transcript.getBoundingClientRect().top; viewport.window.querySelectorAll("[data-process-surface]").forEach(surface => { const rect = surface.getBoundingClientRect(); surface.dataset.prodSticky = String(rect.top < top && rect.bottom > top + 40); }); }

// The outline pane has TWO states: open, and collapsed to its icon rail ("off"). #55 shipped a
// third — HIDDEN, gone altogether with the transcript taking the whole width — and the owner
// asked for it back out (#148): three states need three affordances, and the X that reached the
// third one read as "close" rather than "hide", which is a different promise. The caption's
// collapse button and the rail's expand button walk the two states, `o` toggles them, and the
// change re-measures the transcript at its new width holding the unit at the top of the view,
// so the reader's place does not move with the reflow.
//
// Retiring a shipped feature, not fixing a bug: the history should not read as a regression.
function toggleNavigator(open) {
  // Opening is refused where the outline cannot open, even with the room the host could give
  // (`outlineCanOpen`): its control is disabled there. Folding never is, and on the phone reader,
  // whose outline is the dock's sheets, the choice is only remembered.
  if (open && !uiState.navigatorOpen && !layout.phone() && !layout.arrangement()?.outlineCanOpen) return;
  const changed = uiState.navigatorOpen !== open;
  uiState.navigatorOpen = open; persist();
  // The host hears it first, so a host that makes room for the outline (the app's list narrowing) has
  // done so when the view decides, and the outline opens in this one decision.
  if (changed) say(options.onOutline, { wanted: open });
  layout.glide(); layout.decide(); renderNavigator(); paneLayoutChanged({ decided: true });
}
/** The outline as the order arranged it (#s431 stage 3): open at its track only where the arrangement
 *  says so, its glyphs otherwise (`navigator-off`, in their column or in the top bar), whatever the
 *  reader wants; the phone reader keeps the reader's choice, as before, since its outline is the dock's
 *  sheets. Called while the layout decides, so it is drawn in the same decision. */
function drawOutline(arrangement) {
  const workspace = viewRoot.$(".workspace");
  if (!workspace || !arrangement) return;
  // The reader's choice as the layout was told it: the defaults while the view's state is still being
  // made (the first decision comes before it).
  const wanted = outlineWanted();
  workspace.classList.toggle("navigator-off", arrangement.phone ? !wanted : arrangement.outline !== "open");
  // The glyphs' expand control (design/pane-layout.md, "The reader's choices"): pressed while the
  // reader wants the outline and the width holds it to its glyphs, and then a press folds it for good;
  // not pressed, it opens the outline, and is disabled where it cannot open. Disabled by
  // `aria-disabled`, so its title, which says why, still shows; its press then does nothing.
  const expand = viewRoot.byId("navigatorRailExpand");
  if (expand && !arrangement.phone) {
    const pressed = wanted && arrangement.outline !== "open", blocked = !wanted && !arrangement.outlineCanOpen;
    expand.setAttribute("aria-pressed", String(pressed));
    expand.setAttribute("aria-disabled", String(blocked));
    expand.title = pressed ? "The outline opens when the window has room. Press to keep it folded"
      : blocked ? "Too narrow to open the outline: widen the window" : "Expand the outline";
  }
  placeOutlineGlyphs(arrangement);
}
// #159. The pane selector. No glyph of its own (owner, #159 follow-up): the word "Outline" IS
// the trigger, hovered, exactly as the session title opens the copy menu — same surface, same
// pointer bridge across the gap, same 120ms grace so the pointer can travel into it. Production
// -only chrome layered on at runtime, because `reference-shell.html` is extracted from the demo.
/** The panes whose rows carry a filter sub-row, and what that row is filtering (#215). */
const LIVE_ONLY_PANES = { agents: "running sub-agents" };
/** The Tasks pane's sub-rows: one per state a reader distinguishes (#218). The keys are the pane's
 *  own group keys, so the menu and the grouping cannot drift apart. Agents keeps a single box,
 *  because a sub-agent is running or it is finished and one box already says which. */
const TASK_GROUP_ROWS = [
  { key: "in_progress", label: "Running" },
  { key: "pending", label: "Pending" },
  { key: "completed", label: "Completed" },
];
const panesTrigger = document.createElement("button");
panesTrigger.id = "navigatorPanesTrigger";
panesTrigger.type = "button";
panesTrigger.className = "outline-caption-title";
panesTrigger.textContent = "Outline";
panesTrigger.title = "Which panes this outline shows";
panesTrigger.setAttribute("aria-haspopup", "true");
panesTrigger.setAttribute("aria-expanded", "false");
const panesMenu = document.createElement("div");
panesMenu.className = "panes-menu";
panesMenu.id = "navigatorPanesMenu";
panesMenu.setAttribute("role", "menu");
panesMenu.setAttribute("aria-label", "Which panes this outline shows");
function renderPanesMenu() {
  // The filter menu's row, which is the owner's reference: one full-width button per choice, the
  // mark on the left, the name beside it. A CHECKBOX rather than the filter's dot, because these
  // are independent on/off choices and a dot reads as one-of-many.
  const rows = [...byId("sessionNavigator").querySelectorAll(":scope > .outline-card")].map(card => {
    const key = card.dataset.navCard;
    const label = card.querySelector(":scope > .outline-card-head strong")?.textContent?.trim() || key;
    const on = uiState.navPanes.has(key);
    const row = `<button class="pane-option ${on ? "on" : ""}" type="button" role="menuitemcheckbox" aria-checked="${on}" data-pane-toggle="${escapeText(key)}"><span class="pane-check" aria-hidden="true"></span><span>${escapeText(label)}</span></button>`;
    // #215, the owner: "an active only checkbox besides Tasks and Agents in the outline drop down
    // filter … it is not a frequent operation". A pane row is one full-width button, so the second
    // choice cannot nest inside it; it is an indented sub-row under the pane it belongs to, and
    // only while that pane is on — a filter for a pane nobody is showing is noise.
    if (!on) return row;
    // Tasks: one row per state (#218). The count beside a CHECKED row is what it is showing, so
    // the reader can see what each box is worth before unchecking it.
    if (key === "tasks") {
      const counts = recordState.taskGroupCounts || {};
      return row + TASK_GROUP_ROWS.map(g => {
        const shown = uiState.taskGroupsShown.has(g.key);
        const n = counts[g.key] || 0;
        return `<button class="pane-option pane-suboption ${shown ? "on" : ""}" type="button" role="menuitemcheckbox" aria-checked="${shown}" data-task-group="${escapeText(g.key)}" title="${shown ? `Showing the ${g.label.toLowerCase()} tasks` : `Show the ${g.label.toLowerCase()} tasks`}"><span class="pane-check" aria-hidden="true"></span><span>${escapeText(g.label)}</span>${n ? `<small>${n}</small>` : ""}</button>`;
      }).join("");
    }
    if (!LIVE_ONLY_PANES[key]) return row;
    const live = uiState.liveOnly.has(key);
    const hidden = (recordState.hiddenByFilter || {})[key] || 0;
    const count = live && hidden ? `<small>${hidden} hidden</small>` : "";
    const title = live
      ? `Showing only ${LIVE_ONLY_PANES[key]}${hidden ? ` — ${hidden} hidden` : ""}`
      : `Show only ${LIVE_ONLY_PANES[key]}`;
    return `${row}<button class="pane-option pane-suboption ${live ? "on" : ""}" type="button" role="menuitemcheckbox" aria-checked="${live}" data-live-only="${escapeText(key)}" title="${escapeText(title)}"><span class="pane-check" aria-hidden="true"></span><span>Active only</span>${count}</button>`;
  });
  panesMenu.innerHTML = `<div class="panes-caption">Panes</div>${rows.join("")}`;
}
let panesCloseTimer = 0;
function setPanesMenu(open) {
  clearTimeout(panesCloseTimer);
  if (open) renderPanesMenu();
  panesMenu.classList.toggle("open", open);
  panesTrigger.setAttribute("aria-expanded", String(open));
}
function schedulePanesClose() {
  clearTimeout(panesCloseTimer);
  panesCloseTimer = setTimeout(() => {
    if (!panesMenu.contains(viewRoot.active()) && viewRoot.active() !== panesTrigger) setPanesMenu(false);
  }, 120);
}
panesMenu.onclick = event => {
  // A task state, first: the Tasks pane's rows are one per state (#218). Never all three off — an
  // empty set is a pane that looks broken, and the way back would be a control the reader cannot
  // see the effect of; unchecking the last one puts every state back, as the session filter does.
  const group = event.target.closest("[data-task-group]");
  if (group) {
    const key = group.dataset.taskGroup;
    if (uiState.taskGroupsShown.has(key)) uiState.taskGroupsShown.delete(key);
    else uiState.taskGroupsShown.add(key);
    if (!uiState.taskGroupsShown.size) for (const g of TASK_GROUP_ROWS) uiState.taskGroupsShown.add(g.key);
    persist();
    renderNavigator();
    renderPanesMenu();
    return;
  }
  // The sub-row first: it sits inside the same menu and names its own pane (#215).
  const live = event.target.closest("[data-live-only]");
  if (live) {
    const key = live.dataset.liveOnly;
    if (uiState.liveOnly.has(key)) uiState.liveOnly.delete(key);
    else uiState.liveOnly.add(key);
    persist();
    renderNavigator();
    renderPanesMenu();
    return;
  }
  const toggle = event.target.closest("[data-pane-toggle]");
  if (!toggle) return;
  const key = toggle.dataset.paneToggle;
  // The last pane cannot be turned off — an outline with no panes is a column of nothing, and the
  // reader would have no card left to hover for the control that brings one back.
  if (uiState.navPanes.has(key) && uiState.navPanes.size <= 1) return;
  if (uiState.navPanes.has(key)) {
    uiState.navPanes.delete(key);
  } else {
    uiState.navPanes.add(key);
    // A pane coming BACK comes back open, so the reader is never handed a column where the thing
    // they just asked for is still hidden behind a second control. Since #157 the drawer STATE is
    // what open means, so that is the thing to set; the boolean follows it.
    uiState.navCards.add(key);
    drawers.open.set(key, 1);
    drawers.dir.set(key, "opening");
  }
  persist();
  applyPanes();
  renderPanesMenu();
};
{
  const caption = byId("sessionNavigator").querySelector(":scope > .outline-caption");
  const label = caption && caption.querySelector(":scope > span");
  if (label) label.replaceWith(panesTrigger);
  if (caption) caption.append(panesMenu);
  for (const type of ["pointerenter", "focus"]) panesTrigger.addEventListener(type, () => setPanesMenu(true));
  for (const node of [panesTrigger, panesMenu]) {
    node.addEventListener("pointerleave", schedulePanesClose);
    node.addEventListener("focusout", schedulePanesClose);
  }
  panesMenu.addEventListener("pointerenter", () => clearTimeout(panesCloseTimer));
  panesTrigger.addEventListener("keydown", event => { if (event.key === "Escape") setPanesMenu(false); });
}
{
  // The Info card leaves the stack and its BODY becomes the popover's content, so everything
  // `renderSessionInfo` writes — including the folding groups and their persisted state — keeps
  // working untouched. The card itself is removed, not hidden: `drawerCards()` must not see it,
  // and neither must the pane selector, which lists panes the reader can turn off.
  const nav = byId("sessionNavigator");
  const infoCard = nav.querySelector(':scope > [data-nav-card="session"]');
  const infoBody = viewRoot.byId("navigatorSession"); // in the shell, but it MOVES here
  const popover = document.createElement("div");
  popover.className = "info-popover";
  popover.id = "infoPopover";
  popover.setAttribute("role", "dialog");
  popover.setAttribute("aria-label", "Session details");
  if (infoBody) popover.append(infoBody);
  if (infoCard) infoCard.remove();
  uiState.navPanes.delete("session");
  outlineFooter.append(popover);
  nav.after(outlineFooter);

  const trigger = footerInfo;
  let pinned = false;
  let closeTimer = 0;
  const show = open => {
    popover.classList.toggle("open", open);
    trigger.setAttribute("aria-expanded", String(open));
  };
  const peek = () => { clearTimeout(closeTimer); show(true); };
  const leave = () => {
    if (pinned) return;
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => {
      if (!popover.contains(viewRoot.active()) && viewRoot.active() !== trigger) show(false);
    }, 140);
  };
  for (const type of ["pointerenter", "focus"]) trigger.addEventListener(type, peek);
  for (const node of [trigger, popover]) {
    node.addEventListener("pointerleave", leave);
    node.addEventListener("focusout", leave);
  }
  popover.addEventListener("pointerenter", () => clearTimeout(closeTimer));
  // Click PINS it, so the reader can read inside it, fold a group, or select a value to copy. A press
  // that pins is the reader asking for the session's Info, which the host hears (`onInfo`); a hover's
  // peek is not.
  const pin = () => { pinned = !pinned; show(pinned || true); if (pinned) sayInfo(); else leave(); };
  trigger.addEventListener("click", pin);
  // The phone's "i" in the top bar (#313) is this same control: a tap pins or unpins, and there is
  // no hover to peek with, so an unpin closes at once.
  infoPopoverToggle = () => { pinned = !pinned; show(pinned); if (pinned) sayInfo(); };
  addEventListener("pointerdown", event => {
    if (pinned && !popover.contains(event.target) && !trigger.contains(event.target) && !event.target.closest?.("[data-info-trigger]")) { pinned = false; show(false); }
  }, { capture: true, signal });
  addEventListener("keydown", event => { if (event.key === "Escape" && pinned) { pinned = false; show(false); } }, { signal });
  // The collapsed rail's Info button is the SAME control now — one gesture in both states,
  // rather than click-in-rail and hover-when-open.
  const railInfo = nav.parentElement?.querySelector('[data-nav-card-open="session"]');
  if (railInfo) {
    railInfo.removeAttribute("data-nav-card-open");
    railInfo.id = "outlineRailInfo";
    railInfo.title = "Session details";
    for (const type of ["pointerenter", "focus"]) railInfo.addEventListener(type, peek);
    railInfo.addEventListener("pointerleave", leave);
    railInfo.addEventListener("click", pin);
  }
}
applyPanes(); // the stored choice, before anything measures the column
byId("navigatorClose").onclick = () => toggleNavigator(false);
byId("navigatorRailExpand").onclick = () => toggleNavigator(!uiState.navigatorOpen);
// #148, at the owner's request: the top-bar toggle goes with the X. Its remaining job was
// bringing a HIDDEN pane back, and there is no hidden state now — the caption's collapse
// button and the rail's expand button already say everything two states can say, and the rail
// is a real sticky column at every width (measured at 820px: 40x219, its expand button 32x32
// and hit-testable), so nothing is stranded by removing it. The button lives in the GENERATED
// demo shell, which is never hand-edited, so it is removed here at runtime — the same
// production-chrome layering the X itself used.
byId("navigatorToggle")?.remove();
// The published-artifact roster (#78) lives in the right pane (#95), not in a header menu:
// one row per URL with its republish count — twenty calls for two decks read as two rows —
// as the pane's pinned first tab, and the count on the pane's own button so a closed pane
// still says there is something to see.
function renderArtifacts() {
  preview.setRoster(recordState.session === selectedId() ? artifactRoster(recordState.records, owedIndex()?.artifacts, scanFrom()) : []);
}
// The task details popover (#60): one element, filled per task, anchored beside the row;
// Escape and its close control dismiss it and focus returns to the row it came from.
const taskPopover = document.createElement("div");
taskPopover.className = "task-popover";
taskPopover.id = "taskPopover";
taskPopover.setAttribute("role", "dialog");
taskPopover.setAttribute("aria-modal", "false");
taskPopover.hidden = true;
// #319: on a phone the card is a sheet over the session (and over the Tasks drop-down it was chosen
// from), which read as one muddle — the owner: "it is very hard to read, suggest to dim the
// background". The scrim dims everything behind the card, and a tap on it closes the card. It is
// drawn on a phone only (production.css); elsewhere the card is a popover beside its row.
const taskScrim = document.createElement("div");
taskScrim.className = "task-scrim";
taskScrim.id = "taskScrim";
taskScrim.hidden = true;
viewRoot.layer().append(taskScrim, taskPopover);
// They float over the page, outside the view's element, and a press in them is a press in the view.
floating.add(taskScrim); floating.add(taskPopover);
let taskPopoverOpener = null;
function closeTaskPopover(restoreFocus = true) {
  if (taskPopover.hidden) return;
  taskPopover.hidden = true;
  taskScrim.hidden = true;
  taskPopover.innerHTML = "";
  if (restoreFocus && taskPopoverOpener?.isConnected) taskPopoverOpener.focus();
  taskPopoverOpener = null;
}
function openTaskPopover(index, opener) {
  const tasks = recordState.meta?.tasks || [];
  const task = tasks[index];
  if (!task) return;
  const key = String(task.id ?? index);
  // #s285: while a records-by-turn head is out, a record the page holds is where the task's status
  // was last recorded only when it is in the tail, which is always held and comes after every record
  // of the head. Otherwise the server is asked (`findTaskRecord`): the turn that holds the latest one
  // may never have been read, or been let go.
  const owed = recordStore.windowed ? owedIndex() : null;
  const held = recordState.taskTargets.get(key) ?? null;
  const d = taskDetails(task, owed && held != null && held < owed.upto ? null : held);
  const jump = d.target == null && owed
    ? `<button type="button" class="task-popover-jump" data-task-find="${index}">Go to the turn where this task's status was recorded</button>`
    : d.target == null && recordStore.head
    ? `<button type="button" class="task-popover-jump" disabled title="The session's earlier turns are still loading">Go to the turn — still loading</button>`
    : d.target == null
    ? `<button type="button" class="task-popover-jump" disabled title="This record stream did not keep where the task's status was set">Go to the turn — not kept in this stream</button>`
    : `<button type="button" class="task-popover-jump" data-task-record="${d.target}">Go to the turn where this task's status was recorded</button>`;
  // The card's anatomy is the shared module's (#125): the glyph, the chips, the
  // created·claimed·completed line and the labelled sections, as the queue's own board shows
  // them. What stays this shell's is the popover around it and the jump.
  taskPopover.innerHTML = `<div class="task-popover-head"><strong>${escapeText(d.subject)}</strong><button type="button" class="task-popover-close" aria-label="Close">✕</button></div>
    ${taskCardHtml({ ...task, id: key, blockedBy: d.blockedBy, blocks: d.blocks }, APP_TASK)}
    ${d.activeForm && d.status === "in_progress" ? `<div class="sv-task-card-dates">${escapeText(d.activeForm)}</div>` : ""}
    <div class="task-popover-actions">${jump}</div>`;
  taskPopoverOpener = opener;
  taskPopover.hidden = false;
  taskScrim.hidden = false;
  const r = opener.getBoundingClientRect(), w = Math.min(440, window.innerWidth - 24);
  const left = Math.min(window.innerWidth - w - 12, r.right + 8);
  taskPopover.style.left = `${Math.max(12, left)}px`;
  taskPopover.style.top = `${Math.max(12, Math.min(window.innerHeight - 24 - taskPopover.offsetHeight, r.top))}px`;
  taskPopover.querySelector(".task-popover-close").focus();
}
taskPopover.addEventListener("click", event => {
  if (event.target.closest(".task-popover-close")) { closeTaskPopover(); return; }
  const jump = event.target.closest("[data-task-record]");
  if (jump) { const at = Number(jump.dataset.taskRecord); closeTaskPopover(false); jumpOrFetch(at, "task"); return; }
  const find = event.target.closest("[data-task-find]");
  if (find) { const index = Number(find.dataset.taskFind), task = (recordState.meta?.tasks || [])[index]; closeTaskPopover(false); if (task) findTaskRecord(task, String(task.id ?? index)); }
});
document.addEventListener("keydown", event => { if (event.key === "Escape" && !taskPopover.hidden) { event.stopPropagation(); closeTaskPopover(); } }, { capture: true, signal });
document.addEventListener("pointerdown", event => { if (!taskPopover.hidden && !taskPopover.contains(event.target) && !event.target.closest("[data-task-open]")) closeTaskPopover(false); }, { signal });
// The tasks pane centered on the running tasks (#57), or on the done/pending boundary: the
// target row is the pure function's, the scroll is the PANE's own scroller — never the
// transcript's. Runtime chrome on the card's head, so the generated shell stays exact.
const tasksCenter = document.createElement("button");
tasksCenter.type = "button";
tasksCenter.className = "outline-card-action";
tasksCenter.id = "tasksCenter";
tasksCenter.innerHTML = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><circle cx="8" cy="8" r="5.5"/><circle cx="8" cy="8" r="1.6" fill="currentColor" stroke="none"/><path d="M8 0.8v2.4M8 12.8v2.4M0.8 8h2.4M12.8 8h2.4"/></svg>';
tasksCenter.title = `Center the pane on the running tasks — or on the boundary between done and pending  ( ${hintFor("tasks-center")} )`;
tasksCenter.setAttribute("aria-label", tasksCenter.title);
viewRoot.$('[data-nav-card="tasks"] .outline-card-head').insertAdjacentElement("afterend", tasksCenter);

// #186's live-only control USED to be a dot on each card's head. It moved into the panes menu in
// #215, on the owner's report: the dot was indistinguishable from the decorative dots the counts
// wear, and where it sat it could not be clicked at all — the head is `z-index:2` and painted over
// it, so a real pointer toggled the drawer instead. The choice itself (`uiState.liveOnly`) is
// unchanged and still on by default; only its control moved, to a place that suits how rarely it
// is used.
function paneScroller(el) {
  for (let node = el.parentElement; node; node = node.parentElement) {
    const o = getComputedStyle(node).overflowY;
    if ((o === "auto" || o === "scroll") && node.scrollHeight > node.clientHeight && !node.contains(transcript)) return node;
  }
  return null;
}
function centerTasks() {
  // Aimed at the rows on screen, not at the board behind them (#225).
  const shown = recordState.shownTaskRows || [];
  const target = taskCenterTarget(shown);
  if (!target) return false;
  // #226, the owner, after a click left the column in a state no gesture would produce — the Tasks
  // card sitting ON the Turns list: "The control should not change drawer positions, only the items
  // in the pane." It used to turn the pane on, force its drawer fully open (#139) and open the
  // whole column. The middle one is what broke the stack: one drawer set to fully open takes no
  // room from the others, so the column exceeds its viewport and the sticky cards ride over one
  // another. The control scrolls the pane and does nothing else; on a shut drawer there is nothing
  // to scroll and nothing happens, which is the honest answer rather than acting at a distance.
  const row = viewRoot.$$("#navigatorWork .work-task")[target.index];
  if (!row) return false;
  const pane = paneScroller(row);
  if (!pane) return false;
  const r = row.getBoundingClientRect(), p = pane.getBoundingClientRect();
  const edge = target.edge === "top" ? r.top : target.edge === "bottom" ? r.bottom : (r.top + r.bottom) / 2;
  pane.scrollTop += edge - (p.top + p.height / 2);
  tasksCenter.dataset.centered = target.why;
  return true;
}
tasksCenter.onclick = event => { event.stopPropagation(); centerTasks(); };
byId("sessionFoldAll").onclick = () => { const close = recordState.units.some(unit => unit.type === "process" && !recordState.processFolds.get(unit.key)); for (const unit of recordState.units) if (unit.type === "process") recordState.processFolds.set(unit.key, close); viewport.rerender(); };
// The top bar's two ways to the session list — the phone's back and the desktop's reopen — are the
// app's: the view draws them in its top bar for now (design/session-view-component.md, "The phone
// top bar") and says when one is pressed.
byId("mobileBack").onclick = () => options.openList?.();
byId("sidebarReopen").onclick = () => options.openSidebar?.();
addEventListener("hashchange", () => { landedHash = ""; landOnHash(); }, { signal });
addEventListener("resize", () => { paneLayoutChanged(); updateStickyHeaders(); }, { passive: true, signal });
// The keymap (parity #11): the classic view's keys, resolved by keymap.js, acted on here.
const userUnits = () => recordState.units.filter(unit => unit.type === "user");
// The unit the reader is "at": the first mounted unit that starts at (or within a line above)
// the viewport top. A landed turn sits 18px down, so the element ending just above it must
// not count as current — that made `]` land on the same turn twice.
// The unit the reader is INSIDE: the last mounted unit whose top is at or above a line a little
// below the viewport's top edge — the classic page's spy rule (#199). It used to be the first
// unit that STARTS in the viewport, which is the unit BELOW the one being read whenever the one
// being read spans the top edge, and nothing at all when no unit starts in the viewport (the
// bottom of an answer taller than the window). The first child stands in when nothing has
// scrolled past the line yet. The line sits just BELOW where a jump lands its target (the
// classic page's STICKY_Y over GOTO_Y): otherwise a turn the reader clicked in the pane lands
// under the line and the spy keeps the previous turn selected.
function unitAtTop() {
  const line = viewport.scroller.getBoundingClientRect().top + viewport.landing + 8;
  let key = null;
  for (const child of viewport.window.children) {
    if (child.getBoundingClientRect().top <= line) key = child.dataset.unitKey;
    else break;
  }
  return key ?? viewport.window.firstElementChild?.dataset.unitKey ?? null;
}
/** The record index the viewport starts at — `unitAtTop` names a UNIT, and a unit's key is not
 *  a record index; reading `.from` off the key gave `undefined`, so every cold step and every
 *  filter landed on the session's first hit instead of the nearest one (#118). Inside the unit it
 *  is the last record row drawn at or above the same line (#s264): a process unit groups a turn's
 *  calls and can be thousands of pixels tall, and its `.from` sent a reader deep inside it back to
 *  its first hit. The unit's `.from` stands in where no row of it is drawn there (a folded one).
 *  A record's own sub-rows (a call inside an act, `11.1`) are not records and are passed over;
 *  the first reads `11.0`, which is its record's index again. */
function recordIndexAtTop() {
  const key = unitAtTop();
  const unit = recordState.units.find(u => u.key === key);
  if (!unit) return 0;
  const line = viewport.scroller.getBoundingClientRect().top + viewport.landing + 8;
  let at = unit.from;
  for (const element of unitRow(key)?.querySelectorAll("[data-block-index]") || []) {
    const index = Number(element.dataset.blockIndex), r = element.getBoundingClientRect();
    if (Number.isInteger(index) && index > at && index <= unit.to && r.height > 0 && r.top <= line) at = index;
  }
  return at;
}
function unitRow(key) {
  return [...viewport.window.children].find(child => child.dataset.unitKey === key);
}
// The turn the reader is in — ONE rule for the turn bar, the navigator pane and the keys. At
// the bottom no further header can ever cross the line, so the last turn could otherwise never
// become current — exactly where a pinned live tail sits (the classic page's #89 end rule); at
// the bottom, the last turn wins (#199). `hold` is the slack under which the engine keeps
// following, so "at the bottom" here is the same 80px it is for the tail.
// (Counted inline rather than through `userUnits`: the first render runs while this module is
// still being evaluated, and that `const` is declared further down — a temporal dead zone.)
function currentUserUnitIndex(atKey) {
  const units = recordState.units;
  if (units.length && (viewport.following || viewport.gapToBottom() <= viewport.slacks.hold)) {
    let last = -1;
    for (const unit of units) if (unit.type === "user") last++;
    if (last >= 0) return last;
  }
  // `atKey` given: the engine's own belief of where the reader is (#209), which costs no layout
  // read — the rule below scans the mounted rects. Read at that rule's line (#s450).
  return currentTurnIndex(units, atKey === undefined ? unitAtTop() : keyAtSpyLine(atKey));
}
/** The unit under `unitAtTop`'s line, from the engine's position alone (#s450). A position the
 *  reader's scroll set names the unit crossing the viewport's TOP, and right after a landing that
 *  is the turn before's tail, under the turn bar: one 4px scroll down from a `]` named the turn
 *  before. The line sits `landing + 8` below the top, so walk the engine's heights from that unit
 *  to it. A key that is not the position's is taken as it is.
 *
 *  A position a command set holds its target where it put it, until the reader moves. Put at the
 *  landing (a turn step, `]`), the target is the unit the line crosses and is taken as it is. Put
 *  lower (a head step's 160px, a search's landing, #s454), the line can cross a unit above it, the
 *  opener of the target's own turn among them, and a transaction while the reader rests (a unit
 *  below measured, a live append) named the target's turn where a pixel of scroll named the line's,
 *  so the host heard the turn flip and flip back. That line is read from the window as
 *  `unitAtTop` reads it: the command's `top` is its row's when it named one, not the unit's, so
 *  the walk below has no edge to start from; and a placement has just laid the window out (a
 *  smooth write calls this only on arrival, in the scroll event that reads the same line next).
 *
 *  A captured position's `top` is the unit's border box, the edge `unitAtTop` compares, while the
 *  engine's heights are margin boxes: the walk steps from slot to slot and compares each unit's
 *  own border top, its top margin added (a plan card's 8px, the loading card's 6px). Stepping by
 *  border tops alone put every unit after a plan card 8px low, so in that band the two readings
 *  named different turns. The margin is a style, read without a layout. */
function keyAtSpyLine(key) {
  const p = viewport.position;
  if (!p || p.key !== key || p.index == null) return key;
  if (p.commanded) return Math.abs(p.top - viewport.landing) <= 1 ? key : unitAtTop() ?? key;
  if (p.source !== "anchor") return key;
  const line = viewport.landing + 8;
  const margin = index => {
    const row = unitRow(viewport.identityAt(index));
    return row ? parseFloat(getComputedStyle(row).marginTop) || 0 : 0;
  };
  let index = p.index, slot = p.top - margin(index);
  while (index > 0 && slot + margin(index) > line) slot -= viewport.heightOf(--index);
  while (index < viewport.count - 1 && slot + viewport.heightOf(index) + margin(index + 1) <= line) slot += viewport.heightOf(index++);
  return viewport.identityAt(index) ?? key;
}
// The pane follows the transcript (#52): the outline row of the turn at the viewport top is the
// current one — marked (`current`, `aria-current`) on every scroll and render, and revealed in
// the pane's OWN scroller (never the transcript's), only when the current row changes, so the
// spy never fights the reader. The click direction — a row jumps the transcript to its turn —
// lands that turn at the top, so the spy then names the row that was clicked.
function updateTurnBar(atKey) {
  const index = currentUserUnitIndex(atKey);
  const unit = index >= 0 ? userUnits()[index] : null;
  // Off at the very top of the first turn — nothing has scrolled past, so the turn is on
  // screen naming itself. Anywhere else it names the turn the reader is inside, including
  // right after a jump: the classic page keeps its bar on there too, and that is how a
  // reader sees where a click landed.
  const on = !!unit && viewport.scroller.scrollTop > 8;
  turnStickyBar.classList.toggle("on", on);
  turnStickyBar.setAttribute("aria-hidden", String(!on));
  turnStickyBar.tabIndex = on ? 0 : -1;
  turnStickyAt = on ? unit.from : null;
  turnSubtitle.hidden = !unit;
  if (unit) turnSubtitle.textContent = `Turn ${unit.turn} — ${unit.label || ""}`.trimEnd();
  if (!on) return unit;
  turnStickyText.textContent = `Turn ${unit.turn} — ${unit.label || ""}`.trimEnd();
  turnStickyBar.title = `Back to turn ${unit.turn}`;
  return unit;
}

function updateOutlineFocus() {
  const rows = byId("navigatorTurns").querySelectorAll(".outline-turn-row");
  if (!rows.length) { outlineCurrent = null; return; }
  const index = currentUserUnitIndex();
  const unit = index >= 0 ? userUnits()[index] : null;
  const key = unit ? String(unit.from) : null;
  let target = null;
  rows.forEach(row => {
    const on = key != null && row.dataset.turnRecord === key;
    row.classList.toggle("current", on);
    if (on) { row.setAttribute("aria-current", "true"); target = row; } else row.removeAttribute("aria-current");
  });
  if (key === outlineCurrent) return;
  outlineCurrent = key;
  if (target && turnsFollow) revealInPane(target);
}
function revealInPane(row) {
  // #139: only while the drawer is fully open. A part-way drawer shows the TOP of its list and
  // clips the rest, so scrolling the list inside it would move rows the reader cannot see — and
  // opening the drawer to make room would fight the reader who just shut it, on every scroll of
  // the transcript.
  const card = row.closest(".outline-card");
  if (card && (drawers.open.get(card.dataset.navCard) ?? 1) < 1) return;
  let pane = row.parentElement;
  while (pane && pane !== viewRoot.layer()) {
    const overflow = getComputedStyle(pane).overflowY;
    if ((overflow === "auto" || overflow === "scroll") && pane.scrollHeight > pane.clientHeight) break;
    pane = pane.parentElement;
  }
  if (!pane || pane === viewRoot.layer() || pane.contains(transcript)) return;
  const bounds = pane.getBoundingClientRect(); const rect = row.getBoundingClientRect();
  if (rect.top < bounds.top) pane.scrollTop -= bounds.top - rect.top;
  else if (rect.bottom > bounds.bottom) pane.scrollTop += rect.bottom - bounds.bottom;
}
// #s207 (the owner): the Turns pane is LOCKED to the session — it keeps the current turn in view —
// until the reader scrolls the pane itself, which UNLOCKS it: from then nothing moves it, neither a
// turn the session tails nor the current turn changing. Scrolling the session view, the jump to the
// latest, the sticky turn bar, a turn step, or choosing a turn in the pane LOCKS it again. Both are
// decided on the reader's INPUT, never on a scroll event: the shell's own writes scroll too, and a
// headless renderer delivered the reader's scroll event a second after their wheel. Input that will
// move the list — a wheel it has room for (the outline's wheel handler decides that on the
// desktop), a finger dragging it, a scroll key with focus in it, a press on its scrollbar — unlocks;
// the same input on the session view locks. Wherever the list is drawn (the outline, a phone's
// sheet, a wide phone's column) its scroller is the nearest one around it, and `data-follow` on the
// list says which state it is in.
function turnsScroller() {
  let pane = byId("navigatorTurns");
  while (pane && pane !== viewRoot.layer()) {
    const overflow = getComputedStyle(pane).overflowY;
    if ((overflow === "auto" || overflow === "scroll") && pane.scrollHeight > pane.clientHeight) return pane;
    pane = pane.parentElement;
  }
  return null;
}
/** Lock (`on`) or unlock the pane. Locking shows the current turn at once, unless `reveal` is
 *  false — a turn chosen in the pane is in view already, and the session's jump marks it. */
function setTurnsFollow(on, reveal = true) {
  if (turnsFollow === on) return;
  turnsFollow = on;
  byId("navigatorTurns").dataset.follow = on ? "locked" : "unlocked";
  if (on && reveal) { outlineCurrent = null; updateOutlineFocus(); }
}
byId("navigatorTurns").dataset.follow = "locked";
const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]);
const editable = target => !!target?.closest?.("input, textarea, select, [contenteditable=''], [contenteditable='true']");
function readerMoves(event) {
  const target = event.target, type = event.type;
  // The session first, reading no layout: wheels and touchmoves over the transcript arrive at the
  // engine's own rate, and a glide is no place to make the browser lay out the outline (#372).
  if (transcript.contains(target) || (type === "keydown" && target === page.body())) {
    if (type === "wheel" || type === "touchmove" || (type === "pointerdown" && target === transcript) || (type === "keydown" && SCROLL_KEYS.has(event.key) && !editable(target))) setTurnsFollow(true);
    return;
  }
  // Only then the list, and only for input in or around it.
  const list = byId("navigatorTurns");
  if (!(target instanceof Element) || !(list.contains(target) || target.contains(list))) return;
  const scroller = turnsScroller();
  if (!scroller || !scroller.contains(target)) return;
  if (type === "wheel") {
    // On the desktop the outline's own wheel handler knows whether the gesture scrolls the list.
    if (byId("sessionNavigator").contains(scroller)) return;
    const room = event.deltaY < 0 ? scroller.scrollTop > 1 : scroller.scrollTop < scroller.scrollHeight - scroller.clientHeight - 1;
    if (event.deltaY && room) setTurnsFollow(false);
  } else if (type === "touchmove" || (type === "keydown" && SCROLL_KEYS.has(event.key)) || (type === "pointerdown" && target === scroller)) setTurnsFollow(false);
}
for (const kind of ["wheel", "touchmove", "pointerdown", "keydown"]) document.addEventListener(kind, readerMoves, { capture: true, passive: true, signal });

function stepTurn(delta) {
  setTurnsFollow(true);
  const turns = userUnits(); if (!turns.length) return;
  const next = Math.max(0, Math.min(turns.length - 1, currentUserUnitIndex() + delta));
  viewport.jumpToRecord(turns[next].from, "turn");
}
function stepHead(delta) {
  const heads = [...transcript.querySelectorAll("button.renderer-head")].filter(head => head.offsetParent !== null);
  if (!heads.length) return;
  const at = heads.indexOf(viewRoot.active());
  const next = at < 0 ? (delta > 0 ? 0 : heads.length - 1) : Math.max(0, Math.min(heads.length - 1, at + delta));
  heads[next].focus({ preventScroll: true });
  // The classic page's rule (its `stepHead`): a head clear of the top chrome and of the bottom
  // edge stays where it is; one that is not is brought to 160px, smoothly — the engine's own
  // animation, which the reader's wheel interrupts (framework §4.10). Parity with the reference;
  // this shell used to scroll the head in by its nearest edge, instantly.
  const r = heads[next].getBoundingClientRect(), v = viewport.scroller.getBoundingClientRect();
  if (r.top < v.top + 100 || r.bottom > v.bottom - 60) viewport.reveal(heads[next], { top: 160, smooth: true, intent: true });
  else viewport.markIntent();
}
function pageTranscript(direction) {
  viewport.pageBy(direction, { intent: true });
}
// The view's half of the keymap, by the keymap's action names: the handle's `keys`. In the app
// (`keyboard: "page"`) the app binds the page's keys and adds the list's (app.js); engaged, the view
// binds them itself (below).
const keyActions = {
  "search": () => { setPhoneSearch(true); byId("transcriptSearchInput").focus(); },
  "turn-next": () => stepTurn(1), "turn-prev": () => stepTurn(-1),
  "head-next": () => stepHead(1), "head-prev": () => stepHead(-1),
  "hit-next": () => stepFromBox(1), "hit-prev": () => stepFromBox(-1),
  "wrap": () => setReading({ wrap: !uiState.reading.wrap }),
  "size-down": () => setReading({ size: uiState.reading.size - SIZE_STEP }), "size-up": () => setReading({ size: uiState.reading.size + SIZE_STEP }),
  "page-down": () => pageTranscript(1), "page-up": () => pageTranscript(-1),
  "navigator-toggle": () => toggleNavigator(!uiState.navigatorOpen),
  "tasks-center": () => centerTasks(),
  "parent": () => { if (parentBtn.dataset.parent) selectSession(parentBtn.dataset.parent, true); }
};
// A control a host switched off takes no key (stage 6): its actions are not the view's.
if (!shows("reading")) for (const action of ["wrap", "size-down", "size-up"]) delete keyActions[action];
// Engaged (stage 6, the default for a host): the view answers its keymap only while the reader is
// working in it (`engaged`, above), as `design/embedding.md` says a component does, so it never
// takes the host's keys. The shared keymap resolves the binding; an action the view does not have
// (the list's arrows, `\`) or one that answers `false` is left to the page. It listens in the capture
// phase, so a host's own listener on the page, bound before the view's, finds the key already taken
// (`defaultPrevented`) rather than acting on it beside the view. "page" binds nothing: the host
// routes the page's keys to `keys` (app.js).
if (keyboardMode === "engaged") {
  document.addEventListener("keydown", event => {
    if (!engaged || !element.isConnected) return;
    const binding = resolveKey(event, "view", event.target);
    const act = binding && keyActions[binding.action];
    if (act && act() !== false) event.preventDefault();
  }, { capture: true, signal });
}
// Discoverability in the shell's idiom: the key in the control's own title / hint.
byId("transcriptSearchInput").placeholder = `Search this session  ( ${hintFor("search")} )`;
byId("findPrev").title = `Previous match (${hintFor("hit-prev")})`; byId("findNext").title = `Next match (${hintFor("hit-next")})`;
byId("turnPrev").title = `Previous turn (${hintFor("turn-prev")})`; byId("turnNext").title = `Next turn (${hintFor("turn-next")})`;
// The header's own turn steppers were in the design but never bound; they step the same way the keys do.
byId("turnPrev").onclick = () => stepTurn(-1); byId("turnNext").onclick = () => stepTurn(1);
addEventListener("keydown", event => { if (event.key === "Escape") { setSessionCopyMenu(false); setPopover(null); } }, { signal });

// #313 — while a session loads, a veil over the session area says so. It dims what is below the top
// bars, which already name the new session, and speaks in three phases: loading; after
// WAITING_AFTER_MS with nothing downloaded yet, that the server is reading the session for the first
// time (the same wait `waiting` explains, in the veil's own voice); then how much of the records has
// arrived. The owner, choosing sessions on a phone: "dim the remaining area with some UX feedback
// showing we are loading". It never takes a tap (it is feedback, not a lock), it fades in only
// after a beat so a warm local open does not flash, and it goes the moment the first reply is
// applied — or on an error, which the page states itself.
var sessionLoading;
{
  const veil = document.createElement("div");
  veil.className = "session-loading";
  veil.id = "sessionLoading";
  veil.hidden = true;
  veil.setAttribute("role", "status");
  veil.innerHTML = '<div class="session-loading-card"><span class="session-loading-spinner" aria-hidden="true"></span><span class="session-loading-text">Loading session…</span><span class="session-loading-bar" hidden><i></i></span></div>';
  viewRoot.$(".sv-session-main").append(veil);
  const text = veil.querySelector(".session-loading-text"), bar = veil.querySelector(".session-loading-bar"), fill = bar.querySelector("i");
  let slow = 0, downloading = false;
  const size = n => (n >= 1e6 ? `${(n / 1e6).toFixed(n < 1e7 ? 1 : 0)} MB` : `${Math.max(1, Math.round(n / 1e3))} KB`);
  sessionLoading = {
    show() {
      clearTimeout(slow);
      downloading = false;
      text.textContent = "Loading session…";
      bar.hidden = true;
      veil.hidden = false;
      slow = setTimeout(() => { if (!downloading && !veil.hidden) text.textContent = "Reading this session for the first time…"; }, 1200);
    },
    progress(got, total) {
      if (veil.hidden) return;
      downloading = true;
      const share = total > 0 ? Math.min(1, got / total) : 0;
      text.textContent = total > 0 ? `Loading ${Math.round(share * 100)}% of ${size(total)}` : `Loading ${size(got)}`;
      bar.hidden = !(total > 0);
      fill.style.width = `${(share * 100).toFixed(1)}%`;
    },
    hide() { clearTimeout(slow); veil.hidden = true; },
  };
}

// #313 — the phone shell (the owner: "it does not feel native at all"). One breakpoint, the view's phone family (`PHONE_QUERY` read from its own classes, #s431: 760px, or a landscape phone, #s32):
//  - The session list is a DRAWER over part of the session view, never all of it. A handle fixed at
//    the top left opens and closes it, and stays where it is either way — it is a sibling of the
//    drawer in #app, not inside the top bar, whose layer sits under the drawer. A tap on the dimmed
//    part of the view closes it; choosing a session closes it at once (`selectSession`).
//    `mobile-detail` keeps its meaning from #310: present = the drawer is shut.
//  - #s37 (design/phone-landscape.md): the top bar is ONE row — the handle's slot, the session's
//    title with the current turn as its subtitle, Info and the right pane — and never hides. Turns,
//    Tasks, Agents and Aa float at the bottom in a pill above the home indicator, search beside it.
//    The outline column does not exist at this width; its three panes open from their icons as
//    sheets rising from the pill, holding the SAME live lists (moved in while open, moved back
//    after), so the outline's rendering, its filters and its one click handler serve both.
//  - `--phone-top`, the bar's measured bottom, places the sheets that hang from the top (Info);
//    `--phone-bottom`, the dock's top, the sheets that rise from the pill.
// The drawer and a wide phone's column are the app's (app.js); the rest of the shell is the view's.
var mobileShell;
var infoPopoverToggle;
var phoneDock;
{
  const PHONE = layout.query("phone");
  const WIDE = layout.query("wide");
  const topbar = viewRoot.$(".topbar");

  const info = document.createElement("button");
  info.type = "button";
  info.className = "iconbtn phone-only phone-info";
  info.id = "phoneInfo";
  info.dataset.infoTrigger = "";
  info.title = "Session details";
  info.setAttribute("aria-label", "Session details");
  info.innerHTML = svg("info");
  info.onclick = () => infoPopoverToggle?.();
  const panes = PHONE_PANES.map(([key, label, body]) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "iconbtn phone-only phone-pane";
    button.id = `phonePane-${key}`;
    button.dataset.phonePane = key;
    button.title = label;
    button.setAttribute("aria-label", label);
    button.setAttribute("aria-haspopup", "true");
    button.setAttribute("aria-expanded", "false");
    button.innerHTML = `${svg(key)}<span class="phone-pane-count" hidden></span>`;
    return { key, label, body, button };
  });
  byId("previewBtn").before(info);

  // #s37 (design/phone-landscape.md §3.5a, the owner's idea): the toolbar floats at the bottom in a
  // PILL above the home indicator — Turns, Tasks, Agents and Aa — with search its own control beside
  // it, iOS 26's own pattern (Safari's bottom bar, the tab bar and search circle of Mail, Notes,
  // Music). The top keeps one row. On a phone the reading and the search clusters move into the dock
  // (the SAME nodes, so every handler and state comes along) and back to the top bar off a phone.
  // The search's states decide the dock's look in CSS: closed and empty, a circle beside the pill;
  // open or holding a query, the find pill across the dock, the toolbar pill stepping aside.
  const dock = document.createElement("div");
  dock.className = "phone-dock";
  dock.id = "phoneDock";
  const dockPill = document.createElement("div");
  dockPill.className = "dock-pill";
  dockPill.id = "dockPill";
  dockPill.setAttribute("role", "toolbar");
  dockPill.setAttribute("aria-label", "Session tools");
  dockPill.append(...panes.map(p => p.button));
  dock.append(dockPill);
  sessionView.append(dock);
  phoneDock = dock;
  const searchCluster = viewRoot.$(".header-search-cluster");
  // While the find pill holds the dock, a glass ✕ beside it ENDS the search — the query and its marks
  // go, the keyboard goes down and the toolbar pill comes back — the way an iOS 26 search closes
  // beside its field. The field's own small ✕ only clears it to type again: one control, one meaning.
  const searchDone = document.createElement("button");
  searchDone.type = "button";
  searchDone.className = "phone-search-done";
  searchDone.id = "phoneSearchDone";
  searchDone.innerHTML = svg("x");
  searchDone.title = "End the search";
  searchDone.setAttribute("aria-label", "End the search");
  searchDone.onclick = () => {
    // The search's own sheets go with it (#s41: the filter stayed open over the transcript).
    setPopover(null);
    searchClear.click();
    byId("transcriptSearchInput").blur();
    setPhoneSearch(false);
  };
  // #s39 (design/phone-landscape.md §3.5c, approved 2026-10-08): the compose box is the dock's third
  // mode, as Messages does it. With write mode on and a session that can take a message, a ✎ circle
  // joins the dock beside search in place of the floating compose button; a tap turns the dock into
  // the reply box — the SAME #composer and its store, restyled on a phone (production.css) — which
  // rides the keyboard as the find pill does, its target and warning in sight above the field. Its ✕
  // keeps the draft, and a dot on ✎ says one is waiting.
  const composeFab = byId("composeFab"), composer = byId("composer"), composeInput = byId("composeInput");
  const dockCompose = document.createElement("button");
  dockCompose.type = "button";
  dockCompose.className = "dock-compose";
  dockCompose.id = "dockCompose";
  dockCompose.hidden = true;
  dockCompose.innerHTML = `${svg("edit")}<span class="dock-compose-dot" aria-hidden="true"></span>`;
  dockCompose.title = "Write to this session";
  dockCompose.setAttribute("aria-label", "Write to this session");
  dockCompose.onclick = () => composeFab.click();
  const composeNote = document.createElement("div");
  composeNote.className = "compose-note";
  composer.prepend(composeNote);
  // The field grows with its text to five lines, then scrolls. On a desktop `placeDeskCompose`
  // fits it, once the box has the width the text wraps in.
  const fitCompose = () => {
    if (controls.nativeActive) { controls.fitComposer(); return; }
    composeInput.style.height = "";
    if (PHONE.matches && composer.classList.contains("production-show")) composeInput.style.height = `${Math.min(composeInput.scrollHeight, 132)}px`;
  };
  // #s245: a desktop's open box keeps to the session's reading column — the transcript's own text
  // width, inside its column, never over the outline beside it — and the transcript's end is
  // pushed above it (`--compose-room`), so a reader following the end still sees it as the box grows.
  const workspace = composer.closest(".workspace"), transcriptInner = byId("transcriptInner");
  let deskRoom = null, deskFrame = 0;
  const placeDeskCompose = () => {
    const open = composer.classList.contains("production-show") && !PHONE.matches && !controls.nativeActive && !page.body().dataset.sharedCodex;
    const was = deskRoom;
    workspace.classList.toggle("desk-composing", open);
    if (open) {
      const ws = workspace.getBoundingClientRect(), col = transcript.getBoundingClientRect(), text = transcriptInner.getBoundingClientRect();
      const left = Math.min(Math.max(text.left, col.left + 16), col.right - 16), right = Math.max(left, Math.min(text.right, col.right - 16));
      workspace.style.setProperty("--compose-x", `${(left + right) / 2 - ws.left}px`);
      workspace.style.setProperty("--compose-w", `${right - left}px`);
      workspace.style.setProperty("--compose-bottom", `${ws.bottom - col.bottom + 16}px`);
      // The field's cap is a share of the column, so the box never takes most of a short one.
      workspace.style.setProperty("--compose-cap", `${Math.round(Math.max(42, Math.min(264, col.height * 0.35)))}px`);
      // One line at rest, growing with the draft at THIS width to the cap, then scrolling: measured
      // from no height at all, so a shorter draft or a wider box shrinks it again.
      const kept = composeInput.scrollTop;
      composeInput.style.height = "0px";
      composeInput.style.height = `${composeInput.scrollHeight}px`;
      composeInput.scrollTop = kept;
      const box = composer.getBoundingClientRect(), jumpHome = byId("jumpToBottom").offsetParent;
      deskRoom = Math.round(box.height + 16);
      workspace.style.setProperty("--compose-room", `${deskRoom}px`);
      // The jump to the end sits above the box, measured from its own container's foot.
      if (jumpHome) workspace.style.setProperty("--compose-lift", `${Math.round(jumpHome.getBoundingClientRect().bottom - (col.bottom - deskRoom)) + 10}px`);
    } else deskRoom = null;
    // The end moved under a reader who is following it: keep them there, as new records would.
    if (deskRoom !== was && viewport.following) viewport.convergeBottom();
  };
  // A column that changes size or a text width that changes (a pane, the outline, Wide transcript,
  // the window) moves the box in the next frame: writing it inside the observer's delivery would
  // resize what it watches and loop. The text column's height changes with every record drawn,
  // and that alone moves nothing.
  const deskSizes = new WeakMap();
  const deskObserver = new ResizeObserver(entries => {
    let moved = false;
    for (const { target, contentRect: r } of entries) {
      const size = target === transcriptInner ? `${r.width}` : `${r.width}x${r.height}`;
      if (deskSizes.get(target) !== size) { deskSizes.set(target, size); moved = true; }
    }
    if (!moved || deskFrame) return;
    deskFrame = requestAnimationFrame(() => { deskFrame = 0; placeDeskCompose(); });
  });
  for (const box of [transcript, transcriptInner]) deskObserver.observe(box);
  const paintCompose = () => {
    dockCompose.hidden = !composeFab.classList.contains("show");
    const draft = !!composeInput.value.trim() || !!composer.dataset.imageDraft;
    // #s298: the session waits on an answer the box can give (the desktop's button says so too).
    const asks = composeFab.classList.contains("asks");
    dockCompose.classList.toggle("has-draft", draft && !asks);
    dockCompose.classList.toggle("asks", asks);
    dockCompose.setAttribute("aria-label", asks ? "Claude is asking: answer it" : draft ? "Write to this session — a draft is waiting" : "Write to this session");
    const keyboardOpen = innerHeight - (window.visualViewport?.height || innerHeight) > 80;
    // Moving a pending button on focus would move it between pointerdown and click.
    const sharedFocus = !!controls.nativeActive && (viewRoot.active() === composeInput || keyboardOpen && (composer.contains(viewRoot.active()) || controls.runtimePanel?.contains(viewRoot.active())));
    sessionView.classList.toggle("composing", PHONE.matches && composer.classList.contains("production-show") && (!controls.nativeActive || sharedFocus));
    if (controls.nativeActive) { controls.fitComposer(); placeDeskCompose(); return; }
    // #s236: the phone's note is the same one line the desktop's caption shows (`composeTargetLine`).
    composeNote.textContent = byId("composeTarget").textContent;
    fitCompose();
    placeDeskCompose();
  };
  new MutationObserver(paintCompose).observe(composeFab, { attributes: true, attributeFilter: ["class"] });
  new MutationObserver(paintCompose).observe(composer, { attributes: true, attributeFilter: ["class"] });
  composeInput.addEventListener("input", paintCompose);
  composer.addEventListener("compose-images", paintCompose);
  composer.addEventListener("compose-dialog", paintCompose);
  // Native request cards are installed lazily; delegate focus from their stable workspace.
  composer.closest(".workspace").addEventListener("focusin", paintCompose);
  composer.closest(".workspace").addEventListener("focusout", () => requestAnimationFrame(paintCompose));
  window.visualViewport?.addEventListener("resize", paintCompose, { signal });
  const placeToolbar = () => {
    if (PHONE.matches) {
      if (shows("reading")) dockPill.append(readingCluster);
      dock.append(searchCluster, searchDone, dockCompose);
    } else {
      topbar.querySelector(".top-spacer").after(searchCluster);
      if (shows("reading")) byId("sessionFoldAll").after(readingCluster);
    }
  };
  placeToolbar();
  paintCompose();

  const menu = document.createElement("div");
  menu.className = "phone-pane-menu";
  menu.id = "phonePaneMenu";
  menu.hidden = true;
  menu.setAttribute("role", "dialog");
  menu.innerHTML = '<div class="phone-pane-head"><strong></strong><button type="button" class="phone-pane-close" aria-label="Close">✕</button></div><div class="phone-pane-jumps" role="toolbar" aria-label="Jump to a state" hidden></div><div class="phone-pane-body"></div>';
  // #s55: the session dims behind an open sheet; a tap there closes it (the outside tap below).
  const paneScrim = document.createElement("div");
  paneScrim.className = "pane-scrim";
  paneScrim.setAttribute("aria-hidden", "true");
  sessionView.append(paneScrim, menu);
  const menuBody = menu.querySelector(".phone-pane-body");
  const jumps = menu.querySelector(".phone-pane-jumps");
  let openPane = null, home = null;
  // #319: the Tasks drop-down lists every state (a phone cannot reach the outline's state filter),
  // so its head jumps between them — each named and counted as the state's own head in the list,
  // which stays pinned at the top while its rows scroll under it (production.css).
  const renderJumps = () => {
    const heads = openPane?.key === "tasks" ? [...menuBody.querySelectorAll(".work-group[data-task-group]")] : [];
    jumps.hidden = !heads.length;
    jumps.innerHTML = heads.map(head => {
      const key = head.dataset.taskGroup, state = key === "in_progress" ? "running" : key;
      const label = head.firstElementChild?.textContent || key, count = head.querySelector(".work-group-count")?.textContent || "";
      return `<button type="button" class="phone-pane-jump" data-jump-group="${escapeText(key)}"><span class="task-state ${escapeText(state)}" aria-hidden="true"></span>${escapeText(label)} <span class="phone-pane-jump-count">${escapeText(count)}</span></button>`;
    }).join("");
  };
  // A state's head at the top of the list, its first task right under it. Measured from that
  // first ROW, since a head that is pinned reads where it is held, not where it stands.
  const jumpTo = key => {
    const head = menuBody.querySelector(`.work-group[data-task-group="${CSS.escape(key)}"]`);
    if (!head) return;
    const first = head.nextElementSibling;
    if (!head.previousElementSibling || !first) { menuBody.scrollTop = 0; return; }
    menuBody.scrollTop += first.getBoundingClientRect().top - menuBody.getBoundingClientRect().top - head.offsetHeight;
  };
  const closePane = () => {
    if (!openPane) return;
    const node = byId(openPane.body);
    if (node && home) home.parent.insertBefore(node, home.next && home.next.parentNode === home.parent ? home.next : null);
    openPane.button.setAttribute("aria-expanded", "false");
    openPane = null;
    home = null;
    menu.hidden = true;
    sessionView.classList.remove("pane-open");
    renderJumps();
  };
  const showPane = pane => {
    closePane();
    const node = byId(pane.body);
    if (!node) return;
    home = { parent: node.parentNode, next: node.nextSibling };
    menuBody.append(node);
    menu.querySelector(".phone-pane-head strong").textContent = pane.label;
    openPane = pane;
    pane.button.setAttribute("aria-expanded", "true");
    menu.hidden = false;
    sessionView.classList.add("pane-open");
    renderJumps();
    menuBody.scrollTop = 0;
    // The current turn in the middle of the list — by moving the drop-down's OWN scroller, never
    // by asking the browser to scroll something into view (the contract's rule for this module).
    const current = node.querySelector(".outline-turn-row.current");
    if (current) {
      const box = menuBody.getBoundingClientRect(), row = current.getBoundingClientRect();
      menuBody.scrollTop += row.top - box.top - (box.height - row.height) / 2;
    }
    // Tasks opens on what is still moving (#186's default, now that the finished ones are listed
    // above it): the first state after Completed, at the top.
    const moving = [...node.querySelectorAll(".work-group[data-task-group]")].find(head => head.dataset.taskGroup !== "completed");
    if (pane.key === "tasks" && moving) jumpTo(moving.dataset.taskGroup);
  };
  panes.forEach(pane => pane.button.addEventListener("click", () => (openPane === pane ? closePane() : showPane(pane))));
  menu.addEventListener("click", event => {
    if (event.target.closest(".phone-pane-close")) { closePane(); return; }
    const jump = event.target.closest("[data-jump-group]");
    if (jump) { jumpTo(jump.dataset.jumpGroup); return; }
    navigatorClick(event);
    // A task's card opens OVER the list, which stays open under it (#319, the owner): closing the
    // card goes back to the list, where the next task is one tap away.
    if (event.target.closest("[data-turn-record], [data-task-record], [data-agent-record], [data-child-outline], [data-worker-session]")) closePane();
  });
  // A tap outside closes the list — unless a task's card is open over it, when the tap is the
  // card's (it closes the card; its own handler), and the list is still there after it.
  // A tap on the dim (#s55) is spent closing: the dim keeps catching until that tap's click has
  // passed, or it would land on whatever lies beneath — a link in the transcript. The click follows
  // the LIFT, so the wait is counted from there (a held tap outlasted a wait from the touch); a
  // gesture the browser took over gets no click and lets go at once; and a lift that never comes
  // still lets go, so the dim can never go on swallowing taps. A press on the dim while it catches
  // is that tap's mouse half (a phone's compatibility events) and holds it until its own click.
  let catchingPointer = null, catchTimer = 0;
  const stopCatching = after => {
    clearTimeout(catchTimer);
    catchTimer = setTimeout(() => paneScrim.classList.remove("catching"), after);
  };
  addEventListener("pointerdown", event => {
    if (event.target === paneScrim && (openPane || paneScrim.classList.contains("catching"))) {
      paneScrim.classList.add("catching");
      catchingPointer = event.pointerId;
      stopCatching(4000);
    }
    if (openPane && taskPopover.hidden && !menu.contains(event.target) && !event.target.closest?.("[data-phone-pane], [data-nav-card-open]")) closePane();
  }, { capture: true, signal });
  const lifted = event => {
    if (event.pointerId !== catchingPointer) return;
    catchingPointer = null;
    stopCatching(event.type === "pointercancel" ? 0 : 1000);
  };
  addEventListener("pointerup", lifted, { capture: true, signal });
  addEventListener("pointercancel", lifted, { capture: true, signal });
  paneScrim.addEventListener("click", event => {
    event.preventDefault();
    clearTimeout(catchTimer);
    paneScrim.classList.remove("catching");
  });
  addEventListener("keydown", event => { if (event.key === "Escape") closePane(); }, { signal });
  // #s431 stage 3 (design/pane-layout.md, "The glyphs in the top bar"): off the phone reader, where the
  // owner's order leaves the outline's glyphs no column of their own, the same three glyphs join the
  // session's top bar, after the title and its tools and before Info and the pane's control; each
  // opens its list as a panel over the transcript's right edge, 290px wide, one at a time (this sheet,
  // drawn as a panel there), and a click elsewhere or Esc closes it. A glyph in the column opens the
  // same panel where the outline cannot open. The panel borrows the outline's own list, so it closes
  // before the outline opens again.
  const columnHomes = new Map();
  placeOutlineGlyphs = arrangement => {
    // The panel lives in the session's card off the phone reader, under its top bar; the phone's
    // sheet in the view, as before. A move between them closes what was open.
    const panelHome = arrangement && !arrangement.phone ? viewRoot.$(".workspace") : sessionView;
    if (panelHome && menu.parentElement !== panelHome) { closePane(); panelHome.append(menu); }
    const inBar = !!arrangement && !arrangement.phone && arrangement.outline === "bar";
    if (inBar && panes[0].button.parentElement !== topbar) for (const pane of panes) info.before(pane.button);
    if (!inBar && panes[0].button.parentElement !== dockPill) dockPill.prepend(...panes.map(pane => pane.button));
    // What the column held besides the glyphs comes with them: the outline's expand control (pressed
    // while the reader wants the outline the width holds back, so they can fold it for good), and the
    // session's Info, from the top bar's own Info control, its card hanging under the bar.
    const expand = byId("navigatorRailExpand"), card = viewRoot.$("#infoPopover");
    for (const [node, into, before] of [[expand, topbar, info], [card, viewRoot.$(".workspace"), null]]) {
      if (!node) continue;
      if (inBar && node.parentElement !== into) {
        if (!columnHomes.has(node)) columnHomes.set(node, { parent: node.parentNode, next: node.nextSibling });
        if (before) before.before(node); else into.append(node);
      } else if (!inBar && columnHomes.has(node)) {
        const home = columnHomes.get(node);
        columnHomes.delete(node);
        home.parent.insertBefore(node, home.next && home.next.parentNode === home.parent ? home.next : null);
      }
    }
    sessionView.classList.toggle("outline-in-bar", inBar);
    if (arrangement?.outline === "open") closePane();
  };
  openOutlinePanel = key => {
    const pane = panes.find(p => p.key === key);
    if (!pane) return false;
    if (openPane === pane) closePane(); else showPane(pane);
    return true;
  };
  placeOutlineGlyphs(layout.arrangement());
  // The card's own jump to the turn leaves for the transcript: the list goes with the card.
  taskPopover.addEventListener("click", event => { if (event.target.closest("[data-task-record], [data-task-find]")) closePane(); });


  // `--phone-top` (the bar's bottom) places the sheets that hang from the top, `--phone-bottom` (the
  // dock's top, from the screen's foot) the sheets that rise from the dock.
  // Both are measured against the APP's box, not the window's: while the keyboard is up iOS reports an
  // `innerHeight` that shrinks as it scrolls the page, and the app is moved back over that scroll.
  const place = () => {
    const box = grid.getBoundingClientRect();
    const bottom = Math.round(topbar.getBoundingClientRect().bottom - box.top);
    if (bottom > 0) writeStyle(viewRoot.styleRoot(), "--phone-top", `${bottom}px`);
    const rise = Math.round(box.bottom - dock.getBoundingClientRect().top);
    if (getComputedStyle(dock).display !== "none" && rise > 0) writeStyle(viewRoot.styleRoot(), "--phone-bottom", `${rise}px`);
  };
  new ResizeObserver(place).observe(topbar);
  new ResizeObserver(place).observe(dock);
  addEventListener("resize", place, { signal });
  // #s37 (design/phone-landscape.md §3.5): the dock rides the keyboard. iOS never lifts a fixed bottom
  // element above its keyboard — the layout viewport keeps its height and the keyboard covers its foot
  // — so the find pill would type behind it. What the keyboard (and its accessory bar) covers is the
  // app's height less the visible one; `--phone-kb` lifts the dock by that much, and only the dock:
  // the transcript keeps its height (a moving foot would move the reader, #372).
  // #s41 (the owner's iPhone): iOS also SCROLLS THE PAGE to bring the focused field above its keyboard
  // — measured on iOS 26, 322px upright, the find field being at the foot when it is focused — and the
  // top bar, the drawer's handle and a wide phone's column head went off the top of the screen. Pinning
  // the page back with `scrollTo` fights iOS (it scrolls again, the view jitters), so the app is moved
  // back over the scroll instead (`translateY` of the visual viewport's page offset) and looks as if
  // nothing scrolled; the field, lifted above the keyboard, gives iOS no reason to scroll again. The
  // transform makes the app the box its fixed children are placed in, which is the screen's box here.
  // #s451: a short visual viewport is the keyboard only while a field that takes one has focus.
  // Measured on the iOS 26.4 Simulator: after the software keyboard had been up once, a FRESH Safari
  // tab with focus on the body and no keyboard on screen reported `visualViewport.height` 336 against
  // an `innerHeight` of 714, at 6 s and at 15 s alike, and the dock and the toast floated 378px up the
  // screen over nothing. Whether a real iPhone does the same is not known; the guard holds either way,
  // since iOS raises a keyboard (or a select's picker) only for a focused field. An iframe counts, as
  // its inside cannot be seen. The page-scroll compensation below is left as it was: it answers
  // iOS's scroll, not the keyboard.
  const NO_KEYBOARD_INPUTS = new Set(["button", "checkbox", "radio", "range", "color", "file", "image", "reset", "submit", "hidden"]);
  const typesInto = () => {
    let at = viewRoot.active();
    while (at?.shadowRoot?.activeElement) at = at.shadowRoot.activeElement;
    if (!at) return false;
    if (at.localName === "input") return !NO_KEYBOARD_INPUTS.has(at.type);
    return at.localName === "textarea" || at.localName === "select" || at.localName === "iframe" || !!at.isContentEditable;
  };
  const viewport2 = window.visualViewport;
  if (viewport2) {
    const root = viewRoot.styleRoot();
    const ride = () => {
      const typing = typesInto();
      if (controls.nativeActive && PHONE.matches) {
        const frame = sharedKeyboardFrame(grid.getBoundingClientRect().height, viewport2);
        // Keep one containing block even at zero offset; Safari pans during keyboard animation.
        writeStyle(grid, "transform", Math.abs(viewport2.scale - 1) < 0.01 ? `translateY(${frame.shift}px)` : "");
        sessionView.classList.toggle("keyboard-up", typing && frame.keyboardOpen);
        writeStyle(root, "--phone-kb", `${typing ? frame.inset : 0}px`);
        place();
        // The shared listener can run first; fit again after applying the current viewport frame.
        controls.fitComposer();
        return;
      }
      const steady = PHONE.matches && Math.abs(viewport2.scale - 1) < 0.01;
      const shortfall = steady ? Math.max(0, Math.round(root.clientHeight - viewport2.height)) : 0;
      const covered = typing ? shortfall : 0;
      const shift = steady ? Math.max(0, Math.round(viewport2.pageTop)) : 0;
      // Once the keyboard is down there is nothing to fight: the page goes back to the top. This reads
      // the viewport as it is, unguarded, so it acts only as it did before #s451.
      if (steady && !shortfall && shift) scrollTo(0, 0);
      writeStyle(grid, "transform", shift ? `translateY(${shift}px)` : "");
      sessionView.classList.toggle("keyboard-up", covered > 0);
      writeStyle(root, "--phone-kb", `${covered}px`);
      place();
    };
    viewport2.addEventListener("resize", ride, { signal });
    viewport2.addEventListener("scroll", ride, { signal });
    // The page itself scrolls only under iOS's keyboard; off a phone this listener has nothing to do.
    addEventListener("scroll", () => { if (PHONE.matches || grid.style.transform) ride(); }, { passive: true, signal });
    // Focus moving may come with no resize of the visual viewport (a stale shortfall stays short, and
    // a keyboard rising into it leaves the height as it was), so the ride is asked again once focus
    // has settled where it went, whether it left a field or came to one.
    for (const type of ["focusin", "focusout"]) document.addEventListener(type, () => requestAnimationFrame(ride), { signal });
    ride();
  }
  // Across the breakpoint the Tasks list changes what it shows (#319: every state on a phone); the
  // desktop's rail comes off or goes back on in the app (#s33, `applySidebar`).
  PHONE.addEventListener("change", () => { placeToolbar(); place(); if (!PHONE.matches) closePane(); renderNavigator(); paintCompose(); }, { signal });
  // Into the wide layout the drop-down has no button (the column shows its panes); out of it, the
  // app sends the column's borrowed list home and the handle speaks of the drawer again.
  WIDE.addEventListener("change", () => { if (WIDE.matches) closePane(); place(); }, { signal });
  place();

  mobileShell = {
    closePane,
    paneNode: key => { const pane = panes.find(p => p.key === key); return pane ? byId(pane.body) : null; },
    counts(turns, tasks, agents) {
      for (const [pane, n] of [[panes[0], turns], [panes[1], tasks], [panes[2], agents]]) {
        const badge = pane.button.querySelector(".phone-pane-count");
        badge.hidden = !n;
        // #339: a long session's turns and tasks run to hundreds; 99+ hid how many.
        badge.textContent = n > 999 ? "999+" : String(n);
        // A wide phone's column switch shows the same count on its tab (the app's).
        options.paneCount?.(pane.key, n ? badge.textContent : "");
      }
    },
    tasksChanged: renderJumps,
  };
}

// The first render: the top bar, the outline and the jump, for no session yet.
renderHeader(); renderNavigator(); paintJump();
// From here the pane's layout changes remeasure the transcript (`paneLayoutChanged`), from the width
// the mount left: the one read before the pane shut itself is not the transcript's any more.
paneLaidOutWidth = laidOutWidth();
viewMounted = true;
// The mount's hold on its transitions ends after that read, which laid the view out as built.
element.classList.remove("sv-mounting");
// #114: a reload, a close or the tab going to the background saves the reader's choices with
// the position (the debounced save may not have run yet).
addEventListener("pagehide", () => viewport.remember(), { signal });
document.addEventListener("visibilitychange", () => { if (document.hidden) viewport.remember(); }, { signal });
// Once the view is unmounted every member of its handle does nothing and answers nothing (stage 7):
// a host still holding it, or a late callback of its own, reaches nothing of a view that is gone. The
// key actions are fenced one by one, and the compose box's controls read as none.
function fenced(handle) {
  const fence = run => function (...args) { return unmounted ? undefined : run.apply(this, args); };
  for (const [name, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(handle))) {
    if (name === "unmount") continue;
    if (descriptor.get) Object.defineProperty(handle, name, { ...descriptor, get: fence(descriptor.get) });
    else if (typeof descriptor.value === "function") handle[name] = fence(descriptor.value);
    else if (descriptor.value && typeof descriptor.value === "object") handle[name] = Object.fromEntries(Object.entries(descriptor.value).map(([key, run]) => [key, typeof run === "function" ? fence(run) : run]));
  }
  return handle;
}
// What the view said while it mounted (its first title, a toast) goes to the host once
// `mountSessionView` has returned, in the order it was said (`say`).
queueMicrotask(() => {
  const held = mountingEvents;
  mountingEvents = null;
  for (const [handler, data] of held) say(handler, data);
});

return fenced({
  /** A session was chosen: the view takes it before the app writes its address — the loading veil,
   *  the pane's tabs, the top bar and the compose box. */
  select(id) { sessionLoading?.show(); preview.setSession(id); renderHeader(); controls.paint(); },
  /** Remember the reader's choices of the view now (view-state.js `persistView`), as the view does
   *  after each one. The app asks for it whenever it remembers its own (stage 7), as its one
   *  remembering wrote both halves' before: so the outline's first state (a phone's default, the Info
   *  pane taken out of the stack) reaches storage at the same moments it always did. */
  persist: () => persist(),
  /** …and reads its records. */
  open(id) { recordState.session = id; queueTurn(null); viewport.beginSession(id); recordStore.open(id); },
  /** The session whose records the view reads ("" for none). */
  session: () => recordStore.session,
  /** The selected session is gone: the view stops reading it and says why. */
  close(title, detail) { sessionLoading?.hide(); recordStore.stop(); preview.setSession(""); recordState.session = ""; queueTurn(null); viewport.showEmpty(title, detail, true); },
  /** The index has new rows: the top bar and the compose box follow, and so does the Agents pane. */
  indexChanged() {
    renderHeader(); controls.paint();
    // The open session's workers (#s53) are index rows, not records: their arrival and their state
    // reach the Agents pane on the index's poll, which redraws the outline only when they changed.
    if (recordState.session && workersKey(sessionWorkers()) !== lastWorkers) renderNavigator();
  },
  /** The host's write mode changed (`write.on()`): the compose box follows. */
  writeChanged: () => ordinaryControls.paint(),
  /** Take the view away (stage 7). The reader's place in the session is remembered, as a page leaving
   *  remembers it; then everything the view started stops (its listeners on the page, its timers,
   *  frames and observers, the record store's reads and polls, the viewport's and the artifact
   *  pane's own, the compose box's and the downloads', whose work still out does nothing when it
   *  comes back), what it put outside its element goes (the task card and its scrim, a list a wide
   *  phone's column borrowed, the engine's globals for a bug report), the host's value is put back
   *  wherever the view's is still the one it wrote (the page root's measured properties, the `grid`'s
   *  transform; `viewWrote`), and the element is left as the host gave it: empty, with its own attributes. The view tells its host
   *  nothing more, and every other member of the handle does nothing (`fenced`). Mounting again in
   *  the same element draws a new view. Native mode, which only the app installs and the app never
   *  unmounts, is not taken down. */
  unmount() {
    if (unmounted) return;
    viewport.remember();
    unmounted = true;
    recordStore.stop();
    viewport.destroy();
    preview.destroy();
    ordinaryControls.destroy();
    attachmentViewer.destroy();
    sessionLoading?.hide();
    life.abort();
    for (const id of timers) clearTimeout(id);
    for (const id of frames) cancelAnimationFrame(id);
    for (const observer of observers) observer.disconnect();
    timers.clear(); frames.clear(); observers.clear();
    taskScrim.remove(); taskPopover.remove(); floating.clear();
    for (const [, , id] of PHONE_PANES) { const node = byId(id); if (node && !element.contains(node)) node.remove(); }
    for (const { node, style, values } of hostStyles) {
      for (const [name, value, priority] of values) {
        const wrote = viewWrote.get(node)?.get(name);
        if (wrote === undefined || node.style.getPropertyValue(name) !== wrote) continue;
        if (value) node.style.setProperty(name, value, priority); else node.style.removeProperty(name);
      }
      if (style === null && !node.getAttribute("style")) node.removeAttribute("style");
    }
    element.replaceChildren();
    for (const { name } of [...element.attributes]) element.removeAttribute(name);
    for (const [name, value] of elementBefore) element.setAttribute(name, value);
  },
  /** The view's width changed under it (the sidebar moved). */
  resized: () => paneLayoutChanged(),
  /** Draw in "light" or "dark" (#s436); mdrev's pane and the diagrams follow the view's element. */
  setTheme: applyTheme,
  /** The view's key actions, by the keymap's names. */
  keys: keyActions,
  /** A phone's pane sheet closes; a pane's list, by key, for a wide phone's column to borrow; and a
   *  click inside a borrowed list, which the outline answers. */
  closePane: () => mobileShell.closePane(),
  paneNode: key => mobileShell.paneNode(key),
  outlineClick: event => navigatorClick(event),
  /** The compose box's controls, ordinary or native: native mode hooks the session list too. */
  get controls() { return controls; },
  /** Install native routing (the monitor's own): `index` hooks the app's session index. */
  async installNativeControls(Controller, status, nativeStyles, menuMount, index) {
    const controller = new Controller(ordinaryControls, { ...controlActions, nativeStyles,
      nativeDisabled: () => {
        recordStore.emptySession = null; recordStore.sessionFailure = null;
        index.disabled();
      },
      nativeEnabled: () => {
        index.enabled(controller);
        recordStore.emptySession = session => controller.isEmptySession(session);
        recordStore.sessionFailure = session => controller.sessionReadFailure(session);
      }
    }, menuMount);
    controls = controller;
    await controller.adopt(status);
    return controller;
  },
});
}
