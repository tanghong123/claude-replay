// The web app as one mount (#s551, design/app-mount.md, stage 2): `mountApp` draws the app shell
// (#s429, design/session-view-component.md, stage 3; #s430, design/session-list-component.md, stage 2)
// into an element: the app around the session list and one session view. It keeps what is the app's
// — the drawer and the sidebar's frame, the brand and its version, the theme, write mode, the session
// search layer (⌘K) and the toast — and places the session list (session-list-view.js) and the session
// view (session-view.js) in it, mounting each as a host would. The monitor's page is its first host
// (app.js); the kit carries it for the native app from a second entry, `native-app.js`, which no host
// is offered (design/app-mount.md, "Kept for the native app, as `frame` is").
import { svg } from "./icons.js";
import { checkAssets, useBackend } from "./backend.js";
import { mergedBackend } from "./merged-source.js";
import { machineShortLabel } from "./session-list.js";
import { APP_SHELL_MARKUP } from "./app-shell-markup.js";
import { WIDE_PHONE_QUERY } from "./shared/capabilities.js";
import { controlState, indexState, persist as persistIndex, searchState, selectedRow } from "./state.js";
import { sessionFilterBuckets, FILTER_BUCKETS, FILTER_LABELS } from "./shared/state-labels.js";
import { bindKeymap, hintFor } from "./shared/keymap.js";
import { parseJumpQuery } from "./jump-query.js";
import { escapeText } from "./view-model.js";
import { mountSessionList, OPTIONAL_CONTROLS as LIST_CONTROLS } from "./session-list-view.js";
import { mountSessionView, OPTIONAL_CONTROLS as VIEW_CONTROLS, PHONE_PANES } from "./session-view.js";
import { page, viewRoot } from "./view-root.js";
import { arrangeApp, GLIDE_MS, LIST_MIN } from "./pane-layout.js";

/** Every option `mountApp` takes; any other name is refused (stage 2: one server; stage 3, #s595:
 *  several machines and the assets the app carries; stage 4, #s596: the native app's own). Each is in
 *  the kit's list beside `VIEW_WITHHELD` too (`kit.rs`), which holds it out of the guide and the
 *  declarations: no host is offered the mount. */
export const MOUNT_OPTIONS = Object.freeze(["backend", "machines", "assets", "version", "theme", "show", "onMachines", "reviewSheet", "frame"]);

/** The theme the app opens in (#s596): the reader's stored "dark"; with nothing stored, the start
 *  theme the app passed ("light" or "dark"); else null, the page's own, as a stored "light" leaves it. */
export const themeToStart = (stored, start) => (stored === "dark" ? "dark" : stored == null && (start === "dark" || start === "light") ? start : null);

// The session view's handle and the session list's, once each is mounted (below). Module-wide, since
// native mode's activation reaches them after the mount (`installNativeController`). The view's is
// declared first: the drawer's set-up runs before the view mounts and asks it to close a sheet that
// cannot be open yet; the list's options read it as the list draws.
let viewHandle = null;
let listHandle = null;
// Whether the page holds the web app already: it is drawn once per page and never taken away.
let appMounted = false;

/** Draw the web app into `element`, a wrapper the page gives it (design/app-mount.md, "One entry
 *  point, once per page"): the app's `#app` and its three fixed layers (⌘K's, the confirm dialog and
 *  the toast) go in as siblings, as the page's body held them, and the wrapper draws no box of its own
 *  (`.app-mount`). The element must be on the page and empty (nothing in it but white space), and the
 *  page must hold no `#app` of its own, since the app finds its elements by id. It returns nothing: no
 *  member changes a mounted app. It is called once per page and never undone, since the web app is
 *  built on what a page has one of (its stores, the page's backend handle, mdrev's bundle, the ids it
 *  finds its elements by, the document's keymap, the address and the storage keys); a second call
 *  throws, and a refused call draws nothing, so it may be made again.
 *
 *  Options, each named in `MOUNT_OPTIONS`; any other name is refused. It takes `backend` or
 *  `machines`, one of the two:
 *  - `backend`: the server the web app reads, a backend handle (`backendHandle()`). The page's
 *    components read it from the start (`useBackend`), the list's index, write mode's pairing and the
 *    view alike.
 *  - `machines` (the native app's, #s595, design/app-mount.md, "One list over every machine"): the
 *    machines it reads, `[{ machine, label, handle }]` as `mergedBackend` takes them, never empty. The
 *    list reads all of them at once (`mergedBackend`), each row and project head naming its machine;
 *    the view is mounted on the first machine's handle until a row opens, and mounted again on a
 *    row's own machine when the reader opens a row of another, so every call it makes goes to the
 *    machine the session runs on. A remount loses what the view's unmount does (the draft, the
 *    pane's tabs, the search). The ids the list and the view pass each other are mapped (the address
 *    and the list hold `<machine>/<id>`, the view the machine's own); write mode follows the open
 *    session's machine (off, with no session open); ⌘K's rows name their machine. It takes `assets`.
 *  - `assets`: where mdrev's bundle and the Mermaid runtime load from when the app carries them
 *    itself, `{ mdrev, mdrevVersion, mermaid }`, passed to the view as it is (the view's doc comment
 *    says each); `machines` requires it, since no one machine's bundle may serve every machine. The
 *    mount names the runtime in the page's head where the page names none.
 *  - `version`: the release the brand names (`v<version>`), "dev" without one.
 *
 *  The native app's own (stage 4, #s596, design/app-mount.md, "Kept for the native app"), each
 *  optional:
 *  - `theme`: "light" or "dark", the theme to start in where the reader has stored none (the native
 *    app sets it from the device's appearance); a stored one wins, and the theme toggle stores the
 *    reader's choice as on the monitor's page.
 *  - `show`: the optional controls the app switches off, by name, as `{ detach: false }`: the view's
 *    (`reading`, `history`, `detach`) and the list's (`filter`, `hiding`, `arrangements`), each passed
 *    on to the part that declares it; true or false only.
 *  - `onMachines`: the function a Machines row calls, in the sidebar's nav after Search, and a glyph
 *    on the rail after the filter (Hong on #s553: "A Machines row in the sidebar (native app only)"):
 *    the native app's way into its pairing and machines screens. Neither is drawn without it.
 *  - `reviewSheet`: passed on to the view (Hong on #s555): `true` offers the pane's Review and its
 *    sheet at every width, where a phone reader alone is offered them otherwise.
 *  - `frame`: passed on to the view (its doc comment says it): the native app's frame for an
 *    agent's HTML page, on a scheme of its own. */
