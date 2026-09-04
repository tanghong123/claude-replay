import { agentLogo, svg } from "./icons.js";
import { AttachmentViewer } from "./attachment-viewer.js";
import { bindComponentEvents } from "./components.js";
import { referenceAction } from "./shared/capabilities.js";
import { ControlStore } from "./control-store.js";
import { Preview } from "./preview.js";
import { RecordStore } from "./record-store.js";
import { SessionIndexStore } from "./session-index-store.js";
import { controlState, indexState, persist, recordState, selectedRow, uiState } from "./state.js";
import { families, hideAction, ignoreQuery, visibleTree } from "./shared/session-visibility.js";
import { displayState, needsPerson as needs, denoteState } from "./shared/state-labels.js";
import { SIZE_MAX, SIZE_MIN, SIZE_STEP, clampSize, readingVars } from "./shared/reading.js";
import { RUNTIME_ALWAYS, runtimeRows, runtimeText } from "./shared/runtime.js";
import { bindKeymap, hintFor } from "./shared/keymap.js";
import { agentRecordTargets, currentTurnIndex, escapeText, plainText, Projection, taskRecordTargets, taskStatus, taskGroups, taskOrder, taskCenterTarget, taskDetails, artifactRoster, compactionTick } from "./view-model.js";
import { Viewport } from "./viewport.js";

// The outline row currently marked (#52) — declared ahead of the init code below, which
// renders the navigator before the module's later statements have run.
let outlineCurrent = null;

const byId = id => document.getElementById(id);
const app = byId("app");
const tree = byId("tree");
const transcript = byId("transcript");
const projection = new Projection();
let toastTimer = 0;
let landedHash = "";

document.querySelectorAll("[data-icon]").forEach(element => { element.innerHTML = svg(element.dataset.icon); });
// The sidebar head's controls (#75, #76): the generated shell gives the theme toggle and
// collapse-all no glyph — an empty 34px button each, invisible until hovered — and the sidebar
// collapse beside them was easy to miss in that company. Each is named and drawn here.
for (const [id, icon, label] of [["themeBtn", "moon", "Toggle light and dark"], ["collapseBtn", "collapseStack", "Collapse every group"], ["sidebarCollapse", "sidebar", "Collapse the session list into its icon rail"]]) {
  const button = byId(id);
  if (!button) continue;
  if (!button.querySelector("svg")) button.innerHTML = svg(icon);
  button.setAttribute("aria-label", label);
  if (id !== "sidebarCollapse") button.title = label;
}
const filterIcon = document.querySelector("#filterTranscriptBtn [data-icon]");
if (filterIcon) filterIcon.innerHTML = svg("filterLines");
document.querySelector(".brand small").textContent = `v${document.body.dataset.version || "dev"}`;

// The shell switch. Both frontends are supported while this one is being validated, so the way
// back has to be a control a person can see — not a query parameter they have to remember. It
// writes the preference (`/api/ui`), so the choice survives a reload and a restart; `?ui=` on
// the URL still overrides for one request, which is how the two get compared side by side.
const shellToggle = document.createElement("button");
shellToggle.className = "iconbtn shell-toggle";
shellToggle.id = "shellToggle";
shellToggle.title = "Switch to the classic interface";
shellToggle.textContent = "Classic";
shellToggle.onclick = () => {
  shellToggle.disabled = true;
  fetch("/api/ui?set=classic", { cache: "no-store" })
    .then(() => { location.href = "/"; })
    .catch(() => { shellToggle.disabled = false; });
};
document.querySelector(".head-actions").prepend(shellToggle);

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
const infoCardLabel = document.querySelector('[data-nav-card="session"] .outline-card-head strong');
const infoRailButton = document.querySelector('[data-nav-card-open="session"]');
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
    if (!sessionCopyMenu.contains(document.activeElement) && document.activeElement !== sessionTitle) setSessionCopyMenu(false);
  }, 120);
}
sessionTitle.addEventListener("pointerenter", () => setSessionCopyMenu(true));
sessionTitle.addEventListener("pointerleave", scheduleSessionCopyClose);
sessionTitle.addEventListener("focus", () => setSessionCopyMenu(true));
sessionTitle.addEventListener("blur", scheduleSessionCopyClose);
sessionTitle.addEventListener("click", () => setSessionCopyMenu(true));
sessionTitle.addEventListener("keydown", event => {
  if (["Enter", " ", "ArrowDown"].includes(event.key)) {
    event.preventDefault(); setSessionCopyMenu(true);
    sessionCopyMenu.querySelector("button:not(:disabled)")?.focus();
  }
});
sessionCopyMenu.addEventListener("pointerenter", () => clearTimeout(sessionCopyCloseTimer));
sessionCopyMenu.addEventListener("pointerleave", scheduleSessionCopyClose);
sessionCopyMenu.addEventListener("focusout", scheduleSessionCopyClose);
document.addEventListener("pointerdown", event => {
  if (event.target !== sessionTitle && !sessionCopyMenu.contains(event.target)) setSessionCopyMenu(false);
});

async function copyText(text) {
  if (!text) return false;
  if (navigator.clipboard?.writeText) {
    try { await navigator.clipboard.writeText(text); return true; } catch (_) { /* use fallback */ }
  }
  try {
    const field = document.createElement("textarea");
    field.value = text; field.style.cssText = "position:fixed;top:-1000px;opacity:0";
    document.body.appendChild(field); field.select();
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
  toast(copied ? (kind === "id" ? "Copied session id" : "Copied transcript path") : "Copy failed — check the browser's clipboard permission");
  if (copied) setSessionCopyMenu(false);
});

