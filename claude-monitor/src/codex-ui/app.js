import { agentLogo, svg } from "./icons.js";
import { AttachmentViewer } from "./attachment-viewer.js";
import { bindComponentEvents, referenceAction } from "./components.js";
import { ControlStore } from "./control-store.js";
import { Preview } from "./preview.js";
import { RecordStore } from "./record-store.js";
import { SessionIndexStore } from "./session-index-store.js";
import { controlState, indexState, persist, recordState, selectedRow, uiState } from "./state.js";
import { hideAction, ignoreQuery, visibleTree } from "./session-visibility.js";
import { agentRecordTargets, escapeText, plainText, Projection, taskRecordTargets, taskStatus } from "./view-model.js";
import { Viewport } from "./viewport.js";

const byId = id => document.getElementById(id);
const app = byId("app");
const tree = byId("tree");
const transcript = byId("transcript");
const projection = new Projection();
let toastTimer = 0;
let landedHash = "";

document.querySelectorAll("[data-icon]").forEach(element => { element.innerHTML = svg(element.dataset.icon); });
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
  afterRender: () => { applyFilters(); markSearch(); updateStickyHeaders(); },
  afterScroll: updateStickyHeaders,
  followChanged: paintJump
});
// Delegate from the stable scroll host: the virtual window may replace all of its children
// while a pull reconciliation is in flight, but attachment/fold interactions must survive it.
bindComponentEvents(transcript, recordState, {
  rerender: () => viewport.render(),
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
    if (indexState.selected && !selectedRow()) sessionGone();
    renderTree(); renderHeader(); controls.paint();
    if (!indexState.selected) {
      const requested = new URLSearchParams(location.search).get("session");
      const first = requested && indexState.rows.has(requested) ? requested : [...indexState.rows.values()].find(row => !row.hidden)?.id;
      if (first) selectSession(first, false);
    }
  },
  error: () => toast("Session scan failed — retrying")
});
const recordStore = new RecordStore({
  reset: () => { lastRecordCount = 0; projection.units = []; recordState.records = []; recordState.meta = null; recordState.heights.clear(); recordState.folds.clear(); recordState.processFolds.clear(); recordState.processExpanded.clear(); recordState.promptExpanded.clear(); recordState.taskTargets.clear(); recordState.agentTargets.clear(); recordState.search = ""; byId("transcriptSearchInput").value = ""; viewport.showEmpty("Loading session…", "Reading the normalized record stream."); renderHeader(); renderNavigator(); },
  update: updateRecords,
  error: (error, hasRecords) => hasRecords ? toast(`${error.message}；retrying`) : viewport.showEmpty("Cannot read this session", `${error.message}；The monitor will retry.`, true)
});

function toast(message) {
  const element = byId("toast"); element.textContent = message; element.classList.add("show");
  clearTimeout(toastTimer); toastTimer = setTimeout(() => element.classList.remove("show"), 1900);
}

function displayState(row) {
  const state = row.agentState || (row.state === "growing" ? "busy" : "idle");
  const reason = row.stateReason || (row.state === "finished" ? "exited" : row.state);
  const labels = { busy: "Running", wait: "Needs you", idle: "Idle", permission: "Awaiting permission", question: "Awaiting an answer", "plan-approval": "Awaiting approval", done: "Done", error: "Failed", stalled: "Stalled", "ended-question": "Awaiting a reply", exited: "Done", "exited-mid-work": "Exited abnormally", thinking: "Thinking", tool: "Running a tool", starting: "Starting" };
  return { state, reason, label: labels[reason] || labels[state] || reason };
}
function needs(row) { const s = displayState(row); return s.state === "wait" || ["question", "ended-question", "error", "stalled", "exited-mid-work", "permission", "plan-approval"].includes(s.reason); }
function stateDenote(row) {
  const s = displayState(row);
  if (s.state === "busy") return { label: "Running", tone: "busy" };
  if (s.state === "wait") return { label: s.label, tone: `wait${row.stateConfidence === "inferred" ? " inferred" : ""}` };
  if (["question", "ended-question"].includes(s.reason)) return { label: "Awaiting reply", tone: "attention" };
  if (["error", "stalled", "exited-mid-work"].includes(s.reason)) return { label: s.label, tone: "danger" };
  if ((row.activityTs || 0) > Number(indexState.read[row.id] || 0)) return { label: "New result", tone: "unread" };
  return null;
}

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