export function mountApp(element, options = {}) {
if (appMounted) throw new Error("mountApp draws the web app once per page, and this page holds it already");
/** A value as a refusal names it: its JSON, or its kind where it has none (a function, a BigInt). */
const said = value => { try { return JSON.stringify(value) ?? `a ${typeof value}`; } catch { return `a ${typeof value}`; } };
if (options === null || typeof options !== "object" || Array.isArray(options)) throw new Error(`mountApp takes its options as an object, not ${said(options)}`);
for (const name of Object.keys(options)) if (!MOUNT_OPTIONS.includes(name)) throw new Error(`mountApp takes no option named ${name}; its options are ${MOUNT_OPTIONS.join(", ")}`);
if (options.backend != null && options.machines != null) throw new Error("mountApp reads one server (its backend) or several machines (its machines), not both");
if (options.backend == null && options.machines == null) throw new Error("mountApp needs its backend: the server the web app reads, as a backend handle (backendHandle()), or the machines it reads");
if (options.backend != null && typeof options.backend.fetch !== "function") throw new Error(`mountApp takes its backend as a backend handle (backendHandle()), not ${said(options.backend)}`);
if (options.machines != null && !(Array.isArray(options.machines) && options.machines.length)) throw new Error(`mountApp takes its machines as a list of { machine, label, handle }, never empty, not ${said(options.machines)}`);
if (options.machines != null && options.assets == null) throw new Error("mountApp takes its machines with its assets: where mdrev's bundle and the Mermaid runtime load from, which no one machine may serve for every machine");
checkAssets(options.assets ?? undefined, "mountApp");
if (options.version != null && typeof options.version !== "string") throw new Error(`mountApp takes its version as a string, the release its brand names, not ${said(options.version)}`);
if (options.theme != null && options.theme !== "light" && options.theme !== "dark") throw new Error(`mountApp takes its theme as "light" or "dark", the theme to start in, not ${said(options.theme)}`);
if (options.show != null && (typeof options.show !== "object" || Array.isArray(options.show))) throw new Error(`mountApp takes show as an object naming the controls it switches off, as { detach: false }, not ${said(options.show)}`);
for (const [name, shown] of Object.entries(options.show || {})) {
  if (!VIEW_CONTROLS.includes(name) && !LIST_CONTROLS.includes(name)) throw new Error(`mountApp has no optional control named ${name}; the controls it switches off are ${[...VIEW_CONTROLS, ...LIST_CONTROLS].join(", ")}`);
  if (typeof shown !== "boolean") throw new Error(`mountApp shows ${name} or not by true or false, not ${said(shown)}`);
}
if (options.onMachines != null && typeof options.onMachines !== "function") throw new Error(`mountApp takes onMachines as the function its Machines row calls, not ${said(options.onMachines)}`);
if (options.reviewSheet != null && typeof options.reviewSheet !== "boolean") throw new Error(`mountApp offers the review sheet at every width or not by true or false, not ${said(options.reviewSheet)}`);
if (options.frame != null && typeof options.frame !== "function") throw new Error(`mountApp takes its frame as a function from a page's HTML to the address its frame loads, not ${said(options.frame)}`);
if (!element) throw new Error("mountApp draws into an element the page gives it, and none was given");
if (!element.isConnected) throw new Error("mountApp draws into an element on the page, and this one is not on it");
if (page.byId("app")) throw new Error("mountApp draws the web app's #app, and this page holds one already");
if ([...element.childNodes].some(node => !(node.nodeType === 3 && !node.data.trim()))) throw new Error("mountApp draws into an empty element; this one holds the host's nodes");
const merged = options.machines ? mergedBackend(options.machines) : null;
appMounted = true;
useBackend(options.backend || options.machines[0].handle);
element.classList.add("app-mount");
element.innerHTML = APP_SHELL_MARKUP;
// The view's diagrams load the runtime the page's head names; the app carries its own (`assets`), and
// its page need not name one (#s595).
if (options.assets && !page.$('meta[name="mermaid-runtime"]')) {
  const meta = document.createElement("meta");
  meta.name = "mermaid-runtime";
  meta.content = options.assets.mermaid;
  page.$("head").append(meta);
}
// Over several machines (#s595), the machine the view is mounted on now; "" for the one server.
let viewMachine = merged ? options.machines[0].machine : "";
/** A machine's handle; the one server's for "". Taken as the app mounts, as the merged source takes
 *  its own: a host that changes its list afterwards changes neither. */
const handles = new Map((options.machines || []).map(entry => [entry.machine, entry.handle]));
const handleOf = machine => (merged ? handles.get(machine) : options.backend);
/** Where a list id is read: its machine and the machine's own id (`<machine>/<id>` over several
 *  machines, the id itself for the one server); null for an id no machine of the list holds. */
const place = id => {
  if (!merged) return { machine: "", id };
  const where = merged.locate(id);
  return where && { machine: where.machine, id: where.id };
};
/** The optional controls the app switched off that a part declares (`show`, #s596), for that part. */
const showFor = controls => Object.fromEntries(Object.entries(options.show || {}).filter(([name]) => controls.includes(name)));
// Whether the reader wants the outline, as the view says it (`onOutline`).
let viewOutlineWanted = true;
/** Remember the app's choices, and the view's with them (#s429 stage 7): the one `persist` wrote both
 *  halves' until the view kept its own, so the view's are written at the same moments as before. */
function persist() { persistIndex(); viewHandle?.persist(); }
const app = page.byId("app");
// The session view's own element (#s429, design/session-view-component.md): where the view builds
// its three parts from its own markup (the workspace, the pane's resizer and the artifact pane,
// stage 4) and the state it keeps for itself (stage 2: composing, a phone sheet open, the keyboard up,
// wrapped code, a wide transcript). The app's shell (`app-shell-markup.js`) carries it empty. In the app
// it is the second column of `#app`'s grid, a box that lays its three parts out in a grid of its own
// (#s431 stage 1, design/pane-layout.md); the pane's open, full and resizing states are copied onto
// `#app` too (`paneLayout`), for the app's rules that still read them there (a drag's cursor, a narrow
// window's pane over the session, and a phone's pane beside it).
const sessionView = page.byId("sessionView");
let toastTimer = 0;

// Every glyph the app's markup names (the session view draws its own as it builds its markup).
page.$$("[data-icon]").forEach(element => { element.innerHTML = svg(element.dataset.icon); });
// The sidebar head's controls (#75, #76): the generated shell gives the theme toggle no glyph — an
// empty 34px button, invisible until hovered — and the sidebar collapse beside it was easy to miss in
// that company. Each is named and drawn here; the list draws its own, Collapse every group among them.
for (const [id, icon, label] of [["themeBtn", "moon", "Toggle light and dark"], ["sidebarCollapse", "sidebar", "Collapse the session list into its icon rail"]]) {
  const button = page.byId(id);
  if (!button) continue;
  if (!button.querySelector("svg")) button.innerHTML = svg(icon);
  button.setAttribute("aria-label", label);
  if (id !== "sidebarCollapse") button.title = label;
}
page.$(".brand small").textContent = `v${options.version || "dev"}`;
// The tab's icon (#203) and the Home Screen's (#320) are declared by the production page head
// (`page-head.html`), not injected here: iOS reads the head to find the icon for a Home Screen
// shortcut, and the extracted shell — the hand-written demo, kept byte-identical — declares none.
function toast(message) {
  const element = page.byId("toast"); element.textContent = message; element.classList.add("show");
  clearTimeout(toastTimer); toastTimer = setTimeout(() => element.classList.remove("show"), 1900);
}

// The session list (session-list-view.js, #s430): its element is `#sessionList`, which the page's shell
// carries empty in the sidebar where the tree stood (stage 3), a flex column there (#s431 stage 1); the list
// builds its markup in it and keeps its arrangement on it. Mounted here, before the drawer and the
// view, where its code ran when it was app.js's, so what it puts on the page (the rail's flyout in the
// body, its listeners, the parts it asks the app to place) comes in the order it did. What it needs of
// the view (native mode's projects) is read as it draws, once the view is mounted; it starts reading
// the index at the end, as it did.
// The rail's filter as the list places it (`placeListPart`), for the Machines glyph to follow (#s596).
let railFilter = null;
listHandle = mountSessionList(page.byId("sessionList"), {
  // Over several machines the list reads all of them at once (#s541's merged source, #s595).
  ...(merged ? { backend: merged } : {}),
  // The page's theme as it stands; the stored one, put on the root below, reaches the list through the
  // root's observer, as every later change does.
  theme: page.html().dataset.theme === "dark" ? "dark" : "light",
  // The optional controls the native app switched off (#s596); every one stays on for the monitor's
  // page. The app keeps the page's keyboard (`keyboard: "page"`, the keymap below), as it does for
  // the view.
  show: showFor(LIST_CONTROLS),
  keyboard: "page",
  onToast: toast,
  persist,
  onOpen: ({ id, push }) => selectSession(id, push),
  onIndex: () => indexArrived(),
  projectTree: (filtered, all) => viewHandle.controls.projectTree?.(filtered, all),
  projectActions: () => viewHandle.controls.projectActions,
  newSession: project => viewHandle.controls.newSession?.(project),
  // The rail's flyout floats in the body, a section's menu and its scrim in the app, as they did.
  layer: kind => (kind === "flyout" ? page.body() : app),
  place: placeListPart,
});
if (options.onMachines) drawMachines(options.onMachines);

/** The native app's way into its own screens (#s596, Hong on #s553: "A Machines row in the sidebar
 *  (native app only)"): a Machines row in the nav after Search, which shows at every width the nav does
 *  (the drawer and a wide phone's column), and a glyph on the rail, which hides the nav, after the
 *  list's filter (after Search where the app switched the filter off). Each calls `onMachines`; drawn
 *  after the list, so its rail filter is in place. */
function drawMachines(onMachines) {
  const glyph = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/></svg>';
  const row = Object.assign(document.createElement("button"), { type: "button", className: "navbtn", id: "machinesBtn" });
  row.innerHTML = `<span>${glyph}</span><span class="label">Machines</span>`;
  const rail = Object.assign(document.createElement("button"), { type: "button", className: "sidebar-mini-button", id: "sidebarMiniMachines", title: "Machines" });
  rail.innerHTML = glyph;
  rail.setAttribute("aria-label", "Machines");
  row.onclick = rail.onclick = () => onMachines();
  page.byId("searchBtn").after(row);
  (railFilter || page.byId("sidebarMiniSearch")).after(rail);
}

/** Where the list's parts go in the sidebar (stage 3), node for node where the list put them when it
 *  found the sidebar itself: its strip of head controls in the head's one row before the sidebar's
 *  collapse (Collapse every group, Expand every group, the filter; the strip draws no box, stage 4),
 *  the filter's sheet under the head, the
 *  rail's filter after its search, and the agent marks at the rail's end. Called as the list mounts,
 *  so it reads nothing declared below the mount. */
function placeListPart(part, node) {
  if (part === "controls") page.byId("sidebarCollapse").before(node);
  else if (part === "sheet") page.$("#app>.sidebar>.side-head").append(node);
  else if (part === "rail-filter") page.byId("sidebarMiniSearch").after(railFilter = node);
  else if (part === "marks") page.byId("sidebarMini").append(node);
}

/** The index has new rows (the list draws them next): the app's half. Before #s430 the tree was drawn
 *  between a gone selection and the view told; nothing here reads the drawn tree. */
function indexArrived() {
  if (indexState.selected && indexState.selectedWasRow && !selectedRow()) sessionGone();
  // The view follows the index: its top bar, its compose box, and the open session's workers.
  viewHandle.indexChanged();
  if (!indexState.selected) {
    // A requested id is honoured even when it is not a list row: a sub-agent child never is,
    // and a link to one must open it, not the first session. A bad id shows "Cannot read
    // this session" from the record store rather than silently opening something else.
    const query = new URLSearchParams(location.search);
    const requested = query.get("session") || viewHandle.controls.sessionIdForThread?.(query.get("thread"));
    // A native UUID needs the explicitly enabled catalog before it can resolve to a rollout.
    // Keep ordinary session links authoritative and never silently replace a native link.
    if (query.get("thread") && !query.get("session") && !requested) return;
    const first = requested || [...indexState.rows.values()].find(row => !row.hidden && !row.startedBy)?.id;
    if (first) selectSession(first, false);
  }
}

function sessionUrl(id) {
  const url = new URL(location.href); url.searchParams.set("session", id);
  // A `ui=` left from when there were two shells is dropped (#s98).
  url.searchParams.delete("ui");
  url.hash = ""; return url;
}
/** Open a session: one the reader chose in the list (`onOpen`), or one the app chose (the history,
 *  ⌘K, the view's parent control, the first session). The list's half (`select`: the selection, its
 *  read mark, remembered and drawn), then the app's: the view, then the address. */
function selectSession(id, push) {
  if (!id) return;
  // On a phone choosing a session closes the drawer AT ONCE (#313) — the one already open too —
  // and the top bars take the new session before a byte of it has arrived.
  mobileShell?.setDrawer(false);
  const where = place(id);
  // An id no machine of the list holds (a bare id, a machine gone from the list) is still the
  // selection, as a single server's unreadable id is: the index's next answer does not ask for it
  // again (and shut a phone's drawer each time), and a row chosen after it opens.
  if (!where) { listHandle.select(id); viewHandle.close("Cannot read this session", "No machine of this list holds it."); paintWrite(); return; }
  if (id === indexState.selected && where.machine === viewMachine && viewHandle.session() === where.id) return;
  listHandle.select(id);
  showSession(id, push);
}
/** The app's half of a selection: the view, then the address. Over several machines (#s595) a row of
 *  another machine than the view's mounts the view again on that machine first, and the view takes
 *  the machine's own id, where the list and the address keep the list's. */
function showSession(id, push) {
  const where = place(id);
  if (where.machine !== viewMachine) remountOn(where.machine);
  // The view takes the selection before the address does: its loading veil, the pane's tabs, its
  // top bar and its compose box (a parent's Back is written from the top bar, `parentShown`).
  viewHandle.select(where.id);
  const url = new URL(location.href); url.searchParams.set("session", id); url.searchParams.delete("ui"); if (push) url.hash = ""; history[push ? "pushState" : "replaceState"]({}, "", url);
  viewHandle.open(where.id);
  paintWrite();
}
/** Mount the view again on `machine` (#s595): a confirm still open is cancelled and the wide phone's
 *  column gives back the pane's list it borrowed first, then the view goes as its unmount takes it (the draft, the tabs, the search, a send
 *  still out), and comes back on that machine's handle, which every call it makes then goes to. */
function remountOn(machine) {
  // A grant's confirm still open names the old machine's pane: it is cancelled, as a newer ask would,
  // and a passcode typed for that machine is not offered to the next.
  confirming?.(null);
  const passcode = page.$(".passcode-input"); if (passcode) passcode.value = "";
  mobileShell?.showSessions();
  viewHandle.unmount();
  viewMachine = machine;
  mountView();
}
function sessionGone() { indexState.selected = ""; viewHandle.close("Session is gone", "It may have been deleted or moved. The list keeps scanning."); paintWrite(); }
/** The browser's Back to the parent the view's top bar offers (#82, the classic view's
 *  `synthesizeBack`): a deep-linked child gets a history entry for its parent, once. */
let synthesizedFor = "";
function parentShown(parentId) {
  if (synthesizedFor === indexState.selected || history.length > 1) return;
  try { const child = location.href; history.replaceState({}, "", sessionUrl(parentId)); history.pushState({}, "", child); } catch (_) {}
  synthesizedFor = indexState.selected;
}
function openGlobalSearch() { page.byId("searchLayer").classList.add("production-open"); page.byId("searchInput").value = ""; renderGlobalSearch(); page.byId("searchInput").focus(); }
/** ⌘K is a JUMP-TO (#294, design/global-search.md): agents, projects and sessions by name, ranked
 *  by recency. Finding text inside the open session is the top bar's job — the Transcript tab that
 *  used to do it here, badly (plain substring, no scopes, no stepping, and it had to apologise for
 *  covering one session), is gone.
 *
 *  A row the SIDEBAR is currently hiding is still listed — its filter does not narrow this list —
 *  and marked `stale` so it reads dimmed; choosing one clears that filter (the reader picked the
 *  row, so it is their own act, not a filter moving underneath them).
 *
 *  The query: plain words match names; `agent:` and `project:` narrow, the same `key:value` shape
 *  as the in-session `tool:`. An in-session scope prefix and `tool:` tokens mean nothing here and
 *  are dropped rather than matched as text. */
function globalRows(raw) {
  // ⌘K's own query (jump-query.js): `agent:` and `project:`, the rest matched against names. It
  // shares nothing with the session box's grammar, whose filters mean nothing here (owner).
  const jq = parseJumpQuery(raw);
  const agentQ = { values: jq.agents }, projectQ = { values: jq.projects };
  const text = jq.text;
  const now = Date.now() / 1000;
  // Why the sidebar is not showing a session right now, in the words of its own controls: "hidden"
  // (the reader hid it, or its project) and the filter bucket the reader has unticked ("idle").
  // Empty when the sidebar shows it. The row is listed either way, dimmed, with these words.
  const concealed = (row, project) => {
    const why = [];
    if (!indexState.showHidden && (row.hidden || project.hidden)) why.push("hidden");
    const buckets = sessionFilterBuckets(row, now);
    if (listHandle.filtered() && !buckets.some(bucket => indexState.buckets.has(bucket))) why.push(FILTER_LABELS[buckets[0]].toLowerCase());
    return why;
  };
  const ts = row => Number(row.activityTs) || 0;
  // Over several machines a project and its sessions name their machine as the list does (#s561's
  // short label, #s595); an agent's row spans machines and names none.
  const machines = merged ? merged.machines() : [];
  const labels = machines.map(entry => entry.label);
  const on = session => {
    const label = machines.find(entry => entry.machine === session?.machine)?.label;
    return label ? ` · ${machineShortLabel(label, labels)}` : "";
  };
  const rows = [];
  for (const agent of listHandle.grouped()) {
    const agentSessions = agent.projects.flatMap(project => project.sessions);
    const newest = list => [...list].sort((a, b) => ts(b) - ts(a))[0];
    const projectOf = new Map(agent.projects.flatMap(project => project.sessions.map(session => [session.id, project])));
    const agentAsked = !agentQ.values.length || agentQ.values.some(v => agent.name.toLowerCase().includes(v) || agent.id.toLowerCase().includes(v));
    // A `project:` query is about projects and what is in them, so the agent row is not an answer
    // to it — listing it would put a row above the rows the reader narrowed to.
    if (agentAsked && !projectQ.values.length) {
      const top = newest(agentSessions);
      if (top) rows.push({ kind: "agent", label: agent.name, meta: `${agent.projects.length} project${agent.projects.length === 1 ? "" : "s"}`, sid: top.id, ts: ts(top), why: concealed(top, projectOf.get(top.id)) });
    }
    for (const project of agent.projects) {
      if (projectQ.values.length && !projectQ.values.some(v => project.name.toLowerCase().includes(v) || (project.path || "").toLowerCase().includes(v))) continue;
      if (!agentAsked) continue;
      // A project row lands on its OWN most recent session (owner, 2026-09-26): the project is the
      // address, its newest session is where the reader means to be.
      const top = newest(project.sessions);
      if (top) rows.push({ kind: "project", label: project.name, meta: `${agent.name}${on(top)}`, sid: top.id, ts: ts(top), why: concealed(top, project) });
      for (const session of project.sessions) {
        rows.push({ kind: "session", label: session.name || session.id, meta: `${agent.name} · ${project.name}${on(session)}`, sid: session.id, ts: ts(session), why: concealed(session, project) });
      }
    }
  }
  const match = row => !text || `${row.label} ${row.meta}`.toLowerCase().includes(text);
  // Most recently active first — the list opens on what the reader was just doing — and a row with
  // no activity stamp sorts by name rather than by chance.
  return rows.filter(match).sort((a, b) => b.ts - a.ts || a.label.localeCompare(b.label));
}
function renderGlobalSearch() {
  const query = page.byId("searchInput").value.trim();
  const rows = globalRows(query).slice(0, 80);
  searchState.globalResults = rows; searchState.globalIndex = Math.min(searchState.globalIndex, Math.max(0, rows.length - 1));
  page.byId("searchResults").innerHTML = rows.map((row, index) => `<button class="search-result ${index === searchState.globalIndex ? "active" : ""}${row.why.length ? " search-result-stale" : ""}" data-global-index="${index}"${row.why.length ? ` title="The sidebar is not showing this (${escapeText(row.why.join(", "))}) — opening it shows it there again"` : ""}><span class="result-copy"><b>${escapeText(row.label)}</b><small>${escapeText(row.meta)}</small></span>${row.why.length ? `<span class="result-why">${escapeText(row.why.join(" · "))}</span>` : ""}<span class="result-kind">${escapeText(row.kind)}</span></button>`).join("") || '<div class="no-results">No matches</div>';
}
page.byId("searchBtn").onclick = openGlobalSearch; page.byId("sidebarMiniSearch").onclick = openGlobalSearch;
page.byId("searchInput").oninput = () => { searchState.globalIndex = 0; renderGlobalSearch(); };
/** Go to a ⌘K row — by click, or Enter on the highlighted one. */
function chooseGlobal(index) {
  const row = searchState.globalResults[index]; if (!row) return;
  page.byId("searchLayer").classList.remove("production-open");
  // A row the sidebar is not showing: opening it clears what hides it — the bucket filter to All
  // (owner, 2026-09-26), and Include hidden for a hidden one — or the reader lands in a session
  // the list beside them does not show.
  if (row.why.includes("hidden")) { indexState.showHidden = true; listHandle.redraw(); }
  if (row.why.some(why => why !== "hidden")) listHandle.setBuckets(FILTER_BUCKETS);
  if (row.sid) selectSession(row.sid, true);
}
page.byId("searchResults").onclick = event => { const item = event.target.closest("[data-global-index]"); if (item) chooseGlobal(Number(item.dataset.globalIndex)); };
// A jump-to is done from the keyboard: ↑/↓ move the highlight, Enter goes. The demo's own handler
// never runs here, so without this Enter did nothing (#294's gap, found by #296's case).
page.byId("searchInput").onkeydown = event => {
  const n = searchState.globalResults.length;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault(); if (!n) return;
    searchState.globalIndex = (searchState.globalIndex + (event.key === "ArrowDown" ? 1 : n - 1)) % n;
    renderGlobalSearch();
    // Keep the highlight in view by moving the LIST's own offset — never scrollIntoView, which the
    // contract bans from this file because the browser may move the transcript with it.
    const list = page.byId("searchResults"), active = list.querySelector(".search-result.active");
    if (active) {
      const r = active.getBoundingClientRect(), l = list.getBoundingClientRect();
      if (r.top < l.top) list.scrollTop -= l.top - r.top;
      else if (r.bottom > l.bottom) list.scrollTop += r.bottom - l.bottom;
    }
  } else if (event.key === "Enter") { event.preventDefault(); chooseGlobal(searchState.globalIndex); }
};
page.byId("searchLayer").onclick = event => { if (event.target === page.byId("searchLayer")) page.byId("searchLayer").classList.remove("production-open"); };
// The theme is the app's: its chrome reads it on the page's root, and the list and the view, which
// each carry their own on their element (#s430 stage 3, #s429 stage 5b), are told together, one value.
const tellTheme = theme => { listHandle.setTheme(theme); viewHandle?.setTheme(theme); };
page.byId("themeBtn").onclick = () => { const dark = page.html().dataset.theme !== "dark"; page.html().dataset.theme = dark ? "dark" : ""; tellTheme(dark ? "dark" : "light"); localStorage.setItem("am-demo-theme", dark ? "dark" : "light"); };
// The page's theme is the root's `data-theme`, however it changes (the toggle, a script, the stored
// theme just below): the list and the view follow it, as everything on the page did while they read
// the root's.
new MutationObserver(() => tellTheme(page.html().dataset.theme === "dark" ? "dark" : "light")).observe(page.html(), { attributes: true, attributeFilter: ["data-theme"] });
// The reader's stored theme, else the app's start theme (#s596), else the page's own.
const openTheme = themeToStart(localStorage.getItem("am-demo-theme"), options.theme);
if (openTheme === "dark") page.html().dataset.theme = "dark";
else if (openTheme === "light") delete page.html().dataset.theme;
// #s33: the collapsed rail is the DESKTOP's. On a phone the sidebar is the drawer, and the rail's
// rules hide its head, its nav and its list: a phone that had the rail stored (1.355 drew a phone
// held sideways as a desktop, and the owner collapsed it there) opened a blank drawer with no
// control left to expand it. So the class never goes on there; the choice stays remembered, and a
// window grown past the phone wears it again.
// The rail is the list drawn small (stage 4): the app decides when, and tells the list, which opens its
// flyouts only then and closes one open when the sidebar comes back.
//
// #s431 stage 3 (design/pane-layout.md, "The owner's order"): the app lays out its list from the window,
// beside what the view needs (`arrangeApp`): the drawer (`app-drawer`, the phone's own list, or a wide
// phone's column held sideways), the rail (`sidebar-off`), or the list at a width, which narrows toward
// 232px before the view's outline folds and gives way to the rail before the session or the pane does.
// The reader's collapse is remembered (`indexState.sidebarOpen`) and a wider window never undoes it; a
// fold the width forced comes back once there is room. A decision of the app's never glides
// (`app-holding`); the reader's own collapse and expand do.
const coarseScreen = matchMedia("(pointer:coarse)");
let listArranged = null, listGlideUntil = 0;
/** What the order reads for the list: the window, the screen, the reader's width and collapse, and what
 *  the view has said of itself (its pane, and whether the reader wants its outline). */
