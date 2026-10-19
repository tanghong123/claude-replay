import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";
import { appendDesktopHelp, renderNativeMenu, renderNativeServerStatus, nativeServerPresentation, positionNativeMenu, readNativeStatus } from "../src/codex-ui/native-menu.js";

const source = await readFile(new URL("../src/codex-ui/native-mode.js", import.meta.url), "utf8");
const available = { ok: true, serverListening: true, runtime: "available" };

function fixture(initial, clipboard) {
  let response = initial, starts = 0, stops = 0;
  const node = (tagName = "div") => ({ tagName, hidden: false, disabled: false, title: "", dataset: {}, children: [],
    attributes: {}, setAttribute(key, value) { this.attributes[key] = value; }, getAttribute(key) { return this.attributes[key]; }, matches() { return false; }, focus() {}, showPopover() {},
    querySelector(selector) { return this.children.find(child => `#${child.id}` === selector); },
    replaceChildren(...children) { this.children = children; }, append(...children) { this.children.push(...children); } });
  const nodes = new Map();
  const document = { createElement: node, getElementById: id => { if (!nodes.has(id)) nodes.set(id, node()); return nodes.get(id); } };
  const calls = [], toasts = [];
  const context = { appendDesktopHelp, renderNativeMenu, renderNativeServerStatus, positionNativeMenu, readNativeStatus, document, navigator: { clipboard }, controlState: { write: true }, indexState: { selected: null }, selectedRow: () => null,
    usesNativeControls: () => false, isCodexSession: () => false, fetch: async (path, options) => { calls.push({ path, options }); return response; } };
  const NativeMode = vm.runInNewContext(source.replace(/^import .*\n/gm, "").replace("export class NativeModeControls", "class NativeModeControls") + "\nNativeModeControls", context);
  const controller = Object.assign(Object.create(NativeMode.prototype), { mode: "off", view: "ordinary", ordinary: { paint() {} },
    actions: { toast(message) { toasts.push(message); } },
    choice: node(), button: node(), newButton: node(), menu: Object.assign(node(), { ownerDocument: document }), appendDesktopHelp() {}, positionMenu() {},
    enable() { starts++; }, disable() { stops++; } });
  return { controller, calls, toasts, get starts() { return starts; }, get stops() { return stops; }, respond(value) { response = value; } };
}

for (const clipboardAllowed of [true, false]) test(`shared App help renders and copies only the setup prompt (clipboard allowed: ${clipboardAllowed})`, async () => {
  const copied = [];
  const f = fixture(null, { async writeText(text) {
    if (!clipboardAllowed) throw new Error("Clipboard unavailable over HTTP");
    copied.push(text);
  } });
  const help = {
    title: "Share this server with Codex App",
    description: "Continue the same live session on desktop and mobile.",
    steps: ["Save your work before reopening the App.", "Reconnect here."],
    prompt: "Connect Codex App to ws://127.0.0.1:4500; discover its path and CODEX_HOME.",
    endpoint: "ws://127.0.0.1:4500",
    codexHome: "/Users/private-user/.codex",
    command: "env CODEX_HOME='/Users/private-user/.codex' '/custom/Private App'",
  };
  f.controller.status = { desktopHelp: help };
  delete f.controller.appendDesktopHelp;
  f.controller.appendDesktopHelp(f.controller.menu);
  const details = f.controller.menu.children[0];
  assert.deepEqual(details.children.map(child => [child.tagName, child.textContent]), [
    ["summary", help.title], ["p", help.description], ["pre", help.prompt],
    ["button", "Copy setup prompt"], ...help.steps.map(step => ["p", step]),
  ]);
  await details.children.find(child => child.tagName === "button").onclick();
  assert.deepEqual(copied, clipboardAllowed ? [help.prompt] : []);
  assert.deepEqual(f.toasts, [clipboardAllowed ? "Setup prompt copied" : "Select and copy the displayed prompt"]);
  assert.equal(f.calls.length, 0, "Reading and copying help do not contact or activate Codex.");
  assert.equal(f.starts, 0);
});

test("server status distinguishes process availability, client connection and proven launch ownership", () => {
  const status = { ...available, transport: "unix", connection: "connected" };
  const existing = nativeServerPresentation({ enabled: true, status });
  assert.equal(existing.label, "Running"); assert.match(existing.detail, /Using an existing server.*Monitor is connected/);
  assert.equal(existing.transport, "Private Unix socket");
  const owned = { ...status, homeProvenance: "monitor-started", ownedServerPid: 123 };
  assert.match(nativeServerPresentation({ enabled: true, status: owned }).detail, /Started by this Monitor/);
  assert.doesNotMatch(nativeServerPresentation({ enabled: true, status: { ...owned, ownedServerPid: null } }).detail, /Started by/);
  assert.doesNotMatch(nativeServerPresentation({ enabled: true, status: { ...owned, ownedServerPid: "123" } }).detail, /Started by/);
  const off = nativeServerPresentation({ enabled: false, status: owned });
  assert.equal(off.label, "Running"); assert.match(off.detail, /Native mode is off/); assert.match(off.note, /stops this server/);
  const lost = nativeServerPresentation({ enabled: true, status: { ...status, serverListening: false } });
  assert.equal(lost.label, "Not running"); assert.match(lost.detail, /lost its server connection/);
  assert.equal(nativeServerPresentation({ enabled: true, status: { ...status, statusError: "HTTP 503" } }).label, "Status unavailable");
  assert.equal(nativeServerPresentation({ enabled: false, status, activationFailed: true }).label, "Connection failed");
});