const viewport = new Viewport(transcript, byId("transcriptInner"), recordState, {
  afterRender: () => { applyFilters(); markSearch(); updateStickyHeaders(); updateOutlineFocus(); },
  afterScroll: () => { updateStickyHeaders(); updateOutlineFocus(); },
  followChanged: paintJump
});
// Delegate from the stable scroll host: the virtual window may replace all of its children
// while a pull reconciliation is in flight, but attachment/fold interactions must survive it.
bindComponentEvents(transcript, recordState, {
  rerender: () => { viewport.render(); viewport.scheduleRemember(); },
  remember: () => viewport.scheduleRemember(),
  copySpot: async (id, button) => {
    const url = new URL(location.href);
    url.hash = id;
    const copied = await copyText(url.href);
    if (!copied) { toast("Copy failed — check the browser's clipboard permission"); return; }
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
  openChild: id => selectSession(id, true),
  openAttachment: (id, path, fsig, action, sig) => openAttachment(id, path, fsig, action, sig),
  openReference: path => openReference(path),
  openReferenceOffer: offer => openReferenceOffer(offer),
  toast
});

const preview = new Preview({ layoutChanged: () => viewport.remeasure(), toast, reveal: item => attachmentViewer.reveal(item) });
const attachmentViewer = new AttachmentViewer({ openPreview: item => preview.open(item), toast });
const controls = new ControlStore({ toast, refreshIndex: loadSessions });
const sessionIndex = new SessionIndexStore({
  update: () => {
    if (indexState.selected && indexState.selectedWasRow && !selectedRow()) sessionGone();
    renderTree(); renderHeader(); controls.paint();
    if (!indexState.selected) {
      // A requested id is honoured even when it is not a list row: a sub-agent child never is,
      // and a link to one must open it, not the first session. A bad id shows "Cannot read
      // this session" from the record store rather than silently opening something else.
      const requested = new URLSearchParams(location.search).get("session");
      const first = requested || [...indexState.rows.values()].find(row => !row.hidden)?.id;
      if (first) selectSession(first, false);
    }
  },
  error: () => toast("Session scan failed — retrying")
});
const recordStore = new RecordStore({
  reset: () => { lastRecordCount = -1; projection.units = []; recordState.records = []; recordState.meta = null; recordState.heights.clear(); recordState.folds.clear(); recordState.processFolds.clear(); recordState.processExpanded.clear(); recordState.promptExpanded.clear(); recordState.taskTargets.clear(); recordState.agentTargets.clear(); recordState.rawTurns.clear(); recordState.capOpen.clear(); recordState.openImages.clear(); recordState.filterHits = null; recordState.filterDirect = null; recordState.filterSnapshot = null; recordState.search = ""; byId("transcriptSearchInput").value = ""; viewport.showEmpty("Loading session…", "Reading the normalized record stream."); renderHeader(); renderNavigator(); },
  update: updateRecords,
  error: (error, hasRecords) => hasRecords ? toast(`${error.message}；retrying`) : viewport.showEmpty("Cannot read this session", `${error.message}；The monitor will retry.`, true)
});

function toast(message) {
  const element = byId("toast"); element.textContent = message; element.classList.add("show");
  clearTimeout(toastTimer); toastTimer = setTimeout(() => element.classList.remove("show"), 1900);
}

// The state words come from the shared table (shared/state-labels.js, #44) — the rail's
// tooltip and this shell's chips read the same label for the same verdict. `stateDenote`
// adds the one input that is this viewer's: the read mark.
function stateDenote(row) { return denoteState(row, Number(indexState.read[row.id] || 0)); }

function loadSessions() { return sessionIndex.refresh(); }

function groupedSessions() {
  return sessionIndex.grouped().map(agent => ({ ...agent, name: agentName(agent.id) }));
}
const agentName = id => ({ claude: "Claude Code", codex: "Codex", qoder: "Qoder", qoderwork: "QoderWork" })[id] || id;
const SIDEBAR_SESSION_LIMIT = 5;

// Hide / restore (parity #1). The control lives ON the row, where the shell keeps row-level
// actions, and appears on hover or focus — except on a hidden row, where "restore" is the
// whole point of showing it. The key is the server's (`ignoreKey`), passed through verbatim.
function treeAction(target, kind) {
  const action = hideAction(target, kind);
  return `<button class="tree-action" type="button" data-ignore-op="${action.op}" data-ignore-key="${escapeText(action.key)}" title="${escapeText(action.title)}" aria-label="${escapeText(action.title)}">${svg(action.icon)}</button>`;
}

// A row, or a fork family's representative (`fam`): the family's state and age, not just the
// rep's — a forked conversation is "running" when any fork is — plus a fork count that opens
// the other members, indented, the way the classic rail's ⑂ chip does (#142).
function sessionTreeRow(row, fam = null, member = false) {
  const forked = fam && fam.forks.length ? fam : null;
  const denote = forked && forked.growing && stateDenote(row)?.tone !== "busy" ? { label: "A fork is running", tone: "busy" } : stateDenote(row);
  const newest = forked ? forked.members.reduce((a, b) => ((b.activityTs || 0) > (a.activityTs || 0) ? b : a), row) : row;
  const marker = denote ? `<span class="session-state ${escapeText(denote.tone)}" role="img" aria-label="${escapeText(denote.label)}" title="${escapeText(denote.label)}"></span>` : "";
  const open = forked && (indexState.openFamilies.has(forked.key) || forked.forks.some(r => r.id === indexState.selected));
  const chip = forked ? `<button class="tree-forks" type="button" data-family-toggle="${escapeText(forked.key)}" aria-expanded="${!!open}" title="${forked.forks.length} fork${forked.forks.length === 1 ? "" : "s"} of this session — forking copies the conversation, so they overlap heavily">⑂ ${forked.forks.length}</button>` : "";
  return `<div class="tree-row session ${row.id === indexState.selected ? "selected" : ""} ${row.hidden ? "is-hidden" : ""} ${member ? "is-member" : ""}" role="treeitem" tabindex="0" data-session="${escapeText(row.id)}" title="${escapeText(displayState(row).label)} · ${escapeText(row.stateDetail || "")}"><span class="tree-title">${escapeText(row.name || row.id)}</span><span class="session-end">${chip}${treeAction(row, "session")}${marker}<span class="session-age">${escapeText(newest.activity || row.activity || "")}</span></span></div>`;
}

function renderTree() {
  const agents = groupedSessions(); let html = ""; let attention = 0;
  indexState.rows.forEach(row => { if (!row.hidden && needs(row)) attention++; });
  const shownAgents = visibleTree(agents, { showHidden: indexState.showHidden, attention: indexState.attention, needs });
  for (const agent of shownAgents) {
    const agentKey = `a:${agent.id}`, openAgent = !indexState.collapsed.has(agentKey);
    html += `<div class="agent-label" role="treeitem" tabindex="0" data-toggle="${escapeText(agentKey)}" aria-expanded="${openAgent}">${escapeText(agent.name)}</div>`;
    if (!openAgent) continue;
    for (const project of agent.projects) {
      const rows = project.rows;
      const projectKey = `p:${project.id}`, openProject = !indexState.collapsed.has(projectKey);
      html += `<div class="tree-row project ${project.hidden ? "is-hidden" : ""}" role="treeitem" tabindex="0" data-toggle="${escapeText(projectKey)}" aria-expanded="${openProject}" title="${escapeText(project.path)}">${svg("folder")}<span class="tree-title">${escapeText(project.name)}</span>${project.ignoreKey ? treeAction(project, project.kind) : ""}</div>`;
      if (!openProject) continue;
      const overflowKey = `${agent.id}:${project.id}`;
      const expanded = indexState.expandedProjects.has(overflowKey);
      const fams = families(rows);
      let shown = expanded ? fams : fams.slice(0, SIDEBAR_SESSION_LIMIT);
      const selectedFam = fams.find(fam => fam.members.some(row => row.id === indexState.selected));
      if (!expanded && selectedFam && !shown.includes(selectedFam)) shown = shown.concat(selectedFam);
      for (const fam of shown) {
        html += sessionTreeRow(fam.rep, fam);
        const open = fam.forks.length && (indexState.openFamilies.has(fam.key) || fam.forks.some(row => row.id === indexState.selected));
        if (open) for (const fork of fam.forks) html += sessionTreeRow(fork, null, true);
      }
      const hidden = fams.filter(fam => !shown.includes(fam)).length;
      if (fams.length > SIDEBAR_SESSION_LIMIT) html += `<button class="tree-project-more" type="button" data-project-more="${escapeText(overflowKey)}" aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${hidden} more`}</span><span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>`;
    }
  }
  tree.innerHTML = html || `<div class="no-results">${indexState.attention ? "Nothing needs attention" : "No sessions"}</div>`;
  byId("attentionCount").textContent = String(attention);
  renderHiddenControl();
  byId("sidebarMiniAttentionBadge").hidden = !attention;
  byId("sidebarMiniAttentionBadge").textContent = String(attention);
  const attentionSummary = `Filter to sessions awaiting a reply, a grant, or that failed or stalled · ${attention}`;
  byId("sidebarMiniAttention").setAttribute("aria-label", `Needs attention: ${attention} sessions`);
  byId("sidebarMiniAttentionTooltip").textContent = attentionSummary;
  byId("sidebarMiniAgents").innerHTML = agents.map(agent => { const first = agent.projects.flatMap(project => project.sessions).find(row => !row.hidden); return first ? `<button class="sidebar-mini-agent ${first.id === indexState.selected ? "selected" : ""}" data-mini-agent-session="${escapeText(first.id)}" title="${escapeText(agent.name)}">${agentLogo(agent.id)}</button>` : ""; }).join("");
}

// "Hidden (n)" — the way back for anything hidden. A navbtn beside the attention filter, in
// the same anatomy (icon · label · count), present only while something IS hidden, exactly as
// the classic rail's toggle; the mini rail gets the matching badge. Reveal is a view state
// (not persisted, like classic), so a reload starts clean.
const hiddenBtn = document.createElement("button");
hiddenBtn.className = "navbtn hidden-filter"; hiddenBtn.id = "hiddenBtn"; hiddenBtn.type = "button"; hiddenBtn.hidden = true;
hiddenBtn.setAttribute("aria-pressed", "false");
hiddenBtn.innerHTML = `${svg("x")}<span class="label">Hidden</span><span class="count attention-count" id="hiddenCount">0</span>`;
const hiddenCount = hiddenBtn.querySelector(".count");
byId("attentionBtn").after(hiddenBtn);
const hiddenMini = document.createElement("button");
hiddenMini.className = "sidebar-mini-button hidden-mini"; hiddenMini.id = "sidebarMiniHidden"; hiddenMini.type = "button"; hiddenMini.hidden = true;
hiddenMini.innerHTML = `${svg("x")}<span class="sidebar-mini-badge" id="sidebarMiniHiddenBadge"></span>`;
byId("sidebarMiniAttention").after(hiddenMini);
function renderHiddenControl() {
  const n = indexState.ignoredCount;
  if (!n) indexState.showHidden = false;
  hiddenBtn.hidden = !n; hiddenMini.hidden = !n;
  hiddenCount.textContent = String(n);
  hiddenBtn.classList.toggle("on", indexState.showHidden);
  hiddenBtn.setAttribute("aria-pressed", String(indexState.showHidden));
  hiddenBtn.title = indexState.showHidden ? "Hide them again" : `Show ${n} hidden session${n === 1 ? "" : "s"}, projects and agents`;
  hiddenMini.setAttribute("aria-label", `${n} hidden`); hiddenMini.title = hiddenBtn.title;
}
hiddenBtn.onclick = () => { indexState.showHidden = !indexState.showHidden; renderTree(); };
hiddenMini.onclick = () => { indexState.showHidden = true; byId("sidebarMiniExpand").click(); renderTree(); };
async function applyIgnore(op, key) {
  try {
    const response = await fetch(ignoreQuery({ op, key }), { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    toast(op === "add" ? "Hidden — find it under Hidden" : "Restored");
    await sessionIndex.refresh();
  } catch (error) { toast(`Could not ${op === "add" ? "hide" : "restore"}: ${error.message}`); }
}

tree.onclick = event => {
  const action = event.target.closest("[data-ignore-op]"); if (action) { event.stopPropagation(); applyIgnore(action.dataset.ignoreOp, action.dataset.ignoreKey); return; }
  const forks = event.target.closest("[data-family-toggle]"); if (forks) { event.stopPropagation(); const key = forks.dataset.familyToggle; indexState.openFamilies.has(key) ? indexState.openFamilies.delete(key) : indexState.openFamilies.add(key); renderTree(); return; }
  const session = event.target.closest("[data-session]"); if (session) { selectSession(session.dataset.session, true); return; }
  const more = event.target.closest("[data-project-more]"); if (more) { const key = more.dataset.projectMore; indexState.expandedProjects.has(key) ? indexState.expandedProjects.delete(key) : indexState.expandedProjects.add(key); persist(); renderTree(); return; }
  const toggle = event.target.closest("[data-toggle]"); if (toggle) { const key = toggle.dataset.toggle; indexState.collapsed.has(key) ? indexState.collapsed.delete(key) : indexState.collapsed.add(key); persist(); renderTree(); }
};
tree.onkeydown = event => { if (["Enter", " "].includes(event.key)) { event.preventDefault(); event.target.click(); } };
byId("sidebarMiniAgents").onclick = event => { const button = event.target.closest("[data-mini-agent-session]"); if (button) selectSession(button.dataset.miniAgentSession, true); };

function sessionUrl(id) {
  const url = new URL(location.href); url.searchParams.set("session", id);
  if (document.body.dataset.uiDefault === "true") url.searchParams.delete("ui"); else url.searchParams.set("ui", "app");
  url.hash = ""; return url;
}
function selectSession(id, push) {
  if (!id || (id === indexState.selected && recordStore.session === id)) return;
  indexState.selected = id; indexState.selectedWasRow = indexState.rows.has(id); app.classList.add("mobile-detail");
  preview.setSession(id);
  const row = selectedRow(); if (row) { indexState.read[id] = row.activityTs || Date.now() / 1000; persist(); }
  renderTree(); renderHeader(); controls.paint();
  const url = new URL(location.href); url.searchParams.set("session", id); if (document.body.dataset.uiDefault === "true") url.searchParams.delete("ui"); else url.searchParams.set("ui", "app"); if (push) url.hash = ""; history[push ? "pushState" : "replaceState"]({}, "", url);
  recordState.session = id; viewport.beginSession(id); recordStore.open(id);
}
function sessionGone() { recordStore.stop(); preview.setSession(""); indexState.selected = ""; recordState.session = ""; viewport.showEmpty("Session is gone", "It may have been deleted or moved. The list keeps scanning.", true); }

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
let synthesizedFor = "";
function renderParent(meta) {
  const known = recordState.session === indexState.selected ? meta?.ancestors?.at(-1) : null;
  const hint = parentHints.get(indexState.selected);
  const parent = known || (hint ? { id: hint, title: indexState.rows.get(hint)?.name || hint } : null);
  parentBtn.classList.toggle("is-live", !!parent);
  parentBtn.dataset.parent = parent?.id || "";
  parentBtn.title = parent ? `Back to the parent session: ${parent.title || parent.id}  ( ${hintFor("parent")} )` : "Back to the parent session";
  if (parent && synthesizedFor !== indexState.selected && history.length <= 1) {
    try { const child = location.href; history.replaceState({}, "", sessionUrl(parent.id)); history.pushState({}, "", child); } catch (_) {}
    synthesizedFor = indexState.selected;
  }
  return parent;
}
function renderHeader() {
  renderArtifacts();
  const row = selectedRow();
  const liveMeta = recordState.session === indexState.selected ? recordState.meta : null;
  const parent = renderParent(liveMeta);
  if (!row && liveMeta && parent) {
    // A sub-agent child: not a row, so everything the header shows comes from its own meta.
    byId("sessionTitle").textContent = liveMeta.title || liveMeta.sid || indexState.selected;
    byId("sessionCrumb").textContent = `${agentName(liveMeta.agent)} · sub-agent of ${parent.title || parent.id}`;
    byId("statusChip").className = "status-chip idle";
    byId("statusChip").textContent = liveMeta.agent_type ? `Sub-agent · ${liveMeta.agent_type}` : "Sub-agent";
    byId("statusChip").title = "";
    sessionTitle.tabIndex = 0;
    sessionCopyMenu.hidden = false;
    sessionCopyMenu.querySelector('[data-session-copy-value="id"]').textContent = liveMeta.sid || indexState.selected;
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
  byId("sessionTitle").textContent = row.name || row.id;
  byId("sessionCrumb").textContent = `${agentName(row.agent)} · ${row._group?.label || ""}`;
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
function updateRecords({ records, meta, changedFrom }) {
  const before = lastRecordCount; const wasFollowing = recordState.following;
  const metaArrived = meta && meta !== recordState.meta;
  recordState.records = records; recordState.meta = meta;
  if (metaArrived) renderHeader();
  recordState.taskTargets = taskRecordTargets(meta?.tasks || [], records);
  recordState.agentTargets = agentRecordTargets(directAgents(meta), records);
  const changedUnit = projection.rebuild(records, changedFrom);
  recordState.units = projection.units;
  viewport.setUnits(recordState.units, changedUnit);
  landOnHash();
  // What arrived since the last apply is "new" only once the session is OPEN (#84): the first
  // apply after a reset is the session's own content — a re-open restored to a remembered
  // position is not following, and counting that batch said "11696 new messages" on a session
  // that had none. The classic page counts only what arrives after the open; so does this.
  const delta = Math.max(0, records.length - before);
  const opening = lastRecordCount < 0;
  lastRecordCount = records.length;
  if (!wasFollowing && delta && !opening) recordState.newRecords += delta;
  paintJump(); renderHeader(); renderNavigator(); updateSearch(false);
}

function landOnHash() {
  let id = location.hash.slice(1);
  try { id = decodeURIComponent(id); } catch (_) { return false; }
  if (!id) { landedHash = ""; return false; }
  const identity = `${recordState.session}:${id}`;
  if (identity === landedHash) return true;
  const index = recordState.records.findIndex(record => record.id === id && (record.kind === "user" || record.kind === "assistant"));
  if (index < 0 || !viewport.jumpToRecord(index)) return false;
  landedHash = identity;
  return true;
}

function directAgents(source = recordState.meta) {
  const meta = source || {}, result = [...(meta.children || [])], ids = new Set(result.map(item => item.id));
  for (const run of meta.runs || []) for (const member of run.members || []) if (!ids.has(member.id)) { ids.add(member.id); result.push(member); }
  return result;
}
// The outline column scrolls as a whole (#74); the "Outline" caption sticks at its top and every
// pane's head sits in a stack under it: the slot of a head is the caption plus the heads above
// it, and on every scroll a head whose pane has moved above its slot is shifted down to the
// slot — so the Turns head stays under "Outline" however far the column scrolls, and each
// following head moves up until it sits under the one before it. (CSS sticky cannot do this:
// a sticky head may not leave its own pane, and a pane scrolled past takes its head along.)
// Folded panes count: their heads are in the stack too.
function stackOutlineHeads() {
  const nav = byId("sessionNavigator");
  const caption = nav.querySelector(":scope > .outline-caption");
  const navTop = nav.getBoundingClientRect().top;
  let slot = caption ? caption.getBoundingClientRect().bottom - navTop : 0;
  nav.querySelectorAll(":scope > .outline-card").forEach(card => {
    const head = card.querySelector(":scope > .outline-card-head");
    if (!head) return;
    head.style.transform = "";
    const cardTop = card.getBoundingClientRect().top - navTop;
    const shift = Math.max(0, slot - cardTop);
    if (shift > 0) head.style.transform = `translateY(${shift}px)`;
    slot += head.offsetHeight;
  });
}
byId("sessionNavigator").addEventListener("scroll", stackOutlineHeads, { passive: true });
window.addEventListener("resize", stackOutlineHeads);
function renderNavigator() {
  const turns = recordState.units.filter(unit => unit.type === "user");
  byId("navigatorTurnCount").textContent = turns.length;
  // A compaction is an epoch tick between turns (#69, restyled in #86): a hairline with a glyph
  // for how it happened and the context size from → to, in the rows' own type, no prose — so a
  // session that compacted fifteen times reads as fifteen chapters. The tick jumps to the
  // compaction record like a turn row jumps to its turn.
  const epochs = [];
  recordState.records.forEach((record, i) => { if (record.kind === "compaction") epochs.push({ at: i, tick: compactionTick(record.head || {}) }); });
  const rows = [...turns.map(unit => ({ at: unit.from, unit })), ...epochs].sort((a, b) => a.at - b.at);
  byId("navigatorTurns").innerHTML = rows.map(r => r.unit
    ? `<button class="outline-turn-row" data-turn-record="${r.unit.from}" title="${escapeText(r.unit.label)}"><span class="outline-number">${String(r.unit.turn).padStart(2, "0")} ·</span><span class="outline-label">${escapeText(r.unit.label)}</span></button>`
    : `<button class="outline-epoch" type="button" data-turn-record="${r.at}" title="${escapeText(r.tick.title)}${r.tick.sizes ? ` · ${escapeText(r.tick.sizes)}` : ""} — jump to the compaction" aria-label="${escapeText(r.tick.title)}${r.tick.sizes ? `, ${escapeText(r.tick.sizes)}` : ""}"><span class="outline-epoch-line" aria-hidden="true"></span><span class="outline-epoch-glyph" aria-hidden="true">${r.tick.glyph}</span>${r.tick.sizes ? `<span class="outline-epoch-sizes">${escapeText(r.tick.sizes)}</span>` : ""}<span class="outline-epoch-line" aria-hidden="true"></span></button>`
  ).join("") || '<div class="activity-empty">No turns</div>';
  const tasks = recordState.meta?.tasks || [];
  const runningTasks = tasks.filter(task => taskStatus(task.status) === "in_progress").length;
  const doneTasks = tasks.filter(task => taskStatus(task.status) === "completed").length;
  byId("navigatorWorkCount").innerHTML = outlineSummary(runningTasks, doneTasks, tasks.length);
  outlineCurrent = null; // the rows were rebuilt: mark and reveal the current one afresh
  updateOutlineFocus();
  byId("outlineTaskDot").hidden = !runningTasks;
  // Grouped and ordered (#56): completed, running, pending — then by id within a group; the
  // stream's index rides along on each row because the record targets are keyed by it.
  const taskRow = ({ task, index }) => {
    // The row opens the task's details (#60); the jump to where its status was recorded is an
    // action inside the popover, so a click never moves the transcript by surprise.
    const status = taskStatus(task.status), statusClass = status === "in_progress" ? "running" : status;
    return `<div class="work-task"><button class="work-task-head" type="button" data-task-open="${index}" title="Task details" aria-haspopup="dialog"><span class="task-state ${escapeText(statusClass)}"></span><span class="work-copy"><strong>${escapeText(task.subject || task.title || `Task ${index + 1}`)}</strong></span><span class="work-tail">#${escapeText(task.id || index + 1)}</span></button></div>`;
  };
  byId("navigatorWork").innerHTML = taskGroups(tasks).map(group => `<div class="work-group" data-task-group="${group.key}"><span>${group.label}</span><span class="work-group-count">${group.rows.length}</span></div>${group.rows.map(taskRow).join("")}`).join("") || '<div class="activity-empty">No session tasks</div>';
  const agents = directAgents(), activeAgents = agents.filter(agent => agent.running).length;
  byId("navigatorAgentCount").innerHTML = outlineSummary(activeAgents, agents.length - activeAgents, agents.length);
  // A sub-agent row (#61): the click opens the sub-agent's OWN transcript — the whole view
  // switches, the header shows the child with its way back to the parent — and the spawn point
  // in the parent stays reachable as a small secondary control when the record stream kept it.
  byId("navigatorAgents").innerHTML = agents.map(agent => {
    const target = recordState.agentTargets.get(String(agent.id));
    const spawn = target == null ? "" : `<button class="outline-agent-spawn" type="button" data-agent-record="${target}" title="Jump to where the parent launched this agent" aria-label="Jump to where the parent launched ${escapeText(agent.title || agent.id)}"><span aria-hidden="true">↳</span></button>`;
    return `<div class="outline-agent-row"><button class="outline-agent" type="button" data-child-outline="${escapeText(agent.id)}" title="Open the sub-agent's transcript"><span class="agent-state ${agent.running ? "running" : "completed"}"></span><span class="outline-agent-copy"><strong>${escapeText(agent.title || agent.description || agent.id)}</strong><small>${escapeText(agent.type || agent.agent_type || "agent")}</small></span><span class="outline-agent-tail"></span></button>${spawn}</div>`;
  }).join("") || '<div class="activity-empty">No direct children</div>';
  renderSessionInfo(turns.length, agents.length);
  document.querySelectorAll("[data-nav-card]").forEach(card => card.classList.toggle("open", uiState.navCards.has(card.dataset.navCard)));
  stackOutlineHeads();
  document.querySelector(".workspace").classList.toggle("navigator-off", !uiState.navigatorOpen);
  document.querySelector(".workspace").classList.toggle("navigator-hidden", uiState.navigatorHidden);
  byId("navigatorToggle").classList.toggle("active", uiState.navigatorOpen);
}
const outlineSummary = (active, done, total) => !total ? "0" : `${active ? `<span class="outline-stat-item active"><i class="outline-stat-dot"></i>${active} active</span>` : ""}<span class="outline-stat-item done"><i class="outline-stat-dot"></i>${done}/${total} done</span>`;
// The info pane (#67, #68): only what the shell does not already show — the title, the agent and
// the project are in the header and the tree row, so the Session group carries status and
// counts; the Usage group has the cached-read tokens and, when the session compacted, the
// classic panel's summary ("24× compacted, 4.0M dropped" — a real fact even at zero tokens).
function renderSessionInfo(turns, agents) {
  const row = selectedRow(), meta = recordState.meta || {}, usage = meta.usage || {};
  if (!row) {
    byId("navigatorSessionSummary").textContent = "—";
    byId("navigatorSession").innerHTML = '<div class="activity-empty">No session selected</div>';
    return;
  }
  // A sub-agent roll-up shows SPLIT (own + sub-agents), the total in the hover: the own share
  // is what the transcript's usage panel reports, so one opaque total read as a mismatch.
  const summary = byId("navigatorSessionSummary");
  if (row.cost != null && row.costSubs) { const own = Number(row.cost) - Number(row.costSubs); summary.textContent = `~$${own.toFixed(2)} + $${Number(row.costSubs).toFixed(2)} sub-agents`; summary.title = `total ~$${Number(row.cost).toFixed(2)} = this session $${own.toFixed(2)} + sub-agents $${Number(row.costSubs).toFixed(2)}`; }
  else { summary.textContent = row.cost != null ? `~$${Number(row.cost).toFixed(2)}` : usage.cost || "—"; summary.title = ""; }
  const group = (label, rows) => `<div class="session-info-group"><div class="session-info-label">${label}</div>${rows.map(([key, value]) => `<div class="session-info-row"><span>${escapeText(key)}</span><strong>${escapeText(value ?? "—")}</strong></div>`).join("")}</div>`;
  byId("navigatorSession").innerHTML = `<div class="session-info">${group("Session", [["status", displayState(row).label], ["turns", turns], ["children", agents]])}${group("Usage", [["model", usage.model], ["input", usage.input || usage.input_tokens], ["output", usage.output || usage.output_tokens], ["cache read", usage.cache_read], ...(usage.compacted ? [["compacted", usage.compacted]] : []), ["est. cost", usage.cost || (row.cost != null ? `~$${Number(row.cost).toFixed(2)}` : "—")]])}${group("Runtime", [["cwd", meta.cwd || row._group?.secondary], ...runtimeRows(usage.runtime).filter(r => r.state !== "absent" || RUNTIME_ALWAYS.includes(r.key)).map(r => [r.label, runtimeText(r, agentName(row.agent))])])}</div>`;
}

byId("sessionNavigator").onclick = event => {
  const turn = event.target.closest("[data-turn-record]"); if (turn) { viewport.jumpToRecord(Number(turn.dataset.turnRecord), "turn"); return; }
  const open = event.target.closest("[data-task-open]"); if (open) { openTaskPopover(Number(open.dataset.taskOpen), open); return; }
  const task = event.target.closest("[data-task-record]"); if (task) { closeTaskPopover(); viewport.jumpToRecord(Number(task.dataset.taskRecord), "task"); return; }
  const agent = event.target.closest("[data-agent-record]"); if (agent) { viewport.jumpToRecord(Number(agent.dataset.agentRecord), "agent"); return; }
  const child = event.target.closest("[data-child-outline]"); if (child) { parentHints.set(child.dataset.childOutline, indexState.selected); selectSession(child.dataset.childOutline, true); return; }
  // A card's head folds and unfolds ITS pane (#74) — the whole head, and nothing else changes:
  // the other panes keep their state and their height.
  const card = event.target.closest("[data-nav-card-toggle]");
  if (card) { const key = card.dataset.navCardToggle; uiState.navCards.has(key) ? uiState.navCards.delete(key) : uiState.navCards.add(key); persist(); renderNavigator(); return; }
  const rail = event.target.closest("[data-nav-card-open]"); if (rail) { uiState.navCards.add(rail.dataset.navCardOpen); uiState.navigatorOpen = true; persist(); renderNavigator(); }
};

function updateSearch(reset) {
  const query = byId("transcriptSearchInput").value.trim().toLowerCase(); recordState.search = query;
  recordState.matches = []; if (query) recordState.records.forEach((record, index) => { if (plainText(record).toLowerCase().includes(query)) recordState.matches.push(index); });
  if (reset) recordState.match = recordState.matches.length ? 0 : -1; else recordState.match = Math.min(recordState.match, recordState.matches.length - 1);
  byId("transcriptSearchCount").textContent = `${recordState.matches.length} matches`; markSearch();
}
function stepSearch(delta) { if (!recordState.matches.length) return; recordState.match = (recordState.match + delta + recordState.matches.length) % recordState.matches.length; viewport.jumpToRecord(recordState.matches[recordState.match], "search"); markSearch(); }
function markSearch() {
  viewport.window.querySelectorAll("mark.search-mark").forEach(mark => mark.replaceWith(mark.textContent));
  viewport.window.normalize();
  if (!recordState.search) return;
  // Every hit in the mounted window is marked, the current record's first one stronger (#111,
  // the classic page's rule): a reader sees how many hits a screen holds, not one at a time.
  const needle = recordState.search, matched = new Set(recordState.matches), current = recordState.matches[recordState.match];
  for (const root of viewport.window.querySelectorAll("[data-block-index]")) {
    const index = Number(root.dataset.blockIndex);
    if (!Number.isInteger(index) || !matched.has(index) || root.parentElement?.closest("[data-block-index]")) continue;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT); const texts = [];
    while (walker.nextNode()) if (walker.currentNode.parentElement && !["SCRIPT", "STYLE", "MARK"].includes(walker.currentNode.parentElement.tagName) && walker.currentNode.nodeValue.toLowerCase().includes(needle)) texts.push(walker.currentNode);
    let first = index === current;
    for (const text of texts) {
      const value = text.nodeValue, lower = value.toLowerCase(), frag = document.createDocumentFragment();
      let at = 0, from = 0;
      while ((at = lower.indexOf(needle, from)) !== -1) {
        if (at > from) frag.appendChild(document.createTextNode(value.slice(from, at)));
        const mark = document.createElement("mark"); mark.className = "search-mark" + (first ? " current" : ""); mark.textContent = value.slice(at, at + needle.length); frag.appendChild(mark);
        first = false; from = at + needle.length;
      }
      if (from < value.length) frag.appendChild(document.createTextNode(value.slice(from)));
      text.replaceWith(frag);
    }
  }
}
byId("transcriptSearchInput").oninput = () => updateSearch(true); byId("findNext").onclick = () => stepSearch(1); byId("findPrev").onclick = () => stepSearch(-1);

