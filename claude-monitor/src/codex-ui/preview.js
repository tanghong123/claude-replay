import { escapeText } from "./view-model.js";
import { backend } from "./backend.js";
import { sandboxDocument } from "./sandbox.js";
import { createImageView } from "./shared/image-view.js";
import { isMarkdownName, mdrevVersion, mountMarkdown } from "./mdrev-pane.js";
import { besideAction, RASTER_FILE } from "./shared/capabilities.js";
import { handOver } from "./attachment-viewer.js";
import { svg } from "./icons.js";
import { page, viewRoot } from "./view-root.js";

const byId = viewRoot.byId;

/** An image in a tab of its own (#s11), from the pane: the detached page (`/image` by default), reading a
 *  FILE by the path and stamp it was offered under (`/file` serves it there, under the same rules —
 *  a pasted image's saved original too, #324, larger than the copy the transcript carries), with
 *  the bytes the transcript EMBEDS handed over through sessionStorage — which `window.open` copies
 *  into the tab it makes — as the whole image where there is no file and the fallback where the
 *  file has gone since. "" when there is nothing to hand over. */
export const imageTabbable = (item, shown = "") => !!item && !!((item.path && item.fsig) || item.data || String(shown).startsWith("data:"));

/** What a tab's address carries for an image (#s433: the query alone; the page it goes after is the
 *  host's to choose, the view's `detachTo`). "" when there is nothing to hand over. */
