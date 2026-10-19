/** Recognize a resume ownership refusal, including older gateways without its typed code. */
export function isThreadInUse(error, threadId) {
  if (!threadId || !error) return false;
  if (error.code === "thread_in_use") return true;
  if (error.code !== "rejected" || !error.message?.startsWith("thread/resume: ")) return false;
  try {
    const rpc = JSON.parse(error.message.slice("thread/resume: ".length));
    return rpc.code === -32600 && rpc.message === `thread ${threadId} already has an active writer`;
  } catch { return false; } // A different failure is displayed by the ordinary error UI.
}

/** Copy only public connection metadata; credentials stay on the execution computer. */
export function sharedServerSetup(status) {
  const text = value => typeof value === "string" && value.trim().length > 0;
  if (!text(status?.endpoint) || !text(status?.codexHome)) return null;
  const home = status.codexHome, endpoint = status.endpoint;
  const context = `Server: ${JSON.stringify(endpoint)}\nConfigured CODEX_HOME: ${JSON.stringify(home)}`;
  const handoff = "Inspect the installed App/CLI, then give me a one-line terminal command, or create a local launcher and give its one-line invocation. Do not execute it or quit/stop any App or server from this chat. The launcher should ask me to save work and confirm any restart, preserve unrelated instances, and verify the actual shared connection. Explain if unsupported.";
  if (status.transport === "unix" && status.authentication?.type === "unix-permissions" && text(status.socketPath)) {
    return {
      rows: [["Connection", "Private Unix socket"], ["Socket", status.socketPath], ["Codex home", home]],
      description: "Give this prompt to Codex on the computer running Monitor. It will prepare a terminal command for you to run after saving your work, without restarting itself. This private Unix connection needs no Bearer token.",
      prompt: `Prepare to connect Codex App to Agent Monitor's private Unix server.\n${context}\nSocket: ${JSON.stringify(status.socketPath)}\n${handoff}`,
    };
  }
  if (status.transport === "websocket" && status.authentication?.type === "bearer" && text(status.authentication.tokenFile)) {
    return {
      rows: [["Connection", "WebSocket · Bearer authentication"], ["Server", endpoint], ["Token file", status.authentication.tokenFile], ["Codex home", home]],
      description: "This explicit WebSocket connection requires a Bearer header that Codex App may not support. Give the prompt to Codex to prepare a Unix migration command for you to run after saving your work.",
      prompt: `Prepare a user-run migration from this Bearer WebSocket connection to a private Unix socket shared by Codex App and Agent Monitor.\n${context}\nToken file (Authorization: Bearer): ${JSON.stringify(status.authentication.tokenFile)}\nKeep credentials private. ${handoff}`,
    };
  }
  return null;
}

const make = (tag, className, text) => {
  const node = document.createElement(tag); node.className = className;
  if (text) node.textContent = text;
  return node;
};
const button = (id, label, action, className = "") => {
  const node = make("button", className, label); node.id = id; node.type = "button"; node.onclick = action; return node;
};