const listInputs = () => ({
  width: page.html()?.clientWidth || innerWidth,
  coarse: coarseScreen.matches,
  short: innerHeight <= 500,
  list: readerListWidth(),
  collapsed: !indexState.sidebarOpen,
  view: { paneOpen: !app.classList.contains("preview-off"), outlineWanted: viewOutlineWanted },
});
/** Lays the list out by the order, and tells the view its box changed (`resized`, which remeasures the
 *  transcript once its width settles), except for the view's own pane and outline: the view remeasures
 *  for those itself, from the arrangement it is about to make. */
function arrangeList({ remeasure = true } = {}) {
  const decided = arrangeApp(listInputs());
  if (listArranged && JSON.stringify(decided) === JSON.stringify(listArranged)) return;
  // A window narrowed into the drawer finds it shut, over nothing: it opens from its handle.
  const intoDrawer = !!listArranged && listArranged.list !== "drawer" && decided.list === "drawer";
  listArranged = decided;
  if (intoDrawer) mobileShell?.setDrawer(false);
  const hold = performance.now() >= listGlideUntil;
  if (hold) app.classList.add("app-holding");
  app.classList.toggle("app-drawer", decided.list === "drawer");
  if (typeof decided.list === "number") drawListWidth(decided.list);
  // The drawer is a width of its own (`min(86vw, 360px)`), never one the order gave the list: a list
  // narrowed toward 232px kept its `sidebar-tight` into the drawer, whose 10px head padding then
  // put the brand under the ☰ handle (#s583).
  else if (decided.list === "drawer") app.classList.remove("sidebar-tight");
  app.classList.toggle("sidebar-off", decided.list === "rail");
  listHandle.compact(app.classList.contains("sidebar-off"));
  paintRailExpand();
  app.getBoundingClientRect();
  if (hold) app.classList.remove("app-holding");
  if (remeasure) viewHandle?.resized();
}
// The rail's expand is disabled where the list cannot open beside what the view needs (its title says
// why; `aria-disabled`, so the title still shows); the drawer's handle always opens the drawer.
function paintRailExpand() {
  const expand = page.byId("sidebarMiniExpand"), blocked = !!listArranged && !listArranged.canOpen;
  expand.setAttribute("aria-disabled", String(blocked));
  expand.title = blocked ? "Too narrow to open the sidebar beside the session: widen the window" : `Expand the sidebar  ( ${hintFor("sidebar-toggle")} )`;
}
function toggleSidebar(open) {
  if (open && listArranged && !listArranged.canOpen) return;
  indexState.sidebarOpen = open;
  listGlideUntil = performance.now() + GLIDE_MS;
  arrangeList();
  persist();
}
page.byId("sidebarCollapse").onclick = () => toggleSidebar(false); page.byId("sidebarMiniExpand").onclick = () => toggleSidebar(true);
page.byId("sidebarCollapse").title = `Collapse the sidebar into its icon rail  ( ${hintFor("sidebar-toggle")} )`;
addEventListener("resize", () => arrangeList());
coarseScreen.addEventListener("change", () => arrangeList());
// #96: the session list is resizable, the way the preview pane is — a handle on the sidebar's
// own right edge. The width is the viewer's: it is written to `--sidebar-user` (production.css
// feeds it to the grid only above the mobile breakpoint, so a phone keeps its full-width
// sheet), remembered across reloads, reset by a double-click and nudged by the arrow keys.
const SIDEBAR_WIDTH_KEY = "am-sidebar-width";
const SIDEBAR_MIN = 232, SIDEBAR_MAX = 520, SIDEBAR_DEFAULT = 300;
// Where the head's controls stop fitting at their full size (#202/#210): the row is the shell
// switch plus five 34px glyphs with 1px between them, and the head's padding either side.
const SIDEBAR_TIGHT = 272;
const sidebarResizer = document.createElement("div");
sidebarResizer.className = "sidebar-resizer";
sidebarResizer.id = "sidebarResizer";
sidebarResizer.setAttribute("role", "separator");
sidebarResizer.setAttribute("aria-orientation", "vertical");
sidebarResizer.setAttribute("aria-label", "Resize the session list");
sidebarResizer.setAttribute("aria-valuemin", String(SIDEBAR_MIN));
sidebarResizer.setAttribute("aria-valuemax", String(SIDEBAR_MAX));
sidebarResizer.tabIndex = 0;
page.$(".sidebar").append(sidebarResizer);
// Two widths, kept apart (#s431 stage 3): the READER's, which the drag, the arrow keys and storage
// keep, and the one the list is GIVEN, which the owner's order narrows toward 232px for the view
// (`arrangeList`), and which the head's tightness and the room the app could give the view read.
let readerWidth = SIDEBAR_DEFAULT;
const clampWidth = value => {
  // A drag that reaches x=0 is a drag to the minimum, not a request for the default: 0 is a
  // width, so only a value that is not a number at all falls back.
  const asked = Number(value);
  return Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, Math.round(Number.isFinite(asked) ? asked : SIDEBAR_DEFAULT)));
};
readerWidth = clampWidth(localStorage.getItem(SIDEBAR_WIDTH_KEY) || SIDEBAR_DEFAULT);
sidebarResizer.title = `Drag to resize the session list · double-click for ${SIDEBAR_DEFAULT}px`;
function readerListWidth() { return readerWidth; }
/** The list drawn at the width it is given. */
function drawListWidth(width) {
  page.html().style.setProperty("--sidebar-user", `${width}px`);
  // Below `SIDEBAR_TIGHT` the head's own row — the shell switch and five glyphs — is wider than
  // the sidebar, and the last of them (the sidebar collapse) would sit outside it. The CSS class
  // squeezes the glyphs and the head's padding so every control stays reachable at the narrowest
  // width the reader can drag to, which is what #202's sixth glyph cost (measured: the row is
  // 237px wide and needs 275 with the head's padding, against a 232px minimum).
  app.classList.toggle("sidebar-tight", width < SIDEBAR_TIGHT);
  sidebarResizer.setAttribute("aria-valuenow", String(width));
}
/** The widest the list may be drawn at now: the widest reader's width the owner's order draws as asked,
 *  so the list's edge takes room from the session and the pane down to M each and stops before a part
 *  would fold, or, on a touch screen, before the view would become the phone reader (#s478). */