export function imageTabQuery(item, shown = "") {
  if (!imageTabbable(item, shown)) return "";
  const query = [`name=${encodeURIComponent(item.name || "image")}`];
  if (item.path && item.fsig) query.push(`path=${encodeURIComponent(item.path)}`, `sig=${encodeURIComponent(item.fsig)}`);
  const bytes = item.data || (String(shown).startsWith("data:") ? shown : "");
  if (bytes) {
    const key = `am-image:${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
    try { sessionStorage.setItem(key, bytes); query.push(`held=${encodeURIComponent(key)}`); } catch (_) { /* too large for sessionStorage: the file, if any, still opens */ }
  }
  return query.length > 1 ? query.join("&") : "";
}
const SESSION_CACHE_LIMIT = 6;
const SESSION_TAB_LIMIT = 6;
const SESSION_CACHE_BYTES = 12 * 1024 * 1024;
// The pinned first tab: the session's published artifacts (#95), not a file the user opened.
const ROSTER_ID = "__artifacts";
const tabWeight = tab => 256 + 2 * String(tab.text || "").length + 2 * String(tab.data || "").length;
/** A reason the pane cannot show a file, with the heading that says which kind (#s7). */
const refusal = (title, message) => Object.assign(new Error(message), { title });

// #s627: a field that takes an on-screen keyboard (the review sheet holds no frames).
const NO_KEYBOARD_INPUTS = new Set(["button", "checkbox", "radio", "range", "color", "file", "image", "reset", "submit", "hidden"]);
const takesKeyboard = at => at?.localName === "input" ? !NO_KEYBOARD_INPUTS.has(at.type)
  : at?.localName === "textarea" || at?.localName === "select" || !!at?.isContentEditable;
/** An element's place in the page's layout: its offsets up to the box that places it, which a
 *  transform and a scroll of the page leave alone, and a host moving it does not. */
function layoutTop(element) {
  let top = 0;
  for (let at = element; at; at = at.offsetParent) top += at.offsetTop;
  return top;
}
/** #s627: the review sheet ends above an on-screen keyboard while a field inside it has the focus.
 *  iOS covers the foot of what is drawn with its keyboard and never shortens a fixed layer for it,
 *  and it lifts a focused field by scrolling the page, which the sheet (fixed over the whole app, its
 *  document scrolling inside it) does not answer. So a note box mdrev opens beside a line low on the
 *  screen took the focus under the keyboard and stayed there, its Save past the window's foot
 *  (measured on the iPad Simulator, iPadOS 26.5: the box at 1279pt, the keyboard from 1028pt).
 *  The keyboard is read as the session view's keyboard ride reads it: the layout viewport's height
 *  less the visual viewport's, covering the window from its foot, so the visible part runs from the
 *  window's top for the visual viewport's height. Never from `visualViewport.offsetTop`: with the
 *  keyboard up iOS 26 reports it equal to the page's own offset (338 with the sheet drawn at the
 *  screen's top, measured on the iPhone 17 Pro Simulator), and on a phone the ride moves the app (or
 *  a host's box) back over that offset, so what is visible starts at the top as drawn. The sheet's
 *  foot is raised to the visible part's foot, measured from the top of the box it is placed in (the
 *  window, the view in a host's pane, or what the ride's transform makes its box), and the scroller
 *  holding the field brings into view the field with what belongs to it:
 *  its largest ancestor under the scroller that fits in what stays visible (mdrev's note card: the
 *  quote, the field, the share box and Save), or the field's top where nothing does. Without a field
 *  the sheet is as its rules draw it. Returns the height it fitted the sheet to, as a string ("" for
 *  none), which the caller keys its reveal on, so a reader who scrolled the sheet while typing is left
 *  alone. A sheet that would keep under 120px (an iPhone held sideways in Safari,
 *  its bars and the keyboard leaving about 16px) is not fitted: its head alone would not fit there.
 *  A pinch zoom is not a keyboard: while the scale is off 1 the sheet is as its rules draw it, as the
 *  ride leaves the pan to the reader. Raised, the sheet drops its foot's safe-area padding, which
 *  only the screen's edge needs. */
function fitSheetAboveKeyboard(sheet, field, viewport, applied, typing) {
  // The sheet as its rules draw it, from the box drawn now plus what was applied, so the sheet never
  // grows back for a measure (a scroller grown for a moment would clamp the reader's place), and its
  // inset from its containing block's top as the rules set it. Its height and insets alone, never its
  // place on the screen, which iOS's scroll and the ride move.
  const box = sheet.getBoundingClientRect(), style = getComputedStyle(sheet);
  const drawn = box.height + applied.bottom;
  const ruledTop = parseFloat(style.top) || 0, ruledBottom = (parseFloat(style.bottom) || 0) - applied.bottom;
  // The top of the box the sheet is placed in, below the visible part's top (#s627's reviews). The
  // browser is asked, not told: the sheet's own top less what its rules put between it and that box
  // is the box's top, whatever made the box (the view containing its layers in a host's pane, the
  // ride's transform, a host's own transform or filter, or nothing: the window), in any engine. The
  // view's top is read where it rests as the reader starts typing (the first fit of it, before iOS
  // lifts anything), and not again until a fit finds no field focused, so a pinch or a change of the
  // view's family mid-typing reads no rest mid-lift. Where the ride holds the view (a phone reader,
  // `pl-phone`, on a touch screen, `pointer:coarse`: where `ride` in session-view.js holds it; it lets
  // go during a pinch and holds again after), the view is taken to stay at that rest as the
  // reader sees it, whichever way iOS's lift moves the page (a scroll, or the visible part panned, its
  // `offsetTop` the page's offset): the box is placed against that rest by its offset from the view,
  // which neither moves. Only the view's place in the page's layout is read again, which neither
  // iOS's lift nor the ride's transform changes and a host moving its pane does (a sticky pane's
  // offset is read as a move: a limit). Elsewhere nothing holds the view, and what is drawn is where
  // it is seen: the box is read where it is at every fit. In the app the box starts at the window's
  // top, as walked on the iPhone and iPad Simulators. What the keyboard covers is the ride's own
  // measure, the layout viewport's height less the visible one's: nothing on a desktop, so a sheet
  // taller than the window is left alone. No fit is made at a scale other than 1.
  const holder = sheet.parentElement, holderTop = holder ? holder.getBoundingClientRect().top : 0;
  const laidAt = holder ? layoutTop(holder) : 0;
  const typed = typing && applied.typing;
  if (!typed) { applied.rest = holderTop; applied.laidAt = laidAt; }
  applied.typing = typing;
  const held = typed && !!holder?.classList.contains("pl-phone")
    && typeof matchMedia === "function" && matchMedia("(pointer:coarse)").matches;
  const placedAt = box.top - ruledTop - (parseFloat(style.marginTop) || 0);
  const blockTop = placedAt + (held ? applied.rest + laidAt - applied.laidAt - holderTop : 0);
  const covered = viewport ? (viewRoot.styleRoot()?.clientHeight || 0) - viewport.height : 0;
  let want = 0;
  if (field && covered > 0.5 && Math.abs((viewport.scale || 1) - 1) < 0.01) {
    // The sheet's foot at the visible part's foot: the block's top, the sheet's own top inset and its
    // height, less the visible height.
    want = Math.max(0, Math.round(blockTop + ruledTop + drawn - viewport.height));
    if (drawn - want < 120) want = 0;
  }
  if (want !== applied.bottom) {
    if (want > 0) {
      sheet.style.bottom = `${ruledBottom + want}px`;
      sheet.style.setProperty("padding-bottom", "0px");
    } else {
      sheet.style.removeProperty("bottom");
      sheet.style.removeProperty("padding-bottom");
    }
    applied.bottom = want;
  }
  return want ? String(Math.round(drawn - want)) : "";
}
function revealInSheet(sheet, field) {
  let scroller = field.parentElement;
  while (scroller && scroller !== sheet) {
    const overflow = getComputedStyle(scroller).overflowY;
    if ((overflow === "auto" || overflow === "scroll") && scroller.scrollHeight > scroller.clientHeight) break;
    scroller = scroller.parentElement;
  }
  if (!scroller || scroller === sheet) return;
  const view = scroller.getBoundingClientRect(), shown = sheet.getBoundingClientRect();
  const bandTop = Math.max(view.top, shown.top), bandBottom = Math.min(view.bottom, shown.bottom);
  if (bandBottom - bandTop < 40) return;
  const margin = 8, own = field.getBoundingClientRect();
  let group = field;
  for (let at = field.parentElement; at && at !== scroller; at = at.parentElement) {
    const box = at.getBoundingClientRect();
    // Taller than what stays visible, or not around the field (children placed absolutely).
    if (box.height > bandBottom - bandTop - 2 * margin) break;
    if (box.top > own.top + 1 || box.bottom < own.bottom - 1) break;
    group = at;
  }
  const rect = group.getBoundingClientRect();
  if (rect.bottom > bandBottom - margin) scroller.scrollTop += Math.min(rect.bottom - bandBottom + margin, rect.top - bandTop - margin);
  else if (rect.top < bandTop + margin) scroller.scrollTop -= bandTop + margin - rect.top;
}

export class Preview {
  /** `uiState` is the view's own store (view-state.js), whose `preview`, `previewTabs` and `previewId`
   *  are the pane's: open or shut, its tabs and the one shown. */
  constructor(actions, uiState) { this.actions = actions; this.uiState = uiState; this.sessionId = ""; this.sessionTabs = new Map(); this.renderGeneration = 0; this.objectUrl = ""; this.roster = []; this.rosterKey = ""; this.bind(); this.setOpen(false); this.restoreWidth(); }
  bind() {
    byId("previewBtn").onclick = () => this.setOpen(!this.uiState.preview);
    byId("closePreview").onclick = () => this.setOpen(false);
    // The Markdown document the pane shows, in a tab of its own (#271): production chrome beside
    // the demo's close button, there only while mdrev has a document mounted. NOT `noopener` — the
    // tab `window.open` makes inherits a copy of this page's sessionStorage, which is how held text
    // reaches it (mdrev-pane.js `query`); the tab is the host's page on this origin, at the address
    // the view's `detachTo` names (#s433), the monitor's `/markdown` and `/image` by default.
    this.newTab = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-newtab", textContent: "↗", title: "Open in a new tab", hidden: true });
    this.newTab.dataset.previewNewTab = "";
    this.newTab.setAttribute("aria-label", "Open this document in a new tab");
    // #s11: an image the pane shows goes to a tab of its own too.
    this.newTab.onclick = () => {
      if (this.markdown) { this.actions.openTab("markdown", this.markdown.query()); return; }
      const query = this.imageQuery();
      if (query) this.actions.openTab("image", query);
    };
    // The file manager, for whatever the pane shows (#272). The pane is where every "show me the
    // file" click lands, so this one control gives each view — an image, a page, Markdown, text,
    // a download, an error — the other half the owner asked for ("offering both for now"). Only
    // on a click: a reveal is a side effect on the reader's desktop, so an error never fires one.
    this.revealBtn = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-reveal", title: "Reveal in file manager", hidden: true, innerHTML: svg("folder") });
    this.revealBtn.setAttribute("aria-label", "Reveal this file in the file manager");
    this.revealBtn.onclick = () => { if (this.shown) this.actions.reveal?.(this.shown); };
    // #s29: for a reader elsewhere, the control beside the file is the file itself, as a download.
    this.downloadBtn = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-download", title: "Download this file", hidden: true, textContent: "↓" });
    this.downloadBtn.setAttribute("aria-label", "Download this file");
    this.downloadBtn.onclick = () => { if (this.shown) this.actions.download?.(this.shown); };
    // #s12, the owner: shared review on a phone, where a tab of its own has no way back (#335) — a
    // full-screen REVIEW SHEET over the app, opened from here, closed back to exactly where the reader
    // was. Only for Markdown from a FILE (held text has no store), and only on a phone: the desktop
    // has its detached tab (#s10). The pane itself stays review-free.
    this.reviewBtn = Object.assign(document.createElement("button"), { type: "button", className: "smallbtn preview-review", textContent: "Review", title: "Review with the other reviewers", hidden: true });
    this.reviewBtn.setAttribute("aria-label", "Review this document with the other reviewers");
    this.reviewBtn.onclick = () => this.openReviewSheet(this.markdownItem);
    // #s30, the owner: "for markdown, in mdrev, I actually think print still makes sense". mdrev's
    // own toolbar prints (on a phone from its Aa menu, since 1.1.24); a held reader has no toolbar
    // — there the pane offers mdrev's print itself.
    this.printBtn = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-print", title: "Print this document", hidden: true });
    this.printBtn.innerHTML = `<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M7 9V3h10v6"/><rect x="3" y="9" width="18" height="8" rx="2"/><path d="M7 14h10v7H7z"/></svg>`;
    this.printBtn.setAttribute("aria-label", "Print this document");
    this.printBtn.onclick = () => this.markdown?.print?.();
    // #s49, the owner: held sideways the pane is a second column (production.css), and this takes it
    // to the whole screen and back. Only a phone held sideways shows it; elsewhere the pane is a
    // column with a resizer (a desktop) or the whole screen already (a phone upright).
    this.expandBtn = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-expand" });
    this.expandBtn.onclick = () => this.setFull(!viewRoot.element()?.classList.contains("preview-full"));
    this.setFull(false);
    // #s663, Hong's choice on #s670: held sideways the pane is a column about 450 points wide, and its
    // head held the steps, the strip, the file's actions, full screen and close, each a finger's 44
    // points (#s649), which left the strip 138 points and cut every file's name. There the file's
    // actions and full screen fold into one More menu (production.css shows it on a phone held
    // sideways only, and takes them out of the head there); the steps, the strip and close stay, and
    // the current tab may take the whole strip. The menu lists, as it opens, the controls the head
    // would show, each in its own words, and a choice presses that control: what each does, and when
    // it is offered, stays where it was.
    this.listening = new AbortController();
    this.moreWrap = Object.assign(document.createElement("span"), { className: "preview-more-wrap" });
    this.moreBtn = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-more", title: "More" });
    this.moreBtn.innerHTML = '<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" fill="currentColor"><circle cx="5" cy="12" r="1.9"/><circle cx="12" cy="12" r="1.9"/><circle cx="19" cy="12" r="1.9"/></svg>';
    this.moreBtn.setAttribute("aria-label", "More for this file");
    this.moreBtn.setAttribute("aria-haspopup", "menu");
    this.moreBtn.setAttribute("aria-expanded", "false");
    this.moreBtn.onclick = event => { event.stopPropagation(); this.setMore(this.moreMenu.hidden); };
    this.moreMenu = Object.assign(document.createElement("div"), { className: "preview-more-menu", hidden: true });
    this.moreMenu.setAttribute("role", "menu");
    this.moreMenu.setAttribute("aria-label", "More for this file");
    this.moreWrap.append(this.moreBtn, this.moreMenu);
    // A press anywhere else closes it (the tap still does what it does there), as the session's
    // copy menu closes; the view's Escape closes it too (session-view.js).
    document.addEventListener("pointerdown", event => {
      if (!this.moreMenu.hidden && !this.moreWrap.contains(event.target)) this.setMore(false);
    }, { capture: true, signal: this.listening.signal });
    // A press inside an HTML page the pane shows reaches its sandboxed frame and not this page; the
    // frame takes the focus, so the window's blur says where it went (as the search's hold hears it).
    addEventListener("blur", () => { if (!this.moreMenu.hidden && viewRoot.active()?.tagName === "IFRAME") this.setMore(false); }, { signal: this.listening.signal });
    // A host that switched the ↗ off (`show: { detach: false }`, stage 6) gets none in the head:
    // the show paths still flip its `hidden`, on a button nobody can reach.
    byId("closePreview").before(this.revealBtn, this.downloadBtn, this.printBtn, ...(this.actions.detach === false ? [] : [this.newTab]), this.reviewBtn, this.expandBtn, this.moreWrap);
    // #337, the owner: with many files open the strip squeezed every tab to a few letters and the
    // CURRENT one to nothing. The tabs now keep their width and the strip scrolls; while it
    // overflows, ‹ and › beside it step to the previous and next tab (the pinned roster first),
    // wrapping, and the strip brings that tab into view by its own scroller.
    const step = (dir, label, glyph) => {
      const button = Object.assign(document.createElement("button"), { type: "button", className: "iconbtn preview-tab-step", textContent: glyph, title: label, hidden: true });
      button.setAttribute("aria-label", label);
      button.dataset.previewTabStep = String(dir);
      button.onclick = event => { event.stopPropagation(); this.stepTab(dir); };
      return button;
    };
    this.prevTabBtn = step(-1, "Previous file", "‹");
    this.nextTabBtn = step(1, "Next file", "›");
    byId("previewTabs").before(this.prevTabBtn);
    byId("previewTabs").after(this.nextTabBtn);
    this.tabsObserver = new ResizeObserver(() => this.fitTabs());
    this.tabsObserver.observe(byId("previewTabs"));
    byId("previewHead").onclick = event => {
      const close = event.target.closest("[data-preview-tab-close]");
      if (close) { this.closeTab(close.dataset.previewTabClose); return; }
      const tab = event.target.closest("[data-preview-tab]");
      if (tab) { this.uiState.previewId = tab.dataset.previewTab; this.render(); }
    };
    byId("previewBody").addEventListener("click", event => {
      const jump = event.target.closest("[data-artifact-record]");
      if (jump) this.actions.jumpToRecord?.(Number(jump.dataset.artifactRecord));
    });
    const resizer = byId("resizer");
    resizer.setAttribute("role", "separator"); resizer.setAttribute("aria-orientation", "vertical"); resizer.tabIndex = 0;
    resizer.onpointerdown = event => {
      resizer.setPointerCapture(event.pointerId); resizer.classList.add("dragging"); this.paneState("resizing", true);
      // #s431 stage 3: the drag sets how the slack splits between the session and the pane, each kept
      // at least at M (the view's `dragPane`, which remembers it when the drag ends); a double-click
      // splits it equally again.
      resizer.onpointermove = move => this.actions.dragPane?.(move.clientX);
      resizer.onpointerup = () => { resizer.classList.remove("dragging"); this.paneState("resizing", false); resizer.onpointermove = null; this.actions.dragPane?.(null); };
    };
    resizer.ondblclick = () => this.actions.dragPane?.("equal");
  }
  /** The view is unmounted (#s429 stage 7): the review sheet closes (its key listener on the window
   *  goes with it), mdrev's viewer unmounts, the image viewer lets go of its keys, the observers stop,
   *  a render still reading a file drops what it reads, and the image's object URL is released. */
  destroy() {
    this.reviewSheet?.close?.();
    this.teardownMarkdown();
    this.imageView?.destroy(); this.imageView = null;
    this.listening.abort();
    this.stageObserver?.disconnect(); this.stageObserver = null;
    this.tabsObserver?.disconnect();
    this.renderGeneration++;
    if (this.objectUrl) { URL.revokeObjectURL(this.objectUrl); this.objectUrl = ""; }
  }
  /** The pane's open, full and resizing states: the view's own, whose grid follows them (#s431
   *  stage 1), and its host's (`paneLayout`), whose rules may read them too (the app's do). */
  // The host hears first (#s431 stage 3): the app may lay its list out again for the pane, and it lays
  // the page out as it does, so the view must still wear its old state then. The pane's own state is
  // changed inside the view's decision (`paneToggled`), after the page is read, so no layout styles
  // the view between the change and the arrangement it brings: either would start the pane's
  // transitions in the mode the view is about to leave.
  paneState(name, on) {
    this.actions.paneLayout?.(name, on);
    const view = viewRoot.element();
    const change = () => view?.classList.toggle(name, on);
    // Only a real change glides: the pane shutting itself as it is built does not open a glide's time.
    if (name === "preview-off" && view && view.classList.contains(name) !== on && this.actions.paneToggled) this.actions.paneToggled(change);
    else change();
  }
  setOpen(open) { this.uiState.preview = open; this.paneState("preview-off", !open); byId("previewBtn").classList.toggle("active", open); if (!open) { this.setFull(false); this.setMore(false); } if (open) this.render(); this.actions.layoutChanged?.(); }
  /** The head's More menu (#s663; a phone held sideways): opening lists the head's file controls
   *  offered now — reveal or download, print, review, then full screen — each by its own title, with
   *  its glyph where it has one. A choice closes the menu and presses its control, if that control is
   *  still offered. */
  setMore(open) {
    if (!this.moreMenu) return;
    if (open) {
      const sources = [["reveal", this.revealBtn], ["download", this.downloadBtn], ["print", this.printBtn], ["review", this.reviewBtn], ["expand", this.expandBtn]];
      this.moreMenu.replaceChildren(...sources.filter(([, source]) => !source.hidden).map(([name, source]) => {
        const item = Object.assign(document.createElement("button"), { type: "button", className: "preview-more-item" });
        item.setAttribute("role", "menuitem");
        item.dataset.previewMore = name;
        const glyph = Object.assign(document.createElement("span"), { className: "preview-more-glyph" });
        glyph.setAttribute("aria-hidden", "true");
        if (source.classList.contains("iconbtn")) glyph.innerHTML = source.innerHTML;
        item.append(glyph, Object.assign(document.createElement("span"), { className: "preview-more-label", textContent: source.title }));
        item.onclick = event => { event.stopPropagation(); this.setMore(false); if (!source.hidden) source.click(); };
        return item;
      }));
    }
    this.moreMenu.hidden = !open;
    this.moreBtn.setAttribute("aria-expanded", String(open));
  }
  setFull(full) {
    this.paneState("preview-full", full);
    this.expandBtn.innerHTML = full
      ? '<svg viewBox="0 0 24 24" width="17" height="17" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 14h6v6M20 10h-6V4M14 10l7-7M3 21l7-7"/></svg>'
      : '<svg viewBox="0 0 24 24" width="17" height="17" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>';
    const words = full ? "Back to two columns" : "Take the whole screen";
    this.expandBtn.title = words;
    this.expandBtn.setAttribute("aria-label", words);
    this.expandBtn.setAttribute("aria-pressed", String(full));
  }
  setWidth(value) { const width = Math.max(340, Math.min(680, Number(value) || 420)); (this.actions.rootProperty || ((name, value) => viewRoot.styleRoot().style.setProperty(name, value)))("--preview", `${width}px`); }
  /** The pane's width where the view's layout writes none (#s431 stage 3: a column's comes from the
   *  owner's order, `--preview-fit`): the default, since the width remembered before stage 3
   *  (`am-demo-preview`) stays in storage unread. */
  restoreWidth() { this.setWidth(420); }
  setSession(sessionId) {
    if (sessionId === this.sessionId) return;
    if (this.sessionId) {
      const tabs = this.uiState.previewTabs.slice(-SESSION_TAB_LIMIT);
      this.sessionTabs.delete(this.sessionId);
      this.sessionTabs.set(this.sessionId, { tabs, active: this.uiState.previewId, bytes: tabs.reduce((total, tab) => total + tabWeight(tab), 0) });
      const cacheBytes = () => [...this.sessionTabs.values()].reduce((total, entry) => total + entry.bytes, 0);
      while (this.sessionTabs.size > SESSION_CACHE_LIMIT || cacheBytes() > SESSION_CACHE_BYTES) this.sessionTabs.delete(this.sessionTabs.keys().next().value);
    }
    this.roster = []; this.rosterKey = "";
    // The review sheet is the old session's document: it closes with it (#s431 stage 2), whatever
    // switched the session under it.
    this.reviewSheet?.close?.();
    this.teardownMarkdown();
    this.sessionId = sessionId || "";
    const saved = this.sessionTabs.get(this.sessionId);
    this.uiState.previewTabs = saved?.tabs.slice() || [];
    this.uiState.previewId = saved?.active && this.uiState.previewTabs.some(tab => tab.id === saved.active) ? saved.active : this.uiState.previewTabs.at(-1)?.id || null;
    this.paneBadge();
    this.renderGeneration++;
    if (this.uiState.preview) this.render();
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
    this.paneBadge();
    if (this.uiState.preview) this.render();
  }
  /** The pane button's badge (#336): how many DOCUMENTS are open in the pane — its tabs, not the
   *  pinned roster, which counts itself ("Artifacts (N)"). It used to count the published
   *  artifacts (#95) and read "1" over a pane holding five files, which the owner found
   *  misleading. The roster still names itself in the button's title. */
  paneBadge() {
    const button = byId("previewBtn");
    const badge = button.querySelector(".preview-badge");
    const open = this.uiState.previewTabs.length, published = this.roster.length;
    if (!open) badge?.remove();
    else (badge || button.appendChild(Object.assign(document.createElement("span"), { className: "preview-badge" }))).textContent = String(open);
    const parts = [open ? `${open} open document${open === 1 ? "" : "s"}` : "", published ? `${published} published artifact${published === 1 ? "" : "s"}` : ""].filter(Boolean);
    button.title = parts.length ? `Open the right panel — ${parts.join(", ")}` : "Open the right panel";
  }
  /** A file to show: a tab of its own, selected, and the pane open. The host hears of it (`opened`,
   *  the view's `onFile`), as it does for the first of an Open all, the one the pane shows. */
  open(item) {
    if (!this.uiState.previewTabs.some(tab => tab.id === item.id)) this.uiState.previewTabs.push(item);
    if (this.uiState.previewTabs.length > SESSION_TAB_LIMIT) this.uiState.previewTabs.splice(0, this.uiState.previewTabs.length - SESSION_TAB_LIMIT);
    this.uiState.previewId = item.id; this.paneBadge(); this.setOpen(true);
    this.actions.opened?.(item);
  }
  /** #s222: Open all — every item a tab, in the block's order, the FIRST selected so the reader
   *  steps through them by the tabs. A tab already open moves to its place in that order (the same
   *  tab object, so a mounted viewer keeps its identity, taking the batch's label) rather than
   *  staying where it was, which would make stepping leave the block's order. The tab cap gives way to the batch: an Open all
   *  of nine keeps all nine (older tabs go first, never one it just opened). */
  openAll(items) {
    if (!items.length) return;
    const opened = new Set(items.map(item => item.id));
    const open = new Map(this.uiState.previewTabs.filter(tab => opened.has(tab.id)).map(tab => [tab.id, tab]));
    this.uiState.previewTabs = this.uiState.previewTabs.filter(tab => !opened.has(tab.id)).concat(items.map(item => (open.has(item.id) ? Object.assign(open.get(item.id), { name: item.name }) : item)));
    const cap = Math.max(SESSION_TAB_LIMIT, items.length);
    while (this.uiState.previewTabs.length > cap) {
      const at = this.uiState.previewTabs.findIndex(tab => !opened.has(tab.id));
      if (at < 0) break;
      this.uiState.previewTabs.splice(at, 1);
    }
    this.uiState.previewId = items[0].id; this.paneBadge(); this.setOpen(true);
    this.actions.opened?.(items[0]);
  }
  /** #337: the previous or next tab — the pinned roster first, then the files — wrapping. */
  stepTab(dir) {
    const ids = [...(this.roster.length ? [ROSTER_ID] : []), ...this.uiState.previewTabs.map(tab => tab.id)];
    if (ids.length < 2) return;
    const current = this.uiState.previewTabs.some(tab => tab.id === this.uiState.previewId) ? this.uiState.previewId : ROSTER_ID;
    const at = Math.max(0, ids.indexOf(current));
    this.uiState.previewId = ids[(at + dir + ids.length) % ids.length];
    this.render();
  }
  /** #337: the step buttons while the strip overflows, and the current tab in view — moved by the
   *  strip's OWN scroller, never by asking the browser to scroll something into view. */
  fitTabs() {
    const strip = byId("previewTabs");
    if (!strip || !this.prevTabBtn) return;
    // The strip's width changes as the phone turns: a More menu left open where the head no longer
    // draws it (upright) closes, rather than coming back open when the phone is turned again.
    if (this.moreMenu && !this.moreMenu.hidden && !this.moreWrap.getClientRects().length) this.setMore(false);
    const over = strip.scrollWidth > strip.clientWidth + 1;
    this.prevTabBtn.hidden = this.nextTabBtn.hidden = !over;
    const on = strip.querySelector(".preview-tab.on");
    if (!on || !over) return;
    const box = strip.getBoundingClientRect(), tab = on.getBoundingClientRect();
    if (tab.left < box.left) strip.scrollLeft += tab.left - box.left - 8;
    else if (tab.right > box.right) strip.scrollLeft += tab.right - box.right + 8;
  }
  closeTab(id) { this.uiState.previewTabs = this.uiState.previewTabs.filter(tab => tab.id !== id); if (this.uiState.previewId === id) this.uiState.previewId = this.uiState.previewTabs.at(-1)?.id || null; this.paneBadge(); this.render(); }
  render() {
    const generation = ++this.renderGeneration;
    const item = this.uiState.previewTabs.find(tab => tab.id === this.uiState.previewId);
    this.shown = item || null;
    const beside = item ? besideAction(item, backend().local) : null;
    this.revealBtn.hidden = beside !== "reveal";
    this.downloadBtn.hidden = beside !== "download";
    // #s730: where the tap opens the share sheet, the control says so (its More row too, which copies
    // the glyph and the title).
    if (beside === "download") {
      const { verb, glyph } = handOver();
      if (this.downloadBtn.title !== `${verb} this file`) {
        this.downloadBtn.innerHTML = glyph;
        this.downloadBtn.title = `${verb} this file`;
        this.downloadBtn.setAttribute("aria-label", `${verb} this file`);
      }
    }
    // No file tab selected and something was published: the roster is what the pane shows —
    // so it is also what a freshly opened pane lands on, without hunting for a control.
    const roster = !item && this.roster.length > 0;
    const pinned = this.roster.length ? `<button class="preview-tab sv-pinned ${roster ? "on" : ""}" data-preview-tab="${ROSTER_ID}" title="What this session published"><span class="preview-tab-label">Artifacts (${this.roster.length})</span></button>` : "";
    byId("previewTabs").innerHTML = pinned + this.uiState.previewTabs.map(tab => `<button class="preview-tab ${tab.id === this.uiState.previewId ? "on" : ""}" data-preview-tab="${escapeText(tab.id)}"><span class="preview-tab-label">${escapeText(tab.name)}</span><span class="preview-tab-close" data-preview-tab-close="${escapeText(tab.id)}">×</span></button>`).join("");
    this.fitTabs();
    // A Markdown tab mdrev is already showing stays as it is: the tab strip and the roster re-render
    // around it, and a remount would throw away the reader's place, range and open notes. The tab
    // OBJECT, not its id — an attachment's id is a positional record id two sessions can share.
    if (item && this.markdownItem === item) return;
    this.teardownMarkdown();
    if (roster) { this.showRoster(); return; }
    if (!item) { byId("previewBody").innerHTML = '<div class="preview-empty"><div class="preview-empty-icon">◇</div><strong>No file open</strong><span>Open a file, image or HTML page from the transcript.</span></div>'; return; }
    if (this.mdrevTakes(item)) { this.showMarkdown(item, generation); return; }
    this.showPlain(item, generation);
  }
  /** #s13: the pane hands mdrev every TEXT file, not only Markdown. mdrev (1.1.21) shows a Markdown
   *  path as a document and any other as source code — highlighted, its lines numbered, no notes —
   *  deciding by the path alone; the host's `open` refuses a file whose bytes are not text, and the
   *  mount's failure falls back to `showPlain`, which draws it as `/file` serves it. Never an HTML
   *  page, which renders as a page (the owner, 2026-10-06), nor an image, which the pane draws. */
  mdrevTakes(item) {
    const name = item.name || "";
    return !!mdrevVersion() && !item.data && !/\.html?$/i.test(name) && !RASTER_FILE.test(name);
  }
  /** What the pane showed before mdrev (#270), and still shows for an image, an HTML page, a file
   *  that is not text — and for any file when there is no mdrev, or it cannot mount this one. */
  showPlain(item, generation) {
    if (item.text != null || item.data) { this.show(item, item.text, item.data); return; }
    byId("previewBody").classList.add("production-loading"); byId("previewBody").textContent = "Reading securely…";
    const query = `path=${encodeURIComponent(item.path)}&sig=${encodeURIComponent(item.fsig || "")}`;
    backend().fetch(`/file?${query}`, { cache: "no-store" }).then(response => {
      if (response.status === 401) throw new Error("Reading local files requires pairing — run `agent-monitor --pair`.");
      // #s7: the route says which — 410 for a file that is gone, 403 for one this monitor may not
      // read, with the words — so the pane says it, not "may be gone, or outside".
      // #s24: a file that went with a Claude worktree removed after the session says so — the
      // route names the worktree (the owner: "just say it's gone", naming it).
      if (response.status === 410) {
        const worktree = response.headers.get("x-gone-worktree");
        throw Object.assign(refusal("This file is gone", worktree
          ? `Nothing is at ${item.path} any more: the worktree ${worktree} was removed after the session, and this file went with it.`
          : `Nothing is at ${item.path} any more: it was moved or deleted after the session named it.`), { gone: true });
      }
      if (response.status === 403) return response.text().then(words => { throw refusal("This monitor may not read this file", words || "It is outside what this monitor may read."); });
      if (!response.ok) throw new Error(`HTTP ${response.status} · This monitor did not offer this path. Reload the session for fresh links.`);
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
      // #324 through #s223: an image whose saved original could not be read shows the transcript's
      // own copy, as the lightbox did, rather than the refusal, and says why.
      if (item.fallback) { this.show(item, null, item.fallback, `${error.gone ? "The original is gone" : "The original could not be read"}: this is the copy saved with the session.`); return; }
      this.showError(item, error);
    });
  }
  /** The pane's own word that it cannot show a file: what went wrong, the file manager where the
   *  reader is at this machine, the path to copy, and the tab to close. */
  showError(item, error) {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    body.innerHTML = `<div class="preview-error"><strong>${escapeText(error.title || "Cannot preview this file")}</strong><span>${escapeText(error.message)}</span><div class="preview-error-actions">${besideAction(item, backend().local) === "reveal" ? '<button class="smallbtn" data-preview-reveal>Reveal in file manager</button>' : ""}<button class="smallbtn" data-copy-path>Copy original path</button><button class="smallbtn" data-close-preview>Close tab</button></div></div>`;
    const reveal = body.querySelector("[data-preview-reveal]");
    if (reveal) reveal.onclick = () => this.actions.reveal?.(item);
    body.querySelector("[data-copy-path]").onclick = () => {
      const operation = navigator.clipboard?.writeText(item.path || "");
      if (operation) operation.then(() => this.actions.toast?.("Copied the original path"));
      else this.actions.toast?.("This browser does not support copying");
    };
    body.querySelector("[data-close-preview]").onclick = () => this.closeTab(item.id);
  }
  /** A file the page does not show: a download, with the file's place above it — and the head's
   *  reveal beside it (#272). */
  showDownload(item, size) {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    // #s730: Share where the tap opens the share sheet; a file too large to hold still downloads.
    const { verb } = handOver(size), share = verb === "Share";
    const bytes = size >= 1048576 ? `${(size / 1048576).toFixed(1)} MB` : size >= 1024 ? `${Math.round(size / 1024)} KB` : size ? `${size} bytes` : "";
    body.innerHTML = `<div class="artifact-toolbar"><div class="artifact-location"><span>${escapeText(item.path || item.name)}</span></div></div><div class="preview-error preview-download"><strong>No preview for this file</strong><span>${escapeText(item.name)}${bytes ? ` · ${bytes}` : ""} — this pane shows text and images; ${share ? "share it to save it or open it elsewhere" : "this file downloads"}.</span><div class="preview-error-actions"><button class="smallbtn sv-primary" data-preview-download>${verb}</button></div></div>`;
    body.querySelector("[data-preview-download]").onclick = () => this.actions.download?.(item);
  }
  /** Text through mdrev's viewer (#270; every text file since #s13): a reader for text the
   *  transcript carries, the whole viewer for a file on disk — a document for Markdown, source code
   *  for anything else. Any failure — no bundle, a refused route, a mount that throws — falls back to
   *  `showPlain`, so mdrev can only ever add to what the pane showed. */
  showMarkdown(item, generation) {
    const body = byId("previewBody");
    if (this.objectUrl) { URL.revokeObjectURL(this.objectUrl); this.objectUrl = ""; }
    if (this.imageView) { this.imageView.destroy(); this.imageView = null; }
    if (this.stageObserver) { this.stageObserver.disconnect(); this.stageObserver = null; }
    body.classList.remove("production-loading");
    body.classList.add("mdrev-mounted");
    body.innerHTML = '<div class="mdrev-pane"></div>';
    this.markdownItem = item;
    const token = {}; this.markdownToken = token;
    mountMarkdown(body.firstElementChild, item).then(handle => {
      if (this.markdownToken !== token) { handle?.unmount(); return; }
      if (!handle) throw new Error("no mdrev");
      this.markdown = handle;
      // #335: not on a phone — the new tab is a page with no way back to the monitor.
      this.newTab.hidden = this.actions.phone();
      this.newTab.setAttribute("aria-label", `Open this ${isMarkdownName(item.name) ? "document" : "file"} in a new tab`);
      // #s13: shared review is threads on a document's notes, and mdrev files no notes on code. A
      // phone's alone, unless the host offers the sheet at every width (#s596, `reviewSheet`).
      this.reviewBtn.hidden = !(this.actions.phone() || this.actions.reviewSheet) || handle.held || !isMarkdownName(item.name);
      // #s30: where mdrev's toolbar offers no print of its own — a held reader (mdrev 1.1.24 prints
      // from its own Aa menu on a phone, #s31).
      this.printBtn.hidden = !isMarkdownName(item.name) || !handle.held;
      // #s352: the viewer came from a monitor upgraded since this page loaded; the rest of the page
      // is still the old build, which only a reload replaces.
      if (handle.updated) this.actions.toast?.("Agent Monitor was updated since this page loaded. Reload to get the new version.");
    }).catch(() => {
      if (this.markdownToken !== token) return;
      this.teardownMarkdown();
      if (generation === this.renderGeneration) this.showPlain(item, generation);
    });
  }
  teardownMarkdown() {
    this.markdown?.unmount(); this.markdown = null;
    this.newTab.hidden = true; this.image = null; this.reviewBtn.hidden = true; this.printBtn.hidden = true;
    this.markdownItem = null; this.markdownToken = null;
    byId("previewBody").classList.remove("mdrev-mounted");
  }
  showRoster() {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    body.innerHTML = `<div class="artifacts-list">${this.roster.map(r => `<div class="artifacts-row"><a href="${escapeText(r.url)}" target="_blank" rel="noopener" title="${escapeText(r.desc || r.url)}">${r.icon ? `<span class="artifacts-icon">${escapeText(r.icon)}</span>` : ""}<span class="artifacts-name">${escapeText(r.name || r.url)}</span>${r.desc ? `<span class="artifacts-desc">${escapeText(r.desc)}</span>` : ""}${r.count > 1 ? `<span class="artifacts-count">×${r.count}</span>` : ""}</a><button type="button" class="artifacts-jump" data-artifact-record="${r.at}" title="Go to where it was last published" aria-label="Go to where ${escapeText(r.name || r.url)} was last published">↳</button></div>`).join("")}</div>`;
  }
  /** #s12: the document, full-screen over the whole app, mounted on the review prefix — Share and
   *  Push where the machine is paired. Nothing navigates: closing unmounts it and removes the layer,
   *  and the transcript, the pane and the session list are as they were. It opens only from the open
   *  pane's Review control, and the app's rules that reach it over the whole app read the pane as
   *  open (`#app:not(.preview-off)`, production.css): an opener with the pane shut would need them
   *  changed, or the sheet stays in the view's box. */
  openReviewSheet(item) {
    if (!item || this.reviewSheet) return;
    const sheet = document.createElement("div");
    sheet.className = "review-sheet";
    sheet.setAttribute("role", "dialog");
    sheet.setAttribute("aria-label", `Review ${item.name || "document"}`);
    // The keys are the sheet's while it is open (#s596 review): it covers the window, so a key the
    // page answered would act on the transcript and the list hidden under it (`/` opening the find
    // there and taking the focus, ⌘K the jump-to). Marked as a guest (shared/keymap.js), and its
    // close takes the focus as it opens, which engages it before any key is pressed. The focus stays
    // inside: one that leaves it (Tab past either end, a control under the sheet focused) is put
    // back where it was, after the move's own work has run (mdrev's copy focuses a textarea of its
    // own on the body for a moment). ⌘K, the jump-to, has a listener of its own outside the keymap:
    // the sheet keeps it from the page (its default left alone, Ctrl+K in a field included), and
    // leaves every other key to mdrev, which binds its own on the window.
    // On close the focus goes back to the Review control, and the keymap answers again.
    sheet.dataset.guestKeys = "";
    sheet.innerHTML = `<div class="review-sheet-head"><button type="button" class="review-sheet-close" data-review-close aria-label="Close the review">${svg("x")}</button><strong>${escapeText(item.name || "document")}</strong></div><div class="review-sheet-body mdrev-pane"></div>`;
    (viewRoot.element() || viewRoot.layer()).append(sheet);
    const closeBtn = sheet.querySelector("[data-review-close]");
    const state = { sheet, handle: null, close: null };
    this.reviewSheet = state;
    // #s627: the sheet keeps above an on-screen keyboard while a field inside it has the focus, and
    // brings the field (mdrev's note card) into view: again when the field changes, or what it is
    // keyed on (`fitted`: the height the sheet was fitted to, else the sheet's own height) changes,
    // never for a pan of the same band and never while a pinch has the page zoomed.
    const viewport = window.visualViewport;
    let fitted = "", fittedField = null;
    // `rest` and `laidAt`: the view's top as it rested and its place in the layout then, and whether
    // the last fit found the reader typing (`fitSheetAboveKeyboard`).
    const applied = { bottom: 0, rest: 0, laidAt: 0, typing: false };
    const fit = () => {
      if (this.reviewSheet !== state) return;
      let at = viewRoot.active();
      while (at?.shadowRoot?.activeElement) at = at.shadowRoot.activeElement;
      // As the ride reads it (`typesInto`): a frame inside takes the keyboard too.
      const typing = !!at && (takesKeyboard(at) || at.localName === "iframe");
      const field = typing && sheet.contains(at) ? at : null;
      const height = fitSheetAboveKeyboard(sheet, field, viewport, applied, typing);
      // A pinch is the reader's: nothing is brought into view while the page is zoomed. The key is
      // cleared, so the return to a scale of 1 brings the field into view once: meanwhile the sheet was
      // as its rules draw it, taller than the fit (`fitSheetAboveKeyboard`), and the scroller may have
      // clamped the reader's place to that, which the fit back leaves under the keyboard.
      if (viewport && Math.abs((viewport.scale || 1) - 1) >= 0.01) { fitted = ""; fittedField = null; return; }
      // A host may shorten the page itself for the keyboard: the iOS app's web view is made shorter,
      // and no keyboard covers the page to fit the sheet above (measured on the iPad Simulator, iPadOS
      // 26.5: the page 1344px tall, then 941, the note's field focused before it shrank at 1247px under
      // the sheet's new foot, and iOS scrolls no scroller inside a fixed layer to it). So with nothing
      // fitted, the reveal is keyed on the sheet's own height as laid out now: the field is brought into
      // view as it takes the focus (nothing moves for one already in view) and again whenever the sheet
      // is laid out at another height. Never the page's height as an event reports it: the app's web
      // view says its new height before its fixed layers are laid out at it (the walk read the page at
      // 941 while mdrev's scroll pane still ended at 1278, mid-animation), and the reveal measured then
      // stopped short; the observer below hears the sheet's box itself.
      const band = height || (field ? `page ${Math.round(sheet.getBoundingClientRect().height)}` : "");
      if (field && band && (band !== fitted || field !== fittedField)) revealInSheet(sheet, field);
      fitted = band; fittedField = field;
    };
    const fitSoon = () => queueMicrotask(fit);
    // The sheet laid out at another size (the app's web view shortened for the keyboard, a window
    // resized, the fit itself) fits again in the next frame: outside the observer's report, since a fit
    // changes the box it watches, and after the layout the report came from.
    let frame = 0;
    const sized = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => {
      if (!frame) frame = requestAnimationFrame(() => { frame = 0; fit(); });
    });
    // Focus going from one field of the sheet to another: the focusin that follows refits, and the
    // sheet does not grow back in between.
    const fitAfterLeaving = event => {
      const to = event.relatedTarget;
      if (to && sheet.contains(to) && takesKeyboard(to)) return;
      fitSoon();
    };
    const close = () => {
      if (this.reviewSheet !== state) return;
      this.reviewSheet = null;
      viewport?.removeEventListener("resize", fit);
      viewport?.removeEventListener("scroll", fit);
      sized?.disconnect();
      if (frame) cancelAnimationFrame(frame);
      sheet.removeEventListener("focusin", fitSoon);
      sheet.removeEventListener("focusout", fitAfterLeaving);
      removeEventListener("keydown", onKey, true);
      removeEventListener("focusin", onFocus, true);
      // Read before mdrev goes: its unmount takes the focused control with it, leaving the body.
      const inside = sheet.contains(viewRoot.active());
      state.handle?.unmount();
      sheet.remove();
      // The focus goes back to the control that opened the sheet: Review in the head, or More where the
      // head folds it there (#s663, a phone held sideways).
      const back = [this.reviewBtn, this.moreBtn].find(control => control?.isConnected && !control.hidden && control.getClientRects().length);
      if (inside && back) back.focus({ preventScroll: true });
    };
    const onKey = event => {
      if (event.key === "Escape") { event.preventDefault(); close(); }
      else if ((event.metaKey || event.ctrlKey) && event.key?.toLowerCase() === "k") event.stopPropagation();
    };
    const onFocus = event => {
      if (sheet.contains(event.target)) return;
      const from = event.relatedTarget;
      const back = from && from.isConnected && sheet.contains(from) ? from : closeBtn;
      queueMicrotask(() => {
        if (this.reviewSheet !== state || sheet.contains(viewRoot.active())) return;
        back.focus({ preventScroll: true });
        // A control that cannot take the focus now (hidden since) leaves it outside: the close takes it.
        if (!sheet.contains(viewRoot.active())) closeBtn.focus({ preventScroll: true });
      });
    };
    state.close = close;
    addEventListener("keydown", onKey, true);
    addEventListener("focusin", onFocus, true);
    viewport?.addEventListener("resize", fit);
    viewport?.addEventListener("scroll", fit);
    sheet.addEventListener("focusin", fitSoon);
    sheet.addEventListener("focusout", fitAfterLeaving);
    sized?.observe(sheet);
    closeBtn.onclick = close;
    closeBtn.focus({ preventScroll: true });
    mountMarkdown(sheet.querySelector(".review-sheet-body"), item, { review: true }).then(handle => {
      if (this.reviewSheet !== state) { handle?.unmount(); return; }
      if (!handle) throw new Error("no mdrev");
      state.handle = handle;
    }).catch(() => { close(); this.actions.toast?.("The review cannot be opened"); });
  }
  /** What a tab's address carries for the image the pane shows (#s11). */
  imageQuery() {
    const query = this.image ? imageTabQuery(this.image.item, this.image.data) : "";
    if (!query) this.actions.toast?.("This image cannot be handed to a tab");
    return query;
  }
  show(item, text, data, note = "") {
    const body = byId("previewBody"); body.classList.remove("production-loading");
    this.image = null; if (!this.markdown) this.newTab.hidden = true;
    // One object URL at a time: a tab reopened per session switch minted a new blob and never
    // released the last, so the page held every image it had ever previewed.
    if (this.objectUrl) { URL.revokeObjectURL(this.objectUrl); this.objectUrl = ""; }
    // A previewed image zooms and pans like the enlarged one (#228): this panel is narrow, so it
    // is exactly where a screenshot is "too big to fit". Same shared engine, same gestures.
    if (this.imageView) { this.imageView.destroy(); this.imageView = null; }
    if (this.stageObserver) { this.stageObserver.disconnect(); this.stageObserver = null; }
    if (data) {
      this.objectUrl = data.startsWith("blob:") ? data : "";
      // #324 through #s223: the pane says when it shows the session's copy because the original
      // is gone, as the lightbox said which it was showing. And it carries the lightbox's zoom
      // bar (#228, "allow me to have zoom control"), since every image now opens here: − and +,
      // the percentage that fits again, and 1:1, shown once the picture has loaded.
      const zoom = `<div class="image-zoom" data-zoom-bar hidden><button class="image-zoom-btn" type="button" data-zoom="out" aria-label="Zoom out" title="Zoom out (−)">−</button><button class="image-zoom-level" type="button" data-zoom="fit" title="Fit to the pane (0)"><span data-zoom-percent>100%</span></button><button class="image-zoom-btn" type="button" data-zoom="in" aria-label="Zoom in" title="Zoom in (+)">+</button><button class="image-zoom-btn image-zoom-actual" type="button" data-zoom="actual" title="Actual size (1)">1:1</button></div>`;
      body.innerHTML = `<div class="artifact-surface artifact-stage">${note ? `<div class="preview-note" data-preview-note>${escapeText(note)}</div>` : ""}<img class="artifact-image" alt="${escapeText(item.name)}">${zoom}</div>`;
      const stage = body.querySelector(".artifact-stage"), img = stage.querySelector("img");
      const bar = stage.querySelector("[data-zoom-bar]"), percent = stage.querySelector("[data-zoom-percent]");
      // The preview's zoom keys belong to it only while the pane is open (#268) — the pane
      // can be collapsed with an image still mounted, and a collapsed pane must not own
      // `0`/`-`/`+`… against the compose box beside it. In a host's page, also only while the
      // reader is working in the view (`engaged`, #s429 stage 6): a zoomed image must not take the
      // arrows the host's page scrolls with. There its keys are heard in the capture phase, as the
      // view's own are (`keysFirst`), so a host's listener bound before the viewer finds a key it
      // took already taken; the app's page keeps the document's bubble phase, as before.
      const keyHost = this.actions.keysFirst ? { addEventListener: (type, fn) => document.addEventListener(type, fn, true), removeEventListener: (type, fn) => document.removeEventListener(type, fn, true) } : undefined;
      this.imageView = createImageView(stage, img, { keyHost, isActive: () => this.uiState.preview && this.actions.engaged?.() !== false, onChange: state => { percent.textContent = `${state.percent}%`; } });
      img.addEventListener("load", () => { bar.hidden = false; }, { once: true });
      bar.onclick = event => {
        const how = event.target.closest("[data-zoom]")?.dataset.zoom, view = this.imageView;
        if (!how || !view) return;
        if (how === "in") view.zoomBy(1.25);
        else if (how === "out") view.zoomBy(1 / 1.25);
        else if (how === "actual") view.actual();
        else view.fit();
      };
      // The pane opens, is dragged wider, or the window resizes: the fit follows (#305). `sync`
      // re-fits a reader at the default and only re-clamps one who has zoomed. Without it a fit
      // taken while the pane was still opening stayed a thumbnail.
      if (typeof ResizeObserver !== "undefined") {
        const view = this.imageView;
        this.stageObserver = new ResizeObserver(() => view.sync());
        this.stageObserver.observe(stage);
      }
      img.src = data;
      // #s11: and to a tab of its own, on the desktop (#335: not on a phone, where a tab has no way
      // back). What the tab reads is what the pane was offered: the file by its stamp, or the
      // embedded bytes themselves.
      this.image = { item, data };
      this.newTab.hidden = this.actions.phone();
      this.newTab.setAttribute("aria-label", "Open this image in a new tab");
      return;
    }
    const html = /\.html?$/i.test(item.name || "");
    if (html && backend().paired) { this.showPage(item, text); return; }
    // The file's place above its text; revealing it is the head's control (#272), one for every view.
    body.innerHTML = `<div class="artifact-toolbar"><div class="artifact-location"><span>${escapeText(item.path || item.name)}</span></div></div><div class="artifact-surface"><pre class="artifact-text"></pre></div>`;
    body.querySelector("pre").textContent = text || "";
  }
  /** An agent's HTML page, in a frame sandboxed without its origin, its scripts running under the
   *  preview's policy (sandbox.js), and sending no referrer. Its document is the page's text with
   *  that policy in front (`sandboxDocument`), written as the frame's `srcdoc`, which takes the
   *  policy of the page around it as well. The native app's page allows no scripts, so the native
   *  app frames the page from a source of its own (#s436, design/monitor-hub.md D3): `frame`, given
   *  that document, answers the address the frame loads. An answer that is not an address, or a
   *  refusal, is the pane's own error, never an empty frame. */
  showPage(item, text) {
    const body = byId("previewBody");
    body.innerHTML = '<iframe class="artifact-html-frame" sandbox="allow-scripts" referrerpolicy="no-referrer"></iframe>';
    const frame = body.querySelector("iframe"), source = sandboxDocument(text || "");
    if (!this.actions.frame) { frame.srcdoc = source; return; }
    const generation = this.renderGeneration;
    const current = () => generation === this.renderGeneration && frame.isConnected;
    // One address per tab while its text is the same: the pane draws a tab again whenever it is shown
    // (a tab chosen, the pane reopened, a roster change), and each ask would make the native app keep
    // another copy of the page. The address serves the tab for its whole life, across session
    // switches, and nothing ever says it is released: the native app keeps every one for the life of
    // the page. A refusal is not kept, so the next time the tab is shown asks again.
    this.frames ||= new WeakMap();
    let held = this.frames.get(item);
    if (held?.source !== source) {
      held = { source, address: new Promise(resolve => resolve(this.actions.frame(source))).then(address => {
        if (typeof address !== "string" || !address) throw new Error("No address came back for this page's frame.");
        return address;
      }) };
      this.frames.set(item, held);
      held.address.catch(() => { if (this.frames.get(item) === held) this.frames.delete(item); });
    }
    held.address.then(address => {
      if (current()) frame.src = address;
    }).catch(error => {
      if (current()) this.showError(item, refusal("Cannot show this page", error?.message || String(error || "Its frame could not be made.")));
    });
  }
}