function toolNames(records, into = []) {
  for (const record of records || []) {
    if (["bash", "read", "write", "edit", "skill", "tool"].includes(record.kind)) into.push(record.head?.name || record.tool || record.kind);
    for (const part of record.body || []) if (part.p === "blocks") toolNames(part.items, into);
  }
  return into;
}
function renderFilterMenu() {
  const scopes = [["u", "User messages"], ["a", "Agent replies"], ["t", "Thinking"], ["o", "All tools"], ["b", "Bash output"], ["r", "Reads"], ["e", "Edits"]];
  byId("scopeRow").innerHTML = scopes.map(([key, label]) => `<button class="scope-option ${uiState.searchScopes.has(key) ? "on" : ""}" data-scope="${key}"><span class="scope-check"></span><span>${label}</span><span class="scope-key">${key}</span></button>`).join("");
  // Every tool in the session, nested ones included — a tool call sits inside an activity
  // record, so a top-level scan listed nothing (#110).
  const tools = [...new Set(toolNames(recordState.records))];
  byId("filterOptions").innerHTML = tools.map(tool => `<button class="tool-type-option ${uiState.toolFilters.has(tool) ? "on" : ""}" data-tool-filter="${escapeText(tool)}"><span class="filter-dot"></span><span>${escapeText(tool)}</span></button>`).join("") || '<div class="tool-type-empty">No tool events in this session</div>';
  byId("filterBadge").textContent = uiState.toolFilters.size || "";
}
function applyFilters() {
  const hits = recordState.filterHits;
  viewport.window.querySelectorAll("[data-kind]").forEach(element => {
    const kind = element.dataset.kind, tool = element.dataset.toolName;
    const scope = kind === "user" ? "u" : kind === "assistant" ? "a" : kind === "thinking" ? "t" : kind === "tool" ? (tool === "Bash" ? "b" : ["Read", "Glob", "Grep", "WebFetch", "WebSearch"].includes(tool) ? "r" : ["Write", "Edit", "NotebookEdit"].includes(tool) ? "e" : "o") : null;
    const scopeDim = scope && !uiState.searchScopes.has(scope) && !(kind === "tool" && uiState.searchScopes.has("o"));
    element.classList.toggle("filter-dim", !!scopeDim || (!!hits && kind === "user" && element.classList.contains("turn")));
  });
  // The tool filter (#110), the classic page's rule: a non-turn record that does not match is
  // HIDDEN, user turns stay as dimmed landmarks, and a matching head is accented. Rows are
  // hidden inside their process; a process left with nothing to show goes with them.
  for (const element of viewport.window.querySelectorAll(".filter-hidden, .filter-hit")) element.classList.remove("filter-hidden", "filter-hit");
  if (!hits) return;
  for (const answer of viewport.window.querySelectorAll(":scope > .turn.assistant")) answer.classList.add("filter-hidden");
  for (const surface of viewport.window.querySelectorAll("[data-process-surface]")) {
    let shown = 0;
    for (const event of surface.querySelectorAll(".process-event")) {
      const hit = [...event.querySelectorAll("[data-record-id]")].some(r => hits.has(r.dataset.recordId));
      event.classList.toggle("filter-hidden", !hit);
      if (hit) shown++;
    }
    surface.classList.toggle("filter-hidden", shown === 0);
    for (const renderer of surface.querySelectorAll("[data-record-id]")) {
      if (recordState.filterDirect?.has(renderer.dataset.recordId)) renderer.querySelector(":scope > .renderer-head")?.classList.add("filter-hit");
    }
  }
}
// Does this record — or a record nested in it — carry one of the selected tools? Every record
// on such a chain is a hit (its fold opens); the direct ones are accented.
const toolKindsForFilter = new Set(["bash", "read", "write", "edit", "skill", "tool"]);
function filterChain(record, wanted, hits, direct) {
  const name = record.head?.name || record.tool || record.kind;
  const own = toolKindsForFilter.has(record.kind) && wanted.has(name);
  let inner = false;
  for (const part of record.body || []) if (part.p === "blocks") for (const item of part.items || []) if (filterChain(item, wanted, hits, direct)) inner = true;
  if (own && record.id) direct.add(record.id);
  if ((own || inner) && record.id) hits.add(record.id);
  return own || inner;
}
function applyToolFilter() {
  const wanted = uiState.toolFilters;
  if (wanted.size) {
    if (!recordState.filterSnapshot) recordState.filterSnapshot = { folds: new Map(recordState.folds), processFolds: new Map(recordState.processFolds), processExpanded: new Set(recordState.processExpanded) };
    const hits = new Set(), direct = new Set(), indices = [];
    recordState.records.forEach((record, index) => { if (filterChain(record, wanted, hits, direct)) indices.push(index); });
    recordState.filterHits = hits;
    recordState.filterDirect = direct;
    for (const id of hits) recordState.folds.set(id, false);
    for (const unit of projection.units) {
      if (unit.type !== "process") continue;
      if (indices.some(index => index >= unit.from && index <= unit.to)) { recordState.processFolds.set(unit.key, false); recordState.processExpanded.add(unit.key); }
    }
    viewport.render();
    viewport.remeasure();
    // Land on the nearest hit so the filter visibly did something.
    const top = unitAtTop();
    const start = top ? top.from : 0;
    const target = indices.find(index => index >= start) ?? indices[0];
    if (target != null) viewport.jumpToRecord(target, "filter");
  } else if (recordState.filterHits) {
    const snapshot = recordState.filterSnapshot;
    recordState.filterHits = null; recordState.filterDirect = null; recordState.filterSnapshot = null;
    if (snapshot) { recordState.folds = snapshot.folds; recordState.processFolds = snapshot.processFolds; recordState.processExpanded = snapshot.processExpanded; }
    viewport.render();
    viewport.remeasure();
  }
  applyFilters();
}
// Reading controls (parity #7), in the options popover where the shell keeps view preferences:
// a third section after Scope and Tool types, in the popover's own row anatomy. They apply as
// custom properties and two classes on the app root (see production.css), persist with the
// other production preferences, and are also what the `w` / `-` / `+` keys drive.
const readingSection = document.createElement("div");
readingSection.className = "reading-section";
readingSection.innerHTML = `<div class="scope-menu-divider"></div><div class="scope-menu-head"><strong>Reading</strong><button class="scope-menu-action" type="button" data-reading-reset>Reset</button></div>
<div class="reading-row"><span>Code size</span><span class="reading-step"><button type="button" data-reading-size="-1" aria-label="Smaller code">−</button><span class="reading-value" data-reading-value></span><button type="button" data-reading-size="1" aria-label="Larger code">+</button></span></div>
<div class="reading-row"><span>Wrap long lines</span><button class="mode-switch" type="button" role="switch" data-reading-toggle="wrap" aria-label="Wrap long lines" aria-checked="false"><span></span></button></div>
<div class="reading-row"><span>Wide transcript</span><button class="mode-switch" type="button" role="switch" data-reading-toggle="wide" aria-label="Wide transcript" aria-checked="false"><span></span></button></div>
<div class="reading-row"><span>User turns as raw text</span><button class="mode-switch" type="button" role="switch" data-reading-toggle="rawUser" aria-label="Show user turns as raw text — exactly as typed, whitespace intact" aria-checked="false"><span></span></button></div>`;
byId("navigatorOptions").append(readingSection);
function applyReading() {
  const prefs = uiState.reading;
  // #109: the raw-text preference is the renderer's business — mirror it and re-render the
  // mounted turns when it changes (the first application, before any session, renders nothing).
  const rawChanged = recordState.rawUser !== !!prefs.rawUser;
  recordState.rawUser = !!prefs.rawUser;
  if (rawChanged) recordState.rawTurns.clear(); // a global change clears per-turn overrides, as the classic page does
  if (rawChanged && recordState.records.length) viewport.render();
  for (const [name, value] of Object.entries(readingVars(prefs))) app.style.setProperty(name, value);
  app.classList.toggle("wrap-code", !!prefs.wrap); app.classList.toggle("wide", !!prefs.wide);
  readingSection.querySelector("[data-reading-value]").textContent = `${clampSize(prefs.size)} px`;
  for (const toggle of readingSection.querySelectorAll("[data-reading-toggle]")) toggle.setAttribute("aria-checked", String(!!prefs[toggle.dataset.readingToggle]));
  readingSection.querySelector('[data-reading-size="-1"]').disabled = prefs.size <= SIZE_MIN;
  readingSection.querySelector('[data-reading-size="1"]').disabled = prefs.size >= SIZE_MAX;
  viewport.remeasure();
}
function setReading(patch) { uiState.reading = { ...uiState.reading, ...patch, size: clampSize(patch.size ?? uiState.reading.size) }; uiState.readingChosen = true; persist(); applyReading(); }
readingSection.onclick = event => {
  const step = event.target.closest("[data-reading-size]"); if (step) { setReading({ size: uiState.reading.size + Number(step.dataset.readingSize) * SIZE_STEP }); return; }
  const toggle = event.target.closest("[data-reading-toggle]"); if (toggle) { const key = toggle.dataset.readingToggle; setReading({ [key]: !uiState.reading[key] }); return; }
  if (event.target.closest("[data-reading-reset]")) setReading({ size: 12, wrap: false, wide: false });
};
applyReading();
byId("filterTranscriptBtn").onclick = () => { byId("navigatorOptions").classList.toggle("open"); renderFilterMenu(); };
byId("navigatorOptions").onclick = event => { const scope = event.target.closest("[data-scope]"); if (scope) { uiState.searchScopes.has(scope.dataset.scope) ? uiState.searchScopes.delete(scope.dataset.scope) : uiState.searchScopes.add(scope.dataset.scope); renderFilterMenu(); applyFilters(); } const tool = event.target.closest("[data-tool-filter]"); if (tool) { uiState.toolFilters.has(tool.dataset.toolFilter) ? uiState.toolFilters.delete(tool.dataset.toolFilter) : uiState.toolFilters.add(tool.dataset.toolFilter); renderFilterMenu(); applyToolFilter(); } };
byId("selectAllScopes").onclick = () => { uiState.searchScopes = new Set(["u", "a", "t", "o", "b", "r", "e"]); renderFilterMenu(); };
byId("clearTranscriptFilters").onclick = () => { uiState.toolFilters.clear(); renderFilterMenu(); applyToolFilter(); };

