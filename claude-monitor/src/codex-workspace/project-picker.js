import { svg } from "/monitor-ui/icons.js";

/** @typedef {{id:string, label:string, detail?:string}} ProjectChoice */

/** A compact project listbox; IDs own selection and labels remain display-only text. */
export class ProjectPicker {
  /** @param {{id:string, label:string, onChange:(id:string)=>void}} config */
  constructor({ id, label, onChange }) {
    this.onChange = onChange; this.options = []; this.abort = new AbortController();
    this.root = document.createElement("div"); this.root.className = "shared-project-picker";
    this.button = document.createElement("button"); this.button.id = id; this.button.type = "button"; this.button.className = "shared-project-trigger";
    this.button.setAttribute("aria-label", label); this.button.setAttribute("aria-haspopup", "listbox"); this.button.setAttribute("aria-expanded", "false"); this.button.setAttribute("aria-controls", `${id}Options`);
    this.button.innerHTML = `${svg("folder")}<span class="shared-project-label"></span>${svg("caret")}`;
    this.menu = document.createElement("div"); this.menu.id = `${id}Options`; this.menu.className = "shared-project-menu"; this.menu.setAttribute("popover", "auto"); this.menu.setAttribute("role", "listbox"); this.menu.setAttribute("aria-label", label);
    // Keep the menu inside its owner: a body sibling would be inert in a modal dialog.
    this.root.append(this.button, this.menu);
    this.button.onclick = () => this.menu.matches(":popover-open") ? this.close(false) : this.open();
    this.button.onkeydown = event => { if (["ArrowDown", "ArrowUp"].includes(event.key)) { event.preventDefault(); event.stopPropagation(); this.open(event.key === "ArrowUp" ? -1 : 0); } };
    this.menu.onkeydown = event => this.keydown(event);
    this.menu.addEventListener("toggle", event => { this.button.setAttribute("aria-expanded", String(event.newState === "open")); });
    const reposition = () => { if (this.menu.matches(":popover-open")) this.position(); };
    for (const [target, event] of [[window, "resize"], [window.visualViewport, "resize"], [window.visualViewport, "scroll"]]) target?.addEventListener(event, reposition, { signal: this.abort.signal });
  }
  /**
   * Replace catalog entries without invoking user selection; rejects empty/ambiguous choices.
   * @param {ProjectChoice[]} options
   * @param {string} value
   */
  setOptions(options, value = "") {
    if (!Array.isArray(options) || !options.length) throw new TypeError("Project choices must be a nonempty array");
    const ids = new Set();
    for (const [index, option] of options.entries()) {
      if (typeof option.id !== "string" || typeof option.label !== "string" || (option.detail !== undefined && typeof option.detail !== "string") || ids.has(option.id)) throw new TypeError(`Invalid project choice at index ${index}: unique string IDs and text are required`);
      ids.add(option.id);
    }
    this.close(false); this.options = options; this.menu.replaceChildren();
    for (const option of options) {
      const row = document.createElement("button"); row.type = "button"; row.className = "shared-project-option"; row.setAttribute("role", "option"); row.dataset.projectValue = option.id; row.tabIndex = -1;
      row.innerHTML = `${svg("folder")}<span class="shared-project-copy"><strong></strong><small></small></span><svg class="icon shared-project-check" viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4 10-10"/></svg>`;
      row.querySelector("strong").textContent = option.label; row.querySelector("small").textContent = option.detail || ""; row.title = option.detail || option.label;
      row.onclick = () => { if (this.button.disabled || this.abort.signal.aborted) return; this.setValue(option.id); this.close(true); this.onChange(option.id); };
      this.menu.append(row);
    }
    this.setValue(ids.has(value) ? value : options[0]?.id || "");
  }
  get value() { return this.button.value; }
  /** Update selected display only; callbacks fire exclusively for an explicit user choice. */
  setValue(value) {
    const option = this.options.find(option => option.id === value);
    if (!option) throw new TypeError("Cannot select a project ID absent from the current choices");
    this.button.value = value; this.button.querySelector(".shared-project-label").textContent = option.label; this.button.title = option.detail || option.label;
    for (const row of this.menu.children) row.setAttribute("aria-selected", String(row.dataset.projectValue === value));
  }
  open(edge) {
    if (this.button.disabled || this.abort.signal.aborted || !this.options.length) return;
    if (!this.menu.matches(":popover-open")) this.menu.showPopover();
    this.button.setAttribute("aria-expanded", "true"); this.position();
    const rows = [...this.menu.children], selected = rows.findIndex(row => row.dataset.projectValue === this.value);
    this.focus(rows, edge === -1 ? rows.length - 1 : edge === 0 ? 0 : Math.max(0, selected));
  }
  close(focus = false) {
    if (this.menu.matches(":popover-open")) this.menu.hidePopover();
    this.button.setAttribute("aria-expanded", "false"); if (focus && !this.button.disabled) this.button.focus();
  }
  focus(rows, index) { rows.forEach((row, i) => { row.tabIndex = i === index ? 0 : -1; }); rows[index]?.focus({ preventScroll: true }); rows[index]?.scrollIntoView({ block: "nearest" }); }
  keydown(event) {
    const rows = [...this.menu.children], index = rows.indexOf(document.activeElement);
    if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      event.preventDefault(); event.stopPropagation();
      const next = event.key === "Home" ? 0 : event.key === "End" ? rows.length - 1 : (index + (event.key === "ArrowDown" ? 1 : rows.length - 1)) % rows.length;
      this.focus(rows, next);
    } else if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); this.close(true); }
    else if (event.key === "Tab") { this.close(true); }
    else if (["Enter", " "].includes(event.key)) { event.preventDefault(); event.stopPropagation(); rows[index]?.click(); }
  }
  position() {
    const anchor = this.button.getBoundingClientRect(), view = window.visualViewport, style = getComputedStyle(this.menu);
    const safe = edge => parseFloat(style.getPropertyValue(`--safe-${edge}`)) || 0;
    const left = (view?.offsetLeft || 0) + safe("left") + 8, top = (view?.offsetTop || 0) + safe("top") + 8;
    const right = (view?.offsetLeft || 0) + (view?.width || innerWidth) - safe("right") - 8, bottom = (view?.offsetTop || 0) + (view?.height || innerHeight) - safe("bottom") - 8;
    const below = Math.max(0, bottom - anchor.bottom - 6), above = Math.max(0, anchor.top - top - 6);
    const height = Math.min(320, Math.max(below, above));
    this.menu.style.width = `${Math.max(0, Math.min(320, Math.max(240, anchor.width), right - left))}px`; this.menu.style.maxHeight = `${height}px`;
    const box = this.menu.getBoundingClientRect(), under = below >= box.height || below >= above;
    this.menu.style.left = `${Math.max(left, Math.min(anchor.left, right - box.width))}px`;
    this.menu.style.top = `${Math.max(top, Math.min(under ? anchor.bottom + 6 : anchor.top - box.height - 6, bottom - box.height))}px`;
  }
  /** Release viewport listeners and close the old menu before its form is replaced. */
  destroy() { this.close(false); this.abort.abort(); this.root.remove(); }
}
