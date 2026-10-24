import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

const controlSource = await readFile(new URL("../src/codex-ui/control-store.js", import.meta.url), "utf8");
const nativeSource = await readFile(new URL("../src/codex-ui/native-mode.js", import.meta.url), "utf8");
const loadClass = (source, name, context) => vm.runInNewContext(source.replace(/^import .*\n/gm, "").replace(`export class ${name}`, `class ${name}`) + `\n${name}`, context);

function fixture() {
  const input = { value: "Claude send", focused: 0, dispatchEvent() {}, focus() { this.focused++; } }, send = { disabled: false };
  const document = { getElementById: id => id === "composeInput" ? input : send, activeElement: null };
  const controlState = { row: { id: "claude" }, mode: "resume", consented: false };
  const indexState = { selected: "claude" };
  let finish, payload, closed = 0, painted = 0;
  const NativeMode = loadClass(nativeSource, "NativeModeControls", { document, controlState, indexState });
  const native = Object.assign(Object.create(NativeMode.prototype), { view: "ordinary", composerGeneration: 0, drafts: new Map(), draftRevisions: new Map() });
  const Control = loadClass(controlSource, "ControlStore", { document, controlState, selectedRow: () => ({ id: indexState.selected }), fetch: () => {}, Event: class { constructor(type) { this.type = type; } }, matchMedia: () => ({ matches: false }), runSend: value => { payload = value; return new Promise(resolve => { finish = resolve; }); } });
  const ordinary = Object.assign(Object.create(Control.prototype), { actions: { captureComposer: () => native.captureOrdinarySend(), repaintComposer: () => painted++, toast: () => {}, refreshIndex: () => {} }, close: () => closed++ });
  return { input, send, native, ordinary, controlState, indexState, get payload() { return payload; }, get closed() { return closed; }, get painted() { return painted; }, finish: () => finish({ consented: true, step: "send", outcome: { kind: "sent" } }) };
}

// #s236 (Hong): a sent message empties the field and KEEPS the box open for the next one.
test("ordinary accepted sending empties the field, keeps the box open and keeps its consent", async () => {
  const f = fixture(), pending = f.ordinary.send("Claude send");
  f.finish(); await pending;
  assert.equal(f.input.value, ""); assert.equal(f.closed, 0); assert.equal(f.input.focused, 1); assert.equal(f.controlState.consented, true);
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

for (const draft of ["deploy now ", "deploy now\n", "  deploy now\n"]) test(`ordinary send retires the exact raw draft ${JSON.stringify(draft)}`, async () => {
  const f = fixture(); f.input.value = draft; delete f.ordinary.actions.captureComposer;
  const pending = f.ordinary.send(draft.trim()); f.finish(); await pending;
  assert.equal(f.input.value, ""); assert.equal(f.closed, 0);
});
test("closing an ordinary editor while sending still retires its unchanged draft", async () => {
  const f = fixture(); delete f.ordinary.actions.captureComposer;
  const pending = f.ordinary.send("Claude send"); f.controlState.row = null;
  // Closed by the reader already: emptied, and not closed a second time.
  f.finish(); await pending; assert.equal(f.input.value, ""); assert.equal(f.closed, 0); assert.equal(f.input.focused, 0);
});
test("an ordinary send preserves edits made while the request is pending", async () => {
  const f = fixture(); delete f.ordinary.actions.captureComposer;
  const pending = f.ordinary.send("Claude send"); f.input.value = "New draft";
  f.finish(); await pending; assert.equal(f.input.value, "New draft"); assert.equal(f.closed, 0);
});
test("confirming an earlier prompt does not retire a replacement draft", async () => {
  const f = fixture(); f.input.value = "Replacement draft";
  f.native.drafts.set("claude", "Replacement draft");
  const pending = f.ordinary.send("Claude send"); f.finish(); await pending;
  assert.equal(f.input.value, "Replacement draft"); assert.equal(f.closed, 0);
  assert.equal(f.native.drafts.get("claude"), "Replacement draft");
});

for (const nativeWrapper of [false, true]) test(`session navigation preserves the send target and retires its draft (wrapper=${nativeWrapper})`, async () => {
  const f = fixture();
  if (!nativeWrapper) delete f.ordinary.actions.captureComposer;
  const pending = f.ordinary.send("Claude send");
  f.indexState.selected = "another-session";
  assert.equal(f.controlState.row.id, "claude");
  f.finish(); await pending;
  assert.equal(f.payload.target, "claude");
  assert.equal(f.input.value, ""); assert.equal(f.closed, 1);
  assert.equal(f.send.disabled, false);
});
test("opening another ordinary editor preserves its replacement draft", async () => {
  const f = fixture(); delete f.ordinary.actions.captureComposer;
  const pending = f.ordinary.send("Claude send");
  f.indexState.selected = "other"; f.controlState.row = { id: "other" }; f.input.value = "Other draft";
  f.finish(); await pending;
  assert.equal(f.input.value, "Other draft"); assert.equal(f.closed, 0);
});

for (const nativeWrapper of [false, true]) test(`closing and then navigating retires the unmodified completed draft (wrapper=${nativeWrapper})`, async () => {
  const f = fixture();
  if (!nativeWrapper) delete f.ordinary.actions.captureComposer;
  const pending = f.ordinary.send("Claude send");
  f.controlState.row = null; f.indexState.selected = "other";
  f.finish(); await pending;
  assert.equal(f.input.value, ""); assert.equal(f.closed, 0);
});
