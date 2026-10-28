// #s298 (Hong chose Way 1 on #s299): the dialog a live Claude session holds in its herdr pane — a
// question, a permission prompt, a plan's approval — read off the pane (/api/dialog) and shown in
// the compose box above its field, the box being the session's foot on a desktop and on a phone.
// A tap answers it (/api/answer): the server reads the pane again, refuses a dialog that is not
// the one shown (its fingerprint) and types the answer. A written answer goes from the box's own
// field. A permission prompt offers Yes and No only; the server enforces the same.

const json = response => response.json().catch(() => ({ ok: false, error: `HTTP ${response.status}` }));
const target = row => encodeURIComponent(row?.id || "");

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

const KIND = { question: "Claude asks", review: "Claude asks", permission: "Claude wants to run", plan: "Claude has a plan" };

export class DialogCard {
  // `toast(text)`, `refreshIndex()`, `askGrant(then)` (the compose box's grant, which runs `then`
  // once granted), `writeHere()` (the field, for "tell Claude what to do instead").
  constructor(actions) {
    this.actions = actions;
    this.box = el("div", "compose-dialog");
    this.box.id = "composeDialog";
    this.box.hidden = true;
    this.box.setAttribute("role", "group");
    this.key = "";
    this.row = null;
    this.dialog = null;
    this.busy = false;
    this.loads = 0;
  }
  mount(before) { before.before(this.box); }
  // The open box's row (null when the box is shut or resumes an idle session): load its dialog when
  // the row says it holds one, and again when that wait is a new one.
  sync(row) {
    if (!row?.dialog) { this.clear(); return; }
    this.row = row;
    const key = `${row.id}|${row.dialog}|${row.stateSince || ""}`;
    if (key === this.key) return;
    this.key = key;
    this.load();
  }
  clear() {
    this.key = ""; this.row = null; this.dialog = null;
    this.box.hidden = true;
    this.box.replaceChildren();
    this.box.dispatchEvent(new Event("compose-dialog", { bubbles: true }));
  }
  get active() { return !this.box.hidden && !!this.dialog; }
  get freeText() { return this.dialog?.choices.find(c => c.freeText) || null; }
  async load() {
    const row = this.row, load = ++this.loads;
    if (!row) return;
    this.show([el("p", "dialog-wait", "Reading the session's dialog…")]);
    let d;
    try { d = await fetch(`/api/dialog?target=${target(row)}`, { cache: "no-store" }).then(json); } catch (_) { d = { ok: false, error: "could not reach the monitor" }; }
    if (load !== this.loads || this.row !== row) return;
    this.dialog = null;
    if (d.code === "no-consent") { this.showGrant(); return; }
    if (!d.ok) { this.show([el("p", "dialog-wait", d.error || "The dialog could not be read: answer it in the session.")]); return; }
    // Nothing open (answered, here or in the session): no card. Open but not a dialog this
    // monitor reads: say so, rather than offer anything to tap.
    if (!d.dialog && !d.open) { this.clear(); return; }
    if (!d.dialog) { this.show([el("p", "dialog-wait", "Claude is waiting on something the monitor cannot read here: answer it in the session.")]); return; }
    this.dialog = d.dialog;
    this.render(d.dialog);
  }
  show(children) {
    this.box.replaceChildren(...children);
    this.box.hidden = false;
    this.box.dispatchEvent(new Event("compose-dialog", { bubbles: true }));
  }
  showGrant() {
    const allow = el("button", "dialog-choice primary", "Allow");
    allow.type = "button";
    allow.onclick = () => this.actions.askGrant(() => { this.key = ""; this.sync(this.row); });
    const where = this.row?.target ? `herdr pane ${this.row.target}` : "its herdr pane";
    const choices = el("div", "dialog-choices");
    choices.append(allow);
    this.show([el("p", "dialog-text", "Claude is waiting on you."), el("p", "dialog-note", `To see and answer it here, allow this monitor to type into ${where}.`), choices]);
  }
  render(d) {
    const parts = [];
    const head = el("div", "dialog-head");
    head.append(el("span", "dialog-kind", KIND[d.kind] || "Claude asks"));
    if (d.title) head.append(el("span", "dialog-title", d.title));
    parts.push(head);
    if (d.tabs?.length) {
      const tabs = el("div", "dialog-tabs");
      for (const t of d.tabs) tabs.append(el("span", t.done ? "dialog-tab done" : "dialog-tab", `${t.done ? "✓ " : ""}${t.label}`));
      parts.push(tabs);
    }
    if (d.note) parts.push(el("p", "dialog-note", d.note));
    parts.push(el("p", "dialog-text", d.text));
    const detail = d.kind === "plan" ? (d.plan || d.detail) : d.detail;
    if (detail) parts.push(el("pre", `dialog-detail dialog-${d.kind}`, detail));
    const choices = el("div", "dialog-choices");
    if (d.multi) {
      for (const c of d.choices.filter(c => !c.freeText)) {
        const label = el("label", "dialog-choice dialog-tick");
        const box = el("input");
        box.type = "checkbox"; box.checked = !!c.checked; box.dataset.choice = String(c.n);
        const words = el("span", "dialog-words");
        words.append(el("b", "", c.label));
        if (c.description) words.append(el("span", "dialog-desc", c.description));
        label.append(box, words);
        choices.append(label);
      }
      const next = el("button", "dialog-choice primary dialog-next", "Next");
      next.type = "button";
      next.onclick = () => this.answer({ choices: [...choices.querySelectorAll("input[data-choice]:checked")].map(i => Number(i.dataset.choice)) });
      choices.append(next);
    } else {
      for (const c of d.choices.filter(c => !c.freeText)) {
        const b = el("button", "dialog-choice");
        b.type = "button"; b.dataset.choice = String(c.n);
        b.append(el("b", "", c.label));
        if (c.description) b.append(el("span", "dialog-desc", c.description));
        b.onclick = () => this.answer({ choice: c.n });
        choices.append(b);
      }
      // A permission's No, then the field: "tell Claude what to do instead" is the next message.
      const no = d.kind === "permission" && d.choices.find(c => c.label.startsWith("No"));
      if (no) {
        const b = el("button", "dialog-choice dialog-instead");
        b.type = "button";
        b.append(el("b", "", "No, and tell Claude what to do instead"));
        b.onclick = async () => { if (await this.answer({ choice: no.n })) this.actions.writeHere(); };
        choices.append(b);
      }
    }
    parts.push(choices);
    const free = this.freeText;
    if (free) parts.push(el("p", "dialog-hint", d.kind === "plan" ? "Or tell Claude what to change, below." : "Or write your own answer, below."));
    this.show(parts);
  }
  // Answer the dialog shown; true when the server typed it. A held answer says why and reads the
  // pane again, since what it shows has changed.
  async answer(body) {
    if (!this.dialog || !this.row || this.busy) return false;
    this.busy = true;
    this.box.classList.add("answering");
    const row = this.row;
    let d;
    try {
      d = await fetch(`/api/answer?target=${target(row)}`, { method: "POST", body: JSON.stringify({ fingerprint: this.dialog.fingerprint, ...body }), cache: "no-store" }).then(json);
    } catch (_) { d = { ok: false, error: "could not reach the monitor" }; }
    this.busy = false;
    this.box.classList.remove("answering");
    if (d.code === "no-consent") { this.showGrant(); return false; }
    this.actions.toast(d.ok ? "Answered" : (d.error || "The answer did not go through"));
    // The next part of a question, or the dialog gone: read the pane again once it has redrawn.
    this.key = "";
    setTimeout(() => { if (this.row === row) this.sync(row); this.actions.refreshIndex(); }, 600);
    return !!d.ok;
  }
  answerText(text) { return this.answer({ text }); }
}