function sessionTreeRow(row) {
  const denote = stateDenote(row);
  const marker = denote ? `<span class="session-state ${escapeText(denote.tone)}" role="img" aria-label="${escapeText(denote.label)}" title="${escapeText(denote.label)}"></span>` : "";
  return `<div class="tree-row session ${row.id === indexState.selected ? "selected" : ""} ${row.hidden ? "is-hidden" : ""}" role="treeitem" tabindex="0" data-session="${escapeText(row.id)}" title="${escapeText(displayState(row).label)} · ${escapeText(row.stateDetail || "")}"><span class="tree-title">${escapeText(row.name || row.id)}</span><span class="session-end">${treeAction(row, "session")}${marker}<span class="session-age">${escapeText(row.activity || "")}</span></span></div>`;
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
      let shown = expanded ? rows : rows.slice(0, SIDEBAR_SESSION_LIMIT);
      const selected = rows.find(row => row.id === indexState.selected);
      if (!expanded && selected && !shown.includes(selected)) shown = shown.concat(selected);
      for (const row of shown) html += sessionTreeRow(row);
      const hidden = rows.filter(row => !shown.includes(row)).length;
      if (rows.length > SIDEBAR_SESSION_LIMIT) html += `<button class="tree-project-more" type="button" data-project-more="${escapeText(overflowKey)}" aria-expanded="${expanded}"><span>${expanded ? "Show fewer" : `Show ${hidden} more`}</span><span aria-hidden="true">${expanded ? "⌃" : "⌄"}</span></button>`;
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
  const session = event.target.closest("[data-session]"); if (session) { selectSession(session.dataset.session, true); return; }
  const more = event.target.closest("[data-project-more]"); if (more) { const key = more.dataset.projectMore; indexState.expandedProjects.has(key) ? indexState.expandedProjects.delete(key) : indexState.expandedProjects.add(key); persist(); renderTree(); return; }
  const toggle = event.target.closest("[data-toggle]"); if (toggle) { const key = toggle.dataset.toggle; indexState.collapsed.has(key) ? indexState.collapsed.delete(key) : indexState.collapsed.add(key); persist(); renderTree(); }
};
tree.onkeydown = event => { if (["Enter", " "].includes(event.key)) { event.preventDefault(); event.target.click(); } };
byId("sidebarMiniAgents").onclick = event => { const button = event.target.closest("[data-mini-agent-session]"); if (button) selectSession(button.dataset.miniAgentSession, true); };

function selectSession(id, push) {
  if (!id || (id === indexState.selected && recordStore.session === id)) return;
  indexState.selected = id; app.classList.add("mobile-detail");
  preview.setSession(id);
  const row = selectedRow(); if (row) { indexState.read[id] = row.activityTs || Date.now() / 1000; persist(); }
  renderTree(); renderHeader(); controls.paint();
  const url = new URL(location.href); url.searchParams.set("session", id); if (document.body.dataset.uiDefault === "true") url.searchParams.delete("ui"); else url.searchParams.set("ui", "app"); if (push) url.hash = ""; history[push ? "pushState" : "replaceState"]({}, "", url);
  recordState.session = id; recordStore.open(id);
}
function sessionGone() { recordStore.stop(); preview.setSession(""); indexState.selected = ""; recordState.session = ""; viewport.showEmpty("Session is gone", "It may have been deleted or moved. The list keeps scanning.", true); }

