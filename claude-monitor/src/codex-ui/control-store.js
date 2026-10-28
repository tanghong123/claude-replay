import { canCompose, canInject, controlState, selectedRow } from "./state.js";
// The /api/send + /api/consent protocol is the shared module's (#48): this store keeps the
// app shell's markup, its confirm dialog and its toasts, and asks the protocol what a server
// answer means.
import { consentQuery, grantOutcome, runRevoke, runSend } from "./shared/control-protocol.js";
import { DialogCard } from "./dialog-card.js";

const byId = id => document.getElementById(id);

// #s236 (Hong: the box said too much, and "writes to live tmux" for a herdr pane): ONE line says
// where a message goes — a live pane by its host's own name, or a resume of an idle session.
function composeTargetLine(row, mode) {
  if (mode !== "tmux") return "Resumes this session";
  // #s297 (Hong: "just show herdr is enough"): herdr's pane id is detail the box does not need.
  if (row?.term === "herdr") return "Types into herdr";
  return row?.target ? `Types into tmux pane ${row.target}` : "Types into its tmux pane";
}
// #s237: images go only to a Claude session in a herdr pane (the row says `sendsImages`), within
// Codex native mode's bounds (codex-workspace/attachments.js), which the server checks again.
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const IMAGE_LIMITS = { count: 4, fileBytes: 4 * 1024 * 1024, totalBytes: 8 * 1024 * 1024 };
const IMAGE_GLYPH = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="3.5" y="5" width="17" height="14" rx="2.5"/><circle cx="9" cy="10" r="1.6"/><path d="m4 17 5-4.5 4 3.5 3-2.5 4.5 3.5"/></svg>';
const readDataUrl = file => new Promise((resolve, reject) => {
  const reader = new FileReader();
  reader.onload = () => resolve(String(reader.result));
  reader.onerror = () => reject(reader.error || new Error("could not read the image"));
  reader.readAsDataURL(file);
});
const json = response => response.json().catch(() => ({ ok: false, error: `HTTP ${response.status}` }));
const post = (url, body) => fetch(url, { method: "POST", body, cache: "no-store" }).then(json);

