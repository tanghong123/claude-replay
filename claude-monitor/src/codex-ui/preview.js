import { escapeText } from "./view-model.js";
import { uiState } from "./state.js";
import { sandboxDocument } from "./sandbox.js";

const byId = id => document.getElementById(id);
const SESSION_CACHE_LIMIT = 6;
const SESSION_TAB_LIMIT = 6;
const SESSION_CACHE_BYTES = 12 * 1024 * 1024;
// The pinned first tab: the session's published artifacts (#95), not a file the user opened.
const ROSTER_ID = "__artifacts";
const tabWeight = tab => 256 + 2 * String(tab.text || "").length + 2 * String(tab.data || "").length;

export class Preview {
  constructor(actions) { this.actions = actions; this.sessionId = ""; this.sessionTabs = new Map(); this.renderGeneration = 0; this.objectUrl = ""; this.roster = []; this.rosterKey = ""; this.bind(); this.setOpen(false); this.restoreWidth(); }
  bind() {
    byId("previewBtn").onclick = () => this.setOpen(!uiState.preview);
    byId("closePreview").onclick = () => this.setOpen(false);
    byId("previewHead").onclick = event => {
      const close = event.target.closest("[data-preview-tab-close]");
      if (close) { this.closeTab(close.dataset.previewTabClose); return; }
      const tab = event.target.closest("[data-preview-tab]");
      if (tab) { uiState.previewId = tab.dataset.previewTab; this.render(); }
    };
    byId("previewBody").addEventListener("click", event => {
      const jump = event.target.closest("[data-artifact-record]");
      if (jump) this.actions.jumpToRecord?.(Number(jump.dataset.artifactRecord));
    });
    const resizer = byId("resizer");
    resizer.setAttribute("role", "separator"); resizer.setAttribute("aria-orientation", "vertical"); resizer.tabIndex = 0;
    resizer.onpointerdown = event => {
      resizer.setPointerCapture(event.pointerId); resizer.classList.add("dragging"); byId("app").classList.add("resizing");
      resizer.onpointermove = move => this.setWidth(innerWidth - move.clientX, false);
      resizer.onpointerup = () => { resizer.classList.remove("dragging"); byId("app").classList.remove("resizing"); resizer.onpointermove = null; localStorage.setItem("am-demo-preview", parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--preview")) || 420); };
    };
    resizer.ondblclick = () => this.setWidth(420, true);
  }
  setOpen(open) { uiState.preview = open; byId("app").classList.toggle("preview-off", !open); byId("previewBtn").classList.toggle("active", open); if (open) this.render(); this.actions.layoutChanged?.(); }
  setWidth(value, persist) { const width = Math.max(340, Math.min(680, Number(value) || 420)); document.documentElement.style.setProperty("--preview", `${width}px`); if (persist) localStorage.setItem("am-demo-preview", width); this.actions.layoutChanged?.(); }
  restoreWidth() { this.setWidth(localStorage.getItem("am-demo-preview") || 420, false); }
  setSession(sessionId) {
    if (sessionId === this.sessionId) return;
    if (this.sessionId) {
      const tabs = uiState.previewTabs.slice(-SESSION_TAB_LIMIT);
      this.sessionTabs.delete(this.sessionId);
      this.sessionTabs.set(this.sessionId, { tabs, active: uiState.previewId, bytes: tabs.reduce((total, tab) => total + tabWeight(tab), 0) });
      const cacheBytes = () => [...this.sessionTabs.values()].reduce((total, entry) => total + entry.bytes, 0);
      while (this.sessionTabs.size > SESSION_CACHE_LIMIT || cacheBytes() > SESSION_CACHE_BYTES) this.sessionTabs.delete(this.sessionTabs.keys().next().value);
    }
    this.roster = []; this.rosterKey = ""; this.rosterBadge();
    this.sessionId = sessionId || "";
    const saved = this.sessionTabs.get(this.sessionId);
    uiState.previewTabs = saved?.tabs.slice() || [];
    uiState.previewId = saved?.active && uiState.previewTabs.some(tab => tab.id === saved.active) ? saved.active : uiState.previewTabs.at(-1)?.id || null;
    this.renderGeneration++;
    if (uiState.preview) this.render();
  }
  /** What this session published (#78), as `artifactRoster` groups it — one row per URL. It
   *  lives here rather than in a header menu (#95): a pinned first tab, and a count on the
   *  pane's own button so a closed pane still says there is something to see. Cheap to call on
   *  every header render — an unchanged roster does nothing, so an open file tab is never
   *  re-fetched underneath the reader. */
  setRoster(rows) {
    const list = Array.isArray(rows) ? rows : [];
    const key = list.map(r => `${r.url}\u0000${r.count}\u0000${r.at}\u0000${r.name}\u0000${r.icon}\u0000${r.desc}`).join("\u0001");
    if (key === this.rosterKey) return;
    this.rosterKey = key; this.roster = list;
    this.rosterBadge();
    if (uiState.preview) this.render();
  }
  rosterBadge() {
    const button = byId("previewBtn");
    const badge = button.querySelector(".preview-badge");
    if (!this.roster.length) badge?.remove();
    else (badge || button.appendChild(Object.assign(document.createElement("span"), { className: "preview-badge" }))).textContent = String(this.roster.length);
    button.title = this.roster.length ? `Open the right panel — ${this.roster.length} published artifact${this.roster.length === 1 ? "" : "s"}` : "Open the right panel";
  }
  open(item) {
    if (!uiState.previewTabs.some(tab => tab.id === item.id)) uiState.previewTabs.push(item);
    if (uiState.previewTabs.length > SESSION_TAB_LIMIT) uiState.previewTabs.splice(0, uiState.previewTabs.length - SESSION_TAB_LIMIT);
    uiState.previewId = item.id; this.setOpen(true);
  }
  closeTab(id) { uiState.previewTabs = uiState.previewTabs.filter(tab => tab.id !== id); if (uiState.previewId === id) uiState.previewId = uiState.previewTabs.at(-1)?.id || null; this.render(); }
  render() {
    const generation = ++this.renderGeneration;
    const item = uiState.previewTabs.find(tab => tab.id === uiState.previewId);
    // No file tab selected and something was published: the roster is what the pane shows —
    // so it is also what a freshly opened pane lands on, without hunting for a control.
    const roster = !item && this.roster.length > 0;
    const pinned = this.roster.length ? `<button class="preview-tab pinned ${roster ? "on" : ""}" data-preview-tab="${ROSTER_ID}" title="What this session published"><span class="preview-tab-label">Artifacts (${this.roster.length})</span></button>` : "";
    byId("previewTabs").innerHTML = pinned + uiState.previewTabs.map(tab => `<button class="preview-tab ${tab.id === uiState.previewId ? "on" : ""}" data-preview-tab="${escapeText(tab.id)}"><span class="preview-tab-label">${escapeText(tab.name)}</span><span class="preview-tab-close" data-preview-tab-close="${escapeText(tab.id)}">×</span></button>`).join("");
    if (roster) { this.showRoster(); return; }
    if (!item) { byId("previewBody").innerHTML = '<div class="preview-empty"><div class="preview-empty-icon">◇</div><strong>No file open</strong><span>Open a file, image or HTML page from the transcript.</span></div>'; return; }
    if (item.text != null || item.data) { this.show(item, item.text, item.data); return; }
    byId("previewBody").classList.add("production-loading"); byId("previewBody").textContent = "Reading securely…";
    const query = `path=${encodeURIComponent(item.path)}&sig=${encodeURIComponent(item.fsig || "")}`;
    fetch(`/file?${query}`, { cache: "no-store" }).then(response => {
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const type = response.headers.get("content-type") || "";
      return type.startsWith("image/") ? response.blob().then(blob => { if (generation === this.renderGeneration) this.show(item, null, URL.createObjectURL(blob)); }) : response.text().then(text => { if (generation === this.renderGeneration) this.show(item, text, null); });
    }).catch(error => {
      if (generation !== this.renderGeneration) return;
      const body = byId("previewBody"); body.classList.remove("production-loading");
      body.innerHTML = `<div class="preview-error"><strong>Cannot preview this file</strong><span>${escapeText(error.message)} · The original path may be gone, or the file is outside what this monitor may read.</span><div class="preview-error-actions"><button class="smallbtn" data-copy-path>Copy original path</button><button class="smallbtn" data-close-preview>Close tab</button></div></div>`;
      body.querySelector("[data-copy-path]").onclick = () => {
        const operation = navigator.clipboard?.writeText(item.path || "");
        if (operation) operation.then(() => this.actions.toast?.("Copied the original path"));
        else this.actions.toast?.("This browser does not support copying");
      };
      body.querySelector("[data-close-preview]").onclick = () => this.closeTab(item.id);
    });
  }
  showRoster() {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    body.innerHTML = `<div class="artifacts-list">${this.roster.map(r => `<div class="artifacts-row"><a href="${escapeText(r.url)}" target="_blank" rel="noopener" title="${escapeText(r.desc || r.url)}">${r.icon ? `<span class="artifacts-icon">${escapeText(r.icon)}</span>` : ""}<span class="artifacts-name">${escapeText(r.name || r.url)}</span>${r.desc ? `<span class="artifacts-desc">${escapeText(r.desc)}</span>` : ""}${r.count > 1 ? `<span class="artifacts-count">×${r.count}</span>` : ""}</a><button type="button" class="artifacts-jump" data-artifact-record="${r.at}" title="Go to where it was last published" aria-label="Go to where ${escapeText(r.name || r.url)} was last published">↳</button></div>`).join("")}</div>`;
  }
  show(item, text, data) {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    // One object URL at a time: a tab reopened per session switch minted a new blob and never
    // released the last, so the page held every image it had ever previewed.
    if (this.objectUrl) { URL.revokeObjectURL(this.objectUrl); this.objectUrl = ""; }
    if (data) { this.objectUrl = data.startsWith("blob:") ? data : ""; body.innerHTML = `<div class="artifact-surface"><img class="artifact-image" alt="${escapeText(item.name)}"></div>`; body.querySelector("img").src = data; return; }
    const html = /\.html?$/i.test(item.name || "");
    if (html && document.body.dataset.paired === "true") { body.innerHTML = '<iframe class="artifact-html-frame" sandbox="allow-scripts" referrerpolicy="no-referrer"></iframe>'; body.querySelector("iframe").srcdoc = sandboxDocument(text || ""); return; }
    body.innerHTML = `<div class="artifact-toolbar"><div class="artifact-location"><span>${escapeText(item.path || item.name)}</span></div>${item.path && item.sig ? '<div class="artifact-actions"><button class="smallbtn" type="button" data-preview-reveal>Reveal in file manager</button></div>' : ""}</div><div class="artifact-surface"><pre class="artifact-text"></pre></div>`;
    body.querySelector("pre").textContent = text || "";
    const reveal = body.querySelector("[data-preview-reveal]");
    if (reveal) reveal.onclick = () => this.actions.reveal?.(item);
  }
}