test("pending activation is visible without claiming that the server is running", async () => {
  for (const listening of [false, true]) {
    const status = { ok: true, runtime: listening ? "available" : "stopped", serverListening: listening };
    const f = fixture({ ok: true, json: async () => status });
    await f.controller.openMenu();
    const section = f.controller.menu.querySelector("#nativeServerStatus"), action = f.controller.menu.children[1];
    assert.equal(section.attributes.role, "status");
    await action.onclick();
    assert.equal(section.dataset.state, "pending");
    assert.equal(section.children[0].children[1].textContent, listening ? "Connecting…" : "Starting…");
    renderNativeServerStatus(f.controller.menu, { enabled: false, status, activationFailed: true });
    assert.equal(section.dataset.state, "error");
    assert.equal(f.controller.menu.children[1], action, "Updating server status retains the focused toggle.");
    assert(f.calls.every(call => call.path === "/api/codex/status" && !call.options.method));
  }
});

for (const failure of [
  { ok: false, status: 503 },
  ...[null, [], {}, { ok: true, serverListening: false }].map(value => ({ ok: true, json: async () => value })),
]) test("unknown native status exposes its cause and retries instead of offering Start", async () => {
  const f = fixture(failure);
  await f.controller.openMenu();
  assert.equal(Object.hasOwn(f.controller.status, "serverListening"), false);
  assert.match(f.controller.button.title, /Cannot check Codex server status/);
  assert.match(f.controller.menu.children[2].textContent, /HTTP 503|invalid response envelope/);
  assert.equal(f.controller.menu.children[1].children[0].textContent, "Retry connection status");
  assert.equal(f.starts, 0); assert.equal(f.calls.length, 1);
  f.respond({ ok: true, json: async () => available });
  await f.controller.menu.children[1].onclick();
  assert.equal(f.controller.menu.children[1].attributes["aria-checked"], "false");
  assert.equal(f.starts, 0, "Status recovery never activates Codex automatically.");
  assert(f.calls.every(call => call.path === "/api/codex/status" && !call.options.method));
});

test("a confirmed stopped native server stays clickable while status failure cannot block turning mode off", async () => {
  const f = fixture({ ok: true, json: async () => ({ ok: true, serverListening: false, runtime: "stopped" }) });
  await f.controller.openMenu();
  assert.match(f.controller.button.title, /Server is not running/);
  assert.equal(f.controller.menu.children[1].attributes["aria-checked"], "false");
  f.controller.menu.children[1].onclick(); assert.equal(f.starts, 1);
  f.controller.mode = "on"; f.respond({ ok: false, status: 503 });
  await f.controller.openMenu();
  assert.equal(f.controller.menu.children[1].attributes["aria-checked"], "true");
  f.controller.menu.children[1].onclick(); assert.equal(f.stops, 1);
});

for (const stage of ["headers", "body"]) test(`native discovery bounds stalled ${stage} and permits an explicit retry`, async () => {
  let expire, signal, began = false; const cancelled = [];
  const held = readNativeStatus(async (_path, options) => {
    signal = options.signal; began = true;
    if (stage === "headers") return new Promise(() => {});
    return { ok: true, json: () => new Promise(() => {}) };
  }, { schedule(operation) { expire = operation; return 7; }, cancel(id) { cancelled.push(id); } });
  while (!began) await Promise.resolve(); expire();
  await assert.rejects(held, error => /timed out/.test(error.message) && /deadline/.test(error.cause.message));
  assert.equal(signal.aborted, true); assert.deepEqual(cancelled, [7]);
  assert.deepEqual(await readNativeStatus(async () => ({ ok: true, json: async () => available })), available);
});


test("closing distinguishes owned shutdown from external connection release", () => {
  const external = { ...available, connection: "connected" };
  const managed = { ...external, homeProvenance: "monitor-started", ownedServerPid: 123 };
  assert.match(nativeServerPresentation({ enabled: true, status: external }).note, /externally managed/);
  assert.match(nativeServerPresentation({ enabled: true, status: managed }).note, /stops this server.*Codex apps will disconnect/);
  assert.equal(nativeServerPresentation({ enabled: true, status: managed, pending: true, closing: true }).label, "Closing…");
  const f = fixture(null);
  f.controller.mode = "disabling";
  assert.equal(f.controller.enabled, true, "Keep native controls in place until cleanup finishes");
  assert.equal(f.controller.pending, true, "Repeated toggle clicks must not issue another stop");
  f.controller.paint();
  assert.equal(f.controller.button.disabled, true);
  assert.equal(f.controller.button.title, "Closing Codex native…");
});

test("server readiness updates the existing switch and description before a slow history scan finishes", async () => {
  const f = fixture({ ok: true, json: async () => available });
  await f.controller.openMenu();
  const action = f.controller.menu.querySelector("#nativeEnable");
  f.controller.menu.matches = () => true;
  f.controller.ensureNative = async () => {};
  let scanning = false;
  f.controller.actions.refreshIndex = () => { scanning = true; return new Promise(() => {}); };
  await f.controller.adopt({ ...available, runtime: "activated", connection: "connected" });
  assert.equal(scanning, true);
  assert.equal(action.attributes["aria-checked"], "true"); assert.equal(action.disabled, false);
  assert.match(f.controller.menu.querySelector("#nativeModeDescription").textContent, /Choose Codex models/);
  f.controller.mode = "disabling"; f.controller.paint();
  assert.equal(action.disabled, true); assert.equal(action.attributes["aria-checked"], "true");
  f.controller.mode = "off"; f.controller.paint();
  assert.equal(action.disabled, false); assert.equal(action.attributes["aria-checked"], "false");
  assert.equal(f.controller.menu.querySelector("#nativeEnable"), action);
});