function openGlobalSearch() { byId("searchLayer").classList.add("production-open"); byId("searchInput").value = ""; uiState.searchTab = "all"; renderGlobalSearch(); byId("searchInput").focus(); }
function globalRows(query) {
  const rows = [];
  for (const agent of groupedSessions()) { const firstAgentSession = agent.projects.flatMap(project => project.sessions)[0]; rows.push({ kind: "agent", label: agent.name, meta: `${agent.projects.length} projects`, sid: firstAgentSession?.id }); for (const project of agent.projects) { rows.push({ kind: "project", label: project.name, meta: agent.name, sid: project.sessions[0]?.id }); for (const session of project.sessions) rows.push({ kind: "session", label: session.name || session.id, meta: `${agent.name} · ${project.name}`, sid: session.id }); } }
  if (query) recordState.records.forEach((record, index) => { if (plainText(record).toLowerCase().includes(query)) rows.push({ kind: "transcript", label: record.label || record.head?.summary || record.kind, meta: "current session", record: index, searchHit: true }); });
  return rows;
}
function renderGlobalSearch() {
  const query = byId("searchInput").value.trim().toLowerCase();
  const rows = globalRows(query).filter(row => (uiState.searchTab === "all" || row.kind === uiState.searchTab) && (!query || row.searchHit || `${row.label} ${row.meta}`.toLowerCase().includes(query))).slice(0, 80);
  uiState.globalResults = rows; uiState.globalIndex = Math.min(uiState.globalIndex, Math.max(0, rows.length - 1));
  byId("searchResults").innerHTML = `${uiState.searchTab === "transcript" ? '<div class="search-scope-note">Transcript search covers the current session only</div>' : ""}${rows.map((row, index) => `<button class="search-result ${index === uiState.globalIndex ? "active" : ""}" data-global-index="${index}"><span class="result-copy"><b>${escapeText(row.label)}</b><small>${escapeText(row.meta)}</small></span><span class="result-kind">${escapeText(row.kind)}</span></button>`).join("") || '<div class="no-results">No matches</div>'}`;
}
byId("searchBtn").onclick = openGlobalSearch; byId("sidebarMiniSearch").onclick = openGlobalSearch;
byId("searchInput").oninput = () => { uiState.globalIndex = 0; renderGlobalSearch(); };
byId("searchTabs").onclick = event => { const tab = event.target.closest("[data-search-tab]"); if (!tab) return; uiState.searchTab = tab.dataset.searchTab; byId("searchTabs").querySelectorAll("[data-search-tab]").forEach(item => item.setAttribute("aria-selected", String(item === tab))); renderGlobalSearch(); };
byId("searchResults").onclick = event => { const item = event.target.closest("[data-global-index]"); if (!item) return; const row = uiState.globalResults[Number(item.dataset.globalIndex)]; byId("searchLayer").classList.remove("production-open"); if (row.sid) selectSession(row.sid, true); else if (row.record != null) viewport.jumpToRecord(row.record, "search"); };
byId("searchLayer").onclick = event => { if (event.target === byId("searchLayer")) byId("searchLayer").classList.remove("production-open"); };