function renderHeader() {
  const row = selectedRow();
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

let lastRecordCount = 0;
function updateRecords({ records, meta, changedFrom }) {
  const before = lastRecordCount; const wasFollowing = recordState.following;
  recordState.records = records; recordState.meta = meta;
  recordState.taskTargets = taskRecordTargets(meta?.tasks || [], records);
  recordState.agentTargets = agentRecordTargets(directAgents(meta), records);
  const changedUnit = projection.rebuild(records, changedFrom);
  recordState.units = projection.units;
  viewport.setUnits(recordState.units, changedUnit);
  landOnHash();
  const delta = Math.max(0, records.length - before);
  lastRecordCount = records.length;
  if (!wasFollowing && delta) recordState.newRecords += delta;
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
function renderNavigator() {
  const turns = recordState.units.filter(unit => unit.type === "user");
  byId("navigatorTurnCount").textContent = turns.length;
  byId("navigatorTurns").innerHTML = turns.map(unit => `<button class="outline-turn-row" data-turn-record="${unit.from}" title="${escapeText(unit.label)}"><span class="outline-number">${String(unit.turn).padStart(2, "0")} ·</span><span class="outline-label">${escapeText(unit.label)}</span></button>`).join("") || '<div class="activity-empty">No turns</div>';
  const tasks = recordState.meta?.tasks || [];
  const runningTasks = tasks.filter(task => taskStatus(task.status) === "in_progress").length;
  const doneTasks = tasks.filter(task => taskStatus(task.status) === "completed").length;
  byId("navigatorWorkCount").innerHTML = outlineSummary(runningTasks, doneTasks, tasks.length);
  byId("outlineTaskDot").hidden = !runningTasks;
  byId("navigatorWork").innerHTML = tasks.map((task, index) => {
    const key = String(task.id ?? index), target = recordState.taskTargets.get(key), status = taskStatus(task.status);
    const nav = target == null ? 'disabled aria-disabled="true" title="This record stream did not keep where the task was set"' : `data-task-record="${target}" title="Jump to where this task's status was recorded"`;
    const statusClass = status === "in_progress" ? "running" : status;
    return `<div class="work-task"><button class="work-task-head" ${nav}><span class="task-state ${escapeText(statusClass)}"></span><span class="work-copy"><strong>${escapeText(task.subject || task.title || `Task ${index + 1}`)}</strong></span><span class="work-tail">#${escapeText(task.id || index + 1)}</span></button></div>`;
  }).join("") || '<div class="activity-empty">No session tasks</div>';
  const agents = directAgents(), activeAgents = agents.filter(agent => agent.running).length;
  byId("navigatorAgentCount").innerHTML = outlineSummary(activeAgents, agents.length - activeAgents, agents.length);
  byId("navigatorAgents").innerHTML = agents.map(agent => { const target = recordState.agentTargets.get(String(agent.id)); const nav = target == null ? `data-child-outline="${escapeText(agent.id)}" title="Open the sub-agent transcript"` : `data-agent-record="${target}" title="Jump to the agent execution block in the parent session"`; return `<button class="outline-agent" ${nav}><span class="agent-state ${agent.running ? "running" : "completed"}"></span><span class="outline-agent-copy"><strong>${escapeText(agent.title || agent.description || agent.id)}</strong><small>${escapeText(agent.type || agent.agent_type || "agent")}</small></span><span class="outline-agent-tail"></span></button>`; }).join("") || '<div class="activity-empty">No direct children</div>';
  renderSessionInfo(turns.length, agents.length);
  document.querySelectorAll("[data-nav-card]").forEach(card => card.classList.toggle("open", uiState.navCards.has(card.dataset.navCard)));
  document.querySelector(".workspace").classList.toggle("navigator-off", !uiState.navigatorOpen);
  byId("navigatorToggle").classList.toggle("active", uiState.navigatorOpen);
}
const outlineSummary = (active, done, total) => !total ? "0" : `${active ? `<span class="outline-stat-item active"><i class="outline-stat-dot"></i>${active} active</span>` : ""}<span class="outline-stat-item done"><i class="outline-stat-dot"></i>${done}/${total} done</span>`;
function renderSessionInfo(turns, agents) {
  const row = selectedRow(), meta = recordState.meta || {}, usage = meta.usage || {};
  if (!row) {
    byId("navigatorSessionSummary").textContent = "—";
    byId("navigatorSession").innerHTML = '<div class="activity-empty">No session selected</div>';
    return;
  }
  byId("navigatorSessionSummary").textContent = row.cost != null ? `~$${Number(row.cost).toFixed(2)}` : usage.cost || "—";
  const group = (label, rows) => `<div class="session-info-group"><div class="session-info-label">${label}</div>${rows.map(([key, value]) => `<div class="session-info-row"><span>${escapeText(key)}</span><strong>${escapeText(value ?? "—")}</strong></div>`).join("")}</div>`;
  byId("navigatorSession").innerHTML = `<div class="session-info">${group("Session", [["title", row.name || row.id], ["agent", agentName(row.agent)], ["project", row._group?.label], ["status", displayState(row).label], ["turns", turns], ["children", agents]])}${group("Usage", [["model", usage.model], ["input", usage.input || usage.input_tokens], ["output", usage.output || usage.output_tokens], ["est. cost", usage.cost || (row.cost != null ? `~$${Number(row.cost).toFixed(2)}` : "—")]])}${group("Runtime", [["cwd", meta.cwd || row._group?.secondary], ["context", usage.context || "not provided by this protocol"], ["effort", meta.effort || "not provided by this protocol"], ["sandbox", meta.sandbox || "not provided by this protocol"], ["permission", meta.permission || "not provided by this protocol"]])}</div>`;
}

byId("sessionNavigator").onclick = event => {
  const turn = event.target.closest("[data-turn-record]"); if (turn) { viewport.jumpToRecord(Number(turn.dataset.turnRecord), "turn"); return; }
  const task = event.target.closest("[data-task-record]"); if (task) { viewport.jumpToRecord(Number(task.dataset.taskRecord), "task"); return; }
  const agent = event.target.closest("[data-agent-record]"); if (agent) { viewport.jumpToRecord(Number(agent.dataset.agentRecord), "agent"); return; }
  const child = event.target.closest("[data-child-outline]"); if (child) { selectSession(child.dataset.childOutline, true); return; }
  const card = event.target.closest("[data-nav-card-toggle]"); if (card) { const key = card.dataset.navCardToggle; uiState.navCards.has(key) ? uiState.navCards.delete(key) : uiState.navCards.add(key); persist(); renderNavigator(); return; }
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
  if (!recordState.search) return;
  const current = recordState.matches[recordState.match];
  const root = viewport.window.querySelector(`[data-block-index="${current}"]`); if (!root) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT); const texts = [];
  while (walker.nextNode()) if (walker.currentNode.parentElement && !["SCRIPT", "STYLE", "MARK"].includes(walker.currentNode.parentElement.tagName) && walker.currentNode.nodeValue.toLowerCase().includes(recordState.search)) texts.push(walker.currentNode);
  for (const text of texts) { const value = text.nodeValue, at = value.toLowerCase().indexOf(recordState.search); if (at < 0) continue; const mark = document.createElement("mark"); mark.className = "search-mark"; mark.textContent = value.slice(at, at + recordState.search.length); text.replaceWith(value.slice(0, at), mark, value.slice(at + recordState.search.length)); }
}
byId("transcriptSearchInput").oninput = () => updateSearch(true); byId("findNext").onclick = () => stepSearch(1); byId("findPrev").onclick = () => stepSearch(-1);

function renderFilterMenu() {
  const scopes = [["u", "User messages"], ["a", "Agent replies"], ["t", "Thinking"], ["o", "All tools"], ["b", "Bash output"], ["r", "Reads"], ["e", "Edits"]];
  byId("scopeRow").innerHTML = scopes.map(([key, label]) => `<button class="scope-option ${uiState.searchScopes.has(key) ? "on" : ""}" data-scope="${key}"><span class="scope-check"></span><span>${label}</span><span class="scope-key">${key}</span></button>`).join("");
  const tools = [...new Set(recordState.records.filter(record => ["bash", "read", "write", "edit", "skill", "tool"].includes(record.kind)).map(record => record.head?.name || record.tool || record.kind))];
  byId("filterOptions").innerHTML = tools.map(tool => `<button class="tool-type-option ${uiState.toolFilters.has(tool) ? "on" : ""}" data-tool-filter="${escapeText(tool)}"><span class="filter-dot"></span><span>${escapeText(tool)}</span></button>`).join("") || '<div class="tool-type-empty">No tool events in this session</div>';
  byId("filterBadge").textContent = uiState.toolFilters.size || "";
}
function applyFilters() {
  viewport.window.querySelectorAll("[data-kind]").forEach(element => {
    const kind = element.dataset.kind, tool = element.dataset.toolName;
    const scope = kind === "user" ? "u" : kind === "assistant" ? "a" : kind === "thinking" ? "t" : kind === "tool" ? (tool === "Bash" ? "b" : ["Read", "Glob", "Grep", "WebFetch", "WebSearch"].includes(tool) ? "r" : ["Write", "Edit", "NotebookEdit"].includes(tool) ? "e" : "o") : null;
    const scopeDim = scope && !uiState.searchScopes.has(scope) && !(kind === "tool" && uiState.searchScopes.has("o"));
    const toolDim = uiState.toolFilters.size && kind === "tool" && !uiState.toolFilters.has(tool);
    const dim = scopeDim || toolDim;
    element.classList.toggle("filter-dim", !!dim);
  });
}
byId("filterTranscriptBtn").onclick = () => { byId("navigatorOptions").classList.toggle("open"); renderFilterMenu(); };
byId("navigatorOptions").onclick = event => { const scope = event.target.closest("[data-scope]"); if (scope) { uiState.searchScopes.has(scope.dataset.scope) ? uiState.searchScopes.delete(scope.dataset.scope) : uiState.searchScopes.add(scope.dataset.scope); renderFilterMenu(); applyFilters(); } const tool = event.target.closest("[data-tool-filter]"); if (tool) { uiState.toolFilters.has(tool.dataset.toolFilter) ? uiState.toolFilters.delete(tool.dataset.toolFilter) : uiState.toolFilters.add(tool.dataset.toolFilter); renderFilterMenu(); applyFilters(); } };
byId("selectAllScopes").onclick = () => { uiState.searchScopes = new Set(["u", "a", "t", "o", "b", "r", "e"]); renderFilterMenu(); };
byId("clearTranscriptFilters").onclick = () => { uiState.toolFilters.clear(); renderFilterMenu(); applyFilters(); };

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

function paintJump() { const button = byId("jumpToBottom"); button.classList.toggle("show", !recordState.following); button.setAttribute("aria-hidden", String(recordState.following)); button.title = recordState.newRecords ? `${recordState.newRecords} new records` : "Jump to the latest"; }
byId("jumpToBottom").onclick = () => viewport.toBottom(true);
function updateStickyHeaders() { const top = transcript.getBoundingClientRect().top; viewport.window.querySelectorAll("[data-process-surface]").forEach(surface => { const rect = surface.getBoundingClientRect(); surface.dataset.prodSticky = String(rect.top < top && rect.bottom > top + 40); }); }

byId("attentionBtn").onclick = () => { indexState.attention = !indexState.attention; byId("attentionBtn").classList.toggle("on", indexState.attention); byId("attentionBtn").setAttribute("aria-pressed", String(indexState.attention)); byId("sidebarMiniAttention").classList.toggle("on", indexState.attention); renderTree(); };
byId("sidebarMiniAttention").onclick = () => byId("attentionBtn").click();
byId("themeBtn").onclick = () => { const dark = document.documentElement.dataset.theme !== "dark"; document.documentElement.dataset.theme = dark ? "dark" : ""; localStorage.setItem("am-demo-theme", dark ? "dark" : "light"); };
if (localStorage.getItem("am-demo-theme") === "dark") document.documentElement.dataset.theme = "dark";
function toggleSidebar(open) { indexState.sidebarOpen = open; app.classList.toggle("sidebar-off", !open); persist(); viewport.remeasure(); }
byId("sidebarCollapse").onclick = () => toggleSidebar(false); byId("sidebarMiniExpand").onclick = () => toggleSidebar(true); byId("sidebarReopen").onclick = () => toggleSidebar(true);
byId("navigatorToggle").onclick = () => { uiState.navigatorOpen = !uiState.navigatorOpen; persist(); renderNavigator(); viewport.remeasure(); };
byId("navigatorClose").onclick = () => { uiState.navigatorOpen = false; persist(); renderNavigator(); viewport.remeasure(); };
byId("navigatorRailExpand").onclick = () => { uiState.navigatorOpen = true; persist(); renderNavigator(); viewport.remeasure(); };
byId("sessionFoldAll").onclick = () => { const close = recordState.units.some(unit => unit.type === "process" && !recordState.processFolds.get(unit.key)); for (const unit of recordState.units) if (unit.type === "process") recordState.processFolds.set(unit.key, close); viewport.render(); };
byId("collapseBtn").onclick = () => { for (const agent of groupedSessions()) { indexState.collapsed.add(`a:${agent.id}`); for (const project of agent.projects) indexState.collapsed.add(`p:${project.id}`); } persist(); renderTree(); };
byId("mobileBack").onclick = () => app.classList.remove("mobile-detail");
addEventListener("popstate", () => { const id = new URLSearchParams(location.search).get("session"); if (id && id !== indexState.selected) selectSession(id, false); });
addEventListener("hashchange", () => { landedHash = ""; landOnHash(); });
addEventListener("resize", () => { viewport.remeasure(); updateStickyHeaders(); }, { passive: true });
addEventListener("keydown", event => { if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); openGlobalSearch(); } else if (event.key === "Escape") { setSessionCopyMenu(false); byId("searchLayer").classList.remove("production-open"); byId("navigatorOptions").classList.remove("open"); } });

app.classList.toggle("sidebar-off", !indexState.sidebarOpen);
tree.innerHTML = '<div class="no-results">Scanning sessions…</div>';
byId("sidebarMiniAgents").innerHTML = "";
renderHeader(); renderNavigator(); paintJump(); sessionIndex.start();
