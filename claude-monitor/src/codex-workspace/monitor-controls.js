import { GatewayApi, GatewayError, createActionId, readAllPages, WorkspaceRecovery, applyWorkspaceEvent, preserveUncertainActionError } from "./api.js";
import { activeTurn, applySnapshot, effectiveCapabilities, escapeText, freshWorkspace, modelOptions, reduceEvent, selectedCapabilities, textOfInput } from "./model.js";
import { requestHtml, requestResponsePayload, questionResponse, permissionResponse } from "./requests.js";
import { ImageAttachmentDrafts, IMAGE_TYPES } from "./attachments.js";
import { monitorSessionId, nativeSessionGroups, nativeSessionTree, nativeProjectTree } from "./monitor-model.js";
import { OutgoingMessages } from "./outgoing.js";
import { DeliveryJournal } from "./delivery-journal.js";
import { ProjectPicker } from "./project-picker.js";
import { SessionConflict, isThreadInUse } from "./session-conflict.js";
import { PHONE_QUERY } from "/monitor-ui/shared/capabilities.js";
import { controlState, indexState, selectedRow } from "/monitor-ui/state.js";

const byId = id => document.getElementById(id);
const make = (tag, className, text) => { const node = document.createElement(tag); node.className = className; if (text) node.textContent = text; return node; };
const FAST_ICON = '<svg class="icon shared-fast-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="m13.5 3-8 10h6l-1 8 8-10h-6l1-8Z"/></svg>';
const effortLabel = value => ({ xhigh: "Extra High", max: "Maximum", ultra: "Ultra" })[value] || value.replace(/^./, letter => letter.toUpperCase());

/**
 * Extends the original Monitor shell. JSONL RecordStore and Viewport continue to own reading.
 * @typedef {{threadId:string, text:string, images?:string[], intent:"next"|"steer", expectedTurnId?:string}} SendPayload
 */
