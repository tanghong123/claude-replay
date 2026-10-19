import { appendDesktopHelp, mountNativeMenu, renderNativeMenu, renderNativeServerStatus, positionNativeMenu, readNativeStatus, ownsNativeServer, confirmNativeShutdown } from "./native-menu.js";
import { controlState, indexState, selectedRow } from "./state.js";

const byId = id => document.getElementById(id);
import { isCodexSession, usesNativeControls } from "./native-policy.js";

/** Own only the presentation changes made by the optional native controller. */
class NativePresentation {
  constructor() {
    this.input = byId("composeInput");
    this.parent = this.input.parentNode; this.next = this.input.nextSibling;
    this.nodes = new Set(document.querySelectorAll("*"));
    this.saved = ["writeBtn", "writeNotice", "composeFab", "closeComposer", "sidebarMiniWrite", "sendBtn", "composeInput", "composer"].map(id => {
      const node = byId(id);
      return { node, attributes: [...node.attributes].map(attribute => [attribute.name, attribute.value]),
        events: Object.fromEntries(["onclick", "oninput", "onkeydown", "onpaste"].map(key => [key, node[key]])) };
    });
    this.viewport = document.querySelector("meta[name=viewport]"); this.viewportContent = this.viewport.content;
  }
  capture() {
    this.native = this.saved.map(({ node }) => ({ node, attributes: [...node.attributes].map(attribute => [attribute.name, attribute.value]),
      events: Object.fromEntries(["onclick", "oninput", "onkeydown", "onpaste"].map(key => [key, node[key]])) }));
    this.nativeParent = this.input.parentNode;
    this.owned = [...document.querySelectorAll("*")].filter(node => !this.nodes.has(node) && this.nodes.has(node.parentNode))
      .map(node => ({ node, hidden: node.hidden }));
    for (const { node } of this.owned) node.classList.add("codex-native-only");
  }
  apply(snapshot) {
    for (const { node, attributes, events } of snapshot) {
      for (const attribute of [...node.attributes]) node.removeAttribute(attribute.name);
      for (const [name, value] of attributes) node.setAttribute(name, value);
      Object.assign(node, events);
    }
  }
  show() {
    this.apply(this.native);
    this.nativeParent.append(this.input);
    for (const { node, hidden } of this.owned) node.hidden = hidden;
    document.body.dataset.sharedCodex = "true";
  }
  hide() {
    for (const { node } of this.owned) {
      if (node.matches(":popover-open")) node.hidePopover();
      if (node.open && typeof node.close === "function") node.close();
      node.hidden = true;
    }
    this.parent.insertBefore(this.input, this.next);
    this.apply(this.saved);
    delete document.body.dataset.sharedCodex; delete document.body.dataset.sharedReaderOpen;
    this.viewport.content = this.viewportContent;
    const composer = byId("composer"); composer.removeAttribute("data-shared-constrained");
    composer.classList.remove("production-show"); controlState.row = null;
    document.body.style.removeProperty("--shared-composer-height");
    byId("transcript").style.paddingBottom = "";
  }
}

