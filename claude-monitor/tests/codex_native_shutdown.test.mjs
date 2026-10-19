import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";
import { confirmNativeShutdown, ownsNativeServer, readNativeStatus } from "../src/codex-ui/native-menu.js";

const source = await readFile(new URL("../src/codex-ui/native-mode.js", import.meta.url), "utf8");
const external = { ok: true, runtime: "activated", serverListening: true, homeProvenance: "configured", ownedServerPid: null };
const owned = { ...external, homeProvenance: "monitor-started", ownedServerPid: 123 };
const tick = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }

function fixture(status = owned) {
  const nodes = new Map(), document = { activeElement: null };
  class Element extends EventTarget {
    constructor(tag) { super(); this.tagName = tag; this.children = []; this.attributes = {}; this.isConnected = true; this.disabled = false; }
    set id(value) { this._id = value; nodes.set(value, this); } get id() { return this._id; }
    append(...children) { this.children.push(...children); }
    setAttribute(key, value) { this.attributes[key] = value; }
    focus() { if (!this.disabled) document.activeElement = this; }
    matches() { return false; }
    showModal() { this.open = true; }
    close(value) { this.open = false; this.returnValue = value; if (!this.deferClose) this.dispatchEvent(new Event("close")); }
    remove() { this.isConnected = false; nodes.delete(this.id); }
  }
  document.createElement = tag => new Element(tag); document.body = new Element("body");
  document.getElementById = id => { if (!nodes.has(id)) { const node = new Element("button"); node.id = id; } return nodes.get(id); };
  const controlState = { write: true }, menu = new Element("div"), button = document.getElementById("codexNativeButton");
  const calls = [], toasts = [], events = [], result = deferred(); let statusError;
  const write = document.getElementById("writeSwitch"); write.onclick = () => { controlState.write = !controlState.write; };
  class GatewayApi { request(route, body) { calls.push({ route, body }); return result.promise; } }
  const context = { document, controlState, indexState: { selected: null }, ResizeObserver: class { observe() {} }, visualViewport: null,
    mountNativeMenu: () => ({ button, menu }), readNativeStatus, ownsNativeServer, confirmNativeShutdown, GatewayApi,
    fetch: async () => { calls.push({ route: "status" }); if (statusError) throw statusError; return { ok: true, json: async () => status }; } };
  const NativeMode = vm.runInNewContext(source.replace(/^import .*\n/gm, "").replace("export class NativeModeControls", "class NativeModeControls")
    .replaceAll('await import("/codex-workspace/api.js")', 'await Promise.resolve({ GatewayApi: globalThis.GatewayApi })') + "\nNativeModeControls", context);
  NativeMode.prototype.paint = function () { button.disabled = this.pending; };
  const controller = new NativeMode({ actions: {} }, {
    toast: message => toasts.push(message), pauseIndex: () => events.push("pause"), resumeIndex: () => events.push("resume"),
    refreshIndex() {}, nativeDisabled() {},
  });
  controller.mode = "on"; controller.status = status;
  controller.native = { suspendEvents: () => events.push("suspend"), resumeEvents: () => events.push("subscribe") };
  return { controller, controlState, document, nodes, calls, toasts, events, result, write, button,
    stopCalls: () => calls.filter(call => call.route === "stop"),
    invoke(route) { if (route === "write") { write.focus(); return write.onclick({}); } button.focus(); return controller.disable(); },
    failStatus(error) { statusError = error; },
    complete() { result.resolve({ ok: true, runtime: "stopped", serverListening: false }); },
  };
}

