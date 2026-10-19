/** Read-only discovery shared by the enabling control and the active native menu. */
export async function readNativeStatus(fetcher, { timeoutMs = 30_000, schedule = setTimeout, cancel = clearTimeout } = {}) {
  const controller = new AbortController(); let timer;
  const deadline = new Promise((_, reject) => { timer = schedule(() => {
    const cause = new Error("Codex status response exceeded its deadline");
    reject(new Error("Codex status timed out. Click the native control to retry.", { cause })); controller.abort(cause);
  }, timeoutMs); });
  const read = async () => {
    const response = await fetcher("/api/codex/status", { cache: "no-store", credentials: "same-origin", signal: controller.signal });
    if (!response.ok) throw new Error(`Native status HTTP ${response.status}`);
    const status = await response.json();
    if (!status || typeof status !== "object" || Array.isArray(status) || status.ok !== true
        || typeof status.serverListening !== "boolean" || !["stopped", "available", "activated"].includes(status.runtime)) throw new Error("Native status returned an invalid response envelope");
    if (status.activeWork !== undefined && typeof status.activeWork !== "boolean"
        || status.activeTurns !== undefined && (!Array.isArray(status.activeTurns) || status.activeTurns.some(turn => !turn || typeof turn.threadId !== "string" || typeof turn.threadName !== "string" || turn.turnId !== null && typeof turn.turnId !== "string"))) throw new Error("Native status returned invalid active-work metadata");
    return status;
  };
  try { return await Promise.race([read(), deadline]); }
  finally { cancel(timer); }
}
/** Ownership, rather than the socket transport or connected clients, determines shutdown impact. */
export function ownsNativeServer(status) {
  return status?.homeProvenance === "monitor-started" && Number.isInteger(status.ownedServerPid) && status.ownedServerPid > 0;
}

/** Cancel and Escape keep the server running. The caller sends Stop only after confirmation. */
export function confirmNativeShutdown(document, returnFocus, { owned = true, activeTurns = [] } = {}) {
  const dialog = document.createElement("dialog");
  dialog.id = "nativeStopDialog"; dialog.className = "production-native-stop";
  dialog.setAttribute("aria-labelledby", "nativeStopTitle");
  dialog.setAttribute("aria-describedby", "nativeStopDescription");
  const title = document.createElement("h2"); title.id = "nativeStopTitle"; title.textContent = owned ? "Stop Codex server?" : "Disconnect Codex native?";
  const description = document.createElement("p"); description.id = "nativeStopDescription";
  const turns = activeTurns.map(turn => turn.turnId
    ? `Turn ${turn.turnId} in ${turn.threadName || turn.threadId}`
    : `Active conversation ${turn.threadName || turn.threadId} (turn identity unavailable)`);
  const running = turns.length ? `Running: ${turns.join("; ")}. ` : "";
  description.textContent = running + (owned
    ? "Connected Codex apps will disconnect, and running tasks will be interrupted. Saved conversation history will remain available."
    : "Monitor will disconnect while native work is active or pending. The external server and its turns keep running. In-flight sends may have an uncertain result; check their receipts before sending again.");
  const actions = document.createElement("div"); actions.className = "production-native-stop-actions";
  const cancel = document.createElement("button"); cancel.type = "button"; cancel.id = "nativeStopCancel"; cancel.textContent = "Cancel"; cancel.autofocus = true;
  const stop = document.createElement("button"); stop.type = "button"; stop.id = "nativeStopConfirm"; stop.textContent = owned ? "Stop server" : "Disconnect";
  actions.append(cancel, stop); dialog.append(title, description, actions);
  return new Promise(resolve => {
    let settled = false;
    const settle = confirmed => {
      if (settled) return;
      settled = true;
      // The native close event can be deferred until a rendering update. Settle
      // the explicit action now so background tabs cannot strand the toggle.
      if (dialog.open) dialog.close(confirmed ? "stop" : "cancel");
      dialog.remove(); returnFocus?.focus(); resolve(confirmed);
    };
    dialog.addEventListener("close", () => settle(false), { once: true });
    dialog.addEventListener("cancel", event => { event.preventDefault(); settle(false); });
    cancel.onclick = () => settle(false); stop.onclick = () => settle(true);
    document.body.append(dialog); dialog.showModal(); cancel.focus();
  });
}
/** Keep the shared-server benefit and setup prompt before launcher-dependent steps. */
export function appendDesktopHelp(parent, help, toast, { document, navigator }) {
  if (!help) return;
  const details = document.createElement("details"), summary = document.createElement("summary");
  summary.textContent = help.title; details.append(summary);
  const description = document.createElement("p"); description.textContent = help.description; details.append(description);
  if (help.prompt) {
    const prompt = document.createElement("pre"); prompt.textContent = help.prompt; details.append(prompt);
    const copy = document.createElement("button"); copy.type = "button"; copy.textContent = "Copy setup prompt";
    copy.onclick = async () => { try { await navigator.clipboard.writeText(help.prompt); toast("Setup prompt copied"); } catch { toast("Select and copy the displayed prompt"); } };
    details.append(copy);
  }
  for (const step of help.steps) { const paragraph = document.createElement("p"); paragraph.textContent = step; details.append(paragraph); }
  parent.append(details);
}
/** Clamp the small menu to the visible phone viewport, including its safe areas. */
export function positionNativeMenu(menu, button) {
  if (!menu.matches(":popover-open")) return;
  const box = button.getBoundingClientRect(), view = globalThis.visualViewport, style = getComputedStyle(menu);
  const safe = edge => parseFloat(style.getPropertyValue(`--safe-${edge}`)) || 0;
  const left = (view?.offsetLeft || 0) + safe("left") + 8, top = (view?.offsetTop || 0) + safe("top") + 8;
  const width = Math.max(0, (view?.width || innerWidth) - safe("left") - safe("right") - 16);
  const height = Math.max(0, (view?.height || innerHeight) - safe("top") - safe("bottom") - 16);
  menu.style.maxWidth = `${width}px`; menu.style.maxHeight = `${height}px`;
  menu.style.left = `${Math.max(left, Math.min(box.left, left + width - menu.offsetWidth))}px`;
  menu.style.top = `${Math.max(top, Math.min(box.bottom + 6, top + height - menu.offsetHeight))}px`;
}

