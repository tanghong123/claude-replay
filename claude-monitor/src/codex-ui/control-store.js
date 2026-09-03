import { canCompose, canInject, controlState, selectedRow } from "./state.js";

const byId = id => document.getElementById(id);
const json = response => response.json().catch(() => ({ ok: false, error: `HTTP ${response.status}` }));

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
    byId("confirmSend").onclick = () => this.grantAndSend();
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
    byId("writeSwitch").setAttribute("aria-checked", String(controlState.write));
    byId("writeBtn").classList.toggle("on", controlState.write);
    byId("sidebarMiniWrite").classList.toggle("on", controlState.write);
    byId("writeInfo").hidden = controlState.paired;
    byId("composeFab").classList.toggle("show", controlState.write && available && !byId("composer").classList.contains("production-show"));
    byId("writeNotice").classList.toggle("show", controlState.write && !available);
    if (!available) {
      byId("composer").classList.remove("production-show");
      controlState.row = null;
    }
  }
  open() {
    const row = selectedRow();
    if (!controlState.write || !canCompose(row)) return;
    controlState.row = row;
    controlState.mode = canInject(row) ? "tmux" : "resume";
    controlState.consented = !!row.consented;
    byId("composerTitle").textContent = row.name || row.id;
    byId("composeTarget").textContent = `${row.agent || "Agent"} · ${row._group?.label || ""}`;
    byId("composer").classList.add("production-show");
    byId("composer").classList.toggle("live", controlState.mode === "tmux");
    this.revokeButton.hidden = !(controlState.mode === "tmux" && controlState.consented);
    byId("composeFab").classList.remove("show");
    byId("composeInput").focus();
  }
  close() {
    byId("composer").classList.remove("production-show");
    controlState.row = null;
    this.paintSoon();
  }
  paintSoon() { queueMicrotask(() => this.paint()); }
  attempt() {
    const text = byId("composeInput").value.trim();
    if (!text || !controlState.row) return;
    controlState.pending = text;
    if (controlState.mode === "tmux" && !controlState.consented) {
      byId("confirmText").textContent = `The message goes to ${controlState.row.target || "the current tmux pane"} and runs with that agent's current permissions.`;
      byId("confirmLayer").classList.add("production-open");
      return;
    }
    this.send(text);
  }
  closeConfirm() { byId("confirmLayer").classList.remove("production-open"); }
  async grantAndSend() {
    const passcode = document.querySelector(".passcode-input")?.value || "";
    this.closeConfirm();
    const response = await fetch(`/api/consent?target=${encodeURIComponent(controlState.row.id)}`, { method: "POST", body: passcode, cache: "no-store" });
    const result = await json(response);
    if (["passcode-required", "bad-passcode"].includes(result.code)) {
      const dialog = byId("confirmLayer").querySelector(".dialog");
      let input = dialog.querySelector(".passcode-input");
      if (!input) { input = document.createElement("input"); input.className = "passcode-input"; input.type = "password"; input.placeholder = "Monitor passcode"; dialog.insertBefore(input, dialog.querySelector(".dialog-actions")); }
      byId("confirmText").textContent = result.code === "bad-passcode" ? "Passcode incorrect — try again." : "This monitor requires a passcode before granting write access.";
      byId("confirmLayer").classList.add("production-open"); input.focus(); return;
    }
    if (!result.ok) { this.actions.toast(result.error || "Grant failed"); return; }
    controlState.consented = true;
    await this.send(controlState.pending);
  }
  async revoke() {
    if (!controlState.row || controlState.mode !== "tmux") return;
    const response = await fetch(`/api/consent?op=revoke&target=${encodeURIComponent(controlState.row.id)}`, { method: "POST", cache: "no-store" });
    const result = await json(response);
    if (!result.ok) { this.actions.toast(result.error || "Revoking the grant failed"); return; }
    controlState.consented = false;
    this.revokeButton.hidden = true;
    this.actions.toast("Pane grant revoked");
    this.actions.refreshIndex();
  }
  async send(text) {
    byId("sendBtn").disabled = true;
    try {
      const response = await fetch(`/api/send?target=${encodeURIComponent(controlState.row.id)}`, { method: "POST", body: text, cache: "no-store" });
      const result = await json(response);
      if (!result.ok) { if (result.code === "no-consent") controlState.consented = false; throw new Error(result.error || "Send failed"); }
      byId("composeInput").value = "";
      this.actions.toast(controlState.mode === "tmux" ? "Sent to the pane" : "Resuming the session");
      this.close(); this.actions.refreshIndex();
    } catch (error) { this.actions.toast(error.message); }
    finally { byId("sendBtn").disabled = false; }
  }
}
