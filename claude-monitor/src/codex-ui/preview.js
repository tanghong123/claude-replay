import { escapeText } from "./view-model.js";
import { uiState } from "./state.js";
import { sandboxDocument } from "./sandbox.js";
import { createImageView } from "./shared/image-view.js";
import { isMarkdownName, mdrevVersion, mountMarkdown } from "./mdrev-pane.js";
import { canReveal } from "./shared/capabilities.js";
import { svg } from "./icons.js";

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
    // The Markdown document the pane shows, in a tab of its own (#271): production chrome beside
    // the demo's close button, there only while mdrev has a document mounted. NOT `noopener` — the
    // tab `window.open` makes inherits a copy of this page's sessionStorage, which is how held text
    // reaches it (mdrev-pane.js `href`); the tab is this monitor's own page on this origin.
    this.newTab = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-newtab", textContent: "↗", title: "Open in a new tab", hidden: true });
    this.newTab.dataset.previewNewTab = "";
    this.newTab.setAttribute("aria-label", "Open this document in a new tab");
    this.newTab.onclick = () => { const href = this.markdown?.href(); if (href) window.open(href, "_blank"); };
    // The file manager, for whatever the pane shows (#272). The pane is where every "show me the
    // file" click lands, so this one control gives each view — an image, a page, Markdown, text,
    // a download, an error — the other half the owner asked for ("offering both for now"). Only
    // on a click: a reveal is a side effect on the reader's desktop, so an error never fires one.
    this.revealBtn = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-reveal", title: "Reveal in file manager", hidden: true, innerHTML: svg("folder") });
    this.revealBtn.setAttribute("aria-label", "Reveal this file in the file manager");
    this.revealBtn.onclick = () => { if (this.shown) this.actions.reveal?.(this.shown); };
    byId("closePreview").before(this.revealBtn, this.newTab);
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
    this.teardownMarkdown();
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
    this.shown = item || null;
    this.revealBtn.hidden = !(item && canReveal(item));
    // No file tab selected and something was published: the roster is what the pane shows —
    // so it is also what a freshly opened pane lands on, without hunting for a control.
    const roster = !item && this.roster.length > 0;
    const pinned = this.roster.length ? `<button class="preview-tab pinned ${roster ? "on" : ""}" data-preview-tab="${ROSTER_ID}" title="What this session published"><span class="preview-tab-label">Artifacts (${this.roster.length})</span></button>` : "";
    byId("previewTabs").innerHTML = pinned + uiState.previewTabs.map(tab => `<button class="preview-tab ${tab.id === uiState.previewId ? "on" : ""}" data-preview-tab="${escapeText(tab.id)}"><span class="preview-tab-label">${escapeText(tab.name)}</span><span class="preview-tab-close" data-preview-tab-close="${escapeText(tab.id)}">×</span></button>`).join("");
    // A Markdown tab mdrev is already showing stays as it is: the tab strip and the roster re-render
    // around it, and a remount would throw away the reader's place, range and open notes. The tab
    // OBJECT, not its id — an attachment's id is a positional record id two sessions can share.
    if (item && this.markdownItem === item) return;
    this.teardownMarkdown();
    if (roster) { this.showRoster(); return; }
    if (!item) { byId("previewBody").innerHTML = '<div class="preview-empty"><div class="preview-empty-icon">◇</div><strong>No file open</strong><span>Open a file, image or HTML page from the transcript.</span></div>'; return; }
    if (!item.data && isMarkdownName(item.name) && mdrevVersion()) { this.showMarkdown(item, generation); return; }
    this.showPlain(item, generation);
  }
  /** What the pane showed before mdrev (#270), and still shows for everything that is not
   *  Markdown — and for Markdown when there is no mdrev, or it cannot mount this document. */
  showPlain(item, generation) {
    if (item.text != null || item.data) { this.show(item, item.text, item.data); return; }
    byId("previewBody").classList.add("production-loading"); byId("previewBody").textContent = "Reading securely…";
    const query = `path=${encodeURIComponent(item.path)}&sig=${encodeURIComponent(item.fsig || "")}`;
    fetch(`/file?${query}`, { cache: "no-store" }).then(response => {
      if (response.status === 401) throw new Error("Reading local files requires pairing — run `agent-monitor --pair`.");
      if (!response.ok) throw new Error(`HTTP ${response.status} · The original path may be gone, or the file is outside what this monitor may read.`);
      const type = response.headers.get("content-type") || "";
      if (type.startsWith("image/")) return response.blob().then(blob => { if (generation === this.renderGeneration) this.show(item, null, URL.createObjectURL(blob)); });
      // Bytes the page does not show come as a download (`/file`: octet-stream, `Content-Disposition:
      // attachment`), and the pane offers exactly that — never the bytes read as text (#272).
      if (/attachment/i.test(response.headers.get("content-disposition") || "")) {
        response.body?.cancel();
        if (generation === this.renderGeneration) this.showDownload(item, Number(response.headers.get("content-length")) || 0);
        return;
      }
      return response.text().then(text => { if (generation === this.renderGeneration) this.show(item, text, null); });
    }).catch(error => {
      if (generation !== this.renderGeneration) return;
      const body = byId("previewBody"); body.classList.remove("production-loading");
      body.innerHTML = `<div class="preview-error"><strong>Cannot preview this file</strong><span>${escapeText(error.message)}</span><div class="preview-error-actions">${canReveal(item) ? '<button class="smallbtn" data-preview-reveal>Reveal in file manager</button>' : ""}<button class="smallbtn" data-copy-path>Copy original path</button><button class="smallbtn" data-close-preview>Close tab</button></div></div>`;
      const reveal = body.querySelector("[data-preview-reveal]");
      if (reveal) reveal.onclick = () => this.actions.reveal?.(item);
      body.querySelector("[data-copy-path]").onclick = () => {
        const operation = navigator.clipboard?.writeText(item.path || "");
        if (operation) operation.then(() => this.actions.toast?.("Copied the original path"));
        else this.actions.toast?.("This browser does not support copying");
      };
      body.querySelector("[data-close-preview]").onclick = () => this.closeTab(item.id);
    });
  }
  /** A file the page does not show: a download, with the file's place above it — and the head's
   *  reveal beside it (#272). */
  showDownload(item, size) {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    const bytes = size >= 1048576 ? `${(size / 1048576).toFixed(1)} MB` : size >= 1024 ? `${Math.round(size / 1024)} KB` : size ? `${size} bytes` : "";
    body.innerHTML = `<div class="artifact-toolbar"><div class="artifact-location"><span>${escapeText(item.path || item.name)}</span></div></div><div class="preview-error preview-download"><strong>No preview for this file</strong><span>${escapeText(item.name)}${bytes ? ` · ${bytes}` : ""} — this pane shows text and images; this file downloads.</span><div class="preview-error-actions"><button class="smallbtn primary" data-preview-download>Download</button></div></div>`;
    body.querySelector("[data-preview-download]").onclick = () => this.actions.download?.(item);
  }
  /** Markdown through mdrev's viewer (#270): a reader for text the transcript carries, the whole
   *  viewer for a file on disk. Any failure — no bundle, a refused route, a mount that throws —
   *  falls back to `showPlain`, so mdrev can only ever add to what the pane showed. */
  showMarkdown(item, generation) {
    const body = byId("previewBody");
    if (this.objectUrl) { URL.revokeObjectURL(this.objectUrl); this.objectUrl = ""; }
    if (this.imageView) { this.imageView.destroy(); this.imageView = null; }
    body.classList.remove("production-loading");
    body.classList.add("mdrev-mounted");
    body.innerHTML = '<div class="mdrev-pane"></div>';
    this.markdownItem = item;
    const token = {}; this.markdownToken = token;
    mountMarkdown(body.firstElementChild, item).then(handle => {
      if (this.markdownToken !== token) { handle?.unmount(); return; }
      if (!handle) throw new Error("no mdrev");
      this.markdown = handle;
      this.newTab.hidden = false;
    }).catch(() => {
      if (this.markdownToken !== token) return;
      this.teardownMarkdown();
      if (generation === this.renderGeneration) this.showPlain(item, generation);
    });
  }
  teardownMarkdown() {
    this.markdown?.unmount(); this.markdown = null;
    this.newTab.hidden = true;
    this.markdownItem = null; this.markdownToken = null;
    byId("previewBody").classList.remove("mdrev-mounted");
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
    // A previewed image zooms and pans like the enlarged one (#228): this panel is narrow, so it
    // is exactly where a screenshot is "too big to fit". Same shared engine, same gestures.
    if (this.imageView) { this.imageView.destroy(); this.imageView = null; }
    if (data) {
      this.objectUrl = data.startsWith("blob:") ? data : "";
      body.innerHTML = `<div class="artifact-surface artifact-stage"><img class="artifact-image" alt="${escapeText(item.name)}"></div>`;
      const stage = body.querySelector(".artifact-stage"), img = stage.querySelector("img");
      // The preview's zoom keys belong to it only while the pane is open (#268) — the pane
      // can be collapsed with an image still mounted, and a collapsed pane must not own
      // `0`/`-`/`+`… against the compose box beside it.
      this.imageView = createImageView(stage, img, { isActive: () => uiState.preview });
      img.src = data;
      return;
    }
    const html = /\.html?$/i.test(item.name || "");
    if (html && document.body.dataset.paired === "true") { body.innerHTML = '<iframe class="artifact-html-frame" sandbox="allow-scripts" referrerpolicy="no-referrer"></iframe>'; body.querySelector("iframe").srcdoc = sandboxDocument(text || ""); return; }
    // The file's place above its text; revealing it is the head's control (#272), one for every view.
    body.innerHTML = `<div class="artifact-toolbar"><div class="artifact-location"><span>${escapeText(item.path || item.name)}</span></div></div><div class="artifact-surface"><pre class="artifact-text"></pre></div>`;
    body.querySelector("pre").textContent = text || "";
  }
}