function widestList() {
  if (typeof listArranged?.list !== "number") return SIDEBAR_MAX;
  const inputs = listInputs();
  for (let width = SIDEBAR_MAX; width > SIDEBAR_MIN; width--) if (arrangeApp({ ...inputs, list: width }).list === width) return width;
  return SIDEBAR_MIN;
}
/** The list's width as drawn, which a drag and the arrow keys move from. */
function sidebarWidth() { return typeof listArranged?.list === "number" ? listArranged.list : readerWidth; }
function setSidebarWidth(value, remember) {
  // The edge stops at the widest the order draws. A press or a drag past it asks for nothing the order
  // would draw, so it keeps a reader's width the window narrows, and never lowers it to what is drawn.
  const asked = clampWidth(value), limit = widestList();
  readerWidth = asked <= limit ? asked : Math.max(limit, readerWidth);
  sidebarResizer.title = `Drag to resize the session list · double-click for ${SIDEBAR_DEFAULT}px`;
  if (remember) localStorage.setItem(SIDEBAR_WIDTH_KEY, String(readerWidth));
  arrangeList();
  return readerWidth;
}
sidebarResizer.addEventListener("pointerdown", event => {
  if (!indexState.sidebarOpen) return;
  event.preventDefault();
  // A synthetic pointer (a test, an assistive tool) has no capture to take — the drag is driven
  // from the window either way, so a refused capture must not end it.
  try { sidebarResizer.setPointerCapture(event.pointerId); } catch (_) {}
  sidebarResizer.classList.add("dragging"); app.classList.add("resizing");
  let width = readerWidth;
  const move = moved => { width = setSidebarWidth(moved.clientX, false); };
  const up = () => {
    removeEventListener("pointermove", move); removeEventListener("pointerup", up);
    sidebarResizer.classList.remove("dragging"); app.classList.remove("resizing");
    localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  };
  addEventListener("pointermove", move); addEventListener("pointerup", up);
});
// The double-click names the default: it is the reader's width from then on, drawn as the window allows.
sidebarResizer.ondblclick = () => {
  readerWidth = SIDEBAR_DEFAULT;
  localStorage.setItem(SIDEBAR_WIDTH_KEY, String(readerWidth));
  arrangeList();
};
sidebarResizer.onkeydown = event => {
  const step = event.shiftKey ? 32 : 8;
  if (event.key === "ArrowLeft") setSidebarWidth(sidebarWidth() - step, true);
  else if (event.key === "ArrowRight") setSidebarWidth(sidebarWidth() + step, true);
  else return;
  event.preventDefault();
};
addEventListener("popstate", () => { const id = new URLSearchParams(location.search).get("session"); if (id && id !== indexState.selected) selectSession(id, false); });