for (const route of ["native", "write"]) {
  for (const cancel of ["button", "escape"]) test(`${route}: ${cancel} cancellation keeps both modes, sessions and server unchanged`, async () => {
    const f = fixture(), pending = f.invoke(route); await tick();
    const dialog = f.nodes.get("nativeStopDialog");
    assert.equal(dialog.open, true); assert.equal(f.document.activeElement.id, "nativeStopCancel");
    assert.equal(dialog.attributes["aria-describedby"], "nativeStopDescription");
    assert.equal(f.controller.enabled, true); assert.equal(f.controller.pending, true);
    assert.equal(f.controlState.write, true); assert.equal(f.stopCalls().length, 0); assert.deepEqual(f.events, []);
    assert.equal(await f.controller.disable(), false, "Repeated close requests do not bypass the pending confirmation");
    if (cancel === "button") f.nodes.get("nativeStopCancel").onclick();
    else dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
    await pending;
    assert.equal(f.nodes.has("nativeStopDialog"), false);
    assert.equal(f.controller.mode, "on"); assert.equal(f.controlState.write, true);
    assert.equal(f.stopCalls().length, 0); assert.deepEqual(f.events, []);
    assert.equal(f.document.activeElement.id, route === "write" ? "writeSwitch" : "codexNativeButton");
  });

  test(`${route}: confirmation sends one stop and leaves switches on until it completes`, async () => {
    const f = fixture(), pending = f.invoke(route); await tick();
    f.nodes.get("nativeStopConfirm").onclick(); await tick();
    assert.equal(f.stopCalls().length, 1); assert.equal(f.controller.mode, "disabling");
    assert.equal(f.controller.enabled, true); assert.equal(f.controlState.write, true);
    await f.invoke(route); assert.equal(f.stopCalls().length, 1);
    f.complete(); await pending;
    assert.equal(f.controller.mode, "off"); assert.equal(f.controlState.write, route === "native");
    assert.deepEqual(f.events, ["pause", "suspend", "resume"]);
  });

  test(`${route}: external server detaches without opening a shutdown confirmation`, async () => {
    const f = fixture(external); f.controller.status = owned;
    const pending = f.invoke(route); await tick();
    assert.equal(f.nodes.has("nativeStopDialog"), false); assert.equal(f.stopCalls().length, 1);
    f.complete(); await pending; assert.equal(f.controller.enabled, false);
    assert.equal(f.controlState.write, route === "native");
  });

  test(`${route}: failed shutdown preserves enabled switches and exposes the failure`, async () => {
    const f = fixture(), pending = f.invoke(route); await tick();
    f.nodes.get("nativeStopConfirm").onclick(); await tick();
    f.result.reject(new Error("Stop owned Codex server: test failure")); await pending;
    assert.equal(f.controller.mode, "on"); assert.equal(f.controlState.write, true);
    assert.match(f.toasts[0], /Stop owned Codex server/);
    assert.deepEqual(f.events, ["pause", "suspend", "subscribe", "resume"]);
    assert.equal(f.controller.pending, false);
  });
}

test("shutdown reads current ownership instead of trusting an external cached status", async () => {
  const f = fixture(owned); f.controller.status = external;
  const pending = f.invoke("native"); await tick();
  assert.equal(f.nodes.get("nativeStopDialog").open, true); assert.equal(f.stopCalls().length, 0);
  f.nodes.get("nativeStopCancel").onclick(); await pending;
});

test("unavailable ownership does not silently stop a potentially shared server", async () => {
  const f = fixture(); f.failStatus(new Error("Native status HTTP 503"));
  await f.invoke("write"); assert.equal(f.stopCalls().length, 0);
  assert.equal(f.controller.mode, "on"); assert.equal(f.controlState.write, true);
  assert.match(f.toasts[0], /503/); assert.deepEqual(f.events, []);
});

test("ordinary Write mode toggles without reading Codex status or asking to stop it", async () => {
  const f = fixture(); f.controller.mode = "off";
  await f.invoke("write"); assert.equal(f.controlState.write, false); assert.deepEqual(f.calls, []);
  assert.equal(f.nodes.has("nativeStopDialog"), false);
});

test("shutdown ownership requires a valid Monitor-owned process identity", () => {
  assert.equal(ownsNativeServer(owned), true);
  for (const status of [null, external, { ...owned, ownedServerPid: "123" }, { ...owned, ownedServerPid: 0 }, { ...owned, ownedServerPid: null }]) {
    assert.equal(ownsNativeServer(status), false);
  }
});

test("confirmation settles even when the browser defers its native dialog close event", async () => {
  const f = fixture(), pending = f.invoke("native"); await tick();
  const dialog = f.nodes.get("nativeStopDialog"); dialog.deferClose = true;
  f.nodes.get("nativeStopConfirm").onclick(); await tick();
  assert.equal(f.nodes.has("nativeStopDialog"), false); assert.equal(f.stopCalls().length, 1);
  dialog.dispatchEvent(new Event("close"));
  f.complete(); await pending; assert.equal(f.controller.enabled, false);
});
