// The app shell (#s429, design/session-view-component.md, stage 3; #s430,
// design/session-list-component.md, stage 2): the app around the session list and one session view.
// It keeps what is the app's — the drawer and the sidebar's frame, the brand and its version, the
// theme, write mode, the session search layer (⌘K) and the toast — and places the session list
// (session-list-view.js) and the session view (session-view.js) in it, mounting each as a host would.
import { svg } from "./icons.js";
import { backend } from "./backend.js";
import { PHONE_QUERY, WIDE_PHONE_QUERY } from "./shared/capabilities.js";
import { controlState, indexState, persist as persistIndex, searchState, selectedRow } from "./state.js";
import { sessionFilterBuckets, FILTER_BUCKETS, FILTER_LABELS } from "./shared/state-labels.js";
import { bindKeymap, hintFor } from "./shared/keymap.js";
import { parseJumpQuery } from "./jump-query.js";
import { escapeText } from "./view-model.js";
import { mountSessionList } from "./session-list-view.js";
import { mountSessionView, PHONE_PANES } from "./session-view.js";
import { page, viewRoot } from "./view-root.js";

// The session view's handle, once it is mounted (below). Declared first: the drawer's set-up runs
// before the mount and asks the view to close a sheet that cannot be open yet; the list's options
// read it as the list draws.
let viewHandle = null;
// Whether the reader wants the outline, as the view says it (`onOutline`).
let viewOutlineWanted = true;
/** Remember the app's choices, and the view's with them (#s429 stage 7): the one `persist` wrote both
 *  halves' until the view kept its own, so the view's are written at the same moments as before. */