// #313 — the phone shell (the owner: "it does not feel native at all"), its app half: the drawer.
//  - The session list is a DRAWER over part of the session view, never all of it. A handle fixed at
//    the top left opens and closes it, and stays where it is either way — it is a sibling of the
//    drawer in #app, not inside the top bar, whose layer sits under the drawer. A tap on the dimmed
//    part of the view closes it; choosing a session closes it at once (`selectSession`).
//    `mobile-detail` keeps its meaning from #310: present = the drawer is shut.
// The dock, its sheets and the keyboard ride are the session view's (session-view.js).
var mobileShell;
{
  const WIDE = matchMedia(WIDE_PHONE_QUERY);
  const handle = document.createElement("button");
  handle.type = "button";
  handle.className = "drawer-handle";
  handle.id = "drawerHandle";
  // ☰ for the drawer; held sideways on a wide phone, where it hides and shows a COLUMN, the sidebar
  // glyph, as an iPad keeps its sidebar button by the content (`labelHandle` swaps them).
  const DRAWER_GLYPH = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16"/></svg>';
  handle.innerHTML = DRAWER_GLYPH;
  const scrim = document.createElement("div");
  scrim.className = "drawer-scrim";
  scrim.id = "drawerScrim";
  app.append(scrim, handle);

  // #s37 (design/phone-landscape.md §3.2): on a WIDE phone held sideways the session list is a
  // column beside the session, and the handle hides and shows it. That is its own remembered choice
  // (`am-phone-column`): the drawer's `mobile-detail` and the desktop's rail mean other things, and
  // #s33 was one layout's state governing another.
  const COLUMN_KEY = "am-phone-column";
  let columnOff = (() => { try { return localStorage.getItem(COLUMN_KEY) === "0"; } catch (_) { return false; } })();
  const labelHandle = () => {
    const glyph = WIDE.matches ? svg("sidebar") : DRAWER_GLYPH;
    if (handle.dataset.glyph !== (WIDE.matches ? "column" : "drawer")) { handle.innerHTML = glyph; handle.dataset.glyph = WIDE.matches ? "column" : "drawer"; }
    const open = WIDE.matches ? !columnOff : !app.classList.contains("mobile-detail");
    const words = WIDE.matches ? (open ? "Hide the session list" : "Show the session list") : open ? "Close the session list" : "Open the session list";
    handle.setAttribute("aria-expanded", String(open));
    handle.title = words;
    handle.setAttribute("aria-label", words);
  };
  const setColumn = open => {
    columnOff = !open;
    try { localStorage.setItem(COLUMN_KEY, open ? "1" : "0"); } catch (_) {}
    app.classList.toggle("phone-column-off", columnOff);
    labelHandle();
  };
  app.classList.toggle("phone-column-off", columnOff);
  // The drawer keeps its own state on a wide phone too — choosing a session still shuts it — so a
  // turn to portrait finds it as a phone leaves it; only the column is shown in its place.
  const setDrawer = open => {
    app.classList.toggle("mobile-detail", !open);
    labelHandle();
    if (open) viewHandle?.closePane();
  };
  handle.onclick = () => (WIDE.matches ? setColumn(columnOff) : setDrawer(app.classList.contains("mobile-detail")));
  scrim.addEventListener("click", () => setDrawer(false));
  // Esc closes an open drawer (#s431 stage 3: a mouse reader's drawer too), unless a layer took it.
  addEventListener("keydown", event => {
    if (event.key !== "Escape" || event.defaultPrevented || !app.classList.contains("app-drawer") || app.classList.contains("mobile-detail") || WIDE.matches) return;
    setDrawer(false);
  });
  setDrawer(!app.classList.contains("mobile-detail"));

  // The column's switcher: Sessions, or one of the outline's panes, holding the SAME live list the
  // drop-downs borrow (moved in while shown, back after) — one renderer, one click handler. A row
  // chosen here moves the transcript and leaves the column as it is, as Mail does. Glyphs with their
  // counts, in the head's one row before the sidebar's own glyphs (the owner: glyphs, "so the side bar
  // width can be relatively small"; no brand row, which only took the height).
  const columnSwitch = document.createElement("div");
  columnSwitch.className = "column-switch";
  columnSwitch.id = "columnSwitch";
  columnSwitch.setAttribute("role", "tablist");
  columnSwitch.setAttribute("aria-label", "Show in the column");
  // Sessions wears a conversation, never the sidebar glyph: that one means "hide the sidebar" on every
  // desktop and iPad, and the owner tapped it to hide the column and read the tab as dead (#s41).
  const SESSIONS_GLYPH = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M20 11.5a7.5 7.5 0 0 1-11 6.6L4 19.5l1.4-4.3A7.5 7.5 0 1 1 20 11.5Z"/></svg>';
  columnSwitch.innerHTML = [["sessions", "Sessions", null], ...PHONE_PANES.map(([key, label]) => [key, label, key])]
    .map(([key, label, icon]) => `<button type="button" class="iconbtn${key === "sessions" ? " on" : ""}" role="tab" data-column-tab="${key}" title="${label}" aria-label="${label}" aria-selected="${key === "sessions"}">${icon ? svg(icon) : SESSIONS_GLYPH}<span class="column-switch-count"></span></button>`)
    .join("");
  const columnPane = document.createElement("div");
  columnPane.className = "column-pane phone-pane-body";
  columnPane.id = "columnPane";
  const sideHead = page.$("#app>.sidebar>.side-head");
  sideHead.querySelector(".head-actions").before(columnSwitch);
  sideHead.after(columnPane);
  let columnNode = null, columnHome = null;
  const returnColumnList = () => {
    if (columnNode && columnHome) columnHome.parent.insertBefore(columnNode, columnHome.next && columnHome.next.parentNode === columnHome.parent ? columnHome.next : null);
    columnNode = null;
    columnHome = null;
  };
  const showColumnTab = key => {
    returnColumnList();
    app.classList.toggle("column-pane-on", key !== "sessions");
    for (const tab of columnSwitch.querySelectorAll("[data-column-tab]")) {
      const on = tab.dataset.columnTab === key;
      tab.classList.toggle("on", on);
      tab.setAttribute("aria-selected", String(on));
    }
    const node = viewHandle.paneNode(key);
    if (!node) return;
    viewHandle.closePane();
    columnHome = { parent: node.parentNode, next: node.nextSibling };
    columnNode = node;
    columnPane.append(node);
    // The current turn in the middle of the column, by its OWN scroller (the contract's rule).
    columnPane.scrollTop = 0;
    const current = node.querySelector(".outline-turn-row.current");
    if (current) {
      const box = columnPane.getBoundingClientRect(), row = current.getBoundingClientRect();
      columnPane.scrollTop += row.top - box.top - (box.height - row.height) / 2;
    }
  };
  columnSwitch.addEventListener("click", event => { const tab = event.target.closest("[data-column-tab]"); if (tab) showColumnTab(tab.dataset.columnTab); });
  columnPane.addEventListener("click", event => viewHandle.outlineClick(event));
  // Out of the wide layout the column's borrowed list goes home and the handle speaks of the drawer
  // again (the view closes its drop-down going in).
  WIDE.addEventListener("change", () => { if (!WIDE.matches) showColumnTab("sessions"); labelHandle(); });

  mobileShell = {
    setDrawer,
    /** The column shows its sessions, and the pane's list it borrowed goes back to the view (#s595: before
     *  the view is mounted again on another machine). */
    showSessions: () => showColumnTab("sessions"),
    /** What the handle does: the drawer opens or shuts, or a wide phone's column shows or hides. */
    toggle: () => handle.onclick(),
    /** A pane's count on the column switch's tab, as the dock's badge reads it. */
    paneCount(key, text) {
      const tab = columnSwitch.querySelector(`[data-column-tab="${key}"] .column-switch-count`);
      if (tab) tab.textContent = text;
    },
  };
}