/** Compose ordinary controls with an explicitly enabled Codex controller. */
export class NativeModeControls {
  constructor(ordinary, actions, menuMount = null) {
    this.ordinary = ordinary; this.actions = actions; this.mode = "off"; this.view = "ordinary";
    this.intent = "none"; this.status = null; this.drafts = new Map(); this.draftRevisions = new Map(); this.composerGeneration = 0;
    ordinary.actions = { ...ordinary.actions, captureComposer: () => this.captureOrdinarySend(), repaintComposer: () => this.paint() };
    this.lastOrdinarySession = indexState.selected;
    ({ button: this.button, menu: this.menu } = menuMount || mountNativeMenu(() => this.openMenu()));
    if (menuMount) this.button.onclick = () => this.menu.matches(":popover-open") ? this.menu.hidePopover() : this.openMenu();
    new ResizeObserver(() => this.positionMenu()).observe(this.menu);
    visualViewport?.addEventListener("resize", () => this.positionMenu());
    visualViewport?.addEventListener("scroll", () => this.positionMenu());
    const originalSwitch = byId("writeSwitch").onclick;
    byId("writeSwitch").onclick = async event => {
      if (this.pending) return;
      if (controlState.write && this.enabled && !await this.disable()) return;
      originalSwitch(event); this.paint();
    };
    const originalMiniWrite = byId("sidebarMiniWrite").onclick;
    byId("sidebarMiniWrite").onclick = event => this.enabled && controlState.write && (!selectedRow() || isCodexSession(selectedRow()))
      ? this.newSession() : originalMiniWrite(event);
    byId("composeInput").addEventListener("input", () => {
      const session = controlState.row?.id || indexState.selected;
      if (!this.nativeActive && session) {
        this.drafts.set(session, byId("composeInput").value);
        this.draftRevisions.set(session, (this.draftRevisions.get(session) || 0) + 1);
      }
    });
    this.paint();
  }
  get enabled() { return ["on", "confirming", "disabling"].includes(this.mode); }
  /** A late ordinary response may retire its own revision, never another controller's editor. */
  captureOrdinarySend() {
    const session = controlState.row?.id || indexState.selected, generation = this.composerGeneration, revision = this.draftRevisions.get(session) || 0;
    const isCurrent = () => !this.nativeActive && this.composerGeneration === generation
      && (controlState.row?.id === session || !controlState.row)
      && (this.draftRevisions.get(session) || 0) === revision;
    return {
      isCurrent,
      accepted: () => {
        if ((this.draftRevisions.get(session) || 0) !== revision) return;
        this.drafts.delete(session);
        if (isCurrent()) byId("composeInput").value = "";
      }
    };
  }
  get pending() { return ["enabling", "confirming", "disabling"].includes(this.mode); }
  get nativeActive() { return this.view === "native"; }
  get runtimePanel() { return this.native?.runtimePanel; }
  fitComposer() { if (this.nativeActive) this.native.fitComposer(); }
  open() { return this.nativeActive ? this.native.open() : this.ordinary.open(); }
  close() { return this.nativeActive ? this.native.close?.() : this.ordinary.close(); }
  paintSoon() { queueMicrotask(() => this.paint()); }
  paint() {
    const row = selectedRow(), unavailable = this.native?.sessionReadFailure(indexState.selected);
    const reader = row || (unavailable ? { nativeThreadId: unavailable.id } : indexState.selected ? { agent: "unknown" } : null);
    const wanted = usesNativeControls(this.enabled ? "on" : this.mode, controlState.write, reader, this.intent);
    if (wanted && !this.native && !this.installing) this.ensureNative().catch(error => {
      this.mode = "off"; this.actions.toast(error.message); this.paint();
    });
    if (this.native && wanted !== this.nativeActive) {
      this.composerGeneration++;
      if (wanted) {
        if (this.lastOrdinarySession) this.drafts.set(this.lastOrdinarySession, byId("composeInput").value);
        this.presentation.show(); this.view = "native"; this.native.active = true;
        byId("composeInput").value = ""; this.native.resumeEvents();
      } else {
        this.native.deactivate(); this.view = "ordinary"; this.presentation.hide();
        byId("composeInput").value = this.drafts.get(indexState.selected) || "";
      }
      dispatchEvent(new Event("resize"));
    }
    if (this.nativeActive) this.native.paint();
    else {
      this.lastOrdinarySession = indexState.selected; this.ordinary.paint();
      if (this.pending && isCodexSession(row)) byId("sendBtn").disabled = true;
    }
    this.button.hidden = !controlState.write;
    this.button.dataset.nativeEnabled = String(this.enabled);
    if (!controlState.write && this.menu.matches(":popover-open")) this.menu.hidePopover();
    this.button.title = this.mode === "confirming" ? "Checking Codex shutdown…" : this.mode === "disabling" ? "Closing Codex native…" : this.pending ? "Connecting to Codex…" : this.status?.statusError ? this.status.statusError
      : this.enabled ? "Codex native enabled" : this.status?.serverListening ? "Connect to the running Codex server"
        : this.status?.serverListening === false ? "Server is not running; click to start and enable" : "Check Codex server status";
    this.button.disabled = this.pending;
    const projectUiKey = String(this.projectActions);
    if (this.projectUiKey !== projectUiKey) { this.projectUiKey = projectUiKey; this.actions.renderTree?.(); }
    if (this.menu.matches(":popover-open")) renderNativeServerStatus(this.menu,
      { enabled: this.enabled, status: this.nativeActive ? { ...this.status, connection: this.native.state.connection } : this.status, pending: this.pending, closing: this.mode === "disabling", confirming: this.mode === "confirming", activationFailed: this.activationFailed });
  }
  async checkStatus() {
    this.checking = true;
    try {
      const status = await readNativeStatus(fetch);
      this.status = status; this.activationFailed = false;
    } catch (error) { this.status = { statusError: `Cannot check Codex server status: ${error.message}`, cause: error }; }
    finally { this.checking = false; this.paint(); }
  }
  async openMenu() {
    if (!controlState.write || this.pending) return;
    await this.checkStatus();
    this.showMenu();
  }
  showMenu() {
    const action = renderNativeMenu(this.menu, { enabled: this.enabled, status: this.status, pending: this.pending },
      () => this.enabled ? this.disable() : this.status?.statusError ? this.openMenu() : this.enable());
    this.appendConnectionHelp();
    this.menu.showPopover();
    this.positionMenu();
    action.focus();
  }
  appendConnectionHelp() {
    if (this.nativeActive && this.native && !this.menu.querySelector("#nativeConnectionHelp")) {
      const help = document.createElement("button"); help.type = "button"; help.id = "nativeConnectionHelp";
      help.textContent = "Session connection help";
      help.onclick = () => { this.menu.hidePopover(); this.native.showConnectionHelp(); };
      this.menu.append(help);
    }
  }
  positionMenu() { positionNativeMenu(this.menu, this.button); }
  appendDesktopHelp(parent) { appendDesktopHelp(parent, this.status?.desktopHelp, message => this.actions.toast(message), { document, navigator }); }
  async enable() {
    if (!controlState.write || this.pending) return;
    this.mode = "enabling"; this.activationFailed = false; this.paint();
    try {
      const { GatewayApi } = await import("/codex-workspace/api.js");
      const result = await new GatewayApi().action("start", { writeMode: true, startIfMissing: true });
      await this.actions.nativeStyles?.();
      await this.adopt(result);
      if (this.menu.matches(":popover-open")) this.appendConnectionHelp();
    } catch (error) { this.activationFailed = true; this.actions.toast(error.message); }
    finally { if (this.mode === "enabling") this.mode = "off"; this.paint(); }
  }
  async adopt(status) {
    this.status = status; this.mode = "on";
    this.actions.nativeEnabled?.();
    await this.ensureNative();
    // Activation is ready before the full history inventory finishes scanning.
    this.paint(); this.actions.refreshIndex();
  }
  async disable() {
    if (this.pending || !this.enabled) return false;
    const returnFocus = document.activeElement?.id === "writeSwitch" ? byId("writeSwitch") : this.button;
    let paused = false;
    this.mode = "confirming";
    this.paint();
    try {
      // Refresh ownership: a previously external endpoint may now be Monitor-owned.
      this.status = await readNativeStatus(fetch);
      const owned = ownsNativeServer(this.status);
      if (owned || this.status.activeWork || this.status.activeTurns?.length) {
        if (this.menu.matches(":popover-open")) this.menu.hidePopover();
        if (!await confirmNativeShutdown(document, returnFocus, { owned, activeTurns: this.status.activeTurns || [] })) return false;
      }
      this.mode = "disabling"; this.actions.pauseIndex?.(); paused = true;
      if (this.native) { this.native.closing = true; this.native.suspendEvents(); }
      this.paint();
      const { GatewayApi } = await import("/codex-workspace/api.js");
      this.status = await new GatewayApi().request("stop", {});
      this.finishDisable();
      if (this.status.shutdownWarnings?.length) this.actions.toast(this.status.shutdownWarnings[0]);
      return true;
    } catch (error) { this.actions.toast(error.message); return false; }
    finally {
      if (this.mode === "disabling" || this.mode === "confirming") this.mode = "on";
      if (paused && this.native) { this.native.closing = false; if (this.enabled) this.native.resumeEvents(); }
      this.paint(); if (paused) this.actions.resumeIndex?.();
      if (returnFocus?.isConnected && !returnFocus.hidden) returnFocus.focus();
    }
  }
  finishDisable() {
    this.mode = "off"; this.intent = "none";
    if (this.menu.matches(":popover-open")) this.menu.hidePopover();
    this.paint(); this.actions.nativeDisabled?.(); this.actions.refreshIndex();
  }
  async ensureNative() {
    if (this.native) return this.native;
    if (this.installing) return this.installing;
    this.installing = (async () => {
      const { MonitorControls } = await import("/codex-workspace/monitor-controls.js");
      const presentation = new NativePresentation();
      if (indexState.selected) this.drafts.set(indexState.selected, byId("composeInput").value);
      this.presentation = presentation;
      document.body.dataset.sharedCodex = "true";
      this.native = new MonitorControls({ ...this.actions, nativeManaged: true,
        nativeDisabled: () => { if (this.mode !== "disabling") this.finishDisable(); },
        nativeNewClosed: () => { this.intent = "none"; this.paint(); } });
      presentation.capture(); presentation.show(); this.view = "native"; this.composerGeneration++;
      this.paint();
      return this.native;
    })().finally(() => { this.installing = null; });
    return this.installing;
  }
  async newSession(project = null) {
    if (!this.enabled || !controlState.write || this.pending || this.intent === "create") return;
    this.intent = "create";
    try {
      const native = await this.ensureNative(); this.paint();
      const deadline = Date.now() + 30_000;
      while (!native.models.length || native.state.connection !== "connected" || native.catalogEpoch !== native.observedEpoch) {
        if (!this.enabled || !controlState.write) { this.intent = "none"; this.paint(); return; }
        if (Date.now() >= deadline) throw new Error("The Codex model catalog did not become ready. Reconnect native mode.");
        // Recovery also resumes the old session; opening this dialog only needs its catalog.
        native.recovery.recover();
        await new Promise(resolve => setTimeout(resolve, 150));
      }
      if (project === "add") native.newProject();
      else native.newSession(project);
    }
    catch (error) { this.intent = "none"; this.paint(); this.actions.toast(error.message); }
  }
  async loadSessions() {
    if (this.enabled && controlState.write && this.native) return this.native.loadSessions();
    const response = await fetch("/api/sessions", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return response.json();
  }
  groupSessions(groups, build) { return this.enabled && controlState.write && this.native ? this.native.groupSessions(groups, build) : build(groups); }
  get projectActions() { return this.enabled && controlState.write && !this.pending; }
  projectTree(visible, all) { return this.projectActions && this.native ? this.native.projectTree(visible, all) : visible; }
  observeRecords(session, records) { this.native?.observeRecords(session, records); }
  sessionIdForThread(id) { return this.native?.sessionIdForThread(id); }
  isEmptySession(session) { return this.nativeActive && this.native.isEmptySession(session); }
  sessionReadFailure(session) { return this.nativeActive ? this.native.sessionReadFailure(session) : null; }
}