function openAttachment(id, path, fsig, action = "preview", sig = "") {
  const record = findRecord(id); const head = record?.head || {};
  const item = { id: `attachment:${id || path}`, name: head.att_name || path.split("/").pop() || "attachment", path: head.att_path || path, fsig: head.att_fsig || fsig, sig: head.att_sig || sig, text: head.att_text, data: head.att_datauri, embedded: head.att_datauri != null || head.att_text != null };
  item.source = item.data || (item.path && item.fsig ? `/file?path=${encodeURIComponent(item.path)}&sig=${encodeURIComponent(item.fsig)}` : "");
  if (action === "image") attachmentViewer.openImage(item);
  else if (action === "download") attachmentViewer.download(item);
  else if (action === "copy") attachmentViewer.copyPath(item);
  else if (action === "reveal") attachmentViewer.reveal(item);
  else preview.open(item);
}
// A path the server offered with stamps — from a tool header, an attachment, or a link that
// matched one. `referenceAction` decides by capability: file stamp → preview, reveal stamp →
// file manager, neither → the path goes to the clipboard.
function openReferenceOffer({ path, fileSig = "", revealSig = "", record = null }) {
  const head = record?.head || {};
  const item = { id: `reference:${path}`, name: head.att_name || path.split("/").pop() || "file", path, fsig: fileSig, sig: revealSig, text: head.att_text, data: head.att_datauri };
  const action = referenceAction({ fileSig, revealSig });
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
  button.classList.toggle("show", !recordState.following);
  button.classList.toggle("has-new", n > 0);
  button.setAttribute("aria-hidden", String(recordState.following));
  let count = button.querySelector(".jump-count");
  if (!count) { count = document.createElement("span"); count.className = "jump-count"; button.append(count); }
  count.textContent = n ? `${n} new message${n === 1 ? "" : "s"}` : "";
  const label = n ? `${n} new message${n === 1 ? "" : "s"} — jump to the latest` : "Jump to the latest";
  button.title = label; button.setAttribute("aria-label", label);
}
byId("jumpToBottom").onclick = () => viewport.toBottom(true);
function updateStickyHeaders() { const top = transcript.getBoundingClientRect().top; viewport.window.querySelectorAll("[data-process-surface]").forEach(surface => { const rect = surface.getBoundingClientRect(); surface.dataset.prodSticky = String(rect.top < top && rect.bottom > top + 40); }); }

