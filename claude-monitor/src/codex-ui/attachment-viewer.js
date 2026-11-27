import { revealQuery } from "./shared/capabilities.js";
import { backend } from "./backend.js";
import { viewRoot } from "./view-root.js";

const escapeName = value => String(value || "attachment").replace(/[\\/:*?"<>|]/g, "-");

// #s36, the owner: a download on a phone opened iOS's preview box ("Open in Preview", "More…"); it
// should offer where the file goes — Photos, Files, or an app — and a desktop should simply save it to
// Downloads. A touch-first device (`pointer: coarse`: a phone, a tablet; never a desktop, whose own
// share sheet is not a download) hands the file to the system share sheet through the Web Share API:
// Save Image (to Photos) for media, Save to Files, AirDrop, every app that takes the type. Elsewhere,
// or where the sheet cannot take the file, it is saved as before.
export const shareHere = () => typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches && typeof navigator.share === "function";
// Above this a file is never read into memory (a film on a phone): it streams from /file to the
// browser's own download, as /file serves anything over its viewer cap.
const IN_MEMORY_MAX = Number(new URLSearchParams(typeof location === "object" ? location.search : "").get("inMemoryMax")) || 64 * 1024 * 1024; // `?inMemoryMax=` lets a case use a small file

// iOS's own share glyph, a box open at the top with an arrow rising out of it.
const SHARE_GLYPH = '<svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3v12M8 7l4-4 4 4"/><path d="M8.5 10H7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-7a2 2 0 0 0-2-2h-1.5"/></svg>';

/** What a control that hands the reader a file's bytes shows (#s730, Hong on a real iPhone: "the
 *  download works but the button shows like a down arrow which is not ideal"): where its tap opens
 *  the share sheet (`shareHere`, the test the tap itself makes), the share glyph and the word Share;
 *  elsewhere ↓ and Download. `size`, where the control knows it, keeps ↓ for a file too large to
 *  hold, which streams to the browser's own download on a phone too; a control drawn before the size
 *  is known follows the share sheet. Asked as each control is drawn. */
export const handOver = size => (shareHere() && !(size > IN_MEMORY_MAX)
  ? { verb: "Share", glyph: SHARE_GLYPH }
  : { verb: "Download", glyph: "↓" });
// A file read for the sheet whose tap had expired by the time it arrived (iOS asks for a fresh one
// after a slow fetch) waits this long for the next tap, which shares it at once.
const READY_MS = 120 * 1000;
const itemKey = item => `${item.path || item.name || ""}|${item.fsig || ""}|${item.data ? item.data.length : ""}`;

// The attachment actions every view of a file shares: reveal, copy the path, download. The app
// shell's image lightbox that lived here is gone (#s225): since #s223 every image opens in the
// preview pane, whose stage carries the same zoom and the zoom bar.
export class AttachmentViewer {
  constructor(actions) {
    this.actions = actions;
  }
  /** The view is unmounted (#s429 stage 7): a download still being read stops, and nothing it would
   *  still do (a save, the share sheet, a toast) happens. */
  destroy() { this.ended = true; this.controller?.abort(); this.ready = null; }

  /** Ask the server to show the file in the file manager. It acts only on a path it offered
   *  with a reveal stamp and that still sits inside a hosted session's roots; it hands over
   *  no bytes, which is why this works even where the render policy shows nothing inline. */
  async reveal(item) {
    if (!item?.path || !item?.sig) { this.actions.toast?.("This path was not offered for reveal"); return; }
    try {
      const response = await backend().fetch(revealQuery({ path: item.path, sig: item.sig }), { cache: "no-store" });
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
    const name = escapeName(item.name);
    // The server of the view the reader tapped in, kept past the share sheet's wait (#s595: the view
    // may be mounted on another machine's handle by the time the fetch goes).
    const server = backend();
    // The second tap on a file whose sheet the first could not open: share it now, in this tap.
    if (this.ready && this.ready.key === itemKey(item) && Date.now() - this.ready.at < READY_MS && shareHere()) {
      const { file } = this.ready;
      this.ready = null;
      if (await this.share(file)) return;
    }
    try {
      let response, fileUrl = null, controller = null;
      if (item.data) response = await fetch(item.data);
      // #s29: text the transcript carried, with no file stamp to fetch the file by, is its own bytes.
      else if (item.text != null && !(item.path && item.fsig)) response = new Response(new Blob([item.text], { type: "text/plain;charset=utf-8" }));
      else {
        const filePath = `/file?path=${encodeURIComponent(item.path || "")}&sig=${encodeURIComponent(item.fsig || "")}`;
        fileUrl = server.url(filePath);
        controller = this.controller = new AbortController();
        response = await server.fetch(filePath, { cache: "no-store", signal: controller.signal });
      }
      if (this.ended) { controller?.abort(); return; }
      if (!response.ok) throw Object.assign(new Error(`HTTP ${response.status}`), { status: response.status });
      // Too large to hold: the refusal checks above are done, so the browser streams it to disk.
      if (fileUrl && Number(response.headers.get("Content-Length") || 0) > IN_MEMORY_MAX) {
        controller.abort();
        this.save(fileUrl, name);
        this.actions.toast?.("Download started");
        return;
      }
      const blob = await response.blob();
      if (this.ended) return;
      if (shareHere()) {
        const file = new File([blob], name, { type: blob.type || "application/octet-stream" });
        if (navigator.canShare?.({ files: [file] })) {
          const shared = await this.share(file, itemKey(item));
          if (shared !== false) return;
        }
      }
      const url = URL.createObjectURL(blob);
      this.save(url, name);
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      this.actions.toast?.("Download started");
    } catch (error) {
      if (this.ended) return;
      // #s7: say which — a file that is gone (410, or a fetch that failed outright) is not one this
      // monitor may not read (403).
      const why = error?.status === 403 ? "This monitor may not read that file" : error?.status && error.status !== 410 ? `Could not download (HTTP ${error.status})` : "The original file is gone";
      if (navigator.clipboard?.writeText && item.path) {
        try { await navigator.clipboard.writeText(item.path); this.actions.toast?.(`${why} — copied the recorded path instead`); return; } catch (_) {}
      }
      this.actions.toast?.(why);
    }
  }

  /** A link the browser follows as a download: the bytes, or /file's stream, saved under `name`. */
  save(href, name) {
    const link = document.createElement("a");
    link.href = href; link.download = name; viewRoot.layer().append(link); link.click(); link.remove();
  }

  /** The system share sheet for `file`. True once shared or dismissed by the reader; "ready" when the
   *  tap had expired (the next one shares it); false when the sheet cannot take it — save it instead. */
  async share(file, key) {
    try {
      await navigator.share({ files: [file] });
      return true;
    } catch (error) {
      if (error?.name === "AbortError") return true;
      if (error?.name === "NotAllowedError" && key) {
        this.ready = { key, file, at: Date.now() };
        this.actions.toast?.("Ready — tap Share again to share it");
        return "ready";
      }
      return false;
    }
  }
}
