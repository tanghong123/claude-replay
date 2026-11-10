import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";
import { composeCapability, consentQuery, grantOutcome } from "../../claude-replay-html/src/html/shared/control-protocol.js";

const controlSource = await readFile(new URL("../src/codex-ui/control-store.js", import.meta.url), "utf8");
const nativeSource = await readFile(new URL("../src/codex-ui/native-mode.js", import.meta.url), "utf8");
// #s429: the modules reach the page through view-root.js, whose import is stripped below; the real
// module runs in the same context, over this context's own document.
const viewRootSource = await readFile(new URL("../src/codex-ui/view-root.js", import.meta.url), "utf8");
const VIEW_ROOT = `const { viewRoot, page } = (() => { ${viewRootSource.replace(/^export /gm, "")}\n  return { viewRoot, page };\n})();\n`;
const loadClass = (source, name, context) => vm.runInNewContext(VIEW_ROOT + source.replace(/^import .*\n/gm, "").replace(/^export /gm, "") + `\n${name}`, context);

function fixture() {
  const input = { value: "Claude send", focused: 0, dispatchEvent() {}, focus() { this.focused++; } }, send = { disabled: false };
  const document = { getElementById: id => id === "composeInput" ? input : send, activeElement: null };
  // The compose box's own state (control-store.js's `compose`, stage 7), which native mode reads
  // through the ordinary box; the app's selection comes through the view's actions.
  const controlState = { row: { id: "claude" }, mode: "resume", consented: false };
  const indexState = { selected: "claude" };
  let finish, payload, closed = 0, painted = 0;
  const NativeMode = loadClass(nativeSource, "NativeModeControls", { document });
  const native = Object.assign(Object.create(NativeMode.prototype), { ordinary: { compose: controlState }, actions: { selectedId: () => indexState.selected }, view: "ordinary", composerGeneration: 0, drafts: new Map(), draftRevisions: new Map() });
  const Control = loadClass(controlSource, "ControlStore", { document, fetch: () => {}, Event: class { constructor(type) { this.type = type; } }, matchMedia: () => ({ matches: false }), runSend: value => { payload = value; return new Promise(resolve => { finish = resolve; }); } });
  const ordinary = Object.assign(Object.create(Control.prototype), { compose: controlState, actions: { selectedRow: () => ({ id: indexState.selected }), captureComposer: () => native.captureOrdinarySend(), repaintComposer: () => painted++, toast: () => {}, refreshIndex: () => {} }, close: () => closed++ });
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

// #s429 stage 7: the grant's confirm is the host's (the view's `write` option). A live pane's send
// that the monitor answers with a passcode request asks the host again, with a passcode field, and
// sends the same text with the passcode given; a refused passcode asks once more; a cancel sends
// nothing. A dialog's grant goes through the same confirm, then the grant.
function grantFixture({ answers, sends, grants = [] }) {
  const input = { value: "Claude send", dispatchEvent() {}, focus() {} }, send = { disabled: false };
  const document = { getElementById: id => id === "composeInput" ? input : send, activeElement: null };
  const asks = [], payloads = [], posted = [];
  const Control = loadClass(controlSource, "ControlStore", { document, Event: class { constructor(type) { this.type = type; } }, matchMedia: () => ({ matches: false }),
    composeCapability, consentQuery, grantOutcome,
    backend: () => ({ paired: true, fetch: async (path, init) => { posted.push([path, init.body]); return { json: async () => grants.shift() }; } }),
    runSend: async value => { payloads.push(value); return sends.shift(); } });
  const store = Object.assign(Object.create(Control.prototype), {
    compose: { row: { id: "claude", target: "w1:p2", term: "herdr" }, mode: "tmux", consented: false, pending: "" },
    revokeButton: {}, dialog: { active: false }, close() {}, imagesAllowed: () => false,
    actions: { selectedRow: () => ({ id: "claude" }), toast: () => {}, refreshIndex: () => {},
      write: { on: () => true, confirm: ask => { asks.push(ask); return answers.shift(); } } },
  });
  return { store, asks, payloads, posted, input };
}
const settle = async () => { for (let i = 0; i < 20; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const passcodeRequired = { consented: false, step: "grant", outcome: { kind: "passcode-required" } };
const badPasscode = { consented: false, step: "grant", outcome: { kind: "bad-passcode" } };
const sent = { consented: true, step: "send", outcome: { kind: "sent" } };

test("a send the monitor wants a passcode for asks the host for one and sends the same text with it", async () => {
  const f = grantFixture({ answers: ["", "1234"], sends: [passcodeRequired, sent] });
  f.store.attempt(); await settle();
  assert.deepEqual(f.asks.map(ask => [ask.title, ask.passcode]), [["Allow typing into this pane?", false], ["Allow typing into this pane?", true]], "the grant's confirm, then the passcode's");
  assert.match(f.asks[0].text, /herdr pane w1:p2/);
  assert.equal(f.asks[1].text, "This monitor requires a passcode before granting write access.");
  assert.deepEqual(f.payloads.map(p => [p.prompt, p.passcode]), [["Claude send", ""], ["Claude send", "1234"]], "the same text, the second time with the passcode given");
});

test("a refused passcode asks once more, and a cancel sends nothing", async () => {
  const f = grantFixture({ answers: ["", "wrong", null], sends: [passcodeRequired, badPasscode] });
  f.store.attempt(); await settle();
  assert.deepEqual(f.asks.map(ask => ask.text.split(" ")[0]), ["The", "This", "Passcode"], "the grant, the passcode, the passcode again");
  assert.equal(f.asks[2].passcode, true);
  assert.deepEqual(f.payloads.map(p => p.passcode), ["", "wrong"], "cancelled, nothing more is sent");
});

test("a cancelled grant sends nothing", async () => {
  const f = grantFixture({ answers: [null], sends: [] });
  f.store.attempt(); await settle();
  assert.equal(f.asks.length, 1); assert.deepEqual(f.payloads, []);
});

test("a dialog's grant goes through the host's confirm, then the grant, with a passcode when asked", async () => {
  const went = [];
  const f = grantFixture({ answers: ["", "4321"], sends: [], grants: [{ code: "passcode-required" }, { ok: true }] });
  f.store.askGrant(() => went.push("answered")); await settle();
  assert.deepEqual(f.asks.map(ask => ask.passcode), [false, true]);
  assert.match(f.asks[0].text, /reads the dialog in herdr pane w1:p2/);
  assert.deepEqual(f.posted.map(([, body]) => body), ["", "4321"], "the grant, asked again with the passcode");
  assert.deepEqual(went, ["answered"]); assert.equal(f.store.compose.consented, true);
});

// #s429 stage 7: what is still out when the view is unmounted does nothing when it comes back. The
// view finds its elements by id, so after a mount in the same element they would be a new view's.
test("a send that answers after the unmount re-enables nothing and retires no draft", async () => {
  const f = fixture(); delete f.ordinary.actions.captureComposer;
  f.ordinary.dialog = { destroy() {} };
  const pending = f.ordinary.send("Claude send");
  assert.equal(f.send.disabled, true);
  f.ordinary.destroy();
  f.finish(); await pending;
  assert.equal(f.send.disabled, true, "the old send button is left as it was");
  assert.equal(f.input.value, "Claude send", "and the field (a new view's, by its id) keeps its text");
});
test("a confirm the host answers after the unmount grants and sends nothing", async () => {
  let answer;
  const f = grantFixture({ answers: [new Promise(resolve => { answer = resolve; })], sends: [sent], grants: [{ ok: true }] });
  f.store.dialog.destroy = () => {};
  f.store.attempt();
  const went = [];
  f.store.askGrant(() => went.push("answered"));
  f.store.destroy();
  answer(""); await settle();
  assert.deepEqual([f.payloads, f.posted, went], [[], [], []], "no send, no grant");
  f.store.attempt(); f.store.askGrant(() => went.push("again")); await settle();
  assert.equal(f.asks.length, 2, "and the ended box asks the host nothing more");
});
