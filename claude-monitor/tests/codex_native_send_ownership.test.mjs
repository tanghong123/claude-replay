import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

const controlSource = await readFile(new URL("../src/codex-ui/control-store.js", import.meta.url), "utf8");
const nativeSource = await readFile(new URL("../src/codex-ui/native-mode.js", import.meta.url), "utf8");
const loadClass = (source, name, context) => vm.runInNewContext(source.replace(/^import .*\n/gm, "").replace(`export class ${name}`, `class ${name}`) + `\n${name}`, context);

function fixture() {
  const input = { value: "Claude send" }, send = { disabled: false };
  const document = { getElementById: id => id === "composeInput" ? input : send };
  const controlState = { row: { id: "claude" }, mode: "resume", consented: false };
  const indexState = { selected: "claude" };
  let finish, payload, closed = 0, painted = 0;
  const NativeMode = loadClass(nativeSource, "NativeModeControls", { document, controlState, indexState });
  const native = Object.assign(Object.create(NativeMode.prototype), { view: "ordinary", composerGeneration: 0, drafts: new Map(), draftRevisions: new Map() });
  const Control = loadClass(controlSource, "ControlStore", { document, controlState, fetch: () => {}, runSend: value => { payload = value; return new Promise(resolve => { finish = resolve; }); } });
  const ordinary = Object.assign(Object.create(Control.prototype), { actions: { captureComposer: () => native.captureOrdinarySend(), repaintComposer: () => painted++, toast: () => {}, refreshIndex: () => {} }, close: () => closed++ });
  return { input, send, native, ordinary, controlState, indexState, get payload() { return payload; }, get closed() { return closed; }, get painted() { return painted; }, finish: () => finish({ consented: true, step: "send", outcome: { kind: "sent" } }) };
}

test("ordinary accepted sending retains its close and consent behavior", async () => {
  const f = fixture(), pending = f.ordinary.send("Claude send");
  f.finish(); await pending;
  assert.equal(f.input.value, ""); assert.equal(f.closed, 1); assert.equal(f.controlState.consented, true);
  assert.equal(f.payload.target, "claude"); assert.equal(f.send.disabled, false);
});

test("late Claude acceptance retires its draft without mutating a native editor", async () => {
  const f = fixture(), pending = f.ordinary.send("Claude send");
  f.native.drafts.set("claude", "Claude send"); f.native.view = "native"; f.native.composerGeneration++;
  f.indexState.selected = "codex"; f.controlState.row = { id: "codex" }; f.input.value = "Codex draft";
  f.finish(); await pending;
  assert.equal(f.input.value, "Codex draft"); assert.equal(f.closed, 0); assert.equal(f.controlState.consented, false);
  assert.equal(f.native.drafts.has("claude"), false); assert.equal(f.painted, 1);
});

test("a newer identical Claude draft survives acceptance of the previous revision", async () => {
  const f = fixture(), pending = f.ordinary.send("Claude send");
  f.native.draftRevisions.set("claude", 1); f.native.drafts.set("claude", "Claude send");
  f.finish(); await pending;
  assert.equal(f.input.value, "Claude send"); assert.equal(f.closed, 0); assert.equal(f.native.drafts.get("claude"), "Claude send");
});