export class MonitorControls {
  constructor(actions) {
    this.active = true; this.actions = actions; this.api = new GatewayApi(); this.state = freshWorkspace(); this.projects = []; this.models = [];
    this.browserAccess = "paired"; this.catalogEpoch = null; this.observedEpoch = null; this.connectionEpoch = null; this.latestError = null;
    this.initialThread = new URL(location.href).searchParams.get("thread");
    this.threads = []; this.drafts = new Map(); this.draftRevisions = new Map(); this.imageErrors = new Map(); this.uncertainSendPayloads = new Map(); this.sendDrafts = new Map(); this.requestDrafts = new Map(); this.uncertainActions = new Map(); this.selected = null; this.generation = 0; this.pending = false; this.creating = false; this.archived = false; this.projectId = "";
    this.composerExpanded = false;
    this.outgoing = new OutgoingMessages(); this.journalRequired = true;
    this.attachments = new ImageAttachmentDrafts(undefined, thread => { this.touchDraft(thread); void this.persistDraft(thread).catch(() => {}); if (thread === this.selected) this.paint(); });
    this.bind();
    this.open();
    new ResizeObserver(entries => {
      if (this.active === false) return;
      this.reserveComposerSpace(entries[0].target.getBoundingClientRect().height);
    }).observe(byId("composer"));
    const workspace = byId("composer").closest(".workspace"), transcript = byId("transcript");
    new ResizeObserver(() => {
      if (this.active === false) return;
      const view = transcript.getBoundingClientRect(), parent = workspace.getBoundingClientRect();
      workspace.style.setProperty("--shared-composer-left", `${view.left - parent.left + view.width / 2}px`);
      workspace.style.setProperty("--shared-composer-width", `${Math.min(760, Math.max(0, view.width - 32))}px`);
    }).observe(transcript);

    this.recovery = new WorkspaceRecovery({ loadStatus: async () => {
      const status = await this.api.read("status");
      await this.restoreJournal(status);
      this.sessionConflict.setConnection(status);
      this.browserAccess = "paired"; this.observedEpoch = status.epoch;
      if (this.catalogEpoch !== status.epoch) { this.models = []; this.catalogEpoch = null; }
      this.state.connection = status.connection; this.roots = status.allowedRoots;
      this.paint(); return status;
    }, loadCatalog: async () => {
      const epoch = this.observedEpoch; this.catalogEpoch = null; this.models = []; this.paint();
      const [catalog, projects] = await Promise.all([readAllPages(this.api, "models"), readAllPages(this.api, "projects")]);
      if (epoch !== this.observedEpoch) throw new GatewayError("The model catalog changed during reconnect; reload its current state.", "unavailable", null, undefined, true);
      this.catalogEpoch = epoch; this.models = modelOptions(catalog); this.projects = projects; this.paintProjects(); this.paint(); this.actions.renderTree?.();
    }, openThread: id => this.openThread(id), onError: error => this.report(error), onRecovered: () => { if (this.state.lastError) this.report(new Error(this.state.lastError)); else if (this.latestError?.code !== "uncertain") this.report(null); this.paint(); } });
    this.recovery.recover(); this.connectEvents();
    addEventListener("pagehide", () => this.suspendEvents());
    addEventListener("pageshow", event => { if (event.persisted) this.resumeEvents(); });
    addEventListener("online", () => this.resumeEvents());
    addEventListener("offline", () => this.suspendEvents());
    document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") this.resumeEvents(); else this.suspendEvents(); });
  }
  bind() {
    byId("writeNotice").hidden = true; byId("composeFab").hidden = true;
    byId("sidebarMiniWrite").onclick = () => this.newSession();
    const nav = document.querySelector(".nav"), newButton = make("button", "navbtn shared-new", "+  New session");
    newButton.type = "button"; newButton.id = "sharedNativeNewSession"; newButton.onclick = () => this.newSession();
    if (!this.actions.nativeManaged) nav.prepend(newButton);
    this.projectPicker = new ProjectPicker({ id: "sharedProjectFilter", label: "Filter history by project", onChange: id => { this.projectId = id; this.actions.refreshIndex(); } });
    this.projectFilter = this.projectPicker.button; this.projectFilter.classList.add("shared-project-filter"); this.paintProjects(); nav.append(this.projectPicker.root);
    this.archiveButton = make("button", "navbtn", "Show archived sessions"); this.archiveButton.id = "sharedArchiveHistory"; this.archiveButton.onclick = () => { this.archived = !this.archived; this.archiveButton.textContent = this.archived ? "Show active sessions" : "Show archived sessions"; this.actions.refreshIndex(); }; nav.append(this.archiveButton);
    this.status = make("span", "shared-status", "Connecting to Codex…"); nav.append(this.status);
    this.error = make("div", "shared-error"); this.error.id = "sharedError"; this.error.hidden = true; this.error.setAttribute("role", "alert"); byId("composer").prepend(this.error);
    const foot = byId("composer").querySelector(".compose-foot");
    this.pickButton = make("button", "shared-model-pill", "Model · Reasoning"); this.pickButton.id = "sharedModelPicker"; this.pickButton.type = "button";
    this.modelName = make("span", "shared-model-name"); this.modelEffort = make("span", "shared-model-effort");
    this.pickButton.replaceChildren(this.modelName, make("span", "shared-model-separator", " · "), this.modelEffort);
    this.pickButton.setAttribute("aria-expanded", "false"); this.pickButton.setAttribute("aria-haspopup", "dialog");
    this.pickButton.onclick = () => this.dialog.matches(":popover-open") ? this.closeSettings() : this.openSettings(); foot.prepend(this.pickButton);
    this.attachButton = make("button", "shared-attach", "+"); this.attachButton.id = "sharedAttachImage"; this.attachButton.type = "button"; this.attachButton.setAttribute("aria-label", "Attach images"); this.attachButton.title = "Attach images (or paste them)"; foot.prepend(this.attachButton);
    this.fileInput = make("input", ""); this.fileInput.id = "sharedImageInput"; this.fileInput.type = "file"; this.fileInput.accept = IMAGE_TYPES.join(","); this.fileInput.multiple = true; this.fileInput.hidden = true; foot.append(this.fileInput);
    this.attachButton.onclick = () => this.fileInput.click();
    this.fileInput.onchange = () => { const files = [...this.fileInput.files]; this.fileInput.value = ""; this.addImages(files); };
    this.imageStrip = make("div", "shared-images"); this.imageStrip.id = "sharedImages";
    this.composeBody = make("div", "shared-compose-body");
    this.blockedStatus = make("div", "shared-control-status"); this.blockedStatus.id = "sharedControlStatus"; this.blockedStatus.hidden = true; this.blockedStatus.setAttribute("role", "status");
    this.blockedMessage = make("span", ""); this.reconnectButton = make("button", "", "Reconnect"); this.reconnectButton.type = "button";
    this.reconnectButton.onclick = () => this.resumeEvents(); this.blockedStatus.append(this.blockedMessage, this.reconnectButton);
    this.composeBody.append(this.error, this.blockedStatus, this.imageStrip, byId("composeInput")); byId("composer").prepend(this.composeBody);
    this.sessionConflict = new SessionConflict({ retry: () => this.retryConnection(), newSession: () => this.newSession() });
    this.composeBody.prepend(this.sessionConflict.root);
    this.fastButton = make("button", "shared-fast-pill"); this.fastButton.innerHTML = FAST_ICON; this.fastButton.id = "sharedFastMode"; this.fastButton.type = "button"; this.fastButton.setAttribute("aria-label", "Fast mode");
    this.fastButton.onclick = () => { const model = this.currentModel(); const fast = model?.tiers.find(tier => tier.name.toLowerCase() === "fast"); if (fast) this.performSettings({ ...this.state.settings, serviceTier: this.state.settings.serviceTier === fast.id ? null : fast.id }); }; this.pickButton.after(this.fastButton);
    this.stopButton = make("button", "shared-stop", "Stop"); this.stopButton.id = "sharedInterrupt"; this.stopButton.setAttribute("aria-label", "Stop current turn"); this.stopButton.innerHTML = '<svg class="shared-stop-icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="6" width="12" height="12" rx="2"/></svg><span>Stop</span>'; this.stopButton.onclick = () => this.run(() => this.perform("interrupt", { threadId: this.selected, turnId: activeTurn(this.state.thread)?.id })); foot.insertBefore(this.stopButton, byId("sendBtn"));
    this.steerButton = make("button", "shared-steer", "Steer"); this.steerButton.id = "sharedSteer"; this.steerButton.onclick = () => this.send("steer"); foot.insertBefore(this.steerButton, byId("sendBtn"));
    this.archiveSessionButton = make("button", "shared-stop", "Archive"); this.archiveSessionButton.id = "sharedArchiveSession";
    this.archiveSessionButton.onclick = () => this.run(async () => {
      const id = this.selected, archived = this.state.accessMode !== "archived";
      if (this.archiveSessionButton.disabled) return;
      await this.perform("archive", { threadId: id, archived });
      this.archived = archived; this.archiveButton.textContent = archived ? "Show active sessions" : "Show archived sessions";
      await this.actions.refreshIndex();
      if (this.selected === id) await this.recovery.recover({ thread: true });
    }); foot.insertBefore(this.archiveSessionButton, byId("sendBtn"));
    this.runtimePanel = make("section", "shared-runtime"); byId("composer").before(this.runtimePanel);
    this.outgoingRoot = make("section", "shared-outgoing"); this.outgoingRoot.id = "sharedOutgoing"; this.outgoingRoot.setAttribute("aria-label", "Outgoing messages"); this.runtimePanel.append(this.outgoingRoot);
    this.outgoingRoot.onclick = event => { const check = event.target.closest("[data-check-outgoing]"); if (check) { this.run(() => this.checkOutgoing(check.dataset.checkOutgoing)); return; } const button = event.target.closest("[data-dismiss-outgoing]"); if (button) { this.run(() => this.reviewOutgoing(button.dataset.dismissOutgoing)); } };
    this.requests = make("section", "shared-requests"); this.requests.id = "sharedRequests"; this.runtimePanel.append(this.requests);
    this.queue = make("section", "shared-queue"); this.queue.id = "sharedQueue"; this.runtimePanel.append(this.queue);
    this.queue.onclick = event => {
      const button = event.target.closest("[data-queue-id]");
      if (button) this.run(() => this.controlQueued(button.dataset.queueAction, button.dataset.queueId));
    };
    byId("closeComposer").hidden = true;
    byId("closeComposer").onclick = () => this.open();
    byId("composeFab").onclick = () => this.open();
    byId("sendBtn").onclick = () => this.send("next");
    byId("composeInput").placeholder = "Ask Codex…";
    byId("composeInput").rows = 1;
    byId("composeInput").onpaste = event => {
      const files = [...(event.clipboardData?.items || [])].filter(item => item.kind === "file" && item.type.startsWith("image/")).map(item => item.getAsFile()).filter(Boolean);
      if (!files.length) return;
      event.preventDefault();
      const text = event.clipboardData.getData("text/plain"), input = byId("composeInput");
      if (text) { input.setRangeText(text, input.selectionStart, input.selectionEnd, "end"); input.dispatchEvent(new Event("input", { bubbles: true })); }
      this.addImages(files);
    };
    byId("composeInput").oninput = () => { if (this.selected) { this.drafts.set(this.selected, byId("composeInput").value); this.touchDraft(this.selected); void this.persistDraft(this.selected).catch(() => {}); } this.paint(); };
    byId("composeInput").onkeydown = event => { if (event.key === "Enter" && !event.shiftKey && (event.ctrlKey || event.metaKey)) { event.preventDefault(); this.send("next"); } };
    this.requests.onclick = event => { const button = event.target.closest("[data-decision]"); if (!button) return; const card = button.closest("[data-request-index]"), request = this.state.pending[Number(card.dataset.requestIndex)]; if (request?.requestToken !== card.dataset.requestToken) return; this.answer(request, request.method === "item/permissions/requestApproval" ? permissionResponse(request, button.dataset.decision === "accept") : { decision: button.dataset.decision }); };
    this.requests.onsubmit = event => { event.preventDefault(); const card = event.target.closest("[data-request-index]"), request = this.state.pending[Number(card?.dataset.requestIndex)]; if (!request || request.requestToken !== card.dataset.requestToken) return;
      this.run(async () => { const answers = {}; for (const input of card.querySelectorAll("[data-question-id]")) if (input.type === "radio" ? input.checked : input.value.trim()) answers[input.dataset.questionId] = input.value; await this.answer(request, questionResponse(request, answers)); }); };
    this.dialog = make("div", "shared-settings-popover"); this.dialog.id = "sharedSettingsDialog"; this.dialog.setAttribute("popover", "auto"); this.dialog.setAttribute("role", "dialog"); this.dialog.setAttribute("aria-label", "Session model and reasoning"); document.body.append(this.dialog);
    this.dialog.addEventListener("toggle", event => this.pickButton.setAttribute("aria-expanded", String(event.newState === "open")));
    this.dialog.addEventListener("keydown", event => { if (event.key === "Escape") { event.preventDefault(); this.closeSettings(); } });
    this.readerButton = make("button", "iconbtn shared-reader-toggle"); this.readerButton.id = "sharedReaderTools"; this.readerButton.type = "button";
    this.readerButton.innerHTML = '<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h10M18 7h2M4 17h2M10 17h10"/><circle cx="16" cy="7" r="2"/><circle cx="8" cy="17" r="2"/></svg>';
    this.readerButton.setAttribute("aria-label", "Reader tools"); this.readerButton.setAttribute("aria-controls", "phoneDock"); this.readerButton.setAttribute("aria-expanded", "false");
    this.readerButton.onclick = () => this.setReaderTools(!document.body.hasAttribute("data-shared-reader-open"));
    byId("previewBtn").before(this.readerButton);
    byId("composeInput").addEventListener("focus", () => { if (this.active !== false) { this.setReaderTools(false); this.setComposerExpanded(true); } });
    byId("composer").addEventListener("click", () => this.setComposerExpanded(true));
    document.addEventListener("click", event => {
      // A scrolling touch must not change transcript padding at pointerdown.
      // Desktop clicks and an already folded editor need no layout work.
      if (this.active !== false && this.composerExpanded && this.sidewaysComposer()
        && !byId("composer").contains(event.target) && !this.dialog.contains(event.target)) this.setComposerExpanded(false);
    });
    matchMedia("(orientation:landscape)").addEventListener("change", () => this.composerOrientationChanged());
    const reposition = () => { if (this.active === false) return; if (!matchMedia(PHONE_QUERY).matches) this.setReaderTools(false); this.fitComposer(); if (this.dialog.matches(":popover-open")) this.positionSettings(); };
    window.addEventListener("resize", reposition); window.visualViewport?.addEventListener("resize", reposition); window.visualViewport?.addEventListener("scroll", reposition);
    this.newDialog = make("dialog", "shared-dialog shared-new-dialog"); this.newDialog.id = "sharedNewDialog"; this.newDialog.setAttribute("aria-label", "New Codex session"); document.body.append(this.newDialog); this.newDialog.oncancel = event => { if (this.creating) event.preventDefault(); }; this.newDialog.addEventListener("close", () => { this.newProjectPicker?.destroy(); this.actions.nativeNewClosed?.(); });
  }
  /** Release the native view without terminating the shared server or an accepted action. */
  deactivate() {
    this.active = false; cancelAnimationFrame(this.composeFrame);
    delete document.body.dataset.sharedPending;
    byId("composer").removeAttribute("data-shared-collapsed");
    byId("composer").removeAttribute("data-shared-input-only");
    if (this.selected) { this.drafts.set(this.selected, byId("composeInput").value); this.recovery.forget(this.selected); }
    this.clearSessionConflict();
    this.selected = null; this.generation++;
    this.closeSettings(false); this.sessionConflict.close(); this.suspendEvents();
  }
  currentModel() { return this.models.find(model => model.id === this.state.settings.model || model.model === this.state.settings.model); }
  canControl() { return !this.closing && this.state.thread?.id === this.selected && !!this.selected && !isThreadInUse(this.latestError, this.selected) && this.state.connection === "connected" && !["pairing-required", "forbidden"].includes(this.browserAccess) && !this.state.needsSnapshot && this.state.accessMode !== "archived" && !this.pending && !this.creating; }
  ready() { return (!this.journalRequired || !!this.journal) && this.canControl() && this.catalogEpoch === this.state.epoch && effectiveCapabilities(this.models, this.state.settings).ready; }
  projectChoices(emptyLabel) { return [{ id: "", label: emptyLabel }, ...this.projects.map(project => ({ id: project.id, label: project.name, detail: project.roots?.[0]?.path }))]; }
  paintProjects() { this.projectPicker.setOptions(this.projectChoices("All projects"), this.projectId); this.projectId = this.projectPicker.value; }
  paint() {
    if (this.active === false) return;
    const row = selectedRow();
    // Original Monitor can open a child absent from the index. Never retain its parent as a write target.
    if (!row?.nativeThreadId && this.selected) {
      this.clearSessionConflict();
      this.drafts.set(this.selected, byId("composeInput").value);
      this.recovery?.forget(this.selected); this.selected = null; this.generation++;
      this.state = { ...freshWorkspace(), connection: this.state.connection };
      byId("composeInput").value = "";
      this.closeSettings(false);
    }
    if (row?.nativeThreadId && row.nativeThreadId !== this.selected) {
      this.clearSessionConflict();
      if (this.selected) this.drafts.set(this.selected, byId("composeInput").value);
      this.closeSettings(false);
      this.selected = row.nativeThreadId;
      const url = new URL(location.href); url.searchParams.set("thread", this.selected); history.replaceState(null, "", url);
      this.state = { ...freshWorkspace(), connection: this.state.connection }; this.generation++;
      byId("composeInput").value = this.drafts.get(this.selected) || "";
      this.recovery?.select(this.selected); this.open();
      if (this.imageErrors?.has(this.selected)) this.report(this.imageErrors.get(this.selected));
    }
    const capabilities = effectiveCapabilities(this.models, this.state.settings), model = this.currentModel();
    this.status.textContent = this.browserAccess === "pairing-required" ? "Pair this browser to reconnect" : this.state.connection === "connected" ? "Connected · shared Codex server" : "Reconnecting to Codex…";
    this.modelName.textContent = model?.label || this.state.settings.model || "Choose model";
    this.modelEffort.textContent = capabilities.effort ? effortLabel(capabilities.effort) : "Reasoning";
    this.pickButton.setAttribute("aria-label", `${this.modelName.textContent} · ${this.modelEffort.textContent}`);
    this.pickButton.disabled = ["pairing-required", "forbidden"].includes(this.browserAccess) || this.state.needsSnapshot || this.state.thread?.id !== this.selected || this.catalogEpoch !== this.state.epoch || !this.selected || this.pending || this.creating || this.state.connection !== "connected";
    const fast = model?.tiers.find(tier => tier.name.toLowerCase() === "fast");
    this.fastButton.disabled = !this.ready() || !fast; this.fastButton.title = fast?.description || "Fast mode is unavailable for this model";
    this.fastButton.setAttribute("aria-pressed", String(!!fast && this.state.settings.serviceTier === fast.id));
    this.fastButton.classList.toggle("on", !!fast && this.state.settings.serviceTier === fast.id);
    const turn = activeTurn(this.state.thread);
    byId("composeInput").disabled = !this.selected || this.state.accessMode === "archived";
    const images = this.attachments.items(this.selected);
    const retry = this.uncertainSend(this.selected, byId("composeInput").value.trim(), images);
    byId("sendBtn").disabled = !(retry ? this.canControl() : this.ready()) || images.some(image => image.status === "reading") || (!byId("composeInput").value.trim() && !images.length);
    this.attachButton.disabled = !this.selected || this.state.accessMode === "archived";
    this.paintImages(images);
    byId("sendBtn").title = retry ? "Retry the original send without executing it twice" : turn ? "Queue for the next turn" : "Send message (⌘/Ctrl + Enter)";
    this.stopButton.hidden = !turn; this.stopButton.disabled = !this.canControl(); this.steerButton.hidden = !turn; this.steerButton.disabled = byId("sendBtn").disabled;
    this.archiveSessionButton.hidden = !this.selected || !!turn;
    this.archiveSessionButton.textContent = this.state.accessMode === "archived" ? "Restore" : "Archive";
    this.archiveSessionButton.disabled = !this.selected || !this.state.thread || this.state.connection !== "connected" || this.state.needsSnapshot || ["pairing-required", "forbidden"].includes(this.browserAccess) || this.pending || this.creating || !!turn;
    const pickerSteer = byId("sharedPickerSteer");
    if (pickerSteer) { pickerSteer.hidden = !turn; pickerSteer.disabled = this.steerButton.disabled; }
    const pickerArchive = byId("sharedPickerArchive");
    if (pickerArchive) { pickerArchive.hidden = this.archiveSessionButton.hidden; pickerArchive.disabled = this.archiveSessionButton.disabled; pickerArchive.textContent = this.state.accessMode === "archived" ? "Restore conversation" : "Archive conversation"; }
    byId("composerTitle").textContent = row?.name || "New session";
    byId("composeTarget").textContent = this.state.accessMode === "archived" ? "Archived · read only" : this.state.settings.cwd || row?.cwd || "Choose a session";
    byId("composeInput").setAttribute("aria-label", `Message ${row?.name || this.state.thread?.name || "Codex"} in ${this.state.settings.cwd || row?.cwd || "the selected project"}`);
    if (this.pickButton.disabled) this.closeSettings(false);
    this.paintOutgoing();
    this.paintBlockingState(); this.paintPending(); this.fitComposer();
  }
  /** Explain native state blockers without removing archive, connection or capability guards. */
  paintBlockingState() {
    const occupied = this.sessionConflict?.render({ error: this.latestError, threadId: this.selected, checking: !!this.checkingConnection,
      hasDraft: !!byId("composeInput").value || !!this.attachments?.items(this.selected).length });
    byId("composer").setAttribute("data-session-conflict", String(!!occupied));
    if (!this.blockedStatus) return;
    if (occupied) { this.blockedStatus.hidden = true; return; }
    let message = "", reconnect = false;
    if (this.pending || this.creating) message = "Waiting for the current action…";
    else if (this.selected && this.state.accessMode === "archived") message = "Archived conversation. Restore it from model settings to continue.";
    else if (this.browserAccess === "pairing-required" || this.latestError?.code === "pairing_required") message = "Pair this browser again to reconnect.";
    else if (this.selected && (this.state.connection !== "connected" || this.state.thread?.id !== this.selected || this.state.needsSnapshot || this.catalogEpoch !== this.state.epoch)) {
      message = this.latestError?.message || "Reconnecting this conversation…"; reconnect = true;
    } else if (this.selected) message = effectiveCapabilities(this.models, this.state.settings).reason;
    this.blockedStatus.hidden = !message; this.blockedMessage.textContent = message;
    this.reconnectButton.hidden = !reconnect; this.reconnectButton.disabled = this.pending || this.creating;
    this.pickButton.title = message || "Choose model and reasoning effort";
    if (message) byId("sendBtn").title = message;
  }
  /** Toggle the original phone reader dock without duplicating its search or pane controls. */
  setReaderTools(open) {
    document.body.toggleAttribute("data-shared-reader-open", open && matchMedia(PHONE_QUERY).matches);
    this.readerButton.setAttribute("aria-expanded", String(document.body.hasAttribute("data-shared-reader-open")));
    this.fitComposer();
  }
  reserveComposerSpace(height) {
    document.body.style.setProperty("--shared-composer-height", `${height}px`);
    const extra = matchMedia(PHONE_QUERY).matches ? document.body.hasAttribute("data-shared-reader-open") && !byId("app").classList.contains("composing") ? 100 : 24 : 30;
    byId("transcript").style.paddingBottom = `${height + extra}px`;
  }
  open() { byId("composer").classList.add("production-show"); byId("composer").classList.remove("live"); byId("composeFab").classList.remove("show"); byId("composeFab").hidden = true; }
  setComposerExpanded(expanded) {
    if (this.active === false) return;
    this.composerExpanded = expanded;
    this.fitComposer();
  }
  // Match the shell's short coarse-pointer viewport, never visualViewport height:
  // a portrait keyboard must not enable the sideways one-line variant.
  sidewaysComposer() { return matchMedia("(pointer:coarse) and (max-height:500px)").matches; }
  composerOrientationChanged() {
    if (this.active === false) return;
    if (document.activeElement !== byId("composeInput")) this.composerExpanded = false;
    this.fitComposer();
  }
  /**
   * Fit the mounted composer and pending actions above the phone keyboard and below the header.
   * Text grows to 132 px; constrained body/pending content scrolls without losing draft bytes.
   * @returns {void}
   */
  fitComposer() {
    if (this.active === false) return;
    const input = byId("composeInput"), composer = byId("composer");
    input.style.height = "auto"; composer.style.maxHeight = ""; composer.setAttribute("data-shared-constrained", "false");
    composer.setAttribute("data-shared-input-only", "false");
    composer.setAttribute("data-shared-header-covered", "false");
    if (this.runtimePanel) this.runtimePanel.style.maxHeight = "";
    const collapsed = this.sidewaysComposer() && !this.composerExpanded;
    composer.setAttribute("data-shared-collapsed", String(collapsed));
    let limit = collapsed ? 24 : 132;
    if (matchMedia(PHONE_QUERY).matches) {
      const view = window.visualViewport, style = getComputedStyle(composer);
      const safe = edge => parseFloat(style.getPropertyValue(`--shared-safe-${edge}`)) || 0;
      // Safari scrolls the page as the keyboard opens. The shell already moves
      // the app over that scroll, so every edge must use the same app box.
      const app = byId("app").getBoundingClientRect();
      const top = app.top + safe("top");
      const bottom = app.top + Math.min(app.height, view?.height || app.height) - safe("bottom") - 8;
      const box = composer.getBoundingClientRect(), field = input.getBoundingClientRect();
      const header = document.querySelector(".workspace>.topbar")?.getBoundingClientRect().bottom || top;
      let available = Math.max(0, Math.min(box.bottom, bottom) - Math.max(top, header) - 4);
      if (available < 40) {
        // Safari can leave less than one input row. Clip to its visible bottom
        // instead of letting flex children overflow above the app/header.
        const height = Math.max(0, Math.min(24, view?.height ?? app.height));
        composer.setAttribute("data-shared-input-only", "true");
        composer.setAttribute("data-shared-header-covered", String(app.top + (view?.height ?? app.height) - height < header));
        composer.style.setProperty("--shared-input-only-height", `${height}px`);
        composer.style.maxHeight = `${height}px`; input.style.height = `${height}px`;
        this.reserveComposerSpace(height);
        return;
      }
      const pending = this.runtimePanel?.querySelector(".shared-requests:not(:empty), .shared-queue:not(:empty), .shared-outgoing:not(:empty)");
      if (pending) {
        // Reserve the reader dock only while the user explicitly keeps it open.
        const gap = byId("phoneDock") && getComputedStyle(byId("phoneDock")).display !== "none" ? 72 : 8;
        const minimumBody = this.imageStrip.children.length ? 44 : 30;
        const minimum = composer.querySelector(".compose-foot").getBoundingClientRect().height + minimumBody
          + parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) + 4;
        const pendingHeight = Math.max(0, Math.min(innerHeight * .32, available - gap - minimum));
        this.runtimePanel.style.maxHeight = `${pendingHeight}px`;
        available = Math.max(0, available - gap - pendingHeight);
      }
      const chrome = box.height - field.height;
      composer.setAttribute("data-shared-constrained", String(available < chrome + 30));
      limit = Math.max(parseFloat(getComputedStyle(input).minHeight) || 30, Math.min(limit, available - chrome));
      composer.style.maxHeight = `${available}px`;
    }
    input.style.height = `${Math.min(input.scrollHeight, limit)}px`;
    if (this.runtimePanel) this.reserveComposerSpace(composer.getBoundingClientRect().height);
  }
  paintImages(images) {
    const key = JSON.stringify(images.map(image => [image.id, image.status]));
    if (key === this.imagesKey) return; this.imagesKey = key;
    this.imageStrip.replaceChildren();
    for (const image of images) {
      const tile = make("div", "shared-image"); tile.dataset.imageId = image.id; tile.title = image.name;
      if (image.status === "ready") { const preview = make("img", ""); preview.src = image.url; preview.alt = image.name; tile.append(preview); }
      else tile.append(make("span", "shared-image-loading", "Loading…"));
      const remove = make("button", "shared-image-remove", "×"); remove.type = "button"; remove.setAttribute("aria-label", `Remove ${image.name}`); const thread = this.selected; remove.onclick = () => this.attachments.remove(thread, image.id); tile.append(remove); this.imageStrip.append(tile);
    }
  }
  /** @param {string} thread @returns {number} Monotonic edit revision for draft retirement. */
  draftRevision(thread) { return this.draftRevisions?.get(thread) || 0; }
  /** @param {string} thread @returns {void} Record a text or attachment transition. */
  touchDraft(thread) { if (thread) { this.draftRevisions ||= new Map(); this.draftRevisions.set(thread, this.draftRevision(thread) + 1); } }
  /**
   * @param {string|null} threadId @param {string} text @param {{url?:string}[]} images
   * @returns {SendPayload|null} Original send payload for matching uncertain content, preserving its intent/turn.
   */
  uncertainSend(threadId, text, images) {
    // Receipt retries retain the original intent/turn even after that turn ends or a successor starts.
    for (const payload of this.uncertainSendPayloads?.values() || []) {
      if (payload.threadId === threadId && payload.text === text && (payload.images || []).length === images.length && (payload.images || []).every((url, index) => url === images[index].url)) return payload;
    }
    return null;
  }
  /**
   * @param {File[]} files
   * @returns {Promise<void>} Read files for the captured thread; retain/display file-scoped failures on return.
   */
  async addImages(files) {
    const thread = this.selected;
    if (!thread || this.state.accessMode === "archived") { this.report(new Error("Choose an active Codex session before attaching images.")); return; }
    this.imageErrors ||= new Map();
    const previous = this.imageErrors.get(thread); this.imageErrors.delete(thread);
    if (previous && thread === this.selected && this.latestError === previous) this.report(null);
    const errors = await this.attachments.add(thread, files);
    if (errors.length) {
      const error = new AggregateError(errors, errors.map(error => error.message).join("\n")); this.imageErrors.set(thread, error);
      if (thread === this.selected) this.report(error);
    }
  }
  report(error) {
    if (error?.code === "native_not_enabled") { this.actions.nativeDisabled?.(); return; }
    const denied = ["pairing_required", "forbidden"].includes(error?.code);
    if (denied) { this.browserAccess = error.code === "pairing_required" ? "pairing-required" : "forbidden"; this.events?.close(); this.events = null; }
    error = error ? preserveUncertainActionError(this.latestError, error) : null;
    const occupied = isThreadInUse(error, this.selected);
    this.latestError = error; this.error.hidden = !error || occupied;
    this.error.textContent = occupied ? "" : error?.message || ""; this.error.title = this.error.textContent;
    this.paintBlockingState(); this.fitComposer(); if (denied || occupied) this.paint();
    if (error && !occupied) this.actions.toast(error.message);
  }

  clearSessionConflict() {
    this.sessionConflict?.close();
    if (isThreadInUse(this.latestError, this.selected)) { this.latestError = null; this.error.hidden = true; this.error.textContent = ""; }
  }
  showConnectionHelp() { this.sessionConflict.showHelp({ shared: !isThreadInUse(this.latestError, this.selected) }); }
  /** Retry attachment only. The recovery coordinator fences selection changes; drafts stay untouched. */
  async retryConnection() {
    if (this.checkingConnection || !this.selected || this.pending || this.creating) return;
    this.checkingConnection = true; this.paintBlockingState();
    try { await this.recovery.recover({ thread: true }); }
    finally { this.checkingConnection = false; if (this.active !== false) this.paint(); }
  }

  async run(operation) { try { this.report(null); return await operation(); } catch (error) { this.report(error); return null; } }
  async openThread(id) {
    const read = { generation: this.generation, events: [], overflow: false };
    this.opening = read;
    try {
      const result = await this.api.open(id);
      if (this.selected !== id || this.generation !== read.generation) return false;
      let next = applySnapshot(this.state, result.snapshot);
      for (const event of read.events) next = applyWorkspaceEvent(next, event, reduceEvent, (_id, message) => this.report(new Error(message)), message => this.report(new Error(message)));
      if (read.overflow) next.needsSnapshot = true;
      this.state = next; this.paint(); this.actions.refreshEmpty();
      return !next.needsSnapshot;
    } finally {
      // Resume success or rejection supersedes the temporary creation proof for this thread only.
      if (this.opening === read && this.selected === id && this.generation === read.generation
        && this.createdSnapshot?.thread?.id === id) this.createdSnapshot = null;
      if (this.opening === read) this.opening = null;
    }
  }
  async restoreJournal(status) {
    const scope = JSON.stringify([status.endpoint, status.codexHome, status.allowedRoots]);
    if (this.journal?.scope === scope) return;
    if (this.journal) throw new Error("The native runtime changed. Reload before sending to another runtime.");
    const tab = DeliveryJournal.tab(createActionId);
    const journal = new DeliveryJournal(scope, tab), saved = await journal.load();
    for (const [key, draft] of Object.entries(saved.drafts)) {
      const [owner, thread] = JSON.parse(key);
      if (owner !== tab || this.drafts.has(thread)) continue;
      this.drafts.set(thread, draft.text); this.draftRevisions.set(thread, draft.revision);
      this.attachments.threads.set(thread, draft.images);
      this.attachments.nextId = Math.max(this.attachments.nextId, ...draft.images.map(image => image.id));
      if (this.selected === thread) byId("composeInput").value = draft.text;
    }
    for (const entry of saved.attempts) {
      this.outgoing.restore(entry);
      if (entry.status !== "accepted") {
        const key = `send:${JSON.stringify(entry.payload)}`;
        this.uncertainActions.set(key, entry.id); this.uncertainSendPayloads.set(key, entry.payload);
      }
    }
    this.journal = journal;
    for (const entry of this.outgoing.entries.values()) if (entry.status === "accepted") this.retireAcceptedDraft(entry);
  }
  retireAcceptedDraft(entry) {
    if (entry.draftTab !== this.journal?.tab || entry.draftRevision !== this.draftRevision(entry.threadId)) return;
    const text = this.drafts.get(entry.threadId) || "";
    if (text.trim() !== entry.text) return;
    this.drafts.delete(entry.threadId);
    if (this.selected === entry.threadId && byId("composeInput").value.trim() === entry.text) byId("composeInput").value = "";
    this.attachments.accepted(entry.threadId, entry.draftImages || []);
    void this.persistDraft(entry.threadId).catch(() => {});
  }
  persistDraft(thread) {
    if (!this.journal || !thread) return Promise.resolve();
    const draft = { text: this.drafts.get(thread) || "", revision: this.draftRevision(thread),
      images: this.attachments.items(thread).filter(image => image.status === "ready") };
    return this.journal.draft(thread, draft).catch(error => { this.report(error); throw error; });
  }
  async readReceipt(actionId) {
    const response = await this.api.read("receipt", { actionId });
    const result = response.result;
    if (!result) throw new GatewayError("No receipt was found. Review the conversation before retiring this attempt; it has not been resent.", "uncertain");
    if (!result.ok) throw new GatewayError(result.error?.message || "Delivery remains unconfirmed.", result.error?.code === "receipt_expired" ? "uncertain" : result.error?.code || "uncertain", result.receipt);
    return result;
  }
  async perform(route, payload) {
    if (this.pending) throw new Error("Wait for the current action to finish.");
    const key = `${route}:${JSON.stringify(payload)}`;
    let recoveringReceipt = this.uncertainActions.has(key), actionId = this.uncertainActions.get(key) || createActionId();
    const id = this.selected, generation = this.generation;
    this.pending = true;
    try {
      if (route === "send" && this.journal) {
        await this.persistDraft(payload.threadId);
        const snapshot = this.actions.recordSnapshot?.();
        const preview = { session: snapshot?.session, baseline: (snapshot?.records || []).filter(record => record.kind === "user").map(record => JSON.stringify([record.id, record.src, record.ts])),
          canReconcile: !!snapshot && (snapshot.records.length > 0 || this.isEmptySession(snapshot.session)),
          draftTab: this.journal.tab, draftRevision: this.draftRevision(payload.threadId), draftImages: this.attachments.snapshot(payload.threadId) };
        const entry = await this.journal.prepare(actionId, payload, preview);
        actionId = entry.id; recoveringReceipt ||= !!entry.recovered; this.outgoing.restore(entry);
      } else if (route === "send" && this.journalRequired) throw new Error("Wait for delivery recovery before sending.");
      if (route === "send" && this.outgoing) {
        const snapshot = this.actions.recordSnapshot?.();
        this.outgoing.begin(actionId, payload, snapshot && { ...snapshot, ready: snapshot.records.length > 0 || this.isEmptySession(snapshot.session) });
      }
      this.paint();
      // Recovered sends only read receipts. An absent/replaced receipt database cannot replay one.
      const result = route === "send" && this.journal && recoveringReceipt
        ? await this.readReceipt(actionId) : await this.api.action(route, payload, actionId);
      if (route === "send") {
        try { await this.journal?.settle(actionId, "accepted"); }
        catch (cause) { throw new GatewayError("The send was accepted, but local recovery storage failed. Check its receipt before sending again.", "uncertain", null, cause); }
        this.outgoing?.settle(actionId, "accepted");
      }
      this.uncertainActions.delete(key); this.uncertainSendPayloads?.delete(key);
      if (id === this.selected && generation === this.generation && result.snapshot?.thread?.id === id) {
        if (result.snapshot.epoch === this.state.epoch && result.snapshot.sequence >= this.state.sequence) this.state = applySnapshot(this.state, result.snapshot);
        else this.recovery.recover({ thread: true });
      }
      return result;
    } catch (error) {
      if (error.code === "uncertain" || recoveringReceipt && !error.receipt) {
        this.uncertainActions.set(key, actionId);
        if (route === "send") { this.uncertainSendPayloads ||= new Map(); this.uncertainSendPayloads.set(key, payload); }
      } else { this.uncertainActions.delete(key); this.uncertainSendPayloads?.delete(key); }
      if (route === "send") {
        const status = this.uncertainActions.has(key) ? "uncertain" : "rejected";
        this.outgoing?.settle(actionId, status);
        try { await this.journal?.settle(actionId, status); } catch (cause) { this.report(cause); }
      }
      if (id === this.selected && error.code === "uncertain") { this.state.needsSnapshot = true; this.recovery.recover({ thread: true }); }
      throw error;
    } finally { this.pending = false; this.paint(); }
  }
  async checkOutgoing(id) {
    const entry = this.outgoing.entries.get(id);
    if (!entry || this.pending) return;
    const result = await this.readReceipt(id);
    await this.journal?.settle(id, "accepted");
    for (const [key, value] of this.uncertainActions) if (value === id) { this.uncertainActions.delete(key); this.uncertainSendPayloads.delete(key); this.sendDrafts.delete(key); }
    this.retireAcceptedDraft(entry); this.outgoing.settle(id, "accepted"); this.report(null); this.paint();
    return result;
  }
  async reviewOutgoing(id) {
    const entry = this.outgoing.entries.get(id);
    if (!entry) return;
    if (entry.status === "uncertain" && !globalThis.confirm("This message may already have been delivered. Review the conversation first. Retiring this attempt will allow a new send, which could duplicate it. Retire without sending?")) return;
    await this.journal?.retire(id);
    for (const [key, value] of this.uncertainActions) if (value === id) { this.uncertainActions.delete(key); this.uncertainSendPayloads.delete(key); this.sendDrafts.delete(key); }
    this.outgoing.dismiss(id); this.report(null); this.paint();
  }
  send(intent) { return this.run(async () => {
    const threadId = this.selected, text = byId("composeInput").value.trim(), turn = activeTurn(this.state.thread);
    const images = this.attachments.snapshot(threadId), retry = this.uncertainSend(threadId, text, images);
    if (!(retry ? this.canControl() : this.ready())) throw new Error(effectiveCapabilities(this.models, this.state.settings).reason || "Reconnect this session before sending.");
    if (!text && !images.length) return; if (!retry && intent === "steer" && !turn) throw new Error("This turn has finished. Send a new message.");
    const payload = retry || { threadId, text, ...(images.length ? { images: images.map(image => image.url) } : {}), intent, ...(intent === "steer" ? { expectedTurnId: turn.id } : {}) };
    const key = `send:${JSON.stringify(payload)}`;
    this.sendDrafts ||= new Map();
    // A recovered receipt owns the original draft IDs/revision, even if identical bytes were reattached.
    const draft = this.sendDrafts.get(key) || { images, revision: this.draftRevision(threadId) };
    this.sendDrafts.set(key, draft);
    try {
      await this.perform("send", payload);
      const unchanged = this.draftRevision(threadId) === draft.revision;
      this.attachments.accepted(threadId, draft.images);
      if (unchanged && threadId === this.selected && byId("composeInput").value.trim() === text) { byId("composeInput").value = ""; this.drafts.delete(threadId); }
      else if (unchanged && this.drafts.get(threadId)?.trim() === text) this.drafts.delete(threadId);
      if (unchanged && threadId === this.selected && this.sidewaysComposer()) { byId("composeInput").blur(); this.setComposerExpanded(false); }
    } finally {
      if (!this.uncertainActions?.has(key)) this.sendDrafts.delete(key);
    }
    await this.persistDraft(threadId);
    this.actions.refreshIndex(); this.paint();
  }); }
  performSettings(settings) { return this.run(async () => {
    const thread = this.selected, generation = this.generation;
    const selected = selectedCapabilities(this.models, settings.model, settings.effort);
    await this.perform("settings", { threadId: this.selected, model: selected.model, effort: selected.effort, serviceTier: settings.serviceTier === "default" ? null : settings.serviceTier ?? null });
    if (thread === this.selected && generation === this.generation) this.closeSettings();
  }); }
  closeSettings(focus = true) {
    if (!this.dialog?.matches(":popover-open")) return;
    this.dialog.hidePopover(); this.pickButton.setAttribute("aria-expanded", "false");
    if (focus && !this.pickButton.disabled) this.pickButton.focus();
  }
  positionSettings() {
    const box = this.pickButton.getBoundingClientRect(), view = window.visualViewport;
    const style = getComputedStyle(this.dialog), safe = edge => parseFloat(style.getPropertyValue(`--safe-${edge}`)) || 0;
    const left = (view?.offsetLeft || 0) + safe("left") + 8, top = (view?.offsetTop || 0) + safe("top") + 8;
    const right = (view?.offsetLeft || 0) + (view?.width || innerWidth) - safe("right") - 8;
    const bottom = (view?.offsetTop || 0) + (view?.height || innerHeight) - safe("bottom") - 8;
    this.dialog.style.maxWidth = `${right - left}px`; this.dialog.style.maxHeight = `${bottom - top}px`;
    const popup = this.dialog.getBoundingClientRect();
    this.dialog.style.left = `${Math.max(left, Math.min(box.left, right - popup.width))}px`;
    this.dialog.style.top = `${Math.max(top, Math.min(box.top - popup.height - 8, bottom - popup.height))}px`;
  }
  openSettings() {
    if (this.pickButton.disabled) return;
    const selected = selectedCapabilities(this.models, this.state.settings.model, this.state.settings.effort), model = this.models.find(model => model.id === selected.model);
    const fast = model?.tiers.find(tier => tier.name.toLowerCase() === "fast");
    // The editor is bound to this thread/generation; switching sessions cannot apply stale settings.
    const thread = this.selected, generation = this.generation, settings = { ...this.state.settings, model: selected.model, effort: selected.effort };
    const apply = next => { if (thread === this.selected && generation === this.generation && this.state.accessMode !== "archived" && !this.pickButton.disabled) return this.performSettings(next); };
    this.dialog.innerHTML = `<div class="shared-picker-head"><button type="button" id="sharedPickerFast" aria-label="Fast mode" aria-pressed="${!!fast && settings.serviceTier === fast.id}"${fast ? "" : " disabled"}>${FAST_ICON}</button><strong id="sharedEffortLabel">${escapeText(effortLabel(selected.effort))}</strong><button type="button" aria-label="Close settings">×</button></div><label class="shared-picker-model"><span class="shared-sr-only">Model</span><select id="sharedModel">${this.models.map(option => `<option value="${escapeText(option.id)}"${option.id === selected.model ? " selected" : ""}>${escapeText(option.label)}</option>`).join("")}</select></label><label class="shared-picker-effort"><span class="shared-sr-only">Reasoning effort</span><input id="sharedEffortRange" type="range" min="0" max="${Math.max(0, selected.efforts.length - 1)}" value="${Math.max(0, selected.efforts.findIndex(option => option.value === selected.effort))}" step="1" aria-valuetext="${escapeText(effortLabel(selected.effort))}"${selected.efforts.length ? "" : " disabled"}></label><button type="button" id="sharedPickerSteer" class="shared-picker-session"${this.steerButton.hidden ? " hidden" : ""}${this.steerButton.disabled ? " disabled" : ""}>Steer this draft into the current turn</button><button type="button" id="sharedPickerArchive" class="shared-picker-session"${this.archiveSessionButton.hidden ? " hidden" : ""}${this.archiveSessionButton.disabled ? " disabled" : ""}>${this.state.accessMode === "archived" ? "Restore conversation" : "Archive conversation"}</button>`;
    byId("sharedPickerSteer").onclick = () => { if (thread !== this.selected || generation !== this.generation) return; this.closeSettings(false); this.steerButton.click(); };
    byId("sharedPickerArchive").onclick = () => { this.closeSettings(false); this.archiveSessionButton.click(); };
    this.dialog.querySelector('[aria-label="Close settings"]').onclick = () => this.closeSettings();
    byId("sharedModel").onchange = () => { const next = selectedCapabilities(this.models, byId("sharedModel").value, settings.effort), model = this.models.find(option => option.id === next.model); apply({ ...settings, model: next.model, effort: next.effort, serviceTier: model?.tiers.some(tier => tier.id === settings.serviceTier) ? settings.serviceTier : null }); };
    const range = byId("sharedEffortRange");
    range.oninput = () => { const label = effortLabel(selected.efforts[Number(range.value)].value); byId("sharedEffortLabel").textContent = label; range.setAttribute("aria-valuetext", label); range.style.setProperty("--effort-progress", `${Number(range.value) / Math.max(1, selected.efforts.length - 1) * 100}%`); };
    range.style.setProperty("--effort-progress", `${Number(range.value) / Math.max(1, selected.efforts.length - 1) * 100}%`);
    range.onchange = () => apply({ ...settings, effort: selected.efforts[Number(range.value)].value });
    byId("sharedPickerFast").onclick = () => { if (fast) apply({ ...settings, serviceTier: settings.serviceTier === fast.id ? null : fast.id }); };
    if (this.state.accessMode === "archived") for (const input of this.dialog.querySelectorAll("select,input,#sharedPickerFast")) input.disabled = true;
    this.dialog.showPopover(); this.pickButton.setAttribute("aria-expanded", "true"); this.positionSettings(); (this.state.accessMode === "archived" ? byId("sharedPickerArchive") : range).focus();
  }
  newSession(project = null) {
    if (["pairing-required", "forbidden"].includes(this.browserAccess) || !this.models.length || this.catalogEpoch !== this.observedEpoch || this.state.connection !== "connected") { this.report(new Error("Wait for the shared server model catalog.")); return; }
    let chosenProject = project?.id ?? (this.projectId || this.state.thread?.projectId || this.projects[0]?.id || "");
    let cwd = project?.cwd || this.projects.find(project => project.id === chosenProject)?.roots?.[0]?.path || this.roots?.[0] || "";
    const selected = selectedCapabilities(this.models);
    const model = this.models.find(model => model.id === selected.model);
    const defaults = { model: selected.model, effort: selected.effort, serviceTier: model?.defaultTier ?? null };
    const paint = () => {
      this.newProjectPicker?.destroy();
      this.newDialog.innerHTML = `<form><div class="shared-dialog-head"><strong>New Codex session</strong><button type="button" id="sharedCancelNew" aria-label="Close new session">✕</button></div><div class="shared-field"><span>Project</span><div id="sharedNewProjectField"></div></div><label class="shared-field">Execution directory<input id="sharedNewCwd" value="${escapeText(cwd)}" required><select id="sharedNewRoot" aria-label="Project directory">${(this.projects.find(project => project.id === chosenProject)?.roots || []).map(root => `<option value="${escapeText(root.path)}">${escapeText(root.path)}</option>`).join("")}</select></label><p>Choose the model, reasoning and speed in the conversation before sending.</p><button class="shared-primary" id="sharedCreateSession">Create session</button><div class="shared-new-error" role="alert"></div></form>`;
      byId("sharedCancelNew").onclick = () => this.newDialog.close();
      this.newProjectPicker = new ProjectPicker({ id: "sharedNewProject", label: "Project", onChange: id => { chosenProject = id; cwd = this.projects.find(project => project.id === chosenProject)?.roots?.[0]?.path || cwd; paint(); byId("sharedNewProject").focus(); } });
      this.newProjectPicker.setOptions(this.projectChoices("Directory only"), chosenProject); chosenProject = this.newProjectPicker.value; byId("sharedNewProjectField").append(this.newProjectPicker.root);
      byId("sharedNewCwd").oninput = () => { cwd = byId("sharedNewCwd").value; };
      byId("sharedNewRoot").hidden = (this.projects.find(project => project.id === chosenProject)?.roots || []).length < 2;
      byId("sharedNewRoot").onchange = () => { cwd = byId("sharedNewRoot").value; byId("sharedNewCwd").value = cwd; };
      this.newDialog.querySelector("form").onsubmit = async event => { event.preventDefault(); this.creating = true; this.newProjectPicker.close(false); this.newProjectPicker.button.disabled = true; byId("sharedCreateSession").disabled = true; byId("sharedCreateSession").textContent = "Creating session…"; byId("sharedCancelNew").disabled = true; this.paint();
        try { const result = await this.perform("create", { cwd: byId("sharedNewCwd").value.trim(), ...defaults, ...(chosenProject ? { projectId: chosenProject } : {}) });
          const thread = result.snapshot.thread; this.threads.unshift(thread); this.created = thread; this.createdSnapshot = result.snapshot; this.projectId = ""; this.projectPicker.setValue(""); this.archived = false;
          const sessionId = monitorSessionId(thread);
          // A failed index read must never leave the old thread as the send target.
          if (!indexState.rows.has(sessionId)) {
            const group = nativeSessionGroups({ groups: [] }, [thread], this.projects).groups[0], row = group.rows[0];
            row._group = group; indexState.groups.push(group); indexState.rows.set(sessionId, row);
          }
          this.actions.selectSession(sessionId, true); this.newDialog.close();
          // The creation receipt is sufficient to open this session. History can catch up later.
          this.actions.refreshIndex();
        } catch (error) { this.newDialog.querySelector(".shared-new-error").textContent = error.message; this.newProjectPicker.button.disabled = false; byId("sharedCreateSession").disabled = false; byId("sharedCreateSession").textContent = "Create session"; byId("sharedCancelNew").disabled = false; }
        finally { this.creating = false; this.paint(); }
      };
    }; paint(); this.newDialog.showModal();
  }
  async loadSessions() {
    const legacy = await this.api.readMonitorIndex().catch(error => {
      if (["pairing_required", "forbidden"].includes(error.code)) this.report(error);
      throw error;
    });
    // Native startup/reconnect is independent of the ordinary JSONL scan. Keep the
    // readable index and previous native rows while WorkspaceRecovery retries it.
    if (this.state?.connection && this.state.connection !== "connected") return nativeSessionGroups(legacy, this.threads || [], this.projects);
    const initialThreads = await readAllPages(this.api, "threads", { archived: this.archived, projectId: this.projectId }).catch(error => {
      if (error.code === "unavailable") {
        this.state = { ...this.state, connection: "reconnecting", needsSnapshot: true };
        this.report(error); this.recovery?.recover({ thread: true });
        return null;
      }
      // The ordinary index store owns scan retries, but shared access refusals must fence native controls immediately.
      if (["pairing_required", "forbidden"].includes(error.code)) this.report(error);
      throw error;
    });
    if (initialThreads === null) return nativeSessionGroups(legacy, this.threads || [], this.projects);
    let threads = initialThreads;
    if (this.initialThread && !threads.some(thread => thread.id === this.initialThread)) {
      // Native list omits a newly started session before its first persisted turn.
      // A deep link still has an exact native identity and can resume it safely.
      const requested = this.initialThread;
      try {
        const result = await this.api.open(requested), snapshot = result.snapshot;
        const archived = snapshot.accessMode === "archived";
        if (archived !== this.archived) {
          this.archived = archived; this.archiveButton.textContent = archived ? "Show active sessions" : "Show archived sessions";
          threads = await readAllPages(this.api, "threads", { archived, projectId: this.projectId });
        }
        this.created = snapshot.thread; this.createdSnapshot = snapshot;
      } catch (error) {
        if (!["rejected", "invalid_input"].includes(error.code)) throw error;
        // A definite missing/unreadable deep link must not prevent all other history from loading.
        this.unavailableThread = { id: requested, session: new URL(location.href).searchParams.get("session") || `native:${requested}`, message: error.message };
        this.report(error);
      }
    }
    this.initialThread = null;
    if (this.created && threads.some(thread => thread.id === this.created.id)) this.created = null;
    else if (this.created && !this.archived && (!this.projectId || this.created.projectId === this.projectId)) threads.unshift(this.created);
    this.threads = threads;
    return nativeSessionGroups(legacy, threads, this.projects);
  }
  isEmptySession(session) {
    // A successful create/read confirms an empty native thread before its first rollout exists.
    // Keep write readiness on the resumed state; this proof only classifies a missing record stream.
    const proof = this.state.thread?.id === this.selected ? this.state : this.createdSnapshot;
    const thread = proof?.thread;
    const epoch = this.connectionEpoch ?? this.observedEpoch;
    return !!thread && thread.id === this.selected && monitorSessionId(thread) === session
      && Array.isArray(thread.turns) && thread.turns.length === 0
      && this.state.connection === "connected" && proof.epoch === epoch && proof.epoch === this.observedEpoch
      && (this.state.epoch == null || proof.epoch === this.state.epoch);
  }
  groupSessions(groups, build) { return nativeSessionTree(groups, build); }
  projectTree(visible, all) { return nativeProjectTree(visible, all, this.projects.filter(project => !this.projectId || project.id === this.projectId)); }
  observeRecords(session, records) { this.outgoing?.observe(session, records); if (this.active !== false) { this.paintOutgoing(); this.fitComposer(); } }
  paintOutgoing() {
    if (!this.outgoingRoot) return;
    const entries = this.outgoing.list(this.selected, this.state.queue);
    for (const id of this.outgoing.takeRetired()) void this.journal?.retire(id).catch(error => this.report(error));
    const html = entries.map(entry => `<div class="shared-outgoing-message" data-outgoing-state="${entry.status}"><div class="shared-outgoing-text">${escapeText(entry.text)}${entry.images.length ? `<div class="shared-outgoing-images">${entry.images.map(url => `<img src="${escapeText(url)}" alt="Outgoing image">`).join("")}</div>` : ""}</div><div class="shared-outgoing-status"><span role="status">${entry.status === "sending" ? "Sending…" : entry.status === "uncertain" ? (entry.historyMatch ? "Matching text found in history · review delivery" : "Delivery not confirmed · check receipt or review history") : "Sent · syncing history"}</span>${entry.status === "uncertain" ? `<button type="button" data-check-outgoing="${escapeText(entry.id)}">Check receipt</button>` : ""}${entry.status !== "sending" ? `<button type="button" data-dismiss-outgoing="${escapeText(entry.id)}" aria-label="${entry.status === "uncertain" ? "Review and retire send attempt" : "Dismiss message preview"}" title="${entry.status === "uncertain" ? "Review and retire attempt" : "Dismiss preview"}">×</button>` : ""}</div></div>`).join("");
    if (html !== this.outgoingHtml) { this.outgoingRoot.innerHTML = html; this.outgoingHtml = html; }
  }
  newProject() {
    this.newDialog.innerHTML = `<form><div class="shared-dialog-head"><strong>Add project</strong><button type="button" id="sharedCancelNew" aria-label="Close add project">✕</button></div><label class="shared-field">Project name<input id="sharedProjectName" required maxlength="256" autocomplete="off" placeholder="My project"></label><label class="shared-field">Directory on this computer<input id="sharedProjectDirectory" required placeholder="/path/to/project" autocomplete="off" spellcheck="false"></label><p>Choose an existing directory. Sessions in this project will run there.</p><button class="shared-primary" id="sharedAddProject">Add project</button><div class="shared-new-error" role="alert"></div></form>`;
    byId("sharedCancelNew").onclick = () => this.newDialog.close();
    this.newDialog.querySelector("form").onsubmit = async event => {
      event.preventDefault(); if (this.creating || this.pending) return;
      const name = byId("sharedProjectName").value.trim(), path = byId("sharedProjectDirectory").value.trim();
      if (!name || !path) return;
      this.creating = true; byId("sharedProjectName").disabled = true; byId("sharedProjectDirectory").disabled = true; byId("sharedAddProject").disabled = true; byId("sharedCancelNew").disabled = true; byId("sharedAddProject").textContent = "Adding project…";
      try {
        const result = await this.perform("project/create", { name, roots: [path] });
        if (!result.project?.id || !Array.isArray(result.project.roots)) throw new Error("Project was added. Reconnect to refresh the project list.");
        this.projects = [...this.projects.filter(project => project.id !== result.project.id), result.project];
        this.projectId = ""; this.paintProjects(); this.actions.renderTree?.(); this.actions.refreshIndex(); this.newDialog.close();
      } catch (error) { this.newDialog.querySelector(".shared-new-error").textContent = error.message; }
      finally { this.creating = false; byId("sharedProjectName").disabled = false; byId("sharedProjectDirectory").disabled = false; byId("sharedAddProject").disabled = false; byId("sharedCancelNew").disabled = false; byId("sharedAddProject").textContent = "Add project"; this.paint(); }
    };
    this.newDialog.showModal(); byId("sharedProjectName").focus();
  }
  sessionIdForThread(id) { const thread = this.threads.find(thread => thread.id === id); return thread ? monitorSessionId(thread) : this.unavailableThread?.id === id ? this.unavailableThread.session : null; }
  sessionReadFailure(session) { return this.unavailableThread?.session === session ? this.unavailableThread : null; }
  /** Use the queued message's server-owned input; the current draft stays untouched. */
  async controlQueued(action, clientUserMessageId) {
    if (!this.canControl()) throw new Error("Reconnect this session before changing its queue.");
    if (!this.state.queue?.some(item => item.clientUserMessageId === clientUserMessageId)) throw new Error("This queued message has already resolved. Refresh the conversation.");
    const turn = activeTurn(this.state.thread);
    if (action === "steer" && !turn) throw new Error("This turn has finished. The message remains queued for the next turn.");
    if (action !== "steer" && action !== "cancel") throw new Error("Unknown queued message action.");
    await this.perform(`queue/${action}`, { threadId: this.selected, clientUserMessageId, ...(action === "steer" ? { expectedTurnId: turn.id } : {}) });
    this.recovery.recover({ thread: true });
  }
  paintPending() {
    const tokenKey = (this.state.pending || []).map(request => `${request.requestToken}:${request.id}`).join("|") + this.canControl();
    if (this.requestsKey !== tokenKey) {
      // Connection and action state rerender the same request. Keep its answers in memory until its token retires.
      for (const form of this.requests.querySelectorAll("[data-request-token]")) {
        const values = new Map();
        for (const input of form.querySelectorAll("[data-question-id]")) values.set(JSON.stringify([input.dataset.questionId, input.type, input.type === "radio" ? input.value : ""]), input.type === "radio" ? input.checked : input.value);
        if (form.dataset.requestToken) this.requestDrafts.set(form.dataset.requestToken, values);
      }
      const liveTokens = new Set((this.state.pending || []).map(request => request.requestToken));
      for (const token of this.requestDrafts.keys()) if (!liveTokens.has(token)) this.requestDrafts.delete(token);
      this.requestsKey = tokenKey; this.requests.innerHTML = (this.state.pending || []).map((request, index) => requestHtml(request, index, !this.canControl())).join("");
      for (const form of this.requests.querySelectorAll("[data-request-token]")) {
        const values = this.requestDrafts.get(form.dataset.requestToken);
        for (const input of form.querySelectorAll("[data-question-id]")) {
          const key = JSON.stringify([input.dataset.questionId, input.type, input.type === "radio" ? input.value : ""]);
          if (values?.has(key)) { if (input.type === "radio") input.checked = values.get(key); else input.value = values.get(key); }
        }
      }
    }
    // Explicit state also invalidates WebKit styles when a request is inserted.
    // Desktop-owned or disabled cards leave ordinary reader navigation available.
    document.body.dataset.sharedPending = String(this.active !== false && !!this.requests.querySelector("button:not(:disabled)"));
    const queue = this.state.queue || [];
    const turn = activeTurn(this.state.thread);
    const queueKey = JSON.stringify([this.selected, this.canControl(), turn?.id, queue.map(item => [item.clientUserMessageId, textOfInput(item.input || [])])]);
    // Focus paints must retain the focused queue action node; only changed queue content/state replaces it.
    if (this.queueKey !== queueKey) {
      this.queueKey = queueKey;
      this.queue.innerHTML = queue.map(item => `<div><span>${escapeText(textOfInput(item.input || []))}</span><div class="shared-queue-actions">${turn ? `<button type="button" data-queue-action="steer" data-queue-id="${escapeText(item.clientUserMessageId)}" title="Use this queued message in the current turn"${this.canControl() ? "" : " disabled"}>Steer now</button>` : ""}<button type="button" data-queue-action="cancel" data-queue-id="${escapeText(item.clientUserMessageId)}"${this.canControl() ? "" : " disabled"}>Cancel</button></div></div>`).join("");
    }
  }
  answer(request, response) { return this.run(() => this.perform("respond", requestResponsePayload(request, response))); }
  /** Foreground recovery only reads state; it never replays a pending native action. */
  resumeEvents() {
    if (this.active === false) return;
    this.generation++; this.state.connection = "reconnecting"; this.state.needsSnapshot = !!this.selected;
    this.catalogEpoch = null; this.createdSnapshot = null; this.paint();
    this.connectEvents(); this.recovery.recover({ catalog: true, thread: true });
  }
  suspendEvents() {
    this.generation++; this.events?.close(); this.events = null;
    this.state.connection = "reconnecting"; this.state.needsSnapshot = !!this.selected; this.paint();
  }
  connectEvents() {
    if (this.active === false) return;
    this.events?.close();
    const events = this.events = new EventSource("/api/codex/events", { withCredentials: true });
    events.onmessage = message => {
      if (this.events !== events) return;
      try {
        const event = JSON.parse(message.data);
        if (event.method === "gateway/disabled") { this.deactivate(); this.actions.nativeDisabled?.(); return; }
        // Connection evidence survives selection resets and HTTP responses from an earlier epoch.
        // A fresh stream envelope can reset the Gateway epoch; older events in that stream cannot.
        if (Number.isSafeInteger(event.epoch) && event.epoch >= 0 && (!event.method
          || event.method === "gateway/connection" && (this.connectionEpoch == null || event.epoch >= this.connectionEpoch))) {
          this.connectionEpoch = event.epoch;
          if (this.createdSnapshot && event.epoch !== this.createdSnapshot.epoch) this.createdSnapshot = null;
        }
        if (!event.method) { this.generation++; this.state.needsSnapshot = !!this.selected; if (this.state.epoch !== event.epoch) { this.catalogEpoch = null; this.state.needsSnapshot = true; } this.state.connection = event.connection; this.recovery.recover({ catalog: this.state.epoch !== event.epoch, thread: true }); }
        else {
          if (this.opening) {
            if (this.opening.events.length < 2048) this.opening.events.push(event);
            else this.opening.overflow = true;
          }
          this.state = applyWorkspaceEvent(this.state, event, reduceEvent, (_id, message) => this.report(new Error(message)), message => this.report(new Error(message)));
          if (this.state.needsSnapshot && !this.opening) this.recovery.recover({ thread: true });
        }
        this.paint();
      } catch (error) { this.report(error); }
    };
    events.onerror = () => { if (this.events !== events) return; this.generation++; this.state.needsSnapshot = !!this.selected; this.state.connection = "reconnecting"; this.paint(); this.recovery.recover({ thread: true }); };
  }
}