// Write mode is the app's (#s429 stage 7): its switch in the sidebar, the rail's write button, the
// help that says writing needs a paired monitor, and the confirm layer that grants a live pane. The
// view hears whether writing is on and asks for the confirm through its `write` option, and is told
// when the switch moves (`writeChanged`). Native mode wraps the switch's handler (native-entry.js,
// native-mode.js), so it is bound before either loads.
// #s577: where this reader's writes cannot reach the monitor, the switch is drawn disabled and says
// why under it (`writeReason`), the first thing missing as the server named it (`data-write`): a
// switch that turned on and could not send looked like a broken one. A click on it, or on ⓘ, opens
// the longer help.
const WRITE_NEED = {
  monitor: {
    line: "Needs a paired monitor: <code>agent-monitor --pair</code>",
    help: "<b>Write mode needs a paired monitor</b><p>Stop the monitor and run this in a terminal:</p><code>agent-monitor --pair</code>",
  },
  browser: {
    line: "This browser is not paired with the monitor",
    // A monitor a LaunchAgent starts prints no address to its log (#s204): `--pair` in a terminal
    // hands off to the running monitor and prints the address that carries the token.
    help: "<b>This browser is not paired</b><p>The monitor is paired, but this browser holds no pairing token, so the monitor would refuse what it sends. Run this in a terminal on this machine: it prints the address that pairs a browser, and opens it in the default one.</p><code>agent-monitor --pair</code>",
  },
  host: {
    line: "This page only reads sessions",
    help: "<b>This page does not write</b><p>The server that serves it offers reading only.</p>",
  },
  // #s595: over several machines, writing follows the machine the open session runs on, so with no
  // session open there is none to follow.
  session: {
    line: "Open a session to write to it",
    help: "<b>Open a session to write to it</b><p>Write mode follows the machine the open session runs on.</p>",
  },
};
const writeReason = document.createElement("div");
writeReason.id = "writeReason";
writeReason.className = "write-reason";
page.byId("writeBtn").after(writeReason);
const writeReasonText = need => { const el = document.createElement("div"); el.innerHTML = need.line; return el.textContent; };
/** What writing still needs for the session open now, as `WRITE_NEED` names it, or "" where this
 *  reader's writes reach its server (#s577). Over several machines (#s595) that is the open session's
 *  machine's, and with no session open, a session. A handle that does not say its writes reach the
 *  server (`writes`) is read as needing pairing, as the view reads it (control-store.js). */