byId("attentionBtn").onclick = () => { indexState.attention = !indexState.attention; byId("attentionBtn").classList.toggle("on", indexState.attention); byId("attentionBtn").setAttribute("aria-pressed", String(indexState.attention)); byId("sidebarMiniAttention").classList.toggle("on", indexState.attention); renderTree(); };
byId("sidebarMiniAttention").onclick = () => byId("attentionBtn").click();
byId("themeBtn").onclick = () => { const dark = document.documentElement.dataset.theme !== "dark"; document.documentElement.dataset.theme = dark ? "dark" : ""; localStorage.setItem("am-demo-theme", dark ? "dark" : "light"); };
if (localStorage.getItem("am-demo-theme") === "dark") document.documentElement.dataset.theme = "dark";
function toggleSidebar(open) { indexState.sidebarOpen = open; app.classList.toggle("sidebar-off", !open); persist(); viewport.remeasure(); }
byId("sidebarCollapse").onclick = () => toggleSidebar(false); byId("sidebarMiniExpand").onclick = () => toggleSidebar(true); byId("sidebarReopen").onclick = () => toggleSidebar(true);
// The rail's write button reaches the write switch itself (#54): the demo clicks the write
// BUTTON, whose handler ignores a click that lands on a button — a programmatic click does.
byId("sidebarMiniWrite").onclick = () => byId("writeSwitch").click();
byId("sidebarCollapse").title = `Collapse the sidebar into its icon rail  ( ${hintFor("sidebar-toggle")} )`;
byId("sidebarMiniExpand").title = `Expand the sidebar  ( ${hintFor("sidebar-toggle")} )`;
// The outline pane has three states (#55): open, collapsed to its icon rail ("off"), and
// HIDDEN — gone altogether, the transcript taking the whole remaining width. The header's
// toggle walks open ↔ rail as before and brings a hidden pane back; `o` and the rail's own
// button hide it. Each change re-measures the transcript at its new width holding the unit at
// the top of the view, so the reader's place does not move with the reflow.
function toggleNavigator(open) { uiState.navigatorOpen = open; if (open) uiState.navigatorHidden = false; persist(); renderNavigator(); viewport.remeasure(); }
function setNavigatorHidden(hidden) { uiState.navigatorHidden = hidden; persist(); renderNavigator(); viewport.remeasure(); }
byId("navigatorToggle").onclick = () => (uiState.navigatorHidden ? setNavigatorHidden(false) : toggleNavigator(!uiState.navigatorOpen));
const navigatorRailHide = document.createElement("button");
navigatorRailHide.type = "button";
navigatorRailHide.id = "navigatorRailHide";
navigatorRailHide.title = `Hide the outline — the transcript takes the whole width  ( ${hintFor("navigator-toggle")} )`;
navigatorRailHide.setAttribute("aria-label", navigatorRailHide.title);
navigatorRailHide.innerHTML = '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8"/></svg>';
byId("navigatorRailExpand").insertAdjacentElement("afterend", navigatorRailHide);
navigatorRailHide.onclick = () => setNavigatorHidden(true);
byId("navigatorClose").onclick = () => toggleNavigator(false);
byId("navigatorRailExpand").onclick = () => toggleNavigator(true);
byId("navigatorToggle").title = `Show, collapse or bring back the outline  ( ${hintFor("navigator-toggle")} hides it )`;
// The published-artifact roster (#78): every artifact this session published, one row per
// URL with its republish count — twenty calls for two decks read as two rows. Always present
// and grayed when the session published nothing (the classic page's rule: a control that
// comes and goes cannot be found, and its absence is indistinguishable from a bug).
const artifactsBtn = document.createElement("button");
artifactsBtn.type = "button";
artifactsBtn.className = "artifacts-btn";
artifactsBtn.id = "artifactsBtn";
artifactsBtn.setAttribute("aria-haspopup", "menu");
artifactsBtn.setAttribute("aria-expanded", "false");
const artifactsMenu = document.createElement("div");
artifactsMenu.className = "artifacts-menu";
artifactsMenu.id = "artifactsMenu";
artifactsMenu.setAttribute("role", "menu");
artifactsMenu.hidden = true;
document.querySelector(".session-heading").append(artifactsBtn, artifactsMenu);
function renderArtifacts() {
  const rows = recordState.session === indexState.selected ? artifactRoster(recordState.records) : [];
  artifactsBtn.textContent = rows.length ? `Artifacts (${rows.length}) ▾` : "Artifacts ▾";
  artifactsBtn.classList.toggle("disabled", rows.length === 0);
  artifactsBtn.title = rows.length ? "What this session published" : "This session published nothing";
  artifactsMenu.innerHTML = rows.map(r => `<div class="artifacts-row" role="menuitem"><a href="${escapeText(r.url)}" target="_blank" rel="noopener" title="${escapeText(r.desc || r.url)}">${r.icon ? `<span class="artifacts-icon">${escapeText(r.icon)}</span>` : ""}<span class="artifacts-name">${escapeText(r.name || r.url)}</span>${r.desc ? `<span class="artifacts-desc">${escapeText(r.desc)}</span>` : ""}${r.count > 1 ? `<span class="artifacts-count">×${r.count}</span>` : ""}</a><button type="button" class="artifacts-jump" data-artifact-record="${r.at}" title="Go to where it was last published" aria-label="Go to where ${escapeText(r.name || r.url)} was last published">↳</button></div>`).join("");
  if (!rows.length) { artifactsMenu.hidden = true; artifactsBtn.setAttribute("aria-expanded", "false"); }
}
artifactsBtn.onclick = () => { if (artifactsBtn.classList.contains("disabled")) return; const open = artifactsMenu.hidden; artifactsMenu.hidden = !open; artifactsBtn.setAttribute("aria-expanded", String(open)); };
artifactsMenu.addEventListener("click", event => {
  const jump = event.target.closest("[data-artifact-record]");
  if (jump) { artifactsMenu.hidden = true; artifactsBtn.setAttribute("aria-expanded", "false"); viewport.jumpToRecord(Number(jump.dataset.artifactRecord), "artifact"); }
});
document.addEventListener("pointerdown", event => { if (!artifactsMenu.hidden && !artifactsMenu.contains(event.target) && event.target !== artifactsBtn) { artifactsMenu.hidden = true; artifactsBtn.setAttribute("aria-expanded", "false"); } });
// The task details popover (#60): one element, filled per task, anchored beside the row;
// Escape and its close control dismiss it and focus returns to the row it came from.
const taskPopover = document.createElement("div");
taskPopover.className = "task-popover";
taskPopover.id = "taskPopover";
taskPopover.setAttribute("role", "dialog");
taskPopover.setAttribute("aria-modal", "false");
taskPopover.hidden = true;
document.body.append(taskPopover);
let taskPopoverOpener = null;
function closeTaskPopover(restoreFocus = true) {
  if (taskPopover.hidden) return;
  taskPopover.hidden = true;
  taskPopover.innerHTML = "";
  if (restoreFocus && taskPopoverOpener?.isConnected) taskPopoverOpener.focus();
  taskPopoverOpener = null;
}
function openTaskPopover(index, opener) {
  const tasks = recordState.meta?.tasks || [];
  const task = tasks[index];
  if (!task) return;
  const key = String(task.id ?? index);
  const d = taskDetails(task, recordState.taskTargets.get(key) ?? null);
  const list = (items, empty) => items.length ? items.map(i => `<code>#${escapeText(i)}</code>`).join(" ") : `<span class="task-popover-none">${empty}</span>`;
  const jump = d.target == null
    ? `<button type="button" class="task-popover-jump" disabled title="This record stream did not keep where the task's status was set">Go to the turn — not kept in this stream</button>`
    : `<button type="button" class="task-popover-jump" data-task-record="${d.target}">Go to the turn where this task's status was recorded</button>`;
  taskPopover.innerHTML = `<div class="task-popover-head"><span class="task-state ${escapeText(d.status === "in_progress" ? "running" : d.status)}"></span><strong>${escapeText(d.subject)}</strong><span class="task-popover-id">#${escapeText(d.id)}</span><button type="button" class="task-popover-close" aria-label="Close">×</button></div>
    <dl class="task-popover-facts"><dt>Status</dt><dd>${escapeText(d.label)}</dd>${d.activeForm ? `<dt>While running</dt><dd>${escapeText(d.activeForm)}</dd>` : ""}<dt>Blocked by</dt><dd>${list(d.blockedBy, "nothing")}</dd><dt>Blocks</dt><dd>${list(d.blocks, "nothing")}</dd></dl>
    <div class="task-popover-body">${d.paragraphs.length ? d.paragraphs.map(p => `<p>${escapeText(p)}</p>`).join("") : '<p class="task-popover-none">No description recorded.</p>'}</div>
    <div class="task-popover-actions">${jump}</div>`;
  taskPopoverOpener = opener;
  taskPopover.hidden = false;
  const r = opener.getBoundingClientRect(), w = Math.min(440, window.innerWidth - 24);
  const left = Math.min(window.innerWidth - w - 12, r.right + 8);
  taskPopover.style.left = `${Math.max(12, left)}px`;
  taskPopover.style.top = `${Math.max(12, Math.min(window.innerHeight - 24 - taskPopover.offsetHeight, r.top))}px`;
  taskPopover.querySelector(".task-popover-close").focus();
}
taskPopover.addEventListener("click", event => {
  if (event.target.closest(".task-popover-close")) { closeTaskPopover(); return; }
  const jump = event.target.closest("[data-task-record]");
  if (jump) { const at = Number(jump.dataset.taskRecord); closeTaskPopover(false); viewport.jumpToRecord(at, "task"); }
});
document.addEventListener("keydown", event => { if (event.key === "Escape" && !taskPopover.hidden) { event.stopPropagation(); closeTaskPopover(); } }, true);
document.addEventListener("pointerdown", event => { if (!taskPopover.hidden && !taskPopover.contains(event.target) && !event.target.closest("[data-task-open]")) closeTaskPopover(false); });
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
document.querySelector('[data-nav-card="tasks"] .outline-card-head').insertAdjacentElement("afterend", tasksCenter);
function paneScroller(el) {
  for (let node = el.parentElement; node; node = node.parentElement) {
    const o = getComputedStyle(node).overflowY;
    if ((o === "auto" || o === "scroll") && node.scrollHeight > node.clientHeight && !node.contains(transcript)) return node;
  }
  return null;
}
function centerTasks() {
  const tasks = recordState.meta?.tasks || [];
  const target = taskCenterTarget(taskOrder(tasks));
  if (!target) return false;
  if (!uiState.navCards.has("tasks")) { uiState.navCards.add("tasks"); persist(); renderNavigator(); }
  if (uiState.navigatorHidden) setNavigatorHidden(false);
  if (!uiState.navigatorOpen) toggleNavigator(true);
  const row = document.querySelectorAll("#navigatorWork .work-task")[target.index];
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
byId("sessionFoldAll").onclick = () => { const close = recordState.units.some(unit => unit.type === "process" && !recordState.processFolds.get(unit.key)); for (const unit of recordState.units) if (unit.type === "process") recordState.processFolds.set(unit.key, close); viewport.render(); };
byId("collapseBtn").onclick = () => { for (const agent of groupedSessions()) { indexState.collapsed.add(`a:${agent.id}`); for (const project of agent.projects) indexState.collapsed.add(`p:${project.id}`); } persist(); renderTree(); };
// Expand-all (#77, parity item 12): the counterpart of collapse-all — every agent and project
// group open again, remembered like the folds are. Runtime chrome beside its counterpart.
const expandBtn = document.createElement("button");
expandBtn.type = "button";
expandBtn.className = "iconbtn";
expandBtn.id = "expandBtn";
expandBtn.innerHTML = svg("expandStack");
expandBtn.title = "Expand every group";
expandBtn.setAttribute("aria-label", "Expand every group");
byId("collapseBtn").insertAdjacentElement("afterend", expandBtn);
expandBtn.onclick = () => { indexState.collapsed.clear(); persist(); renderTree(); };
byId("mobileBack").onclick = () => app.classList.remove("mobile-detail");
addEventListener("popstate", () => { const id = new URLSearchParams(location.search).get("session"); if (id && id !== indexState.selected) selectSession(id, false); });
addEventListener("hashchange", () => { landedHash = ""; landOnHash(); });
addEventListener("resize", () => { viewport.remeasure(); updateStickyHeaders(); }, { passive: true });
// The keymap (parity #11): the classic view's keys, resolved by keymap.js, acted on here.
const userUnits = () => recordState.units.filter(unit => unit.type === "user");
// The unit the reader is "at": the first mounted unit that starts at (or within a line above)
// the viewport top. A landed turn sits 18px down, so the element ending just above it must
// not count as current — that made `]` land on the same turn twice.
function unitAtTop() {
  const top = viewport.scroller.getBoundingClientRect().top;
  for (const child of viewport.window.children) if (child.getBoundingClientRect().top >= top - 24) return child.dataset.unitKey;
  return null;
}
function currentUserUnitIndex() {
  return currentTurnIndex(recordState.units, unitAtTop());
}
// The pane follows the transcript (#52): the outline row of the turn at the viewport top is the
// current one — marked (`current`, `aria-current`) on every scroll and render, and revealed in
// the pane's OWN scroller (never the transcript's), only when the current row changes, so the
// spy never fights the reader. The click direction — a row jumps the transcript to its turn —
// lands that turn at the top, so the spy then names the row that was clicked.
function updateOutlineFocus() {
  const rows = byId("navigatorTurns").querySelectorAll(".outline-turn-row");
  if (!rows.length) { outlineCurrent = null; return; }
  const index = currentTurnIndex(recordState.units, unitAtTop());
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
  if (target) revealInPane(target);
}
function revealInPane(row) {
  let pane = row.parentElement;
  while (pane && pane !== document.body) {
    const overflow = getComputedStyle(pane).overflowY;
    if ((overflow === "auto" || overflow === "scroll") && pane.scrollHeight > pane.clientHeight) break;
    pane = pane.parentElement;
  }
  if (!pane || pane === document.body || pane.contains(transcript)) return;
  const bounds = pane.getBoundingClientRect(); const rect = row.getBoundingClientRect();
  if (rect.top < bounds.top) pane.scrollTop -= bounds.top - rect.top;
  else if (rect.bottom > bounds.bottom) pane.scrollTop += rect.bottom - bounds.bottom;
}
function stepTurn(delta) {
  const turns = userUnits(); if (!turns.length) return;
  const next = Math.max(0, Math.min(turns.length - 1, currentUserUnitIndex() + delta));
  viewport.jumpToRecord(turns[next].from, "turn");
}
function stepHead(delta) {
  const heads = [...transcript.querySelectorAll("button.renderer-head")].filter(head => head.offsetParent !== null);
  if (!heads.length) return;
  const at = heads.indexOf(document.activeElement);
  const next = at < 0 ? (delta > 0 ? 0 : heads.length - 1) : Math.max(0, Math.min(heads.length - 1, at + delta));
  viewport.lastUserInput = performance.now();
  heads[next].focus({ preventScroll: true }); heads[next].scrollIntoView({ block: "nearest" });
}
function stepList(delta) {
  const rows = [...tree.querySelectorAll(".tree-row.session")];
  const at = rows.indexOf(document.activeElement); if (at < 0) return;
  const next = Math.max(0, Math.min(rows.length - 1, at + delta));
  rows[next].focus(); selectSession(rows[next].dataset.session, true);
}
function pageTranscript(direction) {
  viewport.lastUserInput = performance.now();
  viewport.scroller.scrollBy({ top: direction * Math.round(viewport.scroller.clientHeight * 0.85), behavior: "auto" });
}
const keyActions = {
  "search": () => { if (indexState.selected) byId("transcriptSearchInput").focus(); else openGlobalSearch(); },
  "turn-next": () => stepTurn(1), "turn-prev": () => stepTurn(-1),
  "head-next": () => stepHead(1), "head-prev": () => stepHead(-1),
  "hit-next": () => stepSearch(1), "hit-prev": () => stepSearch(-1),
  "wrap": () => setReading({ wrap: !uiState.reading.wrap }),
  "size-down": () => setReading({ size: uiState.reading.size - SIZE_STEP }), "size-up": () => setReading({ size: uiState.reading.size + SIZE_STEP }),
  "page-down": () => pageTranscript(1), "page-up": () => pageTranscript(-1),
  "list-next": () => stepList(1), "list-prev": () => stepList(-1),
  "sidebar-toggle": () => toggleSidebar(!indexState.sidebarOpen),
  "navigator-toggle": () => setNavigatorHidden(!uiState.navigatorHidden),
  "tasks-center": () => centerTasks(),
  "parent": () => { if (parentBtn.dataset.parent) selectSession(parentBtn.dataset.parent, true); }
};
bindKeymap(document, target => (target?.closest?.(".tree-row") ? "list" : "view"), action => keyActions[action]?.());
// Discoverability in the shell's idiom: the key in the control's own title / hint.
byId("transcriptSearchInput").placeholder = `Search this session  ( ${hintFor("search")} )`;
byId("findPrev").title = `Previous match (${hintFor("hit-prev")})`; byId("findNext").title = `Next match (${hintFor("hit-next")})`;
byId("turnPrev").title = `Previous turn (${hintFor("turn-prev")})`; byId("turnNext").title = `Next turn (${hintFor("turn-next")})`;
// The header's own turn steppers were in the design but never bound; they step the same way the keys do.
byId("turnPrev").onclick = () => stepTurn(-1); byId("turnNext").onclick = () => stepTurn(1);
readingSection.querySelector('[data-reading-toggle="wrap"]').title = `Wrap long lines (${hintFor("wrap")})`;
readingSection.querySelector('[data-reading-size="-1"]').title = `Smaller code (${hintFor("size-down")})`; readingSection.querySelector('[data-reading-size="1"]').title = `Larger code (${hintFor("size-up")})`;
addEventListener("keydown", event => { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openGlobalSearch(); } else if (event.key === "Escape") { setSessionCopyMenu(false); byId("searchLayer").classList.remove("production-open"); byId("navigatorOptions").classList.remove("open"); } });

app.classList.toggle("sidebar-off", !indexState.sidebarOpen);
tree.innerHTML = '<div class="no-results">Scanning sessions…</div>';
byId("sidebarMiniAgents").innerHTML = "";
renderHeader(); renderNavigator(); paintJump(); sessionIndex.start();
// #114: a reload, a close or the tab going to the background saves the reader's choices with
// the position (the debounced save may not have run yet).
addEventListener("pagehide", () => viewport.remember());
document.addEventListener("visibilitychange", () => { if (document.hidden) viewport.remember(); });