/** A single recoverable state; it never takes ownership, stops processes, or sends a message. */
export class SessionConflict {
  constructor({ retry, newSession }) {
    this.root = make("section", "shared-session-conflict"); this.root.id = "sharedSessionConflict"; this.root.hidden = true;
    this.root.setAttribute("aria-labelledby", "sharedConflictTitle");
    const title = make("strong", "", "Session open elsewhere"); title.id = "sharedConflictTitle"; title.setAttribute("role", "status");
    const description = make("p", "", "Finish the current turn in the other Codex app or terminal, then quit that instance to continue here.");
    this.note = make("p", "shared-conflict-note", "You can still read this session’s history.");
    this.retry = button("sharedConflictRetry", "Try again", retry);
    const help = button("sharedConflictHelp", "How to resolve", () => this.showHelp(), "shared-primary");
    help.setAttribute("aria-haspopup", "dialog");
    const actions = make("div", "shared-conflict-actions"); actions.append(help, this.retry);
    this.root.append(title, description, this.note, actions);

    this.dialog = make("dialog", "shared-dialog shared-connection-help"); this.dialog.id = "sharedConnectionHelp";
    this.dialog.setAttribute("aria-labelledby", "sharedConnectionHelpTitle");
    const head = make("div", "shared-dialog-head"), heading = make("strong", "", "Continue this session here"); heading.id = "sharedConnectionHelpTitle";
    const close = button("sharedConnectionHelpClose", "×", () => this.dialog.close()); close.setAttribute("aria-label", "Close connection help");
    head.append(heading, close);
    const intro = make("p", "", "Another Codex server is using this session. You can read its history here; new messages still belong in the other instance.");
    const steps = make("ol", "shared-connection-steps");
    for (const [label, text] of [
      ["Finish the current turn", "In the Codex app or terminal that opened this session, finish the work and save your changes."],
      ["Quit that Codex instance", "Quit that App or exit that CLI. Closing a window alone may leave its server running."],
      ["Return here and try again", "Reconnect to continue. Your draft stays here and is not sent automatically."],
    ]) {
      const step = make("li", ""); step.append(make("strong", "", label), make("p", "", text)); steps.append(step);
    }
    const alternative = make("p", "shared-connection-alternative", "You can also keep working there, or ");
    alternative.append(button("sharedConflictNew", "start a new session", () => { this.dialog.close(); newSession(); }));
    const sharing = make("details", "shared-connection-sharing");
    sharing.append(make("summary", "", "Use Codex App and Monitor together"),
      make("p", "", "Connecting both clients to one server lets you continue the same session from either place without competing for it."),
    );
    this.connectionDescription = make("p", "");
    this.connectionDetails = make("dl", "shared-connection-details");
    this.prompt = make("textarea", "shared-connection-prompt"); this.prompt.value = ""; this.prompt.readOnly = true; this.prompt.rows = 5; this.prompt.setAttribute("aria-label", "Shared server setup prompt");
    this.copyStatus = make("span", "shared-copy-status"); this.copyStatus.setAttribute("role", "status");
    this.copyButton = button("sharedCopySetup", "Copy setup prompt", () => this.copyPrompt());
    sharing.append(this.connectionDescription, this.connectionDetails, this.prompt, this.copyButton, this.copyStatus);
    this.setConnection(null);
    this.technical = make("details", "shared-connection-technical"); this.technical.hidden = true;
    this.details = make("pre", ""); this.technical.append(make("summary", "", "Technical details"), this.details);
    const footer = make("div", "shared-conflict-actions");
    this.helpRetry = button("sharedConnectionRetry", "Try again", () => { this.dialog.close(); retry(); }, "shared-primary"); footer.append(this.helpRetry);
    Object.assign(this, { heading, intro, steps, alternative, sharing, footer });
    this.dialog.append(head, intro, steps, alternative, sharing, this.technical, footer); document.body.append(this.dialog);
  }
  setConnection(status) {
    const setup = sharedServerSetup(status);
    this.connectionDetails.replaceChildren();
    for (const [label, value] of setup?.rows || []) {
      this.connectionDetails.append(make("dt", "", label), make("dd", "", value));
    }
    this.connectionDescription.textContent = setup?.description || "Connection details are unavailable. Reconnect native mode to load them.";
    this.prompt.value = setup?.prompt || ""; this.prompt.hidden = !setup;
    this.copyButton.disabled = !setup; this.copyStatus.textContent = "";
  }
  render({ error, threadId, checking = false, hasDraft = false }) {
    const occupied = isThreadInUse(error, threadId);
    this.root.hidden = !occupied; this.technical.hidden = !occupied || !!this.sharedHelp;
    this.details.textContent = occupied ? error.message : "";
    this.retry.disabled = checking; this.helpRetry.disabled = checking;
    this.retry.textContent = checking ? "Checking…" : "Try again";
    this.root.setAttribute("aria-busy", String(checking));
    this.note.textContent = hasDraft ? "Your draft is kept here. You can still read the history." : "You can still read this session’s history.";
    return occupied;
  }
  showHelp({ shared = false } = {}) {
    this.sharedHelp = shared; this.copyStatus.textContent = "";
    this.heading.textContent = shared ? "Share a Codex server" : "Continue this session here";
    for (const node of [this.intro, this.steps, this.alternative, this.footer]) node.hidden = shared;
    this.technical.hidden = shared || this.root.hidden;
    this.technical.open = false; this.sharing.open = shared;
    if (!this.dialog.open) this.dialog.showModal();
  }
  close() { if (this.dialog.open) this.dialog.close(); }
  async copyPrompt() {
    if (!this.prompt.value) return;
    try {
      await navigator.clipboard.writeText(this.prompt.value);
      this.copyStatus.textContent = "Copied";
    } catch {
      // Plain HTTP and denied clipboard access still provide a usable copy path.
      this.prompt.focus(); this.prompt.select(); this.copyStatus.textContent = "Select and copy the prompt above.";
    }
  }
}