function writeNeedNow() {
  if (merged && place(indexState.selected)?.machine !== viewMachine) return "session";
  const handle = merged ? handleOf(viewMachine) : controlState;
  return handle.writes ? "" : handle.writeNeed || "monitor";
}
/** The words for what writing needs now, or null where it is open. */
const writeNeedWords = () => { const word = writeNeedNow(); return word ? WRITE_NEED[word] || WRITE_NEED.monitor : null; };
/** Whether writing is on for the session open now. */
const writeOn = () => controlState.write && (!merged || !writeNeedNow());
function paintWrite() {
  const need = writeNeedWords();
  page.byId("writeSwitch").setAttribute("aria-checked", String(writeOn()));
  page.byId("writeSwitch").setAttribute("aria-disabled", String(!!need));
  page.byId("writeBtn").classList.toggle("on", writeOn());
  page.byId("writeBtn").classList.toggle("disabled", !!need);
  page.byId("sidebarMiniWrite").classList.toggle("on", writeOn());
  page.byId("sidebarMiniWrite").classList.toggle("disabled", !!need);
  page.byId("sidebarMiniWrite").title = need ? `Write mode: ${writeReasonText(need)}` : "Write mode";
  page.byId("writeInfo").hidden = !need;
  writeReason.hidden = !need;
  writeReason.innerHTML = need ? need.line : "";
}
function showPairHelp() {
  const help = page.byId("writeHelp");
  const need = writeNeedWords();
  help.innerHTML = need ? need.help
    : "<b>Monitor is paired</b><p>With write mode on, a writable session shows a compose box. A live terminal still needs its own grant.</p>";
  help.hidden = false; help.classList.add("show");
}
page.byId("writeSwitch").onclick = () => {
  if (writeNeedNow()) { showPairHelp(); return; }
  controlState.write = !controlState.write;
  paintWrite();
  viewHandle.writeChanged();
};
page.byId("writeBtn").onclick = event => { if (!event.target.closest("button")) page.byId("writeSwitch").click(); };
page.byId("writeInfo").onclick = event => { event.stopPropagation(); showPairHelp(); };
// The confirm layer answers the view's last ask: the passcode given ("" with no field) on Confirm,
// null on Cancel, and null for an ask a newer one replaced. Its passcode field, once drawn, stays.
let confirming = null;
function confirmWrite({ title, text, passcode }) {
  confirming?.(null);
  const layer = page.byId("confirmLayer"), dialog = layer.querySelector(".dialog");
  layer.querySelector("h3").textContent = title;
  page.byId("confirmText").textContent = text;
  let input = dialog.querySelector(".passcode-input");
  if (passcode && !input) { input = document.createElement("input"); input.className = "passcode-input"; input.type = "password"; input.placeholder = "Monitor passcode"; dialog.insertBefore(input, dialog.querySelector(".dialog-actions")); }
  layer.classList.add("production-open");
  if (passcode) input.focus();
  return new Promise(resolve => {
    confirming = answer => { confirming = null; layer.classList.remove("production-open"); resolve(answer); };
  });
}
page.byId("cancelSend").onclick = () => { if (confirming) confirming(null); else page.byId("confirmLayer").classList.remove("production-open"); };
page.byId("confirmSend").onclick = () => confirming?.(page.$(".passcode-input")?.value || "");
paintWrite();