export class ControlStore {
  constructor(actions) {
    this.actions = actions;
    this.bind();
    this.paint();
  }
  bind() {
    this.revokeButton = document.createElement("button");
    this.revokeButton.type = "button";
    this.revokeButton.className = "production-revoke";
    this.revokeButton.textContent = "Revoke pane grant";
    this.revokeButton.onclick = () => this.revoke();
    byId("closeComposer").before(this.revokeButton);
    byId("writeSwitch").onclick = () => {
      if (!controlState.paired) { this.showPairHelp(); return; }
      controlState.write = !controlState.write;
      this.paint();
    };
    byId("writeBtn").onclick = event => { if (!event.target.closest("button")) byId("writeSwitch").click(); };
    byId("writeInfo").onclick = event => { event.stopPropagation(); this.showPairHelp(); };
    byId("composeFab").onclick = () => this.open();
    byId("closeComposer").onclick = () => this.close();
    byId("sendBtn").onclick = () => this.attempt();
    byId("composeInput").onkeydown = event => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); this.attempt(); } };
    byId("cancelSend").onclick = () => this.closeConfirm();
    byId("confirmLayer").querySelector("h3").textContent = "Allow typing into this pane?";
    byId("confirmSend").onclick = () => this.grantAndSend();
    // #s237: the image half — an attach control where the foot's "＋" was, a strip of what is
    // attached above the field, and an image pasted into the field.
    this.images = new Map();
    this.nextImage = 0;
    this.attachButton = document.createElement("button");
    this.attachButton.type = "button";
    this.attachButton.id = "composeAttach";
    this.attachButton.className = "compose-attach";
    this.attachButton.innerHTML = IMAGE_GLYPH;
    this.attachButton.title = "Attach images (or paste them)";
    this.attachButton.setAttribute("aria-label", "Attach images");
    this.attachButton.hidden = true;
    this.fileInput = document.createElement("input");
    this.fileInput.type = "file";
    this.fileInput.id = "composeImageInput";
    this.fileInput.accept = IMAGE_TYPES.join(",");
    this.fileInput.multiple = true;
    this.fileInput.hidden = true;
    this.attachButton.onclick = () => this.fileInput.click();
    this.fileInput.onchange = () => { const files = [...this.fileInput.files]; this.fileInput.value = ""; this.addImages(files); };
    byId("composer").querySelector(".compose-foot").prepend(this.attachButton, this.fileInput);
    this.imageStrip = document.createElement("div");
    this.imageStrip.id = "composeImages";
    this.imageStrip.className = "compose-images";
    byId("composeInput").before(this.imageStrip);
    // #s298: a dialog the session holds in its herdr pane, shown above the field and answered here.
    this.dialog = new DialogCard({
      toast: text => this.actions.toast(text),
      refreshIndex: () => this.actions.refreshIndex(),
      askGrant: then => this.askGrant(then),
      writeHere: () => byId("composeInput").focus(),
    });
    this.dialog.mount(this.imageStrip);
    byId("composeInput").onpaste = event => {
      if (!this.imagesAllowed()) return;
      const files = [...(event.clipboardData?.items || [])].filter(item => item.kind === "file" && item.type.startsWith("image/")).map(item => item.getAsFile()).filter(Boolean);
      if (!files.length) return;
      event.preventDefault();
      const text = event.clipboardData.getData("text/plain"), input = byId("composeInput");
      if (text) { input.setRangeText(text, input.selectionStart, input.selectionEnd, "end"); input.dispatchEvent(new Event("input", { bubbles: true })); }
      this.addImages(files);
    };
  }
  // Codex native mode shares #composer and sends its own images: none of this one's there.
  imagesAllowed(row = controlState.row) { return controlState.mode === "tmux" && !!row?.sendsImages && !document.body.dataset.sharedCodex; }
  imagesFor(target = controlState.row?.id) { return this.images.get(target) || []; }
  async addImages(files) {
    const target = controlState.row?.id;
    if (!target || !this.imagesAllowed()) return;
    for (const file of files) {
      const items = this.imagesFor(target);
      const reason = !IMAGE_TYPES.includes(file.type) ? "Choose a PNG, JPEG, WebP or GIF image."
        : !(file.size > 0 && file.size <= IMAGE_LIMITS.fileBytes) ? "Each image must be between 1 byte and 4 MiB."
        : items.length >= IMAGE_LIMITS.count ? "Attach at most 4 images per message."
        : items.reduce((total, item) => total + item.size, 0) + file.size > IMAGE_LIMITS.totalBytes ? "Images in one message must total at most 8 MiB." : null;
      if (reason) { this.actions.toast(`${file.name || "Image"}: ${reason}`); continue; }
      const item = { id: ++this.nextImage, name: file.name || "Pasted image", size: file.size, url: null };
      this.images.set(target, [...items, item]);
      this.paintImages();
      try { item.url = await readDataUrl(file); } catch (error) {
        this.removeImage(target, item.id);
        this.actions.toast(`${item.name}: ${error?.message || "could not read the image"}`);
        continue;
      }
      this.paintImages();
    }
  }
  removeImage(target, id) {
    const left = this.imagesFor(target).filter(item => item.id !== id);
    if (left.length) this.images.set(target, left); else this.images.delete(target);
    this.paintImages();
  }
  paintImages() {
    const allowed = this.imagesAllowed() && byId("composer").classList.contains("production-show");
    this.attachButton.hidden = !allowed;
    byId("composer").classList.toggle("attachable", allowed);
    const items = allowed ? this.imagesFor() : [];
    this.imageStrip.replaceChildren(...items.map(item => {
      const tile = document.createElement("div");
      tile.className = "compose-image";
      tile.title = item.name;
      if (item.url) { const img = document.createElement("img"); img.src = item.url; img.alt = item.name; tile.append(img); }
      else { const wait = document.createElement("span"); wait.className = "compose-image-loading"; wait.textContent = "Loading…"; tile.append(wait); }
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "compose-image-remove";
      remove.textContent = "×";
      remove.setAttribute("aria-label", `Remove ${item.name}`);
      const target = controlState.row?.id;
      remove.onclick = () => this.removeImage(target, item.id);
      tile.append(remove);
      return tile;
    }));
    // The dock's draft dot counts images too, for the session the dock writes to.
    byId("composer").dataset.imageDraft = this.imagesFor(selectedRow()?.id).length ? "1" : "";
    byId("composer").dispatchEvent(new Event("compose-images"));
  }
  showPairHelp() {
    const help = byId("writeHelp");
    help.innerHTML = controlState.paired
      ? "<b>Monitor is paired</b><p>With write mode on, a writable session shows a compose box. A live terminal still needs its own grant.</p>"
      : "<b>Write mode needs a paired monitor</b><p>Stop the monitor and run this in a terminal:</p><code>agent-monitor --pair</code>";
    help.hidden = false; help.classList.add("show");
  }
  paint() {
    const row = selectedRow();
    const available = canCompose(row);
    byId("sendBtn").disabled = !!this.pendingSends?.has(controlState.row?.id);
    byId("writeSwitch").setAttribute("aria-checked", String(controlState.write));
    byId("writeBtn").classList.toggle("on", controlState.write);
    byId("sidebarMiniWrite").classList.toggle("on", controlState.write);
    byId("writeInfo").hidden = controlState.paired;
    byId("composeFab").classList.toggle("show", controlState.write && available && !byId("composer").classList.contains("production-show"));
    // #s298: the button says when the session waits on an answer the box can give.
    byId("composeFab").classList.toggle("asks", !!row?.dialog && canInject(row));
    byId("composeFab").title = row?.dialog && canInject(row) ? "Claude is asking: answer it" : "Open the compose box";
    byId("writeNotice").classList.toggle("show", controlState.write && !available);
    if (!available) {
      byId("composer").classList.remove("production-show");
      controlState.row = null;
    }
    this.paintImages();
    this.paintDialog();
  }
  // The open box's dialog, for its row as the index now has it (a live pane only).
  paintDialog() {
    const open = byId("composer").classList.contains("production-show") && controlState.mode === "tmux";
    const row = open && selectedRow()?.id === controlState.row?.id ? selectedRow() : null;
    this.dialog.sync(row);
  }
  open() {
    const row = selectedRow();
    if (!controlState.write || !canCompose(row)) return;
    controlState.row = row;
    controlState.mode = canInject(row) ? "tmux" : "resume";
    controlState.consented = !!row.consented;
    byId("composerTitle").textContent = row.name || row.id;
    byId("composeTarget").textContent = composeTargetLine(row, controlState.mode);
    byId("composer").classList.add("production-show");
    byId("composer").classList.toggle("live", controlState.mode === "tmux");
    this.revokeButton.hidden = !(controlState.mode === "tmux" && controlState.consented);
    byId("composeFab").classList.remove("show");
    this.paintImages();
    this.paintDialog();
    // On a phone a dialog's buttons come first: the keyboard waits until the field is tapped.
    if (!(row.dialog && matchMedia("(pointer: coarse)").matches)) byId("composeInput").focus();
  }
  close() {
    byId("composer").classList.remove("production-show");
    controlState.row = null;
    this.dialog.clear();
    this.paintImages();
    this.paintSoon();
  }
  paintSoon() { queueMicrotask(() => this.paint()); }
  attempt() {
    const text = byId("composeInput").value.trim();
    const images = this.imagesAllowed() ? this.imagesFor() : [];
    if ((!text && !images.length) || !controlState.row) return;
    // #s298: while a dialog holds the session the field answers it, where it takes a written
    // answer; a message would be refused until the dialog is answered.
    if (this.dialog.active) {
      if (!this.dialog.freeText || images.length) { this.actions.toast("Answer the dialog above first: the session takes no message until then."); return; }
      this.answerText(text);
      return;
    }
    if (images.some(item => !item.url)) { this.actions.toast("Wait for the images to finish loading."); return; }
    controlState.pending = text;
    if (controlState.mode === "tmux" && !controlState.consented) {
      // #s226: a herdr pane is named as one; a tmux pane as one too (#s236).
      const where = controlState.row.term === "herdr" ? `herdr pane ${controlState.row.target}` : controlState.row.target ? `tmux pane ${controlState.row.target}` : "its tmux pane";
      byId("confirmText").textContent = `The message goes to ${where} and runs with that agent's current permissions.`;
      // This confirm grants and SENDS: a dialog's grant that was left open does not stand in.
      this.afterGrant = null;
      byId("confirmLayer").classList.add("production-open");
      return;
    }
    this.send(text);
  }
  closeConfirm() { byId("confirmLayer").classList.remove("production-open"); this.afterGrant = null; }
  async grantAndSend() {
    const passcode = document.querySelector(".passcode-input")?.value || "";
    const then = this.afterGrant;
    this.closeConfirm();
    if (then) { await this.grant(passcode, then); return; }
    await this.send(controlState.pending, passcode);
  }
  // #s298: the grant a dialog needs before it is read or answered — the pane's own, the one a
  // send asks for, stated as such.
  askGrant(then) {
    if (!controlState.row) return;
    const where = controlState.row.target ? `herdr pane ${controlState.row.target}` : "its herdr pane";
    byId("confirmText").textContent = `The monitor reads the dialog in ${where} and types your answers there, with that agent's current permissions.`;
    byId("confirmLayer").classList.add("production-open");
    this.afterGrant = then;
  }
  async grant(passcode, then) {
    if (!controlState.row) return;
    let outcome;
    try { outcome = grantOutcome(await post(consentQuery(controlState.row.id), passcode || "")); } catch (_) { outcome = { kind: "error", message: "could not reach the monitor" }; }
    if (outcome.kind === "granted") { controlState.consented = true; this.revokeButton.hidden = false; then(); this.actions.refreshIndex(); return; }
    if (["passcode-required", "bad-passcode"].includes(outcome.kind)) { this.askPasscode(outcome); this.afterGrant = then; return; }
    this.actions.toast(outcome.message || "Grant failed");
  }
  async answerText(text) {
    const target = controlState.row?.id;
    if (!(await this.dialog.answerText(text))) return;
    const input = byId("composeInput");
    if (controlState.row?.id === target && input.value.trim() === text) {
      input.value = "";
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }
  async revoke() {
    if (!controlState.row || controlState.mode !== "tmux") return;
    const outcome = await runRevoke({ target: controlState.row.id, post });
    if (outcome.kind !== "revoked") { this.actions.toast(outcome.kind === "unreachable" ? outcome.message : "Revoking the grant failed"); return; }
    controlState.consented = false;
    this.revokeButton.hidden = true;
    this.actions.toast("Pane grant revoked");
    this.actions.refreshIndex();
  }
  // The passcode dialog: the grant asked for one (or refused the one given).
  askPasscode(outcome) {
    const dialog = byId("confirmLayer").querySelector(".dialog");
    let input = dialog.querySelector(".passcode-input");
    if (!input) { input = document.createElement("input"); input.className = "passcode-input"; input.type = "password"; input.placeholder = "Monitor passcode"; dialog.insertBefore(input, dialog.querySelector(".dialog-actions")); }
    byId("confirmText").textContent = outcome.kind === "bad-passcode" ? "Passcode incorrect — try again." : "This monitor requires a passcode before granting write access.";
    byId("confirmLayer").classList.add("production-open"); input.focus();
  }
  async send(text, passcode = "") {
    const target = controlState.row.id, mode = controlState.mode, consented = controlState.consented;
    this.pendingSends ||= new Map();
    if (this.pendingSends.has(target)) return;
    const pending = {};
    this.pendingSends.set(target, pending);
    const submittedDraft = byId("composeInput").value;
    const images = this.imagesAllowed() ? this.imagesFor(target).filter(item => item.url) : [];
    const owner = this.actions.captureComposer?.();
    // The ordinary editor keeps its target when the reader selects another session.
    // A closed editor has no row; native mode supplies its own generation fence.
    const ownsComposer = () => owner ? owner.isCurrent()
      : controlState.row?.id === target || !controlState.row;
    byId("sendBtn").disabled = true;
    try {
      const result = await runSend({ target, mode, consented, prompt: text, images: images.map(item => item.url), passcode, post });
      if (ownsComposer()) controlState.consented = result.consented;
      if (result.step === "grant") {
        if (["passcode-required", "bad-passcode"].includes(result.outcome.kind) && ownsComposer()) { this.askPasscode(result.outcome); return; }
        this.actions.toast(result.outcome.kind === "unreachable" ? result.outcome.message : (result.outcome.message || "Grant failed"));
        return;
      }
      if (result.outcome.kind !== "sent") { this.actions.toast(result.outcome.kind === "no-consent" ? "This pane needs consent again" : result.outcome.message); return; }
      const submittedMatches = submittedDraft.trim() === text;
      const retireVisibleDraft = submittedMatches && ownsComposer() && byId("composeInput").value === submittedDraft;
      if (submittedMatches) owner?.accepted();
      // #s237: the images sent leave the strip; any added while the send was out stay.
      if (images.length) for (const item of images) this.removeImage(target, item.id);
      // #s236 (Hong): the box stays open after a send, its field emptied for the next message;
      // only ✕ closes it. The field's height follows its (now empty) text, as typing would. A
      // reader who moved to another session while the send was out gets the box closed, as
      // before: left open it would still write to the session they left, under the new one.
      if (retireVisibleDraft) {
        const input = byId("composeInput");
        input.value = "";
        input.dispatchEvent(new Event("input", { bubbles: true }));
        if (controlState.row && selectedRow()?.id !== controlState.row.id) this.close();
        else if (controlState.row && !matchMedia("(pointer: coarse)").matches && document.activeElement !== input) input.focus();
      }
      this.actions.toast(mode === "tmux" ? (images.length ? `Sent to the pane with ${images.length === 1 ? "an image" : `${images.length} images`}` : "Sent to the pane") : "Resuming the session");
      this.actions.refreshIndex();
    } finally {
      if (this.pendingSends.get(target) === pending) this.pendingSends.delete(target);
      if (ownsComposer()) byId("sendBtn").disabled = false;
      else this.actions.repaintComposer?.();
    }
  }
}