/** Keep optional native mode inside the existing Write row, before and after activation. */
export function mountNativeMenu(onOpen) {
  const row = document.getElementById("writeBtn");
  const button = document.createElement("button");
  button.type = "button"; button.id = "codexNativeButton"; button.className = "production-native-trigger";
  button.setAttribute("aria-label", "Write mode options"); button.setAttribute("aria-haspopup", "dialog");
  button.setAttribute("aria-expanded", "false"); button.setAttribute("aria-controls", "codexNativeMenu");
  button.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m7 10 5 5 5-5"/></svg>';
  button.onclick = () => menu.matches(":popover-open") ? menu.hidePopover() : onOpen();
  row.insertBefore(button, document.getElementById("writeInfo"));
  const menu = document.createElement("div"); menu.className = "production-native-menu";
  menu.id = "codexNativeMenu"; menu.setAttribute("popover", "auto"); menu.setAttribute("role", "dialog");
  menu.setAttribute("aria-label", "Write mode options"); document.body.append(menu);
  menu.addEventListener("toggle", event => button.setAttribute("aria-expanded", String(event.newState === "open")));
  menu.addEventListener("keydown", event => {
    if (event.key !== "Escape") return;
    // Consume Escape here rather than letting the browser stop a loading event stream.
    event.preventDefault(); event.stopPropagation(); menu.hidePopover(); button.focus();
  });
  return { button, menu };
}

/** Process availability and this browser's native mode are separate states. */
export function nativeServerPresentation({ enabled, status, pending = false, closing = false, confirming = false, activationFailed = false }) {
  if (confirming) return { state: "running", label: "Checking shutdown…", detail: "The server stays running until shutdown is confirmed." };
  if (closing) return { state: "pending", label: "Closing…", detail: "Releasing Monitor sessions and stopping its server." };
  if (pending) return { state: "pending", label: status?.serverListening ? "Connecting…" : "Starting…",
    detail: status?.serverListening ? "Connecting Monitor to the running server." : "Agent Monitor is starting a local server." };
  if (status?.statusError || !status) return { state: "unknown", label: "Status unavailable", detail: "Retry to check whether the server is running." };
  if (activationFailed) return { state: "error", label: "Connection failed", detail: "Check the connection and try again." };
  if (!status.serverListening) return { state: "stopped", label: "Not running",
    detail: enabled ? "Monitor lost its server connection." : "Enabling Codex native starts a local server." };
  const owned = ownsNativeServer(status);
  const origin = owned ? "Started by this Monitor." : "Using an existing server.";
  const connection = !enabled ? "Native mode is off." : status.connection === "connected"
    ? "Monitor is connected." : "Monitor is reconnecting.";
  const transport = status.transport === "unix" ? "Private Unix socket" : status.transport === "websocket" ? "Authenticated WebSocket" : "";
  return { state: "running", label: "Running", detail: `${origin} ${connection}`, transport,
    note: owned ? "Turning off stops this server and its running work. Connected Codex apps will disconnect. History is kept."
      : "Turning off releases Monitor sessions. This externally managed server and its work keep running." };
}