// The session view (session-view.js): its three parts are columns of this grid, and it draws into
// the page, as it does in every host (design/session-view-component.md, #s438). What it needs from
// the app is passed in here; what the app asks of it is the handle. Every optional control stays on
// unless the native app switched it off (`show`, #s596), and the app keeps the page's keyboard
// (`keyboard: "page"`, the keymap below).
// The list is laid out before the view mounts, so the view's first decision (and the outline's default
// for a reader with none remembered) reads the box it will have beside the list.
arrangeList();
mountView();
/** Mount the session view in its element, with what the app passes it: on the one server, or over
 *  several machines (#s595) on the machine it is mounted on now, with the list's rows and the ids it
 *  passes back mapped between the list's and the machine's own (`forView`). */
function mountView() {
const rows = merged ? merged.forView(listHandle, viewMachine) : null;
viewHandle = mountSessionView(viewRoot.element(), {
  theme: page.html().dataset.theme === "dark" ? "dark" : "light",
  backend: handleOf(viewMachine),
  assets: options.assets ?? undefined,
  // The native app's (#s596): the controls it switched off, Review at every width, and its frame.
  show: showFor(VIEW_CONTROLS),
  reviewSheet: options.reviewSheet ?? undefined,
  frame: options.frame ?? undefined,
  grid: app,
  keyboard: "page",
  write: { on: writeOn, confirm: confirmWrite },
  // A tab of its own (#s433): the server's own pages, one page at three addresses that say what it
  // shows (`/detached?session=…` for the transcript's ↗; `/markdown?…` and `/image?…` for the pane's,
  // as before it). The native app turns every ↗ off with `show: { detach: false }` (#s596) until it
  // opens windows of its own.
  detachTo: ({ href }) => href,
  selectedId: rows ? rows.selectedId : () => listHandle.selected(),
  sessionRow: rows ? rows.sessionRow : id => listHandle.row(id),
  sessionWorkers: rows ? rows.sessionWorkers : id => listHandle.workers(id),
  selectSession: rows ? (id, push) => selectSession(rows.listId(id), push) : selectSession,
  renderList: () => listHandle.redraw(),
  refreshIndex: () => listHandle.refresh(),
  pauseIndex: () => listHandle.stop(),
  resumeIndex: () => listHandle.start(),
  parentShown: rows ? id => parentShown(rows.listId(id)) : parentShown,
  openList: () => app.classList.remove("mobile-detail"),
  openSidebar: () => toggleSidebar(true),
  // The pane opening or shutting can change the list (the owner's order): the app arranges it at once,
  // before the view decides in the same task, so the view decides at its new width.
  paneLayout: (name, on) => { app.classList.toggle(name, on); if (name === "preview-off") arrangeList({ remeasure: false }); },
  paneCount: (key, text) => mobileShell?.paneCount(key, text),
  // What the view tells its host (stage 6). The view still draws its own top bar, title, turn and
  // Info included (the app's phone bar takes them at embedding's step 6), and the app has no use yet
  // for a file opened or a link copied: today's page does nothing with any of them. It shows a toast.
  onTitle: () => {},
  onTurn: () => {},
  onInfo: () => {},
  onFile: () => {},
  onLinkCopied: () => {},
  onToast: toast,
  // The reader's choice of the outline (#s431 stage 3), which the app keeps to arrange its list by.
  onOutline: ({ wanted }) => { viewOutlineWanted = wanted; arrangeList({ remeasure: false }); },
  // The room the app could give the view: its list's width less 232 while the list is shown (it
  // narrows for the outline but never folds for it), and none on the rail or in the drawer.
  room: () => (typeof listArranged?.list === "number" ? listArranged.list - LIST_MIN : 0),
});
}
// The rail's write button reaches the write switch itself (#54): the demo clicks the write
// BUTTON, whose handler ignores a click that lands on a button — a programmatic click does.
// Shared Controls already binds this rail action to native session creation.
if (!viewHandle.controls.nativeActive) page.byId("sidebarMiniWrite").onclick = () => page.byId("writeSwitch").click();
// The keymap (parity #11): the classic view's keys, resolved by keymap.js — the view's actions from
// its handle, the list's and the sidebar's here.
const appKeys = {
  // With no session open there is no session search: `/` opens ⌘K's jump-to, and ⌘F or Ctrl+F
  // (#s509) is left to the browser's find. The view's answer (`false` while a phone's reply box holds
  // the dock) is the press's.
  "search": event => {
    if (indexState.selected) return viewHandle.keys.search();
    if (event?.metaKey || event?.ctrlKey) return false;
    openGlobalSearch();
  },
  // The key toggles what is shown (#s431 stage 3): the list folds to its rail, the rail opens where the
  // list fits beside the view, and the drawer opens and shuts; a rail the width holds stays as it is.
  "sidebar-toggle": () => (listArranged?.list === "drawer" ? mobileShell?.toggle() : toggleSidebar(listArranged?.list === "rail")),
};
/** A key's action, looked up as the key comes (#s595: the view a key reaches is the one mounted now,
 *  never the first one's keys copied at the mount): the sidebar's toggle, then the list's keys, then
 *  the app's search, then the view's. */
function keyAction(action) {
  if (action === "sidebar-toggle") return appKeys[action];
  if (Object.hasOwn(listHandle.keys, action)) return listHandle.keys[action];
  return action === "search" ? appKeys.search : viewHandle.keys[action];
}
// A key with no action here (a control a host switched off) is left to the page, as the view's own
// binding leaves it; every action the keymap names has one in the app, so nothing changes for it.
bindKeymap(document, target => (target?.closest?.(".tree-row") ? "list" : "view"), (action, event) => { const run = keyAction(action); return run ? run(event) : false; });
addEventListener("keydown", event => { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openGlobalSearch(); } else if (event.key === "Escape") page.byId("searchLayer").classList.remove("production-open"); });

arrangeList();
listHandle.start();
}

/** Install native routing only after an explicit server activation succeeds. Native mode is the
 *  monitor's own: the view installs the controls, and the session index follows them here. */
export async function installNativeController(Controller, status, onDisabled, nativeStyles, menuMount = null) {
  return viewHandle.installNativeControls(Controller, status, nativeStyles, menuMount, {
    enabled: controller => {
      listHandle.setSource({ load: () => controller.loadSessions(), group: (groups, build) => controller.groupSessions(groups, build) });
    },
    disabled: () => {
      listHandle.setSource({ load: null, group: null });
      onDisabled();
    },
  });
}
