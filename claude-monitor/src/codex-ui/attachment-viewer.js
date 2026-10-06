import { besideAction, onPhone, revealQuery } from "./shared/capabilities.js";
import { createImageView } from "./shared/image-view.js";
import { imageTabbable, imageTabHref } from "./preview.js";

const escapeName = value => String(value || "attachment").replace(/[\\/:*?"<>|]/g, "-");

export class AttachmentViewer {
  constructor(actions) {
    this.actions = actions;
    this.item = null;
    this.root = document.createElement("div");
    this.root.className = "image-lightbox";
    this.root.hidden = true;
    this.root.setAttribute("role", "dialog");
    this.root.setAttribute("aria-modal", "true");
    this.root.setAttribute("aria-label", "Image preview");
    this.root.innerHTML = `<div class="image-lightbox-card"><div class="image-lightbox-head"><div class="image-lightbox-title"><strong data-lightbox-name></strong><span class="image-lightbox-status" data-lightbox-status></span></div><div class="image-lightbox-actions"><button class="smallbtn" type="button" data-lightbox-newtab hidden aria-label="Open this image in a new tab" title="Open in a new tab">↗</button><button class="smallbtn" type="button" data-lightbox-sidebar>opens in the preview pane</button><button class="smallbtn" type="button" data-lightbox-reveal hidden>Reveal in file manager</button><button class="smallbtn" type="button" data-lightbox-download hidden>Download</button><button class="image-lightbox-close" type="button" data-lightbox-close aria-label="Close preview">×</button></div></div><div class="image-lightbox-stage"><img data-lightbox-image alt=""><div class="image-zoom" data-zoom-bar hidden><button class="image-zoom-btn" type="button" data-zoom="out" aria-label="Zoom out" title="Zoom out (−)">−</button><button class="image-zoom-level" type="button" data-zoom="fit" title="Fit to screen (0)"><span data-zoom-percent>100%</span></button><button class="image-zoom-btn" type="button" data-zoom="in" aria-label="Zoom in" title="Zoom in (+)">+</button><button class="image-zoom-btn image-zoom-actual" type="button" data-zoom="actual" title="Actual size (1)">1:1</button></div><div class="image-lightbox-loading"><span aria-hidden="true"></span><small>Loading image…</small></div><div class="image-lightbox-error" hidden><span class="image-lightbox-error-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M4.8 5.5A2.5 2.5 0 0 1 7.3 3h9.4a2.5 2.5 0 0 1 2.5 2.5v10.1M18.5 19H7.3a2.5 2.5 0 0 1-2.5-2.5V8.8M7.5 14l2.1-2.1 2.6 2.6 1.2-1.2M3 3l18 18"/></svg></span><strong>That image cannot be opened</strong><span data-lightbox-error-detail>Only the original path was kept; a temporary file may have been cleaned up or moved.</span><div class="image-lightbox-error-actions"><button class="smallbtn" type="button" data-lightbox-copy>Copy original path</button><button class="smallbtn primary" type="button" data-lightbox-close>Close</button></div></div></div></div>`;
    document.body.append(this.root);
    this.image = this.root.querySelector("[data-lightbox-image]");
    this.error = this.root.querySelector(".image-lightbox-error");
    this.root.onclick = event => {
      const zoom = event.target.closest("[data-zoom]");
      if (zoom) {
        const how = zoom.dataset.zoom;
        if (how === "in") this.view?.zoomBy(1.25);
        else if (how === "out") this.view?.zoomBy(1 / 1.25);
        else if (how === "actual") this.view?.actual();
        else this.view?.fit();
        return;
      }
      if (event.target === this.root || event.target.closest("[data-lightbox-close]")) this.close();
      else if (event.target.closest("[data-lightbox-sidebar]")) { this.close(); this.actions.openPreview?.(this.item); }
      else if (event.target.closest("[data-lightbox-copy]")) this.copyPath(this.item);
      else if (event.target.closest("[data-lightbox-reveal]")) this.reveal(this.item);
      else if (event.target.closest("[data-lightbox-download]")) this.download(this.item);
      // #s11: the image in a tab of its own, on the desktop. Not `noopener`: the tab inherits a copy
      // of this page's sessionStorage, which is how embedded bytes reach it.
      else if (event.target.closest("[data-lightbox-newtab]")) { const href = imageTabHref(this.item, this.image.getAttribute("src") || ""); if (href) window.open(href, "_blank"); }
    };
    this.percent = this.root.querySelector("[data-zoom-percent]");
    this.zoomBar = this.root.querySelector("[data-zoom-bar]");
    // Zoom and pan are the shared module's (#228), so this viewer, the classic page's lightbox
    // and its file view all behave the same way. It is built once and re-fitted per image: the
    // stage outlives the picture shown in it.
    this.view = createImageView(this.root.querySelector(".image-lightbox-stage"), this.image, {
      // The lightbox owns the zoom keys only while it is open (#268): its root is `hidden`
      // when closed, and the app shell builds this viewer once at load, so without this the
      // closed lightbox ate `0`/`-`/`+`… from the compose box the moment the shell started.
      isActive: () => !this.root.hidden,
      onChange: state => {
        if (this.percent) this.percent.textContent = `${state.percent}%`;
      },
    });
    this.image.onload = () => {
      this.root.dataset.state = "ready";
      this.root.querySelector(".image-lightbox-loading").hidden = true;
      this.zoomBar.hidden = false;
      this.view.fit();
    };
    this.image.onerror = () => {
      // #324: the original is gone, but the session kept a copy: show that instead, and say so.
      if (this.item?.fallback && !this.fellBack) {
        this.fellBack = true;
        const status = this.root.querySelector("[data-lightbox-status]");
        status.textContent = "saved with the session"; status.className = "image-lightbox-status embedded";
        this.image.src = this.item.fallback;
        return;
      }
      this.root.dataset.state = "unavailable";
      this.image.hidden = true;
      this.zoomBar.hidden = true;
      this.root.querySelector(".image-lightbox-loading").hidden = true;
      this.error.hidden = false;
      this.root.querySelector("[data-lightbox-sidebar]").hidden = true;
      this.root.querySelector("[data-lightbox-newtab]").hidden = true;
      this.root.querySelector("[data-lightbox-copy]").hidden = !this.item?.path;
      const status = this.root.querySelector("[data-lightbox-status]");
      status.textContent = "original unavailable"; status.className = "image-lightbox-status unavailable";
    };
    addEventListener("keydown", event => { if (event.key === "Escape" && !this.root.hidden) this.close(); });
  }

  openImage(item) {
    this.item = item;
    this.root.dataset.state = "loading";
    this.root.querySelector("[data-lightbox-name]").textContent = item.name || "image";
    const status = this.root.querySelector("[data-lightbox-status]");
    this.fellBack = false;
    // #324: the file on disk, when the image is opened from it with the session's copy behind it.
    status.textContent = item.fallback ? "original" : item.embedded ? "saved with the session" : "temporary file";
    status.className = `image-lightbox-status ${item.embedded ? "embedded" : "temporary"}`;
    this.root.querySelector("[data-lightbox-sidebar]").hidden = !item.source;
    // #s29: the file manager for a reader at this machine, the image as a download for one elsewhere.
    const beside = besideAction(item);
    this.root.querySelector("[data-lightbox-reveal]").hidden = beside !== "reveal";
    this.root.querySelector("[data-lightbox-download]").hidden = beside !== "download";
    // #s11 (#335: not on a phone, where a tab has no way back).
    this.root.querySelector("[data-lightbox-newtab]").hidden = onPhone() || !imageTabbable(item, item.source || "");
    this.error.hidden = true;
    this.root.querySelector(".image-lightbox-loading").hidden = false;
    this.image.hidden = false;
    this.image.alt = item.name || "Attached image";
    this.image.removeAttribute("src");
    this.image.src = item.source || "";
    this.zoomBar.hidden = true;
    this.root.hidden = false;
    this.root.classList.add("open");
    // A cached image can be `complete` before this frame, so `load` never fires and the bar
    // would stay hidden on every open after the first.
    if (this.image.complete && this.image.naturalWidth) { this.zoomBar.hidden = false; this.view.fit(); }
    this.root.querySelector("[data-lightbox-close]").focus();
  }

  close() {
    this.root.classList.remove("open");
    this.root.hidden = true;
  }

  /** Ask the server to show the file in the file manager. It acts only on a path it offered
   *  with a reveal stamp and that still sits inside a hosted session's roots; it hands over
   *  no bytes, which is why this works even where the render policy shows nothing inline. */
  async reveal(item) {
    if (!item?.path || !item?.sig) { this.actions.toast?.("This path was not offered for reveal"); return; }
    try {
      const response = await fetch(revealQuery({ path: item.path, sig: item.sig }), { cache: "no-store" });
      if (response.ok) { this.actions.toast?.("Revealed in the file manager"); return; }
      this.actions.toast?.(response.status === 404 ? "Nothing to reveal — the path is gone" : `Could not reveal: HTTP ${response.status}`);
    } catch (error) { this.actions.toast?.(`Could not reveal: ${error.message}`); }
  }

  async copyPath(item) {
    const path = item?.path || "";
    if (!path) { this.actions.toast?.("This session kept no path for that attachment"); return; }
    if (!navigator.clipboard?.writeText) { this.actions.toast?.("This browser does not support copying"); return; }
    try { await navigator.clipboard.writeText(path); this.actions.toast?.("Copied attachment path"); }
    catch (_) { this.actions.toast?.("Could not copy the attachment path"); }
  }

  async download(item) {
    try {
      let response;
      if (item.data) response = await fetch(item.data);
      // #s29: text the transcript carried, with no file stamp to fetch the file by, is its own bytes.
      else if (item.text != null && !(item.path && item.fsig)) response = new Response(new Blob([item.text], { type: "text/plain;charset=utf-8" }));
      else response = await fetch(`/file?path=${encodeURIComponent(item.path || "")}&sig=${encodeURIComponent(item.fsig || "")}`, { cache: "no-store" });
      if (!response.ok) throw Object.assign(new Error(`HTTP ${response.status}`), { status: response.status });
      const url = URL.createObjectURL(await response.blob());
      const link = document.createElement("a");
      link.href = url; link.download = escapeName(item.name); document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      this.actions.toast?.("Download started");
    } catch (error) {
      // #s7: say which — a file that is gone (410, or a fetch that failed outright) is not one this
      // monitor may not read (403).
      const why = error?.status === 403 ? "This monitor may not read that file" : error?.status && error.status !== 410 ? `Could not download (HTTP ${error.status})` : "The original file is gone";
      if (navigator.clipboard?.writeText && item.path) {
        try { await navigator.clipboard.writeText(item.path); this.actions.toast?.(`${why} — copied the recorded path instead`); return; } catch (_) {}
      }
      this.actions.toast?.(why);
    }
  }
}