/** Keep the focused control and avoid repeating unchanged live-region announcements. */
export function renderNativeServerStatus(menu, options) {
  const action = menu.querySelector("#nativeEnable");
  if (action) {
    action.disabled = !!options.pending;
    if (action.getAttribute("role") === "switch") action.setAttribute("aria-checked", String(options.enabled));
    const description = menu.querySelector("#nativeModeDescription");
    description.textContent = options.status?.statusError || (options.enabled
      ? "Choose Codex models and reasoning in the conversation. Claude uses ordinary Write mode."
      : options.status?.serverListening ? "Use the running Codex server for native conversation controls."
        : "Starts a local Codex server when enabled. Claude uses ordinary Write mode.");
  }
  const section = menu.querySelector("#nativeServerStatus");
  if (!section) return;
  const document = menu.ownerDocument, state = nativeServerPresentation(options);
  const presentation = JSON.stringify(state);
  if (section.dataset.presentation === presentation) return;
  section.dataset.presentation = presentation;
  section.dataset.state = state.state;
  const heading = document.createElement("div"); heading.className = "production-server-heading";
  const title = document.createElement("span"); title.textContent = "Codex app-server";
  const value = document.createElement("strong"); value.textContent = state.label;
  heading.append(title, value);
  const detail = document.createElement("p"); detail.textContent = state.detail;
  const children = [heading, detail];
  if (state.transport) {
    const transport = document.createElement("span"); transport.className = "production-server-transport"; transport.textContent = state.transport;
    children.push(transport);
  }
  if (state.note) { const note = document.createElement("p"); note.className = "production-server-note"; note.textContent = state.note; children.push(note); }
  section.replaceChildren(...children);
}

/** A switch communicates the mode; status text explains when toggling will start a server. */
export function renderNativeMenu(menu, { enabled, status, pending = false }, activate) {
  const document = menu.ownerDocument;
  menu.replaceChildren();
  const heading = document.createElement("strong"); heading.textContent = "Write mode";
  const action = document.createElement("button"); action.type = "button"; action.id = "nativeEnable";
  action.className = "production-native-toggle"; action.disabled = pending;
  const retry = status?.statusError && !enabled;
  const label = document.createElement("span"); label.textContent = retry ? "Retry connection status" : "Codex native";
  action.append(label);
  if (!retry) {
    action.setAttribute("role", "switch"); action.setAttribute("aria-checked", String(enabled));
    const track = document.createElement("span"); track.className = "production-native-track"; track.setAttribute("aria-hidden", "true");
    track.append(document.createElement("span")); action.append(track);
  }
  action.onclick = async () => {
    action.disabled = true;
    if (!enabled && !retry) renderNativeServerStatus(menu, { enabled, status, pending: true });
    try { await activate(); } finally { action.disabled = false; }
  };
  const description = document.createElement("p"); description.id = "nativeModeDescription";
  description.textContent = status?.statusError || (enabled
    ? "Choose Codex models and reasoning in the conversation. Claude uses ordinary Write mode."
    : status?.serverListening ? "Use the running Codex server for native conversation controls."
      : "Starts a local Codex server when enabled. Claude uses ordinary Write mode.");
  action.setAttribute("aria-describedby", description.id);
  const server = document.createElement("section"); server.id = "nativeServerStatus"; server.className = "production-server-status";
  server.setAttribute("aria-label", "Codex app-server status"); server.setAttribute("role", "status"); server.setAttribute("aria-live", "polite");
  menu.append(heading, action, description, server);
  renderNativeServerStatus(menu, { enabled, status, pending });
  return action;
}