function persist() { persistIndex(); viewHandle?.persist(); }
const app = page.byId("app");
// The session view's own element (#s429, design/session-view-component.md): where the view builds
// its three parts from its own markup (the workspace, the pane's resizer and the artifact pane,
// stage 4) and the state it keeps for itself (stage 2: composing, a phone sheet open, the keyboard up,
// wrapped code, a wide transcript). The page's shell (`app-shell.html`) carries it empty. In the app
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
page.$(".brand small").textContent = `v${page.body().dataset.version || "dev"}`;
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
const listHandle = mountSessionList(page.byId("sessionList"), {
  // The page's theme as it stands; the stored one, put on the root below, reaches the list through the
  // root's observer, as every later change does.
  theme: page.html().dataset.theme === "dark" ? "dark" : "light",
  // Every optional control stays on, and the app keeps the page's keyboard (`keyboard: "page"`, the
  // keymap below), as it does for the view.
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

/** Where the list's parts go in the sidebar (stage 3), node for node where the list put them when it
 *  found the sidebar itself: its strip of head controls in the head's one row before the sidebar's
 *  collapse (Collapse every group, Expand every group, the filter; the strip draws no box, stage 4),
 *  the filter's sheet under the head, the
 *  rail's filter after its search, and the agent marks at the rail's end. Called as the list mounts,
 *  so it reads nothing declared below the mount. */
function placeListPart(part, node) {
  if (part === "controls") page.byId("sidebarCollapse").before(node);
  else if (part === "sheet") page.$("#app>.sidebar>.side-head").append(node);
  else if (part === "rail-filter") page.byId("sidebarMiniSearch").after(node);
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
  if (id === indexState.selected && viewHandle.session() === id) return;
  listHandle.select(id);
  showSession(id, push);
}
/** The app's half of a selection: the view, then the address. */
function showSession(id, push) {
  // The view takes the selection before the address does: its loading veil, the pane's tabs, its
  // top bar and its compose box (a parent's Back is written from the top bar, `parentShown`).
  viewHandle.select(id);
  const url = new URL(location.href); url.searchParams.set("session", id); url.searchParams.delete("ui"); if (push) url.hash = ""; history[push ? "pushState" : "replaceState"]({}, "", url);
  viewHandle.open(id);
}
function sessionGone() { indexState.selected = ""; viewHandle.close("Session is gone", "It may have been deleted or moved. The list keeps scanning."); }
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
      if (top) rows.push({ kind: "project", label: project.name, meta: agent.name, sid: top.id, ts: ts(top), why: concealed(top, project) });
      for (const session of project.sessions) {
        rows.push({ kind: "session", label: session.name || session.id, meta: `${agent.name} · ${project.name}`, sid: session.id, ts: ts(session), why: concealed(session, project) });
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
if (localStorage.getItem("am-demo-theme") === "dark") page.html().dataset.theme = "dark";
// #s33: the collapsed rail is the DESKTOP's. On a phone the sidebar is the drawer, and the rail's
// rules hide its head, its nav and its list: a phone that had the rail stored (1.355 drew a phone
// held sideways as a desktop, and the owner collapsed it there) opened a blank drawer with no
// control left to expand it. So the class never goes on there; the choice stays remembered, and a
// window grown past the phone wears it again.
// The rail is the list drawn small (stage 4): the app decides when, and tells the list, which opens its
// flyouts only then and closes one open when the sidebar comes back.
function applySidebar() {
  app.classList.toggle("sidebar-off", !indexState.sidebarOpen && !matchMedia(PHONE_QUERY).matches);
  listHandle.compact(app.classList.contains("sidebar-off"));
}
function toggleSidebar(open) { indexState.sidebarOpen = open; applySidebar(); persist(); viewHandle.resized(); }
page.byId("sidebarCollapse").onclick = () => toggleSidebar(false); page.byId("sidebarMiniExpand").onclick = () => toggleSidebar(true);
page.byId("sidebarCollapse").title = `Collapse the sidebar into its icon rail  ( ${hintFor("sidebar-toggle")} )`;
page.byId("sidebarMiniExpand").title = `Expand the sidebar  ( ${hintFor("sidebar-toggle")} )`;
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
function sidebarWidth() {
  return Math.round(page.$(".sidebar").getBoundingClientRect().width) || SIDEBAR_DEFAULT;
}
function setSidebarWidth(value, remember) {
  // A drag that reaches x=0 is a drag to the minimum, not a request for the default: 0 is a
  // width, so only a value that is not a number at all falls back.
  const asked = Number(value);
  const width = Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, Math.round(Number.isFinite(asked) ? asked : SIDEBAR_DEFAULT)));
  page.html().style.setProperty("--sidebar-user", `${width}px`);
  // Below `SIDEBAR_TIGHT` the head's own row — the shell switch and five glyphs — is wider than
  // the sidebar, and the last of them (the sidebar collapse) would sit outside it. The CSS class
  // squeezes the glyphs and the head's padding so every control stays reachable at the narrowest
  // width the reader can drag to, which is what #202's sixth glyph cost (measured: the row is
  // 237px wide and needs 275 with the head's padding, against a 232px minimum).
  app.classList.toggle("sidebar-tight", width < SIDEBAR_TIGHT);
  sidebarResizer.setAttribute("aria-valuenow", String(width));
  sidebarResizer.title = `Drag to resize the session list · double-click for ${SIDEBAR_DEFAULT}px`;
  if (remember) localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  viewHandle.resized();
  return width;
}
sidebarResizer.addEventListener("pointerdown", event => {
  if (!indexState.sidebarOpen) return;
  event.preventDefault();
  // A synthetic pointer (a test, an assistive tool) has no capture to take — the drag is driven
  // from the window either way, so a refused capture must not end it.
  try { sidebarResizer.setPointerCapture(event.pointerId); } catch (_) {}
  sidebarResizer.classList.add("dragging"); app.classList.add("resizing");
  let width = sidebarWidth();
  const move = moved => { width = setSidebarWidth(moved.clientX, false); };
  const up = () => {
    removeEventListener("pointermove", move); removeEventListener("pointerup", up);
    sidebarResizer.classList.remove("dragging"); app.classList.remove("resizing");
    localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  };
  addEventListener("pointermove", move); addEventListener("pointerup", up);
});
sidebarResizer.ondblclick = () => setSidebarWidth(SIDEBAR_DEFAULT, true);
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
  const PHONE = matchMedia(PHONE_QUERY);
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
  // Across the breakpoint the desktop's rail comes off or goes back on (#s33, `applySidebar`).
  PHONE.addEventListener("change", () => applySidebar());
  // Out of the wide layout the column's borrowed list goes home and the handle speaks of the drawer
  // again (the view closes its drop-down going in).
  WIDE.addEventListener("change", () => { if (!WIDE.matches) showColumnTab("sessions"); labelHandle(); });

  mobileShell = {
    setDrawer,
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
function paintWrite() {
  page.byId("writeSwitch").setAttribute("aria-checked", String(controlState.write));
  page.byId("writeBtn").classList.toggle("on", controlState.write);
  page.byId("sidebarMiniWrite").classList.toggle("on", controlState.write);
  page.byId("writeInfo").hidden = controlState.paired;
}
function showPairHelp() {
  const help = page.byId("writeHelp");
  help.innerHTML = controlState.paired
    ? "<b>Monitor is paired</b><p>With write mode on, a writable session shows a compose box. A live terminal still needs its own grant.</p>"
    : "<b>Write mode needs a paired monitor</b><p>Stop the monitor and run this in a terminal:</p><code>agent-monitor --pair</code>";
  help.hidden = false; help.classList.add("show");
}
page.byId("writeSwitch").onclick = () => {
  if (!controlState.paired) { showPairHelp(); return; }
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
// the app is passed in here; what the app asks of it is the handle. Every optional control stays on,
// and the app keeps the page's keyboard (`keyboard: "page"`, the keymap below).
viewHandle = mountSessionView(viewRoot.element(), {
  theme: page.html().dataset.theme === "dark" ? "dark" : "light",
  backend: backend(),
  grid: app,
  keyboard: "page",
  write: { on: () => controlState.write, confirm: confirmWrite },
  selectedId: () => listHandle.selected(),
  sessionRow: id => listHandle.row(id),
  sessionWorkers: id => listHandle.workers(id),
  selectSession,
  renderList: () => listHandle.redraw(),
  refreshIndex: () => listHandle.refresh(),
  pauseIndex: () => listHandle.stop(),
  resumeIndex: () => listHandle.start(),
  parentShown,
  openList: () => app.classList.remove("mobile-detail"),
  openSidebar: () => toggleSidebar(true),
  paneLayout: (name, on) => app.classList.toggle(name, on),
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
  // The reader's choice of the outline (#s431 stage 3), which the app keeps to arrange its list by,
  // and the room it could give the view for the outline: none yet, since the app does not yet narrow
  // its list for it.
  onOutline: ({ wanted }) => { viewOutlineWanted = wanted; },
  room: () => 0,
});
// The rail's write button reaches the write switch itself (#54): the demo clicks the write
// BUTTON, whose handler ignores a click that lands on a button — a programmatic click does.
// Shared Controls already binds this rail action to native session creation.
if (!viewHandle.controls.nativeActive) page.byId("sidebarMiniWrite").onclick = () => page.byId("writeSwitch").click();
setSidebarWidth(localStorage.getItem(SIDEBAR_WIDTH_KEY) || SIDEBAR_DEFAULT, false);
// The keymap (parity #11): the classic view's keys, resolved by keymap.js — the view's actions from
// its handle, the list's and the sidebar's here.
const keyActions = {
  ...viewHandle.keys,
  "search": () => { if (indexState.selected) viewHandle.keys.search(); else openGlobalSearch(); },
  ...listHandle.keys,
  "sidebar-toggle": () => toggleSidebar(!indexState.sidebarOpen),
};
// A key with no action here (a control a host switched off) is left to the page, as the view's own
// binding leaves it; every action the keymap names has one in the app, so nothing changes for it.
bindKeymap(document, target => (target?.closest?.(".tree-row") ? "list" : "view"), action => (keyActions[action] ? keyActions[action]() : false));
addEventListener("keydown", event => { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openGlobalSearch(); } else if (event.key === "Escape") page.byId("searchLayer").classList.remove("production-open"); });

applySidebar();
listHandle.start();

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
